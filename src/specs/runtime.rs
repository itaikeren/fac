//! The spec thread, `fac-specs`: it decodes specs from the archive and runs
//! their JavaScript, so nothing slow reaches the keystroke path.
//!
//! The poll thread holds a [`SpecThread`] and talks to it by messages only.
//! [`Job`]s go in through a channel, with a byte on a wake pipe; each
//! [`Outcome`] comes back through a shared queue plus one byte on a
//! non-blocking result pipe, whose read end sits in the poll set. The queue's
//! mutex is held only for a push or a drain.
//!
//! A spec without functions is decoded and handed over; it never gets a
//! JavaScript context. A spec with functions also gets its own QuickJS
//! context, created from the thread's one runtime: its module is evaluated,
//! walked with `walk.js`, and every function is kept in a registry indexed
//! by [`FnRef`](super::model::FnRef), with the object that holds it, so a
//! [`Job::Call`] binds `this` as Fig does. A [`Job::Drop`] frees the context.
//!
//! The thread is an event loop. Each round it polls the job wake pipe, the
//! pipes of every generator child (see [`super::process`]) and a timeout set
//! by the nearest timer, child deadline or job budget (and, while a child
//! runs, a short slice to see it exit). Loads and calls run
//! to completion as they come; a [`Job::Generate`] becomes a task that waits
//! for a fetch (a child process, or a JavaScript promise that may start
//! children of its own through `executeShellCommand`), so many generators
//! run at once. After every event the loop settles the `executeShellCommand`
//! promises whose children finished, runs QuickJS's pending jobs, drains the
//! timers, and answers the tasks whose fetches settled. Fig's generator
//! rules are in [`super::generators`].
//!
//! A user spec (`~/.config/fac/specs/<name>.ts`, `.js` or `.json`, see
//! [`super::user`]) comes before the archive's. Each load notes what the
//! name's user spec files looked like; a [`Job::Recheck`] (sent at most once
//! per prompt per name, see [`super::cache`]) compares them again and, when
//! they changed, loads the spec again and answers [`Outcome::Changed`].
//!
//! A versioned spec loads with its newest version first; then the CLI's own
//! version is asked for (its `getVersionCommand`, or `<cmd> --version`), and
//! when it differs the spec is loaded again for it and sent a second time.
//!
//! The thread blocks every signal, so they land on the poll thread, and
//! catches its own panics: after one it reports [`Outcome::Dead`] and exits,
//! and the panic hook leaves the terminal alone (see
//! [`crate::proxy::in_background`]).

use std::cell::RefCell;
use std::cmp::Ordering as CmpOrdering;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::os::fd::{AsFd, AsRawFd, OwnedFd, RawFd};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use nix::errno::Errno;
use nix::fcntl::{FcntlArg, FdFlag, OFlag, fcntl};
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::sys::signal::SigSet;
use rquickjs::function::{Args, Rest};
use rquickjs::loader::{ImportAttributes, Loader, Resolver};
use rquickjs::module::Declared;
use rquickjs::promise::PromiseState;
use rquickjs::{
    Array, CaughtError, Coerced, Context, Ctx, Exception, Function, Module, Object, Persistent,
    Runtime, Value,
};
use serde::Deserialize;

use super::archive::{Archive, Kind};
use super::generators::{self, Cached, GenCache, GenError, Plan, ScriptInput};
use super::model::{Entry, FnRef, Generator, Script, Subcommand, Suggestion};
use super::process::{self, Groups, ProcId, RunError, Runner};
use super::user::{self, Format, Found, Prepared, SpecError};
use super::{VERSIONS_JS, WALK_JS};
use crate::guards::ShellKind;

/// The runtime's heap limit, for every context together.
pub const MEMORY_LIMIT: usize = 64 << 20;
/// The most stack JavaScript may use.
pub const STACK_LIMIT: usize = 1 << 20;
/// How long evaluating a spec's module (and walking it) may take. It is off
/// the keystroke path; the slowest specs take about 50 ms on a quiet machine
/// and over 130 ms on a loaded one.
pub const EVAL_BUDGET: Duration = Duration::from_secs(1);
/// How long [`SpecThread::shutdown`] waits for the thread.
const SHUTDOWN_WAIT: Duration = Duration::from_millis(50);
/// The thread's own stack: room for JavaScript's [`STACK_LIMIT`] and the
/// Rust frames around it.
const THREAD_STACK: usize = 8 << 20;
/// `console.*` lines are cut to this many characters.
const LOG_CHARS: usize = 200;
/// The longest sleep while a promise waits for a timer, so a stop is seen.
const TIMER_SLICE: Duration = Duration::from_millis(10);
/// Where a deadline is "none" in [`Js::deadline`].
const NO_DEADLINE: u64 = u64::MAX;
/// How long one synchronous piece of a generator's JavaScript may run: a
/// script function, `postProcess`, `filterTemplateSuggestions`, one step of
/// a custom generator's promise.
pub const GEN_JS_BUDGET: Duration = Duration::from_millis(500);

const SHIMS_JS: &str = include_str!("shims.js");
const EXEC_JS: &str = include_str!("exec.js");

/// The shell's environment, as the poll thread has it.
pub type Env = Arc<Vec<(String, String)>>;

/// Which version of the CLI a versioned spec was resolved for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpecVersion {
    /// The CLI's version is not known, so the newest version is used.
    Assumed(String),
    /// The CLI reported this version, and the spec was resolved for it.
    Detected(String),
}

/// A spec, decoded and ready for the poll thread.
#[derive(Debug)]
pub struct LoadedSpec {
    pub name: String,
    pub root: Subcommand,
    /// The function paths, indexed by `FnRef`.
    pub fns: Vec<String>,
    /// Whether the thread keeps a context for it, so its functions can be
    /// called.
    pub has_js: bool,
    /// For a versioned spec, the version it was resolved to.
    pub version: Option<SpecVersion>,
    /// How long the load took on the spec thread.
    pub load_time: Duration,
    /// Which load this is: every load (a reload too) gets its own. Jobs
    /// that call the spec's functions carry it, and the spec thread refuses
    /// one whose load is not the spec's current one, as its function ids
    /// may name other functions now.
    pub generation: u64,
}

/// The next load's [`LoadedSpec::generation`]; 0 is never given.
static GENERATION: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LoadError {
    #[error("no spec of that name")]
    NotFound,
    #[error("archive: {0}")]
    Archive(String),
    #[error("javascript: {0}")]
    Js(String),
    #[error("evaluating the spec took longer than its budget")]
    Timeout,
    #[error("the spec thread is stopping")]
    Stopped,
    #[error("the default export is not a spec ({0})")]
    NotSpec(&'static str),
    #[error("the spec does not fit the model: {0}")]
    Model(String),
    #[error("the walk found {found} functions where the archive lists {expected}")]
    FnMismatch { expected: usize, found: usize },
    #[error("a versioned spec with no version file")]
    NoVersionFile,
    /// A user spec file that never got to run.
    #[error("{0}")]
    User(SpecError),
}

impl LoadError {
    /// What went wrong, in words that never come from the spec's file, for
    /// the log: JavaScript's messages and the model's can quote it.
    pub fn summary(&self) -> String {
        match self {
            LoadError::User(err) => err.summary(),
            LoadError::Js(_) => "a JavaScript error".to_owned(),
            LoadError::Model(_) => "the spec does not fit the model".to_owned(),
            other => other.to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CallError {
    #[error("the spec has no live functions")]
    NotLoaded,
    #[error("no function {0} in the spec")]
    UnknownFunction(u32),
    #[error("the arguments are not a JSON array")]
    BadArgs,
    #[error("javascript: {0}")]
    Js(String),
    #[error("the call took longer than its budget")]
    Timeout,
    #[error("the spec thread is stopping")]
    Stopped,
}

/// What the poll thread asks of the spec thread.
#[derive(Debug)]
pub enum Job {
    /// Answered by [`Outcome::Loaded`]. `cwd` and `env` are where a
    /// versioned spec's version command runs; `None` leaves that to the
    /// first job that carries them. A versioned spec whose CLI reports
    /// another version gets a second `Loaded`.
    Load {
        name: String,
        cwd: Option<PathBuf>,
        env: Option<Env>,
    },
    /// Run generator `generator_index` of the arg at `arg_path` (a
    /// [`CurrentArg::path`], `spec:` prefix allowed) of a loaded spec, as
    /// Fig does: `tokens` from the command word to the word being typed,
    /// `search_term` the parser's search term. Answered by
    /// [`Outcome::Generated`], twice when a stale cached value is given
    /// first. A template generator gives nothing here (the poll thread
    /// answers templates).
    ///
    /// `generator` is the generator itself, with the arg's `isDangerous`:
    /// an arg that came from a function's answer (`generateSpec`,
    /// `loadSpec`) is not in the spec's tree, but its functions were saved
    /// in the spec's context when the answer was walked, so they are
    /// called by their ids like the spec's own. `None` looks the generator
    /// up at `arg_path` in the loaded tree.
    ///
    /// `generation` is the spec's load the arg came from
    /// ([`LoadedSpec::generation`]); another load's job is answered
    /// [`GenError::Cancelled`].
    ///
    /// [`CurrentArg::path`]: super::parser::CurrentArg::path
    Generate {
        id: u64,
        spec: String,
        generation: u64,
        arg_path: String,
        generator_index: usize,
        tokens: Vec<String>,
        search_term: String,
        cwd: PathBuf,
        env: Env,
        generator: Option<Box<(Generator, bool)>>,
    },
    /// A generator's `trigger` and `getQueryTerm` functions, as Fig calls
    /// them (`state/generators.ts:141-177`, `suggestions/helpers.ts:102-118`):
    /// `trigger` is `(fn_id, searchTerm, previousSearchTerm)`, `query_term`
    /// is `(fn_id, searchTerm)`. Answered by [`Outcome::Terms`]; for
    /// another load than the spec's current one (`generation`), with
    /// neither.
    Terms {
        id: u64,
        spec: String,
        generation: u64,
        trigger: Option<(u32, String, String)>,
        query_term: Option<(u32, String)>,
    },
    /// Write the usage store: `json` to `path`, atomically. No outcome.
    SaveUsage { path: PathBuf, json: Vec<u8> },
    /// Call a spec's `filterTemplateSuggestions` function `fn_id` with the
    /// template suggestions; answered by [`Outcome::Generated`]
    /// ([`GenError::Cancelled`] for another load than the spec's current
    /// one, `generation`).
    FilterTemplate {
        id: u64,
        spec: String,
        generation: u64,
        fn_id: u32,
        suggestions: Vec<Suggestion>,
    },
    /// Stop generator job `id`: its children are killed, and it gets no
    /// outcome.
    Cancel { id: u64 },
    /// Call function `fn_id` of a loaded spec with the JSON array
    /// `args_json` as its arguments; answered by [`Outcome::Called`] with
    /// the same `id`. With `with_exec` the function also gets
    /// `executeShellCommand` as its last argument, as Fig hands it to
    /// `generateSpec(tokens, exec)`, `loadSpec(token, exec)` and
    /// `alias(token, exec)`; its commands run in `cwd` with `env` (the
    /// first cwd and environment the thread was given when they are
    /// `None`). A promise it returns runs as a task, as a generator's does;
    /// `budget` covers the call and its promise. A call for another load
    /// than the spec's current one (`generation`) is answered
    /// [`CallError::Stopped`], and asked again.
    Call {
        id: u64,
        spec: String,
        generation: u64,
        fn_id: u32,
        args_json: String,
        cwd: Option<PathBuf>,
        env: Option<Env>,
        with_exec: bool,
        budget: Duration,
    },
    /// Forget a spec and free its context. No outcome.
    Drop { name: String },
    /// The poll side let go of an answer of `spec`'s: forget the functions
    /// walked in it (ids from [`WALKED_FIRST`] on). No outcome.
    Release { spec: String, ids: Vec<u32> },
    /// Load a spec again if its user spec files changed (one was added,
    /// edited or removed) since it was loaded: answered by
    /// [`Outcome::Changed`] then, else not at all.
    Recheck { name: String },
    #[cfg(test)]
    Panic,
    #[cfg(test)]
    EvalForever { budget: Duration },
    #[cfg(test)]
    MemoryUsage,
    #[cfg(test)]
    SignalMask,
    #[cfg(test)]
    PanicPlan,
    #[cfg(test)]
    Children,
    /// A usage save that takes `delay`.
    #[cfg(test)]
    SlowSave { delay: Duration },
    /// How many walked functions `spec`'s context keeps.
    #[cfg(test)]
    Walked { spec: String },
}

/// What the spec thread answers.
#[derive(Debug)]
pub enum Outcome {
    Loaded {
        name: String,
        result: Result<Arc<LoadedSpec>, LoadError>,
    },
    /// A [`Job::Recheck`] found the user spec changed and loaded it again:
    /// this replaces what the spec was, an error included.
    Changed {
        name: String,
        result: Result<Arc<LoadedSpec>, LoadError>,
    },
    /// `result` is the JSON of what the function returned (`null` for
    /// undefined). A function in it is `{"$fn": n}`, and `n` can be called
    /// like the spec's own.
    Called {
        id: u64,
        result: Result<String, CallError>,
    },
    /// A generator's suggestions. `stale`: a cached value given while a
    /// fresh one is fetched; another `Generated` for the same id follows.
    /// The poll thread shows nothing for an error, as Fig does.
    Generated {
        id: u64,
        result: Result<Vec<Suggestion>, GenError>,
        stale: bool,
    },
    /// What a [`Job::Terms`] asked: whether the generator runs again (a
    /// throw counts as yes, as in Fig) and the query term (a throw, or a
    /// result that is not a string, gives the search term back).
    Terms {
        id: u64,
        trigger: Option<bool>,
        query_term: Option<String>,
    },
    /// The thread panicked and is gone.
    Dead,
    #[cfg(test)]
    Evaluated {
        result: Result<(), CallError>,
        elapsed: Duration,
    },
    #[cfg(test)]
    MemoryUsage(i64),
    #[cfg(test)]
    SignalMask(Vec<i32>),
    #[cfg(test)]
    PanicPlan(crate::proxy::PanicPlan),
    /// The pids of the running generator children.
    #[cfg(test)]
    Children(Vec<i32>),
    #[cfg(test)]
    Walked(usize),
}

type Outcomes = Arc<Mutex<VecDeque<Outcome>>>;

fn lock(outcomes: &Outcomes) -> MutexGuard<'_, VecDeque<Outcome>> {
    // A push or a drain cannot leave the queue half-changed.
    outcomes
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The poll thread's handle on the spec thread.
pub struct SpecThread {
    jobs: Sender<Job>,
    /// The job wake pipe's write end, non-blocking.
    wake: OwnedFd,
    outcomes: Outcomes,
    /// The result pipe's read end, non-blocking.
    results: OwnedFd,
    /// Asks running JavaScript to stop (read by the interrupt handler).
    stop: Arc<AtomicBool>,
    /// Gets a message when the thread function returns.
    done: Receiver<()>,
    /// `None` once the thread was asked to stop.
    handle: Option<JoinHandle<()>>,
    /// The thread's children's process groups, killed from here when the
    /// thread does not stop in time.
    groups: Groups,
    dead: std::cell::Cell<bool>,
}

impl SpecThread {
    /// Start the thread for a `shell` session. User specs are read from
    /// `$HOME/.config/fac/specs`.
    pub fn start(shell: ShellKind) -> Result<SpecThread> {
        Self::start_with_home(std::env::var_os("HOME").map(PathBuf::from), shell)
    }

    /// Start the thread with user specs under `home` (none without it).
    pub fn start_with_home(home: Option<PathBuf>, shell: ShellKind) -> Result<SpecThread> {
        Self::spawn(home, EVAL_BUDGET, default_generator_timeout(), shell)
    }

    /// Start the thread with another evaluation budget: unit tests run many
    /// loads at once on a loaded machine, where 200 ms of wall time is no
    /// measure of a spec. The tests of the budget itself set their own.
    #[cfg(test)]
    pub(crate) fn start_with_budget(
        home: Option<PathBuf>,
        eval_budget: Duration,
    ) -> Result<SpecThread> {
        Self::spawn(
            home,
            eval_budget,
            default_generator_timeout(),
            ShellKind::Zsh,
        )
    }

    /// Start the thread with another generator timeout (the setting's
    /// stand-in).
    #[cfg(test)]
    pub(crate) fn start_with_timeouts(
        home: Option<PathBuf>,
        eval_budget: Duration,
        generator_timeout: Duration,
    ) -> Result<SpecThread> {
        Self::spawn(home, eval_budget, generator_timeout, ShellKind::Zsh)
    }

    fn spawn(
        home: Option<PathBuf>,
        eval_budget: Duration,
        generator_timeout: Duration,
        shell: ShellKind,
    ) -> Result<SpecThread> {
        let (jobs, job_rx) = mpsc::channel();
        let (wake_read, wake) = pipe().context("the spec thread's wake pipe")?;
        let (results, results_write) = pipe().context("the spec thread's result pipe")?;
        for fd in [&wake_read, &wake, &results, &results_write] {
            set_nonblocking(fd)?;
        }
        let outcomes = Outcomes::default();
        let stop = Arc::new(AtomicBool::new(false));
        let (done_tx, done) = mpsc::channel();
        let groups = Groups::default();
        let init = Init {
            jobs: job_rx,
            wake: wake_read,
            outcomes: Arc::clone(&outcomes),
            results: results_write,
            stop: Arc::clone(&stop),
            done: done_tx,
            user_dir: home.map(|home| home.join(".config/fac/specs")),
            eval_budget,
            generator_timeout,
            groups: groups.clone(),
            shell,
        };
        let handle = std::thread::Builder::new()
            .name("fac-specs".to_owned())
            .stack_size(THREAD_STACK)
            .spawn(move || thread_main(init))
            .context("spawning the spec thread")?;
        Ok(SpecThread {
            jobs,
            wake,
            outcomes,
            results,
            stop,
            done,
            handle: Some(handle),
            groups,
            dead: std::cell::Cell::new(false),
        })
    }

    /// The result pipe, for the poll set (POLLIN).
    pub fn result_fd(&self) -> RawFd {
        self.results.as_raw_fd()
    }

    /// Queue a job; a no-op once the thread is dead. Never blocks.
    pub fn send(&self, job: Job) {
        if self.dead.get() {
            return;
        }
        if self.jobs.send(job).is_err() {
            self.dead.set(true);
            return;
        }
        // EAGAIN: the pipe is full, so the thread is woken already.
        let _ = nix::unistd::write(&self.wake, &[1]);
    }

    /// Queue a job; it is given back when the thread is gone.
    pub fn try_send(&self, job: Job) -> Option<Job> {
        if self.dead.get() {
            return Some(job);
        }
        if let Err(mpsc::SendError(job)) = self.jobs.send(job) {
            self.dead.set(true);
            return Some(job);
        }
        // EAGAIN: the pipe is full, so the thread is woken already.
        let _ = nix::unistd::write(&self.wake, &[1]);
        None
    }

    /// Drain the result pipe and the outcome queue.
    pub fn take(&mut self) -> Vec<Outcome> {
        let mut buf = [0u8; 64];
        loop {
            match nix::unistd::read(&self.results, &mut buf) {
                Ok(0) => {
                    // The thread is gone (its end closed).
                    self.dead.set(true);
                    break;
                }
                Ok(_) | Err(Errno::EINTR) => {}
                Err(_) => break,
            }
        }
        let outcomes: Vec<Outcome> = lock(&self.outcomes).drain(..).collect();
        if outcomes
            .iter()
            .any(|outcome| matches!(outcome, Outcome::Dead))
        {
            self.dead.set(true);
        }
        outcomes
    }

    /// Whether the thread died (it panicked or went away).
    pub fn is_dead(&self) -> bool {
        self.dead.get()
    }

    /// Ask the thread to stop and wait for it, at most 50 ms. The thread
    /// kills its children before it writes the usage saves still queued; a
    /// thread that does not stop in time (a save that takes long) is
    /// detached, and its children are killed from here. Dropping the handle
    /// does the same.
    pub fn shutdown(mut self) {
        self.stop_thread();
    }

    fn stop_thread(&mut self) {
        let Some(handle) = self.handle.take() else {
            return;
        };
        self.stop.store(true, Ordering::Relaxed);
        // A byte on the wake pipe wakes the thread, which sees the flag.
        let _ = nix::unistd::write(&self.wake, &[1]);
        match self.done.recv_timeout(SHUTDOWN_WAIT) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => {
                if handle.join().is_err() {
                    tracing::warn!("the spec thread panicked on its way out");
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                self.groups.kill_all();
                tracing::warn!(
                    "the spec thread did not stop within {} ms; its children were killed and it was detached",
                    SHUTDOWN_WAIT.as_millis()
                );
            }
        }
    }
}

impl Drop for SpecThread {
    fn drop(&mut self) {
        self.stop_thread();
    }
}

/// Load a user spec file as the spec thread would (the same stripping,
/// module rules, budget and model), here and now, without the thread: what
/// `fac spec check` runs. `name` is the command it is for. Also the keys of
/// its tree the model does not know.
pub fn check_user_spec(
    name: &str,
    path: &Path,
    format: Format,
) -> Result<(Arc<LoadedSpec>, Vec<String>), LoadError> {
    let stop = Arc::new(AtomicBool::new(false));
    // No generator runs here, so its timeout does not matter.
    let mut host = Host::new(
        Archive::embedded(),
        None,
        EVAL_BUDGET,
        Duration::from_secs(5),
        stop,
        Groups::default(),
        // Only names the fallback for a broken spec, which this load does
        // not reach.
        ShellKind::Zsh,
    )
    .map_err(|err| LoadError::Js(format!("{err:#}")))?;
    let found = Found {
        path: path.to_owned(),
        format,
        unused: Vec::new(),
    };
    let loaded = host.load_user(name, &found, Instant::now());
    host.js.disarm();
    loaded
}

/// What the thread starts with.
struct Init {
    jobs: Receiver<Job>,
    wake: OwnedFd,
    outcomes: Outcomes,
    results: OwnedFd,
    stop: Arc<AtomicBool>,
    done: Sender<()>,
    user_dir: Option<PathBuf>,
    eval_budget: Duration,
    generator_timeout: Duration,
    groups: Groups,
    shell: ShellKind,
}

/// The `generator_timeout_ms` setting.
fn default_generator_timeout() -> Duration {
    Duration::from_millis(crate::config::global().generator_timeout_ms)
}

/// Sends on the done channel when dropped, as the thread function returns.
struct DoneGuard(Sender<()>);

impl Drop for DoneGuard {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

fn thread_main(init: Init) {
    let Init {
        jobs,
        wake,
        outcomes,
        results,
        stop,
        done,
        user_dir,
        eval_budget,
        generator_timeout,
        groups,
        shell,
    } = init;
    // Declared first, so it drops last: after the runtime is freed.
    let _done = DoneGuard(done);
    if let Err(err) = SigSet::all().thread_block() {
        tracing::warn!("the spec thread could not block signals: {err}");
    }
    crate::proxy::mark_background();
    let post = Poster { outcomes, results };
    let mut host = match Host::new(
        Archive::embedded(),
        user_dir,
        eval_budget,
        generator_timeout,
        Arc::clone(&stop),
        groups,
        shell,
    ) {
        Ok(host) => host,
        Err(err) => {
            tracing::error!("the spec thread could not start QuickJS: {err}");
            post.post(Outcome::Dead);
            return;
        }
    };
    let mut out = Vec::new();
    // On the way out the children are killed first: the saves may take
    // long, and the poll thread waits only so long.
    let finish = |host: &mut Host| {
        drop(std::mem::take(&mut host.inflight.runner));
        last_saves(&jobs);
    };
    loop {
        if stop.load(Ordering::Relaxed) || !wait(&wake, &host) {
            finish(&mut host);
            return;
        }
        let round = catch_unwind(AssertUnwindSafe(|| {
            loop {
                // A save queued with the stop is written by `finish`.
                if stop.load(Ordering::Relaxed) {
                    return false;
                }
                let job = match jobs.try_recv() {
                    Ok(job) => job,
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => return false,
                };
                host.run(job, &mut out);
                out.drain(..).for_each(|outcome| post.post(outcome));
                if stop.load(Ordering::Relaxed) {
                    return false;
                }
            }
            host.pump(&mut out);
            out.drain(..).for_each(|outcome| post.post(outcome));
            true
        }));
        match round {
            Ok(true) => {}
            Ok(false) => {
                finish(&mut host);
                return;
            }
            Err(payload) => {
                tracing::error!("the spec thread panicked; specs are off");
                tracing::debug!(
                    "spec thread panic payload: {}",
                    crate::proxy::panic_message(&*payload)
                );
                // The children are plain processes: kill them.
                drop(std::mem::take(&mut host.inflight.runner));
                // QuickJS's state is not to be trusted after a panic, so
                // nothing of it is freed: a free could abort the process.
                std::mem::forget(host);
                post.post(Outcome::Dead);
                return;
            }
        }
    }
}

/// On the way out: the usage saves still queued are written; every other
/// job is dropped.
fn last_saves(jobs: &Receiver<Job>) {
    while let Ok(job) = jobs.try_recv() {
        match job {
            Job::SaveUsage { path, json } => save_usage(&path, &json),
            #[cfg(test)]
            Job::SlowSave { delay } => std::thread::sleep(delay),
            _ => {}
        }
    }
}

/// `SaveUsage`: what failed is logged by its kind only.
fn save_usage(path: &Path, json: &[u8]) {
    if let Err(err) = crate::complete::usage::write_file(path, json) {
        tracing::warn!("usage: saving failed: {}", err.kind());
    }
}

/// Wait for a job, a child's output or the host's next deadline, and drain
/// the wake pipe. False once the pipe's write end is closed (the poll thread
/// is done with this thread).
fn wait(wake: &OwnedFd, host: &Host) -> bool {
    let now = Instant::now();
    let timeout = match host.next_wake(now) {
        None => PollTimeout::NONE,
        Some(at) => {
            let left = at.saturating_duration_since(now);
            // Rounded up, so a wait never ends just before its deadline.
            let millis = left.as_micros().div_ceil(1000);
            PollTimeout::from(u16::try_from(millis).unwrap_or(u16::MAX))
        }
    };
    let children = host.inflight.runner.fds();
    let mut fds = Vec::with_capacity(children.len() + 1);
    fds.push(PollFd::new(wake.as_fd(), PollFlags::POLLIN));
    fds.extend(
        children
            .into_iter()
            .map(|fd| PollFd::new(fd, PollFlags::POLLIN)),
    );
    match poll(&mut fds, timeout) {
        Ok(_) | Err(Errno::EINTR) => {}
        Err(err) => {
            tracing::error!("the spec thread's poll failed: {err}");
            return false;
        }
    }
    drop(fds);
    let mut buf = [0u8; 64];
    loop {
        match nix::unistd::read(wake, &mut buf) {
            Ok(0) => return false,
            Ok(_) | Err(Errno::EINTR) => {}
            Err(Errno::EAGAIN) => return true,
            Err(err) => {
                tracing::error!("the spec thread's wake pipe failed: {err}");
                return false;
            }
        }
    }
}

/// The thread's end of the outcome queue and the result pipe.
struct Poster {
    outcomes: Outcomes,
    results: OwnedFd,
}

impl Poster {
    fn post(&self, outcome: Outcome) {
        lock(&self.outcomes).push_back(outcome);
        // EAGAIN: a byte is already waiting, which is enough.
        let _ = nix::unistd::write(&self.results, &[1]);
    }
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

fn set_nonblocking(fd: &OwnedFd) -> Result<()> {
    let flags = OFlag::from_bits_truncate(fcntl(fd, FcntlArg::F_GETFL)?);
    fcntl(fd, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK)).context("O_NONBLOCK")?;
    Ok(())
}

/// A live function and the object that holds it.
type Saved = (Persistent<Function<'static>>, Persistent<Object<'static>>);

/// The first id a function in a call's result gets; a spec's own functions
/// are numbered below it.
pub const WALKED_FIRST: u32 = 1 << 24;

/// A spec's live functions. Its own are numbered from 0, as the archive
/// numbers them. Those in a [`Job::Call`]'s result are numbered from a
/// count the whole thread shares ([`Js::next_walked`]), from
/// [`WALKED_FIRST`] on, so an id walked in one context is in no other
/// context's registry: once a spec is loaded again, an id its old context
/// gave fails as `UnknownFunction` instead of calling another function.
/// They are kept until the poll side lets go of the answer that holds them
/// ([`Job::Release`]). The functions in what the thread calls for itself
/// (a generator's, a `trigger`'s) are numbered but not kept: nothing can
/// call them later.
#[derive(Default)]
struct Registry {
    own: Vec<Saved>,
    walked: HashMap<u32, Saved>,
}

impl Registry {
    fn get(&self, id: u32) -> Option<Saved> {
        match self.own.get(id as usize) {
            Some(saved) => Some(saved.clone()),
            None => self.walked.get(&id).cloned(),
        }
    }
}

/// One spec's context. Fields drop in order: every persistent value before
/// the context that owns it.
struct SpecContext {
    registry: Registry,
    timers: Persistent<Function<'static>>,
    walk: Persistent<Function<'static>>,
    /// `exec.js`'s host object: `make`, `settle`, `drop`.
    exec: Persistent<Object<'static>>,
    context: Context,
}

/// A command `executeShellCommand` was asked to run, as `exec.js` sends it.
#[derive(Debug, Deserialize)]
struct ExecInput {
    command: String,
    args: Vec<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    timeout: Option<f64>,
}

#[derive(Debug)]
struct ExecRequest {
    /// The fetch whose `executeShellCommand` asked.
    owner: u64,
    id: u64,
    input: ExecInput,
}

/// What every context's native `spawn` writes to; plain Rust data only, so
/// the JavaScript function that holds it can be freed at any time.
#[derive(Debug, Default)]
struct ExecQueue {
    next: u64,
    requests: Vec<ExecRequest>,
}

/// The JavaScript side of the thread. Fields drop in order: the contexts
/// (and their persistent values) before the runtime, which aborts if any
/// value outlives it.
struct Js {
    contexts: HashMap<String, SpecContext>,
    /// When running JavaScript must stop, as nanoseconds since `base`;
    /// [`NO_DEADLINE`] when none. Read by the interrupt handler.
    deadline: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    base: Instant,
    requests: Rc<RefCell<ExecQueue>>,
    /// The id the next function walked in a result gets (see [`Registry`]).
    next_walked: u32,
    /// rquickjs caches a class's prototype in the runtime, made in the first
    /// context that needs it, and that keeps the whole context alive. This
    /// empty context makes them first, so no spec's context is pinned.
    _anchor: Context,
    runtime: Runtime,
}

/// Why JavaScript did not give a result.
#[derive(Debug)]
enum Fail {
    Timeout,
    Stopped,
    Js(String),
}

impl From<Fail> for LoadError {
    fn from(fail: Fail) -> Self {
        match fail {
            Fail::Timeout => LoadError::Timeout,
            Fail::Stopped => LoadError::Stopped,
            Fail::Js(message) => LoadError::Js(message),
        }
    }
}

impl From<Fail> for CallError {
    fn from(fail: Fail) -> Self {
        match fail {
            Fail::Timeout => CallError::Timeout,
            Fail::Stopped => CallError::Stopped,
            Fail::Js(message) => CallError::Js(message),
        }
    }
}

/// The deadline and stop flag of the job that runs JavaScript now.
struct Limits<'a> {
    deadline: Instant,
    stop: &'a AtomicBool,
}

impl Limits<'_> {
    /// What a failed QuickJS call means: an interrupt (deadline or stop) or
    /// a JavaScript error.
    fn fail<'js>(&self, ctx: &Ctx<'js>, err: rquickjs::Error) -> Fail {
        let caught = CaughtError::from_error(ctx, err);
        if self.stop.load(Ordering::Relaxed) {
            Fail::Stopped
        } else if Instant::now() >= self.deadline {
            Fail::Timeout
        } else {
            Fail::Js(caught.to_string())
        }
    }

    fn check(&self) -> std::result::Result<(), Fail> {
        if self.stop.load(Ordering::Relaxed) {
            Err(Fail::Stopped)
        } else if Instant::now() >= self.deadline {
            Err(Fail::Timeout)
        } else {
            Ok(())
        }
    }
}

/// What walking a value gave.
struct Walked<'js> {
    /// `None` unless asked for.
    json: Option<String>,
    fns: Vec<String>,
    refs: Vec<(Function<'js>, Object<'js>)>,
}

impl Js {
    fn new(stop: Arc<AtomicBool>) -> Result<Js> {
        let runtime = Runtime::new().context("creating the QuickJS runtime")?;
        runtime.set_memory_limit(MEMORY_LIMIT);
        runtime.set_max_stack_size(STACK_LIMIT);
        let deadline = Arc::new(AtomicU64::new(NO_DEADLINE));
        let base = Instant::now();
        let (handler_deadline, handler_stop) = (Arc::clone(&deadline), Arc::clone(&stop));
        runtime.set_interrupt_handler(Some(Box::new(move || {
            handler_stop.load(Ordering::Relaxed)
                || nanos_since(base, Instant::now()) >= handler_deadline.load(Ordering::Relaxed)
        })));
        runtime.set_loader(Builtins, Builtins);
        let anchor = Context::full(&runtime).context("creating the anchor context")?;
        anchor
            .with(|ctx| install_console(&ctx, "anchor"))
            .context("making rquickjs's prototypes")?;
        Ok(Js {
            contexts: HashMap::new(),
            deadline,
            stop,
            base,
            requests: Rc::default(),
            next_walked: WALKED_FIRST,
            _anchor: anchor,
            runtime,
        })
    }

    /// Set the deadline for the JavaScript a job runs from now on.
    fn arm(&self, budget: Duration) -> Instant {
        let deadline = Instant::now() + budget;
        self.deadline
            .store(nanos_since(self.base, deadline), Ordering::Relaxed);
        deadline
    }

    fn disarm(&self) {
        self.deadline.store(NO_DEADLINE, Ordering::Relaxed);
    }

    fn limits(&self, deadline: Instant) -> Limits<'_> {
        Limits {
            deadline,
            stop: &self.stop,
        }
    }

    /// A fresh context with the shims, and the timer drain and the walker
    /// evaluated in it, within the job's limits.
    fn fresh(&self, name: &str, limits: &Limits<'_>) -> std::result::Result<SpecContext, Fail> {
        let context = Context::full(&self.runtime)
            .map_err(|err| Fail::Js(format!("creating a context: {err}")))?;
        let (timers, walk, exec) = context.with(|ctx| {
            let installed = (|| {
                install_console(&ctx, name)?;
                let timers: Function = ctx.eval(SHIMS_JS)?;
                let walk: Function = ctx.eval(WALK_JS)?;
                let factory: Function = ctx.eval(EXEC_JS)?;
                let exec: Object = factory.call((native_spawn(&ctx, &self.requests)?,))?;
                Ok((timers, walk, exec))
            })();
            installed
                .map(|(timers, walk, exec)| {
                    (
                        Persistent::save(&ctx, timers),
                        Persistent::save(&ctx, walk),
                        Persistent::save(&ctx, exec),
                    )
                })
                .map_err(|err: rquickjs::Error| limits.fail(&ctx, err))
        })?;
        Ok(SpecContext {
            registry: Registry::default(),
            timers,
            walk,
            exec,
            context,
        })
    }

    /// Run QuickJS's pending jobs (of every context), each within
    /// [`GEN_JS_BUDGET`], for at most that long in all; a job that throws is
    /// logged and dropped.
    fn run_jobs(&self) {
        let started = Instant::now();
        while started.elapsed() < GEN_JS_BUDGET && !self.stop.load(Ordering::Relaxed) {
            self.arm(GEN_JS_BUDGET);
            match self.runtime.execute_pending_job() {
                Ok(true) => {}
                Ok(false) => break,
                Err(job) => job.0.with(|ctx| {
                    let caught = CaughtError::from_error(&ctx, rquickjs::Error::Exception);
                    tracing::debug!(target: "specs", "a job threw: {caught}");
                }),
            }
        }
        self.disarm();
    }

    /// Drop a spec's context and collect what it held.
    fn drop_context(&mut self, name: &str) -> bool {
        let dropped = self.contexts.remove(name).is_some();
        if dropped {
            self.runtime.run_gc();
        }
        dropped
    }
}

/// The modules a spec can import: only `@fig/autocomplete-generators`
/// ([`user::GENERATORS`]), fresh in each context that imports it, as each of
/// Fig's specs bundled its own copy. Anything else fails to resolve, with an
/// error that names it.
struct Builtins;

impl Resolver for Builtins {
    fn resolve<'js>(
        &mut self,
        _ctx: &Ctx<'js>,
        base: &str,
        name: &str,
        _attributes: Option<ImportAttributes<'js>>,
    ) -> rquickjs::Result<String> {
        if name == user::GENERATORS {
            Ok(name.to_owned())
        } else {
            Err(rquickjs::Error::new_resolving(base, name))
        }
    }
}

impl Loader for Builtins {
    fn load<'js>(
        &mut self,
        ctx: &Ctx<'js>,
        name: &str,
        _attributes: Option<ImportAttributes<'js>>,
    ) -> rquickjs::Result<Module<'js, Declared>> {
        if name == user::GENERATORS {
            Module::declare(ctx.clone(), name, user::GENERATORS_JS)
        } else {
            Err(rquickjs::Error::new_loading(name))
        }
    }
}

fn nanos_since(base: Instant, at: Instant) -> u64 {
    u64::try_from(at.saturating_duration_since(base).as_nanos()).unwrap_or(NO_DEADLINE - 1)
}

/// `console.{log,info,warn,error,debug}`, to fac's log at debug under the
/// `specs` target, cut to [`LOG_CHARS`].
fn install_console<'js>(ctx: &Ctx<'js>, name: &str) -> rquickjs::Result<()> {
    let console = Object::new(ctx.clone())?;
    for level in ["log", "info", "warn", "error", "debug"] {
        let spec = name.to_owned();
        let method = Function::new(ctx.clone(), move |args: Rest<Coerced<String>>| {
            let text: Vec<&str> = args.0.iter().map(|arg| arg.0.as_str()).collect();
            let text: String = text.join(" ").chars().take(LOG_CHARS).collect();
            tracing::debug!(target: "specs", "{spec} console.{level}: {text}");
        })?;
        console.set(level, method)?;
    }
    ctx.globals().set("console", console)
}

/// The native `spawn(owner, json)` that `exec.js` wraps: it queues the
/// command for the event loop and returns its request id, or -1 when the
/// JSON is not a command.
fn native_spawn<'js>(
    ctx: &Ctx<'js>,
    queue: &Rc<RefCell<ExecQueue>>,
) -> rquickjs::Result<Function<'js>> {
    let queue = Rc::clone(queue);
    Function::new(ctx.clone(), move |owner: f64, json: String| -> f64 {
        let Ok(input) = serde_json::from_str::<ExecInput>(&json) else {
            return -1.0;
        };
        let mut queue = queue.borrow_mut();
        let id = queue.next;
        queue.next += 1;
        queue.requests.push(ExecRequest {
            // Fetch ids are small integers, exact as f64.
            owner: owner as u64,
            id,
            input,
        });
        id as f64
    })
}

/// Drive `value` to a result when it is a promise: run QuickJS's jobs, and
/// the context's timers when no job is left, until it settles or the
/// deadline passes.
fn settle<'js>(
    ctx: &Ctx<'js>,
    value: Value<'js>,
    timers: &Function<'js>,
    limits: &Limits<'_>,
) -> std::result::Result<Value<'js>, Fail> {
    let Some(promise) = value.as_promise().cloned() else {
        return Ok(value);
    };
    loop {
        match promise.state() {
            PromiseState::Resolved | PromiseState::Rejected => {
                return match promise.result::<Value>() {
                    Some(Ok(value)) => Ok(value),
                    Some(Err(err)) => Err(limits.fail(ctx, err)),
                    None => Err(Fail::Js("a settled promise has no result".to_owned())),
                };
            }
            PromiseState::Pending => {}
        }
        limits.check()?;
        if ctx.execute_pending_job() {
            continue;
        }
        let next: f64 = timers.call(()).map_err(|err| limits.fail(ctx, err))?;
        if next < 0.0 {
            return Err(Fail::Js(
                "a promise never settles: no job or timer is left".to_owned(),
            ));
        }
        if next > 0.0 {
            let left = limits.deadline.saturating_duration_since(Instant::now());
            // A wait too long for a Duration is longer than the slice.
            let wait = Duration::try_from_secs_f64(next / 1000.0).unwrap_or(TIMER_SLICE);
            std::thread::sleep(wait.min(left).min(TIMER_SLICE));
        }
    }
}

/// Walk `value` with ids from `first`; `json`: whether to build its JSON.
fn walk_value<'js>(
    walk: &Function<'js>,
    value: Value<'js>,
    prefix: &str,
    first: usize,
    json: bool,
) -> rquickjs::Result<Walked<'js>> {
    let walked: Object = walk.call((value, prefix, first, json))?;
    let json: Option<String> = walked.get("json")?;
    let fns: Vec<String> = walked.get("fns")?;
    let pairs: Vec<Array> = walked.get("refs")?;
    let refs = pairs
        .into_iter()
        .map(|pair| Ok((pair.get(0)?, pair.get(1)?)))
        .collect::<rquickjs::Result<_>>()?;
    Ok(Walked { json, fns, refs })
}

fn save<'js>(ctx: &Ctx<'js>, (function, owner): (Function<'js>, Object<'js>)) -> Saved {
    (
        Persistent::save(ctx, function),
        Persistent::save(ctx, owner),
    )
}

/// Keep a spec's own functions, walked from 0; they must stay below
/// [`WALKED_FIRST`].
fn save_own<'js>(
    ctx: &Ctx<'js>,
    refs: Vec<(Function<'js>, Object<'js>)>,
    registry: &mut Registry,
) -> rquickjs::Result<()> {
    if refs.len() > WALKED_FIRST as usize {
        return Err(Exception::throw_range(
            ctx,
            "the spec has too many functions",
        ));
    }
    registry.own = refs.into_iter().map(|pair| save(ctx, pair)).collect();
    Ok(())
}

/// Walk a call's result: its JSON, with its functions numbered from `next`
/// on, and kept callable under those ids in `registry` when one is given
/// (see [`Registry`]).
fn walk_result<'js>(
    ctx: &Ctx<'js>,
    walk: &Function<'js>,
    value: Value<'js>,
    registry: Option<&mut Registry>,
    next: &mut u32,
) -> rquickjs::Result<Option<String>> {
    let walked = walk_value(walk, value, "", *next as usize, true)?;
    let Some(after) = u32::try_from(walked.refs.len())
        .ok()
        .and_then(|count| next.checked_add(count))
    else {
        return Err(Exception::throw_range(ctx, "out of function ids"));
    };
    if let Some(registry) = registry {
        for (id, pair) in (*next..after).zip(walked.refs) {
            registry.walked.insert(id, save(ctx, pair));
        }
    }
    *next = after;
    Ok(walked.json)
}

/// Evaluate a spec module and drive it to completion: its namespace.
fn evaluate_module<'js>(
    ctx: &Ctx<'js>,
    name: &str,
    source: &[u8],
    timers: &Function<'js>,
    limits: &Limits<'_>,
) -> std::result::Result<Object<'js>, Fail> {
    let fail = |err| limits.fail(ctx, err);
    let module = Module::declare(ctx.clone(), name, source.to_vec()).map_err(fail)?;
    let (module, promise) = module.eval().map_err(fail)?;
    settle(ctx, promise.into_value(), timers, limits)?;
    module.namespace().map_err(fail)
}

/// The default export, which must be a plain spec object.
fn default_spec<'js>(
    namespace: &Object<'js>,
    ctx: &Ctx<'js>,
    limits: &Limits<'_>,
) -> std::result::Result<Value<'js>, LoadError> {
    let fail = |err| LoadError::from(limits.fail(ctx, err));
    if !namespace.contains_key("default").map_err(fail)? {
        return Err(LoadError::NotSpec("no default export"));
    }
    let default: Value = namespace.get("default").map_err(fail)?;
    if default.is_function() {
        return Err(LoadError::NotSpec("a function"));
    }
    if default.is_array() || !default.is_object() {
        return Err(LoadError::NotSpec("not an object"));
    }
    Ok(default)
}

/// The module's `versions` export, if it has one.
fn versions_export<'js>(
    namespace: &Object<'js>,
    ctx: &Ctx<'js>,
    limits: &Limits<'_>,
) -> std::result::Result<Option<Value<'js>>, Fail> {
    let fail = |err| limits.fail(ctx, err);
    if !namespace.contains_key("versions").map_err(fail)? {
        return Ok(None);
    }
    namespace.get("versions").map(Some).map_err(fail)
}

/// Apply the diffs of `versions` to `base`: up to the CLI's `target`
/// version when it is known, else every diff (the newest version). The
/// merged spec and its version, when the table names one.
fn apply_versions<'js>(
    ctx: &Ctx<'js>,
    base: Value<'js>,
    versions: Value<'js>,
    target: Option<&str>,
    limits: &Limits<'_>,
) -> std::result::Result<(Value<'js>, Option<String>), Fail> {
    let fail = |err| limits.fail(ctx, err);
    let resolve: Function = ctx.eval(VERSIONS_JS).map_err(fail)?;
    let resolved: Object = resolve.call((base, versions, target)).map_err(fail)?;
    let spec: Value = resolved.get("spec").map_err(fail)?;
    let version: Option<String> = resolved.get("version").map_err(fail)?;
    Ok((spec, version))
}

/// What a load produced on the JavaScript side.
struct Resolved {
    /// The walked tree's JSON; `None` when the archive's tree is used.
    json: Option<String>,
    fns: Vec<String>,
    version: Option<String>,
}

/// The thread's state. Fields drop in order: the generators (whose
/// persistent values belong to contexts) before the JavaScript side.
struct Host {
    inflight: InFlight,
    archive: &'static Archive,
    user_dir: Option<PathBuf>,
    /// How long evaluating a spec may take: [`EVAL_BUDGET`].
    eval_budget: Duration,
    /// The `generator_timeout_ms` setting.
    generator_timeout: Duration,
    /// The shell of the session: its completions stand in for a broken
    /// user spec fac has none of.
    shell: ShellKind,
    /// The specs loaded, as last sent; generators are found in them.
    specs: HashMap<String, Arc<LoadedSpec>>,
    /// What each loaded name's user spec files looked like when it was
    /// loaded, for [`Job::Recheck`].
    stamps: HashMap<String, user::Stamp>,
    versions: Versions,
    js: Js,
}

impl Host {
    fn new(
        archive: &'static Archive,
        user_dir: Option<PathBuf>,
        eval_budget: Duration,
        generator_timeout: Duration,
        stop: Arc<AtomicBool>,
        groups: Groups,
        shell: ShellKind,
    ) -> Result<Host> {
        Ok(Host {
            shell,
            inflight: InFlight {
                runner: Runner::with_groups(groups),
                ..InFlight::default()
            },
            archive,
            user_dir,
            eval_budget,
            generator_timeout,
            specs: HashMap::new(),
            stamps: HashMap::new(),
            versions: Versions::default(),
            js: Js::new(stop)?,
        })
    }

    fn run(&mut self, job: Job, out: &mut Vec<Outcome>) {
        match job {
            Job::Load { name, cwd, env } => {
                if let (Some(cwd), Some(env)) = (cwd, env) {
                    self.note_context(cwd, env);
                }
                self.load_job(name, false, out);
            }
            Job::Recheck { name } => {
                if self.user_changed(&name) {
                    tracing::debug!(target: "specs", "user spec {name} changed; loading it again");
                    self.load_job(name, true, out);
                }
            }
            Job::Call {
                id,
                spec,
                generation,
                fn_id,
                args_json,
                cwd,
                env,
                with_exec,
                budget,
            } => {
                if self.is_stale(&spec, generation) {
                    return out.push(Outcome::Called {
                        id,
                        result: Err(CallError::Stopped),
                    });
                }
                if let (Some(cwd), Some(env)) = (&cwd, &env) {
                    self.note_context(cwd.clone(), Arc::clone(env));
                }
                let call = CallRequest {
                    id,
                    spec,
                    fn_id,
                    args_json,
                    cwd,
                    env,
                    with_exec,
                    budget,
                };
                self.start_call(call, out);
                self.js.disarm();
            }
            Job::Drop { name } => {
                self.cancel_spec(&name, out);
                self.specs.remove(&name);
                self.stamps.remove(&name);
                self.js.drop_context(&name);
            }
            Job::Release { spec, ids } => {
                if let Some(context) = self.js.contexts.get_mut(&spec) {
                    for id in ids.into_iter().filter(|&id| id >= WALKED_FIRST) {
                        context.registry.walked.remove(&id);
                    }
                }
            }
            Job::Generate {
                id,
                spec,
                generation,
                arg_path,
                generator_index,
                tokens,
                search_term,
                cwd,
                env,
                generator,
            } => {
                if self.is_stale(&spec, generation) {
                    return out.push(Outcome::Generated {
                        id,
                        result: Err(GenError::Cancelled),
                        stale: false,
                    });
                }
                self.note_context(cwd.clone(), Arc::clone(&env));
                let request = Request {
                    id,
                    spec,
                    arg_path,
                    generator_index,
                    tokens,
                    search_term,
                    cwd,
                    env,
                    generator,
                };
                self.generate(request, out);
                self.js.disarm();
            }
            Job::Terms {
                id,
                spec,
                generation,
                trigger,
                query_term,
            } => {
                if self.is_stale(&spec, generation) {
                    return out.push(Outcome::Terms {
                        id,
                        trigger: None,
                        query_term: None,
                    });
                }
                let trigger = trigger.map(|(fn_id, current, previous)| {
                    let args = serde_json::json!([current, previous]).to_string();
                    match self.call(&spec, fn_id, &args, GEN_JS_BUDGET, false) {
                        Ok(json) => parse_json(&json).is_ok_and(|value| generators::truthy(&value)),
                        Err(err) => {
                            tracing::debug!(target: "specs", "trigger {id}: {err}");
                            true
                        }
                    }
                });
                let query_term = query_term.map(|(fn_id, term)| {
                    let args = serde_json::json!([term]).to_string();
                    match self.call(&spec, fn_id, &args, GEN_JS_BUDGET, false) {
                        Ok(json) => match serde_json::from_str::<serde_json::Value>(&json) {
                            Ok(serde_json::Value::String(query)) => query,
                            _ => term,
                        },
                        Err(err) => {
                            tracing::debug!(target: "specs", "getQueryTerm {id}: {err}");
                            term
                        }
                    }
                });
                self.js.disarm();
                out.push(Outcome::Terms {
                    id,
                    trigger,
                    query_term,
                });
            }
            Job::SaveUsage { path, json } => save_usage(&path, &json),
            Job::FilterTemplate {
                id,
                spec,
                generation,
                fn_id,
                suggestions,
            } => {
                let result = if self.is_stale(&spec, generation) {
                    Err(GenError::Cancelled)
                } else {
                    self.filter_template(&spec, fn_id, &suggestions)
                };
                self.js.disarm();
                out.push(Outcome::Generated {
                    id,
                    result,
                    stale: false,
                });
            }
            Job::Cancel { id } => self.cancel_task(id),
            #[cfg(test)]
            Job::Panic => panic!("a test asked the spec thread to panic"),
            #[cfg(test)]
            Job::EvalForever { budget } => {
                let started = Instant::now();
                let result = self.eval_forever(budget);
                self.js.disarm();
                out.push(Outcome::Evaluated {
                    result,
                    elapsed: started.elapsed(),
                });
            }
            #[cfg(test)]
            Job::MemoryUsage => {
                self.js.runtime.run_gc();
                out.push(Outcome::MemoryUsage(
                    self.js.runtime.memory_usage().malloc_size,
                ));
            }
            #[cfg(test)]
            Job::SignalMask => out.push(Outcome::SignalMask(blocked_signals())),
            #[cfg(test)]
            Job::PanicPlan => out.push(Outcome::PanicPlan(crate::proxy::panic_plan())),
            #[cfg(test)]
            Job::Children => out.push(Outcome::Children(self.inflight.runner.pids())),
            #[cfg(test)]
            Job::SlowSave { delay } => std::thread::sleep(delay),
            #[cfg(test)]
            Job::Walked { spec } => out.push(Outcome::Walked(
                self.js
                    .contexts
                    .get(&spec)
                    .map_or(0, |context| context.registry.walked.len()),
            )),
        }
    }

    /// Whether the spec is loaded, and `generation` is not its current
    /// load: a job from another load may hold function ids that name other
    /// functions now. (A spec that is not loaded has no functions to call.)
    fn is_stale(&self, spec: &str, generation: u64) -> bool {
        self.specs
            .get(spec)
            .is_some_and(|loaded| loaded.generation != generation)
    }

    /// `Load`, or a `Recheck` that found a change (answered as
    /// [`Outcome::Changed`]).
    fn load_job(&mut self, name: String, changed: bool, out: &mut Vec<Outcome>) {
        let started = Instant::now();
        // A reload replaces the old context, so what runs in it stops.
        self.cancel_spec(&name, out);
        // Taken before the files are read: a change after it is seen next time.
        if let Some(dir) = &self.user_dir {
            self.stamps.insert(name.clone(), user::stamp(dir, &name));
        }
        let result = self.load(&name, started);
        self.js.disarm();
        let detect = matches!(result, Ok((_, true)));
        let result = match result {
            Ok((spec, _)) => {
                self.specs.insert(name.clone(), Arc::clone(&spec));
                Ok(spec)
            }
            Err(err) => {
                self.specs.remove(&name);
                Err(err)
            }
        };
        out.push(if changed {
            Outcome::Changed {
                name: name.clone(),
                result,
            }
        } else {
            Outcome::Loaded {
                name: name.clone(),
                result,
            }
        });
        if detect {
            self.want_version(&name);
        }
    }

    /// Whether a loaded name's user spec files are not what they were when
    /// it was loaded.
    fn user_changed(&self, name: &str) -> bool {
        match (&self.user_dir, self.stamps.get(name)) {
            (Some(dir), Some(stamp)) => user::stamp(dir, name) != *stamp,
            _ => false,
        }
    }

    /// A spec, and whether its CLI's version is still to be asked for (a
    /// versioned spec loaded for its newest version). A user spec comes
    /// first; a broken one falls back to fac's own, with one warning that
    /// names the file and says what is wrong without quoting it.
    fn load(&mut self, name: &str, started: Instant) -> Result<(Arc<LoadedSpec>, bool), LoadError> {
        self.js.drop_context(name);
        if let Some(found) = self
            .user_dir
            .as_deref()
            .and_then(|dir| user::find(dir, name))
        {
            match self.load_user(name, &found, started) {
                Ok((spec, _)) => return Ok((spec, false)),
                Err(err) => {
                    let ours = self.archive.kind(name).is_some();
                    tracing::warn!(
                        "user spec {} did not load ({}); using {}",
                        found.path.display(),
                        err.summary(),
                        if ours {
                            "fac's own"
                        } else {
                            self.shell.completions_label()
                        }
                    );
                    self.js.disarm();
                    if !ours {
                        return Err(err);
                    }
                }
            }
        }
        match self.archive.kind(name).ok_or(LoadError::NotFound)? {
            Kind::Spec => self.load_spec(name, started).map(|spec| (spec, false)),
            Kind::Versioned => {
                if let Some(Some(version)) = self.versions.known.get(name).cloned() {
                    match self.load_detected(name, &version, started) {
                        Ok(spec) => return Ok((spec, false)),
                        Err(err) => {
                            tracing::debug!("{name} for version {version}: {err}");
                            self.js.disarm();
                        }
                    }
                }
                let file =
                    newest_version_file(self.archive, name).ok_or(LoadError::NoVersionFile)?;
                self.load_version_file(name, &file, started, None)
                    .map(|spec| (spec, true))
            }
            Kind::VersionFile => self
                .load_version_file(name, name, started, None)
                .map(|spec| (spec, false)),
        }
    }

    /// A versioned spec for the CLI's `version`, as Fig resolves it
    /// (`tryResolveSpecToSubcommand.ts:12-37`): the index module's default
    /// export picks the version file, whose diffs are applied up to that
    /// version.
    fn load_detected(
        &mut self,
        name: &str,
        version: &str,
        started: Instant,
    ) -> Result<Arc<LoadedSpec>, LoadError> {
        let source = self.source(name)?;
        let deadline = self.js.arm(self.eval_budget);
        let limits = self.js.limits(deadline);
        let index = self.js.fresh(name, &limits)?;
        let (path, target) = index
            .context
            .with(|ctx| -> Result<(String, String), LoadError> {
                let fail = |err| LoadError::from(limits.fail(&ctx, err));
                let timers = index.timers.clone().restore(&ctx).map_err(fail)?;
                let namespace = evaluate_module(&ctx, name, &source, &timers, &limits)?;
                let default: Function = namespace.get("default").map_err(fail)?;
                let picked = default.call((version,)).map_err(fail)?;
                let picked = settle(&ctx, picked, &timers, &limits)?;
                let picked = picked
                    .into_object()
                    .ok_or(LoadError::NotSpec("no versioned spec path"))?;
                let path: String = picked.get("versionedSpecPath").map_err(fail)?;
                let target: Option<String> = picked.get("version").map_err(fail)?;
                Ok((path, target.unwrap_or_else(|| version.to_owned())))
            })?;
        drop(index);
        if self.archive.kind(&path) != Some(Kind::VersionFile) {
            return Err(LoadError::NoVersionFile);
        }
        let spec = self.load_version_file(name, &path, started, Some(&target))?;
        if !spec.has_js {
            // A reload leaves the old context behind.
            self.js.drop_context(name);
        }
        Ok(spec)
    }

    fn entry(&self, name: &str) -> Result<Entry, LoadError> {
        match self.archive.entry(name) {
            Some(Ok(entry)) => Ok(entry),
            Some(Err(err)) => Err(LoadError::Archive(err.to_string())),
            None => Err(LoadError::NotFound),
        }
    }

    fn source(&self, name: &str) -> Result<Vec<u8>, LoadError> {
        match self.archive.source(name) {
            Some(Ok(source)) => Ok(source),
            Some(Err(err)) => Err(LoadError::Archive(err.to_string())),
            None => Err(LoadError::Archive("no module source stored".to_owned())),
        }
    }

    /// A plain spec: the archive's tree, plus a context when it has
    /// functions.
    fn load_spec(&mut self, name: &str, started: Instant) -> Result<Arc<LoadedSpec>, LoadError> {
        let Entry::Spec { root, fns } = self.entry(name)? else {
            return Err(LoadError::Archive("not a plain spec".to_owned()));
        };
        if fns.is_empty() {
            return Ok(finish(name, root, fns, false, None, started));
        }
        let source = self.source(name)?;
        let deadline = self.js.arm(self.eval_budget);
        let limits = self.js.limits(deadline);
        let mut spec = self.js.fresh(name, &limits)?;
        let SpecContext {
            registry,
            timers,
            walk,
            context,
            ..
        } = &mut spec;
        context.with(|ctx| -> Result<(), LoadError> {
            let fail = |err| LoadError::from(limits.fail(&ctx, err));
            let timers = timers.clone().restore(&ctx).map_err(fail)?;
            let walk = walk.clone().restore(&ctx).map_err(fail)?;
            let namespace = evaluate_module(&ctx, name, &source, &timers, &limits)?;
            let default = default_spec(&namespace, &ctx, &limits)?;
            let walked = walk_value(&walk, default, "", 0, false).map_err(fail)?;
            check_fns(&walked.fns, &fns)?;
            save_own(&ctx, walked.refs, registry).map_err(fail)?;
            Ok(())
        })?;
        self.js.contexts.insert(name.to_owned(), spec);
        Ok(finish(name, root, fns, true, None, started))
    }

    /// One version file of a versioned spec, with its diffs applied up to
    /// the CLI's `target` version (every diff without one). `name` is what
    /// was asked for, `file` the version file's entry.
    fn load_version_file(
        &mut self,
        name: &str,
        file: &str,
        started: Instant,
        target: Option<&str>,
    ) -> Result<Arc<LoadedSpec>, LoadError> {
        let Entry::VersionFile {
            root,
            fns,
            versions,
        } = self.entry(file)?
        else {
            return Err(LoadError::Archive("not a version file".to_owned()));
        };
        let source = if fns.is_empty() {
            None
        } else {
            Some(self.source(file)?)
        };
        let deadline = self.js.arm(self.eval_budget);
        let limits = self.js.limits(deadline);
        let mut spec = self.js.fresh(name, &limits)?;
        let SpecContext {
            registry,
            timers,
            walk,
            context,
            ..
        } = &mut spec;
        let resolved = context.with(|ctx| -> Result<Resolved, LoadError> {
            let fail = |err| LoadError::from(limits.fail(&ctx, err));
            let timers = timers.clone().restore(&ctx).map_err(fail)?;
            let walk = walk.clone().restore(&ctx).map_err(fail)?;
            let (base, table) = match &source {
                Some(source) => {
                    let namespace = evaluate_module(&ctx, file, source, &timers, &limits)?;
                    let base = default_spec(&namespace, &ctx, &limits)?;
                    let table = versions_export(&namespace, &ctx, &limits)?
                        .ok_or(LoadError::NotSpec("a version file without versions"))?;
                    // The module must be the one the archive's ids were taken from.
                    let mut paths = walk_value(&walk, base.clone(), "", 0, false)
                        .map_err(fail)?
                        .fns;
                    let first = paths.len();
                    paths.extend(
                        walk_value(&walk, table.clone(), "versions", first, false)
                            .map_err(fail)?
                            .fns,
                    );
                    check_fns(&paths, &fns)?;
                    (base, table)
                }
                None => {
                    let json = serde_json::to_string(&root)
                        .map_err(|err| LoadError::Model(err.to_string()))?;
                    let base = ctx.json_parse(json).map_err(fail)?;
                    let table = ctx.json_parse(versions.as_str()).map_err(fail)?;
                    (base, table)
                }
            };
            let (merged, version) = apply_versions(&ctx, base, table, target, &limits)?;
            let walked = walk_value(&walk, merged, "", 0, true).map_err(fail)?;
            let fns = walked.fns;
            save_own(&ctx, walked.refs, registry).map_err(fail)?;
            Ok(Resolved {
                json: walked.json,
                fns,
                version,
            })
        })?;
        let version = match target {
            Some(target) => Some(SpecVersion::Detected(target.to_owned())),
            None => resolved
                .version
                .or_else(|| file.rsplit('/').next().map(str::to_owned))
                .map(SpecVersion::Assumed),
        };
        self.resolved(name, spec, resolved.json, resolved.fns, version, started)
    }

    /// A user spec file: a JSON spec is read into the model, with no
    /// context; a TypeScript or JavaScript one is evaluated, walked and read
    /// from its JSON, like a versioned spec's result, and a `versions`
    /// export is applied. Also the keys of its tree the model does not know
    /// (a JSON spec has none: they are an error there).
    fn load_user(
        &mut self,
        name: &str,
        found: &Found,
        started: Instant,
    ) -> Result<(Arc<LoadedSpec>, Vec<String>), LoadError> {
        let text = user::read(&found.path).map_err(LoadError::User)?;
        match user::prepare(&text, found.format).map_err(LoadError::User)? {
            Prepared::Json(root) => Ok((
                finish(name, *root, Vec::new(), false, None, started),
                Vec::new(),
            )),
            Prepared::Js { source, lines } => {
                // Named after its file, which QuickJS's messages then say;
                // a TypeScript file's places are put back where they were.
                let module = found
                    .path
                    .file_name()
                    .and_then(|file| file.to_str())
                    .unwrap_or(name)
                    .to_owned();
                self.load_user_module(name, &module, source.as_bytes(), started)
                    .map_err(|err| match (err, &lines) {
                        (LoadError::Js(message), Some(lines)) => {
                            LoadError::Js(lines.remap(message.trim_end(), &module))
                        }
                        (LoadError::Js(message), None) => {
                            LoadError::Js(message.trim_end().to_owned())
                        }
                        (err, _) => err,
                    })
            }
        }
    }

    fn load_user_module(
        &mut self,
        name: &str,
        module: &str,
        source: &[u8],
        started: Instant,
    ) -> Result<(Arc<LoadedSpec>, Vec<String>), LoadError> {
        let deadline = self.js.arm(self.eval_budget);
        let limits = self.js.limits(deadline);
        let mut spec = self.js.fresh(name, &limits)?;
        let SpecContext {
            registry,
            timers,
            walk,
            context,
            ..
        } = &mut spec;
        let resolved = context.with(|ctx| -> Result<Resolved, LoadError> {
            let fail = |err| LoadError::from(limits.fail(&ctx, err));
            let timers = timers.clone().restore(&ctx).map_err(fail)?;
            let walk = walk.clone().restore(&ctx).map_err(fail)?;
            let namespace = evaluate_module(&ctx, module, source, &timers, &limits)?;
            let mut spec = default_spec(&namespace, &ctx, &limits)?;
            let mut version = None;
            if let Some(table) = versions_export(&namespace, &ctx, &limits)? {
                (spec, version) = apply_versions(&ctx, spec, table, None, &limits)?;
            }
            let plain: Function = ctx.eval(user::PLAIN_GENERATORS_JS).map_err(fail)?;
            let spec: Value = plain.call((spec,)).map_err(fail)?;
            let walked = walk_value(&walk, spec, "", 0, true).map_err(fail)?;
            let fns = walked.fns;
            save_own(&ctx, walked.refs, registry).map_err(fail)?;
            Ok(Resolved {
                json: walked.json,
                fns,
                version,
            })
        })?;
        let unknown = resolved
            .json
            .as_deref()
            .map(unknown_keys)
            .unwrap_or_default();
        let spec = self.resolved(
            name,
            spec,
            resolved.json,
            resolved.fns,
            resolved.version.map(SpecVersion::Assumed),
            started,
        )?;
        Ok((spec, unknown))
    }

    /// Finish a load whose tree came from JavaScript: read the JSON into the
    /// model, and keep the context only when the tree has functions.
    fn resolved(
        &mut self,
        name: &str,
        spec: SpecContext,
        json: Option<String>,
        fns: Vec<String>,
        version: Option<SpecVersion>,
        started: Instant,
    ) -> Result<Arc<LoadedSpec>, LoadError> {
        let json = json.ok_or_else(|| LoadError::Model("no tree".to_owned()))?;
        let root: Subcommand =
            serde_json::from_str(&json).map_err(|err| LoadError::Model(err.to_string()))?;
        let has_js = !fns.is_empty();
        if has_js {
            self.js.contexts.insert(name.to_owned(), spec);
        } else {
            drop(spec);
            self.js.runtime.run_gc();
        }
        Ok(finish(name, root, fns, has_js, version, started))
    }

    /// Call function `fn_id` of a spec with `this` bound to its owner. With
    /// `settle`, a promise it returns is driven to its result; without, the
    /// promise itself is the result (and has no JSON form).
    fn call(
        &mut self,
        spec: &str,
        fn_id: u32,
        args_json: &str,
        budget: Duration,
        settle_promise: bool,
    ) -> Result<String, CallError> {
        let deadline = self.js.arm(budget);
        let limits = Limits {
            deadline,
            stop: &self.js.stop,
        };
        let entry = self.js.contexts.get_mut(spec).ok_or(CallError::NotLoaded)?;
        let SpecContext {
            registry,
            timers,
            walk,
            context,
            ..
        } = entry;
        let next = &mut self.js.next_walked;
        let (function, owner) = registry
            .get(fn_id)
            .ok_or(CallError::UnknownFunction(fn_id))?;
        context.with(|ctx| -> Result<String, CallError> {
            let fail = |err| CallError::from(limits.fail(&ctx, err));
            let function = function.restore(&ctx).map_err(fail)?;
            let owner = owner.restore(&ctx).map_err(fail)?;
            let timers = timers.clone().restore(&ctx).map_err(fail)?;
            let walk = walk.clone().restore(&ctx).map_err(fail)?;
            let parsed = ctx.json_parse(args_json).map_err(|_| {
                // The parse error is not interesting; clear it.
                ctx.catch();
                CallError::BadArgs
            })?;
            let parsed = parsed.into_array().ok_or(CallError::BadArgs)?;
            let mut args = Args::new(ctx.clone(), parsed.len());
            args.this(owner).map_err(fail)?;
            for arg in parsed.iter::<Value>() {
                args.push_arg(arg.map_err(fail)?).map_err(fail)?;
            }
            let mut value: Value = args.apply(&function).map_err(fail)?;
            if settle_promise {
                value = settle(&ctx, value, &timers, &limits)?;
            }
            if value.is_undefined() {
                return Ok("null".to_owned());
            }
            // The thread uses the result itself: its functions are not kept.
            walk_result(&ctx, &walk, value, None, next)
                .map_err(fail)?
                .ok_or_else(|| CallError::Js("the result has no JSON form".to_owned()))
        })
    }

    /// `Call`: the function's result is answered at once; a promise it
    /// returns becomes a fetch, answered when it settles or its budget runs
    /// out. Leaves the interrupt deadline armed.
    fn start_call(&mut self, call: CallRequest, out: &mut Vec<Outcome>) {
        let fetch = self.inflight.next_fetch;
        self.inflight.next_fetch += 1;
        let deadline = self.js.arm(call.budget);
        let limits = Limits {
            deadline,
            stop: &self.js.stop,
        };
        let next = &mut self.js.next_walked;
        let started = match self.js.contexts.get_mut(&call.spec) {
            None => Err(CallError::NotLoaded),
            Some(entry) => {
                let SpecContext {
                    registry,
                    walk,
                    exec,
                    context,
                    ..
                } = entry;
                match registry.get(call.fn_id) {
                    None => Err(CallError::UnknownFunction(call.fn_id)),
                    Some((function, owner)) => context.with(|ctx| {
                        let fail = |err| CallError::from(limits.fail(&ctx, err));
                        let function = function.restore(&ctx).map_err(fail)?;
                        let owner = owner.restore(&ctx).map_err(fail)?;
                        let walk = walk.clone().restore(&ctx).map_err(fail)?;
                        let parsed = ctx.json_parse(call.args_json.as_str()).map_err(|_| {
                            // The parse error is not interesting; clear it.
                            ctx.catch();
                            CallError::BadArgs
                        })?;
                        let parsed = parsed.into_array().ok_or(CallError::BadArgs)?;
                        let mut args = Args::new(ctx.clone(), parsed.len() + 1);
                        args.this(owner).map_err(fail)?;
                        for arg in parsed.iter::<Value>() {
                            args.push_arg(arg.map_err(fail)?).map_err(fail)?;
                        }
                        if call.with_exec {
                            let exec_host = exec.clone().restore(&ctx).map_err(fail)?;
                            let make: Function = exec_host.get("make").map_err(fail)?;
                            let exec: Function = make.call((fetch as f64,)).map_err(fail)?;
                            args.push_arg(exec).map_err(fail)?;
                        }
                        let value: Value = args.apply(&function).map_err(fail)?;
                        if value.is_promise() {
                            return Ok(CallStart::Pending(Persistent::save(&ctx, value)));
                        }
                        if value.is_undefined() {
                            return Ok(CallStart::Done("null".to_owned()));
                        }
                        // Functions in the result become callable by their ids.
                        walk_result(&ctx, &walk, value, Some(registry), next)
                            .map_err(fail)?
                            .map(CallStart::Done)
                            .ok_or_else(|| CallError::Js("the result has no JSON form".to_owned()))
                    }),
                }
            }
        };
        let CallRequest {
            id,
            spec,
            cwd,
            env,
            budget,
            ..
        } = call;
        let promise = match started {
            Ok(CallStart::Pending(promise)) => promise,
            Ok(CallStart::Done(json)) => {
                self.drop_exec(&spec, fetch);
                return out.push(Outcome::Called {
                    id,
                    result: Ok(json),
                });
            }
            Err(err) => {
                self.drop_exec(&spec, fetch);
                return out.push(Outcome::Called {
                    id,
                    result: Err(err),
                });
            }
        };
        let context = self.versions.context.clone();
        let cwd = cwd
            .or_else(|| context.as_ref().map(|(cwd, _)| cwd.clone()))
            .unwrap_or_else(|| PathBuf::from("/"));
        let env = env
            .or_else(|| context.map(|(_, env)| env))
            .unwrap_or_default();
        self.inflight.fetches.insert(
            fetch,
            Fetch {
                kind: FetchKind::Call { id, spec, promise },
                key: None,
                waiters: Vec::new(),
                cwd,
                env,
                deadline: Some(Instant::now() + budget),
            },
        );
    }

    /// Forget the `executeShellCommand` requests of fetch `owner` in a
    /// spec's context (a call that ended without becoming a fetch).
    fn drop_exec(&mut self, spec: &str, owner: u64) {
        if let Some(context) = self.js.contexts.get(spec) {
            self.js.arm(GEN_JS_BUDGET);
            call_exec(context, "drop", (owner as f64,));
            self.js.disarm();
        }
    }

    #[cfg(test)]
    fn eval_forever(&mut self, budget: Duration) -> Result<(), CallError> {
        let deadline = self.js.arm(budget);
        let limits = self.js.limits(deadline);
        let spec = self.js.fresh("eval-forever", &limits)?;
        spec.context.with(|ctx| {
            ctx.eval::<Value, _>("for (;;) {}")
                .map(drop)
                .map_err(|err| CallError::from(limits.fail(&ctx, err)))
        })
    }
}

/// A [`Job::Generate`], unpacked.
struct Request {
    id: u64,
    spec: String,
    arg_path: String,
    generator_index: usize,
    tokens: Vec<String>,
    search_term: String,
    cwd: PathBuf,
    env: Env,
    generator: Option<Box<(Generator, bool)>>,
}

/// A [`Job::Call`], unpacked.
struct CallRequest {
    id: u64,
    spec: String,
    fn_id: u32,
    args_json: String,
    cwd: Option<PathBuf>,
    env: Option<Env>,
    with_exec: bool,
    budget: Duration,
}

/// What a call's function gave.
enum CallStart {
    /// The JSON of a plain value.
    Done(String),
    /// A promise, to wait for.
    Pending(Persistent<Value<'static>>),
}

/// A generator job waiting for its fetch.
struct Task {
    spec: String,
    generator: Generator,
    /// Fig's `isDangerous`: the arg's (`state/generators.ts:34`).
    is_dangerous: bool,
    tokens: Vec<String>,
}

/// What a fetch waits for.
enum FetchKind {
    /// A script generator's command.
    Script(ProcId),
    /// A custom generator's promise, in its spec's context.
    Custom {
        spec: String,
        promise: Persistent<Value<'static>>,
    },
    /// A versioned spec's `getVersionCommand` promise, in a context of its
    /// own (the promise drops first).
    VersionJs {
        name: String,
        promise: Persistent<Value<'static>>,
        context: SpecContext,
    },
    /// `<cmd> --version`.
    VersionRun { name: String, proc: ProcId },
    /// A [`Job::Call`]'s promise, in its spec's context.
    Call {
        id: u64,
        spec: String,
        promise: Persistent<Value<'static>>,
    },
}

/// One run of a generator (several tasks wait on it when the cache joins
/// them), or of a version command.
struct Fetch {
    kind: FetchKind,
    /// The cache entry it fills.
    key: Option<String>,
    waiters: Vec<u64>,
    /// Where `executeShellCommand` runs, unless told otherwise.
    cwd: PathBuf,
    env: Env,
    /// When its promise must have settled.
    deadline: Option<Instant>,
}

impl Fetch {
    fn is_js(&self) -> bool {
        matches!(
            self.kind,
            FetchKind::Custom { .. } | FetchKind::VersionJs { .. } | FetchKind::Call { .. }
        )
    }
}

/// The generators in flight.
#[derive(Default)]
struct InFlight {
    runner: Runner,
    cache: GenCache,
    tasks: HashMap<u64, Task>,
    fetches: HashMap<u64, Fetch>,
    next_fetch: u64,
    /// Children started by `executeShellCommand`: their fetch and request.
    execs: HashMap<ProcId, (u64, u64)>,
    /// Children that are a fetch themselves.
    procs: HashMap<ProcId, u64>,
    /// When the next timer of a context with a promise in flight is due.
    next_timer: Option<Instant>,
}

/// The CLI versions of versioned specs (`loadHelpers.ts:149-190`).
#[derive(Default)]
struct Versions {
    /// What each command reported, for the session; `None` when its
    /// `getVersionCommand` gave no string.
    known: HashMap<String, Option<String>>,
    running: HashSet<String>,
    /// Commands waiting for a cwd and an environment.
    waiting: Vec<String>,
    /// The cwd and environment of the first job that carried them.
    context: Option<(PathBuf, Env)>,
}

/// How a generator's fetch starts.
enum Start {
    Script(ScriptInput),
    Custom(FnRef),
}

fn gen_error(err: CallError) -> GenError {
    match err {
        CallError::Timeout => GenError::Timeout,
        CallError::Stopped => GenError::Cancelled,
        other => GenError::Js(other.to_string()),
    }
}

/// A call's promise failed the way a generator's does.
fn call_error(err: GenError) -> CallError {
    match err {
        GenError::Timeout => CallError::Timeout,
        GenError::Cancelled => CallError::Stopped,
        GenError::Js(message) | GenError::Spawn(message) => CallError::Js(message),
    }
}

fn fail_error(fail: Fail) -> GenError {
    match fail {
        Fail::Timeout => GenError::Timeout,
        Fail::Stopped => GenError::Cancelled,
        Fail::Js(message) => GenError::Js(message),
    }
}

fn run_error(err: RunError) -> GenError {
    match err {
        RunError::Timeout => GenError::Timeout,
        RunError::Spawn(message) => GenError::Spawn(message),
    }
}

fn parse_json(json: &str) -> Result<serde_json::Value, GenError> {
    serde_json::from_str(json)
        .map_err(|err| GenError::Js(format!("a result that is not JSON: {err}")))
}

/// A child for a generator: the shell's environment with the input's on
/// top, in the shell's cwd unless the input names one (relative to it).
fn command(
    program: String,
    args: Vec<String>,
    cwd: Option<&str>,
    overrides: &BTreeMap<String, String>,
    shell_cwd: &Path,
    env: &[(String, String)],
    timeout: Duration,
) -> process::Command {
    let mut vars: Vec<(String, String)> = env
        .iter()
        .filter(|(key, _)| !overrides.contains_key(key))
        .cloned()
        .collect();
    vars.extend(
        overrides
            .iter()
            .map(|(key, value)| (key.clone(), value.clone())),
    );
    process::Command {
        program,
        args,
        cwd: cwd.map_or_else(|| shell_cwd.to_path_buf(), |cwd| shell_cwd.join(cwd)),
        env: vars,
        timeout,
    }
}

/// The context a JavaScript fetch runs in.
fn fetch_context<'a>(fetch: &'a Fetch, js: &'a Js) -> Option<&'a SpecContext> {
    match &fetch.kind {
        FetchKind::Custom { spec, .. } | FetchKind::Call { spec, .. } => js.contexts.get(spec),
        FetchKind::VersionJs { context, .. } => Some(context),
        FetchKind::Script(_) | FetchKind::VersionRun { .. } => None,
    }
}

/// Call `exec.js`'s `method` with `args` in a context; a throw is logged.
fn call_exec<A>(context: &SpecContext, method: &str, args: A)
where
    A: for<'js> rquickjs::function::IntoArgs<'js>,
{
    context.context.with(|ctx| {
        let called = (|| -> rquickjs::Result<()> {
            let exec = context.exec.clone().restore(&ctx)?;
            let function: Function = exec.get(method)?;
            function.call::<_, ()>(args)
        })();
        if let Err(err) = called {
            let caught = CaughtError::from_error(&ctx, err);
            tracing::debug!(target: "specs", "exec.{method} threw: {caught}");
        }
    });
}

impl Host {
    /// When the loop must wake without an fd being ready.
    fn next_wake(&self, now: Instant) -> Option<Instant> {
        let js_live = self.inflight.fetches.values().any(Fetch::is_js);
        let busy = !self.js.requests.borrow().requests.is_empty()
            || (js_live && self.js.runtime.is_job_pending());
        if busy {
            return Some(now);
        }
        let deadlines = self
            .inflight
            .fetches
            .values()
            .filter_map(|fetch| fetch.deadline);
        let timer = self.inflight.next_timer.filter(|_| js_live);
        self.inflight
            .runner
            .next_wake(now)
            .into_iter()
            .chain(deadlines)
            .chain(timer)
            .min()
    }

    /// After an event: hand finished children to their promises (or
    /// fetches), run the pending jobs, drain the timers, start the commands
    /// JavaScript asked for, until nothing moves; then answer the tasks
    /// whose fetches settled.
    fn pump(&mut self, out: &mut Vec<Outcome>) {
        let started = Instant::now();
        self.inflight.runner.pump(started);
        loop {
            let finished = self.inflight.runner.take_finished();
            let mut moved = !finished.is_empty();
            for (proc, result) in finished {
                self.on_child(proc, result, out);
            }
            if self.inflight.fetches.values().any(Fetch::is_js) {
                self.js.run_jobs();
                moved |= self.drain_timers();
            }
            moved |= self.submit_execs();
            if !moved || started.elapsed() >= GEN_JS_BUDGET {
                break;
            }
            self.inflight.runner.pump(Instant::now());
        }
        self.settle_fetches(out);
    }

    fn on_child(
        &mut self,
        proc: ProcId,
        result: Result<process::Output, RunError>,
        out: &mut Vec<Outcome>,
    ) {
        if let Some((owner, request)) = self.inflight.execs.remove(&proc) {
            self.settle_exec(owner, request, result);
            return;
        }
        let Some(fetch) = self.inflight.procs.remove(&proc) else {
            return;
        };
        match self.inflight.fetches.get(&fetch).map(|fetch| &fetch.kind) {
            Some(FetchKind::VersionRun { name, .. }) => {
                let name = name.clone();
                self.end_fetch(fetch);
                // Fig keeps what `--version` printed only when semver.clean
                // reads a version in it.
                let version = result
                    .ok()
                    .and_then(|output| generators::semver_clean(&output.stdout));
                self.version_done(&name, version.map(Some), out);
            }
            Some(_) => {
                let result = result.map(Cached::Output).map_err(run_error);
                self.complete_fetch(fetch, result, out);
            }
            None => {}
        }
    }

    /// Resolve (or reject) an `executeShellCommand` promise. Fig rejects
    /// with `withTimeout`'s "Function timed out", or figterm's error for a
    /// program that did not start.
    fn settle_exec(&mut self, owner: u64, request: u64, result: Result<process::Output, RunError>) {
        let (json, error) = match result {
            Ok(output) => {
                let json = serde_json::json!({
                    "stdout": output.stdout,
                    "stderr": output.stderr,
                    "status": output.status,
                });
                (Some(json.to_string()), None)
            }
            Err(RunError::Timeout) => (None, Some("Function timed out".to_owned())),
            Err(RunError::Spawn(err)) => (None, Some(format!("failed running executable: {err}"))),
        };
        let Some(context) = self
            .inflight
            .fetches
            .get(&owner)
            .and_then(|fetch| fetch_context(fetch, &self.js))
        else {
            return;
        };
        self.js.arm(GEN_JS_BUDGET);
        call_exec(context, "settle", (request as f64, json, error));
        self.js.disarm();
    }

    /// Drain the timers of every context with a promise in flight. True when
    /// a timer ran.
    fn drain_timers(&mut self) -> bool {
        let now = Instant::now();
        let mut ran = false;
        let mut next: Option<Instant> = None;
        let mut drained: HashSet<*const SpecContext> = HashSet::new();
        for fetch in self.inflight.fetches.values() {
            let Some(context) = fetch_context(fetch, &self.js) else {
                continue;
            };
            if !drained.insert(std::ptr::from_ref(context)) {
                continue;
            }
            self.js.arm(GEN_JS_BUDGET);
            let wait = context.context.with(|ctx| {
                let drained = context
                    .timers
                    .clone()
                    .restore(&ctx)
                    .and_then(|timers| timers.call::<_, f64>(()));
                drained
                    .inspect_err(|_| {
                        ctx.catch();
                    })
                    .ok()
            });
            match wait {
                Some(0.0) => ran = true,
                Some(wait) if wait > 0.0 => {
                    // A wait too long for a Duration or an Instant wakes
                    // nothing: the fetch's deadline comes first.
                    if let Some(at) = Duration::try_from_secs_f64(wait / 1000.0)
                        .ok()
                        .and_then(|wait| now.checked_add(wait))
                    {
                        next = Some(next.map_or(at, |next| next.min(at)));
                    }
                }
                _ => {}
            }
        }
        self.js.disarm();
        self.inflight.next_timer = if ran { Some(now) } else { next };
        ran
    }

    /// Start the commands `executeShellCommand` asked for. True when there
    /// were any.
    fn submit_execs(&mut self) -> bool {
        let requests = std::mem::take(&mut self.js.requests.borrow_mut().requests);
        let any = !requests.is_empty();
        for ExecRequest { owner, id, input } in requests {
            let Some(fetch) = self.inflight.fetches.get(&owner) else {
                continue;
            };
            // executeCommandTimeout: the longer of its default and the input's.
            let timeout = generators::script_timeout(
                self.generator_timeout,
                None,
                input.timeout.map(generators::millis),
            );
            let command = command(
                input.command,
                input.args,
                input.cwd.as_deref(),
                &input.env,
                &fetch.cwd,
                &fetch.env,
                timeout,
            );
            let proc = self.inflight.runner.submit(command);
            self.inflight.execs.insert(proc, (owner, id));
        }
        any
    }

    /// Answer the JavaScript fetches whose promises settled or ran out of
    /// time.
    fn settle_fetches(&mut self, out: &mut Vec<Outcome>) {
        let now = Instant::now();
        let live: Vec<u64> = self
            .inflight
            .fetches
            .iter()
            .filter(|(_, fetch)| fetch.is_js())
            .map(|(id, _)| *id)
            .collect();
        for id in live {
            let polled = self.poll_promise(id, now);
            self.js.disarm();
            let Some(result) = polled else {
                continue;
            };
            match self.inflight.fetches.get(&id).map(|fetch| &fetch.kind) {
                Some(FetchKind::VersionJs { name, .. }) => {
                    let name = name.clone();
                    self.end_fetch(id);
                    let version = match result {
                        Ok(json) => Some(
                            serde_json::from_str::<serde_json::Value>(&json)
                                .ok()
                                .and_then(|value| value.as_str().map(str::to_owned)),
                        ),
                        Err(err) => {
                            tracing::debug!(target: "specs", "{name}'s version command failed: {err}");
                            None
                        }
                    };
                    self.version_done(&name, version, out);
                }
                Some(FetchKind::Call { .. }) => {
                    if let Some(Fetch {
                        kind: FetchKind::Call { id, .. },
                        ..
                    }) = self.end_fetch(id)
                    {
                        out.push(Outcome::Called {
                            id,
                            result: result.map_err(call_error),
                        });
                    }
                }
                Some(_) => self.complete_fetch(id, result.map(Cached::Json), out),
                None => {}
            }
        }
    }

    /// A JavaScript fetch's result as JSON (a call's functions are kept in
    /// its spec's registry; a custom generator's are not, as nothing calls
    /// them), once its promise settled or its deadline passed; None while it
    /// is pending. Leaves the interrupt deadline armed.
    fn poll_promise(&mut self, id: u64, now: Instant) -> Option<Result<String, GenError>> {
        let js_deadline = self.js.arm(GEN_JS_BUDGET);
        let fetch = self.inflight.fetches.get_mut(&id)?;
        let late = fetch.deadline.is_some_and(|deadline| now >= deadline);
        // Walked (with its functions kept for a call), or plain JSON.
        let keep = matches!(fetch.kind, FetchKind::Call { .. });
        let (promise, context, walk_it, keep) = match &mut fetch.kind {
            FetchKind::Custom { spec, promise } | FetchKind::Call { spec, promise, .. } => {
                match self.js.contexts.get_mut(spec) {
                    Some(context) => (promise.clone(), context, true, keep),
                    None => return Some(Err(GenError::Cancelled)),
                }
            }
            FetchKind::VersionJs {
                promise, context, ..
            } => (promise.clone(), context, false, false),
            FetchKind::Script(_) | FetchKind::VersionRun { .. } => return None,
        };
        let limits = Limits {
            deadline: js_deadline,
            stop: &self.js.stop,
        };
        let next = &mut self.js.next_walked;
        let SpecContext {
            registry,
            walk,
            context,
            ..
        } = context;
        context.with(|ctx| -> Option<Result<String, GenError>> {
            let fail = |err| fail_error(limits.fail(&ctx, err));
            let value = match promise.restore(&ctx) {
                Ok(value) => value,
                Err(err) => return Some(Err(fail(err))),
            };
            let value = match value.as_promise() {
                None => value,
                Some(promise) => match promise.state() {
                    PromiseState::Pending => return late.then_some(Err(GenError::Timeout)),
                    PromiseState::Resolved | PromiseState::Rejected => {
                        match promise.result::<Value>() {
                            Some(Ok(value)) => value,
                            Some(Err(err)) => return Some(Err(fail(err))),
                            None => {
                                return Some(Err(GenError::Js(
                                    "a settled promise has no result".to_owned(),
                                )));
                            }
                        }
                    }
                },
            };
            if value.is_undefined() {
                return Some(Ok("null".to_owned()));
            }
            if !walk_it {
                return Some(match ctx.json_stringify(value) {
                    Ok(json) => Ok(json
                        .and_then(|json| json.to_string().ok())
                        .unwrap_or_else(|| "null".to_owned())),
                    Err(err) => Err(fail(err)),
                });
            }
            let walked = walk.clone().restore(&ctx).and_then(|walk| {
                let registry = keep.then_some(registry);
                walk_result(&ctx, &walk, value, registry, next)
            });
            match walked {
                Ok(json) => Some(Ok(json.unwrap_or_else(|| "null".to_owned()))),
                Err(err) => Some(Err(fail(err))),
            }
        })
    }

    /// Remove a fetch, kill what it started, and forget its
    /// `executeShellCommand` requests.
    fn end_fetch(&mut self, id: u64) -> Option<Fetch> {
        let fetch = self.inflight.fetches.remove(&id)?;
        if let FetchKind::Script(proc) | FetchKind::VersionRun { proc, .. } = &fetch.kind {
            self.inflight.procs.remove(proc);
            self.inflight.runner.cancel(*proc);
        }
        let execs: Vec<ProcId> = self
            .inflight
            .execs
            .iter()
            .filter(|(_, (owner, _))| *owner == id)
            .map(|(proc, _)| *proc)
            .collect();
        for proc in execs {
            self.inflight.execs.remove(&proc);
            self.inflight.runner.cancel(proc);
        }
        if let Some(context) = fetch_context(&fetch, &self.js) {
            self.js.arm(GEN_JS_BUDGET);
            call_exec(context, "drop", (id as f64,));
            self.js.disarm();
        }
        Some(fetch)
    }

    /// A fetch gave its result: fill its cache entry and answer its tasks.
    fn complete_fetch(
        &mut self,
        id: u64,
        result: Result<Cached, GenError>,
        out: &mut Vec<Outcome>,
    ) {
        let Some(fetch) = self.end_fetch(id) else {
            return;
        };
        let Fetch { key, waiters, .. } = fetch;
        if let Some(key) = &key {
            self.inflight
                .cache
                .finished(key, id, result.as_ref().ok().cloned());
        }
        if let Err(err) = &result {
            tracing::debug!(target: "specs", "a generator failed: {err}");
        }
        for task_id in waiters {
            let Some(task) = self.inflight.tasks.remove(&task_id) else {
                continue;
            };
            let result = match &result {
                Ok(value) => self.finish(&task, value),
                Err(err) => Err(err.clone()),
            };
            out.push(Outcome::Generated {
                id: task_id,
                result,
                stale: false,
            });
        }
    }

    /// Stop a fetch: its tasks are answered `Cancelled`, a call `Stopped`.
    fn abandon_fetch(&mut self, id: u64, out: &mut Vec<Outcome>) {
        let Some(fetch) = self.end_fetch(id) else {
            return;
        };
        if let FetchKind::Call { id, .. } = fetch.kind {
            return out.push(Outcome::Called {
                id,
                result: Err(CallError::Stopped),
            });
        }
        if let Some(key) = &fetch.key {
            self.inflight.cache.finished(key, id, None);
        }
        for task_id in fetch.waiters {
            if self.inflight.tasks.remove(&task_id).is_some() {
                out.push(Outcome::Generated {
                    id: task_id,
                    result: Err(GenError::Cancelled),
                    stale: false,
                });
            }
        }
    }

    /// `Cancel`: drop the task, and kill its fetch when no other task waits
    /// for it. No outcome.
    fn cancel_task(&mut self, id: u64) {
        if self.inflight.tasks.remove(&id).is_none() {
            return;
        }
        let mut orphans = Vec::new();
        for (fetch_id, fetch) in &mut self.inflight.fetches {
            let before = fetch.waiters.len();
            fetch.waiters.retain(|waiter| *waiter != id);
            if fetch.waiters.len() < before && fetch.waiters.is_empty() {
                orphans.push(*fetch_id);
            }
        }
        let mut none = Vec::new();
        for fetch in orphans {
            self.abandon_fetch(fetch, &mut none);
        }
    }

    /// A spec is dropped or reloaded: its tasks are answered `Cancelled`, and
    /// what runs for it (or in its context) is stopped.
    fn cancel_spec(&mut self, name: &str, out: &mut Vec<Outcome>) {
        let tasks: Vec<u64> = self
            .inflight
            .tasks
            .iter()
            .filter(|(_, task)| task.spec == name)
            .map(|(id, _)| *id)
            .collect();
        for id in &tasks {
            self.inflight.tasks.remove(id);
            out.push(Outcome::Generated {
                id: *id,
                result: Err(GenError::Cancelled),
                stale: false,
            });
        }
        let doomed: Vec<u64> = self
            .inflight
            .fetches
            .iter_mut()
            .filter_map(|(id, fetch)| {
                let before = fetch.waiters.len();
                fetch.waiters.retain(|waiter| !tasks.contains(waiter));
                let orphaned = fetch.waiters.len() < before && fetch.waiters.is_empty();
                let in_context = matches!(
                    &fetch.kind,
                    FetchKind::Custom { spec, .. } | FetchKind::Call { spec, .. } if spec == name
                );
                (orphaned || in_context).then_some(*id)
            })
            .collect();
        for id in doomed {
            self.abandon_fetch(id, out);
        }
    }

    /// The generator a job names, and its arg's `isDangerous`.
    fn find_generator(&self, request: &Request) -> Result<(Generator, bool), GenError> {
        if let Some(given) = &request.generator {
            return Ok((**given).clone());
        }
        let spec = self
            .specs
            .get(&request.spec)
            .ok_or_else(|| GenError::Js(format!("{} is not loaded", request.spec)))?;
        let prefix = format!("{}:", request.spec);
        let path = request
            .arg_path
            .strip_prefix(&prefix)
            .unwrap_or(&request.arg_path);
        let arg = generators::arg_at(&spec.root, path)
            .ok_or_else(|| GenError::Js(format!("no arg at {path}")))?;
        let generator = arg.generators.get(request.generator_index).ok_or_else(|| {
            GenError::Js(format!(
                "no generator {} at {path}",
                request.generator_index
            ))
        })?;
        Ok((generator.clone(), arg.is_dangerous))
    }

    /// `Generate`, as Fig's `triggerGenerator` (`state/generators.ts:83-116`)
    /// and the generators it calls.
    fn generate(&mut self, request: Request, out: &mut Vec<Outcome>) {
        let id = request.id;
        let answer = |out: &mut Vec<Outcome>, result, stale| {
            out.push(Outcome::Generated { id, result, stale });
        };
        let (generator, is_dangerous) = match self.find_generator(&request) {
            Ok(found) => found,
            Err(err) => {
                tracing::debug!(target: "specs", "generate {id}: {err}");
                return answer(out, Err(err), false);
            }
        };
        // A template first, then a script, else custom. Without a cwd Fig
        // has no context for a generator (`helpers.ts:25-26`).
        let start = if !generator.template.is_empty() || !generators::have_context(&request.cwd) {
            None
        } else if let Some(script) = &generator.script {
            match self.script_input(&request, script) {
                Ok(input) => input.map(Start::Script),
                Err(err) => {
                    tracing::debug!(target: "specs", "generate {id}: {err}");
                    return answer(out, Err(err), false);
                }
            }
        } else {
            generator.custom.map(Start::Custom)
        };
        let Some(start) = start else {
            return answer(out, Ok(Vec::new()), false);
        };
        let cwd = request.cwd.to_string_lossy().into_owned();
        let key = generator.cache.as_ref().map(|cache| {
            // A script's key is `cache.cacheKey ?? JSON.stringify(input)`
            // (`scriptSuggestionsGenerator.ts:63`); a custom generator's,
            // `cache.cacheKey` (`customSuggestionsGenerator.ts:48`).
            let given = match &start {
                Start::Script(input) => {
                    Some(cache.cache_key.clone().unwrap_or_else(|| input.key.clone()))
                }
                Start::Custom(_) => cache.cache_key.clone(),
            };
            let key = generators::cache_key(
                cache,
                !generator.template.is_empty(),
                &cwd,
                &request.search_term,
                given.as_deref(),
                &request.tokens,
            );
            (key, cache.clone())
        });
        let task = Task {
            spec: request.spec.clone(),
            generator,
            is_dangerous,
            tokens: request.tokens.clone(),
        };
        let now = Instant::now();
        let plan = match &key {
            Some((key, cache)) => self.inflight.cache.plan(key, cache, now),
            None => Plan::Run,
        };
        let (stale, join) = match plan {
            Plan::Hit(value) => {
                let result = self.finish(&task, &value);
                return answer(out, result, false);
            }
            Plan::Run => (None, None),
            Plan::Join(fetch) => (None, Some(fetch)),
            Plan::StaleRun(value) => (Some(value), None),
            Plan::StaleJoin(value, fetch) => (Some(value), Some(fetch)),
        };
        if let Some(value) = stale {
            let result = self.finish(&task, &value);
            answer(out, result, true);
        }
        let fetch = match join.filter(|fetch| self.inflight.fetches.contains_key(fetch)) {
            Some(fetch) => fetch,
            None => match self.start_fetch(&request, &task, start) {
                Ok(fetch) => {
                    if let Some((key, _)) = key {
                        self.inflight.cache.started(&key, fetch, now);
                        if let Some(started) = self.inflight.fetches.get_mut(&fetch) {
                            started.key = Some(key);
                        }
                    }
                    fetch
                }
                Err(err) => {
                    tracing::debug!(target: "specs", "generate {id}: {err}");
                    return answer(out, Err(err), false);
                }
            },
        };
        if let Some(fetch) = self.inflight.fetches.get_mut(&fetch) {
            fetch.waiters.push(id);
        }
        self.inflight.tasks.insert(id, task);
    }

    /// What a script generator runs (`scriptSuggestionsGenerator.ts:26-50`):
    /// `None` when its function returns nothing to run.
    fn script_input(
        &mut self,
        request: &Request,
        script: &Script,
    ) -> Result<Option<ScriptInput>, GenError> {
        let cwd = request.cwd.to_string_lossy();
        match script {
            Script::Argv(argv) => ScriptInput::from_argv(argv, &cwd)
                .map(Some)
                .ok_or_else(|| GenError::Spawn("an empty command".to_owned())),
            Script::Command(input) => Ok(Some(ScriptInput::from_object(input, &cwd))),
            Script::Fn(fn_id) => {
                // Called as Fig calls it, without awaiting what it returns.
                let args = serde_json::json!([request.tokens]).to_string();
                let json = self
                    .call(&request.spec, fn_id.0, &args, GEN_JS_BUDGET, false)
                    .map_err(gen_error)?;
                ScriptInput::from_returned(&parse_json(&json)?, &cwd)
            }
        }
    }

    fn start_fetch(
        &mut self,
        request: &Request,
        task: &Task,
        start: Start,
    ) -> Result<u64, GenError> {
        let id = self.inflight.next_fetch;
        self.inflight.next_fetch += 1;
        let kind = match start {
            Start::Script(input) => {
                let timeout = generators::script_timeout(
                    self.generator_timeout,
                    task.generator.script_timeout,
                    input.timeout,
                );
                let command = command(
                    input.command,
                    input.args,
                    Some(&input.cwd),
                    &input.env,
                    &request.cwd,
                    &request.env,
                    timeout,
                );
                let proc = self.inflight.runner.submit(command);
                self.inflight.procs.insert(proc, id);
                FetchKind::Script(proc)
            }
            Start::Custom(fn_id) => FetchKind::Custom {
                spec: request.spec.clone(),
                promise: self.call_custom(id, request, task, fn_id)?,
            },
        };
        let deadline = matches!(kind, FetchKind::Custom { .. })
            .then(|| Instant::now() + self.generator_timeout);
        self.inflight.fetches.insert(
            id,
            Fetch {
                kind,
                key: None,
                waiters: Vec::new(),
                cwd: request.cwd.clone(),
                env: Arc::clone(&request.env),
                deadline,
            },
        );
        Ok(id)
    }

    /// `custom(tokens, executeShellCommand, context)`
    /// (`customSuggestionsGenerator.ts:24-47`): what it returned, usually a
    /// promise.
    fn call_custom(
        &mut self,
        fetch: u64,
        request: &Request,
        task: &Task,
        fn_id: FnRef,
    ) -> Result<Persistent<Value<'static>>, GenError> {
        let deadline = self.js.arm(GEN_JS_BUDGET);
        let limits = Limits {
            deadline,
            stop: &self.js.stop,
        };
        let spec = self
            .js
            .contexts
            .get(&request.spec)
            .ok_or_else(|| GenError::Js(CallError::NotLoaded.to_string()))?;
        let (function, owner) = spec
            .registry
            .get(fn_id.0)
            .ok_or_else(|| GenError::Js(CallError::UnknownFunction(fn_id.0).to_string()))?;
        let variables: BTreeMap<&str, &str> = request
            .env
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect();
        let context = serde_json::json!({
            "currentWorkingDirectory": request.cwd.to_string_lossy(),
            "currentProcess": request.tokens.first().map_or(request.spec.as_str(), String::as_str),
            "sshPrefix": "",
            "searchTerm": request.search_term,
            "environmentVariables": variables,
            "isDangerous": task.is_dangerous,
        })
        .to_string();
        spec.context.with(|ctx| {
            let fail = |err| fail_error(limits.fail(&ctx, err));
            let function = function.restore(&ctx).map_err(fail)?;
            let owner = owner.restore(&ctx).map_err(fail)?;
            let exec_host = spec.exec.clone().restore(&ctx).map_err(fail)?;
            let make: Function = exec_host.get("make").map_err(fail)?;
            let exec: Function = make.call((fetch as f64,)).map_err(fail)?;
            let context = ctx.json_parse(context).map_err(fail)?;
            let mut args = Args::new(ctx.clone(), 3);
            args.this(owner).map_err(fail)?;
            args.push_arg(request.tokens.clone()).map_err(fail)?;
            args.push_arg(exec).map_err(fail)?;
            args.push_arg(context).map_err(fail)?;
            let value: Value = args.apply(&function).map_err(fail)?;
            Ok(Persistent::save(&ctx, value))
        })
    }

    /// A fetched value as the task's suggestions.
    fn finish(&mut self, task: &Task, value: &Cached) -> Result<Vec<Suggestion>, GenError> {
        let result = match value {
            Cached::Output(output) => self.finish_script(task, &output.stdout),
            Cached::Json(json) => self.finish_custom(task, json),
        };
        self.js.disarm();
        result
    }

    /// `splitOn` wins over `postProcess`, and with neither there is nothing
    /// (`scriptSuggestionsGenerator.ts:66-87`).
    fn finish_script(&mut self, task: &Task, stdout: &str) -> Result<Vec<Suggestion>, GenError> {
        let generator = &task.generator;
        if let Some(separator) = generator
            .split_on
            .as_deref()
            .filter(|split| !split.is_empty())
        {
            return Ok(generators::split_on(stdout, separator, task.is_dangerous));
        }
        let Some(post_process) = generator.post_process else {
            return Ok(Vec::new());
        };
        // Called as Fig calls it, without awaiting what it returns.
        let args = serde_json::json!([stdout, task.tokens]).to_string();
        let json = self
            .call(&task.spec, post_process.0, &args, GEN_JS_BUDGET, false)
            .map_err(gen_error)?;
        generators::post_processed(parse_json(&json)?, task.is_dangerous)
    }

    /// A custom generator's result, mapped as Fig maps it, then through
    /// `filterTemplateSuggestions` when its first item is a template
    /// suggestion (`state/generators.ts:96-113`).
    fn finish_custom(&mut self, task: &Task, json: &str) -> Result<Vec<Suggestion>, GenError> {
        let items = generators::custom_items(parse_json(json)?)?;
        let Some(filter) = task.generator.filter_template_suggestions.filter(|_| {
            items
                .first()
                .is_some_and(generators::is_template_suggestion)
        }) else {
            return Ok(generators::suggestions(items));
        };
        let args = serde_json::json!([items]).to_string();
        let json = self
            .call(&task.spec, filter.0, &args, GEN_JS_BUDGET, true)
            .map_err(gen_error)?;
        match parse_json(&json)? {
            serde_json::Value::Array(items) => Ok(generators::suggestions(items)),
            _ => Err(GenError::Js(
                "filterTemplateSuggestions did not return an array".to_owned(),
            )),
        }
    }

    /// `FilterTemplate`: the template suggestions as Fig's code sees them,
    /// through the spec's `filterTemplateSuggestions`
    /// (`templateSuggestionsGenerator.ts:84-86`).
    fn filter_template(
        &mut self,
        spec: &str,
        fn_id: u32,
        suggestions: &[Suggestion],
    ) -> Result<Vec<Suggestion>, GenError> {
        let items: Vec<serde_json::Value> =
            suggestions.iter().map(generators::fig_suggestion).collect();
        let args = serde_json::json!([items]).to_string();
        let json = self
            .call(spec, fn_id, &args, GEN_JS_BUDGET, true)
            .map_err(gen_error)?;
        match parse_json(&json)? {
            serde_json::Value::Array(items) => Ok(generators::suggestions(items)),
            _ => Err(GenError::Js(
                "filterTemplateSuggestions did not return an array".to_owned(),
            )),
        }
    }

    /// The first cwd and environment seen are where version commands run.
    fn note_context(&mut self, cwd: PathBuf, env: Env) {
        if self.versions.context.is_some() {
            return;
        }
        self.versions.context = Some((cwd.clone(), Arc::clone(&env)));
        for name in std::mem::take(&mut self.versions.waiting) {
            self.start_version(&name, cwd.clone(), Arc::clone(&env));
        }
    }

    /// Ask a versioned spec's CLI for its version, once per session.
    fn want_version(&mut self, name: &str) {
        if self.versions.known.contains_key(name) || self.versions.running.contains(name) {
            return;
        }
        match self.versions.context.clone() {
            Some((cwd, env)) => self.start_version(name, cwd, env),
            None => {
                if !self.versions.waiting.iter().any(|waiting| waiting == name) {
                    self.versions.waiting.push(name.to_owned());
                }
            }
        }
    }

    /// `getVersionFromFullFile` (`loadHelpers.ts:154-190`): the index
    /// module's `getVersionCommand(executeShellCommand)` when it exports one,
    /// else `<cmd> --version`.
    fn start_version(&mut self, name: &str, cwd: PathBuf, env: Env) {
        let source = match self.source(name) {
            Ok(source) => source,
            Err(err) => {
                tracing::debug!(target: "specs", "{name} has no index module: {err}");
                return;
            }
        };
        let id = self.inflight.next_fetch;
        self.inflight.next_fetch += 1;
        let deadline = self.js.arm(self.eval_budget);
        let limits = self.js.limits(deadline);
        let started = self.js.fresh(name, &limits).and_then(|index| {
            let promise = index.context.with(|ctx| -> std::result::Result<_, Fail> {
                let fail = |err| limits.fail(&ctx, err);
                let timers = index.timers.clone().restore(&ctx).map_err(fail)?;
                let namespace = evaluate_module(&ctx, name, &source, &timers, &limits)?;
                let get: Value = namespace.get("getVersionCommand").map_err(fail)?;
                let Some(get) = get.as_function() else {
                    return Ok(None);
                };
                let exec_host = index.exec.clone().restore(&ctx).map_err(fail)?;
                let make: Function = exec_host.get("make").map_err(fail)?;
                let exec: Function = make.call((id as f64,)).map_err(fail)?;
                let value: Value = get.call((exec,)).map_err(fail)?;
                Ok(Some(Persistent::save(&ctx, value)))
            })?;
            Ok((index, promise))
        });
        self.js.disarm();
        let kind = match started {
            Ok((context, Some(promise))) => FetchKind::VersionJs {
                name: name.to_owned(),
                promise,
                context,
            },
            Ok((index, None)) => {
                drop(index);
                let proc = self.inflight.runner.submit(process::Command {
                    program: name.to_owned(),
                    args: vec!["--version".to_owned()],
                    cwd: cwd.clone(),
                    env: env.to_vec(),
                    timeout: self.generator_timeout,
                });
                self.inflight.procs.insert(proc, id);
                FetchKind::VersionRun {
                    name: name.to_owned(),
                    proc,
                }
            }
            Err(fail) => {
                // Fig catches it and goes on without a version.
                tracing::debug!(target: "specs", "{name}'s version command failed: {}", fail_error(fail));
                return;
            }
        };
        let deadline = matches!(kind, FetchKind::VersionJs { .. })
            .then(|| Instant::now() + self.generator_timeout);
        self.versions.running.insert(name.to_owned());
        self.inflight.fetches.insert(
            id,
            Fetch {
                kind,
                key: None,
                waiters: Vec::new(),
                cwd,
                env,
                deadline,
            },
        );
    }

    /// A version command ended: `None` when it failed (it is asked again on
    /// a later load), else what it gave, kept for the session. A version
    /// other than the one assumed reloads the spec for it.
    fn version_done(
        &mut self,
        name: &str,
        version: Option<Option<String>>,
        out: &mut Vec<Outcome>,
    ) {
        self.versions.running.remove(name);
        let Some(version) = version else {
            return;
        };
        self.versions.known.insert(name.to_owned(), version.clone());
        let Some(version) = version else {
            return;
        };
        let assumed = match self.specs.get(name).and_then(|spec| spec.version.as_ref()) {
            Some(SpecVersion::Assumed(assumed)) => assumed.clone(),
            _ => return,
        };
        if assumed == version {
            return;
        }
        let started = Instant::now();
        self.cancel_spec(name, out);
        match self.load_detected(name, &version, started) {
            Ok(spec) => {
                self.specs.insert(name.to_owned(), Arc::clone(&spec));
                out.push(Outcome::Loaded {
                    name: name.to_owned(),
                    result: Ok(spec),
                });
            }
            Err(err) => {
                tracing::debug!(target: "specs", "{name} for version {version}: {err}; the newest stays");
            }
        }
        self.js.disarm();
    }
}

/// The keys of a walked tree's JSON that the model does not know.
fn unknown_keys(json: &str) -> Vec<String> {
    let Ok(original) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    let back = Subcommand::deserialize(&original)
        .ok()
        .and_then(|root| serde_json::to_value(root).ok());
    back.map(|back| user::unknown_keys(&original, &back))
        .unwrap_or_default()
}

fn check_fns(found: &[String], expected: &[String]) -> Result<(), LoadError> {
    if found == expected {
        Ok(())
    } else {
        Err(LoadError::FnMismatch {
            expected: expected.len(),
            found: found.len(),
        })
    }
}

fn finish(
    name: &str,
    root: Subcommand,
    fns: Vec<String>,
    has_js: bool,
    version: Option<SpecVersion>,
    started: Instant,
) -> Arc<LoadedSpec> {
    Arc::new(LoadedSpec {
        name: name.to_owned(),
        root,
        fns,
        has_js,
        version,
        load_time: started.elapsed(),
        generation: GENERATION.fetch_add(1, Ordering::Relaxed),
    })
}

/// The newest version file of a versioned spec, by semver.
fn newest_version_file(archive: &Archive, name: &str) -> Option<String> {
    let prefix = format!("{name}/");
    archive
        .names()
        .filter_map(|entry| Some((entry, entry.strip_prefix(&prefix)?)))
        .filter(|(entry, version)| {
            !version.contains('/') && archive.kind(entry) == Some(Kind::VersionFile)
        })
        .max_by(|(_, a), (_, b)| compare_versions(a, b))
        .map(|(entry, _)| entry.to_owned())
}

/// semver order on the numeric parts (a missing part is 0); a prerelease
/// comes before its release. As `compareVersions` in `versions.js`.
fn compare_versions(a: &str, b: &str) -> CmpOrdering {
    fn parse(version: &str) -> (Vec<u64>, &str) {
        let (core, pre) = version.split_once('-').unwrap_or((version, ""));
        let parts = core
            .split('.')
            .map(|part| {
                let digits: String = part.chars().take_while(char::is_ascii_digit).collect();
                digits.parse().unwrap_or(0)
            })
            .collect();
        (parts, pre)
    }
    let ((x, x_pre), (y, y_pre)) = (parse(a), parse(b));
    let length = x.len().max(y.len());
    let part = |parts: &[u64], i: usize| parts.get(i).copied().unwrap_or(0);
    (0..length)
        .map(|i| part(&x, i).cmp(&part(&y, i)))
        .find(|order| order.is_ne())
        .unwrap_or_else(|| match (x_pre.is_empty(), y_pre.is_empty()) {
            (true, true) => CmpOrdering::Equal,
            (true, false) => CmpOrdering::Greater,
            (false, true) => CmpOrdering::Less,
            (false, false) => x_pre.cmp(y_pre),
        })
}

#[cfg(test)]
fn blocked_signals() -> Vec<i32> {
    // SAFETY: an all-zero sigset_t is a valid (empty) set to read into.
    let mut old: libc::sigset_t = unsafe { std::mem::zeroed() };
    // SAFETY: with a null new set, pthread_sigmask only writes this
    // thread's mask into `old`, which lives on this stack.
    let rc = unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, std::ptr::null(), &mut old) };
    if rc != 0 {
        return Vec::new();
    }
    (1..32)
        // SAFETY: `old` is an initialised set and `signal` is in range.
        .filter(|&signal| unsafe { libc::sigismember(&old, signal) } == 1)
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::specs::model::{Script, StringOrFn, Subcommand};

    /// How long a test waits for one outcome.
    const WAIT: Duration = Duration::from_secs(60);

    /// The evaluation budget of the tests that do not test it.
    pub(crate) const TEST_BUDGET: Duration = Duration::from_secs(10);

    /// Wait until the result pipe is readable, at most `WAIT`.
    pub(crate) fn wait_readable(fd: RawFd) {
        // SAFETY: the caller's SpecThread owns `fd` and outlives this call.
        let fd = unsafe { std::os::fd::BorrowedFd::borrow_raw(fd) };
        let mut fds = [PollFd::new(fd, PollFlags::POLLIN)];
        let millis = u16::try_from(WAIT.as_millis()).unwrap_or(u16::MAX);
        let _ = poll(&mut fds, PollTimeout::from(millis));
    }

    /// A spec thread and the outcomes taken from it but not looked at yet.
    pub(crate) struct Probe {
        pub(crate) thread: SpecThread,
        pending: VecDeque<Outcome>,
        /// The last call id sent.
        calls: u64,
        /// The load of each spec loaded through it.
        generations: HashMap<String, u64>,
    }

    impl Probe {
        pub(crate) fn new() -> Self {
            Self::with_home(None)
        }

        pub(crate) fn with_home(home: Option<PathBuf>) -> Self {
            Self::with_budget(home, TEST_BUDGET)
        }

        pub(crate) fn with_budget(home: Option<PathBuf>, eval_budget: Duration) -> Self {
            Self {
                thread: SpecThread::start_with_budget(home, eval_budget).expect("spec thread"),
                pending: VecDeque::new(),
                calls: 0,
                generations: HashMap::new(),
            }
        }

        /// With `generator_timeout` standing in for the setting.
        pub(crate) fn with_timeouts(home: Option<PathBuf>, generator_timeout: Duration) -> Self {
            Self {
                thread: SpecThread::start_with_timeouts(home, TEST_BUDGET, generator_timeout)
                    .expect("spec thread"),
                pending: VecDeque::new(),
                calls: 0,
                generations: HashMap::new(),
            }
        }

        pub(crate) fn next(&mut self) -> Outcome {
            let deadline = Instant::now() + WAIT;
            loop {
                if let Some(outcome) = self.pending.pop_front() {
                    return outcome;
                }
                assert!(Instant::now() < deadline, "no outcome within {WAIT:?}");
                wait_readable(self.thread.result_fd());
                self.pending.extend(self.thread.take());
            }
        }

        pub(crate) fn load(&mut self, name: &str) -> Result<Arc<LoadedSpec>, LoadError> {
            self.thread.send(Job::Load {
                name: name.to_owned(),
                cwd: None,
                env: None,
            });
            let result = match self.next() {
                Outcome::Loaded {
                    name: loaded,
                    result,
                } if loaded == name => result,
                other => panic!("{name}: {other:?}"),
            };
            if let Ok(spec) = &result {
                self.generations.insert(name.to_owned(), spec.generation);
            }
            result
        }

        /// The load of `name` last loaded through the probe.
        pub(crate) fn generation(&self, name: &str) -> u64 {
            self.generations.get(name).copied().unwrap_or_default()
        }

        pub(crate) fn loaded(&mut self, name: &str) -> Arc<LoadedSpec> {
            self.load(name)
                .unwrap_or_else(|err| panic!("{name}: {err}"))
        }

        fn call(
            &mut self,
            spec: &str,
            fn_id: u32,
            args: &str,
            budget: Duration,
        ) -> Result<String, CallError> {
            self.calls += 1;
            let id = self.calls;
            self.thread.send(Job::Call {
                id,
                spec: spec.to_owned(),
                generation: self.generation(spec),
                fn_id,
                args_json: args.to_owned(),
                cwd: None,
                env: None,
                with_exec: false,
                budget,
            });
            match self.next() {
                Outcome::Called {
                    id: answered,
                    result,
                    ..
                } if answered == id => result,
                other => panic!("{spec}: {other:?}"),
            }
        }

        pub(crate) fn memory(&mut self) -> i64 {
            self.thread.send(Job::MemoryUsage);
            match self.next() {
                Outcome::MemoryUsage(bytes) => bytes,
                other => panic!("{other:?}"),
            }
        }
    }

    /// Every spec in the archive with module source, as kind `spec`.
    pub(crate) fn specs_with_functions() -> Vec<&'static str> {
        let archive = Archive::embedded();
        archive
            .names()
            .filter(|name| archive.kind(name) == Some(Kind::Spec) && archive.source(name).is_some())
            .collect()
    }

    fn fn_id(spec: &LoadedSpec, path: &str) -> u32 {
        let at = spec
            .fns
            .iter()
            .position(|known| known == path)
            .unwrap_or_else(|| panic!("no function at {path}: {:?}", spec.fns));
        u32::try_from(at).expect("id")
    }

    fn json(text: &str) -> serde_json::Value {
        serde_json::from_str(text).unwrap_or_else(|err| panic!("{text}: {err}"))
    }

    fn names(subcommands: &[Subcommand]) -> Vec<&str> {
        subcommands
            .iter()
            .map(|sub| sub.name.first().map_or("", String::as_str))
            .collect()
    }

    /// A temp HOME with these user specs in it.
    fn home_with(tag: &str, specs: &[(&str, &str)]) -> PathBuf {
        let files: Vec<(String, &str)> = specs
            .iter()
            .map(|(name, source)| (format!("{name}.js"), *source))
            .collect();
        let files: Vec<(&str, &str)> = files
            .iter()
            .map(|(file, source)| (file.as_str(), *source))
            .collect();
        home_files(tag, &files)
    }

    /// A temp HOME with these files in its user spec folder.
    fn home_files(tag: &str, files: &[(&str, &str)]) -> PathBuf {
        let home = std::env::temp_dir().join(format!("fac-specs-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join(".config/fac/specs")).expect("mkdir");
        for (file, source) in files {
            write_user(&home, file, source);
        }
        home
    }

    fn write_user(home: &Path, file: &str, source: &str) {
        let path = home.join(".config/fac/specs").join(file);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).expect("mkdir");
        }
        std::fs::write(&path, source).expect("write user spec");
    }

    /// The user spec error a load failed with.
    fn user_error(result: Result<Arc<LoadedSpec>, LoadError>) -> SpecError {
        match result {
            Err(LoadError::User(err)) => err,
            other => panic!("not a user spec error: {other:?}"),
        }
    }

    #[test]
    fn a_sample_loads() {
        let mut probe = Probe::new();
        let archive = Archive::embedded();
        let mut sample: Vec<&str> = vec![
            "git",
            "ls",
            "cd",
            "sudo",
            "clang",
            "az",
            "heroku",
            "aws/ec2",
            "gcloud/compute",
        ];
        sample.extend(
            specs_with_functions()
                .into_iter()
                .filter(|name| name.starts_with('g')),
        );
        for name in sample {
            let spec = probe.loaded(name);
            assert_eq!(spec.name, name);
            match archive.kind(name) {
                Some(Kind::Spec) => {
                    let entry = archive.entry(name).and_then(Result::ok).expect("entry");
                    assert_eq!(spec.has_js, entry.has_functions(), "{name}");
                    assert_eq!(spec.fns, entry.fns(), "{name}");
                    assert!(spec.version.is_none());
                }
                Some(Kind::Versioned) => {
                    assert!(
                        matches!(spec.version, Some(SpecVersion::Assumed(_))),
                        "{name}: {:?}",
                        spec.version
                    );
                    assert!(!spec.root.subcommands.is_empty(), "{name}");
                }
                other => panic!("{name}: {other:?}"),
            }
        }
    }

    #[test]
    #[ignore = "loads all ~1400 entries; run with --ignored"]
    fn every_name_loads() {
        // The real budget: run it in release.
        let mut probe = Probe::with_budget(None, EVAL_BUDGET);
        let started = Instant::now();
        let mut slowest: Vec<(Duration, &str)> = Vec::new();
        for name in Archive::embedded().names() {
            let spec = probe.loaded(name);
            slowest.push((spec.load_time, name));
            probe.thread.send(Job::Drop {
                name: name.to_owned(),
            });
        }
        slowest.sort_by(|a, b| b.cmp(a));
        eprintln!(
            "every_name_loads: {} entries in {:?}; slowest {:?}",
            slowest.len(),
            started.elapsed(),
            &slowest[..slowest.len().min(8)]
        );
    }

    #[test]
    fn versioned_specs_resolve_to_the_newest_file() {
        let archive = Archive::embedded();
        assert_eq!(
            newest_version_file(archive, "heroku").as_deref(),
            Some("heroku/8.6.0")
        );
        assert_eq!(
            newest_version_file(archive, "shopify").as_deref(),
            Some("shopify/3.0.0")
        );
        // semver, not text: 0.10.0 is newer than 0.9.0.
        assert_eq!(
            newest_version_file(archive, "infracost").as_deref(),
            Some("infracost/0.10.0")
        );
        assert_eq!(newest_version_file(archive, "git"), None);
        assert_eq!(compare_versions("1.2.3", "1.2.3"), CmpOrdering::Equal);
        assert_eq!(compare_versions("1.2", "1.2.0"), CmpOrdering::Equal);
        assert_eq!(compare_versions("1.10.0", "1.9.9"), CmpOrdering::Greater);
        assert_eq!(compare_versions("2.0.0-beta", "2.0.0"), CmpOrdering::Less);
        assert_eq!(
            compare_versions("2.0.0-alpha", "2.0.0-beta"),
            CmpOrdering::Less
        );
    }

    #[test]
    fn a_pure_git_post_process_is_called() {
        let mut probe = Probe::new();
        let git = probe.loaded("git");
        assert!(git.has_js);
        let tags = [
            "git",
            "--no-optional-locks",
            "tag",
            "--list",
            "--sort=-committerdate",
        ]
        .map(str::to_owned)
        .to_vec();
        let post_process =
            find_generator(&git.root, &Script::Argv(tags)).expect("the tags generator");
        let out = probe
            .call(
                "git",
                post_process,
                r#"["v1.0\nv2.0"]"#,
                Duration::from_millis(500),
            )
            .expect("call");
        let out = json(&out);
        let names: Vec<&str> = out
            .as_array()
            .expect("an array")
            .iter()
            .filter_map(|item| item.get("name")?.as_str())
            .collect();
        assert_eq!(names, ["v1.0", "v2.0"]);
        assert_eq!(
            probe.call("git", u32::MAX, "[]", Duration::from_millis(100)),
            Err(CallError::UnknownFunction(u32::MAX))
        );
        assert_eq!(
            probe.call("git", post_process, "{}", Duration::from_millis(100)),
            Err(CallError::BadArgs)
        );
        assert_eq!(
            probe.call("ls", 0, "[]", Duration::from_millis(100)),
            Err(CallError::NotLoaded)
        );
    }

    /// The post-process id of the generator running `script`, anywhere in
    /// the tree.
    fn find_generator(sub: &Subcommand, script: &Script) -> Option<u32> {
        let args = sub
            .args
            .iter()
            .chain(sub.options.iter().flat_map(|option| &option.args));
        for arg in args {
            for generator in &arg.generators {
                if generator.script.as_ref() == Some(script) {
                    return generator.post_process.map(|id| id.0);
                }
            }
        }
        sub.subcommands
            .iter()
            .find_map(|sub| find_generator(sub, script))
    }

    /// A temp git repository with an alias, its own HOME, and the
    /// environment to run git there.
    fn repo(tag: &str) -> (PathBuf, Env) {
        let root = std::env::temp_dir().join(format!("fac-call-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (repo, home) = (root.join("repo"), root.join("home"));
        std::fs::create_dir_all(&home).expect("mkdir");
        let env = Arc::new(vec![
            (
                "PATH".to_owned(),
                "/usr/bin:/bin:/usr/sbin:/sbin".to_owned(),
            ),
            ("HOME".to_owned(), home.display().to_string()),
        ]);
        let git = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .args(args)
                .env_clear()
                .envs(env.iter().map(|(key, value)| (key, value)))
                .status()
                .expect("git");
            assert!(status.success(), "git {args:?}");
        };
        git(&["init", "-q", &repo.display().to_string()]);
        git(&[
            "-C",
            &repo.display().to_string(),
            "config",
            "alias.co",
            "checkout -q",
        ]);
        (root, env)
    }

    /// The next `Called` outcomes, by id, until `ids` have answered.
    fn called(probe: &mut Probe, ids: &[u64]) -> HashMap<u64, Result<String, CallError>> {
        let mut answers = HashMap::new();
        while ids.iter().any(|id| !answers.contains_key(id)) {
            match probe.next() {
                Outcome::Called { id, result, .. } => {
                    answers.insert(id, result);
                }
                other => panic!("{other:?}"),
            }
        }
        answers
    }

    #[test]
    fn an_async_call_runs_git_help_in_a_repo() {
        use std::os::unix::fs::PermissionsExt as _;
        let mut probe = Probe::new();
        let git = probe.loaded("git");
        let generate = git.root.generate_spec.expect("git's generateSpec").0;
        let (root, env) = repo("help");
        probe.thread.send(Job::Call {
            id: 7,
            spec: "git".to_owned(),
            generation: probe.generation("git"),
            fn_id: generate,
            args_json: r#"[["git", "ch"]]"#.to_owned(),
            cwd: Some(root.join("repo")),
            env: Some(Arc::clone(&env)),
            with_exec: true,
            budget: Duration::from_secs(10),
        });
        let answer = called(&mut probe, &[7]).remove(&7).expect("answered");
        let spec = json(&answer.expect("generateSpec"));
        assert_eq!(spec["name"], "git");
        // No git-* commands on this PATH, so `git help -a` has no "External
        // commands" header: none, not the lines from the top ("See 'git
        // help <command>'…"), as fac's override of git.js makes it.
        assert_eq!(spec["subcommands"], serde_json::json!([]), "{spec}");
        // With one on PATH, it is offered.
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).expect("mkdir");
        let external = bin.join("git-faccmd");
        std::fs::write(&external, "#!/bin/sh\n").expect("write");
        std::fs::set_permissions(&external, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let with_external = Arc::new(
            env.iter()
                .map(|(key, value)| match key.as_str() {
                    "PATH" => (key.clone(), format!("{}:{value}", bin.display())),
                    _ => (key.clone(), value.clone()),
                })
                .collect::<Vec<_>>(),
        );
        probe.thread.send(Job::Call {
            id: 9,
            spec: "git".to_owned(),
            generation: probe.generation("git"),
            fn_id: generate,
            args_json: r#"[["git", "ch"]]"#.to_owned(),
            cwd: Some(root.join("repo")),
            env: Some(with_external),
            with_exec: true,
            budget: Duration::from_secs(10),
        });
        let answer = called(&mut probe, &[9]).remove(&9).expect("answered");
        let spec = json(&answer.expect("generateSpec"));
        assert_eq!(spec["subcommands"][0]["name"], "faccmd", "{spec}");
        assert_eq!(
            spec["subcommands"][0]["description"], "Run git-faccmd",
            "{spec}"
        );
        // Without exec, the function has nothing to run `git help -a` with.
        probe.thread.send(Job::Call {
            id: 8,
            spec: "git".to_owned(),
            generation: probe.generation("git"),
            fn_id: generate,
            args_json: r#"[["git", "ch"]]"#.to_owned(),
            cwd: Some(root.join("repo")),
            env: Some(env),
            with_exec: false,
            budget: Duration::from_secs(10),
        });
        assert!(matches!(
            called(&mut probe, &[8]).remove(&8),
            Some(Err(CallError::Js(_)))
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn calls_of_one_function_answer_by_their_own_ids() {
        let mut probe = Probe::new();
        let git = probe.loaded("git");
        let alias = match git.root.args[0]
            .parser_directives
            .as_ref()
            .and_then(|directives| directives.alias.as_ref())
        {
            Some(StringOrFn::Fn(id)) => id.0,
            other => panic!("git's alias: {other:?}"),
        };
        let (root, env) = repo("alias");
        for (id, token) in [(1, "co"), (2, "nope")] {
            probe.thread.send(Job::Call {
                id,
                spec: "git".to_owned(),
                generation: probe.generation("git"),
                fn_id: alias,
                args_json: serde_json::json!([token]).to_string(),
                cwd: Some(root.join("repo")),
                env: Some(Arc::clone(&env)),
                with_exec: true,
                budget: Duration::from_secs(10),
            });
        }
        let mut answers = called(&mut probe, &[1, 2]);
        assert_eq!(
            answers
                .remove(&1)
                .map(|result| result.map(|text| json(&text))),
            Some(Ok(serde_json::json!("checkout -q")))
        );
        match answers.remove(&2) {
            Some(Err(CallError::Js(message))) => {
                assert!(message.contains("Failed parsing alias"), "{message}");
            }
            other => panic!("{other:?}"),
        }
        let _ = std::fs::remove_dir_all(root);
    }

    const DEMO: &str = r#"
const later = (ms, value) => new Promise((resolve) => setTimeout(() => resolve(value), ms));
const format = (n) => new Intl.NumberFormat(undefined, {
  notation: "compact", compactDisplay: "short", maximumSignificantDigits: 3,
}).format(n);
export default {
  name: "demo",
  description: "mine",
  args: {
    generators: [
      { custom: async () => [{ name: await later(5, "late") }] },
      { custom: () => new Promise((resolve) => setTimeout(resolve, 5000)) },
      { custom: () => { for (;;) {} } },
      { marker: "bound", postProcess() { return this.marker; } },
      { custom: (base) => ({ name: "x", next: (n) => base + n }) },
      { custom: () => [typeof require, typeof process, typeof fetch, typeof setTimeout] },
      { custom: async () => { try { await import("./other.js"); return "imported"; } catch { return "no import"; } } },
      { custom: () => [1234567, 999999, 1234, 999, 12345, 0.5, -2500].map(format) },
      { custom: () => { console.log("from", "demo", 1); return null; } },
      { custom: () => { const kept = []; for (;;) kept.push(`${kept.length}`.repeat(1 << 16)); } },
    ],
  },
};
"#;

    #[test]
    fn calls_drive_promises_timers_and_budgets() {
        let home = home_with("calls", &[("demo", DEMO)]);
        let mut probe = Probe::with_home(Some(home.clone()));
        let demo = probe.loaded("demo");
        assert!(demo.has_js);
        assert_eq!(demo.root.description.as_deref(), Some("mine"));
        let id = |n: usize, what: &str| fn_id(&demo, &format!("args.generators.{n}.{what}"));
        let budget = Duration::from_millis(500);

        // An async function that awaits a timer is driven to completion.
        assert_eq!(
            probe.call("demo", id(0, "custom"), "[]", budget),
            Ok(r#"[{"name":"late"}]"#.to_owned())
        );

        // A promise past its budget, and a busy loop, time out within it.
        let started = Instant::now();
        let short = Duration::from_millis(100);
        assert_eq!(
            probe.call("demo", id(1, "custom"), "[]", short),
            Err(CallError::Timeout)
        );
        assert_eq!(
            probe.call("demo", id(2, "custom"), "[]", short),
            Err(CallError::Timeout)
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );

        // `this` is the object holding the function.
        assert_eq!(
            probe.call("demo", id(3, "postProcess"), r#"["out"]"#, budget),
            Ok(r#""bound""#.to_owned())
        );

        // A function in a result is kept and callable by its id.
        let made = json(
            &probe
                .call("demo", id(4, "custom"), "[40]", budget)
                .expect("call"),
        );
        assert_eq!(made["name"], "x");
        let next = made["next"]["$fn"].as_u64().expect("a function id");
        // Numbered above every spec's own functions (see `Registry`).
        assert!(next >= u64::from(WALKED_FIRST), "{next}");
        assert!(WALKED_FIRST as usize > demo.fns.len());
        let next = u32::try_from(next).expect("id");
        assert_eq!(probe.call("demo", next, "[2]", budget), Ok("42".to_owned()));

        // No host globals beyond the shims.
        assert_eq!(
            probe.call("demo", id(5, "custom"), "[]", budget),
            Ok(r#"["undefined","undefined","undefined","function"]"#.to_owned())
        );
        assert_eq!(
            probe.call("demo", id(6, "custom"), "[]", budget),
            Ok(r#""no import""#.to_owned())
        );
        assert_eq!(
            probe.call("demo", id(7, "custom"), "[]", budget),
            Ok(r#"["1.23M","1M","1.23K","999","12.3K","0.5","-2.5K"]"#.to_owned())
        );
        assert_eq!(
            probe.call("demo", id(8, "custom"), "[]", budget),
            Ok("null".to_owned())
        );

        // The heap limit holds, and the context is still usable after it.
        match probe.call("demo", id(9, "custom"), "[]", Duration::from_secs(5)) {
            Err(CallError::Js(message)) => assert!(message.contains("out of memory"), "{message}"),
            other => panic!("{other:?}"),
        }
        assert_eq!(
            probe.call("demo", id(3, "postProcess"), "[]", budget),
            Ok(r#""bound""#.to_owned())
        );
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn a_module_that_never_ends_is_interrupted_within_the_eval_budget() {
        let home = home_with("forever", &[("forever", "for (;;) {}\nexport default {};")]);
        let budget = Duration::from_millis(100);
        let mut probe = Probe::with_budget(Some(home.clone()), budget);
        let started = Instant::now();
        assert!(matches!(probe.load("forever"), Err(LoadError::Timeout)));
        let elapsed = started.elapsed();
        assert!(elapsed >= budget, "{elapsed:?}");
        assert!(elapsed < budget + Duration::from_millis(150), "{elapsed:?}");
        // The next load gets its own budget.
        assert!(probe.loaded("cd").has_js);
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn eval_forever_is_interrupted_within_its_budget() {
        let mut probe = Probe::new();
        let budget = Duration::from_millis(100);
        probe.thread.send(Job::EvalForever { budget });
        match probe.next() {
            Outcome::Evaluated { result, elapsed } => {
                assert_eq!(result, Err(CallError::Timeout));
                assert!(elapsed >= budget, "{elapsed:?}");
                assert!(elapsed < budget + Duration::from_millis(150), "{elapsed:?}");
            }
            other => panic!("{other:?}"),
        }
        // The thread goes on.
        assert!(!probe.loaded("ls").has_js);
    }

    #[test]
    fn user_specs_win_and_broken_ones_fall_back() {
        let home = home_with(
            "user",
            &[
                (
                    "git",
                    r#"export default { name: "git", subcommands: [{ name: "mine" }] };"#,
                ),
                // A syntax error, and one that also eats the budget.
                ("aws/ec2", "export default {"),
                ("cd", "for (;;) {}"),
                (
                    "versioned",
                    r#"export const versions = {
  "2.0.0": { options: [{ name: "--x" }] },
  "1.0.0": { subcommands: [{ name: "b", remove: true }, { name: "c" }] },
};
export default { name: "versioned", subcommands: [{ name: "a" }, { name: "b" }] };"#,
                ),
            ],
        );
        let mut probe = Probe::with_budget(Some(home.clone()), Duration::from_secs(1));
        let git = probe.loaded("git");
        assert_eq!(names(&git.root.subcommands), ["mine"]);
        assert!(!git.has_js);
        let ec2 = probe.loaded("aws/ec2");
        assert!(ec2.root.subcommands.len() > 100, "the archive's aws/ec2");
        assert!(ec2.has_js);
        let cd = probe.loaded("cd");
        assert!(cd.has_js, "the archive's cd");
        // A versions export is applied, newest last.
        let versioned = probe.loaded("versioned");
        assert_eq!(names(&versioned.root.subcommands), ["c", "a"]);
        assert_eq!(versioned.root.options.len(), 1);
        assert_eq!(
            versioned.version,
            Some(SpecVersion::Assumed("2.0.0".to_owned()))
        );
        assert!(matches!(probe.load("../escape"), Err(LoadError::NotFound)));
        let _ = std::fs::remove_dir_all(home);
    }

    /// A job from an earlier load of a spec never runs the new load's
    /// function at its id: it is refused (the poll side asks again with
    /// the new load).
    #[test]
    fn a_stale_function_id_never_runs_another_loads_code() {
        let source = |word: &str| {
            format!(
                r#"export default {{ name: "swap", generateSpec: () => ({{ name: "{word}" }}),
  args: {{ generators: {{ custom: async () => [{{ name: "{word}" }}] }} }} }};"#
            )
        };
        let home = home_with("stale-ids", &[("swap", &source("one"))]);
        let mut probe = Probe::with_home(Some(home.clone()));
        let old = probe.loaded("swap");
        let generate = fn_id(&old, "generateSpec");
        let called = |probe: &mut Probe, generation: u64| {
            probe.calls += 1;
            let id = probe.calls;
            probe.thread.send(Job::Call {
                id,
                spec: "swap".to_owned(),
                generation,
                fn_id: generate,
                args_json: "[[]]".to_owned(),
                cwd: None,
                env: None,
                with_exec: false,
                budget: TEST_BUDGET,
            });
            match probe.next() {
                Outcome::Called {
                    id: got, result, ..
                } if got == id => result,
                other => panic!("{other:?}"),
            }
        };
        let generated = |probe: &mut Probe, generation: u64| {
            probe.thread.send(Job::Generate {
                id: 1,
                spec: "swap".to_owned(),
                generation,
                arg_path: "swap:args".to_owned(),
                generator_index: 0,
                tokens: vec!["swap".to_owned(), String::new()],
                search_term: String::new(),
                cwd: std::env::temp_dir(),
                env: Arc::new(Vec::new()),
                generator: None,
            });
            match probe.next() {
                Outcome::Generated { result, .. } => result,
                other => panic!("{other:?}"),
            }
        };
        assert!(called(&mut probe, old.generation).is_ok_and(|json| json.contains("one")));
        write_user(&home, "swap.js", &source("two"));
        let new = probe.loaded("swap");
        assert_ne!(new.generation, old.generation);
        assert_eq!(fn_id(&new, "generateSpec"), generate);
        assert_eq!(called(&mut probe, old.generation), Err(CallError::Stopped));
        assert!(called(&mut probe, new.generation).is_ok_and(|json| json.contains("two")));
        assert_eq!(
            generated(&mut probe, old.generation),
            Err(GenError::Cancelled)
        );
        assert!(
            generated(&mut probe, new.generation)
                .is_ok_and(|items| items.len() == 1 && items[0].name == ["two"])
        );
        let _ = std::fs::remove_dir_all(home);
    }

    /// No timer can take the thread out: a wait of `Infinity`, `1e22` or
    /// `NaN` ms, or a clock that jumps, in a spec's module or in a
    /// generator's promise, loads (or runs out of time), and specs stay
    /// available.
    #[test]
    fn no_timer_takes_the_thread_out() {
        let timer = |delay: &str| {
            format!(
                r#"await new Promise((r) => setTimeout(r, {delay}));
export default {{ name: "t" }};"#
            )
        };
        let clock = r#"let calls = 0;
const real = Date.now;
Date.now = () => (calls++ === 0 ? 1e300 : real());
await new Promise((r) => setTimeout(r, 5));
export default { name: "t-clock" };"#;
        let generator = r#"export default { name: "t-gen", args: { generators: [
  { custom: () => new Promise((r) => setTimeout(() => r([{ name: "late" }]), Infinity)) },
  { custom: () => {
      let calls = 0;
      const real = Date.now;
      Date.now = () => (calls++ === 0 ? 1e300 : real());
      return new Promise((r) => setTimeout(() => r([{ name: "late" }]), 5));
  } },
] } };"#;
        let (inf, big, nan) = (timer("Infinity"), timer("1e22"), timer("NaN"));
        let home = home_with(
            "timers",
            &[
                ("t-inf", &inf),
                ("t-big", &big),
                ("t-nan", &nan),
                ("t-clock", clock),
                ("t-gen", generator),
            ],
        );
        let budget = Duration::from_millis(300);
        let mut probe = Probe {
            thread: SpecThread::start_with_timeouts(Some(home.clone()), budget, budget)
                .expect("spec thread"),
            pending: VecDeque::new(),
            calls: 0,
            generations: HashMap::new(),
        };
        for name in ["t-inf", "t-big", "t-clock"] {
            match probe.load(name) {
                Err(LoadError::Timeout) => {}
                other => panic!("{name}: {other:?}"),
            }
        }
        assert!(probe.load("t-nan").is_ok());
        probe.loaded("t-gen");
        for index in 0..2 {
            probe.thread.send(Job::Generate {
                id: index as u64 + 1,
                spec: "t-gen".to_owned(),
                generation: probe.generation("t-gen"),
                arg_path: "t-gen:args".to_owned(),
                generator_index: index,
                tokens: vec!["t-gen".to_owned(), String::new()],
                search_term: String::new(),
                cwd: home.clone(),
                env: Arc::new(Vec::new()),
                generator: None,
            });
            match probe.next() {
                Outcome::Generated {
                    result: Err(GenError::Timeout),
                    ..
                } => {}
                other => panic!("generator {index}: {other:?}"),
            }
        }
        assert!(!probe.thread.is_dead());
        assert!(probe.loaded("ls").root.options.len() > 10);
        let _ = std::fs::remove_dir_all(home);
    }

    /// A TypeScript spec in Fig's style loads with its types stripped, its
    /// import of Fig's helpers resolved, and a bare `folders` made a plain
    /// generator; a JSON spec loads with no context.
    #[test]
    fn typescript_and_json_specs_load() {
        let home = home_files(
            "formats",
            &[
                (
                    "tool.ts",
                    r#"import type { Thing } from "./nowhere";
import { filepaths, folders, keyValue, type FilepathsOptions } from "@fig/autocomplete-generators";

interface Extra {
  depth: number;
}
type Kind = "run" | "stop";
const kinds = ["run", "stop"] as const;
const options: FilepathsOptions = { extensions: ["md"] };
const extra = { depth: 2 } satisfies Extra;

const completionSpec: Fig.Spec = {
  name: "tool",
  subcommands: [
    ...kinds.map((kind: Kind): Fig.Subcommand => ({ name: kind, priority: extra.depth })),
    { name: "cd", args: { name: "dir", generators: folders } },
    { name: "set", args: { name: "pair", generators: keyValue({ keys: ["a"], values: ["1"] }) } },
  ],
  args: { name: "file", generators: filepaths(options) },
};
export default completionSpec;
"#,
                ),
                (
                    "jtool.json",
                    r#"{"name": "jtool", "subcommands": [{"name": "one"}],
                        "args": {"generators": {"script": ["ls"], "splitOn": "\n"}}}"#,
                ),
            ],
        );
        let mut probe = Probe::with_home(Some(home.clone()));
        let tool = probe.loaded("tool");
        assert_eq!(names(&tool.root.subcommands), ["run", "stop", "cd", "set"]);
        assert_eq!(tool.root.subcommands[0].priority, Some(2.0));
        assert!(tool.has_js);
        // `folders` as it is: a generator object, not a function.
        let cd = &tool.root.subcommands[2].args[0].generators[0];
        assert!(cd.custom.is_some() && cd.trigger.is_some(), "{cd:?}");
        assert!(
            tool.fns
                .contains(&"subcommands.2.args.generators.custom".to_owned()),
            "{:?}",
            tool.fns
        );
        let jtool = probe.loaded("jtool");
        assert_eq!(names(&jtool.root.subcommands), ["one"]);
        assert!(!jtool.has_js && jtool.fns.is_empty());
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn ts_wins_over_js_and_js_over_json() {
        let spec = |sub: &str| {
            format!(r#"export default {{ name: "t", subcommands: [{{ name: "{sub}" }}] }};"#)
        };
        let (ts, js) = (spec("from-ts"), spec("from-js"));
        let home = home_files(
            "precedence",
            &[
                ("a.ts", &ts),
                ("a.js", &js),
                (
                    "a.json",
                    r#"{"name": "a", "subcommands": [{"name": "from-json"}]}"#,
                ),
                ("b.js", &js),
                (
                    "b.json",
                    r#"{"name": "b", "subcommands": [{"name": "from-json"}]}"#,
                ),
                (
                    "c.json",
                    r#"{"name": "c", "subcommands": [{"name": "from-json"}]}"#,
                ),
            ],
        );
        let mut probe = Probe::with_home(Some(home.clone()));
        for (name, from) in [("a", "from-ts"), ("b", "from-js"), ("c", "from-json")] {
            assert_eq!(
                names(&probe.loaded(name).root.subcommands),
                [from],
                "{name}"
            );
        }
        let _ = std::fs::remove_dir_all(home);
    }

    /// What is wrong with a broken spec is said exactly, and a broken spec
    /// for a command fac has a spec of falls back to that spec.
    #[test]
    fn broken_user_specs_say_what_is_wrong() {
        let home = home_files(
            "broken",
            &[
                (
                    "syntax.ts",
                    "const spec: Fig.Spec = {\n  name: \"x\"\n  args: {},\n};\nexport default spec;\n",
                ),
                (
                    "imports.ts",
                    "import { pick } from \"lodash\";\nexport default { name: pick };\n",
                ),
                (
                    "dynamic.js",
                    "await import(\"./elsewhere.js\");\nexport default { name: \"x\" };\n",
                ),
                (
                    "typo.json",
                    r#"{"name": "typo", "subcommands": [{"name": "a", "descripton": "b"}]}"#,
                ),
                ("thrown.js", "throw new Error(\"secret text\");\n"),
                ("git.ts", "export default {\n"),
            ],
        );
        let mut probe = Probe::with_home(Some(home.clone()));
        let syntax = user_error(probe.load("syntax"));
        assert_eq!(
            (syntax.kind, syntax.at),
            (user::ErrorKind::Syntax, Some((3, 3)))
        );
        let imports = user_error(probe.load("imports"));
        assert_eq!(imports.kind, user::ErrorKind::Import);
        assert!(imports.to_string().contains("\"lodash\""), "{imports}");
        match probe.load("dynamic") {
            Err(LoadError::Js(message)) => assert!(message.contains("./elsewhere.js"), "{message}"),
            other => panic!("{other:?}"),
        }
        let typo = user_error(probe.load("typo"));
        assert_eq!(typo.to_string(), "unknown key subcommands.0.descripton");
        match probe.load("thrown") {
            Err(err @ LoadError::Js(_)) => {
                assert!(err.to_string().contains("secret text"));
                assert!(
                    !err.summary().contains("secret"),
                    "the log never quotes the file"
                );
            }
            other => panic!("{other:?}"),
        }
        let git = probe.loaded("git");
        assert!(
            names(&git.root.subcommands).contains(&"commit"),
            "fac's own git"
        );
        let _ = std::fs::remove_dir_all(home);
    }

    /// `@fig/autocomplete-generators` behaves as Fig's own tests say
    /// (`generators/test/keyvalue.test.ts` and `filepaths.test.ts` in
    /// autocomplete-tools, ported): a spec runs them as it loads and gives
    /// back what failed.
    #[test]
    fn fig_generator_helpers_behave_as_in_fig() {
        let tests = r#"import { filepaths, folders, keyValue, keyValueList, valueList, ai }
  from "@fig/autocomplete-generators";

const failures = [];
const same = (a, b) => JSON.stringify(a) === JSON.stringify(b);
const check = (what, got, want) => {
  if (!same(got, want)) failures.push(`${what}: ${JSON.stringify(got)}, not ${JSON.stringify(want)}`);
};
const exec = async () => ({ status: 1, stderr: "", stdout: "" });
const context = { searchTerm: "", currentProcess: "", currentWorkingDirectory: "",
  sshPrefix: "", environmentVariables: {} };
const suggests = (generator) => async (token, want) =>
  check(`custom ${token}`, await generator.custom(["spec", token], exec, context), want);
const queries = (generator) => (token, want) =>
  check(`getQueryTerm ${token}`, generator.getQueryTerm(token), want);
const triggers = (generator) => (a, b, want) =>
  check(`trigger ${a} ${b}`, generator.trigger(a, b), want);
const named = (...names) => names.map((name) => ({ name }));

// valueList
let query = queries(valueList({}));
query("", ""); query("value", "value"); query("value,", ""); query("value,val", "val");
let trigger = triggers(valueList({}));
trigger("", "", false); trigger("", "a", false); trigger("value", "value,", true);
trigger("value,", "value", true); trigger("value,", "value,v", false);
let suggest = suggests(valueList({ values: ["1", "2", "3"], delimiter: "/", allowRepeatedValues: true }));
await suggest("", named("1", "2", "3")); await suggest("1/", named("1", "2", "3"));
suggest = suggests(valueList({ values: ["1", "2", "3"], delimiter: "/", insertDelimiter: true }));
await suggest("", [{ name: "1", insertValue: "1/" }, { name: "2", insertValue: "2/" },
  { name: "3", insertValue: "3/" }]);
suggest = suggests(valueList({ values: ["1", "2", "3"] }));
await suggest("1,", named("2", "3")); await suggest("1,2", named("3"));
let calls = 0;
suggest = suggests(valueList({ values: async () => { calls += 1; return named("value"); },
  allowRepeatedValues: true, cache: true }));
await suggest("", named("value")); await suggest("value,", named("value"));
check("valueList cache calls", calls, 1);

// keyValue
query = queries(keyValue({}));
query("", ""); query("key", "key"); query("key=", ""); query("key=val", "val");
trigger = triggers(keyValue({}));
trigger("", "", false); trigger("key", "key=", true); trigger("key=", "key=v", false);
trigger("key=val", "key=val=", false);
suggest = suggests(keyValue({ keys: ["a", "b", "c"], values: ["1", "2", "3"], separator: ":",
  insertSeparator: false }));
await suggest("", named("a", "b", "c")); await suggest(":", named("1", "2", "3"));
await suggest("key:val", named("1", "2", "3"));
suggest = suggests(keyValue({ keys: ["a", "b"], values: ["1"], separator: ":" }));
await suggest("key", [{ name: "a", insertValue: "a:" }, { name: "b", insertValue: "b:" }]);
await suggest("key:", named("1"));
let keyCalls = 0, valueCalls = 0;
suggest = suggests(keyValue({
  keys: async () => { keyCalls += 1; return named("key"); },
  values: async () => { valueCalls += 1; return named("value"); },
  cache: "keys", insertSeparator: false }));
await suggest("", named("key")); await suggest("key", named("key"));
await suggest("key=", named("value")); await suggest("key=val", named("value"));
check("keyValue cache: keys", [keyCalls, valueCalls], [1, 2]);

// keyValueList
query = queries(keyValueList({}));
query("key=", ""); query("key=val", "val"); query(",abc", "abc"); query("key=val,abc", "abc");
trigger = triggers(keyValueList({}));
trigger("key", "ke", false); trigger("key", "key=", true); trigger("key=val", "key=val,", true);
suggest = suggests(keyValueList({ keys: ["a", "b", "c"], values: ["1", "2", "3"],
  insertSeparator: false }));
await suggest("", named("a", "b", "c")); await suggest("key=", named("1", "2", "3"));
await suggest("key=value,", named("a", "b", "c"));
await suggest("key=value,key2=", named("1", "2", "3"));
suggest = suggests(keyValueList({ keys: ["a", "b", "c"], values: ["1", "2"], insertSeparator: false }));
await suggest("a=1,", named("b", "c")); await suggest("a=1,b=", named("1", "2"));

// filepaths: getCurrentInsertedDirectory, through the directory ls runs in,
// and sortFilesAlphabetically, through the order of the suggestions.
const home = "/home/user";
const cwd = `${home}/current_cwd`;
const listing = ["z.md", "a.txt", ".hidden.md", "b.md", "sub/", ".DS_Store"].join("\n");
const ran = [];
const ls = async (input) => { ran.push(input.cwd); return { status: 0, stderr: "", stdout: listing }; };
const at = (searchTerm, environmentVariables = { HOME: home }) =>
  ({ ...context, currentWorkingDirectory: cwd, searchTerm, environmentVariables });
const files = async (generator, searchTerm, env) =>
  (await generator.custom([], ls, at(searchTerm, env))).map((suggestion) => suggestion.name);
check("filepaths", await files(filepaths, "test/"),
  ["a.txt", "b.md", "sub/", "z.md", ".hidden.md", "../"]);
check("filepaths md", await files(filepaths({ extensions: ["md"] }), "src/packages"),
  ["b.md", "sub/", "z.md", ".hidden.md", "../"]);
check("folders", await files(folders, "src"), ["sub/", "../"]);
check("folders()", await files(folders(), "~/some_dir/src/test/"), ["sub/", "../"]);
await files(filepaths, "/etc/bin/tool");
await files(filepaths, "$HOME/src/$DIR/test/", { HOME: home, DIR: "tmp/folder" });
await files(filepaths, "${HOME:-/fallback}/src/${DIR:-fallback}/test/");
await files(filepaths, "$DIR");
check("filepaths directories", ran, [`${cwd}/test/`, `${cwd}/src/`, `${cwd}/`,
  `${home}/some_dir/src/test/`, "/etc/bin/", `${home}/src/tmp/folder/test/`,
  `${home}/src/fallback/test/`, `${cwd}/`]);
const fileTrigger = triggers(filepaths);
fileTrigger("a", "ab", false); fileTrigger("a", "a/", true); fileTrigger("a/b", "a/bc", false);
fileTrigger("a/b", "c/b", true);
check("filepaths getQueryTerm", filepaths.getQueryTerm("src/ma"), "ma");
const edited = await filepaths({ editFileSuggestions: { priority: 76 } })
  .custom([], ls, at(""));
check("editFileSuggestions", edited.filter((s) => s.priority === 76).length, 4);
check("ai", await ai({ name: "x", message: "y" }).custom([], exec, context), []);

export default { name: "fig-tests", description: JSON.stringify(failures) };
"#;
        let home = home_files("fig-tests", &[("fig-tests.js", tests)]);
        let mut probe = Probe::with_home(Some(home.clone()));
        let spec = probe.loaded("fig-tests");
        assert_eq!(spec.root.description.as_deref(), Some("[]"));
        let _ = std::fs::remove_dir_all(home);
    }

    /// A `Recheck` loads a spec again only when its user spec files changed:
    /// an edit, a new file, a removed one (back to fac's own spec).
    #[test]
    fn a_changed_user_spec_is_loaded_again() {
        let mine = |sub: &str| {
            format!(r#"export default {{ name: "git", subcommands: [{{ name: "{sub}" }}] }};"#)
        };
        let home = home_files("recheck", &[("git.js", &mine("first"))]);
        let mut probe = Probe::with_home(Some(home.clone()));
        assert_eq!(names(&probe.loaded("git").root.subcommands), ["first"]);
        let recheck = |probe: &mut Probe, name: &str| {
            probe.thread.send(Job::Recheck {
                name: name.to_owned(),
            });
            // Something that answers, so an unchanged name shows as nothing.
            probe.thread.send(Job::MemoryUsage);
            match probe.next() {
                Outcome::MemoryUsage(_) => None,
                Outcome::Changed {
                    name: changed,
                    result,
                } if changed == name => {
                    assert!(matches!(probe.next(), Outcome::MemoryUsage(_)));
                    Some(result)
                }
                other => panic!("{other:?}"),
            }
        };
        assert!(recheck(&mut probe, "git").is_none(), "nothing changed");
        write_user(&home, "git.js", &mine("second, edited"));
        let edited = recheck(&mut probe, "git").expect("changed").expect("loads");
        assert_eq!(names(&edited.root.subcommands), ["second, edited"]);
        assert!(recheck(&mut probe, "git").is_none(), "once per change");
        // A TypeScript file comes first.
        write_user(&home, "git.ts", &mine("third"));
        let ts = recheck(&mut probe, "git").expect("changed").expect("loads");
        assert_eq!(names(&ts.root.subcommands), ["third"]);
        for file in ["git.ts", "git.js"] {
            std::fs::remove_file(home.join(".config/fac/specs").join(file)).expect("rm");
        }
        let ours = recheck(&mut probe, "git").expect("changed").expect("loads");
        assert!(
            names(&ours.root.subcommands).contains(&"commit"),
            "fac's own git is back"
        );
        // A command fac has no spec for gets one.
        assert_eq!(probe.load("mytool").map(|_| ()), Err(LoadError::NotFound));
        write_user(
            &home,
            "mytool.json",
            r#"{"name": "mytool", "subcommands": [{"name": "go"}]}"#,
        );
        let made = recheck(&mut probe, "mytool")
            .expect("changed")
            .expect("loads");
        assert_eq!(names(&made.root.subcommands), ["go"]);
        // A name the thread never loaded is not looked at.
        assert!(recheck(&mut probe, "never-loaded").is_none());
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn a_panic_reports_dead_and_leaves_the_terminal_alone() {
        let mut probe = Probe::new();
        probe.thread.send(Job::PanicPlan);
        match probe.next() {
            Outcome::PanicPlan(plan) => assert_eq!(
                plan,
                crate::proxy::PanicPlan {
                    restore_screen: false,
                    restore_termios: false,
                },
                "the hook must not write to fd 1 or touch termios for the spec thread"
            ),
            other => panic!("{other:?}"),
        }
        assert!(
            crate::proxy::panic_plan().restore_screen,
            "other threads still restore"
        );
        probe.thread.send(Job::Panic);
        assert!(matches!(probe.next(), Outcome::Dead));
        assert!(probe.thread.is_dead());
        // Later sends are no-ops.
        probe.thread.send(Job::Load {
            name: "git".to_owned(),
            cwd: None,
            env: None,
        });
        assert!(probe.thread.take().is_empty());
        probe.thread.shutdown();
    }

    #[test]
    fn a_dropped_context_frees_its_memory() {
        let mut probe = Probe::new();
        let before = probe.memory();
        assert!(probe.loaded("aws/ec2").has_js);
        let loaded = probe.memory();
        probe.thread.send(Job::Drop {
            name: "aws/ec2".to_owned(),
        });
        let dropped = probe.memory();
        assert!(loaded > before, "{before} -> {loaded}");
        // Everything the context held goes, not only part of it.
        assert!(
            dropped - before < (loaded - before) / 10,
            "{before} -> {loaded} -> {dropped}"
        );
        // Specs without functions never make a context.
        assert!(!probe.loaded("clang").has_js);
        assert!(probe.memory() <= dropped + 1024);
    }

    #[test]
    fn the_thread_blocks_every_signal() {
        let mut probe = Probe::new();
        probe.thread.send(Job::SignalMask);
        let blocked = match probe.next() {
            Outcome::SignalMask(blocked) => blocked,
            other => panic!("{other:?}"),
        };
        let all: Vec<i32> = (1..32)
            .filter(|&signal| signal != libc::SIGKILL && signal != libc::SIGSTOP)
            .collect();
        assert_eq!(blocked, all);
    }

    #[test]
    fn shutdown_interrupts_running_javascript() {
        let probe = Probe::new();
        probe.thread.send(Job::EvalForever {
            budget: Duration::from_secs(30),
        });
        std::thread::sleep(Duration::from_millis(50));
        let started = Instant::now();
        probe.thread.shutdown();
        assert!(
            started.elapsed() < Duration::from_millis(200),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    #[ignore = "measurement; run in release with --ignored --nocapture"]
    fn measure_load_times() {
        let _ = Archive::embedded().names().count();
        for name in ["git", "aws/ec2", "gcloud/compute", "cd", "az"] {
            let mut probe = Probe::with_budget(None, EVAL_BUDGET);
            let started = Instant::now();
            let cold = probe.loaded(name);
            let cold_wall = started.elapsed();
            probe.thread.send(Job::Drop {
                name: name.to_owned(),
            });
            let started = Instant::now();
            let warm = probe.loaded(name);
            let warm_wall = started.elapsed();
            eprintln!(
                "{name}: cold {:.2} ms (wall {:.2}), warm {:.2} ms (wall {:.2}), has_js={}",
                cold.load_time.as_secs_f64() * 1e3,
                cold_wall.as_secs_f64() * 1e3,
                warm.load_time.as_secs_f64() * 1e3,
                warm_wall.as_secs_f64() * 1e3,
                warm.has_js
            );
        }
    }

    #[test]
    fn terms_call_trigger_and_query_term_functions() {
        let mut probe = Probe::new();
        let cd = probe.loaded("cd");
        let trigger = fn_id(&cd, "args.generators.trigger");
        let query = fn_id(&cd, "args.generators.getQueryTerm");
        let mut terms = |trigger: Option<(u32, &str, &str)>, query: Option<(u32, &str)>| {
            probe.thread.send(Job::Terms {
                id: 9,
                spec: "cd".to_owned(),
                generation: probe.generation("cd"),
                trigger: trigger.map(|(id, a, b)| (id, a.to_owned(), b.to_owned())),
                query_term: query.map(|(id, term)| (id, term.to_owned())),
            });
            match probe.next() {
                Outcome::Terms {
                    id: 9,
                    trigger,
                    query_term,
                } => (trigger, query_term),
                other => panic!("{other:?}"),
            }
        };
        // The folders helper runs again when the directory changes, and
        // its query term is the part after the last `/`.
        assert_eq!(
            terms(Some((trigger, "src/", "src")), Some((query, "src/ma"))),
            (Some(true), Some("ma".to_owned()))
        );
        assert_eq!(
            terms(Some((trigger, "src/mai", "src/ma")), None),
            (Some(false), None)
        );
        // A throw counts as yes, and a failed `getQueryTerm` gives the
        // search term back.
        assert_eq!(
            terms(Some((9999, "a", "b")), Some((9999, "a/b"))),
            (Some(true), Some("a/b".to_owned()))
        );
    }

    /// Only a call's answer keeps the functions walked in it, until the
    /// poll side releases them; what the thread calls for itself (a
    /// generator's result, `postProcess`) keeps none.
    #[test]
    fn walked_functions_are_kept_only_until_released() {
        let home = home_with(
            "walked",
            &[(
                "walk",
                r#"export default { name: "walk",
  generateSpec: () => ({ name: "walk", args: { generators: { custom: () => [] } } }),
  args: { generators: [
    { custom: async () => [{ name: "a", extra: () => 1 }] },
    { script: ["printf", "x"], postProcess: () => [{ name: "b", extra: () => 2 }] },
  ] } };"#,
            )],
        );
        let mut probe = Probe::with_home(Some(home.clone()));
        let spec = probe.loaded("walk");
        let walked = |probe: &mut Probe| {
            probe.thread.send(Job::Walked {
                spec: "walk".to_owned(),
            });
            loop {
                if let Outcome::Walked(count) = probe.next() {
                    return count;
                }
            }
        };
        for index in 0..2 {
            probe.thread.send(Job::Generate {
                id: index as u64,
                spec: "walk".to_owned(),
                generation: spec.generation,
                arg_path: "walk:args".to_owned(),
                generator_index: index,
                tokens: vec!["walk".to_owned(), String::new()],
                search_term: String::new(),
                cwd: std::env::temp_dir(),
                env: Arc::new(Vec::new()),
                generator: None,
            });
            match probe.next() {
                Outcome::Generated {
                    result: Ok(items), ..
                } => assert_eq!(items.len(), 1),
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(walked(&mut probe), 0);
        let json = probe
            .call("walk", fn_id(&spec, "generateSpec"), "[[]]", TEST_BUDGET)
            .expect("generateSpec answers");
        let answer: Subcommand = serde_json::from_str(&json).expect("a spec");
        let id = answer.args[0].generators[0].custom.expect("a function").0;
        assert!(id >= WALKED_FIRST);
        assert_eq!(walked(&mut probe), 1);
        // A spec's own functions are never released.
        probe.thread.send(Job::Release {
            spec: "walk".to_owned(),
            ids: vec![0, id],
        });
        assert_eq!(walked(&mut probe), 0);
        assert_eq!(
            probe.call("walk", id, "[]", TEST_BUDGET),
            Err(CallError::UnknownFunction(id))
        );
        assert!(
            probe
                .call("walk", fn_id(&spec, "generateSpec"), "[[]]", TEST_BUDGET)
                .is_ok()
        );
        let _ = std::fs::remove_dir_all(home);
    }

    /// A generator on an arg from a `generateSpec` answer is not in the
    /// spec's tree, but its functions were saved when the answer was walked:
    /// the job carries the generator, and its functions are called by id.
    #[test]
    fn a_generator_from_an_answer_runs() {
        let home = home_with(
            "answered",
            &[(
                "gen",
                r#"export default { name: "gen", generateSpec: async () => ({
  name: "gen",
  subcommands: [{ name: "sub", args: { name: "x", isDangerous: true,
    generators: { custom: async (tokens, exec, context) =>
      [{ name: "made-" + tokens.length + "-" + context.isDangerous }] } } }],
}) };"#,
            )],
        );
        let mut probe = Probe::with_home(Some(home.clone()));
        let spec = probe.loaded("gen");
        let generate_spec = fn_id(&spec, "generateSpec");
        let json = probe
            .call("gen", generate_spec, r#"[["gen", ""]]"#, TEST_BUDGET)
            .expect("generateSpec answers");
        let answer: Subcommand = serde_json::from_str(&json).expect("a spec");
        let arg = &answer.subcommands[0].args[0];
        let generator = arg.generators[0].clone();
        assert!(generator.custom.is_some());
        let request = |generator: Option<Box<(Generator, bool)>>| Job::Generate {
            id: 4,
            spec: "gen".to_owned(),
            generation: spec.generation,
            arg_path: r#"gen:generateSpec(["gen",""]).subcommands.0.args.0"#.to_owned(),
            generator_index: 0,
            tokens: vec!["gen".to_owned(), "sub".to_owned(), String::new()],
            search_term: String::new(),
            cwd: std::env::temp_dir(),
            env: Arc::new(Vec::new()),
            generator,
        };
        // Looked up by its path, it is not found.
        probe.thread.send(request(None));
        match probe.next() {
            Outcome::Generated {
                id: 4,
                result: Err(_),
                ..
            } => {}
            other => panic!("{other:?}"),
        }
        probe
            .thread
            .send(request(Some(Box::new((generator, arg.is_dangerous)))));
        match probe.next() {
            Outcome::Generated {
                id: 4,
                result: Ok(items),
                stale: false,
            } => {
                let names: Vec<&str> = items.iter().map(|s| s.name[0].as_str()).collect();
                assert_eq!(names, ["made-3-true"]);
            }
            other => panic!("{other:?}"),
        }
        let _ = std::fs::remove_dir_all(home);
    }

    /// The ids of functions walked in a context's answers are in no other
    /// context: after a reload, calling or generating with one fails, and
    /// the new context's answers get ids of their own.
    #[test]
    fn an_id_from_a_reloaded_context_fails_cleanly() {
        let home = home_with(
            "stale",
            &[(
                "stale",
                r#"export default { name: "stale", generateSpec: async () => ({
  name: "stale",
  args: { name: "x", generators: { custom: async () => [{ name: "fresh" }] } },
}) };"#,
            )],
        );
        let mut probe = Probe::with_home(Some(home.clone()));
        let answer = |probe: &mut Probe| -> Generator {
            let spec = probe.loaded("stale");
            let json = probe
                .call(
                    "stale",
                    fn_id(&spec, "generateSpec"),
                    r#"[["stale", ""]]"#,
                    TEST_BUDGET,
                )
                .expect("generateSpec answers");
            let answer: Subcommand = serde_json::from_str(&json).expect("a spec");
            answer.args[0].generators[0].clone()
        };
        let generate = |probe: &mut Probe, id: u64, generator: &Generator| {
            probe.thread.send(Job::Generate {
                id,
                spec: "stale".to_owned(),
                generation: probe.generation("stale"),
                arg_path: r#"stale:generateSpec(["stale",""]).args.0"#.to_owned(),
                generator_index: 0,
                tokens: vec!["stale".to_owned(), String::new()],
                search_term: String::new(),
                cwd: std::env::temp_dir(),
                env: Arc::new(Vec::new()),
                generator: Some(Box::new((generator.clone(), false))),
            });
            match probe.next() {
                Outcome::Generated {
                    id: answered,
                    result,
                    stale: false,
                } if answered == id => result,
                other => panic!("{other:?}"),
            }
        };
        let old = answer(&mut probe);
        let old_id = old.custom.expect("a custom generator").0;
        assert!(old_id >= WALKED_FIRST, "{old_id}");
        assert!(matches!(generate(&mut probe, 1, &old), Ok(items) if items.len() == 1));
        // Loaded again: a new context, which never saw `old_id`.
        let new = answer(&mut probe);
        let new_id = new.custom.expect("a custom generator").0;
        assert_ne!(new_id, old_id);
        assert_eq!(
            probe.call("stale", old_id, "[]", TEST_BUDGET),
            Err(CallError::UnknownFunction(old_id))
        );
        assert!(matches!(
            generate(&mut probe, 2, &old),
            Err(GenError::Js(message)) if message.contains(&old_id.to_string())
        ));
        assert!(matches!(generate(&mut probe, 3, &new), Ok(items) if items.len() == 1));
        let _ = std::fs::remove_dir_all(home);
    }

    /// The usage store is written on the spec thread, atomically, and a
    /// save still queued when the thread is told to stop is written on its
    /// way out.
    #[test]
    fn usage_saves_are_written_even_on_the_way_out() {
        let dir = std::env::temp_dir().join(format!("fac-usage-thread-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let probe = Probe::new();
        let first = dir.join("a/usage.json");
        let last = dir.join("b/usage.json");
        let (a, b) = (
            &b"{\"version\":1,\"commands\":{\"git\":{\"add\":1}}}"[..],
            &b"{\"version\":1,\"commands\":{\"npm\":{\"i\":2}}}"[..],
        );
        probe.thread.send(Job::SaveUsage {
            path: first.clone(),
            json: a.to_vec(),
        });
        probe.thread.send(Job::SaveUsage {
            path: last.clone(),
            json: b.to_vec(),
        });
        probe.thread.shutdown();
        assert_eq!(std::fs::read(&first).ok().as_deref(), Some(a));
        assert_eq!(std::fs::read(&last).ok().as_deref(), Some(b));
        assert!(!dir.join("b/usage.json.tmp").exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// `localeCompare` sorts as V8 does under ICU's root collation, not by
    /// code unit: Fig's `sortFilesAlphabetically` puts `fixture-*` before
    /// `My Documents/`.
    #[test]
    fn locale_compare_sorts_as_v8_does() {
        let home = home_with(
            "collation",
            &[(
                "sorts",
                r#"const sorted = (names) => [...names].sort((a, b) => a.localeCompare(b)).join("\u0001");
const ascii = [];
for (let code = 0x09; code < 0x7f; code += 1) {
  if (code < 0x0e || code >= 0x20) ascii.push(String.fromCharCode(code));
}
export default { name: "sorts", args: { name: "x", generators: { custom: async () => [
  { name: sorted(["My Documents/", "fixture-dir/", "fixture-other/"]) },
  { name: sorted(["c", "B", "A", "a"]) },
  { name: sorted(["résumé", "resume", "Resume", "rest"]) },
  { name: sorted(ascii) },
  { name: String("a".localeCompare("A", undefined, { sensitivity: "base" })) },
] } } };"#,
            )],
        );
        let mut probe = Probe::with_home(Some(home.clone()));
        probe.loaded("sorts");
        probe.thread.send(Job::Generate {
            id: 1,
            spec: "sorts".to_owned(),
            generation: probe.generation("sorts"),
            arg_path: "sorts:args".to_owned(),
            generator_index: 0,
            tokens: vec!["sorts".to_owned(), String::new()],
            search_term: String::new(),
            cwd: std::env::temp_dir(),
            env: Arc::new(Vec::new()),
            generator: None,
        });
        let names: Vec<String> = match probe.next() {
            Outcome::Generated {
                result: Ok(items), ..
            } => items.into_iter().map(|s| s.name[0].clone()).collect(),
            other => panic!("{other:?}"),
        };
        let list = |at: usize| names[at].split('\u{1}').collect::<Vec<_>>();
        assert_eq!(list(0), ["fixture-dir/", "fixture-other/", "My Documents/"]);
        assert_eq!(list(1), ["a", "A", "B", "c"]);
        assert_eq!(list(2), ["rest", "resume", "Resume", "résumé"]);
        // Node's `[...ascii].sort((a, b) => a.localeCompare(b))`, as in the
        // parser's port.
        let node = "\t\n\u{b}\u{c}\r _-,;:!?.'\"()[]{}@*/\\&#%`^+<=>|~$0123456789aAbBcCdDeEfFgGhHiIjJkKlLmMnNoOpPqQrRsStTuUvVwWxXyYzZ";
        assert_eq!(list(3).concat(), node);
        assert_eq!(names[4], "0");
        let _ = std::fs::remove_dir_all(home);
    }
}
