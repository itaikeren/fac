//! Bulk command output. vt100 is by far the slowest part of fac, so once a
//! command has printed more than `BULK_AFTER` bytes its output is still
//! forwarded but no longer fed to the screen model. Only a tail of it is
//! kept, and that tail is fed to the model at the next flush, which comes
//! before anything reads the model: before every output event, on resize,
//! on input, at exit, and once the output has been quiet for `BULK_QUIET`.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use memchr::{memchr, memrchr};

use super::scanner::{OutputEvent, PromptMark};
use super::screen::ScreenModel;

/// Past this many bytes since 133;C, a command's output skips the model.
pub const BULK_AFTER: usize = 512 * 1024;
/// How much of the skipped output is kept for the model, at least.
pub const BULK_TAIL: usize = 256 * 1024;
/// Skipped output quiet this long is flushed into the model.
pub const BULK_QUIET: Duration = Duration::from_millis(20);

/// CAN: aborts a half-parsed sequence.
const CAN: u8 = 0x18;

/// The tail kept for a `rows` x `cols` screen: `BULK_TAIL`, and never less
/// than 4 bytes per cell, so it covers a whole screen.
pub fn tail_cap(rows: u16, cols: u16) -> usize {
    BULK_TAIL.max(usize::from(rows) * usize::from(cols) * 4)
}

/// The last bytes of skipped output, at most `cap` of them.
pub struct Tail {
    bytes: VecDeque<u8>,
    cap: usize,
    /// The last byte dropped from the front, if any was.
    dropped: Option<u8>,
    /// Bytes since the last `\n` of the stream, dropped ones included.
    line_bytes: usize,
}

impl Tail {
    /// `column`: the model's cursor column where the tail starts.
    pub fn new(cap: usize, column: usize) -> Self {
        Self {
            bytes: VecDeque::new(),
            cap: cap.max(1),
            dropped: None,
            line_bytes: column,
        }
    }

    pub fn push(&mut self, bytes: &[u8]) {
        match memrchr(b'\n', bytes) {
            Some(newline) => self.line_bytes = bytes.len() - newline - 1,
            None => self.line_bytes = self.line_bytes.saturating_add(bytes.len()),
        }
        let skip = bytes.len().saturating_sub(self.cap);
        if skip > 0 {
            self.dropped = Some(bytes[skip - 1]);
            self.bytes.clear();
        }
        self.bytes.extend(&bytes[skip..]);
        let excess = self.bytes.len().saturating_sub(self.cap);
        if excess > 0 {
            self.dropped = self.bytes.get(excess - 1).copied();
            self.bytes.drain(..excess);
        }
    }

    /// What the model is fed from the tail on a `cols`-wide screen: a
    /// carriage return first or not, and where in the tail to start.
    ///
    /// Untrimmed, the tail is all of it. Trimmed, it starts after the first
    /// `\n`, at a line start. Without a `\n` it is one long line: the start
    /// is chosen so the model ends in the column the line's length gives,
    /// right for plain single-width text.
    fn start(&self, cols: u16) -> (bool, usize) {
        if matches!(self.dropped, None | Some(b'\n')) {
            return (false, 0);
        }
        let (front, back) = self.bytes.as_slices();
        let newline =
            memchr(b'\n', front).or_else(|| memchr(b'\n', back).map(|at| front.len() + at));
        if let Some(newline) = newline {
            return (false, newline + 1);
        }
        let (len, cols) = (self.bytes.len(), usize::from(cols.max(1)));
        let target = self.line_bytes % cols;
        let mut start = (len % cols + cols - target) % cols;
        // Not inside a UTF-8 character.
        while self
            .bytes
            .get(start)
            .is_some_and(|&byte| byte & 0xc0 == 0x80)
        {
            start += 1;
        }
        (true, start)
    }

    /// The bytes the model is fed for this tail on a `cols`-wide screen.
    pub fn modelled(&self, cols: u16) -> Vec<u8> {
        let (cr, start) = self.start(cols);
        let mut bytes = Vec::with_capacity(self.bytes.len() - start + 2);
        bytes.push(CAN);
        if cr {
            bytes.push(b'\r');
        }
        bytes.extend(self.bytes.range(start..));
        bytes
    }
}

/// Whether a command's output is modelled, and the tail while it is not.
#[derive(Default)]
pub struct Bulk {
    /// A command runs: after 133;C, before the next 133;A.
    command: bool,
    /// The scanner saw the alternate screen turned on (and not off).
    alt_screen: bool,
    /// Bytes forwarded since 133;C.
    forwarded: usize,
    /// In bulk mode: the skipped output, and when it last grew.
    tail: Option<(Tail, Instant)>,
}

impl Bulk {
    pub fn new() -> Self {
        Self::default()
    }

    /// Output is being skipped.
    pub fn active(&self) -> bool {
        self.tail.is_some()
    }

    /// An output event, after `flush`.
    pub fn on_event(&mut self, event: &OutputEvent) {
        match event {
            OutputEvent::Prompt(PromptMark::C) => {
                self.command = true;
                self.forwarded = 0;
            }
            OutputEvent::Prompt(PromptMark::A) => self.command = false,
            OutputEvent::AltScreen(on) => self.alt_screen = *on,
            _ => {}
        }
    }

    /// Shell bytes forwarded at `now`: fed to `model`, or kept in the tail
    /// in bulk mode. `may_start`: whether bulk mode may start here (the
    /// popup is idle). It starts at a `\n`, which the model still takes, so
    /// the model holds no half character when it stops being fed.
    pub fn process(
        &mut self,
        bytes: &[u8],
        model: &mut ScreenModel,
        may_start: bool,
        now: Instant,
    ) {
        self.forwarded = self.forwarded.saturating_add(bytes.len());
        if let Some((tail, last)) = &mut self.tail {
            tail.push(bytes);
            *last = now;
            return;
        }
        let newline =
            (self.command && !self.alt_screen && may_start && self.forwarded > BULK_AFTER)
                .then(|| memrchr(b'\n', bytes))
                .flatten();
        let Some(newline) = newline else {
            model.process(bytes);
            return;
        };
        let (modelled, skipped) = bytes.split_at(newline + 1);
        model.process(modelled);
        let (rows, cols) = model.screen().size();
        let column = usize::from(model.screen().cursor_position().1);
        let mut tail = Tail::new(tail_cap(rows.get(), cols.get()), column);
        tail.push(skipped);
        tracing::debug!("bulk output on after {} bytes", self.forwarded);
        self.tail = Some((tail, now));
    }

    /// Leave bulk mode: feed CAN and the tail to the model.
    pub fn flush(&mut self, model: &mut ScreenModel) {
        let Some((tail, _)) = self.tail.take() else {
            return;
        };
        let bytes = tail.modelled(model.screen().size().1.get());
        model.process(&bytes);
        tracing::debug!("bulk output flushed tail={}", bytes.len() - 1);
    }

    /// When skipped output will have been quiet for `BULK_QUIET`.
    pub fn deadline(&self) -> Option<Instant> {
        self.tail.as_ref().map(|(_, last)| *last + BULK_QUIET)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the model would be fed, without the leading CAN.
    fn fed(tail: &Tail, cols: u16) -> Vec<u8> {
        let bytes = tail.modelled(cols);
        assert_eq!(bytes.first(), Some(&CAN));
        bytes[1..].to_vec()
    }

    #[test]
    fn an_untrimmed_tail_is_fed_whole() {
        let mut tail = Tail::new(64, 0);
        tail.push(b"partial line");
        tail.push(b" and more\nnext");
        assert_eq!(fed(&tail, 80), b"partial line and more\nnext");
    }

    #[test]
    fn a_trimmed_tail_starts_at_a_line_start() {
        let mut tail = Tail::new(14, 0);
        tail.push(b"0000\n1111\n2222\n3333\n");
        // The last 14 bytes start inside "1111"; the rest of it is dropped too.
        assert_eq!(tail.bytes.len(), 14);
        assert_eq!(fed(&tail, 80), b"2222\n3333\n");
        // Pushed in pieces, the same cut.
        let mut tail = Tail::new(14, 0);
        for piece in [b"0000\n".as_slice(), b"1111\n", b"2222\n", b"3333\n"] {
            tail.push(piece);
        }
        assert_eq!(fed(&tail, 80), b"2222\n3333\n");
    }

    #[test]
    fn a_cut_right_after_a_newline_keeps_the_first_line() {
        let mut tail = Tail::new(10, 0);
        tail.push(b"0000\n1111\n2222\n");
        assert_eq!(fed(&tail, 80), b"1111\n2222\n");
    }

    #[test]
    fn the_tail_never_holds_more_than_its_cap() {
        let mut tail = Tail::new(100, 0);
        for n in 0..1000 {
            tail.push(format!("line {n}\n").as_bytes());
            assert!(tail.bytes.len() <= 100);
        }
        let big = vec![b'x'; 500];
        tail.push(&big);
        assert_eq!(tail.bytes.len(), 100);
    }

    #[test]
    fn the_cap_covers_a_whole_screen() {
        assert_eq!(tail_cap(24, 80), BULK_TAIL);
        assert_eq!(tail_cap(500, 500), 500 * 500 * 4);
        assert!(tail_cap(1, 1) >= 4);
    }

    #[test]
    fn one_long_line_ends_in_its_column() {
        let cols = 80u16;
        for len in [1000usize, 1003, 1040, 1079, 1080] {
            let mut tail = Tail::new(200, 0);
            for chunk in vec![b'x'; len].chunks(64) {
                tail.push(chunk);
            }
            let bytes = fed(&tail, cols);
            assert_eq!(bytes[0], b'\r', "len {len}");
            let printed = bytes.len() - 1;
            assert!(printed <= 200 && printed > 200 - usize::from(cols));
            assert_eq!(printed % 80, len % 80, "len {len}");
        }
        // The line started in column 5.
        let mut tail = Tail::new(200, 5);
        tail.push(&[b'x'; 1000]);
        assert_eq!((fed(&tail, cols).len() - 1) % 80, 1005 % 80);
    }

    #[test]
    fn a_long_line_after_lines_is_measured_from_its_newline() {
        let mut tail = Tail::new(200, 0);
        tail.push(b"short\n");
        tail.push(&[b'y'; 1003]);
        let bytes = fed(&tail, 80);
        assert_eq!(bytes[0], b'\r');
        assert_eq!((bytes.len() - 1) % 80, 1003 % 80);
    }

    #[test]
    fn a_long_line_cut_skips_utf8_continuation_bytes() {
        let mut tail = Tail::new(30, 0);
        tail.push("é".repeat(100).as_bytes());
        let bytes = fed(&tail, 7);
        assert!(std::str::from_utf8(&bytes[1..]).is_ok());
    }

    fn scanner_event(mark: PromptMark) -> OutputEvent {
        OutputEvent::Prompt(mark)
    }

    #[test]
    fn bulk_starts_only_inside_a_command_on_the_main_screen() {
        let now = Instant::now();
        let mut model = ScreenModel::new(24, 80);
        let line = [b"x".repeat(1023).as_slice(), b"\n"].concat();
        let mut bulk = Bulk::new();
        // No command: always modelled.
        for _ in 0..1024 {
            bulk.process(&line, &mut model, true, now);
        }
        assert!(!bulk.active());
        bulk.on_event(&scanner_event(PromptMark::C));
        bulk.on_event(&OutputEvent::AltScreen(true));
        for _ in 0..1024 {
            bulk.process(&line, &mut model, true, now);
        }
        assert!(!bulk.active(), "alternate screen");
        bulk.on_event(&OutputEvent::AltScreen(false));
        bulk.process(&line, &mut model, false, now);
        assert!(!bulk.active(), "the popup is not idle");
        bulk.process(&line, &mut model, true, now);
        assert!(bulk.active());
        assert_eq!(bulk.deadline(), Some(now + BULK_QUIET));
        bulk.flush(&mut model);
        assert!(!bulk.active());
        // Still in the same command: on again with the next bytes.
        bulk.process(&line, &mut model, true, now);
        assert!(bulk.active());
        bulk.flush(&mut model);
        bulk.on_event(&scanner_event(PromptMark::A));
        bulk.process(&line, &mut model, true, now);
        assert!(!bulk.active());
        // A new command counts from zero.
        bulk.on_event(&scanner_event(PromptMark::C));
        bulk.process(&line, &mut model, true, now);
        assert!(!bulk.active());
    }

    #[test]
    fn bulk_starts_after_a_newline_and_the_model_catches_up() {
        let now = Instant::now();
        let mut model = ScreenModel::new(4, 20);
        let mut exact = ScreenModel::new(4, 20);
        let mut bulk = Bulk::new();
        bulk.on_event(&scanner_event(PromptMark::C));
        let mut stream = Vec::new();
        let mut n = 0;
        while stream.len() < BULK_AFTER + 1024 * 1024 {
            stream.extend_from_slice(format!("{n} \x1b[32m日本\x1b[0m\r\n").as_bytes());
            n += 1;
        }
        stream.extend_from_slice(b"no newline yet");
        // Odd-sized reads, so they split characters and sequences.
        for chunk in stream.chunks(4093) {
            bulk.process(chunk, &mut model, true, now);
            exact.process(chunk);
        }
        assert!(bulk.active());
        bulk.flush(&mut model);
        assert_eq!(model.cursor(), exact.cursor());
        assert_eq!(model.screen().contents(), exact.screen().contents());
    }
}
