//! fish through fac, end to end: the real hook (`shell/fish/fac.fish`), its
//! worker (`complete -C`), fish's history and quoting, vi mode, a widget
//! that reads the terminal, the apply pipe, and fish's terminal queries.
//! Every session runs the test fish (`FAC_TEST_FISH`) in a fixture tree,
//! with the harness's scratch HOME and XDG dirs.

mod harness;
mod suite;

use std::ffi::OsString;
use std::fmt::Write as _;
use std::os::fd::{AsFd, OwnedFd};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use fac::shell::install;
use fac::shell::protocol::{encode_apply, percent_decode};
use harness::{
    Card, Profile, Session, ShellKind, SpawnOpts, TIMEOUT, has_line, wait_log, wait_prompt,
};
use nix::errno::Errno;
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::sys::signal::{Signal, kill};
use nix::sys::wait::waitpid;
use nix::unistd::Pid;

const DOWN: &[u8] = b"\x1b[B";
const UP: &[u8] = b"\x1b[A";
const ESC: &[u8] = b"\x1b";
const TAB: &[u8] = b"\t";
/// fac.fish's trigger and apply trigger, as typed.
const TRIGGER: &str = "\u{10fffd}";
const APPLY_TRIGGER: &str = "\u{10fffc}";
/// The fixture history, oldest first, as fish writes it.
const HISTORY: &str = "- cmd: ls -la\n  when: 1700000000\n\
                       - cmd: echo from-history\n  when: 1700000100\n";

/// The fixture tree, removed on drop: the cwd (two dirs, two files with
/// names to quote), the only commands on `PATH` besides the system's, fish's
/// data dir (`XDG_DATA_HOME`) with the history, and a user function with
/// its completions.
struct Tree(PathBuf);

impl Tree {
    fn new() -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("fac-fish-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let tree = Self(root);
        for dir in ["cwd/fixture-dir", "cwd/fixture-other", "bin", "functions"] {
            std::fs::create_dir_all(tree.0.join(dir)).expect("creating the fixture tree");
        }
        for (file, text) in [
            ("cwd/a b.txt", "spaced\n"),
            ("cwd/it's.txt", "quoted\n"),
            ("cwd/file-a", ""),
            (
                "functions/myfn.fish",
                "function myfn\n    echo myfn $argv\nend\n",
            ),
            (
                "completions/myfn.fish",
                "complete -c myfn -f -a 'alpha beta' -d 'my arg'\n",
            ),
            // No spec and no completions: fish completes files for it.
            (
                "functions/facshow.fish",
                "function facshow\n    cat $argv\nend\n",
            ),
            ("data/fish/fac_history", HISTORY),
        ] {
            let path = tree.0.join(file);
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).expect("creating the fixture tree");
            }
            std::fs::write(path, text).expect("writing a fixture file");
        }
        // fish builds completions from man pages, in the background, unless
        // this is there.
        std::fs::create_dir_all(tree.0.join("data/fish/generated_completions"))
            .expect("creating the data dir");
        for (name, target) in [
            ("ls", "/bin/ls"),
            ("git", "/usr/bin/git"),
            ("cat", "/bin/cat"),
        ] {
            std::os::unix::fs::symlink(target, tree.0.join("bin").join(name))
                .expect("linking a command");
        }
        tree
    }

    fn cwd(&self) -> PathBuf {
        self.0.join("cwd")
    }

    /// What every session's config.fish gets, then `extra`. fish's grey
    /// autosuggestion is off, so the screen's line is the line.
    fn rc(&self, extra: &str) -> String {
        format!(
            "cd '{}'\nset -gx PATH '{}' /usr/bin /bin\nset -g fish_history fac\n\
             set -g fish_autosuggestion_enabled 0\n\
             set -p fish_function_path '{}'\nset -p fish_complete_path '{}'\n{extra}",
            self.cwd().display(),
            self.0.join("bin").display(),
            self.0.join("functions").display(),
            self.0.join("completions").display(),
        )
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// fac running fish in the fixture tree, `extra` appended to config.fish,
/// `tweak` changing the options; not waited on.
fn spawn(extra: &str, tweak: impl FnOnce(&mut SpawnOpts)) -> (Session, Tree) {
    let tree = Tree::new();
    let mut opts = SpawnOpts {
        shell: ShellKind::Fish,
        rc_extra: Some(tree.rc(extra)),
        extra_env: vec![(
            "XDG_DATA_HOME".to_owned(),
            tree.0.join("data").display().to_string(),
        )],
        ..SpawnOpts::default()
    };
    tweak(&mut opts);
    (Session::spawn(opts), tree)
}

/// `spawn`, then at the first prompt, fac active, the worker ready.
fn start(extra: &str) -> (Session, Tree) {
    start_with(extra, |_| {})
}

fn start_with(extra: &str, tweak: impl FnOnce(&mut SpawnOpts)) -> (Session, Tree) {
    let (s, tree) = spawn(extra, tweak);
    wait_prompt(&s);
    assert_active(&s);
    wait_log(&s, "worker ready");
    (s, tree)
}

/// fac runs fish with its hook: `FAC_ACTIVE` names this terminal, and the
/// hook turned the trigger on.
fn assert_active(s: &Session) {
    s.send_line("echo active=$FAC_ACTIVE");
    s.wait_for(
        "an active= line",
        |screen| has_line(screen, |line| line.starts_with("active=")),
        TIMEOUT,
    );
    let screen = s.screen_text();
    assert!(
        screen.lines().any(|line| line
            .trim_end()
            .strip_prefix("active=")
            .is_some_and(harness::is_fac_active_value)),
        "fac is not active:\n{screen}\n--- log ---\n{}",
        s.log_text()
    );
    wait_log(s, "decision=run shell=fish");
    wait_log(s, "the shell's report trigger is on");
    wait_prompt(s);
}

/// The popup on the screen: fac's card, and its rows' labels.
fn labels(s: &Session) -> Option<(Card, Vec<String>)> {
    let card = s.card()?;
    let labels = s.with_screen(|screen| harness::labels(screen, &card, &harness::colors()));
    Some((card, labels))
}

fn wait_labels(s: &Session, what: &str, pred: impl Fn(&[String]) -> bool) {
    s.wait_card(
        what,
        |card, screen| {
            card.is_some_and(|card| pred(&harness::labels(screen, card, &harness::colors())))
        },
        TIMEOUT,
    );
}

fn wait_no_popup(s: &Session) {
    s.wait_card(
        "no popup",
        |card, screen| card.is_none() && harness::popup_cells(screen, &harness::colors()) == 0,
        TIMEOUT,
    );
}

/// The row the cursor is on, trimmed.
fn cursor_line(screen: &vt100::Screen) -> String {
    let (row, _) = screen.cursor_position();
    suite::row_text(screen, row).trim_end().to_owned()
}

fn wait_line(s: &Session, expected: &str) {
    s.wait_for(
        &format!("the command line {expected:?}"),
        |screen| cursor_line(screen) == expected,
        TIMEOUT,
    );
}

fn wait_output(s: &Session, text: &str) {
    s.wait_for(
        &format!("a line {text:?}"),
        |screen| has_line(screen, |line| line.trim_end() == text),
        TIMEOUT,
    );
}

/// fac's log has a report of exactly `buffer`.
fn wait_report(s: &Session, buffer: &str) {
    wait_report_after(s, 0, buffer);
}

/// fac's log has a report of exactly `buffer` after its first `skip`.
fn wait_report_after(s: &Session, skip: usize, buffer: &str) {
    let needle = format!(" buffer={buffer:?} ");
    let deadline = Instant::now() + TIMEOUT;
    while !s
        .reports()
        .iter()
        .skip(skip)
        .any(|line| line.contains(&needle))
    {
        assert!(
            Instant::now() < deadline,
            "no report of {buffer:?}\n--- screen ---\n{}\n--- log ---\n{}",
            s.screen_text(),
            s.log_text()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Neither trigger is on the screen.
fn no_trigger_typed(s: &Session) {
    let screen = s.screen_text();
    assert!(
        !screen.contains(TRIGGER) && !screen.contains(APPLY_TRIGGER),
        "{screen}"
    );
}

#[test]
fn startup_shows_the_prompt_and_fac_is_active() {
    let (s, _tree) = start("");
    // The hook's context reached fac, and fish's own prompt marks.
    let log = s.log_text();
    for key in [
        "trigger",
        "apply_trigger",
        "defer",
        "armed",
        "path",
        "cwd",
        "histfile",
        "fish_function_path",
        "fish_complete_path",
        "escape_delay",
        "env",
    ] {
        assert!(
            log.contains(&format!("context key={key} ")),
            "no {key}:\n{log}"
        );
    }
    assert!(log.contains("prompt mark A"), "{log}");
    assert!(log.contains("prompt mark B"), "{log}");
    // The command ran, its line came as a command (7792), with `f` first.
    assert!(
        log.contains("flags=f buffer=\"echo active=$FAC_ACTIVE\""),
        "{log}"
    );
    no_trigger_typed(&s);
}

#[test]
fn git_ch_popup_down_tab_and_esc() {
    let (s, _tree) = start("");
    s.send(b"git ch");
    wait_labels(&s, "git's ch subcommands", |labels| {
        labels.iter().any(|l| l == "checkout") && labels.iter().any(|l| l == "cherry-pick")
    });
    let (_, rows) = labels(&s).expect("the popup");
    // Down selects the first row; Tab inserts it (through the apply pipe
    // and its trigger), as with zsh.
    s.send(DOWN);
    s.wait_card(
        "the first row selected",
        |card, _| card.is_some_and(|card| card.selected == Some(0)),
        TIMEOUT,
    );
    s.send(TAB);
    wait_line(&s, &format!("% git {}", rows[0]));
    wait_log(&s, "apply trigger sent");
    // A new word: Esc closes its popup and leaves the line.
    s.send(b"\x15git ch");
    wait_labels(&s, "the popup again", |labels| {
        labels.iter().any(|l| l == "checkout")
    });
    s.send(ESC);
    wait_no_popup(&s);
    std::thread::sleep(Duration::from_millis(100));
    s.with_screen(|screen| assert_eq!(cursor_line(screen), "% git ch"));
    no_trigger_typed(&s);
}

/// A resize with the popup open: the popup closes and is erased, fish
/// redraws the line, and the next key brings it back.
#[test]
fn a_resize_closes_the_popup_and_the_next_key_brings_it_back() {
    let (s, _tree) = start("");
    s.send(b"git ch");
    wait_labels(&s, "git's ch subcommands", |labels| {
        labels.iter().any(|l| l == "checkout")
    });
    s.resize(30, 100);
    wait_log(&s, "popup closed (resize)");
    wait_no_popup(&s);
    let before = s.reports().len();
    s.send(b"e");
    wait_report_after(&s, before, "git che");
    wait_labels(&s, "the popup again", |labels| {
        labels.iter().any(|l| l == "checkout")
    });
    wait_line(&s, "% git che");
    no_trigger_typed(&s);
}

#[test]
fn ls_dash_shows_the_workers_options_with_descriptions() {
    let (s, _tree) = start("");
    s.send(b"ls -");
    wait_labels(&s, "ls's options", |labels| {
        labels.first().is_some_and(|l| l.starts_with('-'))
    });
    // fish's `complete -C` answered the query, with candidates.
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let log = s.log_text();
        let answered = log
            .lines()
            .any(|line| line.contains("worker done id=") && !line.contains(" n=0 "))
            && log.contains("worker query id=");
        if answered {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the worker never answered:\n{log}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    // The first option selected: its description on the bottom line.
    s.send(DOWN);
    s.wait_card(
        "a description line",
        |card, _| card.is_some_and(|card| card.selected == Some(0) && card.lines > 0),
        TIMEOUT,
    );
}

#[test]
fn a_user_functions_completions_from_its_complete_path() {
    let (s, _tree) = start("");
    s.send(b"myfn ");
    wait_labels(&s, "myfn's args", |labels| {
        labels.iter().any(|l| l == "alpha") && labels.iter().any(|l| l == "beta")
    });
    s.send(DOWN);
    s.wait_for(
        "the arg's description",
        |screen| screen.contents().contains("my arg"),
        TIMEOUT,
    );
    s.send(TAB);
    wait_report(&s, "myfn alpha ");
    s.send(b"\r");
    wait_output(&s, "myfn alpha");
}

#[test]
fn names_with_a_space_and_a_quote_are_quoted_as_fish_reads_them() {
    let (s, _tree) = start("");
    s.send(b"cat a");
    wait_labels(&s, "a b.txt", |labels| {
        labels.iter().any(|l| l == "a b.txt")
    });
    s.send(DOWN);
    s.send(TAB);
    wait_report(&s, "cat a\\ b.txt");
    s.send(b"\r");
    wait_output(&s, "spaced");
    s.send(b"cat it");
    wait_labels(&s, "it's.txt", |labels| {
        labels.iter().any(|l| l == "it's.txt")
    });
    s.send(DOWN);
    s.send(TAB);
    wait_report(&s, "cat it\\'s.txt");
    s.send(b"\r");
    wait_output(&s, "quoted");
    // Inside a quote the name stays in it, closed by fish's rules.
    s.send(b"cat 'it");
    wait_labels(&s, "it's.txt again", |labels| {
        labels.iter().any(|l| l == "it's.txt")
    });
    s.send(DOWN);
    s.send(TAB);
    wait_report(&s, "cat 'it\\'s.txt'");
    s.send(b"\r");
    s.wait_for(
        "cat's output twice",
        |screen| {
            screen
                .contents()
                .lines()
                .filter(|line| line.trim_end() == "quoted")
                .count()
                == 2
        },
        TIMEOUT,
    );
    // A command with no spec: fish's own file matches, whole tokens from
    // `complete -C`, quoted the same way.
    wait_prompt(&s);
    s.send(b"facshow a");
    wait_log(&s, "worker done");
    wait_labels(&s, "fish's a b.txt", |labels| {
        labels.iter().any(|l| l == "a b.txt")
    });
    s.send(DOWN);
    s.send(TAB);
    wait_report_prefix(&s, "facshow a\\ b.txt");
    s.send(b"\r");
    s.wait_for(
        "cat's output twice",
        |screen| {
            screen
                .contents()
                .lines()
                .filter(|line| line.trim_end() == "spaced")
                .count()
                == 2
        },
        TIMEOUT,
    );
}

/// fac's log has a report whose buffer starts with `prefix`.
fn wait_report_prefix(s: &Session, prefix: &str) {
    let needle = format!(" buffer={:?}", prefix);
    let needle = needle.trim_end_matches('"');
    let deadline = Instant::now() + TIMEOUT;
    while !s.reports().iter().any(|line| line.contains(needle)) {
        assert!(
            Instant::now() < deadline,
            "no report starting {prefix:?}\n--- log ---\n{}",
            s.log_text()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn history_lines_are_offered_and_inserted_whole() {
    let (s, _tree) = start("");
    s.send(b"echo fr");
    wait_labels(&s, "the history line", |labels| {
        labels.iter().any(|l| l == "echo from-history")
    });
    // History ranks last: Up from nothing selects it.
    s.send(UP);
    s.wait_card(
        "the history line selected",
        |card, screen| {
            card.is_some_and(|card| {
                let labels = harness::labels(screen, card, &harness::colors());
                card.selected
                    .is_some_and(|at| labels.get(at).is_some_and(|l| l == "echo from-history"))
            })
        },
        TIMEOUT,
    );
    s.send(b"\r");
    wait_line(&s, "% echo from-history");
    // A command run now is offered too, from fish's own file.
    s.send(b"\r");
    wait_output(&s, "from-history");
    s.send_line("echo brand-new-line");
    wait_output(&s, "brand-new-line");
    wait_prompt(&s);
    s.send(b"echo bra");
    wait_labels(&s, "the new history line", |labels| {
        labels.iter().any(|l| l == "echo brand-new-line")
    });
}

#[test]
fn vi_mode_insert_and_command_mode() {
    // No mode indicator before the prompt.
    let (s, _tree) = start("fish_vi_key_bindings\nfunction fish_mode_prompt; end");
    // Insert mode: the popup as in emacs mode.
    s.send(b"git ch");
    wait_labels(&s, "the popup in insert mode", |labels| {
        labels.iter().any(|l| l == "checkout")
    });
    // Esc closes the popup; a second Esc is fish's: command mode, `v`, no
    // popup.
    s.send(ESC);
    wait_no_popup(&s);
    let before = s.reports().len();
    s.send(ESC);
    let deadline = Instant::now() + TIMEOUT;
    while !s
        .reports()
        .iter()
        .skip(before)
        .any(|line| line.contains(" flags=v "))
    {
        assert!(
            Instant::now() < deadline,
            "no report in command mode:\n{}",
            s.log_text()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    // Command mode's keys move; no popup.
    s.send(b"h");
    std::thread::sleep(Duration::from_millis(200));
    assert!(s.card().is_none(), "{}", s.log_text());
    // `A` appends in insert mode again: a new word opens the popup.
    s.send(b"A");
    s.send(b" --");
    wait_labels(&s, "checkout's options", |labels| {
        labels.iter().any(|l| l.starts_with("--"))
    });
    no_trigger_typed(&s);
}

#[test]
fn a_widget_that_reads_the_terminal_gets_no_trigger() {
    // An fzf-style widget: it reads a line from the terminal itself.
    let rc = "function pick\n\
                  read -l k </dev/tty\n\
                  commandline -i \"[$k]\"\n\
              end\n\
              bind ctrl-t pick";
    let (s, _tree) = start(rc);
    wait_log(&s, "trigger defer n=");
    s.send(b"x");
    wait_report(&s, "x");
    for keys in [&b"\x14"[..], b"ab", b"c", b"d", b"\r"] {
        s.send(keys);
        std::thread::sleep(Duration::from_millis(40));
    }
    wait_line(&s, "% x[abcd]");
    // No trigger reached the widget, and fish runs the line as typed.
    no_trigger_typed(&s);
    s.send(b"\x15echo after-widget\r");
    wait_output(&s, "after-widget");
    // The next prompt arms the trigger again.
    let before = s.reports().len();
    s.send(b"y");
    wait_report_after(&s, before, "y");
}

/// A widget key with the popup open: the popup closes at once (fish sends
/// no report until the next prompt, so a card left would be stale), and
/// the Tab after the widget reaches fish at once.
#[test]
fn a_widget_key_closes_the_popup_and_the_next_tab_goes_to_fish() {
    let rc = "function pick\n\
                  read -l k </dev/tty\n\
                  commandline -r \"cat file\"\n\
              end\n\
              bind ctrl-t pick";
    let (s, _tree) = start(rc);
    wait_log(&s, "trigger defer n=");
    s.send(b"git ch");
    wait_labels(&s, "git's ch subcommands", |labels| {
        labels.iter().any(|l| l == "checkout")
    });
    s.send(b"\x14");
    wait_no_popup(&s);
    s.send(b"x\r");
    wait_line(&s, "% cat file");
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        s.card().is_none(),
        "no popup after the widget:\n{}",
        s.screen_text()
    );
    // fish completes the only match itself, with no wait.
    s.send(TAB);
    s.wait_for(
        "fish's own completion of the Tab",
        |screen| cursor_line(screen) == "% cat file-a",
        Duration::from_millis(500),
    );
    no_trigger_typed(&s);
}

#[test]
fn an_apply_trigger_with_no_apply_waiting_does_not_hang() {
    let (s, _tree) = start("");
    let before = s.reports().len();
    // Typed by hand: the binding finds the pipe empty and reports.
    s.send(format!("echo still{APPLY_TRIGGER}").as_bytes());
    wait_report_after(&s, before, "echo still");
    s.send(b"-alive\r");
    wait_output(&s, "still-alive");
    wait_prompt(&s);
}

#[test]
fn cat_and_enter_in_one_write_leak_no_trigger() {
    let (s, _tree) = start("");
    s.send(b"cat\rhello\r");
    s.wait_for(
        "cat's copy of the line",
        |screen| {
            screen
                .contents()
                .lines()
                .filter(|line| line.trim_end() == "hello")
                .count()
                == 2
        },
        TIMEOUT,
    );
    s.send(b"\x04");
    wait_prompt(&s);
    // A command typed ahead, still queued when the prompt comes back (with
    // the replies to fish's prompt queries), gets no trigger either.
    s.send(b"sleep 0.3\rcat\r");
    s.wait_for(
        "cat running",
        |screen| {
            screen
                .contents()
                .lines()
                .filter(|l| l.trim_end() == "% cat")
                .count()
                == 2
        },
        TIMEOUT,
    );
    std::thread::sleep(Duration::from_millis(200));
    s.send(b"typed\r");
    s.wait_for(
        "cat's copy of the typed line",
        |screen| {
            screen
                .contents()
                .lines()
                .filter(|line| line.trim_end() == "typed")
                .count()
                == 2
        },
        TIMEOUT,
    );
    s.send(b"\x04");
    wait_prompt(&s);
    let screen = s.screen_text();
    assert!(!screen.contains("^["), "{screen}");
    no_trigger_typed(&s);
}

/// Two lines typed while `sleep 1` runs, then `abc`: fish reads it all
/// ahead at the next prompt and keeps `cat` (and `abc`) in a buffer of its
/// own, where no look at its input finds them. No trigger goes until it has
/// run both lines: nothing reaches cat, which ends at once on Ctrl-D. The
/// prompt after cat holds `abc` as its line, and the trigger is back.
#[test]
fn no_trigger_while_fish_holds_a_line_typed_ahead() {
    let (s, _tree) = start("");
    s.send(b"sleep 1\r");
    std::thread::sleep(Duration::from_millis(200));
    s.send(b"true\rcat\r");
    std::thread::sleep(Duration::from_millis(100));
    s.send(b"abc");
    wait_log(&s, "command \"cat\"");
    std::thread::sleep(Duration::from_millis(500));
    let before = s.reports().len();
    s.send(b"\x04");
    wait_report_after(&s, before, "abc");
    wait_line(&s, "% abc");
    no_trigger_typed(&s);
    send_report_fish(&s, b"x", "abcx");
    no_trigger_typed(&s);
}

/// A binding to `execute` accepts the line: the hook names its key `a`
/// in `defer`, so typed ahead it is an Enter.
#[test]
fn a_key_bound_to_execute_is_an_accept_key() {
    // Run at the first prompt, once fac.fish has loaded (no typing, so no
    // popup can take the Enter).
    let (s, tree) = start(
        "bind ctrl-x execute\n\
         function __t --on-event fish_prompt\n\
         functions -e __t\n\
         __fac_class '' execute >o1\n\
         __fac_class '' 'commandline -f repaint execute' >>o1\n\
         end",
    );
    let path = tree.cwd().join("o1");
    let deadline = Instant::now() + TIMEOUT;
    while std::fs::read_to_string(&path).unwrap_or_default() != "a\na\n" {
        assert!(
            Instant::now() < deadline,
            "o1 is {:?}\n--- screen ---\n{}",
            std::fs::read_to_string(&path),
            s.screen_text()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The reviewer's case with fac's binary deleted mid-session (brew
/// upgrade, then cleanup): fish's look used to run it, gave up, and left
/// the trigger off for good after an Enter typed into cat. fac looks from
/// its own end now; the hook runs nothing (`__fac_bin` is not looked at).
#[test]
fn the_trigger_is_back_after_an_enter_typed_into_cat_whatever_the_binary() {
    let (s, _tree) = start("");
    s.send(b"set -g __fac_bin /nonexistent/fac\r");
    wait_prompt(&s);
    s.send(b"cat\r");
    wait_log(&s, "command \"cat\"");
    std::thread::sleep(Duration::from_millis(200));
    s.send(b"hello\r");
    wait_output(&s, "hello");
    std::thread::sleep(Duration::from_millis(200));
    s.send(b"\x04");
    wait_prompt(&s);
    send_report_fish(&s, b"x", "x");
    no_trigger_typed(&s);
}

/// Typing, editing, history, paste and Tab: each gets a report of the line.
#[test]
fn edits_are_reported() {
    let (s, tree) = start("");
    std::fs::write(tree.cwd().join("zebra-file"), "").expect("write");
    for (keys, line) in [
        (&b"echo hi"[..], "echo hi"),
        (" é".as_bytes(), "echo hi é"),
        (" שלום".as_bytes(), "echo hi é שלום"),
        (" 🙂".as_bytes(), "echo hi é שלום 🙂"),
        (b"\x7f\x7f", "echo hi é שלום"),
        (b"\x01", "echo hi é שלום"),
        (b"\x05x", "echo hi é שלוםx"),
    ] {
        send_report_fish(&s, keys, line);
    }
    send_report_fish(&s, b"\x15", "");
    // History, with the next line typed before its prompt: no trigger
    // reaches `echo`.
    s.send(b"\r");
    s.send(b"echo from-history\r");
    wait_output(&s, "from-history");
    send_report_fish(&s, b"\x1b[A", "echo from-history");
    send_report_fish(&s, b"\x15", "");
    // A paste.
    send_report_fish(&s, b"\x1b[200~echo pasted\x1b[201~", "echo pasted");
    send_report_fish(&s, b"\x15", "");
    // C-w.
    send_report_fish(&s, b"echo one two", "echo one two");
    send_report_fish(&s, b"\x17", "echo one ");
    send_report_fish(&s, b"\x15", "");
    // fish's own Tab (the popup closed with Esc first): it completes.
    send_report_fish(&s, b"ls zeb", "ls zeb");
    s.wait_card("the popup", |card, _| card.is_some(), TIMEOUT);
    s.send(b"\x1b");
    wait_no_popup(&s);
    s.send(b"\t");
    wait_line(&s, "% ls zebra-file");
    send_report_fish(&s, b"x", "ls zebra-file x");
    no_trigger_typed(&s);
}

/// Keys sent one by one while each report's binding is still running
/// (fac.fish's `__fac_report` wrapped, once the hook has loaded, in one
/// that sleeps 20 ms first): the line is what was typed, in order, and
/// nothing is added to it.
#[test]
fn keys_sent_while_the_report_binding_sleeps_reach_the_line_in_order() {
    let rc = "function __t_wrap --on-event fish_prompt\n\
                  set -q __t_wrapped; and return\n\
                  set -g __t_wrapped 1\n\
                  functions -c __fac_report __t_orig_report\n\
                  function __fac_report\n\
                      sleep 0.02\n\
                      __t_orig_report $argv\n\
                  end\n\
                  touch $HOME/slow-wrapped\n\
              end";
    let (s, _tree) = start(rc);
    assert!(s.home().join("slow-wrapped").exists(), "the wrapper ran");
    let typed = "echo burst-of-keys-é";
    for c in typed.chars() {
        s.send(c.to_string().as_bytes());
        std::thread::sleep(Duration::from_millis(3));
    }
    wait_line(&s, &format!("% {typed}"));
    s.send(b"\r");
    wait_output(&s, "burst-of-keys-é");
    wait_prompt(&s);
    no_trigger_typed(&s);
    send_report_fish(&s, b"x", "x");
    no_trigger_typed(&s);
}

/// Two empty lines in one write: the second is typed before fish has its
/// prompt's query reply, so fish may hold it, but taking it (a new prompt
/// with no 133;C) counts: the popup is back at the prompt after it.
#[test]
fn empty_lines_typed_ahead_leave_the_trigger_on() {
    let (s, _tree) = start("");
    s.send(b"\r\r");
    wait_log(&s, "no line typed ahead is queued");
    send_report_fish(&s, b"x", "x");
    no_trigger_typed(&s);
}

/// Send `keys`, then wait for a report of exactly `buffer` after them.
fn send_report_fish(s: &Session, keys: &[u8], buffer: &str) {
    let before = s.reports().len();
    s.send(keys);
    wait_report_after(s, before, buffer);
}

/// With kitty's keyboard protocol on (fish turns it on when the terminal
/// answers its flags query), Ctrl-M comes as `CSI 109 ; 5 u`, which fish
/// binds to `execute`: no trigger follows it into what the line runs.
#[test]
fn kitty_ctrl_m_accepts_and_cat_gets_only_hello() {
    let (s, _tree) = start_with("", |opts| opts.answer_fish_queries = true);
    let raw = s.raw_output();
    assert!(
        raw.windows(5).any(|w| w == b"\x1b[=5u"),
        "fish turned kitty's keys on: {}",
        String::from_utf8_lossy(&raw)
    );
    s.send(b"cat\x1b[109;5u");
    s.wait_for(
        "cat running",
        |screen| {
            has_line(screen, |line| line.trim_end() == "% cat") && cursor_line(screen).is_empty()
        },
        TIMEOUT,
    );
    std::thread::sleep(Duration::from_millis(200));
    s.send(b"hello\r");
    s.wait_for(
        "cat's copy of the line",
        |screen| {
            screen
                .contents()
                .lines()
                .filter(|line| line.trim_end() == "hello")
                .count()
                == 2
        },
        TIMEOUT,
    );
    s.send(b"\x04");
    wait_prompt(&s);
    no_trigger_typed(&s);
}

/// fish's startup queries (kitty's flags, XTVERSION, OSC 11, XTGETTCAP,
/// DA1), sent while fac's own probe is still out, and its queries at each
/// prompt (OSC 11, CPR, DA1): every reply reaches fish, whole and in order
/// (fish shows what it read), and fish's brief alternate screen at startup
/// leaves fac on for the session.
fn queries_get_their_replies() -> bool {
    let light = (0xf5, 0xf5, 0xf5);
    let (s, _tree) = start_with("", |opts| {
        opts.answer_fish_queries = true;
        // Every reply waits for fish's DA1, so fish's OSC 11 and DA1 are
        // asked while fac's are.
        opts.slow_start = Some(Duration::from_millis(800));
        opts.background = light;
    });
    s.send_line(
        "echo T=(status terminal) C=$fish_terminal_color_theme; \
         status test-terminal-feature scroll-content-up; and echo scroll=yes",
    );
    wait_output(&s, "T=fac-harness 1.0 C=light");
    wait_output(&s, "scroll=yes");
    // kitty's flags came back: fish turns its keyboard protocol on.
    let raw = s.raw_output();
    assert!(
        raw.windows(5).any(|w| w == b"\x1b[=5u"),
        "{}",
        String::from_utf8_lossy(&raw)
    );
    // The alternate screen, on and off before the first prompt.
    let log = s.log_text();
    let on = log.find("alt_screen=on").expect("alt screen on");
    let off = log.find("alt_screen=off").expect("alt screen off");
    let prompt = log.find("prompt mark B").expect("a prompt");
    assert!(on < off && off < prompt, "{log}");
    // Prompts' queries: their replies typed nothing, and fac still pops up.
    for n in 0..3 {
        s.send_line(&format!("echo prompt-{n}"));
        wait_output(&s, &format!("prompt-{n}"));
        wait_prompt(&s);
    }
    s.send(b"git ch");
    // (Painted for the light background: its colours are not the default.)
    s.wait_card("the popup", |card, _| card.is_some(), TIMEOUT);
    let screen = s.screen_text();
    assert!(screen.contains("checkout"), "{screen}");
    for residue in ["rgb:", "62;", "$y", "^[", "fac-harness 1.0\n%"] {
        assert!(!screen.contains(residue), "{residue:?} in\n{screen}");
    }
    let log = s.log_text();
    assert!(
        log.lines()
            .filter(|line| line.contains("report seq=") && !line.contains(" buffer=\"\" "))
            .all(
                |line| ["echo T=", "echo prompt-", "git ch", "g", "echo active="]
                    .iter()
                    .any(|typed| line.contains(&format!("buffer=\"{typed}")))
            ),
        "a report of a line not typed:\n{log}"
    );
    assert!(log.contains("cpr forwarded to the shell"), "{log}");
    // fac told fish's startup replies from its own probe's.
    let forwarded = |kind: &str| {
        log.lines().any(|line| {
            line.contains("reply forwarded to the shell") && line.contains(&format!("kind={kind}"))
        })
    };
    forwarded("Some(Background)") && forwarded("Some(DeviceAttrs)")
}

#[test]
fn the_terminal_queries_get_their_replies() {
    // fish must ask while fac's probe is out; a cold start can miss that.
    for attempt in 1..=3 {
        if queries_get_their_replies() {
            return;
        }
        eprintln!("attempt {attempt}: fish asked after fac's probe was answered");
    }
    panic!("fish never asked while fac's probe was out, in 3 attempts");
}

/// fish with fac.fish on a PTY of its own, without fac: the test writes
/// fac's applies to the command pipe itself.
struct Direct {
    master: Option<OwnedFd>,
    pid: Pid,
    output: Vec<u8>,
    commands: OwnedFd,
    home: PathBuf,
    token: String,
}

impl Direct {
    fn start() -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let home = std::env::temp_dir().join(format!("fac-fish-direct-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let config = home.join(".config/fish/config.fish");
        std::fs::create_dir_all(config.parent().expect("a dir")).expect("mkdir");
        std::fs::write(
            &config,
            "set -g fish_greeting\nset -g fish_history ''\n\
             set -g fish_autosuggestion_enabled 0\nfunction fish_prompt; printf '%% '; end\n",
        )
        .expect("config.fish");
        for dir in [
            ".local/share/fish/generated_completions",
            ".cache/fish/generated_completions",
        ] {
            std::fs::create_dir_all(home.join(dir)).expect("mkdir");
        }
        let shell = harness::shell_path(ShellKind::Fish);
        let setup = install::prepare_in(
            ShellKind::Fish,
            &shell,
            &home.join("Library/Caches/fac/shell"),
            None,
        )
        .expect("writing the scripts");
        let mut env: Vec<(OsString, OsString)> = vec![
            ("HOME".into(), home.clone().into()),
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("TERM".into(), "xterm-256color".into()),
            ("LANG".into(), "en_US.UTF-8".into()),
            ("XDG_CONFIG_HOME".into(), home.join(".config").into()),
            ("XDG_DATA_HOME".into(), home.join(".local/share").into()),
            ("XDG_CACHE_HOME".into(), home.join(".cache").into()),
        ];
        env.extend(fac::guards::child_env(std::process::id(), "/dev/ttys999"));
        env.extend(setup.env);
        let size = libc::winsize {
            ws_row: 24,
            ws_col: 200,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let child = fac::pty::spawn_process(
            &shell,
            &setup.launch.args,
            &setup.launch.argv0,
            &env,
            size,
            vec![(setup.cmd_reader, install::CMD_FD)],
        )
        .expect("spawning fish");
        Self {
            master: Some(child.master),
            pid: child.pid,
            output: Vec::new(),
            commands: setup.cmd_writer,
            home,
            token: setup.token.as_str().to_owned(),
        }
    }

    fn master(&self) -> &OwnedFd {
        self.master.as_ref().expect("the master is open")
    }

    fn send(&self, bytes: &[u8]) {
        let mut rest = bytes;
        while !rest.is_empty() {
            match nix::unistd::write(self.master(), rest) {
                Ok(n) => rest = &rest[n..],
                Err(Errno::EAGAIN | Errno::EINTR) => std::thread::sleep(Duration::from_millis(1)),
                Err(err) => panic!("writing to fish: {err}"),
            }
        }
    }

    /// An apply on the command pipe, as fac writes it.
    fn apply(&self, line: &[u8]) {
        nix::unistd::write(&self.commands, line).expect("writing the apply");
    }

    /// The reports so far: (seq, buffer). Each carries the session's
    /// token, as fac.fish took it from the command pipe.
    fn reports(&self) -> Vec<(u64, String)> {
        let text = String::from_utf8_lossy(&self.output);
        text.split("\x1b]7790;1:")
            .skip(1)
            .filter_map(|rest| {
                let body = rest.split('\x07').next()?;
                let fields: Vec<&str> = body.split(';').collect();
                assert_eq!(fields.first(), Some(&self.token.as_str()), "the token");
                let buffer = percent_decode(fields.get(6)?.as_bytes()).ok()?;
                Some((
                    fields.get(1)?.parse().ok()?,
                    String::from_utf8(buffer).ok()?,
                ))
            })
            .collect()
    }

    /// Read until a report newer than `after` comes; it is returned.
    fn next_report(&mut self, after: u64) -> (u64, String) {
        let deadline = Instant::now() + TIMEOUT;
        let mut buf = [0u8; 4096];
        loop {
            if let Some(report) = self.reports().into_iter().rfind(|(seq, _)| *seq > after) {
                return report;
            }
            assert!(
                Instant::now() < deadline,
                "no report after {after}:\n{}",
                String::from_utf8_lossy(&self.output)
            );
            let mut fds = [PollFd::new(self.master().as_fd(), PollFlags::POLLIN)];
            match poll(&mut fds, PollTimeout::from(100u16)) {
                Ok(0) | Err(Errno::EINTR) => continue,
                Ok(_) => {}
                Err(err) => panic!("poll: {err}"),
            }
            let n = match nix::unistd::read(self.master(), &mut buf) {
                Ok(n) => n,
                Err(Errno::EAGAIN | Errno::EINTR) => continue,
                Err(err) => panic!("reading fish's output: {err}"),
            };
            let read = &buf[..n];
            for (query, reply) in [
                (&b"\x1b[0c"[..], &b"\x1b[?62;22c"[..]),
                (b"\x1b[6n", b"\x1b[1;1R"),
            ] {
                if read.windows(query.len()).any(|window| window == query) {
                    self.send(reply);
                }
            }
            self.output.extend_from_slice(read);
        }
    }
}

impl Drop for Direct {
    fn drop(&mut self) {
        let _ = kill(self.pid, Signal::SIGKILL);
        self.master = None;
        let _ = waitpid(self.pid, None);
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

/// The apply trigger's reader never waits: with no apply on the pipe, with
/// stale ones (an old seq, a line that is not fish's), and with several
/// waiting, of which it applies the one for its latest report.
#[test]
fn applies_stale_or_missing_never_hang_the_prompt() {
    let mut fish = Direct::start();
    // fac's first trigger of the line.
    fish.send(TRIGGER.as_bytes());
    let (seq, line) = fish.next_report(0);
    assert_eq!(line, "");
    fish.send(format!("echo ab{TRIGGER}").as_bytes());
    let (seq, line) = fish.next_report(seq);
    assert_eq!(line, "echo ab");
    // No apply waiting.
    fish.send(APPLY_TRIGGER.as_bytes());
    let (seq, line) = fish.next_report(seq);
    assert_eq!(line, "echo ab");
    // An older report's apply.
    fish.apply(&encode_apply(seq - 1, "echo stale", "", Some("echo ab")));
    fish.send(APPLY_TRIGGER.as_bytes());
    let (seq, line) = fish.next_report(seq);
    assert_eq!(line, "echo ab");
    // The latest report's seq, but a line fish does not hold.
    fish.apply(&encode_apply(seq, "echo stale", "", Some("echo other")));
    fish.send(APPLY_TRIGGER.as_bytes());
    let (seq, line) = fish.next_report(seq);
    assert_eq!(line, "echo ab");
    // Several waiting: the stale ones are dropped, the right one applied,
    // its text decoded exactly (a space, a backslash, a newline, UTF-8).
    let text = "echo a\\b é\nnext";
    fish.apply(
        &[
            encode_apply(1, "echo one", "", Some("echo ab")),
            encode_apply(seq, text, " tail", Some("echo ab")),
        ]
        .concat(),
    );
    fish.send(APPLY_TRIGGER.as_bytes());
    let (_, line) = fish.next_report(seq);
    assert_eq!(line, format!("{text} tail"));
}

/// The snapshot of `git ch` and Down, under `profile`.
fn git_ch_down(profile: Profile) -> String {
    let (s, _tree) = start_with("", |opts| opts.set_profile(profile));
    s.send(b"\x0c");
    s.wait_for(
        "a prompt on the first row",
        |screen| screen.cursor_position() == (0, 2) && screen.contents().trim_end() == "%",
        TIMEOUT,
    );
    let colors = harness::profile_colors(profile);
    let mut out = String::new();
    for (name, keys) in [("git ch", &b"git ch"[..]), ("Down", DOWN)] {
        let log_before = s.log_text().len();
        let since = Instant::now();
        s.send(keys);
        suite::settle(&s, name, since);
        suite::check_invariants_with(&s, name, log_before, &colors);
        let card = s.card();
        let _ = writeln!(out, "--- after {name} ---");
        s.with_screen(|screen| out.push_str(&suite::render_with(card.as_ref(), screen, &colors)));
    }
    out
}

#[test]
fn fish_git_ch_down_snapshots() {
    insta::assert_snapshot!("fish_git_ch_down_ghostty", git_ch_down(Profile::Ghostty));
    insta::assert_snapshot!("fish_git_ch_down_apple", git_ch_down(Profile::Apple));
}
