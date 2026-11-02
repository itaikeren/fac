//! Which popup row is selected and what each key does while the popup is
//! open (the agreed Fig/Warp rules).

use super::engine::{Source, Suggestion};
use crate::shell::protocol::BufferReport;

use super::tokenizer::{self, Dialect};

/// Rows shown at once until the controller sets the window (the layout's
/// `MAX_LIST_ROWS`).
pub const VISIBLE: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Tab,
    ShiftTab,
    Up,
    Down,
    Enter,
    Esc,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Accept(usize),
    /// The proxy forwards the key to the shell (Enter also closes the popup).
    PassThrough,
    Close,
    Redraw,
}

/// What a selection follows from one list to the next: the text accepting
/// the item gives, and whether it is a history line. Not where the item came
/// from: the file fast path's `src/` and compsys's `src/` that replaces it
/// are the same row to the user.
struct Identity {
    history: bool,
    text: String,
}

impl Identity {
    fn of(suggestion: &Suggestion) -> Self {
        Self {
            history: suggestion.id.source == Source::History,
            text: suggestion.id.text.clone(),
        }
    }

    fn is(&self, suggestion: &Suggestion) -> bool {
        self.history == (suggestion.id.source == Source::History) && self.text == suggestion.id.text
    }
}

pub struct Selection {
    selected: Option<usize>,
    scroll: usize,
    len: usize,
    ids: Vec<Identity>,
    /// Rows shown at once: the last layout's `list_rows`.
    window: usize,
}

impl Default for Selection {
    fn default() -> Self {
        Self {
            selected: None,
            scroll: 0,
            len: 0,
            ids: Vec::new(),
            window: VISIBLE,
        }
    }
}

impl Selection {
    pub fn new() -> Self {
        Self::default()
    }

    /// The popup shows `rows` list rows: scroll so the selected row is
    /// among them.
    pub fn set_window(&mut self, rows: usize) {
        self.window = rows.max(1);
        if let Some(i) = self.selected {
            self.reveal(i);
        }
        self.scroll = self.scroll.min(self.len.saturating_sub(self.window));
    }

    /// New items: keep the selection on the same item (by `Identity`) if it
    /// is still present, else clear it (and scroll back to the top); clamp
    /// scroll.
    pub fn update(&mut self, items: &[Suggestion]) {
        let previous = self.selected.and_then(|i| self.ids.get(i));
        self.selected = previous.and_then(|id| items.iter().position(|s| id.is(s)));
        self.ids = items.iter().map(Identity::of).collect();
        self.len = items.len();
        match self.selected {
            Some(i) => self.reveal(i),
            None => self.scroll = 0,
        }
        self.scroll = self.scroll.min(self.len.saturating_sub(self.window));
    }

    /// Nothing selected, scrolled to the top (the user typed: Fig starts
    /// every keystroke's list unselected).
    pub fn clear(&mut self) {
        self.selected = None;
        self.scroll = 0;
    }

    pub fn on_key(&mut self, key: Key) -> Action {
        if self.len == 0 {
            return if key == Key::Esc {
                Action::Close
            } else {
                Action::PassThrough
            };
        }
        let last = self.len - 1;
        match (key, self.selected) {
            (Key::Esc, _) => Action::Close,
            (Key::Tab, None) => Action::Accept(0),
            (Key::Tab | Key::Enter, Some(i)) => Action::Accept(i),
            (Key::Enter, None) => Action::PassThrough,
            (Key::Down, None) => self.select(0),
            (Key::Down, Some(i)) => self.select(if i >= last { 0 } else { i + 1 }),
            (Key::Up | Key::ShiftTab, None) => self.select(last),
            (Key::Up | Key::ShiftTab, Some(i)) => self.select(if i == 0 { last } else { i - 1 }),
        }
    }

    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    pub fn scroll(&self) -> usize {
        self.scroll
    }

    fn select(&mut self, i: usize) -> Action {
        self.selected = Some(i);
        self.reveal(i);
        Action::Redraw
    }

    /// Scroll just enough to keep row `i` inside the visible window.
    fn reveal(&mut self, i: usize) {
        if i < self.scroll {
            self.scroll = i;
        } else if i >= self.scroll + self.window {
            self.scroll = i + 1 - self.window;
        }
    }
}

/// Identifies "the current word" so Esc can suppress the popup until it
/// changes: the typed bytes of the line before the word start, plus the word
/// start offset.
pub fn word_identity(report: &BufferReport, dialect: Dialect) -> (String, usize) {
    let at = report
        .buffer
        .char_indices()
        .nth(report.cursor)
        .map_or(report.buffer.len(), |(i, _)| i);
    let (lbuffer, rbuffer) = report.buffer.split_at(at);
    let ctx = tokenizer::analyze(dialect, &report.prebuffer, lbuffer, rbuffer);
    let start = ctx.current.typed.start;
    let before = report.buffer.get(..start).unwrap_or("");
    (before.to_string(), start)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::complete::engine::{CompsysState, Engine, EngineInput};
    use crate::complete::history::History;
    use crate::complete::source::Candidate;
    use crate::complete::usage::Usage;
    use crate::guards::ShellKind;
    use crate::shell::protocol::ReportFlags;
    use std::path::Path;

    fn cand(word: &str) -> Candidate {
        Candidate {
            word: word.to_string(),
            display: word.to_string(),
            description: String::new(),
            group: String::new(),
            suffix: None,
            verbatim: false,
            is_file: false,
            iprefix: String::new(),
            prefix: String::new(),
            apre: String::new(),
            hpre: String::new(),
            hsuf: String::new(),
        }
    }

    fn report(buffer: &str) -> BufferReport {
        BufferReport {
            seq: 1,
            context: "start".to_string(),
            cursor: buffer.chars().count(),
            flags: ReportFlags::default(),
            prebuffer: String::new(),
            buffer: buffer.to_string(),
            undrawn: Vec::new(),
        }
    }

    fn items(words: &[&str]) -> Vec<Suggestion> {
        let cands: Vec<Candidate> = words.iter().map(|w| cand(w)).collect();
        let r = report("x ");
        let history = History::empty();
        let is_dir = |_: &Path| false;
        Engine::new()
            .compute(&EngineInput {
                shell: ShellKind::Zsh,
                report: &r,
                cwd: Path::new("/"),
                home: Path::new("/"),
                history: &history,
                usage: &Usage::default(),
                compsys: CompsysState::Ready(&cands),
                is_dir: &is_dir,
                specs: None,
                generated: None,
            })
            .suggestions
    }

    fn selection(n: usize) -> Selection {
        let words: Vec<String> = (0..n).map(|i| format!("w{i:02}")).collect();
        let refs: Vec<&str> = words.iter().map(String::as_str).collect();
        let mut s = Selection::new();
        s.update(&items(&refs));
        s
    }

    #[test]
    fn nothing_selected_column() {
        let mut s = selection(3);
        assert_eq!(s.on_key(Key::Tab), Action::Accept(0));
        assert_eq!(s.on_key(Key::Enter), Action::PassThrough);
        assert_eq!(s.on_key(Key::Esc), Action::Close);
        assert_eq!(s.selected(), None);
        assert_eq!(s.on_key(Key::Down), Action::Redraw);
        assert_eq!(s.selected(), Some(0));
        let mut s = selection(3);
        assert_eq!(s.on_key(Key::Up), Action::Redraw);
        assert_eq!(s.selected(), Some(2));
        let mut s = selection(3);
        assert_eq!(s.on_key(Key::ShiftTab), Action::Redraw);
        assert_eq!(s.selected(), Some(2));
    }

    #[test]
    fn selected_column_and_wrap() {
        let mut s = selection(3);
        s.on_key(Key::Down);
        assert_eq!(s.on_key(Key::Down), Action::Redraw);
        assert_eq!(s.selected(), Some(1));
        assert_eq!(s.on_key(Key::Tab), Action::Accept(1));
        assert_eq!(s.on_key(Key::Enter), Action::Accept(1));
        assert_eq!(s.on_key(Key::Esc), Action::Close);
        s.on_key(Key::Down);
        assert_eq!(s.selected(), Some(2));
        s.on_key(Key::Down);
        assert_eq!(s.selected(), Some(0), "Down wraps to the first");
        assert_eq!(s.on_key(Key::Up), Action::Redraw);
        assert_eq!(s.selected(), Some(2), "Up wraps to the last");
        assert_eq!(s.on_key(Key::ShiftTab), Action::Redraw);
        assert_eq!(s.selected(), Some(1));
    }

    #[test]
    fn zero_items() {
        let mut s = Selection::new();
        for key in [Key::Tab, Key::ShiftTab, Key::Up, Key::Down, Key::Enter] {
            assert_eq!(s.on_key(key), Action::PassThrough, "{key:?}");
        }
        assert_eq!(s.on_key(Key::Esc), Action::Close);
    }

    #[test]
    fn scroll_keeps_selection_visible() {
        let mut s = selection(20);
        for _ in 0..VISIBLE {
            s.on_key(Key::Down);
        }
        assert_eq!(s.selected(), Some(VISIBLE - 1));
        assert_eq!(s.scroll(), 0);
        s.on_key(Key::Down);
        assert_eq!(s.selected(), Some(VISIBLE));
        assert_eq!(s.scroll(), 1);
        s.on_key(Key::Up);
        s.on_key(Key::Up);
        assert_eq!(s.selected(), Some(VISIBLE - 2));
        assert_eq!(s.scroll(), 1);
        for _ in 0..VISIBLE - 2 {
            s.on_key(Key::Up);
        }
        assert_eq!(s.selected(), Some(0));
        assert_eq!(s.scroll(), 0);
        s.on_key(Key::Up);
        assert_eq!(s.selected(), Some(19));
        assert_eq!(s.scroll(), 20 - VISIBLE);
        s.on_key(Key::Down);
        assert_eq!((s.selected(), s.scroll()), (Some(0), 0));
    }

    #[test]
    fn the_window_follows_the_rows_shown() {
        let mut s = selection(20);
        s.set_window(3);
        for _ in 0..5 {
            s.on_key(Key::Down);
        }
        assert_eq!((s.selected(), s.scroll()), (Some(4), 2));
        let visible = |s: &Selection| {
            s.selected()
                .is_some_and(|i| (s.scroll()..s.scroll() + s.window).contains(&i))
        };
        assert!(visible(&s));
        s.set_window(8);
        assert!(visible(&s));
        assert_eq!(s.scroll(), 2);
        // Further down in the big window, then the window shrinks: the
        // selection is revealed again.
        for _ in 0..5 {
            s.on_key(Key::Down);
        }
        assert_eq!((s.selected(), s.scroll()), (Some(9), 2));
        s.set_window(3);
        assert_eq!(s.scroll(), 7);
        assert!(visible(&s));
        // Up from nothing in a small window shows the last rows.
        let mut s = selection(20);
        s.set_window(3);
        s.on_key(Key::Up);
        assert_eq!((s.selected(), s.scroll()), (Some(19), 17));
    }

    #[test]
    fn update_keeps_selection_by_id() {
        let mut s = Selection::new();
        s.update(&items(&["a", "b", "c"]));
        s.on_key(Key::Down);
        s.on_key(Key::Down);
        assert_eq!(s.selected(), Some(1)); // b
        s.update(&items(&["z", "a", "b", "c"]));
        assert_eq!(s.selected(), Some(2), "followed `b`");
        s.update(&items(&["a", "c"]));
        assert_eq!(s.selected(), None, "`b` is gone");
        assert_eq!(s.on_key(Key::Enter), Action::PassThrough);
    }

    #[test]
    fn clear_unselects_and_scrolls_to_the_top() {
        let mut s = selection(20);
        s.set_window(3);
        s.on_key(Key::Up);
        assert_eq!((s.selected(), s.scroll()), (Some(19), 17));
        s.clear();
        assert_eq!((s.selected(), s.scroll()), (None, 0));
        // The items are kept: keys still act on them.
        assert_eq!(s.on_key(Key::Tab), Action::Accept(0));
        s.on_key(Key::Up);
        assert_eq!((s.selected(), s.scroll()), (Some(19), 17));
    }

    /// The suggestions for `ls s` with compsys `Pending` (the file fast
    /// path) or answered with `src` as a directory.
    fn files_then_compsys(dir: &Path) -> (Vec<Suggestion>, Vec<Suggestion>) {
        let r = report("ls s");
        let history = History::empty();
        let is_dir = |path: &Path| path.is_dir();
        let run = |compsys| {
            Engine::new()
                .compute(&EngineInput {
                    shell: ShellKind::Zsh,
                    report: &r,
                    cwd: dir,
                    home: dir,
                    history: &history,
                    usage: &Usage::default(),
                    compsys,
                    is_dir: &is_dir,
                    specs: None,
                    generated: None,
                })
                .suggestions
        };
        let mut src = cand("src");
        src.is_file = true;
        src.suffix = Some("/".into());
        let mut spec = cand("spec.txt");
        spec.is_file = true;
        // compsys also has a word the files do not: `src/` moves down a row.
        let answered = [spec, src, cand("sb")];
        (
            run(CompsysState::Pending),
            run(CompsysState::Ready(&answered)),
        )
    }

    #[test]
    fn the_selection_survives_compsys_replacing_the_files() {
        let dir = crate::complete::test_util::TempDir::new("selection");
        std::fs::create_dir(dir.path().join("src")).expect("mkdir");
        std::fs::write(dir.path().join("spec.txt"), "").expect("write");
        let (files, compsys) = files_then_compsys(dir.path());
        let labels = |items: &[Suggestion]| -> Vec<(String, Source)> {
            items
                .iter()
                .map(|s| (s.label.clone(), s.id.source))
                .collect()
        };
        assert_eq!(
            labels(&files),
            [
                ("src/".to_string(), Source::File),
                ("spec.txt".to_string(), Source::File)
            ]
        );
        assert_eq!(
            labels(&compsys),
            [
                ("sb".to_string(), Source::Compsys),
                ("src/".to_string(), Source::Compsys),
                ("spec.txt".to_string(), Source::Compsys)
            ]
        );
        let mut s = Selection::new();
        s.update(&files);
        s.on_key(Key::Down);
        assert_eq!(s.selected(), Some(0), "src/ from the fast path");
        s.update(&compsys);
        assert_eq!(s.selected(), Some(1), "compsys's src/ is the same row");
        // A history line with the same text is another row: at `ls`,
        // compsys's `lsof` and the history line `lsof`.
        let r = report("ls");
        let mut history = History::empty();
        history.add("lsof");
        let is_dir = |_: &Path| false;
        let run = |cands: &[Candidate], history: &History| {
            Engine::new()
                .compute(&EngineInput {
                    shell: ShellKind::Zsh,
                    report: &r,
                    cwd: dir.path(),
                    home: dir.path(),
                    history,
                    usage: &Usage::default(),
                    compsys: CompsysState::Ready(cands),
                    is_dir: &is_dir,
                    specs: None,
                    generated: None,
                })
                .suggestions
        };
        let both = run(&[cand("lsof")], &history);
        assert_eq!(
            labels(&both),
            [
                ("lsof".to_string(), Source::Compsys),
                ("lsof".to_string(), Source::History)
            ]
        );
        assert_eq!(both[0].id.text, both[1].id.text);
        let mut s = Selection::new();
        s.update(&both);
        s.on_key(Key::Down);
        assert_eq!(s.selected(), Some(0));
        s.update(&run(&[], &history));
        assert_eq!(s.selected(), None, "not the history line");
    }

    #[test]
    fn update_clamps_scroll() {
        let mut s = selection(20);
        s.on_key(Key::Up);
        assert_eq!(s.scroll(), 12);
        s.update(&items(&["w00", "w01", "w02"]));
        assert_eq!(s.selected(), None);
        assert_eq!(s.scroll(), 0);
        let mut s = selection(20);
        s.on_key(Key::Up);
        let last_ten: Vec<String> = (10..20).map(|i| format!("w{i:02}")).collect();
        let refs: Vec<&str> = last_ten.iter().map(String::as_str).collect();
        s.update(&items(&refs));
        assert_eq!(s.selected(), Some(9));
        assert_eq!(s.scroll(), 2);
    }

    #[test]
    fn esc_identity_follows_word_start() {
        let id = |b: &str| word_identity(&report(b), Dialect::Zsh);
        assert_eq!(id("git ch"), id("git chec"));
        assert_eq!(id("git ch"), id("git c"));
        assert_ne!(id("git ch"), id("git checkout "));
        assert_ne!(id("git ch"), id("gi"));
        assert_ne!(id("git ch"), id("gat ch"));
        assert_eq!(id("git ch"), ("git ".to_string(), 4));
    }
}
