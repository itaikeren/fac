//! A streaming scanner over the shell's output. It finds the sequences fac
//! reacts to and where in the stream they sit, and strips fac's own private
//! OSC messages (7790..=7793). It is the only parser of those messages.
//!
//! Byte flow: every forwarded byte appears in exactly one `Bytes` segment, in
//! stream order. The `Event` of a forwarded sequence comes right after the
//! `Bytes` segment that ends with that sequence's last byte, so feeding the
//! `Bytes` segments to the screen model and to stdout keeps both exact, and
//! anything appended after an `Event` lands right after the sequence. A
//! private OSC appears in no `Bytes` segment, only as its `Event`. One
//! exception: a watched cursor move (`Move::Cursor`) comes right before the
//! `Bytes` segment that starts with its sequence.

use memchr::memchr;

use crate::term::caps;
use crate::term::replies::ReplyKind;

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;
/// CAN and SUB abort a sequence in progress, as terminals do.
const CAN: u8 = 0x18;
const SUB: u8 = 0x1a;
/// A sequence still open past this many bytes is given up on and forwarded.
const MAX_SEQUENCE: usize = 1024 * 1024;
/// CSI parameters past this many are not looked at.
const MAX_PARAMS: usize = 32;

/// An OSC 133 prompt marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptMark {
    /// Prompt start.
    A,
    /// Prompt end, where the command line starts.
    B,
    /// Command output start.
    C,
    /// Command finished, with its exit status when given.
    D(Option<i32>),
    /// Any other marker (e.g. `P;k=s`), everything after `133;`.
    Other(Vec<u8>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputEvent {
    /// OSC 133. Forwarded.
    Prompt(PromptMark),
    /// OSC 7 `file://host/path`, the path percent-decoded. Forwarded.
    Cwd(String),
    /// OSC 7790..=7793, fields split on `;` and still percent-encoded. Stripped.
    Private { code: u16, fields: Vec<Vec<u8>> },
    /// `CSI ? 1049/1047/47 h|l`, anywhere in a parameter list. Forwarded.
    AltScreen(bool),
    /// `CSI 6n` or `CSI ? 6n` from the shell side. Forwarded.
    ShellCursorQuery,
    /// The shell's queries of the kinds `ReplyQueues` tracks, in order:
    /// DA1 (`CSI c`, `CSI 0 c`), DECRQM (`CSI ? Pd $ p`), `OSC 11 ; ?`, and
    /// `OSC 4 ; n ; ?` with one entry per index. Forwarded.
    ShellQueries(Vec<ReplyKind>),
    /// The shell saved its cursor (true: DECSC, `ESC 7`, or SCOSC, `CSI s`
    /// while DECLRMM is off) or no longer has one saved (false: DECRC,
    /// `ESC 8`, SCORC, `CSI u`, or RIS, `ESC c`). Forwarded.
    ShellCursorSaved(bool),
    /// Only while watching (`OutputScanner::set_watch`): a sequence that can
    /// reach cells off the cursor's row. Forwarded.
    Moves(Move),
}

/// What a watched sequence can do off the cursor's row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Move {
    /// Rows scroll or move: IND, NEL, RI, IL, DL, SU, SD, DECSTBM, RIS, and
    /// LF, VT or FF inside a CSI.
    Rows,
    /// Cells off the cursor's row are erased where they are: ED.
    Erase,
    /// LF, VT or FF: the cursor goes down a row, or at a bottom margin the
    /// rows scroll. The event comes right before a `Bytes` segment that
    /// holds that byte alone, so the caller can tell the two apart by the
    /// cursor's row after it.
    LineFeed,
    /// The cursor may go to another row: CUP, HVP, VPA, VPR, CUU, CUD, CNL,
    /// CPL, DECRC, SCORC. The event comes before the sequence's bytes and a
    /// new `Bytes` segment starts with them, so the caller can compare the
    /// cursor's row before the move (a wrap it would hide) and after it.
    Cursor,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Segment<'a> {
    /// Bytes to forward verbatim.
    Bytes(&'a [u8]),
    Event(OutputEvent),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Ground,
    /// After ESC.
    Escape,
    /// After `ESC [`.
    Csi,
    /// After `ESC ]`.
    Osc,
    /// An ESC inside an OSC: ST if `\` follows, else the OSC is aborted.
    OscEscape,
}

/// What a byte inside a sequence did to it.
enum Step {
    Continue,
    /// The sequence ended (with this byte) and is nothing fac looks at.
    Plain,
    /// This byte is an ESC that aborts the sequence and starts a new one.
    RestartHere,
    /// The ESC before this byte aborted the OSC; this byte is rescanned after it.
    RestartBefore,
    /// A complete CSI with this final byte.
    CsiDone(u8),
    /// A complete two-byte escape (`ESC` and this byte) fac looks at.
    EscDone(u8),
    /// A complete OSC; the terminator is this many bytes long.
    OscDone(usize),
}

/// What happens to a complete sequence's bytes.
enum Outcome {
    /// Forwarded. `before` comes ahead of the sequence's bytes (a watched
    /// cursor move), `after` right after them; most sequences make neither.
    Forward {
        before: Option<OutputEvent>,
        after: Vec<OutputEvent>,
    },
    Strip(OutputEvent),
}

/// Where the current `feed` stands in its input.
struct Pos {
    /// Start of the forwarded run not yet emitted.
    run: usize,
    /// Where the open sequence began in the input; `None` while it began in
    /// an earlier `feed` (its first bytes are in `carry`).
    start: Option<usize>,
}

pub struct OutputScanner {
    state: State,
    /// The bytes of a sequence that began in an earlier `feed`.
    carry: Vec<u8>,
    /// Bytes seen after `ESC [`.
    csi_len: usize,
    /// The private marker (`<=>?`) right after `ESC [`.
    csi_private: Option<u8>,
    csi_params: [u32; MAX_PARAMS],
    /// Number of parameters, 0 when there are none.
    csi_count: usize,
    /// The intermediate byte (`0x20..=0x2f`), if there is one.
    csi_intermediate: Option<u8>,
    /// A malformed CSI, which matches nothing.
    csi_invalid: bool,
    /// A line feed (LF, VT, FF) inside the open CSI: the terminal executes it.
    csi_line_feed: bool,
    /// `Move` events are made (`set_watch`).
    watch: bool,
    /// DECLRMM (`CSI ? 69 h`) is on: `CSI s` sets margins, not a save.
    declrmm: bool,
}

impl Default for OutputScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl OutputScanner {
    pub fn new() -> Self {
        Self {
            state: State::Ground,
            carry: Vec::new(),
            csi_len: 0,
            csi_private: None,
            csi_params: [0; MAX_PARAMS],
            csi_count: 0,
            csi_intermediate: None,
            csi_invalid: false,
            csi_line_feed: false,
            watch: false,
            declrmm: false,
        }
    }

    /// Make `OutputEvent::Moves` events (from the next byte on), or stop.
    /// Off by default: line feeds then cost nothing.
    pub fn set_watch(&mut self, on: bool) {
        self.watch = on;
    }

    /// Calls `out` with segments in stream order. Bytes segments are to be
    /// forwarded verbatim.
    pub fn feed(&mut self, input: &[u8], out: &mut impl FnMut(Segment<'_>)) {
        let mut pos = Pos {
            run: 0,
            start: None,
        };
        let mut i = 0;
        while i < input.len() {
            match self.state {
                State::Ground => {
                    let next = if self.watch {
                        input[i..]
                            .iter()
                            .position(|&byte| byte == ESC || is_line_feed(byte))
                    } else {
                        memchr(ESC, &input[i..])
                    };
                    match next {
                        Some(off) if input[i + off] == ESC => {
                            i += off;
                            pos.start = Some(i);
                            self.state = State::Escape;
                            i += 1;
                        }
                        Some(off) => {
                            // A watched line feed: the bytes before it, its
                            // event, then the byte alone.
                            i += off;
                            if pos.run < i {
                                out(Segment::Bytes(&input[pos.run..i]));
                            }
                            out(Segment::Event(OutputEvent::Moves(Move::LineFeed)));
                            out(Segment::Bytes(&input[i..=i]));
                            i += 1;
                            pos.run = i;
                        }
                        None => i = input.len(),
                    }
                }
                State::Osc => match input[i..]
                    .iter()
                    .position(|&byte| matches!(byte, BEL | ESC | CAN | SUB))
                {
                    Some(off) => {
                        i += off;
                        let byte = input[i];
                        i += 1;
                        match byte {
                            BEL => self.finish_osc(input, &mut pos, i, 1, out),
                            ESC => self.state = State::OscEscape,
                            // Aborted: forwarded as it is, nothing fac reads.
                            _ => self.complete(input, &mut pos, i, forward(), out),
                        }
                    }
                    None => i = input.len(),
                },
                _ => {
                    let byte = input[i];
                    i += 1;
                    match self.step(byte) {
                        Step::Continue => {}
                        Step::Plain => self.complete(input, &mut pos, i, forward(), out),
                        Step::RestartHere => {
                            self.complete(input, &mut pos, i - 1, forward(), out);
                            pos.start = Some(i - 1);
                            self.state = State::Escape;
                        }
                        Step::RestartBefore => {
                            if i >= 2 {
                                // The ESC was read in this feed, at i - 2.
                                self.complete(input, &mut pos, i - 2, forward(), out);
                                pos.start = Some(i - 2);
                            } else {
                                // The ESC is the last carried byte.
                                let keep = self.carry.len().saturating_sub(1);
                                if keep > 0 {
                                    out(Segment::Bytes(&self.carry[..keep]));
                                }
                                self.carry.drain(..keep);
                            }
                            self.state = State::Escape;
                            i -= 1;
                        }
                        Step::CsiDone(final_byte) => {
                            let outcome = self.csi_outcome(final_byte);
                            self.complete(input, &mut pos, i, outcome, out);
                        }
                        Step::EscDone(byte) => {
                            let outcome = self.esc_outcome(byte);
                            self.complete(input, &mut pos, i, outcome, out);
                        }
                        Step::OscDone(terminator) => {
                            self.finish_osc(input, &mut pos, i, terminator, out);
                        }
                    }
                }
            }
        }

        if self.state == State::Ground {
            if pos.run < input.len() {
                out(Segment::Bytes(&input[pos.run..]));
            }
            return;
        }
        match pos.start {
            Some(start) => {
                if pos.run < start {
                    out(Segment::Bytes(&input[pos.run..start]));
                }
                self.carry.clear();
                self.carry.extend_from_slice(&input[start..]);
            }
            None => self.carry.extend_from_slice(input),
        }
        if self.carry.len() > MAX_SEQUENCE {
            out(Segment::Bytes(&self.carry));
            self.carry.clear();
            self.state = State::Ground;
        }
    }

    /// Advance the open sequence by one byte (never called in `Ground` or `Osc`).
    fn step(&mut self, byte: u8) -> Step {
        match self.state {
            // CAN and SUB end up in the `_` arm: the sequence is aborted.
            State::Escape => match byte {
                b'[' => {
                    self.state = State::Csi;
                    self.csi_len = 0;
                    self.csi_private = None;
                    self.csi_count = 0;
                    self.csi_intermediate = None;
                    self.csi_invalid = false;
                    self.csi_line_feed = false;
                    Step::Continue
                }
                b']' => {
                    self.state = State::Osc;
                    Step::Continue
                }
                ESC => Step::RestartHere,
                b'7' | b'8' | b'c' | b'D' | b'E' | b'M' => Step::EscDone(byte),
                _ => Step::Plain,
            },
            State::Csi => {
                let first = self.csi_len == 0;
                self.csi_len += 1;
                match byte {
                    b'0'..=b'9' => {
                        if self.csi_intermediate.is_some() {
                            self.csi_invalid = true;
                        }
                        if self.csi_count == 0 {
                            self.csi_count = 1;
                            self.csi_params[0] = 0;
                        }
                        if let Some(param) = self.csi_params.get_mut(self.csi_count - 1) {
                            *param = param
                                .saturating_mul(10)
                                .saturating_add(u32::from(byte - b'0'));
                        }
                        Step::Continue
                    }
                    b';' => {
                        if self.csi_intermediate.is_some() {
                            self.csi_invalid = true;
                        }
                        // `;` closes one parameter and opens the next.
                        self.csi_count = self.csi_count.max(1) + 1;
                        if let Some(param) = self.csi_params.get_mut(self.csi_count - 1) {
                            *param = 0;
                        }
                        Step::Continue
                    }
                    0x3c..=0x3f if first => {
                        self.csi_private = Some(byte);
                        Step::Continue
                    }
                    b':' | 0x3c..=0x3f => {
                        self.csi_invalid = true;
                        Step::Continue
                    }
                    0x20..=0x2f => {
                        // Only one intermediate is looked at.
                        if self.csi_intermediate.replace(byte).is_some() {
                            self.csi_invalid = true;
                        }
                        Step::Continue
                    }
                    0x40..=0x7e => Step::CsiDone(byte),
                    ESC => Step::RestartHere,
                    CAN | SUB => Step::Plain,
                    // C0 controls inside a CSI are executed by the terminal
                    // and the CSI goes on.
                    0x00..=0x1f | 0x7f => {
                        self.csi_line_feed |= is_line_feed(byte);
                        Step::Continue
                    }
                    _ => Step::Plain,
                }
            }
            State::OscEscape => {
                if byte == b'\\' {
                    Step::OscDone(2)
                } else {
                    Step::RestartBefore
                }
            }
            State::Ground | State::Osc => Step::Continue,
        }
    }

    /// What becomes of a complete CSI with `final_byte`.
    fn csi_outcome(&mut self, final_byte: u8) -> Outcome {
        let mut after: Vec<OutputEvent> = self.csi_event(final_byte).into_iter().collect();
        let mut before = None;
        if self.watch && !self.csi_invalid {
            match (self.csi_private, self.csi_intermediate, final_byte) {
                // ED (and DECSED).
                (None | Some(b'?'), None, b'J') => {
                    after.push(OutputEvent::Moves(Move::Erase));
                }
                // IL, DL, SU, SD, DECSTBM.
                (None, None, b'L' | b'M' | b'S' | b'T' | b'r') => {
                    after.push(OutputEvent::Moves(Move::Rows));
                }
                // CUU, CUD, CNL, CPL, CUP, HVP, VPA, VPR, SCORC.
                (None, None, b'A' | b'B' | b'E' | b'F' | b'H' | b'f' | b'd' | b'e') => {
                    before = Some(OutputEvent::Moves(Move::Cursor));
                }
                (None, None, b'u') if self.csi_count == 0 => {
                    before = Some(OutputEvent::Moves(Move::Cursor));
                }
                _ => {}
            }
        }
        if self.watch && self.csi_line_feed {
            after.push(OutputEvent::Moves(Move::Rows));
        }
        Outcome::Forward { before, after }
    }

    /// What becomes of `ESC` followed by `byte`.
    fn esc_outcome(&self, byte: u8) -> Outcome {
        let mut after = Vec::new();
        let mut before = None;
        match byte {
            b'7' => after.push(OutputEvent::ShellCursorSaved(true)),
            b'8' => {
                if self.watch {
                    before = Some(OutputEvent::Moves(Move::Cursor));
                }
                after.push(OutputEvent::ShellCursorSaved(false));
            }
            b'c' => {
                after.push(OutputEvent::ShellCursorSaved(false));
                if self.watch {
                    after.push(OutputEvent::Moves(Move::Rows));
                }
            }
            // IND, NEL, RI.
            _ if self.watch => after.push(OutputEvent::Moves(Move::Rows)),
            _ => {}
        }
        Outcome::Forward { before, after }
    }

    fn csi_event(&mut self, final_byte: u8) -> Option<OutputEvent> {
        if self.csi_invalid {
            return None;
        }
        let params = &self.csi_params[..self.csi_count.min(MAX_PARAMS)];
        match (self.csi_private, self.csi_intermediate, final_byte) {
            (None | Some(b'?'), None, b'n') if params == [6] => Some(OutputEvent::ShellCursorQuery),
            (Some(b'?'), None, b'h' | b'l')
                if params.iter().any(|param| matches!(param, 47 | 1047 | 1049)) =>
            {
                if params.contains(&69) {
                    self.declrmm = final_byte == b'h';
                }
                Some(OutputEvent::AltScreen(final_byte == b'h'))
            }
            (Some(b'?'), None, b'h' | b'l') => {
                if params.contains(&69) {
                    self.declrmm = final_byte == b'h';
                }
                None
            }
            (None, None, b's') if params.is_empty() && !self.declrmm => {
                Some(OutputEvent::ShellCursorSaved(true))
            }
            (None, None, b'u') if params.is_empty() => Some(OutputEvent::ShellCursorSaved(false)),
            (None, None, b'c') if matches!(params, [] | [0]) => {
                Some(OutputEvent::ShellQueries(vec![ReplyKind::DeviceAttrs]))
            }
            (Some(b'?'), Some(b'$'), b'p') => match *params {
                [mode] => u16::try_from(mode)
                    .ok()
                    .map(|mode| OutputEvent::ShellQueries(vec![ReplyKind::Mode(mode)])),
                _ => None,
            },
            _ => None,
        }
    }

    /// A complete OSC ending at `end`, with a terminator of `terminator` bytes.
    fn finish_osc(
        &mut self,
        input: &[u8],
        pos: &mut Pos,
        end: usize,
        terminator: usize,
        out: &mut impl FnMut(Segment<'_>),
    ) {
        let joined;
        let sequence = match pos.start {
            Some(start) => &input[start..end],
            None => {
                joined = [&self.carry[..], &input[..end]].concat();
                &joined[..]
            }
        };
        // `ESC ]` .. terminator
        let payload = &sequence[2..sequence.len() - terminator];
        let outcome = classify_osc(payload);
        self.complete(input, pos, end, outcome, out);
    }

    /// The open sequence ended at `end` (exclusive) with `outcome`.
    fn complete(
        &mut self,
        input: &[u8],
        pos: &mut Pos,
        end: usize,
        outcome: Outcome,
        out: &mut impl FnMut(Segment<'_>),
    ) {
        match pos.start {
            Some(start) => match outcome {
                Outcome::Forward { before, after } => {
                    if let Some(event) = before {
                        if pos.run < start {
                            out(Segment::Bytes(&input[pos.run..start]));
                        }
                        out(Segment::Event(event));
                        pos.run = start;
                    }
                    if !after.is_empty() {
                        out(Segment::Bytes(&input[pos.run..end]));
                        for event in after {
                            out(Segment::Event(event));
                        }
                        pos.run = end;
                    }
                }
                Outcome::Strip(event) => {
                    if pos.run < start {
                        out(Segment::Bytes(&input[pos.run..start]));
                    }
                    out(Segment::Event(event));
                    pos.run = end;
                }
            },
            None => {
                // `run` is 0 here: nothing of this input has been emitted.
                let events = match outcome {
                    Outcome::Forward { before, after } => {
                        // What came before the sequence went out with the
                        // earlier feed.
                        if let Some(event) = before {
                            out(Segment::Event(event));
                        }
                        self.carry.extend_from_slice(&input[..end]);
                        if !self.carry.is_empty() {
                            out(Segment::Bytes(&self.carry));
                        }
                        after
                    }
                    Outcome::Strip(event) => vec![event],
                };
                self.carry.clear();
                for event in events {
                    out(Segment::Event(event));
                }
                pos.run = end;
            }
        }
        pos.start = None;
        self.state = State::Ground;
    }
}

/// Forwarded, with no event.
fn forward() -> Outcome {
    forward_with(Vec::new())
}

/// Forwarded, with these events after it.
fn forward_with(after: Vec<OutputEvent>) -> Outcome {
    Outcome::Forward {
        before: None,
        after,
    }
}

/// LF, VT and FF: the terminal moves down a row (scrolling at the bottom).
fn is_line_feed(byte: u8) -> bool {
    matches!(byte, b'\n' | 0x0b | 0x0c)
}

fn classify_osc(payload: &[u8]) -> Outcome {
    let (number, rest) = match memchr(b';', payload) {
        Some(at) => (&payload[..at], Some(&payload[at + 1..])),
        None => (payload, None),
    };
    match number {
        b"133" => forward_with(vec![OutputEvent::Prompt(prompt_mark(
            rest.unwrap_or_default(),
        ))]),
        b"7" => forward_with(
            rest.and_then(cwd)
                .map(OutputEvent::Cwd)
                .into_iter()
                .collect(),
        ),
        b"11" if rest == Some(b"?") => {
            forward_with(vec![OutputEvent::ShellQueries(vec![ReplyKind::Background])])
        }
        b"4" => {
            let kinds = palette_queries(rest.unwrap_or_default());
            if kinds.is_empty() {
                forward()
            } else {
                forward_with(vec![OutputEvent::ShellQueries(kinds)])
            }
        }
        b"7790" | b"7791" | b"7792" | b"7793" => {
            let code = number
                .iter()
                .fold(0u16, |acc, digit| acc * 10 + u16::from(digit - b'0'));
            let fields = rest
                .map(|rest| {
                    rest.split(|&byte| byte == b';')
                        .map(<[u8]>::to_vec)
                        .collect()
                })
                .unwrap_or_default();
            Outcome::Strip(OutputEvent::Private { code, fields })
        }
        _ => forward(),
    }
}

/// The indices `OSC 4 ; n ; spec ; n ; spec …` asks about (spec `?`), in
/// order. It stops at an index that is not one.
fn palette_queries(rest: &[u8]) -> Vec<ReplyKind> {
    let mut kinds = Vec::new();
    let mut fields = rest.split(|&byte| byte == b';');
    while let (Some(index), Some(spec)) = (fields.next(), fields.next()) {
        let Some(index) = caps::color_index(index) else {
            break;
        };
        if spec == b"?" {
            kinds.push(ReplyKind::Palette(index));
        }
    }
    kinds
}

fn prompt_mark(rest: &[u8]) -> PromptMark {
    let mut fields = rest.split(|&byte| byte == b';');
    match fields.next().unwrap_or_default() {
        b"A" => PromptMark::A,
        b"B" => PromptMark::B,
        b"C" => PromptMark::C,
        b"D" => PromptMark::D(
            fields
                .next()
                .and_then(|status| std::str::from_utf8(status).ok())
                .and_then(|status| status.parse().ok()),
        ),
        _ => PromptMark::Other(rest.to_vec()),
    }
}

/// The percent-decoded path of a `file://host/path` URL.
fn cwd(url: &[u8]) -> Option<String> {
    let rest = url.strip_prefix(b"file://")?;
    let path = &rest[memchr(b'/', rest)?..];
    Some(String::from_utf8_lossy(&percent_decode(path)).into_owned())
}

fn percent_decode(bytes: &[u8]) -> Vec<u8> {
    let hex = |byte: u8| char::from(byte).to_digit(16);
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let (Some(high), Some(low)) = (
                bytes.get(i + 1).copied().and_then(hex),
                bytes.get(i + 2).copied().and_then(hex),
            )
        {
            // Two hex digits make one byte.
            decoded.push((high * 16 + low) as u8);
            i += 3;
        } else {
            decoded.push(bytes[i]);
            i += 1;
        }
    }
    decoded
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A segment with adjacent `Bytes` merged, so results compare across splits.
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Item {
        Bytes(Vec<u8>),
        Event(OutputEvent),
    }

    fn scan_chunks(chunks: &[&[u8]]) -> Vec<Item> {
        scan_chunks_watched(chunks, false)
    }

    fn scan_chunks_watched(chunks: &[&[u8]], watch: bool) -> Vec<Item> {
        let mut scanner = OutputScanner::new();
        scanner.set_watch(watch);
        let mut items: Vec<Item> = Vec::new();
        for chunk in chunks {
            scanner.feed(chunk, &mut |segment| match segment {
                Segment::Bytes(bytes) => {
                    assert!(!bytes.is_empty(), "empty Bytes segment");
                    if let Some(Item::Bytes(last)) = items.last_mut() {
                        last.extend_from_slice(bytes);
                    } else {
                        items.push(Item::Bytes(bytes.to_vec()));
                    }
                }
                Segment::Event(event) => items.push(Item::Event(event)),
            });
        }
        items
    }

    fn scan(input: &[u8]) -> Vec<Item> {
        scan_chunks(&[input])
    }

    fn bytes(b: &[u8]) -> Item {
        Item::Bytes(b.to_vec())
    }

    fn event(event: OutputEvent) -> Item {
        Item::Event(event)
    }

    fn prompt(mark: PromptMark) -> Item {
        event(OutputEvent::Prompt(mark))
    }

    #[test]
    fn plain_text_passes_through() {
        assert_eq!(scan(b"hello\r\nworld"), vec![bytes(b"hello\r\nworld")]);
        assert_eq!(scan(b""), vec![]);
    }

    #[test]
    fn prompt_marks() {
        assert_eq!(
            scan(b"x\x1b]133;A\x07y"),
            vec![
                bytes(b"x\x1b]133;A\x07"),
                prompt(PromptMark::A),
                bytes(b"y")
            ]
        );
        assert_eq!(
            scan(b"\x1b]133;B\x07"),
            vec![bytes(b"\x1b]133;B\x07"), prompt(PromptMark::B)]
        );
        assert_eq!(
            scan(b"\x1b]133;C\x07"),
            vec![bytes(b"\x1b]133;C\x07"), prompt(PromptMark::C)]
        );
        assert_eq!(
            scan(b"\x1b]133;D\x07"),
            vec![bytes(b"\x1b]133;D\x07"), prompt(PromptMark::D(None))]
        );
        assert_eq!(
            scan(b"\x1b]133;D;127\x07"),
            vec![
                bytes(b"\x1b]133;D;127\x07"),
                prompt(PromptMark::D(Some(127)))
            ]
        );
        assert_eq!(
            scan(b"\x1b]133;P;k=s\x07"),
            vec![
                bytes(b"\x1b]133;P;k=s\x07"),
                prompt(PromptMark::Other(b"P;k=s".to_vec()))
            ]
        );
        assert_eq!(
            scan(b"\x1b]133;A;cl=m\x07"),
            vec![bytes(b"\x1b]133;A;cl=m\x07"), prompt(PromptMark::A)]
        );
    }

    #[test]
    fn bel_and_st_terminators() {
        assert_eq!(
            scan(b"\x1b]133;A\x1b\\z"),
            vec![
                bytes(b"\x1b]133;A\x1b\\"),
                prompt(PromptMark::A),
                bytes(b"z")
            ]
        );
        assert_eq!(
            scan(b"\x1b]7790;1\x1b\\z"),
            vec![
                event(OutputEvent::Private {
                    code: 7790,
                    fields: vec![b"1".to_vec()]
                }),
                bytes(b"z")
            ]
        );
    }

    /// fish 4.3+ sends its own marks, ST-terminated and with parameters;
    /// the bytes as fish 4.9.3 wrote them for `echo hi`.
    #[test]
    fn fish_prompt_marks() {
        let marks: [(&[u8], PromptMark); 4] = [
            (b"\x1b]133;A;click_events=1\x1b\\", PromptMark::A),
            (b"\x1b]133;B\x1b\\", PromptMark::B),
            (b"\x1b]133;C;cmdline_url=echo%20hi\x1b\\", PromptMark::C),
            (b"\x1b]133;D;0\x1b\\", PromptMark::D(Some(0))),
        ];
        for (mark, expected) in marks {
            assert_eq!(scan(mark), vec![bytes(mark), prompt(expected)]);
        }
        // A whole prompt, a command and its output in one read: every mark
        // is found, and the command line in `cmdline_url` stays in the
        // forwarded bytes only (the event carries nothing of it).
        let mut input = Vec::new();
        input.extend_from_slice(b"\x1b]133;A;click_events=1\x1b\\> \x1b]133;B\x1b\\\x1b[K");
        input.extend_from_slice(b"echo hi\r\n\x1b]133;C;cmdline_url=echo%20hi\x1b\\");
        input.extend_from_slice(b"\x1b]0;echo hi\x1b\\hi\r\n\x1b]133;D;0\x1b\\");
        let marks: Vec<PromptMark> = scan(&input)
            .into_iter()
            .filter_map(|item| match item {
                Item::Event(OutputEvent::Prompt(mark)) => Some(mark),
                _ => None,
            })
            .collect();
        assert_eq!(
            marks,
            [
                PromptMark::A,
                PromptMark::B,
                PromptMark::C,
                PromptMark::D(Some(0))
            ]
        );
        let forwarded: Vec<u8> = scan(&input)
            .into_iter()
            .filter_map(|item| match item {
                Item::Bytes(bytes) => Some(bytes),
                _ => None,
            })
            .flatten()
            .collect();
        assert_eq!(forwarded, input);
        // Split anywhere, the same.
        for at in 1..input.len() {
            let (head, tail) = input.split_at(at);
            let split: Vec<PromptMark> = scan_chunks(&[head, tail])
                .into_iter()
                .filter_map(|item| match item {
                    Item::Event(OutputEvent::Prompt(mark)) => Some(mark),
                    _ => None,
                })
                .collect();
            assert_eq!(split, marks, "split at {at}");
        }
    }

    #[test]
    fn cwd_is_percent_decoded() {
        assert_eq!(
            scan(b"\x1b]7;file://mac.local/Users/me/My%20Dir%2fx\x07"),
            vec![
                bytes(b"\x1b]7;file://mac.local/Users/me/My%20Dir%2fx\x07"),
                event(OutputEvent::Cwd("/Users/me/My Dir/x".to_owned()))
            ]
        );
        assert_eq!(
            scan(b"\x1b]7;file:///tmp\x07"),
            vec![
                bytes(b"\x1b]7;file:///tmp\x07"),
                event(OutputEvent::Cwd("/tmp".to_owned()))
            ]
        );
        // Not a file URL: forwarded, no event.
        assert_eq!(
            scan(b"\x1b]7;garbage\x07"),
            vec![bytes(b"\x1b]7;garbage\x07")]
        );
    }

    #[test]
    fn private_osc_is_stripped_exactly() {
        for code in [7790u16, 7791, 7792, 7793] {
            let input = format!("before\x1b]{code};1;5;x%3By\x07after");
            assert_eq!(
                scan(input.as_bytes()),
                vec![
                    bytes(b"before"),
                    event(OutputEvent::Private {
                        code,
                        fields: vec![b"1".to_vec(), b"5".to_vec(), b"x%3By".to_vec()]
                    }),
                    bytes(b"after")
                ]
            );
        }
        assert_eq!(
            scan(b"\x1b]7790\x07"),
            vec![event(OutputEvent::Private {
                code: 7790,
                fields: vec![]
            })]
        );
        // Neighbours of the range are ordinary OSCs.
        assert_eq!(scan(b"\x1b]7789;1\x07"), vec![bytes(b"\x1b]7789;1\x07")]);
        assert_eq!(scan(b"\x1b]7794;1\x07"), vec![bytes(b"\x1b]7794;1\x07")]);
    }

    #[test]
    fn alt_screen() {
        for (seq, on) in [
            (&b"\x1b[?1049h"[..], true),
            (b"\x1b[?1049l", false),
            (b"\x1b[?1047h", true),
            (b"\x1b[?47l", false),
            (b"\x1b[?1;1049h", true),
            (b"\x1b[?1049;25l", false),
        ] {
            assert_eq!(
                scan(seq),
                vec![bytes(seq), event(OutputEvent::AltScreen(on))],
                "{seq:?}"
            );
        }
        // Not private, or other modes: no event.
        for seq in [&b"\x1b[1049h"[..], b"\x1b[?25h", b"\x1b[?10490h"] {
            assert_eq!(scan(seq), vec![bytes(seq)], "{seq:?}");
        }
        // Asking about the mode is not switching it.
        assert_eq!(
            scan(b"\x1b[?1049$p"),
            vec![
                bytes(b"\x1b[?1049$p"),
                event(OutputEvent::ShellQueries(vec![ReplyKind::Mode(1049)]))
            ]
        );
    }

    #[test]
    fn shell_cursor_query() {
        for seq in [&b"\x1b[6n"[..], b"\x1b[?6n"] {
            assert_eq!(
                scan(seq),
                vec![bytes(seq), event(OutputEvent::ShellCursorQuery)],
                "{seq:?}"
            );
        }
        for seq in [
            &b"\x1b[5n"[..],
            b"\x1b[16n",
            b"\x1b[6;1n",
            b"\x1b[n",
            b"\x1b[>6n",
        ] {
            assert_eq!(scan(seq), vec![bytes(seq)], "{seq:?}");
        }
    }

    fn queries(kinds: &[ReplyKind]) -> Item {
        event(OutputEvent::ShellQueries(kinds.to_vec()))
    }

    #[test]
    fn shell_queries() {
        use ReplyKind::{Background, DeviceAttrs, Mode, Palette};
        for (seq, kinds) in [
            (&b"\x1b[c"[..], &[DeviceAttrs][..]),
            (b"\x1b[0c", &[DeviceAttrs]),
            (b"\x1b[?2026$p", &[Mode(2026)]),
            (b"\x1b[?1$p", &[Mode(1)]),
            (b"\x1b]11;?\x1b\\", &[Background]),
            (b"\x1b]11;?\x07", &[Background]),
            (b"\x1b]4;1;?\x1b\\", &[Palette(1)]),
            (b"\x1b]4;255;?\x07", &[Palette(255)]),
            (b"\x1b]4;2;?;3;?\x1b\\", &[Palette(2), Palette(3)]),
            // Setting one colour and asking another.
            (b"\x1b]4;1;rgb:0/0/0;6;?\x07", &[Palette(6)]),
        ] {
            let whole = scan(seq);
            assert_eq!(whole, vec![bytes(seq), queries(kinds)], "{seq:?}");
            // The bytes never change, and a split read gives the same.
            let mut input = b"<".to_vec();
            input.extend_from_slice(seq);
            input.push(b'>');
            let whole = scan(&input);
            assert_eq!(
                whole,
                vec![
                    bytes(&input[..input.len() - 1]),
                    queries(kinds),
                    bytes(b">")
                ],
                "{seq:?}"
            );
            for split in 0..=input.len() {
                let (a, b) = input.split_at(split);
                assert_eq!(scan_chunks(&[a, b]), whole, "{seq:?} split at {split}");
            }
            let bytewise: Vec<&[u8]> = input.chunks(1).collect();
            assert_eq!(scan_chunks(&bytewise), whole, "{seq:?} byte by byte");
        }
    }

    #[test]
    fn not_shell_queries() {
        for seq in [
            // DA2, DA3, a DA1 with a parameter, DA1 replies.
            &b"\x1b[>c"[..],
            b"\x1b[=c",
            b"\x1b[1c",
            b"\x1b[?62;22c",
            // ANSI DECRQM, two modes, none, another intermediate, a reply.
            b"\x1b[2026$p",
            b"\x1b[?2026;1$p",
            b"\x1b[?$p",
            b"\x1b[?2026#p",
            b"\x1b[?2026 $p",
            b"\x1b[?2026;2$y",
            // Setting colours, and their replies.
            b"\x1b]11;#000000\x07",
            b"\x1b]10;rgb:0/0/0\x1b\\",
            // OSC 10, which fac never asks: nothing tracks the shell's.
            b"\x1b]10;?\x1b\\",
            b"\x1b]10;?\x07",
            b"\x1b]11;?;?\x07",
            b"\x1b]12;?\x07",
            b"\x1b]4;1;rgb:0/0/0\x07",
            b"\x1b]4;x;?\x07",
            b"\x1b]4;256;?\x07",
            b"\x1b]4;1\x07",
            b"\x1b]4\x07",
            b"\x1b]104;1\x07",
            // Aborted.
            b"\x1b]11;?\x1bx",
            b"\x1b[\x18c",
        ] {
            assert_eq!(scan(seq), vec![bytes(seq)], "{seq:?}");
        }
        // A bad index ends the list.
        assert_eq!(
            scan(b"\x1b]4;2;?;x;?;3;?\x07"),
            vec![
                bytes(b"\x1b]4;2;?;x;?;3;?\x07"),
                queries(&[ReplyKind::Palette(2)])
            ]
        );
    }

    #[test]
    fn a_query_burst_keeps_its_order() {
        use ReplyKind::{Background, DeviceAttrs, Mode, Palette};
        let input = b"\x1b[?1049h\x1b[c\x1b[?2026$p\x1b]11;?\x1b\\\x1b]4;1;?\x1b\\\x1b[6n";
        assert_eq!(
            scan(input),
            vec![
                bytes(b"\x1b[?1049h"),
                event(OutputEvent::AltScreen(true)),
                bytes(b"\x1b[c"),
                queries(&[DeviceAttrs]),
                bytes(b"\x1b[?2026$p"),
                queries(&[Mode(2026)]),
                bytes(b"\x1b]11;?\x1b\\"),
                queries(&[Background]),
                bytes(b"\x1b]4;1;?\x1b\\"),
                queries(&[Palette(1)]),
                bytes(b"\x1b[6n"),
                event(OutputEvent::ShellCursorQuery),
            ]
        );
    }

    #[test]
    fn other_sequences_are_plain() {
        let input = b"\x1b[31mred\x1b[0m\x1b(B\x1b7\x1b8\x1b]0;title\x07\x1bP+q\x1b\\";
        // DECSC and DECRC are forwarded as they are, each with its event.
        assert_eq!(
            scan(input),
            vec![
                bytes(b"\x1b[31mred\x1b[0m\x1b(B\x1b7"),
                event(OutputEvent::ShellCursorSaved(true)),
                bytes(b"\x1b8"),
                event(OutputEvent::ShellCursorSaved(false)),
                bytes(b"\x1b]0;title\x07\x1bP+q\x1b\\"),
            ]
        );
    }

    fn moves(kind: Move) -> Item {
        event(OutputEvent::Moves(kind))
    }

    #[test]
    fn watched_rows_come_after_their_bytes() {
        for (seq, kind) in [
            (&b"\x1bD"[..], Move::Rows),
            (b"\x1bE", Move::Rows),
            (b"\x1bM", Move::Rows),
            (b"\x1b[J", Move::Erase),
            (b"\x1b[2J", Move::Erase),
            (b"\x1b[?J", Move::Erase),
            (b"\x1b[L", Move::Rows),
            (b"\x1b[3M", Move::Rows),
            (b"\x1b[S", Move::Rows),
            (b"\x1b[2T", Move::Rows),
            (b"\x1b[1;10r", Move::Rows),
            (b"\x1b[r", Move::Rows),
            (b"\x1b[3\nC", Move::Rows),
        ] {
            let mut input = b"<".to_vec();
            input.extend_from_slice(seq);
            input.push(b'>');
            let watched = scan_chunks_watched(&[&input], true);
            assert_eq!(
                watched,
                vec![bytes(&input[..input.len() - 1]), moves(kind), bytes(b">")],
                "{seq:?}"
            );
            for split in 0..=input.len() {
                let (a, b) = input.split_at(split);
                assert_eq!(
                    scan_chunks_watched(&[a, b], true),
                    watched,
                    "{seq:?} at {split}"
                );
            }
            // Not watched: nothing.
            assert_eq!(scan(&input), vec![bytes(&input)], "{seq:?}");
        }
    }

    #[test]
    fn watched_line_feeds_come_before_their_byte_alone() {
        for lf in [b'\n', 0x0b, 0x0c] {
            let input = [b'<', lf, b'>', lf];
            let watched = scan_chunks_watched(&[&input], true);
            assert_eq!(
                watched,
                vec![
                    bytes(b"<"),
                    moves(Move::LineFeed),
                    bytes(&[lf, b'>']),
                    moves(Move::LineFeed),
                    bytes(&[lf]),
                ],
                "{lf:?}"
            );
            for split in 0..=input.len() {
                let (a, b) = input.split_at(split);
                assert_eq!(
                    scan_chunks_watched(&[a, b], true),
                    watched,
                    "{lf:?} at {split}"
                );
            }
            // The byte is a segment of its own, unmerged.
            let mut raw: Vec<Vec<u8>> = Vec::new();
            let mut scanner = OutputScanner::new();
            scanner.set_watch(true);
            scanner.feed(&input, &mut |segment| {
                if let Segment::Bytes(bytes) = segment {
                    raw.push(bytes.to_vec());
                }
            });
            assert_eq!(raw, [vec![b'<'], vec![lf], vec![b'>'], vec![lf]]);
            // Not watched: nothing.
            assert_eq!(scan(&input), vec![bytes(&input)], "{lf:?}");
        }
    }

    #[test]
    fn watched_cursor_moves_come_before_their_bytes() {
        for seq in [
            &b"\x1b[A"[..],
            b"\x1b[2B",
            b"\x1b[E",
            b"\x1b[F",
            b"\x1b[5;1H",
            b"\x1b[H",
            b"\x1b[2;3f",
            b"\x1b[4d",
            b"\x1b[e",
            b"\x1b[u",
        ] {
            let mut input = b"<".to_vec();
            input.extend_from_slice(seq);
            input.push(b'>');
            let watched = scan_chunks_watched(&[&input], true);
            let mut rest = seq.to_vec();
            rest.push(b'>');
            let mut expected = vec![bytes(b"<"), moves(Move::Cursor), bytes(&rest)];
            if seq == b"\x1b[u" {
                expected = vec![
                    bytes(b"<"),
                    moves(Move::Cursor),
                    bytes(seq),
                    event(OutputEvent::ShellCursorSaved(false)),
                    bytes(b">"),
                ];
            }
            assert_eq!(watched, expected, "{seq:?}");
            for split in 0..=input.len() {
                let (a, b) = input.split_at(split);
                assert_eq!(
                    scan_chunks_watched(&[a, b], true),
                    watched,
                    "{seq:?} at {split}"
                );
            }
        }
        // Line-local sequences make nothing: cursor columns, erasing in
        // the line, SGR, insert and delete characters.
        let local = b"\x1b[5C\x1b[2D\x1b[10G\x1b[K\x1b[2X\x1b[31m\x1b[2P\x1b[@\rab\x08";
        assert_eq!(scan_chunks_watched(&[local], true), vec![bytes(local)]);
        // DECRC: the move before, the restore after.
        assert_eq!(
            scan_chunks_watched(&[b"a\x1b8b"], true),
            vec![
                bytes(b"a"),
                moves(Move::Cursor),
                bytes(b"\x1b8"),
                event(OutputEvent::ShellCursorSaved(false)),
                bytes(b"b"),
            ]
        );
    }

    #[test]
    fn the_shells_saved_cursor() {
        let saved = |on| event(OutputEvent::ShellCursorSaved(on));
        assert_eq!(scan(b"\x1b7"), vec![bytes(b"\x1b7"), saved(true)]);
        assert_eq!(scan(b"\x1b8"), vec![bytes(b"\x1b8"), saved(false)]);
        assert_eq!(scan(b"\x1b[s"), vec![bytes(b"\x1b[s"), saved(true)]);
        assert_eq!(scan(b"\x1b[u"), vec![bytes(b"\x1b[u"), saved(false)]);
        assert_eq!(scan(b"\x1bc"), vec![bytes(b"\x1bc"), saved(false)]);
        // With DECLRMM on, `CSI s` sets margins.
        assert_eq!(
            scan(b"\x1b[?69h\x1b[s\x1b[?69l\x1b[s"),
            vec![bytes(b"\x1b[?69h\x1b[s\x1b[?69l\x1b[s"), saved(true)]
        );
        // Not saves: with parameters, private, or another escape.
        for seq in [&b"\x1b[1;5s"[..], b"\x1b[?1s", b"\x1b[2u", b"\x1b#8"] {
            assert_eq!(scan(seq), vec![bytes(seq)], "{seq:?}");
        }
    }

    #[test]
    fn aborted_osc_is_forwarded_and_rescanned() {
        // ESC not followed by `\` aborts the OSC; the ESC starts a CSI.
        assert_eq!(
            scan(b"\x1b]133;A\x1b[31mX"),
            vec![bytes(b"\x1b]133;A\x1b[31mX")]
        );
        // An aborted private OSC is forwarded, not stripped.
        assert_eq!(
            scan(b"\x1b]7790;1\x1bxy"),
            vec![bytes(b"\x1b]7790;1\x1bxy")]
        );
        // Nested: the second OSC completes.
        assert_eq!(
            scan(b"\x1b]0;title\x1b]133;A\x07"),
            vec![bytes(b"\x1b]0;title\x1b]133;A\x07"), prompt(PromptMark::A)]
        );
        assert_eq!(
            scan(b"\x1b]0;t\x1b\x1b]7790;9\x07!"),
            vec![
                bytes(b"\x1b]0;t\x1b"),
                event(OutputEvent::Private {
                    code: 7790,
                    fields: vec![b"9".to_vec()]
                }),
                bytes(b"!")
            ]
        );
        // ESC inside a CSI restarts.
        assert_eq!(
            scan(b"\x1b[12\x1b[6n"),
            vec![
                bytes(b"\x1b[12\x1b[6n"),
                event(OutputEvent::ShellCursorQuery)
            ]
        );
        // ESC ESC
        assert_eq!(
            scan(b"\x1b\x1b[?1049h"),
            vec![
                bytes(b"\x1b\x1b[?1049h"),
                event(OutputEvent::AltScreen(true))
            ]
        );
    }

    #[test]
    fn can_and_sub_abort_a_sequence() {
        for abort in [CAN, SUB] {
            let a = char::from(abort);
            // Not held until a later terminator: forwarded at once.
            for input in [
                format!("\x1b]0;title{a}after"),
                format!("\x1b]7790;1;5{a}x"),
                format!("\x1b]133;A{a}"),
                format!("\x1b[12{a}6n"),
                format!("\x1b[?{a}1049h"),
                format!("\x1b{a}x"),
                format!("\x1b]0;t\x1b{a}y"),
            ] {
                let input = input.as_bytes();
                assert_eq!(scan(input), vec![bytes(input)], "{input:?}");
                let mut scanner = OutputScanner::new();
                scanner.feed(input, &mut |_| {});
                assert_eq!(scanner.state, State::Ground, "{input:?}");
                assert!(scanner.carry.is_empty(), "{input:?}");
            }
            // Scanning goes on normally after it.
            let input = format!("\x1b]0;t{a}\x1b]133;A\x07\x1b[?1;{a}\x1b[6n");
            assert_eq!(
                scan(input.as_bytes()),
                vec![
                    bytes(format!("\x1b]0;t{a}\x1b]133;A\x07").as_bytes()),
                    prompt(PromptMark::A),
                    bytes(format!("\x1b[?1;{a}\x1b[6n").as_bytes()),
                    event(OutputEvent::ShellCursorQuery),
                ]
            );
        }
    }

    /// Every kind of sequence, plus aborted ones, in one stream.
    fn mixed() -> Vec<u8> {
        let mut input = Vec::new();
        input.extend_from_slice("plain 日本語\r\n".as_bytes());
        input.extend_from_slice(b"\x1b[1;32mgreen\x1b[0m");
        input.extend_from_slice(b"\x1b]133;A\x07% \x1b]133;B\x1b\\");
        input.extend_from_slice(b"a\x1b]7790;1;5;x\x07b");
        input.extend_from_slice(b"\x1b]7;file://h/tmp/a%20b\x07");
        input.extend_from_slice(b"\x1b[?1;1049h\x1b[6n\x1b[?1049l");
        input.extend_from_slice(b"\x1b]7791;1;2;cwd;/x\x1b\\");
        input.extend_from_slice(b"\x1b]0;aborted\x1b[?6n");
        input.extend_from_slice(b"\x1b]7792;1;3\x1bq");
        input.extend_from_slice(b"\x1b]133;D;1\x07end");
        input
    }

    #[test]
    fn every_split_point_gives_the_same_result() {
        let input = mixed();
        let whole = scan(&input);
        assert_eq!(
            whole
                .iter()
                .filter(|item| matches!(item, Item::Event(_)))
                .count(),
            10,
            "{whole:?}"
        );
        for split in 0..=input.len() {
            let (a, b) = input.split_at(split);
            assert_eq!(scan_chunks(&[a, b]), whole, "split at {split}");
        }
        let bytewise: Vec<&[u8]> = input.chunks(1).collect();
        assert_eq!(scan_chunks(&bytewise), whole, "byte by byte");
    }

    #[test]
    fn every_split_point_of_each_sequence() {
        for seq in [
            &b"\x1b]7790;1;5;x\x07"[..],
            b"\x1b]7793;1;2\x1b\\",
            b"\x1b]133;A\x1b\\",
            b"\x1b[?1049h",
            b"\x1b[6n",
            b"\x1b]0;t\x1b[6n",
            b"\x1b]7790;1\x18",
            b"\x1b[12\x1a",
        ] {
            let mut input = b"<".to_vec();
            input.extend_from_slice(seq);
            input.push(b'>');
            let whole = scan(&input);
            for split in 0..=input.len() {
                let (a, b) = input.split_at(split);
                assert_eq!(scan_chunks(&[a, b]), whole, "{seq:?} split at {split}");
            }
        }
    }

    #[test]
    fn one_mib_of_plain_text_is_one_segment() {
        let input = vec![b'a'; 1024 * 1024];
        let mut scanner = OutputScanner::new();
        let mut segments = 0;
        let mut total = 0;
        scanner.feed(&input, &mut |segment| {
            let Segment::Bytes(bytes) = segment else {
                panic!("unexpected event");
            };
            segments += 1;
            total += bytes.len();
        });
        assert_eq!((segments, total), (1, input.len()));
    }

    #[test]
    fn an_endless_osc_is_flushed_past_the_cap() {
        let mut input = b"\x1b]0;".to_vec();
        input.extend(std::iter::repeat_n(b'x', MAX_SEQUENCE));
        let chunks: Vec<&[u8]> = input.chunks(64 * 1024).collect();
        let mut chunks = chunks;
        chunks.push(b"\x1b]133;A\x07");
        let items = scan_chunks(&chunks);
        let mut expected = input.clone();
        expected.extend_from_slice(b"\x1b]133;A\x07");
        assert_eq!(items, vec![Item::Bytes(expected), prompt(PromptMark::A)]);
    }
}
