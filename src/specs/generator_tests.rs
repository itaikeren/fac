//! The spec thread running generators, end to end: real child processes in
//! temp directories (a git repo, a `package.json`), the vendored specs, and
//! small user specs.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use nix::errno::Errno;
use nix::sys::signal::kill;
use nix::unistd::Pid;

use super::generators::GenError;
use super::model::{Suggestion, SuggestionType};
use super::parser::{Answer, CurrentArg, Need, ParseResult, Resolved, parse};
use super::runtime::tests::Probe;
use super::runtime::{Env, Job, LoadedSpec, Outcome, SpecVersion};

type Generated = (Result<Vec<Suggestion>, GenError>, bool);

/// A fresh temp directory for one test.
fn temp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("fac-gen-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

fn write(path: &Path, text: &str) {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).expect("mkdir");
    }
    std::fs::write(path, text).expect("write");
}

/// An executable shell script.
fn script(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    write(path, &format!("#!/bin/sh\n{body}\n"));
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

/// The environment a shell would send: `PATH` (with `first` in front) and
/// `HOME`.
fn env(home: &Path, first: Option<&Path>) -> Env {
    let mut path = String::new();
    if let Some(first) = first {
        path = format!("{}:", first.display());
    }
    path.push_str("/usr/bin:/bin:/usr/sbin:/sbin");
    Arc::new(vec![
        ("PATH".to_owned(), path),
        ("HOME".to_owned(), home.display().to_string()),
        ("FAC_MARK".to_owned(), "marked".to_owned()),
    ])
}

/// A temp HOME with user specs.
fn home_with(tag: &str, specs: &[(&str, &str)]) -> PathBuf {
    let home = temp(tag);
    for (name, source) in specs {
        write(
            &home.join(".config/fac/specs").join(format!("{name}.js")),
            source,
        );
    }
    home
}

fn tokens(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_owned()).collect()
}

struct Request<'a> {
    spec: &'a str,
    path: &'a str,
    index: usize,
    tokens: Vec<String>,
    cwd: &'a Path,
    env: Env,
}

fn send(probe: &Probe, id: u64, request: &Request<'_>) {
    probe.thread.send(Job::Generate {
        id,
        spec: request.spec.to_owned(),
        generation: probe.generation(request.spec),
        arg_path: request.path.to_owned(),
        generator_index: request.index,
        tokens: request.tokens.clone(),
        search_term: request.tokens.last().cloned().unwrap_or_default(),
        cwd: request.cwd.to_path_buf(),
        env: Arc::clone(&request.env),
        generator: None,
    });
}

/// The next `Generated`; other outcomes are skipped.
fn next_generated(probe: &mut Probe) -> (u64, Generated) {
    loop {
        if let Outcome::Generated { id, result, stale } = probe.next() {
            return (id, (result, stale));
        }
    }
}

/// Run one generator and take its (first) answer.
fn run(probe: &mut Probe, id: u64, request: &Request<'_>) -> Generated {
    send(probe, id, request);
    let (got, answer) = next_generated(probe);
    assert_eq!(got, id);
    answer
}

fn names(result: &Result<Vec<Suggestion>, GenError>) -> Vec<String> {
    match result {
        Ok(suggestions) => suggestions
            .iter()
            .map(|s| s.name.first().cloned().unwrap_or_default())
            .collect(),
        Err(err) => panic!("{err}"),
    }
}

/// The pids of the thread's running children.
fn children(probe: &mut Probe) -> Vec<i32> {
    probe.thread.send(Job::Children);
    loop {
        if let Outcome::Children(pids) = probe.next() {
            return pids;
        }
    }
}

/// Whether process group `pgid` is gone (ESRCH), waiting a little for the
/// kernel to finish with it.
fn group_gone(pgid: i32) -> bool {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if kill(Pid::from_raw(-pgid), None) == Err(Errno::ESRCH) {
            return true;
        }
        if Instant::now() > deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Specs loaded through the probe; every function need fails, as in a
/// shell where git's `generateSpec` finds nothing more.
struct Specs(HashMap<String, Arc<LoadedSpec>>);

impl Resolved for Specs {
    fn spec(&self, name: &str) -> Option<Arc<LoadedSpec>> {
        self.0.get(name).cloned()
    }

    fn answer(&self, _: &Need) -> Option<Arc<Answer>> {
        Some(Arc::new(Answer::Failed))
    }
}

/// The arg being typed at `words` + `current` in a loaded spec.
fn current_arg(spec: &Arc<LoadedSpec>, words: &[&str], current: &str) -> CurrentArg {
    let specs = Specs(HashMap::from([(spec.name.clone(), Arc::clone(spec))]));
    match parse(spec, &tokens(words), current, &specs) {
        ParseResult::Ready(suggest) => suggest.arg.expect("an arg"),
        ParseResult::Needs(need) => panic!("{need:?}"),
    }
}

fn sh(dir: &Path, command: &str) {
    let status = Command::new("/bin/sh")
        .args(["-c", command])
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", dir)
        .status()
        .expect("sh");
    assert!(status.success(), "{command}");
}

#[test]
fn git_checkout_lists_the_branches() {
    let repo = temp("git");
    sh(
        &repo,
        "git init -q -b main && \
         git -c user.name=t -c user.email=t@example.com commit -q --allow-empty -m one && \
         git branch feature/a && git branch fix-b && git tag v1.0 && \
         git -c user.name=t -c user.email=t@example.com commit -q --allow-empty -m two",
    );
    let mut probe = Probe::new();
    let git = probe.loaded("git");
    let arg = current_arg(&git, &["checkout"], "");
    assert!(arg.generators.len() >= 3, "{:?}", arg.generators);
    let request = |index: usize| Request {
        spec: "git",
        path: &arg.path,
        index: arg.generators[index].index,
        tokens: tokens(&["git", "checkout", ""]),
        cwd: &repo,
        env: env(&repo, None),
    };
    let (branches, stale) = run(&mut probe, 1, &request(0));
    assert!(!stale);
    let mut branches = names(&branches);
    branches.sort();
    assert_eq!(branches, ["feature/a", "fix-b", "main"]);
    let (tags, _) = run(&mut probe, 2, &request(1));
    assert_eq!(names(&tags), ["v1.0"]);
    // A template generator is the poll thread's.
    let (template, _) = run(&mut probe, 3, &request(2));
    assert_eq!(names(&template), Vec::<String>::new());
    let _ = std::fs::remove_dir_all(repo);
}

#[test]
fn npm_lists_scripts_and_dependencies() {
    let project = temp("npm");
    write(
        &project.join("package.json"),
        r#"{"name": "fixture", "scripts": {"build": "tsc", "test": "vitest"},
            "dependencies": {"left-pad": "1.0.0"}, "devDependencies": {"vitest": "1.0.0"}}"#,
    );
    // `npm prefix` prints the project's directory.
    let bin = project.join("bin");
    script(&bin.join("npm"), "pwd");
    let mut probe = Probe::new();
    let npm = probe.loaded("npm");
    let env = env(&project, Some(&bin));

    // `npm run`: a script generator (`bash -c ... cat package.json`) with a
    // postProcess, cached stale-while-revalidate by directory.
    let run_arg = current_arg(&npm, &["run"], "");
    let (scripts, stale) = run(
        &mut probe,
        1,
        &Request {
            spec: "npm",
            path: &run_arg.path,
            index: 0,
            tokens: tokens(&["npm", "run", ""]),
            cwd: &project,
            env: Arc::clone(&env),
        },
    );
    assert!(!stale, "the first use has nothing cached");
    assert_eq!(names(&scripts), ["build", "test"]);
    let build = &scripts.as_ref().expect("scripts")[0];
    assert_eq!(build.description.as_deref(), Some("tsc"));
    assert_eq!(build.kind, Some(SuggestionType::Arg));

    // `npm uninstall`: a custom generator that runs `npm prefix` and `cat`
    // through executeShellCommand.
    let uninstall = current_arg(&npm, &["uninstall"], "");
    let generator = &uninstall.generators[0].generator;
    assert!(generator.custom.is_some(), "{generator:?}");
    let (dependencies, _) = run(
        &mut probe,
        2,
        &Request {
            spec: "npm",
            path: &uninstall.path,
            index: 0,
            tokens: tokens(&["npm", "uninstall", ""]),
            cwd: &project,
            env,
        },
    );
    let mut dependencies = names(&dependencies);
    dependencies.sort();
    assert_eq!(dependencies, ["left-pad", "vitest"]);
    let _ = std::fs::remove_dir_all(project);
}

const GEN: &str = r#"
export default {
  name: "gen",
  subcommands: [
    { name: "hang", args: { generators: { script: ["sleep", "30"], splitOn: "\n" } } },
    { name: "split", args: { generators: [
      { script: ["printf", "a\nb\n"], splitOn: "\n", postProcess: () => [{ name: "never" }] },
      { script: ["printf", "a\nb\n"] },
      { script: ["printf", "x"], postProcess: (out, tokens) =>
          [out, { name: tokens[1], description: "d" }, { name: "" }, null, 5, { description: "no name" }] },
      { script: (tokens) => ["printf", tokens.slice(0, 2).join("+")], splitOn: "\n" },
      { script: ["printf", "a"], postProcess: async () => ["late"] },
      { script: () => null, splitOn: "\n" },
      { script: ["printf", "a"], postProcess: () => { for (;;) {} } },
    ] } },
    { name: "exec", args: { isDangerous: true, generators: [
      { custom: async (tokens, exec) => {
          const a = await exec({ command: "echo", args: ["hi"] });
          const b = await exec("echo  there ");
          const c = await exec({ command: "false", args: [] });
          const d = await exec({ command: "sh", args: ["-c", "echo $FAC_X-$FAC_MARK"], env: { FAC_X: "set" } });
          const e = await exec({ command: "pwd", args: [], cwd: "sub" });
          const outcome = (promise) => promise.then(() => "resolved", () => "rejected");
          const f = await outcome(exec({ command: "no-such-program-fac", args: [] }));
          const g = await outcome(exec({ command: "echo" }));
          return [a.stdout, b.stdout, String(c.status), d.stdout, e.stdout.split("/").pop(), f, g]
            .map((name) => ({ name }));
      } },
      { custom: async (tokens, exec, context) => [
          context.currentWorkingDirectory.split("/").pop(), context.currentProcess, context.searchTerm,
          String(context.isDangerous), context.environmentVariables.FAC_MARK,
          context.sshPrefix === "" ? "empty" : "set", typeof exec,
        ].map((name) => ({ name })) },
      { custom: async (tokens, exec) => {
          const r = await exec({ command: "sh", args: ["-c", "exec 3</dev/tty; echo opened"] });
          return [{ name: String(r.status) }, { name: r.stdout || "none" }];
      } },
      { custom: async (tokens, exec) =>
          (await exec({ command: "ls", args: ["/dev/fd"] })).stdout.split("\n").map((name) => ({ name })) },
      { custom: async (tokens, exec) => [
          { name: (await exec({ command: "sh", args: ["-c", "ps -o sigmask= -p $$"] })).stdout.trim() },
          { name: (await exec({ command: "sh", args: ["-c", "kill -USR1 $$; echo survived"] })).stdout || "default" },
      ] },
      { custom: () => [{ name: "sync" }, "nameless"] },
      { custom: async () => [{ name: "a" }, null] },
      { custom: () => new Promise((resolve) => setTimeout(() => resolve([{ name: "timer" }]), 20)) },
    ] } },
    { name: "cache", args: { generators: [
      { script: ["sh", "-c", "echo x >> count; wc -l < count"], splitOn: "\n",
        cache: { strategy: "max-age", ttl: 600000 } },
      { script: ["sh", "-c", "echo x >> swr; wc -l < swr"], splitOn: "\n",
        cache: { strategy: "stale-while-revalidate" } },
      { script: ["cat", "id"], splitOn: "\n", cache: { strategy: "max-age", cacheKey: "k", cacheByDirectory: true } },
      { script: ["cat", "id"], splitOn: "\n", cache: { strategy: "max-age", cacheKey: "k2" } },
      { script: ["sh", "-c", "echo x >> short; wc -l < short"], splitOn: "\n",
        cache: { strategy: "max-age", ttl: 1 } },
    ] } },
    { name: "files", args: { template: "filepaths",
      filterTemplateSuggestions: (paths) => paths.filter((p) => p.name.endsWith(".js")) } },
  ],
};
"#;

/// The `gen` user spec loaded in a probe.
fn gen_probe(tag: &str, generator_timeout: Duration) -> (Probe, PathBuf) {
    let home = home_with(tag, &[("gen", GEN)]);
    let mut probe = Probe::with_timeouts(Some(home.clone()), generator_timeout);
    assert!(probe.loaded("gen").has_js);
    (probe, home)
}

/// Subcommand `sub` of `gen`, generator `index` of its args.
fn gen_request<'a>(
    sub: usize,
    index: usize,
    words: &[&str],
    cwd: &'a Path,
    env: Env,
) -> Request<'a> {
    const PATHS: [&str; 4] = [
        "gen:subcommands.0.args",
        "gen:subcommands.1.args",
        "gen:subcommands.2.args",
        "gen:subcommands.3.args",
    ];
    let mut all = vec!["gen"];
    all.extend_from_slice(words);
    Request {
        spec: "gen",
        path: PATHS[sub],
        index,
        tokens: tokens(&all),
        cwd,
        env,
    }
}

#[test]
fn a_hanging_script_times_out_and_its_group_is_killed() {
    let timeout = Duration::from_millis(300);
    let (mut probe, home) = gen_probe("hang", timeout);
    let started = Instant::now();
    send(
        &probe,
        1,
        &gen_request(0, 0, &["hang", ""], &home, env(&home, None)),
    );
    let pids = children(&mut probe);
    assert_eq!(pids.len(), 1, "{pids:?}");
    let (id, (result, stale)) = next_generated(&mut probe);
    let elapsed = started.elapsed();
    assert_eq!((id, result, stale), (1, Err(GenError::Timeout), false));
    assert!(elapsed >= timeout, "{elapsed:?}");
    assert!(
        elapsed < timeout + Duration::from_millis(500),
        "{elapsed:?}"
    );
    assert!(
        group_gone(pids[0]),
        "the group of {} is still there",
        pids[0]
    );
    assert!(children(&mut probe).is_empty());
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn split_on_wins_and_post_process_follows_fig() {
    let (mut probe, home) = gen_probe("split", Duration::from_secs(10));
    let env = env(&home, None);
    let mut answer = |index: usize| {
        run(
            &mut probe,
            index as u64,
            &gen_request(1, index, &["split", ""], &home, Arc::clone(&env)),
        )
    };
    let (split, _) = answer(0);
    assert_eq!(names(&split), ["a", "b"], "splitOn wins over postProcess");
    let (neither, _) = answer(1);
    assert_eq!(names(&neither), Vec::<String>::new());
    let (processed, _) = answer(2);
    assert_eq!(names(&processed), ["x", "split"]);
    let processed = processed.expect("suggestions");
    // A plain string is converted as Fig does.
    assert_eq!(processed[0].kind, Some(SuggestionType::Arg));
    assert_eq!(processed[0].insert_value.as_deref(), Some("x"));
    assert_eq!(processed[1].description.as_deref(), Some("d"));
    let (from_fn, _) = answer(3);
    assert_eq!(names(&from_fn), ["gen+split"]);
    // Fig does not await postProcess; a promise is no array.
    let (late, _) = answer(4);
    assert!(matches!(late, Err(GenError::Js(_))), "{late:?}");
    // A script function returning nothing runs nothing.
    let (nothing, _) = answer(5);
    assert_eq!(names(&nothing), Vec::<String>::new());
    // A busy postProcess is stopped by the interrupt budget.
    let started = Instant::now();
    let (busy, _) = answer(6);
    assert_eq!(busy, Err(GenError::Timeout));
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn execute_shell_command_and_the_custom_context() {
    let (mut probe, home) = gen_probe("exec", Duration::from_secs(10));
    std::fs::create_dir_all(home.join("sub")).expect("mkdir");
    let env = env(&home, None);
    let mut answer = |index: usize| {
        run(
            &mut probe,
            index as u64,
            &gen_request(2, index, &["exec", "se"], &home, Arc::clone(&env)),
        )
    };
    let (commands, _) = answer(0);
    assert_eq!(
        names(&commands),
        [
            "hi",
            "there",
            "1",
            "set-marked",
            "sub",
            "rejected",
            "rejected"
        ]
    );
    let (context, _) = answer(1);
    let dir = home
        .file_name()
        .and_then(|name| name.to_str())
        .expect("a name");
    assert_eq!(
        names(&context),
        [dir, "gen", "se", "true", "marked", "empty", "function"]
    );
    let (sync, _) = answer(5);
    assert_eq!(
        names(&sync),
        ["sync", ""],
        "a string item has no name, as in Fig"
    );
    let (null_item, _) = answer(6);
    assert!(matches!(null_item, Err(GenError::Js(_))), "{null_item:?}");
    let (timer, _) = answer(7);
    assert_eq!(names(&timer), ["timer"]);
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn children_have_no_tty_no_extra_fds_and_an_empty_signal_mask() {
    let (mut probe, home) = gen_probe("child", Duration::from_secs(10));
    // An fd fac would leak without POSIX_SPAWN_CLOEXEC_DEFAULT: no
    // FD_CLOEXEC, and far above what the child opens itself.
    // SAFETY: F_DUPFD duplicates fd 2 onto a free fd of at least 100; the
    // new fd is closed below.
    let leaky = unsafe { libc::fcntl(2, libc::F_DUPFD, 100) };
    assert!(leaky >= 100, "F_DUPFD");
    let env = env(&home, None);
    let mut answer = |index: usize| {
        run(
            &mut probe,
            index as u64,
            &gen_request(2, index, &["exec", ""], &home, Arc::clone(&env)),
        )
    };
    let (tty, _) = answer(2);
    let tty = names(&tty);
    assert_ne!(tty[0], "0", "opening /dev/tty must fail");
    assert_eq!(tty[1], "none");
    let (fds, _) = answer(3);
    // macOS's ls opens two: its working directory (for fts) and /dev/fd, as
    // it does from a shell with only 0-2 open.
    assert_eq!(
        names(&fds),
        ["0", "1", "2", "3", "4"],
        "0-2 and what ls opens"
    );
    // SAFETY: `leaky` is the fd duplicated above, closed once.
    unsafe { libc::close(leaky) };
    let (signals, _) = answer(4);
    assert_eq!(
        names(&signals),
        ["0", "default"],
        "no signal blocked or ignored"
    );
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn up_to_eight_children_run_at_once() {
    let (mut probe, home) = gen_probe("eight", Duration::from_secs(10));
    let env = env(&home, None);
    for id in 0..10 {
        send(
            &probe,
            id,
            &gen_request(0, 0, &["hang", ""], &home, Arc::clone(&env)),
        );
    }
    let running = children(&mut probe);
    assert_eq!(
        running.len(),
        crate::specs::process::MAX_RUNNING,
        "{running:?}"
    );
    // A freed slot starts a queued one.
    probe.thread.send(Job::Cancel { id: 0 });
    let running = children(&mut probe);
    assert_eq!(
        running.len(),
        crate::specs::process::MAX_RUNNING,
        "{running:?}"
    );
    for id in 1..10 {
        probe.thread.send(Job::Cancel { id });
    }
    assert!(children(&mut probe).is_empty());
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn the_cache_follows_fig() {
    let (mut probe, home) = gen_probe("cache", Duration::from_secs(10));
    let env = env(&home, None);
    let request = |index, cwd| gen_request(3, index, &["cache", ""], cwd, Arc::clone(&env));

    // max-age: a hit within the ttl (from when the fetch started), a miss
    // after it. Each side has a ttl no scheduling delay can cross: ten
    // minutes for the hit, a millisecond (and a sleep of many) for the miss.
    assert_eq!(names(&run(&mut probe, 1, &request(0, &home)).0), ["1"]);
    assert_eq!(names(&run(&mut probe, 2, &request(0, &home)).0), ["1"]);
    assert_eq!(names(&run(&mut probe, 10, &request(4, &home)).0), ["1"]);
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(names(&run(&mut probe, 11, &request(4, &home)).0), ["2"]);

    // stale-while-revalidate: the stale value, then the fresh one.
    let (first, stale) = run(&mut probe, 4, &request(1, &home));
    assert_eq!((names(&first), stale), (vec!["1".to_owned()], false));
    send(&probe, 5, &request(1, &home));
    let (id, (old, stale)) = next_generated(&mut probe);
    assert_eq!((id, names(&old), stale), (5, vec!["1".to_owned()], true));
    let (id, (fresh, stale)) = next_generated(&mut probe);
    assert_eq!((id, names(&fresh), stale), (5, vec!["2".to_owned()], false));

    // cacheByDirectory keeps two directories apart; without it one key is
    // shared.
    let (a, b) = (home.join("a"), home.join("b"));
    write(&a.join("id"), "in a");
    write(&b.join("id"), "in b");
    assert_eq!(names(&run(&mut probe, 6, &request(2, &a)).0), ["in a"]);
    assert_eq!(names(&run(&mut probe, 7, &request(2, &b)).0), ["in b"]);
    assert_eq!(names(&run(&mut probe, 8, &request(3, &a)).0), ["in a"]);
    assert_eq!(names(&run(&mut probe, 9, &request(3, &b)).0), ["in a"]);
    let _ = std::fs::remove_dir_all(home);
}

/// fac's exit takes the generators' children with it, even when the spec
/// thread is busy writing a usage save that takes longer than the exit
/// waits for it, whether the thread is shut down or just dropped.
#[test]
fn children_never_outlive_the_spec_thread() {
    for how in ["shutdown", "drop"] {
        let (mut probe, home) = gen_probe(&format!("exit-{how}"), Duration::from_secs(60));
        send(
            &probe,
            1,
            &gen_request(0, 0, &["hang", ""], &home, env(&home, None)),
        );
        let pids = children(&mut probe);
        assert_eq!(pids.len(), 1, "{how}: {pids:?}");
        // The save is under way when the exit comes.
        probe.thread.send(Job::SlowSave {
            delay: Duration::from_secs(3),
        });
        std::thread::sleep(Duration::from_millis(100));
        let started = Instant::now();
        match how {
            "shutdown" => probe.thread.shutdown(),
            _ => drop(probe),
        }
        assert!(started.elapsed() < Duration::from_secs(1), "{how}");
        assert!(
            group_gone(pids[0]),
            "{how}: the group of {} is still there",
            pids[0]
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{how}: gone only after the save"
        );
        let _ = std::fs::remove_dir_all(home);
    }
}

/// A save queued at the exit is written after the children are killed.
#[test]
fn children_are_killed_before_the_last_saves() {
    let (mut probe, home) = gen_probe("exit-queued", Duration::from_secs(60));
    send(
        &probe,
        1,
        &gen_request(0, 0, &["hang", ""], &home, env(&home, None)),
    );
    let pids = children(&mut probe);
    assert_eq!(pids.len(), 1, "{pids:?}");
    // Something long runs first, so the saves are still queued at the stop.
    probe.thread.send(Job::EvalForever {
        budget: Duration::from_secs(5),
    });
    probe.thread.send(Job::SlowSave {
        delay: Duration::from_secs(3),
    });
    std::thread::sleep(Duration::from_millis(100));
    let started = Instant::now();
    probe.thread.shutdown();
    assert!(
        group_gone(pids[0]),
        "the group of {} is still there",
        pids[0]
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "gone only after the save"
    );
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn cancel_kills_the_child_and_sends_nothing() {
    let (mut probe, home) = gen_probe("cancel", Duration::from_secs(10));
    send(
        &probe,
        1,
        &gen_request(0, 0, &["hang", ""], &home, env(&home, None)),
    );
    let pids = children(&mut probe);
    assert_eq!(pids.len(), 1, "{pids:?}");
    probe.thread.send(Job::Cancel { id: 1 });
    probe.thread.send(Job::Children);
    // The next outcome is the children list, empty: the child was killed and
    // reaped, and the job got no answer.
    match probe.next() {
        Outcome::Children(after) => assert!(after.is_empty(), "{after:?}"),
        other => panic!("{other:?}"),
    }
    assert!(
        group_gone(pids[0]),
        "the group of {} is still there",
        pids[0]
    );
    std::thread::sleep(Duration::from_millis(100));
    probe.thread.send(Job::SignalMask);
    match probe.next() {
        Outcome::SignalMask(_) => {}
        other => panic!("{other:?}"),
    }
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn filter_template_suggestions_is_called() {
    let (mut probe, home) = gen_probe("filter", Duration::from_secs(10));
    let spec = probe.loaded("gen");
    let fn_id = spec
        .fns
        .iter()
        .position(|path| path.ends_with("filterTemplateSuggestions"))
        .expect("the filter");
    let file = |name: &str| Suggestion {
        name: vec![name.to_owned()],
        kind: Some(SuggestionType::File),
        ..Suggestion::default()
    };
    probe.thread.send(Job::FilterTemplate {
        id: 1,
        spec: "gen".to_owned(),
        generation: probe.generation("gen"),
        fn_id: u32::try_from(fn_id).expect("an id"),
        suggestions: vec![file("a.js"), file("b.rs")],
    });
    let (id, (result, _)) = next_generated(&mut probe);
    assert_eq!(id, 1);
    assert_eq!(names(&result), ["a.js"]);
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn a_versioned_spec_reloads_for_the_detected_version() {
    let dir = temp("az");
    let bin = dir.join("bin");
    script(
        &bin.join("az"),
        r#"echo '{"azure-cli": "1.0.0", "extensions": {}}'"#,
    );
    let mut probe = Probe::new();
    probe.thread.send(Job::Load {
        name: "az".to_owned(),
        cwd: Some(dir.clone()),
        env: Some(env(&dir, Some(&bin))),
    });
    let mut loads = Vec::new();
    while loads.len() < 2 {
        if let Outcome::Loaded { name, result } = probe.next() {
            assert_eq!(name, "az");
            loads.push(result.expect("az loads"));
        }
    }
    assert!(
        matches!(&loads[0].version, Some(SpecVersion::Assumed(assumed)) if assumed != "1.0.0"),
        "{:?}",
        loads[0].version
    );
    assert_eq!(
        loads[1].version,
        Some(SpecVersion::Detected("1.0.0".to_owned()))
    );
    assert!(!loads[1].root.subcommands.is_empty());
    // The version is kept for the session: a reload resolves for it at once.
    let again = probe.loaded("az");
    assert_eq!(
        again.version,
        Some(SpecVersion::Detected("1.0.0".to_owned()))
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// A TypeScript user spec whose generators come from Fig's helpers lists a
/// real folder, filtered as `filepaths({ extensions })` and `folders` say.
#[test]
fn fig_helpers_in_a_typescript_spec_list_files() {
    let home = temp("helpers-home");
    write(
        &home.join(".config/fac/specs/tool.ts"),
        r#"import { filepaths, folders } from "@fig/autocomplete-generators";
const spec: Fig.Spec = {
  name: "tool",
  subcommands: [
    { name: "md", args: { name: "file", generators: filepaths({ extensions: ["md"] }) } },
    { name: "cd", args: { name: "dir", generators: folders } },
  ],
};
export default spec;
"#,
    );
    let work = temp("helpers-work");
    for file in ["notes.md", "a.txt", "b.md"] {
        write(&work.join(file), "");
    }
    std::fs::create_dir_all(work.join("sub")).expect("mkdir");
    let mut probe = Probe::with_home(Some(home.clone()));
    let tool = probe.loaded("tool");
    let env = env(&home, None);
    for (sub, want) in [
        ("md", &["b.md", "notes.md", "sub/", "../"][..]),
        ("cd", &["sub/", "../"][..]),
    ] {
        let arg = current_arg(&tool, &[sub], "");
        let request = Request {
            spec: "tool",
            path: &arg.path,
            index: 0,
            tokens: tokens(&["tool", sub, ""]),
            cwd: &work,
            env: Arc::clone(&env),
        };
        let (result, _) = run(&mut probe, 1, &request);
        assert_eq!(names(&result), want, "{sub}");
    }
    let _ = std::fs::remove_dir_all(home);
    let _ = std::fs::remove_dir_all(work);
}
