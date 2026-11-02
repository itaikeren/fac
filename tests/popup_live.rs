//! The popup end to end, through fac: typing opens it, its keys work as
//! agreed, accepted suggestions land in the command line, and it stays out
//! of the way in full-screen apps, pastes and resizes. Every test runs in a
//! fixture cwd (`fixture-dir/`, `fixture-other/`, `file-a`, `file-b`) with
//! a fixture history file.

mod harness;

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use harness::{Card, Session, SpawnOpts, TIMEOUT, assert_active, wait_log, wait_prompt};

const QUIET: Duration = Duration::from_millis(200);
const HISTORY: &str = "ls -la\necho hello\ngit status\ngit commit -m first\n";
const DOWN: &[u8] = b"\x1b[B";
const UP: &[u8] = b"\x1b[A";

/// The fixture tree, removed on drop.
struct Tree(PathBuf);

impl Tree {
    fn new() -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("fac-popup-live-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let cwd = root.join("cwd");
        for dir in ["fixture-dir", "fixture-other"] {
            std::fs::create_dir_all(cwd.join(dir)).expect("creating the fixture tree");
        }
        for file in ["file-a", "file-b"] {
            std::fs::write(cwd.join(file), "").expect("writing a fixture file");
        }
        std::fs::write(root.join("history"), HISTORY).expect("writing the history");
        Self(root)
    }

    fn cwd(&self) -> PathBuf {
        self.0.join("cwd")
    }

    fn history(&self) -> PathBuf {
        self.0.join("history")
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// fac in the fixture cwd with the fixture history, `extra` appended to the
/// fixture `.zshrc`; smoke-checked, at a prompt, with the worker ready.
fn start(extra: &str) -> (Session, Tree) {
    let tree = Tree::new();
    let zshrc = format!(
        "cd '{}'\nHISTFILE='{}'\n{extra}",
        tree.cwd().display(),
        tree.history().display()
    );
    let s = Session::spawn(SpawnOpts {
        zshrc_extra: Some(zshrc),
        ..SpawnOpts::default()
    });
    wait_prompt(&s);
    assert_active(&s);
    wait_log(&s, "worker ready");
    (s, tree)
}

/// The popup on the screen: the card fac's log says it drew, and the
/// labels of its list rows as the screen shows them.
struct Popup {
    card: Card,
    labels: Vec<String>,
}

impl Popup {
    fn top(&self) -> u16 {
        self.card.row
    }

    fn labels(&self) -> Vec<String> {
        self.labels.clone()
    }
}

fn popup(card: Option<&Card>, screen: &vt100::Screen) -> Option<Popup> {
    let card = *card?;
    let labels = harness::labels(screen, &card, &harness::colors());
    Some(Popup { card, labels })
}

/// The popup as it is now.
fn popup_now(s: &Session) -> Option<Popup> {
    let card = s.card();
    s.with_screen(|screen| popup(card.as_ref(), screen))
}

/// How many cells of the screen are in the popup's colours.
fn popup_cells(screen: &vt100::Screen) -> usize {
    harness::popup_cells(screen, &harness::colors())
}

/// Any list row drawn selected.
fn any_selected(screen: &vt100::Screen) -> bool {
    let colors = harness::colors();
    let (rows, cols) = (screen.size().0.get(), screen.size().1.get());
    (0..rows).any(|row| {
        (0..cols).any(|col| {
            screen
                .cell(row, col)
                .is_some_and(|cell| cell.bgcolor() == colors.selected)
        })
    })
}

/// Whether list row `index` of `popup` is drawn selected.
fn row_selected(screen: &vt100::Screen, popup: &Popup, index: u16) -> bool {
    harness::selected_row(screen, &popup.card, &harness::colors()) == Some(index)
}

/// Wait until `pred` holds for the popup (None: there is none) and the
/// screen.
fn wait_popup(s: &Session, what: &str, pred: impl Fn(Option<&Popup>, &vt100::Screen) -> bool) {
    s.wait_card(
        what,
        |card, screen| pred(popup(card, screen).as_ref(), screen),
        TIMEOUT,
    );
}

/// The line the cursor is on, trimmed.
fn cursor_line(screen: &vt100::Screen) -> String {
    let (row, _) = screen.cursor_position();
    screen
        .rows(0, screen.size().1.get())
        .nth(usize::from(row))
        .unwrap_or_default()
        .trim_end()
        .to_owned()
}

fn wait_popup_with(s: &Session, label_text: &str) {
    wait_popup(s, &format!("a popup with {label_text:?}"), |p, _| {
        p.is_some_and(|p| p.labels().iter().any(|l| l == label_text))
    });
}

/// Wait for compsys's options for `ls -` (the first row an option).
fn wait_options(s: &Session) {
    wait_popup(s, "a popup with options", |p, _| {
        p.is_some_and(|p| p.labels().first().is_some_and(|l| l.starts_with('-')))
    });
}

/// No popup, and none of its cells left on the screen.
fn wait_no_popup(s: &Session) {
    wait_popup(s, "no popup", |p, screen| {
        p.is_none() && popup_cells(screen) == 0
    });
}

fn no_popup(s: &Session) -> bool {
    s.card().is_none() && s.with_screen(popup_cells) == 0
}

fn wait_line(s: &Session, expected: &str) {
    s.wait_for(
        &format!("the command line {expected:?}"),
        |screen| cursor_line(screen) == expected,
        TIMEOUT,
    );
}

/// The background SGR of the card's rows, as fac writes it in the harness
/// (256 colours).
fn card_sgr() -> String {
    match harness::colors().card {
        vt100::Color::Idx(index) => format!(";48;5;{index}m"),
        other => panic!("the harness runs fac in 256 colours, not {other:?}"),
    }
}

fn count(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// `bytes` without CSI, OSC and two-byte escape sequences.
fn plain_text(bytes: &[u8]) -> String {
    let mut text = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != 0x1b {
            text.push(bytes[i]);
            i += 1;
            continue;
        }
        match bytes.get(i + 1) {
            Some(b'[') => {
                i += 2;
                while i < bytes.len() && !(0x40..=0x7e).contains(&bytes[i]) {
                    i += 1;
                }
                i += 1;
            }
            Some(b']') => {
                while i < bytes.len() && bytes[i] != 0x07 {
                    i += 1;
                }
                i += 1;
            }
            _ => i += 2,
        }
    }
    String::from_utf8_lossy(&text).into_owned()
}

#[test]
fn typing_opens_the_popup_with_nothing_selected() {
    let (s, _tree) = start("");
    s.send(b"ls -");
    wait_options(&s);
    s.wait_quiet(QUIET, TIMEOUT);
    let p = popup_now(&s).expect("popup");
    s.with_screen(|screen| {
        let (cursor_row, _) = screen.cursor_position();
        assert!(p.top() > cursor_row, "the popup is below the prompt row");
        assert_eq!(cursor_line(screen), "% ls -");
        assert!(!any_selected(screen), "nothing is selected");
    });
    // `-l` is among them (beyond the first rows): typed out, it is the
    // exact match on top.
    s.send(b"l");
    wait_popup(&s, "-l on top", |p, _| {
        p.is_some_and(|p| p.labels().first().map(String::as_str) == Some("-l"))
    });
    assert!(!s.with_screen(any_selected), "still nothing selected");
}

#[test]
fn tab_inserts_the_top_item_and_esc_closes() {
    let (s, _tree) = start("");
    s.send(b"cd fix");
    wait_popup(&s, "fixture-dir/ on top", |p, _| {
        p.is_some_and(|p| p.labels().first().map(String::as_str) == Some("fixture-dir/"))
    });
    s.wait_quiet(QUIET, TIMEOUT);
    s.send(b"\t");
    wait_line(&s, "% cd fixture-dir/");
    // The next level (an empty directory) has only Fig's `../`.
    wait_popup_with(&s, "../");
    s.wait_quiet(QUIET, TIMEOUT);
    let labels = popup_now(&s).map(|p| p.labels()).unwrap_or_default();
    assert_eq!(labels, ["../"]);
    // Esc closes it.
    s.send(b"\x1b");
    wait_no_popup(&s);
    s.wait_quiet(QUIET, TIMEOUT);
    // A new word (an empty line first) ends the Esc's suppression.
    s.send(b"\x15");
    wait_line(&s, "%");

    // Open it again and close it with Esc.
    s.send(b"\x15cd fix");
    wait_popup_with(&s, "fixture-dir/");
    s.wait_quiet(QUIET, TIMEOUT);
    s.send(b"\x1b");
    wait_no_popup(&s);
    s.wait_quiet(QUIET, TIMEOUT);
    s.with_screen(|screen| {
        assert_eq!(cursor_line(screen), "% cd fix", "Esc did not reach zsh");
        let (cursor_row, _) = screen.cursor_position();
        let cols = screen.size().1.get();
        for (row, line) in screen
            .rows(0, cols)
            .enumerate()
            .skip(usize::from(cursor_row) + 1)
        {
            assert!(line.trim().is_empty(), "residue on row {row}: {line:?}");
        }
        assert!(!any_selected(screen));
    });
    // Typing on in the same word keeps it closed.
    s.send(b"t");
    wait_line(&s, "% cd fixt");
    s.wait_quiet(QUIET, TIMEOUT);
    assert!(no_popup(&s));
}

#[test]
fn enter_with_nothing_selected_runs_the_line() {
    let (s, _tree) = start("");
    s.send(b"ls");
    wait_popup(&s, "a popup", |p, _| p.is_some());
    s.wait_quiet(QUIET, TIMEOUT);
    s.send(b"\r");
    s.wait_text("fixture-other", TIMEOUT);
    wait_prompt(&s);
    s.wait_quiet(QUIET, TIMEOUT);
    assert!(no_popup(&s), "{}", s.screen_text());
    assert!(s.screen_text().contains("file-a"));
}

#[test]
fn down_down_tab_inserts_the_second_item() {
    let (s, _tree) = start("");
    s.send(b"ls -");
    wait_options(&s);
    s.wait_quiet(QUIET, TIMEOUT);
    let second = popup_now(&s)
        .expect("popup")
        .labels()
        .get(1)
        .cloned()
        .expect("a second item");
    s.send(DOWN);
    s.send(DOWN);
    wait_popup(&s, "the second row selected", |p, screen| {
        p.is_some_and(|p| row_selected(screen, p, 1))
    });
    s.send(b"\t");
    wait_line(&s, &format!("% ls {second}"));
}

#[test]
fn keys_typed_during_an_apply_are_kept_in_order() {
    let (s, _tree) = start("");
    s.send(b"cd fix");
    wait_popup_with(&s, "fixture-dir/");
    s.wait_quiet(QUIET, TIMEOUT);
    s.send(b"\tx");
    wait_line(&s, "% cd fixture-dir/x");
    wait_log(&s, "apply hold released (report)");
}

#[test]
fn a_tab_typed_before_the_report_acts_on_the_new_line() {
    let (s, _tree) = start("mkdir -p alpha beta");
    s.send(b"cd ");
    wait_popup_with(&s, "alpha/");
    wait_worker_done(&s, "\"cd \" in ");
    s.wait_quiet(QUIET, TIMEOUT);
    // One write: the Tab arrives before the shell has reported `b`.
    s.send(b"b\t");
    wait_line(&s, "% cd beta/");
}

#[test]
fn a_fast_enter_runs_the_line() {
    let (s, _tree) = start("");
    // At the prompt, popup closed.
    s.send(b"ls\r");
    s.wait_text("fixture-other", TIMEOUT);
    wait_prompt(&s);
    // With the popup open for `l`: `s` and Enter in one write.
    s.send(b"l");
    wait_popup(&s, "a popup", |p, _| p.is_some());
    s.wait_quiet(QUIET, TIMEOUT);
    s.send(b"s\r");
    s.wait_for(
        "a second listing",
        |screen| screen.contents().matches("fixture-other").count() == 2,
        TIMEOUT,
    );
    wait_prompt(&s);
    s.wait_quiet(QUIET, TIMEOUT);
    assert!(no_popup(&s));
}

#[test]
fn files_stay_after_a_compsys_timeout() {
    let completions = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/completions");
    let (s, _tree) = start(&format!("fpath=('{}' $fpath)", completions.display()));
    // `_hangcmd` hangs until the worker's ^C at 1 s; files stand in meanwhile.
    s.send(b"hangcmd f");
    wait_popup_with(&s, "file-a");
    wait_log(&s, "worker query timed out");
    // A key after the timeout: files are still offered for the word.
    s.send(b"i");
    wait_popup(&s, "files for `hangcmd fi`", |p, screen| {
        cursor_line(screen) == "% hangcmd fi"
            && p.is_some_and(|p| p.labels().iter().any(|l| l == "file-a"))
    });
    s.wait_quiet(QUIET, TIMEOUT);
    assert!(
        popup_now(&s).is_some_and(|p| p.labels().iter().any(|l| l == "file-a")),
        "{}",
        s.screen_text()
    );
    // The failure is cached: the word is not asked about again.
    let asked = s
        .log_text()
        .lines()
        .filter(|line| line.contains("worker query id=") && line.contains("key=\"hangcmd \""))
        .count();
    assert_eq!(asked, 1, "{}", s.log_text());
}

#[test]
fn a_paste_is_forwarded_whole_and_closes_the_popup() {
    let (s, _tree) = start("");
    s.send(b"echo ");
    wait_popup(&s, "a popup", |p, _| p.is_some());
    s.wait_quiet(QUIET, TIMEOUT);
    let keys_before = s.log_text().matches("popup key=").count();
    s.send(b"\x1b[200~echo pasted\r\x1b[201~");
    wait_log(&s, "popup closed (paste)");
    s.wait_quiet(QUIET, TIMEOUT);
    assert_eq!(
        s.log_text().matches("popup key=").count(),
        keys_before,
        "a key inside the paste was intercepted"
    );
    assert!(no_popup(&s), "{}", s.screen_text());
    s.with_screen(|screen| {
        // The pasted newline did not run the line.
        assert!(
            screen.contents().contains("% echo echo pasted"),
            "{}",
            screen.contents()
        );
    });
}

#[test]
fn no_popup_in_the_alternate_screen() {
    let (s, _tree) = start("");
    s.send(br"printf '\e[?1049h'; read -k1; printf '\e[?1049l'");
    s.wait_quiet(QUIET, TIMEOUT);
    s.send(b"\r");
    wait_log(&s, "alt_screen=on");
    s.wait_quiet(QUIET, TIMEOUT);
    s.send(b"l");
    wait_log(&s, "alt_screen=off");
    wait_prompt(&s);
    let log = s.log_text();
    let on = log.find("alt_screen=on").expect("alt_screen=on logged");
    let off = on
        + log[on..]
            .find("alt_screen=off")
            .expect("alt_screen=off after it");
    assert!(
        !log[on..off].contains("popup drawn"),
        "a popup was drawn in the alternate screen"
    );
    let raw = s.raw_output();
    let on = find(&raw, b"\x1b[?1049h").expect("1049h in the output");
    let off = on + find(&raw[on..], b"\x1b[?1049l").expect("1049l after it");
    assert!(
        find(&raw[on..off], card_sgr().as_bytes()).is_none(),
        "the card's colour was written in the alternate screen"
    );
    s.send(b"l");
    wait_popup(&s, "a popup", |p, _| p.is_some());
}

#[test]
fn resize_erases_the_popup() {
    let (s, _tree) = start("");
    s.send(b"ls -");
    wait_options(&s);
    s.wait_quiet(QUIET, TIMEOUT);
    s.resize(30, 80);
    wait_log(&s, "popup closed (resize)");
    // The log can be read before the erase reaches the screen, when the
    // output has been quiet for long already: wait for the erase itself.
    s.wait_for(
        "the popup erased",
        |screen| popup_cells(screen) == 0,
        TIMEOUT,
    );
    s.wait_quiet(QUIET, TIMEOUT);
    assert_eq!(
        s.with_screen(popup_cells),
        0,
        "the old popup was left behind:\n{}",
        s.screen_text()
    );
    s.send(b"l");
    wait_popup(&s, "one popup for `ls -l`", |p, screen| {
        cursor_line(screen) == "% ls -l" && p.is_some()
    });
    s.wait_quiet(QUIET, TIMEOUT);
    let p = popup_now(&s).expect("popup");
    s.with_screen(|screen| {
        // Every cell in the popup's colours is on the one card.
        let card = p.card;
        let on_card = (card.row..=card.last_row())
            .flat_map(|row| (card.col..card.col + card.cols).map(move |col| (row, col)))
            .filter(|&(row, col)| {
                screen
                    .cell(row, col)
                    .is_some_and(|cell| harness::colors().is_popup(cell))
            })
            .count();
        assert_eq!(on_card, popup_cells(screen), "{}", screen.contents());
        assert!(p.top() > screen.cursor_position().0);
        assert!(p.labels().iter().any(|l| l == "-l"), "{:?}", p.labels());
    });
}

/// fac's log after its line for the latest resize (the log is written in
/// the background, so a length taken before the resize is no mark).
fn after_resize(log: &str) -> &str {
    log.rfind("fac::proxy: resize rows=")
        .map_or("", |at| &log[at..])
}

/// Wait for a CPR reply to fac after the resize: `reconcile` logs either.
fn wait_cpr_after_resize(s: &Session) {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let log = s.log_text();
        let after = after_resize(&log);
        if after.contains("cpr ok") || after.contains("cpr mismatch") {
            return;
        }
        assert!(Instant::now() < deadline, "no cpr reply:\n{log}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// What a resize at a prompt wrote, from byte `from` of the raw output:
/// zsh's redraw (from the prompt's start, ED first), with fac's CPR right
/// after the redraw's 133;A and no other byte of fac's.
fn assert_only_the_redraw(raw: &[u8]) {
    assert!(
        harness::frames::segments(raw, 24).is_empty() && find(raw, b"\x1b[?2026h").is_none(),
        "fac wrote bytes of its own: {:?}",
        String::from_utf8_lossy(raw)
    );
    let ed = find(raw, b"\x1b[J").expect("zsh's ED");
    let mark = find(raw, b"\x1b]133;A\x07").expect("the redraw's 133;A");
    assert!(ed < mark, "{:?}", String::from_utf8_lossy(raw));
    assert_eq!(
        count(raw, b"\x1b[6n"),
        1,
        "{:?}",
        String::from_utf8_lossy(raw)
    );
    assert!(
        raw[mark..].starts_with(b"\x1b]133;A\x07\x1b[6n"),
        "{:?}",
        String::from_utf8_lossy(raw)
    );
}

/// The width changes to `cols` with the popup drawn for `typed` (the
/// command line `line`, the cursor in column `cursor_col`). fac writes nothing for the
/// popup: zsh's redraw goes up to the prompt's start (`moves_up`: with
/// CUU, the line had wrapped above the cursor) and clears below it. Its
/// CPR clears the model's dirty mark, so `next` brings the popup back.
fn width_change_left_to_zsh(
    typed: &[u8],
    line: &str,
    cursor_col: u16,
    cols: u16,
    moves_up: bool,
    next: &[u8],
) {
    let (s, _tree) = start("");
    s.send(typed);
    wait_popup(&s, "a popup at the cursor", |p, screen| {
        p.is_some() && screen.cursor_position().1 == cursor_col
    });
    s.wait_quiet(QUIET, TIMEOUT);
    assert!(s.with_screen(popup_cells) > 0);
    let before = s.raw_output().len();
    s.resize(24, cols);
    wait_log(
        &s,
        "width changed with the popup drawn; dropped without an erase, the shell's redraw clears it",
    );
    wait_log(&s, "popup closed (resize)");
    wait_cpr_after_resize(&s);
    s.wait_quiet(QUIET, TIMEOUT);
    let raw = s.raw_output();
    let after = &raw[before..];
    assert_only_the_redraw(after);
    let ed = find(after, b"\x1b[J").expect("zsh's ED");
    assert_eq!(
        find(&after[..ed], b"\x1b[A").is_some(),
        moves_up,
        "{:?}",
        String::from_utf8_lossy(after)
    );
    s.with_screen(|screen| {
        assert_eq!(popup_cells(screen), 0, "{}", screen.contents());
        assert!(
            screen.contents().contains(&format!("% {line}")),
            "{}",
            screen.contents()
        );
    });
    let log = s.log_text();
    assert!(!after_resize(&log).contains("popup drawn items="), "{log}");
    let key = s.raw_output().len();
    s.send(next);
    wait_popup(&s, "the popup back", |p, _| p.is_some());
    s.wait_quiet(QUIET, TIMEOUT);
    harness::frames::check(&s.raw_output()[key..], 24, cols, s.sync_output())
        .unwrap_or_else(|err| panic!("{err}"));
}

#[test]
fn width_change_leaves_the_clear_to_zsh() {
    width_change_left_to_zsh(b"ls -", "ls -", 6, 60, false, b"l");
}

#[test]
fn width_change_wider_leaves_the_clear_to_zsh() {
    width_change_left_to_zsh(b"ls -", "ls -", 6, 100, false, b"l");
}

/// The line wraps at both widths, with the cursor at its end: zsh moves
/// up to the prompt's row before its ED.
#[test]
fn width_change_with_a_wrapped_line() {
    let line = format!("ls {} -", "x".repeat(90));
    width_change_left_to_zsh(line.as_bytes(), &line, 17, 60, true, b"l");
}

/// The cursor in the middle of a line that wraps onto three rows at the new
/// width; the card covered the line's own rows after the cursor.
#[test]
fn width_change_with_the_cursor_mid_line() {
    let line = format!("ls -l {}", "x".repeat(120));
    let mut typed = line.clone().into_bytes();
    typed.extend(b"\x1b[D".repeat(120));
    width_change_left_to_zsh(&typed, &line, 8, 50, false, b"\x1b[D");
}

#[test]
fn width_change_without_a_popup_writes_nothing() {
    let (s, _tree) = start("");
    s.wait_quiet(QUIET, TIMEOUT);
    let before = s.raw_output().len();
    s.resize(24, 60);
    wait_cpr_after_resize(&s);
    s.wait_quiet(QUIET, TIMEOUT);
    assert_only_the_redraw(&s.raw_output()[before..]);
    let log = s.log_text();
    assert!(!after_resize(&log).contains("width changed"), "{log}");
}

/// A height change with the popup drawn: erased from the model, as before,
/// ahead of zsh's redraw.
#[test]
fn height_change_erases_from_the_model() {
    let (s, _tree) = start("");
    s.send(b"ls -");
    wait_options(&s);
    s.wait_quiet(QUIET, TIMEOUT);
    let before = s.raw_output().len();
    s.resize(30, 80);
    wait_log(&s, "popup closed (resize)");
    wait_cpr_after_resize(&s);
    s.wait_quiet(QUIET, TIMEOUT);
    let raw = s.raw_output();
    let after = &raw[before..];
    let segments = harness::frames::segments(after, 30);
    assert_eq!(segments.len(), 1, "{:?}", String::from_utf8_lossy(after));
    let erase = find(after, segments[0]).expect("the erase");
    let ed = find(after, b"\x1b[J").expect("zsh's ED");
    assert!(erase < ed, "{:?}", String::from_utf8_lossy(after));
    harness::frames::check(after, 30, 80, s.sync_output()).unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(s.with_screen(popup_cells), 0);
    let log = s.log_text();
    assert!(!after_resize(&log).contains("width changed"), "{log}");
}

#[test]
fn one_keystroke_is_one_synchronized_frame() {
    let (s, _tree) = start("");
    s.send(b"ls -");
    wait_options(&s);
    s.wait_quiet(QUIET, TIMEOUT);
    let before = s.raw_output().len();
    let draws = s.log_text().matches("popup drawn").count();
    s.send(b"l");
    let deadline = Instant::now() + TIMEOUT;
    while s.log_text().matches("popup drawn").count() == draws {
        assert!(Instant::now() < deadline, "no new popup:\n{}", s.log_text());
        std::thread::sleep(Duration::from_millis(10));
    }
    // The frame in the byte stream, however many reads it came in: from
    // the first begin after the key to the next end.
    let begin = b"\x1b[?2026h";
    let end = b"\x1b[?2026l";
    let deadline = Instant::now() + TIMEOUT;
    let (raw, start, stop) = loop {
        let raw = s.raw_output();
        let after = &raw[before..];
        let found = find(after, begin).and_then(|start| {
            find(&after[start..], end).map(|len| (before + start, before + start + len + end.len()))
        });
        if let Some((start, stop)) = found {
            break (raw, start, stop);
        }
        assert!(
            Instant::now() < deadline,
            "no whole frame: {:?}",
            String::from_utf8_lossy(&raw[before..])
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(
        start == before,
        "output before the frame: {:?}",
        String::from_utf8_lossy(&raw[before..stop])
    );
    let frame = &raw[start..stop];
    assert_eq!(
        count(frame, begin),
        1,
        "{:?}",
        String::from_utf8_lossy(frame)
    );
    // The echo, then the popup's change: fac's bytes in the frame paint
    // every cell of the new card that the frame changed, each once.
    let text = plain_text(frame);
    assert!(text.trim_start().starts_with('l'), "{text:?}");
    let (rows, cols) = (24, 80);
    let segments = harness::frames::segments(frame, rows);
    assert!(!segments.is_empty(), "{:?}", String::from_utf8_lossy(frame));
    let writes = harness::frames::interpret(&segments, rows, cols).expect("fac's bytes");
    assert_eq!(writes.twice(), None);
    let card = s.card().expect("a popup");
    let screen = |bytes: &[u8]| {
        let size = |n| std::num::NonZeroU16::new(n).expect("not zero");
        let mut parser = vt100::Parser::new(size(rows), size(cols), 0);
        parser.process(bytes);
        parser
    };
    let (before, after) = (screen(&raw[..start]), screen(&raw[..stop]));
    let changed: Vec<(u16, u16)> = (card.row..card.row + card.rows)
        .flat_map(|row| (card.col..card.col + card.cols).map(move |col| (row, col)))
        .filter(|&(row, col)| before.screen().cell(row, col) != after.screen().cell(row, col))
        .collect();
    assert!(!changed.is_empty(), "the frame changed no popup cell");
    for cell in &changed {
        assert!(
            !writes.of(*cell).is_empty(),
            "{cell:?} changed but fac did not paint it"
        );
    }
    assert!(
        harness::labels(after.screen(), &card, &harness::colors()).contains(&"-l".to_owned()),
        "{:?}",
        after.screen().contents()
    );

    // More keystrokes, for the latency stats.
    for key in [b"\x7f", b"l"].iter().cycle().take(20) {
        let draws = s.log_text().matches("popup drawn").count();
        s.send(*key);
        let deadline = Instant::now() + TIMEOUT;
        while s.log_text().matches("popup drawn").count() == draws {
            assert!(Instant::now() < deadline, "no new popup:\n{}", s.log_text());
            std::thread::sleep(Duration::from_millis(5));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    // The latency stats are logged when fac exits.
    s.send(b"\x15exit\r");
    s.wait_exit(TIMEOUT);
    let log = s.log_text();
    let stats = log
        .lines()
        .find(|line| line.contains("popup latency"))
        .expect("latency stats in the log");
    eprintln!("{stats}");
}

/// Waits until the worker finished the query whose log key starts with
/// `key` (`worker query id=N key=...` then `worker done id=N `).
fn wait_worker_done(s: &Session, key: &str) {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let log = s.log_text();
        let done = log
            .lines()
            .filter(|l| l.contains("worker query id=") && l.contains(&format!("key={key}")))
            .filter_map(|l| l.split("worker query id=").nth(1))
            .filter_map(|rest| rest.split(' ').next())
            .any(|id| log.contains(&format!("worker done id={id} ")));
        if done {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "worker never answered {key:?}:\n{log}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn history_lines_are_offered_and_inserted_whole() {
    let (s, _tree) = start("");
    s.send(b"git c");
    wait_popup(&s, "a popup", |p, _| p.is_some());
    // Wait until the worker has answered the `git ` query, then for quiet.
    wait_worker_done(&s, "\"git \" in ");
    s.wait_quiet(QUIET, TIMEOUT);
    // History ranks last: Up from nothing selects it.
    s.send(UP);
    wait_popup(&s, "the history line selected", |p, screen| {
        p.is_some_and(|p| {
            p.labels()
                .iter()
                .position(|l| l == "git commit -m first")
                .is_some_and(|i| row_selected(screen, p, u16::try_from(i).unwrap_or(0)))
        })
    });
    s.send(b"\r");
    wait_line(&s, "% git commit -m first");
}

#[test]
fn down_in_application_cursor_mode_selects_the_first_item() {
    let (s, _tree) = start("zle-line-init() { echoti smkx }\nzle -N zle-line-init");
    assert!(
        find(&s.raw_output(), b"\x1b[?1h").is_some(),
        "smkx was not sent"
    );
    s.send(b"ls -");
    wait_options(&s);
    s.wait_quiet(QUIET, TIMEOUT);
    s.send(b"\x1bOB");
    wait_popup(&s, "the first row selected", |p, screen| {
        p.is_some_and(|p| row_selected(screen, p, 0))
    });
}

/// A `git` first on `PATH` that takes a second to list branches, in a temp
/// dir of its own (removed on drop). Generators run with the shell's
/// exported environment, so they run it.
struct SlowGit(PathBuf);

impl SlowGit {
    fn new() -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("fac-slow-git-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("creating the slow git's dir");
        let git = dir.join("git");
        std::fs::write(
            &git,
            "#!/bin/sh\ncase \"$*\" in *branch*) sleep 1 ;; esac\nexec /usr/bin/git \"$@\"\n",
        )
        .expect("writing the slow git");
        std::fs::set_permissions(&git, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .expect("making it executable");
        Self(dir)
    }

    /// The `.zshrc` line that puts it first on `PATH`.
    fn path_line(&self) -> String {
        format!("PATH='{}':$PATH", self.0.display())
    }
}

impl Drop for SlowGit {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Make `dir` a git repo with branches `main`, `feature-one`, `feature-two`.
fn git_repo(dir: &std::path::Path) {
    for args in [
        &["init", "-q", "-b", "main"][..],
        &[
            "-c",
            "user.name=fac",
            "-c",
            "user.email=fac@example.com",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "init",
        ],
        &["branch", "feature-one"],
        &["branch", "feature-two"],
    ] {
        let status = std::process::Command::new("/usr/bin/git")
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .status()
            .expect("running git");
        assert!(status.success(), "git {args:?}");
    }
}

/// The label of the selected list row, if any.
fn selected_label(popup: &Popup, screen: &vt100::Screen) -> Option<String> {
    let labels = popup.labels();
    (0..labels.len())
        .find(|&i| row_selected(screen, popup, u16::try_from(i).unwrap_or(0)))
        .map(|i| labels[i].clone())
}

/// The popup's bottom line as drawn (its first row), trimmed.
fn bottom_line(popup: &Popup, screen: &vt100::Screen) -> String {
    let card = popup.card;
    if card.lines == 0 {
        return String::new();
    }
    screen
        .rows(card.col, card.cols)
        .nth(usize::from(card.bottom_row()))
        .unwrap_or_default()
        .trim()
        .to_owned()
}

/// `git checkout `: the arg's own `-` and the files of its template show at
/// once; the branches (described `Branch`, `Current branch`) merge in when
/// the slow branch generator answers, and the selected row stays selected.
#[test]
fn git_checkout_branches_merge_in_and_keep_the_selection() {
    let git = SlowGit::new();
    let (s, tree) = start(&git.path_line());
    git_repo(&tree.cwd());
    s.send(b"git checkout ");
    // The spec's own items are in once `-` is.
    wait_popup_with(&s, "-");
    let before = popup_now(&s).map(|p| p.labels()).unwrap_or_default();
    assert!(!before.iter().any(|label| label == "main"), "{before:?}");
    // Down, Down: the second row, `file-a`.
    s.send(DOWN);
    s.send(DOWN);
    wait_popup(&s, "file-a selected", |p, screen| {
        p.is_some_and(|p| selected_label(p, screen).as_deref() == Some("file-a"))
    });
    wait_popup_with(&s, "main");
    s.wait_quiet(QUIET, TIMEOUT);
    let p = popup_now(&s).expect("popup");
    s.with_screen(|screen| {
        let labels = p.labels();
        // Fig ranks them by their priority (100 for the current branch, 75
        // for the others), above the rest.
        assert_eq!(
            labels[..3],
            ["main", "feature-one", "feature-two"],
            "{labels:?}"
        );
        assert_eq!(
            selected_label(&p, screen).as_deref(),
            Some("file-a"),
            "{labels:?}"
        );
        // The selected file is described on the bottom line.
        assert_eq!(bottom_line(&p, screen), "file", "{labels:?}");
    });
    // A branch is inserted without a space after it, as in Fig.
    s.send(b"\x15");
    wait_line(&s, "%");
    s.send(b"git checkout feature-o");
    // The branch on top, its own description on the bottom line.
    wait_popup(&s, "the branch", |p, screen| {
        p.is_some_and(|p| {
            p.labels().first().map(String::as_str) == Some("feature-one")
                && bottom_line(p, screen) == "Branch"
        })
    });
    s.wait_quiet(QUIET, TIMEOUT);
    s.send(b"\t");
    wait_line(&s, "% git checkout feature-one");
    s.wait_quiet(QUIET, TIMEOUT);
    let line = "% git checkout feature-one";
    assert_eq!(
        s.with_screen(vt100::Screen::cursor_position).1,
        u16::try_from(line.len()).unwrap_or(0),
        "a space after the branch"
    );
}

/// `cd `: folders show at once (the file fast path, then zsh's), and Fig's
/// `folders()` helper, a custom generator that runs `ls`, replaces them
/// when it answers (it adds `../`); the popup never closes in between.
#[test]
fn cd_folders_fill_in_from_the_helper_without_going_empty() {
    let (s, _tree) = start("");
    let before = s.log_text().len();
    s.send(b"cd ");
    wait_popup(&s, "a popup", |p, _| p.is_some());
    wait_popup_with(&s, "../");
    s.wait_quiet(QUIET, TIMEOUT);
    let log = s.log_text();
    let log = log.get(before..).unwrap_or_default();
    assert!(log.contains("spec generated id="), "{log}");
    assert!(
        !log.contains("popup closed"),
        "the popup closed on its way to the helper's list:\n{log}"
    );
    let labels = popup_now(&s).map(|p| p.labels()).unwrap_or_default();
    assert!(labels.contains(&"fixture-dir/".to_owned()), "{labels:?}");
    assert!(!labels.contains(&"file-a".to_owned()), "{labels:?}");
}

/// What the shell exports reaches generators, as their process environment
/// and as a custom generator's `environmentVariables`: a variable exported
/// at the prompt is there for the next line (7791 `env`).
#[test]
fn the_shells_exported_environment_reaches_generators() {
    let (s, _tree) = start("");
    let specs = s.home().join(".config/fac/specs");
    std::fs::create_dir_all(&specs).expect("creating the user spec dir");
    std::fs::write(
        specs.join("facenv.js"),
        r#"export default { name: "facenv", args: { name: "value", generators: [
  { script: ["/usr/bin/printenv", "FAC_STEP8_VAR"], splitOn: "\n" },
  { custom: async (tokens, exec, context) =>
      [{ name: "ctx-" + context.environmentVariables.FAC_STEP8_VAR }] },
] } };"#,
    )
    .expect("writing the user spec");
    s.send(b"export FAC_STEP8_VAR=from-the-shell");
    wait_line(&s, "% export FAC_STEP8_VAR=from-the-shell");
    s.send(b"\r");
    s.wait_for(
        "the next prompt",
        |screen| cursor_line(screen) == "%",
        TIMEOUT,
    );
    s.send(b"facenv ");
    wait_popup_with(&s, "from-the-shell");
    wait_popup_with(&s, "ctx-from-the-shell");
}

/// The first column of list row `index`'s label: after the padding, the
/// icon and the space after it (as `harness::label` reads it).
fn label_col(screen: &vt100::Screen, card: &Card, index: u16) -> u16 {
    let row = card.row + index;
    let end = card.col + card.cols - 1;
    let blank = |col: u16| {
        screen
            .cell(row, col)
            .is_none_or(|cell| cell.contents().trim().is_empty())
    };
    let mut col = card.col + 1;
    while col < end && !blank(col) {
        col += 1;
    }
    while col < end && blank(col) {
        col += 1;
    }
    col
}

/// For each visible list row: its label, and which of the label's first
/// `typed + 1` cells have the matched background.
fn matched_cells(screen: &vt100::Screen, popup: &Popup, typed: u16) -> Vec<(String, Vec<bool>)> {
    let colors = harness::colors();
    (0..popup.card.list_rows)
        .map(|index| {
            let row = popup.card.row + index;
            let col = label_col(screen, &popup.card, index);
            let marks = (0..=typed)
                .map(|at| {
                    screen.cell(row, col + at).is_some_and(|cell| {
                        [colors.matched, colors.selected_matched].contains(&cell.bgcolor())
                    })
                })
                .collect();
            let label = popup
                .labels
                .get(usize::from(index))
                .cloned()
                .unwrap_or_default();
            (label, marks)
        })
        .collect()
}

/// At `git ch` every label matched by its prefix: both typed letters have
/// the matched background, the letter after them does not.
fn assert_ch_tinted(s: &Session) {
    wait_popup(s, "the `ch` items", |p, _| {
        p.is_some_and(|p| {
            !p.labels.is_empty() && p.labels.iter().all(|label| label.starts_with("ch"))
        })
    });
    s.wait_quiet(QUIET, TIMEOUT);
    let popup = popup_now(s).unwrap_or_else(|| panic!("no popup\n{}", s.screen_text()));
    let rows = s.with_screen(|screen| matched_cells(screen, &popup, 2));
    for (label, marks) in &rows {
        assert_eq!(
            marks,
            &vec![true, true, false],
            "{label:?}: the tint of `c`, `h` and the next letter\n{rows:?}\n{}",
            s.screen_text()
        );
    }
}

#[test]
fn every_matched_letter_is_tinted_when_typed_at_once() {
    let (s, _tree) = start("");
    s.send(b"git ch");
    assert_ch_tinted(&s);
}

#[test]
fn every_matched_letter_is_tinted_when_typed_one_by_one() {
    let (s, _tree) = start("");
    s.send(b"git c");
    wait_popup(&s, "the `c` items", |p, _| {
        p.is_some_and(|p| p.labels.iter().any(|label| label == "commit"))
    });
    s.wait_quiet(QUIET, TIMEOUT);
    s.send(b"h");
    assert_ch_tinted(&s);
}
