//! Child processes for generators, run on the spec thread only.
//!
//! A child is started with `posix_spawn`, never through a shell:
//! - `POSIX_SPAWN_CLOEXEC_DEFAULT`: it inherits only fds 0, 1 and 2;
//! - `POSIX_SPAWN_SETSID`: a new session, so it has no controlling terminal
//!   and leads its own process group;
//! - `POSIX_SPAWN_SETSIGMASK` with an empty mask, because the spec thread
//!   blocks every signal and the child must not inherit that;
//! - `POSIX_SPAWN_SETSIGDEF` for every signal.
//!
//! fd 0 is `/dev/null`; fds 1 and 2 are pipes whose fac ends are
//! non-blocking. The [`Runner`] runs up to [`MAX_RUNNING`] children at once
//! and queues the rest. Nothing blocks: the spec thread polls [`Runner::fds`]
//! and calls [`Runner::pump`].
//!
//! A child is done when both its pipes are closed, or when it has exited and
//! what it wrote is read: macOS has no `pipe2`, so a pipe's write end can
//! leak into a process another thread forks before `FD_CLOEXEC` is set (fac
//! forks its shell and its zsh workers), and then EOF never comes. The exit
//! is seen with `waitid(WNOWAIT)`, which leaves the child a zombie: it is
//! reaped by pid (`waitpid`) only once fac is done with it, so its pid (and
//! so its process group id) cannot be reused while fac may still signal the
//! group. On a timeout or a cancel the whole group gets `SIGKILL` and the
//! child is reaped.
//!
//! The running children's groups are listed in [`Groups`], which the poll
//! thread holds too: when the spec thread does not stop in time at exit,
//! the poll thread kills and reaps them itself. A group leaves the list as
//! its leader is reaped, under the list's lock, and only a listed group is
//! signalled, so a signal never reaches a group that is not fac's.

use std::collections::VecDeque;
use std::ffi::CString;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use nix::errno::Errno;
use nix::fcntl::{FcntlArg, FdFlag, OFlag, fcntl};
use nix::sys::signal::{Signal, kill};
use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::{AccessFlags, Pid, access};

/// `#define POSIX_SPAWN_SETSID 0x0400` in the macOS SDK's `sys/spawn.h`;
/// libc does not export it.
const POSIX_SPAWN_SETSID: libc::c_int = 0x0400;

/// How many children run at once; more wait in a queue.
pub const MAX_RUNNING: usize = 8;
/// The most output kept per stream. A child that writes more is killed.
pub const OUTPUT_CAP: usize = 1 << 20;
/// `execvp`'s search path when the environment has no `PATH`.
const DEFAULT_PATH: &str = "/usr/bin:/bin";
/// How often a child whose pipes are closed is checked for its exit.
const REAP_SLICE: Duration = Duration::from_millis(2);
/// How often a running child is checked for its exit, for a pipe whose EOF
/// does not come.
const EXIT_SLICE: Duration = Duration::from_millis(10);

pub type ProcId = u64;

/// The process groups of a [`Runner`]'s running children.
#[derive(Debug, Clone, Default)]
pub struct Groups(Arc<Mutex<Vec<i32>>>);

impl Groups {
    fn lock(&self) -> MutexGuard<'_, Vec<i32>> {
        // A push or a removal cannot leave the list half-changed.
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// SIGKILL every group listed and reap its leader; the list is empty
    /// after it.
    pub fn kill_all(&self) {
        for group in std::mem::take(&mut *self.lock()) {
            kill_and_reap(Pid::from_raw(group));
        }
    }
}

/// What to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    /// A name looked up in the environment's `PATH`, or a path (relative to
    /// `cwd`) when it has a `/`.
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// The child's whole environment.
    pub env: Vec<(String, String)>,
    /// From when the command is submitted, queueing included.
    pub timeout: Duration,
}

/// What a child gave, as Fig's `ExecuteCommandOutput`: both streams are
/// decoded lossily and cleaned with [`clean_output`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    pub stdout: String,
    pub stderr: String,
    /// The exit code. A child killed by a signal has status 0, as in Fig
    /// (`exit_code: output.status.code().unwrap_or(0)`,
    /// `crates/figterm/src/message.rs:486`).
    pub status: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RunError {
    #[error("could not start the program: {0}")]
    Spawn(String),
    #[error("the program ran out of time")]
    Timeout,
}

/// Fig's `cleanOutput` (`api-bindings-wrappers/src/executeCommand.ts:11-17`):
/// CRLF becomes LF, the cursor-show sequence `ESC[?25h` goes, and leading
/// and trailing newlines are trimmed.
pub fn clean_output(output: &str) -> String {
    let output = output.replace("\r\n", "\n").replace("\x1b[?25h", "");
    output
        .trim_start_matches('\n')
        .trim_end_matches('\n')
        .to_owned()
}

/// One of a child's output pipes.
struct Stream {
    /// None once closed (EOF, or the child was killed).
    fd: Option<OwnedFd>,
    data: Vec<u8>,
}

impl Stream {
    fn new(fd: OwnedFd) -> Self {
        Self {
            fd: Some(fd),
            data: Vec::new(),
        }
    }

    /// Read what is there. True when the stream went past [`OUTPUT_CAP`].
    fn read(&mut self) -> bool {
        let Some(fd) = &self.fd else {
            return false;
        };
        let mut buf = [0u8; 16 << 10];
        loop {
            match nix::unistd::read(fd, &mut buf) {
                Ok(0) => {
                    self.fd = None;
                    return false;
                }
                Ok(n) => {
                    self.data.extend_from_slice(&buf[..n]);
                    if self.data.len() > OUTPUT_CAP {
                        self.data.truncate(OUTPUT_CAP);
                        return true;
                    }
                }
                Err(Errno::EINTR) => {}
                Err(Errno::EAGAIN) => return false,
                Err(_) => {
                    self.fd = None;
                    return false;
                }
            }
        }
    }

    fn text(&self) -> String {
        clean_output(&String::from_utf8_lossy(&self.data))
    }
}

struct Child {
    id: ProcId,
    pid: Pid,
    stdout: Stream,
    stderr: Stream,
    deadline: Instant,
}

impl Child {
    fn closed(&self) -> bool {
        self.stdout.fd.is_none() && self.stderr.fd.is_none()
    }

    /// Whether the child has exited, without reaping it.
    fn exited(&self) -> bool {
        // SAFETY: an all-zero siginfo_t is a valid value for waitid to fill.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let Ok(pid) = libc::id_t::try_from(self.pid.as_raw()) else {
            return false;
        };
        // SAFETY: `info` lives on this stack; WNOWAIT leaves the child
        // waitable, so the later waitpid still reaps it.
        let rc = unsafe {
            libc::waitid(
                libc::P_PID,
                pid,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        // With WNOHANG, si_pid stays 0 while the child runs.
        rc == 0 && info.si_pid != 0
    }

    fn output(&self, status: i32) -> Output {
        Output {
            stdout: self.stdout.text(),
            stderr: self.stderr.text(),
            status,
        }
    }

    /// SIGKILL the whole group, then wait for the leader and take the
    /// group off `groups`. While it is listed the leader is not reaped, so
    /// its pid still names this group; once [`Groups::kill_all`] took it
    /// off, it is gone already.
    fn kill(&mut self, groups: &Groups) -> i32 {
        self.stdout.fd = None;
        self.stderr.fd = None;
        let mut listed = groups.lock();
        let Some(at) = listed.iter().position(|&group| group == self.pid.as_raw()) else {
            return -1;
        };
        listed.swap_remove(at);
        kill_and_reap(self.pid)
    }
}

/// SIGKILL a child's group, then wait for the child: its exit status.
fn kill_and_reap(pid: Pid) -> i32 {
    // ESRCH: the group is gone already.
    let _ = kill(Pid::from_raw(-pid.as_raw()), Signal::SIGKILL);
    loop {
        match waitpid(pid, None) {
            Ok(status) => return exit_status(status),
            Err(Errno::EINTR) => {}
            Err(_) => return -1,
        }
    }
}

/// A wait status as Fig's status: the exit code, or 0 after a signal.
fn exit_status(status: WaitStatus) -> i32 {
    match status {
        WaitStatus::Exited(_, code) => code,
        WaitStatus::Signaled(..) => 0,
        _ => -1,
    }
}

struct Queued {
    id: ProcId,
    command: Command,
    deadline: Instant,
}

/// The spec thread's children.
pub struct Runner {
    next: ProcId,
    queued: VecDeque<Queued>,
    running: Vec<Child>,
    finished: Vec<(ProcId, Result<Output, RunError>)>,
    groups: Groups,
}

impl Default for Runner {
    fn default() -> Self {
        Self::new()
    }
}

impl Runner {
    pub fn new() -> Self {
        Self::with_groups(Groups::default())
    }

    /// A runner that lists its children's groups in `groups`.
    pub fn with_groups(groups: Groups) -> Self {
        Self {
            next: 0,
            queued: VecDeque::new(),
            running: Vec::with_capacity(MAX_RUNNING),
            finished: Vec::new(),
            groups,
        }
    }

    /// Queue a command and start it when a slot is free.
    pub fn submit(&mut self, command: Command) -> ProcId {
        let id = self.next;
        self.next += 1;
        let deadline = Instant::now() + command.timeout;
        self.queued.push_back(Queued {
            id,
            command,
            deadline,
        });
        self.start_queued(Instant::now());
        id
    }

    /// Stop a command: a queued one is forgotten, a running one killed with
    /// its group and reaped. It gives no result.
    pub fn cancel(&mut self, id: ProcId) {
        self.queued.retain(|queued| queued.id != id);
        if let Some(at) = self.running.iter().position(|child| child.id == id) {
            let mut child = self.running.swap_remove(at);
            child.kill(&self.groups);
        }
        self.finished.retain(|(done, _)| *done != id);
        self.start_queued(Instant::now());
    }

    /// The open pipes, to poll for POLLIN.
    pub fn fds(&self) -> Vec<BorrowedFd<'_>> {
        self.running
            .iter()
            .flat_map(|child| [&child.stdout.fd, &child.stderr.fd])
            .filter_map(|fd| fd.as_ref().map(AsFd::as_fd))
            .collect()
    }

    /// When [`Runner::pump`] must run next without any fd being ready: the
    /// nearest deadline, or soon while a child runs (to see it exit).
    pub fn next_wake(&self, now: Instant) -> Option<Instant> {
        let deadlines = self
            .running
            .iter()
            .map(|child| {
                let slice = if child.closed() {
                    REAP_SLICE
                } else {
                    EXIT_SLICE
                };
                child.deadline.min(now + slice)
            })
            .chain(self.queued.iter().map(|queued| queued.deadline));
        let finished = (!self.finished.is_empty()).then_some(now);
        deadlines.chain(finished).min()
    }

    #[cfg(test)]
    pub fn is_idle(&self) -> bool {
        self.running.is_empty() && self.queued.is_empty() && self.finished.is_empty()
    }

    /// Read every pipe, reap the children that are done, kill the ones past
    /// their deadline, and start queued commands in the freed slots.
    pub fn pump(&mut self, now: Instant) {
        let mut at = 0;
        while at < self.running.len() {
            let child = &mut self.running[at];
            let mut overflow = child.stdout.read() | child.stderr.read();
            let done = !overflow && (child.closed() || child.exited());
            if done {
                // After the exit everything the child wrote is in its pipes.
                overflow = child.stdout.read() | child.stderr.read();
            }
            let result = if overflow {
                tracing::debug!(target: "specs", "a generator child wrote over {OUTPUT_CAP} bytes; killed");
                let status = child.kill(&self.groups);
                Some(Ok(child.output(status)))
            } else if done {
                let mut listed = self.groups.lock();
                match listed.iter().position(|&group| group == child.pid.as_raw()) {
                    // Reaped by `Groups::kill_all` already.
                    None => Some(Ok(child.output(-1))),
                    Some(at) => match waitpid(child.pid, Some(WaitPidFlag::WNOHANG)) {
                        Ok(WaitStatus::StillAlive) | Err(Errno::EINTR) => None,
                        Ok(status) => {
                            listed.swap_remove(at);
                            Some(Ok(child.output(exit_status(status))))
                        }
                        Err(_) => {
                            listed.swap_remove(at);
                            Some(Ok(child.output(-1)))
                        }
                    },
                }
            } else {
                None
            };
            let result = match result {
                None if now >= child.deadline => {
                    child.kill(&self.groups);
                    Some(Err(RunError::Timeout))
                }
                other => other,
            };
            match result {
                Some(result) => {
                    let child = self.running.swap_remove(at);
                    self.finished.push((child.id, result));
                }
                None => at += 1,
            }
        }
        self.start_queued(now);
    }

    /// What finished since the last take.
    pub fn take_finished(&mut self) -> Vec<(ProcId, Result<Output, RunError>)> {
        std::mem::take(&mut self.finished)
    }

    /// The running children's pids (for tests that look at their groups).
    #[cfg(test)]
    pub fn pids(&self) -> Vec<i32> {
        self.running
            .iter()
            .map(|child| child.pid.as_raw())
            .collect()
    }

    fn start_queued(&mut self, now: Instant) {
        while self.running.len() < MAX_RUNNING {
            let Some(queued) = self.queued.pop_front() else {
                break;
            };
            if now >= queued.deadline {
                self.finished.push((queued.id, Err(RunError::Timeout)));
                continue;
            }
            // Listed as it starts, so no child is ever running unlisted.
            let mut listed = self.groups.lock();
            let spawned = spawn(&queued.command);
            if let Ok((pid, ..)) = &spawned {
                listed.push(pid.as_raw());
            }
            drop(listed);
            match spawned {
                Ok((pid, stdout, stderr)) => self.running.push(Child {
                    id: queued.id,
                    pid,
                    stdout: Stream::new(stdout),
                    stderr: Stream::new(stderr),
                    deadline: queued.deadline,
                }),
                Err(err) => {
                    tracing::debug!(target: "specs", "a generator child did not start: {err}");
                    self.finished.push((queued.id, Err(RunError::Spawn(err))));
                }
            }
        }
    }
}

impl Drop for Runner {
    fn drop(&mut self) {
        for child in &mut self.running {
            child.kill(&self.groups);
        }
    }
}

/// The `PATH` of an environment, or execvp's default.
fn search_path(env: &[(String, String)]) -> &str {
    env.iter()
        .rev()
        .find(|(key, _)| key == "PATH")
        .map_or(DEFAULT_PATH, |(_, value)| value.as_str())
}

/// Find `program` as `execvp` would, in the child's `PATH` (not fac's) and
/// relative to the child's `cwd`.
fn resolve(program: &str, cwd: &Path, path: &str) -> Option<PathBuf> {
    if program.is_empty() {
        return None;
    }
    if program.contains('/') {
        return Some(cwd.join(program));
    }
    path.split(':')
        .map(|dir| cwd.join(dir).join(program))
        .find(|candidate| candidate.is_file() && access(candidate, AccessFlags::X_OK).is_ok())
}

fn c_string(text: &[u8]) -> Result<CString, String> {
    CString::new(text).map_err(|_| "a NUL byte in the command or its environment".to_owned())
}

/// A pipe as (reader, writer), FD_CLOEXEC on both ends and the reader
/// non-blocking. macOS has no pipe2.
fn pipe() -> Result<(OwnedFd, OwnedFd), String> {
    let fail = |err: Errno| format!("pipe: {err}");
    let (reader, writer) = nix::unistd::pipe().map_err(fail)?;
    for fd in [&reader, &writer] {
        let flags = FdFlag::from_bits_truncate(fcntl(fd, FcntlArg::F_GETFD).map_err(fail)?);
        fcntl(fd, FcntlArg::F_SETFD(flags | FdFlag::FD_CLOEXEC)).map_err(fail)?;
    }
    let flags = OFlag::from_bits_truncate(fcntl(&reader, FcntlArg::F_GETFL).map_err(fail)?);
    fcntl(&reader, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK)).map_err(fail)?;
    Ok((reader, writer))
}

/// `posix_spawn_file_actions_t`, destroyed on drop.
struct FileActions(libc::posix_spawn_file_actions_t);

impl FileActions {
    fn new() -> Result<Self, String> {
        let mut raw = std::mem::MaybeUninit::uninit();
        // SAFETY: init writes an initialised object into `raw`, which is
        // read only after it returned 0.
        let rc = unsafe { libc::posix_spawn_file_actions_init(raw.as_mut_ptr()) };
        if rc != 0 {
            return Err(format!("file actions: {}", Errno::from_raw(rc)));
        }
        // SAFETY: initialised by the successful call above.
        Ok(Self(unsafe { raw.assume_init() }))
    }

    fn check(rc: libc::c_int, what: &str) -> Result<(), String> {
        if rc == 0 {
            Ok(())
        } else {
            Err(format!("{what}: {}", Errno::from_raw(rc)))
        }
    }

    fn open_null(&mut self, fd: libc::c_int) -> Result<(), String> {
        // SAFETY: `self.0` is initialised; the path is a NUL-terminated
        // literal, which the call copies.
        let rc = unsafe {
            libc::posix_spawn_file_actions_addopen(
                &mut self.0,
                fd,
                c"/dev/null".as_ptr(),
                libc::O_RDONLY,
                0,
            )
        };
        Self::check(rc, "open /dev/null")
    }

    fn dup2(&mut self, from: &OwnedFd, to: libc::c_int) -> Result<(), String> {
        // SAFETY: `self.0` is initialised and `from` is an open fd that
        // outlives the spawn.
        let rc =
            unsafe { libc::posix_spawn_file_actions_adddup2(&mut self.0, from.as_raw_fd(), to) };
        Self::check(rc, "dup2")
    }

    fn chdir(&mut self, dir: &CString) -> Result<(), String> {
        // SAFETY: `self.0` is initialised; `dir` is NUL-terminated and the
        // call copies it.
        let rc = unsafe { libc::posix_spawn_file_actions_addchdir_np(&mut self.0, dir.as_ptr()) };
        Self::check(rc, "chdir")
    }
}

impl Drop for FileActions {
    fn drop(&mut self) {
        // SAFETY: `self.0` was initialised in `new` and is destroyed once.
        unsafe { libc::posix_spawn_file_actions_destroy(&mut self.0) };
    }
}

/// `posix_spawnattr_t`, destroyed on drop.
struct SpawnAttr(libc::posix_spawnattr_t);

impl SpawnAttr {
    /// New session, an empty signal mask, every signal at its default, and
    /// only the fds named in the file actions inherited.
    fn new() -> Result<Self, String> {
        let mut raw = std::mem::MaybeUninit::uninit();
        // SAFETY: init writes an initialised object into `raw`, which is
        // read only after it returned 0.
        let rc = unsafe { libc::posix_spawnattr_init(raw.as_mut_ptr()) };
        if rc != 0 {
            return Err(format!("spawn attributes: {}", Errno::from_raw(rc)));
        }
        // SAFETY: initialised by the successful call above.
        let mut attr = Self(unsafe { raw.assume_init() });
        let flags = libc::POSIX_SPAWN_CLOEXEC_DEFAULT
            | POSIX_SPAWN_SETSID
            | libc::POSIX_SPAWN_SETSIGMASK
            | libc::POSIX_SPAWN_SETSIGDEF;
        let flags = libc::c_short::try_from(flags).map_err(|_| "spawn flags".to_owned())?;
        // SAFETY: an all-zero sigset_t is a valid set for sigemptyset and
        // sigfillset to overwrite.
        let mut empty: libc::sigset_t = unsafe { std::mem::zeroed() };
        // SAFETY: as above.
        let mut all: libc::sigset_t = unsafe { std::mem::zeroed() };
        // SAFETY: both sets live on this stack; `attr.0` is initialised, and
        // the setters copy the sets.
        let rcs = unsafe {
            [
                libc::sigemptyset(&mut empty),
                libc::sigfillset(&mut all),
                libc::posix_spawnattr_setflags(&mut attr.0, flags),
                libc::posix_spawnattr_setsigmask(&mut attr.0, &empty),
                libc::posix_spawnattr_setsigdefault(&mut attr.0, &all),
            ]
        };
        match rcs.into_iter().find(|&rc| rc != 0) {
            None => Ok(attr),
            Some(rc) => Err(format!("spawn attributes: {}", Errno::from_raw(rc))),
        }
    }
}

impl Drop for SpawnAttr {
    fn drop(&mut self) {
        // SAFETY: `self.0` was initialised in `new` and is destroyed once.
        unsafe { libc::posix_spawnattr_destroy(&mut self.0) };
    }
}

/// Start `command`: its pid and the read ends of its stdout and stderr.
fn spawn(command: &Command) -> Result<(Pid, OwnedFd, OwnedFd), String> {
    let program = resolve(&command.program, &command.cwd, search_path(&command.env))
        .ok_or_else(|| "not found".to_owned())?;
    let path = c_string(program.as_os_str().as_bytes())?;
    let mut argv = vec![c_string(command.program.as_bytes())?];
    for arg in &command.args {
        argv.push(c_string(arg.as_bytes())?);
    }
    let envp = command
        .env
        .iter()
        .map(|(key, value)| c_string(format!("{key}={value}").as_bytes()))
        .collect::<Result<Vec<_>, _>>()?;
    let cwd = c_string(command.cwd.as_os_str().as_bytes())?;
    let mut argv_ptrs: Vec<*mut libc::c_char> =
        argv.iter().map(|arg| arg.as_ptr().cast_mut()).collect();
    argv_ptrs.push(std::ptr::null_mut());
    let mut envp_ptrs: Vec<*mut libc::c_char> =
        envp.iter().map(|var| var.as_ptr().cast_mut()).collect();
    envp_ptrs.push(std::ptr::null_mut());

    let (stdout_read, stdout_write) = pipe()?;
    let (stderr_read, stderr_write) = pipe()?;
    let mut actions = FileActions::new()?;
    actions.open_null(0)?;
    actions.dup2(&stdout_write, 1)?;
    actions.dup2(&stderr_write, 2)?;
    actions.chdir(&cwd)?;
    let attr = SpawnAttr::new()?;
    let mut pid: libc::pid_t = 0;
    // SAFETY: every pointer is valid for the call: the path, and the argv
    // and envp arrays (NUL-terminated strings, null-terminated arrays), live
    // in this frame; the actions and attributes are initialised. posix_spawn
    // does not keep any of them.
    let rc = unsafe {
        libc::posix_spawn(
            &mut pid,
            path.as_ptr(),
            &actions.0,
            &attr.0,
            argv_ptrs.as_ptr(),
            envp_ptrs.as_ptr(),
        )
    };
    if rc != 0 {
        return Err(Errno::from_raw(rc).to_string());
    }
    Ok((Pid::from_raw(pid), stdout_read, stderr_read))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(program: &str, args: &[&str]) -> Command {
        Command {
            program: program.to_owned(),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            cwd: PathBuf::from("/"),
            env: vec![("PATH".to_owned(), "/usr/bin:/bin".to_owned())],
            timeout: Duration::from_secs(10),
        }
    }

    fn run(runner: &mut Runner, command: Command) -> Result<Output, RunError> {
        let id = runner.submit(command);
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            runner.pump(Instant::now());
            if let Some((_, result)) = runner
                .take_finished()
                .into_iter()
                .find(|(done, _)| *done == id)
            {
                return result;
            }
            assert!(Instant::now() < deadline, "no result");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn clean_output_is_figs() {
        assert_eq!(clean_output("\n\na\r\nb\x1b[?25h\n\n"), "a\nb");
        assert_eq!(clean_output(" a \n"), " a ");
        assert_eq!(clean_output("\n"), "");
    }

    #[test]
    fn output_status_and_errors() {
        let mut runner = Runner::new();
        let out = run(
            &mut runner,
            command("sh", &["-c", "echo out; echo err >&2; exit 3"]),
        );
        assert_eq!(
            out,
            Ok(Output {
                stdout: "out".to_owned(),
                stderr: "err".to_owned(),
                status: 3
            })
        );
        // A signal gives Fig's status, 0.
        let out = run(&mut runner, command("sh", &["-c", "kill -TERM $$"]));
        assert_eq!(out.map(|out| out.status), Ok(0));
        assert!(matches!(
            run(&mut runner, command("no-such-program-fac", &[])),
            Err(RunError::Spawn(_))
        ));
        let mut missing = command("pwd", &[]);
        missing.cwd = PathBuf::from("/no/such/dir/fac");
        assert!(matches!(run(&mut runner, missing), Err(RunError::Spawn(_))));
        // The program is found in the child's PATH, not fac's.
        let mut no_path = command("ls", &[]);
        no_path.env = vec![("PATH".to_owned(), "/nonexistent".to_owned())];
        assert!(matches!(run(&mut runner, no_path), Err(RunError::Spawn(_))));
        assert!(runner.is_idle());
    }

    #[test]
    fn an_exited_child_is_done_while_its_pipe_is_still_open() {
        // The background sleep holds stdout open after the shell exits, as
        // a process that inherited the pipe's write end would; waiting for
        // EOF would take its 5 s.
        let mut runner = Runner::new();
        let started = Instant::now();
        let out = run(&mut runner, command("sh", &["-c", "sleep 5 & echo done"]));
        assert_eq!(out.map(|out| out.stdout), Ok("done".to_owned()));
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn output_past_the_cap_kills_the_child() {
        let mut runner = Runner::new();
        let out = run(&mut runner, command("yes", &[])).expect("output");
        assert_eq!(
            out.stdout.len(),
            OUTPUT_CAP - 1,
            "the cap, less the last newline"
        );
    }
}
