//! `fac init`, `fac enable`, `fac disable` and `fac uninstall`, end to end:
//! the block each writes, run by real shells (zsh, the test bash and fish)
//! in a PTY, starting fac in a ticked terminal and only there, never in a
//! loop, and removed byte for byte. Every run has a scratch HOME, ZDOTDIR,
//! XDG_CONFIG_HOME and SHELL, under a cleared environment; `sandbox`
//! panics before anything runs against the real home. The scratch dirs are
//! under cargo's `target/tmp`, not the system's temp folder, which `fac
//! init` refuses to write in.

mod harness;

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use fac::init::block::{self, Block};
use fac::term::ident::{self, KNOWN, TermKind};
use harness::{
    Profile, Program, Session, ShellKind, SpawnOpts, TIMEOUT, assert_active, has_line, wait_prompt,
};

const FAC: &str = env!("CARGO_BIN_EXE_fac");

/// Where every scratch dir (and session HOME) of these tests goes.
fn scratch_root() -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join("init")
}

/// Panics unless `home` is a scratch dir: never the real home.
fn sandbox(home: &Path) {
    let real = fac::init::passwd_home().expect("the real home");
    assert_ne!(
        home, real,
        "a test was about to run fac init in the real home"
    );
    assert!(
        home.starts_with(scratch_root()),
        "{} is not a scratch dir",
        home.display()
    );
}

/// A session's options in a terminal like `profile`, its HOME a scratch
/// dir.
fn spawn_opts(profile: Profile) -> SpawnOpts {
    let mut opts = SpawnOpts::with_profile(profile);
    opts.home_root = Some(scratch_root());
    opts
}

/// A scratch dir, removed on drop.
struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = scratch_root().join(format!("fac-init-{name}-{}-{n}", std::process::id()));
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

/// `fac <args>` with a cleared env: HOME, ZDOTDIR (`zdotdir`, else HOME),
/// XDG_CONFIG_HOME (`<home>/.config`), SHELL and a plain PATH, with no
/// controlling terminal.
fn fac(home: &Path, zdotdir: Option<&Path>, shell: &Path, args: &[&str]) -> Output {
    if let Some(zdotdir) = zdotdir {
        sandbox(zdotdir);
    }
    fac_with(home, zdotdir.unwrap_or(home), shell, &[], args)
}

/// [`fac`] with `ZDOTDIR` as given (it may be anywhere: fac must not
/// write there) and `extra` variables.
fn fac_with(
    home: &Path,
    zdotdir: &Path,
    shell: &Path,
    extra: &[(&str, &Path)],
    args: &[&str],
) -> Output {
    sandbox(home);
    let mut cmd = Command::new(FAC);
    cmd.args(args)
        .env_clear()
        .env("HOME", home)
        .env("ZDOTDIR", zdotdir)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("SHELL", shell)
        .env("PATH", "/usr/bin:/bin")
        .envs(extra.iter().copied())
        .stdin(Stdio::null());
    // SAFETY: setsid is async-signal-safe.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    cmd.output().expect("running fac")
}

/// `path` as the commands show it: `~/` for `home`.
fn tilde(home: &Path, path: &Path) -> String {
    format!(
        "~/{}",
        path.strip_prefix(home).expect("under home").display()
    )
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// `fac <args>`, which must succeed; its stdout.
fn fac_ok(home: &Path, zdotdir: Option<&Path>, shell: &Path, args: &[&str]) -> String {
    let out = fac(home, zdotdir, shell, args);
    assert!(
        out.status.success(),
        "fac {args:?}: {:?}\n{}{}",
        out.status,
        text(&out.stdout),
        text(&out.stderr)
    );
    text(&out.stdout)
}

/// The shell binary of `kind` the tests run.
fn shell(kind: ShellKind) -> PathBuf {
    harness::shell_path(kind)
}

// --- the terminal test against ident.rs -----------------------------------

/// Values each of the three variables takes in the agreement test.
const TERM_PROGRAMS: [Option<&str>; 12] = [
    None,
    Some(""),
    Some("Apple_Terminal"),
    Some("iTerm.app"),
    Some("vscode"),
    Some("zed"),
    Some("ghostty"),
    Some("WarpTerminal"),
    Some("tmux"),
    Some("Apple_terminal"),
    Some("ghostty "),
    Some("*"),
];
const LC_TERMINALS: [Option<&str>; 4] = [None, Some(""), Some("iTerm2"), Some("iTerm.app")];
const ZED_TERMS: [Option<&str>; 3] = [None, Some(""), Some("true")];

/// Every combination, and whether `ident::detect` makes it a ticked one.
fn combinations(ticked: &[TermKind]) -> Vec<([Option<&'static str>; 3], bool)> {
    let mut out = Vec::new();
    for term_program in TERM_PROGRAMS {
        for lc_terminal in LC_TERMINALS {
            for zed_term in ZED_TERMS {
                let kind = ident::detect(term_program, lc_terminal, zed_term);
                out.push((
                    [term_program, lc_terminal, zed_term],
                    ticked.contains(&kind),
                ));
            }
        }
    }
    out
}

/// A script for `kind` that runs the block's terminal test for every
/// combination and prints `<n> yes` or `<n> no`.
fn agreement_script(kind: ShellKind, ticked: &[TermKind]) -> String {
    let names = ["TERM_PROGRAM", "LC_TERMINAL", "ZED_TERM"];
    let mut script = String::new();
    for (n, (values, _)) in combinations(ticked).iter().enumerate() {
        match kind {
            ShellKind::Fish => {
                script.push_str("set -e TERM_PROGRAM LC_TERMINAL ZED_TERM\n");
                for (name, value) in names.iter().zip(values) {
                    if let Some(value) = value {
                        script.push_str(&format!("set -gx {name} '{value}'\n"));
                    }
                }
                let (set_terminal, test) = block::ticked_test_fish(ticked, "");
                script.push_str(&set_terminal);
                script.push_str(&format!(
                    "if {test}\n echo '{n} yes'\nelse\n echo '{n} no'\nend\n"
                ));
            }
            _ => {
                script.push_str("unset TERM_PROGRAM LC_TERMINAL ZED_TERM\n");
                for (name, value) in names.iter().zip(values) {
                    if let Some(value) = value {
                        script.push_str(&format!("export {name}='{value}'\n"));
                    }
                }
                let test = block::ticked_test_sh(kind, ticked, "");
                script.push_str(&format!(
                    "if {test}\nthen echo '{n} yes'; else echo '{n} no'; fi\n"
                ));
            }
        }
    }
    script
}

/// The block's terminal test tells terminals apart exactly as
/// `ident::detect` does, in each shell, for several ticked sets.
#[test]
fn the_terminal_test_agrees_with_ident() {
    let dir = Dir::new("agree");
    let sets: [&[TermKind]; 6] = [
        &KNOWN,
        &[],
        &[TermKind::Apple, TermKind::Ghostty],
        &[TermKind::Iterm2],
        &[TermKind::Zed],
        &[TermKind::Vscode, TermKind::Iterm2],
    ];
    for kind in [ShellKind::Zsh, ShellKind::Bash, ShellKind::Fish] {
        for ticked in sets {
            let script = dir.0.join("agree");
            std::fs::write(&script, agreement_script(kind, ticked)).expect("write");
            let args: &[&str] = match kind {
                ShellKind::Zsh => &["-f"],
                ShellKind::Bash => &["--norc", "--noprofile"],
                ShellKind::Fish => &["--no-config"],
            };
            let out = Command::new(shell(kind))
                .args(args)
                .arg(&script)
                .env_clear()
                .env("HOME", &dir.0)
                .env("PATH", "/usr/bin:/bin")
                .output()
                .expect("running the shell");
            let printed = text(&out.stdout);
            let expected: Vec<String> = combinations(ticked)
                .iter()
                .enumerate()
                .map(|(n, (_, yes))| format!("{n} {}", if *yes { "yes" } else { "no" }))
                .collect();
            let got: Vec<&str> = printed.lines().collect();
            assert_eq!(got, expected, "{kind:?} {ticked:?}\n{}", text(&out.stderr));
        }
    }
    // sh itself (bash's sh mode) and dash run the sh form too.
    for sh in ["/bin/sh", "/bin/dash"] {
        if !Path::new(sh).exists() {
            continue;
        }
        let ticked = [TermKind::Apple, TermKind::Zed];
        let script = dir.0.join("agree-sh");
        std::fs::write(&script, agreement_script(ShellKind::Bash, &ticked)).expect("write");
        let out = Command::new(sh)
            .arg(&script)
            .env_clear()
            .output()
            .expect("sh");
        let expected = combinations(&ticked)
            .iter()
            .enumerate()
            .map(|(n, (_, yes))| format!("{n} {}\n", if *yes { "yes" } else { "no" }))
            .collect::<String>();
        assert_eq!(text(&out.stdout), expected, "{sh}: {}", text(&out.stderr));
    }
}

/// Each shell parses its block; the bash block also parses in sh and dash
/// and does nothing there (it is in `.profile`), nor in bash's sh mode.
#[test]
fn each_block_parses_and_bashs_acts_only_under_bash() {
    let dir = Dir::new("parse");
    for kind in [ShellKind::Zsh, ShellKind::Bash, ShellKind::Fish] {
        for on in [true, false] {
            let text_ = block::render(
                kind,
                &Block {
                    on,
                    terminals: KNOWN.to_vec(),
                    created: false,
                },
                Path::new("/no/such/fac"),
            )
            .expect("render");
            let file = dir.0.join("block");
            std::fs::write(&file, &text_).expect("write");
            let out = Command::new(shell(kind))
                .arg("-n")
                .arg(&file)
                .output()
                .expect("-n");
            assert!(
                out.status.success(),
                "{kind:?}: {}\n{text_}",
                text(&out.stderr)
            );
        }
    }
    let bash_block = block::render(
        ShellKind::Bash,
        &Block {
            on: true,
            terminals: KNOWN.to_vec(),
            created: false,
        },
        Path::new(FAC),
    )
    .expect("render");
    let profile = dir.0.join(".profile");
    std::fs::write(&profile, format!("{bash_block}echo after\n")).expect("write");
    for sh in ["/bin/sh", "/bin/dash"] {
        if !Path::new(sh).exists() {
            continue;
        }
        let out = Command::new(sh)
            .arg("-c")
            .arg(format!(
                ". '{}'; echo FAC_SHELL=${{FAC_SHELL-unset}}",
                profile.display()
            ))
            .env_clear()
            .env("TERM_PROGRAM", "Apple_Terminal")
            .output()
            .expect("sh");
        assert!(out.status.success(), "{sh}: {}", text(&out.stderr));
        assert_eq!(text(&out.stdout), "after\nFAC_SHELL=unset\n", "{sh}");
        assert_eq!(text(&out.stderr), "", "{sh}");
    }
}

// --- sessions ---------------------------------------------------------------

/// A line appended to the startup file under test (after the block, which
/// goes on top): counts the shell starts that got past the block.
fn count_line(kind: ShellKind) -> &'static str {
    match kind {
        ShellKind::Fish => "echo x >> $HOME/starts",
        _ => "echo x >> \"$HOME/starts\"",
    }
}

fn starts(home: &Path) -> usize {
    std::fs::read_to_string(home.join("starts"))
        .map(|text| text.lines().count())
        .unwrap_or(0)
}

/// How a session starts its shell.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Start {
    /// A login shell (`-zsh`), as Terminal.app and iTerm2 start it.
    Login,
    /// An interactive non-login shell (`bash -i`).
    NonLogin,
    /// fac itself, the old way.
    Fac,
}

/// A session of `kind` in a terminal like `profile`, with `extra_env`;
/// `setup` runs fac commands in its HOME (and ZDOTDIR) first.
fn session(
    kind: ShellKind,
    profile: Profile,
    start: Start,
    extra_env: &[(&str, &str)],
    setup: impl FnOnce(&Path, Option<&Path>) + Send + 'static,
) -> Session {
    let mut opts = spawn_opts(profile);
    opts.shell = kind;
    match kind {
        // A copy of the fixture ZDOTDIR in the session's HOME.
        ShellKind::Zsh => opts.zshrc_extra = Some(count_line(kind).to_owned()),
        _ => opts.rc_extra = Some(count_line(kind).to_owned()),
    }
    if kind == ShellKind::Fish {
        opts.answer_fish_queries = true;
    }
    opts.extra_env = extra_env
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect();
    let path = shell(kind);
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .expect("a shell name")
        .to_owned();
    opts.program = match start {
        Start::Login => Some(Program {
            path,
            argv0: Some(format!("-{name}")),
            args: Vec::new(),
        }),
        Start::NonLogin => Some(Program {
            path,
            argv0: None,
            args: vec!["-i".to_owned()],
        }),
        Start::Fac => None,
    };
    opts.before_start = Some(Box::new(move |home: &Path, zdotdir: Option<&Path>| {
        sandbox(home);
        if let Some(zdotdir) = zdotdir {
            assert!(zdotdir.starts_with(home), "{}", zdotdir.display());
        }
        if kind == ShellKind::Bash && start == Start::NonLogin {
            // What a non-login bash reads: the fixture's prompt, and the
            // count line.
            std::fs::write(
                home.join(".bashrc"),
                format!("PS1='% '\nunset HISTFILE\n{}\n", count_line(kind)),
            )
            .expect("writing .bashrc");
        }
        setup(home, zdotdir);
    }));
    Session::spawn(opts)
}

/// `fac init --terminals <list>` for `kind`, in a session's dirs.
fn init(kind: ShellKind, terminals: &str) -> impl FnOnce(&Path, Option<&Path>) + Send + 'static {
    let terminals = terminals.to_owned();
    move |home, zdotdir| {
        fac_ok(
            home,
            zdotdir,
            &shell(kind),
            &["init", "--terminals", &terminals],
        );
    }
}

/// fac is running the shell: `FAC_ACTIVE` names the terminal.
fn assert_fac(s: &Session, kind: ShellKind) {
    wait_prompt(s);
    if kind == ShellKind::Fish {
        s.send_line("echo active=$FAC_ACTIVE");
        s.wait_for(
            "an active= line",
            |screen| {
                has_line(screen, |line| {
                    line.strip_prefix("active=")
                        .is_some_and(harness::is_fac_active_value)
                })
            },
            TIMEOUT,
        );
        wait_prompt(s);
    } else {
        assert_active(s);
    }
}

/// The shell runs without fac: no `FAC_ACTIVE`, and fac never started.
fn assert_plain(s: &Session, kind: ShellKind) {
    wait_prompt(s);
    s.send_line(match kind {
        ShellKind::Fish => "set -q FAC_ACTIVE; or echo active=none",
        _ => "echo active=${FAC_ACTIVE:-none}",
    });
    s.wait_for(
        "active=none",
        |screen| has_line(screen, |line| line == "active=none"),
        TIMEOUT,
    );
    assert!(
        !s.home().join("fac.log").exists(),
        "fac ran:\n{}",
        s.log_text()
    );
    wait_prompt(s);
}

/// The popup opens for `git ch`.
fn assert_popup(s: &Session) {
    s.send(b"git ch");
    s.wait_card("the popup", |card, _| card.is_some(), TIMEOUT);
    s.send(b"\x1b");
    s.wait_card("no popup", |card, _| card.is_none(), TIMEOUT);
    // Clear the line.
    s.send(b"\x15");
}

/// `exit` ends the session normally.
fn exit_normally(s: Session) {
    s.send_line("exit");
    assert_eq!(s.wait_exit(TIMEOUT), 0, "{}", s.screen_text());
}

#[test]
fn zsh_starts_fac_in_a_ticked_terminal() {
    let s = session(
        ShellKind::Zsh,
        Profile::Apple,
        Start::Login,
        &[],
        init(ShellKind::Zsh, "terminal,ghostty"),
    );
    assert_fac(&s, ShellKind::Zsh);
    assert_popup(&s);
    // The first zsh exec'd fac at the block, above the count line: only
    // fac's zsh got past it.
    assert_eq!(starts(s.home()), 1);
    s.send_line(
        "echo shell=${FAC_SHELL-unset} exported=$(/usr/bin/env | /usr/bin/grep -c '^FAC_SHELL=')",
    );
    s.wait_for(
        "the mark",
        |screen| has_line(screen, |line| line == "shell=run exported=0"),
        TIMEOUT,
    );
    // Sourcing .zshrc again does not start fac again.
    s.send_line("source $ZDOTDIR/.zshrc; echo sourced");
    s.wait_for(
        "sourced",
        |screen| has_line(screen, |line| line == "sourced"),
        TIMEOUT,
    );
    assert_eq!(starts(s.home()), 2);
    assert_fac(&s, ShellKind::Zsh);
    exit_normally(s);
}

#[test]
fn zsh_stays_plain_in_an_unticked_terminal() {
    let s = session(
        ShellKind::Zsh,
        Profile::Vscode104,
        Start::Login,
        &[],
        init(ShellKind::Zsh, "terminal,ghostty"),
    );
    assert_plain(&s, ShellKind::Zsh);
    assert_eq!(starts(s.home()), 1);
    exit_normally(s);
}

#[test]
fn zsh_disable_then_enable() {
    let disabled = |home: &Path, zdotdir: Option<&Path>| {
        init(ShellKind::Zsh, "all")(home, zdotdir);
        fac_ok(home, zdotdir, &shell(ShellKind::Zsh), &["disable"]);
    };
    let s = session(ShellKind::Zsh, Profile::Apple, Start::Login, &[], disabled);
    assert_plain(&s, ShellKind::Zsh);
    exit_normally(s);
    let enabled = |home: &Path, zdotdir: Option<&Path>| {
        init(ShellKind::Zsh, "all")(home, zdotdir);
        fac_ok(home, zdotdir, &shell(ShellKind::Zsh), &["disable"]);
        fac_ok(home, zdotdir, &shell(ShellKind::Zsh), &["enable"]);
    };
    let s = session(ShellKind::Zsh, Profile::Apple, Start::Login, &[], enabled);
    assert_fac(&s, ShellKind::Zsh);
    exit_normally(s);
}

/// The steps a user takes, and the startup files byte for byte after
/// `fac uninstall`.
fn round_trip(kind: ShellKind, files: &[&str], zdotdir: bool) {
    let dir = Dir::new("round-trip");
    let home = &dir.0;
    let zd = home.join("zdotdir");
    std::fs::create_dir_all(&zd).expect("mkdir");
    let zdotdir = zdotdir.then_some(zd.as_path());
    let base = zdotdir.unwrap_or(home);
    let mut originals = Vec::new();
    for (n, name) in files.iter().enumerate() {
        let path = if name.starts_with(".z") {
            base.join(name)
        } else {
            home.join(name)
        };
        // One with a trailing newline, one without.
        let original = if n % 2 == 0 {
            format!("# {name}\nexport A=1\n")
        } else {
            format!("# {name}\nexport A=1")
        };
        std::fs::write(&path, &original).expect("write");
        originals.push((path, original));
    }
    let shell = shell(kind);
    let out = fac_ok(home, zdotdir, &shell, &["init", "--terminals", "all"]);
    for (path, _) in &originals {
        assert!(out.contains(&tilde(home, path)), "{out}");
    }
    fac_ok(home, zdotdir, &shell, &["init", "--terminals", "zed"]);
    fac_ok(home, zdotdir, &shell, &["disable"]);
    fac_ok(home, zdotdir, &shell, &["enable"]);
    for (path, original) in &originals {
        let now = std::fs::read(path).expect("read");
        assert_eq!(
            block::find(&now).expect("find").len(),
            1,
            "{}",
            path.display()
        );
        let backup = fac::init::files::backup_path(path);
        assert_eq!(std::fs::read_to_string(&backup).expect("backup"), *original);
    }
    let out = fac_ok(home, zdotdir, &shell, &["uninstall"]);
    for (path, original) in &originals {
        assert_eq!(
            std::fs::read_to_string(path).expect("read"),
            *original,
            "{}",
            path.display()
        );
        assert!(out.contains(&tilde(home, path)), "{out}");
    }
    for folder in ["Library/Logs/fac", "Library/Caches/fac", ".config/fac"] {
        assert!(out.contains(&tilde(home, &home.join(folder))), "{out}");
    }
}

#[test]
fn uninstall_leaves_each_file_as_it_was() {
    round_trip(ShellKind::Zsh, &[".zshrc"], true);
    round_trip(ShellKind::Zsh, &[".zshrc"], false);
    round_trip(ShellKind::Zsh, &[".zshrc", ".zprofile"], true);
    round_trip(ShellKind::Zsh, &[".zprofile", ".zshrc"], false);
    round_trip(ShellKind::Bash, &[".bash_profile", ".bashrc"], false);
    round_trip(ShellKind::Bash, &[".profile", ".bashrc"], false);
    round_trip(ShellKind::Bash, &[".bash_login", ".bashrc"], false);
}

/// Created files go again; nothing new is left behind.
#[test]
fn uninstall_deletes_only_the_files_fac_created() {
    let dir = Dir::new("created");
    let home = &dir.0;
    for kind in [ShellKind::Zsh, ShellKind::Bash, ShellKind::Fish] {
        fac_ok(home, None, &shell(kind), &["init", "--terminals", "all"]);
    }
    let fish_file = home.join(".config/fish/conf.d/00-fac.fish");
    for path in [
        home.join(".zshrc"),
        home.join(".bash_profile"),
        home.join(".bashrc"),
        fish_file.clone(),
    ] {
        assert!(path.exists(), "{}", path.display());
    }
    assert!(!home.join(".profile").exists());
    let out = fac_ok(home, None, &shell(ShellKind::Zsh), &["uninstall"]);
    assert!(out.contains("Deleted"), "{out}");
    assert!(out.contains(&tilde(home, &fish_file)), "{out}");
    // ~/Library/Caches/fac holds the shell-version cache from `fac init`
    // for bash and fish: one of the folders fac keeps.
    let left: Vec<_> = walk(home)
        .into_iter()
        .filter(|path| !path.starts_with(home.join("Library")))
        .collect();
    assert_eq!(
        left,
        [
            home.join(".config"),
            home.join(".config/fish"),
            home.join(".config/fish/conf.d")
        ],
        "{out}"
    );
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).expect("ls") {
        let path = entry.expect("entry").path();
        out.push(path.clone());
        if path.is_dir() {
            out.extend(walk(&path));
        }
    }
    out.sort();
    out
}

#[test]
fn bash_login_and_non_login_start_fac() {
    for start in [Start::Login, Start::NonLogin] {
        let s = session(
            ShellKind::Bash,
            Profile::Apple,
            start,
            &[],
            init(ShellKind::Bash, "terminal"),
        );
        assert_fac(&s, ShellKind::Bash);
        assert_popup(&s);
        assert_eq!(starts(s.home()), 1);
        exit_normally(s);
    }
}

#[test]
fn bash_stays_plain_in_an_unticked_terminal_and_when_disabled() {
    for start in [Start::Login, Start::NonLogin] {
        let s = session(
            ShellKind::Bash,
            Profile::Vscode104,
            start,
            &[],
            init(ShellKind::Bash, "terminal"),
        );
        assert_plain(&s, ShellKind::Bash);
        exit_normally(s);
        let disabled = |home: &Path, zdotdir: Option<&Path>| {
            init(ShellKind::Bash, "all")(home, zdotdir);
            fac_ok(home, zdotdir, &shell(ShellKind::Bash), &["disable"]);
        };
        let s = session(ShellKind::Bash, Profile::Apple, start, &[], disabled);
        assert_plain(&s, ShellKind::Bash);
        exit_normally(s);
    }
}

#[test]
fn fish_starts_fac_in_a_ticked_terminal_only() {
    let s = session(
        ShellKind::Fish,
        Profile::Apple,
        Start::Login,
        &[],
        init(ShellKind::Fish, "terminal"),
    );
    assert_fac(&s, ShellKind::Fish);
    assert_popup(&s);
    exit_normally(s);
    let s = session(
        ShellKind::Fish,
        Profile::Vscode104,
        Start::Login,
        &[],
        init(ShellKind::Fish, "terminal"),
    );
    assert_plain(&s, ShellKind::Fish);
    exit_normally(s);
    let off = |home: &Path, zdotdir: Option<&Path>| {
        init(ShellKind::Fish, "terminal")(home, zdotdir);
        fac_ok(home, zdotdir, &shell(ShellKind::Fish), &["disable"]);
    };
    let s = session(ShellKind::Fish, Profile::Apple, Start::Login, &[], off);
    assert_plain(&s, ShellKind::Fish);
    exit_normally(s);
}

// --- no loops --------------------------------------------------------------

/// With `FAC_DISABLE=1` the block does nothing; started the old way, fac
/// execs the plain shell, and its block skips: the shell starts once and
/// exits normally.
#[test]
fn fac_disable_starts_the_shell_once() {
    for kind in [ShellKind::Zsh, ShellKind::Bash, ShellKind::Fish] {
        for start in [Start::Login, Start::Fac] {
            let s = session(
                kind,
                Profile::Apple,
                start,
                &[("FAC_DISABLE", "1")],
                init(kind, "all"),
            );
            wait_prompt(&s);
            assert_eq!(starts(s.home()), 1, "{}", s.screen_text());
            exit_normally(s);
        }
    }
}

/// fac's own fallbacks exec a plain shell marked `plain:<pid>`; the block
/// in it skips, so the shell starts once. Warp and Kiro, started the old
/// way.
#[test]
fn warp_and_kiro_fallbacks_start_the_shell_once() {
    for (name, value) in [("TERM_PROGRAM", "WarpTerminal"), ("Q_TERM", "1")] {
        let s = session(
            ShellKind::Zsh,
            Profile::Apple,
            Start::Fac,
            &[(name, value)],
            init(ShellKind::Zsh, "all"),
        );
        wait_prompt(&s);
        s.send_line("echo mark=$FAC_SHELL pid=$$");
        s.wait_for(
            "the plain mark",
            |screen| {
                has_line(screen, |line| {
                    line.strip_prefix("mark=plain:")
                        .and_then(|rest| rest.split_once(" pid="))
                        .is_some_and(|(mark, pid)| mark == pid)
                })
            },
            TIMEOUT,
        );
        assert_eq!(starts(s.home()), 1);
        exit_normally(s);
    }
}

/// A bash too old for fac (Apple's 3.2) with the block: the block execs
/// fac, which execs bash plain, whose block skips on the mark.
#[test]
fn a_too_old_bash_starts_once() {
    let old = Path::new("/bin/bash");
    let mut opts = spawn_opts(Profile::Apple);
    opts.shell = ShellKind::Bash;
    opts.shell_binary = Some(old.to_path_buf());
    opts.rc_extra = Some(count_line(ShellKind::Bash).to_owned());
    opts.program = Some(Program {
        path: old.to_path_buf(),
        argv0: Some("-bash".to_owned()),
        args: Vec::new(),
    });
    opts.before_start = Some(Box::new(|home: &Path, _: Option<&Path>| {
        fac_ok(
            home,
            None,
            Path::new("/bin/bash"),
            &["init", "--terminals", "all"],
        );
    }));
    let s = Session::spawn(opts);
    wait_prompt(&s);
    s.send_line("echo mark=$FAC_SHELL pid=$$ active=${FAC_ACTIVE:-none}");
    s.wait_for(
        "the plain mark",
        |screen| {
            has_line(screen, |line| {
                line.strip_prefix("mark=plain:")
                    .and_then(|rest| rest.split_once(" pid="))
                    .is_some_and(|(mark, rest)| rest == format!("{mark} active=none"))
            })
        },
        TIMEOUT,
    );
    assert_eq!(starts(s.home()), 1);
    assert!(s.log_text().contains("decision=plain"), "{}", s.log_text());
    exit_normally(s);
}

/// fac's shell integration cannot be set up (no cache dir): fac runs the
/// shell without its hook, marked `run`, and the block skips.
#[test]
fn a_degraded_start_does_not_loop() {
    let blocked = |home: &Path, zdotdir: Option<&Path>| {
        init(ShellKind::Zsh, "all")(home, zdotdir);
        // ~/Library is a file: ~/Library/Caches/fac cannot be made.
        std::fs::write(home.join("Library"), "").expect("write");
    };
    let s = session(ShellKind::Zsh, Profile::Apple, Start::Login, &[], blocked);
    wait_prompt(&s);
    s.send_line("echo mark=$FAC_SHELL");
    s.wait_for(
        "mark=run",
        |screen| has_line(screen, |line| line == "mark=run"),
        TIMEOUT,
    );
    assert!(
        s.log_text().contains("shell integration unavailable"),
        "{}",
        s.log_text()
    );
    assert_eq!(starts(s.home()), 1);
    exit_normally(s);
}

/// A nested shell on fac's terminal: zsh's block skips (its `$TTY` is
/// fac's), and so does bash's (its stdin is fac's tty): a nested bash stays
/// the non-login bash it was started as, with no second fac start. Back
/// out of it, the outer fac shell still works.
#[test]
fn a_nested_shell_starts_once() {
    let s = session(
        ShellKind::Zsh,
        Profile::Apple,
        Start::Login,
        &[],
        init(ShellKind::Zsh, "all"),
    );
    assert_fac(&s, ShellKind::Zsh);
    s.send_line("zsh -i");
    wait_prompt(&s);
    s.send_line("echo nested=${FAC_SHELL-unset}");
    s.wait_for(
        "nested=unset",
        |screen| has_line(screen, |line| line == "nested=unset"),
        TIMEOUT,
    );
    assert_eq!(starts(s.home()), 2);
    s.send_line("exit");
    assert_fac(&s, ShellKind::Zsh);
    assert_popup(&s);
    exit_normally(s);

    let s = session(
        ShellKind::Bash,
        Profile::Apple,
        Start::Login,
        &[],
        init(ShellKind::Bash, "all"),
    );
    assert_fac(&s, ShellKind::Bash);
    s.send_line(&format!("{} -i", shell(ShellKind::Bash).display()));
    wait_prompt(&s);
    s.send_line(
        "echo nested=${FAC_SHELL-unset} login=$(shopt -q login_shell && echo yes || echo no)",
    );
    s.wait_for(
        "a nested non-login bash with no mark",
        |screen| has_line(screen, |line| line == "nested=unset login=no"),
        TIMEOUT,
    );
    // A non-login bash reads only .bashrc, not the .bash_profile with the
    // count line, and fac started once.
    assert_eq!(starts(s.home()), 1);
    assert_eq!(
        s.log_text().matches("decision=").count(),
        1,
        "{}",
        s.log_text()
    );
    s.send_line("exit");
    assert_fac(&s, ShellKind::Bash);
    exit_normally(s);
}

/// A VS Code started from a fac shell inherits no mark: a shell in its
/// terminal (ticked) starts fac. Here: a terminal app's shell started from
/// the fac shell with its own controlling terminal is out of reach, so the
/// check is that the fac shell exports no mark.
#[test]
fn a_fac_shell_exports_no_mark() {
    for kind in [ShellKind::Zsh, ShellKind::Bash, ShellKind::Fish] {
        let s = session(kind, Profile::Apple, Start::Login, &[], init(kind, "all"));
        assert_fac(&s, kind);
        // `; true`: grep's status 1 is not the shell's at `exit`.
        s.send_line("/usr/bin/env | /usr/bin/grep -c FAC_SHELL=; true");
        s.wait_for(
            "a count of 0",
            |screen| has_line(screen, |line| line == "0"),
            TIMEOUT,
        );
        exit_normally(s);
    }
}

/// With no terminal stdin, fac execs the shell plain (`NotATty`); with the
/// block in its startup file, it starts once and ends.
#[test]
fn no_tty_does_not_loop() {
    let dir = Dir::new("no-tty");
    let home = &dir.0;
    fac_ok(
        home,
        None,
        Path::new("/bin/zsh"),
        &["init", "--terminals", "all"],
    );
    let started = Instant::now();
    let out = fac(home, None, Path::new("/bin/zsh"), &[]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(started.elapsed() < Duration::from_secs(10));
}

// --- the checklist ---------------------------------------------------------

/// The screen has no line of the checklist left on it.
fn assert_no_checklist(screen: &str) {
    for gone in [
        "Start fac in new windows of",
        "esc cancel",
        "Adds a short block",
        "Terminal.app\n",
        "❯",
        "●",
        "○",
    ] {
        assert!(!screen.contains(gone), "{gone:?} left on:\n{screen}");
    }
}

/// `fac init` in a terminal: Down, Space, Enter writes the terminals still
/// ticked; another run cancelled with Esc changes nothing.
#[test]
fn the_checklist_through_a_pty() {
    let run = |keys: &'static [&'static [u8]], previous: Option<String>| {
        let mut opts = spawn_opts(Profile::Apple);
        opts.cols = 200;
        opts.zshrc_extra = Some(String::new());
        opts.program = Some(Program {
            path: PathBuf::from(FAC),
            argv0: None,
            args: vec!["init".to_owned()],
        });
        opts.before_start = Some(Box::new(move |home: &Path, zdotdir: Option<&Path>| {
            sandbox(home);
            let zdotdir = zdotdir.expect("a zdotdir");
            assert!(zdotdir.starts_with(home));
            if let Some(previous) = previous {
                std::fs::write(zdotdir.join(".zshrc"), previous).expect("write");
            }
        }));
        let s = Session::spawn(opts);
        s.wait_text("esc cancel", TIMEOUT);
        let file = s.home().join("zdotdir/.zshrc");
        s.wait_text("Adds a short block to ~/zdotdir/.zshrc", TIMEOUT);
        for key in keys {
            s.send(key);
            std::thread::sleep(Duration::from_millis(100));
        }
        let status = s.wait_exit(TIMEOUT);
        let screen = s.screen_text();
        (
            status,
            screen,
            std::fs::read_to_string(&file).expect("read"),
        )
    };
    let (status, screen, written) = run(&[b"\x1b[B", b" ", b"\r"], None);
    assert_eq!(status, 0, "{screen}");
    assert!(screen.contains("✓ fac is set up"), "{screen}");
    assert!(
        screen.contains("Starts in   Terminal.app, Ghostty, VS Code, Zed"),
        "{screen}"
    );
    assert!(screen.contains("Changed     ~/zdotdir/.zshrc"), "{screen}");
    assert!(screen.contains("fac uninstall   remove it"), "{screen}");
    assert!(
        screen.contains(
            "If fac ever fails to start, new windows open plain and fac doctor shows why."
        ),
        "{screen}"
    );
    assert_no_checklist(&screen);
    let block = block::read(written.as_bytes())
        .expect("read")
        .expect("a block");
    assert_eq!(
        block.terminals,
        [
            TermKind::Apple,
            TermKind::Ghostty,
            TermKind::Vscode,
            TermKind::Zed
        ]
    );
    // Esc: nothing changes; the list started from the block's choices.
    let (status, screen, after) = run(&[b"\x1b"], Some(written.clone()));
    assert_eq!(status, 1, "{screen}");
    assert!(screen.contains("Cancelled. Nothing changed."), "{screen}");
    assert_no_checklist(&screen);
    assert_eq!(after, written);
}

/// `fac init` on a terminal with `env` added, Enter pressed at once: the
/// raw bytes it wrote and the final screen.
fn init_on_a_terminal(env: &[(&str, &str)]) -> (String, String) {
    let mut opts = spawn_opts(Profile::Apple);
    opts.cols = 200;
    opts.zshrc_extra = Some(String::new());
    opts.extra_env = env
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    opts.program = Some(Program {
        path: PathBuf::from(FAC),
        argv0: None,
        args: vec!["init".to_owned()],
    });
    opts.before_start = Some(Box::new(|home: &Path, _: Option<&Path>| sandbox(home)));
    let s = Session::spawn(opts);
    s.wait_text("esc cancel", TIMEOUT);
    s.wait_text("Adds a short block", TIMEOUT);
    s.send(b"\r");
    assert_eq!(s.wait_exit(TIMEOUT), 0, "{}", s.screen_text());
    (text(&s.raw_output()), s.screen_text())
}

/// Whether `bytes` hold a colour or style sequence (`ESC [ ... m`).
fn has_sgr(bytes: &str) -> bool {
    bytes.split("\x1b[").skip(1).any(|rest| {
        let end = rest.find(|c: char| !(c.is_ascii_digit() || c == ';'));
        end.is_some_and(|end| rest[end..].starts_with('m'))
    })
}

/// Colours come from the terminal's own 16: cyan accent, green marks.
#[test]
fn the_checklist_uses_the_ansi_colours() {
    let (raw, screen) = init_on_a_terminal(&[]);
    assert!(has_sgr(&raw));
    assert!(raw.contains("\x1b[36m"), "no cyan accent");
    assert!(raw.contains("\x1b[32m"), "no green");
    assert!(raw.contains("\x1b[2m"), "no dim");
    assert!(!raw.contains("38;"), "only the 16 colours");
    assert!(screen.contains("✓ fac is set up"), "{screen}");
}

/// `NO_COLOR` (any value, even empty) and a pipe give no colour bytes.
#[test]
fn no_colour_without_a_terminal_or_with_no_color() {
    let (raw, screen) = init_on_a_terminal(&[("NO_COLOR", "")]);
    assert!(!has_sgr(&raw), "{raw:?}");
    assert!(screen.contains("✓ fac is set up"), "{screen}");
    let dir = Dir::new("pipe");
    let out = fac_ok(
        &dir.0,
        None,
        Path::new("/bin/zsh"),
        &["init", "--terminals", "all"],
    );
    assert!(!out.contains('\x1b'), "{out:?}");
    assert!(out.contains("fac is set up"), "{out}");
}

/// Without a UTF-8 locale every mark is ASCII.
#[test]
fn ascii_without_a_utf8_locale() {
    let (raw, screen) = init_on_a_terminal(&[("LANG", "C")]);
    assert!(screen.contains("+ fac is set up"), "{screen}");
    assert!(raw.is_ascii(), "{raw:?}");
    let (raw, _) = init_on_a_terminal(&[("LANG", "C"), ("LC_ALL", "en_US.UTF-8")]);
    assert!(!raw.is_ascii(), "LC_ALL wins");
    // The piped run (no LANG) is ASCII too.
    let dir = Dir::new("ascii");
    let out = fac_ok(
        &dir.0,
        None,
        Path::new("/bin/zsh"),
        &["init", "--terminals", "all"],
    );
    assert!(out.is_ascii() && out.contains("+ fac is set up"), "{out}");
}

/// Ctrl-C and q cancel too, and the terminal is as it was after: fac init
/// runs under sh, the session leader, so the termios can be read after it
/// exits.
#[test]
fn the_checklist_restores_the_terminal() {
    for key in [&b"\x03"[..], b"q"] {
        let mut opts = spawn_opts(Profile::Apple);
        opts.cols = 200;
        opts.zshrc_extra = Some(String::new());
        opts.program = Some(Program {
            path: PathBuf::from("/bin/sh"),
            argv0: None,
            args: vec![
                "-c".to_owned(),
                r#""$0" init; echo "init exited $?"; read _"#.to_owned(),
                FAC.to_owned(),
            ],
        });
        opts.before_start = Some(Box::new(|home: &Path, _: Option<&Path>| sandbox(home)));
        let s = Session::spawn(opts);
        s.wait_text("esc cancel", TIMEOUT);
        s.send(key);
        s.wait_text("init exited 1", TIMEOUT);
        let (before, after) = (s.initial_termios(), s.slave_termios());
        assert_eq!(after.c_lflag, before.c_lflag, "{key:?}");
        assert_eq!(after.c_iflag, before.c_iflag, "{key:?}");
        assert_eq!(after.c_oflag, before.c_oflag, "{key:?}");
        let screen = s.screen_text();
        assert!(screen.contains("Cancelled. Nothing changed."), "{screen}");
        assert_no_checklist(&screen);
        // The cursor is shown again.
        assert!(!s.with_screen(|screen| screen.hide_cursor()), "{screen}");
        assert!(
            !s.home().join("zdotdir/.zshrc").exists() || {
                let text = std::fs::read(s.home().join("zdotdir/.zshrc")).expect("read");
                block::find(&text).expect("find").is_empty()
            }
        );
    }
}

// --- arguments -------------------------------------------------------------

#[test]
fn usage_errors() {
    let dir = Dir::new("usage");
    let home = &dir.0;
    let zsh = Path::new("/bin/zsh");
    // No terminal and no --terminals.
    let out = fac(home, None, zsh, &["init"]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("usage: fac init"),
        "{}",
        text(&out.stderr)
    );
    for (shell, args) in [
        (zsh, &["init", "--terminals", "warp"][..]),
        (zsh, &["init", "--terminals", "all", "--shell", "nu"]),
        (zsh, &["init", "--bogus"]),
        (Path::new("/bin/sh"), &["init", "--terminals", "all"]),
        (zsh, &["enable", "now"]),
    ] {
        let out = fac(home, None, shell, args);
        assert_eq!(
            out.status.code(),
            Some(2),
            "{args:?}: {}",
            text(&out.stderr)
        );
    }
    // No SHELL at all.
    let out = Command::new(FAC)
        .args(["init", "--terminals", "all"])
        .env_clear()
        .env("HOME", home)
        .output()
        .expect("fac");
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    // enable and disable with no block.
    for verb in ["enable", "disable"] {
        let out = fac(home, None, zsh, &[verb]);
        assert_eq!(out.status.code(), Some(1));
        assert!(
            text(&out.stderr).contains("Run fac init"),
            "{}",
            text(&out.stderr)
        );
    }
    // Nothing was written.
    assert_eq!(walk(home), Vec::<PathBuf>::new());
    // --help lists the four commands.
    let help = text(
        &Command::new(FAC)
            .arg("--help")
            .output()
            .expect("help")
            .stdout,
    );
    for verb in ["init", "enable", "disable", "uninstall"] {
        assert!(help.contains(&format!("fac {verb}")), "{verb}: {help}");
    }
}

/// With Kiro's block in the file (or `Q_TERM` set), one line says fac
/// starts instead of it; the block goes above Kiro's.
#[test]
fn kiro_is_named_and_fac_goes_first() {
    let dir = Dir::new("kiro");
    let home = &dir.0;
    let kiro = "# Added by Kiro CLI\n# Q pre block. Keep at the top of this file.\nexport A=1\n";
    std::fs::write(home.join(".zshrc"), kiro).expect("write");
    let out = fac_ok(
        home,
        None,
        Path::new("/bin/zsh"),
        &["init", "--terminals", "all"],
    );
    assert!(out.contains("Kiro is set up too"), "{out}");
    let text = std::fs::read_to_string(home.join(".zshrc")).expect("read");
    assert!(text.starts_with(block::BEGIN), "{text}");
    assert!(text.ends_with(kiro), "{text}");
    let plain = Dir::new("no-kiro");
    let out = fac_ok(
        &plain.0,
        None,
        Path::new("/bin/zsh"),
        &["init", "--terminals", "all"],
    );
    assert!(!out.contains("Kiro"), "{out}");
}

/// An unwritable file: a clear error, and nothing half-written.
#[test]
fn an_unwritable_file_is_an_error() {
    use std::os::unix::fs::PermissionsExt;
    let dir = Dir::new("unwritable");
    let home = &dir.0;
    let zshrc = home.join(".zshrc");
    std::fs::write(&zshrc, "export A=1\n").expect("write");
    std::fs::set_permissions(&zshrc, std::fs::Permissions::from_mode(0o444)).expect("chmod");
    let out = fac(
        home,
        None,
        Path::new("/bin/zsh"),
        &["init", "--terminals", "all"],
    );
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains(".zshrc"),
        "{}",
        text(&out.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(&zshrc).expect("read"),
        "export A=1\n"
    );
    assert_eq!(walk(home), [zshrc]);
}

/// Prints each shell's block (for the record).
#[test]
fn the_blocks() {
    for kind in [ShellKind::Zsh, ShellKind::Bash, ShellKind::Fish] {
        let text = block::render(
            kind,
            &Block {
                on: true,
                terminals: vec![TermKind::Apple, TermKind::Ghostty],
                created: false,
            },
            Path::new("/opt/homebrew/bin/fac"),
        )
        .expect("render");
        println!("--- {kind:?}\n{text}");
    }
}

// --- sign-off fixes ----------------------------------------------------------

/// A login zsh runs `.zprofile` before `.zshrc`: with an exec in it below
/// fac's block (as Kiro's pre block does, skipped when
/// `PROCESS_LAUNCHED_BY_Q` is set), the block there starts fac first.
#[test]
fn a_zprofile_exec_does_not_win() {
    let setup = |home: &Path, zdotdir: Option<&Path>| {
        let zdotdir = zdotdir.expect("a zdotdir");
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(zdotdir.join(".zprofile"))
            .expect("open .zprofile");
        std::io::Write::write_all(
            &mut file,
            b"[[ -n ${PROCESS_LAUNCHED_BY_Q-} ]] || exec /bin/zsh -f -i\n",
        )
        .expect("append");
        init(ShellKind::Zsh, "terminal")(home, Some(zdotdir));
        let text = std::fs::read(zdotdir.join(".zprofile")).expect("read");
        assert!(block::starts_with_block(&text));
    };
    let s = session(ShellKind::Zsh, Profile::Apple, Start::Login, &[], setup);
    assert_fac(&s, ShellKind::Zsh);
    assert_eq!(starts(s.home()), 1);
    exit_normally(s);
}

/// `bash -i script.sh` and `zsh -i script.zsh` in a ticked terminal run
/// the script: fac never starts.
#[test]
fn an_interactive_shell_on_a_script_runs_it() {
    for kind in [ShellKind::Zsh, ShellKind::Bash] {
        let dir = Dir::new("script");
        let script = dir.0.join(match kind {
            ShellKind::Zsh => "script.zsh",
            _ => "script.sh",
        });
        std::fs::write(&script, "echo ran > \"$HOME/ran\"\nexit 0\n").expect("write");
        let mut opts = spawn_opts(Profile::Apple);
        opts.shell = kind;
        match kind {
            ShellKind::Zsh => opts.zshrc_extra = Some(String::new()),
            _ => opts.rc_extra = Some(String::new()),
        }
        opts.program = Some(Program {
            path: shell(kind),
            argv0: None,
            args: vec!["-i".to_owned(), script.display().to_string()],
        });
        opts.before_start = Some(Box::new(move |home: &Path, zdotdir: Option<&Path>| {
            sandbox(home);
            std::fs::write(home.join(".bashrc"), "PS1='% '\n").expect("writing .bashrc");
            init(kind, "all")(home, zdotdir);
        }));
        let s = Session::spawn(opts);
        assert_eq!(s.wait_exit(TIMEOUT), 0, "{kind:?}: {}", s.screen_text());
        assert!(
            s.home().join("ran").exists(),
            "{kind:?}: {}",
            s.screen_text()
        );
        assert!(
            !s.home().join("fac.log").exists(),
            "{kind:?}: {}",
            s.log_text()
        );
    }
}

/// An old bash in `$SHELL`: the block sets `SHELL` to the bash running it
/// (`$BASH`), so fac starts that one (5.3+), not Apple's 3.2. A non-login
/// bash started by its path, as VS Code, Zed and Ghostty start one: a login
/// `-bash` has the passwd shell in `$BASH` (the module docs of
/// `fac::init`).
#[test]
fn bash_starts_fac_with_the_bash_that_ran_the_block() {
    let s = session(
        ShellKind::Bash,
        Profile::Apple,
        Start::NonLogin,
        &[("SHELL", "/bin/bash")],
        init(ShellKind::Bash, "terminal"),
    );
    assert_fac(&s, ShellKind::Bash);
    let expected = format!("shell={}", shell(ShellKind::Bash).display());
    s.send_line("echo shell=$SHELL");
    s.wait_for(
        "SHELL is the test bash",
        |screen| has_line(screen, |line| line == expected),
        TIMEOUT,
    );
    exit_normally(s);
}

/// Inside tmux or screen (`TMUX`, `STY`), every block stays plain.
#[test]
fn tmux_and_screen_stay_plain() {
    for kind in [ShellKind::Zsh, ShellKind::Bash, ShellKind::Fish] {
        for var in ["TMUX", "STY"] {
            if kind != ShellKind::Zsh && var == "STY" {
                continue;
            }
            let s = session(
                kind,
                Profile::Apple,
                Start::Login,
                &[(var, "/private/tmp/tmux-501/default,1,0")],
                init(kind, "all"),
            );
            assert_plain(&s, kind);
            exit_normally(s);
        }
    }
}

/// Safe mode: while `~/.config/fac/no-start` exists, every block stays
/// plain and says so in one dim line; `fac init` says what happens.
#[test]
fn the_no_start_file_keeps_the_shell_plain() {
    for kind in [ShellKind::Zsh, ShellKind::Bash, ShellKind::Fish] {
        let setup = move |home: &Path, zdotdir: Option<&Path>| {
            let out = fac_ok(home, zdotdir, &shell(kind), &["init", "--terminals", "all"]);
            let words = out.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(
                words.contains("new windows open plain and fac doctor shows why")
                    && out.contains("fac disable"),
                "{out}"
            );
            safe_mode_on(home, "");
        };
        let s = session(kind, Profile::Apple, Start::Login, &[], setup);
        assert_plain(&s, kind);
        assert_says_plain(&s, true);
        exit_normally(s);
    }
}

/// Inside VS Code's zsh integration, `ZDOTDIR` is its temp folder and
/// `USER_ZDOTDIR` the user's: fac init writes the user's. With no
/// `USER_ZDOTDIR`, it refuses to write in the temp folder.
#[test]
fn vscode_zdotdir_is_seen_through() {
    let dir = Dir::new("vscode");
    let home = &dir.0;
    let user = home.join("zd");
    std::fs::create_dir_all(&user).expect("mkdir");
    std::fs::write(user.join(".zshrc"), "export MINE=1\n").expect("write");
    // VS Code's own folder, in the system's temp folder.
    let vscode = std::env::temp_dir().join(format!("fac-init-code-zsh-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&vscode);
    std::fs::create_dir_all(&vscode).expect("mkdir");
    let theirs = "# VS Code's shellIntegration-rc.zsh\n";
    std::fs::write(vscode.join(".zshrc"), theirs).expect("write");
    let zsh = Path::new("/bin/zsh");
    let out = fac_with(
        home,
        &vscode,
        zsh,
        &[
            ("USER_ZDOTDIR", &user),
            ("VSCODE_INJECTION", Path::new("1")),
        ],
        &["init", "--terminals", "all"],
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    let mine = std::fs::read(user.join(".zshrc")).expect("read");
    assert!(block::starts_with_block(&mine));
    assert_eq!(
        std::fs::read_to_string(vscode.join(".zshrc")).expect("read"),
        theirs
    );
    // No USER_ZDOTDIR: refused, exit 1, nothing written.
    let out = fac_with(home, &vscode, zsh, &[], &["init", "--terminals", "all"]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stdout));
    assert!(
        text(&out.stderr).contains("temporary folder"),
        "{}",
        text(&out.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(vscode.join(".zshrc")).expect("read"),
        theirs
    );
    assert!(!fac::init::files::backup_path(&vscode.join(".zshrc")).exists());
    // uninstall still works from there (nothing of fac's in the temp folder).
    let out = fac_with(
        home,
        &vscode,
        zsh,
        &[("USER_ZDOTDIR", &user)],
        &["uninstall"],
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        std::fs::read(user.join(".zshrc")).expect("read"),
        b"export MINE=1\n"
    );
    let _ = std::fs::remove_dir_all(&vscode);
}

/// fish sources conf.d files in name order: fac's `00-fac.fish` runs before
/// a Kiro-style `00_kiro_pre.fish` that execs another program.
#[test]
fn fishs_file_runs_before_other_conf_d_files() {
    let setup = |home: &Path, zdotdir: Option<&Path>| {
        let conf_d = home.join(".config/fish/conf.d");
        std::fs::create_dir_all(&conf_d).expect("mkdir");
        std::fs::write(
            conf_d.join("00_kiro_pre.fish"),
            "set -q PROCESS_LAUNCHED_BY_Q; or exec /bin/sh\n",
        )
        .expect("write");
        init(ShellKind::Fish, "terminal")(home, zdotdir);
        assert!(conf_d.join("00-fac.fish").exists());
    };
    let s = session(ShellKind::Fish, Profile::Apple, Start::Login, &[], setup);
    assert_fac(&s, ShellKind::Fish);
    exit_normally(s);
}

// --- safe mode and fac doctor ------------------------------------------------

/// Safe mode's file in a scratch `home`, holding `text`.
fn safe_mode_on(home: &Path, text: &str) {
    sandbox(home);
    let file = home.join(".config/fac/no-start");
    std::fs::create_dir_all(file.parent().expect("dir")).expect("mkdir");
    std::fs::write(&file, text).expect("write");
}

/// The block said once that the window is plain, dim unless `dim` is
/// false.
fn assert_says_plain(s: &Session, dim: bool) {
    let screen = s.screen_text();
    assert_eq!(screen.matches(block::NO_START_LINE).count(), 1, "{screen}");
    let raw = text(&s.raw_output());
    let dimmed = format!("\x1b[2m{}\x1b[0m", block::NO_START_LINE);
    assert_eq!(raw.contains(&dimmed), dim, "{raw:?}");
}

/// A panic as fac starts from the block turns safe mode on: the file says
/// `panic` and nothing else. The next shell in a ticked terminal stays
/// plain and says so; `fac doctor` there turns safe mode off.
#[test]
fn a_panic_at_start_turns_safe_mode_on() {
    for kind in [ShellKind::Zsh, ShellKind::Bash, ShellKind::Fish] {
        let s = session(
            kind,
            Profile::Apple,
            Start::Login,
            &[("FAC_TEST_PANIC_AT_START", "1")],
            init(kind, "all"),
        );
        let status = s.wait_exit(TIMEOUT);
        assert_ne!(status, 0, "{kind:?}: {}", s.screen_text());
        let file = s.home().join(".config/fac/no-start");
        let written = std::fs::read_to_string(&file).expect("safe mode's file");
        assert_eq!(written, "fac failed to start: panic\n", "{kind:?}");
        assert!(
            s.log_text().contains("safe mode is on (panic)"),
            "{}",
            s.log_text()
        );

        let next = session(
            kind,
            Profile::Apple,
            Start::Login,
            &[],
            move |home, zdotdir| {
                init(kind, "all")(home, zdotdir);
                safe_mode_on(home, &written);
            },
        );
        assert_plain(&next, kind);
        assert_says_plain(&next, true);
        // The marker is not in the command line, which may wrap.
        next.send_line(&format!("{FAC} doctor; printf 'doc%s\\n' tor-end"));
        next.wait_for(
            "doctor's end",
            |screen| has_line(screen, |line| line == "doctor-end"),
            TIMEOUT,
        );
        let screen = next.screen_text();
        assert!(
            screen.contains("✓ Safe mode is off: new windows start fac again"),
            "{kind:?}:\n{screen}"
        );
        assert!(!next.home().join(".config/fac/no-start").exists());
        exit_normally(next);
    }
}

/// Under `NO_COLOR` the line has no colour.
#[test]
fn the_plain_line_has_no_colour_under_no_color() {
    let s = session(
        ShellKind::Zsh,
        Profile::Apple,
        Start::Login,
        &[("NO_COLOR", "1")],
        |home, zdotdir| {
            init(ShellKind::Zsh, "all")(home, zdotdir);
            safe_mode_on(home, "fac failed to start: panic\n");
        },
    );
    assert_plain(&s, ShellKind::Zsh);
    assert_says_plain(&s, false);
    exit_normally(s);
}

/// A shell that exits with a status of its own is not a failed start.
#[test]
fn a_normal_exit_leaves_safe_mode_off() {
    for kind in [ShellKind::Zsh, ShellKind::Bash, ShellKind::Fish] {
        let s = session(kind, Profile::Apple, Start::Login, &[], init(kind, "all"));
        assert_fac(&s, kind);
        s.send_line("exit 3");
        assert_eq!(s.wait_exit(TIMEOUT), 3, "{kind:?}");
        assert!(!s.home().join(".config/fac/no-start").exists(), "{kind:?}");
    }
}

/// `fac enable` turns safe mode off too.
#[test]
fn enable_turns_safe_mode_off() {
    let dir = Dir::new("enable-safe");
    let home = &dir.0;
    fac_ok(
        home,
        None,
        Path::new("/bin/zsh"),
        &["init", "--terminals", "all"],
    );
    safe_mode_on(home, "fac failed to start: panic\n");
    let out = fac_ok(home, None, Path::new("/bin/zsh"), &["enable"]);
    assert!(
        out.contains("Safe mode   off: new windows start fac again"),
        "{out}"
    );
    assert!(!home.join(".config/fac/no-start").exists());
    // Again: nothing to say.
    let out = fac_ok(home, None, Path::new("/bin/zsh"), &["enable"]);
    assert!(!out.contains("Safe mode"), "{out}");
}

/// `fac doctor` in a scratch HOME, with a UTF-8 locale: its exit status
/// and output.
fn doctor(home: &Path) -> (i32, String) {
    let utf8 = Path::new("en_US.UTF-8");
    let out = fac_with(
        home,
        home,
        Path::new("/bin/zsh"),
        &[("LANG", utf8)],
        &["doctor"],
    );
    assert_eq!(text(&out.stderr), "");
    (
        out.status.code().expect("an exit status"),
        text(&out.stdout),
    )
}

#[test]
fn doctor_on_a_clean_setup_exits_0() {
    let dir = Dir::new("doctor-clean");
    let home = &dir.0;
    // No block yet: only notes.
    let (status, out) = doctor(home);
    assert_eq!(status, 0, "{out}");
    assert!(out.contains("! fac starts only when you run it"), "{out}");
    fac_ok(
        home,
        None,
        Path::new("/bin/zsh"),
        &["init", "--terminals", "all"],
    );
    let (status, out) = doctor(home);
    assert_eq!(status, 0, "{out}");
    for line in [
        "  fac doctor\n",
        &format!("    ✓ fac {}\n", env!("CARGO_PKG_VERSION")),
        "    ✓ zsh at /bin/zsh\n",
        "    ✓ ~/.zshrc: on in Terminal.app, iTerm2, Ghostty, VS Code and Zed\n",
        "    ✓ Safe mode is off\n",
        "  ✓ No problems found\n",
    ] {
        assert!(out.contains(line), "{line:?} in:\n{out}");
    }
    assert!(!out.contains('✗'), "{out}");
}

#[test]
fn doctor_finds_a_missing_fac_and_a_broken_spec() {
    let dir = Dir::new("doctor-broken");
    let home = &dir.0;
    let text = block::render(
        ShellKind::Zsh,
        &Block {
            on: true,
            terminals: KNOWN.to_vec(),
            created: false,
        },
        &home.join("gone/fac"),
    )
    .expect("render");
    std::fs::write(home.join(".zshrc"), text).expect("write");
    safe_mode_on(home, "fac failed to start: panic\n");
    let (status, out) = doctor(home);
    assert_eq!(status, 1, "{out}");
    let words = out.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        words.contains("✗ ~/.zshrc starts ~/gone/fac, which does not exist"),
        "{out}"
    );
    assert!(words.contains("Safe mode stays on"), "{out}");
    assert!(home.join(".config/fac/no-start").exists());

    // A spec that does not load is named.
    fac_ok(
        home,
        None,
        Path::new("/bin/zsh"),
        &["init", "--terminals", "all"],
    );
    let specs = home.join(".config/fac/specs");
    std::fs::create_dir_all(&specs).expect("mkdir");
    std::fs::write(specs.join("broken.json"), "{").expect("write");
    let (status, out) = doctor(home);
    assert_eq!(status, 1, "{out}");
    let words = out.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        words.contains("✗ ~/.config/fac/specs/broken.json does not load"),
        "{out}"
    );
    assert!(home.join(".config/fac/no-start").exists());
    std::fs::remove_file(specs.join("broken.json")).expect("rm");
    let (status, out) = doctor(home);
    assert_eq!(status, 0, "{out}");
    assert!(!home.join(".config/fac/no-start").exists());
}

/// `fac doctor` on a terminal: colour, and none under `NO_COLOR`.
#[test]
fn doctor_has_no_escapes_under_no_color() {
    for no_color in [false, true] {
        let mut opts = spawn_opts(Profile::Apple);
        opts.cols = 120;
        opts.zshrc_extra = Some(String::new());
        if no_color {
            opts.extra_env = vec![("NO_COLOR".to_owned(), "1".to_owned())];
        }
        opts.program = Some(Program {
            path: PathBuf::from(FAC),
            argv0: None,
            args: vec!["doctor".to_owned()],
        });
        opts.before_start = Some(Box::new(|home: &Path, _: Option<&Path>| sandbox(home)));
        let s = Session::spawn(opts);
        s.wait_exit(TIMEOUT);
        let raw = text(&s.raw_output());
        assert!(raw.contains("No problems found"), "{raw:?}");
        assert_eq!(raw.contains('\x1b'), !no_color, "{raw:?}");
    }
}
