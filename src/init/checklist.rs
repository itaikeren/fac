//! `fac init`'s checklist, drawn on the terminal with plain ANSI: Up/Down
//! (and k/j) move, Space ticks, Enter confirms; Esc, q and Ctrl-C cancel.
//! The terminal is in raw mode only while the list is up, and is put back
//! on every way out, a panic included (`RawModeGuard` and `Shown` restore
//! on drop).

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::AsFd;

use anyhow::{Context, Result};
use nix::sys::termios::{SetArg, SpecialCharacterIndices, tcgetattr, tcsetattr};

use super::style::Style;
use crate::pty::RawModeGuard;

/// How long after a lone ESC a following byte still makes it a key's
/// sequence (an arrow), not Esc: in tenths of a second (`VTIME`).
const ESC_WAIT_DECISECONDS: u8 = 1;

/// What comes within [`ESC_WAIT_DECISECONDS`] on `tty` (in raw mode),
/// read into `buf`; 0 bytes when nothing does. The tty's own timer
/// (`VMIN` 0, `VTIME`), since macOS's poll does not work on `/dev/tty`.
fn read_soon(tty: &File, buf: &mut [u8]) -> usize {
    let Ok(saved) = tcgetattr(tty.as_fd()) else {
        return 0;
    };
    let mut timed = saved.clone();
    timed.control_chars[SpecialCharacterIndices::VMIN as usize] = 0;
    timed.control_chars[SpecialCharacterIndices::VTIME as usize] = ESC_WAIT_DECISECONDS;
    if tcsetattr(tty.as_fd(), SetArg::TCSANOW, &timed).is_err() {
        return 0;
    }
    let mut input = tty;
    let n = input.read(buf).unwrap_or(0);
    let _ = tcsetattr(tty.as_fd(), SetArg::TCSANOW, &saved);
    n
}

/// A key the list acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    Toggle,
    Confirm,
    Cancel,
}

/// The keys in `bytes`, read from the terminal at once (a lone ESC at the
/// end is Esc: the caller waited for the rest of a sequence).
pub fn keys(bytes: &[u8]) -> Vec<Key> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let rest = &bytes[i..];
        let (key, used) = match rest {
            [0x1b, b'[' | b'O', b'A', ..] => (Some(Key::Up), 3),
            [0x1b, b'[' | b'O', b'B', ..] => (Some(Key::Down), 3),
            // Another sequence (Left, Right, a function key): skip it.
            [0x1b, b'[', ..] => {
                let end = rest[2..]
                    .iter()
                    .position(|byte| (0x40..=0x7e).contains(byte))
                    .map_or(rest.len(), |at| at + 3);
                (None, end)
            }
            [0x1b, b'O', _, ..] => (None, 3),
            [0x1b, ..] => (Some(Key::Cancel), 1),
            [b'k', ..] => (Some(Key::Up), 1),
            [b'j', ..] => (Some(Key::Down), 1),
            [b' ', ..] => (Some(Key::Toggle), 1),
            [b'\r' | b'\n', ..] => (Some(Key::Confirm), 1),
            [b'q' | 0x03, ..] => (Some(Key::Cancel), 1),
            _ => (None, 1),
        };
        out.extend(key);
        i += used;
    }
    out
}

/// The list's state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct List {
    pub ticked: Vec<bool>,
    pub at: usize,
}

impl List {
    /// Apply `key`; `Some(true)` confirms, `Some(false)` cancels.
    pub fn press(&mut self, key: Key) -> Option<bool> {
        let last = self.ticked.len().saturating_sub(1);
        match key {
            Key::Up => self.at = self.at.saturating_sub(1),
            Key::Down => self.at = (self.at + 1).min(last),
            Key::Toggle => {
                if let Some(ticked) = self.ticked.get_mut(self.at) {
                    *ticked = !*ticked;
                }
            }
            Key::Confirm => return Some(true),
            Key::Cancel => return Some(false),
        }
        None
    }

    /// The list's rows.
    pub fn rows(&self, labels: &[&str], style: &Style) -> Vec<String> {
        labels
            .iter()
            .zip(&self.ticked)
            .enumerate()
            .map(|(index, (label, ticked))| {
                let dot = if *ticked {
                    style.green(style.on())
                } else {
                    style.dim(style.off())
                };
                if index == self.at {
                    format!(
                        "  {} {dot} {}",
                        style.accent(style.cursor()),
                        style.bold(label)
                    )
                } else {
                    format!("    {dot} {label}")
                }
            })
            .collect()
    }
}

/// What the list says around its rows.
pub struct Screen<'a> {
    pub style: &'a Style,
    pub labels: &'a [&'a str],
    /// The files the run will change, for the line under the hint.
    pub files: String,
    /// Kiro or Amazon Q is set up too.
    pub kiro: bool,
}

impl Screen<'_> {
    /// Every line of the list with `list`'s state: a blank line around
    /// each part and a 2-space margin.
    pub fn frame(&self, list: &List) -> Vec<String> {
        let style = self.style;
        let dot = style.dot();
        let hint = format!(
            "  {} move {dot} space toggle {dot} enter confirm {dot} esc cancel",
            style.updown()
        );
        let mut lines = vec![
            String::new(),
            format!(
                "  {}{}",
                style.bold_accent("fac"),
                style.dim(&format!(" {} autocomplete for your terminal", style.dot()))
            ),
            String::new(),
            "  Start fac in new windows of:".to_owned(),
            String::new(),
        ];
        lines.extend(list.rows(self.labels, style));
        lines.push(String::new());
        lines.push(style.dim(&hint));
        lines.push(style.dim(&format!("  Adds a short block to {}.", self.files)));
        if self.kiro {
            let note = style
                .note("Kiro is set up too: fac will start instead of it in the ticked terminals.");
            lines.extend(note.split('\n').map(str::to_owned));
        }
        lines.push(String::new());
        lines
    }
}

/// Shows the cursor again on drop.
struct Shown<'a>(&'a File);

impl Drop for Shown<'_> {
    fn drop(&mut self) {
        let mut tty = self.0;
        let _ = tty.write_all(b"\x1b[?25h");
        let _ = tty.flush();
    }
}

/// Run the list on `tty` with `screen`, starting `ticked`. `Some`: what
/// was ticked at Enter; `None`: cancelled. The list is erased either way,
/// the cursor left where it began.
pub fn run(tty: &File, screen: &Screen, ticked: Vec<bool>) -> Result<Option<Vec<bool>>> {
    let mut list = List { ticked, at: 0 };
    let raw = RawModeGuard::enable_on(tty.as_fd())?;
    let shown = Shown(tty);
    let mut out = tty;
    let height = screen.frame(&list).len();
    let draw = |out: &mut &File, list: &List, again: bool| -> std::io::Result<()> {
        let mut text = String::new();
        if again {
            // Back to the list's first line.
            text.push_str(&format!("\r\x1b[{height}A"));
        } else {
            text.push_str("\x1b[?25l");
        }
        for line in screen.frame(list) {
            text.push_str(&format!("\r\x1b[2K{line}\r\n"));
        }
        out.write_all(text.as_bytes())?;
        out.flush()
    };
    draw(&mut out, &list, false).context("drawing the list")?;
    let mut input = tty;
    let mut buf = [0u8; 64];
    let confirmed = 'keys: loop {
        let n = match input.read(&mut buf) {
            Ok(0) => break false,
            Ok(n) => n,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err).context("reading a key"),
        };
        let mut bytes = buf[..n].to_vec();
        // A lone ESC: wait a moment for the rest of an arrow's sequence.
        if bytes.last() == Some(&0x1b) {
            let more = read_soon(tty, &mut buf);
            bytes.extend_from_slice(&buf[..more]);
        }
        for key in keys(&bytes) {
            if let Some(confirmed) = list.press(key) {
                break 'keys confirmed;
            }
        }
        draw(&mut out, &list, true).context("drawing the list")?;
    };
    // Erase the list: back to its first line, then clear to the end.
    let _ = out.write_all(format!("\r\x1b[{height}A\x1b[J").as_bytes());
    let _ = out.flush();
    drop(shown);
    drop(raw);
    Ok(confirmed.then_some(list.ticked))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_from_bytes() {
        assert_eq!(keys(b"\x1b[A\x1b[B"), [Key::Up, Key::Down]);
        assert_eq!(keys(b"\x1bOA\x1bOB"), [Key::Up, Key::Down]);
        assert_eq!(
            keys(b"kj \r"),
            [Key::Up, Key::Down, Key::Toggle, Key::Confirm]
        );
        assert_eq!(keys(b"\n"), [Key::Confirm]);
        assert_eq!(keys(b"\x1b"), [Key::Cancel]);
        assert_eq!(keys(b"q"), [Key::Cancel]);
        assert_eq!(keys(b"\x03"), [Key::Cancel]);
        // Left, Right, Delete and other keys do nothing.
        assert_eq!(keys(b"\x1b[C\x1b[D\x1b[3~xa\x1bOP"), []);
    }

    #[test]
    fn the_list_moves_ticks_and_stops_at_its_ends() {
        let mut list = List {
            ticked: vec![true; 3],
            at: 0,
        };
        assert_eq!(list.press(Key::Up), None);
        assert_eq!(list.at, 0);
        list.press(Key::Down);
        list.press(Key::Toggle);
        assert_eq!(list.ticked, [true, false, true]);
        list.press(Key::Down);
        list.press(Key::Down);
        assert_eq!(list.at, 2);
        let style = Style::new(false, false, None);
        assert_eq!(
            list.rows(&["a", "b", "c"], &style),
            ["    * a", "    o b", "  > * c"]
        );
        assert_eq!(list.press(Key::Confirm), Some(true));
        assert_eq!(list.press(Key::Cancel), Some(false));
    }
}
