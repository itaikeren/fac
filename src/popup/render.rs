//! The popup as bytes: one frame per draw, and an erase that repaints what
//! the popup covered from fac's screen model.
//!
//! The erase never uses vt100's `rows_formatted` / `contents_formatted`: they
//! skip default cells and end rows with clear-to-end-of-line, which would
//! wipe cells outside the popup. It writes each of the popup's cells once,
//! from `Screen::cell()`: a cell with a character gets its SGR and the
//! character, a run of empty cells of one style gets that style and one ECH.
//! Nothing is blanked and then repainted, so a terminal without synchronized
//! output that shows half a frame shows old and new cells, never blanks.

use std::collections::HashMap;
use std::io::Write as _;

use super::layout::{self, Item, Layout, PopupView, Rect};
use super::text::{self, Glyph};
use super::theme::{RowStyles, Theme};
use crate::config::Icons;

/// Where a frame drew: the card (every row it may take, drawn or not) and
/// the popout beside it. An erase covers all of it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Area {
    pub card: Rect,
    pub popout: Option<Rect>,
}

impl Area {
    fn rects(&self) -> impl Iterator<Item = Rect> {
        std::iter::once(self.card).chain(self.popout)
    }
}

pub struct Frame {
    pub bytes: Vec<u8>,
    pub area: Area,
    /// The scroll-to-make-room bytes, already included at the start of
    /// `bytes`; the caller must also feed these to the model.
    pub scroll: Vec<u8>,
    /// Every cell the popup shows after this frame, in screen coordinates
    /// from after its scroll: what the next frame can leave alone.
    pub painted: Painted,
}

/// The popup's cells as a frame left them on the screen: each cell's text
/// (empty for the second column of a wide character) and style.
#[derive(Default, Clone, Debug, PartialEq, Eq)]
pub struct Painted {
    cells: HashMap<(u16, u16), PaintedCell>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PaintedCell {
    text: String,
    style: String,
}

impl Painted {
    /// The positions of the cells, in no order.
    pub fn positions(&self) -> impl Iterator<Item = (u16, u16)> + '_ {
        self.cells.keys().copied()
    }
}

pub struct Renderer {
    theme: Theme,
    sync: bool,
    /// Whether each erase and draw is its own 2026 frame.
    wrap: bool,
    /// The shell has a cursor saved with DECSC: fac's own DECSC would
    /// overwrite it, so frames end with a CUP and the shell's SGR instead.
    restore_by_cup: bool,
}

pub const SYNC_BEGIN: &[u8] = b"\x1b[?2026h";
pub const SYNC_END: &[u8] = b"\x1b[?2026l";
const SAVE_CURSOR: &[u8] = b"\x1b7";
const RESTORE_CURSOR: &[u8] = b"\x1b8";
const HIDE_CURSOR: &[u8] = b"\x1b[?25l";
const SHOW_CURSOR: &[u8] = b"\x1b[?25h";
const RESET: &[u8] = b"\x1b[0m";

impl Renderer {
    pub fn new(theme: Theme, sync_output: bool) -> Self {
        Self {
            theme,
            sync: sync_output,
            wrap: sync_output,
            restore_by_cup: false,
        }
    }

    /// Whether the shell has a cursor saved (`ScreenModel::shell_saved_cursor`)
    /// for the next draws and erases.
    pub fn set_restore_by_cup(&mut self, on: bool) {
        self.restore_by_cup = on;
    }

    /// The start of a frame's drawing: DECSC, unless the shell's saved
    /// cursor is in the way.
    fn save_cursor(&self, out: &mut Vec<u8>) {
        if !self.restore_by_cup {
            out.extend_from_slice(SAVE_CURSOR);
        }
    }

    /// Back to the shell's cursor `at` and SGR (the model's current one).
    ///
    /// DECRC keeps a pending wrap in Zed and Ghostty, where a CUP never can;
    /// Terminal.app, iTerm2 and VS Code lose it on DECRC anyway (findings.md,
    /// fact 7). With the shell's own cursor saved, DECSC/DECRC would replace
    /// it, so a CUP and the SGR restore the cursor instead.
    fn restore_cursor(&self, out: &mut Vec<u8>, at: (u16, u16), model: &vt100::Screen) {
        if self.restore_by_cup {
            cup(out, at.0, at.1);
            CellStyle::current(model).write_sgr(out);
        } else {
            out.extend_from_slice(RESTORE_CURSOR);
        }
    }

    /// Erases and draws without their own 2026 pair, for a caller that puts
    /// several of them (and the shell's output between them) in one frame.
    pub fn unwrapped(mut self) -> Self {
        self.wrap = false;
        self
    }

    pub fn sync_output(&self) -> bool {
        self.sync
    }

    /// Draw `view` (erasing `previous` where the new popup does not cover
    /// it), in one synchronized frame.
    ///
    /// `model` is fac's screen model as it is before this frame, and `cursor`
    /// is its cursor. When `layout.scroll_deficit` is above zero the frame
    /// starts by scrolling the screen up that many rows (`Frame::scroll`).
    /// The caller feeds those bytes to the model and from then on treats
    /// `cursor.0 - scroll_deficit` as the cursor row; the frame leaves the
    /// terminal's cursor there. `previous` is an area returned by an earlier
    /// draw, in screen coordinates from before this frame's scroll.
    ///
    /// The card's rows below the bottom line's drawn lines are not painted:
    /// where the previous frame painted them, they are erased.
    pub fn draw(
        &self,
        view: &PopupView,
        layout: &Layout,
        cursor: (u16, u16),
        previous: Option<Area>,
        model: &vt100::Screen,
    ) -> Frame {
        self.draw_over(view, layout, cursor, previous, None, model)
    }

    /// `draw`, leaving alone the cells where `last` (the `painted` of the
    /// frame that drew `previous`, still on the screen exactly as it left
    /// them) already shows what this frame would paint. Each cell is still
    /// written at most once.
    pub fn draw_over(
        &self,
        view: &PopupView,
        layout: &Layout,
        cursor: (u16, u16),
        previous: Option<Area>,
        last: Option<&Painted>,
        model: &vt100::Screen,
    ) -> Frame {
        let (rows, cols) = screen_size(model);
        let deficit = layout.scroll_deficit;
        let restore_at = (cursor.0.saturating_sub(deficit), cursor.1);
        let mut scroll = Vec::new();
        if deficit > 0 {
            // New rows take the current background (BCE): the default one,
            // then the shell's SGR again.
            scroll.extend_from_slice(RESET);
            let _ = write!(scroll, "\x1b[{rows};1H");
            scroll.extend(std::iter::repeat_n(b'\n', usize::from(deficit)));
            let shell = CellStyle::current(model);
            if shell != CellStyle::default() {
                shell.write_sgr(&mut scroll);
            }
            cup(&mut scroll, restore_at.0, restore_at.1);
        }

        let mut out = Vec::new();
        if self.wrap {
            out.extend_from_slice(SYNC_BEGIN);
        }
        // Hidden before the scroll too: Terminal.app leaves a sliver of its
        // unfocused (outline) cursor in the row above when the screen
        // scrolls under the cursor.
        let hide = !self.sync && !model.hide_cursor();
        if hide {
            out.extend_from_slice(HIDE_CURSOR);
        }
        out.extend_from_slice(&scroll);
        self.save_cursor(&mut out);
        let painted = Area {
            card: Rect {
                rows: layout.list_rows + layout.lines,
                ..layout.rect
            },
            popout: layout.popout,
        };
        if let Some(previous) = previous {
            // The scroll moved the old popup and everything under it up.
            let cells = Cells {
                screen: model,
                shift: deficit,
            };
            erase_outside(&mut out, &cells, previous, deficit, painted);
        }
        let last = previous.and(last).map(|last| (last, deficit));
        let painted = self.paint(&mut out, view, layout, last, cols);
        self.restore_cursor(&mut out, restore_at, model);
        if hide {
            out.extend_from_slice(SHOW_CURSOR);
        }
        if self.wrap {
            out.extend_from_slice(SYNC_END);
        }
        Frame {
            bytes: out,
            area: Area {
                card: layout.rect,
                popout: layout.popout,
            },
            scroll,
            painted,
        }
    }

    /// Erase a popup previously drawn at `area`, restoring the model's cells.
    pub fn erase(&self, area: Area, model: &vt100::Screen) -> Vec<u8> {
        let mut out = Vec::new();
        if self.wrap {
            out.extend_from_slice(SYNC_BEGIN);
        }
        self.save_cursor(&mut out);
        let hide = !self.sync && !model.hide_cursor();
        if hide {
            out.extend_from_slice(HIDE_CURSOR);
        }
        let cells = Cells {
            screen: model,
            shift: 0,
        };
        let (rows, cols) = screen_size(model);
        for rect in area.rects() {
            for row in rect.row..rect.row.saturating_add(rect.rows).min(rows) {
                let end = rect.col.saturating_add(rect.cols).min(cols);
                erase_span(&mut out, &cells, row, (rect.col, end), (true, true));
            }
        }
        let (row, col) = model.cursor_position();
        self.restore_cursor(&mut out, (row, col.min(cols.saturating_sub(1))), model);
        if hide {
            out.extend_from_slice(SHOW_CURSOR);
        }
        if self.wrap {
            out.extend_from_slice(SYNC_END);
        }
        out
    }

    /// Paint the popup's rows, skipping the cells `last` shows already
    /// (`last`: what the previous frame painted, and the rows this frame
    /// scrolls first). Returns what the popup shows now.
    fn paint(
        &self,
        out: &mut Vec<u8>,
        view: &PopupView,
        layout: &Layout,
        last: Option<(&Painted, u16)>,
        cols: u16,
    ) -> Painted {
        let rect = layout.rect;
        let theme = &self.theme;
        let first = view.first_visible(layout.list_rows);
        let selected = view.selected_item();
        let visible = view
            .items
            .iter()
            .enumerate()
            .skip(first)
            .take(usize::from(layout.list_rows));
        let mut rows: Vec<(u16, u16, Vec<Span>)> = Vec::new();
        let mut row = rect.row;
        for (index, item) in visible {
            let spans = self.list_row(item, selected == Some(index), view.icons, layout);
            rows.push((row, rect.col, spans));
            row += 1;
        }
        for line in layout::bottom_lines(view, layout) {
            rows.push((row, rect.col, vec![band_row(&theme.band, &line, rect.cols)]));
            row += 1;
        }
        if let Some(popout) = layout.popout {
            let mut lines = layout::popout_lines(view, layout).into_iter();
            for row in popout.row..popout.row + popout.rows {
                let line = lines.next().unwrap_or_default();
                rows.push((
                    row,
                    popout.col,
                    vec![band_row(&theme.band, &line, popout.cols)],
                ));
            }
        }
        let mut painted = Painted::default();
        for (row, col, spans) in rows {
            let cells = cells_of(&spans);
            write_row(out, (row, col), &cells, last, cols);
            for (offset, cell) in cells.into_iter().enumerate() {
                let at = col.saturating_add(u16::try_from(offset).unwrap_or(u16::MAX));
                painted.cells.insert((row, at), cell);
            }
        }
        painted
    }

    /// ` icon label hint ` across the card: the padding, the icon in its
    /// column, the label with its matched letters tinted, then the hint,
    /// dimmed. When the row is too narrow, the hint is cut with `…`, then
    /// dropped, and only then is the label cut.
    fn list_row<'t>(
        &'t self,
        item: &Item,
        selected: bool,
        icons: Icons,
        layout: &Layout,
    ) -> Vec<Span<'t>> {
        let styles: &RowStyles = if selected {
            &self.theme.selected
        } else {
            &self.theme.card
        };
        let mut inner = layout.rect.cols.saturating_sub(layout::PAD_COLS);
        let mut spans = vec![Span::new(&styles.text, " ".to_string())];
        if layout.icon_cols > 0 {
            // Left-aligned in the icon column, then a space.
            let icon = item.icon(icons).unwrap_or_default();
            let pad = layout.icon_cols.saturating_sub(text::width(icon));
            spans.push(Span::new(styles.icon(item.kind), icon.to_owned()));
            spans.push(Span::new(&styles.text, format!("{} ", spaces(pad))));
            inner = inner.saturating_sub(layout.icon_cols + 1);
        }
        let label = item.display_label();
        let label_width = text::glyphs_width(&label);
        let hint = text::plain(&item.display_hint());
        let hint_width = text::glyphs_width(&hint);
        let with_hint = if hint.is_empty() {
            label_width
        } else {
            label_width + 1 + hint_width
        };
        let (label, label_width, hint, hint_width) = if with_hint <= inner {
            (label, label_width, hint, hint_width)
        } else if !hint.is_empty() && label_width + 3 <= inner {
            // A cut hint keeps at least one character before its `…`.
            let (hint, hint_width) = text::truncate(&hint, inner - label_width - 1);
            (label, label_width, hint, hint_width)
        } else {
            let (label, label_width) = text::truncate(&label, inner);
            (label, label_width, Vec::new(), 0)
        };
        for run in label.chunk_by(|a, b| a.highlight == b.highlight) {
            let style = if run[0].highlight {
                &styles.matched
            } else {
                &styles.text
            };
            spans.push(Span::new(style, glyph_text(run)));
        }
        let mut used = label_width;
        if !hint.is_empty() {
            spans.push(Span::new(&styles.text, " ".to_string()));
            spans.push(Span::new(&styles.dim, glyph_text(&hint)));
            used += 1 + hint_width;
        }
        spans.push(Span::new(
            &styles.text,
            format!("{} ", spaces(inner.saturating_sub(used))),
        ));
        spans
    }
}

/// ` text ` padded to `cols` columns, for the bottom line and the popout.
fn band_row<'a>(style: &'a str, line: &str, cols: u16) -> Span<'a> {
    let pad = cols
        .saturating_sub(layout::PAD_COLS)
        .saturating_sub(text::width(line));
    Span::new(style, format!(" {line}{} ", spaces(pad)))
}

struct Span<'a> {
    style: &'a str,
    text: String,
}

impl<'a> Span<'a> {
    fn new(style: &'a str, text: String) -> Self {
        Self { style, text }
    }
}

/// The cells `spans` take, left to right: one per column, a wide
/// character's second column with empty text. A zero-width character joins
/// the cell before it.
fn cells_of(spans: &[Span]) -> Vec<PaintedCell> {
    let mut cells: Vec<PaintedCell> = Vec::new();
    for span in spans {
        for ch in span.text.chars() {
            match text::char_width(ch) {
                0 => {
                    if let Some(cell) = cells.last_mut() {
                        cell.text.push(ch);
                    }
                }
                width => {
                    cells.push(PaintedCell {
                        text: ch.to_string(),
                        style: span.style.to_owned(),
                    });
                    if width == 2 {
                        cells.push(PaintedCell {
                            text: String::new(),
                            style: span.style.to_owned(),
                        });
                    }
                }
            }
        }
    }
    cells
}

/// One row of cells from `at`, written left to right. A character (one cell,
/// or two for a wide one) whose cells `last` already shows the same is left
/// alone; the rest go out with the cursor moved to them. The row's first
/// style goes out whole, after it only what changes (`StyleParts`), which
/// keeps a frame small. No reset at the end: the next row starts with a
/// whole style, and the frame ends by giving the shell its SGR back.
fn write_row(
    out: &mut Vec<u8>,
    (row, col): (u16, u16),
    cells: &[PaintedCell],
    last: Option<(&Painted, u16)>,
    cols: u16,
) {
    let mut current: Option<&str> = None;
    // Where the cursor is, when it is in this row.
    let mut at: Option<u16> = None;
    let mut i = 0;
    while i < cells.len() {
        let width = if cells.get(i + 1).is_some_and(|next| next.text.is_empty()) {
            2
        } else {
            1
        };
        let start = col.saturating_add(u16::try_from(i).unwrap_or(u16::MAX));
        let shown = last.is_some_and(|(last, shift)| {
            (0..width).all(|k| {
                let cell = (row.saturating_add(shift), start.saturating_add(k as u16));
                last.cells.get(&cell) == Some(&cells[i + k])
            })
        });
        if !shown {
            match at {
                Some(at) if at == start => {}
                Some(at) if at < start => {
                    let _ = write!(out, "\x1b[{}C", start - at);
                }
                _ => cup(out, row, start),
            }
            let style = cells[i].style.as_str();
            match current {
                Some(from) if from == style => {}
                Some(from) => match (StyleParts::of(from), StyleParts::of(style)) {
                    (Some(from), Some(to)) => from.write_change(&to, out),
                    _ => {
                        let _ = write!(out, "\x1b[{style}m");
                    }
                },
                None => {
                    let _ = write!(out, "\x1b[{style}m");
                }
            }
            current = Some(style);
            out.extend_from_slice(cells[i].text.as_bytes());
            let end = start.saturating_add(width as u16);
            // At the last column the cursor waits to wrap.
            at = (end < cols).then_some(end);
        }
        i += width;
    }
}

/// A theme style (`0;` then `1;` for bold or `3;` for italic, then
/// `38;...` and `48;...`), taken apart.
#[derive(PartialEq, Eq)]
struct StyleParts<'a> {
    bold: bool,
    italic: bool,
    fg: &'a str,
    bg: &'a str,
}

impl<'a> StyleParts<'a> {
    /// None for a style of another shape (it then goes out whole).
    fn of(style: &'a str) -> Option<Self> {
        let rest = style.strip_prefix("0;")?;
        let (rest, bold) = rest
            .strip_prefix("1;")
            .map_or((rest, false), |rest| (rest, true));
        let (rest, italic) = rest
            .strip_prefix("3;")
            .map_or((rest, false), |rest| (rest, true));
        let at = rest.find(";48;")?;
        let (fg, bg) = (&rest[..at], &rest[at + 1..]);
        fg.starts_with("38;").then_some(Self {
            bold,
            italic,
            fg,
            bg,
        })
    }

    /// The SGR that turns this style into `to`.
    fn write_change(&self, to: &StyleParts, out: &mut Vec<u8>) {
        let mut params: Vec<&str> = Vec::new();
        if self.bold != to.bold {
            params.push(if to.bold { "1" } else { "22" });
        }
        if self.italic != to.italic {
            params.push(if to.italic { "3" } else { "23" });
        }
        if self.fg != to.fg {
            params.push(to.fg);
        }
        if self.bg != to.bg {
            params.push(to.bg);
        }
        if !params.is_empty() {
            let _ = write!(out, "\x1b[{}m", params.join(";"));
        }
    }
}

fn glyph_text(glyphs: &[Glyph]) -> String {
    glyphs.iter().map(|glyph| glyph.ch).collect()
}

fn spaces(count: u16) -> String {
    " ".repeat(usize::from(count))
}

/// CUP to a 0-based (row, col).
fn cup(out: &mut Vec<u8>, row: u16, col: u16) {
    let _ = write!(out, "\x1b[{};{}H", u32::from(row) + 1, u32::from(col) + 1);
}

fn screen_size(screen: &vt100::Screen) -> (u16, u16) {
    let (rows, cols) = screen.size();
    (rows.get(), cols.get())
}

/// The model's cells, seen `shift` rows further down: what the terminal shows
/// after scrolling up `shift` rows that the model has not seen yet. None is a
/// default cell.
struct Cells<'a> {
    screen: &'a vt100::Screen,
    shift: u16,
}

impl Cells<'_> {
    fn cell(&self, row: u16, col: u16) -> Option<&vt100::Cell> {
        let row = row.checked_add(self.shift)?;
        self.screen.cell(row, col)
    }

    fn cols(&self) -> u16 {
        screen_size(self.screen).1
    }

    fn rows(&self) -> u16 {
        screen_size(self.screen).0
    }
}

/// Erase the cells of `previous` (drawn before a scroll of `deficit` rows)
/// that `current` (what this frame paints) does not cover.
fn erase_outside(out: &mut Vec<u8>, cells: &Cells, previous: Area, deficit: u16, current: Area) {
    let (rows, cols) = (cells.rows(), cells.cols());
    for old in previous.rects() {
        let start = old.col.min(cols);
        let end = old.col.saturating_add(old.cols).min(cols);
        for old_row in old.row..old.row.saturating_add(old.rows) {
            // Rows scrolled off the top are gone from the screen.
            let Some(row) = old_row.checked_sub(deficit) else {
                continue;
            };
            if row >= rows {
                continue;
            }
            // The old span less what the new frame paints on this row.
            let mut spans = vec![(start, end)];
            for new in current.rects() {
                if row < new.row || row >= new.row.saturating_add(new.rows) {
                    continue;
                }
                let (cut_start, cut_end) = (new.col, new.col.saturating_add(new.cols));
                spans = spans
                    .into_iter()
                    .flat_map(|(a, b)| [(a, b.min(cut_start)), (a.max(cut_end), b)])
                    .filter(|(a, b)| a < b)
                    .collect();
            }
            // A wide character across a span's edge is repainted whole only
            // where this frame does not paint the cell next to the span.
            let painted = |col: u16| {
                current.rects().any(|new| {
                    (new.row..new.row.saturating_add(new.rows)).contains(&row)
                        && (new.col..new.col.saturating_add(new.cols)).contains(&col)
                })
            };
            for (a, b) in spans {
                let widen = (a == 0 || !painted(a - 1), !painted(b));
                erase_span(out, cells, row, (a, b), widen);
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
struct CellStyle {
    fg: vt100::Color,
    bg: vt100::Color,
    bold: bool,
    dim: bool,
    italic: bool,
    underline: bool,
    inverse: bool,
}

impl CellStyle {
    fn of(cell: &vt100::Cell) -> Self {
        Self {
            fg: cell.fgcolor(),
            bg: cell.bgcolor(),
            bold: cell.bold(),
            dim: cell.dim(),
            italic: cell.italic(),
            underline: cell.underline(),
            inverse: cell.inverse(),
        }
    }

    /// The SGR the model draws with now: the shell's.
    fn current(screen: &vt100::Screen) -> Self {
        Self {
            fg: screen.fgcolor(),
            bg: screen.bgcolor(),
            bold: screen.bold(),
            dim: screen.dim(),
            italic: screen.italic(),
            underline: screen.underline(),
            inverse: screen.inverse(),
        }
    }

    fn write_sgr(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(b"\x1b[0");
        for (on, code) in [
            (self.bold, 1),
            (self.dim, 2),
            (self.italic, 3),
            (self.underline, 4),
            (self.inverse, 7),
        ] {
            if on {
                let _ = write!(out, ";{code}");
            }
        }
        write_color(out, self.fg, 30, 90, 38);
        write_color(out, self.bg, 40, 100, 48);
        out.push(b'm');
    }
}

/// `normal` (30/40) for colours 0–7, `bright` (90/100) for 8–15, `extended`
/// (38/48) with `5;n` or `2;r;g;b` otherwise.
fn write_color(out: &mut Vec<u8>, color: vt100::Color, normal: u8, bright: u8, extended: u8) {
    let _ = match color {
        vt100::Color::Default => Ok(()),
        vt100::Color::Idx(index @ 0..=7) => write!(out, ";{}", normal + index),
        vt100::Color::Idx(index @ 8..=15) => write!(out, ";{}", bright + index - 8),
        vt100::Color::Idx(index) => write!(out, ";{extended};5;{index}"),
        vt100::Color::Rgb(r, g, b) => write!(out, ";{extended};2;{r};{g};{b}"),
    };
}

/// Restore the cells `start..end` of `row` from `cells`, each written once:
/// a cell with a character gets its SGR and the character, a run of empty
/// cells of one style gets that SGR and one ECH (which fills them with its
/// background). Where a wide character straddles an edge of the span, the
/// span grows by a column on that side (`widen`), so the character is
/// repainted whole; where the cell beyond the edge is painted in the same
/// frame instead, the character's half inside the span is blanked with ECH,
/// as the terminal shows it once its other half is painted over.
fn erase_span(out: &mut Vec<u8>, cells: &Cells, row: u16, span: (u16, u16), widen: (bool, bool)) {
    let (start, end) = span;
    if start >= end {
        return;
    }
    let cols = cells.cols();
    let is_wide = |col: u16| cells.cell(row, col).is_some_and(vt100::Cell::is_wide);
    let (left_half, right_half) = (
        start > 0 && is_wide(start - 1),
        end < cols && is_wide(end - 1),
    );
    let blank_first = left_half && !widen.0;
    let blank_last = right_half && !widen.1;
    let start = if left_half && widen.0 {
        start - 1
    } else {
        start
    };
    let end = if right_half && widen.1 { end + 1 } else { end };

    // The SGR is unknown until the first cell sets it.
    let mut current: Option<CellStyle> = None;
    // Where the cursor is, when it is in this row.
    let mut at: Option<u16> = None;
    let mut col = start;
    while col < end {
        let Some(cell) = cells.cell(row, col) else {
            col += 1;
            continue;
        };
        // Half a wide character, its other half painted over.
        let half = if col == start && blank_first {
            cells.cell(row, col - 1)
        } else if col + 1 == end && blank_last {
            Some(cell)
        } else {
            None
        };
        if cell.is_wide_continuation() && half.is_none() {
            // Written with the wide character before it.
            col += 1;
            continue;
        }
        let style = CellStyle::of(half.unwrap_or(cell));
        let has_contents = half.is_none() && cell.has_contents();
        // An empty cell starts a run of empty cells of its style.
        let run_end = if has_contents {
            col
        } else if half.is_some() {
            col + 1
        } else {
            let mut run_end = col + 1;
            while run_end < end
                && cells.cell(row, run_end).is_some_and(|next| {
                    !next.has_contents()
                        && !next.is_wide_continuation()
                        && CellStyle::of(next) == style
                })
            {
                run_end += 1;
            }
            run_end
        };
        match at {
            Some(at) if at == col => {}
            Some(at) if at < col => {
                let _ = write!(out, "\x1b[{}C", col - at);
            }
            _ => cup(out, row, col),
        }
        if current != Some(style) {
            style.write_sgr(out);
            current = Some(style);
        }
        if has_contents {
            let contents = cell.contents();
            out.extend_from_slice(contents.as_bytes());
            let width = if cell.is_wide() { 2 } else { 1 };
            col += width;
            // At the last column the cursor waits to wrap: not a column of
            // this row a CUF can count from. Nor is the column after a cell
            // that isn't one ASCII character: the terminal may give it
            // another width than the model does (a VS16 or ZWJ sequence;
            // findings.md, fact 8), so the next write goes by CUP.
            let ascii = contents.len() == 1 && contents.is_ascii();
            at = (col < cols && ascii).then_some(col);
        } else {
            let _ = write!(out, "\x1b[{}X", run_end - col);
            // ECH does not move the cursor.
            at = Some(col);
            col = run_end;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::complete::engine::Kind;
    use crate::popup::palette;
    use crate::popup::theme::ColorDepth;
    use crate::term::caps::Ansi;
    use std::num::NonZeroU16;

    fn item(label: &str, description: &str) -> Item {
        Item {
            label: label.to_string(),
            description: description.to_string(),
            highlights: Vec::new(),
            is_dir: false,
            is_history: false,
            kind: Kind::Option,
            args_hint: String::new(),
            icon: None,
            arg_name: None,
            arg_description: None,
        }
    }

    fn renderer() -> Renderer {
        let roles = palette::Theme::default().roles(None, &Ansi::default());
        Renderer::new(Theme::new(roles, None, ColorDepth::TrueColor), false)
    }

    /// The card's rows as drawn on a blank screen 24 rows by `cols`, the
    /// cursor at (0, 0): every row of its rect, drawn or not.
    fn drawn(items: &[Item], selected: Option<usize>, cols: u16, icons: Icons) -> Vec<String> {
        let size = |n: u16| NonZeroU16::new(n).unwrap_or(NonZeroU16::MIN);
        let mut parser = vt100::Parser::new(size(24), size(cols), 0);
        let view = PopupView {
            items,
            selected,
            scroll: 0,
            icons,
            popout: false,
        };
        let layout = layout::compute(&view, (0, 0), 0, (24, cols), None).expect("layout");
        let frame = renderer().draw(&view, &layout, (0, 0), None, parser.screen());
        parser.process(&frame.bytes);
        let rect = frame.area.card;
        parser
            .screen()
            .rows(rect.col, rect.cols)
            .skip(usize::from(rect.row))
            .take(usize::from(rect.rows))
            .collect()
    }

    #[test]
    fn a_row_is_padding_icon_label_hint_padding() {
        let mut long = item("--long", "");
        long.args_hint = "<when> ".to_owned();
        let items = [item("-a", "list all"), long];
        let rows = drawn(&items, None, 80, Icons::Plain);
        assert_eq!(rows[0], format!(" - -a{} ", " ".repeat(18)), "{rows:#?}");
        assert_eq!(rows[1], format!(" - --long <when>{} ", " ".repeat(7)));
        // The bottom line: the top item's description.
        assert_eq!(rows[2], format!(" list all{} ", " ".repeat(14)));
        assert!(rows.iter().all(|row| row.chars().count() == 24));
        // Without icons the label follows the padding.
        let rows = drawn(&items, None, 80, Icons::None);
        assert!(rows[1].starts_with(" --long <when> "), "{rows:#?}");
    }

    #[test]
    fn the_hint_is_cut_then_dropped_before_the_label() {
        let mut it = item(&"x".repeat(14), "");
        it.args_hint = "<file>".to_owned();
        // 22 columns: 18 inside the padding and the icon.
        let rows = drawn(std::slice::from_ref(&it), None, 24, Icons::Plain);
        assert_eq!(rows[0], format!(" - {} <f… ", "x".repeat(14)), "{rows:#?}");
        // No room for a cut hint: dropped, the label whole.
        it.label = "x".repeat(16);
        let rows = drawn(std::slice::from_ref(&it), None, 24, Icons::Plain);
        assert_eq!(rows[0], format!(" - {}   ", "x".repeat(16)), "{rows:#?}");
        // Only then is the label cut.
        it.label = "x".repeat(30);
        let rows = drawn(std::slice::from_ref(&it), None, 24, Icons::Plain);
        assert_eq!(rows[0], format!(" - {}… ", "x".repeat(17)), "{rows:#?}");
        // Without a hint, cut the same way: the row never runs past the card.
        it.args_hint.clear();
        let rows = drawn(std::slice::from_ref(&it), None, 24, Icons::Plain);
        assert_eq!(rows[0], format!(" - {}… ", "x".repeat(17)), "{rows:#?}");
    }

    #[test]
    fn the_bottom_line_grows_when_something_is_selected() {
        let long = "one two three four five six seven eight nine ten eleven twelve";
        let items = [item("-a", long), item("-b", "short")];
        let rows = drawn(&items, None, 80, Icons::Plain);
        // 24 columns, 22 of text: 3 lines when selected, so 3 rows kept.
        assert_eq!(rows.len(), 2 + 3, "{rows:#?}");
        assert_eq!(rows[2], " one two three four fi… ");
        assert_eq!(rows[3], "", "not drawn");
        assert_eq!(rows[4], "", "not drawn");
        let rows = drawn(&items, Some(0), 80, Icons::Plain);
        assert_eq!(rows[2], " one two three four     ");
        assert_eq!(rows[3], " five six seven eight   ");
        assert_eq!(rows[4], " nine ten eleven twelve ");
        let rows = drawn(&items, Some(1), 80, Icons::Plain);
        assert_eq!(rows[2], format!(" short{} ", " ".repeat(17)));
        assert_eq!(rows[3], "");
    }

    #[test]
    fn a_row_sends_its_first_style_whole_then_only_changes() {
        let text = "0;38;2;1;2;3;48;2;4;5;6";
        let icon = "0;1;38;5;7;48;2;4;5;6";
        let matched = "0;38;2;1;2;3;48;5;9";
        let band = "0;3;38;5;8;48;5;10";
        let mut out = Vec::new();
        let cells = cells_of(&[
            Span::new(text, " ".into()),
            Span::new(icon, "$".into()),
            Span::new(text, " ".into()),
            Span::new(matched, "ch".into()),
            Span::new(text, "eckout".into()),
            Span::new(band, "x".into()),
            Span::new("1;2", "y".into()),
        ]);
        write_row(&mut out, (0, 0), &cells, None, 20);
        assert_eq!(
            String::from_utf8_lossy(&out),
            "\x1b[1;1H\x1b[0;38;2;1;2;3;48;2;4;5;6m \x1b[1;38;5;7m$\x1b[22;38;2;1;2;3m \
             \x1b[48;5;9mch\x1b[48;2;4;5;6meckout\x1b[3;38;5;8;48;5;10mx\x1b[1;2my"
        );
        // The same cells as with every style whole.
        let size = |n| NonZeroU16::new(n).unwrap_or(NonZeroU16::MIN);
        let mut deltas = vt100::Parser::new(size(1), size(20), 0);
        deltas.process(&out);
        let mut whole = vt100::Parser::new(size(1), size(20), 0);
        whole.process(
            format!(
                "\x1b[{text}m \x1b[{icon}m$\x1b[{text}m \x1b[{matched}mch\x1b[{text}meckout\x1b[{band}mx\x1b[1;2my"
            )
            .as_bytes(),
        );
        for col in 0..20 {
            assert_eq!(
                deltas.screen().cell(0, col),
                whole.screen().cell(0, col),
                "{col}"
            );
        }
    }

    #[test]
    fn a_row_leaves_alone_what_the_last_frame_shows() {
        let text = "0;38;5;1;48;5;2";
        let other = "0;38;5;3;48;5;2";
        let row = |label: &str, style: &'static str| {
            cells_of(&[Span::new(text, " ".into()), Span::new(style, label.into())])
        };
        let mut last = Painted::default();
        for (offset, cell) in row("ab日cd", text).into_iter().enumerate() {
            last.cells.insert((4, 10 + offset as u16), cell);
        }
        // Unchanged: nothing.
        let mut out = Vec::new();
        write_row(
            &mut out,
            (4, 10),
            &row("ab日cd", text),
            Some((&last, 0)),
            80,
        );
        assert!(out.is_empty(), "{:?}", String::from_utf8_lossy(&out));
        // One letter's style and one letter: those two, the cursor moved
        // forward between them.
        let mut out = Vec::new();
        let mut cells = row("ab日cx", text);
        cells[1].style = other.to_owned();
        write_row(&mut out, (4, 10), &cells, Some((&last, 0)), 80);
        assert_eq!(
            String::from_utf8_lossy(&out),
            "\x1b[5;12H\x1b[0;38;5;3;48;5;2ma\x1b[4C\x1b[38;5;1mx"
        );
        // Shifted by a scroll of 2 rows: the last frame's row 6 is row 4.
        let mut shifted = Painted::default();
        for ((row, col), cell) in &last.cells {
            shifted.cells.insert((row + 2, *col), cell.clone());
        }
        let mut out = Vec::new();
        write_row(
            &mut out,
            (4, 10),
            &row("ab日cd", text),
            Some((&shifted, 2)),
            80,
        );
        assert!(out.is_empty());
        // A wide character where `b` and the left half of another were:
        // everything from it on moved, and is written.
        let mut out = Vec::new();
        write_row(
            &mut out,
            (4, 10),
            &row("a日日cd", text),
            Some((&last, 0)),
            80,
        );
        assert_eq!(
            String::from_utf8_lossy(&out),
            "\x1b[5;13H\x1b[0;38;5;1;48;5;2m日日cd"
        );
        // A narrow character on a wide one's left half, the wide one moved
        // a column on: both written, the letter after them left alone.
        let mut out = Vec::new();
        write_row(
            &mut out,
            (4, 10),
            &row("abx日d", text),
            Some((&last, 0)),
            80,
        );
        assert_eq!(
            String::from_utf8_lossy(&out),
            "\x1b[5;14H\x1b[0;38;5;1;48;5;2mx日"
        );
    }

    #[test]
    fn cell_sgr() {
        let mut out = Vec::new();
        let style = CellStyle {
            fg: vt100::Color::Idx(1),
            bg: vt100::Color::Idx(10),
            bold: true,
            underline: true,
            ..CellStyle::default()
        };
        style.write_sgr(&mut out);
        assert_eq!(out, b"\x1b[0;1;4;31;102m");
        out.clear();
        let style = CellStyle {
            fg: vt100::Color::Idx(200),
            bg: vt100::Color::Rgb(1, 2, 3),
            dim: true,
            italic: true,
            inverse: true,
            ..CellStyle::default()
        };
        style.write_sgr(&mut out);
        assert_eq!(out, b"\x1b[0;2;3;7;38;5;200;48;2;1;2;3m");
    }
}
