//! `fac doctor`: a health check of fac's setup, shown in the look of `fac
//! init` ([`super::style`]): a bold title, sections of `✓`, `!` and `✗`
//! lines with dim details under them. It exits 1 when any line is a `✗`.
//!
//! It only reads, with one exception: when safe mode is on (the `no-start`
//! file, see [`super::start_failed`]) and nothing is a `✗`, it removes the
//! file, so new windows start fac again. It never starts a shell (a bash
//! or fish version not in fac's cache is read from `--version`, and not
//! written back), never edits a startup file and never uses the network.

use std::io::{IsTerminal, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::guards::{self, PlainReason, ShellKind, Unsupported};
use crate::specs::user;
use crate::term::ident::{self, TermKind};

use super::block::{self, Block};
use super::style::{Style, WIDTH, join_words, wrap};
use super::{Env, files, mentions_kiro};

/// How much of the log's end is read.
const LOG_TAIL: u64 = 64 * 1024;
/// How many warn and error lines of the log are shown.
const LOG_LINES: usize = 5;
/// The longest reason line read from safe mode's file.
const REASON_MAX: usize = 120;

/// What a line says about its subject.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    Ok,
    Warn,
    Fail,
}

/// One line of a section, with dim details under it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub mark: Mark,
    pub text: String,
    /// Wrapped to the width.
    pub details: Vec<String>,
    /// Shown as they are, one per line (log lines).
    pub raw: Vec<String>,
}

impl Item {
    fn new(mark: Mark, text: impl Into<String>) -> Self {
        Self {
            mark,
            text: text.into(),
            details: Vec::new(),
            raw: Vec::new(),
        }
    }

    fn ok(text: impl Into<String>) -> Self {
        Self::new(Mark::Ok, text)
    }

    fn warn(text: impl Into<String>) -> Self {
        Self::new(Mark::Warn, text)
    }

    fn fail(text: impl Into<String>) -> Self {
        Self::new(Mark::Fail, text)
    }

    fn detail(mut self, text: impl Into<String>) -> Self {
        self.details.push(text.into());
        self
    }
}

/// A titled list of lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub title: &'static str,
    pub items: Vec<Item>,
}

/// Everything `fac doctor` found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub sections: Vec<Section>,
}

impl Report {
    fn push(&mut self, title: &'static str, items: Vec<Item>) {
        self.sections.push(Section { title, items });
    }

    /// The `✗` lines so far.
    fn failures(&self) -> impl Iterator<Item = &Item> {
        self.sections
            .iter()
            .flat_map(|section| &section.items)
            .filter(|item| item.mark == Mark::Fail)
    }

    pub fn failed(&self) -> bool {
        self.failures().next().is_some()
    }
}

/// The report as the screen shows it: a blank line around it, a 2-space
/// margin, sections indented under their titles.
pub fn render(style: &Style, report: &Report) -> String {
    let mut lines = vec![
        String::new(),
        format!("  {}{}", style.bold_accent("fac"), style.bold(" doctor")),
    ];
    for section in &report.sections {
        lines.push(String::new());
        lines.push(format!("  {}", style.bold(section.title)));
        for item in &section.items {
            let mark = match item.mark {
                Mark::Ok => style.green(style.tick()),
                Mark::Warn => style.yellow(style.warn()),
                Mark::Fail => style.red(style.cross()),
            };
            for (n, line) in wrap(&style.tilde(&item.text), WIDTH, 6).iter().enumerate() {
                lines.push(match n {
                    0 => format!("    {mark} {line}"),
                    _ => format!("      {line}"),
                });
            }
            for detail in &item.details {
                for line in wrap(&style.tilde(detail), WIDTH, 6) {
                    lines.push(style.dim(&format!("      {line}")));
                }
            }
            for raw in &item.raw {
                lines.push(style.dim(&format!("      {raw}")));
            }
        }
    }
    lines.push(String::new());
    let failures = report.failures().count();
    lines.push(match failures {
        0 => format!(
            "  {} {}",
            style.green(style.tick()),
            style.bold("No problems found")
        ),
        n => format!(
            "  {} {}",
            style.red(style.cross()),
            style.bold(&format!(
                "{n} {} to fix: the {} lines above",
                if n == 1 { "problem" } else { "problems" },
                style.cross()
            ))
        ),
    });
    let mut out = String::new();
    for line in lines {
        out.push_str(&line);
        out.push('\n');
    }
    out.push('\n');
    out
}

/// `fac doctor`.
pub fn doctor(args: &[String]) -> ExitCode {
    if !args.is_empty() {
        return super::usage("fac doctor takes no arguments", "usage: fac doctor");
    }
    let env = match Env::from_process() {
        Ok(env) => env,
        Err(err) => return super::fail(&err),
    };
    let report = check(&env, &Process::read());
    let style = Style::detect(std::io::stdout().is_terminal());
    print!("{}", render(&style, &report));
    if report.failed() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// What the checks read from the process, besides [`Env`].
#[derive(Debug, Clone, Default)]
pub struct Process {
    /// The running binary.
    pub exe: Option<PathBuf>,
    /// `$SHELL` as set.
    pub shell_var: Option<std::ffi::OsString>,
    pub term_program: Option<String>,
    pub lc_terminal: Option<String>,
    pub zed_term: Option<String>,
    pub fac_active: Option<String>,
    /// The terminal this runs on (stdin, else stdout, else stderr).
    pub tty: Option<String>,
    pub tmux: bool,
    pub screen: bool,
    pub ssh: bool,
    pub fac_config: Option<std::ffi::OsString>,
    pub log_file: Option<PathBuf>,
}

impl Process {
    pub fn read() -> Self {
        let var = |name: &str| std::env::var(name).ok();
        let set = |name: &str| std::env::var_os(name).is_some_and(|value| !value.is_empty());
        let tty = [
            nix::unistd::ttyname(std::io::stdin()),
            nix::unistd::ttyname(std::io::stdout()),
            nix::unistd::ttyname(std::io::stderr()),
        ]
        .into_iter()
        .find_map(Result::ok)
        .map(|path| path.to_string_lossy().into_owned());
        Self {
            exe: std::env::current_exe().ok(),
            shell_var: std::env::var_os("SHELL"),
            term_program: var("TERM_PROGRAM"),
            lc_terminal: var("LC_TERMINAL"),
            zed_term: var("ZED_TERM"),
            fac_active: var("FAC_ACTIVE"),
            tty,
            tmux: set("TMUX"),
            screen: set("STY"),
            ssh: set("SSH_CONNECTION"),
            fac_config: std::env::var_os("FAC_CONFIG"),
            log_file: crate::logging::log_file_path().ok(),
        }
    }
}

/// A block found in a startup file.
struct Found {
    kind: ShellKind,
    path: PathBuf,
    block: Block,
    /// The fac its `exec` line starts.
    fac: Option<PathBuf>,
}

/// Run every check. Safe mode's file is removed here when nothing failed.
pub fn check(env: &Env, process: &Process) -> Report {
    let mut report = Report::default();
    report.push("fac", fac_section(process));
    report.push("Shell", shell_section(env, process));
    let (items, found) = startup_section(env, process);
    report.push("Startup (fac init)", items);
    report.push("This terminal", terminal_section(env, process, &found));
    report.push("Settings", settings_section(env, process));
    let safe = safe_mode_section(env, &report);
    report.push("Safe mode", safe);
    report.push("Log", log_section(process));
    report
}

fn fac_section(process: &Process) -> Vec<Item> {
    let item = Item::ok(format!("fac {}", env!("CARGO_PKG_VERSION")));
    vec![match &process.exe {
        Some(exe) => item.detail(exe.display().to_string()),
        None => item.detail("the path of the running fac is unknown"),
    }]
}

fn shell_section(env: &Env, process: &Process) -> Vec<Item> {
    let mut items = Vec::new();
    let (shell, is_fac) =
        guards::resolve_shell(process.shell_var.as_deref(), process.exe.as_deref());
    if process
        .shell_var
        .as_deref()
        .is_none_or(|value| value.is_empty())
    {
        items.push(Item::warn(format!(
            "SHELL is not set, so fac runs {}",
            guards::DEFAULT_SHELL
        )));
    } else if is_fac {
        items.push(Item::warn(format!(
            "SHELL names fac itself, so fac runs {}",
            guards::DEFAULT_SHELL
        )));
    }
    let shown = shell.display();
    let Some(kind) = ShellKind::of(&shell) else {
        items.push(
            Item::fail(format!("{shown}: fac runs zsh, bash and fish"))
                .detail("New windows start it without fac."),
        );
        return items;
    };
    let cache = env.home.join("Library/Caches/fac");
    let minimum = kind.minimum();
    // Read once, for the gate and the line; zsh's never.
    let found = minimum.and_then(|_| guards::shell_version_cached(&shell, kind, Some(&cache)));
    items.push(match (guards::gate(Some(kind), |_| found), minimum) {
        (Ok(()), None) => {
            Item::ok(format!("{} at {shown}", kind.name())).detail("fac runs every zsh version")
        }
        (Ok(()), Some(minimum)) => {
            let text = match found {
                Some(found) => format!("{} {found} at {shown}", kind.name()),
                None => format!("{} at {shown}", kind.name()),
            };
            Item::ok(text).detail(format!("fac runs {} {minimum} or later", kind.name()))
        }
        (
            Err(PlainReason::Unsupported {
                version: Some(found),
                why: Unsupported::TooOld,
                ..
            }),
            Some(minimum),
        ) => Item::fail(format!(
            "{} {found} at {shown} is too old: fac needs {minimum} or later",
            kind.name()
        ))
        .detail("New windows start it without fac until it is upgraded."),
        (Err(_), minimum) => Item::fail(format!(
            "cannot read the version of {} at {shown}{}",
            kind.name(),
            minimum.map_or(String::new(), |minimum| format!(
                ": fac needs {minimum} or later"
            ))
        ))
        .detail("New windows start it without fac."),
    });
    items
}

/// Whether `path` is a file this user can run.
fn executable(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|meta| meta.is_file())
        && nix::unistd::access(path, nix::unistd::AccessFlags::X_OK).is_ok()
}

fn startup_section(env: &Env, process: &Process) -> (Vec<Item>, Vec<Found>) {
    let mut items = Vec::new();
    let mut found = Vec::new();
    // Every file read, for Kiro.
    let mut read = Vec::new();
    for (kind, path) in super::all_files(env) {
        let dangling =
            std::fs::symlink_metadata(&path).is_ok() && std::fs::metadata(&path).is_err();
        if dangling {
            continue;
        }
        let current = match files::read(&path) {
            Ok(current) => current,
            Err(err) => {
                items.push(Item::warn(format!(
                    "{err:#}; fac cannot tell what is in it"
                )));
                continue;
            }
        };
        let Some(bytes) = current.bytes.clone() else {
            continue;
        };
        if block::has_marker(&bytes) {
            match block::read(&bytes) {
                Ok(Some(block)) => found.push(Found {
                    kind,
                    path: path.clone(),
                    fac: block::exec_path(kind, &bytes),
                    block,
                }),
                Ok(None) => {}
                Err(err) => items.push(
                    Item::warn(format!("{}: {err}", path.display()))
                        .detail("fac uninstall and then fac init write it again."),
                ),
            }
        }
        read.push((kind, path, bytes));
    }
    let this = process
        .exe
        .as_deref()
        .and_then(|exe| std::fs::canonicalize(exe).ok());
    for block in &found {
        let file = block.path.display();
        let terminals: Vec<&str> = block.block.terminals.iter().map(|t| t.label()).collect();
        let ticked = match terminals.is_empty() {
            true => "no terminal ticked".to_owned(),
            false => join_words(&terminals),
        };
        items.push(match block.block.on {
            true => Item::ok(format!("{file}: on in {ticked}")),
            false => {
                Item::warn(format!("{file}: off ({ticked})")).detail("fac enable turns it on.")
            }
        });
        match &block.fac {
            None => items.push(
                Item::fail(format!(
                    "{file}: fac cannot read which fac its block starts"
                ))
                .detail("Run fac init to write the block again."),
            ),
            Some(fac) if !fac.exists() => items.push(
                Item::fail(format!(
                    "{file} starts {}, which does not exist",
                    fac.display()
                ))
                .detail("Run fac init to point it at this fac."),
            ),
            Some(fac) if !executable(fac) => items.push(
                Item::fail(format!(
                    "{file} starts {}, which is not executable",
                    fac.display()
                ))
                .detail("Run fac init to point it at this fac."),
            ),
            Some(fac) if this.is_some() && std::fs::canonicalize(fac).ok() != this => items.push(
                Item::warn(format!("{file} starts another fac: {}", fac.display()))
                    .detail("Run fac init to point it at this one."),
            ),
            Some(_) => {}
        }
    }
    if found.is_empty() {
        items.push(Item::warn(
            "fac starts only when you run it; fac init sets it up",
        ));
    }
    let kiro: Vec<&(ShellKind, PathBuf, Vec<u8>)> = read
        .iter()
        .filter(|(_, _, bytes)| mentions_kiro(bytes))
        .collect();
    if !kiro.is_empty() {
        let on: Vec<ShellKind> = found
            .iter()
            .filter(|found| found.block.on)
            .map(|found| found.kind)
            .collect();
        // A file of a shell fac starts in, where Kiro's lines come first.
        let first: Option<&PathBuf> = kiro
            .iter()
            .filter(|(kind, path, _)| {
                on.contains(kind) && super::files_for(*kind, env).contains(path)
            })
            .find(|(_, _, bytes)| !block::starts_with_block(bytes))
            .map(|(_, path, _)| path);
        items.push(match (on.is_empty(), first) {
            (true, _) => Item::warn(
                "Kiro or Amazon Q is set up: it starts in new windows, and fac does not",
            ),
            (false, None) => Item::warn(
                "Kiro or Amazon Q is set up too: fac's block comes first, so fac starts instead of it in the ticked terminals",
            ),
            (false, Some(path)) => Item::warn(format!(
                "Kiro or Amazon Q is set up too, and starts before fac in {}",
                path.display()
            ))
            .detail("Run fac init to put fac's block first."),
        });
    }
    (items, found)
}

fn terminal_section(env: &Env, process: &Process, found: &[Found]) -> Vec<Item> {
    let mut items = Vec::new();
    if process.term_program.as_deref() == Some("WarpTerminal") {
        items.push(Item::warn(
            "Warp: fac stays off in Warp, which has its own autocomplete",
        ));
    } else {
        let kind = ident::detect(
            process.term_program.as_deref(),
            process.lc_terminal.as_deref(),
            process.zed_term.as_deref(),
        );
        let ticked_on = found
            .iter()
            .any(|found| found.block.on && found.block.terminals.contains(&kind));
        let ticked_off = found
            .iter()
            .any(|found| !found.block.on && found.block.terminals.contains(&kind));
        let label = kind.label();
        items.push(if kind == TermKind::Unknown {
            Item::warn(format!(
                "{label}: fac init can set up {}",
                join_words(&ident::KNOWN.map(TermKind::label))
            ))
        } else if ticked_on {
            Item::ok(format!("{label}, ticked"))
        } else if ticked_off {
            Item::warn(format!("{label} is ticked, but the block is off"))
                .detail("fac enable turns it on.")
        } else {
            Item::warn(format!("{label} is not ticked")).detail("fac init can tick it.")
        });
    }
    if guards::is_nested(process.fac_active.as_deref(), process.tty.as_deref()) {
        items.push(Item::ok("fac is running in this window"));
    } else {
        items.push(Item::warn("fac is not running in this window"));
    }
    if process.tmux {
        items.push(Item::warn("inside tmux: fac's block leaves fac off here"));
    }
    if process.screen {
        items.push(Item::warn("inside screen: fac's block leaves fac off here"));
    }
    if process.ssh {
        items.push(Item::warn("over SSH: fac's block leaves fac off here"));
    }
    if env.q_term {
        items.push(Item::warn(
            "inside Kiro or Amazon Q (Q_TERM is set): fac stays off here",
        ));
    }
    items
}

fn settings_section(env: &Env, process: &Process) -> Vec<Item> {
    let mut items = Vec::new();
    let path = crate::config::path(process.fac_config.as_deref(), Some(&env.home));
    let config = match &path {
        None => {
            items.push(Item::warn("the config file's path is unknown"));
            crate::config::Config::default()
        }
        Some(path) => match crate::config::read(path) {
            None => {
                items.push(Item::ok(format!("no {}, so the defaults", path.display())));
                crate::config::Config::default()
            }
            Some((config, warnings)) if warnings.is_empty() => {
                items.push(Item::ok(format!("{} loads", path.display())));
                config
            }
            Some((config, warnings)) => {
                let mut item = Item::warn(format!(
                    "{}: {} {}",
                    path.display(),
                    warnings.len(),
                    if warnings.len() == 1 {
                        "warning"
                    } else {
                        "warnings"
                    }
                ));
                item.details = warnings;
                items.push(item);
                config
            }
        },
    };
    let theme = &config.theme;
    items.push(match crate::popup::palette::resolve(theme) {
        (_, None) => Item::ok(format!("theme {theme}")),
        (_, Some(warning)) => Item::warn(warning),
    });
    items.extend(specs_items(&user::dir(&env.home)));
    items
}

/// The user's specs: how many, and a `✗` for each that does not load, as
/// `fac spec check` loads it.
fn specs_items(dir: &Path) -> Vec<Item> {
    let listed = user::list(dir);
    if listed.is_empty() {
        return vec![Item::ok("no specs of your own")];
    }
    let mut broken = Vec::new();
    for spec in &listed {
        let Some(found) = user::find(dir, &spec.name) else {
            continue;
        };
        let problem =
            match crate::specs::runtime::check_user_spec(&spec.name, &found.path, found.format) {
                Ok(_) => continue,
                Err(crate::specs::runtime::LoadError::User(err)) => err.to_string(),
                Err(err) => err.to_string(),
            };
        broken.push(
            Item::fail(format!("{} does not load", found.path.display()))
                .detail(problem)
                .detail(format!("fac spec check {} shows more.", spec.name)),
        );
    }
    let loaded = listed.len() - broken.len();
    let plural = |n: usize| if n == 1 { "spec" } else { "specs" };
    let mut items = Vec::new();
    if broken.is_empty() {
        items.push(Item::ok(format!(
            "{loaded} {} of your own, all load",
            plural(loaded)
        )));
    } else if loaded > 0 {
        items.push(Item::ok(format!(
            "{loaded} of your {} {} load",
            listed.len(),
            plural(listed.len())
        )));
    }
    items.extend(broken);
    items
}

/// Safe mode's file's first line, cleaned: no control characters, at most
/// [`REASON_MAX`] characters.
fn reason(text: &str) -> String {
    let line = text.lines().next().unwrap_or_default().trim();
    let clean: String = line
        .chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .take(REASON_MAX)
        .collect();
    clean
}

fn safe_mode_section(env: &Env, report: &Report) -> Vec<Item> {
    let path = env.no_start();
    let text = match std::fs::read(&path) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound && !path.exists() => {
            return vec![Item::ok("Safe mode is off")];
        }
        // There, but not readable (a folder, say): still safe mode.
        Err(_) => String::new(),
    };
    let reason = reason(&text);
    let mut items = vec![
        Item::warn(match reason.is_empty() {
            true => "Safe mode is on: new windows open plain".to_owned(),
            false => format!("Safe mode is on: {reason}"),
        })
        .detail(path.display().to_string()),
    ];
    match report.failures().next() {
        None => items.push(match super::clear_safe_mode(&path) {
            Ok(_) => Item::ok("Safe mode is off: new windows start fac again"),
            Err(err) => Item::fail(format!("cannot remove {}: {err}", path.display())),
        }),
        Some(first) => items.push(
            Item::warn("Safe mode stays on until the lines marked with a cross are fixed")
                .detail(format!("Fix this first: {}", first.text))
                .detail("Then run fac doctor again."),
        ),
    }
    items
}

/// Whether a log line is at warn or error level: its second word, as
/// fac's log writes it (`<time>  WARN <target>: ...`).
fn warn_or_error(line: &str) -> bool {
    matches!(line.split_whitespace().nth(1), Some("WARN" | "ERROR"))
}

/// The last [`LOG_LINES`] warn and error lines in the last [`LOG_TAIL`]
/// bytes of the log at `path`, with control characters replaced.
fn log_tail(path: &Path) -> std::io::Result<Vec<String>> {
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    let start = len.saturating_sub(LOG_TAIL);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    file.take(LOG_TAIL).read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes);
    let mut lines: Vec<&str> = text.lines().collect();
    if start > 0 && !lines.is_empty() {
        // The first line is cut.
        lines.remove(0);
    }
    let picked: Vec<String> = lines
        .into_iter()
        .filter(|line| warn_or_error(line))
        .map(|line| {
            line.chars()
                .map(|c| if c.is_control() { '?' } else { c })
                .collect()
        })
        .collect();
    let skip = picked.len().saturating_sub(LOG_LINES);
    Ok(picked.into_iter().skip(skip).collect())
}

fn log_section(process: &Process) -> Vec<Item> {
    let Some(path) = &process.log_file else {
        return vec![Item::warn("the log's path is unknown (HOME is not set)")];
    };
    let shown = path.display();
    if !path.exists() {
        return vec![Item::ok(format!("{shown}: no log yet"))];
    }
    match log_tail(path) {
        Ok(lines) if lines.is_empty() => {
            vec![Item::ok(format!("{shown}")).detail("no warnings or errors at its end")]
        }
        Ok(lines) => {
            let mut item = Item::warn(format!("{shown}: its last warnings and errors"));
            item.raw = lines;
            vec![item]
        }
        Err(err) => vec![Item::warn(format!("cannot read {shown}: {err}"))],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(text: &str) -> Option<std::ffi::OsString> {
        Some(std::ffi::OsString::from(text))
    }

    struct Dir(PathBuf);

    impl Dir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "fac-doctor-{name}-{}-{:?}",
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

    fn env(home: &Path) -> Env {
        Env {
            home: home.to_path_buf(),
            zdotdir: Some(home.to_path_buf()),
            xdg_config_home: Some(home.join(".config")),
            shell: Some(PathBuf::from("/bin/zsh")),
            ..Env::default()
        }
    }

    fn process(home: &Path, exe: &Path) -> Process {
        Process {
            exe: Some(exe.to_path_buf()),
            shell_var: os("/bin/zsh"),
            term_program: Some("ghostty".to_owned()),
            log_file: Some(home.join("fac.log")),
            ..Process::default()
        }
    }

    fn plain() -> Style {
        Style::new(false, true, None)
    }

    /// The report's text with its line breaks and indents as single
    /// spaces: long temporary paths wrap.
    fn words(report: &Report) -> String {
        render(&plain(), report)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// A fake fac binary in `home`, executable.
    fn fake_fac(home: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let exe = home.join("bin/fac");
        std::fs::create_dir_all(exe.parent().expect("dir")).expect("mkdir");
        std::fs::write(&exe, "#!/bin/sh\n").expect("write");
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        exe
    }

    fn init(env: &Env, fac: &Path) {
        super::super::run_init(ShellKind::Zsh, env, Some(ident::KNOWN.to_vec()), fac)
            .expect("init");
    }

    #[test]
    fn a_clean_setup_has_no_cross_and_clears_safe_mode() {
        let dir = Dir::new("clean");
        let env = env(&dir.0);
        let exe = fake_fac(&dir.0);
        init(&env, &exe);
        super::super::start_failed(&env.no_start(), "panic").expect("write");
        let report = check(&env, &process(&dir.0, &exe));
        let text = words(&report);
        assert!(!report.failed(), "{text}");
        assert!(text.contains("✓ Ghostty, ticked"), "{text}");
        assert!(
            text.contains("! Safe mode is on: fac failed to start: panic"),
            "{text}"
        );
        assert!(
            text.contains("✓ Safe mode is off: new windows start fac again"),
            "{text}"
        );
        assert!(text.contains("✓ No problems found"), "{text}");
        assert!(!env.no_start().exists());
    }

    #[test]
    fn a_missing_fac_is_a_cross_and_keeps_safe_mode() {
        let dir = Dir::new("missing");
        let env = env(&dir.0);
        let exe = fake_fac(&dir.0);
        init(&env, &dir.0.join("gone/fac"));
        super::super::start_failed(&env.no_start(), "panic").expect("write");
        let report = check(&env, &process(&dir.0, &exe));
        let text = words(&report);
        assert!(report.failed(), "{text}");
        assert!(text.contains("gone/fac, which does not exist"), "{text}");
        assert!(text.contains("Safe mode stays on"), "{text}");
        assert!(env.no_start().exists());
        // Another fac that exists: only a warning.
        let other = dir.0.join("other/fac");
        std::fs::create_dir_all(other.parent().expect("dir")).expect("mkdir");
        std::fs::copy(&exe, &other).expect("copy");
        init(&env, &other);
        let report = check(&env, &process(&dir.0, &exe));
        let text = words(&report);
        assert!(!report.failed(), "{text}");
        assert!(text.contains("starts another fac"), "{text}");
    }

    #[test]
    fn no_block_and_an_old_bash() {
        let dir = Dir::new("none");
        let env = env(&dir.0);
        let exe = fake_fac(&dir.0);
        let mut process = process(&dir.0, &exe);
        process.shell_var = os("/bin/bash");
        let report = check(&env, &process);
        let text = words(&report);
        assert!(text.contains("! fac starts only when you run it"), "{text}");
        // macOS's own bash is 3.2.
        assert!(
            text.contains("is too old: fac needs 5.3.0 or later"),
            "{text}"
        );
        assert!(report.failed());
        // The cache was not written.
        assert!(!dir.0.join("Library/Caches/fac/shell-versions").exists());
    }

    #[test]
    fn only_warn_and_error_lines_of_the_logs_end() {
        let dir = Dir::new("log");
        let log = dir.0.join("fac.log");
        let mut text = String::new();
        for n in 0..8 {
            text.push_str(&format!(
                "2026-11-02T10:00:0{n}.000000Z  INFO fac: info {n}\n"
            ));
            text.push_str(&format!(
                "2026-11-02T10:00:0{n}.000000Z  WARN fac: warn {n}\n"
            ));
        }
        text.push_str("2026-11-02T10:00:09.000000Z ERROR fac: proxy failed \x1b[2J\n");
        std::fs::write(&log, &text).expect("write");
        let lines = log_tail(&log).expect("tail");
        assert_eq!(lines.len(), LOG_LINES, "{lines:?}");
        assert!(lines[0].ends_with("warn 4"), "{lines:?}");
        assert!(lines[4].ends_with("proxy failed ?[2J"), "{lines:?}");
        assert!(lines.iter().all(|line| !line.contains("INFO")));
        // Only the end is read.
        let big = "x".repeat(200) + "\n";
        let mut long = String::from("2026-11-02T00:00:00.000000Z  WARN fac: old\n");
        for _ in 0..1000 {
            long.push_str(&big);
        }
        std::fs::write(&log, long).expect("write");
        assert!(log_tail(&log).expect("tail").is_empty());
    }

    #[test]
    fn the_reason_line_is_clean() {
        assert_eq!(
            reason("fac failed to start: panic\n"),
            "fac failed to start: panic"
        );
        assert_eq!(reason("a\x1bb\nsecond"), "a?b");
        assert_eq!(reason(&"y".repeat(500)).len(), REASON_MAX);
        assert_eq!(reason(""), "");
    }

    #[test]
    fn colour_only_with_a_colour_style() {
        let report = Report {
            sections: vec![Section {
                title: "fac",
                items: vec![Item::fail("x").detail("y")],
            }],
        };
        let plain = render(&Style::new(false, false, None), &report);
        assert!(!plain.contains('\x1b') && plain.is_ascii(), "{plain}");
        assert!(plain.contains("    x x\n      y\n"), "{plain}");
        let coloured = render(&Style::new(true, true, None), &report);
        assert!(coloured.contains("\x1b[31m✗\x1b[0m"), "{coloured}");
    }
}
