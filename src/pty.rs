//! The pseudo-terminal the user's shell runs in, and raw mode on the real
//! terminal.

use std::ffi::{OsStr, OsString};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};

use anyhow::{Context, Result, anyhow};
use nix::errno::Errno;
use nix::fcntl::{FcntlArg, FdFlag, OFlag, fcntl};
use nix::pty::{OpenptyResult, openpty};
use nix::sys::termios::{
    InputFlags, LocalFlags, SetArg, SpecialCharacterIndices, Termios, cfmakeraw, tcgetattr,
    tcsetattr,
};
use nix::unistd::Pid;

use crate::shell::install::Launch;

/// The termios of fd 0 as it was before raw mode, for the panic hook.
static SAVED_TERMIOS: OnceLock<libc::termios> = OnceLock::new();
/// Serialises openpty; see `open_pty`.
static OPENPTY: Mutex<()> = Mutex::new(());

/// The user's shell running on the slave side of a PTY.
pub struct ShellProcess {
    pub master: OwnedFd,
    pub pid: Pid,
}

/// Window size of the given terminal fd (TIOCGWINSZ). Fallback 80x24 on error.
pub fn window_size(fd: BorrowedFd<'_>) -> libc::winsize {
    let mut size = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: TIOCGWINSZ writes one `winsize` through the pointer, which points
    // at a live, correctly sized local.
    let res = unsafe { libc::ioctl(fd.as_raw_fd(), libc::TIOCGWINSZ, &mut size) };
    if res == -1 {
        tracing::debug!("TIOCGWINSZ failed ({}), using 80x24", Errno::last());
        return libc::winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
    }
    size
}

/// Set the PTY size (TIOCSWINSZ on the master). The kernel then signals the shell.
pub fn resize(master: BorrowedFd<'_>, size: libc::winsize) -> nix::Result<()> {
    // SAFETY: TIOCSWINSZ only reads one `winsize` through the pointer, which
    // points at a live local.
    let res = unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ, &size) };
    Errno::result(res).map(drop)
}

/// How many bytes of the PTY's input no one has read yet, partial lines
/// included, looked at from the master (`shell::trigger`, "Typeahead").
/// Out of cooked mode the kernel's count (FIONREAD) is of every byte, and
/// the modes are left alone, unless VMIN is above 1 with no VTIME (XNU
/// then counts none while fewer than VMIN bytes wait). In cooked mode its
/// count is of whole lines only, so for an instant the line discipline
/// leaves cooked mode, where it counts every byte, and goes back:
/// - out of cooked mode, XNU puts the line being typed after the whole
///   lines, in order, and echoes nothing;
/// - back in it, XNU types the queue in again (PENDIN) at the next read or
///   count, echoed if ECHO is on: so it goes back with ECHO off first, and
///   a count runs that retype, then the saved modes come back.
///
/// What a reader then gets is what it would have got. The modes change
/// only with `change_modes`, which says that nothing reads or sets them
/// meanwhile: the shell waits for fac's answer however long it takes (its
/// line editor would otherwise save fac's passing modes, or have its own
/// set back under it), and fac's own loop is the only writer to the
/// master, so no key comes in meanwhile. `None` when the kernel would not
/// say, or the count needs modes it may not change; the saved modes are
/// put back all the same.
pub fn input_queued(master: BorrowedFd<'_>, change_modes: bool) -> Option<u64> {
    let mut saved = tcgetattr(master).ok()?;
    let cooked = saved.local_flags.contains(LocalFlags::ICANON);
    let vmin = saved.control_chars[SpecialCharacterIndices::VMIN as usize];
    let vtime = saved.control_chars[SpecialCharacterIndices::VTIME as usize];
    if !cooked && (vmin <= 1 || vtime > 0) {
        return pending_input(master);
    }
    if !change_modes {
        return None;
    }
    // A retype the kernel owes is done below, with no echo; set back with
    // the modes, the flag would echo the queue a second time.
    saved.local_flags.remove(LocalFlags::PENDIN);
    let mut counting = saved.clone();
    counting.local_flags.remove(LocalFlags::ICANON);
    counting.control_chars[SpecialCharacterIndices::VMIN as usize] = 1;
    counting.control_chars[SpecialCharacterIndices::VTIME as usize] = 0;
    let count = set_termios(master, &counting)
        .ok()
        .and_then(|()| pending_input(master));
    if cooked {
        let mut quiet = saved.clone();
        quiet
            .local_flags
            .remove(LocalFlags::ECHO | LocalFlags::ECHONL);
        if set_termios(master, &quiet).is_ok() {
            // The retype, with no echo.
            let _ = pending_input(master);
        }
    }
    if let Err(err) = set_termios(master, &saved) {
        tracing::warn!("putting the PTY's modes back failed: {err}");
        return None;
    }
    count
}

/// Whether the line discipline of the PTY at `master` deletes input only
/// for control characters: its erase, kill, word-erase, reprint,
/// literal-next, discard, signal and flow-control characters are each off
/// or a control character other than Tab, NL, CR and ESC, and it does not
/// drop CR (IGNCR). Those are the bytes `shell::trigger` watches for. With
/// another setting a count of unread bytes says only whether there are
/// any. `false` when the kernel would not say.
pub fn only_controls_delete(master: BorrowedFd<'_>) -> bool {
    let Ok(termios) = tcgetattr(master) else {
        return false;
    };
    if termios.input_flags.contains(InputFlags::IGNCR) {
        return false;
    }
    [
        libc::VERASE,
        libc::VKILL,
        libc::VWERASE,
        libc::VREPRINT,
        libc::VLNEXT,
        libc::VDISCARD,
        libc::VINTR,
        libc::VQUIT,
        libc::VSUSP,
        libc::VDSUSP,
        libc::VSTATUS,
        libc::VSTART,
        libc::VSTOP,
    ]
    .into_iter()
    .all(|index| {
        let byte = termios.control_chars[index];
        byte == libc::_POSIX_VDISABLE
            || byte == 0x7f
            || (byte < 0x20 && !matches!(byte, b'\t' | b'\n' | b'\r' | 0x1b))
    })
}

/// tcsetattr now, again after an interrupt.
fn set_termios(fd: BorrowedFd<'_>, termios: &Termios) -> nix::Result<()> {
    loop {
        match tcsetattr(fd, SetArg::TCSANOW, termios) {
            Err(Errno::EINTR) => {}
            other => return other,
        }
    }
}

/// FIONREAD: the bytes of the terminal's input the kernel counts as ready.
fn pending_input(fd: BorrowedFd<'_>) -> Option<u64> {
    let mut pending: libc::c_int = 0;
    // SAFETY: FIONREAD writes one c_int to the pointer, which points at a
    // live local.
    let res = unsafe { libc::ioctl(fd.as_raw_fd(), libc::FIONREAD, &mut pending) };
    if res == -1 {
        return None;
    }
    u64::try_from(pending).ok()
}

/// The path of the slave side of the PTY whose master is `master`
/// (TIOCPTYGNAME, what openpty itself opened). `ttyname` on the slave can
/// fail with ERANGE right after openpty under load: it looks the device up
/// in /dev, which this does not.
pub fn slave_name(master: BorrowedFd<'_>) -> nix::Result<String> {
    // TIOCPTYGNAME writes a NUL-terminated path of at most 128 bytes.
    let mut buf = [0u8; 128];
    // SAFETY: the ioctl writes at most 128 bytes into the live local buffer.
    let res = unsafe {
        libc::ioctl(
            master.as_raw_fd(),
            libc::TIOCPTYGNAME.into(),
            buf.as_mut_ptr(),
        )
    };
    Errno::result(res)?;
    let name = std::ffi::CStr::from_bytes_until_nul(&buf).map_err(|_| Errno::EINVAL)?;
    Ok(name.to_string_lossy().into_owned())
}

/// A process on the slave side of its own PTY.
pub struct ChildPty {
    pub master: OwnedFd,
    pub pid: Pid,
}

/// openpty with `size`; the master O_NONBLOCK, both ends FD_CLOEXEC.
fn open_pty(size: libc::winsize) -> Result<(OwnedFd, OwnedFd)> {
    let OpenptyResult { master, slave } = {
        // macOS's openpty uses ptsname, which returns a static buffer, so two
        // threads opening PTYs at once can fail or mix up their slaves.
        let _guard = OPENPTY.lock().unwrap_or_else(|err| err.into_inner());
        openpty(&size, None).context("openpty")?
    };
    // Neither end may leak into the child under its own number; the slave
    // reaches it as fds 0-2 via dup2, which clears FD_CLOEXEC on the copies.
    set_fd_flag(master.as_fd(), FdFlag::FD_CLOEXEC).context("FD_CLOEXEC on the master")?;
    set_fd_flag(slave.as_fd(), FdFlag::FD_CLOEXEC).context("FD_CLOEXEC on the slave")?;
    let flags = OFlag::from_bits_truncate(fcntl(&master, FcntlArg::F_GETFL)?);
    fcntl(&master, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK))
        .context("O_NONBLOCK on the master")?;
    Ok((master, slave))
}

/// Spawn `cmd` as session leader with `slave` as its stdio and controlling
/// tty. `inherit_fds`: (fd, target fd number) pairs dup2'd in the child.
fn spawn_on_slave(
    mut cmd: Command,
    slave: OwnedFd,
    inherit_fds: Vec<(OwnedFd, RawFd)>,
    what: &Path,
) -> Result<Pid> {
    cmd.stdin(Stdio::from(slave.try_clone()?))
        .stdout(Stdio::from(slave.try_clone()?))
        .stderr(Stdio::from(slave));
    // Raw numbers are collected here, in the parent, so the child needs no
    // allocation; the owned fds stay open until the spawn is done.
    let inherit: Vec<(RawFd, RawFd)> = inherit_fds
        .iter()
        .map(|(fd, target)| (fd.as_raw_fd(), *target))
        .collect();
    // Above every target, so a copy never lands on one.
    let above = inherit
        .iter()
        .map(|&(_, target)| target + 1)
        .max()
        .unwrap_or(0);
    let mut copies: Vec<RawFd> = vec![-1; inherit.len()];
    // SAFETY: the closure runs in the forked child and only makes
    // async-signal-safe calls (setsid, ioctl, fcntl, dup2) and builds errors
    // with `last_os_error`, which does not allocate; `copies` is written in
    // place. std's docs say that when the closure runs, "aspects such as the
    // stdio file descriptors and working directory have successfully been
    // changed", so fd 0 is already the slave here.
    unsafe {
        cmd.pre_exec(move || {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::ioctl(0, libc::TIOCSCTTY.into(), 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            // Every source is copied out of the way first, so a dup2 onto
            // one target never closes a source still to be copied. The
            // copies close on exec; dup2 clears FD_CLOEXEC on the targets.
            for (copy, &(fd, _)) in copies.iter_mut().zip(&inherit) {
                *copy = libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, above);
                if *copy == -1 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            for (&copy, &(_, target)) in copies.iter().zip(&inherit) {
                if libc::dup2(copy, target) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    // Dropping `Child` neither kills nor waits; the owner reaps with waitpid.
    let child = cmd
        .spawn()
        .with_context(|| format!("spawning {}", what.display()))?;
    // Closes the parent's copies of the slave.
    drop(cmd);
    drop(inherit_fds);
    let pid = i32::try_from(child.id()).map_err(|_| anyhow!("child pid out of range"))?;
    Ok(Pid::from_raw(pid))
}

/// openpty with `size`, spawn `shell` with `launch`'s argv0 and args (zsh:
/// a login shell, `-zsh`) with the current env plus `extra_env`, as session
/// leader with the slave as controlling tty. `extra_env` is given the
/// slave's tty name, which is only known once the PTY is open.
/// `inherit_fds`: (fd, target fd number) pairs dup2'd in the child.
pub fn spawn_shell(
    shell: &Path,
    launch: &Launch,
    extra_env: impl FnOnce(&str) -> Vec<(OsString, OsString)>,
    size: libc::winsize,
    inherit_fds: Vec<(OwnedFd, RawFd)>,
) -> Result<ShellProcess> {
    let (master, slave) = open_pty(size)?;
    let slave_tty = slave_name(master.as_fd()).context("the name of the slave")?;

    let mut cmd = Command::new(shell);
    cmd.arg0(&launch.argv0)
        .args(&launch.args)
        .envs(extra_env(&slave_tty));
    let pid = spawn_on_slave(cmd, slave, inherit_fds, shell)?;
    // $SHELL itself only at debug.
    tracing::info!(pid = pid.as_raw(), tty = %slave_tty, "spawned the shell");
    tracing::debug!("the shell is {}", shell.display());
    Ok(ShellProcess { master, pid })
}

/// openpty with `size` and spawn `program` with `argv0` and `args`, as
/// session leader with the slave as controlling tty, in exactly the
/// environment `env` (nothing is inherited). `inherit_fds`: (fd, target fd
/// number) pairs dup2'd in the child.
pub fn spawn_process(
    program: &Path,
    args: &[OsString],
    argv0: &OsStr,
    env: &[(OsString, OsString)],
    size: libc::winsize,
    inherit_fds: Vec<(OwnedFd, RawFd)>,
) -> Result<ChildPty> {
    let (master, slave) = open_pty(size)?;
    let mut cmd = Command::new(program);
    cmd.arg0(argv0)
        .args(args)
        .env_clear()
        .envs(env.iter().map(|(key, value)| (key, value)));
    let pid = spawn_on_slave(cmd, slave, inherit_fds, program)?;
    tracing::debug!(pid = pid.as_raw(), "spawned {}", program.display());
    Ok(ChildPty { master, pid })
}

fn set_fd_flag(fd: BorrowedFd<'_>, flag: FdFlag) -> nix::Result<()> {
    let flags = FdFlag::from_bits_truncate(fcntl(fd, FcntlArg::F_GETFD)?);
    fcntl(fd, FcntlArg::F_SETFD(flags | flag)).map(drop)
}

/// The termios fd 0 had before raw mode, once raw mode has been enabled.
/// Read by the panic hook; `tcsetattr` with it is async-signal-safe.
pub fn saved_termios() -> Option<&'static libc::termios> {
    SAVED_TERMIOS.get()
}

/// Raw mode on a terminal (fd 0, or one `enable_on` was given). Dropping it
/// restores the saved termios.
pub struct RawModeGuard {
    saved: Termios,
    fd: RawFd,
}

impl RawModeGuard {
    /// cfmakeraw on fd 0, applied with TCSANOW. The termios before it is
    /// kept for the panic hook too ([`saved_termios`]).
    pub fn enable() -> Result<Self> {
        let stdin = std::io::stdin();
        let saved = tcgetattr(stdin.as_fd()).context("tcgetattr on stdin")?;
        let _ = SAVED_TERMIOS.set(libc::termios::from(saved.clone()));
        Self::make_raw(stdin.as_fd(), saved)
    }

    /// cfmakeraw on `fd`, a terminal the caller keeps open for as long as
    /// the guard lives (`fac init`'s `/dev/tty`). Only the guard restores
    /// it, on drop: an unwinding panic drops it too.
    pub fn enable_on(fd: BorrowedFd<'_>) -> Result<Self> {
        let saved = tcgetattr(fd).context("tcgetattr on the terminal")?;
        Self::make_raw(fd, saved)
    }

    fn make_raw(fd: BorrowedFd<'_>, saved: Termios) -> Result<Self> {
        let mut raw = saved.clone();
        cfmakeraw(&mut raw);
        tcsetattr(fd, SetArg::TCSANOW, &raw).context("tcsetattr on the terminal")?;
        Ok(Self {
            saved,
            fd: fd.as_raw_fd(),
        })
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        // SAFETY: the fd outlives the guard (fd 0, or the caller's terminal;
        // see `enable_on`).
        let fd = unsafe { BorrowedFd::borrow_raw(self.fd) };
        if let Err(err) = tcsetattr(fd, SetArg::TCSANOW, &self.saved) {
            if terminal_gone(err) {
                tracing::debug!("restoring termios failed: {err}; the terminal is gone");
            } else {
                tracing::warn!("restoring termios failed: {err}");
            }
        }
    }
}

/// Whether a failed call on the terminal means it is gone (its window
/// closed, the tty revoked), so there is nothing left to restore.
fn terminal_gone(err: Errno) -> bool {
    matches!(
        err,
        Errno::EBADF | Errno::EIO | Errno::ENXIO | Errno::ENOTTY
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_terminal_that_is_gone_is_not_a_warning() {
        for err in [Errno::EBADF, Errno::EIO, Errno::ENXIO, Errno::ENOTTY] {
            assert!(terminal_gone(err), "{err}");
        }
        for err in [Errno::EINTR, Errno::EINVAL, Errno::EPERM, Errno::EAGAIN] {
            assert!(!terminal_gone(err), "{err}");
        }
    }

    const SIZE: libc::winsize = libc::winsize {
        ws_row: 24,
        ws_col: 80,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };

    /// What the master can read within 50 ms (the echo).
    fn drain(master: &OwnedFd) -> Vec<u8> {
        use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
        let mut out = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let mut fds = [PollFd::new(master.as_fd(), PollFlags::POLLIN)];
            if poll(&mut fds, PollTimeout::from(50u16)).unwrap_or(0) == 0 {
                return out;
            }
            match nix::unistd::read(master, &mut buf) {
                Ok(n) if n > 0 => out.extend_from_slice(&buf[..n]),
                _ => return out,
            }
        }
    }

    /// The slave out of cooked mode (as a line editor reads it), and what
    /// it reads.
    fn read_raw(slave: &OwnedFd) -> Vec<u8> {
        let mut termios = tcgetattr(slave).expect("tcgetattr");
        termios.local_flags.remove(LocalFlags::ICANON);
        termios.control_chars[SpecialCharacterIndices::VMIN as usize] = 0;
        termios.control_chars[SpecialCharacterIndices::VTIME as usize] = 1;
        tcsetattr(slave, SetArg::TCSANOW, &termios).expect("tcsetattr");
        let mut buf = [0u8; 4096];
        let n = nix::unistd::read(slave, &mut buf).expect("read");
        buf[..n].to_vec()
    }

    /// The count takes in the line being typed, in cooked mode, and leaves
    /// everything as it was: no second echo, the same modes, and the
    /// reader gets the same bytes (what the line discipline made of them:
    /// an erased character gone, a quoted DEL kept, CR as NL).
    #[test]
    fn input_queued_counts_every_byte_and_changes_nothing() {
        for (typed, count, reads) in [
            (&b"abc"[..], 3, &b"abc"[..]),
            (b"cat\nabc", 7, b"cat\nabc"),
            (b"ls\ncat\n", 7, b"ls\ncat\n"),
            (b"abx\x7fc", 3, b"abc"),
            (b"a\x16\x7fb", 3, b"a\x7fb"),
            (b"cat >o1\x18abc", 11, b"cat >o1\x18abc"),
            (b"cat\rabc", 7, b"cat\nabc"),
        ] {
            let (master, slave) = open_pty(SIZE).expect("open_pty");
            nix::unistd::write(&master, typed).expect("write");
            assert!(!drain(&master).is_empty(), "{typed:?}: echoed once");
            let before = tcgetattr(&slave).expect("tcgetattr");
            assert_eq!(input_queued(master.as_fd(), true), Some(count), "{typed:?}");
            assert_eq!(drain(&master), b"", "{typed:?}: no second echo");
            assert_eq!(tcgetattr(&slave).expect("tcgetattr"), before, "{typed:?}");
            assert_eq!(read_raw(&slave), reads, "{typed:?}");
        }
    }

    /// Out of cooked mode, with VMIN above what waits, the kernel's own
    /// count is 0; this one is not (when it may change the modes), and
    /// VMIN stays. With VMIN 1, as a line editor reads, the modes are not
    /// touched at all, and cooked mode needs leave to change them.
    #[test]
    fn input_queued_counts_below_vmin() {
        let (master, slave) = open_pty(SIZE).expect("open_pty");
        let mut termios = tcgetattr(&slave).expect("tcgetattr");
        termios
            .local_flags
            .remove(LocalFlags::ICANON | LocalFlags::ECHO);
        termios.control_chars[SpecialCharacterIndices::VMIN as usize] = 5;
        tcsetattr(&slave, SetArg::TCSANOW, &termios).expect("tcsetattr");
        nix::unistd::write(&master, b"ab").expect("write");
        assert_eq!(pending_input(master.as_fd()), Some(0));
        assert_eq!(
            input_queued(master.as_fd(), false),
            None,
            "modes it may not change"
        );
        assert_eq!(input_queued(master.as_fd(), true), Some(2));
        assert_eq!(tcgetattr(&slave).expect("tcgetattr"), termios);
        termios.control_chars[SpecialCharacterIndices::VMIN as usize] = 1;
        tcsetattr(&slave, SetArg::TCSANOW, &termios).expect("tcsetattr");
        assert_eq!(input_queued(master.as_fd(), false), Some(2));
        let (master, _slave) = open_pty(SIZE).expect("open_pty");
        nix::unistd::write(&master, b"cat\nab").expect("write");
        assert_eq!(input_queued(master.as_fd(), false), None, "cooked");
        assert_eq!(input_queued(master.as_fd(), true), Some(6));
    }

    /// A new PTY's settings delete only for control characters; an erase
    /// set to a letter, or IGNCR, is another setting.
    #[test]
    fn only_controls_delete_by_default() {
        let (master, slave) = open_pty(SIZE).expect("open_pty");
        assert!(only_controls_delete(master.as_fd()));
        let saved = tcgetattr(&slave).expect("tcgetattr");
        let mut termios = saved.clone();
        termios.control_chars[libc::VERASE] = b'x';
        tcsetattr(&slave, SetArg::TCSANOW, &termios).expect("tcsetattr");
        assert!(!only_controls_delete(master.as_fd()));
        let mut termios = saved.clone();
        termios.control_chars[libc::VKILL] = libc::_POSIX_VDISABLE;
        tcsetattr(&slave, SetArg::TCSANOW, &termios).expect("tcsetattr");
        assert!(
            only_controls_delete(master.as_fd()),
            "a character turned off"
        );
        let mut termios = saved;
        termios.input_flags.insert(InputFlags::IGNCR);
        tcsetattr(&slave, SetArg::TCSANOW, &termios).expect("tcsetattr");
        assert!(!only_controls_delete(master.as_fd()));
    }

    #[test]
    fn slave_name_is_the_slave_tty() {
        let size = libc::winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let (master, slave) = open_pty(size).expect("open_pty");
        let name = slave_name(master.as_fd()).expect("slave_name");
        assert!(name.starts_with("/dev/ttys"), "{name}");
        let tty = nix::unistd::ttyname(&slave).expect("ttyname");
        assert_eq!(Path::new(&name), tty);
    }
}
