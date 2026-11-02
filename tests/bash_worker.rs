//! bash's completion worker, driven directly as `zsh_worker.rs` drives
//! zsh's: a real hidden bash 5.3 (`FAC_TEST_BASH`, else Homebrew's) that
//! sources bash-completion from `target/test-shells/` (`tools/
//! test-shells.sh`; `FAC_TEST_BASH_COMPLETION` names another loader), with a
//! temp HOME as the shell's own (its `env`), the test completions in
//! `tests/fixtures/bash-worker/completions` as the user's, and a temp
//! fixture tree as the cwd. Plus one session through the harness.

mod harness;

use std::collections::HashSet;
use std::os::fd::BorrowedFd;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use fac::complete::source::{Candidate, Lookup, QueryKey};
use fac::complete::zsh_worker::{MAX_TRIES, RETRY_AFTER, Worker, WorkerSpec};
use fac::guards::ShellKind;
use fac::shell::install::write_scripts;
use fac::shell::protocol::ContextValue;
use nix::errno::Errno;
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::Pid;

/// Only a hang guard: `drive` returns as soon as its condition holds.
const TIMEOUT: Duration = Duration::from_secs(60);
const PREWARM: &[&str] = &["git ", "cd ", "ls -"];
const PATH: [&str; 4] = ["/usr/bin", "/bin", "/usr/sbin", "/sbin"];

/// A scratch directory, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "fac-bash-worker-test-{label}-{}-{n}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create temp dir");
        // The canonical path: what `cd` and `$PWD` give.
        Self(path.canonicalize().expect("canonical temp dir"))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The bash the tests run. A missing one fails the test: never skipped.
fn bash() -> PathBuf {
    let path = std::env::var_os("FAC_TEST_BASH")
        .filter(|path| !path.is_empty())
        .map_or_else(|| PathBuf::from("/opt/homebrew/bin/bash"), PathBuf::from);
    assert!(path.is_file(), "no bash at {}", path.display());
    path
}

/// bash-completion's loader. A missing one fails the test.
fn loader() -> String {
    let path = std::env::var_os("FAC_TEST_BASH_COMPLETION")
        .filter(|path| !path.is_empty())
        .map_or_else(
            || crate_dir().join("target/test-shells/bash-completion/bash_completion"),
            PathBuf::from,
        );
    assert!(
        path.is_file(),
        "no bash-completion at {}: run tools/test-shells.sh",
        path.display()
    );
    path.to_str().expect("UTF-8 path").to_owned()
}

fn text(value: &str) -> ContextValue {
    ContextValue::Text(value.to_owned())
}

fn list(items: &[&str]) -> ContextValue {
    ContextValue::List(items.iter().map(|item| (*item).to_owned()).collect())
}

/// What a test sets up before the worker starts.
struct Setup<'a> {
    /// Send bash-completion's loader as `bash_completion`; otherwise the
    /// key is never sent, as from a shell without bash-completion.
    loader: bool,
    /// The shell's `complete -p` lines.
    complete: &'a [&'a str],
    /// More `NAME=value` pairs for the shell's `env`.
    env: &'a [&'a str],
    /// Files to write under HOME before the start: (relative path, text).
    home_files: &'a [(&'a str, &'a str)],
    /// The shell's PATH, if not [`PATH`].
    path: Option<&'a [&'a str]>,
}

impl Default for Setup<'_> {
    fn default() -> Self {
        Self {
            loader: true,
            complete: &[],
            env: &[],
            home_files: &[],
            path: None,
        }
    }
}

struct Harness {
    worker: Worker,
    completed: HashSet<QueryKey>,
    cwd: String,
    home: TempDir,
    tree: TempDir,
    _dirs: Vec<TempDir>,
}

impl Harness {
    fn new() -> Self {
        Self::with(Setup::default())
    }

    /// A started worker whose prewarm queries are done. The cwd is a tree
    /// with the directories `alpha` (holding `inner.txt`) and `beta` and
    /// the files `file.txt` and `notes.md`.
    fn with(setup: Setup<'_>) -> Self {
        let scripts = TempDir::new("scripts");
        let cache = TempDir::new("cache");
        let home = TempDir::new("home");
        let tree = TempDir::new("tree");
        for dir in ["alpha", "beta"] {
            std::fs::create_dir(tree.path().join(dir)).expect("fixture dir");
        }
        for file in ["file.txt", "notes.md", "alpha/inner.txt"] {
            std::fs::write(tree.path().join(file), "").expect("fixture file");
        }
        let user = home.path().join(".local/share/bash-completion/completions");
        std::fs::create_dir_all(&user).expect("user completions dir");
        let fixtures = crate_dir().join("tests/fixtures/bash-worker/completions");
        for entry in std::fs::read_dir(&fixtures).expect("fixtures") {
            let entry = entry.expect("fixture entry");
            std::fs::copy(entry.path(), user.join(entry.file_name())).expect("copy fixture");
        }
        for (path, body) in setup.home_files {
            let path = home.path().join(path);
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).expect("fixture dir");
            }
            std::fs::write(&path, body).expect("fixture file");
        }

        let script_dir = write_scripts(scripts.path()).expect("writing the scripts");
        let spec = WorkerSpec::new(ShellKind::Bash, &bash(), &script_dir);
        let mut worker = Worker::with_spec(spec, cache.path());
        let home_dir = home.path().to_str().expect("UTF-8 path").to_owned();
        let mut env = vec![
            format!("HOME={home_dir}"),
            format!("XDG_CONFIG_HOME={home_dir}/.config"),
            format!("XDG_DATA_HOME={home_dir}/.local/share"),
            format!("XDG_CACHE_HOME={home_dir}/.cache"),
            "LANG=en_US.UTF-8".to_owned(),
            "GIT_CONFIG_NOSYSTEM=1".to_owned(),
            format!("PATH={}", setup.path.unwrap_or(&PATH).join(":")),
        ];
        env.extend(
            setup
                .env
                .iter()
                .map(|pair| pair.replace("$HOME", &home_dir)),
        );
        let cwd = tree.path().to_str().expect("UTF-8 path").to_owned();
        worker.set_context("path", &list(setup.path.unwrap_or(&PATH)));
        worker.set_context("env", &ContextValue::List(env));
        worker.set_context("cwd", &text(&cwd));
        worker.set_context("complete", &list(setup.complete));
        if setup.loader {
            worker.set_context("bash_completion", &text(&loader()));
        }
        worker.start_if_ready();
        let mut harness = Self {
            worker,
            completed: HashSet::new(),
            cwd,
            home,
            tree,
            _dirs: vec![scripts, cache],
        };
        let prewarm: Vec<QueryKey> = PREWARM.iter().map(|lbuffer| harness.key(lbuffer)).collect();
        assert!(
            drive(&mut harness, TIMEOUT, |h| prewarm
                .iter()
                .all(|key| h.completed.contains(key))),
            "the prewarm queries never completed: {:?}",
            harness.completed
        );
        harness.completed.clear();
        harness
    }

    fn key(&self, lbuffer: &str) -> QueryKey {
        QueryKey {
            cwd: self.cwd.clone(),
            lbuffer: lbuffer.to_owned(),
        }
    }

    fn pid(&self) -> Option<Pid> {
        self.worker.pids().first().copied()
    }

    /// One round of the poll loop, waiting at most `max`.
    fn step(&mut self, max: Duration) {
        let fds = self.worker.poll_fds();
        let now = Instant::now();
        let wait = self.worker.next_deadline().map_or(max, |deadline| {
            deadline.saturating_duration_since(now).min(max)
        });
        let millis = u16::try_from(wait.as_micros().div_ceil(1000)).unwrap_or(u16::MAX);
        let mut polled: Vec<PollFd<'_>> = fds
            .iter()
            // SAFETY: the worker owns the fds and is not touched until
            // `polled` is dropped.
            .map(|&(fd, events)| PollFd::new(unsafe { BorrowedFd::borrow_raw(fd) }, events))
            .collect();
        match poll(&mut polled, PollTimeout::from(millis)) {
            Ok(_) | Err(Errno::EINTR) => {}
            Err(err) => panic!("poll: {err}"),
        }
        let ready: Vec<_> = fds
            .iter()
            .zip(&polled)
            .map(|(&(fd, _), pollfd)| (fd, pollfd.revents().unwrap_or(PollFlags::empty())))
            .collect();
        drop(polled);
        for (fd, revents) in ready {
            self.worker.on_ready(fd, revents);
        }
        self.worker.on_timer(Instant::now());
        // What the proxy does on SIGCHLD.
        for pid in self.worker.pids() {
            let status = match waitpid(pid, Some(WaitPidFlag::WNOHANG)) {
                Ok(WaitStatus::Exited(_, code)) => code,
                Ok(WaitStatus::Signaled(_, signal, _)) => 128 + signal as i32,
                _ => continue,
            };
            self.worker.on_child_exit(pid, status);
        }
        self.completed.extend(self.worker.take_completed());
    }

    /// The results for `lbuffer`, from the cache or a query. A query the
    /// worker gave up on (its 1 s budget: git's completion runs `git`,
    /// which a loaded machine can make slower than that) is looked up
    /// again after the worker's [`RETRY_AFTER`], as fac does on a later
    /// key; the worker asks it again then, up to its [`MAX_TRIES`].
    fn query(&mut self, lbuffer: &str) -> Arc<Vec<Candidate>> {
        let key = self.key(lbuffer);
        let deadline = Instant::now() + TIMEOUT;
        let mut failed = 0;
        loop {
            match self.worker.lookup(&key, "") {
                Lookup::Ready(candidates) => return candidates,
                Lookup::Unavailable => panic!("the worker is unavailable"),
                Lookup::Failed => {
                    failed += 1;
                    assert!(
                        failed < MAX_TRIES,
                        "the query for {lbuffer:?} failed {failed} times"
                    );
                    println!("the query for {lbuffer:?} was given up; waiting for the retry");
                    drive(self, RETRY_AFTER, |_| false);
                    continue;
                }
                Lookup::Pending => {}
            }
            self.completed.remove(&key);
            assert!(
                drive(
                    self,
                    deadline.saturating_duration_since(Instant::now()),
                    |h| h.completed.contains(&key)
                ),
                "no results for {lbuffer:?}"
            );
        }
    }
}

/// Run the poll loop until `done` or `timeout`. Whether `done` held.
fn drive(h: &mut Harness, timeout: Duration, done: impl Fn(&Harness) -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if done(h) {
            return true;
        }
        let now = Instant::now();
        if now >= deadline {
            return false;
        }
        h.step((deadline - now).min(Duration::from_millis(20)));
    }
}

fn words(candidates: &[Candidate]) -> Vec<&str> {
    candidates
        .iter()
        .map(|candidate| candidate.word.as_str())
        .collect()
}

fn find<'a>(candidates: &'a [Candidate], word: &str) -> &'a Candidate {
    candidates
        .iter()
        .find(|candidate| candidate.word == word)
        .unwrap_or_else(|| panic!("no {word:?} in {:?}", words(candidates)))
}

/// `git` in `dir`, with no system or user config.
fn git(dir: &Path, home: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args([
            "-c",
            "user.name=fac",
            "-c",
            "user.email=fac@example.invalid",
        ])
        .args([
            "-c",
            "init.defaultBranch=main",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("HOME", home)
        .env("PATH", PATH.join(":"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()
        .expect("running git");
    assert!(status.success(), "git {args:?}");
}

/// git's own completion (git-completion.bash, found from git itself) at
/// `git ch`: its subcommands, each followed by a space as git asks
/// (`-o nospace` and a trailing space).
#[test]
fn git_subcommands() {
    let mut h = Harness::new();
    let candidates = h.query("git ch");
    for word in ["checkout", "cherry-pick"] {
        let candidate = find(&candidates, word);
        assert_eq!(candidate.suffix.as_deref(), Some(" "), "{candidate:?}");
        assert_eq!(candidate.prefix, "ch", "{candidate:?}");
        assert!(candidate.verbatim && !candidate.is_file, "{candidate:?}");
        assert!(candidate.description.is_empty(), "{candidate:?}");
    }
    assert!(
        candidates.iter().all(|c| c.word.starts_with("ch")),
        "{:?}",
        words(&candidates)
    );
}

/// A git whose own completion takes 1.5 s to load, past the 1 s query
/// budget: the worker loads it before `ready`, so the first `git ` query
/// (the prewarm one) is answered.
#[test]
fn gits_completion_is_loaded_before_ready() {
    let prefix = TempDir::new("slow-git");
    let bin = prefix.path().join("bin");
    let share = prefix.path().join("share/git-core");
    std::fs::create_dir_all(&bin).expect("bin dir");
    std::fs::create_dir_all(&share).expect("share dir");
    let git = bin.join("git");
    std::fs::write(
        &git,
        format!(
            "#!/bin/sh\n[ \"$1\" = --exec-path ] && echo '{}/libexec/git-core'\n",
            prefix.path().display()
        ),
    )
    .expect("fake git");
    std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o755)).expect("chmod git");
    std::fs::write(
        share.join("git-completion.bash"),
        "sleep 1.5\n__fac_test_git() { COMPREPLY=(slowgit-sub); }\ncomplete -F __fac_test_git git\n",
    )
    .expect("git's completion");
    let bin = bin.to_str().expect("UTF-8 path");
    let path = [bin, "/usr/bin", "/bin", "/usr/sbin", "/sbin"];
    let mut h = Harness::with(Setup {
        loader: false,
        path: Some(&path),
        ..Setup::default()
    });
    let key = h.key("git ");
    match h.worker.lookup(&key, "") {
        Lookup::Ready(candidates) => assert_eq!(words(&candidates), ["slowgit-sub"]),
        other => panic!("the first `git ` query: {other:?}"),
    }
}

/// `git checkout ` lists the branches of the repository in the cwd.
#[test]
fn git_checkout_branches() {
    let mut h = Harness::new();
    let repo = h.tree.path().join("repo");
    std::fs::create_dir(&repo).expect("repo dir");
    git(&repo, h.home.path(), &["init", "--quiet"]);
    git(
        &repo,
        h.home.path(),
        &["commit", "--quiet", "--allow-empty", "-m", "first"],
    );
    git(&repo, h.home.path(), &["branch", "fac-feature"]);
    h.cwd = repo.to_str().expect("UTF-8 path").to_owned();
    let candidates = h.query("git checkout ");
    for branch in ["main", "fac-feature"] {
        find(&candidates, branch);
    }
}

/// `ssh ` offers the hosts of `~/.ssh/config` in the shell's HOME.
#[test]
fn ssh_hosts_from_the_users_config() {
    let mut h = Harness::with(Setup {
        home_files: &[(
            ".ssh/config",
            "Host fac-alpha\n  HostName alpha.example.invalid\nHost fac-beta\n",
        )],
        ..Setup::default()
    });
    let candidates = h.query("ssh ");
    for host in ["fac-alpha", "fac-beta"] {
        find(&candidates, host);
    }
}

/// `make ` offers the targets of the Makefile in the cwd.
#[test]
fn make_targets() {
    let mut h = Harness::new();
    std::fs::write(
        h.tree.path().join("Makefile"),
        "all: fac-build\n\nfac-build:\n\t@true\n\nfac-clean:\n\t@true\n",
    )
    .expect("Makefile");
    let candidates = h.query("make ");
    for target in ["fac-build", "fac-clean"] {
        find(&candidates, target);
    }
}

/// `kill -` offers the signals.
#[test]
fn kill_signals() {
    let mut h = Harness::new();
    let candidates = h.query("kill -");
    for signal in ["-HUP", "-KILL", "-TERM"] {
        let candidate = find(&candidates, signal);
        assert_eq!(candidate.prefix, "-", "{candidate:?}");
    }
}

/// The shell's `complete` lines are replayed, quoting and all, and a line
/// that changes later is replayed then.
#[test]
fn complete_lines_are_replayed() {
    let mut h = Harness::with(Setup {
        complete: &[
            "complete -W 'alpha beta' facw",
            // `complete -p` quotes a `'` as `'\''`; -P is taken as it is.
            r"complete -o nospace -P 'it'\''s-' -W 'one two' -- facq",
            // Never a removal or a listing.
            "complete -r facw",
        ],
        ..Setup::default()
    });
    let mut found = words(&h.query("facw ")).join(" ");
    assert_eq!(found, "alpha beta");
    let candidates = h.query("facq ");
    let one = find(&candidates, "it's-one");
    assert_eq!(one.suffix.as_deref(), Some(""), "{one:?}");
    find(&candidates, "it's-two");
    h.worker.set_context(
        "complete",
        &list(&["complete -W 'alpha beta' facw", "complete -W 'gamma' facw"]),
    );
    found = words(&h.query("facw ")).join(" ");
    assert_eq!(found, "gamma");
    // And back: a line seen before is replayed again.
    h.worker
        .set_context("complete", &list(&["complete -W 'alpha beta' facw"]));
    found = words(&h.query("facw ")).join(" ");
    assert_eq!(found, "alpha beta");
}

/// The shell's specs as the worker runs them: one whose function
/// bash-completion loads lazily, one whose function the worker cannot have
/// (from the user's rc), and a `-C` command, which sees COMP_LINE.
#[test]
fn replayed_specs_run_here() {
    let mut h = Harness::with(Setup {
        complete: &[
            "complete -F _comp_cmd_ssh ssh",
            "complete -o default -F _fac_users_own facmissing",
            "complete -C '$HOME/facc-completer' facc",
        ],
        home_files: &[
            (".ssh/config", "Host fac-alpha\n"),
            (
                "facc-completer",
                "#!/bin/sh\nprintf '%s\\n' c-one \"line=$COMP_LINE\"\n",
            ),
        ],
        ..Setup::default()
    });
    let completer = h.home.path().join("facc-completer");
    std::fs::set_permissions(
        &completer,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .expect("chmod");
    find(&h.query("ssh "), "fac-alpha");
    // No `_fac_users_own` here: readline's default, file names.
    let candidates = h.query("facmissing ");
    find(&candidates, "file.txt");
    assert!(candidates.iter().all(|c| c.is_file), "{candidates:?}");
    let candidates = h.query("facc ");
    find(&candidates, "c-one");
    // `facc ` (a word's one trailing space is its separator, as git's are).
    find(&candidates, "line=facc");
}

/// `compopt -o nospace` in a spec function: the match takes no suffix.
#[test]
fn nospace_is_captured() {
    let mut h = Harness::new();
    let candidates = h.query("facnospace --");
    let name = find(&candidates, "--name=");
    assert_eq!(name.suffix.as_deref(), Some(""), "{name:?}");
    assert_eq!(name.prefix, "--", "{name:?}");
}

/// `compopt -o filenames`: file matches, the directory part of a path in
/// the hidden prefix, as readline shows only the name.
#[test]
fn filenames_are_captured() {
    let mut h = Harness::new();
    let candidates = h.query("facfiles ");
    let file = find(&candidates, "file.txt");
    assert!(file.is_file && !file.verbatim, "{file:?}");
    assert_eq!(file.suffix, None, "{file:?}");
    find(&candidates, "alpha");
    let candidates = h.query("facfiles alpha/");
    let inner = find(&candidates, "inner.txt");
    assert_eq!(inner.hpre, "alpha/", "{inner:?}");
    assert_eq!(inner.prefix, "alpha/", "{inner:?}");
    assert_eq!(inner.insertion(), "alpha/inner.txt", "{inner:?}");
    // A word after `=`: readline completes after it, and the shell word
    // before is the iprefix.
    let candidates = h.query("facfiles --out=");
    let file = find(&candidates, "file.txt");
    assert_eq!(
        (file.iprefix.as_str(), file.prefix.as_str()),
        ("--out=", ""),
        "{file:?}"
    );
}

/// bash-completion's `cd` completes directories only.
#[test]
fn cd_completes_directories() {
    let mut h = Harness::new();
    let candidates = h.query("cd ");
    let mut found = words(&candidates);
    found.sort_unstable();
    assert_eq!(found, ["alpha", "beta"], "{candidates:?}");
    assert!(candidates.iter().all(|c| c.is_file), "{candidates:?}");
}

/// A spec function that never returns: the query is given up after 1 s
/// (failed), the worker that does not recover is killed and restarted, and
/// the next query works.
#[test]
fn a_hung_spec_restarts_the_worker() {
    let mut h = Harness::with(Setup {
        complete: &["complete -W 'alpha beta' facw"],
        ..Setup::default()
    });
    let pid = h.pid().expect("a worker");
    let hang = h.key("hangcmd ");
    let start = Instant::now();
    assert!(matches!(h.worker.lookup(&hang, ""), Lookup::Pending));
    assert!(
        drive(&mut h, TIMEOUT, |h| h.completed.contains(&hang)),
        "the hung query was never given up"
    );
    let abandoned = start.elapsed();
    println!("hangcmd abandoned after {abandoned:?}");
    assert!(
        abandoned >= Duration::from_millis(900) && abandoned <= Duration::from_millis(1500),
        "{abandoned:?}"
    );
    assert!(matches!(h.worker.lookup(&hang, ""), Lookup::Failed));
    assert!(
        drive(&mut h, TIMEOUT, |h| h.pid() != Some(pid)),
        "the hung worker was never killed"
    );
    assert!(h.pid().is_some(), "no new worker");
    assert_eq!(words(&h.query("facw ")).join(" "), "alpha beta");
}

/// A spec function waiting in a command: ^C ends the command, the query
/// fails, and the same worker answers the next one.
#[test]
fn an_interrupted_spec_recovers() {
    let mut h = Harness::new();
    let pid = h.pid();
    let slow = h.key("slowcmd ");
    assert!(matches!(h.worker.lookup(&slow, ""), Lookup::Pending));
    assert!(
        drive(&mut h, TIMEOUT, |h| h.completed.contains(&slow)),
        "the slow query was never given up"
    );
    assert!(matches!(h.worker.lookup(&slow, ""), Lookup::Failed));
    let start = Instant::now();
    let candidates = h.query("kill -");
    find(&candidates, "-HUP");
    assert!(
        start.elapsed() < Duration::from_secs(3),
        "{:?}",
        start.elapsed()
    );
    // ^C was enough: no restart.
    assert_eq!(h.pid(), pid);
}

/// No bash-completion (no `bash_completion` key): the worker starts on
/// the `complete` lines, and compgen answers their specs.
#[test]
fn without_bash_completion() {
    let mut h = Harness::with(Setup {
        loader: false,
        complete: &["complete -W 'alpha beta' facw", "complete -d facd"],
        ..Setup::default()
    });
    assert_eq!(words(&h.query("facw a")).join(" "), "alpha");
    let candidates = h.query("facd ");
    let mut dirs = words(&candidates);
    dirs.sort_unstable();
    assert_eq!(dirs, ["alpha", "beta"]);
    assert!(candidates.iter().all(|c| c.is_file), "{candidates:?}");
    // No spec: readline completes file names.
    let candidates = h.query("facnone ");
    find(&candidates, "file.txt");
    assert!(candidates.iter().all(|c| c.is_file), "{candidates:?}");
}

/// The user's rc files, bash-completion's user file and BASH_ENV never
/// run in the worker.
#[test]
fn the_users_rc_files_never_run() {
    const MARK: &str = "touch \"$HOME/ran-$(basename \"${BASH_SOURCE[0]}\")\"\n";
    let files = [
        ".bashrc",
        ".bash_profile",
        ".bash_login",
        ".profile",
        ".bash_completion",
        "bash-env.bash",
    ];
    let home_files: Vec<(&str, &str)> = files.iter().map(|file| (*file, MARK)).collect();
    let mut h = Harness::with(Setup {
        home_files: &home_files,
        // Assigned after the start, so it is only a value then.
        env: &["BASH_ENV=$HOME/bash-env.bash"],
        ..Setup::default()
    });
    find(&h.query("kill -"), "-HUP");
    let ran: Vec<_> = std::fs::read_dir(h.home.path())
        .expect("reading HOME")
        .filter_map(|entry| {
            let name = entry.ok()?.file_name().into_string().ok()?;
            name.starts_with("ran-").then_some(name)
        })
        .collect();
    assert!(ran.is_empty(), "{ran:?}");
}

/// A word of the line, quoting and all, reaches the spec as readline
/// would split it.
#[test]
fn the_current_command_is_completed() {
    let mut h = Harness::with(Setup {
        complete: &["complete -W 'alpha beta' facw"],
        ..Setup::default()
    });
    assert_eq!(words(&h.query("echo hi; facw ")).join(" "), "alpha beta");
    assert_eq!(words(&h.query("FOO=1 facw b")).join(" "), "beta");
    let candidates = h.query("facw 'a");
    let alpha = find(&candidates, "alpha");
    assert_eq!(alpha.prefix, "a", "{alpha:?}");
}

/// Per query, after a warm-up: run with `--ignored --nocapture`.
#[test]
#[ignore = "timing table; run with --ignored --nocapture"]
fn timing() {
    const RUNS: usize = 20;
    let mut h = Harness::with(Setup {
        home_files: &[(".ssh/config", "Host fac-alpha\nHost fac-beta\n")],
        ..Setup::default()
    });
    std::fs::write(
        h.tree.path().join("Makefile"),
        "all: fac-build\nfac-build:\n\t@true\n",
    )
    .expect("Makefile");
    let repo = crate_dir();
    println!(
        "{:<16} {:>9} {:>9} {:>9} {:>6}",
        "query", "first ms", "median", "worst", "n"
    );
    for lbuffer in [
        "",
        "git ",
        "git ch",
        "git checkout ",
        "ssh ",
        "make ",
        "kill -",
    ] {
        if lbuffer.starts_with("git") {
            h.cwd = repo.to_str().expect("UTF-8 path").to_owned();
        } else {
            h.cwd = h.tree.path().to_str().expect("UTF-8 path").to_owned();
        }
        let mut times = Vec::new();
        let mut count = 0;
        for _ in 0..=RUNS {
            h.worker.invalidate();
            let start = Instant::now();
            count = h.query(lbuffer).len();
            times.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        let first = times.remove(0);
        times.sort_by(f64::total_cmp);
        println!(
            "{:<16} {first:>9.1} {:>9.1} {:>9.1} {count:>6}",
            format!("{lbuffer:?}"),
            times[times.len() / 2],
            times[times.len() - 1]
        );
    }
}

// Through fac: a bash session whose test rc sends the worker's context
// itself, with the helpers in tests/fixtures/trigger/bash.bash.

/// `git ch` in a bash session shows the worker's items: git's own
/// completion, which lists an external `git-chfacx` on PATH (Fig's spec
/// knows nothing of it).
#[test]
fn git_ch_shows_the_workers_items() {
    use harness::{Session, SpawnOpts, wait_log};

    let fixture = crate_dir().join("tests/fixtures/trigger/bash.bash");
    let rc = format!(
        r#"cd "$HOME"
mkdir -p "$HOME/bin"
printf '#!/bin/sh\n' > "$HOME/bin/git-chfacx"
chmod +x "$HOME/bin/git-chfacx"
PATH=$HOME/bin:$PATH
source '{}'
__fact_context path "${{PATH//:/,}}"
__fact_encode "$FAC_TEST_LOADER"
__fact_context bash_completion "$__fact_s"
__fact_context complete ''
"#,
        fixture.display()
    );
    let s = Session::spawn(SpawnOpts {
        shell: harness::ShellKind::Bash,
        rc_extra: Some(rc),
        extra_env: vec![("FAC_TEST_LOADER".to_owned(), loader())],
        ..SpawnOpts::default()
    });
    wait_log(&s, "the shell's report trigger is on");
    wait_log(&s, "worker ready load_ms=");
    let colors = harness::colors();
    let has = |card: Option<&harness::Card>, screen: &vt100::Screen, wanted: &[&str]| {
        card.is_some_and(|card| {
            let labels = harness::labels(screen, card, &colors);
            wanted.iter().all(|w| labels.iter().any(|l| l == w))
        })
    };
    s.send(b"git ch");
    s.wait_card(
        "a popup with checkout and cherry-pick",
        |card, screen| has(card, screen, &["checkout", "cherry-pick"]),
        harness::TIMEOUT,
    );
    s.send(b"f");
    // On a loaded machine the worker's `git ` query can be given up (git's
    // completion runs `git`): fac asks it again on a key typed
    // `RETRY_AFTER` later, so one is typed then.
    let given_up = |log: &str| {
        let ids: Vec<&str> = log
            .lines()
            .filter(|line| line.contains("worker query id=") && line.contains(r#"key="git " in"#))
            .filter_map(|line| line.split("worker query id=").nth(1)?.split(' ').next())
            .collect();
        log.lines()
            .filter_map(|line| {
                line.split("worker query timed out id=")
                    .nth(1)?
                    .split(' ')
                    .next()
            })
            .filter(|id| ids.contains(id))
            .count()
    };
    let mut handled = 0;
    let mut deadline = Instant::now() + harness::TIMEOUT;
    loop {
        let card = s.card();
        if s.with_screen(|screen| has(card.as_ref(), screen, &["chfacx"])) {
            break;
        }
        let log = s.log_text();
        let n = given_up(&log);
        if n > handled {
            assert!(
                n < MAX_TRIES as usize,
                "the worker's `git ` query was given up {n} times:\n{log}"
            );
            handled = n;
            println!("the worker's `git ` query was given up ({n}); typing again after the pause");
            std::thread::sleep(RETRY_AFTER);
            s.send(b"\x7ff");
            deadline = Instant::now() + harness::TIMEOUT;
            continue;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for a popup with chfacx\n--- card ---\n{card:?}\n--- screen ---\n{}\n--- log ---\n{log}",
            s.screen_text()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    // The worker answered `git `, the line cut at the word. (The log is
    // written in the background: its line can come after the card.)
    let deadline = Instant::now() + harness::TIMEOUT;
    let id = loop {
        let log = s.log_text();
        let id = log
            .lines()
            .filter(|line| line.contains("worker query id=") && line.contains(r#"key="git " in"#))
            .filter_map(|line| line.split("worker query id=").nth(1)?.split(' ').next())
            .find(|id| log.contains(&format!("worker done id={id} ")))
            .map(str::to_owned);
        if let Some(id) = id {
            break id;
        }
        assert!(
            Instant::now() < deadline,
            "the worker never answered \"git \":\n{log}"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    println!("worker query id={id} answered");
}
