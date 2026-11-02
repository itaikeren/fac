//! What the real terminal supports, asked once at startup, and pure parsers
//! for its replies.

use std::time::{Duration, Instant};

use crate::term::ident::TermKind;

/// DECRQM `?2026` (synchronized output), OSC 11 `?` (background colour),
/// OSC 4 `?` for ANSI colours 2–6 (green, yellow, blue, magenta, cyan), CPR
/// and DA1, in that order. Terminals answer in order, so the DA1 reply means
/// every reply that is coming has come. All five measured terminals answer
/// every index of a combined OSC 4 (findings.md); the queries are kept
/// separate on purpose, for terminals that were not measured.
pub const PROBE: &[u8] = b"\x1b[?2026$p\x1b]11;?\x1b\\\
\x1b]4;2;?\x1b\\\x1b]4;3;?\x1b\\\x1b]4;4;?\x1b\\\x1b]4;5;?\x1b\\\x1b]4;6;?\x1b\\\
\x1b[6n\x1b[c";

/// The DECRQM 2026 query that starts `PROBE`.
const DECRQM_SYNC: &[u8] = b"\x1b[?2026$p";

/// What fac asks `kind` at startup. Terminal.app answers no DECRQM and
/// prints its final `p` instead (findings.md), so it is not asked about
/// synchronized output, which it does not have; everything else gets
/// `PROBE`.
pub fn probe_for(kind: TermKind) -> &'static [u8] {
    match kind {
        TermKind::Apple => &PROBE[DECRQM_SYNC.len()..],
        _ => PROBE,
    }
}

/// How long the shell's output is held while the probe waits for DA1.
/// Every measured terminal answers DA1 last, in order, so the hold ends at
/// DA1 and this is only the cap for a slow terminal or one that never
/// answers. VS Code 1.104 spaces its replies about 12 ms apart and answers
/// DA1 at 85 ms (findings.md, fact 4), too close to the old 100 ms; this is
/// about three times that.
pub const HOLD: Duration = Duration::from_millis(250);
/// Until when replies to the probe are still consumed when DA1 never comes.
/// At least `HOLD`.
pub const CONSUME: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TermCaps {
    /// The terminal fac runs in, from its environment.
    pub kind: TermKind,
    pub sync_output: bool,
    pub background: Option<(u8, u8, u8)>,
    /// True when the background is unknown.
    pub dark: bool,
    /// The ANSI colours the terminal reported (OSC 4).
    pub ansi: Ansi,
}

impl Default for TermCaps {
    fn default() -> Self {
        Self {
            kind: TermKind::Unknown,
            sync_output: false,
            background: None,
            dark: true,
            ansi: Ansi::default(),
        }
    }
}

impl TermCaps {
    pub fn set_background(&mut self, rgb: (u8, u8, u8)) {
        self.background = Some(rgb);
        self.dark = is_dark(rgb);
    }

    /// An OSC 4 answer; indices other than 2–6 are ignored.
    pub fn set_palette(&mut self, index: u8, rgb: (u8, u8, u8)) {
        let slot = match index {
            2 => &mut self.ansi.green,
            3 => &mut self.ansi.yellow,
            4 => &mut self.ansi.blue,
            5 => &mut self.ansi.magenta,
            6 => &mut self.ansi.cyan,
            _ => return,
        };
        *slot = Some(rgb);
    }
}

/// The terminal's own ANSI colours 2–6; `None` where it did not answer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Ansi {
    pub green: Option<(u8, u8, u8)>,
    pub yellow: Option<(u8, u8, u8)>,
    pub blue: Option<(u8, u8, u8)>,
    pub magenta: Option<(u8, u8, u8)>,
    pub cyan: Option<(u8, u8, u8)>,
}

/// The Ps of a DECRQM `CSI ? 2026 ; Ps $ y` reply: 1 (set), 2 (reset) and 3
/// (permanently set) mean supported; 0 (not recognised) and 4 (permanently
/// reset) do not.
pub fn sync_supported(ps: u8) -> bool {
    matches!(ps, 1..=3)
}

/// An OSC 11 reply payload (after `11;`): `rgb:R/G/B` with 1-4 hex digits per
/// channel, each scaled to 8 bits.
pub fn parse_background(payload: &[u8]) -> Option<(u8, u8, u8)> {
    let rest = payload.strip_prefix(b"rgb:")?;
    let mut channels = rest.split(|&byte| byte == b'/').map(channel);
    let rgb = (channels.next()??, channels.next()??, channels.next()??);
    channels.next().is_none().then_some(rgb)
}

/// An OSC 4 reply payload (after `4;`): `n;rgb:R/G/B`, the colour as in
/// [`parse_background`]. Returns the index and the colour.
pub fn parse_palette(payload: &[u8]) -> Option<(u8, (u8, u8, u8))> {
    let at = payload.iter().position(|&byte| byte == b';')?;
    let (index, color) = (&payload[..at], &payload[at + 1..]);
    Some((color_index(index)?, parse_background(color)?))
}

/// An OSC 4 colour index: 1-3 digits, at most 255.
pub fn color_index(digits: &[u8]) -> Option<u8> {
    if digits.is_empty() || digits.len() > 3 || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let value = digits
        .iter()
        .fold(0u16, |value, &digit| value * 10 + u16::from(digit - b'0'));
    u8::try_from(value).ok()
}

fn channel(hex: &[u8]) -> Option<u8> {
    if hex.is_empty() || hex.len() > 4 {
        return None;
    }
    let mut value = 0u32;
    for &byte in hex {
        value = value * 16 + char::from(byte).to_digit(16)?;
    }
    let max = (1u32 << (4 * hex.len())) - 1;
    // Rounded to the nearest 8-bit value; at most 255.
    u8::try_from((value * 255 + max / 2) / max).ok()
}

/// Rec. 709 luminance below one half.
pub fn is_dark((r, g, b): (u8, u8, u8)) -> bool {
    let luminance = 0.2126 * f64::from(r) + 0.7152 * f64::from(g) + 0.0722 * f64::from(b);
    luminance / 255.0 < 0.5
}

/// The startup probe's progress.
pub struct Probe {
    started: Instant,
    da1: bool,
    pub caps: TermCaps,
}

impl Probe {
    /// `probe_for(kind)` was written at `started`. Synchronized output stays
    /// unsupported until a DECRQM reply says otherwise, so it stays off
    /// where it is not asked.
    pub fn new(started: Instant, kind: TermKind) -> Self {
        Self {
            started,
            da1: false,
            caps: TermCaps {
                kind,
                ..TermCaps::default()
            },
        }
    }

    /// The DA1 reply came: every reply that is coming has come.
    pub fn on_device_attrs(&mut self) {
        self.da1 = true;
    }

    pub fn answered(&self) -> bool {
        self.da1
    }

    /// Whether the shell's output is still held back.
    pub fn holding(&self, now: Instant) -> bool {
        !self.da1 && now < self.hold_deadline()
    }

    /// Whether replies to the probe are still expected (and consumed).
    pub fn consuming(&self, now: Instant) -> bool {
        !self.da1 && now < self.consume_deadline()
    }

    pub fn hold_deadline(&self) -> Instant {
        self.started + HOLD
    }

    pub fn consume_deadline(&self) -> Instant {
        self.started + CONSUME
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_order() {
        let mut expected = b"\x1b[?2026$p\x1b]11;?\x1b\\".to_vec();
        for index in *b"23456" {
            expected.extend_from_slice(b"\x1b]4;");
            expected.push(index);
            expected.extend_from_slice(b";?\x1b\\");
        }
        // DA1 comes last and closes the probe.
        expected.extend_from_slice(b"\x1b[6n\x1b[c");
        assert_eq!(PROBE, &expected[..]);
    }

    #[test]
    fn terminal_app_is_not_asked_decrqm() {
        let apple = probe_for(TermKind::Apple);
        assert!(!apple.windows(2).any(|pair| pair == b"$p"), "{apple:?}");
        assert_eq!([DECRQM_SYNC, apple].concat(), PROBE);
        // OSC 11 first, DA1 last, as in `PROBE`.
        assert!(apple.starts_with(b"\x1b]11;?"));
        assert!(apple.ends_with(b"\x1b[c"));
        for kind in [
            TermKind::Iterm2,
            TermKind::Vscode,
            TermKind::Zed,
            TermKind::Ghostty,
            TermKind::Unknown,
        ] {
            assert_eq!(probe_for(kind), PROBE, "{kind:?}");
        }
        let probe = Probe::new(Instant::now(), TermKind::Apple);
        assert_eq!(probe.caps.kind, TermKind::Apple);
        assert!(!probe.caps.sync_output);
    }

    #[test]
    fn hold_has_room_for_vscode() {
        // VS Code 1.104's DA1 came at 84.9 ms (findings.md).
        assert!(HOLD >= Duration::from_micros(84_900) * 2);
        assert!(CONSUME >= HOLD);
    }

    #[test]
    fn decrqm() {
        assert!(!sync_supported(0));
        assert!(sync_supported(1));
        assert!(sync_supported(2));
        assert!(sync_supported(3));
        assert!(!sync_supported(4));
    }

    #[test]
    fn background_digits_scale_to_8_bits() {
        assert_eq!(parse_background(b"rgb:1e1e/1e1e/1e1e"), Some((30, 30, 30)));
        assert_eq!(parse_background(b"rgb:ff/80/00"), Some((255, 128, 0)));
        assert_eq!(parse_background(b"rgb:f/8/0"), Some((255, 136, 0)));
        assert_eq!(parse_background(b"rgb:fff/000/800"), Some((255, 0, 128)));
        assert_eq!(parse_background(b"rgb:FFFF/0000/ffff"), Some((255, 0, 255)));
        assert_eq!(parse_background(b"rgb:1/2"), None);
        assert_eq!(parse_background(b"rgb:1/2/3/4"), None);
        assert_eq!(parse_background(b"rgb:12345/0/0"), None);
        assert_eq!(parse_background(b"rgb:/0/0"), None);
        assert_eq!(parse_background(b"rgb:zz/0/0"), None);
        assert_eq!(parse_background(b"#ffffff"), None);
    }

    #[test]
    fn palette_payloads() {
        assert_eq!(
            parse_palette(b"4;rgb:6161/afaf/efef"),
            Some((4, (0x61, 0xaf, 0xef)))
        );
        assert_eq!(parse_palette(b"2;rgb:9/c/7"), Some((2, (0x99, 0xcc, 0x77))));
        assert_eq!(
            parse_palette(b"6;rgb:56/b6/c2"),
            Some((6, (0x56, 0xb6, 0xc2)))
        );
        assert_eq!(parse_palette(b"255;rgb:0/0/0"), Some((255, (0, 0, 0))));
        assert_eq!(parse_palette(b"256;rgb:0/0/0"), None);
        assert_eq!(parse_palette(b"1234;rgb:0/0/0"), None);
        assert_eq!(parse_palette(b";rgb:0/0/0"), None);
        assert_eq!(parse_palette(b"x;rgb:0/0/0"), None);
        assert_eq!(parse_palette(b"+4;rgb:0/0/0"), None);
        assert_eq!(parse_palette(b"4"), None);
        assert_eq!(parse_palette(b"4;"), None);
        assert_eq!(parse_palette(b"4;?"), None);
        assert_eq!(parse_palette(b"4;rgb:0/0"), None);
        assert_eq!(parse_palette(b"4;rgb:0/0/0;5;rgb:0/0/0"), None);
    }

    #[test]
    fn palette_slots() {
        let mut caps = TermCaps::default();
        assert_eq!(caps.ansi, Ansi::default());
        for index in 0..=8 {
            caps.set_palette(index, (index, 0, 0));
        }
        caps.set_palette(9, (9, 9, 9));
        assert_eq!(
            caps.ansi,
            Ansi {
                green: Some((2, 0, 0)),
                yellow: Some((3, 0, 0)),
                blue: Some((4, 0, 0)),
                magenta: Some((5, 0, 0)),
                cyan: Some((6, 0, 0)),
            }
        );
    }

    #[test]
    fn darkness() {
        assert!(is_dark((0x1e, 0x1e, 0x1e)));
        assert!(is_dark((0, 0, 0)));
        assert!(!is_dark((255, 255, 255)));
        assert!(!is_dark((0xfd, 0xf6, 0xe3)));
        // Pure blue is dark, pure green is light.
        assert!(is_dark((0, 0, 255)));
        assert!(!is_dark((0, 255, 0)));
        let mut caps = TermCaps::default();
        assert!(caps.dark);
        caps.set_background((255, 255, 255));
        assert!(!caps.dark);
    }

    #[test]
    fn probe_windows() {
        let start = Instant::now();
        let mut probe = Probe::new(start, TermKind::Unknown);
        assert!(probe.holding(start));
        assert!(!probe.holding(start + HOLD));
        assert!(probe.consuming(start + HOLD));
        assert!(!probe.consuming(start + CONSUME));
        probe.on_device_attrs();
        assert!(!probe.holding(start));
        assert!(!probe.consuming(start));
    }
}
