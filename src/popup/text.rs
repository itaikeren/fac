//! Text shaping for the popup: sanitizing, display width, truncation and
//! word wrapping. Every width here is in terminal columns, measured the way
//! vt100 measures a cell (unicode-width, capped at 2).

use std::borrow::Cow;
use std::fmt::Write as _;

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// One character of popup text and whether it is a matched character.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Glyph {
    pub ch: char,
    pub highlight: bool,
}

/// The columns `ch` takes on the screen.
pub fn char_width(ch: char) -> u16 {
    match ch.width() {
        Some(width) => u16::try_from(width.min(2)).unwrap_or(1),
        None => 0,
    }
}

pub fn width(text: &str) -> u16 {
    text.chars()
        .map(char_width)
        .fold(0u16, |sum, width| sum.saturating_add(width))
}

/// The columns `text` takes when the terminal writes it as typed, counted
/// the way the screen model (vt100 0.18.1, `Screen::text` and
/// `Cell::append_char`) counts them: a character takes its width capped at 2
/// (a control character below U+0100 and U+FFFD take none, any other character
/// with no width takes 1); a zero-width character joins the cell before it, and U+FE0F
/// widens that cell to 2 when the cell's contents are then wider than 1
/// (`❤` + VS16). Unlike `width`, this is for text the shell echoed, so
/// nothing is removed from it first.
pub fn typed_width(text: &str) -> u16 {
    let mut total = 0u16;
    // The contents of the last cell drawn, and whether it is wide.
    let mut cell = String::new();
    let mut wide = false;
    for ch in text.chars() {
        let width = match ch.width() {
            // vt100 prints neither (perform.rs, `print`).
            _ if ch == '\u{fffd}' => continue,
            None if u32::from(ch) < 256 => continue,
            width => width.unwrap_or(1),
        };
        if width > 0 {
            cell.clear();
            cell.push(ch);
            wide = width > 1;
            total = total.saturating_add(u16::try_from(width.min(2)).unwrap_or(2));
        } else if !cell.is_empty() {
            cell.push(ch);
            if ch == '\u{fe0f}' && !wide && cell.width() > 1 {
                wide = true;
                total = total.saturating_add(1);
            }
        }
    }
    total
}

/// `typed` as ZLE (zsh's line editor) draws it, so that `typed_width` counts
/// what the terminal receives. `first` is the index (in chars) of `typed`'s
/// first character in the buffer and `undrawn` the sorted indices zsh
/// reported as characters ZLE does not draw as themselves (see `fac.zsh`'s
/// `_fac_undrawn`). Following `zle_refresh.c` (zsh 5.9, `zrefresh`):
/// - a control character below U+0100 is `^` and one letter: `?` when the
///   code without its high bit is above 31, else the code with `@` or'ed in
///   (lines 1396-1415);
/// - any other character is `<` and its code in hex, 4 digits, or 8 above
///   U+FFFF, and `>` ("Resort to hackery", lines 1417-1446).
pub fn drawn_text<'a>(typed: &'a str, first: usize, undrawn: &[usize]) -> Cow<'a, str> {
    if undrawn.is_empty() {
        return Cow::Borrowed(typed);
    }
    let mut out = String::with_capacity(typed.len());
    for (offset, ch) in typed.chars().enumerate() {
        if undrawn.binary_search(&(first + offset)).is_err() {
            out.push(ch);
            continue;
        }
        let code = u32::from(ch);
        if code <= 0xff {
            out.push('^');
            let letter = if code & !0x80 > 31 { 0x3f } else { code | 0x40 };
            out.push(char::from_u32(letter).unwrap_or('?'));
        } else if code > 0xffff {
            let _ = write!(out, "<{code:08x}>");
        } else {
            let _ = write!(out, "<{code:04x}>");
        }
    }
    Cow::Owned(out)
}

pub fn glyphs_width(glyphs: &[Glyph]) -> u16 {
    glyphs
        .iter()
        .map(|glyph| char_width(glyph.ch))
        .fold(0u16, |sum, width| sum.saturating_add(width))
}

/// C0 controls, ESC, DEL and C1 controls are removed; a tab becomes one
/// space.
///
/// The characters that make terminals disagree on a width are removed too,
/// so that every terminal draws exactly the width fac counts (step 1b
/// measured the disagreement; findings.md fact 8). This is the one place
/// that rule lives, and it only changes what is drawn: what accepting an
/// item inserts is never sanitized.
/// - VS16 (`U+FE0F`): a heart with it is 1 cell in Terminal.app, VS Code and
///   Zed and 2 in iTerm2 and Ghostty. Without it, the base character is in
///   its text form, narrow everywhere.
/// - ZWJ (`U+200D`): a family of three people is 8 cells in Terminal.app
///   (3 x 2, plus one cell per ZWJ), 6 in VS Code and Zed and 2 in iTerm2 and
///   Ghostty. Without it, each person is drawn on their own, 6 cells
///   everywhere.
/// - Emoji modifiers (`U+1F3FB`..=`U+1F3FF`): terminals that join them to the
///   base draw 2 cells where fac counts 4. This case was not measured.
fn sanitize_char(ch: char) -> Option<char> {
    match ch {
        '\t' => Some(' '),
        '\0'..='\x1f' | '\x7f'..='\u{9f}' => None,
        '\u{fe0f}' | '\u{200d}' | '\u{1f3fb}'..='\u{1f3ff}' => None,
        _ => Some(ch),
    }
}

pub fn sanitize(text: &str) -> String {
    text.chars().filter_map(sanitize_char).collect()
}

/// `text` sanitized, each character marked when its index (into the original
/// `text`, in chars) is in `highlights`.
pub fn sanitize_highlighted(text: &str, highlights: &[u32]) -> Vec<Glyph> {
    text.chars()
        .enumerate()
        .filter_map(|(index, ch)| {
            let highlight = u32::try_from(index).is_ok_and(|index| highlights.contains(&index));
            sanitize_char(ch).map(|ch| Glyph { ch, highlight })
        })
        .collect()
}

/// At most `max` columns of `glyphs`. Truncated text ends in `…` (spaces
/// before it are dropped); a wide character that would straddle the edge is
/// dropped, and the caller pads the column it leaves with a space. Returns
/// the glyphs and their width.
pub fn truncate(glyphs: &[Glyph], max: u16) -> (Vec<Glyph>, u16) {
    let total = glyphs_width(glyphs);
    if total <= max {
        return (glyphs.to_vec(), total);
    }
    if max == 0 {
        return (Vec::new(), 0);
    }
    let room = max - 1;
    let mut out = Vec::new();
    let mut used = 0u16;
    for glyph in glyphs {
        let width = char_width(glyph.ch);
        if used + width > room {
            break;
        }
        used += width;
        out.push(*glyph);
    }
    while out.last().is_some_and(|glyph| glyph.ch == ' ') {
        out.pop();
        used -= 1;
    }
    out.push(Glyph {
        ch: '…',
        highlight: false,
    });
    (out, used + 1)
}

pub fn plain(text: &str) -> Vec<Glyph> {
    text.chars()
        .map(|ch| Glyph {
            ch,
            highlight: false,
        })
        .collect()
}

/// `text` (already sanitized) word-wrapped into at most `lines` lines of at
/// most `max` columns; when it does not fit, the last line ends in `…`.
/// A word longer than a line is broken between characters.
pub fn wrap(text: &str, max: u16, lines: usize) -> Vec<String> {
    if max == 0 || lines == 0 {
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::new();
    let mut line = String::new();
    let mut used = 0u16;
    for word in text.split_whitespace() {
        let word_width = width(word);
        let gap = u16::from(!line.is_empty());
        if used + gap + word_width <= max {
            if gap == 1 {
                line.push(' ');
            }
            line.push_str(word);
            used += gap + word_width;
            continue;
        }
        if !line.is_empty() {
            out.push(std::mem::take(&mut line));
            used = 0;
        }
        for ch in word.chars() {
            let ch_width = char_width(ch);
            if used + ch_width > max {
                out.push(std::mem::take(&mut line));
                used = 0;
            }
            line.push(ch);
            used += ch_width;
        }
    }
    if !line.is_empty() {
        out.push(line);
    }
    if out.len() > lines {
        let rest = out[lines - 1..].join(" ");
        out.truncate(lines - 1);
        let (cut, _) = truncate(&plain(&rest), max);
        out.push(cut.iter().map(|glyph| glyph.ch).collect());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(glyphs: &[Glyph]) -> String {
        glyphs.iter().map(|glyph| glyph.ch).collect()
    }

    /// The column vt100 puts the cursor at after writing `text` from column 0.
    fn vt100_columns(text: &str) -> u16 {
        let size = |n| std::num::NonZeroU16::new(n).expect("non-zero size");
        let mut parser = vt100::Parser::new(size(5), size(200), 0);
        parser.process(text.as_bytes());
        parser.screen().cursor_position().1
    }

    #[test]
    fn typed_width_is_the_screen_models() {
        for text in [
            "abc",
            "\u{65e5}\u{672c}",
            "e\u{301}",
            "\u{2764}",
            "\u{2764}\u{fe0f}",
            "a\u{2764}\u{fe0f}b",
            "\u{2764}\u{fe0f}\u{fe0f}",
            "\u{fe0f}",
            "a\u{fe0f}",
            "\u{2600}\u{fe0f}x",
            "1\u{fe0f}\u{20e3}",
            "\u{1f44d}\u{1f3fd}",
            "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}",
            "\u{2764}\u{fe0f}\u{200d}\u{1f525}",
            "a\u{7}b\u{1}c",
            "\u{301}",
            "x\u{17d8}y",
            "a\u{fffd}b",
        ] {
            assert_eq!(typed_width(text), vt100_columns(text), "{text:?}");
        }
        assert_eq!(typed_width("\u{2764}\u{fe0f}"), 2);
        assert_eq!(width("\u{2764}\u{fe0f}"), 1);
    }

    /// The drawn text of `typed`, its characters from index 3, with these
    /// indices reported undrawn.
    fn drawn(typed: &str, undrawn: &[usize]) -> String {
        drawn_text(typed, 3, undrawn).into_owned()
    }

    #[test]
    fn zle_draws_what_it_reports_as_text() {
        // Section 1 of step 6b, measured in zsh 5.9 on macOS.
        assert_eq!(drawn("a", &[]), "a");
        assert_eq!(drawn("\u{2764}\u{fe0f}", &[4]), "\u{2764}<fe0f>");
        assert_eq!(drawn("e\u{301}", &[4]), "e<0301>");
        assert_eq!(drawn("\u{301}", &[3]), "<0301>");
        let family = "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}";
        assert_eq!(
            drawn(family, &[4, 6]),
            "\u{1f468}<200d>\u{1f469}<200d>\u{1f467}"
        );
        assert_eq!(drawn("\u{2028}", &[3]), "<2028>");
        assert_eq!(drawn("\u{378}", &[3]), "<0378>");
        // Above U+FFFF the code has 8 digits.
        assert_eq!(drawn("\u{10ffff}", &[3]), "<0010ffff>");
        assert_eq!(drawn("\u{e0067}", &[3]), "<000e0067>");
        // Controls: `^?` when the code without its high bit is above 31, else
        // the code with `@` or'ed in.
        assert_eq!(drawn("\u{ad}", &[3]), "^?");
        assert_eq!(drawn("\u{85}", &[3]), "^\u{c5}");
        assert_eq!(drawn("\u{80}", &[3]), "^\u{c0}");
        assert_eq!(drawn("\u{9f}", &[3]), "^\u{df}");
        assert_eq!(drawn("\u{1}", &[3]), "^A");
        assert_eq!(drawn("\u{7f}", &[3]), "^?");
        // Only the reported characters, by index.
        assert_eq!(drawn("e\u{301}e\u{301}", &[6]), "e\u{301}e<0301>");
    }

    #[test]
    fn what_zle_draws_is_counted_as_the_terminal_gets_it() {
        // `e<0301>` is 7 cells; the joined `é` is 1.
        assert_eq!(typed_width(&drawn("e\u{301}", &[4])), 7);
        assert_eq!(typed_width(&drawn("e\u{301}", &[])), 1);
        // 3 wide parts and 2 `<200d>`.
        let family = "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}";
        assert_eq!(typed_width(&drawn(family, &[4, 6])), 6 + 12);
        assert_eq!(typed_width(&drawn("\u{85}", &[3])), 2);
        assert_eq!(typed_width("a\u{fffd}b"), 2);
    }

    proptest::proptest! {
        #[test]
        fn typed_width_matches_vt100_for_any_mix(
            text in proptest::collection::vec(
                proptest::sample::select(vec![
                    'a', ' ', '\u{2764}', '\u{fe0f}', '\u{fe0e}', '\u{200d}', '\u{301}',
                    '\u{1f468}', '\u{1f3fd}', '\u{65e5}', '\u{2600}', '\u{20e3}', '\u{7}', '\u{fffd}',
                ]),
                0..24,
            )
        ) {
            let text: String = text.into_iter().collect();
            proptest::prop_assert_eq!(typed_width(&text), vt100_columns(&text), "{:?}", text);
        }
    }

    #[test]
    fn widths() {
        assert_eq!(width("abc"), 3);
        assert_eq!(width("日本語"), 6);
        assert_eq!(width("e\u{301}"), 1);
    }

    #[test]
    fn sanitize_strips_controls() {
        assert_eq!(
            sanitize("a\x1b[31mb\x07c\x7fd\u{9b}e\tf\r\n"),
            "a[31mbcde f"
        );
        assert_eq!(sanitize("日本\u{85}語"), "日本語");
    }

    #[test]
    fn a_heart_with_vs16_is_drawn_in_its_text_form() {
        assert_eq!(sanitize("a\u{2764}\u{fe0f}b"), "a\u{2764}b");
        assert_eq!(width(&sanitize("\u{2764}\u{fe0f}")), 1);
    }

    #[test]
    fn a_zwj_family_is_drawn_as_its_parts() {
        let family = "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}";
        assert_eq!(sanitize(family), "\u{1f468}\u{1f469}\u{1f467}");
        // Three 2-column emoji: the 6 cells VS Code and Zed draw, not the 8
        // Terminal.app draws for the joined form.
        assert_eq!(width(&sanitize(family)), 6);
    }

    #[test]
    fn a_skin_tone_modifier_is_dropped() {
        assert_eq!(sanitize("\u{1f44d}\u{1f3fd}"), "\u{1f44d}");
        assert_eq!(width(&sanitize("\u{1f44d}\u{1f3fd}")), 2);
    }

    #[test]
    fn flags_cjk_and_ambiguous_characters_are_kept() {
        let flag = "\u{1f1ee}\u{1f1f1}";
        assert_eq!(sanitize(flag), flag);
        assert_eq!(sanitize("漢字"), "漢字");
        assert_eq!(sanitize("±§"), "±§");
        assert_eq!(width("±§"), 2);
        assert_eq!(width("漢字"), 4);
    }

    #[test]
    fn highlights_across_dropped_characters_keep_their_characters() {
        // chars: 0 a, 1 heart, 2 VS16, 3 b, 4 ZWJ, 5 c
        let glyphs = sanitize_highlighted("a\u{2764}\u{fe0f}b\u{200d}c", &[0, 2, 3, 4]);
        let marked: Vec<(char, bool)> = glyphs.iter().map(|g| (g.ch, g.highlight)).collect();
        assert_eq!(
            marked,
            vec![('a', true), ('\u{2764}', false), ('b', true), ('c', false)]
        );
    }

    #[test]
    fn highlights_survive_sanitizing() {
        let glyphs = sanitize_highlighted("a\x1bbc", &[0, 2]);
        let marked: Vec<(char, bool)> = glyphs.iter().map(|g| (g.ch, g.highlight)).collect();
        assert_eq!(marked, vec![('a', true), ('b', true), ('c', false)]);
    }

    #[test]
    fn truncation() {
        let (out, used) = truncate(&plain("hello"), 5);
        assert_eq!((text(&out).as_str(), used), ("hello", 5));
        let (out, used) = truncate(&plain("hello world"), 5);
        assert_eq!((text(&out).as_str(), used), ("hell…", 5));
        // 日 fits in 3 columns before the ellipsis, 本 would straddle.
        let (out, used) = truncate(&plain("日本語"), 4);
        assert_eq!((text(&out).as_str(), used), ("日…", 3));
        let (out, used) = truncate(&plain("日本語"), 5);
        assert_eq!((text(&out).as_str(), used), ("日本…", 5));
        let (out, used) = truncate(&plain("abc"), 0);
        assert_eq!((out.len(), used), (0, 0));
        let (out, used) = truncate(&plain("abc"), 1);
        assert_eq!((text(&out).as_str(), used), ("…", 1));
    }

    #[test]
    fn wrapping() {
        assert_eq!(wrap("one two three", 7, 3), vec!["one two", "three"]);
        assert_eq!(wrap("abcdefghij", 4, 3), vec!["abcd", "efgh", "ij"]);
        assert_eq!(
            wrap("one two three four five six", 7, 2),
            vec!["one two", "three…"]
        );
        let lines = wrap("aa bb cc dd ee ff gg", 5, 3);
        assert_eq!(lines, vec!["aa bb", "cc dd", "ee f…"]);
        assert!(wrap("", 5, 3).is_empty());
        assert!(wrap("x", 0, 3).is_empty());
    }
}
