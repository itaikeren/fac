//! Where the popup goes and how big it is: Fig's card (option A).
//!
//! The card has no border. Each list row is one space of padding, the
//! item's icon, a space, its label, a space and its argument hint, then the
//! padding on the right. Below the list rows is the bottom line, which
//! describes the selected item (or the top one). The card's height is
//! decided per word (`compute`'s `decided`), so moving the selection never
//! changes it and never scrolls the screen.

use super::text;
use crate::complete::engine::Kind;
use crate::config::Icons;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub label: String,
    pub description: String,
    /// Char indices into `label`.
    pub highlights: Vec<u32>,
    pub is_dir: bool,
    pub is_history: bool,
    /// What the item is, for its icon.
    pub kind: Kind,
    /// A spec item's args, shown dimmed after its name.
    pub args_hint: String,
    /// The spec's own icon. Used when it is 1-3 plain characters, else the
    /// kind's icon is (`Item::icon`).
    pub icon: Option<String>,
    /// The arg being typed, if any: its name and description, which the
    /// bottom line shows when the item has nothing of its own to say.
    pub arg_name: Option<String>,
    pub arg_description: Option<String>,
}

pub struct PopupView<'a> {
    pub items: &'a [Item],
    pub selected: Option<usize>,
    /// The first visible item.
    pub scroll: usize,
    pub icons: Icons,
    /// The description popout is toggled on.
    pub popout: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rect {
    pub row: u16,
    pub col: u16,
    pub rows: u16,
    pub cols: u16,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Layout {
    /// The card: its list rows and every row the bottom line may take
    /// (`bottom_rows`), drawn or not, so that an erase covers them all.
    pub rect: Rect,
    pub list_rows: u16,
    /// The rows kept for the bottom line: the word's `decided` rows, less
    /// what a short screen took. 0: no bottom line.
    pub bottom_rows: u16,
    /// The bottom line's rows as decided for the word (`D`), before a short
    /// screen cut them. The caller keeps it for the word.
    pub decided: u16,
    /// The bottom line's rows drawn now: 1 while nothing is selected, up to
    /// `bottom_rows` while something is; 0 without a bottom line. The rows
    /// below them, up to `bottom_rows`, are not drawn.
    pub lines: u16,
    /// The icon column's width (0 with icons off); a space follows it.
    pub icon_cols: u16,
    /// Rows the screen must scroll up to make room below the cursor.
    pub scroll_deficit: u16,
    /// The description popout, when it is on and fits beside the card.
    pub popout: Option<Rect>,
}

pub const MAX_LIST_ROWS: u16 = 6;
/// The most rows the bottom line takes (`D` is capped at it).
pub const MAX_BOTTOM_ROWS: u16 = 4;
/// A short screen drops list rows down to this; below it the popup hides.
pub const MIN_LIST_ROWS: u16 = 1;
pub const MIN_WIDTH: u16 = 24;
pub const MAX_WIDTH: u16 = 60;
/// A screen too narrow for `MIN_WIDTH` gets a narrower card, down to this
/// (phase 1's minimum); below it the popup hides.
pub const MIN_SHOWN_WIDTH: u16 = 20;
/// One space of padding on each side of every row.
pub const PAD_COLS: u16 = 2;
pub const MAX_POPOUT_COLS: u16 = 50;
/// The popout opens on a side with at least this many columns (or as many
/// as its text needs, if fewer).
pub const MIN_POPOUT_COLS: u16 = 20;

/// The kind's icon. Nerd Font glyphs are from nerd-fonts' `glyphnames.json`
/// (v3.5.1); the glyph name is in each comment.
pub fn kind_icon(kind: Kind, icons: Icons) -> &'static str {
    match (icons, kind) {
        // nf-cod-terminal
        (Icons::Nerd, Kind::Subcommand) => "\u{ea85}",
        // nf-fa-flag
        (Icons::Nerd, Kind::Option) => "\u{f024}",
        // nf-cod-symbol_variable
        (Icons::Nerd, Kind::Arg) => "\u{ea88}",
        // nf-cod-folder
        (Icons::Nerd, Kind::Folder) => "\u{ea83}",
        // nf-cod-file
        (Icons::Nerd, Kind::File) => "\u{ea7b}",
        // nf-cod-history
        (Icons::Nerd, Kind::History) => "\u{ea82}",
        // nf-cod-star_full
        (Icons::Nerd, Kind::Special | Kind::Shortcut) => "\u{eb59}",
        (_, Kind::Subcommand) => "$",
        (_, Kind::Option) => "-",
        (_, Kind::Arg) => "@",
        (_, Kind::Folder) => "/",
        (_, Kind::File) => ".",
        (_, Kind::History) => "^",
        (_, Kind::Special | Kind::Shortcut) => "*",
    }
}

/// A spec's icon when it can be drawn as is: 1-3 characters, each one
/// column wide and printable, and not a URL (`fig://…`, `http…`).
fn spec_icon(icon: &str) -> Option<&str> {
    if icon.starts_with("fig://") || icon.starts_with("http") {
        return None;
    }
    let count = icon.chars().count();
    let plain = icon
        .chars()
        .all(|ch| !ch.is_control() && !ch.is_whitespace() && text::char_width(ch) == 1);
    ((1..=3).contains(&count) && plain).then_some(icon)
}

impl Item {
    /// The label as drawn: sanitized, with a trailing `/` on directories.
    pub fn display_label(&self) -> Vec<text::Glyph> {
        let mut glyphs = text::sanitize_highlighted(&self.label, &self.highlights);
        if self.is_dir && glyphs.last().is_none_or(|glyph| glyph.ch != '/') {
            glyphs.push(text::Glyph {
                ch: '/',
                highlight: false,
            });
        }
        glyphs
    }

    /// The argument hint as drawn: sanitized, trailing spaces trimmed.
    pub fn display_hint(&self) -> String {
        text::sanitize(&self.args_hint).trim_end().to_owned()
    }

    /// The icon drawn for it: the spec's own when it is usable, else its
    /// kind's. None with icons off.
    pub fn icon(&self, icons: Icons) -> Option<&str> {
        if icons == Icons::None {
            return None;
        }
        Some(
            self.icon
                .as_deref()
                .and_then(spec_icon)
                .unwrap_or_else(|| kind_icon(self.kind, icons)),
        )
    }

    /// What the bottom line says about it, as Fig's `Description.tsx`
    /// does: its own description, else `folder` or `file` for a path; when
    /// that is nothing, the arg being typed: `name: description`, or
    /// whichever of the two it has; else nothing.
    pub fn bottom_text(&self) -> String {
        let clean = |text: Option<&str>| {
            text.map(|text| text::sanitize(text).trim().to_owned())
                .filter(|text| !text.is_empty())
        };
        if let Some(description) = clean(Some(&self.description)) {
            return description;
        }
        if self.is_dir || self.kind == Kind::Folder {
            return "folder".to_owned();
        }
        if self.kind == Kind::File {
            return "file".to_owned();
        }
        match (
            clean(self.arg_name.as_deref()),
            clean(self.arg_description.as_deref()),
        ) {
            (Some(name), Some(description)) => format!("{name}: {description}"),
            (Some(text), None) | (None, Some(text)) => text,
            (None, None) => String::new(),
        }
    }

    /// The columns its row's content takes: the label, and the hint after a
    /// space.
    fn content_width(&self) -> u16 {
        let label = text::glyphs_width(&self.display_label());
        let hint = text::width(&self.display_hint());
        if hint == 0 {
            label
        } else {
            label.saturating_add(1).saturating_add(hint)
        }
    }
}

impl PopupView<'_> {
    /// `scroll`, clamped so the last page is full.
    pub fn first_visible(&self, list_rows: u16) -> usize {
        self.scroll
            .min(self.items.len().saturating_sub(usize::from(list_rows)))
    }

    /// The selected item, if there is one.
    pub fn selected_item(&self) -> Option<usize> {
        self.selected.filter(|&index| index < self.items.len())
    }

    /// The item the bottom line and the popout describe: the selected one,
    /// else the top one shown.
    pub fn described(&self, list_rows: u16) -> Option<&Item> {
        let index = self
            .selected_item()
            .unwrap_or_else(|| self.first_visible(list_rows));
        self.items.get(index)
    }
}

/// The bottom line's text, one entry per row drawn: the described item's
/// text on one line cut with `…` while nothing is selected, or wrapped to
/// at most `layout.bottom_rows` lines (the last cut with `…`) while
/// something is. An item with nothing to say gets one empty line. Empty
/// without a bottom line.
pub fn bottom_lines(view: &PopupView, layout: &Layout) -> Vec<String> {
    bottom_text_lines(
        view,
        layout.list_rows,
        layout.bottom_rows,
        layout.rect.cols.saturating_sub(PAD_COLS),
    )
}

fn bottom_text_lines(
    view: &PopupView,
    list_rows: u16,
    bottom_rows: u16,
    width: u16,
) -> Vec<String> {
    if bottom_rows == 0 {
        return Vec::new();
    }
    let text = view
        .described(list_rows)
        .map(Item::bottom_text)
        .unwrap_or_default();
    let max = if view.selected_item().is_some() {
        bottom_rows
    } else {
        1
    };
    let mut lines = text::wrap(&text, width, usize::from(max));
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// The popout's text, one entry per row (fewer when it is shorter): the
/// described item's text wrapped to the popout, the last line cut with `…`.
pub fn popout_lines(view: &PopupView, layout: &Layout) -> Vec<String> {
    let Some(rect) = layout.popout else {
        return Vec::new();
    };
    let text = view
        .described(layout.list_rows)
        .map(Item::bottom_text)
        .unwrap_or_default();
    text::wrap(
        &text,
        rect.cols.saturating_sub(PAD_COLS),
        usize::from(rect.rows),
    )
}

/// `D` for a word: the most lines any of the visible items' bottom texts
/// wraps to at `width` columns, at most `MAX_BOTTOM_ROWS`.
fn decide_bottom_rows(view: &PopupView, list_rows: u16, width: u16) -> u16 {
    let first = view.first_visible(list_rows);
    let most = view
        .items
        .iter()
        .skip(first)
        .take(usize::from(list_rows))
        .map(|item| text::wrap(&item.bottom_text(), width, usize::from(MAX_BOTTOM_ROWS)).len())
        .max()
        .unwrap_or(0);
    u16::try_from(most)
        .unwrap_or(MAX_BOTTOM_ROWS)
        .min(MAX_BOTTOM_ROWS)
}

/// (list rows, bottom-line rows) on a screen with `available` rows below
/// the cursor. A short screen first drops the bottom line's rows beyond the
/// first, then the bottom line, then list rows down to `MIN_LIST_ROWS`.
fn fit(list_rows: u16, bottom_rows: u16, available: u16) -> Option<(u16, u16)> {
    if list_rows + bottom_rows <= available {
        Some((list_rows, bottom_rows))
    } else if bottom_rows > 0 && list_rows < available {
        Some((list_rows, 1))
    } else if list_rows <= available {
        Some((list_rows, 0))
    } else if available >= MIN_LIST_ROWS {
        Some((available, 0))
    } else {
        None
    }
}

/// The popout beside `card` (the card's drawn rows) on a screen `cols`
/// wide: right of it when that side has room, else left of it, else None.
/// As wide as its text needs, at most `MAX_POPOUT_COLS` and the room.
fn place_popout(text: &str, card: Rect, cols: u16) -> Option<Rect> {
    if text.is_empty() {
        return None;
    }
    let want = text::width(text)
        .saturating_add(PAD_COLS)
        .min(MAX_POPOUT_COLS);
    let need = want.min(MIN_POPOUT_COLS);
    let right = card.col.saturating_add(card.cols);
    let right_room = cols.saturating_sub(right);
    let left_room = card.col;
    let (col, width) = if right_room >= need {
        (right, want.min(right_room))
    } else if left_room >= need {
        let width = want.min(left_room);
        (card.col - width, width)
    } else {
        return None;
    };
    Some(Rect {
        row: card.row,
        col,
        rows: card.rows,
        cols: width,
    })
}

/// `cursor` = the terminal cursor (row, col); `anchor_col` = screen column
/// where the current word starts (the caller computes it); `screen` = (rows,
/// cols). None when the popup cannot be shown (too small, no items).
///
/// `decided` is the bottom line's height for the word (`D`), as an
/// earlier call returned it in `Layout::decided`; `None` decides it now
/// from the visible items. The caller decides once per word, so the card's
/// height depends only on the number of items and never changes when the
/// selection moves.
///
/// The card may take the whole screen except the cursor's row (scrolling
/// to make room); a short screen cuts it as `fit` says.
pub fn compute(
    view: &PopupView,
    cursor: (u16, u16),
    anchor_col: u16,
    screen: (u16, u16),
    decided: Option<u16>,
) -> Option<Layout> {
    let (rows, cols) = screen;
    if view.items.is_empty() {
        return None;
    }
    let max_width = MAX_WIDTH.min(cols.saturating_sub(2));
    if max_width < MIN_SHOWN_WIDTH {
        return None;
    }
    let icon_cols = view
        .items
        .iter()
        .filter_map(|item| item.icon(view.icons))
        .map(text::width)
        .max()
        .unwrap_or(0);
    let lead = if icon_cols > 0 { icon_cols + 1 } else { 0 };
    let content = view
        .items
        .iter()
        .map(Item::content_width)
        .max()
        .unwrap_or(0);
    let width = content
        .saturating_add(lead)
        .saturating_add(PAD_COLS)
        .max(MIN_WIDTH)
        .min(max_width);
    let full_rows = u16::try_from(view.items.len())
        .unwrap_or(u16::MAX)
        .min(MAX_LIST_ROWS);
    let decided = decided
        .unwrap_or_else(|| decide_bottom_rows(view, full_rows, width - PAD_COLS))
        .min(MAX_BOTTOM_ROWS);
    let (list_rows, bottom_rows) = fit(full_rows, decided, rows.saturating_sub(1))?;
    let height = list_rows + bottom_rows;

    // The label starts under the word.
    let mut col = anchor_col.saturating_sub(1 + lead);
    if col.saturating_add(width) > cols {
        col = cols - width;
    }
    let (mut row, mut scroll_deficit) = (cursor.0.saturating_add(1), 0);
    let bottom = u32::from(cursor.0) + 1 + u32::from(height);
    if bottom > u32::from(rows) {
        scroll_deficit = u16::try_from(bottom - u32::from(rows)).unwrap_or(u16::MAX);
        row = rows - height;
    }
    let rect = Rect {
        row,
        col,
        rows: height,
        cols: width,
    };
    let lines =
        u16::try_from(bottom_text_lines(view, list_rows, bottom_rows, width - PAD_COLS).len())
            .unwrap_or(bottom_rows);
    let popout = if view.popout {
        let text = view
            .described(list_rows)
            .map(Item::bottom_text)
            .unwrap_or_default();
        let drawn = Rect {
            rows: list_rows + lines,
            ..rect
        };
        place_popout(&text, drawn, cols)
    } else {
        None
    };
    Some(Layout {
        rect,
        list_rows,
        bottom_rows,
        decided,
        lines,
        icon_cols,
        scroll_deficit,
        popout,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(label: &str, description: &str) -> Item {
        Item {
            label: label.to_string(),
            description: description.to_string(),
            highlights: Vec::new(),
            is_dir: false,
            is_history: false,
            kind: Kind::Arg,
            args_hint: String::new(),
            icon: None,
            arg_name: None,
            arg_description: None,
        }
    }

    fn view(items: &[Item]) -> PopupView<'_> {
        PopupView {
            items,
            selected: None,
            scroll: 0,
            icons: Icons::Plain,
            popout: false,
        }
    }

    /// Decided now, plain icons, nothing selected.
    fn layout(
        items: &[Item],
        cursor: (u16, u16),
        anchor: u16,
        screen: (u16, u16),
    ) -> Option<Layout> {
        compute(&view(items), cursor, anchor, screen, None)
    }

    #[test]
    fn below_the_cursor() {
        let items = [item("checkout", "Switch branches"), item("commit", "")];
        let got = layout(&items, (5, 10), 8, (24, 80)).expect("layout");
        // The minimum width: 1 + `@ ` + 8 + 1 is less. 2 list rows, and the
        // bottom line's 1 row.
        assert_eq!(
            got,
            Layout {
                rect: Rect {
                    row: 6,
                    col: 5,
                    rows: 3,
                    cols: MIN_WIDTH
                },
                list_rows: 2,
                bottom_rows: 1,
                decided: 1,
                lines: 1,
                icon_cols: 1,
                scroll_deficit: 0,
                popout: None,
            }
        );
    }

    #[test]
    fn rows_are_capped_at_6() {
        let items: Vec<Item> = (0..20).map(|i| item(&format!("item{i}"), "")).collect();
        let got = layout(&items, (0, 0), 0, (24, 80)).expect("layout");
        assert_eq!(got.list_rows, 6);
        // Nothing to describe: no bottom line.
        assert_eq!((got.bottom_rows, got.lines, got.rect.rows), (0, 0, 6));
    }

    #[test]
    fn width_fits_the_content() {
        let got = layout(&[item("a", "")], (0, 0), 0, (24, 80)).expect("layout");
        assert_eq!(got.rect.cols, MIN_WIDTH);
        // Padding, icon, space, label, space, hint.
        let mut hinted = item(&"x".repeat(30), "");
        hinted.args_hint = "<file> ".to_owned();
        let got = layout(&[hinted], (0, 0), 0, (24, 80)).expect("layout");
        assert_eq!(got.rect.cols, 1 + 2 + 30 + 1 + 6 + 1);
        let long = "x".repeat(100);
        let got = layout(&[item(&long, "")], (0, 0), 0, (24, 80)).expect("layout");
        assert_eq!(got.rect.cols, MAX_WIDTH);
        let got = layout(&[item(&long, "")], (0, 0), 0, (24, 30)).expect("layout");
        assert_eq!(got.rect.cols, 28);
        // Under the minimum width when the screen is narrow, down to
        // `MIN_SHOWN_WIDTH`.
        let got = layout(&[item("a", "")], (0, 0), 0, (24, 24)).expect("layout");
        assert_eq!(got.rect.cols, 22);
        assert!(layout(&[item("a", "")], (0, 0), 0, (24, 22)).is_some());
        assert!(layout(&[item("a", "")], (0, 0), 0, (24, 21)).is_none());
    }

    #[test]
    fn icons_take_one_column_for_the_whole_card() {
        let mut spec = item("checkout", "");
        spec.icon = Some("git".to_owned());
        let items = [item("a", ""), spec.clone()];
        let got = layout(&items, (0, 0), 10, (24, 80)).expect("layout");
        assert_eq!(got.icon_cols, 3);
        // The label starts under the word: padding, 3 icon columns, space.
        assert_eq!(got.rect.col, 10 - 5);
        let none = PopupView {
            icons: Icons::None,
            ..view(&items)
        };
        let got = compute(&none, (0, 0), 10, (24, 80), None).expect("layout");
        assert_eq!((got.icon_cols, got.rect.col), (0, 9));
        let nerd = PopupView {
            icons: Icons::Nerd,
            ..view(&items[..1])
        };
        let got = compute(&nerd, (0, 0), 10, (24, 80), None).expect("layout");
        assert_eq!(got.icon_cols, 1);
    }

    #[test]
    fn icon_choice() {
        let mut it = item("x", "");
        for (kind, plain) in [
            (Kind::Subcommand, "$"),
            (Kind::Option, "-"),
            (Kind::Arg, "@"),
            (Kind::Folder, "/"),
            (Kind::File, "."),
            (Kind::History, "^"),
            (Kind::Special, "*"),
            (Kind::Shortcut, "*"),
        ] {
            it.kind = kind;
            assert_eq!(it.icon(Icons::Plain), Some(plain));
            let nerd = it.icon(Icons::Nerd).expect("an icon");
            assert_eq!(text::width(nerd), 1, "{kind:?}");
            assert_eq!(it.icon(Icons::None), None);
        }
        it.kind = Kind::Subcommand;
        for (icon, used) in [
            ("🔀", false),
            ("fig://icon?type=git", false),
            ("https://example.com/x.png", false),
            ("http", false),
            ("abcd", false),
            ("", false),
            ("a\x1b", false),
            (" ", false),
            ("é", true),
            ("git", true),
            ("λ", true),
        ] {
            it.icon = Some(icon.to_owned());
            let want = if used { icon } else { "$" };
            assert_eq!(it.icon(Icons::Plain), Some(want), "{icon:?}");
        }
    }

    #[test]
    fn bottom_text() {
        // The item's own description wins over the arg's.
        let mut it = item("x", "  Switch\tbranches\x1b ");
        it.arg_name = Some("branch".to_owned());
        it.arg_description = Some("The branch".to_owned());
        assert_eq!(it.bottom_text(), "Switch branches");
        // Without one, the arg's, whatever the item's kind.
        it.description.clear();
        assert_eq!(it.bottom_text(), "branch: The branch");
        it.kind = Kind::History;
        assert_eq!(it.bottom_text(), "branch: The branch");
        it.arg_description = Some(" \x1b".to_owned());
        assert_eq!(it.bottom_text(), "branch");
        it.arg_name = None;
        it.arg_description = Some("The branch".to_owned());
        assert_eq!(it.bottom_text(), "The branch");
        it.arg_description = None;
        assert_eq!(it.bottom_text(), "");
        // A path says what it is, before the arg (Fig's `itemDescription`).
        let mut path = item("src", "");
        path.arg_name = Some("branch".to_owned());
        path.kind = Kind::Folder;
        assert_eq!(path.bottom_text(), "folder");
        path.kind = Kind::File;
        assert_eq!(path.bottom_text(), "file");
        path.is_dir = true;
        assert_eq!(path.bottom_text(), "folder");
    }

    #[test]
    fn anchored_under_the_word() {
        let items = [item("a", "")];
        let col = |anchor| layout(&items, (0, 0), anchor, (24, 80)).map(|l| l.rect.col);
        assert_eq!(col(0), Some(0));
        assert_eq!(col(2), Some(0));
        assert_eq!(col(30), Some(27));
        // Overflowing the right edge shifts left.
        assert_eq!(col(75), Some(80 - MIN_WIDTH));
    }

    #[test]
    fn scroll_deficit() {
        let items = [item("a", "x"), item("b", "")];
        // Height 3.
        let got = layout(&items, (20, 0), 0, (24, 80)).expect("layout");
        assert_eq!((got.rect.row, got.scroll_deficit), (21, 0));
        let got = layout(&items, (21, 0), 0, (24, 80)).expect("layout");
        assert_eq!((got.rect.row, got.scroll_deficit), (21, 1));
        let got = layout(&items, (23, 0), 0, (24, 80)).expect("layout");
        assert_eq!((got.rect.row, got.scroll_deficit), (21, 3));
    }

    /// Items whose descriptions wrap to 1-6 lines on a 24-column card.
    fn described() -> Vec<Item> {
        (1..=6)
            .map(|lines| {
                item(
                    &format!("item{lines}"),
                    &"twenty-one characters ".repeat(lines),
                )
            })
            .collect()
    }

    #[test]
    fn the_bottom_line_is_as_tall_as_the_longest_visible_description() {
        let items = described();
        // 22 columns of text: each repeat is a line; capped at 4.
        let got = layout(&items, (0, 0), 0, (24, 80)).expect("layout");
        assert_eq!((got.decided, got.bottom_rows), (4, 4));
        assert_eq!(got.rect.rows, 6 + 4);
        // Idle: one line drawn.
        assert_eq!(got.lines, 1);
        // Only the short ones visible.
        let got = layout(&items[..2], (0, 0), 0, (24, 80)).expect("layout");
        assert_eq!((got.decided, got.rect.rows), (2, 4));
        // A decision made earlier wins, whatever the items now.
        let got = compute(&view(&items[..1]), (0, 0), 0, (24, 80), Some(3)).expect("layout");
        assert_eq!((got.decided, got.bottom_rows, got.rect.rows), (3, 3, 4));
    }

    #[test]
    fn a_selection_draws_the_whole_description() {
        let items = described();
        let mut v = view(&items);
        let at = |v: &PopupView| compute(v, (0, 0), 0, (24, 80), None).expect("layout");
        let idle = at(&v);
        assert_eq!(bottom_lines(&v, &idle), ["twenty-one characters"]);
        assert_eq!(idle.lines, 1);
        // A long one on top: one line, cut.
        let reversed: Vec<Item> = items.iter().rev().cloned().collect();
        let top = view(&reversed);
        let lines = bottom_lines(&top, &at(&top));
        assert_eq!(lines, ["twenty-one characters…"]);
        v.selected = Some(2);
        let selected = at(&v);
        // The same card: the height does not move.
        assert_eq!(selected.rect, idle.rect);
        assert_eq!(selected.lines, 3);
        assert_eq!(bottom_lines(&v, &selected).len(), 3);
        // Longer than D: cut at D lines.
        v.selected = Some(5);
        let selected = at(&v);
        let lines = bottom_lines(&v, &selected);
        assert_eq!(lines.len(), 4);
        assert!(lines[3].ends_with('…'), "{lines:?}");
        // An item with nothing to say: one empty line.
        let mut items = described();
        items[0].description.clear();
        let mut v = view(&items);
        v.selected = Some(0);
        let got = at(&v);
        assert_eq!(bottom_lines(&v, &got), vec![String::new()]);
        assert_eq!(got.lines, 1);
    }

    /// 50 described items, the cursor on the last row of a `rows`-row
    /// screen.
    fn short_screen(rows: u16) -> Option<Layout> {
        let items: Vec<Item> = (0..50)
            .map(|i| item(&format!("item{i}"), &"twenty-one characters ".repeat(2)))
            .collect();
        compute(&view(&items), (rows - 1, 0), 0, (rows, 80), None)
    }

    #[test]
    fn short_screens_drop_the_bottom_line_then_list_rows() {
        // 6 list rows and a 2-line bottom line need 9 rows.
        let got = short_screen(9).expect("layout");
        assert_eq!((got.list_rows, got.bottom_rows, got.rect.rows), (6, 2, 8));
        // It scrolls the screen to sit below the cursor.
        assert_eq!((got.rect.row, got.scroll_deficit), (1, 8));
        // The bottom line's second row goes first...
        let got = short_screen(8).expect("layout");
        assert_eq!((got.list_rows, got.bottom_rows, got.decided), (6, 1, 2));
        // ... then the bottom line...
        let got = short_screen(7).expect("layout");
        assert_eq!((got.list_rows, got.bottom_rows, got.lines), (6, 0, 0));
        // ... then list rows, down to one.
        let got = short_screen(5).expect("layout");
        assert_eq!((got.list_rows, got.bottom_rows, got.rect.rows), (4, 0, 4));
        assert_eq!((got.rect.row, got.scroll_deficit), (1, 4));
        let got = short_screen(4).expect("layout");
        assert_eq!((got.list_rows, got.rect.rows), (3, 3));
        let got = short_screen(2).expect("layout");
        assert_eq!(got.list_rows, MIN_LIST_ROWS);
        assert!(short_screen(1).is_none());
    }

    #[test]
    fn first_visible_is_clamped() {
        let items: Vec<Item> = (0..20).map(|i| item(&i.to_string(), "")).collect();
        let v = PopupView {
            scroll: 15,
            ..view(&items)
        };
        assert_eq!(v.first_visible(6), 14);
    }

    #[test]
    fn the_popout_opens_right_then_left() {
        let items = [item("a", &"word ".repeat(20))];
        let at = |anchor: u16, cols: u16| {
            let v = PopupView {
                popout: true,
                ..view(&items)
            };
            compute(&v, (0, 0), anchor, (24, cols), None).expect("layout")
        };
        // Right of the card, as tall as its drawn rows, 50 columns.
        let got = at(2, 80);
        let popout = got.popout.expect("a popout");
        assert_eq!(
            popout,
            Rect {
                row: got.rect.row,
                col: got.rect.col + got.rect.cols,
                rows: got.list_rows + got.lines,
                cols: MAX_POPOUT_COLS,
            }
        );
        // Not enough room on the right: on the left, narrower.
        let got = at(40, 60);
        let popout = got.popout.expect("a popout");
        assert_eq!(popout.col + popout.cols, got.rect.col);
        assert_eq!((popout.col, popout.cols), (0, 36));
        assert_eq!(popout.rows, got.list_rows + got.lines);
        // Room on neither side.
        assert_eq!(at(10, 40).popout, None);
        // Nothing to describe: no popout.
        let plain = [item("a", "")];
        let v = PopupView {
            popout: true,
            ..view(&plain)
        };
        let got = compute(&v, (0, 0), 0, (24, 80), None).expect("layout");
        assert_eq!(got.popout, None);
    }
}
