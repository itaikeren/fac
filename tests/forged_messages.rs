//! Only the shell's own hook may talk to fac. A program's output that
//! carries fac's private OSCs (7790 to 7793) without the session token, or
//! with a guessed one, changes nothing: no bytes are typed, the completion
//! worker runs nothing it names, and the popup never shows its line. Each
//! forged message would leave a marker file if fac believed it.

mod harness;

use std::path::Path;
use std::time::Duration;

use harness::{Session, ShellKind, SpawnOpts, TIMEOUT, has_line, wait_log};

/// Every message a program could forge, with the first field `first`
/// (none, a guessed token, a token with no version). Each one that fac
/// acted on would make a `PWNED-*` file in `home`.
fn forged(home: &Path, first: &str) -> Vec<u8> {
    let home = home.display();
    let mut out = String::new();
    let context = |out: &mut String, key: &str, value: &str| {
        out.push_str(&format!("\x1b]7791;{first};90;{key};{value}\x07"));
    };
    // A trigger fac would type at every prompt: `touch` and Enter.
    context(
        &mut out,
        "trigger",
        &format!("touch%20{home}/PWNED-trigger%0D"),
    );
    context(
        &mut out,
        "apply_trigger",
        &format!("touch%20{home}/PWNED-apply%0D"),
    );
    context(&mut out, "defer", "");
    context(&mut out, "armed", "1");
    // What the workers run: bash's `complete` lines (replayed) and the
    // bash-completion loader (sourced), zsh's aliases and zstyles (eval'd),
    // fish's function and completion paths (autoloaded).
    context(
        &mut out,
        "complete",
        &format!("complete -C 'touch {home}/PWNED-complete' ls"),
    );
    context(
        &mut out,
        "bash_completion",
        &format!("{home}/evil/evil.bash"),
    );
    context(&mut out, "aliases", &format!("touch {home}/PWNED-aliases"));
    context(&mut out, "zstyles", &format!("touch {home}/PWNED-zstyles"));
    context(&mut out, "fish_function_path", &format!("{home}/evil"));
    context(&mut out, "fish_complete_path", &format!("{home}/evil"));
    out.push_str(&format!(
        "\x1b]7792;{first};91;touch {home}/PWNED-command\x07"
    ));
    out.push_str(&format!("\x1b]7793;{first};92;cmd_fd_closed\x07"));
    out.push_str(&format!("\x1b]7790;{first};93;start;11;;;FORGED-LINE\x07"));
    out.into_bytes()
}

/// The files a believed message would have run, for the paths above.
fn write_bait(home: &Path) {
    let evil = home.join("evil");
    std::fs::create_dir_all(&evil).expect("mkdir");
    let touch = |name: &str| format!("touch '{}/PWNED-{name}'\n", home.display());
    std::fs::write(evil.join("evil.bash"), touch("bash-completion")).expect("bait");
    std::fs::write(evil.join("ls.fish"), touch("fish-path")).expect("bait");
    let forged: Vec<u8> = ["1", &format!("1:{}", "0".repeat(32)), &"0".repeat(32)]
        .iter()
        .flat_map(|first| forged(home, first))
        .collect();
    std::fs::write(home.join("forged.txt"), forged).expect("the forged messages");
}

fn pwned(home: &Path) -> Vec<String> {
    let mut found: Vec<String> = std::fs::read_dir(home)
        .expect("read_dir")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("PWNED"))
        .collect();
    found.sort();
    found
}

/// A line of the screen is `text`.
fn wait_line(s: &Session, text: &str) {
    s.wait_for(
        &format!("a line {text:?}"),
        |screen| has_line(screen, |line| line.trim_end() == text),
        TIMEOUT,
    );
}

/// The forged messages printed by `cat`, then a command line the workers
/// complete: nothing of them is believed.
fn forged_messages_change_nothing(kind: ShellKind) {
    let opts = match kind {
        ShellKind::Zsh => SpawnOpts {
            zshrc_extra: Some("cd ~".to_owned()),
            ..SpawnOpts::default()
        },
        ShellKind::Bash => SpawnOpts {
            shell: kind,
            rc_extra: Some("cd \"$HOME\"".to_owned()),
            ..SpawnOpts::default()
        },
        ShellKind::Fish => SpawnOpts {
            shell: kind,
            rc_extra: Some("cd $HOME".to_owned()),
            ..SpawnOpts::default()
        },
    };
    let s = Session::spawn(opts);
    write_bait(s.home());
    if kind != ShellKind::Zsh {
        wait_log(&s, "the shell's report trigger is on");
    }
    wait_log(&s, "worker ready");
    wait_line(&s, "%");
    s.send_line("cat forged.txt");
    wait_log(&s, "private osc code=7791 dropped: not the hook's");
    wait_line(&s, "%");
    s.wait_quiet(Duration::from_millis(200), TIMEOUT);
    // The workers complete `ls `: a believed `complete`, alias, zstyle or
    // completion path would run now.
    s.send(b"ls ");
    wait_log(&s, "worker query id=");
    s.wait_quiet(Duration::from_millis(500), TIMEOUT);
    // Two prompts: a believed trigger is typed at each.
    s.send(b"\x15\r");
    wait_line(&s, "%");
    s.send(b"\r");
    s.wait_quiet(Duration::from_millis(500), TIMEOUT);
    let log = s.log_text();
    assert_eq!(pwned(s.home()), Vec::<String>::new(), "{log}");
    assert!(
        !s.reports().iter().any(|report| report.contains("FORGED")),
        "{log}"
    );
    let screen = s.screen_text();
    assert!(
        !screen.contains("touch") && !screen.contains("FORGED"),
        "{screen}"
    );
    // The hook's own messages still count: the next line is reported.
    let before = s.reports().len();
    s.send(b"echo still-here");
    s.wait_for(
        "the hook's report",
        |_| {
            s.reports()
                .iter()
                .skip(before)
                .any(|report| report.contains(" buffer=\"echo still-here\" "))
        },
        TIMEOUT,
    );
}

#[test]
fn zsh_forged_messages_change_nothing() {
    forged_messages_change_nothing(ShellKind::Zsh);
}

#[test]
fn bash_forged_messages_change_nothing() {
    forged_messages_change_nothing(ShellKind::Bash);
}

#[test]
fn fish_forged_messages_change_nothing() {
    forged_messages_change_nothing(ShellKind::Fish);
}

/// Whether `text` holds a run of 32 hex digits: a session token, as a
/// trace of a hook line that sends one would print it.
fn has_token(text: &str) -> bool {
    text.lines().any(|line| {
        line.as_bytes()
            .split(|byte| !byte.is_ascii_hexdigit())
            .any(|run| run.len() >= 32)
    })
}

/// With the shell tracing every command (zsh `setopt xtrace`, bash `set
/// -x`, fish `fish_trace`, all from the login files), the trace of the
/// hook's lines never shows the token: the code that sends it runs with
/// tracing off. The user's own commands still trace. zsh's and bash's
/// traces go to a file (zsh's would fill the screen; bash draws its
/// prompt on stderr), fish's to the screen.
fn no_token_in_a_trace(kind: ShellKind) {
    let opts = match kind {
        ShellKind::Zsh => SpawnOpts {
            zshrc_extra: Some("exec 2>>~/trace.txt\nsetopt xtrace".to_owned()),
            ..SpawnOpts::default()
        },
        ShellKind::Bash => SpawnOpts {
            shell: kind,
            rc_extra: Some("exec 9>>\"$HOME/trace.txt\"\nBASH_XTRACEFD=9\nset -x".to_owned()),
            ..SpawnOpts::default()
        },
        ShellKind::Fish => SpawnOpts {
            shell: kind,
            rc_extra: Some("set -g fish_trace 1".to_owned()),
            rows: 1000,
            ..SpawnOpts::default()
        },
    };
    let s = Session::spawn(opts);
    if kind != ShellKind::Zsh {
        wait_log(&s, "the shell's report trigger is on");
    }
    wait_log(&s, "worker ready");
    // A command: the hook sends 7792, and the next prompt's context.
    wait_line(&s, "%");
    s.send_line("echo traced-here");
    s.wait_for(
        "echo's output",
        |screen| has_line(screen, |line| line.trim_end() == "traced-here"),
        TIMEOUT,
    );
    s.send_line("cd /");
    wait_log(&s, "command \"cd /\"");
    s.wait_quiet(Duration::from_millis(500), TIMEOUT);
    let trace = match kind {
        ShellKind::Fish => s.screen_text(),
        _ => std::fs::read_to_string(s.home().join("trace.txt")).unwrap_or_default(),
    };
    assert!(
        trace
            .lines()
            .any(|line| line.contains("echo traced-here") && !line.contains("% ")),
        "the user's command is traced:\n{trace}"
    );
    assert!(!has_token(&trace), "{trace}");
}

#[test]
fn zsh_traces_show_no_token() {
    no_token_in_a_trace(ShellKind::Zsh);
}

#[test]
fn bash_traces_show_no_token() {
    no_token_in_a_trace(ShellKind::Bash);
}

#[test]
fn fish_traces_show_no_token() {
    no_token_in_a_trace(ShellKind::Fish);
}
