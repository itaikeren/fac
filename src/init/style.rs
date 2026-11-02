//! How the setup commands look: the terminal's own 16 ANSI colours (so
//! every theme, light or dark, fits), a UTF-8 or an ASCII set of marks,
//! and paths shown with `~` for the home folder.
//!
//! - bold: titles; cyan: `fac`, the cursor and command names; green:
//!   ticks and the success mark; yellow: notes; red: errors; dim: hints
//!   and labels.
//! - No colour when `NO_COLOR` is set (any value) or the output is not a
//!   terminal. Without a UTF-8 locale the marks are ASCII.

use std::path::{Path, PathBuf};

/// The width the long paragraphs wrap to.
pub const WIDTH: usize = 80;

/// The width of a label in a details row (`Starts in`), without the gap.
pub const LABEL: usize = 9;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Style {
    color: bool,
    utf8: bool,
    home: Option<PathBuf>,
}

/// Whether the locale in `lc_all`, `lc_ctype` and `lang` is UTF-8: the
/// first one that is set and not empty decides.
fn utf8_locale(lc_all: Option<&str>, lc_ctype: Option<&str>, lang: Option<&str>) -> bool {
    [lc_all, lc_ctype, lang]
        .into_iter()
        .flatten()
        .find(|value| !value.is_empty())
        .is_some_and(|value| {
            let value = value.to_ascii_lowercase();
            value.contains("utf-8") || value.contains("utf8")
        })
}

impl Style {
    pub fn new(color: bool, utf8: bool, home: Option<PathBuf>) -> Self {
        Self {
            color,
            utf8,
            home: home.filter(|home| home.as_os_str().len() > 1),
        }
    }

    /// From the process: colour when `is_tty` and `NO_COLOR` is unset,
    /// UTF-8 marks by the locale, `~` for `HOME`.
    pub fn detect(is_tty: bool) -> Self {
        let var = |name: &str| std::env::var(name).ok();
        Self::new(
            is_tty && std::env::var_os("NO_COLOR").is_none(),
            utf8_locale(
                var("LC_ALL").as_deref(),
                var("LC_CTYPE").as_deref(),
                var("LANG").as_deref(),
            ),
            std::env::var_os("HOME")
                .filter(|home| !home.is_empty())
                .map(PathBuf::from),
        )
    }

    fn paint(&self, code: &str, text: &str) -> String {
        if self.color && !text.is_empty() {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.to_owned()
        }
    }

    pub fn bold(&self, text: &str) -> String {
        self.paint("1", text)
    }

    pub fn dim(&self, text: &str) -> String {
        self.paint("2", text)
    }

    pub fn accent(&self, text: &str) -> String {
        self.paint("36", text)
    }

    pub fn green(&self, text: &str) -> String {
        self.paint("32", text)
    }

    pub fn yellow(&self, text: &str) -> String {
        self.paint("33", text)
    }

    pub fn red(&self, text: &str) -> String {
        self.paint("31", text)
    }

    pub fn bold_accent(&self, text: &str) -> String {
        self.paint("1;36", text)
    }

    fn mark<'a>(&self, utf8: &'a str, ascii: &'a str) -> &'a str {
        if self.utf8 { utf8 } else { ascii }
    }

    /// The cursor.
    pub fn cursor(&self) -> &'static str {
        self.mark("❯", ">")
    }

    /// A ticked box.
    pub fn on(&self) -> &'static str {
        self.mark("●", "*")
    }

    /// An unticked box.
    pub fn off(&self) -> &'static str {
        self.mark("○", "o")
    }

    pub fn tick(&self) -> &'static str {
        self.mark("✓", "+")
    }

    pub fn cross(&self) -> &'static str {
        self.mark("✗", "x")
    }

    pub fn warn(&self) -> &'static str {
        "!"
    }

    /// The separator between parts of a line.
    pub fn dot(&self) -> &'static str {
        self.mark("·", "-")
    }

    /// The arrow keys.
    pub fn updown(&self) -> &'static str {
        self.mark("↑↓", "Up/Down")
    }

    /// The `Shell › New Command` arrow.
    pub fn arrow(&self) -> &'static str {
        self.mark("›", ">")
    }

    /// `text` with the home folder written `~`, wherever a path in it
    /// starts with it.
    pub fn tilde(&self, text: &str) -> String {
        match &self.home {
            Some(home) => {
                let home = home.to_string_lossy();
                let home = home.trim_end_matches('/');
                text.replace(&format!("{home}/"), "~/")
            }
            None => text.to_owned(),
        }
    }

    /// `path` for people: `~/.zshrc`.
    pub fn path(&self, path: &Path) -> String {
        if let Some(home) = &self.home
            && path == home
        {
            return "~".to_owned();
        }
        self.tilde(&path.display().to_string())
    }

    /// `paths` joined with commas.
    pub fn paths(&self, paths: &[PathBuf]) -> String {
        paths
            .iter()
            .map(|path| self.path(path))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// A green success line: `  ✓ fac is set up`.
    pub fn success(&self, text: &str) -> String {
        format!("  {} {}", self.green(self.tick()), self.bold(text))
    }

    /// A yellow note line: `  ! text`, wrapped under itself.
    pub fn note(&self, text: &str) -> String {
        let lines = wrap(text, WIDTH, 4);
        let mut out = String::new();
        for (n, line) in lines.iter().enumerate() {
            if n == 0 {
                out.push_str(&format!("  {} {}", self.yellow(self.warn()), line));
            } else {
                out.push_str(&format!("\n    {line}"));
            }
        }
        out
    }

    /// A details row: `    Label       value`.
    pub fn row(&self, label: &str, value: &str) -> String {
        format!("    {}   {value}", self.dim(&format!("{label:<LABEL$}")))
    }

    /// A details row whose value is several lines, the label on the first.
    pub fn rows(&self, label: &str, values: &[String]) -> Vec<String> {
        values
            .iter()
            .enumerate()
            .map(|(n, value)| self.row(if n == 0 { label } else { "" }, value))
            .collect()
    }

    /// A command and what it does: `    fac disable     turn it off`.
    pub fn command(&self, command: &str, what: &str, width: usize) -> String {
        format!(
            "    {}{}   {}",
            self.accent(command),
            " ".repeat(width.saturating_sub(command.chars().count())),
            self.dim(what)
        )
    }
}

/// `labels` joined for a sentence: `a`, `a and b`, `a, b and c`.
pub fn join_words(labels: &[&str]) -> String {
    match labels {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// `text` wrapped to `width` columns on spaces, each line indented by
/// `indent` wide; the lines come back without the indent, which the
/// caller adds.
pub fn wrap(text: &str, width: usize, indent: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let used = line.chars().count() + indent;
        if !line.is_empty() && used + 1 + word.chars().count() > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() || lines.is_empty() {
        lines.push(line);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain() -> Style {
        Style::new(false, false, Some(PathBuf::from("/Users/me")))
    }

    #[test]
    fn locale_decides_utf8() {
        assert!(utf8_locale(None, None, Some("en_US.UTF-8")));
        assert!(utf8_locale(Some("C.utf8"), None, Some("C")));
        // The first one set decides.
        assert!(!utf8_locale(Some("C"), None, Some("en_US.UTF-8")));
        assert!(utf8_locale(Some(""), None, Some("en_US.UTF-8")));
        assert!(!utf8_locale(None, None, None));
    }

    #[test]
    fn no_colour_means_no_escapes_and_ascii_means_ascii() {
        let style = plain();
        assert_eq!(style.bold_accent("fac"), "fac");
        assert_eq!(style.success("fac is set up"), "  + fac is set up");
        assert_eq!(style.cursor(), ">");
        assert_eq!(style.on(), "*");
        assert_eq!(style.off(), "o");
        assert_eq!(style.cross(), "x");
        assert_eq!(style.warn(), "!");
        let fancy = Style::new(true, true, None);
        assert_eq!(fancy.green("x"), "\x1b[32mx\x1b[0m");
        assert_eq!(fancy.on(), "●");
    }

    #[test]
    fn paths_use_a_tilde() {
        let style = plain();
        assert_eq!(style.path(Path::new("/Users/me/.zshrc")), "~/.zshrc");
        assert_eq!(style.path(Path::new("/Users/me")), "~");
        // Not another user's folder with the same start.
        assert_eq!(
            style.path(Path::new("/Users/mealso/.zshrc")),
            "/Users/mealso/.zshrc"
        );
        assert_eq!(
            style.tilde("cannot write /Users/me/.zshrc: no"),
            "cannot write ~/.zshrc: no"
        );
        assert_eq!(
            style.paths(&[PathBuf::from("/Users/me/a"), PathBuf::from("/x/b")]),
            "~/a, /x/b"
        );
    }

    #[test]
    fn wrapping_fits_the_width() {
        let lines = wrap("one two three four five six", 12, 2);
        assert_eq!(lines, ["one two", "three four", "five six"]);
        assert_eq!(wrap("", 10, 2), [""]);
    }
}
