//! Splits what the terminal sends on stdin into the user's bytes and the
//! terminal's replies to fac's own queries. User bytes are passed on verbatim
//! and in order; nothing is re-encoded.

use memchr::{memchr, memchr2};

use crate::term::caps;
use crate::term::replies::{Expect, ReplyKind};

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;
/// A partial reply longer than this is not a reply.
const MAX_REPLY: usize = 256;

#[derive(Debug, PartialEq, Eq)]
pub enum InputItem<'a> {
    User(&'a [u8]),
    /// A reply, with its bytes as they came.
    Reply {
        reply: TermReply,
        raw: &'a [u8],
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TermReply {
    /// CPR, 1-based as the terminal sends it.
    CursorPos { row: u16, col: u16 },
    /// DECRPM: a private mode and its Ps.
    Mode { mode: u16, ps: u8 },
    /// OSC 11, scaled to 8 bits per channel.
    Background((u8, u8, u8)),
    /// OSC 4: an ANSI colour's index and its value, scaled to 8 bits per
    /// channel. Any index; the caps keep only 2–6.
    Palette(u8, (u8, u8, u8)),
    /// An OSC 4 reply whose body is malformed, with its index when one can
    /// be read. It pops its owner like any reply; fac's is dropped.
    Malformed(Option<u8>),
    /// DA1.
    DeviceAttrs,
}

impl TermReply {
    /// The kind of query it answers. `None` for CPR (`CprQueue` tracks it)
    /// and for an OSC 4 reply whose index can't be read.
    pub fn kind(&self) -> Option<ReplyKind> {
        match *self {
            TermReply::CursorPos { .. } | TermReply::Malformed(None) => None,
            TermReply::Mode { mode, .. } => Some(ReplyKind::Mode(mode)),
            TermReply::Background(_) => Some(ReplyKind::Background),
            TermReply::Palette(index, _) | TermReply::Malformed(Some(index)) => {
                Some(ReplyKind::Palette(index))
            }
            TermReply::DeviceAttrs => Some(ReplyKind::DeviceAttrs),
        }
    }
}

enum Parse {
    /// A reply this many bytes long.
    Reply(usize, TermReply),
    NotReply,
    /// The input ends inside what may still become a reply.
    Incomplete,
}

#[derive(Default)]
pub struct InputTokenizer {
    /// A partial sequence held at the end of a read.
    pending: Vec<u8>,
}

impl InputTokenizer {
    pub fn new() -> Self {
        Self::default()
    }

    /// `expect`: the reply kinds fac has a query of outstanding (only
    /// during the startup probe); DECRPM, OSC 11, OSC 4 and DA1
    /// replies are recognised only for those, an OSC 4 one by its index.
    /// `cpr_outstanding`: true while any CPR (fac's or the shell's) is
    /// outstanding. A CPR is always recognised, except that `CSI 1 ; N R` is
    /// also modified F3, so it is a key unless a CPR is outstanding.
    /// `partial_keys`: true while the popup is open; a partial key sequence
    /// (a lone ESC, `ESC [ …`, `ESC O`) is then held too, so a key split
    /// across reads is seen whole.
    /// A partial sequence at the end of the input is held only when one of the
    /// three expects something; otherwise every byte is User immediately.
    pub fn feed(
        &mut self,
        input: &[u8],
        expect: &Expect,
        cpr_outstanding: bool,
        partial_keys: bool,
        out: &mut impl FnMut(InputItem<'_>),
    ) {
        let joined;
        let data = if self.pending.is_empty() {
            input
        } else {
            let mut pending = std::mem::take(&mut self.pending);
            pending.extend_from_slice(input);
            joined = pending;
            &joined[..]
        };
        let hold = expect.any() || cpr_outstanding || partial_keys;
        let mut run = 0;
        let mut i = 0;
        while let Some(off) = memchr(ESC, &data[i..]) {
            let at = i + off;
            match parse_reply(&data[at..], expect, cpr_outstanding, partial_keys) {
                Parse::Reply(len, reply) => {
                    if run < at {
                        out(InputItem::User(&data[run..at]));
                    }
                    out(InputItem::Reply {
                        reply,
                        raw: &data[at..at + len],
                    });
                    i = at + len;
                    run = i;
                }
                Parse::Incomplete if hold => {
                    if run < at {
                        out(InputItem::User(&data[run..at]));
                    }
                    self.pending = data[at..].to_vec();
                    return;
                }
                Parse::Incomplete | Parse::NotReply => i = at + 1,
            }
        }
        if run < data.len() {
            out(InputItem::User(&data[run..]));
        }
    }

    /// A partial sequence held at the end of a read. The proxy flushes it as
    /// User bytes after 10 ms.
    pub fn take_pending(&mut self) -> Option<Vec<u8>> {
        (!self.pending.is_empty()).then(|| std::mem::take(&mut self.pending))
    }

    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }
}

/// Parse a reply at the start of `seq`, which starts with ESC. `keys`: an
/// SS3 key with its last byte still to come is incomplete too.
fn parse_reply(seq: &[u8], expect: &Expect, cpr: bool, keys: bool) -> Parse {
    let parse = match seq.get(1) {
        None => Parse::Incomplete,
        Some(b'[') => parse_csi(seq, expect, cpr, keys),
        Some(b']') if expect.any_osc() => parse_osc(seq, expect),
        Some(b'O') if keys && seq.len() == 2 => Parse::Incomplete,
        Some(_) => Parse::NotReply,
    };
    match parse {
        Parse::Incomplete if seq.len() > MAX_REPLY => Parse::NotReply,
        parse => parse,
    }
}

/// CPR `CSI r ; c R` (and DECXCPR `CSI ? r ; c [; p] R`), DECRQM
/// `CSI ? Pd ; Ps $ y` and DA1 `CSI ? … c`. `keys`: a CSI with a `:` in its
/// parameters (a kitty key event) and its final byte still to come is
/// incomplete too.
fn parse_csi(seq: &[u8], expect: &Expect, cpr: bool, keys: bool) -> Parse {
    let mut i = 2;
    let private = seq.get(i) == Some(&b'?');
    if private {
        i += 1;
    }
    let mut params = [0u32; 4];
    let mut count = 0;
    let mut empty_param = true;
    let mut dollar = false;
    let final_byte = loop {
        let Some(&byte) = seq.get(i) else {
            return Parse::Incomplete;
        };
        i += 1;
        match byte {
            b'0'..=b'9' if !dollar => {
                if count == 0 {
                    count = 1;
                }
                if let Some(param) = params.get_mut(count - 1) {
                    *param = param
                        .saturating_mul(10)
                        .saturating_add(u32::from(byte - b'0'));
                }
                empty_param = false;
            }
            b';' if !dollar => {
                if empty_param {
                    return Parse::NotReply;
                }
                count = count.max(1) + 1;
                empty_param = true;
            }
            b'$' if !dollar => dollar = true,
            b':' if !dollar => return colon_params(&seq[i..], keys),
            0x40..=0x7e => break byte,
            _ => return Parse::NotReply,
        }
    };
    let params = &params[..count.min(params.len())];
    let as_u16 = |value: u32| u16::try_from(value).unwrap_or(u16::MAX);
    let reply = match (private, dollar, final_byte, params) {
        (false, false, b'R', &[row, col]) if !empty_param && (row != 1 || cpr) => {
            TermReply::CursorPos {
                row: as_u16(row),
                col: as_u16(col),
            }
        }
        (true, false, b'R', &[row, col] | &[row, col, _]) if !empty_param => TermReply::CursorPos {
            row: as_u16(row),
            col: as_u16(col),
        },
        (true, true, b'y', &[mode, ps])
            if !empty_param
                && u16::try_from(mode).is_ok_and(|mode| expect.expects(ReplyKind::Mode(mode))) =>
        {
            TermReply::Mode {
                mode: as_u16(mode),
                ps: u8::try_from(ps).unwrap_or(u8::MAX),
            }
        }
        (true, false, b'c', _) if expect.expects(ReplyKind::DeviceAttrs) => TermReply::DeviceAttrs,
        _ => return Parse::NotReply,
    };
    Parse::Reply(i, reply)
}

/// The rest of a CSI's parameters after a `:`, which no reply has: not a
/// reply, but incomplete when `keys` and they run to the end of `rest`.
fn colon_params(rest: &[u8], keys: bool) -> Parse {
    match rest.iter().find(|byte| !(0x30..=0x3f).contains(*byte)) {
        None if keys => Parse::Incomplete,
        _ => Parse::NotReply,
    }
}

/// OSC 11 `ESC ] 11 ; rgb:… (BEL | ESC \)` and OSC 4 `ESC ] 4 ; n ; rgb:…
/// (BEL | ESC \)`, each only when `expect` has its kind. An OSC 11 reply
/// with a bad body is not a reply; an OSC 4 one is a
/// [`TermReply::Malformed`].
fn parse_osc(seq: &[u8], expect: &Expect) -> Parse {
    if expect.expects(ReplyKind::Background) {
        match osc_body(seq, b"\x1b]11;") {
            Ok((len, body)) => {
                return caps::parse_background(body).map_or(Parse::NotReply, |rgb| {
                    Parse::Reply(len, TermReply::Background(rgb))
                });
            }
            Err(Parse::NotReply) => {}
            Err(parse) => return parse,
        }
    }
    if !expect.any_palette() {
        return Parse::NotReply;
    }
    match osc_body(seq, b"\x1b]4;") {
        Ok((len, body)) => {
            // By its index; one that can't be read is a reply while any
            // OSC 4 one is expected.
            let index = memchr(b';', body).map_or(body, |at| &body[..at]);
            let index = caps::color_index(index);
            if index.is_some_and(|index| !expect.expects(ReplyKind::Palette(index))) {
                return Parse::NotReply;
            }
            let reply = caps::parse_palette(body)
                .map_or(TermReply::Malformed(index), |(index, rgb)| {
                    TermReply::Palette(index, rgb)
                });
            Parse::Reply(len, reply)
        }
        Err(parse) => parse,
    }
}

/// The length of the OSC sequence at the start of `seq` and its body, the
/// bytes between `prefix` and the BEL or ST that ends it. `Err` when `seq`
/// does not start with `prefix` (NotReply), or ends before the terminator
/// (Incomplete), or an ESC in it is not ST (NotReply).
fn osc_body<'a>(seq: &'a [u8], prefix: &[u8]) -> Result<(usize, &'a [u8]), Parse> {
    let prefix_len = seq.len().min(prefix.len());
    if seq[..prefix_len] != prefix[..prefix_len] {
        return Err(Parse::NotReply);
    }
    if seq.len() < prefix.len() {
        return Err(Parse::Incomplete);
    }
    let body = &seq[prefix.len()..];
    let Some(at) = memchr2(BEL, ESC, body) else {
        return Err(Parse::Incomplete);
    };
    let len = if body[at] == BEL {
        prefix.len() + at + 1
    } else {
        match body.get(at + 1) {
            None => return Err(Parse::Incomplete),
            Some(b'\\') => prefix.len() + at + 2,
            Some(_) => return Err(Parse::NotReply),
        }
    };
    Ok((len, &body[..at]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::caps::PROBE;
    use crate::term::replies::queries;

    /// What fac's whole probe is outstanding for.
    fn probe() -> Expect {
        Expect::of(queries(PROBE))
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Item {
        User(Vec<u8>),
        Reply(TermReply),
    }

    fn collect(
        tokenizer: &mut InputTokenizer,
        input: &[u8],
        expect: &Expect,
        cpr: bool,
        items: &mut Vec<Item>,
    ) {
        tokenizer.feed(input, expect, cpr, false, &mut |item| match item {
            InputItem::User(bytes) => {
                assert!(!bytes.is_empty(), "empty User item");
                if let Some(Item::User(last)) = items.last_mut() {
                    last.extend_from_slice(bytes);
                } else {
                    items.push(Item::User(bytes.to_vec()));
                }
            }
            InputItem::Reply { reply, .. } => items.push(Item::Reply(reply)),
        });
    }

    fn tokenize(input: &[u8], expect: &Expect, cpr: bool) -> Vec<Item> {
        let mut tokenizer = InputTokenizer::new();
        let mut items = Vec::new();
        collect(&mut tokenizer, input, expect, cpr, &mut items);
        assert!(!tokenizer.has_pending(), "{input:?} left a partial");
        items
    }

    fn user(bytes: &[u8]) -> Item {
        Item::User(bytes.to_vec())
    }

    fn reply(reply: TermReply) -> Item {
        Item::Reply(reply)
    }

    fn cpr(row: u16, col: u16) -> Item {
        reply(TermReply::CursorPos { row, col })
    }

    #[test]
    fn each_reply() {
        assert_eq!(tokenize(b"\x1b[12;40R", &probe(), true), vec![cpr(12, 40)]);
        assert_eq!(tokenize(b"\x1b[?12;40R", &probe(), true), vec![cpr(12, 40)]);
        assert_eq!(
            tokenize(b"\x1b[?12;40;1R", &probe(), true),
            vec![cpr(12, 40)]
        );
        assert_eq!(
            tokenize(b"\x1b[?2026;2$y", &probe(), false),
            vec![reply(TermReply::Mode { mode: 2026, ps: 2 })]
        );
        assert_eq!(
            tokenize(b"\x1b]11;rgb:1e1e/1e1e/1e1e\x1b\\", &probe(), false),
            vec![reply(TermReply::Background((30, 30, 30)))]
        );
        assert_eq!(
            tokenize(b"\x1b]11;rgb:ff/ff/ff\x07", &probe(), false),
            vec![reply(TermReply::Background((255, 255, 255)))]
        );
        assert_eq!(
            tokenize(b"\x1b[?62;22c", &probe(), false),
            vec![reply(TermReply::DeviceAttrs)]
        );
        assert_eq!(
            tokenize(b"\x1b[?1;2c", &probe(), false),
            vec![reply(TermReply::DeviceAttrs)]
        );
    }

    #[test]
    fn replies_interleaved_with_typing() {
        let input = b"ab\x1b[5;10Rcd\x1b[?2026;0$y\x1b[Ae\x1b[?62c\x7f";
        assert_eq!(
            tokenize(input, &probe(), true),
            vec![
                user(b"ab"),
                cpr(5, 10),
                user(b"cd"),
                reply(TermReply::Mode { mode: 2026, ps: 0 }),
                user(b"\x1b[Ae"),
                reply(TermReply::DeviceAttrs),
                user(b"\x7f"),
            ]
        );
    }

    #[test]
    fn keys_pass_verbatim() {
        for keys in [
            &b"\x1b[A\x1b[B\x1bOP\x1b[15~"[..],
            b"\x1b[200~pasted\x1b[201~",
            b"\x1bb\x1b\x7f",
            b"\x1b[1;5D",
            b"\x1b]0;x\x07",
            b"\x1b[?2026;2$y\x1b[?62c\x1b]11;rgb:0/0/0\x07",
            b"\x1b]4;4;rgb:61/af/ef\x07\x1b]4;9;rgb:0/0/0\x1b\\\x1b]4;x\x07",
        ] {
            assert_eq!(
                tokenize(keys, &Expect::none(), false),
                vec![user(keys)],
                "{keys:?}"
            );
        }
        // With replies expected, non-replies still pass verbatim.
        assert_eq!(
            tokenize(b"\x1b[1;5D\x1bOQ\x1b]0;x\x07", &probe(), true),
            vec![user(b"\x1b[1;5D\x1bOQ\x1b]0;x\x07")]
        );
    }

    #[test]
    fn a_reply_comes_with_its_bytes() {
        let mut tokenizer = InputTokenizer::new();
        let mut raws = Vec::new();
        tokenizer.feed(
            b"a\x1b[?5;10;1Rb\x1b[7;3R",
            &Expect::none(),
            true,
            false,
            &mut |item| {
                if let InputItem::Reply { raw, .. } = item {
                    raws.push(raw.to_vec());
                }
            },
        );
        assert_eq!(raws, vec![b"\x1b[?5;10;1R".to_vec(), b"\x1b[7;3R".to_vec()]);
    }

    #[test]
    fn a_cpr_is_recognised_without_expectations() {
        assert_eq!(
            tokenize(b"x\x1b[7;3Ry", &Expect::none(), false),
            vec![user(b"x"), cpr(7, 3), user(b"y")]
        );
    }

    #[test]
    fn lone_esc_passes_straight_through_when_nothing_is_expected() {
        let mut tokenizer = InputTokenizer::new();
        let mut items = Vec::new();
        collect(&mut tokenizer, b"\x1b", &Expect::none(), false, &mut items);
        assert_eq!(items, vec![user(b"\x1b")]);
        assert!(!tokenizer.has_pending());
        let mut items = Vec::new();
        collect(
            &mut tokenizer,
            b"a\x1b[12;",
            &Expect::none(),
            false,
            &mut items,
        );
        assert_eq!(items, vec![user(b"a\x1b[12;")]);
        assert!(!tokenizer.has_pending());
    }

    #[test]
    fn partial_reply_is_held_and_flushed() {
        let mut tokenizer = InputTokenizer::new();
        let mut items = Vec::new();
        collect(&mut tokenizer, b"a\x1b", &probe(), false, &mut items);
        assert_eq!(items, vec![user(b"a")]);
        assert!(tokenizer.has_pending());
        assert_eq!(tokenizer.take_pending(), Some(b"\x1b".to_vec()));
        assert!(!tokenizer.has_pending());
        assert_eq!(tokenizer.take_pending(), None);
    }

    #[test]
    fn partial_keys_are_held_while_the_popup_is_open() {
        for (first, rest) in [
            (&b"a\x1b"[..], &b"[B"[..]),
            (b"a\x1b[", b"B"),
            (b"a\x1bO", b"B"),
            (b"a\x1b[20", b"0~x"),
            (b"a\x1b[13;1", b":3u"),
            (b"a\x1b[27;2;", b"9~"),
        ] {
            let mut tokenizer = InputTokenizer::new();
            let mut items = Vec::new();
            let mut feed = |input: &[u8], items: &mut Vec<Item>| {
                tokenizer.feed(input, &Expect::none(), false, true, &mut |item| {
                    if let InputItem::User(bytes) = item {
                        items.push(user(bytes));
                    }
                });
            };
            feed(first, &mut items);
            assert_eq!(items, vec![user(b"a")], "{first:?}");
            feed(rest, &mut items);
            let mut whole = first[1..].to_vec();
            whole.extend_from_slice(rest);
            assert_eq!(items, vec![user(b"a"), user(&whole)], "{first:?}");
        }
        // A whole key is not held.
        let mut tokenizer = InputTokenizer::new();
        tokenizer.feed(b"\x1bOB", &Expect::none(), false, true, &mut |_| {});
        assert!(!tokenizer.has_pending());
    }

    #[test]
    fn kitty_keys_split_at_every_byte_reach_the_key_decoder_whole() {
        use crate::complete::selection::Key;
        use crate::term::keys::{KeyDecoder, Token};

        for (input, expected) in [
            (&b"\x1b[13;1:3u"[..], Token::Release),
            (b"\x1b[1;1:2B", Token::Key(Key::Down)),
        ] {
            for split in 1..input.len() {
                let (first, rest) = input.split_at(split);
                let mut tokenizer = InputTokenizer::new();
                let mut user = Vec::new();
                for chunk in [first, rest] {
                    tokenizer.feed(chunk, &Expect::none(), false, true, &mut |item| {
                        if let InputItem::User(bytes) = item {
                            user.push(bytes.to_vec());
                        }
                    });
                }
                assert_eq!(user, vec![input.to_vec()], "{input:?} split at {split}");
                let mut decoder = KeyDecoder::with_popout(None);
                assert_eq!(
                    decoder.next(&user[0], true),
                    (expected, input.len()),
                    "{input:?} split at {split}"
                );
            }
        }
        // With the popup closed, nothing is held: the halves pass as they
        // came, as before.
        let mut tokenizer = InputTokenizer::new();
        let mut user = Vec::new();
        for chunk in [&b"\x1b[13;1:"[..], b"3u"] {
            tokenizer.feed(chunk, &Expect::none(), false, false, &mut |item| {
                if let InputItem::User(bytes) = item {
                    user.push(bytes.to_vec());
                }
            });
        }
        assert_eq!(user, vec![b"\x1b[13;1:".to_vec(), b"3u".to_vec()]);
        // A whole one, and a colon sequence with a byte no parameter has,
        // are not held.
        for input in [&b"\x1b[13;1:3u"[..], b"\x1b[13;1:\x07"] {
            let mut tokenizer = InputTokenizer::new();
            tokenizer.feed(input, &Expect::none(), false, true, &mut |_| {});
            assert!(!tokenizer.has_pending(), "{input:?}");
        }
    }

    #[test]
    fn f3_is_a_key_unless_a_cpr_is_outstanding() {
        assert_eq!(
            tokenize(b"\x1b[1;5R", &probe(), false),
            vec![user(b"\x1b[1;5R")]
        );
        assert_eq!(
            tokenize(b"\x1b[1;5R", &Expect::none(), false),
            vec![user(b"\x1b[1;5R")]
        );
        assert_eq!(
            tokenize(b"\x1b[1;5R", &Expect::none(), true),
            vec![cpr(1, 5)]
        );
        assert_eq!(
            tokenize(b"\x1b[2;5R", &Expect::none(), false),
            vec![cpr(2, 5)]
        );
    }

    #[test]
    fn malformed_replies_are_user_bytes() {
        for input in [
            &b"\x1b[;5R"[..],
            b"\x1b[5;R",
            b"\x1b[5R",
            b"\x1b[1;2;3R",
            b"\x1b[?2027;1$y",
            b"\x1b]11;nonsense\x07",
            b"\x1b]11;rgb:0/0/0\x1bx",
        ] {
            assert_eq!(
                tokenize(input, &probe(), true),
                vec![user(input)],
                "{input:?}"
            );
        }
    }

    fn mixed() -> Vec<u8> {
        let mut input = Vec::new();
        input.extend_from_slice("ls 日本\x1b[A".as_bytes());
        input.extend_from_slice(b"\x1b[?2026;2$y");
        input.extend_from_slice(b"q\x1b]11;rgb:1e1e/1e1e/1e1e\x1b\\");
        input.extend_from_slice(b"\x1b[24;80R");
        input.extend_from_slice(b"\x1bOPz\x1b[1;5R");
        input.extend_from_slice(b"\x1b]11;rgb:ff/ff/ff\x07\x1b[?62;22c!");
        input
    }

    #[test]
    fn every_split_point_gives_the_same_result() {
        let input = mixed();
        let whole = tokenize(&input, &probe(), true);
        assert_eq!(
            whole
                .iter()
                .filter(|item| matches!(item, Item::Reply(_)))
                .count(),
            6,
            "{whole:?}"
        );
        for split in 0..=input.len() {
            let (a, b) = input.split_at(split);
            let mut tokenizer = InputTokenizer::new();
            let mut items = Vec::new();
            collect(&mut tokenizer, a, &probe(), true, &mut items);
            collect(&mut tokenizer, b, &probe(), true, &mut items);
            assert!(!tokenizer.has_pending());
            assert_eq!(items, whole, "split at {split}");
        }
    }

    #[test]
    fn overlong_partial_is_not_held() {
        let mut input = b"\x1b]11;".to_vec();
        input.extend(std::iter::repeat_n(b'x', MAX_REPLY));
        assert_eq!(tokenize(&input, &probe(), true), vec![user(&input)]);
        let mut input = b"\x1b]4;4;".to_vec();
        input.extend(std::iter::repeat_n(b'x', MAX_REPLY));
        assert_eq!(tokenize(&input, &probe(), true), vec![user(&input)]);
    }

    fn palette(index: u8, rgb: (u8, u8, u8)) -> Item {
        reply(TermReply::Palette(index, rgb))
    }

    #[test]
    fn osc4_ends_in_bel_or_st() {
        assert_eq!(
            tokenize(b"\x1b]4;4;rgb:6161/afaf/efef\x1b\\", &probe(), false),
            vec![palette(4, (0x61, 0xaf, 0xef))]
        );
        assert_eq!(
            tokenize(b"\x1b]4;2;rgb:9898/c3c3/7979\x07", &probe(), false),
            vec![palette(2, (0x98, 0xc3, 0x79))]
        );
        // With the bytes as they came.
        let mut tokenizer = InputTokenizer::new();
        let mut raws = Vec::new();
        tokenizer.feed(
            b"\x1b]4;3;rgb:e5/c0/7b\x07",
            &probe(),
            false,
            false,
            &mut |item| {
                if let InputItem::Reply { raw, .. } = item {
                    raws.push(raw.to_vec());
                }
            },
        );
        assert_eq!(raws, vec![b"\x1b]4;3;rgb:e5/c0/7b\x07".to_vec()]);
    }

    #[test]
    fn osc4_channel_digits() {
        for (input, rgb) in [
            (&b"\x1b]4;5;rgb:c/7/d\x07"[..], (0xcc, 0x77, 0xdd)),
            (b"\x1b]4;5;rgb:c6/78/dd\x07", (0xc6, 0x78, 0xdd)),
            (b"\x1b]4;5;rgb:c6c6/7878/dddd\x07", (0xc6, 0x78, 0xdd)),
            // Mixed counts, as rgb: allows.
            (b"\x1b]4;5;rgb:f/80/0000\x07", (0xff, 0x80, 0x00)),
        ] {
            assert_eq!(
                tokenize(input, &probe(), false),
                vec![palette(5, rgb)],
                "{input:?}"
            );
        }
    }

    #[test]
    fn osc4_with_a_garbage_body_is_consumed_and_dropped() {
        for (input, index) in [
            (&b"\x1b]4;4;nonsense\x07"[..], Some(4)),
            (b"\x1b]4;4;rgb:12345/0/0\x1b\\", Some(4)),
            (b"\x1b]4;4;rgb:0/0\x07", Some(4)),
            (b"\x1b]4;x;rgb:0/0/0\x07", None),
            (b"\x1b]4;\x07", None),
            (b"\x1b]4;4;?\x1b\\", Some(4)),
            (b"\x1b]4;2;rgb:0/0/0;3;rgb:0/0/0\x07", Some(2)),
        ] {
            assert_eq!(
                tokenize(input, &probe(), false),
                vec![reply(TermReply::Malformed(index))],
                "{input:?}"
            );
        }
        // Typing around it is kept, in order.
        assert_eq!(
            tokenize(b"a\x1b]4;4;junk\x07b", &probe(), false),
            vec![user(b"a"), reply(TermReply::Malformed(Some(4))), user(b"b")]
        );
        // One whose index fac didn't ask is the user's; one with no index
        // is a reply while any OSC 4 one is expected.
        let blue = Expect::of([ReplyKind::Palette(4)]);
        assert_eq!(
            tokenize(b"\x1b]4;5;junk\x07", &blue, false),
            vec![user(b"\x1b]4;5;junk\x07")]
        );
        assert_eq!(
            tokenize(b"\x1b]4;junk\x07", &blue, false),
            vec![reply(TermReply::Malformed(None))]
        );
        assert_eq!(
            tokenize(
                b"\x1b]4;junk\x07",
                &Expect::of([ReplyKind::Background]),
                false
            ),
            vec![user(b"\x1b]4;junk\x07")]
        );
    }

    #[test]
    fn osc4_is_recognised_by_its_index() {
        // fac asks 2-6 only: another index is the user's (or the shell's).
        for input in [
            &b"\x1b]4;1;rgb:ff/00/00\x07"[..],
            b"\x1b]4;9;rgb:ff/00/00\x07",
            b"\x1b]4;255;rgb:0/0/0\x1b\\",
        ] {
            assert_eq!(
                tokenize(input, &probe(), false),
                vec![user(input)],
                "{input:?}"
            );
        }
        let expect = Expect::of([ReplyKind::Palette(9), ReplyKind::Palette(255)]);
        assert_eq!(
            tokenize(b"\x1b]4;9;rgb:ff/00/00\x07", &expect, false),
            vec![palette(9, (255, 0, 0))]
        );
        assert_eq!(
            tokenize(b"\x1b]4;255;rgb:0/0/0\x1b\\", &expect, false),
            vec![palette(255, (0, 0, 0))]
        );
    }

    /// One reply of each kind fac can recognise.
    const EACH_KIND: [(&[u8], TermReply); 5] = [
        (b"\x1b[?2026;2$y", TermReply::Mode { mode: 2026, ps: 2 }),
        (
            b"\x1b]11;rgb:1e/1e/1e\x07",
            TermReply::Background((30, 30, 30)),
        ),
        (
            b"\x1b]4;1;rgb:ff/00/00\x07",
            TermReply::Palette(1, (255, 0, 0)),
        ),
        (b"\x1b]4;1;junk\x07", TermReply::Malformed(Some(1))),
        (b"\x1b[?62;22c", TermReply::DeviceAttrs),
    ];

    #[test]
    fn each_kind_is_recognised_only_while_expected() {
        for (input, expected) in EACH_KIND {
            let Some(kind) = expected.kind() else {
                panic!("{expected:?} has a kind");
            };
            assert_eq!(
                tokenize(input, &Expect::of([kind]), false),
                vec![reply(expected)],
                "{input:?}"
            );
            // Not without its own flag, whatever else is expected.
            for (_, other) in EACH_KIND {
                let Some(other) = other.kind().filter(|&other| other != kind) else {
                    continue;
                };
                assert_eq!(
                    tokenize(input, &Expect::of([other]), true),
                    vec![user(input)],
                    "{input:?} with {other:?}"
                );
            }
            assert_eq!(tokenize(input, &Expect::none(), true), vec![user(input)]);
        }
        // DECRPM by mode.
        assert_eq!(
            tokenize(b"\x1b[?2004;2$y", &probe(), false),
            vec![user(b"\x1b[?2004;2$y")]
        );
        assert_eq!(
            tokenize(
                b"\x1b[?2004;2$y",
                &Expect::of([ReplyKind::Mode(2004)]),
                false
            ),
            vec![reply(TermReply::Mode { mode: 2004, ps: 2 })]
        );
    }

    #[test]
    fn osc_replies_end_in_bel_or_st() {
        for (kind, body, expected) in [
            (
                ReplyKind::Background,
                &b"11;rgb:1e1e/1e1e/1e1e"[..],
                TermReply::Background((30, 30, 30)),
            ),
            (
                ReplyKind::Palette(3),
                b"4;3;rgb:e5e5/c0c0/7b7b",
                TermReply::Palette(3, (0xe5, 0xc0, 0x7b)),
            ),
        ] {
            for end in [&b"\x07"[..], b"\x1b\\"] {
                let input = [&b"\x1b]"[..], body, end].concat();
                let expect = Expect::of([kind]);
                assert_eq!(
                    tokenize(&input, &expect, false),
                    vec![reply(expected)],
                    "{input:?}"
                );
                // Whole, and split anywhere.
                for split in 0..=input.len() {
                    let (a, b) = input.split_at(split);
                    let mut tokenizer = InputTokenizer::new();
                    let mut items = Vec::new();
                    collect(&mut tokenizer, a, &expect, false, &mut items);
                    collect(&mut tokenizer, b, &expect, false, &mut items);
                    assert!(!tokenizer.has_pending());
                    assert_eq!(items, vec![reply(expected)], "split at {split}");
                }
            }
        }
    }

    #[test]
    fn nothing_is_held_unless_something_is_outstanding() {
        // The tokenizer holds a partial sequence only while a reply kind,
        // a CPR or the popup expects one, as before per-kind flags.
        let held = |input: &[u8], expect: &Expect| {
            let mut tokenizer = InputTokenizer::new();
            let mut items = Vec::new();
            collect(&mut tokenizer, input, expect, false, &mut items);
            assert_eq!(items.is_empty(), tokenizer.has_pending(), "{input:?}");
            tokenizer.has_pending()
        };
        let osc = [&b"\x1b]"[..], b"\x1b]11;rgb:"];
        let other = [&b"\x1b"[..], b"\x1b[", b"\x1b[?62"];
        for input in osc.iter().chain(&other) {
            assert!(!held(input, &Expect::none()), "{input:?}");
            assert!(held(input, &probe()), "{input:?}");
        }
        // With no OSC kind expected, a partial OSC isn't waited for either.
        let da1 = Expect::of([ReplyKind::DeviceAttrs]);
        for input in osc {
            assert!(!held(input, &da1), "{input:?}");
        }
        for input in other {
            assert!(held(input, &da1), "{input:?}");
        }
    }

    #[test]
    fn osc4_with_a_broken_terminator_is_not_a_reply() {
        // As for OSC 11: an ESC in the body that is not ST ends the sequence.
        for input in [
            &b"\x1b]4;4;rgb:0/0/0\x1bx"[..],
            b"\x1b]40;x\x07",
            b"\x1b]0;x\x07",
        ] {
            assert_eq!(
                tokenize(input, &probe(), true),
                vec![user(input)],
                "{input:?}"
            );
        }
    }

    #[test]
    fn osc4_after_da1_inside_the_consume_window() {
        // The reads that carry DA1 and what follows it are still parsed with
        // replies expected.
        let input = b"\x1b]11;rgb:1e/1e/1e\x07\x1b[?62;22c\x1b]4;6;rgb:56/b6/c2\x1b\\x";
        assert_eq!(
            tokenize(input, &probe(), false),
            vec![
                reply(TermReply::Background((30, 30, 30))),
                reply(TermReply::DeviceAttrs),
                palette(6, (0x56, 0xb6, 0xc2)),
                user(b"x"),
            ]
        );
        let mut tokenizer = InputTokenizer::new();
        let mut items = Vec::new();
        collect(
            &mut tokenizer,
            b"\x1b[?62;22c\x1b]4;6;rgb:56",
            &probe(),
            false,
            &mut items,
        );
        assert!(tokenizer.has_pending());
        collect(&mut tokenizer, b"/b6/c2\x07", &probe(), false, &mut items);
        assert!(!tokenizer.has_pending());
        assert_eq!(
            items,
            vec![
                reply(TermReply::DeviceAttrs),
                palette(6, (0x56, 0xb6, 0xc2))
            ]
        );
    }

    /// The probe's whole answer, as a terminal that knows OSC 4 sends it.
    fn probe_answer() -> Vec<u8> {
        let mut input = Vec::new();
        input.extend_from_slice(b"\x1b[?2026;2$y\x1b]11;rgb:1e1e/1e1e/1e1e\x1b\\");
        input.extend_from_slice(b"\x1b]4;2;rgb:9898/c3c3/7979\x1b\\");
        input.extend_from_slice(b"\x1b]4;3;rgb:e5/c0/7b\x07");
        input.extend_from_slice(b"\x1b]4;4;rgb:6/a/e\x1b\\");
        input.extend_from_slice(b"\x1b]4;5;garbage\x07");
        input.extend_from_slice(b"ls\x1b]4;6;rgb:5656/b6b6/c2c2\x07");
        input.extend_from_slice(b"\x1b[24;80R\x1b[?62;22c!");
        input
    }

    #[test]
    fn osc4_split_at_every_point_gives_the_same_result() {
        let input = probe_answer();
        let whole = tokenize(&input, &probe(), true);
        assert_eq!(
            whole,
            vec![
                reply(TermReply::Mode { mode: 2026, ps: 2 }),
                reply(TermReply::Background((30, 30, 30))),
                palette(2, (0x98, 0xc3, 0x79)),
                palette(3, (0xe5, 0xc0, 0x7b)),
                palette(4, (0x66, 0xaa, 0xee)),
                reply(TermReply::Malformed(Some(5))),
                user(b"ls"),
                palette(6, (0x56, 0xb6, 0xc2)),
                cpr(24, 80),
                reply(TermReply::DeviceAttrs),
                user(b"!"),
            ]
        );
        for split in 0..=input.len() {
            let (a, b) = input.split_at(split);
            let mut tokenizer = InputTokenizer::new();
            let mut items = Vec::new();
            collect(&mut tokenizer, a, &probe(), true, &mut items);
            collect(&mut tokenizer, b, &probe(), true, &mut items);
            assert!(!tokenizer.has_pending());
            assert_eq!(items, whole, "split at {split}");
        }
        // No byte of any OSC 4 reply is ever User.
        for split in 0..=input.len() {
            let (a, b) = input.split_at(split);
            let mut tokenizer = InputTokenizer::new();
            let mut typed = Vec::new();
            for part in [a, b] {
                tokenizer.feed(part, &probe(), true, false, &mut |item| {
                    if let InputItem::User(bytes) = item {
                        typed.extend_from_slice(bytes);
                    }
                });
            }
            assert_eq!(typed, b"ls!", "split at {split}");
        }
    }
}
