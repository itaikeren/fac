//! A few scenarios under every terminal profile (`harness::Profile`), each
//! judged by named checks. What fails today is listed in `EXPECTED_FAIL`,
//! asserted both ways: a failure that is not listed fails the run, and so
//! does a listed one that passes.

mod harness;
mod suite;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::time::Instant;

use harness::{Colors, Profile, Session};
use suite::{frame_errors, invariant_errors, settle};

const DOWN: &[u8] = b"\x1b[B";
const ESC: &[u8] = b"\x1b";
const CTRL_U: &[u8] = b"\x15";

const STARTUP: &str = "startup";
const GIT_CH: &str = "git ch";
const DOWN_KEY: &str = "Down";
const ESC_KEY: &str = "Esc";
/// `git ch` in a 5-row pane: fewer rows than its card needs.
const SHORT_PANE: &str = "short pane";
const LS: &str = "ls -";
const SCENARIOS: [&str; 6] = [STARTUP, GIT_CH, DOWN_KEY, ESC_KEY, SHORT_PANE, LS];

const NO_BANNED: &str = "no_banned";
const NO_JUNK: &str = "no_junk";
const CAPS: &str = "caps";
const POPUP: &str = "popup";
const NO_REPLY_LEAK: &str = "no_reply_leak";
const STARTUP_COMPLETE: &str = "startup_complete";

/// The (profile, scenario, check) that fail today, each with the step that
/// is expected to fix it. Empty since step 3a: Terminal.app is no longer
/// sent DECRQM, which it printed as a `p` above the first prompt.
const EXPECTED_FAIL: &[(Profile, &str, &str)] = &[];

/// A profile as it runs. `colorterm` adds `COLORTERM=truecolor`, which
/// `Profile::Ghostty` leaves out (the harness's historical env) and the
/// measured Kero env has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Run {
    profile: Profile,
    colorterm: bool,
}

impl Run {
    const fn plain(profile: Profile) -> Self {
        Self {
            profile,
            colorterm: false,
        }
    }

    /// The colours fac paints the popup with in this run.
    fn colors(self) -> Colors {
        if self.colorterm {
            let profile = self.profile;
            let roles =
                harness::roles_for("default", profile.background(), Some(profile.palette()));
            harness::colors_true(&roles)
        } else {
            harness::profile_colors(self.profile)
        }
    }
}

/// Every profile as measured, and Ghostty with Kero's `COLORTERM` too. The
/// variant is not in `EXPECTED_FAIL`: every check must pass for it. Since
/// step 3a plain Ghostty paints in true colour by its kind, so the variant
/// draws the same; it stays to cover `COLORTERM` explicitly.
const RUNS: [Run; 6] = [
    Run::plain(Profile::Ghostty),
    Run::plain(Profile::Apple),
    Run::plain(Profile::Iterm2),
    Run::plain(Profile::Vscode104),
    Run::plain(Profile::Zed),
    Run {
        profile: Profile::Ghostty,
        colorterm: true,
    },
];

/// What one scenario left behind, read when it ended.
struct Seen {
    /// `Session::banned`.
    banned: Vec<Vec<u8>>,
    /// Text outside the prompt row and the popup, or a prompt row that is not
    /// the expected one.
    junk: Option<String>,
    /// fac's `caps sync=` lines.
    caps: Vec<String>,
    /// fac's `caps ansi` line.
    ansi: Option<String>,
    /// The popup's labels and selected list row, if a popup is up.
    popup: Option<(Vec<String>, Option<u16>)>,
    /// The first broken snapshot-suite invariant after any of its steps.
    invariants: Result<(), String>,
    /// Reply bytes seen in the shell's buffer or on the screen.
    leak: Option<String>,
}

/// One scenario as it runs: its steps' invariants, from where its log
/// starts.
struct Window<'a> {
    s: &'a Session,
    colors: Colors,
    log_start: usize,
    invariants: Result<(), String>,
}

impl<'a> Window<'a> {
    fn open(s: &'a Session, run: Run) -> Self {
        Self {
            s,
            colors: run.colors(),
            log_start: s.log_text().len(),
            invariants: Ok(()),
        }
    }

    /// Send `bytes`, wait for a quiet screen and check the invariants,
    /// paint-once included.
    fn step(&mut self, name: &str, bytes: &[u8]) {
        let log_before = self.s.log_text().len();
        let raw_before = self.s.raw_output().len();
        let since = Instant::now();
        self.s.send(bytes);
        settle(self.s, name, since);
        if self.invariants.is_ok() {
            self.invariants = invariant_errors(self.s, name, log_before, &self.colors)
                .and_then(|()| frame_errors(self.s, name, raw_before));
        }
    }

    /// One step per character of `text`.
    fn type_text(&mut self, text: &str) {
        for ch in text.chars() {
            self.step(&format!("{ch:?}"), ch.to_string().as_bytes());
        }
    }

    /// What the scenario left, with `typed` on the prompt row after `% `.
    fn seen(&self, typed: &str) -> Seen {
        let s = self.s;
        let log = s.log_text();
        let lines_with = |needle: &str| -> Vec<String> {
            log.lines()
                .filter(|line| line.contains(needle))
                .map(str::to_owned)
                .collect()
        };
        let card = s.card();
        let popup = card.map(|card| {
            s.with_screen(|screen| {
                (
                    harness::labels(screen, &card, &self.colors),
                    harness::selected_row(screen, &card, &self.colors),
                )
            })
        });
        let window = log.get(self.log_start..).unwrap_or_default();
        Seen {
            banned: s.banned(),
            junk: junk(s, typed),
            caps: lines_with("caps sync="),
            ansi: lines_with("caps ansi ").into_iter().next(),
            popup,
            invariants: self.invariants.clone(),
            leak: leak(s, window, typed),
        }
    }
}

/// Text on the screen that is neither the expected prompt row (`% ` and
/// `typed`) nor inside the popup fac says it drew.
fn junk(s: &Session, typed: &str) -> Option<String> {
    let expected = format!("% {typed}");
    let card = s.card();
    s.with_screen(|screen| {
        let (rows, cols) = (screen.size().0.get(), screen.size().1.get());
        let (cursor_row, _) = screen.cursor_position();
        let prompt = suite::row_text(screen, cursor_row);
        if prompt.trim_end() != expected.trim_end() {
            return Some(format!(
                "prompt row {cursor_row} is {prompt:?}, not {expected:?}"
            ));
        }
        let inside = |row: u16, col: u16| {
            card.iter().any(|card| {
                std::iter::once((card.row, card.col, card.rows, card.cols))
                    .chain(card.popout)
                    .any(|(top, left, height, width)| {
                        (top..top + height).contains(&row) && (left..left + width).contains(&col)
                    })
            })
        };
        for row in (0..rows).filter(|&row| row != cursor_row) {
            for col in (0..cols).filter(|&col| !inside(row, col)) {
                let text = screen.cell(row, col).map(|cell| cell.contents());
                if text.is_some_and(|text| !text.trim().is_empty()) {
                    return Some(format!(
                        "{text:?} at ({row}, {col}) on row {:?}",
                        suite::row_text(screen, row)
                    ));
                }
            }
        }
        None
    })
}

/// Reply bytes that reached zsh: a buffer fac reported in `window` (its
/// log) that is not a prefix of `typed`, or reply residue on the screen.
fn leak(s: &Session, window: &str, typed: &str) -> Option<String> {
    let prefixes: Vec<String> = typed
        .char_indices()
        .map(|(at, _)| &typed[..at])
        .chain([typed])
        .map(|prefix| format!("{prefix:?}"))
        .collect();
    for line in window.lines().filter(|line| line.contains("report seq=")) {
        let buffer = line
            .split(" buffer=")
            .nth(1)
            .and_then(|rest| rest.split(" prebuffer=").next())
            .unwrap_or_default();
        if !prefixes.iter().any(|prefix| prefix == buffer) {
            return Some(format!("buffer {buffer} while typing {typed:?}"));
        }
    }
    residue(&s.screen_text()).map(|found| format!("{found:?} on the screen"))
}

/// What replies leave when zsh takes them as typed text: `rgb:` (OSC 10,
/// 11, 4), `$y` (DECRPM), `^[`, and the parameters of a CPR or DA1 reply
/// (digits and `;` ending in `R` or `c`, with a `;` or after `?`).
fn residue(text: &str) -> Option<String> {
    for needle in ["rgb:", "$y", "^["] {
        if text.contains(needle) {
            return Some(needle.to_owned());
        }
    }
    let bytes = text.as_bytes();
    for (end, &byte) in bytes.iter().enumerate() {
        if byte != b'R' && byte != b'c' {
            continue;
        }
        let start = bytes[..end]
            .iter()
            .rposition(|&b| !(b.is_ascii_digit() || b == b';'))
            .map_or(0, |at| at + 1);
        let params = &bytes[start..end];
        let after_query = start > 0 && matches!(bytes[start - 1], b'?' | b'>');
        if params.iter().any(u8::is_ascii_digit) && (params.contains(&b';') || after_query) {
            return Some(text[start.saturating_sub(1)..=end].to_owned());
        }
    }
    None
}

/// zsh's default, which the fixture `.zshrc` turns off. Without it the first
/// prompt's carriage return hides what a terminal drew before it; with it
/// that stays above the prompt, as users see it (Terminal.app's `p%` in
/// findings.md).
const PROMPT_SP: &str = "setopt prompt_sp";

/// Spawn fac in the fixture tree in a terminal that behaves like `run`.
fn spawn(run: Run, rows: u16, cols: u16) -> (Session, suite::Tree, Instant) {
    let spawned = Instant::now();
    let env = if run.colorterm {
        vec![("COLORTERM".to_owned(), "truecolor".to_owned())]
    } else {
        Vec::new()
    };
    let (s, tree) = suite::spawn_in_tree(PROMPT_SP, rows, cols, env, |opts| {
        opts.set_profile(run.profile);
    });
    (s, tree, spawned)
}

/// Wait for the first prompt, with whatever the terminal drew before it.
fn wait_first_prompt(s: &Session, spawned: Instant) {
    s.wait_for(
        "the first prompt",
        |screen| {
            let (row, _) = screen.cursor_position();
            suite::row_text(screen, row).trim_end().ends_with('%')
        },
        harness::TIMEOUT,
    );
    settle(s, "startup", spawned);
}

/// startup, `ls -`, `git ch`, Down and Esc in a 24x80 terminal.
fn main_session(run: Run) -> Vec<((Run, &'static str), Seen)> {
    let (s, _tree, spawned) = spawn(run, 24, 80);
    let mut seen = Vec::new();
    let window = Window::open(&s, run);
    wait_first_prompt(&s, spawned);
    seen.push(((run, STARTUP), window.seen("")));
    suite::ready(&s);

    let mut window = Window::open(&s, run);
    window.type_text("ls -");
    seen.push(((run, LS), window.seen("ls -")));

    let mut window = Window::open(&s, run);
    window.step("Ctrl-U", CTRL_U);
    window.type_text("git ch");
    seen.push(((run, GIT_CH), window.seen("git ch")));

    let mut window = Window::open(&s, run);
    window.step("Down", DOWN);
    seen.push(((run, DOWN_KEY), window.seen("git ch")));

    let mut window = Window::open(&s, run);
    window.step("Esc", ESC);
    seen.push(((run, ESC_KEY), window.seen("git ch")));
    seen
}

/// `git ch` in a 5-row pane.
fn short_session(run: Run) -> Vec<((Run, &'static str), Seen)> {
    let (s, _tree, spawned) = spawn(run, 5, 80);
    wait_first_prompt(&s, spawned);
    suite::ready(&s);
    let mut window = Window::open(&s, run);
    window.type_text("git ch");
    vec![((run, SHORT_PANE), window.seen("git ch"))]
}

/// fac's caps line for this profile's answers. `dark` is false only on
/// Terminal.app's white background (findings.md's startup table).
fn expected_caps(profile: Profile) -> String {
    let (r, g, b) = profile.background();
    format!(
        "caps sync={} dark={} bg=Some(({r}, {g}, {b}))",
        profile.sync(),
        profile != Profile::Apple,
    )
}

/// fac's `caps ansi` line for this profile's OSC 4 answers.
fn expected_ansi(profile: Profile) -> String {
    let [green, yellow, blue, magenta, cyan] = profile
        .palette()
        .map(|(r, g, b)| format!("#{r:02x}{g:02x}{b:02x}"));
    format!("caps ansi green={green} yellow={yellow} blue={blue} magenta={magenta} cyan={cyan}")
}

/// Each check's result for `seen` under `profile`, with what it saw when
/// it failed. `ghostty` is the same scenario under `Profile::Ghostty`.
fn checks(
    profile: Profile,
    seen: &Seen,
    ghostty: &Seen,
) -> Vec<(&'static str, Result<(), String>)> {
    let check = |ok: bool, what: String| if ok { Ok(()) } else { Err(what) };
    let caps = expected_caps(profile);
    let ansi = expected_ansi(profile);
    vec![
        (
            NO_BANNED,
            check(
                seen.banned.is_empty(),
                format!(
                    "{:?}",
                    seen.banned
                        .iter()
                        .map(|seq| String::from_utf8_lossy(seq).into_owned())
                        .collect::<Vec<_>>()
                ),
            ),
        ),
        (NO_JUNK, seen.junk.clone().map_or(Ok(()), Err)),
        (
            CAPS,
            check(
                seen.caps.last().is_some_and(|line| line.ends_with(&caps)),
                format!("{:?}, not {caps:?}", seen.caps.last()),
            ),
        ),
        (
            POPUP,
            seen.invariants.clone().and_then(|()| {
                check(
                    seen.popup == ghostty.popup,
                    format!("{:?}, under Ghostty {:?}", seen.popup, ghostty.popup),
                )
            }),
        ),
        (NO_REPLY_LEAK, seen.leak.clone().map_or(Ok(()), Err)),
        (
            STARTUP_COMPLETE,
            check(
                seen.caps.len() == 1
                    && seen.caps[0].ends_with(&caps)
                    && seen.ansi.as_ref().is_some_and(|line| line.ends_with(&ansi)),
                format!("caps {:?}, ansi {:?}", seen.caps, seen.ansi),
            ),
        ),
    ]
}

#[test]
fn every_profile_fails_only_where_expected() {
    let results: BTreeMap<(Run, &str), Seen> = std::thread::scope(|scope| {
        let handles: Vec<_> = RUNS
            .into_iter()
            .flat_map(|run| {
                [
                    scope.spawn(move || main_session(run)),
                    scope.spawn(move || short_session(run)),
                ]
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|handle| {
                handle
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            })
            .collect()
    });

    let mut failing = BTreeSet::new();
    let mut variant_failing = Vec::new();
    let mut report = String::new();
    for run in RUNS {
        let profile = run.profile;
        for scenario in SCENARIOS {
            let seen = &results[&(run, scenario)];
            let ghostty = &results[&(Run::plain(Profile::Ghostty), scenario)];
            for (check, result) in checks(profile, seen, ghostty) {
                if let Err(what) = result {
                    let _ = writeln!(report, "FAIL {run:?} {scenario:?} {check}: {what}");
                    if run.colorterm {
                        variant_failing.push((run, scenario, check));
                    } else {
                        failing.insert((profile, scenario, check));
                    }
                }
            }
        }
    }
    println!("--- failing ---\n{report}");

    let expected: BTreeSet<_> = EXPECTED_FAIL.iter().copied().collect();
    let unexpected: Vec<_> = failing.difference(&expected).collect();
    let passing: Vec<_> = expected.difference(&failing).collect();
    assert!(
        passing.is_empty(),
        "these pass now: remove it from EXPECTED_FAIL: {passing:?}\n{report}"
    );
    assert!(
        unexpected.is_empty(),
        "failing and not in EXPECTED_FAIL: {unexpected:?}\n{report}"
    );
    assert!(
        variant_failing.is_empty(),
        "failing with COLORTERM=truecolor: {variant_failing:?}\n{report}"
    );
}

#[test]
fn residue_finds_replies_and_not_typed_text() {
    for text in ["% git ch", "% ls -", "% echo c", "%"] {
        assert_eq!(residue(text), None, "{text:?}");
    }
    for text in [
        "% 20;16R",
        "% ?1;2c",
        "% ?6c",
        "% 11;rgb:ffff/ffff/ffff",
        "% 2026;2$y",
        "% ^[[?62;22c",
    ] {
        assert!(residue(text).is_some(), "{text:?}");
    }
}
