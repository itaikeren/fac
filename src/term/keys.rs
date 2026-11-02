//! Finds the popup's keys in the user's bytes (after the input tokenizer has
//! taken the terminal's replies out) and follows bracketed pastes, so that
//! nothing inside a paste is ever taken for a key. Bytes are never
//! re-encoded: every token is a slice of the input.

use memchr::{memchr, memchr3};

use crate::complete::selection::Key;

const ESC: u8 = 0x1b;
const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Token {
    /// Bytes to forward as they are.
    Bytes,
    /// A popup key (only while intercepting).
    Key(Key),
    /// `ESC [ 200 ~` while intercepting: a paste begins. Its bytes are
    /// forwarded like the rest of the paste.
    PasteStart,
    /// The description popout's key (only while intercepting).
    Popout,
    /// The release event (kitty keyboard protocol) of a key the popup
    /// takes; the popup's own, so it is not forwarded.
    Release,
}

#[derive(Default)]
pub struct KeyDecoder {
    /// Inside a bracketed paste.
    paste: bool,
    /// How much of the paste marker being looked for (the start outside a
    /// paste, the end inside) the previous bytes ended with.
    matched: usize,
    /// The byte of the description popout's key, when it has one.
    popout: Option<u8>,
}

impl KeyDecoder {
    /// A decoder that also finds the popout's key, `byte` (a control byte).
    pub fn with_popout(byte: Option<u8>) -> Self {
        Self {
            popout: byte,
            ..Self::default()
        }
    }

    #[cfg(test)]
    fn in_paste(&self) -> bool {
        self.paste
    }

    /// The next token at the start of `data` (not empty) and its length.
    /// `intercept`: the popup is open, so its keys are tokens of their own.
    /// Outside a paste with `intercept` off, and inside a paste always, the
    /// bytes are passed on whole (up to and including a paste marker).
    pub fn next(&mut self, data: &[u8], intercept: bool) -> (Token, usize) {
        if self.paste {
            return match self.find_marker(data, PASTE_END) {
                Some(end) => {
                    self.paste = false;
                    (Token::Bytes, end)
                }
                None => (Token::Bytes, data.len()),
            };
        }
        if !intercept {
            return match self.find_marker(data, PASTE_START) {
                Some(end) => {
                    self.paste = true;
                    (Token::Bytes, end)
                }
                None => (Token::Bytes, data.len()),
            };
        }
        if self.matched > 0 {
            // A paste start split from an earlier read that was not
            // intercepted: finish it before looking for keys.
            let rest = PASTE_START.len() - self.matched;
            if let Some(end) = self.find_marker(&data[..rest.min(data.len())], PASTE_START) {
                self.paste = true;
                return (Token::PasteStart, end);
            }
            if data.len() < rest && self.matched > 0 {
                return (Token::Bytes, data.len());
            }
            self.matched = 0;
        }
        let key = memchr3(b'\t', b'\r', ESC, data);
        let popout = self.popout.and_then(|byte| memchr(byte, data));
        let Some(at) = key.into_iter().chain(popout).min() else {
            return (Token::Bytes, data.len());
        };
        if at > 0 {
            return (Token::Bytes, at);
        }
        match data[0] {
            b'\t' => (Token::Key(Key::Tab), 1),
            b'\r' => (Token::Key(Key::Enter), 1),
            byte if Some(byte) == self.popout => (Token::Popout, 1),
            _ => self.escape(data),
        }
    }

    /// `data` starts with ESC.
    fn escape(&mut self, data: &[u8]) -> (Token, usize) {
        if data.len() == 1 {
            return (Token::Key(Key::Esc), 1);
        }
        if data.starts_with(PASTE_START) {
            self.paste = true;
            return (Token::PasteStart, PASTE_START.len());
        }
        let key = match &data[1..data.len().min(3)] {
            b"[Z" => Some(Key::ShiftTab),
            b"[A" | b"OA" => Some(Key::Up),
            b"[B" | b"OB" => Some(Key::Down),
            _ => None,
        };
        if let Some(key) = key {
            return (Token::Key(key), 3);
        }
        if let Some(found) = extended_key(data) {
            return found;
        }
        (Token::Bytes, sequence_len(data))
    }

    /// Where `marker` ends in `data`, continuing a match the previous bytes
    /// ended with.
    fn find_marker(&mut self, data: &[u8], marker: &[u8]) -> Option<usize> {
        let mut i = 0;
        while i < data.len() {
            if self.matched == 0 {
                i += memchr(ESC, &data[i..])?;
            }
            let byte = data[i];
            i += 1;
            if byte == marker[self.matched] {
                self.matched += 1;
                if self.matched == marker.len() {
                    self.matched = 0;
                    return Some(i);
                }
            } else {
                self.matched = usize::from(byte == ESC);
            }
        }
        None
    }
}

/// The popup keys in the encodings a program can switch the terminal to: the
/// kitty keyboard protocol and xterm's modifyOtherKeys. `data` starts with
/// ESC. None for every other sequence, which then passes whole.
fn extended_key(data: &[u8]) -> Option<(Token, usize)> {
    let body = data.strip_prefix(b"\x1b[")?;
    let end = body.iter().position(|byte| (0x40..=0x7e).contains(byte))?;
    let params = &body[..end];
    let token = match body[end] {
        b'u' => kitty_key(params)?,
        b'A' => kitty_arrow(params, Key::Up)?,
        b'B' => kitty_arrow(params, Key::Down)?,
        // xterm's `CSI 27 ; modifier ; code ~` (modifyOtherKeys): only
        // Shift-Tab is a popup key in this form, as a plain Tab, Enter and
        // Esc are sent as their own bytes.
        b'~' if params == b"27;2;9" => Token::Key(Key::ShiftTab),
        _ => return None,
    };
    Some((token, end + 3))
}

/// The kitty keyboard protocol's `CSI code[:alternates] ; modifiers[:event]
/// u` (https://sw.kovidgoyal.net/kitty/keyboard-protocol/, "An overview",
/// "Modifiers" and "Event types"). Only the key code and the modifier field
/// are read (the optional text field is not sent for these keys):
/// - code 9 is Tab, or Shift-Tab with the shift modifier; 13 is Enter; 27 is
///   Esc;
/// - the modifiers field is `1 + bits`, and an absent one is 1 (none). The
///   lock modifiers (caps 64, num 128) are ignored: with "report all keys"
///   the terminal sets them on every key while the lock is on;
/// - the event type is 1 (press, the default), 2 (repeat) or 3 (release). A
///   release is the popup's own and is dropped.
///
/// Any other key code or modifier, or anything unparsable, is not a popup
/// key.
fn kitty_key(params: &[u8]) -> Option<Token> {
    let fields = kitty_fields(params)?;
    let key = match (fields.code, fields.modifiers) {
        (9, Some(0) | None) => Key::Tab,
        (9, Some(1)) => Key::ShiftTab,
        (13, Some(0) | None) => Key::Enter,
        (27, Some(0) | None) => Key::Esc,
        _ => return None,
    };
    kitty_event(key, fields.event)
}

/// An arrow in the kitty keyboard protocol's legacy form with an event
/// type, `CSI 1 ; modifiers[:event] A` (or `B`; the spec's "Legacy
/// functional keys"), read as `kitty_key` reads its fields: with no
/// modifier (the field is 1) a press or repeat is the arrow `key`, and a
/// release is the popup's own. Without the modifiers field, or with
/// alternate key codes (functional keys have none), it is not a key.
fn kitty_arrow(params: &[u8], key: Key) -> Option<Token> {
    if !params.starts_with(b"1;") {
        return None;
    }
    let fields = kitty_fields(params)?;
    if fields.modifiers != Some(0) {
        return None;
    }
    kitty_event(key, fields.event)
}

/// What a kitty key sequence's parameters say, by `kitty_key`'s rules.
struct KittyFields {
    code: u32,
    /// The modifier bits, lock modifiers left out; None without the field.
    modifiers: Option<u32>,
    event: u32,
}

/// `code[:alternates] [; modifiers[:event]]`; None when unparsable or with
/// more fields.
fn kitty_fields(params: &[u8]) -> Option<KittyFields> {
    const LOCKS: u32 = 64 | 128;
    let number = |digits: &[u8]| -> Option<Option<u32>> {
        if digits.is_empty() {
            return Some(None);
        }
        std::str::from_utf8(digits).ok()?.parse().ok().map(Some)
    };
    let mut fields = params.split(|byte| *byte == b';');
    let code = number(fields.next()?.split(|byte| *byte == b':').next()?)??;
    let mut modifiers = None;
    let mut event = 1;
    if let Some(field) = fields.next() {
        let mut parts = field.split(|byte| *byte == b':');
        let value = number(parts.next()?)?.unwrap_or(1);
        modifiers = Some(value.checked_sub(1)? & !LOCKS);
        event = number(parts.next().unwrap_or_default())?.unwrap_or(1);
        if parts.next().is_some() {
            return None;
        }
    }
    if fields.next().is_some() {
        return None;
    }
    Some(KittyFields {
        code,
        modifiers,
        event,
    })
}

/// A press or repeat (event 1 or 2) of `key` is the key; a release (3) is
/// the popup's own.
fn kitty_event(key: Key, event: u32) -> Option<Token> {
    match event {
        1 | 2 => Some(Token::Key(key)),
        3 => Some(Token::Release),
        _ => None,
    }
}

/// What [`KeySplitter`] found at the start of the bytes it was given.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Piece {
    /// Printable text, UTF-8 included: every character is a key of its own.
    /// `whole`: false when the bytes end inside a character, which the next
    /// bytes finish.
    Text { whole: bool },
    /// One key: a C0 control or DEL, ESC with what follows it (Alt with a
    /// character, a CSI or an SS3), or a CSI or SS3 sequence (arrows,
    /// function keys, kitty's `CSI … u`, xterm's `CSI 27 ; … ~`). `whole`:
    /// false when the bytes end inside it; the next bytes finish it.
    Key { whole: bool },
    /// Bytes of a bracketed paste, its markers included. `ends`: the paste
    /// ends with them.
    Paste { ends: bool },
}

/// One piece of a read: what it is, how many bytes of the read it takes,
/// and, for a key, all of its bytes (those of an earlier read included).
#[derive(Debug)]
pub struct Split<'a> {
    pub piece: Piece,
    pub len: usize,
    /// The whole key, for `Piece::Key`; empty otherwise.
    pub key: &'a [u8],
}

/// The most bytes of a key that started in an earlier read that are kept.
const CARRY: usize = 32;

/// A key cut by the end of a read, finished by the next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Carry {
    None,
    /// The bytes of an escape key so far.
    Key,
    /// This many continuation bytes of a character are still to come.
    Char(u8),
}

/// Finds where the user's keys start and end in what goes to the shell, the
/// way its line editor reads them: without the popup (whatever is open),
/// across reads, and without copying the bytes (except a key cut by the
/// end of a read: its first bytes are kept, at most [`CARRY`]).
pub struct KeySplitter {
    paste: bool,
    /// How much of the paste's end marker the earlier bytes ended with.
    matched: usize,
    carry: Carry,
    /// The bytes of a cut key so far (`Carry::Key`), then the whole key.
    buf: [u8; CARRY],
    buf_len: usize,
    /// More than `CARRY` bytes of the cut key came: only its end is looked for.
    overflow: bool,
}

impl Default for KeySplitter {
    fn default() -> Self {
        Self {
            paste: false,
            matched: 0,
            carry: Carry::None,
            buf: [0; CARRY],
            buf_len: 0,
            overflow: false,
        }
    }
}

impl KeySplitter {
    /// Whether a key or a character is cut: the bytes so far end inside it.
    pub fn is_cut(&self) -> bool {
        self.carry != Carry::None
    }

    /// Inside a bracketed paste.
    pub fn in_paste(&self) -> bool {
        self.paste
    }

    /// The bytes of a cut key so far, now taken as the whole key (no more
    /// came in time: a lone ESC is the Esc key). Empty for a cut character.
    pub fn finish_cut(&mut self) -> &[u8] {
        let carry = std::mem::replace(&mut self.carry, Carry::None);
        if carry != Carry::Key || self.overflow {
            self.overflow = false;
            return &[];
        }
        &self.buf[..self.buf_len]
    }

    /// The piece at the start of `data` (not empty).
    pub fn next<'a>(&'a mut self, data: &'a [u8]) -> Split<'a> {
        match self.carry {
            Carry::Key => return self.continue_key(data),
            Carry::Char(rest) => {
                // Continuation bytes finish the character; anything else
                // means it never will.
                let take = data
                    .iter()
                    .take(usize::from(rest))
                    .take_while(|byte| (0x80..0xc0).contains(*byte))
                    .count();
                if take > 0 {
                    let rest = rest - take as u8;
                    self.carry = if rest == 0 {
                        Carry::None
                    } else {
                        Carry::Char(rest)
                    };
                    return Split {
                        piece: Piece::Text { whole: rest == 0 },
                        len: take,
                        key: &[],
                    };
                }
                self.carry = Carry::None;
            }
            Carry::None => {}
        }
        if self.paste {
            let (len, ends) = self.paste_end(data);
            return Split {
                piece: Piece::Paste { ends },
                len,
                key: &[],
            };
        }
        match data[0] {
            ESC => {
                // A terminal's reply that passes through to the shell, whole.
                if let Some(len) = control_string_len(data) {
                    return Split {
                        piece: Piece::Key { whole: true },
                        len,
                        key: &data[..len],
                    };
                }
                let (len, whole) = escape_key(data);
                if !whole {
                    self.cut(data);
                    return Split {
                        piece: Piece::Key { whole: false },
                        len,
                        key: &[],
                    };
                }
                let key = &data[..len];
                if key == PASTE_START {
                    self.paste = true;
                    return self.paste_rest(data, len);
                }
                Split {
                    piece: Piece::Key { whole: true },
                    len,
                    key,
                }
            }
            byte if byte < 0x20 || byte == 0x7f => Split {
                piece: Piece::Key { whole: true },
                len: 1,
                key: &data[..1],
            },
            _ => {
                let len = data
                    .iter()
                    .position(|&byte| byte < 0x20 || byte == 0x7f)
                    .unwrap_or(data.len());
                let missing = char_missing(&data[..len]);
                if missing > 0 {
                    self.carry = Carry::Char(missing);
                }
                Split {
                    piece: Piece::Text {
                        whole: missing == 0,
                    },
                    len,
                    key: &[],
                }
            }
        }
    }

    /// Keep the start of a cut escape key.
    fn cut(&mut self, data: &[u8]) {
        self.carry = Carry::Key;
        self.overflow = data.len() > CARRY;
        self.buf_len = data.len().min(CARRY);
        self.buf[..self.buf_len].copy_from_slice(&data[..self.buf_len]);
    }

    /// The rest of a cut escape key at the start of `data`.
    fn continue_key<'a>(&'a mut self, data: &'a [u8]) -> Split<'a> {
        // Joined in the buffer and looked at whole: the cut may be anywhere.
        let old = self.buf_len;
        let room = (CARRY - old).min(data.len());
        let mut joined = [0u8; CARRY];
        joined[..old].copy_from_slice(&self.buf[..old]);
        joined[old..old + room].copy_from_slice(&data[..room]);
        let (len, whole) = if self.overflow {
            // Only the end of a long CSI is still looked for.
            match data.iter().position(|byte| (0x40..=0x7e).contains(byte)) {
                Some(at) => (old + at + 1, true),
                None => (old + data.len(), false),
            }
        } else {
            let (len, whole) = escape_key(&joined[..old + room]);
            if !whole && room < data.len() {
                // Longer than the buffer: the end is looked for from here on.
                (old + data.len(), false)
            } else {
                (len, whole)
            }
        };
        let taken = len - old;
        if !whole {
            self.overflow |= old + taken > CARRY;
            self.buf_len = (old + taken).min(CARRY);
            self.buf[..self.buf_len].copy_from_slice(&joined[..self.buf_len]);
            return Split {
                piece: Piece::Key { whole: false },
                len: taken,
                key: &[],
            };
        }
        self.carry = Carry::None;
        let overflow = std::mem::take(&mut self.overflow);
        self.buf_len = len.min(CARRY);
        self.buf[..self.buf_len].copy_from_slice(&joined[..self.buf_len]);
        if !overflow && &self.buf[..self.buf_len] == PASTE_START {
            self.paste = true;
            return self.paste_rest(data, taken);
        }
        Split {
            piece: Piece::Key { whole: true },
            len: taken,
            key: if overflow {
                &[]
            } else {
                &self.buf[..self.buf_len]
            },
        }
    }

    /// A paste started with the first `start` bytes of `data`: the piece
    /// runs on to its end, or the end of `data`.
    fn paste_rest(&mut self, data: &[u8], start: usize) -> Split<'static> {
        let (len, ends) = self.paste_end(&data[start..]);
        Split {
            piece: Piece::Paste { ends },
            len: start + len,
            key: &[],
        }
    }

    /// How much of `data` is still paste, and whether the paste ends there.
    fn paste_end(&mut self, data: &[u8]) -> (usize, bool) {
        let mut i = 0;
        while i < data.len() {
            if self.matched == 0 {
                match memchr(ESC, &data[i..]) {
                    Some(at) => i += at,
                    None => return (data.len(), false),
                }
            }
            let byte = data[i];
            i += 1;
            if byte == PASTE_END[self.matched] {
                self.matched += 1;
                if self.matched == PASTE_END.len() {
                    self.matched = 0;
                    self.paste = false;
                    return (i, true);
                }
            } else {
                self.matched = usize::from(byte == ESC);
            }
        }
        (data.len(), false)
    }
}

/// The length of the escape key at the start of `data` (which starts with
/// ESC), and whether it is all there.
/// The length of a control string (OSC `ESC ]`, DCS `ESC P`, APC `ESC _`,
/// PM `ESC ^`) at the start of `data`, with its BEL or ST, when it ends in
/// `data`: a terminal's reply (OSC 11, XTGETTCAP) read whole. Otherwise
/// `None`, and the bytes are keys as before (Alt-] and what follows).
fn control_string_len(data: &[u8]) -> Option<usize> {
    if !matches!(data, [ESC, b']' | b'P' | b'_' | b'^', ..]) {
        return None;
    }
    let mut i = 2;
    while i < data.len() {
        match data[i] {
            0x07 => return Some(i + 1),
            ESC => return (data.get(i + 1) == Some(&b'\\')).then_some(i + 2),
            _ => i += 1,
        }
    }
    None
}

/// Whether `key`, a whole key from [`KeySplitter`], is a terminal's reply
/// rather than a key: a control string ended by BEL or ST, or a CSI with a
/// private marker (`?`, `>`, `=`) first, as DA1, DA2, DECRPM and kitty's
/// flags replies have and no key does. The replies to the shell's own
/// queries reach it as user bytes.
pub fn is_reply(key: &[u8]) -> bool {
    match key {
        [ESC, b'[', marker, ..] => matches!(marker, b'?' | b'>' | b'='),
        [ESC, b']' | b'P' | b'_' | b'^', ..] => control_string_len(key) == Some(key.len()),
        _ => false,
    }
}

fn escape_key(data: &[u8]) -> (usize, bool) {
    let Some(&second) = data.get(1) else {
        return (1, false);
    };
    match second {
        b'[' => csi_len(data, 2),
        b'O' => match data.get(2) {
            Some(_) => (3, true),
            None => (2, false),
        },
        // ESC ESC: Alt with an escape key, or Alt-Esc.
        ESC => match data.get(2) {
            Some(b'[') => csi_len(data, 3),
            Some(b'O') => match data.get(3) {
                Some(_) => (4, true),
                None => (3, false),
            },
            Some(_) => (2, true),
            None => (2, false),
        },
        // Alt with a character, UTF-8 included.
        lead => {
            let need = 1 + utf8_len(lead);
            if data.len() >= need {
                (need, true)
            } else {
                (data.len(), false)
            }
        }
    }
}

/// A CSI whose parameters start at `from`: its length up to and with its
/// final byte, and whether that came.
fn csi_len(data: &[u8], from: usize) -> (usize, bool) {
    match data[from..]
        .iter()
        .position(|byte| (0x40..=0x7e).contains(byte))
    {
        Some(at) => (from + at + 1, true),
        None => (data.len(), false),
    }
}

/// How many bytes the UTF-8 character that starts with `lead` has (1 for a
/// byte that starts none).
fn utf8_len(lead: u8) -> usize {
    match lead {
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => 1,
    }
}

/// How many continuation bytes the last character of `text` still needs.
fn char_missing(text: &[u8]) -> u8 {
    // A character has at most 4 bytes: its lead is among the last 4.
    for back in 1..=text.len().min(4) {
        let byte = text[text.len() - back];
        if (0x80..0xc0).contains(&byte) {
            continue;
        }
        let need = utf8_len(byte);
        return need.saturating_sub(back) as u8;
    }
    0
}

/// Whether `key` (one key from [`KeySplitter`]) accepts the line: Enter
/// (`\r`, keypad Enter `ESC O M`), C-m (`\r` too), C-j (`\n`) and C-o, in
/// their legacy bytes or as kitty's `CSI … u` or xterm's `CSI 27 ; … ~`
/// (fish binds kitty's C-m, `CSI 109 ; 5 u`, to `execute`). A kitty Enter
/// with any modifier counts: missing a popup for the rest of a line is
/// better than a trigger typed into a program.
pub fn is_accept(key: &[u8]) -> bool {
    match key {
        b"\r" | b"\n" | b"\x0f" | b"\x1bOM" => true,
        _ => match csi_key(key) {
            Some(CsiKey {
                code: 13 | 57414,
                event: 1 | 2,
                ..
            }) => true,
            // Not a key release.
            Some(csi) => {
                csi.event != 3 && matches!(csi.legacy(), Some((b'\r' | b'\n' | b'\x0f', false)))
            }
            None => false,
        },
    }
}

/// The legacy bytes of `key` (one key from [`KeySplitter`]) into `buf`, for
/// matching against keys a shell names in its legacy bytes: a kitty
/// `CSI … u` or xterm `CSI 27 ; … ~` key that has a legacy form gets it
/// (C-x is `\x18`, Alt-e `ESC e`, Esc `ESC`), and every other key is
/// itself. `None` for a key release (kitty's event type 3), which is no
/// key to the shell.
pub fn legacy_bytes<'a>(key: &'a [u8], buf: &'a mut [u8; 8]) -> Option<&'a [u8]> {
    let Some(csi) = csi_key(key) else {
        return Some(key);
    };
    if csi.event == 3 {
        return None;
    }
    let Some((byte, alt)) = csi.legacy() else {
        return Some(key);
    };
    let mut len = 0;
    if alt {
        buf[0] = ESC;
        len = 1;
    }
    match byte {
        // A character beyond ASCII: its UTF-8 bytes.
        _ if csi.code >= 0x80 => {
            let Some(c) = char::from_u32(csi.code) else {
                return Some(key);
            };
            len += c.encode_utf8(&mut buf[len..]).len();
        }
        _ => {
            buf[len] = byte;
            len += 1;
        }
    }
    Some(&buf[..len])
}

/// A key as kitty (`CSI code[:alternates] ; modifiers[:event] u`) or xterm's
/// modifyOtherKeys (`CSI 27 ; modifiers ; code ~`) sends it.
struct CsiKey {
    code: u32,
    /// The modifier bits, lock modifiers left out.
    modifiers: u32,
    event: u32,
}

impl CsiKey {
    const SHIFT: u32 = 1;
    const ALT: u32 = 2;
    const CTRL: u32 = 4;

    /// The legacy byte of the key and whether Alt (an ESC before it) goes
    /// with it; `None` when it has no legacy form (Super, Hyper or Meta, a
    /// functional key, Ctrl with a key that has no control byte). For a
    /// character beyond ASCII the byte is unused: it is the character.
    fn legacy(&self) -> Option<(u8, bool)> {
        if self.modifiers & !(Self::SHIFT | Self::ALT | Self::CTRL) != 0 {
            return None;
        }
        let alt = self.modifiers & Self::ALT != 0;
        let ctrl = self.modifiers & Self::CTRL != 0;
        let shift = self.modifiers & Self::SHIFT != 0;
        let byte = match self.code {
            9 | 13 | 27 | 127 if !ctrl && !shift => self.code as u8,
            code @ 0x20..=0x7e => {
                let byte = code as u8;
                match (ctrl, shift) {
                    (true, _) => match byte.to_ascii_lowercase() {
                        b'a'..=b'z' => byte.to_ascii_lowercase() - 0x60,
                        b'@' | b'[' | b'\\' | b']' | b'^' | b'_' => byte & 0x1f,
                        b' ' => 0,
                        b'?' => 0x7f,
                        _ => return None,
                    },
                    (false, true) => byte.to_ascii_uppercase(),
                    (false, false) => byte,
                }
            }
            // kitty's functional keys (57344-63743) have code points of
            // their own, in a private use area: they are no characters.
            code if code >= 0x80
                && !(57344..=63743).contains(&code)
                && !ctrl
                && char::from_u32(code).is_some() =>
            {
                0
            }
            _ => return None,
        };
        Some((byte, alt))
    }
}

/// `key` read as kitty's or xterm's form of a key; `None` for anything else.
fn csi_key(key: &[u8]) -> Option<CsiKey> {
    let body = key.strip_prefix(b"\x1b[")?;
    let (&last, params) = body.split_last()?;
    match last {
        b'u' => {
            let fields = kitty_fields(params)?;
            Some(CsiKey {
                code: fields.code,
                modifiers: fields.modifiers.unwrap_or(0),
                event: fields.event,
            })
        }
        b'~' => {
            let rest = params.strip_prefix(b"27;")?;
            let (modifiers, code) = std::str::from_utf8(rest).ok()?.split_once(';')?;
            let modifiers: u32 = modifiers.parse().ok()?;
            Some(CsiKey {
                code: code.parse().ok()?,
                modifiers: modifiers.checked_sub(1)? & !(64 | 128),
                event: 1,
            })
        }
        _ => None,
    }
}

/// The length of the escape sequence at the start of `data` that is not a
/// popup key, so none of its bytes is read as one: a CSI up to its final
/// byte, an SS3 with its one byte, or ESC with the byte after it (Alt). An
/// unfinished one runs to the end of `data`.
fn sequence_len(data: &[u8]) -> usize {
    match data.get(1) {
        Some(b'[') => data[2..]
            .iter()
            .position(|byte| (0x40..=0x7e).contains(byte))
            .map_or(data.len(), |at| at + 3),
        Some(b'O') => data.len().min(3),
        Some(_) => 2,
        None => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tokens of `chunks` fed one after another, each as (token, bytes).
    fn tokens(chunks: &[&[u8]], intercept: bool) -> Vec<(Token, Vec<u8>)> {
        let mut decoder = KeyDecoder::with_popout(None);
        let mut out = Vec::new();
        for chunk in chunks {
            let mut i = 0;
            while i < chunk.len() {
                let (token, len) = decoder.next(&chunk[i..], intercept);
                assert!(len > 0, "empty token");
                out.push((token, chunk[i..i + len].to_vec()));
                i += len;
            }
        }
        out
    }

    fn key(key: Key, bytes: &[u8]) -> (Token, Vec<u8>) {
        (Token::Key(key), bytes.to_vec())
    }

    fn bytes(bytes: &[u8]) -> (Token, Vec<u8>) {
        (Token::Bytes, bytes.to_vec())
    }

    #[test]
    fn each_key_in_csi_and_ss3_form() {
        for (input, expected) in [
            (&b"\t"[..], Key::Tab),
            (b"\r", Key::Enter),
            (b"\x1b[Z", Key::ShiftTab),
            (b"\x1b[A", Key::Up),
            (b"\x1bOA", Key::Up),
            (b"\x1b[B", Key::Down),
            (b"\x1bOB", Key::Down),
            (b"\x1b", Key::Esc),
        ] {
            assert_eq!(
                tokens(&[input], true),
                vec![key(expected, input)],
                "{input:?}"
            );
        }
    }

    fn release(bytes: &[u8]) -> (Token, Vec<u8>) {
        (Token::Release, bytes.to_vec())
    }

    #[test]
    fn each_key_in_the_kitty_form() {
        for (input, expected) in [
            (&b"\x1b[9u"[..], Key::Tab),
            (b"\x1b[13u", Key::Enter),
            (b"\x1b[27u", Key::Esc),
            (b"\x1b[9;2u", Key::ShiftTab),
            // The modifier field of no modifier, and the press and repeat
            // event types.
            (b"\x1b[9;1u", Key::Tab),
            (b"\x1b[9;1:1u", Key::Tab),
            (b"\x1b[9;1:2u", Key::Tab),
            (b"\x1b[13;1:2u", Key::Enter),
            (b"\x1b[27;1:1u", Key::Esc),
            (b"\x1b[9;2:1u", Key::ShiftTab),
            (b"\x1b[9;2:2u", Key::ShiftTab),
            // Caps lock and num lock are not modifiers of the key.
            (b"\x1b[9;65u", Key::Tab),
            (b"\x1b[13;129u", Key::Enter),
            (b"\x1b[9;66u", Key::ShiftTab),
            // Alternate key codes after the key code.
            (b"\x1b[9::9;2u", Key::ShiftTab),
        ] {
            assert_eq!(
                tokens(&[input], true),
                vec![key(expected, input)],
                "{input:?}"
            );
        }
    }

    #[test]
    fn the_release_of_a_popup_key_is_the_popups_own() {
        for input in [
            &b"\x1b[9;1:3u"[..],
            b"\x1b[13;1:3u",
            b"\x1b[27;1:3u",
            b"\x1b[9;2:3u",
            b"\x1b[9;65:3u",
        ] {
            assert_eq!(tokens(&[input], true), vec![release(input)], "{input:?}");
        }
        // The release of any other key goes to the shell.
        for input in [&b"\x1b[97;1:3u"[..], b"\x1b[13;2:3u", b"\x1b[57441;2:3u"] {
            assert_eq!(tokens(&[input], true), vec![bytes(input)], "{input:?}");
        }
        // Press, then release, with typing around them.
        assert_eq!(
            tokens(&[b"a\x1b[13u\x1b[13;1:3ub"], true),
            vec![
                bytes(b"a"),
                key(Key::Enter, b"\x1b[13u"),
                release(b"\x1b[13;1:3u"),
                bytes(b"b"),
            ]
        );
    }

    #[test]
    fn kitty_arrow_events() {
        for (input, expected) in [
            (&b"\x1b[1;1A"[..], Key::Up),
            (b"\x1b[1;1B", Key::Down),
            (b"\x1b[1;1:1A", Key::Up),
            (b"\x1b[1;1:1B", Key::Down),
            (b"\x1b[1;1:2A", Key::Up),
            (b"\x1b[1;1:2B", Key::Down),
            // Caps lock and num lock are not modifiers of the key.
            (b"\x1b[1;65:2B", Key::Down),
            (b"\x1b[1;129A", Key::Up),
        ] {
            assert_eq!(
                tokens(&[input], true),
                vec![key(expected, input)],
                "{input:?}"
            );
        }
        // A release is the popup's own.
        for input in [&b"\x1b[1;1:3A"[..], b"\x1b[1;1:3B", b"\x1b[1;65:3B"] {
            assert_eq!(tokens(&[input], true), vec![release(input)], "{input:?}");
        }
        // With a modifier, another event type, another code or another
        // arrow, or no modifier field, it passes whole.
        for input in [
            &b"\x1b[1;2:2B"[..],
            b"\x1b[1;5:1A",
            b"\x1b[1;5:3B",
            b"\x1b[1;1:4B",
            b"\x1b[1;1:1:1B",
            b"\x1b[2;1:1A",
            b"\x1b[1:1;1:1A",
            b"\x1b[1;1;1B",
            b"\x1b[1B",
            b"\x1b[;1B",
            b"\x1b[1;1:1C",
            b"\x1b[1;1:3D",
        ] {
            assert_eq!(tokens(&[input], true), vec![bytes(input)], "{input:?}");
        }
        // Repeats then the release, with typing around them.
        assert_eq!(
            tokens(&[b"a\x1b[1;1:2B\x1b[1;1:2B\x1b[1;1:3Bb"], true),
            vec![
                bytes(b"a"),
                key(Key::Down, b"\x1b[1;1:2B"),
                key(Key::Down, b"\x1b[1;1:2B"),
                release(b"\x1b[1;1:3B"),
                bytes(b"b"),
            ]
        );
        // Without intercepting, nothing changes.
        let input = b"\x1b[1;1:2B\x1b[1;1:3A\x1b[1;1A";
        assert_eq!(tokens(&[input], false), vec![bytes(input)]);
    }

    #[test]
    fn other_kitty_sequences_pass_whole() {
        for input in [
            // Other modifiers.
            &b"\x1b[9;5u"[..],
            b"\x1b[9;3u",
            b"\x1b[13;2u",
            b"\x1b[13;5u",
            b"\x1b[27;2u",
            b"\x1b[27;3u",
            b"\x1b[9;9u",
            // Other keys.
            b"\x1b[97u",
            b"\x1b[97;5u",
            b"\x1b[127u",
            b"\x1b[57441;2u",
            b"\x1b[0;;229u",
            // The text field, an unknown event type, and malformed fields.
            b"\x1b[13;1;13u",
            b"\x1b[9;1:4u",
            b"\x1b[9;1:1:1u",
            b"\x1b[9;0u",
            b"\x1b[u",
            b"\x1b[;2u",
            b"\x1b[?9u",
            b"\x1b[?u",
            b"\x1b[=1;1u",
            b"\x1b[99999999999u",
        ] {
            assert_eq!(tokens(&[input], true), vec![bytes(input)], "{input:?}");
        }
        // A key after one is still a key.
        assert_eq!(
            tokens(&[b"\x1b[9;5u\x1b[9u"], true),
            vec![bytes(b"\x1b[9;5u"), key(Key::Tab, b"\x1b[9u")]
        );
    }

    #[test]
    fn modify_other_keys_shift_tab_only() {
        assert_eq!(
            tokens(&[b"\x1b[27;2;9~"], true),
            vec![key(Key::ShiftTab, b"\x1b[27;2;9~")]
        );
        for input in [
            &b"\x1b[27;1;9~"[..],
            b"\x1b[27;5;9~",
            b"\x1b[27;2;13~",
            b"\x1b[27;5;13~",
            b"\x1b[27;2;27~",
            b"\x1b[27;2;9;1~",
            b"\x1b[27;2;90~",
            b"\x1b[27~",
            b"\x1b[2;2;9~",
        ] {
            assert_eq!(tokens(&[input], true), vec![bytes(input)], "{input:?}");
        }
    }

    #[test]
    fn modified_arrows_and_option_pass_whole_and_in_order() {
        for input in [
            // Modifier forms of the arrows (shift, alt, ctrl, and the
            // kitty event types), CSI and SS3.
            &b"\x1b[1;2A"[..],
            b"\x1b[1;3B",
            b"\x1b[1;5A",
            b"\x1b[1;5B",
            b"\x1b[1;9A",
            b"\x1b[1;5:2B",
            b"\x1b[1;2C",
            b"\x1b[1;2D",
            b"\x1b[1;5C",
            // Option as an ESC prefix.
            b"\x1bx",
            b"\x1b\t",
            b"\x1b\r",
            b"\x1b\x1b[A",
            b"\x1b\x1b[B",
            b"\x1b\x1bOA",
            b"\x1b\x1bOB",
            b"\x1b\x1b",
        ] {
            let mut expected = vec![bytes(input)];
            // ESC ESC is two bytes, then the rest is plain bytes: nothing
            // of it is a key.
            if input.starts_with(b"\x1b\x1b") && input.len() > 2 {
                expected = vec![bytes(&input[..2]), bytes(&input[2..])];
            }
            assert_eq!(tokens(&[input], true), expected, "{input:?}");
            // Followed by a real key.
            let mut with_key = input.to_vec();
            with_key.extend_from_slice(b"\t");
            let got = tokens(&[&with_key], true);
            assert_eq!(
                got.last(),
                Some(&key(Key::Tab, b"\t")),
                "{input:?} then Tab"
            );
            assert!(
                got[..got.len() - 1]
                    .iter()
                    .all(|(token, _)| *token == Token::Bytes),
                "{input:?}: {got:?}"
            );
        }
    }

    #[test]
    fn split_reads_never_take_part_of_a_sequence_for_a_key() {
        // A sequence cut in the middle: no part of it is a key, in either
        // read. (A cut right after a lone ESC is the Esc key; the input
        // tokenizer holds that one back, so the decoder never sees it.)
        for input in [
            &b"\x1b[1;5A"[..],
            b"\x1b[1;2B",
            b"\x1bO5A",
            b"\x1b[9;5u",
            b"\x1b[13;2u",
            b"\x1b[27;2;13~",
            b"\x1b[27;5;9~",
            b"\x1b[15~",
        ] {
            for split in 2..input.len() {
                let (a, b) = input.split_at(split);
                let got = tokens(&[a, b], true);
                assert!(
                    got.iter().all(|(token, _)| *token == Token::Bytes),
                    "{input:?} split at {split}: {got:?}"
                );
                let joined: Vec<u8> = got.into_iter().flat_map(|(_, bytes)| bytes).collect();
                assert_eq!(joined, input, "{input:?} split at {split}");
            }
        }
    }

    #[test]
    fn the_new_forms_are_bytes_without_intercepting() {
        let input = b"\x1b[9u\x1b[13;1:3u\x1b[27;2;9~\x1b[27u\x1b[9;2u";
        assert_eq!(tokens(&[input], false), vec![bytes(input)]);
    }

    #[test]
    fn the_new_forms_are_bytes_inside_a_paste() {
        let paste = b"\x1b[200~\x1b[9u\x1b[13;1:3u\x1b[27;2;9~\x1b[201~";
        assert_eq!(
            tokens(&[paste], true),
            vec![
                (Token::PasteStart, PASTE_START.to_vec()),
                bytes(&paste[PASTE_START.len()..]),
            ]
        );
    }

    #[test]
    fn bytes_around_keys_keep_their_order() {
        assert_eq!(
            tokens(&[b"ab\tc\x1b[Bd\r"], true),
            vec![
                bytes(b"ab"),
                key(Key::Tab, b"\t"),
                bytes(b"c"),
                key(Key::Down, b"\x1b[B"),
                bytes(b"d"),
                key(Key::Enter, b"\r"),
            ]
        );
    }

    #[test]
    fn other_sequences_pass_whole() {
        for input in [
            &b"\x1b[1;5A"[..],
            b"\x1b[D",
            b"\x1bOD",
            b"\x1bb",
            b"\x1b\x1b",
            b"\x1b[15~",
            b"\x1b[",
            b"\x1b[1;",
        ] {
            assert_eq!(tokens(&[input], true), vec![bytes(input)], "{input:?}");
        }
        // A key right after another sequence is still a key.
        assert_eq!(
            tokens(&[b"\x1b[1;5D\x1b[A"], true),
            vec![bytes(b"\x1b[1;5D"), key(Key::Up, b"\x1b[A")]
        );
        // ESC followed by something is Alt, not Esc.
        assert_eq!(tokens(&[b"\x1bx"], true), vec![bytes(b"\x1bx")]);
    }

    #[test]
    fn the_popout_key_only_when_it_has_one_and_intercepting() {
        let input = b"a\x0bb\x1b[200~\x0b\x1b[201~";
        let run = |decoder: &mut KeyDecoder, intercept: bool| {
            let mut out = Vec::new();
            let mut i = 0;
            while i < input.len() {
                let (token, len) = decoder.next(&input[i..], intercept);
                out.push((token, input[i..i + len].to_vec()));
                i += len;
            }
            out
        };
        let mut decoder = KeyDecoder::with_popout(Some(0x0b));
        assert_eq!(
            run(&mut decoder, true),
            vec![
                bytes(b"a"),
                (Token::Popout, b"\x0b".to_vec()),
                bytes(b"b"),
                (Token::PasteStart, PASTE_START.to_vec()),
                // Inside a paste it is only a byte.
                bytes(b"\x0b\x1b[201~"),
            ]
        );
        let mut decoder = KeyDecoder::with_popout(Some(0x0b));
        // Not intercepting: passed on whole, up to and after the paste start.
        assert_eq!(
            run(&mut decoder, false),
            vec![bytes(&input[..9]), bytes(&input[9..])]
        );
        assert_eq!(tokens(&[b"a\x0bb"], true), vec![bytes(b"a\x0bb")]);
    }

    #[test]
    fn nothing_is_a_key_without_intercepting() {
        let input = b"a\t\r\x1b[A\x1b";
        assert_eq!(tokens(&[input], false), vec![bytes(input)]);
    }

    #[test]
    fn nothing_inside_a_paste_is_a_key() {
        let paste = b"\x1b[200~echo pasted\r\t\x1b[A\x1b\x1b[201~";
        let mut input = paste.to_vec();
        input.extend_from_slice(b"\t");
        assert_eq!(
            tokens(&[&input], true),
            vec![
                (Token::PasteStart, PASTE_START.to_vec()),
                bytes(&paste[PASTE_START.len()..]),
                key(Key::Tab, b"\t"),
            ]
        );
    }

    #[test]
    fn paste_markers_split_across_reads() {
        // Not intercepting when the paste starts, intercepting after.
        let mut decoder = KeyDecoder::with_popout(None);
        assert_eq!(decoder.next(b"x\x1b[20", false), (Token::Bytes, 5));
        assert!(!decoder.in_paste());
        assert_eq!(decoder.next(b"0~a\tb", true), (Token::PasteStart, 2));
        assert!(decoder.in_paste());
        assert_eq!(decoder.next(b"a\tb\x1b[2", true), (Token::Bytes, 6));
        assert_eq!(decoder.next(b"01~\t", true), (Token::Bytes, 3));
        assert!(!decoder.in_paste());
        assert_eq!(decoder.next(b"\t", true), (Token::Key(Key::Tab), 1));

        // Every split point of a paste gives the same tokens, except inside
        // the start marker: while the popup is open the input tokenizer
        // holds a partial sequence, so the decoder never sees that split.
        let input = b"a\x1b[200~\t\r\x1b[201~\t";
        let whole: Vec<Token> = tokens(&[input], true)
            .into_iter()
            .map(|(token, _)| token)
            .filter(|token| *token != Token::Bytes)
            .collect();
        assert_eq!(whole, [Token::PasteStart, Token::Key(Key::Tab)]);
        for split in (0..=input.len()).filter(|split| !(2..7).contains(split)) {
            let (a, b) = input.split_at(split);
            let keys: Vec<Token> = tokens(&[a, b], true)
                .into_iter()
                .map(|(token, _)| token)
                .filter(|token| *token != Token::Bytes)
                .collect();
            assert_eq!(keys, whole, "split at {split}");
        }
    }

    /// What `KeySplitter` makes of `chunks`, one after another: text runs
    /// joined, each whole key with all its bytes, each paste whole.
    fn split(chunks: &[&[u8]]) -> Vec<(&'static str, Vec<u8>)> {
        let mut splitter = KeySplitter::default();
        let mut out: Vec<(&'static str, Vec<u8>)> = Vec::new();
        let mut push = |kind: &'static str, bytes: &[u8], join: bool| match out.last_mut() {
            Some((last, joined)) if join && *last == kind => joined.extend_from_slice(bytes),
            _ => out.push((kind, bytes.to_vec())),
        };
        let mut open_paste = false;
        // The bytes of a cut key so far.
        let mut cut = Vec::new();
        for chunk in chunks {
            let mut i = 0;
            while i < chunk.len() {
                let split = splitter.next(&chunk[i..]);
                let bytes = &chunk[i..i + split.len];
                match split.piece {
                    Piece::Text { .. } => push("text", bytes, true),
                    Piece::Key { whole: true } => {
                        cut.clear();
                        push("key", split.key, false);
                    }
                    Piece::Key { whole: false } => cut.extend_from_slice(bytes),
                    Piece::Paste { ends } => {
                        // The start marker's first bytes may have come as a
                        // cut key.
                        let mut whole = std::mem::take(&mut cut);
                        whole.extend_from_slice(bytes);
                        push("paste", &whole, open_paste);
                        open_paste = !ends;
                    }
                }
                i += split.len;
            }
        }
        out
    }

    fn piece(kind: &'static str, bytes: &[u8]) -> (&'static str, Vec<u8>) {
        (kind, bytes.to_vec())
    }

    /// A terminal's reply in one read is one piece, and `is_reply` knows
    /// it; keys never are.
    #[test]
    fn replies_are_whole_and_known() {
        let osc = b"\x1b]11;rgb:2828/2c2c/3434\x1b\\";
        let bel = b"\x1b]11;rgb:2828/2c2c/3434\x07";
        let dcs = b"\x1bP1+r696e646e=5c455b25703125645c\x1b\\";
        let da1 = b"\x1b[?62;22c";
        let input = [
            &b"a"[..],
            osc,
            bel,
            dcs,
            da1,
            b"\x1b[?0u",
            b"\x1b[>1;2c",
            b"b",
        ]
        .concat();
        assert_eq!(
            split(&[&input]),
            vec![
                piece("text", b"a"),
                piece("key", osc),
                piece("key", bel),
                piece("key", dcs),
                piece("key", da1),
                piece("key", b"\x1b[?0u"),
                piece("key", b"\x1b[>1;2c"),
                piece("text", b"b"),
            ]
        );
        for reply in [&osc[..], bel, dcs, da1, b"\x1b[?2026;2$y", b"\x1b[=1c"] {
            assert!(is_reply(reply), "{reply:?}");
        }
        for key in [
            &b"\x1b]"[..],
            b"\x1b[A",
            b"\x1b[97;5u",
            b"\x1b[1;5R",
            b"\x1bP",
            b"\x1b\\",
            b"a",
            b"\r",
        ] {
            assert!(!is_reply(key), "{key:?}");
        }
        // Not ended in the read: keys, as before (Alt-] and text).
        assert_eq!(
            split(&[b"\x1b]11;rgb"]),
            vec![piece("key", b"\x1b]"), piece("text", b"11;rgb")]
        );
    }

    #[test]
    fn the_splitter_finds_every_key() {
        let input = "ab\x1b[1;5Aé\x1b\x1bOA\x1bx\x1b[97;5u\x1b[13;1:3u\x7f\x1bOP\x1b[15~\x1b\x1b\x1bé\x1b[27;2;9~ש🙂\r\t\x1b[Z"
            .as_bytes();
        let expected = vec![
            piece("text", b"ab"),
            piece("key", b"\x1b[1;5A"),
            piece("text", "é".as_bytes()),
            piece("key", b"\x1b\x1bOA"),
            piece("key", b"\x1bx"),
            piece("key", b"\x1b[97;5u"),
            piece("key", b"\x1b[13;1:3u"),
            piece("key", b"\x7f"),
            piece("key", b"\x1bOP"),
            piece("key", b"\x1b[15~"),
            piece("key", b"\x1b\x1b"),
            piece("key", "\x1bé".as_bytes()),
            piece("key", b"\x1b[27;2;9~"),
            piece("text", "ש🙂".as_bytes()),
            piece("key", b"\r"),
            piece("key", b"\t"),
            piece("key", b"\x1b[Z"),
        ];
        assert_eq!(split(&[input]), expected);
        // Cut anywhere, once or twice, the keys are the same.
        for a in 0..=input.len() {
            let (x, rest) = input.split_at(a);
            assert_eq!(split(&[x, rest]), expected, "cut at {a}");
            for b in 0..=rest.len() {
                let (y, z) = rest.split_at(b);
                assert_eq!(split(&[x, y, z]), expected, "cut at {a} and {}", a + b);
            }
        }
    }

    #[test]
    fn the_splitter_keeps_a_paste_whole() {
        let input = b"x\x1b[200~echo \r\t\x1b[A\x1b\x1b[201~y\x1b[200~\x1b[201~";
        let expected = vec![
            piece("text", b"x"),
            piece("paste", b"\x1b[200~echo \r\t\x1b[A\x1b\x1b[201~"),
            piece("text", b"y"),
            piece("paste", b"\x1b[200~\x1b[201~"),
        ];
        assert_eq!(split(&[input]), expected);
        for a in 0..=input.len() {
            let (x, rest) = input.split_at(a);
            for b in 0..=rest.len() {
                let (y, z) = rest.split_at(b);
                assert_eq!(split(&[x, y, z]), expected, "cut at {a} and {}", a + b);
            }
        }
    }

    #[test]
    fn the_splitter_says_what_is_cut() {
        let mut splitter = KeySplitter::default();
        let split = splitter.next(b"a\x1b");
        assert_eq!((split.piece, split.len), (Piece::Text { whole: true }, 1));
        let split = splitter.next(b"\x1b");
        assert_eq!(split.piece, Piece::Key { whole: false });
        assert!(splitter.is_cut());
        // Nothing more came: it is the Esc key.
        assert_eq!(splitter.finish_cut(), b"\x1b");
        assert!(!splitter.is_cut());
        let split = splitter.next("a\u{e9}".as_bytes().split_at(2).0);
        assert_eq!((split.piece, split.len), (Piece::Text { whole: false }, 2));
        assert!(splitter.is_cut());
        assert_eq!(splitter.finish_cut(), b"");
        // A long sequence is followed to its end without its bytes.
        let mut long = b"\x1b[".to_vec();
        long.extend(std::iter::repeat_n(b'1', 40));
        let mut splitter = KeySplitter::default();
        let split = splitter.next(&long);
        assert_eq!(split.piece, Piece::Key { whole: false });
        let split = splitter.next(b"1;2u");
        assert_eq!((split.piece, split.len), (Piece::Key { whole: true }, 4));
        assert!(split.key.is_empty());
        assert!(!splitter.is_cut());
        // ESC ESC then a letter: the cut key was whole by itself.
        let mut splitter = KeySplitter::default();
        assert_eq!(
            splitter.next(b"\x1b\x1b").piece,
            Piece::Key { whole: false }
        );
        let split = splitter.next(b"x");
        assert_eq!(
            (split.piece, split.len, split.key),
            (Piece::Key { whole: true }, 0, &b"\x1b\x1b"[..])
        );
        let split = splitter.next(b"x");
        assert_eq!((split.piece, split.len), (Piece::Text { whole: true }, 1));
    }

    #[test]
    fn accept_keys() {
        for key in [
            &b"\r"[..],
            b"\n",
            b"\x0f",
            b"\x1bOM",
            b"\x1b[13u",
            b"\x1b[13;1u",
            b"\x1b[13;2u",
            b"\x1b[13;5u",
            b"\x1b[13;1:2u",
            b"\x1b[57414u",
            b"\x1b[106;5u",
            b"\x1b[106;69u",
            b"\x1b[109;5u",
            b"\x1b[109;5:2u",
            b"\x1b[111;5u",
            b"\x1b[27;5;106~",
            b"\x1b[27;5;109~",
            b"\x1b[27;5;111~",
            b"\x1b[27;2;13~",
        ] {
            assert!(is_accept(key), "{key:?}");
        }
        for key in [
            &b"a"[..],
            b"\t",
            b"\x1b",
            b"\x1b\r",
            b"\x1b[13;1:3u",
            b"\x1b[106u",
            b"\x1b[106;3u",
            b"\x1b[109;7u",
            b"\x1b[109;5:3u",
            b"\x1b[111;9u",
            b"\x1b[A",
            b"\x1bOA",
            b"\x1b[27;1;106~",
        ] {
            assert!(!is_accept(key), "{key:?}");
        }
    }

    #[test]
    fn kitty_keys_are_matched_by_their_legacy_bytes() {
        let legacy = |key: &[u8]| {
            let mut buf = [0u8; 8];
            legacy_bytes(key, &mut buf).map(<[u8]>::to_vec)
        };
        for (key, expected) in [
            (&b"\x1b[120;5u"[..], &b"\x18"[..]),
            (b"\x1b[101;3u", b"\x1be"),
            (b"\x1b[101;7u", b"\x1b\x05"),
            (b"\x1b[27u", b"\x1b"),
            (b"\x1b[27;3u", b"\x1b\x1b"),
            (b"\x1b[9u", b"\t"),
            (b"\x1b[127u", b"\x7f"),
            (b"\x1b[97;2u", b"A"),
            (b"\x1b[97;69u", b"\x01"),
            (b"\x1b[32;5u", b"\x00"),
            (b"\x1b[233u", "é".as_bytes()),
            (b"\x1b[233;3u", "\x1bé".as_bytes()),
            (b"\x1b[27;5;120~", b"\x18"),
            (b"\x1b[118;5u", b"\x16"),
            // No legacy form: the key itself.
            (b"\x1b[9;2u", b"\x1b[9;2u"),
            (b"\x1b[97;9u", b"\x1b[97;9u"),
            (b"\x1b[57399u", b"\x1b[57399u"),
            (b"\x1b[49;5u", b"\x1b[49;5u"),
            // Not kitty's.
            (b"\x1b[A", b"\x1b[A"),
            (b"\x18", b"\x18"),
            (b"\x1bOP", b"\x1bOP"),
        ] {
            assert_eq!(legacy(key), Some(expected.to_vec()), "{key:?}");
        }
        // A release is no key to the shell.
        assert_eq!(legacy(b"\x1b[97;1:3u"), None);
    }

    #[test]
    fn a_paste_without_intercepting_is_still_followed() {
        let mut decoder = KeyDecoder::with_popout(None);
        assert_eq!(decoder.next(b"\x1b[200~ab", false), (Token::Bytes, 6));
        assert!(decoder.in_paste());
        assert_eq!(decoder.next(b"ab\t", true), (Token::Bytes, 3));
        assert_eq!(decoder.next(b"\x1b[201~", false), (Token::Bytes, 6));
        assert!(!decoder.in_paste());
    }
}
