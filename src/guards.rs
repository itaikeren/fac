//! Startup guards: when fac must step aside and exec a plain shell, and
//! which shell it runs.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::anyhow;

/// What fac does at startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Run,
    Plain(PlainReason),
}

/// Why fac steps aside and execs a plain shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlainReason {
    NotATty,
    /// stdin is a terminal, but stdout is not.
    StdoutNotATty,
    Disabled,
    Warp,
    Kiro,
    Nested,
    /// A shell fac does not run: not zsh, bash or fish, or a version it
    /// cannot read or does not support. `kind` from the shell's name,
    /// `version` from its `--version`, when it was read.
    Unsupported {
        kind: Option<ShellKind>,
        version: Option<Version>,
        why: Unsupported,
    },
}

/// Why a shell is not run with fac.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unsupported {
    /// Not zsh, bash or fish.
    UnknownShell,
    /// `--version` gave nothing fac could read.
    NoVersion,
    /// Older than the kind's minimum.
    TooOld,
}

/// The shells fac knows, by the shell's file name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShellKind {
    Zsh,
    Bash,
    Fish,
}

impl ShellKind {
    /// The kind of the shell at `shell`, by its file name (`zsh`, `bash`
    /// or `fish`); `None` for any other.
    pub fn of(shell: &Path) -> Option<ShellKind> {
        match shell.file_name()?.to_str()? {
            "zsh" => Some(ShellKind::Zsh),
            "bash" => Some(ShellKind::Bash),
            "fish" => Some(ShellKind::Fish),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            ShellKind::Zsh => "zsh",
            ShellKind::Bash => "bash",
            ShellKind::Fish => "fish",
        }
    }

    /// The oldest version fac runs; `None`: no gate (zsh's `--version` is
    /// never run).
    pub fn minimum(self) -> Option<Version> {
        match self {
            ShellKind::Zsh => None,
            ShellKind::Bash => Some(Version::new(5, 3, 0)),
            ShellKind::Fish => Some(Version::new(4, 3, 0)),
        }
    }

    /// What the shell's own completions are called where the user sees
    /// it.
    pub fn completions_label(self) -> &'static str {
        match self {
            ShellKind::Zsh => "zsh's completions",
            ShellKind::Bash => "bash's completions",
            ShellKind::Fish => "fish's completions",
        }
    }
}

/// A shell's version, `major.minor.patch` (a missing part is 0).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Version {
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    /// The version at the start of `text` (`5.3.9(1)-release`, `4.9.3`,
    /// `4.3`); `None` unless it starts with a number.
    fn parse(text: &str) -> Option<Self> {
        let end = text
            .find(|c: char| !c.is_ascii_digit() && c != '.')
            .unwrap_or(text.len());
        let mut parts = text[..end].split('.').map(|part| part.parse::<u32>());
        let major = parts.next()?.ok()?;
        let mut next = || parts.next().and_then(Result::ok).unwrap_or(0);
        let (minor, patch) = (next(), next());
        Some(Self::new(major, minor, patch))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl fmt::Debug for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// Everything `decide` looks at, read from the process environment by the caller.
pub struct GuardInput<'a> {
    pub stdin_is_tty: bool,
    pub stdout_is_tty: bool,
    /// `ttyname(0)`.
    pub stdin_tty: Option<&'a str>,
    /// The shell from `resolve_shell`.
    pub shell: &'a Path,
    pub fac_disable: Option<&'a str>,
    pub term_program: Option<&'a str>,
    pub q_term: Option<&'a str>,
    pub fac_active: Option<&'a str>,
    /// The version of a bash or fish shell; only asked once every other
    /// guard passed (see [`shell_version`]).
    pub shell_version: &'a dyn Fn(ShellKind) -> Option<Version>,
}

/// The shell when `$SHELL` is unset or names fac itself.
pub const DEFAULT_SHELL: &str = "/bin/zsh";

/// The shell fac runs or execs: `$SHELL`, unless it is unset or names fac
/// itself (as after `chsh -s fac`), which would exec fac again in a loop;
/// then `/bin/zsh`. `current_exe`: fac's own binary. The flag says whether
/// `$SHELL` was set aside for naming fac.
pub fn resolve_shell(shell: Option<&OsStr>, current_exe: Option<&Path>) -> (PathBuf, bool) {
    let Some(shell) = shell.filter(|shell| !shell.is_empty()).map(Path::new) else {
        return (PathBuf::from(DEFAULT_SHELL), false);
    };
    let canonical = |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    let is_fac = shell.file_name().is_some_and(|name| name == "fac")
        || current_exe.is_some_and(|exe| canonical(shell) == canonical(exe));
    if is_fac {
        (PathBuf::from(DEFAULT_SHELL), true)
    } else {
        (shell.to_owned(), false)
    }
}

/// Decide whether fac runs. The first matching reason wins, in the order
/// NotATty, StdoutNotATty, Disabled, Warp, Kiro, Nested, Unsupported.
pub fn decide(input: &GuardInput) -> Decision {
    let reason = if !input.stdin_is_tty {
        PlainReason::NotATty
    } else if !input.stdout_is_tty {
        PlainReason::StdoutNotATty
    } else if input.fac_disable == Some("1") {
        PlainReason::Disabled
    } else if input.term_program == Some("WarpTerminal") {
        PlainReason::Warp
    } else if input.q_term.is_some_and(|value| !value.is_empty()) {
        PlainReason::Kiro
    } else if is_nested(input.fac_active, input.stdin_tty) {
        PlainReason::Nested
    } else {
        return match gate(ShellKind::of(input.shell), input.shell_version) {
            Ok(()) => Decision::Run,
            Err(reason) => Decision::Plain(reason),
        };
    };
    Decision::Plain(reason)
}

/// Whether fac runs a shell of `kind`: zsh always; bash and fish when
/// `version` (asked only for them) reaches the kind's minimum.
pub fn gate(
    kind: Option<ShellKind>,
    version: impl FnOnce(ShellKind) -> Option<Version>,
) -> Result<(), PlainReason> {
    let unsupported = |kind, version, why| PlainReason::Unsupported { kind, version, why };
    let Some(kind) = kind else {
        return Err(unsupported(None, None, Unsupported::UnknownShell));
    };
    let Some(minimum) = kind.minimum() else {
        return Ok(());
    };
    let Some(found) = version(kind) else {
        return Err(unsupported(Some(kind), None, Unsupported::NoVersion));
    };
    if found < minimum {
        return Err(unsupported(Some(kind), Some(found), Unsupported::TooOld));
    }
    Ok(())
}

/// How long `<shell> --version` may take. The first run of a binary new to
/// macOS (just installed or upgraded) waits for its security scan, which
/// can take a second.
const VERSION_TIMEOUT: Duration = Duration::from_secs(2);
/// The version cache, in fac's cache dir.
const VERSION_CACHE: &str = "shell-versions";

/// The version of the `kind` shell at `shell`, from `<shell> --version`.
/// It is read once per binary: the answer is cached in `cache_dir`
/// (`~/Library/Caches/fac`), keyed by the path and the binary's mtime and
/// size, so later starts do not run it. `None` when it cannot be read.
pub fn shell_version(shell: &Path, kind: ShellKind, cache_dir: Option<&Path>) -> Option<Version> {
    shell_version_within(shell, kind, cache_dir, VERSION_TIMEOUT, true)
}

/// [`shell_version`] that only reads the cache: a version not in it is
/// read from `--version` and not written back (`fac doctor`).
pub fn shell_version_cached(
    shell: &Path,
    kind: ShellKind,
    cache_dir: Option<&Path>,
) -> Option<Version> {
    shell_version_within(shell, kind, cache_dir, VERSION_TIMEOUT, false)
}

/// [`shell_version`], with `--version` given `timeout`; the answer goes
/// into the cache when `write` is set.
fn shell_version_within(
    shell: &Path,
    kind: ShellKind,
    cache_dir: Option<&Path>,
    timeout: Duration,
    write: bool,
) -> Option<Version> {
    let meta = std::fs::metadata(shell).ok()?;
    let key = cache_key(shell, &meta);
    let cache = cache_dir.map(|dir| dir.join(VERSION_CACHE));
    let cached = cache
        .as_deref()
        .and_then(|cache| std::fs::read_to_string(cache).ok())
        .unwrap_or_default();
    if let Some(key) = &key
        && let Some(version) = cached.lines().find_map(|line| {
            let (version, rest) = line.split_once('\t')?;
            (rest == key).then(|| Version::parse(version)).flatten()
        })
    {
        return Some(version);
    }
    let version = parse_version(kind, &run_version(shell, timeout)?)?;
    tracing::info!("{} version {version}", kind.name());
    if let (true, Some(cache), Some(key)) = (write, &cache, &key) {
        let mut lines: Vec<&str> = cached
            .lines()
            .filter(|line| {
                line.split_once('\t')
                    .is_some_and(|(_, rest)| !same_path(rest, key))
            })
            .collect();
        let line = format!("{version}\t{key}");
        lines.push(&line);
        if let Err(err) = write_cache(cache, &lines) {
            tracing::debug!("writing the version cache failed: {err}");
        }
    }
    Some(version)
}

/// `<mtime-ns>\t<size>\t<path>`, or `None` for a path that cannot sit on
/// one line.
fn cache_key(shell: &Path, meta: &std::fs::Metadata) -> Option<String> {
    let path = shell.to_str().filter(|path| !path.contains(['\t', '\n']))?;
    let mtime = i128::from(meta.mtime()) * 1_000_000_000 + i128::from(meta.mtime_nsec());
    Some(format!("{mtime}\t{}\t{path}", meta.size()))
}

/// Whether two cache keys are for the same path.
fn same_path(a: &str, b: &str) -> bool {
    let path = |key: &str| key.splitn(3, '\t').nth(2).map(str::to_owned);
    path(a) == path(b)
}

/// Write the cache through a temp file and a rename.
fn write_cache(cache: &Path, lines: &[&str]) -> std::io::Result<()> {
    if let Some(dir) = cache.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let temp = cache.with_extension(format!("tmp-{}", std::process::id()));
    let mut text = lines.join("\n");
    text.push('\n');
    std::fs::write(&temp, text)?;
    std::fs::rename(&temp, cache).inspect_err(|_| {
        let _ = std::fs::remove_file(&temp);
    })
}

/// What `<shell> --version` prints, in the C locale (bash translates
/// "version"), or `None` if it fails or takes over `timeout`.
fn run_version(shell: &Path, timeout: Duration) -> Option<String> {
    let mut child = Command::new(shell)
        .arg("--version")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .inspect_err(|err| tracing::info!("running the shell's --version failed: {err}"))
        .ok()?;
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(2));
            }
            Ok(None) | Err(_) => {
                tracing::info!("the shell's --version did not finish; killed");
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    let mut output = String::new();
    // A few lines: they fit in the pipe, so reading after the exit is safe.
    child.stdout.take()?.read_to_string(&mut output).ok()?;
    status.success().then_some(output)
}

/// The version in a `--version` output: `GNU bash, version 5.3.9(1)-release
/// (...)`, `fish, version 4.9.3`. The first line must name the shell.
pub fn parse_version(kind: ShellKind, output: &str) -> Option<Version> {
    let line = output.lines().next()?.to_ascii_lowercase();
    if !line.contains(kind.name()) {
        return None;
    }
    let at = line.find("version ")? + "version ".len();
    Version::parse(&line[at..])
}

/// `FAC_ACTIVE` is `<pid>:<tty>` and `<tty>` is the terminal on our stdin.
/// A bare pid is not enough: a GUI app started from fac inherits it.
pub fn is_nested(fac_active: Option<&str>, stdin_tty: Option<&str>) -> bool {
    let Some((pid, tty)) = fac_active.and_then(|value| value.split_once(':')) else {
        return false;
    };
    pid.parse::<u32>().is_ok() && !tty.is_empty() && stdin_tty == Some(tty)
}

/// Exec `shell` as a login shell in place of fac. Only returns on exec failure.
pub fn exec_plain_shell(shell: &Path, reason: &PlainReason) -> anyhow::Error {
    if *reason == PlainReason::Kiro {
        let name = shell
            .file_name()
            .map_or_else(|| "the shell".into(), |name| name.to_string_lossy());
        eprintln!(
            "fac: Kiro is active in this terminal (Q_TERM is set); starting {name} without fac."
        );
    }
    exec_login_shell(shell)
}

/// Exec `shell` as a login shell (argv0 = "-" + basename), marked
/// [`SHELL_MARK`]`=plain:<pid>`: exec keeps fac's pid, so the mark names
/// the shell's own `$$`. Only returns on failure.
pub fn exec_login_shell(shell: &Path) -> anyhow::Error {
    let Some(name) = shell.file_name() else {
        return anyhow!("shell path has no file name: {}", shell.display());
    };
    let mut argv0 = OsString::from("-");
    argv0.push(name);
    let err = Command::new(shell)
        .arg0(argv0)
        .env(SHELL_MARK, plain_mark(std::process::id()))
        .exec();
    anyhow!(err).context(format!("exec {}", shell.display()))
}

/// Marks a shell fac started, so that the startup block `fac init` writes
/// does not exec fac again in it (`src/init/mod.rs`, "No loops"):
/// - [`SHELL_MARK_RUN`] in fac's own child, the shell with fac's hook. The
///   hook stops exporting it once startup is done.
/// - `plain:<pid>` ([`plain_mark`]) in a plain shell fac execs in its own
///   place, whose `$$` is then `<pid>`. No hook runs there; in any other
///   process the value names another pid and means nothing.
pub const SHELL_MARK: &str = "FAC_SHELL";
/// [`SHELL_MARK`]'s value in fac's own child shell.
pub const SHELL_MARK_RUN: &str = "run";

/// [`SHELL_MARK`]'s value in a plain shell exec'd by the process `pid`.
pub fn plain_mark(pid: u32) -> String {
    format!("plain:{pid}")
}

/// The variables fac adds to the shell's environment.
pub fn child_env(fac_pid: u32, slave_tty: &str) -> Vec<(OsString, OsString)> {
    vec![
        ("FAC_ACTIVE".into(), format!("{fac_pid}:{slave_tty}").into()),
        // Makes Kiro's shell integration skip `exec kiro-cli-term`. Each
        // hook script unsets it once startup is done, so the shell's
        // children do not inherit it.
        ("PROCESS_LAUNCHED_BY_Q".into(), "1".into()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_version(_: ShellKind) -> Option<Version> {
        None
    }

    fn input() -> GuardInput<'static> {
        GuardInput {
            stdin_is_tty: true,
            stdout_is_tty: true,
            stdin_tty: Some("/dev/ttys004"),
            shell: Path::new("/bin/zsh"),
            fac_disable: None,
            term_program: None,
            q_term: None,
            fac_active: None,
            shell_version: &no_version,
        }
    }

    fn unsupported(
        kind: Option<ShellKind>,
        version: Option<Version>,
        why: Unsupported,
    ) -> PlainReason {
        PlainReason::Unsupported { kind, version, why }
    }

    fn plain(reason: PlainReason) -> Decision {
        Decision::Plain(reason)
    }

    #[test]
    fn runs_by_default() {
        assert_eq!(decide(&input()), Decision::Run);
    }

    #[test]
    fn not_a_tty() {
        let input = GuardInput {
            stdin_is_tty: false,
            stdin_tty: None,
            ..input()
        };
        assert_eq!(decide(&input), plain(PlainReason::NotATty));
    }

    #[test]
    fn stdout_not_a_tty() {
        let input = GuardInput {
            stdout_is_tty: false,
            ..input()
        };
        assert_eq!(decide(&input), plain(PlainReason::StdoutNotATty));
    }

    #[test]
    fn disabled_only_by_one() {
        let on = GuardInput {
            fac_disable: Some("1"),
            ..input()
        };
        assert_eq!(decide(&on), plain(PlainReason::Disabled));
        for value in ["0", "", "yes"] {
            let off = GuardInput {
                fac_disable: Some(value),
                ..input()
            };
            assert_eq!(decide(&off), Decision::Run, "FAC_DISABLE={value}");
        }
    }

    #[test]
    fn warp() {
        let warp = GuardInput {
            term_program: Some("WarpTerminal"),
            ..input()
        };
        assert_eq!(decide(&warp), plain(PlainReason::Warp));
        let ghostty = GuardInput {
            term_program: Some("ghostty"),
            ..input()
        };
        assert_eq!(decide(&ghostty), Decision::Run);
    }

    #[test]
    fn kiro_needs_a_non_empty_q_term() {
        let kiro = GuardInput {
            q_term: Some("1"),
            ..input()
        };
        assert_eq!(decide(&kiro), plain(PlainReason::Kiro));
        let empty = GuardInput {
            q_term: Some(""),
            ..input()
        };
        assert_eq!(decide(&empty), Decision::Run);
    }

    #[test]
    fn nested_on_the_same_tty() {
        let nested = GuardInput {
            fac_active: Some("123:/dev/ttys004"),
            ..input()
        };
        assert_eq!(decide(&nested), plain(PlainReason::Nested));
    }

    #[test]
    fn not_nested_on_another_tty_or_garbage() {
        for value in [
            "123:/dev/ttys999",
            "123",
            "/dev/ttys004",
            ":/dev/ttys004",
            "abc:/dev/ttys004",
            "-1:/dev/ttys004",
            "123:",
            "",
            "garbage",
        ] {
            let input = GuardInput {
                fac_active: Some(value),
                ..input()
            };
            assert_eq!(decide(&input), Decision::Run, "FAC_ACTIVE={value}");
        }
        let no_tty = GuardInput {
            stdin_tty: None,
            fac_active: Some("123:/dev/ttys004"),
            ..input()
        };
        assert_eq!(decide(&no_tty), Decision::Run);
    }

    #[test]
    fn not_zsh() {
        for shell in ["/bin/sh", "/opt/homebrew/bin/nu", "/bin/zsh5", "/"] {
            let input = GuardInput {
                shell: Path::new(shell),
                ..input()
            };
            assert_eq!(
                decide(&input),
                plain(unsupported(None, None, Unsupported::UnknownShell)),
                "SHELL={shell}"
            );
        }
        for (shell, kind) in [
            ("/bin/bash", ShellKind::Bash),
            ("/opt/homebrew/bin/fish", ShellKind::Fish),
        ] {
            let input = GuardInput {
                shell: Path::new(shell),
                ..input()
            };
            assert_eq!(
                decide(&input),
                plain(unsupported(Some(kind), None, Unsupported::NoVersion)),
                "SHELL={shell}"
            );
        }
        let brew_zsh = GuardInput {
            shell: Path::new("/opt/homebrew/bin/zsh"),
            ..input()
        };
        assert_eq!(decide(&brew_zsh), Decision::Run);
    }

    #[test]
    fn shell_kinds_by_name() {
        assert_eq!(ShellKind::of(Path::new("/bin/zsh")), Some(ShellKind::Zsh));
        assert_eq!(
            ShellKind::of(Path::new("/opt/homebrew/bin/bash")),
            Some(ShellKind::Bash)
        );
        assert_eq!(ShellKind::of(Path::new("fish")), Some(ShellKind::Fish));
        for shell in ["/bin/sh", "/bin/zsh-5.9", "/usr/bin/-zsh", "/", ""] {
            assert_eq!(ShellKind::of(Path::new(shell)), None, "{shell}");
        }
        assert_eq!(ShellKind::Zsh.completions_label(), "zsh's completions");
        assert_eq!(ShellKind::Bash.completions_label(), "bash's completions");
        assert_eq!(ShellKind::Fish.completions_label(), "fish's completions");
    }

    #[test]
    fn the_gate() {
        let version = |text: &'static str| move |_: ShellKind| Version::parse(text);
        let asked = std::cell::Cell::new(false);
        let watch = |_: ShellKind| {
            asked.set(true);
            None
        };
        // zsh has no gate: its version is never asked for.
        assert_eq!(gate(Some(ShellKind::Zsh), watch), Ok(()));
        assert!(!asked.get());
        assert_eq!(
            gate(None, watch),
            Err(unsupported(None, None, Unsupported::UnknownShell))
        );
        assert!(!asked.get());
        let bash = Some(ShellKind::Bash);
        let fish = Some(ShellKind::Fish);
        assert_eq!(gate(bash, version("5.3")), Ok(()));
        assert_eq!(gate(bash, version("5.3.9")), Ok(()));
        assert_eq!(gate(bash, version("6.0")), Ok(()));
        assert_eq!(gate(fish, version("4.3.0")), Ok(()));
        assert_eq!(gate(fish, version("4.9.3")), Ok(()));
        for (kind, found) in [
            (bash, "3.2.57"),
            (bash, "5.2.37"),
            (fish, "3.7.1"),
            (fish, "4.2.9"),
        ] {
            assert_eq!(
                gate(kind, version(found)),
                Err(unsupported(
                    kind,
                    Version::parse(found),
                    Unsupported::TooOld
                )),
                "{found}"
            );
        }
        assert_eq!(
            gate(bash, |_| None),
            Err(unsupported(bash, None, Unsupported::NoVersion))
        );
    }

    #[test]
    fn version_outputs() {
        let bash = |text| parse_version(ShellKind::Bash, text);
        let fish = |text| parse_version(ShellKind::Fish, text);
        assert_eq!(
            bash(
                "GNU bash, version 5.3.9(1)-release (aarch64-apple-darwin25.1.0)\nCopyright (C) 2025 Free Software Foundation, Inc.\n"
            ),
            Some(Version::new(5, 3, 9))
        );
        assert_eq!(
            bash("GNU bash, version 3.2.57(1)-release (arm64-apple-darwin25)\n"),
            Some(Version::new(3, 2, 57))
        );
        // A translated "version".
        assert_eq!(
            bash("GNU bash, Version 5.3.9(1)-release (aarch64-apple-darwin25.1.0)"),
            Some(Version::new(5, 3, 9))
        );
        assert_eq!(fish("fish, version 4.9.3\n"), Some(Version::new(4, 9, 3)));
        assert_eq!(fish("fish, version 4.3"), Some(Version::new(4, 3, 0)));
        assert_eq!(
            fish("fish, version 4.4.0-12-gabcdef0"),
            Some(Version::new(4, 4, 0))
        );
        // Another program's output, or none.
        assert_eq!(bash("fish, version 4.9.3"), None);
        assert_eq!(fish("GNU bash, version 5.3.9(1)-release"), None);
        assert_eq!(bash("GNU bash, version unknown"), None);
        assert_eq!(bash("GNU bash"), None);
        assert_eq!(bash(""), None);
        assert_eq!(Version::new(5, 3, 9).to_string(), "5.3.9");
        assert!(Version::new(5, 3, 0) > Version::new(5, 2, 37));
        assert!(Version::new(4, 10, 0) > Version::new(4, 9, 3));
    }

    /// A temp dir, removed on drop.
    struct Temp(PathBuf);

    impl Temp {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "fac-guards-{name}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("mkdir");
            Self(dir)
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A fake `<name>` that prints `version` for `--version` and counts its
    /// runs in `<dir>/runs`.
    fn fake_shell(dir: &Path, name: &str, version: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        let script = format!(
            "#!/bin/sh\necho run >> '{}'\n[ \"$1\" = --version ] && printf '%s\\n' '{version}'\n",
            dir.join("runs").display()
        );
        std::fs::write(&path, script).expect("writing the fake shell");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        path
    }

    const TEST_TIMEOUT: Duration = Duration::from_secs(20);

    fn runs(dir: &Path) -> usize {
        std::fs::read_to_string(dir.join("runs"))
            .map(|runs| runs.lines().count())
            .unwrap_or(0)
    }

    #[test]
    fn the_version_is_cached_per_binary() {
        let temp = Temp::new("cache");
        let bin = temp.0.join("bin");
        let cache = temp.0.join("cache");
        std::fs::create_dir_all(&bin).expect("mkdir");
        let bash = fake_shell(&bin, "bash", "GNU bash, version 5.3.9(1)-release (fake)");
        // A script new to macOS waits for its security scan on its first
        // run, which can take over a second.
        let version = |shell: &Path, kind| {
            shell_version_within(shell, kind, Some(&cache), TEST_TIMEOUT, true)
        };
        assert_eq!(version(&bash, ShellKind::Bash), Some(Version::new(5, 3, 9)));
        assert_eq!(runs(&bin), 1);
        // Read from the cache: not run again.
        assert_eq!(version(&bash, ShellKind::Bash), Some(Version::new(5, 3, 9)));
        assert_eq!(runs(&bin), 1);

        // Another binary has its own line.
        let other = temp.0.join("other");
        std::fs::create_dir_all(&other).expect("mkdir");
        let fish = fake_shell(&other, "fish", "fish, version 4.9.3");
        assert_eq!(version(&fish, ShellKind::Fish), Some(Version::new(4, 9, 3)));
        assert_eq!(runs(&other), 1);
        let cached = std::fs::read_to_string(cache.join(VERSION_CACHE)).expect("cache");
        assert_eq!(cached.lines().count(), 2, "{cached}");

        // An upgrade in place (another size and mtime) is run again, and
        // replaces the line for its path.
        std::thread::sleep(Duration::from_millis(10));
        let bash = fake_shell(
            &bin,
            "bash",
            "GNU bash, version 5.4.0(1)-release (fake, longer)",
        );
        assert_eq!(version(&bash, ShellKind::Bash), Some(Version::new(5, 4, 0)));
        assert_eq!(runs(&bin), 2);
        let cached = std::fs::read_to_string(cache.join(VERSION_CACHE)).expect("cache");
        assert_eq!(cached.lines().count(), 2, "{cached}");
        assert_eq!(version(&bash, ShellKind::Bash), Some(Version::new(5, 4, 0)));
        assert_eq!(runs(&bin), 2);

        // Not cached: a failed read, and the cache is not needed.
        let broken = fake_shell(&temp.0, "bash", "not a version");
        assert_eq!(version(&broken, ShellKind::Bash), None);
        assert_eq!(version(&broken, ShellKind::Bash), None);
        assert_eq!(runs(&temp.0), 2);
        assert_eq!(
            shell_version_within(&bash, ShellKind::Bash, None, TEST_TIMEOUT, true),
            Some(Version::new(5, 4, 0))
        );
        assert_eq!(
            shell_version(&temp.0.join("missing"), ShellKind::Bash, None),
            None
        );
    }

    #[test]
    fn a_hung_version_is_killed() {
        use std::os::unix::fs::PermissionsExt;
        let temp = Temp::new("hung");
        let path = temp.0.join("fish");
        std::fs::write(&path, "#!/bin/sh\nexec sleep 30\n").expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let started = Instant::now();
        let timeout = Duration::from_millis(300);
        assert_eq!(
            shell_version_within(&path, ShellKind::Fish, Some(&temp.0), timeout, true),
            None
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
    }

    /// Apple's bash 3.2, read for real: too old, so a plain shell.
    #[test]
    fn apple_bash_is_too_old() {
        let temp = Temp::new("apple");
        let bash = Path::new("/bin/bash");
        let version = shell_version(bash, ShellKind::Bash, Some(&temp.0));
        assert!(
            version.is_some_and(|version| version.major == 3 && version.minor == 2),
            "{version:?}"
        );
        let input = GuardInput {
            shell: bash,
            shell_version: &|kind| shell_version(bash, kind, Some(&temp.0)),
            ..input()
        };
        assert_eq!(
            decide(&input),
            plain(unsupported(
                Some(ShellKind::Bash),
                version,
                Unsupported::TooOld
            ))
        );
    }

    #[test]
    fn order_of_reasons() {
        let all = GuardInput {
            stdin_is_tty: false,
            stdout_is_tty: false,
            stdin_tty: Some("/dev/ttys004"),
            shell: Path::new("/bin/bash"),
            fac_disable: Some("1"),
            term_program: Some("WarpTerminal"),
            q_term: Some("1"),
            fac_active: Some("123:/dev/ttys004"),
            shell_version: &no_version,
        };
        assert_eq!(decide(&all), plain(PlainReason::NotATty));
        let all = GuardInput {
            stdin_is_tty: true,
            ..all
        };
        assert_eq!(decide(&all), plain(PlainReason::StdoutNotATty));
        let all = GuardInput {
            stdout_is_tty: true,
            ..all
        };
        assert_eq!(decide(&all), plain(PlainReason::Disabled));
        let all = GuardInput {
            fac_disable: None,
            ..all
        };
        assert_eq!(decide(&all), plain(PlainReason::Warp));
        let all = GuardInput {
            term_program: None,
            ..all
        };
        assert_eq!(decide(&all), plain(PlainReason::Kiro));
        let all = GuardInput {
            q_term: None,
            ..all
        };
        assert_eq!(decide(&all), plain(PlainReason::Nested));
        let all = GuardInput {
            fac_active: None,
            ..all
        };
        assert_eq!(
            decide(&all),
            plain(unsupported(
                Some(ShellKind::Bash),
                None,
                Unsupported::NoVersion
            ))
        );
    }

    #[test]
    fn shell_resolution() {
        let resolve =
            |shell: Option<&str>, exe: Option<&Path>| resolve_shell(shell.map(OsStr::new), exe);
        let zsh = (PathBuf::from(DEFAULT_SHELL), false);
        let fac = (PathBuf::from(DEFAULT_SHELL), true);
        assert_eq!(resolve(None, None), zsh);
        assert_eq!(resolve(Some(""), None), zsh);
        assert_eq!(resolve(Some("/bin/zsh"), None), zsh);
        assert_eq!(
            resolve(Some("/bin/bash"), None),
            (PathBuf::from("/bin/bash"), false)
        );
        // Named fac, wherever it is and whether or not it exists.
        assert_eq!(resolve(Some("/opt/homebrew/bin/fac"), None), fac);
        assert_eq!(resolve(Some("fac"), None), fac);

        // Another name for fac's own binary (this test binary stands in).
        let exe = std::env::current_exe().expect("current_exe");
        let dir = std::env::temp_dir().join(format!("fac-guards-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let link = dir.join("my-shell");
        std::os::unix::fs::symlink(&exe, &link).expect("symlink");
        let link = link.to_str().expect("utf-8 path");
        assert_eq!(resolve(Some(link), Some(&exe)), fac);
        assert_eq!(
            resolve(Some(exe.to_str().expect("utf-8 path")), Some(&exe)),
            fac
        );
        // Without knowing fac's binary only the name counts.
        assert_eq!(resolve(Some(link), None), (PathBuf::from(link), false));
        assert_eq!(
            resolve(Some("/bin/zsh"), Some(&exe)),
            (PathBuf::from("/bin/zsh"), false)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn child_env_marks_fac_and_kiro() {
        let env = child_env(42, "/dev/ttys007");
        assert_eq!(
            env,
            vec![
                (
                    OsString::from("FAC_ACTIVE"),
                    OsString::from("42:/dev/ttys007")
                ),
                (OsString::from("PROCESS_LAUNCHED_BY_Q"), OsString::from("1")),
            ]
        );
    }
}
