//! Puts fac's shell scripts on disk and sets up what the shell needs to
//! load them: how it is started (zsh: the ZDOTDIR shim; bash: an init file;
//! fish: an init command) and the command pipe.

use std::ffi::{OsStr, OsString};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd, RawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Context, Result, anyhow};
use nix::fcntl::{FcntlArg, FdFlag, OFlag, fcntl};

use crate::guards::ShellKind;
use crate::shell::protocol::Token;

/// The fd number the shell reads fac's commands from.
pub const CMD_FD: RawFd = 63;

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The scripts, by file name, for every shell (one dir holds them all).
const SCRIPTS: &[(&str, &str)] = &[
    (".zshenv", include_str!("../../shell/zsh/.zshenv")),
    ("fac.zsh", include_str!("../../shell/zsh/fac.zsh")),
    ("worker.zsh", include_str!("../../shell/zsh/worker.zsh")),
    ("fac.bash", include_str!("../../shell/bash/fac.bash")),
    ("worker.bash", include_str!("../../shell/bash/worker.bash")),
    ("fac.fish", include_str!("../../shell/fish/fac.fish")),
    ("worker.fish", include_str!("../../shell/fish/worker.fish")),
];

/// Tells temp dirs of one process apart.
static TEMP_COUNTER: AtomicUsize = AtomicUsize::new(0);

pub struct ShellSetup {
    /// Where the scripts are (also zsh's `ZDOTDIR`).
    pub script_dir: PathBuf,
    /// Variables to add to the shell's environment.
    pub env: Vec<(OsString, OsString)>,
    /// fac's end of the command pipe: O_CLOEXEC, O_NONBLOCK.
    pub cmd_writer: OwnedFd,
    /// The shell's end, O_CLOEXEC; to be dup2'd to `CMD_FD` in the child.
    pub cmd_reader: OwnedFd,
    /// The session token, which only the hook is given (see
    /// `protocol`, "The session token").
    pub token: Token,
    /// How the shell is started so it loads fac's hook.
    pub launch: Launch,
}

/// The shell's argv: `argv[0]` and the arguments after it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    pub argv0: OsString,
    pub args: Vec<OsString>,
}

impl Launch {
    /// A login shell with no arguments: argv0 is `-` + the file name
    /// (`-zsh`). How zsh is started, and any shell without fac's hook.
    pub fn login(shell: &Path) -> Self {
        let mut argv0 = OsString::from("-");
        argv0.push(shell.file_name().unwrap_or(shell.as_os_str()));
        Self {
            argv0,
            args: Vec::new(),
        }
    }

    /// How a `kind` shell at `shell` loads the hook in `script_dir`:
    /// - zsh: a login shell; the ZDOTDIR shim (in the env) loads fac.zsh.
    /// - bash: `<shell> --init-file <dir>/fac.bash -i`, argv0 the shell's
    ///   path without a dash, since a login bash ignores `--init-file`.
    ///   fac.bash then reads the login files itself, but `shopt login_shell`
    ///   is off. The path, not just `bash`: bash sets `$BASH` from argv0,
    ///   and for a bare name it takes the first `bash` on `PATH`, which may
    ///   be another bash.
    /// - fish: a login shell (`-fish`) with `--init-command 'source
    ///   <dir>/fac.fish'`, which fish runs after config.fish and conf.d.
    pub fn for_shell(kind: ShellKind, shell: &Path, script_dir: &Path) -> Self {
        match kind {
            ShellKind::Zsh => Self::login(shell),
            ShellKind::Bash => Self {
                argv0: shell.as_os_str().to_owned(),
                args: vec![
                    "--init-file".into(),
                    script_dir.join("fac.bash").into(),
                    "-i".into(),
                ],
            },
            ShellKind::Fish => {
                let mut command = b"source ".to_vec();
                command.extend_from_slice(&fish_quote(script_dir.join("fac.fish").as_os_str()));
                Self {
                    args: vec!["--init-command".into(), OsString::from_vec(command)],
                    ..Self::login(shell)
                }
            }
        }
    }
}

/// `text` as one fish word: in single quotes, with `\` and `'` escaped.
fn fish_quote(text: &OsStr) -> Vec<u8> {
    let mut quoted = vec![b'\''];
    for &byte in text.as_bytes() {
        if matches!(byte, b'\\' | b'\'') {
            quoted.push(b'\\');
        }
        quoted.push(byte);
    }
    quoted.push(b'\'');
    quoted
}

/// Write the embedded scripts (if not already present) to
/// `$HOME/Library/Caches/fac/shell/<version>-<hash>/` and create the
/// command pipe; see [`prepare_in`].
pub fn prepare(kind: ShellKind, shell: &Path) -> Result<ShellSetup> {
    let home = std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .ok_or_else(|| anyhow!("HOME is not set"))?;
    prepare_in(
        kind,
        shell,
        &Path::new(&home).join("Library/Caches/fac/shell"),
        std::env::var_os("ZDOTDIR"),
    )
}

/// Write the scripts under `base`, create the command pipe and the session
/// token, and return the env to add and how to start a `kind` shell at
/// `shell`. The env is `FAC_CMD_FD`, plus for zsh `ZDOTDIR` (the script
/// dir) and `FAC_USER_ZDOTDIR` (`user_zdotdir`, the current `ZDOTDIR`, or
/// empty), and for zsh and bash `FAC_TOKEN`, the token. fish's hook loads
/// only after config.fish, whose programs would inherit it: fish gets it
/// as the first line on the command pipe, `token <token>`.
pub fn prepare_in(
    kind: ShellKind,
    shell: &Path,
    base: &Path,
    user_zdotdir: Option<OsString>,
) -> Result<ShellSetup> {
    let dir = write_scripts(base)?;
    let (cmd_reader, cmd_writer) = command_pipe()?;
    if kind == ShellKind::Fish {
        // fish's `read` has no timeout: its end is non-blocking, so an apply
        // trigger with no apply waiting returns at once (fac.fish).
        set_nonblocking(cmd_reader.as_fd())?;
    }
    let token = Token::generate().map_err(|err| anyhow!("no session token: {err}"))?;
    let mut env = Vec::new();
    if kind == ShellKind::Fish {
        let lines = format!("token {}\n", token.as_str()).into_bytes();
        let written = nix::unistd::write(&cmd_writer, &lines)
            .context("writing the token to the command pipe")?;
        if written != lines.len() {
            return Err(anyhow!("the token did not fit in the command pipe"));
        }
    } else {
        env.push((OsString::from("FAC_TOKEN"), OsString::from(token.as_str())));
    }
    if kind == ShellKind::Zsh {
        env.push((OsString::from("ZDOTDIR"), dir.clone().into_os_string()));
        env.push((
            OsString::from("FAC_USER_ZDOTDIR"),
            user_zdotdir.unwrap_or_default(),
        ));
    }
    env.push((OsString::from("FAC_CMD_FD"), CMD_FD.to_string().into()));
    Ok(ShellSetup {
        launch: Launch::for_shell(kind, shell, &dir),
        script_dir: dir,
        env,
        cmd_writer,
        cmd_reader,
        token,
    })
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// 64-bit FNV-1a of `bytes`, continuing from `hash`.
fn fnv1a(hash: u64, bytes: &[u8]) -> u64 {
    bytes.iter().fold(hash, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME)
    })
}

/// FNV-1a over every script's name and bytes, each followed by a NUL.
fn scripts_hash() -> u64 {
    SCRIPTS.iter().fold(FNV_OFFSET, |hash, (name, body)| {
        let hash = fnv1a(hash, name.as_bytes());
        let hash = fnv1a(hash, &[0]);
        let hash = fnv1a(hash, body.as_bytes());
        fnv1a(hash, &[0])
    })
}

/// Whether `dir` holds every script with the right contents.
fn complete(dir: &Path) -> bool {
    SCRIPTS.iter().all(|(name, body)| {
        std::fs::read(dir.join(name)).is_ok_and(|bytes| bytes == body.as_bytes())
    })
}

/// Write the scripts into `<base>/<version>-<hash>/` unless they are there
/// already, through a temp sibling dir and a rename, so a shell never sees a
/// half-written dir. Returns the dir.
pub fn write_scripts(base: &Path) -> Result<PathBuf> {
    let dir = base.join(format!("{VERSION}-{:016x}", scripts_hash()));
    if complete(&dir) {
        return Ok(dir);
    }
    if dir.exists() {
        tracing::warn!("{} is incomplete; rewriting it", dir.display());
        std::fs::remove_dir_all(&dir).with_context(|| format!("removing {}", dir.display()))?;
    }
    std::fs::create_dir_all(base).with_context(|| format!("creating {}", base.display()))?;
    let temp = base.join(format!(
        ".tmp-{}-{}",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir(&temp).with_context(|| format!("creating {}", temp.display()))?;
    for (name, body) in SCRIPTS {
        let path = temp.join(name);
        std::fs::write(&path, body).with_context(|| format!("writing {}", path.display()))?;
    }
    if let Err(err) = std::fs::rename(&temp, &dir) {
        let _ = std::fs::remove_dir_all(&temp);
        // Another fac starting at the same time won the race.
        if !complete(&dir) {
            return Err(err).with_context(|| format!("renaming to {}", dir.display()));
        }
    }
    tracing::info!("wrote shell scripts to {}", dir.display());
    Ok(dir)
}

/// The command pipe as (reader, writer): O_CLOEXEC on both ends, the writer
/// O_NONBLOCK. macOS has no pipe2, so the flags are set right after.
fn command_pipe() -> Result<(OwnedFd, OwnedFd)> {
    let (reader, writer) = nix::unistd::pipe().context("creating the command pipe")?;
    set_cloexec(reader.as_fd())?;
    set_cloexec(writer.as_fd())?;
    set_nonblocking(writer.as_fd())?;
    Ok((reader, writer))
}

fn set_nonblocking(fd: BorrowedFd<'_>) -> Result<()> {
    let flags = OFlag::from_bits_truncate(fcntl(fd, FcntlArg::F_GETFL)?);
    fcntl(fd, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK))
        .context("O_NONBLOCK on the command pipe")?;
    Ok(())
}

fn set_cloexec(fd: BorrowedFd<'_>) -> Result<()> {
    let flags = FdFlag::from_bits_truncate(fcntl(fd, FcntlArg::F_GETFD)?);
    fcntl(fd, FcntlArg::F_SETFD(flags | FdFlag::FD_CLOEXEC))
        .context("FD_CLOEXEC on the command pipe")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_base(name: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!("fac-install-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        base
    }

    #[test]
    fn writes_the_scripts_once() {
        let base = temp_base("once");
        let dir = write_scripts(&base).expect("first write");
        assert!(complete(&dir));
        let name = dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        assert_eq!(name, format!("{VERSION}-{:016x}", scripts_hash()));
        let again = write_scripts(&base).expect("second write");
        assert_eq!(dir, again);
        // Only the final dir is left, no temp dirs.
        let entries: Vec<_> = std::fs::read_dir(&base)
            .expect("read_dir")
            .map(|entry| entry.expect("entry").file_name())
            .collect();
        assert_eq!(entries.len(), 1, "{entries:?}");
        std::fs::remove_dir_all(&base).expect("cleanup");
    }

    #[test]
    fn repairs_an_incomplete_dir() {
        let base = temp_base("repair");
        let dir = write_scripts(&base).expect("first write");
        std::fs::write(dir.join("fac.zsh"), "truncated").expect("truncate");
        assert!(!complete(&dir));
        assert_eq!(write_scripts(&base).expect("rewrite"), dir);
        assert!(complete(&dir));
        std::fs::remove_dir_all(&base).expect("cleanup");
    }

    #[test]
    fn hash_is_fnv1a() {
        // Known FNV-1a 64 values of the empty input and of "a".
        let fnv = |bytes: &[u8]| fnv1a(FNV_OFFSET, bytes);
        assert_eq!(fnv(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv(b"a"), 0xaf63_dc4c_8601_ec8c);
        let mut all = Vec::new();
        for (name, body) in SCRIPTS {
            all.extend_from_slice(name.as_bytes());
            all.push(0);
            all.extend_from_slice(body.as_bytes());
            all.push(0);
        }
        assert_eq!(scripts_hash(), fnv(&all));
    }

    #[test]
    fn launch_specs() {
        let dir = Path::new("/cache/fac/shell/0.1.0-abc");
        let os = |text: &str| OsString::from(text);
        assert_eq!(
            Launch::for_shell(ShellKind::Zsh, Path::new("/bin/zsh"), dir),
            Launch {
                argv0: os("-zsh"),
                args: Vec::new(),
            }
        );
        assert_eq!(
            Launch::login(Path::new("/opt/homebrew/bin/fish")).argv0,
            "-fish"
        );
        assert_eq!(
            Launch::for_shell(ShellKind::Bash, Path::new("/opt/homebrew/bin/bash"), dir),
            Launch {
                argv0: os("/opt/homebrew/bin/bash"),
                args: vec![
                    os("--init-file"),
                    os("/cache/fac/shell/0.1.0-abc/fac.bash"),
                    os("-i"),
                ],
            }
        );
        assert_eq!(
            Launch::for_shell(ShellKind::Fish, Path::new("/opt/homebrew/bin/fish"), dir),
            Launch {
                argv0: os("-fish"),
                args: vec![
                    os("--init-command"),
                    os("source '/cache/fac/shell/0.1.0-abc/fac.fish'"),
                ],
            }
        );
        // A path with quotes, backslashes and spaces stays one word.
        let odd = Path::new("/Users/o'neil/my \\ dir");
        assert_eq!(
            Launch::for_shell(ShellKind::Fish, Path::new("fish"), odd).args[1],
            os(r"source '/Users/o\'neil/my \\ dir/fac.fish'")
        );
    }

    #[test]
    fn the_env_is_per_shell() {
        let base = temp_base("env");
        let setup = |kind| {
            prepare_in(
                kind,
                Path::new("/bin/sh"),
                &base,
                Some(OsString::from("/z")),
            )
            .expect("prepare")
        };
        let names = |setup: &ShellSetup| {
            setup
                .env
                .iter()
                .map(|(name, value)| format!("{}={}", name.display(), value.display()))
                .collect::<Vec<_>>()
        };
        let zsh = setup(ShellKind::Zsh);
        let dir = write_scripts(&base).expect("scripts");
        assert_eq!(
            names(&zsh),
            [
                format!("FAC_TOKEN={}", zsh.token.as_str()),
                format!("ZDOTDIR={}", dir.display()),
                "FAC_USER_ZDOTDIR=/z".to_owned(),
                "FAC_CMD_FD=63".to_owned(),
            ]
        );
        let bash = setup(ShellKind::Bash);
        assert_eq!(
            names(&bash),
            [
                format!("FAC_TOKEN={}", bash.token.as_str()),
                "FAC_CMD_FD=63".to_owned()
            ]
        );
        assert_ne!(zsh.token, bash.token, "a token per session");
        // fish: the token on the command pipe, not in the env.
        let fish = setup(ShellKind::Fish);
        assert_eq!(names(&fish), ["FAC_CMD_FD=63"]);
        let mut buf = [0u8; 4096];
        let n = nix::unistd::read(&fish.cmd_reader, &mut buf).expect("reading the pipe");
        assert_eq!(
            &buf[..n],
            format!("token {}\n", fish.token.as_str()).as_bytes()
        );
        for name in ["fac.bash", "worker.bash", "fac.fish", "worker.fish"] {
            assert!(dir.join(name).is_file(), "{name}");
        }
        std::fs::remove_dir_all(&base).expect("cleanup");
    }

    #[test]
    fn pipe_flags() {
        let (reader, writer) = command_pipe().expect("pipe");
        for fd in [&reader, &writer] {
            let flags = FdFlag::from_bits_truncate(fcntl(fd, FcntlArg::F_GETFD).expect("F_GETFD"));
            assert!(flags.contains(FdFlag::FD_CLOEXEC));
        }
        let status = |fd: &OwnedFd| {
            OFlag::from_bits_truncate(fcntl(fd, FcntlArg::F_GETFL).expect("F_GETFL"))
        };
        assert!(status(&writer).contains(OFlag::O_NONBLOCK));
        assert!(!status(&reader).contains(OFlag::O_NONBLOCK));
    }

    /// fish's end of the pipe is non-blocking (its `read` has no timeout);
    /// zsh's and bash's block, as they always have.
    #[test]
    fn only_fishs_reader_is_non_blocking() {
        let base = temp_base("nonblock");
        for (kind, nonblocking) in [
            (ShellKind::Zsh, false),
            (ShellKind::Bash, false),
            (ShellKind::Fish, true),
        ] {
            let setup = prepare_in(kind, Path::new("/bin/sh"), &base, None).expect("prepare");
            let flags = OFlag::from_bits_truncate(
                fcntl(&setup.cmd_reader, FcntlArg::F_GETFL).expect("F_GETFL"),
            );
            assert_eq!(flags.contains(OFlag::O_NONBLOCK), nonblocking, "{kind:?}");
        }
        std::fs::remove_dir_all(&base).expect("cleanup");
    }
}
