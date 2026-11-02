//! Runs fac in a headless PTY with a clean environment and keeps a vt100
//! model of what it draws.

#![allow(dead_code)]

pub mod frames;

use std::io::Write;
use std::num::NonZeroU16;
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use nix::errno::Errno;
use nix::fcntl::{FcntlArg, FdFlag, fcntl};
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::pty::{OpenptyResult, openpty};
use nix::sys::signal::{Signal, kill};
use nix::sys::termios::tcgetattr;
use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::Pid;

pub use fac::guards::ShellKind;

static COUNTER: AtomicUsize = AtomicUsize::new(0);
/// macOS's openpty uses ptsname, which returns a static buffer, so two
/// threads opening PTYs at once can fail or mix up their slaves.
static OPENPTY: Mutex<()> = Mutex::new(());

pub const TIMEOUT: Duration = Duration::from_secs(10);

/// At most this many sessions are alive at once in one test binary (cargo
/// runs the tests in parallel threads); `FAC_TEST_SESSIONS` overrides it.
const MAX_SESSIONS: usize = 4;

/// How many sessions are alive, and a wake-up for one waiting to start.
static ALIVE: Mutex<usize> = Mutex::new(0);
static FREED: Condvar = Condvar::new();

fn max_sessions() -> usize {
    static MAX: OnceLock<usize> = OnceLock::new();
    *MAX.get_or_init(|| {
        std::env::var("FAC_TEST_SESSIONS")
            .ok()
            .and_then(|value| value.parse().ok())
            .filter(|&max| max > 0)
            .unwrap_or(MAX_SESSIONS)
    })
}

/// One of the `max_sessions` places, held by a `Session` until it is dropped.
struct Slot;

impl Slot {
    /// Wait for a free place.
    fn acquire() -> Self {
        let max = max_sessions();
        let mut alive = ALIVE.lock().unwrap_or_else(PoisonError::into_inner);
        while *alive >= max {
            alive = FREED.wait(alive).unwrap_or_else(PoisonError::into_inner);
        }
        *alive += 1;
        Slot
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        let mut alive = ALIVE.lock().unwrap_or_else(PoisonError::into_inner);
        *alive -= 1;
        FREED.notify_one();
    }
}

/// How much of the raw output `Session::raw_tail` keeps.
const RAW_TAIL: usize = 256;

pub struct SpawnOpts {
    pub rows: u16,
    pub cols: u16,
    /// The shell fac runs (`$SHELL`): zsh is `/bin/zsh`; bash and fish come
    /// from [`shell_path`]. bash and fish sessions start from their own
    /// fixture rc file (`tests/fixtures/<shell>/`) in the temp `HOME`, with
    /// the XDG dirs under it; `zdotdir`, `zshrc_extra` and `direct` are
    /// zsh's.
    pub shell: ShellKind,
    pub extra_env: Vec<(String, String)>,
    /// `ZDOTDIR` for fac's shell. `None`: no `ZDOTDIR` in the env, and the
    /// fixture files are copied into the temp `HOME` instead.
    pub zdotdir: Option<PathBuf>,
    /// Appended to a copy of the fixture `.zshrc` (in a temp `ZDOTDIR`, or
    /// in the temp `HOME` when `zdotdir` is `None`).
    pub zshrc_extra: Option<String>,
    /// Appended to the copy of a bash or fish session's fixture rc file.
    pub rc_extra: Option<String>,
    /// The shell binary instead of [`shell_path`]'s (a fake shell).
    pub shell_binary: Option<PathBuf>,
    /// Run `/bin/zsh -f -i` with `shell/zsh/fac.zsh` instead of fac. See
    /// `Direct`.
    pub direct: Option<Direct>,
    /// Run fac under `/bin/sh` as the session leader instead of making fac
    /// the leader. macOS revokes a session's controlling tty when its leader
    /// exits, so the slave's termios can only be read after fac exits this
    /// way. sh prints `fac exited <status>` and then waits for a line.
    pub under_sh: bool,
    /// Answer fac's terminal queries (CPR, DECRQM 2026, OSC 10, OSC 11,
    /// OSC 4 1-6, DA1) like a real terminal.
    pub answer_queries: bool,
    /// The terminal the fake one behaves like: its env, DA1, terminators,
    /// reply timing and banned bytes. `SpawnOpts::with_profile` also sets
    /// `sync`, `background` and `palette` from it; on their own they stay
    /// overrides.
    pub profile: Profile,
    /// The DECRQM 2026 answer: supported (Ps 2) or not (Ps 0). Unused when
    /// the profile prints DECRQM instead of answering it.
    pub sync: bool,
    /// The OSC 11 answer.
    pub background: (u8, u8, u8),
    /// The OSC 4 answers for ANSI colours 2–6 (green, yellow, blue, magenta,
    /// cyan). `None`: OSC 4 queries are ignored, as some terminals do.
    pub palette: Option<[(u8, u8, u8); 5]>,
    /// Lines the harness prints into its own vt100 before fac starts, so fac
    /// starts mid-screen.
    pub prefill_lines: u16,
    /// Count fac's output bytes instead of feeding vt100 (for throughput).
    /// Queries are not answered meanwhile. See `Session::set_raw_drain`.
    pub raw_drain: bool,
    /// Override the profile's `TERM_PROGRAM` with `UNKNOWN_TERM_PROGRAM`,
    /// so fac sees a terminal it does not know. Only `SpawnOpts::default()`
    /// sets it; see there.
    pub unknown_terminal: bool,
    /// Also answer the queries fish 4 sends at startup that fac never
    /// sends: kitty's `CSI ? u` (flags 0), XTVERSION (`fac-harness 1.0`)
    /// and XTGETTCAP for `indn`. Off unless a fish test asks.
    pub answer_fish_queries: bool,
    /// A terminal slow to answer at startup (a loaded machine, an ssh hop):
    /// no reply goes out sooner than this after fac's first output (its
    /// probe) arrived, or than the second DA1 query (fac's probe has the
    /// first, so the second is the shell's), whichever comes first. The
    /// replies due sooner all go out then, in order, in one write.
    pub slow_start: Option<Duration>,
    /// Run this as the session's process instead of fac, in the same env:
    /// a shell whose startup file starts fac (`fac init`'s block), or fac
    /// with arguments.
    pub program: Option<Program>,
    /// Called with the session's HOME and ZDOTDIR (when it has one) once
    /// the fixture files are in place, before the process starts: to run
    /// `fac init` in them.
    pub before_start: Option<BeforeStart>,
    /// The folder the session's temp `HOME` goes in; `None`: the system's
    /// temp folder. `fac init` will not write under a temp folder, so its
    /// tests put HOME elsewhere.
    pub home_root: Option<PathBuf>,
}

/// `SpawnOpts::before_start`: called with HOME and ZDOTDIR.
pub type BeforeStart = Box<dyn FnOnce(&Path, Option<&Path>) + Send>;

/// A program a session runs instead of fac.
#[derive(Debug, Clone)]
pub struct Program {
    pub path: PathBuf,
    /// `argv[0]` (`-zsh` for a login shell); `None`: the path.
    pub argv0: Option<String>,
    pub args: Vec<String>,
}

/// A `TERM_PROGRAM` that names no terminal fac knows (`TermKind::Unknown`).
pub const UNKNOWN_TERM_PROGRAM: &str = "fac-harness";

/// The OSC 4 answer for colour 1 (red), in every profile; not measured.
pub const RED: (u8, u8, u8) = (0xe0, 0x6c, 0x75);

/// One Dark's green, yellow, blue, magenta and cyan (the popup mockup's
/// `ansiFor` dark set): the default OSC 4 answers.
pub const ONE_DARK: [(u8, u8, u8); 5] = [
    (0x98, 0xc3, 0x79),
    (0xe5, 0xc0, 0x7b),
    (0x61, 0xaf, 0xef),
    (0xc6, 0x78, 0xdd),
    (0x56, 0xb6, 0xc2),
];

impl Default for SpawnOpts {
    /// `Profile::Ghostty`'s replies, as an unknown terminal. The harness has
    /// always painted in 256 colours, and every existing colour test and
    /// snapshot assumes it; a measured kind (Ghostty included) without
    /// `COLORTERM` now means true colour, so the default names no kind.
    fn default() -> Self {
        Self {
            unknown_terminal: true,
            ..Self::with_profile(Profile::default())
        }
    }
}

impl SpawnOpts {
    /// The defaults, in a terminal that behaves like `profile`.
    pub fn with_profile(profile: Profile) -> Self {
        Self {
            rows: 24,
            cols: 80,
            shell: ShellKind::Zsh,
            extra_env: Vec::new(),
            zdotdir: Some(fixture_zdotdir()),
            zshrc_extra: None,
            rc_extra: None,
            shell_binary: None,
            direct: None,
            under_sh: false,
            answer_queries: true,
            profile,
            sync: profile.sync(),
            background: profile.background(),
            palette: Some(profile.palette()),
            prefill_lines: 0,
            raw_drain: false,
            unknown_terminal: false,
            answer_fish_queries: false,
            slow_start: None,
            program: None,
            before_start: None,
            home_root: None,
        }
    }

    /// Behave like `profile`, with its DECRQM 2026, OSC 11 and OSC 4
    /// answers and its measured env.
    pub fn set_profile(&mut self, profile: Profile) {
        self.profile = profile;
        self.unknown_terminal = false;
        self.sync = profile.sync();
        self.background = profile.background();
        self.palette = Some(profile.palette());
    }
}

/// A real terminal the fake one behaves like, from the step 1b measurements
/// (`docs/plans/phase-3-live/probe/findings.md` and the JSON files next to
/// it, cited below by file name).
///
/// Profiles vary the replies, their timing, the env and the banned bytes
/// only. Rendering, reflow, character widths and BCE are still vt100's, so
/// a profile test passing is not proof that the real terminal looks right.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub enum Profile {
    /// Ghostty (measured in Kero, libghostty 1.3.2-dev): the default.
    #[default]
    Ghostty,
    /// Terminal.app 2.15 (build 466).
    Apple,
    /// iTerm2 3.7.6.
    Iterm2,
    /// VS Code 1.104.3 (xterm.js 5.6.0-beta.118).
    Vscode104,
    /// Zed 1.25.2.
    Zed,
}

/// How a terminal ends its OSC replies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OscEnd {
    /// Always BEL.
    Bel,
    /// Always ST.
    St,
    /// With the query's own terminator.
    Mirror,
}

/// How a terminal takes DECRQM (`CSI ? Ps $ p`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decrqm {
    /// It answers (DECRPM).
    Answer,
    /// It does not answer and draws the final `p` as text.
    Print,
}

/// When a terminal sends its replies: each reply is due `delay` after its
/// query arrives, and not sooner than `gap` after the reply before it.
/// Both depend on the reply's kind. Replies keep their order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Timing {
    pub csi_delay: Duration,
    pub osc_delay: Duration,
    pub csi_gap: Duration,
    pub osc_gap: Duration,
}

impl Timing {
    /// Replies written at once, by the reader, as the harness always did.
    pub fn is_zero(&self) -> bool {
        *self == Self::default()
    }

    fn delay(&self, osc: bool) -> Duration {
        if osc { self.osc_delay } else { self.csi_delay }
    }

    fn gap(&self, osc: bool) -> Duration {
        if osc { self.osc_gap } else { self.csi_gap }
    }
}

/// A byte sequence fac must not send to a terminal, by pattern.
#[derive(Clone, Copy)]
pub struct Banned {
    pub name: &'static str,
    /// Whether a complete CSI sequence (from `ESC [` to its final byte) is
    /// this one.
    pub matches: fn(&[u8]) -> bool,
}

impl std::fmt::Debug for Banned {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name)
    }
}

/// DECRQM for a private mode: `CSI ? Ps $ p`.
fn is_decrqm(seq: &[u8]) -> bool {
    seq.strip_prefix(b"\x1b[?")
        .and_then(|rest| rest.strip_suffix(b"$p"))
        .is_some_and(|params| {
            !params.is_empty() && params.iter().all(|&b| b.is_ascii_digit() || b == b';')
        })
}

const DECRQM: Banned = Banned {
    name: "DECRQM",
    matches: is_decrqm,
};

const fn ms(tenths: u64) -> Duration {
    Duration::from_micros(tenths * 100)
}

impl Profile {
    pub const ALL: [Profile; 5] = [
        Profile::Ghostty,
        Profile::Apple,
        Profile::Iterm2,
        Profile::Vscode104,
        Profile::Zed,
    ];

    /// The env the terminal gives its command, beyond `TERM=xterm-256color`
    /// (all five): each JSON file's `env`.
    pub fn env(self) -> &'static [(&'static str, &'static str)] {
        match self {
            // The measured Kero env (ghostty.json) also sets
            // COLORTERM=truecolor. The harness has always run without it;
            // fac paints Ghostty in true colour either way, by its kind.
            // terminal_profiles.rs runs Ghostty with it as well.
            Profile::Ghostty => &[
                ("TERM_PROGRAM", "ghostty"),
                ("TERM_PROGRAM_VERSION", "1.3.2-dev"),
            ],
            Profile::Apple => &[
                ("TERM_PROGRAM", "Apple_Terminal"),
                ("TERM_PROGRAM_VERSION", "466"),
                ("COLORTERM", "truecolor"),
            ],
            Profile::Iterm2 => &[
                ("TERM_PROGRAM", "iTerm.app"),
                ("TERM_PROGRAM_VERSION", "3.7.6"),
                ("COLORTERM", "truecolor"),
                ("LC_TERMINAL", "iTerm2"),
            ],
            Profile::Vscode104 => &[
                ("TERM_PROGRAM", "vscode"),
                ("TERM_PROGRAM_VERSION", "1.104.3"),
                ("COLORTERM", "truecolor"),
            ],
            Profile::Zed => &[
                ("TERM_PROGRAM", "zed"),
                (
                    "TERM_PROGRAM_VERSION",
                    "1.25.2+stable.379.d25fa01ccd788d812f236810b71af4feff6b2e38",
                ),
                ("COLORTERM", "truecolor"),
                ("ZED_TERM", "true"),
            ],
        }
    }

    /// Whether fac paints in true colour here, by fac's own rule: from
    /// `COLORTERM` and the terminal kind its env names.
    pub fn truecolor(self) -> bool {
        use fac::popup::theme::{ColorDepth, Theme};
        let var = |name: &str| {
            self.env()
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| *value)
        };
        let kind =
            fac::term::ident::detect(var("TERM_PROGRAM"), var("LC_TERMINAL"), var("ZED_TERM"));
        Theme::depth_from_env(var("COLORTERM"), kind) == ColorDepth::TrueColor
    }

    /// The DA1 reply: findings.md's DA1 row.
    pub fn da1(self) -> &'static [u8] {
        match self {
            Profile::Ghostty => b"\x1b[?62;22c",
            Profile::Apple | Profile::Vscode104 => b"\x1b[?1;2c",
            Profile::Iterm2 => b"\x1b[?64;1;2;4;6;17;18;21;22;52c",
            Profile::Zed => b"\x1b[?6c",
        }
    }

    /// DECRQM: findings.md's DECRQM rows (Terminal.app prints `p` for every
    /// mode and answers none).
    pub fn decrqm(self) -> Decrqm {
        match self {
            Profile::Apple => Decrqm::Print,
            _ => Decrqm::Answer,
        }
    }

    /// The DECRQM 2026 answer: Ps 2 (iTerm2, Zed, Ghostty) or Ps 0 (VS Code),
    /// from findings.md's DECRQM 2026 row. Terminal.app: none (`false`).
    pub fn sync(self) -> bool {
        matches!(self, Profile::Ghostty | Profile::Iterm2 | Profile::Zed)
    }

    /// The terminator of OSC 10, 11 and 4 replies: findings.md's OSC rows
    /// (and each JSON's OSC*-BEL replies: Zed and Ghostty mirror).
    pub fn osc_end(self) -> OscEnd {
        match self {
            Profile::Apple => OscEnd::Bel,
            Profile::Iterm2 | Profile::Vscode104 => OscEnd::St,
            Profile::Zed | Profile::Ghostty => OscEnd::Mirror,
        }
    }

    /// The OSC 11 answer: fac's `caps … bg=` in findings.md's startup table.
    pub fn background(self) -> (u8, u8, u8) {
        match self {
            // The harness's historical background, not Kero's (35, 35, 40).
            Profile::Ghostty => (0x1e, 0x1e, 0x1e),
            Profile::Apple => (255, 255, 255),
            Profile::Iterm2 => (17, 19, 32),
            Profile::Vscode104 => (24, 24, 24),
            Profile::Zed => (1, 4, 8),
        }
    }

    /// The OSC 4 answers for colours 2–6: fac's `caps ansi` in findings.md's
    /// startup table.
    pub fn palette(self) -> [(u8, u8, u8); 5] {
        match self {
            // The harness's historical colours, not Kero's.
            Profile::Ghostty => ONE_DARK,
            Profile::Apple => [
                (0x00, 0xa6, 0x00),
                (0x99, 0x99, 0x00),
                (0x00, 0x00, 0xb3),
                (0xb3, 0x00, 0xb3),
                (0x00, 0xa6, 0xb3),
            ],
            Profile::Iterm2 => [
                (0xab, 0xe0, 0x47),
                (0xf7, 0xc7, 0x42),
                (0x44, 0x9f, 0xf3),
                (0x7b, 0x5d, 0xb0),
                (0x5f, 0xda, 0xec),
            ],
            Profile::Vscode104 => [
                (0x0d, 0xbc, 0x79),
                (0xe5, 0xe5, 0x10),
                (0x24, 0x72, 0xc8),
                (0xbc, 0x3f, 0xbc),
                (0x11, 0xa8, 0xcd),
            ],
            Profile::Zed => [
                (0x3e, 0xb9, 0x4f),
                (0xd2, 0x99, 0x21),
                (0x57, 0xa5, 0xff),
                (0xbe, 0x8f, 0xff),
                (0x39, 0xc4, 0xcf),
            ],
        }
    }

    /// The OSC 10 answer: each JSON's OSC10 reply (iTerm2's
    /// `ff5c/fc15/f76f` to the nearest 8 bits).
    pub fn foreground(self) -> (u8, u8, u8) {
        match self {
            Profile::Ghostty => (0xe6, 0xe6, 0xea),
            Profile::Apple => (0, 0, 0),
            Profile::Iterm2 => (0xfe, 0xfb, 0xf6),
            Profile::Vscode104 => (0xcc, 0xcc, 0xcc),
            Profile::Zed => (0xf0, 0xf6, 0xfc),
        }
    }

    /// Fitted to each JSON's FAC-PROBE row (ms from fac's `PROBE` to each
    /// reply).
    pub fn timing(self) -> Timing {
        match self {
            // Kero replies inline (0.3 ms, the last three at 5.2 ms); zero
            // keeps the harness's historical replies, written at once.
            Profile::Ghostty => Timing::default(),
            // 0.6, 0.6, 0.6, then 2.2 for the rest: modelled 0.6 + 0.23 per
            // reply, DA1 at 2.2.
            Profile::Apple => Timing {
                csi_delay: ms(6),
                osc_delay: ms(6),
                csi_gap: Duration::from_micros(230),
                osc_gap: Duration::from_micros(230),
            },
            // DECRQM and OSC 11 at 10.0, the OSC 4s at 24.4, CPR 24.4, DA1
            // 30.2: modelled DECRQM 10, OSC 4 6 at 24.4, CPR 27.3, DA1 30.2.
            Profile::Iterm2 => Timing {
                csi_delay: ms(100),
                osc_delay: ms(100),
                csi_gap: ms(29),
                osc_gap: ms(24),
            },
            // DECRQM 7.8, OSC 11 12.3, OSC 4s 23.8-72.8 (about 12 apart),
            // CPR 78.7, DA1 84.9: modelled DECRQM 7.8, OSC 11 18.6, OSC 4s
            // 10.8 apart to 72.6, CPR 78.6, DA1 84.6.
            Profile::Vscode104 => Timing {
                csi_delay: ms(78),
                osc_delay: ms(123),
                csi_gap: ms(60),
                osc_gap: ms(108),
            },
            // All nine at 3.9.
            Profile::Zed => Timing {
                csi_delay: ms(39),
                osc_delay: ms(39),
                csi_gap: Duration::ZERO,
                osc_gap: Duration::ZERO,
            },
        }
    }

    /// What fac must not send here. Terminal.app prints DECRQM's `p`
    /// (findings.md, "Junk on screen").
    pub fn banned(self) -> &'static [Banned] {
        match self {
            Profile::Apple => &[DECRQM],
            _ => &[],
        }
    }
}

/// A zsh without fac: `/bin/zsh -f -i` with a pipe at fd 63 and
/// `FAC_CMD_FD=63`, which sources a setup file (`PS1='%% '`, then
/// `shell/zsh/fac.zsh`) as its first command, so fac.zsh's init runs at the
/// second prompt. Its raw output still holds every 779x message.
#[derive(Default)]
pub struct Direct {
    /// `FAC_ACTIVE`; `None` is `1:<the slave's tty>`.
    pub fac_active: Option<String>,
}

pub fn fixture_zdotdir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/zdotdir")
}

/// The bash the tests run: `FAC_TEST_BASH`, else Homebrew's.
const DEFAULT_BASH: &str = "/opt/homebrew/bin/bash";
/// The fish the tests run: `FAC_TEST_FISH`, else the one
/// `tools/test-shells.sh` unpacks, relative to the crate.
const DEFAULT_FISH: &str = "target/test-shells/fish/base/usr/local/bin/fish";

/// The binary of a `kind` shell under test. A missing bash or fish fails
/// the test: it is never skipped.
pub fn shell_path(kind: ShellKind) -> PathBuf {
    let (var, default) = match kind {
        ShellKind::Zsh => return PathBuf::from("/bin/zsh"),
        ShellKind::Bash => ("FAC_TEST_BASH", PathBuf::from(DEFAULT_BASH)),
        ShellKind::Fish => (
            "FAC_TEST_FISH",
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(DEFAULT_FISH),
        ),
    };
    let path = std::env::var_os(var)
        .filter(|path| !path.is_empty())
        .map_or(default, PathBuf::from);
    assert!(
        path.is_file(),
        "no {} at {} ({var}): run tools/test-shells.sh",
        kind.name(),
        path.display()
    );
    path
}

/// The fixture rc file of a bash or fish session, and where it goes under
/// `HOME`.
fn shell_fixture(kind: ShellKind) -> (PathBuf, &'static str) {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    match kind {
        ShellKind::Zsh => (fixture_zdotdir().join(".zshrc"), ".zshrc"),
        ShellKind::Bash => (dir.join("bash/.bash_profile"), ".bash_profile"),
        ShellKind::Fish => (dir.join("fish/config.fish"), ".config/fish/config.fish"),
    }
}

/// Copy the regular files of `from` (not recursive) into `to`.
fn copy_files(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap_or_else(|err| panic!("creating {}: {err}", to.display()));
    for entry in std::fs::read_dir(from).expect("read_dir") {
        let entry = entry.expect("dir entry");
        if entry.file_type().expect("file type").is_file() {
            std::fs::copy(entry.path(), to.join(entry.file_name())).expect("copy fixture");
        }
    }
}

fn append(path: &Path, text: &str) {
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap_or_else(|err| panic!("opening {}: {err}", path.display()));
    writeln!(file, "\n{text}").expect("appending");
}

pub struct Session {
    master: OwnedFd,
    /// Held open so the PTY outlives fac and its termios can be read after exit.
    slave: OwnedFd,
    pid: Pid,
    exit_status: Mutex<Option<i32>>,
    parser: Arc<Mutex<vt100::Parser>>,
    last_output: Arc<Mutex<Instant>>,
    stop: Arc<AtomicBool>,
    raw_drain: Arc<AtomicBool>,
    raw: Arc<Mutex<RawCount>>,
    /// Serialises writes to the master between `send` and query answers.
    write_lock: Arc<Mutex<()>>,
    reader: Option<JoinHandle<()>>,
    /// Writes the replies a profile with a reply timing delays.
    writer: Option<(Arc<ReplyQueue>, JoinHandle<()>)>,
    /// The banned sequences fac wrote, in order.
    banned: Arc<Mutex<Vec<Vec<u8>>>>,
    home: PathBuf,
    log_path: PathBuf,
    initial_termios: libc::termios,
    /// The write end of the direct session's command pipe.
    cmd_writer: Mutex<Option<OwnedFd>>,
    /// When the session got its place and started spawning.
    started: Instant,
    /// The terminal answers DECRQM 2026 with "supported".
    sync_output: bool,
    /// Dropped last, after `Drop` has taken fac down.
    _slot: Slot,
}

fn size(rows: u16, cols: u16) -> libc::winsize {
    libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    }
}

fn non_zero(value: u16) -> NonZeroU16 {
    NonZeroU16::new(value).expect("terminal size is not zero")
}

fn set_cloexec(fd: &OwnedFd) {
    let flags = FdFlag::from_bits_truncate(fcntl(fd, FcntlArg::F_GETFD).expect("F_GETFD"));
    fcntl(fd, FcntlArg::F_SETFD(flags | FdFlag::FD_CLOEXEC)).expect("F_SETFD");
}

impl Session {
    pub fn spawn(opts: SpawnOpts) -> Session {
        let slot = Slot::acquire();
        let started = Instant::now();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let home = opts
            .home_root
            .clone()
            .unwrap_or_else(std::env::temp_dir)
            .join(format!("fac-test-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home)
            .unwrap_or_else(|err| panic!("creating {}: {err}", home.display()));
        let log_path = home.join("fac.log");

        let OpenptyResult { master, slave } = {
            let _guard = OPENPTY.lock().unwrap_or_else(|err| err.into_inner());
            openpty(&size(opts.rows, opts.cols), None).expect("openpty")
        };
        // Tests run in parallel; another test's child must not inherit these.
        set_cloexec(&master);
        set_cloexec(&slave);
        let initial_termios = libc::termios::from(tcgetattr(&slave).expect("tcgetattr"));
        // Not ttyname: it can fail with ERANGE right after openpty under load.
        let slave_tty = fac::pty::slave_name(master.as_fd()).expect("the slave's name");

        let shell = opts
            .shell_binary
            .clone()
            .unwrap_or_else(|| shell_path(opts.shell));
        if opts.shell != ShellKind::Zsh {
            assert!(
                opts.direct.is_none() && opts.zshrc_extra.is_none(),
                "direct sessions and zshrc_extra are zsh's"
            );
            let (fixture, to) = shell_fixture(opts.shell);
            let to = home.join(to);
            if let Some(dir) = to.parent() {
                std::fs::create_dir_all(dir).expect("creating the rc file's dir");
            }
            std::fs::copy(&fixture, &to).expect("copying the rc file");
            if let Some(extra) = &opts.rc_extra {
                append(&to, extra);
            }
            // fish builds completions from man pages, in the background,
            // unless these are there.
            for dir in [
                ".local/share/fish/generated_completions",
                ".cache/fish/generated_completions",
            ] {
                std::fs::create_dir_all(home.join(dir)).expect("creating the XDG dirs");
            }
        }
        let zdotdir = match (&opts.zdotdir, &opts.zshrc_extra) {
            _ if opts.shell != ShellKind::Zsh => None,
            (Some(dir), None) => Some(dir.clone()),
            (Some(dir), Some(extra)) => {
                let copy = home.join("zdotdir");
                copy_files(dir, &copy);
                append(&copy.join(".zshrc"), extra);
                Some(copy)
            }
            (None, extra) => {
                copy_files(&fixture_zdotdir(), &home);
                if let Some(extra) = extra {
                    append(&home.join(".zshrc"), extra);
                }
                None
            }
        };

        if let Some(before_start) = opts.before_start {
            before_start(&home, zdotdir.as_deref());
        }
        let fac = env!("CARGO_BIN_EXE_fac");
        let mut cmd = if let Some(program) = &opts.program {
            let mut cmd = Command::new(&program.path);
            if let Some(argv0) = &program.argv0 {
                cmd.arg0(argv0);
            }
            cmd.args(&program.args);
            cmd
        } else if opts.direct.is_some() {
            let mut cmd = Command::new("/bin/zsh");
            cmd.args(["-f", "-i"]);
            cmd
        } else if opts.under_sh {
            let mut cmd = Command::new("/bin/sh");
            cmd.args(["-c", r#""$0"; echo "fac exited $?"; read _"#, fac]);
            cmd
        } else {
            Command::new(fac)
        };
        cmd.env_clear()
            .env("HOME", &home)
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("SHELL", &shell)
            .env("LANG", "en_US.UTF-8")
            .env("TERM", "xterm-256color")
            .env("FAC_LOG_FILE", &log_path)
            .env("FAC_LOG", "debug")
            .envs(opts.profile.env().iter().copied());
        if opts.unknown_terminal {
            cmd.env("TERM_PROGRAM", UNKNOWN_TERM_PROGRAM);
        }
        if opts.shell != ShellKind::Zsh {
            cmd.env("XDG_CONFIG_HOME", home.join(".config"))
                .env("XDG_DATA_HOME", home.join(".local/share"))
                .env("XDG_CACHE_HOME", home.join(".cache"));
        }
        cmd.envs(opts.extra_env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::from(slave.try_clone().expect("dup slave")))
            .stdout(Stdio::from(slave.try_clone().expect("dup slave")))
            .stderr(Stdio::from(slave.try_clone().expect("dup slave")));
        if let Some(zdotdir) = &zdotdir {
            cmd.env("ZDOTDIR", zdotdir);
        }
        if let Some(direct) = &opts.direct {
            let setup = home.join("direct-setup.zsh");
            let fac_zsh = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("shell/zsh/fac.zsh");
            std::fs::write(
                &setup,
                format!(
                    "PS1='%% '\nRPROMPT=\nunsetopt prompt_sp beep\nHISTFILE=/dev/null\nsource '{}'\n",
                    fac_zsh.display()
                ),
            )
            .expect("writing the setup file");
            let fac_active = direct
                .fac_active
                .clone()
                .unwrap_or_else(|| format!("1:{slave_tty}"));
            cmd.env("FAC_ACTIVE", fac_active)
                .env("FAC_CMD_FD", "63")
                .env("FAC_TEST_SETUP", &setup);
        }
        // The direct session's command pipe; both ends close on exec, the
        // read end reaches zsh as fd 63 through dup2.
        let (cmd_reader, cmd_writer) = match opts.direct {
            Some(_) => {
                let (reader, writer) = nix::unistd::pipe().expect("pipe");
                set_cloexec(&reader);
                set_cloexec(&writer);
                (Some(reader), Some(writer))
            }
            None => (None, None),
        };
        let reader_fd = cmd_reader.as_ref().map(|fd| fd.as_raw_fd());
        // SAFETY: only async-signal-safe calls in the forked child; fd 0 is
        // already the slave when the closure runs.
        unsafe {
            cmd.pre_exec(move || {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::ioctl(0, libc::TIOCSCTTY.into(), 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                if let Some(fd) = reader_fd
                    && libc::dup2(fd, 63) == -1
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        #[expect(
            clippy::zombie_processes,
            reason = "reaped with waitpid in try_reap and Drop"
        )]
        let child = cmd.spawn().expect("spawning fac");
        let pid = Pid::from_raw(child.id() as i32);
        drop(cmd);
        drop(cmd_reader);

        let mut parser = vt100::Parser::new(non_zero(opts.rows), non_zero(opts.cols), 0);
        for line in 0..opts.prefill_lines {
            parser.process(format!("prefill {line}\r\n").as_bytes());
        }
        let parser = Arc::new(Mutex::new(parser));
        let last_output = Arc::new(Mutex::new(Instant::now()));
        let stop = Arc::new(AtomicBool::new(false));
        let raw_drain = Arc::new(AtomicBool::new(opts.raw_drain));
        let raw = Arc::new(Mutex::new(RawCount::default()));
        let write_lock = Arc::new(Mutex::new(()));
        let banned = Arc::new(Mutex::new(Vec::new()));
        let timing = opts.profile.timing();
        let sync_output =
            opts.answer_queries && opts.sync && opts.profile.decrqm() == Decrqm::Answer;
        let writer = (!timing.is_zero() || opts.slow_start.is_some()).then(|| {
            let queue = Arc::new(ReplyQueue::default());
            let writer = ReplyWriter {
                master: master.try_clone().expect("dup master"),
                write_lock: Arc::clone(&write_lock),
                stop: Arc::clone(&stop),
                queue: Arc::clone(&queue),
            };
            (queue, std::thread::spawn(move || writer.write_loop()))
        });
        let reader = {
            let terminal = FakeTerminal {
                master: master.try_clone().expect("dup master"),
                parser: Arc::clone(&parser),
                last_output: Arc::clone(&last_output),
                stop: Arc::clone(&stop),
                raw_drain: Arc::clone(&raw_drain),
                raw: Arc::clone(&raw),
                write_lock: Arc::clone(&write_lock),
                answers: opts.answer_queries.then(|| Answers {
                    sync: opts.sync,
                    background: opts.background,
                    palette: opts.palette,
                    foreground: opts.profile.foreground(),
                    da1: opts.profile.da1(),
                    osc_end: opts.profile.osc_end(),
                    decrqm: opts.profile.decrqm(),
                    fish: opts.answer_fish_queries,
                }),
                queries: QueryScanner::default(),
                profile: opts.profile,
                csis: CsiScanner::default(),
                banned: Arc::clone(&banned),
                timing,
                queue: writer.as_ref().map(|(queue, _)| Arc::clone(queue)),
                last_due: None,
                slow_start: opts.slow_start,
                burst_until: None,
                da1_queries: 0,
            };
            std::thread::spawn(move || terminal.read_loop())
        };

        let session = Session {
            master,
            slave,
            pid,
            exit_status: Mutex::new(None),
            parser,
            last_output,
            stop,
            raw_drain,
            raw,
            write_lock,
            reader: Some(reader),
            writer,
            banned,
            home,
            log_path,
            initial_termios,
            cmd_writer: Mutex::new(cmd_writer),
            started,
            sync_output,
            _slot: slot,
        };
        if opts.direct.is_some() {
            session.send_line("source \"$FAC_TEST_SETUP\"");
        }
        session
    }

    pub fn send(&self, bytes: &[u8]) {
        let _guard = self.write_lock.lock().expect("write lock");
        write_master(&self.master, bytes);
    }

    pub fn send_line(&self, s: &str) {
        self.send(format!("{s}\r").as_bytes());
    }

    pub fn wait_for(&self, what: &str, pred: impl Fn(&vt100::Screen) -> bool, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        loop {
            if pred(self.parser.lock().expect("parser lock").screen()) {
                return;
            }
            if Instant::now() >= deadline {
                panic!(
                    "timed out after {timeout:?} waiting for {what}\n--- screen ---\n{}\n--- log ---\n{}",
                    self.screen_text(),
                    self.log_text()
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    pub fn wait_text(&self, needle: &str, timeout: Duration) {
        self.wait_for(
            &format!("{needle:?}"),
            |screen| screen.contents().contains(needle),
            timeout,
        );
    }

    pub fn wait_quiet(&self, quiet: Duration, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        loop {
            let last = *self.last_output.lock().expect("last_output lock");
            if last.elapsed() >= quiet {
                return;
            }
            if Instant::now() >= deadline {
                panic!(
                    "output did not go quiet for {quiet:?} within {timeout:?}\n--- screen ---\n{}",
                    self.screen_text()
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Whether the output has been quiet for `quiet` by now.
    pub fn quiet_for(&self, quiet: Duration) -> bool {
        self.last_output.lock().expect("last_output lock").elapsed() >= quiet
    }

    /// The terminal draws `bytes` that fac never sent, as when it draws a
    /// character at another width than fac's model does: the terminal and
    /// fac's model disagree from here on, until fac reconciles.
    pub fn terminal_only(&self, bytes: &[u8]) {
        self.parser.lock().expect("parser lock").process(bytes);
    }

    pub fn screen_text(&self) -> String {
        self.parser.lock().expect("parser lock").screen().contents()
    }

    /// Look at the screen as it is now (cells and attributes too).
    pub fn with_screen<R>(&self, f: impl FnOnce(&vt100::Screen) -> R) -> R {
        f(self.parser.lock().expect("parser lock").screen())
    }

    pub fn resize(&self, rows: u16, cols: u16) {
        let size = size(rows, cols);
        // SAFETY: TIOCSWINSZ reads one `winsize` from a live local.
        let res = unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &size) };
        assert_ne!(res, -1, "TIOCSWINSZ: {}", Errno::last());
        self.parser
            .lock()
            .expect("parser lock")
            .set_size(non_zero(rows), non_zero(cols));
    }

    pub fn wait_exit(&self, timeout: Duration) -> i32 {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.try_reap() {
                return status;
            }
            if Instant::now() >= deadline {
                panic!(
                    "fac did not exit within {timeout:?}\n--- screen ---\n{}\n--- log ---\n{}",
                    self.screen_text(),
                    self.log_text()
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    pub fn slave_termios(&self) -> libc::termios {
        libc::termios::from(tcgetattr(self.slave.as_fd()).expect("tcgetattr"))
    }

    /// The slave's termios as it was before fac started.
    pub fn initial_termios(&self) -> libc::termios {
        self.initial_termios
    }

    pub fn log_text(&self) -> String {
        std::fs::read_to_string(&self.log_path).unwrap_or_default()
    }

    /// When the session started spawning, after waiting for its place.
    pub fn started(&self) -> Instant {
        self.started
    }

    /// Whether fac was told the terminal has synchronized output, so it
    /// wraps each write in a 2026 pair.
    pub fn sync_output(&self) -> bool {
        self.sync_output
    }

    /// The temp dir that is fac's `$HOME`.
    pub fn home(&self) -> &Path {
        &self.home
    }

    /// Switch between counting fac's output (raw) and feeding vt100.
    pub fn set_raw_drain(&self, on: bool) {
        self.raw_drain.store(on, Ordering::Relaxed);
    }

    /// Bytes of output counted while raw draining.
    pub fn raw_count(&self) -> usize {
        self.raw.lock().expect("raw lock").count
    }

    /// The last bytes of output seen while raw draining.
    pub fn raw_tail(&self) -> Vec<u8> {
        self.raw.lock().expect("raw lock").tail.clone()
    }

    /// Every byte of output received (before vt100), except while raw
    /// draining.
    pub fn raw_output(&self) -> Vec<u8> {
        self.raw.lock().expect("raw lock").all.clone()
    }

    /// The banned sequences (for this session's profile) fac wrote so far.
    pub fn banned(&self) -> Vec<Vec<u8>> {
        self.banned.lock().expect("banned lock").clone()
    }

    /// The `report seq=` lines of fac's log.
    pub fn reports(&self) -> Vec<String> {
        self.log_text()
            .lines()
            .filter(|line| line.contains("report seq="))
            .map(str::to_owned)
            .collect()
    }

    /// The pid of what the harness spawned: fac, or zsh in a direct session.
    pub fn pid(&self) -> Pid {
        self.pid
    }

    /// Write to the direct session's command pipe.
    pub fn write_cmd(&self, bytes: &[u8]) {
        let writer = self.cmd_writer.lock().expect("cmd_writer lock");
        let fd = writer.as_ref().expect("a direct session with an open pipe");
        write_master(fd, bytes);
    }

    /// Close the direct session's command pipe.
    pub fn close_cmd(&self) {
        self.cmd_writer.lock().expect("cmd_writer lock").take();
    }

    /// The terminal goes away, as when its window closes: the fake terminal
    /// stops and the master is closed. The kernel then hangs up the slave
    /// and SIGHUPs fac, its session leader. Nothing is read or sent after
    /// this.
    pub fn hang_up(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        if let Some((queue, writer)) = self.writer.take() {
            queue.ready.notify_all();
            let _ = writer.join();
        }
        // The threads held the only other copies of the master.
        let null = std::fs::File::open("/dev/null").expect("/dev/null");
        drop(std::mem::replace(&mut self.master, OwnedFd::from(null)));
    }

    /// The exit status once fac has exited, reaping it on first sight.
    fn try_reap(&self) -> Option<i32> {
        let mut status = self.exit_status.lock().expect("exit_status lock");
        if status.is_none() {
            *status = match waitpid(self.pid, Some(WaitPidFlag::WNOHANG)) {
                Ok(WaitStatus::Exited(_, code)) => Some(code),
                Ok(WaitStatus::Signaled(_, signal, _)) => Some(128 + signal as i32),
                Ok(_) => None,
                Err(err) => panic!("waitpid on fac: {err}"),
            };
        }
        *status
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if self.try_reap().is_none() {
            let _ = kill(self.pid, Signal::SIGHUP);
            let deadline = Instant::now() + Duration::from_secs(1);
            while self.try_reap().is_none() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            if self.try_reap().is_none() {
                let _ = kill(self.pid, Signal::SIGKILL);
                let _ = waitpid(self.pid, None);
            }
        }
        self.stop.store(true, Ordering::Relaxed);
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        if let Some((queue, writer)) = self.writer.take() {
            queue.ready.notify_all();
            let _ = writer.join();
        }
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn write_master(master: &OwnedFd, bytes: &[u8]) {
    let mut rest = bytes;
    while !rest.is_empty() {
        match nix::unistd::write(master, rest) {
            Ok(n) => rest = &rest[n..],
            Err(Errno::EINTR | Errno::EAGAIN) => {}
            Err(err) => panic!("writing to the PTY: {err}"),
        }
    }
}

#[derive(Default)]
struct RawCount {
    count: usize,
    tail: Vec<u8>,
    all: Vec<u8>,
}

/// How the fake terminal answers.
struct Answers {
    sync: bool,
    background: (u8, u8, u8),
    palette: Option<[(u8, u8, u8); 5]>,
    foreground: (u8, u8, u8),
    da1: &'static [u8],
    osc_end: OscEnd,
    decrqm: Decrqm,
    /// `SpawnOpts::answer_fish_queries`.
    fish: bool,
}

/// An OSC query's terminator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum End {
    Bel,
    St,
}

/// A query fac wrote to the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Query {
    CursorPos,
    SyncMode,
    Foreground(End),
    Background(End),
    /// OSC 4 for this ANSI colour index.
    Palette(u8, End),
    DeviceAttrs,
    /// fish's: kitty's `CSI ? u`, XTVERSION, XTGETTCAP `indn`.
    KittyFlags,
    XtVersion,
    TermcapIndn,
}

impl Query {
    /// Whether the reply is an OSC (else a CSI).
    fn osc(self) -> bool {
        matches!(
            self,
            Query::Foreground(_) | Query::Background(_) | Query::Palette(..)
        )
    }
}

const QUERIES: &[(&[u8], Query)] = &[
    (b"\x1b[6n", Query::CursorPos),
    (b"\x1b[?2026$p", Query::SyncMode),
    (b"\x1b]10;?\x1b\\", Query::Foreground(End::St)),
    (b"\x1b]10;?\x07", Query::Foreground(End::Bel)),
    (b"\x1b]11;?\x1b\\", Query::Background(End::St)),
    (b"\x1b]11;?\x07", Query::Background(End::Bel)),
    (b"\x1b]4;1;?\x1b\\", Query::Palette(1, End::St)),
    (b"\x1b]4;1;?\x07", Query::Palette(1, End::Bel)),
    (b"\x1b]4;2;?\x1b\\", Query::Palette(2, End::St)),
    (b"\x1b]4;3;?\x1b\\", Query::Palette(3, End::St)),
    (b"\x1b]4;4;?\x1b\\", Query::Palette(4, End::St)),
    (b"\x1b]4;5;?\x1b\\", Query::Palette(5, End::St)),
    (b"\x1b]4;6;?\x1b\\", Query::Palette(6, End::St)),
    (b"\x1b]4;2;?\x07", Query::Palette(2, End::Bel)),
    (b"\x1b]4;3;?\x07", Query::Palette(3, End::Bel)),
    (b"\x1b]4;4;?\x07", Query::Palette(4, End::Bel)),
    (b"\x1b]4;5;?\x07", Query::Palette(5, End::Bel)),
    (b"\x1b]4;6;?\x07", Query::Palette(6, End::Bel)),
    (b"\x1b[c", Query::DeviceAttrs),
    (b"\x1b[0c", Query::DeviceAttrs),
    (b"\x1b[?u", Query::KittyFlags),
    (b"\x1b[>0q", Query::XtVersion),
    (b"\x1bP+q696e646e\x1b\\", Query::TermcapIndn),
];

/// Finds the queries in fac's output, across reads.
#[derive(Default)]
struct QueryScanner {
    /// The escape sequence read so far, while it is a prefix of a query.
    seq: Vec<u8>,
}

impl QueryScanner {
    /// The first query that ends in `bytes`, as (end offset, query).
    fn next(&mut self, bytes: &[u8]) -> Option<(usize, Query)> {
        let mut i = 0;
        while i < bytes.len() {
            if self.seq.is_empty() {
                i += memchr::memchr(0x1b, &bytes[i..])?;
            }
            let byte = bytes[i];
            i += 1;
            self.seq.push(byte);
            if let Some(&(_, query)) = QUERIES.iter().find(|(seq, _)| *seq == self.seq) {
                self.seq.clear();
                return Some((i, query));
            }
            if !QUERIES.iter().any(|(seq, _)| seq.starts_with(&self.seq)) {
                self.seq.clear();
                if byte == 0x1b {
                    self.seq.push(byte);
                }
            }
        }
        None
    }
}

/// The longest CSI sequence `CsiScanner` keeps; a longer one is dropped.
const CSI_MAX: usize = 64;

/// Finds the complete CSI sequences in fac's output, across reads.
#[derive(Default)]
struct CsiScanner {
    /// `ESC`, or `ESC [` and what follows, while a sequence is open.
    seq: Vec<u8>,
}

impl CsiScanner {
    /// The end offset in `bytes` and the bytes of every CSI sequence that
    /// ends in `bytes`.
    fn ends(&mut self, bytes: &[u8]) -> Vec<(usize, Vec<u8>)> {
        let mut found = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            if self.seq.is_empty() {
                let Some(at) = memchr::memchr(0x1b, &bytes[i..]) else {
                    break;
                };
                i += at;
            }
            let byte = bytes[i];
            i += 1;
            if byte == 0x1b {
                self.seq.clear();
                self.seq.push(byte);
                continue;
            }
            if self.seq.len() == 1 {
                if byte == b'[' {
                    self.seq.push(byte);
                } else {
                    self.seq.clear();
                }
                continue;
            }
            self.seq.push(byte);
            if (0x40..=0x7e).contains(&byte) {
                found.push((i, std::mem::take(&mut self.seq)));
            } else if self.seq.len() > CSI_MAX {
                self.seq.clear();
            }
        }
        found
    }
}

/// Replies waiting for their due time, oldest first.
#[derive(Default)]
struct ReplyQueue {
    replies: Mutex<std::collections::VecDeque<(Instant, Vec<u8>)>>,
    ready: Condvar,
}

/// Writes queued replies when they are due, so the reader keeps feeding
/// vt100 meanwhile.
struct ReplyWriter {
    master: OwnedFd,
    write_lock: Arc<Mutex<()>>,
    stop: Arc<AtomicBool>,
    queue: Arc<ReplyQueue>,
}

impl ReplyWriter {
    fn write_loop(self) {
        let mut replies = self.queue.replies.lock().expect("reply queue lock");
        while !self.stop.load(Ordering::Relaxed) {
            let now = Instant::now();
            let wait = match replies.front() {
                Some((due, _)) if *due <= now => {
                    let Some((_, reply)) = replies.pop_front() else {
                        continue;
                    };
                    drop(replies);
                    {
                        let _guard = self.write_lock.lock().expect("write lock");
                        write_master(&self.master, &reply);
                    }
                    replies = self.queue.replies.lock().expect("reply queue lock");
                    continue;
                }
                Some((due, _)) => *due - now,
                // Wakes to check `stop` too.
                None => Duration::from_millis(50),
            };
            replies = self
                .queue
                .ready
                .wait_timeout(replies, wait)
                .expect("reply queue lock")
                .0;
        }
    }
}

/// What the terminal does at a point in fac's output besides drawing it.
enum Event {
    Query(Query),
    /// A complete CSI sequence.
    Csi(Vec<u8>),
}

/// Reads fac's output into vt100 and answers fac's queries like a terminal.
struct FakeTerminal {
    master: OwnedFd,
    parser: Arc<Mutex<vt100::Parser>>,
    last_output: Arc<Mutex<Instant>>,
    stop: Arc<AtomicBool>,
    raw_drain: Arc<AtomicBool>,
    raw: Arc<Mutex<RawCount>>,
    write_lock: Arc<Mutex<()>>,
    answers: Option<Answers>,
    queries: QueryScanner,
    profile: Profile,
    csis: CsiScanner,
    banned: Arc<Mutex<Vec<Vec<u8>>>>,
    timing: Timing,
    /// Where delayed replies go; `None` with a zero timing.
    queue: Option<Arc<ReplyQueue>>,
    /// When the last reply is due.
    last_due: Option<Instant>,
    slow_start: Option<Duration>,
    /// `slow_start` after fac's first output, or the second DA1 query:
    /// replies due sooner wait for it, and go out together.
    burst_until: Option<Instant>,
    /// DA1 queries seen.
    da1_queries: usize,
}

impl FakeTerminal {
    fn read_loop(mut self) {
        let mut buf = vec![0u8; 64 * 1024];
        while !self.stop.load(Ordering::Relaxed) {
            let mut fds = [PollFd::new(self.master.as_fd(), PollFlags::POLLIN)];
            match poll(&mut fds, PollTimeout::from(50u16)) {
                Ok(0) | Err(Errno::EINTR) => continue,
                Ok(_) => {}
                Err(err) => panic!("poll on the PTY: {err}"),
            }
            match nix::unistd::read(&self.master, &mut buf) {
                Ok(0) | Err(Errno::EIO) => return,
                Ok(n) => {
                    if self.raw_drain.load(Ordering::Relaxed) {
                        let mut raw = self.raw.lock().expect("raw lock");
                        raw.count += n;
                        raw.tail.extend_from_slice(&buf[..n]);
                        let excess = raw.tail.len().saturating_sub(RAW_TAIL);
                        raw.tail.drain(..excess);
                    } else {
                        self.raw
                            .lock()
                            .expect("raw lock")
                            .all
                            .extend_from_slice(&buf[..n]);
                        self.process(&buf[..n]);
                    }
                    *self.last_output.lock().expect("last_output lock") = Instant::now();
                }
                Err(Errno::EINTR | Errno::EAGAIN) => {}
                Err(err) => panic!("reading the PTY: {err}"),
            }
        }
    }

    /// Feed vt100, answering each query at the byte where it ends, and
    /// noting each banned sequence.
    fn process(&mut self, bytes: &[u8]) {
        let now = Instant::now();
        if self.burst_until.is_none() {
            self.burst_until = self.slow_start.map(|delay| now + delay);
        }
        let mut events = Vec::new();
        let mut at = 0;
        while let Some((end, query)) = self.queries.next(&bytes[at..]) {
            at += end;
            events.push((at, Event::Query(query)));
        }
        if self.profile.decrqm() == Decrqm::Print || !self.profile.banned().is_empty() {
            events.extend(
                self.csis
                    .ends(bytes)
                    .into_iter()
                    .map(|(end, seq)| (end, Event::Csi(seq))),
            );
            // Stable: a query's reply comes before what the terminal draws
            // for the same sequence.
            events.sort_by_key(|(end, _)| *end);
        }
        let mut fed = 0;
        for (end, event) in events {
            let da1 = matches!(event, Event::Query(Query::DeviceAttrs));
            let reply = {
                let mut parser = self.parser.lock().expect("parser lock");
                parser.process(&bytes[fed..end]);
                fed = end;
                match event {
                    Event::Query(query) => self
                        .answers
                        .as_ref()
                        .and_then(|answers| answer(answers, query, parser.screen()))
                        .map(|reply| (query.osc(), reply)),
                    Event::Csi(seq) => {
                        if self.profile.decrqm() == Decrqm::Print && is_decrqm(&seq) {
                            parser.process(b"p");
                        }
                        if self.profile.banned().iter().any(|ban| (ban.matches)(&seq)) {
                            self.banned.lock().expect("banned lock").push(seq);
                        }
                        None
                    }
                }
            };
            if let Some((osc, reply)) = reply {
                self.reply(now, osc, reply);
            }
            if da1 {
                self.da1_queries += 1;
                if self.da1_queries == 2 {
                    self.end_burst(now);
                }
            }
        }
        self.parser
            .lock()
            .expect("parser lock")
            .process(&bytes[fed..]);
    }

    /// End `SpawnOpts::slow_start` at `now`: the burst queued for its end
    /// goes now, still in one write.
    fn end_burst(&mut self, now: Instant) {
        let Some(until) = self.burst_until.filter(|&until| until > now) else {
            return;
        };
        self.burst_until = Some(now);
        if self.last_due == Some(until) {
            self.last_due = Some(now);
        }
        if let Some(queue) = &self.queue {
            for (due, _) in queue.replies.lock().expect("reply queue lock").iter_mut() {
                if *due == until {
                    *due = now;
                }
            }
            queue.ready.notify_all();
        }
    }

    /// Send `reply` (to a query that arrived at `now`) at once, or queue
    /// it for when the profile's timing says.
    fn reply(&mut self, now: Instant, osc: bool, reply: Vec<u8>) {
        let Some(queue) = &self.queue else {
            let _guard = self.write_lock.lock().expect("write lock");
            write_master(&self.master, &reply);
            return;
        };
        let mut due = now + self.timing.delay(osc);
        if let Some(last) = self.last_due {
            due = due.max(last + self.timing.gap(osc));
        }
        let burst = self.burst_until.filter(|&until| due <= until);
        if let Some(until) = burst {
            due = until;
        }
        self.last_due = Some(due);
        let mut replies = queue.replies.lock().expect("reply queue lock");
        match replies.back_mut() {
            // Still queued: joins the burst's one write.
            Some((last, bytes)) if burst.is_some() && *last == due => {
                bytes.extend_from_slice(&reply);
            }
            _ => replies.push_back((due, reply)),
        }
        drop(replies);
        queue.ready.notify_all();
    }
}

/// `ESC ] <body> <terminator>`, the terminator from the terminal's rule.
fn osc(answers: &Answers, body: String, query_end: End) -> Vec<u8> {
    let end = match (answers.osc_end, query_end) {
        (OscEnd::Bel, _) | (OscEnd::Mirror, End::Bel) => "\x07",
        (OscEnd::St, _) | (OscEnd::Mirror, End::St) => "\x1b\\",
    };
    format!("\x1b]{body}{end}").into_bytes()
}

/// `rgb:rrrr/gggg/bbbb` with 4-digit hex.
fn rgb((r, g, b): (u8, u8, u8)) -> String {
    format!("rgb:{r:02x}{r:02x}/{g:02x}{g:02x}/{b:02x}{b:02x}")
}

/// The reply to `query`; `None` for a query this terminal ignores.
fn answer(answers: &Answers, query: Query, screen: &vt100::Screen) -> Option<Vec<u8>> {
    Some(match query {
        Query::CursorPos => {
            let (row, col) = screen.cursor_position();
            // A pending wrap reports the last column.
            let col = col.min(screen.size().1.get() - 1);
            format!("\x1b[{};{}R", row + 1, col + 1).into_bytes()
        }
        Query::SyncMode => match answers.decrqm {
            Decrqm::Answer => {
                format!("\x1b[?2026;{}$y", if answers.sync { 2 } else { 0 }).into_bytes()
            }
            Decrqm::Print => return None,
        },
        Query::Foreground(end) => osc(answers, format!("10;{}", rgb(answers.foreground)), end),
        Query::Background(end) => osc(answers, format!("11;{}", rgb(answers.background)), end),
        Query::Palette(index, end) => {
            let colors = answers.palette?;
            let color = match index {
                1 => RED,
                _ => *colors.get(usize::from(index).checked_sub(2)?)?,
            };
            osc(answers, format!("4;{index};{}", rgb(color)), end)
        }
        Query::DeviceAttrs => answers.da1.to_vec(),
        Query::KittyFlags if answers.fish => b"\x1b[?0u".to_vec(),
        Query::XtVersion if answers.fish => b"\x1bP>|fac-harness 1.0\x1b\\".to_vec(),
        // `indn` is `\E[%p1%dS`.
        Query::TermcapIndn if answers.fish => b"\x1bP1+r696e646e=5c455b257031256453\x1b\\".to_vec(),
        Query::KittyFlags | Query::XtVersion | Query::TermcapIndn => return None,
    })
}

/// Lines of the screen, with trailing blanks trimmed.
pub fn lines(screen: &vt100::Screen) -> Vec<String> {
    screen
        .contents()
        .lines()
        .map(|line| line.trim_end().to_owned())
        .collect()
}

pub fn has_line(screen: &vt100::Screen, pred: impl Fn(&str) -> bool) -> bool {
    lines(screen).iter().any(|line| pred(line))
}

/// `^\d+:/dev/ttys\d+$`
pub fn is_fac_active_value(line: &str) -> bool {
    let all_digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    line.split_once(':').is_some_and(|(pid, tty)| {
        all_digits(pid) && tty.strip_prefix("/dev/ttys").is_some_and(all_digits)
    })
}

/// Wait until the cursor sits on a fresh `% ` prompt.
pub fn wait_prompt(s: &Session) {
    s.wait_for(
        "a prompt",
        |screen| {
            let (row, _) = screen.cursor_position();
            screen
                .rows(0, screen.size().1.get())
                .nth(usize::from(row))
                .is_some_and(|line| line.trim_end() == "%")
        },
        TIMEOUT,
    );
}

/// The smoke assertion every e2e test starts with: fac is really active.
/// Returns the shell's `FAC_ACTIVE`.
pub fn assert_active(s: &Session) -> String {
    s.send_line("echo ${FAC_ACTIVE:-none}");
    s.wait_for(
        "a FAC_ACTIVE value or none",
        |screen| has_line(screen, |line| line == "none" || is_fac_active_value(line)),
        TIMEOUT,
    );
    let screen = s.screen_text();
    assert!(
        !screen.lines().any(|line| line.trim_end() == "none"),
        "fac is not active:\n{screen}\n--- log ---\n{}",
        s.log_text()
    );
    let value = screen
        .lines()
        .map(str::trim_end)
        .find(|line| is_fac_active_value(line))
        .expect("FAC_ACTIVE line")
        .to_owned();
    wait_prompt(s);
    value
}

pub fn wait_log(s: &Session, needle: &str) {
    let deadline = Instant::now() + TIMEOUT;
    while !s.log_text().contains(needle) {
        assert!(
            Instant::now() < deadline,
            "log never contained {needle:?}:\n{}",
            s.log_text()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The popup as fac's `popup drawn` log line gives it: the card's rect
/// (every row it keeps, drawn or not), its list rows, the bottom line's
/// rows kept (`bottom`), drawn (`lines`) and decided for the word (`d`),
/// the popout's rect, and the selected item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Card {
    pub row: u16,
    pub col: u16,
    pub rows: u16,
    pub cols: u16,
    pub list_rows: u16,
    pub bottom: u16,
    pub lines: u16,
    pub d: u16,
    /// (row, col, rows, cols).
    pub popout: Option<(u16, u16, u16, u16)>,
    pub selected: Option<usize>,
}

impl Card {
    /// The card's last row (drawn or not).
    pub fn last_row(&self) -> u16 {
        self.row + self.rows - 1
    }

    /// The first row of the bottom line.
    pub fn bottom_row(&self) -> u16 {
        self.row + self.list_rows
    }
}

/// `a,b,c,d` as four numbers.
fn four(text: &str) -> Option<(u16, u16, u16, u16)> {
    let mut parts = text.split(',').map(|part| part.parse::<u16>().ok());
    let rect = (
        parts.next()??,
        parts.next()??,
        parts.next()??,
        parts.next()??,
    );
    parts.next().is_none().then_some(rect)
}

/// A `popup drawn …` log line's fields.
pub fn parse_card(line: &str) -> Option<Card> {
    let field = |key: &str| -> Option<&str> {
        let rest = line.split(&format!(" {key}=")).nth(1)?;
        rest.split(' ').next()
    };
    let number = |key: &str| field(key)?.parse::<u16>().ok();
    let (row, col, rows, cols) = four(field("at")?)?;
    let popout = match field("popout")? {
        "none" => None,
        rect => Some(four(rect)?),
    };
    let selected = match line.split(" selected=").nth(1)?.trim_end() {
        "None" => None,
        some => Some(
            some.strip_prefix("Some(")?
                .strip_suffix(')')?
                .parse()
                .ok()?,
        ),
    };
    Some(Card {
        row,
        col,
        rows,
        cols,
        list_rows: number("rows")?,
        bottom: number("bottom")?,
        lines: number("lines")?,
        d: number("d")?,
        popout,
        selected,
    })
}

/// The popup fac last drew, unless it closed it after (`popup closed`).
pub fn last_card(log: &str) -> Option<Card> {
    let line = log
        .lines()
        .rfind(|line| line.contains("popup drawn ") || line.contains("popup closed ("))?;
    if line.contains("popup closed (") {
        return None;
    }
    let card = parse_card(line);
    assert!(card.is_some(), "unparsable log line {line:?}");
    card
}

impl Session {
    /// The popup on the screen, as fac's log says.
    pub fn card(&self) -> Option<Card> {
        last_card(&self.log_text())
    }

    /// Wait until `pred` holds for fac's popup (from the log) and the
    /// screen, read together.
    pub fn wait_card(
        &self,
        what: &str,
        pred: impl Fn(Option<&Card>, &vt100::Screen) -> bool,
        timeout: Duration,
    ) {
        let deadline = Instant::now() + timeout;
        loop {
            let card = self.card();
            if self.with_screen(|screen| pred(card.as_ref(), screen)) {
                return;
            }
            if Instant::now() >= deadline {
                panic!(
                    "timed out after {timeout:?} waiting for {what}\n--- card ---\n{card:?}\n--- screen ---\n{}\n--- log ---\n{}",
                    self.screen_text(),
                    self.log_text()
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

/// The colours fac paints its popup with in the harness: the default
/// palette on `SpawnOpts::default()`'s background and ANSI colours, in 256
/// colours (the default session is an unknown terminal without
/// `COLORTERM`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Colors {
    pub card: vt100::Color,
    pub band: vt100::Color,
    pub selected: vt100::Color,
    pub matched: vt100::Color,
    pub selected_matched: vt100::Color,
    pub text: vt100::Color,
    pub dim: vt100::Color,
    pub selected_dim: vt100::Color,
}

pub fn colors() -> Colors {
    let defaults = SpawnOpts::default();
    colors_for("default", defaults.background, defaults.palette)
}

/// The roles of palette `theme` on a terminal with this background and
/// these OSC 4 answers (`None`: the terminal ignores OSC 4), as
/// `palette::build` gives them.
pub fn roles_for(
    theme: &str,
    background: (u8, u8, u8),
    palette: Option<[(u8, u8, u8); 5]>,
) -> fac::popup::palette::Roles {
    let [green, yellow, blue, magenta, cyan] = palette.map_or([None; 5], |colors| colors.map(Some));
    let ansi = fac::term::caps::Ansi {
        green,
        yellow,
        blue,
        magenta,
        cyan,
    };
    let (resolved, warning) = fac::popup::palette::resolve(theme);
    assert_eq!(warning, None, "palette {theme:?} is built in");
    resolved.roles(Some(background), &ansi)
}

/// The colours fac paints palette `theme` with in 256 colours.
pub fn colors_for(
    theme: &str,
    background: (u8, u8, u8),
    palette: Option<[(u8, u8, u8); 5]>,
) -> Colors {
    colors_of(&roles_for(theme, background, palette), background)
}

/// The colours fac paints `roles` with in 256 colours on this background.
pub fn colors_of(roles: &fac::popup::palette::Roles, background: (u8, u8, u8)) -> Colors {
    use fac::popup::palette;
    use fac::popup::theme::nearest_256;
    let background = Some(background);
    let index = |rgb| vt100::Color::Idx(nearest_256(rgb));
    Colors {
        card: vt100::Color::Idx(palette::card_index_256(roles, background)),
        band: vt100::Color::Idx(palette::band_index_256(roles, background)),
        selected: index(roles.selected),
        matched: index(roles.matched),
        selected_matched: index(roles.selected_matched),
        text: index(roles.text),
        dim: index(roles.dim),
        selected_dim: index(roles.selected_dim),
    }
}

/// The colours fac paints `roles` with in true colour.
pub fn colors_true(roles: &fac::popup::palette::Roles) -> Colors {
    let rgb = |(r, g, b)| vt100::Color::Rgb(r, g, b);
    Colors {
        card: rgb(roles.card),
        band: rgb(roles.band),
        selected: rgb(roles.selected),
        matched: rgb(roles.matched),
        selected_matched: rgb(roles.selected_matched),
        text: rgb(roles.text),
        dim: rgb(roles.dim),
        selected_dim: rgb(roles.selected_dim),
    }
}

/// The colours fac paints the default palette with under `profile`: in
/// true colour where `Profile::truecolor` says so, else in 256 colours.
pub fn profile_colors(profile: Profile) -> Colors {
    let roles = roles_for("default", profile.background(), Some(profile.palette()));
    if profile.truecolor() {
        colors_true(&roles)
    } else {
        colors_of(&roles, profile.background())
    }
}

impl Colors {
    /// A cell fac's popup painted: one with the card's, the selected row's,
    /// a match's or the bottom line's background.
    pub fn is_popup(&self, cell: &vt100::Cell) -> bool {
        [
            self.card,
            self.band,
            self.selected,
            self.matched,
            self.selected_matched,
        ]
        .contains(&cell.bgcolor())
    }
}

/// How many cells of the screen fac's popup painted.
pub fn popup_cells(screen: &vt100::Screen, colors: &Colors) -> usize {
    let (rows, cols) = (screen.size().0.get(), screen.size().1.get());
    (0..rows)
        .flat_map(|row| (0..cols).map(move |col| (row, col)))
        .filter(|&(row, col)| {
            screen
                .cell(row, col)
                .is_some_and(|cell| colors.is_popup(cell))
        })
        .count()
}

/// The label of list row `index` of `card`: after the padding, the icon
/// and a space, up to the hint (drawn in a dim colour) or the padding.
pub fn label(screen: &vt100::Screen, card: &Card, index: u16, colors: &Colors) -> String {
    let row = card.row + index;
    let end = card.col + card.cols - 1;
    let blank = |col: u16| {
        screen
            .cell(row, col)
            .is_none_or(|cell| cell.contents().trim().is_empty())
    };
    let mut col = card.col + 1;
    while col < end && !blank(col) {
        col += 1;
    }
    while col < end && blank(col) {
        col += 1;
    }
    let mut out = String::new();
    while col < end {
        let Some(cell) = screen.cell(row, col) else {
            break;
        };
        if [colors.dim, colors.selected_dim].contains(&cell.fgcolor()) {
            break;
        }
        out.push_str(if cell.has_contents() {
            cell.contents()
        } else {
            " "
        });
        col += if cell.is_wide() { 2 } else { 1 };
    }
    out.trim_end().to_owned()
}

/// The labels of `card`'s list rows.
pub fn labels(screen: &vt100::Screen, card: &Card, colors: &Colors) -> Vec<String> {
    (0..card.list_rows)
        .map(|index| label(screen, card, index, colors))
        .collect()
}

/// The list row (an index among the card's rows) drawn with the selected
/// background, if any.
pub fn selected_row(screen: &vt100::Screen, card: &Card, colors: &Colors) -> Option<u16> {
    (0..card.list_rows).find(|&index| {
        screen
            .cell(card.row + index, card.col + 1)
            .is_some_and(|cell| cell.bgcolor() == colors.selected)
    })
}
