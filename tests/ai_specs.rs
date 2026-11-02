//! The specs for AI command-line tools (`specs/overrides/`): each is in the
//! packed archive and parses, and in a real fac the popup lists `claude`'s
//! options and `codex`'s subcommands.

mod harness;

use fac::specs::archive::{Archive, Kind};
use harness::{Session, SpawnOpts, TIMEOUT, assert_active, wait_log, wait_prompt};

const NAMES: [&str; 10] = [
    "aider",
    "amp",
    "claude",
    "codex",
    "cursor-agent",
    "gemini",
    "goose",
    "kiro-cli",
    "llm",
    "opencode",
];

#[test]
fn the_ai_tool_specs_are_packed_and_complete_in_the_popup() {
    let archive = Archive::embedded();
    for name in NAMES {
        assert_eq!(archive.kind(name), Some(Kind::Spec), "{name} is a spec");
        let entry = match archive.entry(name) {
            Some(Ok(entry)) => entry,
            other => panic!("{name}: {other:?}"),
        };
        let root = entry.root().expect("a spec tree");
        assert_eq!(
            root.name,
            [name],
            "{name}: the root is named after the tool"
        );
        assert!(
            !root.options.is_empty() || !root.subcommands.is_empty(),
            "{name} has options or subcommands"
        );
    }

    let s = Session::spawn(SpawnOpts::default());
    wait_prompt(&s);
    assert_active(&s);
    wait_log(&s, "worker ready");

    s.send(b"claude --");
    s.wait_card(
        "claude's options",
        |card, screen| {
            let Some(card) = card else { return false };
            let labels = harness::labels(screen, card, &harness::colors());
            !labels.is_empty() && labels.iter().all(|l| l.starts_with('-'))
        },
        TIMEOUT,
    );
    // Back to an empty prompt: Ctrl-C drops the line.
    s.send(b"\x03");
    wait_prompt(&s);

    s.send(b"codex ");
    s.wait_card(
        "codex's subcommands",
        |card, screen| {
            let Some(card) = card else { return false };
            let labels = harness::labels(screen, card, &harness::colors());
            labels.iter().any(|l| l.starts_with("agents"))
                && labels.iter().any(|l| l.starts_with("apply"))
        },
        TIMEOUT,
    );
}
