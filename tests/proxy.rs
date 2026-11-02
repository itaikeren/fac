//! End-to-end tests of the passthrough proxy and the startup guards.

mod harness;

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use harness::{
    Session, ShellKind, SpawnOpts, TIMEOUT, assert_active, has_line, is_fac_active_value, wait_log,
    wait_prompt,
};

fn spawn(extra_env: &[(&str, &str)]) -> Session {
    let s = Session::spawn(SpawnOpts {
        extra_env: extra_env
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect(),
        ..SpawnOpts::default()
    });
    wait_prompt(&s);
    s
}

/// Assert fac stepped aside: no FAC_ACTIVE in the shell, and the reason logged.
fn assert_plain(s: &Session, reason: &str) {
    s.send_line("echo ${FAC_ACTIVE:-none}");
    s.wait_for(
        "none",
        |screen| has_line(screen, |line| line == "none"),
        TIMEOUT,
    );
    wait_log(s, &format!("reason={reason}"));
}

#[test]
fn smoke() {
    let s = spawn(&[]);
    let value = assert_active(&s);
    assert!(is_fac_active_value(&value), "{value}");
    wait_log(&s, "decision=run");
}

/// The default session names no terminal fac knows, so it paints in 256
/// colours as the harness always has (`SpawnOpts::default`).
#[test]
fn the_default_session_is_an_unknown_terminal() {
    let s = spawn(&[]);
    wait_log(&s, "terminal kind=Unknown");
    let log = s.log_text();
    let kinds: Vec<&str> = log
        .lines()
        .filter(|line| line.contains("terminal kind="))
        .map(str::trim_end)
        .collect();
    assert_eq!(kinds.len(), 1, "{kinds:?}");
    assert!(kinds[0].ends_with("terminal kind=Unknown"), "{kinds:?}");
}

#[test]
fn echo_reaches_the_screen() {
    let s = spawn(&[]);
    assert_active(&s);
    s.send_line("echo hello");
    s.wait_for(
        "hello",
        |screen| has_line(screen, |line| line == "hello"),
        TIMEOUT,
    );
}

#[test]
fn exits_with_the_shell_status() {
    let s = spawn(&[]);
    assert_active(&s);
    s.send_line("exit 3");
    assert_eq!(s.wait_exit(TIMEOUT), 3);
}

#[test]
fn resize_reaches_the_shell() {
    let s = spawn(&[]);
    assert_active(&s);
    s.resize(30, 100);
    s.wait_quiet(Duration::from_millis(200), TIMEOUT);
    s.send_line("tput cols");
    s.wait_for(
        "100 columns",
        |screen| has_line(screen, |line| line == "100"),
        TIMEOUT,
    );
}

#[test]
fn paste_of_64_kib_does_not_deadlock() {
    let s = spawn(&[]);
    // Not counting the wait for a free session.
    let started = s.started();
    assert_active(&s);
    s.send_line("cat > \"$HOME/paste.txt\"");
    s.wait_quiet(Duration::from_millis(200), TIMEOUT);
    let line = format!("{}\n", "a".repeat(63));
    let paste = line.repeat(1024);
    assert_eq!(paste.len(), 65536);
    s.send(paste.as_bytes());
    s.send(b"\x04");
    wait_prompt(&s);
    s.send_line("wc -c < \"$HOME/paste.txt\"");
    s.wait_for(
        "65536",
        |screen| has_line(screen, |line| line.trim() == "65536"),
        TIMEOUT,
    );
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "took {:?}",
        started.elapsed()
    );
}

#[test]
fn termios_is_restored_after_exit() {
    // fac runs under sh here: see `SpawnOpts::under_sh`.
    let s = Session::spawn(SpawnOpts {
        under_sh: true,
        ..SpawnOpts::default()
    });
    wait_prompt(&s);
    let before = s.initial_termios();
    assert_active(&s);
    let during = s.slave_termios();
    assert_eq!(
        during.c_lflag & libc::ICANON,
        0,
        "fac put the terminal in raw mode"
    );
    s.send_line("exit 0");
    s.wait_text("fac exited 0", TIMEOUT);
    let after = s.slave_termios();
    assert_eq!(before.c_lflag, after.c_lflag, "c_lflag");
    assert_eq!(before.c_iflag, after.c_iflag, "c_iflag");
    assert_eq!(before.c_oflag, after.c_oflag, "c_oflag");
}

#[test]
fn warp_gets_a_plain_shell() {
    let s = spawn(&[("TERM_PROGRAM", "WarpTerminal")]);
    assert_plain(&s, "Warp");
}

#[test]
fn kiro_gets_a_plain_shell_and_a_notice() {
    let s = spawn(&[("Q_TERM", "1")]);
    assert_plain(&s, "Kiro");
    assert!(
        s.screen_text().contains(
            "fac: Kiro is active in this terminal (Q_TERM is set); starting zsh without fac."
        ),
        "{}",
        s.screen_text()
    );
}

#[test]
fn fac_disable_gets_a_plain_shell() {
    let s = spawn(&[("FAC_DISABLE", "1")]);
    assert_plain(&s, "Disabled");
}

#[test]
fn nested_fac_steps_aside() {
    let s = spawn(&[("FAC_BIN", env!("CARGO_BIN_EXE_fac"))]);
    let outer = assert_active(&s);
    s.send_line("\"$FAC_BIN\"");
    wait_log(&s, "reason=Nested");
    wait_prompt(&s);
    s.send_line("echo \"inner=$FAC_ACTIVE\"");
    let expected = format!("inner={outer}");
    s.wait_for(
        &expected,
        |screen| has_line(screen, |line| line == expected),
        TIMEOUT,
    );
}

/// A `shell` session with `extra_env`, at its fixture rc file's `% `
/// prompt; fac runs it or not, as its guards decide.
fn spawn_shell(shell: ShellKind, extra_env: &[(&str, &str)]) -> Session {
    let s = Session::spawn(SpawnOpts {
        shell,
        extra_env: extra_env
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect(),
        ..SpawnOpts::default()
    });
    wait_prompt(&s);
    s
}

#[test]
fn apple_bash_gets_a_plain_shell() {
    let s = spawn_shell(ShellKind::Bash, &[("SHELL", "/bin/bash")]);
    assert_plain(&s, "Unsupported { kind: Some(Bash), version: Some(3.2.");
    wait_log(&s, "why: TooOld }");
    // A login bash, as it would start without fac.
    s.send_line("shopt -q login_shell && echo login");
    s.wait_for(
        "login",
        |screen| has_line(screen, |line| line == "login"),
        TIMEOUT,
    );
}

/// bash 5.3+ passes the version gate and runs with fac; its version is
/// read once and cached.
#[test]
fn bash_5_3_runs_with_fac() {
    let s = spawn_shell(ShellKind::Bash, &[]);
    wait_log(&s, "decision=run shell=bash");
    assert_active(&s);
    // The version was read once and cached for the next start.
    let cache = s.home().join("Library/Caches/fac/shell-versions");
    let cached = std::fs::read_to_string(&cache).expect("the version cache");
    assert_eq!(cached.lines().count(), 1, "{cached}");
    // The fixture's rc file ran.
    s.send_line("echo fixture=$FAC_FIXTURE_BASH_PROFILE");
    s.wait_for(
        "the fixture's mark",
        |screen| has_line(screen, |line| line == "fixture=r"),
        TIMEOUT,
    );
}

#[test]
fn foreign_fac_active_is_not_nested() {
    let foreign = "12345:/dev/ttys999";
    let s = spawn(&[("FAC_ACTIVE", foreign)]);
    let value = assert_active(&s);
    assert_ne!(value, foreign);
}

#[test]
fn shell_set_to_fac_does_not_exec_fac_again() {
    let fac = env!("CARGO_BIN_EXE_fac");
    let home = std::env::temp_dir().join(format!("fac-test-shell-loop-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).expect("creating the temp home");
    let log = home.join("fac.log");
    // As after `chsh -s fac`, with stdin a pipe: fac execs the plain shell.
    let mut child = Command::new(fac)
        .env_clear()
        .env("HOME", &home)
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("SHELL", fac)
        .env("ZDOTDIR", harness::fixture_zdotdir())
        .env("FAC_LOG_FILE", &log)
        .env("FAC_LOG", "debug")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawning fac");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(b"echo \"shell=$0\"; exit 7\n")
        .expect("writing to the shell");
    let deadline = Instant::now() + TIMEOUT;
    let status = loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "fac did not exit within {TIMEOUT:?}\n--- log ---\n{}",
                std::fs::read_to_string(&log).unwrap_or_default()
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let mut stdout = String::new();
    child
        .stdout
        .take()
        .expect("stdout")
        .read_to_string(&mut stdout)
        .expect("reading stdout");
    let log_text = std::fs::read_to_string(&log).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&home);
    assert_eq!(status.code(), Some(7), "{stdout}\n--- log ---\n{log_text}");
    assert_eq!(stdout.trim(), "shell=-zsh", "--- log ---\n{log_text}");
    // fac ran once: zsh was exec'd, not fac again.
    assert_eq!(
        log_text.matches(" started").count(),
        1,
        "--- log ---\n{log_text}"
    );
    assert!(log_text.contains("SHELL names fac itself"), "{log_text}");
    assert!(log_text.contains("reason=NotATty"), "{log_text}");
}

#[test]
fn exits_while_a_background_job_keeps_writing() {
    let s = spawn(&[]);
    assert_active(&s);
    // A disowned job writing when the shell exits. (macOS revokes the tty
    // then, which ends the job; the drain's own bound is unit-tested.)
    s.send_line("yes &!");
    s.wait_for(
        "yes running",
        |screen| {
            harness::lines(screen)
                .iter()
                .filter(|line| *line == "y")
                .count()
                > 3
        },
        TIMEOUT,
    );
    s.send_line("exit 4");
    assert_eq!(s.wait_exit(TIMEOUT), 4);
}

#[test]
fn specs_prewarm_once_the_prompt_is_idle_and_exit_stays_quick() {
    let s = spawn(&[]);
    assert_active(&s);
    for name in ["git", "cd", "ls", "npm", "docker", "brew"] {
        wait_log(&s, &format!("spec ready name={name} ms="));
    }
    let started = Instant::now();
    s.send_line("exit 5");
    assert_eq!(s.wait_exit(TIMEOUT), 5);
    // The spec thread is joined within its 50 ms; the rest is the shell.
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    let log = s.log_text();
    assert_eq!(log.matches("spec ready name=git ms=").count(), 1, "{log}");
    assert!(!log.contains("spec thread did not stop"), "{log}");
}

/// fac's resident set once the common specs are loaded; compare with a
/// build from before the spec thread. Run with --release --ignored
/// --nocapture.
#[test]
#[ignore = "measurement"]
fn measure_rss_after_prewarm() {
    let s = spawn(&[]);
    assert_active(&s);
    wait_log(&s, "spec ready name=brew ms=");
    std::thread::sleep(Duration::from_millis(500));
    let out = Command::new("ps")
        .args(["-o", "rss=", "-p", &s.pid().to_string()])
        .output()
        .expect("ps");
    eprintln!(
        "fac rss after prewarm: {} KiB",
        String::from_utf8_lossy(&out.stdout).trim()
    );
}

/// How fac learns that its terminal went away.
#[derive(Debug, Clone, Copy)]
enum HangUp {
    /// SIGHUP to fac, as `tools/live/terminal.sh close` sends.
    Signal,
    /// The terminal's master is closed.
    Close,
    /// SIGHUP, then the master closed.
    Both,
}

/// fac learns at a prompt that its terminal went away and takes the shell
/// down: the shell exits and is reaped with no warning. This zsh writes on
/// its way out, as zsh leaving ZLE from a trap does (bracketed paste off).
/// fac once stopped reading the master here, and the shell's exit then
/// waited for that unread output to drain, past SIGHUP's grace and past
/// SIGKILL's.
#[test]
fn a_hangup_takes_the_shell_down_cleanly() {
    for how in [HangUp::Signal, HangUp::Close, HangUp::Both] {
        let mut s = Session::spawn(SpawnOpts {
            zshrc_extra: Some("TRAPHUP() { print -n bye; exit 129 }".to_owned()),
            ..SpawnOpts::default()
        });
        wait_prompt(&s);
        assert_active(&s);
        if matches!(how, HangUp::Signal | HangUp::Both) {
            nix::sys::signal::kill(s.pid(), nix::sys::signal::Signal::SIGHUP).expect("SIGHUP");
        }
        if matches!(how, HangUp::Close | HangUp::Both) {
            s.hang_up();
        }
        assert_eq!(s.wait_exit(TIMEOUT), 129, "{how:?}");
        let log = s.log_text();
        let warnings: Vec<&str> = log
            .lines()
            .filter(|line| line.contains(" WARN ") || line.contains(" ERROR "))
            .collect();
        assert!(warnings.is_empty(), "{how:?}: {warnings:#?}");
        assert!(
            log.contains("shell taken down status=129"),
            "{how:?}\n{log}"
        );
    }
}
