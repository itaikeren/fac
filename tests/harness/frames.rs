//! A test-only interpreter for fac's own frame bytes. It takes what the
//! terminal received (`Session::raw_output`), finds fac's frames in it and
//! fac's own bytes in each frame, and counts the writes fac makes to each
//! cell in one frame.
//!
//! Which bytes are fac's: the raw output with the shell's bytes left out.
//! fac's bytes come in segments that start with DECSC (`ESC 7`), or with
//! the scroll-to-make-room prefix right before it (`CSI 0 m`, `CSI rows;1H`,
//! LFs, an optional SGR and a CUP), and end with DECRC (`ESC 8`) and an
//! optional cursor show. Everything else in a frame is the shell's. The
//! fixtures' zsh never writes DECSC, so this holds in the suites; a frame
//! restored by CUP (the shell has saved a cursor) is not split this way.
//!
//! What is a frame: with synchronized output, one 2026 pair (fac wraps each
//! write in one). Without it the terminal sees no frame boundary, so each
//! segment is checked on its own.

use std::collections::HashMap;

pub const SYNC_BEGIN: &[u8] = b"\x1b[?2026h";
pub const SYNC_END: &[u8] = b"\x1b[?2026l";

/// How fac wrote a cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Write {
    /// A character (text or a restored model cell).
    Text,
    /// Erased with ECH.
    Erase,
}

/// The writes of one frame, cell by cell, in order.
#[derive(Debug, Default)]
pub struct Writes {
    cells: HashMap<(u16, u16), Vec<Write>>,
}

impl Writes {
    /// The first cell written twice, with its writes.
    pub fn twice(&self) -> Option<((u16, u16), &[Write])> {
        let mut found: Vec<_> = self
            .cells
            .iter()
            .filter(|(_, writes)| writes.len() > 1)
            .collect();
        found.sort_by_key(|(cell, _)| **cell);
        found
            .first()
            .map(|(cell, writes)| (**cell, writes.as_slice()))
    }

    /// The first cell erased and then painted (a character) in this frame.
    pub fn erased_then_painted(&self) -> Option<(u16, u16)> {
        let mut found: Vec<_> = self
            .cells
            .iter()
            .filter(|(_, writes)| {
                writes
                    .iter()
                    .position(|write| *write == Write::Erase)
                    .is_some_and(|erase| writes[erase..].contains(&Write::Text))
            })
            .map(|(cell, _)| *cell)
            .collect();
        found.sort_unstable();
        found.first().copied()
    }

    /// How many cells were written.
    pub fn len(&self) -> usize {
        self.cells.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    /// The writes to `cell`.
    pub fn of(&self, cell: (u16, u16)) -> &[Write] {
        self.cells.get(&cell).map_or(&[], Vec::as_slice)
    }

    /// The cells written, in no order.
    pub fn cells(&self) -> impl Iterator<Item = (u16, u16)> + '_ {
        self.cells.keys().copied()
    }
}

/// The 2026 frames in `raw`, each without its pair. Bytes outside a pair
/// are left out; an unfinished last frame too.
pub fn sync_frames(raw: &[u8]) -> Vec<&[u8]> {
    let mut frames = Vec::new();
    let mut at = 0;
    while let Some(begin) = find(&raw[at..], SYNC_BEGIN) {
        let start = at + begin + SYNC_BEGIN.len();
        let Some(len) = find(&raw[start..], SYNC_END) else {
            break;
        };
        frames.push(&raw[start..start + len]);
        at = start + len + SYNC_END.len();
    }
    frames
}

/// fac's segments in `bytes` (a frame, or any stretch of output) on a
/// screen of `rows` rows, in order.
pub fn segments(bytes: &[u8], rows: u16) -> Vec<&[u8]> {
    let mut found = Vec::new();
    let mut at = 0;
    while let Some(save) = find(&bytes[at..], b"\x1b7") {
        let save = at + save;
        let start = scroll_prefix(&bytes[at..save], rows).map_or(save, |len| save - len);
        let Some(restore) = find(&bytes[save..], b"\x1b8") else {
            break;
        };
        let mut end = save + restore + 2;
        if bytes[end..].starts_with(b"\x1b[?25h") {
            end += 6;
        }
        found.push(&bytes[start..end]);
        at = end;
    }
    found
}

/// The length of the scroll-to-make-room prefix that `before` ends with,
/// if it does: `CSI 0 m` (optional), `CSI rows;1H`, one or more LFs, an
/// optional SGR, then one CUP.
fn scroll_prefix(before: &[u8], rows: u16) -> Option<usize> {
    let to_bottom = format!("\x1b[{rows};1H");
    let start = rfind(before, to_bottom.as_bytes())?;
    let mut rest = &before[start + to_bottom.len()..];
    let lfs = rest.iter().take_while(|&&byte| byte == b'\n').count();
    if lfs == 0 {
        return None;
    }
    rest = &rest[lfs..];
    if let Some((len, b'm')) = csi(rest) {
        rest = &rest[len..];
    }
    let (len, b'H') = csi(rest)? else {
        return None;
    };
    if len != rest.len() {
        return None;
    }
    let reset = before[..start].ends_with(b"\x1b[0m");
    Some(before.len() - start + if reset { 4 } else { 0 })
}

/// The length and final byte of the CSI sequence `bytes` starts with.
fn csi(bytes: &[u8]) -> Option<(usize, u8)> {
    let rest = bytes.strip_prefix(b"\x1b[")?;
    let end = rest.iter().position(|byte| (0x40..=0x7e).contains(byte))?;
    Some((end + 3, rest[end]))
}

/// The numeric parameters of a CSI sequence's body (between `ESC [` and
/// the final byte), `default` for an empty one.
fn params(body: &[u8], default: u16) -> Vec<u16> {
    std::str::from_utf8(body)
        .unwrap_or_default()
        .trim_start_matches('?')
        .split(';')
        .map(|param| param.parse().unwrap_or(default))
        .collect()
}

/// Run fac's `segments` (one frame's) on a `rows` x `cols` screen and
/// count its writes. Fails on a byte fac does not write.
pub fn interpret(segments: &[&[u8]], rows: u16, cols: u16) -> Result<Writes, String> {
    interpret_with(segments, rows, cols, fac::popup::text::char_width)
}

/// `interpret` on a terminal that gives each character `width(ch)` columns,
/// one after another, the way Terminal.app counts a VS16 or ZWJ sequence
/// (findings.md, fact 8) where fac's model counts them otherwise.
pub fn interpret_with(
    segments: &[&[u8]],
    rows: u16,
    cols: u16,
    width: impl Fn(char) -> u16,
) -> Result<Writes, String> {
    let mut writes = Writes::default();
    let mut cursor = (0u16, 0u16);
    let mut saved = cursor;
    for segment in segments {
        let mut i = 0;
        while i < segment.len() {
            let rest = &segment[i..];
            match rest[0] {
                0x1b => {
                    if rest.starts_with(b"\x1b7") {
                        saved = cursor;
                        i += 2;
                        continue;
                    }
                    if rest.starts_with(b"\x1b8") {
                        cursor = saved;
                        i += 2;
                        continue;
                    }
                    let (len, last) = csi(rest)
                        .ok_or_else(|| format!("not fac's: {:?}", String::from_utf8_lossy(rest)))?;
                    let body = &rest[2..len - 1];
                    match last {
                        b'H' => {
                            let p = params(body, 1);
                            let row = p.first().copied().unwrap_or(1).max(1) - 1;
                            let col = p.get(1).copied().unwrap_or(1).max(1) - 1;
                            cursor = (row.min(rows - 1), col.min(cols - 1));
                        }
                        b'C' => {
                            let n = params(body, 1)[0].max(1);
                            cursor.1 = cursor.1.saturating_add(n).min(cols - 1);
                        }
                        b'X' => {
                            let n = params(body, 1)[0].max(1);
                            for col in cursor.1..cursor.1.saturating_add(n).min(cols) {
                                writes
                                    .cells
                                    .entry((cursor.0, col))
                                    .or_default()
                                    .push(Write::Erase);
                            }
                        }
                        b'm' => {}
                        b'h' | b'l' if body == b"?25" || body == b"?2026" => {}
                        _ => {
                            return Err(format!(
                                "not fac's: {:?}",
                                String::from_utf8_lossy(&rest[..len])
                            ));
                        }
                    }
                    i += len;
                }
                b'\n' => {
                    if cursor.0 + 1 == rows {
                        // The screen scrolls: what this frame wrote moves up.
                        writes.cells = writes
                            .cells
                            .drain()
                            .filter(|((row, _), _)| *row > 0)
                            .map(|((row, col), w)| ((row - 1, col), w))
                            .collect();
                    } else {
                        cursor.0 += 1;
                    }
                    i += 1;
                }
                byte if byte < 0x20 || byte == 0x7f => {
                    return Err(format!("not fac's: control byte {byte:#04x}"));
                }
                _ => {
                    let text = std::str::from_utf8(rest)
                        .or_else(|err| std::str::from_utf8(&rest[..err.valid_up_to()]))
                        .unwrap_or_default();
                    let Some(ch) = text.chars().next() else {
                        return Err("invalid UTF-8 in fac's bytes".to_owned());
                    };
                    let width = width(ch);
                    // A wide character at the last column wraps first; fac
                    // never writes one there.
                    for col in cursor.1..cursor.1 + width {
                        writes
                            .cells
                            .entry((cursor.0, col.min(cols - 1)))
                            .or_default()
                            .push(Write::Text);
                    }
                    cursor.1 = (cursor.1 + width).min(cols - 1);
                    i += ch.len_utf8();
                }
            }
        }
    }
    Ok(writes)
}

/// Each frame in `raw` from a `rows` x `cols` terminal: with `sync`, each
/// 2026 frame; without, each of fac's segments on its own. Fails on the
/// first frame where fac wrote a cell twice, or erased a cell and then
/// painted it.
pub fn check(raw: &[u8], rows: u16, cols: u16, sync: bool) -> Result<(), String> {
    let frames: Vec<Vec<&[u8]>> = if sync {
        sync_frames(raw)
            .into_iter()
            .map(|frame| segments(frame, rows))
            .collect()
    } else {
        segments(raw, rows).into_iter().map(|s| vec![s]).collect()
    };
    for (index, frame) in frames.iter().enumerate() {
        let writes = interpret(frame, rows, cols)?;
        let show = || {
            frame
                .iter()
                .map(|segment| format!("{:?}", String::from_utf8_lossy(segment)))
                .collect::<Vec<_>>()
                .join("\n")
        };
        if let Some(cell) = writes.erased_then_painted() {
            return Err(format!(
                "frame {index}: cell {cell:?} erased and then painted\n{}",
                show()
            ));
        }
        if let Some((cell, how)) = writes.twice() {
            return Err(format!(
                "frame {index}: cell {cell:?} written {} times ({how:?})\n{}",
                how.len(),
                show()
            ));
        }
    }
    Ok(())
}

/// fac's bytes in `frame` (its segments), and all of the frame's bytes.
pub fn sizes(frame: &[u8], rows: u16) -> (usize, usize) {
    let fac = segments(frame, rows).iter().map(|s| s.len()).sum();
    (fac, frame.len())
}

/// Whether `frame` scrolls the screen to make room for the popup.
pub fn scrolls(frame: &[u8], rows: u16) -> bool {
    let to_bottom = format!("\x1b[{rows};1H\n");
    segments(frame, rows).iter().any(|segment| {
        let segment = segment
            .strip_prefix(b"\x1b[0m".as_slice())
            .unwrap_or(segment);
        segment.starts_with(to_bottom.as_bytes())
    })
}

pub fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn rfind(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .rposition(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_writes_and_finds_segments() {
        let frame = b"l\x1b7\x1b[2;1H\x1b[0;31mab\x1b[0m\x1b[3;1H\x1b[2X\x1b8x";
        let found = segments(frame, 24);
        assert_eq!(found.len(), 1);
        let writes = interpret(&found, 24, 80).expect("fac's bytes");
        assert_eq!(writes.len(), 4);
        assert_eq!(writes.of((1, 0)), [Write::Text]);
        assert_eq!(writes.of((2, 1)), [Write::Erase]);
        assert!(writes.twice().is_none());
        // ECH, then a repaint inside it.
        let twice = b"\x1b7\x1b[2;1H\x1b[3X\x1b[2;2Hx\x1b8";
        let writes = interpret(&segments(twice, 24), 24, 80).expect("fac's bytes");
        assert_eq!(writes.erased_then_painted(), Some((1, 1)));
        assert_eq!(writes.twice().map(|(cell, _)| cell), Some((1, 1)));
    }

    #[test]
    fn the_scroll_prefix_is_facs_and_moves_earlier_writes() {
        let frame =
            b"\x1b7\x1b[5;1Ha\x1b8echo\x1b[0m\x1b[5;1H\n\n\x1b[3;7H\x1b7\x1b[4;1Hb\x1b8\x1b[?25h";
        let found = segments(frame, 5);
        assert_eq!(found.len(), 2);
        assert!(found[1].starts_with(b"\x1b[0m\x1b[5;1H\n\n"));
        assert!(found[1].ends_with(b"\x1b[?25h"));
        let writes = interpret(&found, 5, 80).expect("fac's bytes");
        // `a` was on the last row; two LFs moved it up to row 2.
        assert_eq!(writes.of((2, 0)), [Write::Text]);
        assert_eq!(writes.of((3, 0)), [Write::Text]);
    }

    #[test]
    fn frames_are_2026_pairs() {
        let raw = b"x\x1b[?2026hA\x1b[?2026ly\x1b[?2026hB\x1b[?2026l\x1b[?2026hC";
        assert_eq!(sync_frames(raw), [b"A".as_slice(), b"B".as_slice()]);
    }
}
