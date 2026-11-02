//! A microsecond timer for the poll loop. poll's timeout is whole
//! milliseconds, so a paint due 1.4 ms out would wait 2 ms or more; a
//! kqueue holding one one-shot `EVFILT_TIMER` is readable when the timer
//! fires and goes into the poll set as one more fd.

use std::os::fd::{AsFd, BorrowedFd};
use std::time::{Duration, Instant};

use nix::errno::Errno;
use nix::sys::event::{EvFlags, EventFilter, FilterFlag, KEvent, Kqueue};

/// The timer's kevent ident; the kqueue holds nothing else.
const IDENT: usize = 1;

/// poll's own timeout for a wait of `wait`, rounded up to whole
/// milliseconds so the deadline has passed when poll returns. With the
/// timer armed this is only the fallback.
pub fn poll_millis(wait: Duration) -> u16 {
    u16::try_from(wait.as_micros().div_ceil(1000)).unwrap_or(u16::MAX)
}

/// A kqueue fd that is readable once the deadline it was set to has passed.
pub struct WakeTimer {
    kq: Kqueue,
}

impl WakeTimer {
    pub fn new() -> nix::Result<Self> {
        Ok(Self { kq: Kqueue::new()? })
    }

    /// Fire at `deadline`, or never with `None`. A fired event left from
    /// the last setting is taken first, so the fd is readable only for this
    /// one.
    pub fn set(&self, deadline: Option<Instant>) -> nix::Result<()> {
        let mut fired = [timer_event(EvFlags::empty(), 0)];
        let zero = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        self.kq.kevent(&[], &mut fired, Some(zero))?;
        let change = match deadline {
            Some(deadline) => {
                let wait = deadline.saturating_duration_since(Instant::now());
                // Rounded up, so the deadline has passed when it fires.
                let micros = isize::try_from(wait.as_nanos().div_ceil(1000)).unwrap_or(isize::MAX);
                timer_event(EvFlags::EV_ADD | EvFlags::EV_ONESHOT, micros)
            }
            None => timer_event(EvFlags::EV_DELETE, 0),
        };
        match self.kq.kevent(&[change], &mut [], None) {
            // Nothing to delete: it fired or was never set.
            Err(Errno::ENOENT) if deadline.is_none() => Ok(()),
            result => result.map(drop),
        }
    }
}

impl AsFd for WakeTimer {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.kq.as_fd()
    }
}

/// The timer's kevent with `flags`, `micros` out. `NOTE_CRITICAL` asks the
/// kernel not to coalesce it with other timers.
fn timer_event(flags: EvFlags, micros: isize) -> KEvent {
    KEvent::new(
        IDENT,
        EventFilter::EVFILT_TIMER,
        flags,
        FilterFlag::NOTE_USECONDS | FilterFlag::from_bits_retain(libc::NOTE_CRITICAL),
        micros,
        0,
    )
}
