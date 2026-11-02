//! The shell's own completions (zsh's compsys; bash's and fish's), from a
//! hidden shell that fac keeps warm on its own PTY and in its own session:
//! `shell/zsh/worker.zsh`, `shell/bash/worker.bash`, `shell/fish/worker.fish`.
//! Every shell's worker speaks the same lines (`ready`, `c`, `d`, `err`);
//! a [`WorkerSpec`] says how each one is started.
//!
//! Queries are asynchronous: `lookup` answers from the cache or queues the
//! query, the poll loop feeds the worker's fds to `on_ready` and its
//! deadline to `on_timer`, and `take_completed` says which keys have
//! results since. One query is in flight at a time and at most one waits
//! (the newest). A query that takes over 1 s is interrupted with `^C`; a
//! worker that has not recovered 3 s after the query was sent is killed and
//! restarted, at most 3 times a minute. A key given up is asked again on a
//! lookup 2 s later (at once if it was a prewarm query); after 3 tries it
//! stays failed until the next command.

use std::collections::{HashMap, VecDeque};
use std::ffi::{OsStr, OsString};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use nix::errno::Errno;
use nix::fcntl::{FcntlArg, FdFlag, OFlag, fcntl};
use nix::poll::PollFlags;
use nix::sys::signal::{Signal, kill};
use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::Pid;

use crate::complete::source::{Candidate, Lookup, QueryKey};
use crate::guards::ShellKind;
use crate::pty;
use crate::shell::protocol::{self, ContextValue};

/// The fd number the worker reads fac's commands from.
pub const WORKER_IN_FD: RawFd = 60;
/// The fd number the worker writes its results to.
pub const WORKER_OUT_FD: RawFd = 61;

/// zsh's worker is always the system zsh; bash's and fish's are the
/// user's own shell.
const ZSH: &str = "/bin/zsh";
/// The worker's PTY; ZLE needs a non-zero size.
const COLS: u16 = 300;
const ROWS: u16 = 50;
/// After this a query gets `^C`.
const QUERY_TIMEOUT: Duration = Duration::from_secs(1);
/// After this (since the query was sent) a worker that has not recovered is
/// killed.
const KILL_TIMEOUT: Duration = Duration::from_secs(3);
/// How long a worker may take to get ready (zsh's compinit, bash's loader
/// and git warm-up) before it is killed.
const INIT_TIMEOUT: Duration = Duration::from_secs(15);
/// A key given up is asked again on a lookup this long after. By then the
/// worker has recovered from the `^C` or is being killed (`KILL_TIMEOUT` is
/// 2 s after the `^C`), so the retry does not queue behind that; a load
/// spike (a slow fork or exec) has had time to pass; and a key that is
/// always slow keeps the worker busy at most 1 s in 3.
pub const RETRY_AFTER: Duration = Duration::from_secs(2);
const _: () =
    assert!(RETRY_AFTER.as_millis() >= KILL_TIMEOUT.as_millis() - QUERY_TIMEOUT.as_millis());
/// After this many tries given up, a key stays failed until the next
/// command: a key that is always slow costs the worker at most 3 s and 3
/// `^C` per command.
pub const MAX_TRIES: u32 = 3;
const RESTART_WINDOW: Duration = Duration::from_secs(60);
const MAX_RESTARTS: usize = 3;
const CACHE_CAP: usize = 64;
/// Queried in the current directory once the worker is ready, so their
/// completion functions are loaded before anyone asks.
const PREWARM: &[&str] = &["git ", "cd ", "ls -"];
/// How often killed workers are reaped until they are gone.
const REAP_RETRY: Duration = Duration::from_millis(50);
/// A longer line from the worker is dropped.
const MAX_LINE: usize = 1 << 20;
const FALLBACK_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";
const READ_CHUNK: usize = 64 * 1024;

/// Tells the private dumps of several workers in one process apart.
static INSTANCES: AtomicUsize = AtomicUsize::new(0);

/// How a shell's worker is started and what it is told.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerSpec {
    pub kind: ShellKind,
    /// The shell the worker runs in.
    pub binary: PathBuf,
    /// The worker script, in fac's script dir.
    pub script: PathBuf,
    /// The context keys that must all be known before it starts; an entry
    /// `a|b` is known once either is.
    pub start_gate: &'static [&'static str],
    /// The context keys it mirrors, in the order they are sent.
    pub forwarded: &'static [&'static str],
    /// A forwarded key whose change restarts it instead.
    pub restart_key: Option<&'static str>,
}

impl WorkerSpec {
    /// The worker for a `kind` shell at `shell`, with its script in
    /// `script_dir`.
    pub fn new(kind: ShellKind, shell: &Path, script_dir: &Path) -> Self {
        match kind {
            ShellKind::Zsh => Self {
                kind,
                binary: PathBuf::from(ZSH),
                script: script_dir.join("worker.zsh"),
                // `compdump` may be empty. The shell sends the three
                // together, but they can arrive in separate reads, and a
                // dump that came late would cost a full compinit.
                start_gate: &["path", "fpath", "compdump"],
                forwarded: &[
                    "path",
                    "fpath",
                    "options",
                    "zstyles",
                    "aliases",
                    "functions",
                ],
                restart_key: Some("fpath"),
            },
            // The loader is sourced once, at `init`. A shell without
            // bash-completion may send no `bash_completion` (or an empty
            // one): its `complete` lines open the gate then, and a loader
            // that comes after them restarts the worker.
            ShellKind::Bash => Self {
                kind,
                binary: shell.to_owned(),
                script: script_dir.join("worker.bash"),
                start_gate: &["path", "bash_completion|complete"],
                forwarded: &["path", "cwd", "env", "bash_completion", "complete"],
                restart_key: Some("bash_completion"),
            },
            // `complete -C` with the user's paths, so their own functions
            // and completions load. fish reads a changed path at once (a
            // completion already loaded stays): no restart.
            ShellKind::Fish => Self {
                kind,
                binary: shell.to_owned(),
                script: script_dir.join("worker.fish"),
                start_gate: &["path", "fish_function_path", "fish_complete_path"],
                forwarded: &["path", "fish_function_path", "fish_complete_path"],
                restart_key: None,
            },
        }
    }

    /// argv0 and the arguments. The script path reaches the shell as an
    /// argument (`$1`, `$argv[1]`), so it needs no quoting; no rc file is
    /// read.
    fn argv(&self) -> (&'static str, Vec<OsString>) {
        let script: OsString = self.script.clone().into();
        match self.kind {
            ShellKind::Zsh => (
                "zsh",
                vec![
                    "-f".into(),
                    "-i".into(),
                    "-c".into(),
                    r#"source "$1""#.into(),
                    "zsh".into(),
                    script,
                ],
            ),
            ShellKind::Bash => (
                "bash",
                vec![
                    "--noprofile".into(),
                    "--norc".into(),
                    "-c".into(),
                    r#"source "$1""#.into(),
                    "bash".into(),
                    script,
                ],
            ),
            ShellKind::Fish => (
                "fish",
                vec![
                    "--no-config".into(),
                    "-c".into(),
                    "source $argv[1]".into(),
                    script,
                ],
            ),
        }
    }

    /// Whether the worker keeps a compdump (zsh's compsys).
    fn uses_dump(&self) -> bool {
        self.kind == ShellKind::Zsh
    }

    /// The line that follows the context at a start, after which the
    /// worker loads what it was told and sends `ready`: zsh's names its
    /// dump, bash's has no argument. None: the worker gets ready by itself.
    fn init_line(&self, dump: &Path) -> Option<Vec<u8>> {
        match self.kind {
            ShellKind::Zsh => {
                let mut line = b"init ".to_vec();
                line.extend_from_slice(&protocol::encode_word(dump.as_os_str().as_encoded_bytes()));
                line.push(b'\n');
                Some(line)
            }
            ShellKind::Bash => Some(b"init\n".to_vec()),
            ShellKind::Fish => None,
        }
    }
}

/// A cache entry: the shell's answer, or that the query failed.
#[derive(Debug, Clone)]
enum Cached {
    Ready(Arc<Vec<Candidate>>),
    /// The worker said it failed (`err`), or it was given up [`MAX_TRIES`]
    /// times: not asked again until the cache is invalidated.
    Failed,
    /// Given up `tries` times: failed until `retry_at`, then asked again on
    /// the next lookup.
    GivenUp {
        tries: u32,
        retry_at: Instant,
    },
}

#[derive(Debug, Clone)]
struct Query {
    key: QueryKey,
    rbuffer: String,
}

struct InFlight {
    id: u64,
    query: Query,
    prewarm: bool,
    sent: Instant,
    /// The cache epoch it was sent in; results from an older one are stale.
    epoch: u64,
    candidates: Vec<Candidate>,
    /// `^C` was sent; it already counts as failed.
    interrupted: bool,
}

/// A running worker.
struct Process {
    pid: Pid,
    master: OwnedFd,
    /// fac's end of the command pipe (the worker's fd 60), non-blocking.
    input: OwnedFd,
    /// fac's end of the result pipe (the worker's fd 61), non-blocking.
    output: OwnedFd,
    /// What is still to be written to `input`.
    queue: Vec<u8>,
    /// A partial line read from `output`.
    line: Vec<u8>,
    /// Bytes of an over-long line still to be skipped.
    skipping: bool,
    started: Instant,
    ready: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Not started: the spec's start keys are not all known yet.
    Waiting,
    Running,
    /// Failed too often, or shut down.
    Disabled,
}

pub struct Worker {
    spec: WorkerSpec,
    /// fac's private copy of the user's dump, or the dump compinit writes.
    dump: PathBuf,
    context: HashMap<String, ContextValue>,
    state: State,
    process: Option<Process>,
    /// Counts spawns, so lines of a worker killed mid-read are not taken
    /// for its successor's.
    generation: u64,
    in_flight: Option<InFlight>,
    queued: Option<Query>,
    prewarm: VecDeque<Query>,
    cache: HashMap<QueryKey, (Cached, u64)>,
    /// LRU clock for `cache`.
    tick: u64,
    /// Bumped by `invalidate` and context changes.
    epoch: u64,
    completed: Vec<QueryKey>,
    restarts: VecDeque<Instant>,
    /// Killed workers not reaped yet.
    dying: Vec<Pid>,
    reap_at: Option<Instant>,
    /// The dump has to be prepared again before the next start.
    fresh_dump: bool,
    next_id: u64,
}

impl Worker {
    /// A worker that runs `spec` once started; zsh's keeps its dump in
    /// `~/Library/Caches/fac`.
    pub fn new(spec: WorkerSpec) -> Self {
        let cache_dir = std::env::var_os("HOME")
            .filter(|home| !home.is_empty())
            .map_or_else(std::env::temp_dir, |home| {
                Path::new(&home).join("Library/Caches/fac")
            });
        Self::with_spec(spec, &cache_dir)
    }

    /// zsh's worker, `<script_dir>/worker.zsh`, with its dump in
    /// `cache_dir`.
    pub fn with_cache_dir(script_dir: &Path, cache_dir: &Path) -> Self {
        Self::with_spec(
            WorkerSpec::new(ShellKind::Zsh, Path::new(ZSH), script_dir),
            cache_dir,
        )
    }

    /// Like `new`, with `cache_dir` as the worker's cache dir (only zsh's
    /// keeps a dump there).
    pub fn with_spec(spec: WorkerSpec, cache_dir: &Path) -> Self {
        let pid = std::process::id();
        let name = match INSTANCES.fetch_add(1, Ordering::Relaxed) {
            0 => format!("zcompdump-{pid}"),
            n => format!("zcompdump-{pid}-{n}"),
        };
        Self {
            spec,
            dump: cache_dir.join(name),
            context: HashMap::new(),
            state: State::Waiting,
            process: None,
            generation: 0,
            in_flight: None,
            queued: None,
            prewarm: VecDeque::new(),
            cache: HashMap::new(),
            tick: 0,
            epoch: 0,
            completed: Vec::new(),
            restarts: VecDeque::new(),
            dying: Vec::new(),
            reap_at: None,
            fresh_dump: true,
            next_id: 1,
        }
    }

    /// Remember a context value from the shell (7791). A running worker gets
    /// the spec's forwarded keys at once (zsh's: `path`, `options`,
    /// `zstyles`, `aliases` and `functions`); a change to its restart key
    /// (zsh's `fpath`) restarts it.
    pub fn set_context(&mut self, key: &str, value: &ContextValue) {
        if self.context.get(key) == Some(value) {
            return;
        }
        self.context.insert(key.to_owned(), value.clone());
        if self.process.is_none() || !self.spec.forwarded.contains(&key) {
            return;
        }
        // What was cached was completed under the old value.
        self.epoch += 1;
        self.cache.clear();
        if self.spec.restart_key == Some(key) {
            self.fresh_dump = true;
            self.restart(key, false);
        } else {
            self.send_context(key);
        }
    }

    /// Start the worker once the spec's start keys are known (zsh's:
    /// `path`, `fpath` and `compdump`).
    pub fn start_if_ready(&mut self) {
        if self.state == State::Waiting && self.gate_open() {
            self.spawn();
        }
    }

    /// Whether every start key is known.
    fn gate_open(&self) -> bool {
        self.spec
            .start_gate
            .iter()
            .all(|entry| entry.split('|').any(|key| self.context.contains_key(key)))
    }

    /// The cached results for `key`, or queue a query for it (replacing a
    /// queued one that is not sent yet). A key given up is asked again from
    /// its retry time.
    pub fn lookup(&mut self, key: &QueryKey, rbuffer: &str) -> Lookup {
        self.lookup_at(key, rbuffer, Instant::now())
    }

    /// `lookup` at `now`.
    fn lookup_at(&mut self, key: &QueryKey, rbuffer: &str, now: Instant) -> Lookup {
        if self.state == State::Disabled {
            return Lookup::Unavailable;
        }
        if let Some((cached, used)) = self.cache.get_mut(key) {
            self.tick += 1;
            *used = self.tick;
            match cached {
                Cached::Ready(candidates) => return Lookup::Ready(Arc::clone(candidates)),
                Cached::Failed => return Lookup::Failed,
                Cached::GivenUp { retry_at, .. } if now < *retry_at => return Lookup::Failed,
                // Kept until the key is answered or given up again, which
                // counts its tries.
                Cached::GivenUp { tries, .. } => {
                    tracing::debug!("worker query asked again tries={tries} key={key}");
                }
            }
        }
        if self.in_flight.as_ref().is_some_and(|flight| {
            flight.query.key == *key && !flight.interrupted && flight.epoch == self.epoch
        }) {
            return Lookup::Pending;
        }
        self.prewarm.retain(|query| query.key != *key);
        let query = Query {
            key: key.clone(),
            rbuffer: rbuffer.to_owned(),
        };
        if let Some(old) = self.queued.replace(query)
            && old.key != *key
        {
            tracing::debug!("worker query superseded key={}", old.key);
        }
        self.pump();
        Lookup::Pending
    }

    /// The fds to poll, with the events wanted.
    pub fn poll_fds(&self) -> Vec<(RawFd, PollFlags)> {
        let Some(process) = &self.process else {
            return Vec::new();
        };
        let mut fds = vec![
            (process.output.as_raw_fd(), PollFlags::POLLIN),
            (process.master.as_raw_fd(), PollFlags::POLLIN),
        ];
        if !process.queue.is_empty() {
            fds.push((process.input.as_raw_fd(), PollFlags::POLLOUT));
        }
        fds
    }

    /// `fd` (one of `poll_fds`) is ready: read results, drain the PTY, or
    /// write what is queued.
    pub fn on_ready(&mut self, fd: RawFd, revents: PollFlags) {
        let Some(process) = &self.process else {
            return;
        };
        if revents.is_empty() {
            return;
        }
        if fd == process.output.as_raw_fd() {
            self.read_output();
        } else if fd == process.master.as_raw_fd() {
            self.drain_master();
        } else if fd == process.input.as_raw_fd() {
            self.flush();
        }
        self.pump();
    }

    /// The pids to `waitpid` on SIGCHLD: the worker and killed ones not
    /// reaped yet.
    pub fn pids(&self) -> Vec<Pid> {
        self.process
            .iter()
            .map(|process| process.pid)
            .chain(self.dying.iter().copied())
            .collect()
    }

    /// A child was reaped with `status`. True if it was a worker.
    pub fn on_child_exit(&mut self, pid: Pid, status: i32) -> bool {
        if let Some(at) = self.dying.iter().position(|&dying| dying == pid) {
            self.dying.swap_remove(at);
            return true;
        }
        if self
            .process
            .as_ref()
            .is_some_and(|process| process.pid == pid)
        {
            tracing::info!(status, "worker exited");
            // Reaped already, so it must not be killed: the pid may be reused.
            if self.process.take().is_some_and(|process| !process.ready) {
                self.fresh_dump = true;
            }
            self.restart("exited", true);
            return true;
        }
        false
    }

    /// When `on_timer` next has something to do.
    pub fn next_deadline(&self) -> Option<Instant> {
        let query = self.in_flight.as_ref().map(|flight| {
            flight.sent
                + if flight.interrupted {
                    KILL_TIMEOUT
                } else {
                    QUERY_TIMEOUT
                }
        });
        let init = self
            .process
            .as_ref()
            .filter(|process| !process.ready)
            .map(|process| process.started + INIT_TIMEOUT);
        [query, init, self.reap_at].into_iter().flatten().min()
    }

    /// Timers: interrupt a slow query, kill a worker that does not recover
    /// or never gets ready, reap killed workers.
    pub fn on_timer(&mut self, now: Instant) {
        if self.reap_at.is_some_and(|at| now >= at) {
            self.reap_dying();
        }
        if let Some(flight) = &mut self.in_flight {
            if !flight.interrupted && now >= flight.sent + QUERY_TIMEOUT {
                flight.interrupted = true;
                let (id, key, prewarm) = (flight.id, flight.query.key.clone(), flight.prewarm);
                let current = flight.epoch == self.epoch;
                let tries = match self.cache.get(&key) {
                    Some((Cached::GivenUp { tries, .. }, _)) => tries + 1,
                    _ => 1,
                };
                tracing::info!(
                    "worker query timed out id={id} lbuffer_len={} tries={tries}",
                    key.lbuffer.len()
                );
                if let Some(process) = &self.process
                    && let Err(err) = nix::unistd::write(&process.master, b"\x03")
                {
                    tracing::debug!("writing ^C to the worker failed: {err}");
                }
                if current {
                    let cached = if tries >= MAX_TRIES {
                        Cached::Failed
                    } else {
                        // A prewarm query was the key's first ask, made
                        // before anyone typed and usually cold: the next
                        // lookup (at once, for a user who joined it) asks
                        // again.
                        let retry_at = if prewarm { now } else { now + RETRY_AFTER };
                        Cached::GivenUp { tries, retry_at }
                    };
                    self.cache_insert(key.clone(), cached);
                }
                self.complete(key);
            } else if flight.interrupted && now >= flight.sent + KILL_TIMEOUT {
                self.restart("hung", true);
                return;
            }
        }
        if self
            .process
            .as_ref()
            .is_some_and(|process| !process.ready && now >= process.started + INIT_TIMEOUT)
        {
            self.restart("init timeout", true);
        }
    }

    /// The keys whose results arrived since the last call.
    pub fn take_completed(&mut self) -> Vec<QueryKey> {
        std::mem::take(&mut self.completed)
    }

    /// Clear the cache (a command ran, so what it completed may have changed).
    /// A query not sent yet was for the line that ran: it is dropped, so a
    /// worker that gets ready later does not answer it first.
    pub fn invalidate(&mut self) {
        tracing::debug!("worker cache invalidated n={}", self.cache.len());
        self.cache.clear();
        self.epoch += 1;
        if let Some(query) = self.queued.take() {
            tracing::debug!("worker query dropped key={}", query.key);
        }
    }

    /// Kill the worker and reap it; no more queries after this.
    pub fn shutdown(&mut self) {
        self.state = State::Disabled;
        self.in_flight = None;
        self.queued = None;
        self.prewarm.clear();
        if let Some(process) = self.process.take() {
            kill_worker(process.pid);
            self.dying.push(process.pid);
        }
        for pid in self.dying.drain(..) {
            while let Err(Errno::EINTR) = waitpid(pid, None) {}
        }
        self.reap_at = None;
        match std::fs::remove_file(&self.dump) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => tracing::debug!("removing {}: {err}", self.dump.display()),
        }
    }

    fn spawn(&mut self) {
        if self.fresh_dump {
            self.prepare_dump();
            self.fresh_dump = false;
        }
        match self.spawn_process() {
            Ok(process) => {
                self.generation += 1;
                self.process = Some(process);
                self.state = State::Running;
                for key in self.spec.forwarded {
                    self.send_context(key);
                }
                if let Some(line) = self.spec.init_line(&self.dump) {
                    self.send(&line);
                }
            }
            Err(err) => {
                tracing::error!("worker cannot start: {err:#}; compsys completions are off");
                self.state = State::Disabled;
            }
        }
    }

    /// Copy the user's dump (the `compdump` context key) to the private
    /// path, or remove the private one so a full compinit writes it.
    fn prepare_dump(&mut self) {
        if !self.spec.uses_dump() {
            return;
        }
        match std::fs::remove_file(&self.dump) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => tracing::warn!("removing {}: {err}", self.dump.display()),
        }
        let Some(ContextValue::Text(user)) = self.context.get("compdump") else {
            return;
        };
        let user = Path::new(user);
        if user.as_os_str().is_empty() || !user.is_file() {
            return;
        }
        if let Some(dir) = self.dump.parent()
            && let Err(err) = std::fs::create_dir_all(dir)
        {
            tracing::warn!("creating {}: {err}", dir.display());
            return;
        }
        if let Err(err) = std::fs::copy(user, &self.dump) {
            // The user's $ZSH_COMPDUMP: named at debug only.
            tracing::warn!("copying the user's dump failed ({err}); full compinit");
            tracing::debug!("the user's dump is {}", user.display());
            let _ = std::fs::remove_file(&self.dump);
        }
    }

    fn spawn_process(&self) -> Result<Process> {
        if self.spec.uses_dump()
            && let Some(dir) = self.dump.parent()
        {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let (worker_in, input) = pipe().context("the worker's command pipe")?;
        let (output, worker_out) = pipe().context("the worker's result pipe")?;
        set_nonblocking(input.as_fd())?;
        set_nonblocking(output.as_fd())?;

        let path = match self.context.get("path") {
            Some(ContextValue::List(dirs)) if !dirs.is_empty() => dirs.join(":"),
            _ => FALLBACK_PATH.to_owned(),
        };
        let var = |name: &str, fallback: &str| {
            std::env::var_os(name)
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| fallback.into())
        };
        // bash's worker gets the shell's whole environment (`env`) before
        // `init`; it starts in the shell's HOME too, so nothing it reads
        // is fac's.
        let shell_home = match self.context.get("env") {
            Some(ContextValue::List(entries)) if self.spec.kind == ShellKind::Bash => entries
                .iter()
                .find_map(|entry| entry.strip_prefix("HOME="))
                .filter(|home| !home.is_empty())
                .map(OsString::from),
            _ => None,
        };
        let env: Vec<(OsString, OsString)> = vec![
            (
                "HOME".into(),
                shell_home.unwrap_or_else(|| var("HOME", "/")),
            ),
            ("LANG".into(), var("LANG", "en_US.UTF-8")),
            ("TERM".into(), "xterm-256color".into()),
            ("PATH".into(), path.into()),
            ("FAC_WORKER_IN".into(), WORKER_IN_FD.to_string().into()),
            ("FAC_WORKER_OUT".into(), WORKER_OUT_FD.to_string().into()),
        ];
        let (argv0, args) = self.spec.argv();
        let size = libc::winsize {
            ws_row: ROWS,
            ws_col: COLS,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let child = pty::spawn_process(
            &self.spec.binary,
            &args,
            OsStr::new(argv0),
            &env,
            size,
            vec![(worker_in, WORKER_IN_FD), (worker_out, WORKER_OUT_FD)],
        )?;
        tracing::info!(pid = child.pid.as_raw(), "worker started");
        Ok(Process {
            pid: child.pid,
            master: child.master,
            input,
            output,
            queue: Vec::new(),
            line: Vec::new(),
            skipping: false,
            started: Instant::now(),
            ready: false,
        })
    }

    /// Kill the worker (unless already reaped) and start another, unless it
    /// has failed too often. `counted`: whether this counts toward the cap.
    fn restart(&mut self, reason: &str, counted: bool) {
        tracing::info!("worker restart reason={reason}");
        let was_ready = match self.process.take() {
            Some(process) => {
                kill_worker(process.pid);
                self.dying.push(process.pid);
                self.reap_dying();
                process.ready
            }
            None => true,
        };
        // A worker that died before `ready` may have left a half-written dump.
        if !was_ready {
            self.fresh_dump = true;
        }
        // A query that was cut short (not one that hung) is asked again.
        if let Some(flight) = self.in_flight.take()
            && !flight.interrupted
            && !flight.prewarm
            && self.queued.is_none()
        {
            self.queued = Some(flight.query);
        }
        self.prewarm.clear();
        if counted {
            let now = Instant::now();
            while self
                .restarts
                .front()
                .is_some_and(|&at| now.duration_since(at) >= RESTART_WINDOW)
            {
                self.restarts.pop_front();
            }
            if self.restarts.len() >= MAX_RESTARTS {
                tracing::error!(
                    "worker failed {} times within a minute; compsys completions are off until fac restarts",
                    MAX_RESTARTS + 1
                );
                self.state = State::Disabled;
                self.queued = None;
                return;
            }
            self.restarts.push_back(now);
        }
        self.state = State::Waiting;
        self.spawn();
    }

    fn reap_dying(&mut self) {
        self.dying
            .retain(|&pid| match waitpid(pid, Some(WaitPidFlag::WNOHANG)) {
                Ok(WaitStatus::StillAlive) | Err(Errno::EINTR) => true,
                Ok(_) | Err(_) => false,
            });
        self.reap_at = (!self.dying.is_empty()).then(|| Instant::now() + REAP_RETRY);
    }

    /// Queue `ctx <key> <value>` if the value is known.
    fn send_context(&mut self, key: &str) {
        let Some(value) = self.context.get(key) else {
            return;
        };
        let mut line = format!("ctx {key} ").into_bytes();
        match value {
            ContextValue::Text(text) => {
                line.extend_from_slice(&protocol::encode_word(text.as_bytes()))
            }
            ContextValue::List(items) => line.extend_from_slice(&protocol::encode_word_list(items)),
        }
        line.push(b'\n');
        self.send(&line);
    }

    fn send(&mut self, line: &[u8]) {
        if let Some(process) = &mut self.process {
            process.queue.extend_from_slice(line);
        }
        self.flush();
    }

    /// Write what the command pipe takes now.
    fn flush(&mut self) {
        let Some(process) = &mut self.process else {
            return;
        };
        while !process.queue.is_empty() {
            match nix::unistd::write(&process.input, &process.queue) {
                Ok(n) => {
                    process.queue.drain(..n);
                }
                Err(Errno::EINTR) => {}
                Err(Errno::EAGAIN) => return,
                Err(err) => {
                    tracing::debug!("writing to the worker failed: {err}");
                    self.restart("pipe closed", true);
                    return;
                }
            }
        }
    }

    /// Send the next query, if the worker is idle: the queued one first,
    /// then the prewarm ones.
    fn pump(&mut self) {
        if self.in_flight.is_some() || !self.process.as_ref().is_some_and(|process| process.ready) {
            return;
        }
        let (query, prewarm) = if let Some(query) = self.queued.take() {
            (query, false)
        } else {
            loop {
                match self.prewarm.pop_front() {
                    Some(query) if self.cache.contains_key(&query.key) => {}
                    Some(query) => break (query, true),
                    None => return,
                }
            }
        };
        let id = self.next_id;
        self.next_id += 1;
        tracing::debug!("worker query id={id} key={}", query.key);
        let mut line = format!("q {id} ").into_bytes();
        for field in [&query.key.cwd, &query.key.lbuffer, &query.rbuffer] {
            line.extend_from_slice(&protocol::encode_word(field.as_bytes()));
            line.push(b' ');
        }
        line.pop();
        line.push(b'\n');
        self.in_flight = Some(InFlight {
            id,
            query,
            prewarm,
            sent: Instant::now(),
            epoch: self.epoch,
            candidates: Vec::new(),
            interrupted: false,
        });
        self.send(&line);
    }

    /// Read what the worker wrote and handle each complete line.
    fn read_output(&mut self) {
        let generation = self.generation;
        let mut lines = Vec::new();
        let mut dead = false;
        if let Some(process) = &mut self.process {
            let mut buf = vec![0u8; READ_CHUNK];
            loop {
                match nix::unistd::read(&process.output, &mut buf) {
                    Ok(0) => {
                        dead = true;
                        break;
                    }
                    Ok(n) => split_lines(process, &buf[..n], &mut lines),
                    Err(Errno::EINTR) => {}
                    Err(Errno::EAGAIN) => break,
                    Err(err) => {
                        tracing::debug!("reading from the worker failed: {err}");
                        dead = true;
                        break;
                    }
                }
            }
        }
        for line in lines {
            // A restart while handling the lines leaves the rest unread.
            if self.generation != generation {
                return;
            }
            self.on_line(&line);
        }
        if dead && self.generation == generation {
            self.restart("result pipe closed", true);
        }
    }

    /// Discard what the worker drew on its PTY.
    fn drain_master(&mut self) {
        let Some(process) = &self.process else {
            return;
        };
        let mut buf = [0u8; 4096];
        loop {
            match nix::unistd::read(&process.master, &mut buf) {
                Ok(0) | Err(Errno::EIO) => {
                    self.restart("pty closed", true);
                    return;
                }
                Ok(_) | Err(Errno::EINTR) => {}
                Err(Errno::EAGAIN) => return,
                Err(err) => {
                    tracing::debug!("reading the worker's pty failed: {err}");
                    self.restart("pty error", true);
                    return;
                }
            }
        }
    }

    fn on_line(&mut self, line: &[u8]) {
        let mut parts = line.splitn(3, |&byte| byte == b' ');
        let (command, id, rest) = (parts.next(), parts.next(), parts.next());
        let id = id.and_then(|id| std::str::from_utf8(id).ok()?.parse::<u64>().ok());
        match (command, id) {
            (Some(b"ready"), _) => self.on_worker_ready(line.get(6..).unwrap_or_default()),
            (Some(b"c"), Some(id)) => {
                let Some(flight) = self.in_flight.as_mut().filter(|flight| flight.id == id) else {
                    return;
                };
                match parse_candidate(rest.unwrap_or_default()) {
                    Some(candidate) => flight.candidates.push(candidate),
                    None => tracing::debug!("worker sent a bad record: {}", lossy(line)),
                }
            }
            (Some(b"d"), Some(id)) => self.on_done(id, rest.unwrap_or_default()),
            (Some(b"err"), Some(id)) => self.on_error(id, rest.unwrap_or_default()),
            _ => tracing::debug!("worker sent an unknown line: {}", lossy(line)),
        }
    }

    /// `ready <compinit-ms> <completions> <rebuilt>` (bash's: `ready
    /// <load-ms> <specs> 0`)
    fn on_worker_ready(&mut self, rest: &[u8]) {
        let Some(process) = &mut self.process else {
            return;
        };
        process.ready = true;
        let mut fields = rest.split(|&byte| byte == b' ');
        let (compinit_ms, completions, rebuilt) = (fields.next(), fields.next(), fields.next());
        let compinit_ms = lossy(compinit_ms.unwrap_or_default());
        let completions = lossy(completions.unwrap_or_default());
        if self.spec.kind == ShellKind::Bash {
            // No specs is a bash without bash-completion and `complete`
            // lines: compgen still completes.
            tracing::info!("worker ready load_ms={compinit_ms} specs={completions}");
        } else {
            tracing::info!("worker ready compinit_ms={compinit_ms} completions={completions}");
            if rebuilt == Some(b"1") {
                tracing::info!("the worker's dump defined no completions; rebuilt it");
            }
            if completions == "0" {
                tracing::warn!("compinit defined no completions; compsys completions are empty");
            }
        }
        let cwd = match self.context.get("cwd") {
            Some(ContextValue::Text(cwd)) if !cwd.is_empty() => cwd.clone(),
            _ => std::env::var("HOME").unwrap_or_else(|_| "/".to_owned()),
        };
        self.prewarm = PREWARM
            .iter()
            .map(|lbuffer| Query {
                key: QueryKey {
                    cwd: cwd.clone(),
                    lbuffer: (*lbuffer).to_owned(),
                },
                rbuffer: String::new(),
            })
            .collect();
        self.pump();
    }

    /// `d <id> <count> <elapsed-ms>`
    fn on_done(&mut self, id: u64, rest: &[u8]) {
        let Some(flight) = self.in_flight.take_if(|flight| flight.id == id) else {
            tracing::debug!("worker done id={id} for a query no longer in flight");
            return;
        };
        if flight.interrupted {
            tracing::debug!("worker recovered id={id}");
            return;
        }
        let ms = rest.split(|&byte| byte == b' ').nth(1).unwrap_or_default();
        let rtt = flight.sent.elapsed().as_secs_f64() * 1000.0;
        tracing::debug!(
            "worker done id={id} n={} ms={} rtt_ms={rtt:.1}",
            flight.candidates.len(),
            lossy(ms)
        );
        let key = flight.query.key;
        if flight.epoch == self.epoch {
            self.cache_insert(key.clone(), Cached::Ready(Arc::new(flight.candidates)));
        }
        self.complete(key);
    }

    /// `err <id> <code>`: the query failed; it is cached as failed.
    fn on_error(&mut self, id: u64, code: &[u8]) {
        let Some(flight) = self.in_flight.take_if(|flight| flight.id == id) else {
            return;
        };
        if flight.interrupted {
            tracing::debug!("worker recovered id={id}");
            return;
        }
        tracing::debug!("worker failed id={id} code={}", lossy(code));
        let key = flight.query.key;
        if flight.epoch == self.epoch {
            self.cache_insert(key.clone(), Cached::Failed);
        }
        self.complete(key);
    }

    fn complete(&mut self, key: QueryKey) {
        if !self.completed.contains(&key) {
            self.completed.push(key);
        }
    }

    fn cache_insert(&mut self, key: QueryKey, cached: Cached) {
        if self.cache.len() >= CACHE_CAP
            && !self.cache.contains_key(&key)
            && let Some(oldest) = self
                .cache
                .iter()
                .min_by_key(|(_, (_, used))| *used)
                .map(|(key, _)| key.clone())
        {
            self.cache.remove(&oldest);
        }
        self.tick += 1;
        self.cache.insert(key, (cached, self.tick));
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// SIGKILL a worker fac started and has not reaped.
fn kill_worker(pid: Pid) {
    if let Err(err) = kill(pid, Signal::SIGKILL) {
        tracing::debug!("SIGKILL to the worker failed: {err}");
    }
}

/// Append the complete lines in `bytes` (after `process.line`) to `lines`.
fn split_lines(process: &mut Process, mut bytes: &[u8], lines: &mut Vec<Vec<u8>>) {
    while let Some(end) = memchr::memchr(b'\n', bytes) {
        if process.skipping {
            process.skipping = false;
        } else {
            process.line.extend_from_slice(&bytes[..end]);
            lines.push(std::mem::take(&mut process.line));
        }
        bytes = &bytes[end + 1..];
    }
    if process.skipping {
        return;
    }
    process.line.extend_from_slice(bytes);
    if process.line.len() > MAX_LINE {
        tracing::debug!("worker line over {MAX_LINE} bytes dropped");
        process.line.clear();
        process.skipping = true;
    }
}

/// `<word>;<display>;<description>;<group>;<suffix>;<flags>;<iprefix>;<prefix>;<apre>;<hpre>;<hsuf>`
fn parse_candidate(fields: &[u8]) -> Option<Candidate> {
    let fields: Vec<&[u8]> = fields.split(|&byte| byte == b';').collect();
    let [
        word,
        display,
        description,
        group,
        suffix,
        flags,
        iprefix,
        prefix,
        apre,
        hpre,
        hsuf,
    ] = fields[..]
    else {
        return None;
    };
    let text = |field: &[u8]| -> Option<String> {
        let bytes = protocol::percent_decode(field).ok()?;
        Some(match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(err) => String::from_utf8_lossy(err.as_bytes()).into_owned(),
        })
    };
    let flags = text(flags)?;
    Some(Candidate {
        word: text(word)?,
        display: text(display)?,
        description: text(description)?,
        group: text(group)?,
        suffix: flags.contains('S').then(|| text(suffix)).flatten(),
        verbatim: flags.contains('Q'),
        is_file: flags.contains('f'),
        iprefix: text(iprefix)?,
        prefix: text(prefix)?,
        apre: text(apre)?,
        hpre: text(hpre)?,
        hsuf: text(hsuf)?,
    })
}

fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// A pipe as (reader, writer), FD_CLOEXEC on both ends. macOS has no pipe2.
fn pipe() -> Result<(OwnedFd, OwnedFd)> {
    let (reader, writer) = nix::unistd::pipe()?;
    for fd in [&reader, &writer] {
        let flags = FdFlag::from_bits_truncate(fcntl(fd, FcntlArg::F_GETFD)?);
        fcntl(fd, FcntlArg::F_SETFD(flags | FdFlag::FD_CLOEXEC))?;
    }
    Ok((reader, writer))
}

fn set_nonblocking(fd: BorrowedFd<'_>) -> Result<()> {
    let flags = OFlag::from_bits_truncate(fcntl(fd, FcntlArg::F_GETFL)?);
    fcntl(fd, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK)).context("O_NONBLOCK")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_drops_the_query_not_sent_yet() {
        // Not started: a lookup stays queued.
        let mut worker = Worker::with_cache_dir(Path::new("/nonexistent"), &std::env::temp_dir());
        let key = QueryKey {
            cwd: "/".into(),
            lbuffer: "echo ".into(),
        };
        assert!(matches!(worker.lookup(&key, ""), Lookup::Pending));
        assert!(worker.queued.is_some());
        worker.invalidate();
        assert!(worker.queued.is_none());
    }

    #[test]
    fn a_spec_per_shell() {
        let dir = Path::new("/scripts");
        let zsh = WorkerSpec::new(ShellKind::Zsh, Path::new("/opt/homebrew/bin/zsh"), dir);
        // zsh's worker is the system zsh, whatever the user's zsh is.
        assert_eq!(zsh.binary, Path::new("/bin/zsh"));
        assert_eq!(zsh.script, Path::new("/scripts/worker.zsh"));
        assert_eq!(zsh.start_gate, ["path", "fpath", "compdump"]);
        assert_eq!(
            zsh.forwarded,
            [
                "path",
                "fpath",
                "options",
                "zstyles",
                "aliases",
                "functions"
            ]
        );
        assert_eq!(zsh.restart_key, Some("fpath"));
        assert!(zsh.uses_dump());
        let (argv0, args) = zsh.argv();
        assert_eq!(argv0, "zsh");
        assert_eq!(
            args,
            [
                "-f",
                "-i",
                "-c",
                r#"source "$1""#,
                "zsh",
                "/scripts/worker.zsh"
            ]
        );

        let bash = WorkerSpec::new(ShellKind::Bash, Path::new("/opt/homebrew/bin/bash"), dir);
        assert_eq!(bash.binary, Path::new("/opt/homebrew/bin/bash"));
        assert_eq!(bash.script, Path::new("/scripts/worker.bash"));
        assert_eq!(bash.start_gate, ["path", "bash_completion|complete"]);
        assert_eq!(
            bash.forwarded,
            ["path", "cwd", "env", "bash_completion", "complete"]
        );
        assert_eq!(bash.restart_key, Some("bash_completion"));
        assert!(!bash.uses_dump());
        assert_eq!(
            bash.init_line(Path::new("/d")).as_deref(),
            Some(&b"init\n"[..])
        );
        assert_eq!(
            zsh.init_line(Path::new("/d d")).as_deref(),
            Some(&b"init /d%20d\n"[..])
        );
        let (argv0, args) = bash.argv();
        assert_eq!(argv0, "bash");
        assert_eq!(
            args,
            [
                "--noprofile",
                "--norc",
                "-c",
                r#"source "$1""#,
                "bash",
                "/scripts/worker.bash"
            ]
        );

        let fish = WorkerSpec::new(ShellKind::Fish, Path::new("/usr/local/bin/fish"), dir);
        assert_eq!(fish.binary, Path::new("/usr/local/bin/fish"));
        assert_eq!(
            fish.start_gate,
            ["path", "fish_function_path", "fish_complete_path"]
        );
        assert_eq!(fish.forwarded, fish.start_gate);
        assert_eq!(fish.restart_key, None);
        assert!(!fish.uses_dump());
        assert_eq!(fish.init_line(Path::new("/d")), None);
        let (argv0, args) = fish.argv();
        assert_eq!(argv0, "fish");
        assert_eq!(
            args,
            [
                "--no-config",
                "-c",
                "source $argv[1]",
                "/scripts/worker.fish"
            ]
        );
    }

    #[test]
    fn the_start_gate_is_the_specs() {
        let dir = Path::new("/nonexistent");
        let cache = std::env::temp_dir();
        let path = ContextValue::List(vec!["/bin".into()]);
        let mut zsh = Worker::with_cache_dir(dir, &cache);
        zsh.set_context("path", &path);
        assert!(!zsh.gate_open());
        zsh.set_context("fpath", &ContextValue::List(Vec::new()));
        assert!(!zsh.gate_open());
        zsh.set_context("compdump", &ContextValue::Text(String::new()));
        assert!(zsh.gate_open());
        // fish's waits for the function and completion paths too.
        let mut fish = Worker::with_spec(
            WorkerSpec::new(ShellKind::Fish, Path::new("/x"), dir),
            &cache,
        );
        for key in ["path", "fish_function_path", "fish_complete_path"] {
            assert!(!fish.gate_open(), "{key}");
            fish.set_context(key, &path);
        }
        assert!(fish.gate_open());
        // bash: `path` and the loader, or `path` and the `complete` lines
        // of a shell without bash-completion.
        for (second, value) in [
            ("bash_completion", ContextValue::Text(String::new())),
            ("complete", ContextValue::List(Vec::new())),
        ] {
            let mut bash = Worker::with_spec(
                WorkerSpec::new(ShellKind::Bash, Path::new("/x"), dir),
                &cache,
            );
            bash.set_context("path", &path);
            assert!(!bash.gate_open(), "{second}");
            bash.set_context(second, &value);
            assert!(bash.gate_open(), "{second}");
        }
    }

    #[test]
    fn parses_records() {
        let record = b"-l;-l%20%20--%20long%20listing;long%20listing;options;;Qf;;-;;;";
        assert_eq!(
            parse_candidate(record),
            Some(Candidate {
                word: "-l".into(),
                display: "-l  -- long listing".into(),
                description: "long listing".into(),
                group: "options".into(),
                suffix: None,
                verbatim: true,
                is_file: true,
                iprefix: String::new(),
                prefix: "-".into(),
                apre: String::new(),
                hpre: String::new(),
                hsuf: String::new(),
            })
        );
        let suffixed = parse_candidate(b"a%3Bb;a;;g;;S;$;HO;;;").expect("record");
        assert_eq!(suffixed.word, "a;b");
        assert_eq!(suffixed.suffix.as_deref(), Some(""));
        assert_eq!(
            (suffixed.iprefix.as_str(), suffixed.prefix.as_str()),
            ("$", "HO")
        );
        assert_eq!(parse_candidate(b"a;b;c"), None);
        assert_eq!(parse_candidate(b"a%;a;;;;;;;;;"), None);
        // The old records: without compadd's prefixes and suffixes, and
        // with the typed suffixes and compadd's -i and -I.
        assert_eq!(parse_candidate(b"a;a;;g;;;;a;;"), None);
        assert_eq!(parse_candidate(b"a;a;;g;;;;a;;;;;;;"), None);
    }

    #[test]
    fn parses_compadd_prefixes() {
        let record = b"local;local;;files;;f;;%2Fusr%2Flo;pre%3B;%2Fusr%2F;%2F";
        let candidate = parse_candidate(record).expect("record");
        assert_eq!(
            (
                candidate.apre.as_str(),
                candidate.hpre.as_str(),
                candidate.hsuf.as_str()
            ),
            ("pre;", "/usr/", "/")
        );
        assert_eq!(candidate.prefix, "/usr/lo");
        assert!(candidate.is_file);
        assert_eq!(candidate.insertion(), "pre;/usr/local/");
        // One field too many.
        let mut longer = record.to_vec();
        longer.push(b';');
        assert_eq!(parse_candidate(&longer), None);
    }

    #[test]
    fn cache_is_lru() {
        let mut worker = Worker::with_cache_dir(Path::new("/nonexistent"), &std::env::temp_dir());
        let key = |n: usize| QueryKey {
            cwd: "/".into(),
            lbuffer: format!("cmd{n} "),
        };
        let empty = || Cached::Ready(Arc::new(Vec::new()));
        for n in 0..CACHE_CAP {
            worker.cache_insert(key(n), empty());
        }
        // Touch the oldest, so the second oldest goes next.
        assert!(matches!(worker.lookup(&key(0), ""), Lookup::Ready(_)));
        worker.cache_insert(key(CACHE_CAP), empty());
        assert_eq!(worker.cache.len(), CACHE_CAP);
        assert!(worker.cache.contains_key(&key(0)));
        assert!(!worker.cache.contains_key(&key(1)));
        worker.invalidate();
        assert!(worker.cache.is_empty());
    }

    #[test]
    fn a_failure_is_cached_until_invalidated() {
        let mut worker = Worker::with_cache_dir(Path::new("/nonexistent"), &std::env::temp_dir());
        let key = QueryKey {
            cwd: "/".into(),
            lbuffer: "slowcmd ".into(),
        };
        worker.cache_insert(key.clone(), Cached::Failed);
        // Not asked again on the next keystroke.
        assert!(matches!(worker.lookup(&key, "x"), Lookup::Failed));
        assert!(worker.queued.is_none());
        // A command ran: asked again.
        worker.invalidate();
        assert!(matches!(worker.lookup(&key, ""), Lookup::Pending));
        assert!(worker.queued.is_some());
    }

    /// A worker with no process: queries are sent by hand.
    fn idle_worker() -> Worker {
        Worker::with_cache_dir(Path::new("/nonexistent"), &std::env::temp_dir())
    }

    fn slow_key() -> QueryKey {
        QueryKey {
            cwd: "/".into(),
            lbuffer: "git ".into(),
        }
    }

    /// Send the queued query at `at`, as `pump` would.
    fn send_queued(worker: &mut Worker, at: Instant) {
        let query = worker.queued.take().expect("a query queued");
        send(worker, query, false, at);
    }

    fn send(worker: &mut Worker, query: Query, prewarm: bool, at: Instant) {
        let id = worker.next_id;
        worker.next_id += 1;
        worker.in_flight = Some(InFlight {
            id,
            query,
            prewarm,
            sent: at,
            epoch: worker.epoch,
            candidates: Vec::new(),
            interrupted: false,
        });
    }

    /// Give the query in flight up (its budget ends at `at`), and let the
    /// worker recover from the `^C`. When it was given up.
    fn give_up(worker: &mut Worker, at: Instant) -> Instant {
        worker.on_timer(at - Duration::from_millis(1));
        assert!(worker.in_flight.as_ref().is_some_and(|f| !f.interrupted));
        worker.on_timer(at);
        let flight = worker.in_flight.as_ref().expect("in flight");
        assert!(flight.interrupted);
        let (id, key) = (flight.id, flight.query.key.clone());
        assert!(worker.take_completed().contains(&key));
        worker.on_line(format!("d {id} 0 1").as_bytes());
        assert!(worker.in_flight.is_none());
        at
    }

    /// One user ask of `key` at `at`, given up.
    fn ask_and_give_up(worker: &mut Worker, key: &QueryKey, at: Instant) -> Instant {
        assert!(matches!(worker.lookup_at(key, "", at), Lookup::Pending));
        send_queued(worker, at);
        give_up(worker, at + QUERY_TIMEOUT)
    }

    #[test]
    fn a_given_up_query_is_asked_again_after_the_pause() {
        let mut worker = idle_worker();
        let key = slow_key();
        let given_up = ask_and_give_up(&mut worker, &key, Instant::now());
        assert!(matches!(
            worker.lookup_at(&key, "", given_up + RETRY_AFTER),
            Lookup::Pending
        ));
        assert_eq!(worker.queued.as_ref().map(|q| &q.key), Some(&key));
        // Answered: cached as any answer.
        send_queued(&mut worker, given_up + RETRY_AFTER);
        worker.on_line(b"c 2 a;a;;g;;;;;;;");
        worker.on_line(b"d 2 1 5");
        assert!(matches!(
            worker.lookup_at(&key, "", given_up + RETRY_AFTER * 2),
            Lookup::Ready(candidates) if candidates.len() == 1
        ));
    }

    #[test]
    fn a_given_up_query_is_not_asked_again_before_the_pause() {
        let mut worker = idle_worker();
        let key = slow_key();
        let given_up = ask_and_give_up(&mut worker, &key, Instant::now());
        for at in [
            given_up,
            given_up + Duration::from_millis(500),
            given_up + RETRY_AFTER - Duration::from_millis(1),
        ] {
            assert!(matches!(worker.lookup_at(&key, "x", at), Lookup::Failed));
            assert!(worker.queued.is_none());
        }
    }

    #[test]
    fn a_key_stays_failed_after_its_last_try() {
        let mut worker = idle_worker();
        let key = slow_key();
        let mut at = Instant::now();
        for _ in 0..MAX_TRIES {
            at = ask_and_give_up(&mut worker, &key, at) + RETRY_AFTER;
        }
        for at in [at, at + RETRY_AFTER * 10] {
            assert!(matches!(worker.lookup_at(&key, "", at), Lookup::Failed));
            assert!(worker.queued.is_none());
        }
    }

    #[test]
    fn a_command_resets_the_tries() {
        let mut worker = idle_worker();
        let key = slow_key();
        let mut at = Instant::now();
        for _ in 0..MAX_TRIES {
            at = ask_and_give_up(&mut worker, &key, at) + RETRY_AFTER;
        }
        assert!(matches!(worker.lookup_at(&key, "", at), Lookup::Failed));
        worker.invalidate();
        // Asked at once, and the count starts over.
        for _ in 0..MAX_TRIES {
            at = ask_and_give_up(&mut worker, &key, at) + RETRY_AFTER;
        }
        assert!(matches!(worker.lookup_at(&key, "", at), Lookup::Failed));
    }

    #[test]
    fn a_user_who_joined_a_given_up_prewarm_query_is_asked_again_at_once() {
        let mut worker = idle_worker();
        let key = slow_key();
        let sent = Instant::now();
        let query = Query {
            key: key.clone(),
            rbuffer: String::new(),
        };
        send(&mut worker, query, true, sent);
        // The user types `git ` while the prewarm query runs: joined.
        let typed = sent + Duration::from_millis(300);
        assert!(matches!(worker.lookup_at(&key, "", typed), Lookup::Pending));
        assert!(worker.queued.is_none());
        // Given up: the user's lookup (what `take_completed` brings) asks
        // again at once.
        let given_up = give_up(&mut worker, sent + QUERY_TIMEOUT);
        assert!(matches!(
            worker.lookup_at(&key, "", given_up),
            Lookup::Pending
        ));
        assert_eq!(worker.queued.as_ref().map(|q| &q.key), Some(&key));
        // That ask is a try: given up again, it waits for the pause.
        send_queued(&mut worker, given_up);
        let again = give_up(&mut worker, given_up + QUERY_TIMEOUT);
        assert!(matches!(worker.lookup_at(&key, "", again), Lookup::Failed));
        assert!(matches!(
            worker.lookup_at(&key, "", again + RETRY_AFTER),
            Lookup::Pending
        ));
        // The prewarm query counted: this is the last try.
        send_queued(&mut worker, again + RETRY_AFTER);
        let last = give_up(&mut worker, again + RETRY_AFTER + QUERY_TIMEOUT);
        assert!(matches!(
            worker.lookup_at(&key, "", last + RETRY_AFTER),
            Lookup::Failed
        ));
    }

    #[test]
    fn supersedes_queued_queries() {
        let mut worker = Worker::with_cache_dir(Path::new("/nonexistent"), &std::env::temp_dir());
        let key = |lbuffer: &str| QueryKey {
            cwd: "/".into(),
            lbuffer: lbuffer.into(),
        };
        // Not started: everything waits in the queue, newest wins.
        assert!(matches!(worker.lookup(&key("a "), ""), Lookup::Pending));
        assert!(matches!(worker.lookup(&key("b "), "x"), Lookup::Pending));
        let queued = worker.queued.as_ref().expect("queued");
        assert_eq!(
            (queued.key.lbuffer.as_str(), queued.rbuffer.as_str()),
            ("b ", "x")
        );
        assert!(worker.poll_fds().is_empty());
    }

    #[test]
    fn long_lines_are_dropped() {
        let (reader, writer) = pipe().expect("pipe");
        let (master, input) = pipe().expect("pipe");
        let mut process = Process {
            pid: Pid::from_raw(0),
            master,
            input,
            output: reader,
            queue: Vec::new(),
            line: Vec::new(),
            skipping: false,
            started: Instant::now(),
            ready: true,
        };
        drop(writer);
        let mut lines = Vec::new();
        split_lines(&mut process, b"a\nb", &mut lines);
        split_lines(&mut process, &vec![b'x'; MAX_LINE], &mut lines);
        split_lines(&mut process, b"tail\nc\n", &mut lines);
        assert_eq!(lines, vec![b"a".to_vec(), b"c".to_vec()]);
    }
}
