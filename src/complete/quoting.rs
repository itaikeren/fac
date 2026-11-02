//! Quote completion text for insertion into the word being typed.

use super::tokenizer::{Dialect, Quote};

/// Characters that are backslash-escaped anywhere in an unquoted word.
const ESCAPED: &str = "\\'\"$&;|<>()*?[]{}!` \t";

/// Quote `text` for insertion into a word whose open quote state is `quote`,
/// by `dialect`'s rules. `verbatim` (compsys -Q): return `text` unchanged.
/// `close`: append the closing quote (when not continuing, e.g. not a
/// directory).
pub fn quote_for(
    dialect: Dialect,
    text: &str,
    quote: Quote,
    verbatim: bool,
    close: bool,
) -> String {
    match dialect {
        // bash reads these escapes and quotes as zsh does.
        Dialect::Zsh | Dialect::Bash => quote_zsh(text, quote, verbatim, close),
        Dialect::Fish => quote_fish(text, quote, verbatim, close),
    }
}

/// Characters fish's own completion escapes with a backslash anywhere in an
/// unquoted word (`string escape --no-quoted`, fish 4.9); `#` only where it
/// would start a comment: first, or after an escaped character.
const FISH_ESCAPED: &str = "\\'\"$&;|<>()[]{}*%~ ";

/// [`quote_for`] by fish's rules: backslashes outside quotes (`\n`, `\t`,
/// `\e`, `\xHH` for control characters, fish has no `$'...'`); inside
/// single quotes only `\\` and `\'`, inside double quotes `\\`, `\"` and
/// `\$`. A control character inside quotes closes the quote around its
/// escape.
fn quote_fish(text: &str, quote: Quote, verbatim: bool, close: bool) -> String {
    if verbatim {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len() + 4);
    match quote {
        Quote::None => {
            let mut escaped = false;
            for (i, c) in text.chars().enumerate() {
                if push_fish_control(&mut out, c) {
                    escaped = true;
                    continue;
                }
                escaped = FISH_ESCAPED.contains(c) || (c == '#' && (i == 0 || escaped));
                if escaped {
                    out.push('\\');
                }
                out.push(c);
            }
        }
        Quote::Single | Quote::Dollar | Quote::Double => {
            let mark = if quote == Quote::Double { '"' } else { '\'' };
            for c in text.chars() {
                if c.is_ascii_control() && c != '\n' {
                    out.push(mark);
                    push_fish_control(&mut out, c);
                    out.push(mark);
                    continue;
                }
                let special = match quote {
                    Quote::Double => matches!(c, '\\' | '"' | '$'),
                    _ => matches!(c, '\\' | '\''),
                };
                if special {
                    out.push('\\');
                }
                out.push(c);
            }
            if close {
                out.push(mark);
            }
        }
    }
    out
}

/// fish's escape for an ASCII control character (`\n`, `\t`, `\e`, `\xHH`)
/// pushed onto `out`; false for any other character.
fn push_fish_control(out: &mut String, c: char) -> bool {
    match c {
        '\n' => out.push_str("\\n"),
        '\t' => out.push_str("\\t"),
        '\r' => out.push_str("\\r"),
        '\x1b' => out.push_str("\\e"),
        c if c.is_ascii_control() => out.push_str(&format!("\\x{:02x}", u32::from(c))),
        _ => return false,
    }
    true
}

/// [`quote_for`] by zsh's rules.
fn quote_zsh(text: &str, quote: Quote, verbatim: bool, close: bool) -> String {
    if verbatim {
        return text.to_string();
    }
    match quote {
        Quote::None => {
            if text.chars().any(is_exotic_control) {
                let mut out = String::from("$'");
                push_dollar_escaped(&mut out, text);
                out.push('\'');
                out
            } else {
                let mut out = String::with_capacity(text.len() + 4);
                for (i, c) in text.chars().enumerate() {
                    if ESCAPED.contains(c) || (i == 0 && matches!(c, '~' | '=' | '#')) {
                        out.push('\\');
                    }
                    out.push(c);
                }
                out
            }
        }
        Quote::Single => {
            let mut out = text.replace('\'', "'\\''");
            if close {
                out.push('\'');
            }
            out
        }
        Quote::Double => {
            let mut out = String::with_capacity(text.len() + 2);
            for c in text.chars() {
                if c == '!' {
                    // History expansion runs inside double quotes, and `\!`
                    // there is read as `!` only while it is on (otherwise it
                    // keeps the backslash): the quote is closed around `\!`.
                    out.push_str("\"\\!\"");
                    continue;
                }
                if matches!(c, '\\' | '"' | '$' | '`') {
                    out.push('\\');
                }
                out.push(c);
            }
            if close {
                out.push('"');
            }
            out
        }
        Quote::Dollar => {
            let mut out = String::with_capacity(text.len() + 2);
            push_dollar_escaped(&mut out, text);
            if close {
                out.push('\'');
            }
            out
        }
    }
}

/// An ASCII control character other than tab (tab is just backslash-escaped).
fn is_exotic_control(c: char) -> bool {
    c.is_ascii_control() && c != '\t'
}

/// Escape for the inside of `$'...'`.
fn push_dollar_escaped(out: &mut String, text: &str) {
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c if c.is_ascii_control() => {
                out.push_str(&format!("\\x{:02x}", u32::from(c)));
            }
            c => out.push(c),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::complete::tokenizer::analyze;
    use proptest::prelude::*;

    #[test]
    fn unquoted_table() {
        let cases = [
            ("plain", "plain"),
            ("My Documents", "My\\ Documents"),
            ("a\tb", "a\\\tb"),
            ("it's", "it\\'s"),
            ("say \"hi\"", "say\\ \\\"hi\\\""),
            ("a$b", "a\\$b"),
            ("a&b;c|d", "a\\&b\\;c\\|d"),
            ("<a>(b)", "\\<a\\>\\(b\\)"),
            ("*?[x]{y}!", "\\*\\?\\[x\\]\\{y\\}\\!"),
            ("`cmd`", "\\`cmd\\`"),
            ("back\\slash", "back\\\\slash"),
            ("~/x", "\\~/x"),
            ("a~b", "a~b"),
            ("=x", "\\=x"),
            ("a=b", "a=b"),
            ("#tag", "\\#tag"),
            ("a#b", "a#b"),
            ("é✅", "é✅"),
            ("", ""),
        ];
        for (text, want) in cases {
            assert_eq!(
                quote_for(Dialect::Zsh, text, Quote::None, false, false),
                want,
                "{text:?}"
            );
            // `close` makes no difference without a quote.
            assert_eq!(
                quote_for(Dialect::Zsh, text, Quote::None, false, true),
                want
            );
        }
    }

    #[test]
    fn control_characters_switch_to_dollar_quotes() {
        let q = |t| quote_for(Dialect::Zsh, t, Quote::None, false, false);
        assert_eq!(q("a\nb"), "$'a\\nb'");
        assert_eq!(q("a\nb c"), "$'a\\nb c'");
        assert_eq!(q("it's\n"), "$'it\\'s\\n'");
        assert_eq!(q("back\\\nslash"), "$'back\\\\\\nslash'");
        assert_eq!(q("\x01\x7f"), "$'\\x01\\x7f'");
        assert_eq!(q("a\tb\r"), "$'a\\tb\\x0d'");
    }

    #[test]
    fn single_quote_table() {
        let q = |t, close| quote_for(Dialect::Zsh, t, Quote::Single, false, close);
        assert_eq!(q("My Documents", false), "My Documents");
        assert_eq!(q("My Documents", true), "My Documents'");
        assert_eq!(q("it's", false), "it'\\''s");
        assert_eq!(q("it's", true), "it'\\''s'");
        assert_eq!(q("a$b\"c\\", false), "a$b\"c\\");
    }

    #[test]
    fn double_quote_table() {
        let q = |t, close| quote_for(Dialect::Zsh, t, Quote::Double, false, close);
        assert_eq!(q("My Documents", true), "My Documents\"");
        assert_eq!(q("a\"b$c`d\\e", false), "a\\\"b\\$c\\`d\\\\e");
        assert_eq!(q("x", true), "x\"");
        // `!` would start a history expansion: the quote is closed around it.
        assert_eq!(q("a!b", true), "a\"\\!\"b\"");
        assert_eq!(q("it's !", false), "it's \"\\!\"");
        assert_eq!(q("!!", true), "\"\\!\"\"\\!\"\"");
    }

    #[test]
    fn dollar_quote_table() {
        let q = |t, close| quote_for(Dialect::Zsh, t, Quote::Dollar, false, close);
        assert_eq!(q("a b", true), "a b'");
        assert_eq!(q("it's", false), "it\\'s");
        assert_eq!(q("a\nb\\", true), "a\\nb\\\\'");
        assert_eq!(q("\x1b", false), "\\x1b");
    }

    /// bash quotes as zsh.
    #[test]
    fn bash_quotes_as_zsh() {
        for quote in [Quote::None, Quote::Single, Quote::Double, Quote::Dollar] {
            for text in ["My Documents", "it's", "a$b!c", "tab\there", "\u{7}"] {
                for close in [false, true] {
                    assert_eq!(
                        quote_for(Dialect::Bash, text, quote, false, close),
                        quote_for(Dialect::Zsh, text, quote, false, close),
                        "{quote:?} {text:?}"
                    );
                }
            }
        }
    }

    /// fish's unquoted escapes, as `string escape --no-quoted` writes them
    /// (fish 4.9.3).
    #[test]
    fn fish_unquoted_table() {
        let cases = [
            ("plain", "plain"),
            ("My Documents", "My\\ Documents"),
            ("it's", "it\\'s"),
            ("say \"hi\"", "say\\ \\\"hi\\\""),
            ("a$b", "a\\$b"),
            ("a&b;c|d", "a\\&b\\;c\\|d"),
            ("<a>(b)", "\\<a\\>\\(b\\)"),
            ("*[x]{y}", "\\*\\[x\\]\\{y\\}"),
            // Not special to fish.
            ("?!=^,@`", "?!=^,@`"),
            ("back\\slash", "back\\\\slash"),
            ("~/x", "\\~/x"),
            ("x~", "x\\~"),
            ("%x", "\\%x"),
            ("#x", "\\#x"),
            ("x#", "x#"),
            ("$#", "\\$\\#"),
            ("a b#", "a\\ b#"),
            ("a\tb\nc", "a\\tb\\nc"),
            ("\x1b\r\x01\x7f", "\\e\\r\\x01\\x7f"),
            ("é✅ 中", "é✅\\ 中"),
            ("", ""),
        ];
        for (text, want) in cases {
            for close in [false, true] {
                assert_eq!(
                    quote_for(Dialect::Fish, text, Quote::None, false, close),
                    want,
                    "{text:?}"
                );
            }
        }
    }

    #[test]
    fn fish_quoted_tables() {
        let single = |t, close| quote_for(Dialect::Fish, t, Quote::Single, false, close);
        assert_eq!(single("My Documents", true), "My Documents'");
        assert_eq!(single("it's", false), "it\\'s");
        assert_eq!(single("a\\b", true), "a\\\\b'");
        assert_eq!(single("a$b\"c(d)", false), "a$b\"c(d)");
        assert_eq!(single("a\x01b", true), "a'\\x01'b'");
        let double = |t, close| quote_for(Dialect::Fish, t, Quote::Double, false, close);
        assert_eq!(double("My Documents", true), "My Documents\"");
        assert_eq!(double("a\"b$c\\d", false), "a\\\"b\\$c\\\\d");
        // No history expansion and no backtick substitution in fish.
        assert_eq!(double("a!b`c'", true), "a!b`c'\"");
        assert_eq!(double("a\x1bb", false), "a\"\\e\"b");
        // fish has no `$'...'`: as a single quote.
        assert_eq!(
            quote_for(Dialect::Fish, "it's", Quote::Dollar, false, true),
            "it\\'s'"
        );
    }

    fn fish_round_trips(text: &str) {
        let q = quote_for(Dialect::Fish, text, Quote::None, false, false);
        let ctx = analyze(Dialect::Fish, "", &format!("cmd {q}"), "");
        assert_eq!(ctx.current.unquoted, text, "{q:?}");
        assert_eq!(ctx.words_before, ["cmd"], "{q:?}");
        for (open, quote) in [("'", Quote::Single), ("\"", Quote::Double)] {
            let line = format!(
                "cmd {open}{}",
                quote_for(Dialect::Fish, text, quote, false, true)
            );
            let ctx = analyze(Dialect::Fish, "", &line, "");
            assert_eq!(ctx.current.unquoted, text, "{line:?}");
            assert_eq!(ctx.current.quote, Quote::None, "{line:?}");
            assert_eq!(ctx.words_before, ["cmd"], "{line:?}");
        }
    }

    #[test]
    fn fish_round_trip_examples() {
        for text in [
            "plain",
            "My Documents",
            "it's",
            "a\"b",
            "~",
            "#",
            "a\nb",
            "\\",
            "\\'",
            "(x)",
            "$(x)",
            "`x`",
            "$'x'",
            "é ✅",
            "\x01\x7f",
            "a;b&&c||d",
            "<>",
            "{a,b}",
            "and",
        ] {
            fish_round_trips(text);
        }
    }

    #[test]
    fn verbatim_returns_the_text_unchanged() {
        for quote in [Quote::None, Quote::Single, Quote::Double, Quote::Dollar] {
            assert_eq!(
                quote_for(Dialect::Zsh, "a b'\"$\n", quote, true, true),
                "a b'\"$\n"
            );
        }
    }

    fn round_trips(text: &str, quote: Quote) {
        let (open, expect_closed) = match quote {
            Quote::None => ("", Quote::None),
            Quote::Single => ("'", Quote::None),
            Quote::Double => ("\"", Quote::None),
            Quote::Dollar => ("$'", Quote::None),
        };
        for close in [false, true] {
            if quote == Quote::None && close {
                continue;
            }
            let line = format!(
                "cmd {open}{}",
                quote_for(Dialect::Zsh, text, quote, false, close)
            );
            let ctx = analyze(Dialect::Zsh, "", &line, "");
            assert_eq!(ctx.current.unquoted, text, "{line:?}");
            let want = if close { expect_closed } else { quote };
            assert_eq!(ctx.current.quote, want, "{line:?}");
            assert_eq!(ctx.words_before, ["cmd"], "{line:?}");
        }
    }

    #[test]
    fn round_trip_examples() {
        for text in [
            "plain",
            "My Documents",
            "it's",
            "a\"b",
            "~",
            "=",
            "#",
            "a\nb",
            "\\",
            "\\\n",
            "$(x)",
            "`x`",
            "$'x'",
            "2",
            "é ✅",
            "\x00\x7f",
            "a;b&&c||d",
            "<>",
            "{a,b}",
            "!",
        ] {
            for quote in [Quote::None, Quote::Single, Quote::Double, Quote::Dollar] {
                round_trips(text, quote);
            }
        }
    }

    proptest! {
        #[test]
        fn unquoted_round_trip(text in any::<String>()) {
            let q = quote_for(Dialect::Zsh, &text, Quote::None, false, false);
            let ctx = analyze(Dialect::Zsh, "", &q, "");
            prop_assert_eq!(ctx.current.unquoted, text);
        }

        #[test]
        fn fish_round_trip(text in any::<String>()) {
            // A NUL cannot be in a fish word at all.
            prop_assume!(!text.contains('\0'));
            fish_round_trips(&text);
        }

        #[test]
        fn quoted_round_trip(text in any::<String>()) {
            for quote in [Quote::Single, Quote::Double, Quote::Dollar] {
                let open = match quote {
                    Quote::Single => "'",
                    Quote::Double => "\"",
                    _ => "$'",
                };
                let line = format!("{open}{}", quote_for(Dialect::Zsh, &text, quote, false, true));
                let ctx = analyze(Dialect::Zsh, "", &line, "");
                prop_assert_eq!(&ctx.current.unquoted, &text);
                prop_assert_eq!(ctx.current.quote, Quote::None);
            }
        }
    }
}
