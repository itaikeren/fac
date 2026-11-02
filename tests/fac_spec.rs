//! fac completes itself: the packed `fac` spec (`specs/overrides/fac.js`)
//! names exactly the subcommands and options in `fac --help`, and the popup
//! offers them.

mod harness;

use std::collections::BTreeSet;
use std::process::Command;

use fac::specs::archive::{Archive, Kind};
use fac::specs::model::Subcommand;
use harness::{Session, SpawnOpts, TIMEOUT, assert_active, wait_log, wait_prompt};

/// `path option` pairs (the path is empty for the top level; an entry with
/// no option is the subcommand itself).
type Pairs = BTreeSet<(String, String)>;

/// Every subcommand path and option in the packed spec, by first name.
fn from_spec() -> Pairs {
    fn walk(sub: &Subcommand, path: &str, out: &mut Pairs) {
        for option in &sub.options {
            out.insert((path.to_owned(), option.name[0].clone()));
        }
        for child in &sub.subcommands {
            let path = format!("{path} {}", child.name[0]).trim().to_owned();
            out.insert((path.clone(), String::new()));
            walk(child, &path, out);
        }
    }
    let archive = Archive::embedded();
    assert_eq!(archive.kind("fac"), Some(Kind::Spec), "fac has a spec");
    let entry = match archive.entry("fac") {
        Some(Ok(entry)) => entry,
        other => panic!("fac: {other:?}"),
    };
    let root = entry.root().expect("a spec tree");
    let mut out = Pairs::new();
    walk(root, "", &mut out);
    out
}

/// The same pairs from the usage block of `fac --help`: on each line the
/// words before the first `[` or `<` are the subcommand path, and every
/// word starting with `-` is an option of that path.
fn from_help(help: &str) -> Pairs {
    let usage: Vec<&str> = help
        .lines()
        .skip_while(|line| !line.starts_with("Usage:"))
        .take_while(|line| !line.trim().is_empty())
        .collect();
    assert!(usage.len() >= 6, "the usage block: {usage:?}");
    let mut out = Pairs::new();
    for line in usage {
        let line = line.trim_start_matches("Usage:");
        // `fac enable | fac disable | fac uninstall` is three commands.
        for command in line.split(" | fac ") {
            let mut words = command
                .split_whitespace()
                .map(|w| w.trim_start_matches("fac"))
                .filter(|w| !w.is_empty())
                .peekable();
            let mut path = Vec::new();
            while let Some(word) = words.peek() {
                if word.starts_with(['[', '<', '-']) {
                    break;
                }
                path.push(words.next().unwrap().to_owned());
            }
            let mut prefix = String::new();
            for word in &path {
                prefix = format!("{prefix} {word}").trim().to_owned();
                out.insert((prefix.clone(), String::new()));
            }
            for word in words {
                let word = word.trim_matches(|c| "[]<>|,;".contains(c));
                // `[--js | --json]` and `zsh|bash|fish` leave pieces behind.
                for piece in word.split('|').filter(|p| p.starts_with("--")) {
                    out.insert((prefix.clone(), piece.to_owned()));
                }
            }
        }
    }
    out
}

#[test]
fn the_spec_and_the_help_name_the_same_commands_and_options() {
    let fac = env!("CARGO_BIN_EXE_fac");
    let help = Command::new(fac)
        .arg("--help")
        .output()
        .expect("fac --help");
    assert!(help.status.success());
    let help = String::from_utf8(help.stdout).expect("utf-8 help");
    let (in_help, in_spec) = (from_help(&help), from_spec());
    let only_help: Vec<_> = in_help.difference(&in_spec).collect();
    let only_spec: Vec<_> = in_spec.difference(&in_help).collect();
    assert!(
        only_help.is_empty() && only_spec.is_empty(),
        "in `fac --help` only: {only_help:?}; in the fac spec only: {only_spec:?}"
    );
    // The short forms are the parser's too.
    for flag in ["-h", "-V"] {
        let out = Command::new(fac).arg(flag).output().expect("fac flag");
        assert!(out.status.success(), "fac {flag}");
    }
}

#[test]
fn typing_fac_offers_its_commands_and_init_its_options() {
    let s = Session::spawn(SpawnOpts::default());
    wait_prompt(&s);
    assert_active(&s);
    wait_log(&s, "worker ready");

    s.send(b"fac ");
    s.wait_card(
        "fac's subcommands",
        |card, screen| {
            let Some(card) = card else { return false };
            let labels = harness::labels(screen, card, &harness::colors());
            labels.iter().any(|l| l.starts_with("init"))
        },
        TIMEOUT,
    );
    s.send(b"\x03");
    wait_prompt(&s);

    s.send(b"fac init --");
    s.wait_card(
        "fac init's options",
        |card, screen| {
            let Some(card) = card else { return false };
            let labels = harness::labels(screen, card, &harness::colors());
            labels.iter().any(|l| l.starts_with("--terminals"))
                && labels.iter().any(|l| l.starts_with("--shell"))
        },
        TIMEOUT,
    );
}
