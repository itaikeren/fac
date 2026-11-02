use std::os::fd::AsFd;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use fac::complete::zsh_worker::WorkerSpec;
use fac::guards::{self, Decision, GuardInput, PlainReason, ShellKind};
use fac::proxy;
use fac::pty::{self, RawModeGuard};
use fac::shell::install::{self, CMD_FD, Launch, ShellSetup};
use fac::specs::user::{self, Format};
use nix::sys::signal::{Signal, kill};
use nix::sys::wait::waitpid;
use nix::unistd::{isatty, ttyname};
use tracing_appender::non_blocking::WorkerGuard;

const VERSION: &str = env!("CARGO_PKG_VERSION");

const HELP: &str = "fac: Fig-style autocomplete for the macOS terminal

Usage: fac [--help] [--version]
       fac init [--terminals <list> | all] [--shell zsh|bash|fish]
       fac enable | fac disable | fac uninstall
       fac doctor
       fac theme new <name> [--light] [--stdout]
       fac spec new <name> [--js | --json]
       fac spec check [<name> | <path>]
       fac spec list

Runs your shell ($SHELL: zsh, bash or fish) with autocomplete.

  init               make new windows of your terminals start fac: asks which
                     (Terminal.app, iTerm2, Ghostty, VS Code, Zed) and adds a
                     block to your shell's startup file (~/.zshrc and
                     ~/.zprofile; bash: your login file and ~/.bashrc; fish:
                     conf.d/00-fac.fish);
                     --terminals terminal,iterm2,ghostty,vscode,zed or all
                     sets them without asking, --shell picks the shell
  enable, disable    turn that block on or off; it keeps your choices
  uninstall          remove the block from every startup file it is in
  doctor             check fac's setup and say what is wrong; when fac failed
                     to start, new windows open plain (safe mode) until
                     fac doctor finds nothing wrong or fac enable runs
  theme new <name>   write ~/.config/fac/themes/<name>.json, a theme file with
                     fac's colours to start your own from (--light: for a light
                     terminal), and show how to use it
  theme new <name> --stdout   print the file instead of writing it
  spec new <name>    write ~/.config/fac/specs/<name>.ts, a starter completion
                     spec for the command <name> in Fig's format, with types for
                     your editor (--js: JavaScript, --json: JSON); it is live at
                     your next prompt
  spec check [<name> | <path>]   load a spec as fac would and say what is in
                     it or exactly what is wrong; with no argument, every spec in
                     ~/.config/fac/specs
  spec list          list your specs and the spec of fac's each one replaces

Environment:
  FAC_LOG        log filter, e.g. debug (default: info)
  FAC_LOG_FILE   log file path (default: ~/Library/Logs/fac/fac.log)
  FAC_CONFIG     config file path (default: ~/.config/fac/config.toml)
  FAC_DATA_DIR   where fac keeps usage.json (default: ~/Library/Application Support/fac)
  FAC_DISABLE    set to 1 to start your shell plain, without fac
  FAC_STATS      set to 1 to log the popup latency every 100 painted keystrokes";

/// Written to the terminal by the panic hook: end synchronized output, show the cursor.
const RESTORE: &[u8] = b"\x1b[?2026l\x1b[?25h";

/// Exit status when the passthrough core panics (EX_SOFTWARE).
const EXIT_PANIC: u8 = 70;

/// Safe mode's file (`fac::init::no_start_path`), read once at start for
/// the panic hook.
static NO_START: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Turn safe mode on: fac failed to start (or failed while proxying), so
/// new windows open plain until `fac doctor` or `fac enable`. `reason` is
/// a fixed class, never a user value. This is done for every failed start,
/// also of a fac run by hand: a window would close the same way.
fn start_failed(reason: &str) {
    let Some(path) = NO_START.get() else {
        return;
    };
    match fac::init::start_failed(path, reason) {
        Ok(()) => tracing::error!("safe mode is on ({reason}): new windows open plain"),
        Err(err) => tracing::error!("cannot turn safe mode on: {err}"),
    }
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("--version" | "-V") => {
            println!("fac {VERSION}");
            ExitCode::SUCCESS
        }
        Some("--help" | "-h") => {
            println!("{HELP}");
            ExitCode::SUCCESS
        }
        Some("theme") => theme_command(args.collect()),
        Some("spec") => spec_command(args.collect()),
        Some("init") => fac::init::init(&args.collect::<Vec<_>>()),
        Some("enable") => fac::init::enable(true, &args.collect::<Vec<_>>()),
        Some("disable") => fac::init::enable(false, &args.collect::<Vec<_>>()),
        Some("uninstall") => fac::init::uninstall(&args.collect::<Vec<_>>()),
        Some("doctor") => fac::init::doctor::doctor(&args.collect::<Vec<_>>()),
        Some(arg) => {
            eprintln!("fac: unknown argument '{arg}'");
            ExitCode::from(2)
        }
        None => run(),
    }
}

/// `fac theme new <name> [--light] [--stdout]`; it never starts the shell.
fn theme_command(args: Vec<String>) -> ExitCode {
    const USAGE: &str = "usage: fac theme new <name> [--light] [--stdout]";
    let [verb, rest @ ..] = args.as_slice() else {
        eprintln!("fac: {USAGE}");
        return ExitCode::from(2);
    };
    let mut name = None;
    let (mut light, mut stdout) = (false, false);
    for arg in rest {
        match arg.as_str() {
            "--light" => light = true,
            "--stdout" => stdout = true,
            other if !other.starts_with('-') && name.is_none() => name = Some(other),
            _ => name = None,
        }
    }
    let (true, Some(name)) = (verb == "new", name) else {
        eprintln!("fac: {USAGE}");
        return ExitCode::from(2);
    };
    if stdout {
        println!("{}", fac::popup::palette::print_theme(light));
        return ExitCode::SUCCESS;
    }
    match fac::popup::palette::create_theme_file(name, light) {
        Ok(path) => {
            println!("Wrote {}", path.display());
            println!("Edit its colours, then add this line to ~/.config/fac/config.toml:");
            println!("theme = \"{name}\"");
            ExitCode::SUCCESS
        }
        Err(problem) => {
            eprintln!("fac: {problem}");
            ExitCode::FAILURE
        }
    }
}

/// `fac spec new|check|list`; it never starts the shell.
fn spec_command(args: Vec<String>) -> ExitCode {
    const USAGE: &str = "usage: fac spec new <name> [--js | --json] | fac spec check [<name> | <path>] | fac spec list";
    let Some(dir) = std::env::var_os("HOME").map(|home| user::dir(Path::new(&home))) else {
        eprintln!("fac: HOME is not set, so there is no ~/.config/fac/specs");
        return ExitCode::FAILURE;
    };
    match args.split_first() {
        Some((verb, rest)) if verb == "new" => {
            let mut name = None;
            let mut format = Some(Format::Ts);
            for arg in rest {
                match arg.as_str() {
                    "--js" if format == Some(Format::Ts) => format = Some(Format::Js),
                    "--json" if format == Some(Format::Ts) => format = Some(Format::Json),
                    "--js" | "--json" => format = None,
                    other if !other.starts_with('-') && name.is_none() => name = Some(other),
                    _ => format = None,
                }
            }
            match (name, format) {
                (Some(name), Some(format)) => spec_new(&dir, name, format),
                _ => {
                    eprintln!("fac: {USAGE}");
                    ExitCode::from(2)
                }
            }
        }
        Some((verb, rest)) if verb == "check" && rest.len() <= 1 => match rest.first() {
            Some(target) => spec_check_one(&dir, target),
            None => spec_check_all(&dir),
        },
        Some((verb, rest)) if verb == "list" && rest.is_empty() => spec_list(&dir),
        _ => {
            eprintln!("fac: {USAGE}");
            ExitCode::from(2)
        }
    }
}

fn spec_new(dir: &Path, name: &str, format: Format) -> ExitCode {
    let created = match fac::specs::starter::create(dir, name, format) {
        Ok(created) => created,
        Err(problem) => {
            eprintln!("fac: {problem}");
            return ExitCode::FAILURE;
        }
    };
    println!("Wrote {}", created.path.display());
    let editor: Vec<&str> = [
        created.types.as_ref().map(|_| "fig.d.ts"),
        created.tsconfig.as_ref().map(|_| "tsconfig.json"),
    ]
    .into_iter()
    .flatten()
    .collect();
    if !editor.is_empty() {
        println!(
            "Also wrote {} next to it, so your editor completes and checks Fig.Spec.",
            editor.join(" and ")
        );
    }
    if format == Format::Json {
        println!(
            "JSON has no comments; the fields are Fig's: {}",
            fac::specs::starter::TYPES_URL
        );
    }
    let command = name.rsplit('/').next().unwrap_or(name);
    println!("It is live at your next prompt: type `{command} ` to see it.");
    println!("If something looks wrong: fac spec check {name}");
    ExitCode::SUCCESS
}

/// `fac spec check <name | path>`.
fn spec_check_one(dir: &Path, target: &str) -> ExitCode {
    // A file (`./mytool.ts`, or `mytool.ts` in the spec folder), else a name.
    let in_dir = dir.join(target);
    let path = if Path::new(target).is_file() {
        Path::new(target)
    } else {
        in_dir.as_path()
    };
    let file_format = Format::of(path).filter(|_| path.is_file());
    let (name, found) = if let Some(format) = file_format {
        // Inside the folder a nested name keeps its folder; elsewhere the
        // file's stem is the command.
        let canonical = std::fs::canonicalize(path).ok();
        let relative = canonical
            .clone()
            .zip(std::fs::canonicalize(dir).ok())
            .and_then(|(file, dir)| file.strip_prefix(dir).ok().map(Path::to_path_buf));
        let inside = relative.is_some();
        let name = relative
            .unwrap_or_else(|| PathBuf::from(path.file_name().unwrap_or_default()))
            .with_extension("")
            .to_string_lossy()
            .into_owned();
        // In the folder, the file fac would pick for the name may be another.
        let mut unused = Vec::new();
        if inside && let Some(picked) = user::find(dir, &name) {
            if std::fs::canonicalize(&picked.path).ok() == canonical {
                unused = picked.unused;
            } else {
                println!(
                    "warning: fac uses {} for {name}, not this file",
                    picked.path.display()
                );
            }
        }
        let found = user::Found {
            path: path.to_path_buf(),
            format,
            unused,
        };
        (name, found)
    } else {
        match user::find(dir, target) {
            Some(found) => (target.to_owned(), found),
            None => {
                eprintln!(
                    "fac: no spec named {target:?} in {} (looked for {target}.ts, {target}.js and {target}.json)",
                    dir.display()
                );
                return ExitCode::FAILURE;
            }
        }
    };
    if check_found(&name, &found) {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// `fac spec check` with no argument: every spec in the folder.
fn spec_check_all(dir: &Path) -> ExitCode {
    let listed = user::list(dir);
    if listed.is_empty() {
        println!(
            "No specs in {}. Start one with: fac spec new <command>",
            dir.display()
        );
        return ExitCode::SUCCESS;
    }
    let mut failed = 0;
    for (index, spec) in listed.iter().enumerate() {
        if index > 0 {
            println!();
        }
        let Some(found) = user::find(dir, &spec.name) else {
            continue;
        };
        if !check_found(&spec.name, &found) {
            failed += 1;
        }
    }
    if failed == 0 {
        ExitCode::SUCCESS
    } else {
        eprintln!("fac: {failed} of {} specs did not load", listed.len());
        ExitCode::FAILURE
    }
}

/// Load one spec file as fac would and print what it holds or what is
/// wrong with it; true when it loads.
fn check_found(name: &str, found: &user::Found) -> bool {
    let shown = found.path.display();
    let (spec, unknown) =
        match fac::specs::runtime::check_user_spec(name, &found.path, found.format) {
            Ok(loaded) => loaded,
            Err(fac::specs::runtime::LoadError::User(err)) => {
                match err.at {
                    Some((line, column)) => {
                        eprintln!("fac: {shown}:{line}:{column}: {}", err.detail)
                    }
                    None => eprintln!("fac: {shown}: {}", err.detail),
                }
                return false;
            }
            Err(err) => {
                eprintln!("fac: {shown}: {err}");
                return false;
            }
        };
    let summary = user::summary(&spec.root);
    println!("{shown} ({}): loads", found.format.name());
    let replaces = fac::specs::archive::Archive::embedded()
        .kind(name)
        .is_some();
    if replaces {
        println!("  replaces fac's own spec for {name}");
    } else {
        println!("  adds a spec for {name} (fac has none)");
    }
    let plural = |count: usize, one: &str, many: &str| {
        format!("{count} {}", if count == 1 { one } else { many })
    };
    println!(
        "  {}, {}, {}, {} ({} {} JavaScript)",
        plural(summary.subcommands, "subcommand", "subcommands"),
        plural(summary.options, "option", "options"),
        plural(summary.args, "arg", "args"),
        plural(summary.generators, "generator", "generators"),
        summary.js_generators,
        if summary.js_generators == 1 {
            "runs"
        } else {
            "run"
        },
    );
    for other in &found.unused {
        println!(
            "warning: {} is not used: {} comes first",
            other.display(),
            found.path.display()
        );
    }
    for key in unknown {
        println!("warning: unknown key {key}: fac ignores it");
    }
    for path in &summary.silent {
        println!(
            "warning: {path} runs a script but has no splitOn or postProcess, so it suggests nothing"
        );
    }
    for (path, what) in &summary.commands {
        println!("warning: {path}: {what}");
    }
    true
}

/// `fac spec list`: one line per spec.
fn spec_list(dir: &Path) -> ExitCode {
    let listed = user::list(dir);
    if listed.is_empty() {
        println!(
            "No specs in {}. Start one with: fac spec new <command>",
            dir.display()
        );
        return ExitCode::SUCCESS;
    }
    let archive = fac::specs::archive::Archive::embedded();
    for spec in listed {
        let Some((used, others)) = spec.formats.split_first() else {
            continue;
        };
        let file = format!("{}.{}", spec.name, used.extension());
        let what = if archive.kind(&spec.name).is_some() {
            format!("replaces fac's {} spec", spec.name)
        } else {
            format!("adds {} (fac has no spec for it)", spec.name)
        };
        let unused: Vec<String> = others
            .iter()
            .map(|other| format!("{}.{}", spec.name, other.extension()))
            .collect();
        if unused.is_empty() {
            println!("{file}  {what}");
        } else {
            println!("{file}  {what}; {} not used", unused.join(" and "));
        }
    }
    ExitCode::SUCCESS
}

fn run() -> ExitCode {
    if let Some(path) = fac::init::no_start_path() {
        let _ = NO_START.set(path);
    }
    install_panic_hook();
    // Without logs fac still runs; there is nowhere better to report it.
    let log_guard = fac::logging::init().ok();
    fac::config::init(fac::config::load());
    tracing::info!("fac {VERSION} started");

    let current_exe = std::env::current_exe().ok();
    let (shell, shell_is_fac) =
        guards::resolve_shell(std::env::var_os("SHELL").as_deref(), current_exe.as_deref());
    if shell_is_fac {
        tracing::info!("SHELL names fac itself; using {}", guards::DEFAULT_SHELL);
    }
    let stdin = std::io::stdin();
    let stdin_is_tty = isatty(stdin.as_fd()).unwrap_or(false);
    let stdout_is_tty = isatty(std::io::stdout().as_fd()).unwrap_or(false);
    let stdin_tty = ttyname(stdin.as_fd())
        .ok()
        .map(|path| path.to_string_lossy().into_owned());
    let var = |name: &str| std::env::var(name).ok();
    let (fac_disable, term_program, q_term, fac_active) = (
        var("FAC_DISABLE"),
        var("TERM_PROGRAM"),
        var("Q_TERM"),
        var("FAC_ACTIVE"),
    );
    let cache_dir = std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(|home| Path::new(&home).join("Library/Caches/fac"));
    let shell_version = |kind| guards::shell_version(&shell, kind, cache_dir.as_deref());
    let decision = guards::decide(&GuardInput {
        stdin_is_tty,
        stdout_is_tty,
        stdin_tty: stdin_tty.as_deref(),
        shell: &shell,
        fac_disable: fac_disable.as_deref(),
        term_program: term_program.as_deref(),
        q_term: q_term.as_deref(),
        fac_active: fac_active.as_deref(),
        shell_version: &shell_version,
    });
    if let Decision::Plain(reason) = decision {
        tracing::info!("decision=plain reason={reason:?}");
        return plain_shell(&shell, Some(&reason), log_guard);
    }
    // `decide` runs only zsh, bash and fish.
    let Some(kind) = ShellKind::of(&shell) else {
        return plain_shell(&shell, None, log_guard);
    };
    tracing::info!("decision=run shell={}", kind.name());
    // Tests only: a panic at start, to test safe mode.
    #[cfg(debug_assertions)]
    if std::env::var_os("FAC_TEST_PANIC_AT_START").is_some() {
        panic!("FAC_TEST_PANIC_AT_START");
    }

    // Without the integration fac still passes everything through.
    let (integration_env, inherit_fds, cmd_writer, token, worker, launch) =
        match install::prepare(kind, &shell) {
            Ok(ShellSetup {
                script_dir,
                env,
                cmd_writer,
                cmd_reader,
                token,
                launch,
            }) => (
                env,
                vec![(cmd_reader, CMD_FD)],
                Some(cmd_writer),
                Some(token),
                Some(WorkerSpec::new(kind, &shell, &script_dir)),
                launch,
            ),
            Err(err) => {
                tracing::warn!("shell integration unavailable: {err:#}; passthrough only");
                (
                    Vec::new(),
                    Vec::new(),
                    None,
                    None,
                    None,
                    Launch::login(&shell),
                )
            }
        };

    let size = pty::window_size(stdin.as_fd());
    let fac_pid = std::process::id();
    let shell_process = match pty::spawn_shell(
        &shell,
        &launch,
        |slave_tty| {
            let mut env = guards::child_env(fac_pid, slave_tty);
            env.push((guards::SHELL_MARK.into(), guards::SHELL_MARK_RUN.into()));
            env.extend(integration_env);
            env
        },
        size,
        inherit_fds,
    ) {
        Ok(shell_process) => shell_process,
        Err(err) => {
            tracing::error!("cannot start the shell in a PTY: {err:#}");
            return plain_shell(&shell, None, log_guard);
        }
    };
    let raw_mode = match RawModeGuard::enable() {
        Ok(guard) => guard,
        Err(err) => {
            tracing::error!("cannot enable raw mode: {err:#}");
            let _ = kill(shell_process.pid, Signal::SIGKILL);
            let _ = waitpid(shell_process.pid, None);
            return plain_shell(&shell, None, log_guard);
        }
    };

    let result = catch_unwind(AssertUnwindSafe(|| {
        proxy::run(shell_process, kind, cmd_writer, token, worker)
    }));
    drop(raw_mode);
    let code = match result {
        Ok(Ok(status)) => u8::try_from(status).unwrap_or(1),
        Ok(Err(err)) => {
            tracing::error!("proxy failed: {err:#}");
            start_failed("proxy error");
            1
        }
        Err(_) => {
            tracing::error!("the passthrough core panicked; exiting");
            EXIT_PANIC
        }
    };
    drop(log_guard);
    ExitCode::from(code)
}

/// Exec the plain login shell in place of fac. The log guard is dropped first
/// so buffered log lines reach the file before exec replaces the process; an
/// exec failure therefore goes to stderr.
fn plain_shell(
    shell: &Path,
    reason: Option<&PlainReason>,
    log_guard: Option<WorkerGuard>,
) -> ExitCode {
    drop(log_guard);
    let err = match reason {
        Some(reason) => guards::exec_plain_shell(shell, reason),
        None => guards::exec_login_shell(shell),
    };
    eprintln!("fac: {err:#}");
    // No shell started: the window would close. The log is closed, so
    // this goes only to the file.
    if let Some(path) = NO_START.get() {
        let _ = fac::init::start_failed(path, "shell exec error");
    }
    ExitCode::FAILURE
}

/// Restore the terminal on panic without taking locks, then log. The default
/// hook is not called: it would print over the user's terminal. The payload
/// may quote what the user typed, so it is logged at debug only. A panic
/// that ends fac (on the main thread, not in a handler that catches it)
/// turns safe mode on.
fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        // A panic on the spec thread touches neither; see `panic_plan`.
        let plan = proxy::panic_plan();
        if plan.restore_screen {
            // SAFETY: write(2) on fd 1 from a static buffer; no locks, no allocation.
            unsafe { libc::write(1, RESTORE.as_ptr().cast(), RESTORE.len()) };
        }
        if plan.restore_termios
            && let Some(saved) = pty::saved_termios()
        {
            // SAFETY: tcsetattr(3) on fd 0 with a termios that lives in a
            // static; it is async-signal-safe.
            unsafe { libc::tcsetattr(0, libc::TCSANOW, saved) };
        }
        match info.location() {
            Some(location) => {
                tracing::error!("panic at {}:{}", location.file(), location.line());
            }
            None => tracing::error!("panic at an unknown location"),
        }
        tracing::debug!("panic payload: {}", proxy::panic_message(info.payload()));
        let main = std::thread::current().name() == Some("main");
        if main && plan.restore_termios {
            start_failed("panic");
        }
    }));
}
