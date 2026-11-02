//! fac under the prompt fixtures in `tests/fixtures/prompts`, which stand for
//! what powerlevel10k and starship do: where fac reconciles its screen model
//! when a theme draws its prompts without fac's 133;A/B, and that a prompt
//! redrawn while the popup is open goes out in one frame with the popup's
//! repaint. The scenarios themselves are in the snapshot suite.

mod harness;
mod suite;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use harness::{Profile, Session, TIMEOUT, frames, wait_log};
use suite::{Tree, settle, spawn_in_tree};

/// fac in the snapshot suite's fixture tree with the prompt fixture
/// `fixture` sourced, in a terminal like `profile` (the default one for
/// `None`), at a fresh prompt at the top of a cleared screen, where the
/// cursor is at `cursor`.
fn start(fixture: &str, profile: Option<Profile>, cursor: (u16, u16)) -> (Session, Tree) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/prompts")
        .join(format!("{fixture}.zsh"));
    let (s, tree) = spawn_in_tree(
        &format!("source '{}'", path.display()),
        24,
        80,
        Vec::new(),
        |opts| {
            if let Some(profile) = profile {
                opts.set_profile(profile);
            }
        },
    );
    suite::ready_at(&s, cursor);
    (s, tree)
}

/// Send `bytes` and wait for a settled screen.
fn step(s: &Session, name: &str, bytes: &[u8]) {
    let since = Instant::now();
    s.send(bytes);
    settle(s, name, since);
}

/// fac's CPR queries in what the terminal received.
fn cpr_queries(s: &Session) -> usize {
    s.raw_output()
        .windows(4)
        .filter(|window| *window == b"\x1b[6n")
        .count()
}

/// Wait for a popup drawn on the row under the cursor, and return its row
/// and the cursor's.
fn card_under_the_cursor(s: &Session, what: &str) -> (u16, u16) {
    s.wait_card(
        what,
        |card, screen| card.is_some_and(|card| card.row == screen.cursor_position().0 + 1),
        TIMEOUT,
    );
    let card = s.card().expect("a popup");
    (card.row, s.with_screen(|screen| screen.cursor_position().0))
}

/// With fac's marks, every prompt has its 133;A and fac's CPR there, and
/// nothing more; without them (a prompt rebuilt after fac wrapped it), the
/// line editor's start has the CPR instead: one per prompt either way.
#[test]
fn one_cpr_per_prompt_with_marks_or_without() {
    for (fixture, marks) in [("two-line", true), ("rebuilt-prompt", false)] {
        let (s, _tree) = start(fixture, None, (1, 2));
        let (queries, log) = (cpr_queries(&s), s.log_text());
        for n in 1..=3 {
            step(&s, "Enter (a new prompt)", b"\r");
            wait_log(&s, "cpr ok");
            assert_eq!(cpr_queries(&s), queries + n, "{fixture}: prompt {n}");
        }
        let log_now = s.log_text();
        let new = &log_now[log.len()..];
        let raw = String::from_utf8_lossy(&s.raw_output()).into_owned();
        if marks {
            assert_eq!(new.matches("prompt mark A").count(), 3, "{new}");
            assert!(!new.contains("without a prompt mark"), "{new}");
        } else {
            assert!(!raw.contains("\x1b]133;A"), "{fixture}: no marks");
            assert_eq!(new.matches("without a prompt mark").count(), 3, "{new}");
        }
        assert!(!new.contains("cpr mismatch"), "{new}");
    }
}

/// After a width change the model is not trusted until fac's next CPR is
/// answered. Without marks, the next report asks: the first key after the
/// resize brings the popup back.
#[test]
fn a_width_change_then_a_key_brings_the_popup_back() {
    for fixture in ["two-line", "rebuilt-prompt"] {
        let (s, _tree) = start(fixture, None, (1, 2));
        s.resize(24, 100);
        std::thread::sleep(Duration::from_millis(300));
        s.wait_quiet(Duration::from_millis(150), TIMEOUT);
        s.send(b"ls -");
        let (card, cursor) = card_under_the_cursor(&s, &format!("{fixture}: a popup again"));
        assert_eq!(card, cursor + 1, "{fixture}");
    }
}

/// The terminal ends up a row below fac's model (it drew a line fac's
/// model did not see wrap): at the next prompt fac's CPR finds it, with or
/// without marks, and the card opens on the row under the line typed on.
#[test]
fn the_models_row_is_corrected_at_the_next_prompt() {
    for fixture in ["two-line", "rebuilt-prompt"] {
        let (s, _tree) = start(fixture, None, (1, 2));
        s.terminal_only(b"\n");
        step(&s, "Enter (a new prompt)", b"\r");
        wait_log(&s, "cpr mismatch");
        s.send(b"ls -");
        let (card, cursor) = card_under_the_cursor(&s, &format!("{fixture}: a popup"));
        assert_eq!(card, cursor + 1, "{fixture}");
    }
}

/// User specs are rechecked at each prompt, marks or not: an edit to one
/// shows at the next prompt.
#[test]
fn user_specs_are_rechecked_at_each_prompt_without_marks() {
    let (s, _tree) = start("rebuilt-prompt", None, (1, 2));
    s.send_line(&format!("'{}' spec new mytool", env!("CARGO_BIN_EXE_fac")));
    s.wait_text("live at your next prompt", TIMEOUT);
    s.wait_quiet(Duration::from_millis(200), TIMEOUT);
    let colors = harness::colors();
    let shows = |s: &Session, name: &str| {
        let name = name.to_owned();
        s.wait_card(
            &format!("a popup with {name:?}"),
            move |card, screen| {
                card.is_some_and(|card| {
                    harness::labels(screen, card, &colors)
                        .iter()
                        .any(|label| label.split([' ', ',']).next() == Some(name.as_str()))
                })
            },
            TIMEOUT,
        );
    };
    s.send(b"mytool ");
    shows(&s, "open");
    let spec = s.home().join(".config/fac/specs/mytool.ts");
    let edited = std::fs::read_to_string(&spec)
        .expect("the starter")
        .replace("name: \"open\"", "name: \"deploy\"");
    std::fs::write(&spec, edited).expect("editing the spec");
    // Clear the line and run the empty line, for a fresh prompt.
    step(&s, "Ctrl-U, Enter", b"\x15\r");
    s.send(b"mytool ");
    shows(&s, "deploy");
}

/// A prompt redrawn while the popup is open, with no key (an async
/// `zle -F` handler): with synchronized output, every frame that holds the
/// shell's bytes also holds fac's repaint, so no frame leaves the card's
/// old cells, and every frame paints each cell once.
#[test]
fn an_async_redraw_goes_out_with_the_repaint() {
    for line in ["ed", "reprint"] {
        let (s, tree) = start("async-prompt", Some(Profile::Ghostty), (1, 2));
        step(&s, "ls -", b"ls -");
        assert!(s.card().is_some(), "a popup");
        let raw_before = s.raw_output().len();
        let since = Instant::now();
        let mut fifo = std::fs::OpenOptions::new()
            .write(true)
            .open(tree.0.join("async.fifo"))
            .expect("the fixture's FIFO");
        std::io::Write::write_all(&mut fifo, format!("{line}\n").as_bytes()).expect("writing");
        settle(&s, line, since);
        let raw = s.raw_output();
        let since = &raw[raw_before..];
        let redrawn = format!("top line ({line})");
        assert!(
            String::from_utf8_lossy(since).contains(&redrawn),
            "{line}: the shell redrew"
        );
        let mut found = false;
        for frame in frames::sync_frames(since) {
            let fac: usize = frames::segments(frame, 24).iter().map(|s| s.len()).sum();
            if frames::find(frame, redrawn.as_bytes()).is_some() {
                found = true;
                assert!(fac > 0, "{line}: the redraw's frame has no repaint");
            }
        }
        assert!(found, "{line}: the redraw is in a frame");
        frames::check(since, 24, 80, true).unwrap_or_else(|err| panic!("{line}: {err}"));
        let card = s.card().expect("still open");
        assert_eq!(
            card.row,
            s.with_screen(|screen| screen.cursor_position().0) + 1
        );
    }
}
