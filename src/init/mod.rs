//! `fac init`, `fac enable`, `fac disable` and `fac uninstall`: fac starts
//! with every new window of the terminals the user ticks, through one
//! fac-owned block in the shell's startup file. When an interactive shell
//! starts in a ticked terminal, the block `exec`s fac, and fac starts the
//! shell again with its hook. Terminal settings are never edited. The
//! block is the only state (`block.rs`).
//!
//! # Where the block goes
//!
//! - zsh: the very top of `.zshrc` in the user's ZDOTDIR ([`Env::zsh_dir`]),
//!   above Kiro's block and p10k's instant prompt, so the exec comes before
//!   the rest of the file runs; and the very top of `.zprofile` there when
//!   it exists, since a login shell runs `.zprofile` first and an exec in
//!   it (Kiro's pre block) would start before fac. A missing `.zprofile` is
//!   never created: nothing in it can run first. In a login shell the
//!   `.zshrc` block runs again after a `.zprofile` block that skipped, and
//!   skips the same way; in fac's own login shell both skip on the mark.
//! - bash: see [`bash_files`].
//! - fish: its own file, `${XDG_CONFIG_HOME:-~/.config}/fish/conf.d/00-fac.fish`;
//!   `config.fish` is never edited. fish sources the conf.d files of all
//!   its conf.d folders in one natural-sort order of their names, so
//!   another file's exec (`00_kiro_pre.fish`, `00_fig_pre.fish`) could run
//!   before a `fac.fish`; `00-fac.fish` sorts before those, before any name
//!   that starts with a letter, a `_` or a number above 00, and after only
//!   names that start with `!`, `+` or `-`. An older `fac.fish` (fac's, by
//!   its header line) is removed by `fac init` and `fac uninstall`.
//!
//! Nothing is ever written under a temporary folder (`$TMPDIR`, `/tmp`,
//! `/private/var/folders`): VS Code's zsh integration points `ZDOTDIR` at
//! one, and a block there would be lost. None of the commands runs as
//! root: `sudo` keeps HOME, and the rename would leave a root-owned file.
//!
//! # No loops
//!
//! Every shell fac starts carries `FAC_SHELL` ([`crate::guards::SHELL_MARK`]),
//! and the block never execs fac in a shell that has it:
//! - fac's own child (the normal run, with fac's hook; also when the hook
//!   could not be set up) gets `FAC_SHELL=run`. The block skips on it.
//! - a plain shell fac execs in its own place (every fallback in
//!   `guards.rs`: `FAC_DISABLE`, Warp, inside Kiro, nested, an unsupported
//!   or too old shell, no tty, and a degraded start) gets
//!   `FAC_SHELL=plain:<pid>`. exec keeps the pid, so it is the shell's own
//!   `$$` (fish: `$fish_pid`). The block skips when it names this shell.
//!
//! Not exported any more, it reaches no other process: the block stops
//! exporting it when it skips on it, and so does each hook once startup is
//! done (zsh at the first prompt, bash at the end of fac.bash, fish at the
//! top of fac.fish), as with `PROCESS_LAUNCHED_BY_Q`. It stays a shell
//! variable, so sourcing the startup file again (`source ~/.zshrc`) skips
//! too. A nested `zsh` or a terminal app started from a fac shell inherits
//! nothing, so VS Code started from fac starts fac in its own terminal.
//! `FAC_ACTIVE` (`<pid>:<tty>`, inherited) is not the mark: a nested zsh on
//! fac's tty skips by it (`$TTY`, a builtin), and so does a nested bash
//! (`/dev/stdin -ef` the tty in `FAC_ACTIVE`, a builtin test), so it stays
//! the non-login bash it was started as. A nested fish, which cannot tell
//! its tty without a command, execs fac, which sees the nesting and execs a
//! plain shell marked `plain:<pid>`. A `plain`
//! value left in an environment (a plain shell with no block of its kind
//! exports it on) names a pid that is not the reader's, so it changes
//! nothing; a `run` value lives only until the hook's startup ends.
//!
//! The block also checks, with builtins only, that the shell is
//! interactive, not running a `-c` command or a script (zsh: `ZSH_SCRIPT`
//! is `ZSH_ARGZERO`; bash: `$0` is not `bash` or `-bash`; fish cannot
//! tell), on a terminal (stdin and stdout), not `FAC_DISABLE=1` (fac's own
//! rule), not inside Kiro or Amazon Q (`Q_TERM`), not over SSH
//! (`SSH_CONNECTION`), not inside tmux or screen (`TMUX`, `STY`: they are
//! not tested terminals and inherit the outer terminal's variables), that
//! fac is there and executable, and that the terminal is a ticked one.
//! Then, while `${XDG_CONFIG_HOME:-~/.config}/fac/no-start` exists (safe
//! mode: fac writes it when it fails to start, see [`start_failed`]), the
//! block execs nothing and prints one dim line to stderr instead, once per
//! shell, when stderr is a terminal ([`block::NO_START_LINE`]).
//!
//! bash sets `SHELL` before the exec, so fac starts the bash that ran the
//! block, not an older one `$SHELL` names: `SHELL=$BASH`. `$BASH` is the
//! running binary in a non-login shell. In a login shell started as
//! `-bash` (Terminal.app, iTerm2) bash sets `$BASH` to the user's passwd
//! shell instead, which login also put in `$SHELL`; when that is not a bash
//! at all (a zsh user whose terminal runs bash), `$SHELL` is kept if it
//! names a bash, else PATH's bash (`type -P`, a subshell only on the exec
//! branch). No builtin can tell a login shell's own binary.

pub mod block;
pub mod checklist;
pub mod doctor;
pub mod files;
pub mod screens;
pub mod style;

use std::ffi::{CStr, OsStr, OsString};
use std::fs::OpenOptions;
use std::io::IsTerminal;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Result, anyhow};

use crate::guards::{self, ShellKind};
use crate::term::ident::{KNOWN, TermKind};
use block::Block;
use files::{After, Change, Current, Done};
use style::Style;

/// The first line of fac's ZDOTDIR shim (`shell/zsh/.zshenv`).
const SHIM_FIRST_LINE: &[u8] = b"# fac's ZDOTDIR shim.";

/// The folders fac keeps after `fac uninstall`, under HOME.
const KEPT: [&str; 4] = [
    ".config/fac",
    "Library/Application Support/fac",
    "Library/Logs/fac",
    "Library/Caches/fac",
];

/// fish's file under the config folder, named to be sourced first (see
/// the module docs).
const FISH_FILE: &str = "fish/conf.d/00-fac.fish";
/// The name fish's file had before.
const OLD_FISH_FILE: &str = "fish/conf.d/fac.fish";

const USAGE: &str = "usage: fac init [--terminals terminal,iterm2,ghostty,vscode,zed | all] [--shell zsh|bash|fish]";

/// The user's home from the password database.
pub fn passwd_home() -> Option<PathBuf> {
    // SAFETY: getpwuid_r writes only into `pwd` and `buf`, which outlive
    // the call; `pw_dir` points into `buf`.
    unsafe {
        let mut pwd: libc::passwd = std::mem::zeroed();
        let mut buf = vec![0 as libc::c_char; 16 * 1024];
        let mut found = std::ptr::null_mut();
        let rc = libc::getpwuid_r(
            libc::getuid(),
            &mut pwd,
            buf.as_mut_ptr(),
            buf.len(),
            &mut found,
        );
        if rc != 0 || found.is_null() || pwd.pw_dir.is_null() {
            return None;
        }
        let dir = CStr::from_ptr(pwd.pw_dir).to_bytes();
        Some(PathBuf::from(OsStr::from_bytes(dir)))
    }
}

/// What the commands read from the environment.
#[derive(Debug, Clone, Default)]
pub struct Env {
    pub home: PathBuf,
    pub zdotdir: Option<PathBuf>,
    pub fac_user_zdotdir: Option<PathBuf>,
    /// `USER_ZDOTDIR`: VS Code's zsh integration keeps the user's ZDOTDIR
    /// (or HOME) here and points `ZDOTDIR` at a temporary folder.
    pub user_zdotdir: Option<PathBuf>,
    pub xdg_config_home: Option<PathBuf>,
    pub shell: Option<PathBuf>,
    pub path: Option<OsString>,
    pub q_term: bool,
    /// Folders nothing is written under: the temporary ones.
    pub temp_roots: Vec<PathBuf>,
}

impl Env {
    /// From the process. An error without `HOME`.
    pub fn from_process() -> Result<Env> {
        let var = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
        let home = var("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| anyhow!("HOME is not set"))?;
        Ok(Env {
            home,
            zdotdir: var("ZDOTDIR").map(PathBuf::from),
            fac_user_zdotdir: var("FAC_USER_ZDOTDIR").map(PathBuf::from),
            user_zdotdir: var("USER_ZDOTDIR").map(PathBuf::from),
            xdg_config_home: var("XDG_CONFIG_HOME").map(PathBuf::from),
            shell: var("SHELL").map(PathBuf::from),
            path: var("PATH"),
            q_term: var("Q_TERM").is_some(),
            temp_roots: [
                "/tmp",
                "/private/tmp",
                "/var/folders",
                "/private/var/folders",
            ]
            .into_iter()
            .map(PathBuf::from)
            .chain(var("TMPDIR").map(PathBuf::from))
            .collect(),
        })
    }

    /// The user's own ZDOTDIR, the folder zsh reads `.zshrc` from:
    /// - inside fac, while `ZDOTDIR` still names fac's shim:
    ///   `FAC_USER_ZDOTDIR` (else as below, without `ZDOTDIR`);
    /// - inside VS Code's zsh integration, which points `ZDOTDIR` at a
    ///   temporary folder: `USER_ZDOTDIR`, where VS Code keeps the user's;
    /// - else `ZDOTDIR`, else HOME.
    pub fn zsh_dir(&self) -> PathBuf {
        let is_shim = |dir: &Path| {
            std::fs::read(dir.join(".zshenv")).is_ok_and(|text| text.starts_with(SHIM_FIRST_LINE))
        };
        let shim = self.zdotdir.as_deref().is_some_and(is_shim);
        let own = if shim {
            self.fac_user_zdotdir.as_ref()
        } else {
            None
        };
        own.or(self.user_zdotdir.as_ref())
            .or(self.zdotdir.as_ref().filter(|_| !shim))
            .unwrap_or(&self.home)
            .clone()
    }

    fn fish_file(&self, config: &Path) -> PathBuf {
        config.join(FISH_FILE)
    }

    fn config_home(&self) -> PathBuf {
        self.xdg_config_home
            .clone()
            .unwrap_or_else(|| self.home.join(".config"))
    }

    /// Safe mode's file, where every block looks for it:
    /// `${XDG_CONFIG_HOME:-$HOME/.config}/fac/no-start`.
    pub fn no_start(&self) -> PathBuf {
        self.config_home().join(block::NO_START)
    }
}

/// Safe mode's file for this process ([`Env::no_start`]); `None` without
/// `HOME`.
pub fn no_start_path() -> Option<PathBuf> {
    Env::from_process().ok().map(|env| env.no_start())
}

/// Turn safe mode on after a failed start: write `path` with one line,
/// `fac failed to start: <reason>`. `reason` is a fixed class (`panic`,
/// `proxy error`), never a user value.
pub fn start_failed(path: &Path, reason: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, format!("fac failed to start: {reason}\n"))
}

/// Turn safe mode off: remove `path`. True when it was there.
pub fn clear_safe_mode(path: &Path) -> std::io::Result<bool> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err),
    }
}

/// The bash files the block goes in, under `home`. bash reads, in a login
/// shell (Terminal.app and iTerm2 start those), the first that exists of
/// `~/.bash_profile`, `~/.bash_login` and `~/.profile`, and in an
/// interactive non-login shell (other terminals may start those)
/// `~/.bashrc`. The rule: the block goes in both the login file bash reads
/// and `~/.bashrc`, so both kinds of shell run it. A login file that
/// sources `.bashrc` runs it twice, which is harmless: the first one execs
/// fac or skips, and the second skips the same way. When none of the three
/// login files exists, `~/.bash_profile` is created; a `.bash_profile` is
/// never created next to a `.bash_login` or `.profile`, which bash would
/// then stop reading. A missing `.bashrc` is created. The block in
/// `.profile` parses in sh and acts only under bash (not bash's sh mode).
pub fn bash_files(home: &Path) -> [PathBuf; 2] {
    let login = [".bash_profile", ".bash_login", ".profile"]
        .into_iter()
        .map(|name| home.join(name))
        .find(|path| path.exists())
        .unwrap_or_else(|| home.join(".bash_profile"));
    [login, home.join(".bashrc")]
}

/// The files the block of a `kind` shell goes in. zsh: `.zshrc`, and
/// `.zprofile` when it exists (see the module docs).
pub fn files_for(kind: ShellKind, env: &Env) -> Vec<PathBuf> {
    match kind {
        ShellKind::Zsh => {
            let dir = env.zsh_dir();
            let profile = dir.join(".zprofile");
            let mut files = vec![dir.join(".zshrc")];
            if profile.exists() {
                files.push(profile);
            }
            files
        }
        ShellKind::Bash => bash_files(&env.home).to_vec(),
        ShellKind::Fish => vec![env.fish_file(&env.config_home())],
    }
}

/// Every file a block of any shell may be in, each once.
pub fn all_files(env: &Env) -> Vec<(ShellKind, PathBuf)> {
    let mut out: Vec<(ShellKind, PathBuf)> = Vec::new();
    let mut add = |kind, path: PathBuf| {
        if !out.iter().any(|(_, seen)| *seen == path) {
            out.push((kind, path));
        }
    };
    for name in [".zshrc", ".zprofile"] {
        add(ShellKind::Zsh, env.zsh_dir().join(name));
        add(ShellKind::Zsh, env.home.join(name));
    }
    for name in [".bash_profile", ".bash_login", ".profile", ".bashrc"] {
        add(ShellKind::Bash, env.home.join(name));
    }
    for config in [env.config_home(), env.home.join(".config")] {
        add(ShellKind::Fish, env.fish_file(&config));
        add(ShellKind::Fish, config.join(OLD_FISH_FILE));
    }
    out
}

/// A file `uninstall`, `enable` or `disable` looks at, read, when it may
/// hold a block; `None` for one that cannot: missing, a symlink to
/// nothing, or without the marker bytes (checked before any parsing). A
/// file that cannot be read is skipped too, with a line saying so: fac
/// never wrote it, or it would be readable.
fn scan(path: &Path) -> Option<Current> {
    let dangling = std::fs::symlink_metadata(path).is_ok()
        && std::fs::metadata(path).is_err_and(|err| err.kind() == std::io::ErrorKind::NotFound);
    if dangling {
        return None;
    }
    match files::read(path) {
        Ok(current) => current
            .bytes
            .as_deref()
            .is_some_and(block::has_marker)
            .then_some(current),
        Err(err) => {
            let style = Style::detect(std::io::stderr().is_terminal());
            eprint!("{}", screens::note(&style, &format!("{err:#}; skipped it")));
            None
        }
    }
}

/// The temporary folder `path` is in, if any: as given, and with its
/// symlinks resolved (`/tmp` is `/private/tmp`).
fn temp_root<'a>(path: &Path, roots: &'a [PathBuf]) -> Option<&'a PathBuf> {
    // The longest part of `path` that exists, resolved, and the rest.
    let resolved = path
        .ancestors()
        .find_map(|part| {
            let real = std::fs::canonicalize(part).ok()?;
            Some(real.join(path.strip_prefix(part).ok()?))
        })
        .unwrap_or_else(|| path.to_path_buf());
    roots.iter().find(|root| {
        let real_root = std::fs::canonicalize(root).unwrap_or_else(|_| (*root).clone());
        path.starts_with(root) || resolved.starts_with(root) || resolved.starts_with(&real_root)
    })
}

/// An error when any of `paths` is under a temporary folder.
fn refuse_temp<'a>(paths: impl IntoIterator<Item = &'a Path>, env: &Env) -> Result<()> {
    for path in paths {
        if let Some(root) = temp_root(path, &env.temp_roots) {
            return Err(problem(
                format!(
                    "{} is in a temporary folder ({}), so fac will not write it",
                    path.display(),
                    root.display()
                ),
                "ZDOTDIR (or HOME or XDG_CONFIG_HOME) points there, as VS Code's zsh integration does: run fac in another terminal, or set ZDOTDIR to your own folder",
            ));
        }
    }
    Ok(())
}

/// [`files::apply`], after checking no change is under a temporary folder.
fn apply(changes: &[Change], env: &Env) -> Result<Vec<Done>> {
    refuse_temp(
        changes
            .iter()
            .flat_map(|change| [change.current.path.as_path(), change.current.real.as_path()]),
        env,
    )?;
    files::apply(changes)
}

/// The refusal to run `fac <command>` as root (`euid` 0), or `None`.
fn root_refusal(euid: u32, command: &str) -> Option<Problem> {
    (euid == 0).then(|| Problem {
        reason: format!("fac {command} changes your own startup files, so it will not run as root"),
        hint: Some(
            "Run it as yourself, not as root: sudo keeps HOME, and the files would end up owned by root."
                .to_owned(),
        ),
    })
}

/// Exit 1 with a message when running as root.
fn refuse_root(command: &str) -> Option<ExitCode> {
    // SAFETY: geteuid has no preconditions and cannot fail.
    let euid = unsafe { libc::geteuid() };
    root_refusal(euid, command).map(|found| {
        show_problem(&found.reason, found.hint.as_deref());
        ExitCode::FAILURE
    })
}

/// The path the block starts fac by: the `PATH` entry named `fac` that is
/// the running binary (canonical paths equal), as the user runs it, so a
/// Homebrew upgrade (a new Cellar folder behind the same link) keeps it
/// working; else the running binary itself.
pub fn fac_path(path_var: Option<&OsStr>, exe: &Path) -> PathBuf {
    let canonical = std::fs::canonicalize(exe).unwrap_or_else(|_| exe.to_path_buf());
    path_var
        .into_iter()
        .flat_map(std::env::split_paths)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join("fac"))
        .find(|candidate| {
            std::fs::canonicalize(candidate).is_ok_and(|resolved| resolved == canonical)
        })
        .unwrap_or_else(|| exe.to_path_buf())
}

/// The shell kind: `--shell`, else `$SHELL`'s file name.
fn shell_kind(flag: Option<&str>, env: &Env) -> Result<ShellKind, String> {
    if let Some(name) = flag {
        return ShellKind::of(Path::new(name))
            .filter(|_| !name.contains('/'))
            .ok_or_else(|| format!("--shell takes zsh, bash or fish, not {name:?}"));
    }
    let Some(shell) = &env.shell else {
        return Err("SHELL is not set; pass --shell zsh, bash or fish".to_owned());
    };
    ShellKind::of(shell).ok_or_else(|| {
        format!(
            "fac works with zsh, bash and fish, and SHELL is {}; pass --shell zsh, bash or fish",
            shell.display()
        )
    })
}

/// `--terminals`' list: ids, or `all`.
fn parse_terminals(list: &str) -> Result<Vec<TermKind>, String> {
    if list == "all" {
        return Ok(KNOWN.to_vec());
    }
    let mut out = Vec::new();
    for id in list.split(',') {
        match TermKind::from_id(id.trim()) {
            Some(kind) => out.push(kind),
            None => {
                return Err(format!(
                    "unknown terminal {id:?}: --terminals takes terminal, iterm2, ghostty, vscode, zed or all"
                ));
            }
        }
    }
    Ok(block::sorted(out))
}

fn labels(terminals: &[TermKind]) -> String {
    terminals
        .iter()
        .map(|kind| kind.label())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Whether Kiro or Amazon Q is set up in `text`: its blocks or its CLI.
fn mentions_kiro(text: &[u8]) -> bool {
    let text = String::from_utf8_lossy(text);
    [
        "Q pre block",
        "Kiro CLI",
        "kiro-cli",
        "Amazon Q",
        "amazon-q",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

/// The block in a file read, or an error naming the file.
fn block_in(current: &Current) -> Result<Option<Block>> {
    match &current.bytes {
        Some(bytes) => {
            block::read(bytes).map_err(|err| anyhow!("{}: {err}", current.path.display()))
        }
        None => Ok(None),
    }
}

/// The change that puts `block` (for a `kind` shell) in a file read, or
/// `None` when it is there already as it would be written.
fn put_change(
    kind: ShellKind,
    current: Current,
    block: &Block,
    fac: &Path,
) -> Result<Option<Change>> {
    let old = block_in(&current)?;
    if kind == ShellKind::Fish
        && let Some(bytes) = &current.bytes
        && !block::starts_with_block(bytes)
    {
        return Err(anyhow!(
            "{} is there already and is not fac's; move it away and run fac init again",
            current.path.display()
        ));
    }
    let block = Block {
        // Whether fac created the file stays as the file's own block says.
        created: old
            .as_ref()
            .map_or(current.bytes.is_none(), |old| old.created),
        ..block.clone()
    };
    let text = block::render(kind, &block, fac)?;
    let bytes = current.bytes.as_deref().unwrap_or_default();
    let in_file = |err: anyhow::Error| anyhow!("{}: {err}", current.path.display());
    let after = block::put(bytes, &text).map_err(in_file)?;
    if current.bytes.as_deref() == Some(after.as_slice()) {
        return Ok(None);
    }
    // The backup is the file without fac's block.
    let backup = match block.created {
        true => None,
        false => Some(block::strip(bytes).map_err(in_file)?),
    };
    Ok(Some(Change {
        backup,
        after: After::Write(after),
        current,
    }))
}

/// A refusal or error: a reason and, when there is one, what to do.
#[derive(Debug, PartialEq, Eq)]
struct Problem {
    reason: String,
    hint: Option<String>,
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.reason)
    }
}

impl std::error::Error for Problem {}

/// A [`Problem`] as an error, to be shown with its hint by [`fail`].
fn problem(reason: impl Into<String>, hint: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(Problem {
        reason: reason.into(),
        hint: Some(hint.into()),
    })
}

/// Print a problem on stderr.
fn show_problem(reason: &str, hint: Option<&str>) {
    let style = Style::detect(std::io::stderr().is_terminal());
    eprint!("{}", screens::problem(&style, reason, hint));
}

fn fail(err: &anyhow::Error) -> ExitCode {
    match err.downcast_ref::<Problem>() {
        Some(found) => show_problem(&found.reason, found.hint.as_deref()),
        None => show_problem(&format!("{err:#}"), None),
    }
    ExitCode::FAILURE
}

/// A usage error: exit 2.
fn usage(reason: &str, usage: &str) -> ExitCode {
    show_problem(reason, Some(usage));
    ExitCode::from(2)
}

/// `fac init [--terminals <list>] [--shell <name>]`.
pub fn init(args: &[String]) -> ExitCode {
    if let Some(code) = refuse_root("init") {
        return code;
    }
    let mut terminals = None;
    let mut shell = None;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        let (name, value) = match arg.split_once('=') {
            Some((name, value)) => (name, Some(value.to_owned())),
            None => (arg.as_str(), None),
        };
        let slot = match name {
            "--terminals" => &mut terminals,
            "--shell" => &mut shell,
            _ => return usage("That is not an option of fac init", USAGE),
        };
        match value.or_else(|| rest.next().cloned()) {
            Some(value) if slot.is_none() => *slot = Some(value),
            _ => return usage("An option of fac init is missing its value", USAGE),
        }
    }
    let env = match Env::from_process() {
        Ok(env) => env,
        Err(err) => return fail(&err),
    };
    let kind = match shell_kind(shell.as_deref(), &env) {
        Ok(kind) => kind,
        Err(reason) => return usage(&reason, USAGE),
    };
    let chosen = match terminals.as_deref().map(parse_terminals) {
        Some(Ok(chosen)) => Some(chosen),
        Some(Err(reason)) => return usage(&reason, USAGE),
        None => None,
    };
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(err) => return fail(&anyhow!("cannot find fac's own binary: {err}")),
    };
    let fac = fac_path(env.path.as_deref(), &exe);
    match run_init(kind, &env, chosen, &fac) {
        Ok(code) => code,
        Err(err) => fail(&err),
    }
}

/// The terminal to draw the checklist on, or `None` without one.
fn open_tty() -> Option<std::fs::File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .ok()
}

fn run_init(
    kind: ShellKind,
    env: &Env,
    chosen: Option<Vec<TermKind>>,
    fac: &Path,
) -> Result<ExitCode> {
    let paths = files_for(kind, env);
    refuse_temp(paths.iter().map(PathBuf::as_path), env)?;
    let read: Vec<Current> = paths
        .iter()
        .map(|path| files::read(path))
        .collect::<Result<_>>()?;
    let mut existing = None;
    for current in &read {
        if let Some(found) = block_in(current)? {
            existing.get_or_insert(found);
        }
    }
    let kiro = env.q_term
        || read
            .iter()
            .any(|current| current.bytes.as_deref().is_some_and(mentions_kiro));
    let out_style = Style::detect(std::io::stdout().is_terminal());
    let (chosen, interactive) = match chosen {
        Some(chosen) => (chosen, false),
        None => {
            let Some(tty) = open_tty() else {
                return Ok(usage(
                    "There is no terminal to show the checklist on; pass --terminals",
                    USAGE,
                ));
            };
            // On the terminal with the list, even when stdout is not it.
            let tty_style = Style::detect(true);
            let names: Vec<&str> = KNOWN.iter().map(|kind| kind.label()).collect();
            let screen = checklist::Screen {
                style: &tty_style,
                labels: &names,
                files: style::join_words(
                    &paths
                        .iter()
                        .map(|path| tty_style.path(path))
                        .collect::<Vec<_>>()
                        .iter()
                        .map(String::as_str)
                        .collect::<Vec<_>>(),
                ),
                kiro,
            };
            let ticked = KNOWN
                .iter()
                .map(|kind| existing.as_ref().is_none_or(|b| b.terminals.contains(kind)))
                .collect();
            match checklist::run(&tty, &screen, ticked)? {
                Some(ticked) => (
                    KNOWN
                        .iter()
                        .zip(ticked)
                        .filter_map(|(kind, on)| on.then_some(*kind))
                        .collect(),
                    true,
                ),
                None => {
                    print!(
                        "{}",
                        screens::dim_line(&out_style, "Cancelled. Nothing changed.")
                    );
                    return Ok(ExitCode::FAILURE);
                }
            }
        }
    };
    if chosen.is_empty() {
        // Nothing ticked: as `fac disable`.
        return match existing {
            Some(_) => set_on(env, false, Some(kind), fac),
            None => {
                print!(
                    "{}",
                    screens::dim_line(&out_style, "Nothing ticked. Nothing changed.")
                );
                Ok(ExitCode::SUCCESS)
            }
        };
    }
    let block = Block {
        on: true,
        terminals: chosen.clone(),
        created: false,
    };
    let mut changes = Vec::new();
    for current in read {
        changes.extend(put_change(kind, current, &block, fac)?);
    }
    if kind == ShellKind::Fish {
        // fish's file under its old name, so only one block runs.
        for config in [env.config_home(), env.home.join(".config")] {
            if let Some(current) = scan(&config.join(OLD_FISH_FILE)) {
                changes.extend(remove_change(kind, current)?);
            }
        }
    }
    let done = apply(&changes, env)?;
    let mut notes = Vec::new();
    if kiro && !interactive {
        notes.push("Kiro is set up too: fac starts instead of it in these terminals.".to_owned());
    }
    notes.extend(old_shell_note(kind, env, &out_style));
    print!(
        "{}",
        screens::set_up(
            &out_style,
            &screens::SetUp {
                terminals: &labels(&chosen),
                done: &done,
                notes,
            },
        )
    );
    Ok(ExitCode::SUCCESS)
}

/// A note when the user's bash or fish is one fac starts plain.
fn old_shell_note(kind: ShellKind, env: &Env, style: &Style) -> Option<String> {
    let shell = env
        .shell
        .as_deref()
        .filter(|shell| ShellKind::of(shell) == Some(kind))?;
    let cache = env.home.join("Library/Caches/fac");
    let version = |kind| guards::shell_version(shell, kind, Some(&cache));
    if let Err(guards::PlainReason::Unsupported {
        version: Some(found),
        why: guards::Unsupported::TooOld,
        ..
    }) = guards::gate(Some(kind), version)
        && let Some(minimum) = kind.minimum()
    {
        return Some(format!(
            "{} is {} {found}, and fac needs {minimum} or later: until it is upgraded, new windows start it without fac.",
            style.path(shell),
            kind.name()
        ));
    }
    None
}

/// `fac enable` and `fac disable`. `fac enable` also turns safe mode off.
pub fn enable(on: bool, args: &[String]) -> ExitCode {
    if let Some(code) = refuse_root(if on { "enable" } else { "disable" }) {
        return code;
    }
    if !args.is_empty() {
        let verb = if on { "enable" } else { "disable" };
        return usage(
            &format!("fac {verb} takes no arguments"),
            &format!("usage: fac {verb}"),
        );
    }
    let result = Env::from_process().and_then(|env| {
        let exe = std::env::current_exe()
            .map_err(|err| anyhow!("cannot find fac's own binary: {err}"))?;
        set_on(&env, on, None, &fac_path(env.path.as_deref(), &exe))
    });
    result.unwrap_or_else(|err| fail(&err))
}

/// Turn every block on or off (only `kind`'s files, when given), keeping
/// its terminals; the blocks start `fac`. Turning them on also turns safe
/// mode off.
fn set_on(env: &Env, on: bool, kind: Option<ShellKind>, fac: &Path) -> Result<ExitCode> {
    let mut changes = Vec::new();
    let mut found = Vec::new();
    for (shell, path) in all_files(env) {
        if kind.is_some_and(|kind| kind != shell) {
            continue;
        }
        let Some(current) = scan(&path) else {
            continue;
        };
        let Some(block) = block_in(&current)? else {
            continue;
        };
        found.push(block.terminals.clone());
        let block = Block { on, ..block };
        changes.extend(put_change(shell, current, &block, fac)?);
    }
    if found.is_empty() {
        show_problem(
            "fac is not set up to start with your terminal",
            Some("Run fac init to set it up."),
        );
        return Ok(ExitCode::FAILURE);
    }
    let done = apply(&changes, env)?;
    let no_start = env.no_start();
    let safe_mode_off = match on {
        true => clear_safe_mode(&no_start)
            .map_err(|err| anyhow!("cannot remove {}: {err}", no_start.display()))?,
        false => false,
    };
    let terminals = block::sorted(found.concat());
    let style = Style::detect(std::io::stdout().is_terminal());
    print!(
        "{}",
        screens::toggled(&style, on, &labels(&terminals), &done, safe_mode_off)
    );
    Ok(ExitCode::SUCCESS)
}

/// `fac uninstall`.
pub fn uninstall(args: &[String]) -> ExitCode {
    if let Some(code) = refuse_root("uninstall") {
        return code;
    }
    if !args.is_empty() {
        return usage("fac uninstall takes no arguments", "usage: fac uninstall");
    }
    let result = Env::from_process().and_then(|env| run_uninstall(&env));
    result.unwrap_or_else(|err| fail(&err))
}

/// The change that takes the block out of a file read, if it has one. A
/// block whose state line was edited or cannot be read is taken out too,
/// as one in a file fac did not create.
fn remove_change(kind: ShellKind, current: Current) -> Result<Option<Change>> {
    let Some(bytes) = &current.bytes else {
        return Ok(None);
    };
    let in_file = |err: anyhow::Error| anyhow!("{}: {err}", current.path.display());
    if block::find(bytes).map_err(in_file)?.is_empty() {
        return Ok(None);
    }
    let created = block::read(bytes).ok().flatten().is_some_and(|b| b.created);
    let rest = block::strip(bytes).map_err(in_file)?;
    // fish's file goes only when it is fac's (its header line); a file fac
    // created goes once nothing else is left, unless it is behind a link.
    let ours = match kind {
        ShellKind::Fish => block::starts_with_block(bytes),
        _ => created,
    };
    let backup = (!created).then(|| rest.clone());
    let after = if ours && rest.is_empty() && !current.link {
        After::Delete
    } else {
        After::Write(rest)
    };
    Ok(Some(Change {
        backup,
        after,
        current,
    }))
}

fn run_uninstall(env: &Env) -> Result<ExitCode> {
    let mut changes = Vec::new();
    for (kind, path) in all_files(env) {
        if let Some(current) = scan(&path) {
            changes.extend(remove_change(kind, current)?);
        }
    }
    let done = apply(&changes, env)?;
    let backups: Vec<PathBuf> = all_files(env)
        .into_iter()
        .map(|(_, path)| files::backup_path(&path))
        .filter(|path| path.exists())
        .collect();
    let folders: Vec<PathBuf> = KEPT.iter().map(|folder| env.home.join(folder)).collect();
    let style = Style::detect(std::io::stdout().is_terminal());
    print!("{}", screens::removed(&style, &done, &backups, &folders));
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dir(PathBuf);

    impl Dir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "fac-init-{name}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("mkdir");
            Self(dir)
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn the_bash_files_for_each_combination() {
        let names = [".bash_profile", ".bash_login", ".profile"];
        for mask in 0..8u8 {
            let dir = Dir::new(&format!("bash-{mask}"));
            let present: Vec<&str> = names
                .iter()
                .enumerate()
                .filter(|(bit, _)| mask & (1 << bit) != 0)
                .map(|(_, name)| *name)
                .collect();
            for name in &present {
                std::fs::write(dir.0.join(name), "").expect("write");
            }
            let [login, rc] = bash_files(&dir.0);
            let expected = present.first().copied().unwrap_or(".bash_profile");
            assert_eq!(login, dir.0.join(expected), "{present:?}");
            assert_eq!(rc, dir.0.join(".bashrc"));
            // A .bash_profile is created only when none of the three exists.
            if login == dir.0.join(".bash_profile") {
                assert!(present.is_empty() || present[0] == ".bash_profile");
            }
        }
    }

    #[test]
    fn the_path_through_a_fake_cellar() {
        use std::os::unix::fs::symlink;
        let dir = Dir::new("cellar");
        let cellar = dir.0.join("Cellar/fac/0.2.0/bin");
        let bin = dir.0.join("bin");
        let other = dir.0.join("other");
        for folder in [&cellar, &bin, &other] {
            std::fs::create_dir_all(folder).expect("mkdir");
        }
        let exe = cellar.join("fac");
        std::fs::write(&exe, "").expect("write");
        symlink("../Cellar/fac/0.2.0/bin/fac", bin.join("fac")).expect("symlink");
        // Another fac first on PATH: not the running one.
        std::fs::write(other.join("fac"), "").expect("write");
        let path = std::env::join_paths([&other, &bin]).expect("PATH");
        assert_eq!(fac_path(Some(&path), &exe), bin.join("fac"));
        // Through a symlinked folder too.
        let linked = dir.0.join("linked-bin");
        symlink(&bin, &linked).expect("symlink");
        let path = std::env::join_paths([&linked]).expect("PATH");
        assert_eq!(fac_path(Some(&path), &exe), linked.join("fac"));
        // Not on PATH, or a relative entry: the running binary.
        let path = std::env::join_paths([&other]).expect("PATH");
        assert_eq!(fac_path(Some(&path), &exe), exe);
        assert_eq!(fac_path(Some(OsStr::new("bin")), &exe), exe);
        assert_eq!(fac_path(None, &exe), exe);
    }

    fn env(home: &Path) -> Env {
        Env {
            home: home.to_path_buf(),
            zdotdir: Some(home.join("zdotdir")),
            xdg_config_home: Some(home.join("xdg")),
            shell: Some(PathBuf::from("/bin/zsh")),
            ..Env::default()
        }
    }

    fn write_block(kind: ShellKind, path: &Path, block: &Block) -> Vec<Done> {
        let current = files::read(path).expect("read");
        let change =
            put_change(kind, current, block, Path::new("/opt/homebrew/bin/fac")).expect("change");
        files::apply(&change.into_iter().collect::<Vec<_>>()).expect("apply")
    }

    fn on(terminals: &[TermKind]) -> Block {
        Block {
            on: true,
            terminals: terminals.to_vec(),
            created: false,
        }
    }

    #[test]
    fn init_disable_enable_and_uninstall_round_trip() {
        let dir = Dir::new("round-trip");
        let env = env(&dir.0);
        std::fs::create_dir_all(dir.0.join("zdotdir")).expect("mkdir");
        let zshrc = dir.0.join("zdotdir/.zshrc");
        let original = b"# Q pre block. Keep at the top of this file.\nexport A=1";
        std::fs::write(&zshrc, original).expect("write");
        let done = write_block(ShellKind::Zsh, &zshrc, &on(&[TermKind::Apple]));
        assert_eq!(done[0].backup, Some(files::backup_path(&zshrc)));
        let first = std::fs::read(&zshrc).expect("read");
        assert!(first.ends_with(original));
        // Again with other terminals: one block, replaced.
        write_block(
            ShellKind::Zsh,
            &zshrc,
            &on(&[TermKind::Apple, TermKind::Ghostty]),
        );
        let text = std::fs::read(&zshrc).expect("read");
        assert_eq!(block::find(&text).expect("find").len(), 1);
        assert_eq!(
            block::read(&text).expect("read"),
            Some(on(&[TermKind::Apple, TermKind::Ghostty]))
        );
        // Off and on again keep the terminals.
        let fac = Path::new("/opt/homebrew/bin/fac");
        set_on(&env, false, None, fac).expect("disable");
        let off = block::read(&std::fs::read(&zshrc).expect("read")).expect("read");
        assert_eq!(
            off,
            Some(Block {
                on: false,
                ..on(&[TermKind::Apple, TermKind::Ghostty])
            })
        );
        set_on(&env, true, None, fac).expect("enable");
        assert_eq!(std::fs::read(&zshrc).expect("read"), text);
        // Uninstall: byte for byte as before.
        run_uninstall(&env).expect("uninstall");
        assert_eq!(std::fs::read(&zshrc).expect("read"), original);
    }

    #[test]
    fn a_created_file_is_deleted_by_uninstall_only_when_empty() {
        let dir = Dir::new("created");
        let env = env(&dir.0);
        let [login, rc] = bash_files(&dir.0);
        for path in [&login, &rc] {
            write_block(ShellKind::Bash, path, &on(&KNOWN));
            assert!(path.exists());
            assert!(!files::backup_path(path).exists());
        }
        // The user adds a line to .bashrc after the block.
        let mut text = std::fs::read(&rc).expect("read");
        text.extend_from_slice(b"alias ll='ls -l'\n");
        std::fs::write(&rc, &text).expect("write");
        run_uninstall(&env).expect("uninstall");
        assert!(!login.exists());
        assert_eq!(std::fs::read(&rc).expect("read"), b"alias ll='ls -l'\n");
        // An empty file that existed stays, empty.
        std::fs::write(&login, "").expect("write");
        write_block(ShellKind::Bash, &login, &on(&KNOWN));
        run_uninstall(&env).expect("uninstall");
        assert_eq!(std::fs::read(&login).expect("read"), b"");
    }

    #[test]
    fn fish_has_its_own_file() {
        let dir = Dir::new("fish");
        let env = env(&dir.0);
        let path = files_for(ShellKind::Fish, &env).remove(0);
        assert_eq!(path, dir.0.join("xdg/fish/conf.d/00-fac.fish"));
        std::fs::create_dir_all(path.parent().expect("dir")).expect("mkdir");
        write_block(ShellKind::Fish, &path, &on(&[TermKind::Zed]));
        assert!(block::starts_with_block(
            &std::fs::read(&path).expect("read")
        ));
        run_uninstall(&env).expect("uninstall");
        assert!(!path.exists());
        // Someone else's fac.fish is left alone.
        std::fs::write(&path, "set -g mine 1\n").expect("write");
        let current = files::read(&path).expect("read");
        assert!(put_change(ShellKind::Fish, current, &on(&KNOWN), Path::new("/f")).is_err());
        run_uninstall(&env).expect("uninstall");
        assert_eq!(std::fs::read(&path).expect("read"), b"set -g mine 1\n");
    }

    #[test]
    fn zsh_dir_sees_through_facs_shim() {
        let dir = Dir::new("shim");
        let shim = dir.0.join("shim");
        std::fs::create_dir_all(&shim).expect("mkdir");
        std::fs::write(
            shim.join(".zshenv"),
            include_str!("../../shell/zsh/.zshenv"),
        )
        .expect("write");
        let mut env = env(&dir.0);
        env.zdotdir = Some(shim.clone());
        assert_eq!(env.zsh_dir(), dir.0);
        env.fac_user_zdotdir = Some(dir.0.join("mine"));
        assert_eq!(env.zsh_dir(), dir.0.join("mine"));
        env.zdotdir = Some(dir.0.join("zdotdir"));
        assert_eq!(env.zsh_dir(), dir.0.join("zdotdir"));
        env.zdotdir = None;
        assert_eq!(env.zsh_dir(), dir.0);
    }

    /// VS Code's zsh integration: `ZDOTDIR` is its temp folder and
    /// `USER_ZDOTDIR` the user's own, which wins; fac's shim still comes
    /// first.
    #[test]
    fn zsh_dir_inside_vscode() {
        let dir = Dir::new("vscode");
        let mut env = env(&dir.0);
        env.zdotdir = Some(PathBuf::from("/private/var/folders/xx/T/me-code-zsh"));
        env.user_zdotdir = Some(dir.0.join("mine"));
        assert_eq!(env.zsh_dir(), dir.0.join("mine"));
        let shim = dir.0.join("shim");
        std::fs::create_dir_all(&shim).expect("mkdir");
        std::fs::write(
            shim.join(".zshenv"),
            include_str!("../../shell/zsh/.zshenv"),
        )
        .expect("write");
        env.zdotdir = Some(shim);
        assert_eq!(env.zsh_dir(), dir.0.join("mine"));
        env.fac_user_zdotdir = Some(dir.0.join("fac-kept"));
        assert_eq!(env.zsh_dir(), dir.0.join("fac-kept"));
    }

    /// Nothing is written under a temporary folder: init stops before
    /// anything is read or written, and the message names the file.
    #[test]
    fn a_temp_folder_is_refused() {
        let dir = Dir::new("temp");
        let mut env = env(&dir.0);
        env.temp_roots = vec![dir.0.join("zdotdir")];
        std::fs::create_dir_all(dir.0.join("zdotdir")).expect("mkdir");
        let fac = Path::new("/opt/homebrew/bin/fac");
        let err = run_init(ShellKind::Zsh, &env, Some(KNOWN.to_vec()), fac).expect_err("temp");
        assert!(err.to_string().contains("temporary folder"), "{err:#}");
        assert!(err.to_string().contains(".zshrc"), "{err:#}");
        assert!(!dir.0.join("zdotdir/.zshrc").exists());
        // Through a symlink to it too.
        std::os::unix::fs::symlink(dir.0.join("zdotdir"), dir.0.join("link")).expect("symlink");
        assert!(temp_root(&dir.0.join("link/.zshrc"), &env.temp_roots).is_some());
        assert!(temp_root(&dir.0.join(".zshrc"), &env.temp_roots).is_none());
        // The real temp folders, as fac sees them.
        let roots = Env::from_process().expect("env").temp_roots;
        for path in [
            "/tmp/x/.zshrc",
            "/private/var/folders/ab/T/me-code-zsh/.zshrc",
        ] {
            assert!(temp_root(Path::new(path), &roots).is_some(), "{path}");
        }
    }

    #[test]
    fn root_is_refused() {
        let found = root_refusal(0, "init").expect("refused");
        let hint = found.hint.expect("a hint");
        assert!(hint.contains("not as root"), "{hint}");
        assert!(hint.contains("sudo keeps HOME"), "{hint}");
        assert_eq!(root_refusal(501, "init"), None);
    }

    /// `.zprofile` gets the block too when it exists, never created; the
    /// uninstall leaves it as it was.
    #[test]
    fn zprofile_gets_the_block_when_it_exists() {
        let dir = Dir::new("zprofile");
        let env = env(&dir.0);
        let fac = Path::new("/opt/homebrew/bin/fac");
        let zd = dir.0.join("zdotdir");
        std::fs::create_dir_all(&zd).expect("mkdir");
        assert_eq!(files_for(ShellKind::Zsh, &env), [zd.join(".zshrc")]);
        run_init(ShellKind::Zsh, &env, Some(KNOWN.to_vec()), fac).expect("init");
        assert!(!zd.join(".zprofile").exists());
        let original = b"eval \"$(/opt/homebrew/bin/brew shellenv)\"\nexec other\n";
        std::fs::write(zd.join(".zprofile"), original).expect("write");
        run_init(ShellKind::Zsh, &env, Some(KNOWN.to_vec()), fac).expect("init");
        let text = std::fs::read(zd.join(".zprofile")).expect("read");
        assert!(block::starts_with_block(&text));
        assert!(text.ends_with(original));
        run_uninstall(&env).expect("uninstall");
        assert_eq!(std::fs::read(zd.join(".zprofile")).expect("read"), original);
    }

    /// The backup is the file without fac's block, also when the file had
    /// a block before its first backup.
    #[test]
    fn the_backup_never_has_the_block() {
        let dir = Dir::new("backup");
        let fac = Path::new("/opt/homebrew/bin/fac");
        let zshrc = dir.0.join(".zshrc");
        let rest = b"export A=1\n";
        let old = block::render(ShellKind::Zsh, &on(&[TermKind::Apple]), fac).expect("render");
        std::fs::write(&zshrc, [old.as_bytes(), rest].concat()).expect("write");
        write_block(ShellKind::Zsh, &zshrc, &on(&KNOWN));
        assert_eq!(
            std::fs::read(files::backup_path(&zshrc)).expect("backup"),
            rest
        );
        // Uninstall's backup too.
        std::fs::remove_file(files::backup_path(&zshrc)).expect("rm");
        let current = files::read(&zshrc).expect("read");
        let change = remove_change(ShellKind::Zsh, current).expect("change");
        files::apply(&change.into_iter().collect::<Vec<_>>()).expect("apply");
        assert_eq!(std::fs::read(&zshrc).expect("read"), rest);
        assert_eq!(
            std::fs::read(files::backup_path(&zshrc)).expect("backup"),
            rest
        );
    }

    /// uninstall takes out a block whose state line was edited or cannot
    /// be read, as one in a file fac did not create.
    #[test]
    fn uninstall_strips_a_block_with_a_bad_state_line() {
        let fac = Path::new("/opt/homebrew/bin/fac");
        for state in ["# fac: maybe", "# fac: on warp", "# edited by hand"] {
            let dir = Dir::new("bad-state");
            let env = env(&dir.0);
            let zshrc = dir.0.join(".zshrc");
            let created = Block {
                created: true,
                ..on(&[TermKind::Apple])
            };
            let good = block::render(ShellKind::Zsh, &created, fac).expect("render");
            let bad = good.replace("# fac: on terminal; fac created this file", state);
            assert_ne!(bad, good);
            std::fs::write(&zshrc, &bad).expect("write");
            assert!(block::read(bad.as_bytes()).is_err(), "{state}");
            run_uninstall(&env).expect("uninstall");
            // Not deleted: created is taken as false.
            assert_eq!(std::fs::read(&zshrc).expect("read"), b"", "{state}");
        }
    }

    /// Files fac never touched block nothing: an unreadable one and a
    /// symlink to nothing, with no block, are skipped by uninstall,
    /// disable and enable.
    #[test]
    fn untouched_files_block_nothing() {
        use std::os::unix::fs::PermissionsExt;
        let dir = Dir::new("untouched");
        let env = env(&dir.0);
        let fac = Path::new("/opt/homebrew/bin/fac");
        std::fs::create_dir_all(dir.0.join("zdotdir")).expect("mkdir");
        run_init(ShellKind::Zsh, &env, Some(KNOWN.to_vec()), fac).expect("init");
        let locked = dir.0.join(".profile");
        std::fs::write(&locked, "secret\n").expect("write");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).expect("chmod");
        std::os::unix::fs::symlink(dir.0.join("gone"), dir.0.join(".bashrc")).expect("symlink");
        assert_eq!(
            set_on(&env, false, None, fac).expect("disable"),
            ExitCode::SUCCESS
        );
        assert_eq!(
            set_on(&env, true, None, fac).expect("enable"),
            ExitCode::SUCCESS
        );
        assert_eq!(run_uninstall(&env).expect("uninstall"), ExitCode::SUCCESS);
        assert!(!dir.0.join("zdotdir/.zshrc").exists());
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o600)).expect("chmod");
        assert_eq!(std::fs::read(&locked).expect("read"), b"secret\n");
        // A file without the marker bytes is never parsed.
        assert!(!block::has_marker(b"# >>> conda >>>\n"));
        assert!(block::has_marker(block::END.as_bytes()));
    }

    /// fish's file under its old name goes when fac init writes the new
    /// one, and with fac uninstall.
    #[test]
    fn fishs_old_file_goes() {
        let dir = Dir::new("old-fish");
        let env = env(&dir.0);
        let fac = Path::new("/opt/homebrew/bin/fac");
        let old = dir.0.join("xdg").join(OLD_FISH_FILE);
        std::fs::create_dir_all(old.parent().expect("dir")).expect("mkdir");
        let text = block::render(ShellKind::Fish, &on(&KNOWN), fac).expect("render");
        std::fs::write(&old, &text).expect("write");
        run_init(ShellKind::Fish, &env, Some(KNOWN.to_vec()), fac).expect("init");
        assert!(!old.exists());
        assert!(dir.0.join("xdg").join(FISH_FILE).exists());
        std::fs::write(&old, &text).expect("write");
        run_uninstall(&env).expect("uninstall");
        assert!(!old.exists());
        assert!(!dir.0.join("xdg").join(FISH_FILE).exists());
    }

    #[test]
    fn shells_and_terminals_from_the_command_line() {
        let env = Env {
            shell: Some(PathBuf::from("/opt/homebrew/bin/fish")),
            ..Env::default()
        };
        assert_eq!(shell_kind(None, &env), Ok(ShellKind::Fish));
        assert_eq!(shell_kind(Some("bash"), &env), Ok(ShellKind::Bash));
        assert!(shell_kind(Some("/bin/bash"), &env).is_err());
        assert!(shell_kind(Some("nu"), &env).is_err());
        assert!(shell_kind(None, &Env::default()).is_err());
        let sh = Env {
            shell: Some(PathBuf::from("/bin/sh")),
            ..Env::default()
        };
        assert!(shell_kind(None, &sh).is_err());
        assert_eq!(parse_terminals("all"), Ok(KNOWN.to_vec()));
        assert_eq!(
            parse_terminals("zed,terminal"),
            Ok(vec![TermKind::Apple, TermKind::Zed])
        );
        assert!(parse_terminals("warp").is_err());
        assert!(parse_terminals("").is_err());
        assert_eq!(style::join_words(&["a", "b", "c"]), "a, b and c");
    }

    /// Nothing ticked is `fac disable` for the shell: the block stays, off,
    /// with its terminals; with no block, nothing is written.
    #[test]
    fn unticking_everything_disables() {
        let dir = Dir::new("untick");
        let env = env(&dir.0);
        let fac = Path::new("/opt/homebrew/bin/fac");
        let zshrc = dir.0.join("zdotdir/.zshrc");
        std::fs::create_dir_all(dir.0.join("zdotdir")).expect("mkdir");
        run_init(ShellKind::Zsh, &env, Some(Vec::new()), fac).expect("init");
        assert!(!zshrc.exists());
        let chosen = vec![TermKind::Apple, TermKind::Zed];
        run_init(ShellKind::Zsh, &env, Some(chosen.clone()), fac).expect("init");
        run_init(ShellKind::Zsh, &env, Some(Vec::new()), fac).expect("init");
        let block = block::read(&std::fs::read(&zshrc).expect("read")).expect("read");
        assert_eq!(
            block,
            Some(Block {
                on: false,
                terminals: chosen,
                created: true,
            })
        );
    }

    #[test]
    fn kiro_is_seen() {
        assert!(mentions_kiro(
            b"# Q pre block. Keep at the top of this file.\n"
        ));
        assert!(mentions_kiro(b"# Added by Kiro CLI\n"));
        assert!(!mentions_kiro(b"export Q=1\n"));
    }

    #[test]
    fn the_real_home_is_known() {
        assert!(passwd_home().is_some_and(|home| home.is_absolute()));
    }
}
