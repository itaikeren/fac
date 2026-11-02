//! fac with bash 5.3 and its real hook (`shell/bash/fac.bash`), end to end
//! in the harness's terminal. The suggestions here come from Fig's specs,
//! history and files; bash's own completions are tests/bash_worker.rs.

mod harness;
mod suite;

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use harness::{Profile, Session, ShellKind, SpawnOpts, TIMEOUT, assert_active, has_line, wait_log};

const DOWN: &[u8] = b"\x1b[B";
const ESC: &[u8] = b"\x1b";
const TAB: &[u8] = b"\t";
const CPR_QUERY: &[u8] = b"\x1b[6n";

fn hook() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("shell/bash/fac.bash")
}

/// A bash session's own tree: a cwd and a bin dir, in a temp dir removed on
/// drop.
struct Dirs(PathBuf);

impl Dirs {
    fn new() -> Self {
        static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("fac-bash-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("cwd")).expect("creating the cwd");
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).expect("creating the bin dir");
        // The only commands on PATH besides the system's, so command-name
        // lists stay short and the same.
        for (name, target) in [("ls", "/bin/ls"), ("git", "/usr/bin/git")] {
            std::os::unix::fs::symlink(target, bin.join(name)).expect("linking a command");
        }
        Dirs(root)
    }

    fn cwd(&self) -> PathBuf {
        self.0.join("cwd")
    }

    fn bin(&self) -> PathBuf {
        self.0.join("bin")
    }

    fn touch(&self, name: &str) {
        let path = self.cwd().join(name);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).expect("mkdir");
        }
        std::fs::write(path, "").expect("writing a fixture file");
    }
}

impl Drop for Dirs {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// How a session starts.
struct Start {
    /// Appended to the fixture `.bash_profile`, after the cd and PATH.
    rc: String,
    /// PATH is only the session's bin dir (for snapshots), else the bin
    /// dir and the system's.
    only_bin: bool,
    profile: Option<Profile>,
    rows: u16,
    cols: u16,
}

impl Default for Start {
    fn default() -> Self {
        Start {
            rc: String::new(),
            only_bin: false,
            profile: None,
            rows: 24,
            cols: 80,
        }
    }
}

/// fac running bash in `dirs`' cwd, at its first prompt with the line's
/// first report in.
fn spawn(dirs: &Dirs, start: Start) -> Session {
    let path = if start.only_bin {
        dirs.bin().display().to_string()
    } else {
        format!("{}:/usr/bin:/bin", dirs.bin().display())
    };
    let rc = format!("cd '{}'\nPATH='{path}'\n{}", dirs.cwd().display(), start.rc);
    let mut opts = match start.profile {
        Some(profile) => SpawnOpts::with_profile(profile),
        None => SpawnOpts::default(),
    };
    opts.shell = ShellKind::Bash;
    opts.rows = start.rows;
    opts.cols = start.cols;
    opts.rc_extra = Some(rc);
    let s = Session::spawn(opts);
    wait_log(&s, "the shell's report trigger is on");
    wait_report(&s, 0, |r| r.contains(" buffer=\"\" "));
    s
}

/// A report after the first `skip` that `pred` takes.
fn wait_report(s: &Session, skip: usize, pred: impl Fn(&str) -> bool) {
    let deadline = Instant::now() + TIMEOUT;
    while !s.reports().iter().skip(skip).any(|line| pred(line)) {
        assert!(
            Instant::now() < deadline,
            "no such report\n--- screen ---\n{}\n--- reports ---\n{}\n--- log ---\n{}",
            s.screen_text(),
            s.reports().join("\n"),
            s.log_text()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Send `keys`, then wait for a report of exactly `buffer` after them.
fn send_report(s: &Session, keys: &[u8], buffer: &str) {
    let before = s.reports().len();
    s.send(keys);
    let needle = format!(" buffer={buffer:?} ");
    wait_report(s, before, |line| line.contains(&needle));
}

/// Type `text` a character at a time, `gap` apart.
fn type_slowly(s: &Session, text: &str, gap: Duration) {
    for c in text.chars() {
        s.send(c.to_string().as_bytes());
        std::thread::sleep(gap);
    }
}

/// A line of the screen is `text`.
fn wait_line(s: &Session, text: &str) {
    s.wait_for(
        &format!("a line {text:?}"),
        |screen| has_line(screen, |line| line.trim_end() == text),
        TIMEOUT,
    );
}

fn wait_popup(s: &Session) {
    s.wait_card("the popup", |card, _| card.is_some(), TIMEOUT);
}

fn wait_no_popup(s: &Session) {
    s.wait_card("no popup", |card, _| card.is_none(), TIMEOUT);
}

/// The popup's labels, as the screen shows them.
fn popup_labels(s: &Session) -> Vec<String> {
    let card = s.card().expect("a popup");
    let colors = harness::colors();
    s.with_screen(|screen| harness::labels(screen, &card, &colors))
}

/// Wait until the popup lists an item `pred` takes.
fn wait_label(s: &Session, what: &str, pred: impl Fn(&str) -> bool) {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if s.card().is_some() && popup_labels(s).iter().any(|label| pred(label)) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the popup never listed {what}\n--- screen ---\n{}\n--- log ---\n{}",
            s.screen_text(),
            s.log_text()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// No trigger, and nothing of one, ever reached the screen.
fn no_trigger_on_screen(s: &Session) {
    let screen = s.screen_text();
    assert!(
        !screen.contains("9999") && !screen.contains("9998") && !screen.contains("^["),
        "{screen}"
    );
}

#[test]
fn startup_shows_the_prompt_and_fac_is_active() {
    let dirs = Dirs::new();
    let s = spawn(&dirs, Start::default());
    wait_line(&s, "%");
    assert_active(&s);
    // The hook announced everything the trigger and the worker need.
    let log = s.log_text();
    for key in [
        "trigger defer n=",
        "context key=path ",
        "context key=cwd ",
        "context key=env ",
        "context key=histfile ",
        // Sent even empty: the completion worker starts on it.
        "context key=complete len=0",
        "context key=escape_delay ",
    ] {
        assert!(log.contains(key), "no {key:?} in the log:\n{log}");
    }
    // No bash-completion here, so no loader to report.
    assert!(!log.contains("context key=bash_completion "), "{log}");
    no_trigger_on_screen(&s);
}

/// bash-completion's loader, from the clone `tools/test-shells.sh` makes:
/// `FAC_TEST_BASH_COMPLETION` (as `tests/bash_worker.rs` takes it), else
/// the one in `target/test-shells/`. A missing one fails the test.
fn bash_completion_loader() -> PathBuf {
    let path = std::env::var_os("FAC_TEST_BASH_COMPLETION")
        .filter(|path| !path.is_empty())
        .map_or_else(
            || {
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("target/test-shells/bash-completion/bash_completion")
            },
            PathBuf::from,
        );
    assert!(
        path.is_file(),
        "no bash-completion at {} (FAC_TEST_BASH_COMPLETION): run tools/test-shells.sh",
        path.display()
    );
    path
}

/// With bash-completion sourced: its loader's file, and the specs it has
/// (a spec the rc adds, and the ones it loads as they are used).
#[test]
fn the_loader_and_the_specs_are_reported() {
    let loader = bash_completion_loader();
    let dirs = Dirs::new();
    let s = spawn(
        &dirs,
        Start {
            rc: format!(
                "source '{}'\ncomplete -W 'alpha beta' mytool",
                loader.display()
            ),
            ..Start::default()
        },
    );
    let log = s.log_text();
    let path_len = loader.display().to_string().chars().count();
    assert!(
        log.contains(&format!("context key=bash_completion len={path_len}\n")),
        "{log}"
    );
    let specs = |log: &str| {
        log.lines()
            .filter_map(|line| line.split("context key=complete len=").nth(1))
            .filter_map(|n| n.trim().parse::<usize>().ok())
            .collect::<Vec<_>>()
    };
    let first = *specs(&log).last().expect("complete was sent");
    assert!(first >= 2, "the default loader and mytool: {first}");
    // `complete` is sent again when a spec is added.
    s.send(b"complete -W x othertool\r");
    let deadline = Instant::now() + TIMEOUT;
    while specs(&s.log_text()).last() == Some(&first) {
        assert!(Instant::now() < deadline, "{}", s.log_text());
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(specs(&s.log_text()).last(), Some(&(first + 1)));
}

#[test]
fn git_ch_down_tab_and_esc() {
    let dirs = Dirs::new();
    let s = spawn(&dirs, Start::default());
    send_report(&s, b"git ch", "git ch");
    wait_label(&s, "checkout", |label| label.starts_with("checkout"));
    // Nothing is selected until Down; Tab then takes the first item.
    let first = popup_labels(&s)[0].clone();
    let name = first.split_whitespace().next().expect("a label").to_owned();
    s.send(DOWN);
    s.wait_card(
        "the first item selected",
        |card, _| card.is_some_and(|card| card.selected == Some(0)),
        TIMEOUT,
    );
    // (With or without a space after it, by Fig's rule for the item.)
    let before = s.reports().len();
    s.send(TAB);
    let inserted = format!(" buffer=\"git {name}");
    wait_report(&s, before, |r| r.contains(&inserted));
    wait_line(&s, &format!("% git {name}"));
    // Esc closes the popup the space opened, if it did.
    s.send(ESC);
    wait_no_popup(&s);
    no_trigger_on_screen(&s);
}

/// C-r opens bash's own search, which reads the keys (no report comes):
/// the popup closes, and no trigger reaches the search.
#[test]
fn ctrl_r_closes_the_popup() {
    let dirs = Dirs::new();
    let s = spawn(&dirs, Start::default());
    send_report(&s, b"git ch", "git ch");
    wait_popup(&s);
    s.send(b"\x12");
    wait_no_popup(&s);
    s.wait_for(
        "bash's search",
        |screen| has_line(screen, |line| line.contains("reverse-i-search")),
        TIMEOUT,
    );
    no_trigger_on_screen(&s);
}

/// Ctrl-C drops the line: the popup closes, no trigger follows the key
/// (bash runs PROMPT_COMMAND with the terminal echoing), and the next
/// prompt gets its first trigger.
#[test]
fn ctrl_c_closes_the_popup_and_the_next_prompt_reports() {
    let dirs = Dirs::new();
    let s = spawn(&dirs, Start::default());
    send_report(&s, b"git ch", "git ch");
    wait_popup(&s);
    let from = s.log_text().len();
    let before = s.reports().len();
    s.send(b"\x03");
    wait_no_popup(&s);
    wait_report(&s, before, |r| r.contains(" buffer=\"\" "));
    let log = s.log_text();
    let after = &log[from..];
    assert!(!after.contains("trigger sent after a key"), "{after}");
    assert!(
        after.contains("trigger sent at the line's start"),
        "{after}"
    );
    send_report(&s, b"x", "x");
    no_trigger_on_screen(&s);
}

/// bash's `Display all 200 possibilities? (y or n)`: readline reads the
/// answer itself, so no trigger types into it (each of its bytes rang the
/// bell); the line's trigger goes once it is answered.
#[test]
fn no_trigger_into_bashs_display_all_question() {
    let dirs = Dirs::new();
    for i in 0..200 {
        dirs.touch(&format!("big/file-{i}"));
    }
    let s = spawn(&dirs, Start::default());
    send_report(&s, b"ls big/file-", "ls big/file-");
    wait_popup(&s);
    s.send(ESC);
    wait_no_popup(&s);
    let from = s.log_text().len();
    s.send(b"\t\t");
    s.wait_for(
        "bash's question",
        |screen| has_line(screen, |line| line.contains("possibilities? (y or n)")),
        TIMEOUT,
    );
    std::thread::sleep(Duration::from_millis(400));
    let log = s.log_text();
    assert!(!log[from..].contains("trigger sent"), "{}", &log[from..]);
    let before = s.reports().len();
    s.send(b"n");
    wait_report(&s, before, |r| r.contains(" buffer=\"ls big/file-\" "));
    no_trigger_on_screen(&s);
}

/// A resize with the popup open: the popup closes and is erased, bash
/// redraws the line, and the next key brings it back.
#[test]
fn a_resize_closes_the_popup_and_the_next_key_brings_it_back() {
    let dirs = Dirs::new();
    let s = spawn(&dirs, Start::default());
    send_report(&s, b"git ch", "git ch");
    wait_label(&s, "checkout", |label| label.starts_with("checkout"));
    s.resize(30, 100);
    wait_log(&s, "popup closed (resize)");
    wait_no_popup(&s);
    send_report(&s, b"e", "git che");
    wait_label(&s, "checkout again", |label| label.starts_with("checkout"));
    wait_line(&s, "% git che");
    no_trigger_on_screen(&s);
}

#[test]
fn ls_dash_shows_options() {
    let dirs = Dirs::new();
    let s = spawn(&dirs, Start::default());
    send_report(&s, b"ls -", "ls -");
    wait_label(&s, "an option", |label| label.starts_with("-1"));
    assert!(
        popup_labels(&s).iter().all(|label| label.starts_with('-')),
        "{:?}",
        popup_labels(&s)
    );
}

#[test]
fn names_with_a_space_or_a_quote_are_quoted() {
    let dirs = Dirs::new();
    dirs.touch("my file");
    dirs.touch("it's");
    let s = spawn(&dirs, Start::default());
    send_report(&s, b"cat my", "cat my");
    wait_label(&s, "my file", |label| label.starts_with("my file"));
    send_report(&s, TAB, "cat my\\ file");
    send_report(&s, b" it", "cat my\\ file it");
    wait_label(&s, "it's", |label| label.starts_with("it's"));
    send_report(&s, TAB, "cat my\\ file it\\'s");
    // bash takes the quoting: both files exist, and cat says nothing.
    let from = s.log_text().len();
    s.send(b"\r");
    wait_log(&s, "prompt mark D");
    let deadline = Instant::now() + TIMEOUT;
    while !s.log_text()[from..].contains("prompt mark D(") {
        assert!(Instant::now() < deadline, "{}", s.log_text());
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        s.log_text()[from..].contains("prompt mark D(Some(0))"),
        "{}",
        s.log_text()
    );
    let screen = s.screen_text();
    assert!(!screen.contains("No such file"), "{screen}");
}

/// History from HISTFILE, in bash's two formats.
fn history_is_offered(file: &str, rc: &str) {
    let dirs = Dirs::new();
    let hist = dirs.0.join("history");
    std::fs::write(&hist, file).expect("writing the history");
    let s = spawn(
        &dirs,
        Start {
            rc: format!("HISTFILE='{}'\n{rc}", hist.display()),
            ..Start::default()
        },
    );
    send_report(&s, b"git com", "git com");
    wait_label(&s, "the history item", |label| {
        label.starts_with("git commit -m first")
    });
    send_report(&s, b"\x15", "");
    wait_no_popup(&s);
}

#[test]
fn history_without_timestamps() {
    history_is_offered("ls -la\ngit commit -m first\necho done\n", "");
}

#[test]
fn history_with_timestamps() {
    history_is_offered(
        "#1700000000\nls -la\n#1700000100\ngit commit -m first\n#1700000200\necho done\n",
        "HISTTIMEFORMAT='%F %T '",
    );
}

#[test]
fn vi_mode_insert_and_command() {
    let dirs = Dirs::new();
    let s = spawn(
        &dirs,
        Start {
            rc: "set -o vi".to_owned(),
            ..Start::default()
        },
    );
    send_report(&s, b"git ch", "git ch");
    wait_popup(&s);
    // Esc closes the popup; a second goes to bash: vi command mode, whose
    // reports have `v` (after readline's own wait for more of an escape).
    s.send(ESC);
    wait_no_popup(&s);
    let before = s.reports().len();
    s.send(ESC);
    wait_report(&s, before, |r| {
        r.contains("flags=v ") && r.contains(" buffer=\"git ch\" ")
    });
    // `x` deletes in command mode, with no popup; `a` appends again.
    let before = s.reports().len();
    s.send(b"x");
    wait_report(&s, before, |r| {
        r.contains("flags=v ") && r.contains(" buffer=\"git c\" ")
    });
    assert!(s.card().is_none());
    let before = s.reports().len();
    s.send(b"a");
    std::thread::sleep(Duration::from_millis(40));
    s.send(b"h");
    wait_report(&s, before, |r| {
        r.contains("flags= ") && r.contains(" buffer=\"git ch\" ")
    });
    // (Esc keeps the popup closed for this word, as in Fig.)
    assert!(s.card().is_none());
    // A count and an operator wait for the key after them: `0`, `2dw`
    // deletes the two words, and nothing of a trigger lands in between.
    let before = s.reports().len();
    s.send(ESC);
    wait_report(&s, before, |r| r.contains("flags=v "));
    for key in [&b"0"[..], b"2", b"d", b"w"] {
        s.send(key);
        std::thread::sleep(Duration::from_millis(40));
    }
    wait_report(&s, before, |r| {
        r.contains("flags=v ") && r.contains(" buffer=\"\" ")
    });
    // R's replace mode has no trigger binding: none goes after it (one
    // would rewrite the line), from ESC on.
    send_report(&s, b"i", "");
    send_report(&s, b"abc", "abc");
    let before = s.reports().len();
    s.send(ESC);
    wait_report(&s, before, |r| r.contains("flags=v "));
    for key in [&b"0"[..], b"R", b"Z"] {
        s.send(key);
        std::thread::sleep(Duration::from_millis(40));
    }
    std::thread::sleep(Duration::from_millis(300));
    wait_line(&s, "% Zbc");
    no_trigger_on_screen(&s);
}

/// A widget that reads the terminal itself (as fzf's do), bound with
/// `bind -x`: the hook defers its key, so no trigger reaches it.
#[test]
fn a_bind_x_widget_gets_no_trigger() {
    let rc = r#"__pick() { local k; IFS= read -rsn4 k; READLINE_LINE+="<$k>"; READLINE_POINT=${#READLINE_LINE}; }
bind -x '"\C-t": __pick'"#;
    let dirs = Dirs::new();
    let s = spawn(
        &dirs,
        Start {
            rc: rc.to_owned(),
            ..Start::default()
        },
    );
    send_report(&s, b"x", "x");
    for keys in [&b"\x14"[..], b"ab", b"c", b"d"] {
        s.send(keys);
        std::thread::sleep(Duration::from_millis(40));
    }
    // The widget took the 4 keys; the next key's trigger reports it all.
    send_report(&s, b"y", "x<abcd>y");
    no_trigger_on_screen(&s);
}

#[test]
fn double_tab_lists_and_ctrl_w_kills_accumulate() {
    let dirs = Dirs::new();
    dirs.touch("zebra-one");
    dirs.touch("zebra-two");
    let s = spawn(&dirs, Start::default());
    send_report(&s, b"cat zebra-", "cat zebra-");
    wait_popup(&s);
    s.send(ESC);
    wait_no_popup(&s);
    // bash's own Tab, twice: the line is as far as it completes, so the
    // second lists.
    s.send(TAB);
    std::thread::sleep(Duration::from_millis(40));
    s.send(TAB);
    s.wait_for(
        "bash's list",
        |screen| {
            has_line(screen, |line| {
                line.contains("zebra-one") && line.contains("zebra-two")
            })
        },
        TIMEOUT,
    );
    send_report(&s, b"\x15", "");
    // C-w C-w kills two words into one kill: C-y gives both back (apart,
    // it would give back only `two `).
    send_report(&s, b"echo one two three", "echo one two three");
    for key in [&b"\x17"[..], b"\x17"] {
        s.send(key);
        std::thread::sleep(Duration::from_millis(40));
    }
    send_report(&s, b"\x19", "echo one two three");
    no_trigger_on_screen(&s);
}

/// bash's own Tab sends no trigger after it, idle or not: the second Tab
/// lists after any pause, as in plain bash, and the next key's report has
/// the line as bash completed it.
#[test]
fn double_tab_lists_after_a_pause_and_the_next_key_reports() {
    let dirs = Dirs::new();
    dirs.touch("zebra-one");
    dirs.touch("zebra-two");
    let s = spawn(&dirs, Start::default());
    for pause in [400, 1000] {
        // The line as far as it completes: the second Tab lists.
        send_report(&s, b"\x0ccat zebra-", "cat zebra-");
        wait_popup(&s);
        s.send(ESC);
        wait_no_popup(&s);
        s.send(TAB);
        std::thread::sleep(Duration::from_millis(pause));
        s.send(TAB);
        s.wait_for(
            &format!("bash's list after {pause} ms"),
            |screen| {
                has_line(screen, |line| {
                    line.contains("zebra-one") && line.contains("zebra-two")
                })
            },
            TIMEOUT,
        );
        send_report(&s, b"\x15", "");
    }
    // A Tab that completes: no report until the next key, whose trigger
    // reports the completed line.
    send_report(&s, b"\x0ccat zeb", "cat zeb");
    wait_popup(&s);
    s.send(ESC);
    wait_no_popup(&s);
    let before = s.reports().len();
    s.send(TAB);
    wait_line(&s, "% cat zebra-");
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(s.reports().len(), before, "no report after bash's Tab");
    send_report(&s, b"o", "cat zebra-o");
    no_trigger_on_screen(&s);
}

/// Each `needle` in `raw`, with the synchronized frame (2026) around it
/// and whether there is one.
fn frames_with<'a>(raw: &'a [u8], needle: &[u8]) -> Vec<(&'a [u8], bool)> {
    const BEGIN: &[u8] = b"\x1b[?2026h";
    const END: &[u8] = b"\x1b[?2026l";
    let find = |hay: &[u8], what: &[u8]| hay.windows(what.len()).position(|w| w == what);
    let rfind = |hay: &[u8], what: &[u8]| hay.windows(what.len()).rposition(|w| w == what);
    let mut found = Vec::new();
    let mut at = 0;
    while let Some(off) = find(&raw[at..], needle) {
        let pos = at + off;
        let begin = rfind(&raw[..pos], BEGIN);
        let end_before = rfind(&raw[..pos], END);
        let inside = begin.is_some_and(|begin| end_before.is_none_or(|end| end < begin));
        let start = begin.unwrap_or(pos);
        let end = find(&raw[pos..], END).map_or(raw.len(), |end| pos + end);
        found.push((&raw[start..end], inside));
        at = pos + needle.len();
    }
    found
}

/// bash's clear and redraw around each report reach the terminal in one
/// write: every clear is inside a synchronized frame that also holds the
/// redrawn prompt.
#[test]
fn the_clear_and_redraw_are_one_frame() {
    let dirs = Dirs::new();
    let s = spawn(&dirs, Start::default());
    assert!(s.sync_output());
    type_slowly(&s, "echo one-frame", Duration::from_millis(15));
    wait_report(&s, 0, |r| r.contains(" buffer=\"echo one-frame\" "));
    s.wait_quiet(Duration::from_millis(100), TIMEOUT);
    let raw = s.raw_output();
    let clears = frames_with(&raw, b"\r\x1b[K\r");
    assert!(clears.len() >= 14, "{} clears", clears.len());
    for (frame, inside) in clears {
        assert!(
            inside,
            "a clear outside a frame: {:?}",
            String::from_utf8_lossy(frame)
        );
        assert!(
            frame.windows(2).any(|w| w == b"% "),
            "the redraw is not in the clear's frame: {:?}",
            String::from_utf8_lossy(frame)
        );
    }
}

/// bash redraws its prompt with each report, 133;B with it: fac takes one
/// A and one B per prompt, and asks for the cursor once.
#[test]
fn one_prompt_mark_each_and_one_cursor_query_per_line() {
    let dirs = Dirs::new();
    let s = spawn(&dirs, Start::default());
    s.wait_quiet(Duration::from_millis(100), TIMEOUT);
    let log_from = s.log_text().len();
    let raw_from = s.raw_output().len();
    let before = s.reports().len();
    // A fresh prompt, then a line of 10 keys, each with its report.
    s.send(b"\r");
    wait_report(&s, before, |r| r.contains(" buffer=\"\" "));
    let typed = "xyzzy-1234";
    for end in 1..=typed.len() {
        send_report(&s, &typed.as_bytes()[end - 1..end], &typed[..end]);
    }
    s.wait_quiet(Duration::from_millis(100), TIMEOUT);
    let log = s.log_text();
    let log = &log[log_from..];
    let count = |needle: &str| log.lines().filter(|line| line.ends_with(needle)).count();
    assert_eq!(count("prompt mark A"), 1, "{log}");
    assert_eq!(count("prompt mark B"), 1, "{log}");
    let raw = s.raw_output();
    let cprs = raw[raw_from..]
        .windows(CPR_QUERY.len())
        .filter(|w| *w == CPR_QUERY)
        .count();
    assert_eq!(cprs, 1, "fac's cursor queries");
}

/// `cat`, Enter and what is typed for cat, in one write: no trigger lands
/// in what cat reads.
#[test]
fn cat_and_enter_in_one_write_leak_no_trigger() {
    let dirs = Dirs::new();
    let s = spawn(&dirs, Start::default());
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
    // After a line typed ahead, the next prompt's first key asks.
    s.send(b"\x04");
    s.wait_for(
        "the next prompt",
        |screen| {
            let (row, _) = screen.cursor_position();
            harness::lines(screen)
                .get(usize::from(row))
                .is_some_and(|line| line.trim_end() == "%")
        },
        TIMEOUT,
    );
    send_report(&s, b"x", "x");
    no_trigger_on_screen(&s);
}

/// A line typed ahead, then Up pressed as soon as it ran, before the next
/// prompt is drawn: the prompt's first trigger reports the recalled line.
#[test]
fn up_typed_before_the_prompt_after_a_line_typed_ahead_is_reported() {
    let dirs = Dirs::new();
    let s = spawn(
        &dirs,
        Start {
            // Each prompt comes 400 ms after its command: time to type.
            rc: "PROMPT_COMMAND='sleep 0.4'".to_owned(),
            ..Start::default()
        },
    );
    // The empty line's prompt is not drawn yet when the next line comes.
    s.send(b"\r");
    s.send(b"echo from-history\r");
    s.wait_for(
        "echo's output",
        |screen| has_line(screen, |line| line.trim_end() == "from-history"),
        TIMEOUT,
    );
    let before = s.reports().len();
    s.send(b"\x1b[A");
    wait_report(&s, before, |r| r.contains(" buffer=\"echo from-history\" "));
    // The terminal echoed Up (typed in cooked mode) before the prompt;
    // bash's redraw for the report clears it, and fac may still hold that
    // redraw when the report is in.
    s.wait_for(
        "the recalled line, redrawn",
        |screen| {
            let (row, _) = screen.cursor_position();
            harness::lines(screen)
                .get(usize::from(row))
                .is_some_and(|line| line.trim_end() == "% echo from-history")
        },
        TIMEOUT,
    );
    no_trigger_on_screen(&s);
}

/// The reviewer's case: `sleep 1` runs while `cat`, Enter and `abc` are
/// typed, and the shell is slow between its prompt and the queued line's
/// 133;C (a PS0 that takes 500 ms). No trigger goes while that line may be
/// queued: cat reads only `abc`. The prompt after cat finds nothing typed
/// ahead, and the trigger is back.
#[test]
fn no_trigger_while_a_line_typed_ahead_is_queued() {
    let dirs = Dirs::new();
    let s = spawn(
        &dirs,
        Start {
            rc: "PS0='$(sleep 0.5)'".to_owned(),
            ..Start::default()
        },
    );
    // Typed once `sleep 1` runs (its 133;C, after PS0's 500 ms), so the
    // terminal echoes them, however slow the shell is to read the line.
    let commands = s.log_text().matches("prompt mark C").count();
    s.send(b"sleep 1\r");
    wait_log_count(&s, "prompt mark C", commands + 1);
    s.send(b"cat\r");
    std::thread::sleep(Duration::from_millis(100));
    s.send(b"abc");
    // The Enter for cat once cat runs (its 133;C, after PS0's 500 ms
    // again): the prompt between has come and gone with no trigger.
    wait_log_count(&s, "prompt mark C", commands + 2);
    std::thread::sleep(Duration::from_millis(200));
    s.send(b"\r");
    s.wait_for(
        "cat's copy of abc",
        |screen| {
            screen
                .contents()
                .lines()
                .filter(|line| line.trim_end() == "abc")
                .count()
                == 2
        },
        TIMEOUT,
    );
    s.send(b"\x04");
    wait_line(&s, "%");
    no_trigger_on_screen(&s);
    // The fallback ends: this prompt's look found nothing typed ahead.
    wait_log(&s, "no line typed ahead is queued");
    send_report(&s, b"x", "x");
    no_trigger_on_screen(&s);
}

/// The log has `needle` at least `n` times.
fn wait_log_count(s: &Session, needle: &str, n: usize) {
    let deadline = Instant::now() + TIMEOUT;
    while s.log_text().matches(needle).count() < n {
        assert!(
            Instant::now() < deadline,
            "fewer than {n} {needle:?} in the log\n--- screen ---\n{}\n--- log ---\n{}",
            s.screen_text(),
            s.log_text()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The text of `name` in the session's cwd, once it is `expected`.
fn wait_file(dirs: &Dirs, s: &Session, name: &str, expected: &str) {
    let path = dirs.cwd().join(name);
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let text = std::fs::read(&path).unwrap_or_default();
        if text == expected.as_bytes() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{name} is {:?}, not {expected:?}\n--- screen ---\n{}\n--- log ---\n{}",
            String::from_utf8_lossy(&text),
            s.screen_text(),
            s.log_text()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The reviewer's B1: `cat >o1`, Enter and `abc` typed while bash still
/// runs its login files, before the hook has announced its trigger. The
/// Enter is typed ahead like any other: the first prompt sends no trigger
/// while `cat >o1` may be queued, so cat reads only `abc`, and the prompt
/// after cat has its trigger again.
#[test]
fn a_line_typed_while_bash_starts_gets_no_trigger() {
    let dirs = Dirs::new();
    let s = Session::spawn(SpawnOpts {
        shell: ShellKind::Bash,
        rc_extra: Some(format!("cd '{}'\nsleep 1.5", dirs.cwd().display())),
        ..SpawnOpts::default()
    });
    wait_log(&s, "terminal kind=");
    s.send(b"cat >o1\r");
    std::thread::sleep(Duration::from_millis(50));
    s.send(b"abc");
    assert!(
        !s.log_text().contains("report trigger is on"),
        "the keys came after the hook loaded: the test proves nothing"
    );
    // cat runs (its 133;C), then ends with two Ctrl-D.
    wait_log_count(&s, "prompt mark C", 1);
    std::thread::sleep(Duration::from_millis(200));
    s.send(b"\x04");
    std::thread::sleep(Duration::from_millis(200));
    s.send(b"\x04");
    wait_file(&dirs, &s, "o1", "abc");
    wait_line(&s, "%");
    send_report(&s, b"x", "x");
    no_trigger_on_screen(&s);
}

/// `sleep 1` runs while `cat >o1`, `enter` and `abc` are typed: readline
/// takes `enter` as the line's end, though the terminal's line discipline
/// does not end a line on it (nor does bash's old look, `read -t 0` in
/// cooked mode, see one). No trigger goes while that line is queued: cat
/// reads only what readline leaves of `abc` (`cat_reads`; readline keeps
/// `kept` for its next line), and the prompt after cat has its trigger
/// again.
fn typed_ahead_into_cat(rc: &str, enter: &[u8], cat_reads: &str, kept: &str) {
    let dirs = Dirs::new();
    let s = spawn(
        &dirs,
        Start {
            rc: rc.to_owned(),
            ..Start::default()
        },
    );
    let commands = s.log_text().matches("prompt mark C").count();
    s.send(b"sleep 1\r");
    wait_log_count(&s, "prompt mark C", commands + 1);
    s.send(&[b"cat >o1".as_slice(), enter].concat());
    std::thread::sleep(Duration::from_millis(50));
    s.send(b"abc");
    // cat runs (its 133;C), then ends with two Ctrl-D.
    wait_log_count(&s, "prompt mark C", commands + 2);
    std::thread::sleep(Duration::from_millis(200));
    s.send(b"\x04");
    std::thread::sleep(Duration::from_millis(200));
    s.send(b"\x04");
    wait_file(&dirs, &s, "o1", cat_reads);
    wait_line(&s, format!("% {kept}").trim_end());
    send_report(&s, b"x", &format!("{kept}x"));
    no_trigger_on_screen(&s);
}

/// The reviewer's B2: a key the user bound to accept-line. C-x is also the
/// prefix of C-x C-e: readline runs its own binding once `a` goes on no
/// sequence, and keeps `a` for its next line (plain bash does the same).
#[test]
fn a_key_bound_to_accept_line_and_typed_ahead_gets_no_trigger_after_it() {
    typed_ahead_into_cat(r#"bind '"\C-x": accept-line'"#, b"\x18", "bc", "a");
}

/// The reviewer's B2: with `stty -icrnl`, Enter (CR) ends no line for the
/// line discipline, but readline accepts on it.
#[test]
fn an_enter_typed_ahead_with_icrnl_off_gets_no_trigger_after_it() {
    typed_ahead_into_cat("stty -icrnl", b"\r", "abc", "");
}

/// Ctrl-V then Enter typed ahead: the line discipline takes the Ctrl-V
/// (literal-next) and keeps the CR as a character of the line, so readline
/// gets a bare Enter and accepts on it.
#[test]
fn ctrl_v_enter_typed_ahead_gets_no_trigger_after_it() {
    typed_ahead_into_cat("", b"\x16\r", "abc", "");
}

/// The reviewer's s-less: `q` to less and the next line in one write. less
/// reads only `q`; the Enter after it was typed while fac still saw the
/// alternate screen, and it is typed ahead all the same: cat reads only
/// `abc`, and the prompt after cat has its trigger again.
#[test]
fn a_line_typed_with_the_key_that_quits_less_gets_no_trigger() {
    let dirs = Dirs::new();
    let s = spawn(&dirs, Start::default());
    let commands = s.log_text().matches("prompt mark C").count();
    send_report(&s, b"less /etc/hosts", "less /etc/hosts");
    s.send(b"\r");
    wait_log(&s, "alt_screen=on");
    std::thread::sleep(Duration::from_millis(300));
    s.send(b"qcat >o1\rabc");
    wait_log_count(&s, "prompt mark C", commands + 2);
    std::thread::sleep(Duration::from_millis(200));
    s.send(b"\x04");
    std::thread::sleep(Duration::from_millis(200));
    s.send(b"\x04");
    wait_file(&dirs, &s, "o1", "abc");
    wait_line(&s, "%");
    send_report(&s, b"x", "x");
    no_trigger_on_screen(&s);
}

/// vim reads the Enters typed into it, so the prompt after it finds them
/// read and the popup is on at once.
#[test]
fn the_popup_is_on_at_the_prompt_after_vim() {
    let dirs = Dirs::new();
    let s = spawn(&dirs, Start::default());
    send_report(&s, b"vim -u NONE -N -n f.txt", "vim -u NONE -N -n f.txt");
    s.send(b"\r");
    wait_log(&s, "alt_screen=on");
    std::thread::sleep(Duration::from_millis(300));
    s.send(b"ihello\rworld\r\x1b");
    std::thread::sleep(Duration::from_millis(200));
    s.send(b":wq\r");
    wait_log(&s, "alt_screen=off");
    wait_file(&dirs, &s, "f.txt", "hello\nworld\n\n");
    wait_line(&s, "%");
    send_report(&s, b"git ch", "git ch");
    wait_label(&s, "checkout", |label| label.starts_with("checkout"));
    no_trigger_on_screen(&s);
}

/// A login file with `set -euo pipefail` and no bash-completion (so the
/// hook's look for `_comp_load` fails): every hook function runs with the
/// options off and puts them back, and a failed status in an exempt place
/// (`! true`) does not end the shell through fac's PROMPT_COMMAND parts.
#[test]
fn errexit_nounset_and_pipefail_in_the_login_files() {
    let dirs = Dirs::new();
    let s = spawn(
        &dirs,
        Start {
            rc: "set -euo pipefail".to_owned(),
            ..Start::default()
        },
    );
    for (line, output) in [("echo one", "one"), ("! true", ""), ("echo $- three", "")] {
        send_report(&s, line.as_bytes(), line);
        s.send(b"\r");
        if !output.is_empty() {
            wait_line(&s, output);
        }
        wait_report(&s, 0, |r| r.contains(" buffer=\"\" "));
    }
    // The options are the user's again: errexit and nounset are on.
    s.wait_for(
        "the shell's options",
        |screen| {
            has_line(screen, |line| {
                line.ends_with(" three") && line.contains('e') && line.contains('u')
            })
        },
        TIMEOUT,
    );
    send_report(&s, b"git ch", "git ch");
    wait_label(&s, "checkout", |label| label.starts_with("checkout"));
    no_trigger_on_screen(&s);
}

/// With readline's line editing off (`set +o emacs`), a trigger would go
/// into the terminal's cooked input, so the hook does not arm it: commands
/// run as typed. `set -o emacs` brings it back at the next prompt.
#[test]
fn no_trigger_with_line_editing_off() {
    let dirs = Dirs::new();
    let s = spawn(&dirs, Start::default());
    send_report(&s, b"set +o emacs", "set +o emacs");
    s.send(b"\r");
    wait_line(&s, "%");
    std::thread::sleep(Duration::from_millis(200));
    let reports = s.reports().len();
    for word in ["one", "two"] {
        s.send(format!("echo {word}\r").as_bytes());
        wait_line(&s, word);
    }
    s.wait_quiet(Duration::from_millis(200), TIMEOUT);
    assert_eq!(s.reports().len(), reports, "no report with editing off");
    no_trigger_on_screen(&s);
    // The next prompt has a line editor again: its first trigger reports.
    s.send(b"set -o emacs\r");
    wait_report(&s, reports, |r| r.contains(" buffer=\"\" "));
    send_report(&s, b"x", "x");
    no_trigger_on_screen(&s);
}

/// In emacs mode ESC has no binding of its own: readline waits for the key
/// after it however long it takes. ESC, a second, then `.` is M-.
/// (yank-last-arg), judged as one key: no trigger right after it, so a
/// quick second M-. still cycles back through the history (a trigger in
/// between would start it over), and no trigger lands between ESC and `.`.
#[test]
fn esc_then_a_key_a_second_later_is_one_key() {
    let dirs = Dirs::new();
    let s = spawn(&dirs, Start::default());
    for (line, output) in [
        ("echo one first", "one first"),
        ("echo two second", "two second"),
    ] {
        send_report(&s, line.as_bytes(), line);
        s.send(b"\r");
        wait_line(&s, output);
    }
    wait_report(&s, 0, |r| r.contains(" buffer=\"\" "));
    s.send(ESC);
    std::thread::sleep(Duration::from_millis(1000));
    s.send(b".");
    wait_line(&s, "% second");
    s.send(b"\x1b.");
    wait_line(&s, "% first");
    send_report(&s, b"x", "firstx");
    no_trigger_on_screen(&s);
}

/// Typing, editing, history, paste and Tab: each gets a report of the line.
#[test]
fn edits_are_reported() {
    let dirs = Dirs::new();
    dirs.touch("zebra-file");
    let s = spawn(&dirs, Start::default());
    for (keys, line) in [
        (&b"echo hi"[..], "echo hi"),
        (" é".as_bytes(), "echo hi é"),
        (" שלום".as_bytes(), "echo hi é שלום"),
        (" 🙂".as_bytes(), "echo hi é שלום 🙂"),
        (b"\x7f\x7f", "echo hi é שלום"),
        (b"\x01", "echo hi é שלום"),
        (b"\x05x", "echo hi é שלוםx"),
    ] {
        send_report(&s, keys, line);
    }
    send_report(&s, b"\x15", "");
    // History, with the next line typed before its prompt: no trigger
    // reaches `echo`.
    s.send(b"\r");
    s.send(b"echo from-history\r");
    wait_line(&s, "from-history");
    send_report(&s, b"\x1b[A", "echo from-history");
    send_report(&s, b"\x15", "");
    // A paste.
    send_report(&s, b"\x1b[200~echo pasted\x1b[201~", "echo pasted");
    send_report(&s, b"\x15", "");
    // C-w, an idle key: its report comes once no key has.
    send_report(&s, b"echo one two", "echo one two");
    send_report(&s, b"\x17", "echo one ");
    send_report(&s, b"\x15", "");
    // bash's own Tab (the popup closed with Esc first): it completes, and
    // the next key's report has the completed line (no trigger goes after
    // bash's Tab, so that a second Tab lists).
    send_report(&s, b"ls zeb", "ls zeb");
    wait_popup(&s);
    s.send(ESC);
    wait_no_popup(&s);
    s.send(TAB);
    wait_line(&s, "% ls zebra-file");
    send_report(&s, b"x", "ls zebra-file x");
    no_trigger_on_screen(&s);
}

/// Keys sent one by one while each report's binding is still running
/// (fac.bash's `__fac_report` wrapped, once the hook has loaded, in one
/// that sleeps 20 ms first): the line is what was typed, in order, and
/// nothing is added to it.
#[test]
fn keys_sent_while_the_report_binding_sleeps_reach_the_line_in_order() {
    let rc = r#"__t_wrap() {
  [[ -n ${__t_wrapped-} ]] && return 0
  __t_wrapped=1
  local src
  src=$(declare -f __fac_report)
  eval "__t_orig${src#__fac}"
  __fac_report() { sleep 0.02; __t_orig_report "$@"; }
  : > "$HOME/slow-wrapped"
}
PROMPT_COMMAND+=(__t_wrap)"#;
    let dirs = Dirs::new();
    let s = spawn(
        &dirs,
        Start {
            rc: rc.to_owned(),
            ..Start::default()
        },
    );
    // A prompt after the wrap, so every report from here sleeps.
    s.send(b"\r");
    wait_report(&s, 1, |r| r.contains(" buffer=\"\" "));
    assert!(s.home().join("slow-wrapped").exists(), "the wrapper ran");
    let typed = "echo burst-of-keys-é";
    type_slowly(&s, typed, Duration::from_millis(3));
    wait_line(&s, &format!("% {typed}"));
    s.send(b"\r");
    wait_line(&s, "burst-of-keys-é");
    no_trigger_on_screen(&s);
    // The next prompt reports again.
    send_report(&s, b"x", "x");
    no_trigger_on_screen(&s);
}

#[test]
fn a_ps2_line_reports_cont() {
    let dirs = Dirs::new();
    let s = spawn(&dirs, Start::default());
    send_report(&s, b"echo \"a", "echo \"a");
    s.send(b"\r");
    wait_line(&s, ">");
    let before = s.reports().len();
    s.send(b"b");
    wait_report(&s, before, |r| {
        r.contains("ctx=cont ") && r.ends_with(" buffer=\"b\" prebuffer=\"echo \\\"a\\n\"")
    });
    // The command ends; the next prompt's line is `start` again.
    s.send(b"\"\r");
    wait_line(&s, "b");
    send_report(&s, b"z", "z");
    let last = s.reports().pop().expect("a report");
    assert!(last.contains("ctx=start "), "{last}");
    assert!(
        s.log_text().contains("command \"echo \\\"a\\nb\\\"\""),
        "{}",
        s.log_text()
    );
}

/// A tool that sets PS0 and PROMPT_COMMAND anew after fac's setup, as
/// bash-preexec does at its first prompt: fac's parts come back, and
/// theirs stay.
#[test]
fn a_ps0_set_anew_keeps_the_command_marks() {
    let rc = r#"__t_pre() { printf 'PRE-%s ' "${1-}"; }
__t_install() {
  PS0='${ __t_pre x >&2; }'
  PROMPT_COMMAND="__t_precmd"
}
__t_precmd() { :; }
PROMPT_COMMAND+=(__t_install)"#;
    let dirs = Dirs::new();
    let s = spawn(
        &dirs,
        Start {
            rc: rc.to_owned(),
            ..Start::default()
        },
    );
    for word in ["one", "two"] {
        send_report(
            &s,
            format!("echo {word}").as_bytes(),
            &format!("echo {word}"),
        );
        let from = s.log_text().len();
        let before = s.reports().len();
        s.send(b"\r");
        wait_line(&s, &format!("PRE-x {word}"));
        wait_report(&s, before, |r| r.contains(" buffer=\"\" "));
        let log = s.log_text();
        let log = &log[from..];
        assert!(log.contains(&format!("command \"echo {word}\"")), "{log}");
        assert!(log.contains("prompt mark C"), "{log}");
        assert!(log.contains("prompt mark D"), "{log}");
    }
}

#[test]
fn the_hook_parses_under_bash_3_2() {
    let bash = harness::shell_path(ShellKind::Bash);
    for bash in [Path::new("/bin/bash"), bash.as_path()] {
        let out = std::process::Command::new(bash)
            .arg("-n")
            .arg(hook())
            .output()
            .expect("running bash -n");
        assert!(
            out.status.success(),
            "{} -n: {}",
            bash.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// `git ch` and Down, under a terminal profile, rendered after each step.
fn git_ch_snapshot(profile: Profile) -> String {
    let dirs = Dirs::new();
    let s = spawn(
        &dirs,
        Start {
            only_bin: true,
            profile: Some(profile),
            ..Start::default()
        },
    );
    // The worker gives the command names (`g`): ready before the first
    // key, as the zsh and fish snapshots wait for theirs.
    wait_log(&s, "worker ready");
    let colors = harness::profile_colors(profile);
    // Clear the screen: the prompt alone at the top.
    let since = Instant::now();
    s.send(b"\x0c");
    s.wait_for(
        "a prompt on the first row",
        |screen| screen.cursor_position() == (0, 2),
        TIMEOUT,
    );
    suite::settle(&s, "clear", since);
    let mut out = String::new();
    let mut step = |name: &str, bytes: &[u8]| {
        let log_before = s.log_text().len();
        let since = Instant::now();
        s.send(bytes);
        suite::settle(&s, name, since);
        suite::check_invariants_with(&s, name, log_before, &colors);
        let card = s.card();
        let _ = writeln!(out, "--- after {name} ---");
        s.with_screen(|screen| out.push_str(&suite::render_with(card.as_ref(), screen, &colors)));
    };
    for c in "git ch".chars() {
        let name = if c == ' ' {
            "space".to_owned()
        } else {
            format!("{c:?}")
        };
        step(&name, c.to_string().as_bytes());
    }
    step("Down", DOWN);
    out
}

#[test]
fn bash_git_ch_down_ghostty() {
    insta::assert_snapshot!(
        "bash_git_ch_down_ghostty",
        git_ch_snapshot(Profile::Ghostty)
    );
}

#[test]
fn bash_git_ch_down_apple() {
    insta::assert_snapshot!("bash_git_ch_down_apple", git_ch_snapshot(Profile::Apple));
}
