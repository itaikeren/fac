//! The popup's frames against two vt100 parsers: `term` stands for the real
//! terminal and `model` for fac's screen model. Both get the background and
//! the scroll-to-make-room bytes; only `term` gets the rest of each frame and
//! the erase bytes.

// The paint-once interpreter, on the frames `draw` and `erase` return.
#[path = "harness/frames.rs"]
#[allow(dead_code)]
mod frames;

use std::num::NonZeroU16;

use fac::complete::engine::Kind;
use fac::config::Icons;
use fac::popup::layout::{self, Item, Layout, PopupView, Rect};
use fac::popup::palette::{self, Roles};
use fac::popup::render::{Area, Frame, Renderer};
use fac::popup::theme::{ColorDepth, Rgb, Theme, nearest_256};
use fac::term::caps::Ansi;

const ROWS: u16 = 24;
const COLS: u16 = 80;
/// The terminal background the palette is built on: the mockup's dark one.
const BACKGROUND: Rgb = (0x1e, 0x1f, 0x23);
/// One Dark's colours, as a terminal reports them (OSC 4).
const ANSI: Ansi = Ansi {
    green: Some((0x98, 0xc3, 0x79)),
    yellow: Some((0xe5, 0xc0, 0x7b)),
    blue: Some((0x61, 0xaf, 0xef)),
    magenta: Some((0xc6, 0x78, 0xdd)),
    cyan: Some((0x56, 0xb6, 0xc2)),
};

struct Pair {
    term: vt100::Parser,
    model: vt100::Parser,
    rows: u16,
    cols: u16,
}

impl Pair {
    fn new() -> Self {
        Self::sized(ROWS, COLS)
    }

    fn sized(rows: u16, cols: u16) -> Self {
        let size = |n: u16| NonZeroU16::new(n).unwrap_or(NonZeroU16::MIN);
        Self {
            term: vt100::Parser::new(size(rows), size(cols), 0),
            model: vt100::Parser::new(size(rows), size(cols), 0),
            rows,
            cols,
        }
    }

    fn both(&mut self, bytes: &[u8]) {
        self.term.process(bytes);
        self.model.process(bytes);
    }

    /// Lay out and draw `view` at the model's cursor, the word starting at
    /// `anchor`, the bottom line's rows decided now. None when the popup
    /// does not fit.
    fn draw(
        &mut self,
        renderer: &Renderer,
        view: &PopupView,
        anchor: u16,
        previous: Option<Area>,
    ) -> Option<Frame> {
        self.draw_decided(renderer, view, anchor, previous, None)
            .map(|(frame, _)| frame)
    }

    /// `draw`, with the bottom line's rows as decided for the word (the
    /// controller passes the first draw's on the redraws of a word).
    fn draw_decided(
        &mut self,
        renderer: &Renderer,
        view: &PopupView,
        anchor: u16,
        previous: Option<Area>,
        decided: Option<u16>,
    ) -> Option<(Frame, Layout)> {
        let cursor = self.model.screen().cursor_position();
        let layout = layout::compute(view, cursor, anchor, (self.rows, self.cols), decided)?;
        let frame = renderer.draw(view, &layout, cursor, previous, self.model.screen());
        assert_clean(&frame.bytes);
        let body = frame
            .bytes
            .strip_prefix(b"\x1b[?2026h".as_slice())
            .unwrap_or(&frame.bytes);
        let body = body.strip_prefix(b"\x1b[?25l".as_slice()).unwrap_or(body);
        assert!(body.starts_with(&frame.scroll));
        self.model.process(&frame.scroll);
        self.term.process(&frame.bytes);
        Some((frame, layout))
    }

    fn erase(&mut self, renderer: &Renderer, area: Area) {
        let bytes = renderer.erase(area, self.model.screen());
        assert_clean(&bytes);
        self.term.process(&bytes);
    }

    fn cursors_match(&self, what: &str) {
        assert_eq!(
            self.term.screen().cursor_position(),
            self.model.screen().cursor_position(),
            "cursor after {what}"
        );
    }

    /// `term` equals `model`, cell by cell: contents and every attribute.
    fn screens_match(&self, what: &str) {
        self.rows_match(what, 0..self.rows);
    }

    /// `rows` of `term` equal `model`'s, cell by cell.
    fn rows_match(&self, what: &str, rows: std::ops::Range<u16>) {
        let (term, model) = (self.term.screen(), self.model.screen());
        for row in rows {
            for col in 0..self.cols {
                let (got, want) = (term.cell(row, col), model.cell(row, col));
                assert_eq!(
                    got,
                    want,
                    "{what}: cell ({row}, {col}) differs\nterm:\n{}\nmodel:\n{}",
                    term.contents(),
                    model.contents()
                );
            }
        }
    }

    fn snapshot(&self) -> String {
        let screen = self.term.screen();
        format!(
            "{}\n--- cursor {:?}",
            screen.contents(),
            screen.cursor_position()
        )
    }

    /// The text of `rect`'s rows on `term`.
    fn rows_of(&self, rect: Rect) -> Vec<String> {
        self.term
            .screen()
            .rows(rect.col, rect.cols)
            .skip(usize::from(rect.row))
            .take(usize::from(rect.rows))
            .collect()
    }
}

/// No erase-in-line or erase-in-display, in any form.
fn assert_clean(bytes: &[u8]) {
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == 0x1b && bytes[i + 1] == b'[' {
            let mut j = i + 2;
            while j < bytes.len() && (0x20..=0x3f).contains(&bytes[j]) {
                j += 1;
            }
            let last = bytes.get(j).copied();
            assert!(
                last != Some(b'K') && last != Some(b'J'),
                "erase sequence {:?} in frame",
                String::from_utf8_lossy(&bytes[i..=j.min(bytes.len() - 1)])
            );
            i = j;
        }
        i += 1;
    }
}

fn item(label: &str, description: &str, highlights: &[u32], kind: Kind) -> Item {
    Item {
        label: label.to_string(),
        description: description.to_string(),
        highlights: highlights.to_vec(),
        is_dir: kind == Kind::Folder,
        is_history: kind == Kind::History,
        kind,
        args_hint: String::new(),
        icon: None,
        arg_name: None,
        arg_description: None,
    }
}

/// git's subcommands for `git c`: the widest description wraps to 3 lines
/// on the 24-column card.
fn git_items() -> Vec<Item> {
    let sub = |label: &str, description: &str| item(label, description, &[0], Kind::Subcommand);
    let mut checkout = sub("checkout", "Switch branches or restore working tree files");
    checkout.args_hint = "[branch] ".to_string();
    vec![
        checkout,
        sub(
            "cherry-pick",
            "Apply the changes introduced by some existing commits",
        ),
        sub("clean", "Remove untracked files from the working tree"),
        sub("clone", "Clone a repository into a new directory"),
        sub("commit", "Record changes to the repository"),
        sub("config", "Get and set repository or global options"),
    ]
}

fn wide_items() -> Vec<Item> {
    vec![
        item(
            "日本語.txt",
            "日本語の説明がここにあります",
            &[0, 1],
            Kind::File,
        ),
        item("日本語", "フォルダ", &[0], Kind::Folder),
        item("echo 日本", "", &[5], Kind::History),
        item(
            "abc日本語def日本語ghi日本語jkl日本語mno日本語pq",
            "長い説明",
            &[3],
            Kind::Arg,
        ),
    ]
}

/// Labels wider than any card: one with a hint, one without, one a history
/// line.
fn long_items() -> Vec<Item> {
    let mut hinted = item(&"h".repeat(70), "a hint after it", &[0], Kind::Option);
    hinted.args_hint = "<value>".to_string();
    vec![
        hinted,
        item(&"n".repeat(70), "no hint", &[1], Kind::Subcommand),
        item(
            &format!("git commit -m {}", "w".repeat(60)),
            "",
            &[],
            Kind::History,
        ),
    ]
}

fn many_items() -> Vec<Item> {
    (0..20)
        .map(|i| {
            item(
                &format!("item-{i:02}"),
                &format!("Item number {i}"),
                &[0, 1],
                Kind::Option,
            )
        })
        .collect()
}

/// The built-in `default` theme on `BACKGROUND` with `ANSI`.
fn roles() -> Roles {
    palette::Theme::default().roles(Some(BACKGROUND), &ANSI)
}

fn renderer_with(roles: Roles, depth: ColorDepth, sync: bool) -> Renderer {
    Renderer::new(Theme::new(roles, Some(BACKGROUND), depth), sync)
}

fn dark() -> Renderer {
    renderer_with(roles(), ColorDepth::TrueColor, false)
}

fn view(items: &[Item], selected: Option<usize>, scroll: usize) -> PopupView<'_> {
    PopupView {
        items,
        selected,
        scroll,
        icons: Icons::Plain,
        popout: false,
    }
}

/// Some shell history above a prompt `~/fac $ git c` on `row`, the cursor at
/// its end.
fn prompt_at(pair: &mut Pair, row: u16) {
    for r in 0..row {
        pair.both(format!("\x1b[{};1Hline {r}: some earlier output", r + 1).as_bytes());
    }
    pair.both(format!("\x1b[{};1H~/fac $ git c", row + 1).as_bytes());
}

fn rgb((r, g, b): Rgb) -> vt100::Color {
    vt100::Color::Rgb(r, g, b)
}

/// One character per cell of the card and the popout (true colour, the
/// default roles): `b` the bottom line or popout, `m` a match tint, `i` an
/// icon (bold), `d` a hint (dim colour), `S` the selected row, `c` the
/// card, `.` anything else (not drawn).
fn attr_map(pair: &Pair, area: Area) -> String {
    let roles = roles();
    let screen = pair.term.screen();
    let mut out = String::new();
    for rect in std::iter::once(area.card).chain(area.popout) {
        for row in rect.row..rect.row + rect.rows {
            for col in rect.col..rect.col + rect.cols {
                let ch = screen.cell(row, col).map_or('?', |cell| {
                    let (bg, fg) = (cell.bgcolor(), cell.fgcolor());
                    if bg == rgb(roles.band) {
                        'b'
                    } else if bg == rgb(roles.matched) || bg == rgb(roles.selected_matched) {
                        'm'
                    } else if cell.bold() {
                        'i'
                    } else if fg == rgb(roles.dim) || fg == rgb(roles.selected_dim) {
                        'd'
                    } else if bg == rgb(roles.selected) {
                        'S'
                    } else if bg == rgb(roles.card) {
                        'c'
                    } else {
                        '.'
                    }
                });
                out.push(ch);
            }
            out.push('\n');
        }
        out.push_str("--\n");
    }
    out
}

/// The popup drawn at a prompt on `row`, with the cell map.
fn draw_snapshot(items: &[Item], selected: Option<usize>, scroll: usize, row: u16) -> String {
    let mut pair = Pair::new();
    prompt_at(&mut pair, row);
    let renderer = dark();
    let frame = pair.draw(&renderer, &view(items, selected, scroll), 12, None);
    let frame = frame.expect("popup shown");
    pair.cursors_match("draw");
    format!("{}\n{}", pair.snapshot(), attr_map(&pair, frame.area))
}

#[test]
fn middle() {
    insta::assert_snapshot!(draw_snapshot(&git_items(), None, 0, 10));
}

#[test]
fn top() {
    insta::assert_snapshot!(draw_snapshot(&git_items(), None, 0, 0));
}

/// No border: every drawn row starts and ends with a space of padding in
/// the card's (or the selected row's, or the bottom line's) colour.
#[test]
fn no_border_and_one_space_of_padding() {
    let mut pair = Pair::new();
    prompt_at(&mut pair, 4);
    let renderer = dark();
    let frame = pair
        .draw(&renderer, &view(&git_items(), Some(1), 0), 12, None)
        .expect("popup shown");
    let rect = frame.area.card;
    let roles = roles();
    let screen = pair.term.screen();
    let backgrounds = [rgb(roles.card), rgb(roles.selected), rgb(roles.band)];
    // 6 list rows and the selected item's 3 lines: every row is drawn.
    assert_eq!(rect.rows, 9);
    for row in rect.row..rect.row + rect.rows {
        for col in [rect.col, rect.col + rect.cols - 1] {
            let cell = screen.cell(row, col).expect("cell");
            assert_eq!(cell.contents(), " ", "({row}, {col})");
            assert!(backgrounds.contains(&cell.bgcolor()), "({row}, {col})");
        }
    }
    let text = pair.rows_of(rect).join("\n");
    for border in ['╭', '╮', '╰', '╯', '│', '─', '├', '┤', '┃'] {
        assert!(!text.contains(border), "{text}");
    }
}

#[test]
fn bottom_scrolls_to_make_room() {
    let mut pair = Pair::new();
    prompt_at(&mut pair, ROWS - 1);
    let renderer = dark();
    let frame = pair.draw(&renderer, &view(&git_items(), None, 0), 12, None);
    let frame = frame.expect("popup shown");
    // 6 list rows and the 3 the bottom line may take, below row 23: 9 rows
    // of scroll, with the default SGR (the new rows take its background).
    assert_eq!(
        frame.scroll,
        b"\x1b[0m\x1b[24;1H\n\n\n\n\n\n\n\n\n\x1b[15;14H"
    );
    assert_eq!(frame.area.card.row, 15);
    assert_eq!(frame.area.card.rows, 9);
    pair.cursors_match("draw");
    assert_eq!(pair.model.screen().cursor_position(), (14, 13));
    insta::assert_snapshot!(pair.snapshot());
    pair.erase(&renderer, frame.area);
    pair.screens_match("erase after scroll");
}

#[test]
fn right_edge_overflow() {
    let mut pair = Pair::new();
    pair.both(b"\x1b[6;1H~/fac $ some long command line that reaches far to the right c");
    let renderer = dark();
    let frame = pair.draw(&renderer, &view(&git_items(), None, 0), 74, None);
    let frame = frame.expect("popup shown");
    assert_eq!(frame.area.card.col + frame.area.card.cols, COLS);
    pair.cursors_match("draw");
    insta::assert_snapshot!(pair.snapshot());
}

#[test]
fn wide_characters() {
    let mut pair = Pair::new();
    prompt_at(&mut pair, 3);
    let renderer = dark();
    let frame = pair.draw(&renderer, &view(&wide_items(), Some(0), 0), 12, None);
    let frame = frame.expect("popup shown");
    pair.cursors_match("draw");
    insta::assert_snapshot!(format!(
        "{}\n{}",
        pair.snapshot(),
        attr_map(&pair, frame.area)
    ));
}

/// A light terminal background: the palette is built on it, and every
/// colour drawn is one of its roles.
#[test]
fn light_palette() {
    let background = (0xfb, 0xfb, 0xf9);
    let roles = palette::Theme::default().roles(Some(background), &ANSI);
    assert!(!roles.dark);
    let mut pair = Pair::new();
    prompt_at(&mut pair, 2);
    let renderer = Renderer::new(
        Theme::new(roles, Some(background), ColorDepth::TrueColor),
        false,
    );
    let frame = pair.draw(&renderer, &view(&git_items(), Some(1), 0), 12, None);
    let rect = frame.expect("popup shown").area.card;
    let screen = pair.term.screen();
    let cells = [
        ("left padding", rect.row, rect.col, roles.text, roles.card),
        (
            "icon",
            rect.row,
            rect.col + 1,
            roles.icons.subcommand,
            roles.card,
        ),
        ("matched", rect.row, rect.col + 3, roles.text, roles.matched),
        ("label", rect.row, rect.col + 4, roles.text, roles.card),
        ("hint", rect.row, rect.col + 12, roles.dim, roles.card),
        (
            "selected icon",
            rect.row + 1,
            rect.col + 1,
            roles.icons.subcommand,
            roles.selected,
        ),
        (
            "selected matched",
            rect.row + 1,
            rect.col + 3,
            roles.selected_text,
            roles.selected_matched,
        ),
        (
            "selected label",
            rect.row + 1,
            rect.col + 4,
            roles.selected_text,
            roles.selected,
        ),
        (
            "bottom line",
            rect.row + 6,
            rect.col + 1,
            roles.band_text,
            roles.band,
        ),
    ];
    let mut out = String::new();
    for (name, row, col, fg, bg) in cells {
        let cell = screen.cell(row, col).expect("cell");
        assert_eq!(
            (cell.fgcolor(), cell.bgcolor()),
            (rgb(fg), rgb(bg)),
            "{name}"
        );
        out.push_str(&format!(
            "{name}: {:?} fg={:?} bg={:?} bold={} italic={}\n",
            cell.contents(),
            cell.fgcolor(),
            cell.bgcolor(),
            cell.bold(),
            cell.italic()
        ));
    }
    insta::assert_snapshot!(out);
}

/// In 256 colours the card is `card_index_256` (kept off the background's
/// index) and every other role its nearest index.
#[test]
fn ansi_256_uses_card_index_256() {
    let roles = roles();
    // A terminal background that rounds to the card's own index: the card
    // steps away from it.
    let background = roles.card;
    let card = palette::card_index_256(&roles, Some(background));
    assert_ne!(card, nearest_256(roles.card));
    let mut pair = Pair::new();
    prompt_at(&mut pair, 2);
    let renderer = Renderer::new(
        Theme::new(roles, Some(background), ColorDepth::Ansi256),
        false,
    );
    let rect = pair
        .draw(&renderer, &view(&git_items(), Some(1), 0), 12, None)
        .expect("popup shown")
        .area
        .card;
    let screen = pair.term.screen();
    let cell = |row: u16, col: u16| screen.cell(row, col).expect("cell");
    let index = |rgb: Rgb| vt100::Color::Idx(nearest_256(rgb));
    assert_eq!(cell(rect.row, rect.col).bgcolor(), vt100::Color::Idx(card));
    assert_eq!(cell(rect.row, rect.col + 4).fgcolor(), index(roles.text));
    assert_eq!(cell(rect.row, rect.col + 3).bgcolor(), index(roles.matched));
    assert_eq!(
        cell(rect.row + 1, rect.col).bgcolor(),
        index(roles.selected)
    );
    assert_eq!(
        cell(rect.row + 6, rect.col).bgcolor(),
        vt100::Color::Idx(palette::band_index_256(&roles, Some(background)))
    );
    assert_ne!(
        cell(rect.row + 6, rect.col).bgcolor(),
        vt100::Color::Idx(card)
    );
}

#[test]
fn nothing_selected() {
    let items = git_items();
    let mut pair = Pair::new();
    prompt_at(&mut pair, 4);
    let area = pair
        .draw(&dark(), &view(&items, None, 0), 12, None)
        .expect("popup shown")
        .area;
    insta::assert_snapshot!(format!("{}\n{}", pair.snapshot(), attr_map(&pair, area)));
}

#[test]
fn second_item_selected() {
    let items = git_items();
    let mut pair = Pair::new();
    prompt_at(&mut pair, 4);
    let area = pair
        .draw(&dark(), &view(&items, Some(1), 0), 12, None)
        .expect("popup shown")
        .area;
    insta::assert_snapshot!(format!("{}\n{}", pair.snapshot(), attr_map(&pair, area)));
}

#[test]
fn long_list_at_the_top() {
    insta::assert_snapshot!(draw_snapshot(&many_items(), Some(0), 0, 2));
}

#[test]
fn long_list_at_the_bottom() {
    insta::assert_snapshot!(draw_snapshot(&many_items(), Some(19), 14, 2));
}

#[test]
fn long_description_in_the_bottom_line() {
    let items = vec![
        item(
            "rebase",
            "Reapply commits on top of another base tip. If <branch> is specified, git rebase \
             will perform an automatic git switch <branch> before doing anything else.",
            &[0],
            Kind::Subcommand,
        ),
        item(
            "reset",
            "Reset current HEAD to the specified state",
            &[0],
            Kind::Subcommand,
        ),
    ];
    // Idle: one line, cut. Selected: 4 lines, the last cut.
    insta::assert_snapshot!(format!(
        "{}\n{}",
        draw_snapshot(&items, None, 0, 2),
        draw_snapshot(&items, Some(0), 0, 2)
    ));
}

#[test]
fn hostile_text_is_sanitized() {
    let mut evil = item(
        "evil\x1b[2J\x07\u{9b}31m\tname",
        "desc\x1b]0;title\x07\r\nmore",
        &[0],
        Kind::Arg,
    );
    evil.args_hint = "<a\x1b[31mb>".to_string();
    evil.icon = Some("\x1b".to_string());
    let items = vec![evil];
    let mut pair = Pair::new();
    prompt_at(&mut pair, 2);
    let renderer = dark();
    let frame = pair
        .draw(&renderer, &view(&items, None, 0), 12, None)
        .expect("popup shown");
    let rows = pair.rows_of(frame.area.card);
    assert!(rows[0].contains("@ evil[2J31m name <a[31mb>"), "{rows:?}");
    assert!(rows[1].contains("desc]0;titlemore"), "{rows:?}");
    pair.erase(&renderer, frame.area);
    pair.screens_match("erase");
}

/// The match tint and the selected row's colours are the palette's roles.
#[test]
fn colours_come_from_the_roles() {
    let items = git_items();
    let mut pair = Pair::new();
    prompt_at(&mut pair, 2);
    let rect = pair
        .draw(&dark(), &view(&items, Some(1), 0), 12, None)
        .expect("popup shown")
        .area
        .card;
    let roles = roles();
    let screen = pair.term.screen();
    let cell = |row: u16, col: u16| screen.cell(row, col).expect("cell");
    let (first, last) = (rect.col, rect.col + rect.cols - 1);

    // The selected row (the second list row): every cell, padding included,
    // on `selected`; the label in `selected_text`; its matched letter on
    // `selected_matched`.
    let selected = rect.row + 1;
    for col in first..=last {
        let bg = cell(selected, col).bgcolor();
        let want = if col == first + 3 {
            roles.selected_matched
        } else {
            roles.selected
        };
        assert_eq!(bg, rgb(want), "col {col}");
    }
    assert_eq!(
        cell(selected, first + 4).fgcolor(),
        rgb(roles.selected_text)
    );
    assert_eq!(
        cell(selected, first + 3).fgcolor(),
        rgb(roles.selected_text)
    );
    // The icon keeps its colour on the selected row.
    assert_eq!(
        cell(selected, first + 1).fgcolor(),
        rgb(roles.icons.subcommand)
    );
    // Other rows: on the card, the matched `c` on `matched` in `text`.
    for row in (rect.row..rect.row + 6).filter(|&row| row != selected) {
        assert_eq!(cell(row, first).bgcolor(), rgb(roles.card), "row {row}");
        assert_eq!(cell(row, last).bgcolor(), rgb(roles.card), "row {row}");
        assert_eq!(cell(row, first + 3).contents(), "c", "row {row}");
        assert_eq!(
            cell(row, first + 3).bgcolor(),
            rgb(roles.matched),
            "row {row}"
        );
        assert_eq!(cell(row, first + 3).fgcolor(), rgb(roles.text), "row {row}");
        assert_eq!(cell(row, first + 4).bgcolor(), rgb(roles.card), "row {row}");
        assert!(cell(row, first + 1).bold(), "row {row}: the icon is bold");
    }
    // The hint after `checkout`, in `dim`.
    assert_eq!(cell(rect.row, first + 12).contents(), "[");
    assert_eq!(cell(rect.row, first + 12).fgcolor(), rgb(roles.dim));
    // The bottom line: italic `band_text` on `band`.
    let bottom = cell(rect.row + 6, first + 1);
    assert_eq!(
        (bottom.fgcolor(), bottom.bgcolor(), bottom.italic()),
        (rgb(roles.band_text), rgb(roles.band), true)
    );
}

/// Plain, Nerd Font and no icons; a spec's own icon; a URL icon ignored.
#[test]
fn icons() {
    let mut items = vec![
        item("checkout", "", &[], Kind::Subcommand),
        item("--force", "", &[], Kind::Option),
        item("main", "", &[], Kind::Arg),
        item("src", "", &[], Kind::Folder),
        item("README.md", "", &[], Kind::File),
        item("git commit", "", &[], Kind::History),
        item("HEAD", "", &[], Kind::Special),
    ];
    let roles = roles();
    let colors = [
        roles.icons.subcommand,
        roles.icons.option,
        roles.icons.arg,
        roles.icons.folder,
        roles.icons.file,
        roles.icons.history,
        roles.icons.special,
    ];
    // Each kind's list row, its icon column's text and colour; the card
    // shows 6 rows, so the 7th kind is drawn on its own.
    let drawn = |items: &[Item], icons: Icons| {
        let mut rows = Vec::new();
        let mut fgs = Vec::new();
        for group in [&items[..6], &items[6..]] {
            let mut pair = Pair::new();
            prompt_at(&mut pair, 0);
            let v = PopupView {
                icons,
                ..view(group, None, 0)
            };
            let layout = layout::compute(&v, (0, 13), 12, (ROWS, COLS), None).expect("layout");
            let rect = pair.draw(&dark(), &v, 12, None).expect("popup").area.card;
            for i in 0..layout.list_rows {
                let cell = pair.term.screen().cell(rect.row + i, rect.col + 1);
                fgs.push(cell.map_or(vt100::Color::Default, |cell| cell.fgcolor()));
            }
            rows.extend(
                pair.rows_of(rect)
                    .into_iter()
                    .take(usize::from(layout.list_rows)),
            );
        }
        (rows, fgs)
    };
    let (rows, fgs) = drawn(&items, Icons::Plain);
    let firsts: Vec<String> = rows.iter().map(|row| row[..4].to_string()).collect();
    assert_eq!(
        firsts,
        [" $ c", " - -", " @ m", " / s", " . R", " ^ g", " * H"]
    );
    assert_eq!(fgs, colors.map(rgb));
    let (rows, fgs) = drawn(&items, Icons::Nerd);
    let nerd: Vec<char> = rows
        .iter()
        .map(|row| row.chars().nth(1).unwrap_or(' '))
        .collect();
    assert_eq!(
        nerd,
        [
            '\u{ea85}', '\u{f024}', '\u{ea88}', '\u{ea83}', '\u{ea7b}', '\u{ea82}', '\u{eb59}'
        ]
    );
    assert_eq!(fgs, colors.map(rgb));
    let (rows, _) = drawn(&items, Icons::None);
    assert!(rows[0].starts_with(" checkout "), "{rows:?}");
    // A spec's icon of up to 3 plain characters: the column is as wide as
    // the widest icon, for every row. A URL icon is not drawn.
    items[0].icon = Some("git".to_string());
    items[1].icon = Some("fig://icon?type=flag".to_string());
    items[2].icon = Some("🔀".to_string());
    let (rows, fgs) = drawn(&items, Icons::Plain);
    assert!(rows[0].starts_with(" git checkout "), "{rows:?}");
    assert!(rows[1].starts_with(" -   --force "), "{rows:?}");
    assert!(rows[2].starts_with(" @   main "), "{rows:?}");
    // A spec's icon takes its kind's colour.
    assert_eq!(fgs[0], rgb(roles.icons.subcommand));
}

/// The bottom line keeps its rows (`D`) for the word: one line drawn while
/// nothing is selected, up to `D` once an item is, and back to one after a
/// typed character clears the selection. The rows not drawn show what is
/// under the popup.
#[test]
fn the_bottom_line_grows_on_selection_and_shrinks_on_typing() {
    let items = git_items();
    let mut pair = Pair::new();
    busy_background(&mut pair);
    pair.both(b"\x1b[5;14H");
    let renderer = dark();
    let (idle, layout) = pair
        .draw_decided(&renderer, &view(&items, None, 0), 13, None, None)
        .expect("popup shown");
    let d = layout.decided;
    assert_eq!((d, layout.bottom_rows, layout.lines), (3, 3, 1));
    let card = idle.area.card;
    assert_eq!(card.rows, 6 + 3);
    // The two rows not drawn are the screen's own.
    pair.rows_match("idle", card.row + 7..card.row + 9);

    let (selected, layout) = pair
        .draw_decided(
            &renderer,
            &view(&items, Some(0), 0),
            13,
            Some(idle.area),
            Some(d),
        )
        .expect("popup shown");
    assert_eq!(selected.area, idle.area);
    assert_eq!(layout.lines, 3);
    let rows = pair.rows_of(card);
    assert_eq!(rows[6].trim(), "Switch branches or");
    assert_eq!(rows[7].trim(), "restore working tree");
    assert_eq!(rows[8].trim(), "files");

    // A typed character: a new list (a shorter one) with nothing selected,
    // the same word, so the same `D`.
    let fewer = &items[..2];
    let (typed, layout) = pair
        .draw_decided(
            &renderer,
            &view(fewer, None, 0),
            13,
            Some(selected.area),
            Some(d),
        )
        .expect("popup shown");
    assert_eq!((layout.bottom_rows, layout.lines), (3, 1));
    assert_eq!(typed.area.card.rows, 2 + 3);
    // Everything the bigger card drew below the new one's line is erased.
    pair.rows_match(
        "after typing",
        typed.area.card.row + 3..card.row + card.rows,
    );
    pair.erase(&renderer, typed.area);
    pair.screens_match("erase");
}

/// At the bottom of the screen the popup scrolls once, when it opens. Down,
/// Down and Up redraw it in place: the cursor's row and every row above the
/// popup are unchanged, and no frame scrolls.
#[test]
fn an_arrow_key_never_scrolls_the_screen() {
    let items = git_items();
    let mut pair = Pair::new();
    prompt_at(&mut pair, ROWS - 1);
    let renderer = dark();
    let (first, layout) = pair
        .draw_decided(&renderer, &view(&items, None, 0), 12, None, None)
        .expect("popup shown");
    assert!(!first.scroll.is_empty(), "opening scrolls");
    let card = first.area.card;
    let above = |pair: &Pair| -> Vec<String> {
        pair.term
            .screen()
            .rows(0, COLS)
            .take(usize::from(card.row))
            .collect()
    };
    let before = (above(&pair), pair.term.screen().cursor_position());
    let mut previous = first.area;
    for selected in [Some(0), Some(1), Some(0)] {
        let (frame, _) = pair
            .draw_decided(
                &renderer,
                &view(&items, selected, 0),
                12,
                Some(previous),
                Some(layout.decided),
            )
            .expect("popup shown");
        assert!(frame.scroll.is_empty(), "{selected:?} scrolled");
        assert_eq!(frame.area, first.area, "{selected:?}");
        assert_eq!(
            (above(&pair), pair.term.screen().cursor_position()),
            before,
            "{selected:?}"
        );
        pair.cursors_match("redraw");
        previous = frame.area;
    }
    pair.erase(&renderer, previous);
    pair.screens_match("erase");
}

fn popout_view(items: &[Item], selected: Option<usize>) -> PopupView<'_> {
    PopupView {
        popout: true,
        ..view(items, selected, 0)
    }
}

/// The popout: right of the card, as tall as its drawn rows, up to 50
/// columns, the full description wrapped; it follows the selection and is
/// erased with the card.
#[test]
fn the_popout_opens_right_and_follows_the_selection() {
    let items = git_items();
    let mut pair = Pair::new();
    busy_background(&mut pair);
    pair.both(b"\x1b[3;4H");
    let renderer = dark();
    let frame = pair
        .draw(&renderer, &popout_view(&items, None), 3, None)
        .expect("popup shown");
    let card = frame.area.card;
    let popout = frame.area.popout.expect("a popout");
    // Card: 6 list rows and 1 bottom line drawn.
    assert_eq!(
        popout,
        Rect {
            row: card.row,
            col: card.col + card.cols,
            rows: 7,
            cols: 47,
        }
    );
    let rows = pair.rows_of(popout);
    assert_eq!(
        rows[0].trim(),
        "Switch branches or restore working tree files"
    );
    assert!(
        rows[1..].iter().all(|row| row.trim().is_empty()),
        "{rows:?}"
    );
    insta::assert_snapshot!(format!(
        "{}\n{}",
        pair.snapshot(),
        attr_map(&pair, frame.area)
    ));
    // The selection moves: so does the popout's text, and its size.
    let second = pair
        .draw(
            &renderer,
            &popout_view(&items, Some(1)),
            3,
            Some(frame.area),
        )
        .expect("popup shown");
    let moved = second.area.popout.expect("a popout");
    assert_eq!((moved.rows, moved.cols), (9, 50));
    let rows = pair.rows_of(moved);
    assert_eq!(
        rows[0].trim(),
        "Apply the changes introduced by some existing"
    );
    assert_eq!(rows[1].trim(), "commits");
    // Toggled off: the popout's cells are the screen's again.
    let third = pair
        .draw(&renderer, &view(&items, Some(1), 0), 3, Some(second.area))
        .expect("popup shown");
    assert_eq!(third.area.popout, None);
    pair.rows_match("popout closed", 0..card.row);
    let (term, model) = (pair.term.screen(), pair.model.screen());
    for row in moved.row..moved.row + moved.rows {
        for col in moved.col..moved.col + moved.cols {
            assert_eq!(term.cell(row, col), model.cell(row, col), "({row}, {col})");
        }
    }
    pair.erase(&renderer, third.area);
    pair.screens_match("erase");
}

/// No room on the right: the popout opens left of the card. No room on
/// either side: none. Either way the erase restores every cell.
#[test]
fn the_popout_opens_left_or_not_at_all() {
    let items = git_items();
    for (cols, anchor, side) in [(80u16, 70u16, Some("left")), (40, 20, None)] {
        let mut pair = Pair::sized(ROWS, cols);
        busy_background(&mut pair);
        pair.both(format!("\x1b[3;{}H", anchor + 1).as_bytes());
        let renderer = dark();
        let frame = pair
            .draw(&renderer, &popout_view(&items, Some(0)), anchor, None)
            .expect("popup shown");
        let card = frame.area.card;
        match side {
            Some(_) => {
                let popout = frame.area.popout.expect("a popout");
                assert_eq!(popout.col + popout.cols, card.col, "{cols}");
                assert!(popout.cols >= layout::MIN_POPOUT_COLS);
                assert_eq!(popout.rows, 6 + 3);
            }
            None => assert_eq!(frame.area.popout, None, "{cols}"),
        }
        pair.erase(&renderer, frame.area);
        pair.screens_match(&format!("{cols} columns"));
    }
}

#[test]
fn sync_frames() {
    let items = git_items();
    let mut pair = Pair::new();
    prompt_at(&mut pair, 5);
    let renderer = renderer_with(roles(), ColorDepth::TrueColor, true);
    let frame = pair
        .draw(&renderer, &view(&items, None, 0), 12, None)
        .expect("popup shown");
    assert!(frame.bytes.starts_with(b"\x1b[?2026h"));
    assert!(frame.bytes.ends_with(b"\x1b[?2026l"));
    assert!(!contains(&frame.bytes, b"\x1b[?25l"));
    let erase = renderer.erase(frame.area, pair.model.screen());
    assert!(erase.starts_with(b"\x1b[?2026h\x1b7"));
    assert!(erase.ends_with(b"\x1b8\x1b[?2026l"));

    // Without synchronized output the cursor is hidden while drawing.
    let v = view(&items, None, 0);
    let layout = layout::compute(&v, (5, 13), 12, (ROWS, COLS), None).expect("layout");
    let frame = dark().draw(&v, &layout, (5, 13), None, pair.model.screen());
    assert!(frame.bytes.starts_with(b"\x1b[?25l\x1b7"));
    assert!(frame.bytes.ends_with(b"\x1b8\x1b[?25h"));
    // A cursor the shell hid stays hidden.
    pair.both(b"\x1b[?25l");
    let frame = dark().draw(&v, &layout, (5, 13), None, pair.model.screen());
    assert!(!contains(&frame.bytes, b"\x1b[?25"));
}

/// Without synchronized output, a frame that scrolls to make room hides
/// the cursor before the scroll: Terminal.app leaves a sliver of its
/// unfocused (outline) cursor in the row above when the screen scrolls
/// under a cursor it shows (step 9's F4).
#[test]
fn the_cursor_is_hidden_across_the_scroll() {
    let mut pair = Pair::new();
    prompt_at(&mut pair, ROWS - 1);
    let frame = pair
        .draw(&dark(), &view(&git_items(), None, 0), 12, None)
        .expect("popup shown");
    assert!(!frame.scroll.is_empty(), "the frame scrolls");
    let hide = find(&frame.bytes, b"\x1b[?25l").expect("the cursor is hidden");
    let lf = find(&frame.bytes, b"\n").expect("the scroll's LFs");
    assert!(hide < lf, "{:?}", String::from_utf8_lossy(&frame.bytes));
    // Shown again once, at the end, by the frame and by its erase.
    let erase = dark().erase(frame.area, pair.model.screen());
    for bytes in [&frame.bytes, &erase] {
        assert!(bytes.ends_with(b"\x1b[?25h"));
        assert_eq!(count(bytes, b"\x1b[?25l"), 1);
        assert_eq!(count(bytes, b"\x1b[?25h"), 1);
    }
    pair.cursors_match("draw");
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

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// Every row full: coloured text (16-colour, bold, underline, inverse,
/// 256-colour, RGB), wide characters at both column parities, coloured blank
/// cells, and text all the way to the right edge.
fn busy_background(pair: &mut Pair) {
    let (rows, cols) = (pair.rows, pair.cols);
    // (SGR and other set-up, text, columns the piece takes).
    let pieces: [(&str, &str, u16); 12] = [
        ("\x1b[31m", "red ", 4),
        ("\x1b[42m", "green-bg ", 9),
        ("\x1b[0;1m", "bold ", 5),
        ("\x1b[0;4m", "under ", 6),
        ("\x1b[0;7m", "inverse ", 8),
        ("\x1b[0;38;5;208m", "idx208 ", 7),
        ("\x1b[0;38;2;10;200;30;48;2;50;0;90m", "rgb ", 4),
        ("\x1b[0m", "日本語", 6),
        ("\x1b[33;44m", "漢字x", 5),
        ("\x1b[0;45m\x1b[3X", "\x1b[3C", 3),
        ("\x1b[0;1;3;96;101m", "Bi", 2),
        ("\x1b[0m", " plain ", 7),
    ];
    for row in 0..rows {
        let mut line = format!("\x1b[{};1H\x1b[0m", row + 1);
        // Shift each row so wide characters land on every column parity.
        let mut used = row % 3;
        line.push_str(&"·".repeat(usize::from(used)));
        // Coloured blanks to the right edge on some rows, text on the others.
        let coloured_end = row % 4 == 1;
        let limit = if coloured_end { cols - 8 } else { cols };
        for k in 0.. {
            let (setup, text, width) = pieces[(usize::from(row) + k) % pieces.len()];
            if used + width > limit {
                break;
            }
            line.push_str(setup);
            line.push_str(text);
            used += width;
        }
        if coloured_end {
            line.push_str("\x1b[0;46m\x1b[K");
        } else {
            line.push_str("\x1b[0;35m");
            line.push_str(&"z".repeat(usize::from(cols - used)));
        }
        pair.both(line.as_bytes());
    }
    pair.both(b"\x1b[0m");
}

fn invariant_case(items: &[Item], selected: Option<usize>, scroll: usize, row: u16, col: u16) {
    let mut pair = Pair::new();
    busy_background(&mut pair);
    pair.both(format!("\x1b[{};{}H", row + 1, col + 1).as_bytes());
    let renderer = dark();
    let what = format!("cursor ({row}, {col}), {} items", items.len());
    let Some(frame) = pair.draw(&renderer, &view(items, selected, scroll), col, None) else {
        return;
    };
    pair.cursors_match(&what);
    pair.erase(&renderer, frame.area);
    pair.cursors_match(&what);
    pair.screens_match(&what);
}

#[test]
fn draw_then_erase_restores_the_screen() {
    let sets = [git_items(), wide_items(), many_items(), long_items()];
    for items in &sets {
        for row in 0..ROWS {
            for col in [0, 1, 5, 13, 26, 37, 41, 60, 71, 79] {
                invariant_case(items, Some(1), 0, row, col);
                invariant_case(items, None, 0, row, col);
            }
        }
    }
    invariant_case(&many_items(), Some(15), 12, 23, 40);
}

/// On a 9-row screen the full popup (6 list rows and a 3-row bottom line)
/// does not fit: the bottom line keeps one row, and the erase still
/// restores every cell.
#[test]
fn shrunk_popup_draw_then_erase_restores_the_screen() {
    let items = git_items();
    for row in 0..9 {
        for col in [0, 13, 41, 79] {
            for selected in [None, Some(1), Some(5)] {
                let mut pair = Pair::sized(9, COLS);
                busy_background(&mut pair);
                pair.both(format!("\x1b[{};{}H", row + 1, col + 1).as_bytes());
                let cursor = pair.model.screen().cursor_position();
                let view = view(&items, selected, 0);
                let got = layout::compute(&view, cursor, col, (9, COLS), None).expect("layout");
                // 8 rows below the cursor: 6 list rows and 1 of the bottom
                // line's 3.
                assert_eq!((got.decided, got.bottom_rows), (3, 1));
                assert_eq!((got.list_rows, got.rect.rows, got.lines), (6, 7, 1));
                let renderer = dark();
                let what = format!("9 rows, cursor ({row}, {col}), selected {selected:?}");
                let frame = pair.draw(&renderer, &view, col, None).expect("popup shown");
                assert_eq!(frame.area.card, got.rect, "{what}");
                pair.cursors_match(&what);
                pair.erase(&renderer, frame.area);
                pair.cursors_match(&what);
                pair.screens_match(&what);
            }
        }
    }
}

/// Panes of 4 and 5 rows: the popup takes every row but the cursor's, all
/// of them list rows (no room for the bottom line). Drawn, redrawn scrolled
/// with another selection, and erased, every cell is restored.
#[test]
fn short_panes_draw_then_erase_restore_the_screen() {
    let items = many_items();
    for rows in [4u16, 5] {
        let list_rows = rows - 1;
        for row in 0..rows {
            for col in [0, 13, 41, 79] {
                for selected in [None, Some(7), Some(19)] {
                    // Scrolled as the controller does: the selection on the
                    // last row shown.
                    let scroll = selected.map_or(0, |index| index + 1 - usize::from(list_rows));
                    let mut pair = Pair::sized(rows, COLS);
                    busy_background(&mut pair);
                    pair.both(format!("\x1b[{};{}H", row + 1, col + 1).as_bytes());
                    let renderer = dark();
                    let what = format!("{rows} rows, cursor ({row}, {col}), selected {selected:?}");
                    let (first, layout) = pair
                        .draw_decided(&renderer, &view(&items, None, 0), col, None, None)
                        .expect("popup shown");
                    assert_eq!(first.area.card.rows, rows - 1, "{what}");
                    assert_eq!(layout.bottom_rows, 0, "{what}");
                    let view = view(&items, selected, scroll);
                    let (second, _) = pair
                        .draw_decided(
                            &renderer,
                            &view,
                            col,
                            Some(first.area),
                            Some(layout.decided),
                        )
                        .expect("popup shown");
                    let shown = pair.rows_of(second.area.card);
                    assert_eq!(shown.len(), usize::from(list_rows), "{what}");
                    // The selected item is among the rows shown.
                    if let Some(index) = selected {
                        let label = &items[index].label;
                        assert!(
                            shown
                                .iter()
                                .any(|line| line.starts_with(&format!(" - {label} "))),
                            "{what}: {shown:#?}"
                        );
                    }
                    pair.cursors_match(&what);
                    pair.erase(&renderer, second.area);
                    pair.cursors_match(&what);
                    pair.screens_match(&what);
                }
            }
        }
    }
}

#[test]
fn background_has_what_the_invariant_needs() {
    let mut pair = Pair::new();
    busy_background(&mut pair);
    let screen = pair.model.screen();
    let mut wide = [false; 2];
    let (mut rgb_fg, mut idx_fg, mut coloured_blank) = (false, false, false);
    for row in 0..ROWS {
        for col in 0..COLS {
            let Some(cell) = screen.cell(row, col) else {
                continue;
            };
            if cell.is_wide() {
                wide[usize::from(col % 2)] = true;
            }
            rgb_fg |= matches!(cell.fgcolor(), vt100::Color::Rgb(..));
            idx_fg |= matches!(cell.fgcolor(), vt100::Color::Idx(208));
            coloured_blank |= !cell.has_contents() && cell.bgcolor() != vt100::Color::Default;
        }
        assert!(
            screen
                .cell(row, COLS - 1)
                .is_some_and(vt100::Cell::has_contents)
                || screen
                    .cell(row, COLS - 1)
                    .is_some_and(|cell| cell.bgcolor() != vt100::Color::Default),
            "row {row} reaches the right edge"
        );
    }
    assert_eq!(wide, [true, true]);
    assert!(rgb_fg && idx_fg && coloured_blank);
}

#[test]
fn redraw_smaller_and_moved_then_erase() {
    for row in 0..ROWS {
        for col in [0, 3, 13, 40, 70, 79] {
            for (a, b, shift, popout) in [
                (git_items(), git_items()[..3].to_vec(), 7u16, false),
                (wide_items(), git_items(), 0, true),
                (git_items()[..2].to_vec(), many_items(), 5, false),
                (git_items(), wide_items(), 3, true),
            ] {
                let mut pair = Pair::new();
                busy_background(&mut pair);
                pair.both(format!("\x1b[{};{}H", row + 1, col + 1).as_bytes());
                let renderer = dark();
                let what = format!("cursor ({row}, {col}), {} then {}", a.len(), b.len());
                let first_view = PopupView {
                    popout,
                    ..view(&a, Some(0), 0)
                };
                let Some(first) = pair.draw(&renderer, &first_view, col, None) else {
                    continue;
                };
                let anchor = (col + shift).min(COLS - 1);
                let second = pair.draw(&renderer, &view(&b, None, 0), anchor, Some(first.area));
                let area = match second {
                    Some(second) => second.area,
                    None => first.area,
                };
                pair.cursors_match(&what);
                pair.erase(&renderer, area);
                pair.cursors_match(&what);
                pair.screens_match(&what);
            }
        }
    }
}

/// No cell of `bytes` (one frame or erase) is written twice.
fn paints_each_cell_once(bytes: &[u8], rows: u16, cols: u16, what: &str) {
    let writes =
        frames::interpret(&[bytes], rows, cols).unwrap_or_else(|err| panic!("{what}: {err}"));
    assert_eq!(
        writes.twice(),
        None,
        "{what}: {:?}",
        String::from_utf8_lossy(bytes)
    );
    assert!(!writes.is_empty(), "{what}: nothing written");
}

/// Terminal.app's widths (findings.md, fact 8): a ZWJ takes a column of its
/// own and VS16 none, so a heart with VS16 is 1 cell and a family of three
/// is 8, where fac's model makes them 2 and 6.
fn terminal_app_width(ch: char) -> u16 {
    match ch {
        '\u{200d}' => 1,
        ch => fac::popup::text::char_width(ch),
    }
}

/// The erase over shell text whose widths the model gets wrong for this
/// terminal: every row under the card has a heart with VS16 and a ZWJ
/// family inside the card, then ASCII to the right edge. Each write after
/// them is placed by CUP, so none lands outside the card.
#[test]
fn the_erase_over_vs16_and_zwj_text_stays_in_the_card() {
    let items = git_items();
    let (cursor, anchor) = ((3u16, 10u16), 10u16);
    let shown = view(&items, Some(0), 0);
    let card = layout::compute(&shown, cursor, anchor, (ROWS, COLS), None)
        .expect("layout")
        .rect;
    let mut pair = Pair::new();
    for row in 0..ROWS {
        let mut line = format!("\x1b[{};1H\x1b[0m", row + 1);
        line.push_str(&"a".repeat(usize::from(card.col)));
        // 1, 2 (❤️ in the model), 1, 6 (the family in the model) columns.
        line.push_str(
            "b\u{2764}\u{fe0f}c\x1b[33m\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}\x1b[0m",
        );
        line.push_str(&"d".repeat(usize::from(COLS - card.col - 10)));
        pair.both(line.as_bytes());
    }
    assert!(
        pair.model
            .screen()
            .cell(card.row, card.col + 1)
            .is_some_and(vt100::Cell::is_wide),
        "the model's heart is wide"
    );
    pair.both(format!("\x1b[{};{}H", cursor.0 + 1, cursor.1 + 1).as_bytes());
    let renderer = dark();
    let frame = pair
        .draw(&renderer, &shown, anchor, None)
        .expect("popup shown");
    assert_eq!(frame.area.card, card);
    let erase = renderer.erase(frame.area, pair.model.screen());
    let writes = frames::interpret_with(&[&erase], ROWS, COLS, terminal_app_width)
        .unwrap_or_else(|err| panic!("{err}"));
    let inside = |(row, col): (u16, u16)| {
        std::iter::once(frame.area.card)
            .chain(frame.area.popout)
            .any(|rect| {
                (rect.row..rect.row + rect.rows).contains(&row)
                    && (rect.col..rect.col + rect.cols).contains(&col)
            })
    };
    let mut outside: Vec<_> = writes.cells().filter(|&cell| !inside(cell)).collect();
    outside.sort_unstable();
    assert_eq!(
        outside,
        [],
        "card {card:?}: {:?}",
        String::from_utf8_lossy(&erase)
    );
    // And the model's own widths still restore the screen.
    pair.term.process(&erase);
    pair.screens_match("erase");
}

/// Over ASCII-only rows, the erase places each row once and writes the rest
/// of it straight on (or by CUF), as before.
#[test]
fn the_erase_over_ascii_rows_places_each_row_once() {
    let items = git_items();
    let mut pair = Pair::new();
    for row in 0..ROWS {
        let mut line = format!("\x1b[{};1H\x1b[0m", row + 1);
        for k in 0..(COLS / 8) {
            line.push_str(["\x1b[31m", "\x1b[0;1m", "\x1b[0;44m", "\x1b[0m"][usize::from(k % 4)]);
            line.push_str(if k % 3 == 0 { "ab   cd " } else { "efghijkl" });
        }
        pair.both(line.as_bytes());
    }
    pair.both(b"\x1b[4;11H");
    let renderer = dark();
    let frame = pair
        .draw(&renderer, &view(&items, Some(0), 0), 10, None)
        .expect("popup shown");
    let erase = renderer.erase(frame.area, pair.model.screen());
    let rows: u16 = std::iter::once(frame.area.card)
        .chain(frame.area.popout)
        .map(|rect| rect.rows)
        .sum();
    let cups = erase
        .windows(2)
        .enumerate()
        .filter(|(at, pair)| {
            pair == b"\x1b[" && {
                let rest = &erase[at + 2..];
                rest.iter()
                    .find(|byte| !byte.is_ascii_digit() && **byte != b';')
                    == Some(&b'H')
            }
        })
        .count();
    assert_eq!(
        cups,
        usize::from(rows),
        "{:?}",
        String::from_utf8_lossy(&erase)
    );
    pair.term.process(&erase);
    pair.screens_match("erase");
}

/// A line-local echo at the cursor: two letters and a backspace, or near
/// the right edge a backspace and a letter.
fn local_echo(pair: &mut Pair) {
    let (_, col) = pair.model.screen().cursor_position();
    if col + 3 < pair.cols {
        pair.both(b"ab\x08\x1b[K");
    } else {
        pair.both(b"\x08z");
    }
}

/// The erase deferred to the next frame: a popup drawn, the shell's
/// line-local echo, then the next popup drawn over the first from the model
/// after the echo (or the popup closed after it), then erased. On 4-, 5- and
/// 24-row screens every cell is restored, and no frame or erase writes a
/// cell twice.
#[test]
fn a_deferred_erase_restores_the_screen() {
    let pairs = [
        (many_items(), git_items()),
        (git_items()[..2].to_vec(), many_items()),
        (wide_items(), git_items()[..3].to_vec()),
        (git_items(), wide_items()),
    ];
    for rows in [4u16, 5, 24] {
        for row in 0..rows {
            for col in [0, 13, 41, 77, 79] {
                for (a, b) in &pairs {
                    let what = format!(
                        "{rows} rows, cursor ({row}, {col}), {} then {}",
                        a.len(),
                        b.len()
                    );
                    let mut pair = Pair::sized(rows, COLS);
                    busy_background(&mut pair);
                    pair.both(format!("\x1b[{};{}H", row + 1, col + 1).as_bytes());
                    let renderer = dark();
                    let Some(first) = pair.draw(&renderer, &view(a, Some(0), 0), col, None) else {
                        continue;
                    };
                    paints_each_cell_once(&first.bytes, rows, COLS, &what);
                    local_echo(&mut pair);
                    // Closed after the echo: erased from the model after it.
                    let mut closed = Pair::sized(rows, COLS);
                    closed.term = pair.term.clone_screen();
                    closed.model = pair.model.clone_screen();
                    let erase = renderer.erase(first.area, closed.model.screen());
                    paints_each_cell_once(&erase, rows, COLS, &what);
                    closed.term.process(&erase);
                    closed.cursors_match(&what);
                    closed.screens_match(&format!("{what}, closed after the echo"));
                    // Or drawn again over it.
                    let (_, anchor) = pair.model.screen().cursor_position();
                    let Some(second) = pair.draw(
                        &renderer,
                        &view(b, None, 0),
                        anchor.min(COLS - 1),
                        Some(first.area),
                    ) else {
                        continue;
                    };
                    paints_each_cell_once(&second.bytes, rows, COLS, &what);
                    pair.cursors_match(&what);
                    local_echo(&mut pair);
                    pair.erase(&renderer, second.area);
                    pair.cursors_match(&what);
                    pair.screens_match(&what);
                }
            }
        }
    }
}

trait CloneScreen {
    fn clone_screen(&self) -> vt100::Parser;
}

impl CloneScreen for vt100::Parser {
    /// A parser showing the same screen (contents, attributes, cursor and
    /// SGR), from `state_formatted`.
    fn clone_screen(&self) -> vt100::Parser {
        let screen = self.screen();
        let size = |n: u16| NonZeroU16::new(n).unwrap_or(NonZeroU16::MIN);
        let (rows, cols) = (screen.size().0.get(), screen.size().1.get());
        let mut copy = vt100::Parser::new(size(rows), size(cols), 0);
        copy.process(screen.state_formatted().as_bytes());
        copy
    }
}

/// The SGR a screen draws with now.
fn sgr(screen: &vt100::Screen) -> (vt100::Color, vt100::Color, bool, bool, bool, bool) {
    (
        screen.fgcolor(),
        screen.bgcolor(),
        screen.bold(),
        screen.italic(),
        screen.underline(),
        screen.inverse(),
    )
}

/// The rows a scroll adds take the SGR's background on a real terminal
/// (BCE): the scroll runs with the default SGR, then gives the shell's back.
#[test]
fn the_scroll_runs_with_the_default_sgr() {
    let mut pair = Pair::new();
    prompt_at(&mut pair, ROWS - 1);
    pair.both(b"\x1b[1;44m");
    let renderer = dark();
    let frame = pair
        .draw(&renderer, &view(&git_items(), None, 0), 12, None)
        .expect("popup shown");
    let mut expected = b"\x1b[0m\x1b[24;1H".to_vec();
    expected.extend_from_slice(&[b'\n'; 9]);
    expected.extend_from_slice(b"\x1b[0;1;44m\x1b[15;14H");
    assert_eq!(
        String::from_utf8_lossy(&frame.scroll),
        String::from_utf8_lossy(&expected)
    );
    pair.cursors_match("draw");
    assert_eq!(sgr(pair.term.screen()), sgr(pair.model.screen()));
    assert_eq!(pair.term.screen().bgcolor(), vt100::Color::Idx(4));
    pair.erase(&renderer, frame.area);
    pair.both(b"Q");
    pair.screens_match("the shell's next letter");
}

/// The shell saved its cursor (DECSC) and has not restored it: fac's frames
/// restore the cursor with a CUP and the shell's SGR instead of DECSC/DECRC,
/// which would replace the shell's. Its DECRC then finds its own cursor.
#[test]
fn a_cursor_the_shell_saved_is_kept() {
    let items = git_items();
    let mut pair = Pair::new();
    busy_background(&mut pair);
    pair.both(b"\x1b[3;3H\x1b7\x1b[6;14H\x1b[0;3;31;44m");
    let mut renderer = dark();
    renderer.set_restore_by_cup(true);
    let first = pair
        .draw(&renderer, &view(&items, None, 0), 13, None)
        .expect("popup shown");
    let second = pair
        .draw(&renderer, &view(&items, Some(2), 0), 13, Some(first.area))
        .expect("popup shown");
    let erase = renderer.erase(second.area, pair.model.screen());
    for bytes in [&first.bytes, &second.bytes, &erase] {
        assert!(
            !contains(bytes, b"\x1b7") && !contains(bytes, b"\x1b8"),
            "{:?}",
            String::from_utf8_lossy(bytes)
        );
        paints_each_cell_once(bytes, ROWS, COLS, "CUP restore");
    }
    pair.term.process(&erase);
    pair.cursors_match("erase");
    assert_eq!(sgr(pair.term.screen()), sgr(pair.model.screen()));
    pair.screens_match("erase");
    pair.both(b"Q\x1b8");
    assert_eq!(pair.term.screen().cursor_position(), (2, 2));
    pair.cursors_match("the shell's DECRC");
    pair.screens_match("the shell's letter");

    // Without one saved, fac's frames save and restore the cursor themselves.
    let frame = dark().draw(
        &view(&items, None, 0),
        &layout::compute(&view(&items, None, 0), (5, 13), 13, (ROWS, COLS), None).expect("layout"),
        (5, 13),
        None,
        pair.model.screen(),
    );
    assert!(frame.bytes.starts_with(b"\x1b[?25l\x1b7") && contains(&frame.bytes, b"\x1b8"));
}
