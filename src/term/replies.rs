//! Who is waiting for each terminal reply fac asks about, other than CPR
//! (which has its own queue, `cpr.rs`): DECRPM by mode, OSC 11, OSC 4 by
//! index and DA1. Every query of a tracked kind is queued in write
//! order, fac's own (from the startup probe) and the shell's alike;
//! terminals answer in order, so each reply pops its owner.
//!
//! A reply is taken for fac's only while fac has a query of its kind
//! queued. The shell's queries are recorded only then too: otherwise its
//! replies already pass through untouched. So once fac's last query of a
//! kind is answered or dropped, the shell's still queued for that kind are
//! forgotten with it: a later query of fac's must not find them first.

use std::collections::VecDeque;

use crate::term::scanner::{OutputEvent, OutputScanner, Segment};

/// The shell's queries kept per kind; older ones past this are dropped.
pub const MAX_SHELL: usize = 32;

/// A kind of query, and of the reply that answers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyKind {
    /// DECRQM `CSI ? Pd $ p`, answered by DECRPM, for this private mode.
    Mode(u16),
    /// OSC 11 `?`.
    Background,
    /// OSC 4 `?` for this colour index.
    Palette(u8),
    /// DA1 `CSI c`.
    DeviceAttrs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owner {
    /// fac's own query: its reply is consumed.
    Ours,
    /// The shell's query: its reply is forwarded to the shell, raw.
    Shell,
}

/// The queries in `bytes`, in order, as the output scanner finds them.
pub fn queries(bytes: &[u8]) -> Vec<ReplyKind> {
    let mut scanner = OutputScanner::new();
    let mut kinds = Vec::new();
    scanner.feed(bytes, &mut |segment| {
        if let Segment::Event(OutputEvent::ShellQueries(found)) = segment {
            kinds.extend(found);
        }
    });
    kinds
}

#[derive(Debug, Clone, Copy)]
struct Entry {
    owner: Owner,
    /// Write order across every kind.
    seq: u64,
}

#[derive(Default)]
pub struct ReplyQueues {
    /// One queue per kind that has had a query; few, so a list.
    queues: Vec<(ReplyKind, VecDeque<Entry>)>,
    next_seq: u64,
    /// `Ours` entries queued, over every kind.
    ours: usize,
}

impl ReplyQueues {
    pub fn new() -> Self {
        Self::default()
    }

    /// fac wrote `bytes`: an `Ours` entry for each query in it.
    pub fn push_ours_for(&mut self, bytes: &[u8]) {
        for kind in queries(bytes) {
            self.push(kind, Owner::Ours);
            self.ours += 1;
        }
    }

    /// The shell's output carried a query of `kind`, about to be written
    /// to the terminal. Recorded only while fac has one of the same kind
    /// queued.
    pub fn push_shell(&mut self, kind: ReplyKind) {
        if !self.has_ours(kind) {
            return;
        }
        self.push(kind, Owner::Shell);
        let Some(queue) = self.queue_mut(kind) else {
            return;
        };
        let shell = queue
            .iter()
            .filter(|entry| entry.owner == Owner::Shell)
            .count();
        if shell > MAX_SHELL
            && let Some(at) = queue.iter().position(|entry| entry.owner == Owner::Shell)
        {
            queue.remove(at);
            tracing::debug!(
                ?kind,
                "more than {MAX_SHELL} shell queries queued; oldest dropped"
            );
        }
    }

    /// The owner of a reply of `kind` that just arrived; `None` when no
    /// query of its kind is queued. When it was fac's last of its kind, the
    /// shell's of that kind are forgotten. fac's DA1 reply comes last: when
    /// it pops, fac's queries of other kinds still queued will never be
    /// answered and are dropped, the shell's of those kinds with them.
    pub fn pop(&mut self, kind: ReplyKind) -> Option<Owner> {
        let queue = self.queue_mut(kind)?;
        let entry = queue.pop_front()?;
        if entry.owner == Owner::Ours {
            if !queue.iter().any(|entry| entry.owner == Owner::Ours) {
                // Only the shell's are left; their replies pass raw now.
                queue.clear();
            }
            self.ours -= 1;
            if kind == ReplyKind::DeviceAttrs {
                self.drop_ours(|other| other != ReplyKind::DeviceAttrs, "DA1");
            }
        }
        Some(entry.owner)
    }

    /// The owner of an OSC 4 reply whose index can't be read: the OSC 4
    /// query written first.
    pub fn pop_palette(&mut self) -> Option<Owner> {
        let kind = self
            .queues
            .iter()
            .filter(|(kind, _)| matches!(kind, ReplyKind::Palette(_)))
            .filter_map(|(kind, queue)| queue.front().map(|entry| (entry.seq, *kind)))
            .min_by_key(|&(seq, _)| seq)?
            .1;
        self.pop(kind)
    }

    /// The probe's consume window ended with no DA1: fac's queries still
    /// queued are dropped. A reply to one that comes after this finds no
    /// `Ours` entry and goes to the shell: fac can no longer tell it from
    /// a reply to the shell. No measured terminal gets here; all of them
    /// answer DA1 well inside the window.
    pub fn expire_ours(&mut self) {
        if self.ours > 0 {
            self.drop_ours(|_| true, "the consume window");
        }
    }

    /// What a reply on stdin is recognised as now: the kinds fac has a
    /// query of queued.
    pub fn expect(&self) -> Expect {
        let mut expect = Expect::default();
        if self.ours == 0 {
            return expect;
        }
        for (kind, queue) in &self.queues {
            if queue.iter().any(|entry| entry.owner == Owner::Ours) {
                expect.add(*kind);
            }
        }
        expect
    }

    fn push(&mut self, kind: ReplyKind, owner: Owner) {
        let entry = Entry {
            owner,
            seq: self.next_seq,
        };
        self.next_seq += 1;
        match self.queue_mut(kind) {
            Some(queue) => queue.push_back(entry),
            None => self.queues.push((kind, VecDeque::from([entry]))),
        }
    }

    fn queue_mut(&mut self, kind: ReplyKind) -> Option<&mut VecDeque<Entry>> {
        self.queues
            .iter_mut()
            .find(|(queued, _)| *queued == kind)
            .map(|(_, queue)| queue)
    }

    fn has_ours(&self, kind: ReplyKind) -> bool {
        self.ours > 0
            && self.queues.iter().any(|(queued, queue)| {
                *queued == kind && queue.iter().any(|entry| entry.owner == Owner::Ours)
            })
    }

    /// Drop fac's entries of the kinds `which` picks, at `when`, and the
    /// shell's of the kinds that had one of fac's.
    fn drop_ours(&mut self, which: impl Fn(ReplyKind) -> bool, when: &str) {
        for (kind, queue) in &mut self.queues {
            if !which(*kind) {
                continue;
            }
            let dropped = queue
                .iter()
                .filter(|entry| entry.owner == Owner::Ours)
                .count();
            if dropped > 0 {
                queue.clear();
                self.ours -= dropped;
                tracing::debug!(?kind, "fac's query not answered by {when}; dropped");
            }
        }
    }
}

/// The reply kinds the input tokenizer recognises, for one read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Expect {
    modes: Vec<u16>,
    background: bool,
    /// One bit per OSC 4 index.
    palette: [u64; 4],
    device_attrs: bool,
}

impl Expect {
    /// Nothing but CPR is recognised.
    #[cfg(test)]
    pub fn none() -> Self {
        Self::default()
    }

    #[cfg(test)]
    pub fn of(kinds: impl IntoIterator<Item = ReplyKind>) -> Self {
        let mut expect = Self::default();
        for kind in kinds {
            expect.add(kind);
        }
        expect
    }

    fn add(&mut self, kind: ReplyKind) {
        match kind {
            ReplyKind::Mode(mode) => {
                if !self.modes.contains(&mode) {
                    self.modes.push(mode);
                }
            }
            ReplyKind::Background => self.background = true,
            ReplyKind::Palette(index) => {
                self.palette[usize::from(index / 64)] |= 1 << (index % 64);
            }
            ReplyKind::DeviceAttrs => self.device_attrs = true,
        }
    }

    pub fn expects(&self, kind: ReplyKind) -> bool {
        match kind {
            ReplyKind::Mode(mode) => self.modes.contains(&mode),
            ReplyKind::Background => self.background,
            ReplyKind::Palette(index) => {
                self.palette[usize::from(index / 64)] & (1 << (index % 64)) != 0
            }
            ReplyKind::DeviceAttrs => self.device_attrs,
        }
    }

    /// Whether any OSC 4 reply is recognised.
    pub fn any_palette(&self) -> bool {
        self.palette.iter().any(|&bits| bits != 0)
    }

    /// Whether any OSC reply is recognised.
    pub fn any_osc(&self) -> bool {
        self.background || self.any_palette()
    }

    /// Whether any reply but CPR is recognised.
    pub fn any(&self) -> bool {
        !self.modes.is_empty() || self.device_attrs || self.any_osc()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::caps::{PROBE, probe_for};
    use crate::term::ident::TermKind;

    use ReplyKind::{Background, DeviceAttrs, Mode, Palette};

    fn probed() -> ReplyQueues {
        let mut queues = ReplyQueues::new();
        queues.push_ours_for(PROBE);
        queues
    }

    #[test]
    fn the_probe_queues_exactly_its_queries() {
        assert_eq!(
            queries(PROBE),
            vec![
                Mode(2026),
                Background,
                Palette(2),
                Palette(3),
                Palette(4),
                Palette(5),
                Palette(6),
                DeviceAttrs,
            ]
        );
        assert_eq!(queries(probe_for(TermKind::Apple))[0], Background);
        assert!(!queries(probe_for(TermKind::Apple)).contains(&Mode(2026)));
        let expect = probed().expect();
        for kind in queries(PROBE) {
            assert!(expect.expects(kind), "{kind:?}");
        }
        for kind in [Mode(2004), Palette(1), Palette(7)] {
            assert!(!expect.expects(kind), "{kind:?}");
        }
    }

    #[test]
    fn replies_pop_in_write_order() {
        let mut queues = ReplyQueues::new();
        queues.push_ours_for(b"\x1b]11;?\x07\x1b[c\x1b]11;?\x07\x1b[c");
        queues.push_shell(Background);
        queues.push_shell(DeviceAttrs);
        queues.push_ours_for(b"\x1b]11;?\x07");
        assert_eq!(queues.pop(Background), Some(Owner::Ours));
        assert_eq!(queues.pop(Background), Some(Owner::Ours));
        assert_eq!(queues.pop(Background), Some(Owner::Shell));
        assert_eq!(queues.pop(Background), Some(Owner::Ours));
        assert_eq!(queues.pop(Background), None);
        assert_eq!(queues.pop(DeviceAttrs), Some(Owner::Ours));
        assert_eq!(queues.pop(DeviceAttrs), Some(Owner::Ours));
        // fac's last DA1 has popped: the shell's is forgotten, and its
        // reply, no longer expected, passes raw.
        assert_eq!(queues.pop(DeviceAttrs), None);
        assert!(!queues.expect().any());
    }

    #[test]
    fn the_shells_entries_go_when_facs_last_of_the_kind_does() {
        // fac's reply pops; the shell's entry after it is forgotten, so
        // fac's next query of the kind finds its own reply first.
        let mut queues = probed();
        queues.push_shell(Background);
        assert_eq!(queues.pop(Background), Some(Owner::Ours));
        assert!(!queues.expect().expects(Background));
        queues.push_ours_for(b"\x1b]11;?\x07");
        assert_eq!(queues.pop(Background), Some(Owner::Ours));
        assert_eq!(queues.pop(Background), None);
        // The same when fac's are dropped, by DA1 or by the window.
        let mut queues = probed();
        queues.push_shell(Palette(3));
        assert_eq!(queues.pop(DeviceAttrs), Some(Owner::Ours));
        queues.push_ours_for(b"\x1b]4;3;?\x07");
        assert_eq!(queues.pop(Palette(3)), Some(Owner::Ours));
        let mut queues = probed();
        queues.push_shell(Background);
        queues.expire_ours();
        queues.push_ours_for(b"\x1b]11;?\x07");
        assert_eq!(queues.pop(Background), Some(Owner::Ours));
        // A kind fac still has one of keeps the shell's.
        let mut queues = ReplyQueues::new();
        queues.push_ours_for(b"\x1b]11;?\x07\x1b]11;?\x07");
        queues.push_shell(Background);
        assert_eq!(queues.pop(Background), Some(Owner::Ours));
        assert_eq!(queues.pop(Background), Some(Owner::Ours));
        assert_eq!(queues.pop(Background), None);
    }

    #[test]
    fn a_shell_reply_between_two_of_ours() {
        let mut queues = ReplyQueues::new();
        queues.push_ours_for(b"\x1b]11;?\x1b\\");
        queues.push_shell(Background);
        queues.push_ours_for(b"\x1b]11;?\x07");
        assert_eq!(queues.pop(Background), Some(Owner::Ours));
        assert!(queues.expect().expects(Background));
        assert_eq!(queues.pop(Background), Some(Owner::Shell));
        assert!(queues.expect().expects(Background));
        assert_eq!(queues.pop(Background), Some(Owner::Ours));
        assert!(!queues.expect().any());
    }

    #[test]
    fn the_da1_fence_drops_facs_other_queries() {
        let mut queues = ReplyQueues::new();
        // A terminal that answers no OSC 4.
        queues.push_ours_for(b"\x1b]4;2;?\x1b\\\x1b]4;3;?\x1b\\\x1b[c");
        queues.push_shell(Palette(3));
        queues.push_shell(DeviceAttrs);
        assert!(queues.expect().expects(Palette(2)));
        assert_eq!(queues.pop(DeviceAttrs), Some(Owner::Ours));
        assert_eq!(queues.expect(), Expect::none());
        // Nothing is expected now, so the shell's replies pass raw: their
        // entries are gone with fac's.
        assert_eq!(queues.pop(Palette(2)), None);
        assert_eq!(queues.pop(Palette(3)), None);
        assert_eq!(queues.pop(DeviceAttrs), None);
    }

    #[test]
    fn expiry_drops_ours() {
        let mut queues = probed();
        queues.push_shell(Background);
        queues.expire_ours();
        assert!(!queues.expect().any());
        // A late reply to fac, or the shell's, is not expected and passes
        // raw; nothing is queued for it.
        assert_eq!(queues.pop(Background), None);
        assert_eq!(queues.pop(DeviceAttrs), None);
        queues.expire_ours();
    }

    #[test]
    fn the_shell_is_recorded_only_while_ours_is_queued() {
        let mut queues = ReplyQueues::new();
        queues.push_shell(Background);
        assert_eq!(queues.pop(Background), None);
        let mut queues = probed();
        // Kinds fac didn't ask.
        queues.push_shell(Mode(2004));
        queues.push_shell(Palette(1));
        assert_eq!(queues.pop(Mode(2004)), None);
        assert_eq!(queues.pop(Palette(1)), None);
        // Once fac's reply came, the shell's next query isn't recorded.
        assert_eq!(queues.pop(Palette(5)), Some(Owner::Ours));
        queues.push_shell(Palette(5));
        assert_eq!(queues.pop(Palette(5)), None);
    }

    #[test]
    fn the_shell_entries_are_capped() {
        let mut queues = probed();
        for _ in 0..MAX_SHELL + 5 {
            queues.push_shell(Background);
        }
        // fac asks again, so the shell's entries before it are popped.
        queues.push_ours_for(b"\x1b]11;?\x07");
        assert_eq!(queues.pop(Background), Some(Owner::Ours));
        for _ in 0..MAX_SHELL {
            assert_eq!(queues.pop(Background), Some(Owner::Shell));
        }
        assert_eq!(queues.pop(Background), Some(Owner::Ours));
        assert_eq!(queues.pop(Background), None);
    }

    #[test]
    fn an_unreadable_palette_reply_pops_the_first_osc4_query() {
        let mut queues = ReplyQueues::new();
        queues.push_ours_for(b"\x1b]4;5;?\x07\x1b]4;2;?\x07");
        queues.push_shell(Palette(2));
        queues.push_ours_for(b"\x1b]4;2;?\x07");
        assert_eq!(queues.pop_palette(), Some(Owner::Ours));
        assert_eq!(queues.pop(Palette(2)), Some(Owner::Ours));
        assert_eq!(queues.pop_palette(), Some(Owner::Shell));
        assert_eq!(queues.pop_palette(), Some(Owner::Ours));
        assert_eq!(queues.pop_palette(), None);
    }

    #[test]
    fn expect_by_kind() {
        let expect = Expect::of([Mode(2026), Palette(0), Palette(255), Palette(64)]);
        assert!(expect.any() && expect.any_osc() && expect.any_palette());
        for kind in [Mode(2026), Palette(0), Palette(64), Palette(255)] {
            assert!(expect.expects(kind), "{kind:?}");
        }
        for kind in [Mode(2027), Palette(1), Palette(63), Background, DeviceAttrs] {
            assert!(!expect.expects(kind), "{kind:?}");
        }
        assert!(!Expect::none().any());
        assert!(Expect::of([DeviceAttrs]).any());
        assert!(!Expect::of([DeviceAttrs]).any_osc());
        assert!(Expect::of([Background]).any_osc());
    }
}
