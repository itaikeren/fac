//! What the snapshot suite and the terminal profile tests share: the
//! fixture tree, starting fac in it, settling after a step, and the
//! invariants checked after every step.

#![allow(dead_code)]

use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crate::harness::{self, Card, Colors, Session, SpawnOpts, assert_active, wait_log};

/// How long a quiet screen must stay quiet to count as settled.
pub const QUIET: Duration = Duration::from_millis(150);
/// The most a step may take to settle, compsys included. Only a guard
/// against a hang: `settle` returns as soon as the screen is quiet.
pub const SETTLE: Duration = Duration::from_secs(15);

/// Oldest first, in zsh's extended history format.
const HISTORY: &str = ": 1700000000:0;ls -la\n\
                       : 1700000100:0;faccmd build --release\n\
                       : 1700000200:0;git commit -m first\n\
                       : 1700000300:0;echo ✅ done\n";

/// The fixture tree, removed on drop.
pub struct Tree(pub PathBuf);

impl Tree {
    pub fn new() -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("fac-snapshots-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let cwd = root.join("cwd");
        for dir in ["fixture-dir/inner-b", "fixture-other", "My Documents"] {
            std::fs::create_dir_all(cwd.join(dir)).expect("creating the fixture tree");
        }
        for file in ["file-a", "file-b", "fixture-dir/inner-a"] {
            std::fs::write(cwd.join(file), "").expect("writing a fixture file");
        }
        std::fs::write(root.join("history"), HISTORY).expect("writing the history");
        // The only commands on PATH, so the command-name popups do not list
        // whatever this macOS has in /usr/bin.
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).expect("creating the bin dir");
        for (name, target) in [("ls", "/bin/ls"), ("git", "/usr/bin/git")] {
            std::os::unix::fs::symlink(target, bin.join(name)).expect("linking a command");
        }
        Self(root)
    }

    pub fn cwd(&self) -> PathBuf {
        self.0.join("cwd")
    }

    pub fn bin(&self) -> PathBuf {
        self.0.join("bin")
    }

    pub fn history(&self) -> PathBuf {
        self.0.join("history")
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// fac in the fixture cwd with the fixture history, the `faccmd` fixture and
/// `extra` appended to the fixture `.zshrc`, in a `rows` x `cols` terminal,
/// with `extra_env` added to fac's environment and `tweak` changing the
/// session's options before it spawns. Not waited on.
pub fn spawn_in_tree(
    extra: &str,
    rows: u16,
    cols: u16,
    extra_env: Vec<(String, String)>,
    tweak: impl FnOnce(&mut SpawnOpts),
) -> (Session, Tree) {
    let tree = Tree::new();
    let completions = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/completions");
    let zshrc = format!(
        "cd '{}'\nPATH='{}'\nHISTFILE='{}'\nfpath=('{}' $fpath)\nfaccmd() {{ true }}\n{extra}",
        tree.cwd().display(),
        tree.bin().display(),
        tree.history().display(),
        completions.display(),
    );
    let mut opts = SpawnOpts {
        rows,
        cols,
        zshrc_extra: Some(zshrc),
        extra_env,
        ..SpawnOpts::default()
    };
    tweak(&mut opts);
    (Session::spawn(opts), tree)
}

/// After the first prompt: smoke-checked, at a prompt at the top of a
/// cleared screen, with the worker ready.
pub fn ready(s: &Session) {
    assert_active(s);
    wait_log(s, "worker ready");
    // Clear the smoke check's output (its pid and tty are not deterministic).
    s.send(b"\x0c");
    s.wait_for(
        "a prompt on the first row",
        |screen| screen.cursor_position() == (0, 2) && screen.contents().trim_end() == "%",
        harness::TIMEOUT,
    );
}

/// `ready` for a prompt fixture whose fresh prompt leaves the cursor at
/// `cursor` (its prompt is not `% ` alone on the screen): smoke-checked, at
/// a prompt at the top of a cleared screen, with the worker ready.
pub fn ready_at(s: &Session, cursor: (u16, u16)) {
    wait_log(s, "report seq=");
    s.send_line("echo ${FAC_ACTIVE:-none}");
    s.wait_for(
        "a FAC_ACTIVE value or none",
        |screen| {
            harness::has_line(screen, |line| {
                line == "none" || harness::is_fac_active_value(line)
            })
        },
        harness::TIMEOUT,
    );
    assert!(
        !harness::has_line(&s.with_screen(Clone::clone), |line| line == "none"),
        "fac is not active:\n{}",
        s.screen_text()
    );
    wait_log(s, "worker ready");
    s.wait_quiet(QUIET, harness::TIMEOUT);
    // Clear the smoke check's output (its pid and tty are not deterministic).
    let since = Instant::now();
    s.send(b"\x0c");
    s.wait_for(
        "the fixture's prompt at the top",
        |screen| screen.cursor_position() == cursor && !screen.contents().contains("ttys"),
        harness::TIMEOUT,
    );
    settle(s, "clear", since);
}

/// The text of screen row `row`.
pub fn row_text(screen: &vt100::Screen, row: u16) -> String {
    screen
        .rows(0, screen.size().1.get())
        .nth(usize::from(row))
        .unwrap_or_default()
}

/// The popup on the screen: the card's first and last row (as fac's log
/// gives them), its list rows, the bottom line's rows kept and drawn, and
/// the row drawn with the selected background, if any.
#[derive(Debug, PartialEq, Eq)]
pub struct PopupState {
    pub top: u16,
    pub bottom: u16,
    pub list_rows: u16,
    pub bottom_rows: u16,
    pub lines: u16,
    pub selected: Option<u16>,
}

impl std::fmt::Display for PopupState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "popup: rows {}..{} ({} list, bottom line {} of {}), selected ",
            self.top, self.bottom, self.list_rows, self.lines, self.bottom_rows
        )?;
        match self.selected {
            Some(row) => write!(f, "row {row}"),
            None => write!(f, "none"),
        }
    }
}

pub fn popup_state(card: Option<&Card>, screen: &vt100::Screen) -> Option<PopupState> {
    popup_state_with(card, screen, &harness::colors())
}

/// `popup_state` for a popup painted with `colors`.
pub fn popup_state_with(
    card: Option<&Card>,
    screen: &vt100::Screen,
    colors: &Colors,
) -> Option<PopupState> {
    let card = card?;
    Some(PopupState {
        top: card.row,
        bottom: card.last_row(),
        list_rows: card.list_rows,
        bottom_rows: card.bottom,
        lines: card.lines,
        selected: harness::selected_row(screen, card, colors).map(|index| card.row + index),
    })
}

/// The screen as a numbered listing down to the last row with text or the
/// cursor, then the cursor and the popup state.
pub fn render(card: Option<&Card>, screen: &vt100::Screen) -> String {
    render_with(card, screen, &harness::colors())
}

/// `render` for a popup painted with `colors`.
pub fn render_with(card: Option<&Card>, screen: &vt100::Screen, colors: &Colors) -> String {
    let (cursor_row, cursor_col) = screen.cursor_position();
    let rows = screen.size().0.get();
    let last = (0..rows)
        .rev()
        .find(|&row| !row_text(screen, row).trim().is_empty())
        .unwrap_or(0)
        .max(cursor_row);
    let mut out = String::new();
    for row in 0..=last {
        let _ = writeln!(out, "{row:02}|{}", row_text(screen, row).trim_end());
    }
    let _ = writeln!(out, "cursor: row {cursor_row}, col {cursor_col}");
    match popup_state_with(card, screen, colors) {
        Some(state) => {
            let _ = writeln!(out, "{state}");
        }
        None => {
            let _ = writeln!(out, "popup: none");
        }
    }
    out
}

/// The ids of the compsys queries in the log that have no answer yet.
fn queries_in_flight(log: &str) -> Vec<u64> {
    let id_after = |line: &str, marker: &str| -> Option<u64> {
        let rest = line.split(marker).nth(1)?;
        rest.split(|ch: char| !ch.is_ascii_digit())
            .next()?
            .parse()
            .ok()
    };
    let mut open = Vec::new();
    let mut ended = HashSet::new();
    for line in log.lines() {
        if let Some(id) = id_after(line, "worker query id=") {
            open.push(id);
        }
        for marker in [
            "worker done id=",
            "worker failed id=",
            "worker recovered id=",
            "worker query timed out id=",
        ] {
            if let Some(id) = id_after(line, marker) {
                ended.insert(id);
            }
        }
    }
    open.retain(|id| !ended.contains(id));
    open
}

/// The spec loads, spec function calls and generator jobs in the log that
/// have no answer yet: `name=` for a load, `id=` for a call, `gen=` for a
/// generator job (a run, debounced or sent, or a function it waits on).
fn specs_in_flight(log: &str) -> Vec<String> {
    let after = |line: &str, marker: &str| -> Option<String> {
        let rest = line.split(marker).nth(1)?;
        Some(rest.split([' ', ':']).next()?.to_owned())
    };
    let mut open: Vec<String> = Vec::new();
    for line in log.lines() {
        if let Some(name) = after(line, "spec load name=") {
            open.push(format!("name={name}"));
        } else if let Some(name) =
            after(line, "spec ready name=").or_else(|| after(line, "spec failed name="))
        {
            open.retain(|load| *load != format!("name={name}"));
        } else if let Some(id) = after(line, "spec call id=") {
            open.push(format!("id={id}"));
        } else if let Some(id) = after(line, "spec called id=") {
            open.retain(|call| *call != format!("id={id}"));
        } else if let Some(id) = after(line, "spec generate id=") {
            open.push(format!("gen={id}"));
        } else if let Some(id) = after(line, "spec generated id=") {
            open.retain(|job| *job != format!("gen={id}"));
        }
    }
    open
}

/// Wait until fac's output has been quiet for `QUIET`, at least that long
/// has passed since the step started, no compsys query, spec load or spec
/// function call is in flight, and fac's log has not grown for `QUIET`.
///
/// The last condition closes a gap the in-flight lists cannot see: fac queues
/// a compsys query without logging it, and logs `query id=` only when it
/// sends it, right after the previous query's `done id=` (or after the
/// shell's report, when the worker is idle). A snapshot taken between the
/// two would miss a query that is about to start. Everything fac does about
/// a query logs, so a log that stays still for `QUIET` has no query on its
/// way; the cost is that a step never settles sooner than `QUIET` after its
/// last log line.
pub fn settle(s: &Session, step: &str, since: Instant) {
    let mut log_len = 0;
    let mut log_changed = Instant::now();
    loop {
        let log = s.log_text();
        if log.len() != log_len {
            log_len = log.len();
            log_changed = Instant::now();
        }
        let quiet_since = since.elapsed() >= QUIET;
        let waiting = s.quiet_for(QUIET);
        if quiet_since
            && waiting
            && log_changed.elapsed() >= QUIET
            && queries_in_flight(&log).is_empty()
            && specs_in_flight(&log).is_empty()
        {
            return;
        }
        assert!(
            since.elapsed() < SETTLE,
            "after step {step:?}: no quiet screen within {SETTLE:?} (queries in flight: {:?}, specs in flight: {:?})\n--- screen ---\n{}\n--- log ---\n{}",
            queries_in_flight(&log),
            specs_in_flight(&log),
            s.screen_text(),
            log
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The `real=(row, col)` of a `cpr mismatch` log line.
fn parse_real(line: &str) -> Option<(u16, u16)> {
    let rest = line.split("real=(").nth(1)?;
    let (pair, _) = rest.split_once(')')?;
    let (row, col) = pair.split_once(',')?;
    Some((row.trim().parse().ok()?, col.trim().parse().ok()?))
}

/// The invariants checked after every step, for a session painted with
/// `colors`: the first one broken, as a message.
pub fn invariant_errors(
    s: &Session,
    step: &str,
    log_before: usize,
    colors: &Colors,
) -> Result<(), String> {
    let card = s.card();
    s.with_screen(|screen| {
        let (rows, cols) = (screen.size().0.get(), screen.size().1.get());
        let (cursor_row, _) = screen.cursor_position();
        // (a) Nothing below the popup, or below the prompt when there is no
        // popup: a dismissed popup leaves no residue.
        let from = match &card {
            Some(card) => card.last_row() + 1,
            None => cursor_row + 1,
        };
        for row in from..rows {
            if !row_text(screen, row).trim().is_empty() {
                return Err(format!(
                    "after step {step:?}: residue on row {row}: {:?}\n{}",
                    row_text(screen, row),
                    render(card.as_ref(), screen)
                ));
            }
        }
        // (b) No cell in the popup's colours outside the card and the
        // popout fac says it drew; none at all without a popup.
        let inside = |row: u16, col: u16| {
            card.iter().any(|card| {
                let rects =
                    std::iter::once((card.row, card.col, card.rows, card.cols)).chain(card.popout);
                rects.into_iter().any(|(top, left, height, width)| {
                    (top..top + height).contains(&row) && (left..left + width).contains(&col)
                })
            })
        };
        for row in 0..rows {
            for col in 0..cols {
                let painted = screen
                    .cell(row, col)
                    .is_some_and(|cell| colors.is_popup(cell));
                if painted && !inside(row, col) {
                    return Err(format!(
                        "after step {step:?}: a popup cell left at ({row}, {col}): {:?}\n{}\ncard: {card:?}\ncolours: {colors:?}",
                        screen.cell(row, col),
                        render(card.as_ref(), screen)
                    ));
                }
            }
        }
        Ok(())
    })?;
    // (c) When fac reconciled its cursor with the terminal's in this step,
    // the terminal's cursor (the harness's) is what fac logged.
    let log = s.log_text();
    let new = log.get(log_before..).unwrap_or_default();
    if let Some(real) = new.lines().filter_map(parse_real).next_back() {
        let cursor = s.with_screen(vt100::Screen::cursor_position);
        if real != cursor {
            return Err(format!(
                "after step {step:?}: fac's cursor reconcile disagrees with the terminal: fac {real:?}, terminal {cursor:?}\n--- log ---\n{new}"
            ));
        }
    }
    Ok(())
}

/// The paint-once invariants for the output since `raw_before` (one step's,
/// settled): in no frame did fac write a cell twice, or erase a cell and
/// then paint it (`harness::frames`). A frame is a 2026 pair when the
/// terminal has synchronized output, else each of fac's segments.
pub fn frame_errors(s: &Session, step: &str, raw_before: usize) -> Result<(), String> {
    let raw = s.raw_output();
    let (rows, cols) = s.with_screen(|screen| (screen.size().0.get(), screen.size().1.get()));
    let since = raw.get(raw_before..).unwrap_or_default();
    harness::frames::check(since, rows, cols, s.sync_output())
        .map_err(|err| format!("after step {step:?}: {err}"))
}

/// `invariant_errors` as a panic.
pub fn check_invariants_with(s: &Session, step: &str, log_before: usize, colors: &Colors) {
    if let Err(err) = invariant_errors(s, step, log_before, colors) {
        panic!("{err}");
    }
}

/// Send `bytes` and wait for a quiet screen.
pub fn step(s: &Session, name: &str, bytes: &[u8]) {
    let since = Instant::now();
    s.send(bytes);
    settle(s, name, since);
}
