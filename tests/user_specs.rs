//! Your own spec, end to end through fac with a temp `HOME`: `fac spec new
//! mytool` run inside the session makes `mytool ` show the starter's items,
//! an edit shows up at the next prompt, and once the file is gone the
//! command has no spec again.

mod harness;

use std::time::Duration;

use harness::{Session, SpawnOpts, TIMEOUT, assert_active, wait_log, wait_prompt};

const QUIET: Duration = Duration::from_millis(200);

/// The names on the popup's list rows (a row also shows the item's other
/// names and its args: `--config, -c <path>`).
fn names(screen: &vt100::Screen, card: &harness::Card) -> Vec<String> {
    harness::labels(screen, card, &harness::colors())
        .iter()
        .filter_map(|label| label.split([' ', ',']).next().map(str::to_owned))
        .collect()
}

fn labels(s: &Session) -> Vec<String> {
    let Some(card) = s.card() else {
        return Vec::new();
    };
    s.with_screen(|screen| names(screen, &card))
}

fn wait_label(s: &Session, label: &str) {
    s.wait_card(
        &format!("a popup with {label:?}"),
        |card, screen| {
            card.is_some_and(|card| names(screen, card).iter().any(|shown| shown == label))
        },
        TIMEOUT,
    );
}

/// Clear the line and run the empty line, for a fresh prompt.
fn next_prompt(s: &Session) {
    s.send(b"\x15\r");
    s.wait_quiet(QUIET, TIMEOUT);
    wait_prompt(s);
}

#[test]
fn a_new_spec_is_live_at_the_next_prompt_and_edits_follow() {
    let s = Session::spawn(SpawnOpts::default());
    wait_prompt(&s);
    assert_active(&s);
    wait_log(&s, "worker ready");

    s.send_line(&format!("'{}' spec new mytool", env!("CARGO_BIN_EXE_fac")));
    s.wait_text("live at your next prompt", TIMEOUT);
    s.wait_quiet(QUIET, TIMEOUT);
    wait_prompt(&s);
    let spec = s.home().join(".config/fac/specs/mytool.ts");
    assert!(spec.is_file(), "the starter is in the temp HOME");

    s.send(b"mytool ");
    wait_label(&s, "open");
    wait_label(&s, "--config");

    // Edited: the old items stay until the next prompt, then the new ones.
    let edited = std::fs::read_to_string(&spec)
        .expect("the starter")
        .replace("name: \"open\"", "name: \"deploy\"");
    std::fs::write(&spec, edited).expect("editing the spec");
    next_prompt(&s);
    s.send(b"mytool ");
    wait_label(&s, "deploy");
    assert!(
        !labels(&s).iter().any(|label| label == "open"),
        "{:?}",
        labels(&s)
    );

    // Gone: no spec, so none of its items.
    std::fs::remove_file(&spec).expect("removing the spec");
    next_prompt(&s);
    s.send(b"mytool ");
    s.wait_quiet(Duration::from_millis(500), TIMEOUT);
    assert!(
        !labels(&s)
            .iter()
            .any(|label| label == "deploy" || label == "--config"),
        "{:?}\n--- log ---\n{}",
        labels(&s),
        s.log_text()
    );
}
