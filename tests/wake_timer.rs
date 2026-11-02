//! How late the poll loop wakes for a deadline 1.4 ms out (a paint after
//! `PAINT_QUIET`). `timer_options` compares the ways to wait and asserts
//! nothing; it and the p99 check are ignored by default:
//!
//! `cargo test --test wake_timer -- --ignored --nocapture`

use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::time::{Duration, Instant};

use fac::wake::{WakeTimer, poll_millis};
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::sys::event::{EvFlags, EventFilter, FilterFlag, KEvent, Kqueue};
use nix::sys::select::{FdSet, select};
use nix::sys::time::TimeVal;

const AHEAD: Duration = Duration::from_micros(1400);

/// The `q`th percentile of `sorted`.
fn percentile(sorted: &[u64], q: usize) -> u64 {
    sorted
        .get((sorted.len().saturating_sub(1)) * q / 100)
        .copied()
        .unwrap_or(0)
}

/// Overshoots in microseconds, sorted.
fn overshoots(tries: usize, mut wait_until: impl FnMut(Instant)) -> Vec<u64> {
    let mut values: Vec<u64> = (0..tries)
        .map(|_| {
            let deadline = Instant::now() + AHEAD;
            wait_until(deadline);
            let late = Instant::now().saturating_duration_since(deadline);
            u64::try_from(late.as_micros()).unwrap_or(u64::MAX)
        })
        .collect();
    values.sort_unstable();
    values
}

fn report(name: &str, values: &[u64]) {
    println!(
        "{name}: n={} p50={} p99={} max={} (us late)",
        values.len(),
        percentile(values, 50),
        percentile(values, 99),
        values.last().copied().unwrap_or(0)
    );
}

/// Two fds that never become ready, standing in for stdin and the master.
fn idle_fds() -> (OwnedFd, OwnedFd, OwnedFd, OwnedFd) {
    let (a, b) = nix::unistd::pipe().expect("pipe");
    let (c, d) = nix::unistd::pipe().expect("pipe");
    (a, b, c, d)
}

/// Poll `idle` (plus `extra`) until `deadline` has passed, the way the
/// proxy loop does: an early wake just polls again.
fn poll_until(idle: &[BorrowedFd<'_>], extra: Option<BorrowedFd<'_>>, deadline: Instant) {
    loop {
        let now = Instant::now();
        if now >= deadline {
            return;
        }
        let mut fds: Vec<PollFd<'_>> = idle
            .iter()
            .chain(extra.iter())
            .map(|fd| PollFd::new(*fd, PollFlags::POLLIN))
            .collect();
        let millis = poll_millis(deadline.saturating_duration_since(now));
        poll(&mut fds, PollTimeout::from(millis)).expect("poll");
    }
}

/// A kqueue holding one one-shot microsecond timer, for option (c).
fn arm_kqueue(kq: &Kqueue, deadline: Instant, critical: bool) {
    let mut drained = [KEvent::new(
        0,
        EventFilter::EVFILT_TIMER,
        EvFlags::empty(),
        FilterFlag::empty(),
        0,
        0,
    )];
    kq.kevent(
        &[],
        &mut drained,
        Some(libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        }),
    )
    .expect("drain");
    let wait = deadline.saturating_duration_since(Instant::now());
    let mut fflags = FilterFlag::NOTE_USECONDS;
    if critical {
        fflags |= FilterFlag::from_bits_retain(libc::NOTE_CRITICAL);
    }
    let micros = isize::try_from(wait.as_nanos().div_ceil(1000)).unwrap_or(isize::MAX);
    let change = KEvent::new(
        1,
        EventFilter::EVFILT_TIMER,
        EvFlags::EV_ADD | EvFlags::EV_ONESHOT,
        fflags,
        micros,
        0,
    );
    kq.kevent(&[change], &mut [], None).expect("arm");
}

#[test]
#[ignore = "a measurement; run with --ignored --nocapture"]
fn timer_options() {
    const TRIES: usize = 1000;
    let (r1, _w1, r2, _w2) = idle_fds();
    let idle = [r1.as_fd(), r2.as_fd()];

    let a = overshoots(TRIES, |deadline| poll_until(&idle, None, deadline));
    report("(a) poll, ms timeout rounded up", &a);

    let b = overshoots(TRIES, |deadline| {
        loop {
            let now = Instant::now();
            if now >= deadline {
                return;
            }
            let mut set = FdSet::new();
            for fd in idle {
                set.insert(fd);
            }
            let micros = deadline
                .saturating_duration_since(now)
                .as_nanos()
                .div_ceil(1000);
            let mut timeout = TimeVal::new(0, i32::try_from(micros).unwrap_or(i32::MAX));
            select(None, &mut set, None, None, &mut timeout).expect("select");
        }
    });
    report("(b) select, us timeval", &b);

    for critical in [false, true] {
        let kq = Kqueue::new().expect("kqueue");
        let c = overshoots(TRIES, |deadline| {
            loop {
                if Instant::now() >= deadline {
                    return;
                }
                arm_kqueue(&kq, deadline, critical);
                poll_until(&idle, Some(kq.as_fd()), deadline);
            }
        });
        let name = if critical {
            "(c) kqueue timer, NOTE_USECONDS | NOTE_CRITICAL"
        } else {
            "(c) kqueue timer, NOTE_USECONDS"
        };
        report(name, &c);
    }

    let timer = WakeTimer::new().expect("wake timer");
    let chosen = overshoots(TRIES, |deadline| wake_until(&timer, &idle, deadline));
    report("fac::wake::WakeTimer", &chosen);
}

/// Poll until `deadline` with `timer` armed for it, as the proxy loop does.
fn wake_until(timer: &WakeTimer, idle: &[BorrowedFd<'_>], deadline: Instant) {
    loop {
        if Instant::now() >= deadline {
            return;
        }
        timer.set(Some(deadline)).expect("arm");
        poll_until(idle, Some(timer.as_fd()), deadline);
    }
}

/// A paint deadline 1.4 ms out is met within 1.0 ms at p99 over 200 tries.
/// Ignored by default: with every core busy it failed one run in three.
#[test]
#[ignore = "timing-sensitive under CPU load; run with --ignored"]
fn paint_deadline_fires_within_1ms() {
    let (r1, _w1, r2, _w2) = idle_fds();
    let idle = [r1.as_fd(), r2.as_fd()];
    let timer = WakeTimer::new().expect("wake timer");
    let values = overshoots(200, |deadline| wake_until(&timer, &idle, deadline));
    report("wake timer", &values);
    let p99 = percentile(&values, 99);
    assert!(p99 <= 1000, "p99 {p99} us late, values {values:?}");
}

/// A cleared timer does not wake poll, even after it fired.
#[test]
fn cleared_timer_stays_quiet() {
    let timer = WakeTimer::new().expect("wake timer");
    // Fired and not taken, then cleared.
    timer
        .set(Some(Instant::now() + Duration::from_micros(200)))
        .expect("arm");
    std::thread::sleep(Duration::from_millis(2));
    timer.set(None).expect("clear");
    let mut fds = [PollFd::new(timer.as_fd(), PollFlags::POLLIN)];
    let ready = poll(&mut fds, PollTimeout::from(5u16)).expect("poll");
    assert_eq!(ready, 0);
}
