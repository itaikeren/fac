//! Who is waiting for each cursor position report. Every `CSI 6n` written to
//! the terminal, by fac or by the shell, is queued here in write order; the
//! terminal answers in the same order, so each reply pops its owner.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Queries older than this are taken to be unanswered and dropped.
pub const MAX_AGE: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CprOwner {
    /// The startup probe's query: its reply seeds the model's cursor.
    Startup,
    /// fac's query at a prompt, with the model's cursor at the byte where it was injected.
    Ours { model_cursor_at_query: (u16, u16) },
    /// The shell's own query: the reply is forwarded to it.
    Shell,
}

#[derive(Default)]
pub struct CprQueue {
    entries: VecDeque<(Instant, CprOwner)>,
}

impl CprQueue {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, owner: CprOwner, now: Instant) {
        self.entries.push_back((now, owner));
    }

    /// The owner of a reply that just arrived.
    pub fn pop(&mut self, now: Instant) -> Option<CprOwner> {
        self.expire(now);
        self.entries.pop_front().map(|(_, owner)| owner)
    }

    /// Drop queries older than `MAX_AGE`.
    pub fn expire(&mut self, now: Instant) {
        while let Some(&(at, owner)) = self.entries.front() {
            if now.saturating_duration_since(at) < MAX_AGE {
                break;
            }
            tracing::debug!(?owner, "cpr query unanswered for {MAX_AGE:?}; dropped");
            self.entries.pop_front();
        }
    }

    /// Drop the startup query; true if it was still waiting.
    pub fn remove_startup(&mut self) -> bool {
        let before = self.entries.len();
        self.entries
            .retain(|&(_, owner)| owner != CprOwner::Startup);
        self.entries.len() != before
    }

    /// The model's cursor was just moved by `delta` (rows, cols): the
    /// snapshots of fac's queries still waiting were taken before that, so
    /// they move with it, clamped into a `size` (rows, cols) screen.
    pub fn shift_ours(&mut self, delta: (i32, i32), size: (u16, u16)) {
        if delta == (0, 0) {
            return;
        }
        let shift = |at: u16, by: i32, len: u16| {
            // Clamped into 0..len, so it fits.
            (i32::from(at) + by).clamp(0, i32::from(len.max(1)) - 1) as u16
        };
        for (_, owner) in &mut self.entries {
            if let CprOwner::Ours {
                model_cursor_at_query: (row, col),
            } = owner
            {
                *row = shift(*row, delta.0, size.0);
                *col = shift(*col, delta.1, size.1);
            }
        }
    }

    /// Whether one of fac's own queries (the startup probe's or a prompt's)
    /// is waiting for its reply.
    pub fn has_ours(&self) -> bool {
        self.entries
            .iter()
            .any(|&(_, owner)| owner != CprOwner::Shell)
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OURS: CprOwner = CprOwner::Ours {
        model_cursor_at_query: (3, 4),
    };

    #[test]
    fn replies_pop_owners_in_order() {
        let now = Instant::now();
        let mut queue = CprQueue::new();
        assert_eq!(queue.pop(now), None);
        queue.push(OURS, now);
        queue.push(CprOwner::Shell, now);
        queue.push(OURS, now);
        assert!(queue.has_ours());
        assert_eq!(queue.pop(now), Some(OURS));
        assert_eq!(queue.pop(now), Some(CprOwner::Shell));
        assert_eq!(queue.pop(now), Some(OURS));
        assert!(queue.is_empty());
    }

    #[test]
    fn old_queries_expire() {
        let start = Instant::now();
        let mut queue = CprQueue::new();
        queue.push(OURS, start);
        queue.push(CprOwner::Shell, start + Duration::from_secs(20));
        assert_eq!(queue.pop(start + MAX_AGE), Some(CprOwner::Shell));
        queue.push(OURS, start);
        queue.expire(start + MAX_AGE);
        assert!(queue.is_empty());
    }

    #[test]
    fn shift_moves_only_our_snapshots() {
        let now = Instant::now();
        let mut queue = CprQueue::new();
        queue.push(CprOwner::Shell, now);
        queue.push(OURS, now);
        queue.push(
            CprOwner::Ours {
                model_cursor_at_query: (20, 78),
            },
            now,
        );
        queue.shift_ours((10, 5), (24, 80));
        assert_eq!(queue.pop(now), Some(CprOwner::Shell));
        assert_eq!(
            queue.pop(now),
            Some(CprOwner::Ours {
                model_cursor_at_query: (13, 9)
            })
        );
        // Clamped to the screen.
        assert_eq!(
            queue.pop(now),
            Some(CprOwner::Ours {
                model_cursor_at_query: (23, 79)
            })
        );
        queue.push(OURS, now);
        queue.shift_ours((-10, -10), (24, 80));
        assert_eq!(
            queue.pop(now),
            Some(CprOwner::Ours {
                model_cursor_at_query: (0, 0)
            })
        );
    }

    #[test]
    fn has_ours_ignores_the_shell() {
        let now = Instant::now();
        let mut queue = CprQueue::new();
        queue.push(CprOwner::Shell, now);
        assert!(!queue.has_ours());
        assert!(!queue.is_empty());
        queue.push(CprOwner::Startup, now);
        assert!(queue.has_ours());
        assert!(queue.remove_startup());
        assert!(!queue.remove_startup());
        assert!(!queue.has_ours());
        assert_eq!(queue.pop(now), Some(CprOwner::Shell));
    }
}
