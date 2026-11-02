//! The compsys worker, driven directly: real hidden `zsh -f` workers with
//! zsh's default fpath plus the test-only completers in
//! `tests/fixtures/completions`, a full compinit into a temp dump, and a
//! temp fixture tree as the cwd.

use std::collections::HashSet;
use std::os::fd::BorrowedFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use fac::complete::source::{Candidate, Lookup, QueryKey};
use fac::complete::zsh_worker::{MAX_TRIES, RETRY_AFTER, Worker};
use fac::shell::install::write_scripts;
use fac::shell::protocol::ContextValue;
use nix::errno::Errno;
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::sys::signal::{Signal, kill};
use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::Pid;

/// Only a hang guard: `drive` returns as soon as its condition holds, so a
/// generous bound costs nothing and a loaded machine cannot trip it.
const TIMEOUT: Duration = Duration::from_secs(60);
const PREWARM: &[&str] = &["git ", "cd ", "ls -"];

/// A scratch directory, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "fac-worker-test-{label}-{}-{n}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// zsh's default fpath.
fn default_fpath() -> &'static [String] {
    static FPATH: OnceLock<Vec<String>> = OnceLock::new();
    FPATH.get_or_init(|| {
        let output = std::process::Command::new("/bin/zsh")
            .args(["-f", "-c", "print -rl -- $fpath"])
            .output()
            .expect("running zsh");
        String::from_utf8(output.stdout)
            .expect("fpath is UTF-8")
            .lines()
            .map(str::to_owned)
            .collect()
    })
}

struct Harness {
    worker: Worker,
    completed: HashSet<QueryKey>,
    cwd: String,
    dirs: Vec<TempDir>,
}

impl Harness {
    /// A started worker whose prewarm queries are done. `cwd`: the
    /// directory queries run in; by default a fixture tree with the
    /// directories `alpha` and `beta` and the files `file.txt` and
    /// `notes.md`.
    fn new(cwd: Option<&Path>) -> Self {
        Self::with_dump(cwd, "")
    }

    /// Like `new`, with `compdump` as the user's dump (empty: none).
    fn with_dump(cwd: Option<&Path>, compdump: &str) -> Self {
        Self::with_context(cwd, compdump, &[])
    }

    /// Like `with_dump`, with `early` context set before the worker starts,
    /// as when the shell's whole startup context arrives in one read.
    fn with_context(cwd: Option<&Path>, compdump: &str, early: &[(&str, ContextValue)]) -> Self {
        let scripts = TempDir::new("scripts");
        let cache = TempDir::new("cache");
        let tree = TempDir::new("tree");
        for dir in ["alpha", "beta"] {
            std::fs::create_dir(tree.0.join(dir)).expect("fixture dir");
        }
        for file in ["file.txt", "notes.md"] {
            std::fs::write(tree.0.join(file), "").expect("fixture file");
        }
        let cwd = cwd
            .unwrap_or(&tree.0)
            .to_str()
            .expect("UTF-8 path")
            .to_owned();
        let script_dir = write_scripts(&scripts.0).expect("writing the scripts");
        let mut worker = Worker::with_cache_dir(&script_dir, &cache.0);
        let completions =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/completions");
        let mut fpath = vec![completions.to_str().expect("UTF-8 path").to_owned()];
        fpath.extend_from_slice(default_fpath());
        let path = ["/usr/bin", "/bin", "/usr/sbin", "/sbin"].map(str::to_owned);
        worker.set_context("path", &ContextValue::List(path.to_vec()));
        worker.set_context("fpath", &ContextValue::List(fpath));
        worker.set_context("cwd", &ContextValue::Text(cwd.clone()));
        for (key, value) in early {
            worker.set_context(key, value);
        }
        worker.set_context("compdump", &ContextValue::Text(compdump.to_owned()));
        worker.start_if_ready();
        let mut harness = Self {
            worker,
            completed: HashSet::new(),
            cwd,
            dirs: vec![scripts, cache, tree],
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

    /// The results for `lbuffer`, from the cache or a query.
    ///
    /// The worker gives a query up after its 1 s budget (`QUERY_TIMEOUT`)
    /// and caches it as failed, which is what this sees, not a hang: on a
    /// loaded machine (the whole suite at once, every test starting its own
    /// worker) a cold completer can take longer than that, such as the
    /// prewarm's first `ls -`, which loads `_ls` and `_arguments`. Such a
    /// query is looked up again after the worker's [`RETRY_AFTER`], as fac
    /// does on a later key, and the worker asks it again then, up to its
    /// [`MAX_TRIES`] (a given-up prewarm query at once); one that fails for
    /// another reason (`err cwd`, `err init`) fails every time.
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

#[test]
fn ls_options_have_descriptions() {
    let mut h = Harness::new(None);
    let candidates = h.query("ls -");
    let long = candidates
        .iter()
        .find(|candidate| candidate.word == "-l")
        .unwrap_or_else(|| panic!("no -l in {:?}", words(&candidates)));
    assert!(!long.description.is_empty(), "{long:?}");
    assert!(long.display.contains(" -- "), "{long:?}");
    assert_eq!(long.prefix, "-", "{long:?}");
}

#[test]
fn cd_completes_only_directories() {
    let mut h = Harness::new(None);
    let candidates = h.query("cd ");
    let mut found = words(&candidates);
    found.sort_unstable();
    assert_eq!(found, ["alpha", "beta"], "{candidates:?}");
}

#[test]
fn parameters() {
    let mut h = Harness::new(None);
    let candidates = h.query("echo $HO");
    let home = candidates
        .iter()
        .find(|candidate| candidate.word == "HOME")
        .unwrap_or_else(|| panic!("no HOME in {:?}", words(&candidates)));
    show("echo $HO", home);
    assert_eq!((home.iprefix.as_str(), home.prefix.as_str()), ("$", "HO"));
    assert_eq!(home.insertion(), "HOME", "{home:?}");
}

/// Prints what a test buffer's candidate reports.
fn show(lbuffer: &str, candidate: &Candidate) {
    println!(
        "{lbuffer}: word={:?} apre={:?} hpre={:?} hsuf={:?} \
         iprefix={:?} prefix={:?} suffix={:?} is_file={} insertion={:?}",
        candidate.word,
        candidate.apre,
        candidate.hpre,
        candidate.hsuf,
        candidate.iprefix,
        candidate.prefix,
        candidate.suffix,
        candidate.is_file,
        candidate.insertion()
    );
}

#[test]
fn path_matches_keep_the_directory_in_the_hidden_prefix() {
    let mut h = Harness::new(None);
    let candidates = h.query("ls /usr/lo");
    let local = candidates
        .iter()
        .find(|candidate| candidate.word == "local")
        .unwrap_or_else(|| panic!("no local in {candidates:?}"));
    show("ls /usr/lo", local);
    assert_eq!(local.hpre, "/usr/", "{local:?}");
    assert_eq!(local.prefix, "/usr/lo", "{local:?}");
    assert!(local.is_file, "{local:?}");
    assert_eq!(local.insertion(), "/usr/local", "{local:?}");
}

#[test]
fn absolute_paths_into_the_fixture_tree() {
    let mut h = Harness::new(None);
    let sub = Path::new(&h.cwd).join("sub");
    std::fs::create_dir(&sub).expect("fixture dir");
    std::fs::write(sub.join("file-a"), "").expect("fixture file");
    let lbuffer = format!("ls {}/sub/fi", h.cwd);
    let candidates = h.query(&lbuffer);
    let file = candidates
        .iter()
        .find(|candidate| candidate.word == "file-a")
        .unwrap_or_else(|| panic!("no file-a in {candidates:?}"));
    show(&lbuffer, file);
    assert!(file.insertion().ends_with("/sub/file-a"), "{file:?}");
}

#[test]
fn git_subcommands() {
    let mut h = Harness::new(None);
    let candidates = h.query("git ch");
    if candidates.is_empty() {
        println!("git ch: no candidates; git's completer is unavailable, skipped");
        return;
    }
    for candidate in candidates.iter() {
        show("git ch", candidate);
    }
    for candidate in candidates.iter() {
        let insertion = candidate.insertion();
        assert!(
            insertion.starts_with("ch") || insertion == candidate.word,
            "{candidate:?}"
        );
    }
}

#[test]
fn a_slow_query_and_the_next_both_complete() {
    let mut h = Harness::new(None);
    // `slowcmd` must be the query in flight when `ls -` is queued, or `ls -`
    // replaces it in the queue. On a loaded machine a prewarm query can
    // outlive its 1 s budget: it is reported (as failed) but stays in flight
    // until the worker recovers. No timer means nothing is in flight.
    assert!(
        drive(&mut h, TIMEOUT, |h| h.worker.next_deadline().is_none()),
        "the worker never went idle"
    );
    // `ls -` was prewarmed; it has to be asked again.
    h.worker.invalidate();
    let (slow, ls) = (h.key("slowcmd "), h.key("ls -"));
    assert!(matches!(h.worker.lookup(&slow, ""), Lookup::Pending));
    assert!(matches!(h.worker.lookup(&ls, ""), Lookup::Pending));
    assert!(
        drive(&mut h, TIMEOUT, |h| h.completed.contains(&slow)
            && h.completed.contains(&ls)),
        "{:?}",
        h.completed
    );
    let Lookup::Ready(candidates) = h.worker.lookup(&slow, "") else {
        panic!("slowcmd is not cached");
    };
    assert_eq!(words(&candidates), ["one", "two"]);
    let Lookup::Ready(candidates) = h.worker.lookup(&ls, "") else {
        panic!("ls - is not cached");
    };
    assert!(words(&candidates).contains(&"-l"));
}

#[test]
fn only_the_newest_queued_query_runs() {
    let mut h = Harness::new(None);
    // `slowcmd` must be the query in flight, so the others queue behind it.
    // On a loaded machine a prewarm query can outlive its 1 s budget: it is
    // reported (as failed) but stays in flight until the worker recovers,
    // and `slowcmd` would then be queued and superseded like the rest. No
    // timer means nothing is in flight.
    assert!(
        drive(&mut h, TIMEOUT, |h| h.worker.next_deadline().is_none()),
        "the worker never went idle"
    );
    let slow = h.key("slowcmd ");
    let queued: Vec<QueryKey> = ["ls x1", "ls x2", "ls x3"]
        .iter()
        .map(|lbuffer| h.key(lbuffer))
        .collect();
    assert!(matches!(h.worker.lookup(&slow, ""), Lookup::Pending));
    for key in &queued {
        assert!(matches!(h.worker.lookup(key, ""), Lookup::Pending));
    }
    assert!(
        drive(&mut h, TIMEOUT, |h| h.completed.contains(&slow)
            && h.completed.contains(&queued[2])),
        "{:?}",
        h.completed
    );
    // Give a wrongly sent query time to arrive.
    drive(&mut h, Duration::from_millis(300), |_| false);
    assert!(!h.completed.contains(&queued[0]), "{:?}", h.completed);
    assert!(!h.completed.contains(&queued[1]), "{:?}", h.completed);
}

#[test]
fn a_killed_worker_restarts() {
    let mut h = Harness::new(None);
    let pid = h.pid().expect("a worker");
    kill(pid, Signal::SIGKILL).expect("SIGKILL");
    let candidates = h.query("ls -l");
    assert!(!candidates.is_empty());
    assert_ne!(h.pid(), Some(pid));
}

#[test]
fn a_hung_query_is_interrupted() {
    let mut h = Harness::new(None);
    let pid = h.pid();
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
        abandoned >= Duration::from_millis(900) && abandoned <= Duration::from_millis(1200),
        "{abandoned:?}"
    );
    assert!(
        matches!(h.worker.lookup(&hang, ""), Lookup::Failed),
        "the failed query is not cached as failed"
    );

    h.worker.invalidate();
    let start = Instant::now();
    let candidates = h.query("ls -");
    assert!(words(&candidates).contains(&"-l"));
    assert!(
        start.elapsed() < Duration::from_secs(4),
        "{:?}",
        start.elapsed()
    );
    // ^C was enough: the same worker answered.
    assert_eq!(h.pid(), pid);
}

#[test]
fn restarts_are_capped() {
    let mut h = Harness::new(None);
    let stuck = h.key("stuckcmd ");
    for round in 1..=4 {
        h.worker.invalidate();
        let pid = h.pid().expect("a worker");
        assert!(
            matches!(h.worker.lookup(&stuck, ""), Lookup::Pending),
            "round {round}"
        );
        // ^C after 1 s does nothing; the kill comes at 3 s.
        assert!(
            drive(&mut h, TIMEOUT, |h| h.pid() != Some(pid)),
            "round {round}: the stuck worker was never killed"
        );
        if round < 4 {
            assert!(h.pid().is_some(), "round {round}: no new worker");
        }
    }
    assert!(matches!(
        h.worker.lookup(&h.key("ls -"), ""),
        Lookup::Unavailable
    ));
}

#[test]
fn options_and_zstyles_are_mirrored() {
    let mut h = Harness::new(None);
    std::fs::create_dir(Path::new(&h.cwd).join(".dotdir")).expect("fixture dir");
    let sorted = |candidates: &[Candidate]| {
        let mut dirs = words(candidates);
        dirs.sort_unstable();
        dirs.join(" ")
    };
    assert_eq!(sorted(&h.query("cd ")), "alpha beta");
    h.worker.set_context(
        "options",
        &ContextValue::List(vec!["globdots".into(), "interactive".into()]),
    );
    h.worker.set_context(
        "zstyles",
        &ContextValue::Text("zstyle ':completion:*:*:cd:*' ignored-patterns alpha".into()),
    );
    assert_eq!(sorted(&h.query("cd ")), ".dotdir beta");
}

#[test]
fn the_users_functions_are_commands() {
    let mut h = Harness::new(None);
    let groups = |candidates: &[Candidate], word: &str| -> Vec<String> {
        candidates
            .iter()
            .filter(|candidate| candidate.word == word)
            .map(|candidate| candidate.group.clone())
            .collect()
    };
    assert!(groups(&h.query("facf"), "facfn").is_empty());
    // As many as oh-my-zsh defines, plus names that are something already
    // and zsh's special functions.
    let mut names: Vec<String> = (0..600).map(|n| format!("omzfn{n:03}")).collect();
    names.extend(
        ["facfn", "echo", "if", "TRAPDEBUG", "precmd", "chpwd"]
            .iter()
            .map(|name| (*name).to_owned()),
    );
    let started = Instant::now();
    h.worker
        .set_context("functions", &ContextValue::List(names.clone()));
    assert_eq!(groups(&h.query("facf"), "facfn"), ["functions"]);
    println!("600 stubs, then a query: {:?}", started.elapsed());
    let omz = h.query("omzfn");
    assert_eq!(
        omz.iter()
            .filter(|candidate| candidate.word.starts_with("omzfn"))
            .count(),
        600
    );
    // A builtin or a reserved word keeps what it is; no hook or trap stub.
    assert!(!groups(&h.query("ech"), "echo").contains(&"functions".to_owned()));
    assert!(!groups(&h.query("i"), "if").contains(&"functions".to_owned()));
    assert!(groups(&h.query("TRAPD"), "TRAPDEBUG").is_empty());
    assert!(groups(&h.query("prec"), "precmd").is_empty());
    assert!(groups(&h.query("chp"), "chpwd").is_empty());
    // A new list replaces the old stubs.
    h.worker
        .set_context("functions", &ContextValue::List(vec!["facfn2".to_owned()]));
    let candidates = h.query("facf");
    assert!(groups(&candidates, "facfn").is_empty(), "{candidates:?}");
    assert_eq!(groups(&candidates, "facfn2"), ["functions"]);
}

#[test]
fn a_users_dump_is_copied() {
    let dumps = TempDir::new("dump");
    let user = dumps.0.join("zcompdump");
    let status = std::process::Command::new("/bin/zsh")
        .args([
            "-f",
            "-c",
            "autoload -Uz compinit; compinit -d \"$1\"",
            "zsh",
        ])
        .arg(&user)
        .status()
        .expect("running zsh");
    assert!(status.success());
    let before = std::fs::read(&user).expect("reading the dump");
    let mut h = Harness::with_dump(None, user.to_str().expect("UTF-8 path"));
    assert!(words(&h.query("ls -")).contains(&"-l"));
    // The cache dir holds the private copy.
    let copies: Vec<_> = std::fs::read_dir(&h.dirs[1].0)
        .expect("reading the cache dir")
        .map(|entry| entry.expect("entry").path())
        .collect();
    assert_eq!(copies.len(), 1, "{copies:?}");
    assert_eq!(std::fs::read(&copies[0]).expect("reading the copy"), before);
    drop(h);
    assert_eq!(std::fs::read(&user).expect("reading the dump"), before);
}

/// The functions any shell that ran compinit has (`compinit` among them),
/// and an alias, all known before the worker starts: as when the shell's
/// startup output was held behind a slow probe (VS Code) and came in one
/// read.
#[test]
fn context_before_the_start_keeps_compinit() {
    let functions = [
        "compaudit",
        "compdef",
        "compdump",
        "compinit",
        "compinstall",
        "getent",
        "facfn",
    ]
    .map(str::to_owned);
    let early = [
        ("functions", ContextValue::List(functions.to_vec())),
        ("aliases", ContextValue::Text("alias ll='ls -l'".to_owned())),
        (
            "options",
            ContextValue::List(vec!["interactive".to_owned()]),
        ),
    ];
    let mut h = Harness::with_context(None, "", &early);
    assert!(words(&h.query("ls -")).contains(&"-l"));
    let candidates = h.query("facf");
    assert!(
        candidates
            .iter()
            .any(|candidate| candidate.word == "facfn" && candidate.group == "functions"),
        "{candidates:?}"
    );
    assert!(words(&h.query("l")).contains(&"ll"));
}

/// A user's dump that defines nothing (cut short by a session that ended,
/// or not a dump at all) passes `[[ -s ]]`: the private copy is rebuilt
/// once, and the user's file is left alone.
#[test]
fn a_users_dump_that_defines_nothing_is_rebuilt() {
    for (what, text) in [
        ("header only", "#files: 900\tversion: 5.9\n"),
        ("cut short", "#files: 900\tversion: 5.9\n\n_comps=(\n'-a"),
        ("not a dump", "\n"),
    ] {
        let dumps = TempDir::new("broken-dump");
        let user = dumps.0.join("zcompdump");
        std::fs::write(&user, text).expect("writing the dump");
        let mut h = Harness::with_dump(None, user.to_str().expect("UTF-8 path"));
        let candidates = h.query("ls -");
        assert!(words(&candidates).contains(&"-l"), "{what}: {candidates:?}");
        drop(h);
        assert_eq!(
            std::fs::read_to_string(&user).expect("reading the dump"),
            text,
            "{what}"
        );
    }
}

#[test]
fn dropping_the_worker_ends_it() {
    let h = Harness::new(None);
    let pids = h.worker.pids();
    assert!(!pids.is_empty());
    drop(h);
    for pid in pids {
        assert_eq!(kill(pid, None), Err(Errno::ESRCH), "{pid} is still there");
    }
}

#[test]
#[ignore = "timing table; run with --ignored --nocapture"]
fn timing() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut h = Harness::new(Some(repo));
    println!(
        "{:<16} {:>9} {:>9} {:>6}",
        "query", "first ms", "warm ms", "n"
    );
    for lbuffer in [
        "ls -",
        "ls -l",
        "cd ",
        "git ch",
        "git checkout ",
        "git commit --",
        "ssh ",
        "kill ",
        "make ",
        "echo $HO",
        "ls /usr/local/",
    ] {
        let mut times = Vec::new();
        let mut count = 0;
        for _ in 0..2 {
            h.worker.invalidate();
            let start = Instant::now();
            count = h.query(lbuffer).len();
            times.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        println!(
            "{:<16} {:>9.1} {:>9.1} {count:>6}",
            format!("{lbuffer:?}"),
            times[0],
            times[1]
        );
    }
}
