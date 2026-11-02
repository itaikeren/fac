//! fac next to a terminal's own shell integration, as VS Code's and
//! iTerm2's scripts set it up from `~/.zshrc`
//! (`tests/fixtures/prompts/terminal-integration.zsh` stands for what they
//! do to the prompt): the terminal's marks and hooks run beside fac's, and
//! fac sends exactly one CPR per prompt.

mod harness;
mod suite;

use std::path::PathBuf;
use std::time::Instant;

use harness::{Profile, Session, TIMEOUT, frames, wait_log};
use suite::{Tree, settle, spawn_in_tree};

/// fac under VS Code's profile with the fixture sourced, at a fresh prompt
/// at the top of a cleared screen.
fn start() -> (Session, Tree) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/prompts/terminal-integration.zsh");
    let (s, tree) = spawn_in_tree(
        &format!("source '{}'", path.display()),
        24,
        80,
        Vec::new(),
        |opts| opts.set_profile(Profile::Vscode104),
    );
    suite::ready_at(&s, (0, 2));
    (s, tree)
}

/// How many times `needle` is in `haystack`.
fn count(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}

/// The offset of the first `needle` in `haystack`.
fn offset(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Every prompt has the terminal's A and B marks and its `zle-line-init`
/// once, fac's 133;A once with fac's single CPR right after it, and nothing
/// in fac's log is a warning.
#[test]
fn one_cpr_and_one_set_of_marks_per_prompt() {
    let (s, _tree) = start();
    for n in 1..=3 {
        let (raw_before, log_before) = (s.raw_output().len(), s.log_text().len());
        let since = Instant::now();
        s.send(b"\r");
        settle(&s, "Enter (a new prompt)", since);
        wait_log(&s, "cpr ok");
        let (raw, log) = (s.raw_output(), s.log_text());
        let (new, new_log) = (&raw[raw_before..], &log[log_before..]);
        let text = String::from_utf8_lossy(new);
        for (what, needle, expected) in [
            ("the terminal's A", "\x1b]633;A\x07", 1),
            ("the terminal's B", "\x1b]633;B\x07", 1),
            ("the terminal's line init", "\x1b]633;P;LineInit\x07", 1),
            ("fac's A", "\x1b]133;A\x07", 1),
            ("fac's B", "\x1b]133;B\x07", 1),
            ("fac's CPR", "\x1b[6n", 1),
        ] {
            assert_eq!(
                count(new, needle.as_bytes()),
                expected,
                "prompt {n}: {what}: {text:?}"
            );
        }
        assert!(
            offset(new, b"\x1b]133;A\x07") < offset(new, b"\x1b[6n"),
            "prompt {n}: fac's CPR follows its A mark: {text:?}"
        );
        assert_eq!(new_log.matches("prompt mark A").count(), 1, "{new_log}");
        assert!(!new_log.contains("without a prompt mark"), "{new_log}");
        assert!(!new_log.contains("cpr mismatch"), "{new_log}");
        assert!(
            !new_log
                .lines()
                .any(|line| line.contains(" WARN ") || line.contains(" ERROR ")),
            "{new_log}"
        );
        frames::check(new, 24, 80, s.sync_output()).unwrap_or_else(|err| panic!("{err}"));
    }
}

/// fac's precmd hooks stay around the terminal's, and fac's `zle-line-init`
/// hook still runs beside the terminal's widget: the line is reported and
/// the card opens under it.
#[test]
fn fac_hooks_run_beside_the_terminals() {
    let (s, _tree) = start();
    s.send_line("print -r -- \"hooks: ${precmd_functions[*]}\"");
    s.wait_text(
        "hooks: _fac_precmd_first _ti_precmd _fac_precmd_last",
        TIMEOUT,
    );
    let reports = s.reports().len();
    suite::step(&s, "ls -", b"ls -");
    assert!(s.reports().len() > reports, "fac's line init and edits");
    let card = s.card().expect("a popup");
    let cursor = s.with_screen(|screen| screen.cursor_position().0);
    assert_eq!(card.row, cursor + 1);
}
