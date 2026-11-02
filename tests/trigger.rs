//! The report trigger end to end: fac in the harness's terminal, with a
//! fake bash that records every byte it reads (`fixtures/trigger/
//! fake-shell/bash`), so a test sees exactly what reached the shell. The
//! real bash and fish, with their hooks, are tests/bash.rs and
//! tests/fish.rs.

mod harness;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use harness::{Session, ShellKind, SpawnOpts, TIMEOUT, wait_log};

const T: &[u8] = b"\x1b[9999~";
const A: &[u8] = b"\x1b[9998~";

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/trigger")
}

fn env(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

/// fac running the fake bash, with `FAC_TEST_DEFER=defer` and `extra` env.
fn spawn_fake(defer: &str, extra: &[(&str, &str)]) -> Session {
    let mut pairs = vec![("FAC_TEST_DEFER", defer)];
    pairs.extend_from_slice(extra);
    let s = Session::spawn(SpawnOpts {
        shell: ShellKind::Bash,
        shell_binary: Some(fixtures().join("fake-shell/bash")),
        extra_env: env(&pairs),
        ..SpawnOpts::default()
    });
    wait_log(&s, "the shell's report trigger is on");
    // The line's first trigger, at the first prompt.
    wait_bytes(&s, "the first trigger", |bytes| bytes == T);
    s
}

/// What the fake shell has read.
fn shell_bytes(s: &Session) -> Vec<u8> {
    std::fs::read(s.home().join("fake-shell.log")).unwrap_or_default()
}

fn wait_bytes(s: &Session, what: &str, done: impl Fn(&[u8]) -> bool) {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let bytes = shell_bytes(s);
        if done(&bytes) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}: the shell read {:?}\n--- log ---\n{}",
            String::from_utf8_lossy(&bytes),
            s.log_text()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Send `keys` and wait until the shell has read them and what fac adds
/// (`expected`, all the shell read since `from` bytes).
fn send_expect(s: &Session, keys: &[u8], expected: &[u8]) {
    let from = shell_bytes(s).len();
    s.send(keys);
    wait_bytes(
        s,
        &format!("{:?}", String::from_utf8_lossy(expected)),
        |bytes| bytes.len() >= from + expected.len(),
    );
    let bytes = shell_bytes(s);
    assert_eq!(
        String::from_utf8_lossy(&bytes[from..]),
        String::from_utf8_lossy(expected),
        "after {keys:?}"
    );
}

fn with_t(bytes: &[u8]) -> Vec<u8> {
    [bytes, T].concat()
}

/// fac's log has a report of exactly `buffer`.
fn wait_report(s: &Session, buffer: &str) {
    wait_report_after(s, 0, buffer);
}

/// A report of exactly `buffer` among fac's reports after the first `skip`.
fn wait_report_after(s: &Session, skip: usize, buffer: &str) {
    let needle = format!(" buffer={buffer:?} ");
    let deadline = Instant::now() + TIMEOUT;
    while !s
        .reports()
        .iter()
        .skip(skip)
        .any(|line| line.contains(&needle))
    {
        assert!(
            Instant::now() < deadline,
            "no report of {buffer:?}\n--- screen ---\n{}\n--- reports ---\n{}\n--- trigger ---\n{}",
            s.screen_text(),
            s.reports().join("\n"),
            s.log_text()
                .lines()
                .filter(|line| line.contains("trigger"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn the_fake_shell_gets_one_trigger_per_read() {
    let s = spawn_fake("", &[]);
    send_expect(&s, b"l", &with_t(b"l"));
    wait_report(&s, "l");
    send_expect(&s, b"s -", &with_t(b"s -"));
    wait_report(&s, "ls -");
    send_expect(&s, "é ש 🙂".as_bytes(), &with_t("é ש 🙂".as_bytes()));
    wait_report(&s, "ls -é ש 🙂");
    send_expect(&s, b"\x7f\x1b[D\x1b[1;5C", &with_t(b"\x7f\x1b[D\x1b[1;5C"));
}

#[test]
fn nothing_reaches_cat_but_the_users_bytes() {
    let s = spawn_fake("", &[]);
    // `cat`, Enter and what is typed for cat, in one write: the trigger
    // may go before Enter, never after it. The fake runs both lines. The
    // line typed ahead keeps the prompt after `cat` without a trigger (its
    // look finds `hello` unread); the prompt after `hello` finds it read,
    // and gets its first trigger.
    send_expect(
        &s,
        b"cat\rhello\r",
        &[b"cat".as_slice(), T, b"\rhello\r", T].concat(),
    );
    wait_log(&s, "no line typed ahead is queued");
    std::thread::sleep(Duration::from_millis(100));
    assert!(shell_bytes(&s).ends_with(&[b"\rhello\r".as_slice(), T].concat()));
    wait_report(&s, "");
    send_expect(&s, b"x", &with_t(b"x"));
    wait_report(&s, "x");
    send_expect(&s, b"\r", &[b"\r".as_slice(), T].concat());
}

#[test]
fn a_paste_gets_its_trigger_after_its_end() {
    let s = spawn_fake("", &[]);
    let from = shell_bytes(&s).len();
    for part in [&b"\x1b[200~echo "[..], b"one\rtwo", b"\x1b[A\x1b[201~"] {
        s.send(part);
        std::thread::sleep(Duration::from_millis(30));
    }
    let paste = b"\x1b[200~echo one\rtwo\x1b[A\x1b[201~";
    wait_bytes(&s, "the paste", |bytes| {
        bytes.len() >= from + paste.len() + T.len()
    });
    assert_eq!(shell_bytes(&s)[from..], with_t(paste));
}

#[test]
fn an_idle_key_gets_its_trigger_150_ms_later() {
    let s = spawn_fake("i%09", &[]);
    let from = shell_bytes(&s).len();
    let sent = Instant::now();
    s.send(b"\t");
    wait_bytes(&s, "the idle trigger", |bytes| {
        bytes.len() >= from + 1 + T.len()
    });
    assert!(sent.elapsed() >= Duration::from_millis(150));
    assert_eq!(shell_bytes(&s)[from..], with_t(b"\t"));
    // A lone ESC waits for the escape delay.
    let from = shell_bytes(&s).len();
    let sent = Instant::now();
    s.send(b"\x1b");
    wait_bytes(&s, "the escape trigger", |bytes| {
        bytes.len() >= from + 1 + T.len()
    });
    assert!(sent.elapsed() >= Duration::from_millis(30));
    assert_eq!(shell_bytes(&s)[from..], with_t(b"\x1b"));
}

#[test]
fn a_widget_key_in_defer_gets_no_trigger() {
    let s = spawn_fake("n%14", &[]);
    let from = shell_bytes(&s).len();
    // C-t opens the widget; what is typed goes to it, Enter included.
    for keys in [&b"\x14"[..], b"abc", b"\x1b[B", b"\r"] {
        s.send(keys);
        std::thread::sleep(Duration::from_millis(40));
    }
    // The fake runs the line at Enter: its next prompt has a trigger.
    let typed = b"\x14abc\x1b[B\r";
    wait_bytes(&s, "the next prompt's trigger", |bytes| {
        bytes.len() >= from + typed.len() + T.len()
    });
    assert_eq!(shell_bytes(&s)[from..], with_t(typed));
}

#[test]
fn a_tab_accept_sends_the_apply_then_its_trigger() {
    let s = spawn_fake("", &[("FAC_TEST_CWD", "1")]);
    std::fs::create_dir(s.home().join("alpha")).expect("mkdir");
    send_expect(&s, b"cd al", &with_t(b"cd al"));
    wait_report(&s, "cd al");
    s.wait_card("the popup", |card, _| card.is_some(), TIMEOUT);
    // Tab takes the suggestion: no Tab reaches the shell, the apply
    // trigger does, and the apply carried the line it expects.
    send_expect(&s, b"\t", A);
    wait_report(&s, "cd alpha/");
    wait_log(&s, "apply seq=");
}

/// Two unanswered triggers turn it off for the rest of the line (a shell
/// that stalled); the shell's next `armed` turns it on again.
#[test]
fn off_for_the_line_when_triggers_go_unanswered() {
    let s = spawn_fake("", &[("FAC_TEST_SILENT", "1")]);
    send_expect(&s, b"a", &with_t(b"a"));
    std::thread::sleep(Duration::from_millis(60));
    send_expect(&s, b"b", &with_t(b"b"));
    wait_log(&s, "report trigger is off for the line");
    std::thread::sleep(Duration::from_millis(60));
    send_expect(&s, b"c", b"c");
    // Enter: the fake shell's next prompt sends `armed`. The Enter went
    // with the trigger off, so it is typeahead until that prompt's look
    // finds it read; then the line's first trigger goes.
    send_expect(&s, b"\r", &[b"\r".as_slice(), T].concat());
    wait_log(&s, "report trigger is on again");
    send_expect(&s, b"d", &with_t(b"d"));
}
