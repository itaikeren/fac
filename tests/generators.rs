//! Generators run at once on the spec thread: three slow generators are all
//! running before the first one finishes. Each one appends `start` to a log
//! file when it begins and `end` when it is done, so concurrency shows in
//! the file's order (three starts, then three ends) and does not depend on
//! how fast this machine starts a process. A serial thread would write
//! start, end, start, end, start, end. The wall-clock bound is only a guard
//! against a hang. That several run at once is also checked in
//! `src/specs/generator_tests.rs`.

use std::os::fd::BorrowedFd;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use fac::guards::ShellKind;
use fac::specs::runtime::{Env, Job, Outcome, SpecThread};
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};

/// How long each generator takes; long next to how far apart three process
/// starts can be on a busy machine.
const SLEEP: &str = "1.5";

/// A spec whose generator logs its start and its end to `log`.
fn sleepy(log: &Path) -> String {
    format!(
        r#"
export default {{
  name: "sleepy",
  args: {{ generators: {{
    script: ["sh", "-c", "echo start >> '{}'; sleep {SLEEP}; echo end >> '{}'"],
    splitOn: "\n",
  }} }},
}};
"#,
        log.display(),
        log.display()
    )
}

/// The next outcomes, waiting at most 10 s for them.
fn take(thread: &mut SpecThread) -> Vec<Outcome> {
    // SAFETY: `thread` owns the fd and outlives this borrow.
    let fd = unsafe { BorrowedFd::borrow_raw(thread.result_fd()) };
    let mut fds = [PollFd::new(fd, PollFlags::POLLIN)];
    let _ = poll(&mut fds, PollTimeout::from(10_000u16));
    thread.take()
}

#[test]
fn three_generators_run_at_once() {
    let home = std::env::temp_dir().join(format!("fac-gen-it-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let specs = home.join(".config/fac/specs");
    std::fs::create_dir_all(&specs).expect("mkdir");
    let log = home.join("generators.log");
    std::fs::write(specs.join("sleepy.js"), sleepy(&log)).expect("write");
    let mut thread =
        SpecThread::start_with_home(Some(home.clone()), ShellKind::Zsh).expect("spec thread");
    thread.send(Job::Load {
        name: "sleepy".to_owned(),
        cwd: None,
        env: None,
    });
    let loaded = take(&mut thread);
    let generation = match &loaded[..] {
        [
            Outcome::Loaded {
                result: Ok(spec), ..
            },
        ] => spec.generation,
        other => panic!("{other:?}"),
    };

    let env = Arc::new(vec![("PATH".to_owned(), "/usr/bin:/bin".to_owned())]);
    let took = three_at_once(&mut thread, &env, generation, 0);
    // The hang guard; one run takes SLEEP, three in a row would take 3x.
    assert!(took < Duration::from_secs(20), "{took:?}");
    let order: Vec<String> = std::fs::read_to_string(&log)
        .expect("the generators' log")
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(
        order,
        ["start", "start", "start", "end", "end", "end"],
        "all three started before the first one finished"
    );
    thread.shutdown();
    let _ = std::fs::remove_dir_all(home);
}

/// Run the generator (of the spec's load `generation`) three times at once,
/// as jobs `first..first + 3`: how long until all three answered.
fn three_at_once(thread: &mut SpecThread, env: &Env, generation: u64, first: u64) -> Duration {
    let started = Instant::now();
    for id in first..first + 3 {
        thread.send(Job::Generate {
            id,
            spec: "sleepy".to_owned(),
            generation,
            arg_path: "sleepy:args".to_owned(),
            generator_index: 0,
            tokens: vec!["sleepy".to_owned(), String::new()],
            search_term: String::new(),
            cwd: PathBuf::from("/"),
            env: Arc::clone(env),
            generator: None,
        });
    }
    let mut done = Vec::new();
    while done.len() < 3 && started.elapsed() < Duration::from_secs(20) {
        for outcome in take(thread) {
            if let Outcome::Generated { id, result, .. } = outcome {
                assert_eq!(result, Ok(Vec::new()), "sleep prints nothing");
                done.push(id);
            }
        }
    }
    let elapsed = started.elapsed();
    done.sort_unstable();
    assert_eq!(done, [first, first + 1, first + 2]);
    elapsed
}
