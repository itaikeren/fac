//! End-to-end tests of the zsh integration: the ZDOTDIR shim, fac.zsh's
//! reports and markers, and the command channel.

mod harness;

use std::time::{Duration, Instant};

use fac::shell::protocol::encode_apply;
use harness::{
    Direct, Session, SpawnOpts, TIMEOUT, assert_active, fixture_zdotdir, has_line, wait_log,
    wait_prompt,
};

fn start(opts: SpawnOpts) -> Session {
    let s = Session::spawn(opts);
    wait_prompt(&s);
    assert_active(&s);
    s
}

fn with_zshrc(extra: &str) -> SpawnOpts {
    SpawnOpts {
        zshrc_extra: Some(extra.to_owned()),
        ..SpawnOpts::default()
    }
}

/// The tail of a logged report line, from `ctx=` on.
fn report(ctx: &str, cursor: usize, flags: &str, buffer: &str, prebuffer: &str) -> String {
    format!("ctx={ctx} cursor={cursor} flags={flags} buffer={buffer:?} prebuffer={prebuffer:?}")
}

fn wait_report(s: &Session, expected: &str) {
    let deadline = Instant::now() + TIMEOUT;
    while !s.reports().iter().any(|line| line.ends_with(expected)) {
        assert!(
            Instant::now() < deadline,
            "no report {expected}\n--- reports ---\n{}\n--- screen ---\n{}",
            s.reports().join("\n"),
            s.screen_text()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn count(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    count(haystack, needle) > 0
}

/// The payloads of every `ESC ] <code> ; … BEL` in `raw`, in order.
fn oscs(raw: &[u8], code: &str) -> Vec<String> {
    let start = format!("\x1b]{code};");
    let mut found = Vec::new();
    let mut rest = raw;
    while let Some(at) = rest
        .windows(start.len())
        .position(|window| window == start.as_bytes())
    {
        rest = &rest[at + start.len()..];
        let end = rest.iter().position(|&b| b == 0x07).unwrap_or(rest.len());
        found.push(String::from_utf8_lossy(&rest[..end]).into_owned());
        rest = &rest[end..];
    }
    found
}

/// Assert fac's prompt markers alternate A, B, A, B … with `renders` pairs,
/// and return the statuses of the D markers.
fn check_marks(s: &Session, renders: usize) -> Vec<i32> {
    let deadline = Instant::now() + TIMEOUT;
    while count(&s.raw_output(), b"\x1b]133;B\x07") < renders {
        assert!(
            Instant::now() < deadline,
            "fewer than {renders} prompts: {:?}",
            oscs(&s.raw_output(), "133")
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    s.wait_quiet(Duration::from_millis(300), TIMEOUT);
    let marks = oscs(&s.raw_output(), "133");
    let ab: String = marks
        .iter()
        .filter(|mark| *mark == "A" || *mark == "B")
        .map(String::as_str)
        .collect();
    assert_eq!(ab, "AB".repeat(renders), "{marks:?}");
    marks
        .iter()
        .filter_map(|mark| mark.strip_prefix("D;"))
        .map(|status| status.parse().expect("D status"))
        .collect()
}

/// Send `keys` and wait until they have drawn one more prompt.
fn new_prompt(s: &Session, keys: &[u8]) {
    let prompts = || count(&s.raw_output(), b"\x1b]133;B\x07");
    let before = prompts();
    s.send(keys);
    let deadline = Instant::now() + TIMEOUT;
    while prompts() == before {
        assert!(
            Instant::now() < deadline,
            "no new prompt after {keys:?}\n--- screen ---\n{}",
            s.screen_text()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    wait_prompt(s);
}

/// Type `keys`, then the same with Ctrl-U to clear the line again.
fn type_and_check(s: &Session, keys: &str, expected: &str) {
    s.send(keys.as_bytes());
    wait_report(s, expected);
    s.send(b"\x15");
    wait_report(s, &report("start", 0, "", "", ""));
}

#[test]
fn round_trip() {
    let s = start(SpawnOpts::default());
    for buffer in [
        "ls -la",
        "a;b",
        "100%",
        "echo ✅ 日本",
        "back\\slash",
        "trailing%",
    ] {
        let cursor = buffer.chars().count();
        type_and_check(&s, buffer, &report("start", cursor, "", buffer, ""));
    }

    // A continuation line: PREBUFFER and the `cont` context.
    s.send(b"echo \"a\r");
    wait_report(&s, &report("cont", 0, "", "", "echo \"a\n"));
    s.send(b"b");
    wait_report(&s, &report("cont", 1, "", "b", "echo \"a\n"));
    // Enter: the finish report has `f`.
    s.send(b"\"\r");
    wait_report(&s, &report("cont", 2, "f", "b\"", "echo \"a\n"));
    wait_prompt(&s);

    // ESC, newline and rarer control bytes (the slow path) in the buffer.
    let buffer = "x\x1by;%\n2\x01\x7f\tz";
    s.send_line(r"print -z $'x\ey;%\n2\x01\x7f\tz'");
    wait_report(&s, &report("start", buffer.chars().count(), "", buffer, ""));
}

#[test]
fn cursor() {
    let s = start(SpawnOpts::default());
    s.send(b"hello world");
    wait_report(&s, &report("start", 11, "", "hello world", ""));
    for _ in 0..3 {
        s.send(b"\x1b[D");
        std::thread::sleep(Duration::from_millis(30));
    }
    wait_report(&s, &report("start", 8, "", "hello world", ""));
    s.wait_quiet(Duration::from_millis(200), TIMEOUT);
    let reports = s.reports();
    let last = reports.last().expect("a report");
    assert!(
        last.ends_with(&report("start", 8, "", "hello world", "")),
        "{last}"
    );
}

#[test]
fn markers() {
    let s = start(with_zshrc("bindkey '^T' reset-prompt"));
    new_prompt(&s, b"false\r");
    new_prompt(&s, b"true\r");
    s.send(b"abc");
    wait_report(&s, &report("start", 3, "", "abc", ""));
    // zsh is idle in zle again, not inside a hook.
    s.wait_quiet(Duration::from_millis(100), TIMEOUT);
    new_prompt(&s, b"\x03");
    new_prompt(&s, b"\r");
    new_prompt(&s, b"\x14");
    // Prompts: the first, after `echo $FAC_ACTIVE`, `false`, `true`, Ctrl-C
    // and the empty Enter, plus the reset-prompt redraw.
    let statuses = check_marks(&s, 7);
    assert_eq!(statuses[..3], [0, 1, 0], "{statuses:?}");
    // zsh sets $? to 130 or 1 after Ctrl-C, depending on where the signal
    // lands; the empty Enter keeps it.
    assert!(matches!(statuses[3], 1 | 130), "{statuses:?}");
    assert_eq!(statuses[3..], [statuses[3]; 2], "{statuses:?}");
}

/// Kiro's post block, reduced: its precmd forces itself last, saves PS1 and
/// wraps it in OSC 697 markers; its preexec, forced first, restores PS1.
/// Ctrl-C (no preexec) is handled by calling the preexec from the precmd.
const KIRO_FIXTURE: &str = r#"
_kiro_has_set_prompt=0
_kiro_preexec() {
  PS1=$_kiro_user_ps1
  _kiro_has_set_prompt=0
  printf '\e]697;PreExec\a'
}
_kiro_precmd() {
  local last_status=$?
  if [[ $precmd_functions[-1] != _kiro_precmd ]]; then
    precmd_functions=(${(@)precmd_functions:#_kiro_precmd} _kiro_precmd)
  fi
  if [[ $preexec_functions[1] != _kiro_preexec ]]; then
    preexec_functions=(_kiro_preexec ${(@)preexec_functions:#_kiro_preexec})
  fi
  printf '\e]697;ExitCode=%s\a' $last_status
  (( _kiro_has_set_prompt )) && _kiro_preexec
  _kiro_user_ps1=$PS1
  PS1=$'%{\e]697;StartPrompt\a%}'$PS1$'%{\e]697;EndPrompt\a%}'
  _kiro_has_set_prompt=1
}
precmd_functions+=(_kiro_precmd)
preexec_functions=(_kiro_preexec $preexec_functions)
"#;

#[test]
fn kiro_style_prompt_rewriting() {
    let s = start(with_zshrc(KIRO_FIXTURE));
    new_prompt(&s, b"false\r");
    new_prompt(&s, b"true\r");
    s.send(b"abc");
    wait_report(&s, &report("start", 3, "", "abc", ""));
    // zsh is idle in zle again, not inside a hook.
    s.wait_quiet(Duration::from_millis(100), TIMEOUT);
    new_prompt(&s, b"\x03");
    new_prompt(&s, b"\r");
    new_prompt(&s, b"false\r");
    let statuses = check_marks(&s, 7);
    assert_eq!(statuses.len(), 6, "{statuses:?}");
    assert_eq!(statuses[..3], [0, 1, 0], "{statuses:?}");
    assert!(matches!(statuses[3], 1 | 130), "{statuses:?}");
    assert_eq!(statuses[4..], [statuses[3], 1], "{statuses:?}");
    let raw = s.raw_output();
    // The fixture really did wrap every prompt, and saw the same $? as fac
    // (Kiro's precmd also runs at the first prompt).
    assert_eq!(count(&raw, b"\x1b]697;StartPrompt\x07"), 7);
    let kiro: Vec<i32> = oscs(&raw, "697")
        .iter()
        .filter_map(|osc| osc.strip_prefix("ExitCode="))
        .map(|code| code.parse().expect("ExitCode"))
        .collect();
    assert_eq!(kiro[1..], statuses[..], "{kiro:?}");
}

#[test]
fn smkx_in_line_init() {
    let s = start(with_zshrc(
        "zle-line-init() { echoti smkx }\nzle -N zle-line-init",
    ));
    assert!(contains(&s.raw_output(), b"\x1b[?1h"), "smkx was not sent");
    s.send(b"abc");
    wait_report(&s, &report("start", 3, "", "abc", ""));
    s.send(b"\x1bOD");
    wait_report(&s, &report("start", 2, "", "abc", ""));
}

#[test]
fn zsh_syntax_highlighting() {
    let real_home = std::env::var("HOME").unwrap_or_default();
    let plugin = std::path::Path::new(&real_home)
        .join(".oh-my-zsh/plugins/zsh-syntax-highlighting/zsh-syntax-highlighting.zsh");
    if !plugin.exists() {
        eprintln!("skipped: {} not found", plugin.display());
        return;
    }
    let s = start(with_zshrc(&format!("source '{}'", plugin.display())));
    for buffer in ["ls -la", "echo 'a;b' | cat"] {
        type_and_check(
            &s,
            buffer,
            &report("start", buffer.chars().count(), "", buffer, ""),
        );
    }
    assert!(s.log_text().contains("context key=path"));
}

fn screen_line(s: &Session, expected: &str) {
    s.wait_for(
        expected,
        |screen| has_line(screen, |line| line == expected),
        TIMEOUT,
    );
}

const MARKERS: &str =
    "echo markers=$FAC_FIXTURE_ZSHENV$FAC_FIXTURE_ZPROFILE$FAC_FIXTURE_ZSHRC$FAC_FIXTURE_ZLOGIN";

#[test]
fn zdotdir_is_restored() {
    let s = start(SpawnOpts::default());
    s.send_line("echo $ZDOTDIR");
    screen_line(&s, &fixture_zdotdir().display().to_string());
    s.send_line(MARKERS);
    screen_line(&s, "markers=eprl");
}

#[test]
fn kiro_opt_out_is_set_for_startup_only() {
    let s = start(SpawnOpts::default());
    // Kiro's pre block (in .zprofile) saw it; the shell's children do not.
    s.send_line("print -r -- startup=$FAC_FIXTURE_KIRO_OPT_OUT now=${PROCESS_LAUNCHED_BY_Q-unset}");
    screen_line(&s, "startup=1 now=unset");
    s.send_line("/usr/bin/env | grep -c PROCESS_LAUNCHED_BY_Q");
    screen_line(&s, "0");
}

#[test]
fn no_zdotdir_reads_home() {
    let s = start(SpawnOpts {
        zdotdir: None,
        ..SpawnOpts::default()
    });
    s.send_line("echo ${ZDOTDIR-unset}");
    screen_line(&s, "unset");
    s.send_line(MARKERS);
    screen_line(&s, "markers=eprl");
}

/// A cd in a subshell leaves the shell where it was, so no cwd goes to
/// fac, and `$(cd …)` captures no message of fac's (its OSC 7 for the
/// terminal is as before).
#[test]
fn a_subshell_cd_sends_no_cwd() {
    let s = start(SpawnOpts::default());
    wait_log(&s, "context key=cwd ");
    let cwds = || s.log_text().matches("context key=cwd ").count();
    let before = cwds();
    s.send_line("(cd /tmp); x=$(cd /usr && pwd); [[ $x == *7791* ]] && echo leaked || echo clean");
    screen_line(&s, "clean");
    wait_prompt(&s);
    assert_eq!(cwds(), before, "{}", s.log_text());
}

#[test]
fn context() {
    let s = start(SpawnOpts::default());
    for key in [
        "fpath",
        "path",
        "histfile",
        "cwd",
        "aliases",
        "functions",
        "options",
    ] {
        wait_log(&s, &format!("context key={key} "));
    }
    let lines = |key: &str| {
        let needle = format!("context key={key} ");
        s.log_text().matches(&needle).count()
    };
    let cwd = lines("cwd");
    s.send_line("cd /tmp");
    wait_prompt(&s);
    let deadline = Instant::now() + TIMEOUT;
    while lines("cwd") == cwd {
        assert!(Instant::now() < deadline, "no new cwd:\n{}", s.log_text());
        std::thread::sleep(Duration::from_millis(20));
    }
    let aliases = lines("aliases");
    s.send_line("alias zz=ls");
    wait_prompt(&s);
    let deadline = Instant::now() + TIMEOUT;
    while lines("aliases") == aliases {
        assert!(
            Instant::now() < deadline,
            "no new aliases:\n{}",
            s.log_text()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let functions = lines("functions");
    s.send_line("zzfn() { : }; _zzfn() { : }");
    wait_prompt(&s);
    let deadline = Instant::now() + TIMEOUT;
    while lines("functions") == functions {
        assert!(
            Instant::now() < deadline,
            "no new functions:\n{}",
            s.log_text()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    // A `_` name alone changes nothing that is sent.
    s.send_line("_zzother() { : }");
    wait_prompt(&s);
    s.send_line("true");
    wait_prompt(&s);
    assert_eq!(lines("functions"), functions + 1, "{}", s.log_text());
    // Nothing else changed, so nothing else was sent again.
    assert_eq!(lines("fpath"), 1);
    assert_eq!(lines("zstyles"), 1);
    // fd 63 reached the shell and every hook went in.
    assert!(!s.log_text().contains("diagnostic"), "{}", s.log_text());
}

/// What `quote_for` gives inside an open double quote reads back as the
/// text in a real interactive zsh, with history expansion on (and off).
#[test]
fn double_quoted_words_read_back_in_zsh() {
    use std::io::Write as _;
    use std::process::{Command, Stdio};

    use fac::complete::quoting::quote_for;
    use fac::complete::tokenizer::{Dialect, Quote};

    let texts = ["a!b", "!", "x!!y", "it's !", "a\"b$c`d\\e!f", "!$", "plain"];
    for setup in ["", "setopt no_bang_hist"] {
        let mut script = format!("{setup}\n");
        for text in texts {
            let word = quote_for(Dialect::Zsh, text, Quote::Double, false, true);
            script.push_str(&format!("print -r -- \"{word}\n"));
        }
        // -i: read from stdin as typed lines, so history expansion applies.
        let mut zsh = Command::new("/bin/zsh")
            .args(["-f", "-i"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("starting zsh");
        zsh.stdin
            .take()
            .expect("zsh's stdin")
            .write_all(script.as_bytes())
            .expect("writing to zsh");
        let output = zsh.wait_with_output().expect("zsh's output");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(
            stdout.lines().collect::<Vec<_>>(),
            texts,
            "{setup:?}\n--- script ---\n{script}\n--- stderr ---\n{stderr}"
        );
        assert!(!stderr.contains("event not found"), "{stderr}");
    }
}

#[test]
fn command_report() {
    let s = start(SpawnOpts::default());
    s.send_line("echo 'x;y' # 100%");
    wait_log(&s, r#"command "echo 'x;y' # 100%""#);
}

#[test]
fn compsys_worker() {
    let s = start(SpawnOpts::default());
    wait_log(&s, "worker ready compinit_ms=");
    let deadline = Instant::now() + TIMEOUT;
    for lbuffer in ["git ", "cd ", "ls -"] {
        wait_log(&s, &format!("key={lbuffer:?} in"));
    }
    while s.log_text().matches("worker done id=").count() < 3 {
        assert!(
            Instant::now() < deadline,
            "fewer than 3 prewarm queries done:\n{}",
            s.log_text()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    s.send_line("true");
    wait_log(&s, "worker cache invalidated n=3");
}

// Direct sessions: zsh with fac.zsh and no fac in between.

fn direct(fac_active: Option<String>) -> Session {
    let s = Session::spawn(SpawnOpts {
        direct: Some(Direct { fac_active }),
        ..SpawnOpts::default()
    });
    wait_prompt(&s);
    s
}

/// The seq of the latest 7790 in the raw output whose buffer is `buffer`.
fn latest_seq(s: &Session, buffer: &str) -> u64 {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let seq = oscs(&s.raw_output(), "7790")
            .iter()
            .rev()
            .find_map(|report| {
                let fields: Vec<&str> = report.split(';').collect();
                (fields.len() == 7 && fields[6] == buffer).then(|| fields[1].parse().expect("seq"))
            });
        if let Some(seq) = seq {
            return seq;
        }
        assert!(
            Instant::now() < deadline,
            "no 7790 with buffer {buffer:?}:\n{:?}",
            oscs(&s.raw_output(), "7790")
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn wait_raw(s: &Session, needle: &[u8]) {
    let deadline = Instant::now() + TIMEOUT;
    while !contains(&s.raw_output(), needle) {
        assert!(
            Instant::now() < deadline,
            "raw output never contained {:?}\n--- screen ---\n{}",
            String::from_utf8_lossy(needle),
            s.screen_text()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn prompt_line(s: &Session, expected: &str) {
    s.wait_for(
        expected,
        |screen| {
            let (row, _) = screen.cursor_position();
            screen
                .rows(0, screen.size().1.get())
                .nth(usize::from(row))
                .is_some_and(|line| line.trim_end() == expected)
        },
        TIMEOUT,
    );
}

#[test]
fn apply_sets_the_buffer() {
    let s = direct(None);
    s.send(b"ech");
    let seq = latest_seq(&s, "ech");
    s.write_cmd(&encode_apply(seq, "echo ✅ applied", "", None));
    prompt_line(&s, "% echo ✅ applied");
    let applied = latest_seq(&s, "echo ✅ applied");
    assert!(applied > seq);

    // A stale seq is ignored; the next line, with the latest seq, is not.
    let mut lines = encode_apply(seq, "stale", "", None);
    lines.extend_from_slice(&encode_apply(applied, "echo ", "fresh", None));
    s.write_cmd(&lines);
    prompt_line(&s, "% echo fresh");
    assert!(!s.screen_text().contains("stale"));
    let fresh = latest_seq(&s, "echo fresh");
    assert!(
        oscs(&s.raw_output(), "7790")
            .iter()
            .any(|report| report == &format!("1;{fresh};start;5;;;echo fresh"))
    );
    s.send(b"\r");
    s.wait_for(
        "fresh",
        |screen| has_line(screen, |line| line == "fresh"),
        TIMEOUT,
    );
}

/// `ps -o cputime=` of `pid`, in seconds.
fn cpu_seconds(pid: nix::unistd::Pid) -> f64 {
    let output = std::process::Command::new("/bin/ps")
        .args(["-o", "cputime=", "-p", &pid.to_string()])
        .output()
        .expect("ps");
    let text = String::from_utf8_lossy(&output.stdout);
    text.trim().split(':').fold(0.0, |total, part| {
        total * 60.0 + part.parse::<f64>().expect("cputime")
    })
}

#[test]
fn eof_on_the_pipe_does_not_spin() {
    let s = direct(None);
    // fac.zsh is set up once the first report is out.
    latest_seq(&s, "");
    s.close_cmd();
    wait_raw(&s, b";cmd_fd_closed\x07");
    let before = cpu_seconds(s.pid());
    std::thread::sleep(Duration::from_secs(2));
    let after = cpu_seconds(s.pid());
    assert!(
        after - before < 0.2,
        "zsh used {:.2}s of CPU",
        after - before
    );
    s.send_line("echo ok");
    s.wait_for(
        "ok",
        |screen| has_line(screen, |line| line == "ok"),
        TIMEOUT,
    );
}

#[test]
fn foreign_tty_emits_nothing() {
    let s = direct(Some("1:/dev/ttys999".into()));
    s.send_line("echo done");
    s.wait_for(
        "done",
        |screen| has_line(screen, |line| line == "done"),
        TIMEOUT,
    );
    wait_prompt(&s);
    s.send(b"abc");
    s.wait_quiet(Duration::from_millis(200), TIMEOUT);
    let raw = s.raw_output();
    assert!(
        !contains(&raw, b"\x1b]779"),
        "{:?}",
        String::from_utf8_lossy(&raw)
    );
    assert!(
        !contains(&raw, b"\x1b]133"),
        "{:?}",
        String::from_utf8_lossy(&raw)
    );
}

/// The total lifetime of zsh's reaped children (`ri_child_elapsed_abstime`):
/// it grows with every fork that is waited for.
fn child_time(pid: nix::unistd::Pid) -> u64 {
    // SAFETY: rusage_info_v2 is plain old data, all zeroes is a valid value.
    let mut info: libc::rusage_info_v2 = unsafe { std::mem::zeroed() };
    // SAFETY: the buffer is a live rusage_info_v2, the size RUSAGE_INFO_V2 asks for.
    let res = unsafe {
        libc::proc_pid_rusage(
            pid.as_raw(),
            libc::RUSAGE_INFO_V2,
            (&mut info as *mut libc::rusage_info_v2).cast(),
        )
    };
    assert_eq!(
        res,
        0,
        "proc_pid_rusage: {}",
        std::io::Error::last_os_error()
    );
    info.ri_child_elapsed_abstime
}

#[test]
fn no_fork_per_keystroke() {
    let s = direct(None);
    latest_seq(&s, "");
    s.wait_quiet(Duration::from_millis(200), TIMEOUT);
    let before = child_time(s.pid());
    let reports_before = oscs(&s.raw_output(), "7790").len();
    let typed: String = (0..50).map(|n| char::from(b'a' + (n % 26) as u8)).collect();
    for c in typed.chars() {
        s.send(c.to_string().as_bytes());
        std::thread::sleep(Duration::from_millis(20));
    }
    latest_seq(&s, &typed);
    let after = child_time(s.pid());
    assert_eq!(before, after, "zsh forked while typing");
    // Keys that arrive together share one report, so not quite one each.
    let reports = oscs(&s.raw_output(), "7790").len() - reports_before;
    assert!(reports >= 25, "{reports} reports for 50 keys");

    // The measure does see a fork.
    s.send(b"\x15");
    s.send_line("/usr/bin/true");
    let deadline = Instant::now() + TIMEOUT;
    while child_time(s.pid()) == after {
        assert!(Instant::now() < deadline, "the fork was not seen");
        std::thread::sleep(Duration::from_millis(20));
    }
}
