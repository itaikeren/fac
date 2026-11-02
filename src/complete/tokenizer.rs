//! Understand the command line around the cursor: which word is being
//! completed, how it is quoted, whether it is a command name, where the
//! completion context is cut, and how typed bytes map to the unquoted word.
//!
//! The line is `LBUFFER + RBUFFER`; the cursor is at `LBUFFER.len()`.
//!
//! Simplifications, all deliberate:
//! - Command substitution (`$(`, `` ` ``, `<(`, `>(`) is recognised only
//!   outside quotes; inside quotes `$(` is literal text. Variables, globs and
//!   braces are never expanded: `$HO` stays `$HO`.
//! - `words_before` holds the unquoted words of the current simple command
//!   without redirection operators, redirection targets or leading
//!   `NAME=value` assignments. Precommand modifiers (`sudo`, `env`, ...) stay.
//!   A finished `$(...)` or `<(...)` counts as one empty word.
//! - A backslash at the very end of the line (nothing left to escape) belongs
//!   to the word but adds no unquoted bytes; `map[len]` stays `typed.end`.
//! - `$'\xHH'` and `$'\NNN'` escapes of bytes `>= 0x80` that together form
//!   one UTF-8 character yield that character (zsh prints a character it
//!   cannot print as such a run). A lone or malformed byte yields the char
//!   `U+00HH`, not a raw byte.
//! - Reserved words (`if`, `then`, `do`, `!`, ...) are not modelled: the word
//!   after them is not at command position.
//! - A word broken up by `\` + newline is not joined; the continuation is
//!   dropped and the word goes on.
//!
//! fish's rules ([`Dialect::Fish`]) differ where fish does:
//! - No `$'...'`: `$` is a plain character, `'` opens a single quote.
//! - Inside single quotes `\\` and `\'` are escapes; inside double quotes
//!   `\\`, `\"` and `\$` (and `\` + newline). Unquoted, `\n`, `\t`, `\e`,
//!   `\xHH`, `\uXXXX`, `\cX` and the like are the characters they stand for,
//!   and any other escaped character is itself.
//! - `(cmd)` and `$(cmd)` are command substitutions anywhere outside quotes
//!   (a word glued to one is abandoned, as for zsh's `$(`); there is no
//!   subshell group, no backtick substitution (`` ` `` is a plain
//!   character) and no `<(`.
//! - `and`, `or`, `not`, `!`, `begin`, `end`, `if`, `while` and `else` at
//!   command position start a new command: the word after them is a
//!   command name. Leading `NAME=value` is an assignment, as in fish 3.1+.

use std::ops::Range;

use crate::guards::ShellKind;

/// The shell whose rules a line is read (and a completion quoted) by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialect {
    Zsh,
    /// Read as zsh's: the quotes, escapes and words this models are the
    /// same in bash.
    Bash,
    /// fish's own rules (see the module doc).
    Fish,
}

impl From<ShellKind> for Dialect {
    fn from(kind: ShellKind) -> Self {
        match kind {
            ShellKind::Zsh => Dialect::Zsh,
            ShellKind::Bash => Dialect::Bash,
            ShellKind::Fish => Dialect::Fish,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quote {
    None,
    Single,
    Double,
    Dollar, /* $'...' */
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Word {
    /// Byte range of the word in the line (`LBUFFER + RBUFFER`, not PREBUFFER), may extend right of the cursor.
    pub typed: Range<usize>,
    /// The word with quoting removed (what the shell would see).
    pub unquoted: String,
    /// `map[i]` = byte offset in the line of the typed byte that produced unquoted byte `i`
    /// (for an escape sequence, the byte where the sequence starts; quote marks produce nothing);
    /// `map[unquoted.len()]` = `typed.end` (one past the end).
    pub map: Vec<usize>,
    /// Unquoted byte index that corresponds to the cursor.
    pub cursor: usize,
    /// Quote state open at the cursor.
    pub quote: Quote,
    /// When `quote` is open: the byte offset in the line just past the mark
    /// that opened it (0 when PREBUFFER opened it).
    pub quote_open: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineContext {
    /// Unquoted words of the current simple command, before the current word.
    pub words_before: Vec<String>,
    /// May be empty (cursor after a space).
    pub current: Word,
    /// The current word is a command name.
    pub command_position: bool,
    /// Unquoted byte index inside `current` where the completion context is cut:
    /// just after the last unquoted `/` left of the cursor, or after the first `=`
    /// (when the word starts with `-` or has no `/` before that `=`), whichever
    /// is further right; 0 when there is neither.
    pub cut: usize,
    /// The rules the line was read by.
    pub dialect: Dialect,
}

/// Analyse the command line.
///
/// `prebuffer` (earlier lines of a multi-line command) only matters when it
/// ends inside an unfinished construct: an open quote, a trailing `\`, or an
/// open `$(` / `(` / backtick. Then the current line continues that command
/// (`words_before`, `command_position`, and an open quote at line start).
/// A word left half-typed inside an open quote at the end of `prebuffer` is
/// dropped; the line then starts a word of its own in that quote.
/// Otherwise the line starts a fresh command.
pub fn analyze(dialect: Dialect, prebuffer: &str, lbuffer: &str, rbuffer: &str) -> LineContext {
    let carry = if prebuffer.is_empty() {
        Carry::default()
    } else {
        let mut scanner = Scanner::new(dialect, prebuffer, None, Carry::default());
        scanner.run();
        scanner.into_carry()
    };
    let line = format!("{lbuffer}{rbuffer}");
    let mut scanner = Scanner::new(dialect, &line, Some(lbuffer.len()), carry);
    scanner.run();
    scanner.finish();
    scanner.into_context(dialect)
}

/// Typed byte range (in the line) to replace when inserting a completion whose replaced part starts at
/// unquoted index `from` of `current` and runs to the end of the word.
///
/// When no quote is open at the cursor the inserted text is quoted from
/// scratch, so `from == 0` replaces the whole typed word, quote marks included.
pub fn replace_range(word: &Word, from: usize) -> Range<usize> {
    let end = word.typed.end;
    if from == 0 && word.quote == Quote::None {
        return word.typed.start..end;
    }
    let start = word.map.get(from).copied().unwrap_or(end).min(end);
    start..end
}

/// `text` with quoting removed, read as if typed where `quote` is open (the
/// way a compsys `-Q` match is inserted). None when the shell would not read
/// it as one whole word there (an unquoted blank or operator in it).
pub fn unquote(dialect: Dialect, text: &str, quote: Quote) -> Option<String> {
    let open = match quote {
        Quote::None => "",
        Quote::Single => "'",
        Quote::Double => "\"",
        // fish has no `$'...'`.
        Quote::Dollar if dialect == Dialect::Fish => "'",
        Quote::Dollar => "$'",
    };
    let line = format!("{open}{text}");
    let ctx = analyze(dialect, "", &line, "");
    (ctx.words_before.is_empty() && ctx.current.typed == (0..line.len()))
        .then_some(ctx.current.unquoted)
}

/// fish's keywords after which a new command starts (at command position).
const FISH_SEPARATORS: [&str; 9] = [
    "and", "or", "not", "!", "begin", "end", "if", "while", "else",
];

/// Precommand modifiers whose following word is again a command name.
const PRECOMMANDS: [&str; 9] = [
    "sudo",
    "command",
    "builtin",
    "exec",
    "noglob",
    "nocorrect",
    "time",
    "nohup",
    "env",
];

#[derive(Clone, Default)]
struct Level {
    words: Vec<String>,
    after_redirect: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FrameKind {
    Group,
    Subst,
    Backtick,
}

struct Carry {
    level: Level,
    stack: Vec<(FrameKind, Level)>,
    quote: Quote,
}

impl Default for Carry {
    fn default() -> Self {
        Self {
            level: Level::default(),
            stack: Vec::new(),
            quote: Quote::None,
        }
    }
}

/// Scanner state at the moment the scan reaches the cursor.
struct Snapshot {
    words: Vec<String>,
    after_redirect: bool,
    cursor_in_word: Option<usize>,
    quote: Quote,
    quote_open: Option<usize>,
}

#[derive(Default)]
struct Pending {
    active: bool,
    quoted: bool,
    start: usize,
    unquoted: String,
    map: Vec<usize>,
}

struct Scanner<'a> {
    dialect: Dialect,
    text: &'a str,
    pos: usize,
    cursor: Option<usize>,
    level: Level,
    stack: Vec<(FrameKind, Level)>,
    quote: Quote,
    /// Just past the mark that opened `quote` (0 for a carried quote).
    quote_open: usize,
    pending: Pending,
    last_dollar: Option<usize>,
    continuation: bool,
    dangling: bool,
    snapshot: Option<Snapshot>,
    found: Option<Word>,
}

/// The byte a `\xHH` (one or two hex digits) or `\NNN` (one to three octal
/// digits, taken modulo 256) escape at the start of `s` stands for, and the
/// escape's length in bytes, as zsh reads them in `$'...'`.
fn byte_escape(s: &str) -> Option<(u8, usize)> {
    let digits = s.strip_prefix('\\')?;
    let (radix, skip, max) = match digits.as_bytes().first()? {
        b'x' => (16, 1, 2),
        b'0'..=b'7' => (8, 0, 3),
        _ => return None,
    };
    let run = digits[skip..]
        .bytes()
        .take(max)
        .take_while(|b| char::from(*b).is_digit(radix))
        .count();
    if run == 0 {
        return None;
    }
    let value = u32::from_str_radix(&digits[skip..skip + run], radix).ok()?;
    Some((u8::try_from(value & 0xff).ok()?, 1 + skip + run))
}

/// The length of the UTF-8 sequence a lead byte starts (1 for any other byte).
fn utf8_len(lead: u8) -> usize {
    match lead {
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => 1,
    }
}

impl<'a> Scanner<'a> {
    fn new(dialect: Dialect, text: &'a str, cursor: Option<usize>, carry: Carry) -> Self {
        let mut pending = Pending::default();
        if carry.quote != Quote::None {
            pending.active = true;
            pending.quoted = true;
        }
        Self {
            dialect,
            text,
            pos: 0,
            cursor,
            level: carry.level,
            stack: carry.stack,
            quote: carry.quote,
            quote_open: 0,
            pending,
            last_dollar: None,
            continuation: false,
            dangling: false,
            snapshot: None,
            found: None,
        }
    }

    fn peek(&self) -> Option<char> {
        self.text.get(self.pos..)?.chars().next()
    }

    /// Consume the next char. Takes the cursor snapshot when the cursor is at
    /// the position about to be read (also at end of text).
    fn bump(&mut self) -> Option<(usize, char)> {
        if self.cursor == Some(self.pos) && self.snapshot.is_none() {
            self.take_snapshot();
        }
        let c = self.peek()?;
        let at = self.pos;
        self.pos += c.len_utf8();
        Some((at, c))
    }

    fn take_snapshot(&mut self) {
        self.snapshot = Some(Snapshot {
            words: self.level.words.clone(),
            after_redirect: self.level.after_redirect,
            cursor_in_word: self.pending.active.then_some(self.pending.unquoted.len()),
            quote: self.quote,
            quote_open: (self.quote != Quote::None).then_some(self.quote_open),
        });
    }

    fn run(&mut self) {
        while let Some((i, c)) = self.bump() {
            self.step(i, c);
        }
    }

    /// Complete the word that runs to the end of the text.
    fn finish(&mut self) {
        self.end_word(self.text.len());
    }

    fn begin(&mut self, at: usize) {
        if !self.pending.active {
            self.pending.active = true;
            self.pending.start = at;
        }
    }

    fn push_char(&mut self, c: char, at: usize) {
        self.pending.unquoted.push(c);
        for _ in 0..c.len_utf8() {
            self.pending.map.push(at);
        }
    }

    fn step(&mut self, i: usize, c: char) {
        self.continuation = false;
        match self.quote {
            Quote::Single => self.step_single(i, c),
            Quote::Double => self.step_double(i, c),
            Quote::Dollar => self.step_dollar(i, c),
            Quote::None => self.step_plain(i, c),
        }
    }

    fn fish(&self) -> bool {
        self.dialect == Dialect::Fish
    }

    fn step_single(&mut self, i: usize, c: char) {
        if c == '\'' {
            self.quote = Quote::None;
        } else if c == '\\' && self.fish() && matches!(self.peek(), Some('\\' | '\'')) {
            // fish: `\\` and `\'` are escapes inside single quotes.
            if let Some((_, n)) = self.bump() {
                self.push_char(n, i);
            }
        } else {
            self.push_char(c, i);
        }
    }

    fn step_double(&mut self, i: usize, c: char) {
        let fish = self.fish();
        match c {
            '"' => self.quote = Quote::None,
            '\\' => match self.peek() {
                Some('\n') => {
                    self.bump();
                }
                Some(n @ ('\\' | '"' | '$')) => {
                    self.bump();
                    self.push_char(n, i);
                }
                // fish takes `` \` `` as a backslash and a backtick.
                Some('`') if !fish => {
                    self.bump();
                    self.push_char('`', i);
                }
                Some(_) => self.push_char('\\', i),
                None => self.dangling = true,
            },
            _ => self.push_char(c, i),
        }
    }

    fn step_dollar(&mut self, i: usize, c: char) {
        match c {
            '\'' => self.quote = Quote::None,
            '\\' => self.dollar_escape(i),
            _ => self.push_char(c, i),
        }
    }

    fn dollar_escape(&mut self, at: usize) {
        if let Some((byte, len)) = byte_escape(&self.text[at..]) {
            self.byte_escapes(at, byte, len);
            return;
        }
        let Some((_, n)) = self.bump() else {
            self.dangling = true;
            return;
        };
        let ch = match n {
            'n' => '\n',
            't' => '\t',
            '\\' | '\'' | '"' | '?' => n,
            'e' | 'E' => '\x1b',
            'a' => '\x07',
            'b' => '\x08',
            'f' => '\x0c',
            'r' => '\r',
            'v' => '\x0b',
            other => {
                self.push_char('\\', at);
                other
            }
        };
        self.push_char(ch, at);
    }

    /// An unquoted fish escape at `at` (the backslash; a character follows
    /// it): `\n` and the like, `\xHH` and `\NNN` (bytes, joined into a
    /// UTF-8 character as zsh's are), `\uXXXX`, `\UXXXXXXXX`, `\cX`; any
    /// other escaped character is itself.
    fn fish_escape(&mut self, at: usize) {
        if let Some((byte, len)) = byte_escape(&self.text[at..]) {
            self.byte_escapes(at, byte, len);
            return;
        }
        let Some((_, n)) = self.bump() else {
            self.dangling = true;
            return;
        };
        let ch = match n {
            'n' => '\n',
            't' => '\t',
            'r' => '\r',
            'e' => '\x1b',
            'a' => '\x07',
            'b' => '\x08',
            'f' => '\x0c',
            'v' => '\x0b',
            'u' | 'U' => {
                let max = if n == 'u' { 4 } else { 8 };
                let rest = &self.text[self.pos..];
                let run = rest
                    .bytes()
                    .take(max)
                    .take_while(u8::is_ascii_hexdigit)
                    .count();
                match u32::from_str_radix(&rest[..run], 16)
                    .ok()
                    .and_then(char::from_u32)
                {
                    Some(c) if run > 0 => {
                        for _ in 0..run {
                            self.bump();
                        }
                        c
                    }
                    _ => n,
                }
            }
            'c' => match self.peek() {
                Some(x) if x.is_ascii() && (x as u8) >= 0x40 => {
                    self.bump();
                    char::from(x as u8 & 0x1f)
                }
                _ => n,
            },
            other => other,
        };
        self.push_char(ch, at);
    }

    /// The `\xHH` or `\NNN` escape of `byte` at `at` (`len` bytes long). A
    /// byte `>= 0x80` that starts a run of such escapes spelling one UTF-8
    /// character takes the whole run.
    fn byte_escapes(&mut self, at: usize, byte: u8, len: usize) {
        let mut end = at + len;
        let mut ch = char::from(byte);
        if byte >= 0x80 {
            let mut bytes = vec![byte];
            let mut next = end;
            while bytes.len() < utf8_len(byte) {
                let Some((b, l)) = byte_escape(&self.text[next..]) else {
                    break;
                };
                bytes.push(b);
                next += l;
            }
            // A cursor inside the run keeps the escapes apart.
            let cursor_inside = self.cursor.is_some_and(|c| c > at && c < next);
            if !cursor_inside
                && let Some(c) = std::str::from_utf8(&bytes)
                    .ok()
                    .and_then(|text| text.chars().next())
            {
                ch = c;
                end = next;
            }
        }
        while self.pos < end {
            self.bump();
        }
        self.push_char(ch, at);
    }

    fn step_plain(&mut self, i: usize, c: char) {
        let fish = self.fish();
        match c {
            '\\' => match self.peek() {
                Some('\n') => {
                    self.bump();
                    self.continuation = true;
                }
                Some(_) if fish => {
                    self.begin(i);
                    self.fish_escape(i);
                }
                Some(n) => {
                    self.begin(i);
                    self.bump();
                    self.push_char(n, i);
                }
                None => {
                    self.begin(i);
                    self.dangling = true;
                }
            },
            '\'' => {
                self.begin(i);
                self.pending.quoted = true;
                self.quote = Quote::Single;
                self.quote_open = self.pos;
            }
            '"' => {
                self.begin(i);
                self.pending.quoted = true;
                self.quote = Quote::Double;
                self.quote_open = self.pos;
            }
            '$' => {
                self.begin(i);
                if self.peek() == Some('\'') && !fish {
                    self.bump();
                    self.pending.quoted = true;
                    self.quote = Quote::Dollar;
                    self.quote_open = self.pos;
                } else {
                    self.push_char('$', i);
                    self.last_dollar = Some(i);
                }
            }
            ' ' | '\t' => self.end_word(i),
            ';' | '|' | '\n' => {
                self.end_word(i);
                self.command_break();
            }
            '&' => {
                self.end_word(i);
                if self.peek() == Some('>') {
                    self.bump();
                    if self.peek() == Some('>') {
                        self.bump();
                    }
                    self.level.after_redirect = true;
                } else {
                    self.command_break();
                }
            }
            '<' | '>' => {
                if self.peek() == Some('(') && !fish {
                    self.end_word(i);
                    self.bump();
                    self.push_frame(FrameKind::Subst);
                } else {
                    self.redirect(i);
                }
            }
            '(' if fish => {
                // fish: a substitution wherever it is; a word glued to it
                // is abandoned.
                self.pending = Pending::default();
                self.push_frame(FrameKind::Subst);
            }
            '`' if fish => {
                self.begin(i);
                self.push_char(c, i);
            }
            '(' => {
                let subst = self.last_dollar.is_some_and(|d| d + 1 == i)
                    && self.pending.active
                    && self.pending.unquoted.ends_with('$');
                if subst {
                    // `$(`: the word it was glued to is abandoned.
                    self.pending = Pending::default();
                    self.push_frame(FrameKind::Subst);
                } else {
                    self.end_word(i);
                    self.push_frame(FrameKind::Group);
                }
            }
            ')' => {
                self.end_word(i);
                match self.stack.last() {
                    Some((FrameKind::Subst | FrameKind::Group, _)) => self.pop_frame(),
                    _ => self.command_break(),
                }
            }
            '`' => {
                self.end_word(i);
                if matches!(self.stack.last(), Some((FrameKind::Backtick, _))) {
                    self.pop_frame();
                } else {
                    self.push_frame(FrameKind::Backtick);
                }
            }
            _ => {
                self.begin(i);
                self.push_char(c, i);
            }
        }
    }

    /// A redirection operator (`<`, `>`, `>>`, `<<<`, `>&`, `>|`, ...).
    /// The word after it is its target, not a command word.
    fn redirect(&mut self, at: usize) {
        let fd = self.pending.active
            && !self.pending.quoted
            && !self.pending.unquoted.is_empty()
            && self.pending.unquoted.bytes().all(|b| b.is_ascii_digit());
        if fd {
            self.pending = Pending::default();
        } else {
            self.end_word(at);
        }
        for _ in 0..2 {
            if matches!(self.peek(), Some('<' | '>' | '&' | '|')) {
                self.bump();
            } else {
                break;
            }
        }
        self.level.after_redirect = true;
    }

    fn command_break(&mut self) {
        self.level = Level::default();
    }

    fn push_frame(&mut self, kind: FrameKind) {
        let outer = std::mem::take(&mut self.level);
        self.stack.push((kind, outer));
    }

    fn pop_frame(&mut self) {
        match self.stack.pop() {
            Some((FrameKind::Group, _)) | None => self.command_break(),
            Some((_, mut outer)) => {
                // The substitution stands in for one word of the outer command.
                if outer.after_redirect {
                    outer.after_redirect = false;
                } else {
                    outer.words.push(String::new());
                }
                self.level = outer;
            }
        }
    }

    fn end_word(&mut self, end: usize) {
        if !self.pending.active {
            return;
        }
        let pending = std::mem::take(&mut self.pending);
        self.complete(pending, end);
    }

    fn complete(&mut self, pending: Pending, end: usize) {
        let Pending {
            start,
            quoted,
            unquoted,
            mut map,
            ..
        } = pending;
        map.push(end);
        let covers = self.cursor.is_some_and(|c| start <= c && c <= end);
        if covers && self.found.is_none() {
            let (cursor, quote, quote_open) = match &self.snapshot {
                Some(s) => (s.cursor_in_word.unwrap_or(0), s.quote, s.quote_open),
                None => (0, Quote::None, None),
            };
            self.found = Some(Word {
                typed: start..end,
                cursor: cursor.min(unquoted.len()),
                unquoted: unquoted.clone(),
                map,
                quote,
                quote_open,
            });
        }
        if self.level.after_redirect {
            self.level.after_redirect = false;
        } else if unquoted.is_empty() && !quoted {
            // Only a dangling backslash: no word.
        } else if self.level.words.is_empty() && is_assignment(&unquoted) {
            // Leading `NAME=value`: not part of the command.
        } else if self.fish()
            && !quoted
            && self.level.words.is_empty()
            && FISH_SEPARATORS.contains(&unquoted.as_str())
        {
            // `and`, `not`, `begin`, ...: the next word is a command again.
            self.command_break();
        } else {
            self.level.words.push(unquoted);
        }
    }

    /// What PREBUFFER leaves for the next line, if it ends mid-command.
    fn into_carry(mut self) -> Carry {
        let unfinished = self.quote != Quote::None
            || self.continuation
            || self.dangling
            || !self.stack.is_empty();
        if !unfinished {
            return Carry::default();
        }
        // A word still open at the end is dropped, except that its open quote carries.
        if self.quote == Quote::None {
            self.finish();
        }
        Carry {
            level: std::mem::take(&mut self.level),
            stack: std::mem::take(&mut self.stack),
            quote: self.quote,
        }
    }

    fn into_context(self, dialect: Dialect) -> LineContext {
        let cursor = self.cursor.unwrap_or(0);
        let (words_before, after_redirect, quote, quote_open) = match self.snapshot {
            Some(s) => (s.words, s.after_redirect, s.quote, s.quote_open),
            None => {
                let open = (self.quote != Quote::None).then_some(self.quote_open);
                (
                    self.level.words,
                    self.level.after_redirect,
                    self.quote,
                    open,
                )
            }
        };
        let current = self.found.unwrap_or_else(|| Word {
            typed: cursor..cursor,
            unquoted: String::new(),
            map: vec![cursor],
            cursor: 0,
            quote,
            quote_open,
        });
        let prefix = current.unquoted.get(..current.cursor).unwrap_or("");
        let cut = cut_point(prefix);
        let command_position = is_command_position(&words_before, prefix, after_redirect);
        LineContext {
            words_before,
            current,
            command_position,
            cut,
            dialect,
        }
    }
}

/// See [`LineContext::cut`]; `prefix` is the current word left of the cursor.
fn cut_point(prefix: &str) -> usize {
    let slash = prefix.rfind('/').map(|i| i + 1);
    let eq = prefix
        .find('=')
        .filter(|&i| prefix.starts_with('-') || !prefix[..i].contains('/'))
        .map(|i| i + 1);
    slash.into_iter().chain(eq).max().unwrap_or(0)
}

/// `NAME=...` or `NAME+=...` with a valid parameter name.
fn is_assignment(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
    let name = name.strip_suffix('+').unwrap_or(name);
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Index after the options of a precommand modifier. May exceed
/// `words.len()` when the last option's argument is the word being typed.
fn skip_options(words: &[String], mut i: usize, takes_arg: &[&str]) -> usize {
    while let Some(w) = words.get(i) {
        let w = w.as_str();
        if w == "--" {
            return i + 1;
        }
        if is_assignment(w) {
            i += 1;
        } else if w.starts_with('-') {
            i += if takes_arg.contains(&w) { 2 } else { 1 };
        } else {
            break;
        }
    }
    i
}

/// Is the word being typed a command name? `words` are the words before it.
fn is_command_position(words: &[String], current: &str, after_redirect: bool) -> bool {
    if after_redirect || is_assignment(current) {
        return false;
    }
    let mut i = 0;
    let mut modified = false;
    while let Some(w) = words.get(i) {
        let w = w.as_str();
        if !PRECOMMANDS.contains(&w) {
            return false;
        }
        modified = true;
        let takes_arg: &[&str] = match w {
            "sudo" => &["-u", "-g"],
            "env" => &["-u"],
            _ => &[],
        };
        i = skip_options(words, i + 1, takes_arg);
        if i > words.len() {
            return false;
        }
    }
    !(modified && current.starts_with('-'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    struct Case {
        lbuffer: &'static str,
        rbuffer: &'static str,
        typed: Range<usize>,
        unquoted: &'static str,
        cursor: usize,
        quote: Quote,
        cut: usize,
        command_position: bool,
        words_before: &'static [&'static str],
    }

    /// What every dialect reads alike, and the context says which it was
    /// read by.
    #[test]
    fn dialects() {
        for dialect in [Dialect::Zsh, Dialect::Bash, Dialect::Fish] {
            let ctx = analyze(dialect, "", "cd My\\ Do", "");
            assert_eq!(ctx.dialect, dialect);
            let zsh = analyze(Dialect::Zsh, "", "cd My\\ Do", "");
            assert_eq!(ctx.current, zsh.current, "{dialect:?}");
            assert_eq!(ctx.words_before, zsh.words_before, "{dialect:?}");
            assert_eq!(
                unquote(dialect, "My\\ Documents", Quote::None).as_deref(),
                Some("My Documents")
            );
        }
        assert_eq!(Dialect::from(ShellKind::Zsh), Dialect::Zsh);
        assert_eq!(Dialect::from(ShellKind::Bash), Dialect::Bash);
        assert_eq!(Dialect::from(ShellKind::Fish), Dialect::Fish);
    }

    fn fish(lbuffer: &str) -> LineContext {
        analyze(Dialect::Fish, "", lbuffer, "")
    }

    /// The current word's unquoted text, and the words before it.
    fn fish_word(lbuffer: &str) -> (String, Vec<String>) {
        let ctx = fish(lbuffer);
        (ctx.current.unquoted, ctx.words_before)
    }

    #[test]
    fn fish_has_no_dollar_quotes() {
        // `$'` is `$` and a single quote.
        let ctx = fish("echo $'a\\nb");
        assert_eq!(ctx.current.unquoted, "$a\\nb");
        assert_eq!(ctx.current.quote, Quote::Single);
        let zsh = analyze(Dialect::Zsh, "", "echo $'a\\nb", "");
        assert_eq!(zsh.current.unquoted, "a\nb");
        assert_eq!(zsh.current.quote, Quote::Dollar);
    }

    #[test]
    fn fish_quote_escapes() {
        // Single quotes: `\\` and `\'` only.
        assert_eq!(fish_word("cat 'it\\'s a\\\\b \\n'").0, "it's a\\b \\n");
        assert_eq!(fish("cat 'it\\'s").current.quote, Quote::Single);
        // Double quotes: `\\`, `\"`, `\$`; a backtick is plain.
        assert_eq!(
            fish_word("echo \"a\\\"b\\$c\\\\d\\`e\\nf\"").0,
            "a\"b$c\\d\\`e\\nf"
        );
        // zsh takes `` \` `` inside double quotes.
        assert_eq!(
            analyze(Dialect::Zsh, "", "echo \"\\`\"", "")
                .current
                .unquoted,
            "`"
        );
    }

    #[test]
    fn fish_unquoted_escapes() {
        for (typed, unquoted) in [
            ("a\\ b", "a b"),
            ("a\\nb", "a\nb"),
            ("a\\tb", "a\tb"),
            ("\\e\\a\\r", "\x1b\x07\r"),
            ("\\x41\\x2d", "A-"),
            ("\\xc3\\xa9", "é"),
            ("\\101", "A"),
            ("\\u00e9\\U0001F642", "é🙂"),
            ("\\cA\\ca", "\x01\x01"),
            ("\\q\\#\\~\\%\\[", "q#~%["),
            ("\\u", "u"),
        ] {
            assert_eq!(fish_word(&format!("cat {typed}")).0, unquoted, "{typed:?}");
        }
        // The typed bytes map back to where each unquoted one came from.
        let ctx = fish("cat a\\nb");
        assert_eq!(ctx.current.map, vec![4, 5, 7, 8]);
    }

    #[test]
    fn fish_substitutions() {
        // `(cmd)` and `$(cmd)`: a command at their start.
        for line in ["echo (ech", "echo $(ech", "echo foo(ech", "set x (git ch"] {
            let ctx = fish(line);
            assert!(ctx.command_position || line.contains("git"), "{line:?}");
        }
        assert_eq!(fish_word("echo (git ch").1, ["git"]);
        // A finished one is one word of the command around it.
        assert_eq!(fish_word("echo (pwd) x").1, ["echo", ""]);
        // A backtick is a plain character; there is no subshell group.
        assert_eq!(fish_word("echo a`b").0, "a`b");
        assert!(!fish("echo a`b").command_position);
        // `<(` is a redirect and a substitution: its target.
        assert_eq!(fish_word("cat <(ls) x").1, ["cat"]);
        // zsh: `(` alone is a group, a backtick a substitution.
        assert!(analyze(Dialect::Zsh, "", "echo `ech", "").command_position);
    }

    #[test]
    fn fish_keywords_start_commands() {
        for line in [
            "true; and git ch",
            "false; or git ch",
            "not git ch",
            "! git ch",
            "begin; git ch",
            "begin git ch",
            "if git ch",
            "while git ch",
            "else git ch",
            "end; git ch",
            "true && git ch",
        ] {
            let ctx = fish(line);
            assert_eq!(ctx.words_before, ["git"], "{line:?}");
            assert!(!ctx.command_position, "{line:?}");
        }
        assert!(fish("and gi").command_position);
        assert!(fish("not ").command_position);
        // Only at command position, and not quoted.
        assert_eq!(fish_word("echo and gi").1, ["echo", "and"]);
        assert_eq!(fish_word("'and' gi").1, ["and"]);
        // zsh has no such keywords.
        assert_eq!(
            analyze(Dialect::Zsh, "", "not git ch", "").words_before,
            ["not", "git"]
        );
    }

    #[test]
    fn fish_takes_leading_assignments() {
        // fish 3.1+ runs `FOO=1 cmd` with FOO set, as zsh does.
        let ctx = fish("FOO=1 BAR=2 git ch");
        assert_eq!(ctx.words_before, ["git"]);
        assert!(fish("FOO=1 gi").command_position);
    }

    #[test]
    fn fish_unquote() {
        assert_eq!(
            unquote(Dialect::Fish, "it\\'s", Quote::Single).as_deref(),
            Some("it's")
        );
        assert_eq!(
            unquote(Dialect::Fish, "it\\'s", Quote::Dollar).as_deref(),
            Some("it's")
        );
        assert_eq!(
            unquote(Dialect::Fish, "a\\ b", Quote::None).as_deref(),
            Some("a b")
        );
        assert_eq!(unquote(Dialect::Fish, "a b", Quote::None), None);
    }

    fn check(case: &Case) {
        let ctx = analyze(Dialect::Zsh, "", case.lbuffer, case.rbuffer);
        let label = format!("{:?} | {:?}", case.lbuffer, case.rbuffer);
        assert_eq!(ctx.current.typed, case.typed, "typed: {label}");
        assert_eq!(ctx.current.unquoted, case.unquoted, "unquoted: {label}");
        assert_eq!(ctx.current.cursor, case.cursor, "cursor: {label}");
        assert_eq!(ctx.current.quote, case.quote, "quote: {label}");
        assert_eq!(ctx.cut, case.cut, "cut: {label}");
        assert_eq!(
            ctx.command_position, case.command_position,
            "command_position: {label}"
        );
        assert_eq!(ctx.words_before, case.words_before, "words_before: {label}");
    }

    fn simple(
        line: &'static str,
        typed: Range<usize>,
        unquoted: &'static str,
        cursor: usize,
        cut: usize,
        command_position: bool,
        words_before: &'static [&'static str],
    ) -> Case {
        Case {
            lbuffer: line,
            rbuffer: "",
            typed,
            unquoted,
            cursor,
            quote: Quote::None,
            cut,
            command_position,
            words_before,
        }
    }

    #[test]
    fn table() {
        let cases = [
            simple("", 0..0, "", 0, 0, true, &[]),
            simple("l", 0..1, "l", 1, 0, true, &[]),
            simple("ls ", 3..3, "", 0, 0, false, &["ls"]),
            simple("ls -l", 3..5, "-l", 2, 0, false, &["ls"]),
            simple("git ch", 4..6, "ch", 2, 0, false, &["git"]),
            simple(
                "git checkout origin/ma",
                13..22,
                "origin/ma",
                9,
                7,
                false,
                &["git", "checkout"],
            ),
            simple("ls --color=al", 3..13, "--color=al", 10, 8, false, &["ls"]),
            simple("cd ~/De", 3..7, "~/De", 4, 2, false, &["cd"]),
            simple("echo $HO", 5..8, "$HO", 3, 0, false, &["echo"]),
            simple("cd My\\ Do", 3..9, "My Do", 5, 0, false, &["cd"]),
            Case {
                quote: Quote::Single,
                ..simple("cd 'quoted pa", 3..13, "quoted pa", 9, 0, false, &["cd"])
            },
            Case {
                quote: Quote::Double,
                ..simple("echo \"a b", 5..9, "a b", 3, 0, false, &["echo"])
            },
            simple("FOO=1 make te", 11..13, "te", 2, 0, false, &["make"]),
            simple(
                "sudo -u root vi",
                13..15,
                "vi",
                2,
                0,
                true,
                &["sudo", "-u", "root"],
            ),
            simple("ls | gr", 5..7, "gr", 2, 0, true, &[]),
            simple("cat < fi", 6..8, "fi", 2, 0, false, &["cat"]),
        ];
        for case in &cases {
            check(case);
        }
    }

    #[test]
    fn cursor_in_the_middle_of_a_word() {
        check(&Case {
            lbuffer: "git chec",
            rbuffer: "kout main",
            typed: 4..12,
            unquoted: "checkout",
            cursor: 4,
            quote: Quote::None,
            cut: 0,
            command_position: false,
            words_before: &["git"],
        });
    }

    #[test]
    fn cursor_right_before_a_word() {
        check(&Case {
            lbuffer: "git ",
            rbuffer: "checkout",
            typed: 4..12,
            unquoted: "checkout",
            cursor: 0,
            quote: Quote::None,
            cut: 0,
            command_position: false,
            words_before: &["git"],
        });
    }

    #[test]
    fn cursor_inside_a_closed_quote_keeps_the_quote_open_state() {
        check(&Case {
            lbuffer: "cd 'my di",
            rbuffer: "r' x",
            typed: 3..11,
            unquoted: "my dir",
            cursor: 5,
            quote: Quote::Single,
            cut: 0,
            command_position: false,
            words_before: &["cd"],
        });
        // Quote closed before the cursor: nothing open.
        check(&Case {
            lbuffer: "cd 'my dir' ",
            rbuffer: "",
            typed: 12..12,
            unquoted: "",
            cursor: 0,
            quote: Quote::None,
            cut: 0,
            command_position: false,
            words_before: &["cd", "my dir"],
        });
    }

    #[test]
    fn assignments_and_precommands() {
        let cases = [
            simple("FOO=1 ma", 6..8, "ma", 2, 0, true, &[]),
            simple("FOO=ba", 0..6, "FOO=ba", 6, 4, false, &[]),
            simple("A=1 B=2 make te", 13..15, "te", 2, 0, false, &["make"]),
            simple("sudo gi", 5..7, "gi", 2, 0, true, &["sudo"]),
            simple("sudo -E gi", 8..10, "gi", 2, 0, true, &["sudo", "-E"]),
            simple("sudo -u ro", 8..10, "ro", 2, 0, false, &["sudo", "-u"]),
            simple("sudo -", 5..6, "-", 1, 0, false, &["sudo"]),
            simple("sudo git ch", 9..11, "ch", 2, 0, false, &["sudo", "git"]),
            simple("env FOO=1 gi", 10..12, "gi", 2, 0, true, &["env", "FOO=1"]),
            simple("env -u X gi", 9..11, "gi", 2, 0, true, &["env", "-u", "X"]),
            simple("command gi", 8..10, "gi", 2, 0, true, &["command"]),
            simple(
                "time sudo nohup gi",
                16..18,
                "gi",
                2,
                0,
                true,
                &["time", "sudo", "nohup"],
            ),
        ];
        for case in &cases {
            check(case);
        }
    }

    #[test]
    fn command_boundaries() {
        let cases = [
            simple("a; b; gr", 6..8, "gr", 2, 0, true, &[]),
            simple("a && gr", 5..7, "gr", 2, 0, true, &[]),
            simple("a || gr", 5..7, "gr", 2, 0, true, &[]),
            simple("a & gr", 4..6, "gr", 2, 0, true, &[]),
            simple("a |& gr", 5..7, "gr", 2, 0, true, &[]),
            simple("(gr", 1..3, "gr", 2, 0, true, &[]),
            simple("echo $(git ch", 11..13, "ch", 2, 0, false, &["git"]),
            simple("echo $(gi", 7..9, "gi", 2, 0, true, &[]),
            simple("echo `gi", 6..8, "gi", 2, 0, true, &[]),
            simple("echo `ls` ar", 10..12, "ar", 2, 0, false, &["echo", ""]),
            simple("echo $(ls) ar", 11..13, "ar", 2, 0, false, &["echo", ""]),
            simple("diff <(ls) fi", 11..13, "fi", 2, 0, false, &["diff", ""]),
            simple("echo $(ls) | gr", 13..15, "gr", 2, 0, true, &[]),
            simple("(cd x; ls) && gr", 14..16, "gr", 2, 0, true, &[]),
        ];
        for case in &cases {
            check(case);
        }
        assert_eq!(
            analyze(Dialect::Zsh, "", "a\ngr", "").words_before,
            Vec::<String>::new()
        );
        assert!(analyze(Dialect::Zsh, "", "a\ngr", "").command_position);
    }

    #[test]
    fn redirections() {
        let cases = [
            simple("cat 2> fi", 7..9, "fi", 2, 0, false, &["cat"]),
            simple("cat >fi", 5..7, "fi", 2, 0, false, &["cat"]),
            simple("cat >> fi", 7..9, "fi", 2, 0, false, &["cat"]),
            simple("cat &> fi", 7..9, "fi", 2, 0, false, &["cat"]),
            simple("cat <<< fi", 8..10, "fi", 2, 0, false, &["cat"]),
            simple("cat < in ar", 9..11, "ar", 2, 0, false, &["cat"]),
            simple("cat x > o; gr", 11..13, "gr", 2, 0, true, &[]),
            simple("echo 12 > o", 10..11, "o", 1, 0, false, &["echo", "12"]),
        ];
        for case in &cases {
            check(case);
        }
    }

    #[test]
    fn quoting_forms() {
        let cases = [
            simple("echo $'a\\tb\\x41'", 5..16, "a\tbA", 4, 0, false, &["echo"]),
            simple("echo 'a'\"b\"c", 5..12, "abc", 3, 0, false, &["echo"]),
            simple("echo foo\\ bar", 5..13, "foo bar", 7, 0, false, &["echo"]),
            simple("echo \\$HOME", 5..11, "$HOME", 5, 0, false, &["echo"]),
            simple("echo ''", 5..7, "", 0, 0, false, &["echo"]),
            Case {
                quote: Quote::Single,
                ..simple("echo '", 5..6, "", 0, 0, false, &["echo"])
            },
            simple("echo é", 5..7, "é", 2, 0, false, &["echo"]),
        ];
        for case in &cases {
            check(case);
        }
    }

    #[test]
    fn unterminated_double_and_dollar_quotes() {
        let ctx = analyze(Dialect::Zsh, "", "echo \"a\\\"b", "");
        assert_eq!(ctx.current.unquoted, "a\"b");
        assert_eq!(ctx.current.quote, Quote::Double);
        let ctx = analyze(Dialect::Zsh, "", "echo $'a\\tb\\x41", "");
        assert_eq!(ctx.current.unquoted, "a\tbA");
        assert_eq!(ctx.current.quote, Quote::Dollar);
    }

    #[test]
    fn dollar_quote_escapes() {
        let ctx = analyze(Dialect::Zsh, "", r"echo $'a\nb\e\\\'c\q'", "");
        assert_eq!(ctx.current.unquoted, "a\nb\x1b\\'c\\q");
        assert_eq!(ctx.current.quote, Quote::None);
    }

    #[test]
    fn dollar_quote_byte_runs_are_one_character() {
        // How zsh quotes a name with U+200D: `addunprintable`, byte by byte.
        let zwj = analyze(Dialect::Zsh, "", r"ls a$'\342\200\215'b", "");
        assert_eq!(zwj.current.unquoted, "a\u{200d}b");
        // The same bytes in hex, and mixed with octal.
        let hex = analyze(Dialect::Zsh, "", r"ls $'\xe2\x80\x8d'", "");
        assert_eq!(hex.current.unquoted, "\u{200d}");
        let mixed = analyze(Dialect::Zsh, "", r"ls $'\xe2\200\x8d'", "");
        assert_eq!(mixed.current.unquoted, "\u{200d}");
        // Two, three and four byte characters.
        let two = analyze(Dialect::Zsh, "", r"ls $'\302\255'", "");
        assert_eq!(two.current.unquoted, "\u{ad}");
        let four = analyze(Dialect::Zsh, "", r"ls $'\360\237\221\215'", "");
        assert_eq!(four.current.unquoted, "\u{1f44d}");
        // Short octal escapes are one to three digits.
        let short = analyze(Dialect::Zsh, "", r"ls $'\1\0012'", "");
        assert_eq!(short.current.unquoted, "\u{1}\u{1}2");
        // A lone or malformed byte stays the char U+00HH.
        let lone = analyze(Dialect::Zsh, "", r"ls $'\377x'", "");
        assert_eq!(lone.current.unquoted, "\u{ff}x");
        let cut = analyze(Dialect::Zsh, "", r"ls $'\342\200'", "");
        assert_eq!(cut.current.unquoted, "\u{e2}\u{80}");
        // Every typed byte of a character maps to where its run starts.
        assert_eq!(zwj.current.map[1..4], [zwj.current.map[1]; 3]);
    }

    #[test]
    fn a_cursor_inside_a_byte_run_keeps_the_escapes_apart() {
        let ctx = analyze(Dialect::Zsh, "", r"ls $'\342\200", r"\215'");
        assert_eq!(ctx.current.unquoted, "\u{e2}\u{80}\u{8d}");
        let ctx = analyze(Dialect::Zsh, "", r"ls $'\342", r"\200\215'");
        assert_eq!(ctx.current.unquoted, "\u{e2}\u{80}\u{8d}");
        // At either end of the run it is one character.
        let ctx = analyze(Dialect::Zsh, "", r"ls $'\342\200\215", "'");
        assert_eq!(ctx.current.unquoted, "\u{200d}");
        let ctx = analyze(Dialect::Zsh, "", r"ls $'", r"\342\200\215'");
        assert_eq!(ctx.current.unquoted, "\u{200d}");
    }

    #[test]
    fn dangling_backslash_belongs_to_the_word() {
        let ctx = analyze(Dialect::Zsh, "", "cd foo/\\", "");
        assert_eq!(ctx.current.typed, 3..8);
        assert_eq!(ctx.current.unquoted, "foo/");
        assert_eq!(ctx.words_before, ["cd"]);
        // Replacing from the start takes the backslash with it.
        assert_eq!(replace_range(&ctx.current, 0), 3..8);
        let ctx = analyze(Dialect::Zsh, "", "cd \\", "");
        assert_eq!(ctx.current.typed, 3..4);
        assert_eq!(ctx.current.unquoted, "");
        assert_eq!(replace_range(&ctx.current, 0), 3..4);
    }

    #[test]
    fn cut_rules() {
        assert_eq!(cut_point(""), 0);
        assert_eq!(cut_point("abc"), 0);
        assert_eq!(cut_point("a/b/c"), 4);
        assert_eq!(cut_point("--opt=va"), 6);
        assert_eq!(cut_point("--file=/usr/bi"), 12);
        assert_eq!(cut_point("a=b/c"), 4);
        assert_eq!(cut_point("x/y=z"), 2);
        assert_eq!(cut_point("-x=a/b"), 5);
    }

    #[test]
    fn cut_uses_only_text_left_of_the_cursor() {
        let ctx = analyze(Dialect::Zsh, "", "cat dir/fi", "le/more");
        assert_eq!(ctx.current.unquoted, "dir/file/more");
        assert_eq!(ctx.current.cursor, 6);
        assert_eq!(ctx.cut, 4);
    }

    #[test]
    fn prebuffer_continues_only_an_unfinished_command() {
        // Finished lines: fresh start.
        let ctx = analyze(Dialect::Zsh, "ls\n", "gr", "");
        assert!(ctx.command_position);
        assert!(ctx.words_before.is_empty());
        let ctx = analyze(Dialect::Zsh, "echo hi;\n", "gr", "");
        assert!(ctx.command_position);
        let ctx = analyze(Dialect::Zsh, "echo hi |\n", "gr", "");
        assert!(ctx.command_position);

        // Trailing backslash: the command goes on.
        let ctx = analyze(Dialect::Zsh, "ls \\\n", "-l", "");
        assert!(!ctx.command_position);
        assert_eq!(ctx.words_before, ["ls"]);
        assert_eq!(ctx.current.typed, 0..2);
        let ctx = analyze(Dialect::Zsh, "git \\", "ch", "");
        assert_eq!(ctx.words_before, ["git"]);

        // Open quote: the line starts inside it.
        let ctx = analyze(Dialect::Zsh, "echo 'abc\n", "def", "");
        assert_eq!(ctx.words_before, ["echo"]);
        assert_eq!(ctx.current.typed, 0..3);
        assert_eq!(ctx.current.unquoted, "def");
        assert_eq!(ctx.current.quote, Quote::Single);
        assert!(!ctx.command_position);
        // ... and closing it returns to normal words.
        let ctx = analyze(Dialect::Zsh, "echo 'abc\n", "def' gh", "");
        assert_eq!(ctx.words_before, ["echo", "def"]);
        assert_eq!(ctx.current.quote, Quote::None);

        // Open command substitution.
        let ctx = analyze(Dialect::Zsh, "echo $(\n", "gi", "");
        assert!(ctx.command_position);
        let ctx = analyze(Dialect::Zsh, "echo $(ls\n", "gi", "");
        assert!(ctx.command_position);
        assert!(ctx.words_before.is_empty());
    }

    #[test]
    fn replace_range_table() {
        let range = |l: &str, r: &str, from_cut: bool| {
            let ctx = analyze(Dialect::Zsh, "", l, r);
            let from = if from_cut { ctx.cut } else { 0 };
            replace_range(&ctx.current, from)
        };
        // Whole typed word including the backslash escape.
        assert_eq!(range("cd My\\ Do", "", false), 3..9);
        // Only the part after the cut.
        assert_eq!(range("git checkout origin/ma", "", true), 20..22);
        assert_eq!(range("ls --color=al", "", true), 11..13);
        // After the opening quote.
        assert_eq!(range("cd 'quoted pa", "", false), 4..13);
        // Mid-word cursor: to the end of the word.
        assert_eq!(range("git chec", "kout main", false), 4..12);
        assert_eq!(range("cat dir/fi", "le more", true), 8..12);
        // Empty word.
        assert_eq!(range("ls ", "", false), 3..3);
        // Cut in the middle of an escape-laden word.
        assert_eq!(range("cd My\\ Dir/su", "", true), 11..13);
        // The escape sequence is replaced from its backslash.
        let ctx = analyze(Dialect::Zsh, "", "cd My\\ Do", "");
        assert_eq!(replace_range(&ctx.current, 2), 5..9);
        // Past the end clamps.
        assert_eq!(replace_range(&ctx.current, 99), 9..9);
        // Closed quotes are replaced with the word when none is open.
        let ctx = analyze(Dialect::Zsh, "", "cd 'a b'", "");
        assert_eq!(ctx.current.quote, Quote::None);
        assert_eq!(replace_range(&ctx.current, 0), 3..8);
        // Unterminated double quote.
        let ctx = analyze(Dialect::Zsh, "", "echo \"a b", "");
        assert_eq!(replace_range(&ctx.current, 0), 6..9);
        // Just-opened quote: empty replace range after the mark.
        let ctx = analyze(Dialect::Zsh, "", "cd '", "");
        assert_eq!(ctx.current.quote, Quote::Single);
        assert_eq!(replace_range(&ctx.current, 0), 4..4);
    }

    #[test]
    fn unquote_reads_text_in_the_open_quote() {
        let none = |t: &str| unquote(Dialect::Zsh, t, Quote::None);
        assert_eq!(none("My\\ Documents").as_deref(), Some("My Documents"));
        assert_eq!(none("plain").as_deref(), Some("plain"));
        assert_eq!(none("it\\'s").as_deref(), Some("it's"));
        assert_eq!(none("").as_deref(), Some(""));
        // Not one word unquoted.
        assert_eq!(none("My Documents"), None);
        assert_eq!(none("a;b"), None);
        // Inside an open quote, the text continues that quote.
        assert_eq!(
            unquote(Dialect::Zsh, "My Documents", Quote::Single).as_deref(),
            Some("My Documents")
        );
        assert_eq!(
            unquote(Dialect::Zsh, "a \\\"b", Quote::Double).as_deref(),
            Some("a \"b")
        );
        assert_eq!(
            unquote(Dialect::Zsh, "a\\tb", Quote::Dollar).as_deref(),
            Some("a\tb")
        );
    }

    #[test]
    fn quote_open_is_just_past_the_opening_mark() {
        let open = |l: &str, r: &str| analyze(Dialect::Zsh, "", l, r).current.quote_open;
        assert_eq!(open("cd '", ""), Some(4));
        assert_eq!(open("cd 'fix", ""), Some(4));
        assert_eq!(open("echo \"a b", ""), Some(6));
        assert_eq!(open("echo $'a", ""), Some(7));
        // The quote open at the cursor, not the first one in the word.
        assert_eq!(open("cd 'it'\\''s", ""), Some(10));
        assert_eq!(open("cd ab'cd", ""), Some(6));
        assert_eq!(open("cd 'my di", "r' x"), Some(4));
        // None open.
        assert_eq!(open("cd 'a b'", ""), None);
        assert_eq!(open("cd fix", ""), None);
        assert_eq!(open("cd ", ""), None);
        // Opened in PREBUFFER.
        assert_eq!(
            analyze(Dialect::Zsh, "echo 'abc\n", "def", "")
                .current
                .quote_open,
            Some(0)
        );
    }

    #[test]
    fn map_points_at_the_typed_construct() {
        let ctx = analyze(Dialect::Zsh, "", "x 'a b'\\ c", "");
        let w = &ctx.current;
        assert_eq!(w.unquoted, "a b c");
        assert_eq!(w.typed, 2..10);
        assert_eq!(w.map, [3, 4, 5, 7, 9, 10]);
    }

    proptest! {
        #[test]
        fn analyze_keeps_its_invariants(
            pre in "[ -~\\n\\t\\\\'\"$`(){}|;&<>=/~éü✅]{0,24}",
            left in "[ -~\\n\\t\\\\'\"$`(){}|;&<>=/~éü✅]{0,40}",
            right in "[ -~\\n\\t\\\\'\"$`(){}|;&<>=/~éü✅]{0,40}",
        ) {
            let ctx = analyze(Dialect::Zsh, &pre, &left, &right);
            let line = format!("{left}{right}");
            let w = &ctx.current;
            prop_assert!(w.typed.start <= left.len() && left.len() <= w.typed.end);
            prop_assert!(w.typed.end <= line.len());
            prop_assert!(line.is_char_boundary(w.typed.start));
            prop_assert!(line.is_char_boundary(w.typed.end));
            prop_assert_eq!(w.map.len(), w.unquoted.len() + 1);
            prop_assert_eq!(w.map.last().copied(), Some(w.typed.end));
            prop_assert!(w.map.windows(2).all(|p| p[0] <= p[1]));
            prop_assert!(w.map.iter().all(|&o| w.typed.start <= o && o <= w.typed.end));
            prop_assert!(w.cursor <= w.unquoted.len());
            prop_assert!(w.unquoted.is_char_boundary(w.cursor));
            prop_assert!(ctx.cut <= w.cursor);
            prop_assert_eq!(w.quote_open.is_some(), w.quote != Quote::None);
            prop_assert!(w.quote_open.is_none_or(|o| o <= left.len() && line.is_char_boundary(o)));
            for from in 0..=w.unquoted.len() + 1 {
                let r = replace_range(w, from);
                prop_assert!(r.start <= r.end && r.end == w.typed.end);
                prop_assert!(line.is_char_boundary(r.start));
            }
        }

        #[test]
        fn analyze_never_panics_on_arbitrary_text(
            pre in "\\PC{0,20}", left in "\\PC{0,30}", right in "\\PC{0,30}",
        ) {
            let _ = analyze(Dialect::Zsh, &pre, &left, &right);
        }
    }
}
