//! fac's model of the terminal screen: a vt100 parser fed every byte fac
//! forwards to the terminal (never the popup drawing).

use std::num::NonZeroU16;

pub struct ScreenModel {
    parser: vt100::Parser<Scrolls>,
    /// Set by a width change: vt100 does not reflow, real terminals do, so the
    /// model is not trusted until the next reconcile.
    dirty: bool,
    /// The shell saved its cursor (DECSC) and has not restored it: fac must
    /// not save its own over it.
    shell_saved_cursor: bool,
}

/// Counts the rows pushed off the top of the screen (the whole screen, or a
/// scroll region that starts at the top, scrolled up).
#[derive(Default)]
struct Scrolls(u64);

impl vt100::Callbacks for Scrolls {
    fn on_scroll(&mut self, _: vt100::capture::RowContents<'_>, _alternate_screen: bool) {
        self.0 = self.0.wrapping_add(1);
    }
}

fn non_zero(value: u16) -> NonZeroU16 {
    NonZeroU16::new(value).unwrap_or(NonZeroU16::MIN)
}

impl ScreenModel {
    pub fn new(rows: u16, cols: u16) -> Self {
        Self {
            parser: vt100::Parser::new_with_callbacks(
                non_zero(rows),
                non_zero(cols),
                0,
                Scrolls::default(),
            ),
            dirty: false,
            shell_saved_cursor: false,
        }
    }

    pub fn process(&mut self, bytes: &[u8]) {
        self.parser.process(bytes);
    }

    /// (row, col), 0-based. Past the last column (a pending wrap) the column
    /// is the last one, as a terminal reports it in a CPR.
    pub fn cursor(&self) -> (u16, u16) {
        let screen = self.parser.screen();
        let (row, col) = screen.cursor_position();
        let cols = screen.size().1.get();
        (row, col.min(cols - 1))
    }

    /// Move the model's cursor (only the model; nothing reaches the terminal).
    pub fn set_cursor(&mut self, row: u16, col: u16) {
        let cup = format!("\x1b[{};{}H", u32::from(row) + 1, u32::from(col) + 1);
        self.parser.process(cup.as_bytes());
    }

    /// A change of width sets `dirty`.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        let (_, old_cols) = self.parser.screen().size();
        let (rows, cols) = (non_zero(rows), non_zero(cols));
        if cols != old_cols {
            self.dirty = true;
        }
        self.parser.set_size(rows, cols);
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn clear_dirty(&mut self) {
        self.dirty = false;
    }

    pub fn screen(&self) -> &vt100::Screen {
        self.parser.screen()
    }

    /// How many rows have scrolled off the top so far (it wraps). A change
    /// means the screen's rows moved up.
    pub fn scrolls(&self) -> u64 {
        self.parser.callbacks().0
    }

    /// The shell saved its cursor (true) or restored or reset it (false):
    /// the scanner's `ShellCursorSaved`.
    pub fn set_shell_saved_cursor(&mut self, saved: bool) {
        self.shell_saved_cursor = saved;
    }

    pub fn shell_saved_cursor(&self) -> bool {
        self.shell_saved_cursor
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_follows_output() {
        let mut model = ScreenModel::new(24, 80);
        assert_eq!(model.cursor(), (0, 0));
        model.process(b"hello\r\nab");
        assert_eq!(model.cursor(), (1, 2));
        model.process("日本".as_bytes());
        assert_eq!(model.cursor(), (1, 6));
    }

    #[test]
    fn pending_wrap_reports_the_last_column() {
        let mut model = ScreenModel::new(24, 10);
        model.process(b"0123456789");
        assert_eq!(model.cursor(), (0, 9));
        model.process(b"x");
        assert_eq!(model.cursor(), (1, 1));
    }

    #[test]
    fn set_cursor_moves_only_the_model() {
        let mut model = ScreenModel::new(24, 80);
        model.set_cursor(10, 3);
        assert_eq!(model.cursor(), (10, 3));
        model.set_cursor(100, 300);
        assert_eq!(model.cursor(), (23, 79));
    }

    #[test]
    fn width_change_sets_dirty() {
        let mut model = ScreenModel::new(24, 80);
        model.resize(30, 80);
        assert!(!model.is_dirty());
        model.resize(30, 100);
        assert!(model.is_dirty());
        model.clear_dirty();
        assert!(!model.is_dirty());
        model.resize(0, 0);
        assert_eq!(model.screen().size().1.get(), 1);
    }

    #[test]
    fn scrolls_count_rows_pushed_off_the_top() {
        let mut model = ScreenModel::new(3, 5);
        model.process(b"a\r\nb\r\nc");
        assert_eq!(model.scrolls(), 0);
        // A line feed on the last row, and a wrap there.
        model.process(b"\r\nd");
        assert_eq!(model.scrolls(), 1);
        model.process(b"\x1b[3;5Hxy");
        assert_eq!(model.scrolls(), 2);
        // Moving and erasing scroll nothing.
        model.process(b"\x1b[H\x1b[2J\x1b[A");
        assert_eq!(model.scrolls(), 2);
    }

    #[test]
    fn alternate_screen() {
        let mut model = ScreenModel::new(24, 80);
        model.process(b"\x1b[?1049h");
        assert!(model.screen().alternate_screen());
        model.process(b"\x1b[?1049l");
        assert!(!model.screen().alternate_screen());
    }
}
