//! The per-keystroke end-to-end suite: fac runs in the headless terminal in
//! a fixture cwd with a fixture history and a deterministic test command
//! (`faccmd`), and after every step the screen, the cursor and the popup's
//! state are recorded. One insta snapshot per scenario holds all its steps,
//! so a whole keystroke history is reviewable in one file. Plain asserts
//! run after every step: no residue below the prompt or the popup, and the
//! harness cursor agrees with fac's last cursor reconcile.

mod harness;
mod suite;

use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use harness::{Card, Colors, Profile, Session, SpawnOpts, wait_log, wait_prompt};
use suite::{
    QUIET, Tree, check_invariants_with, popup_state_with, render_with, row_text, settle, step,
};

const DOWN: &[u8] = b"\x1b[B";
const UP: &[u8] = b"\x1b[A";
const ESC: &[u8] = b"\x1b";
const TAB: &[u8] = b"\t";
const ENTER: &[u8] = b"\r";
/// Turns on application cursor mode, as oh-my-zsh's key bindings do.
const SMKX: &str = "zle-line-init() { echoti smkx }\nzle -N zle-line-init";

/// What a step does.
enum Act {
    Send(Vec<u8>),
    Resize(u16, u16),
    /// A line written to the FIFO the async prompt fixture reads
    /// (`tests/fixtures/prompts/async-prompt.zsh`): no key is pressed.
    Fifo(&'static str),
}

struct Step {
    name: String,
    act: Act,
}

fn send(name: &str, bytes: &[u8]) -> Step {
    Step {
        name: name.to_owned(),
        act: Act::Send(bytes.to_vec()),
    }
}

/// One step per character of `text`, each named after its character.
fn typed(text: &str) -> Vec<Step> {
    text.chars()
        .map(|ch| {
            let name = if ch == ' ' {
                "space".to_owned()
            } else {
                format!("{ch:?}")
            };
            send(&name, ch.to_string().as_bytes())
        })
        .collect()
}

fn fifo(name: &str, line: &'static str) -> Step {
    Step {
        name: name.to_owned(),
        act: Act::Fifo(line),
    }
}

fn resize(name: &str, rows: u16, cols: u16) -> Step {
    Step {
        name: name.to_owned(),
        act: Act::Resize(rows, cols),
    }
}

/// fac in the fixture cwd with the fixture history, the `faccmd` fixture and
/// `extra` appended to the fixture `.zshrc`, in a `rows` x `cols` terminal.
/// Smoke-checked, at a prompt at the top of a cleared screen, with the
/// worker ready.
fn start(extra: &str, rows: u16, cols: u16) -> (Session, Tree) {
    start_with_env(extra, rows, cols, Vec::new())
}

/// `start` with `extra_env` added to fac's environment.
fn start_with_env(
    extra: &str,
    rows: u16,
    cols: u16,
    extra_env: Vec<(String, String)>,
) -> (Session, Tree) {
    start_with(extra, rows, cols, extra_env, |_| {})
}

/// `start_with_env`, with `tweak` changing the session's options (the
/// terminal's colours, for one) before it spawns.
fn start_with(
    extra: &str,
    rows: u16,
    cols: u16,
    extra_env: Vec<(String, String)>,
    tweak: impl FnOnce(&mut SpawnOpts),
) -> (Session, Tree) {
    start_ready(extra, rows, cols, extra_env, tweak, Ready::Default)
}

/// Where the cursor waits at a fresh prompt.
#[derive(Clone, Copy)]
enum Ready {
    /// The fixture's `% ` alone on the screen, at (0, 2).
    Default,
    /// A prompt fixture's own: the cursor's (row, col) on a cleared screen.
    At(u16, u16),
}

/// `start_with`, at a fresh prompt as `ready` says.
fn start_ready(
    extra: &str,
    rows: u16,
    cols: u16,
    extra_env: Vec<(String, String)>,
    tweak: impl FnOnce(&mut SpawnOpts),
    ready: Ready,
) -> (Session, Tree) {
    let (s, tree) = suite::spawn_in_tree(extra, rows, cols, extra_env, tweak);
    match ready {
        Ready::Default => {
            wait_prompt(&s);
            suite::ready(&s);
        }
        Ready::At(row, col) => suite::ready_at(&s, (row, col)),
    }
    (s, tree)
}

/// The invariants checked after every step.
fn check_invariants(s: &Session, step: &str, log_before: usize) {
    check_invariants_with(s, step, log_before, &harness::colors());
}

/// Run `steps` and snapshot the screen after each.
fn scenario(name: &str, extra: &str, steps: Vec<Step>) {
    scenario_sized(name, (24, 80), extra, steps, |_, _, _, _| {});
}

/// The terminals without synchronized output every scenario also runs in:
/// fac draws the same there, and paints each cell once per frame too.
const WITHOUT_SYNC: [Profile; 2] = [Profile::Apple, Profile::Vscode104];

/// Under a profile, the shell's own view stays the default run's, so the
/// lists match: fac has read `COLORTERM` and `FAC_FULL_REPAINT` already (the
/// shell's exported variables are offered after `echo $`), and `/etc/zshrc`
/// defines Terminal.app's `update_terminal_cwd` for
/// `TERM_PROGRAM=Apple_Terminal`.
const SAME_SHELL: &str = "unset COLORTERM FAC_FULL_REPAINT\n\
    if (( $+functions[update_terminal_cwd] )); then\n\
    add-zsh-hook -d precmd update_terminal_cwd; unfunction update_terminal_cwd\n\
    fi";

/// `scenario` in a (rows, cols) terminal; `check` also runs after every
/// step, with the step's index, the popup, the screen and the popup's
/// colours. The scenario runs again under each of `WITHOUT_SYNC`, which
/// must render the same, and under Ghostty and Apple both with frames that
/// leave alone what still shows and with every frame whole
/// (`FAC_FULL_REPAINT`): the terminal's screen, characters and attributes,
/// must be the same after every step.
fn scenario_sized(
    name: &str,
    size: (u16, u16),
    extra: &str,
    steps: Vec<Step>,
    check: impl Fn(usize, Option<&Card>, &vt100::Screen, &Colors),
) {
    scenario_ready(name, size, extra, Ready::Default, steps, check);
}

/// A scenario in a prompt fixture (`tests/fixtures/prompts/<fixture>.zsh`),
/// whose fresh prompt leaves the cursor at `cursor`.
fn prompt_scenario(name: &str, fixture: &str, cursor: (u16, u16), steps: Vec<Step>) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/prompts")
        .join(format!("{fixture}.zsh"));
    let extra = format!("source '{}'", path.display());
    scenario_ready(
        name,
        (24, 80),
        &extra,
        Ready::At(cursor.0, cursor.1),
        steps,
        |_, _, _, _| {},
    );
}

/// `scenario_sized`, at a fresh prompt as `ready` says.
fn scenario_ready(
    name: &str,
    size: (u16, u16),
    extra: &str,
    ready: Ready,
    steps: Vec<Step>,
    check: impl Fn(usize, Option<&Card>, &vt100::Screen, &Colors),
) {
    let run = |profile, full| run_scenario(profile, full, size, extra, ready, &steps, &check);
    let (out, _) = run(None, false);
    insta::assert_snapshot!(name, out);
    let mut diffed = Vec::new();
    for profile in WITHOUT_SYNC {
        let (other, screens) = run(Some(profile), false);
        assert_eq!(other, out, "{name} under {profile:?}");
        diffed.push((profile, screens));
    }
    let (other, screens) = run(Some(Profile::Ghostty), false);
    assert_eq!(other, out, "{name} under Ghostty");
    diffed.push((Profile::Ghostty, screens));
    for (profile, screens) in diffed {
        if profile == Profile::Vscode104 {
            continue;
        }
        let (other, whole) = run(Some(profile), true);
        assert_eq!(other, out, "{name} under {profile:?}, every frame whole");
        for ((step, diff), whole) in steps.iter().zip(&screens).zip(&whole) {
            same_screen(
                diff,
                whole,
                &format!("{name} under {profile:?}, after {}", step.name),
            );
        }
    }
}

/// `a` and `b` show the same: every cell's characters and attributes, and
/// the cursor.
fn same_screen(a: &vt100::Screen, b: &vt100::Screen, what: &str) {
    assert_eq!(
        a.cursor_position(),
        b.cursor_position(),
        "{what}: the cursor"
    );
    let (rows, cols) = (a.size().0.get(), a.size().1.get());
    for row in 0..rows {
        for col in 0..cols {
            assert_eq!(
                a.cell(row, col),
                b.cell(row, col),
                "{what}: cell ({row}, {col})\n--- frames that leave cells alone ---\n{}\n--- whole frames ---\n{}",
                a.contents(),
                b.contents()
            );
        }
    }
}

/// Run `steps` in a terminal that behaves like `profile` (the default one
/// for `None`), with every frame whole when `full`, checking after each.
/// Returns the screen rendered after each, and the screens themselves.
fn run_scenario(
    profile: Option<Profile>,
    full: bool,
    (rows, cols): (u16, u16),
    extra: &str,
    ready: Ready,
    steps: &[Step],
    check: &impl Fn(usize, Option<&Card>, &vt100::Screen, &Colors),
) -> (String, Vec<vt100::Screen>) {
    let extra = match profile {
        Some(_) => format!("{extra}\n{SAME_SHELL}"),
        None => extra.to_owned(),
    };
    let env = if full {
        vec![("FAC_FULL_REPAINT".to_owned(), "1".to_owned())]
    } else {
        Vec::new()
    };
    let (s, tree) = start_ready(
        &extra,
        rows,
        cols,
        env,
        |opts| {
            if let Some(profile) = profile {
                opts.set_profile(profile);
            }
        },
        ready,
    );
    let colors = profile.map_or_else(harness::colors, harness::profile_colors);
    let mut out = String::new();
    let mut screens = Vec::new();
    let _ = writeln!(out, "--- start ---");
    let card = s.card();
    s.with_screen(|screen| out.push_str(&render_with(card.as_ref(), screen, &colors)));
    for (index, step) in steps.iter().enumerate() {
        let log_before = s.log_text().len();
        let raw_before = s.raw_output().len();
        let since = Instant::now();
        match &step.act {
            Act::Send(bytes) => s.send(bytes),
            Act::Resize(rows, cols) => s.resize(*rows, *cols),
            Act::Fifo(line) => {
                let mut fifo = std::fs::OpenOptions::new()
                    .write(true)
                    .open(tree.0.join("async.fifo"))
                    .expect("the async prompt fixture's FIFO");
                std::io::Write::write_all(&mut fifo, format!("{line}\n").as_bytes())
                    .expect("writing to the FIFO");
            }
        }
        settle(&s, &step.name, since);
        let under = match (profile, full) {
            (None, _) => String::new(),
            (Some(profile), false) => format!(" under {profile:?}"),
            (Some(profile), true) => format!(" under {profile:?}, every frame whole"),
        };
        check_invariants_with(&s, &format!("{}{under}", step.name), log_before, &colors);
        if let Err(err) = suite::frame_errors(&s, &format!("{}{under}", step.name), raw_before) {
            panic!("{err}");
        }
        let card = s.card();
        s.with_screen(|screen| check(index, card.as_ref(), screen, &colors));
        let _ = writeln!(out, "--- after {} ---", step.name);
        s.with_screen(|screen| out.push_str(&render_with(card.as_ref(), screen, &colors)));
        screens.push(s.with_screen(Clone::clone));
    }
    (out, screens)
}

/// Steps from several parts.
fn steps(parts: Vec<Vec<Step>>) -> Vec<Step> {
    parts.into_iter().flatten().collect()
}

fn one(name: &str, bytes: &[u8]) -> Vec<Step> {
    vec![send(name, bytes)]
}

#[test]
fn ls_options() {
    scenario(
        "ls_options",
        "",
        steps(vec![
            typed("ls -"),
            one("Down", DOWN),
            one("Down", DOWN),
            one("Tab", TAB),
        ]),
    );
}

#[test]
fn ls_enter_runs() {
    scenario(
        "ls_enter_runs",
        "",
        steps(vec![typed("ls"), one("Enter", ENTER)]),
    );
}

#[test]
fn cd_tab_into_dir() {
    scenario(
        "cd_tab_into_dir",
        "",
        steps(vec![
            typed("cd fix"),
            one("Tab", TAB),
            one("Tab again", TAB),
            one("Esc", ESC),
        ]),
    );
}

#[test]
fn subcommands_after_space() {
    scenario(
        "subcommands_after_space",
        "",
        steps(vec![
            typed("faccmd "),
            one("Tab", TAB),
            typed("config "),
            one("Down", DOWN),
            one("Enter (inserts)", ENTER),
            one("Enter (runs)", ENTER),
        ]),
    );
}

/// The user's shell functions are commands too: `faccmd` (a function in
/// the fixture `.zshrc`) is offered at command position, above the history
/// line, and Tab inserts it.
#[test]
fn function_at_command_position() {
    scenario(
        "function_at_command_position",
        "",
        steps(vec![typed("facc"), one("Tab", TAB)]),
    );
}

/// A file name with a heart with VS16, a ZWJ family and a skin tone: the
/// characters terminals disagree on the width of.
const EMOJI_FILE: &str =
    "hello\u{2764}\u{fe0f}\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}\u{1f44d}\u{1f3fd}.txt";

/// A `.zshrc` line that creates `EMOJI_FILE` in the fixture cwd (a redirect,
/// as `touch` is not on the fixture PATH; byte escapes, so it does not depend
/// on the shell's locale).
fn make_emoji_file() -> String {
    let escaped: String = EMOJI_FILE
        .bytes()
        .map(|byte| format!("\\x{byte:02x}"))
        .collect();
    format!(": > $'{escaped}'")
}

/// The popup draws the file without the characters terminals disagree on
/// (the default profile; the snapshot is read by hand), and every profile
/// and frame style agrees.
#[test]
fn emoji_file_names_are_drawn_at_one_width() {
    scenario(
        "emoji_file_names",
        &make_emoji_file(),
        steps(vec![typed("ls hel"), one("Down", DOWN), one("Tab", TAB)]),
    );
}

/// What Tab inserts is the file's real name, byte for byte: the redirect
/// writes what the shell received into a file.
#[test]
fn accepting_an_emoji_file_name_inserts_its_real_bytes() {
    let (s, tree) = start(&make_emoji_file(), 24, 80);
    let out = tree.0.join("accepted");
    step(&s, "printf", b"printf %s hel");
    step(&s, "Tab", TAB);
    step(
        &s,
        "redirect and run",
        format!(" > {}\r", out.display()).as_bytes(),
    );
    let deadline = Instant::now() + harness::TIMEOUT;
    let written = loop {
        match std::fs::read(&out) {
            Ok(bytes) if !bytes.is_empty() => break bytes,
            _ if Instant::now() > deadline => panic!("nothing written\n{}", s.screen_text()),
            _ => std::thread::sleep(std::time::Duration::from_millis(20)),
        }
    };
    assert_eq!(written, EMOJI_FILE.as_bytes());
}

/// A name only zsh offers (with `glob_dots`, zsh lists a dot file for an empty
/// word and fac's own listing does not) with a ZWJ, which zsh prints as
/// `$'\342\200\215'`: it is shown by its real name, and accepting it inserts
/// the real bytes.
#[test]
fn accepting_a_name_only_zsh_offers_inserts_its_real_bytes() {
    let rc = "setopt glob_dots; : > $'.a\\xe2\\x80\\x8db'";
    let (s, tree) = start(rc, 24, 80);
    let out = tree.0.join("accepted");
    step(&s, "printf", b"printf %s ");
    s.wait_text(".a", harness::TIMEOUT);
    let shown = s.screen_text();
    assert!(!shown.contains("\\342"), "{shown}");
    step(&s, "Tab", TAB);
    step(
        &s,
        "redirect and run",
        format!(" > {}\r", out.display()).as_bytes(),
    );
    let deadline = Instant::now() + harness::TIMEOUT;
    let written = loop {
        match std::fs::read(&out) {
            Ok(bytes) if !bytes.is_empty() => break bytes,
            _ if Instant::now() > deadline => panic!("nothing written\n{}", s.screen_text()),
            _ => std::thread::sleep(std::time::Duration::from_millis(20)),
        }
    };
    assert_eq!(written, ".a\u{200d}b".as_bytes());
}

#[test]
fn unicode_echo() {
    scenario(
        "unicode_echo",
        "",
        steps(vec![typed("echo ✅"), one("Enter", ENTER)]),
    );
}

#[test]
fn esc_suppresses_until_next_word() {
    scenario(
        "esc_suppresses_until_next_word",
        "",
        steps(vec![
            typed("faccmd b"),
            one("Esc", ESC),
            typed("u"),
            typed(" "),
            typed("x"),
        ]),
    );
}

/// The popup comes back at the next word (the scenario above ends in a word
/// that has nothing to offer).
#[test]
fn esc_suppression_ends_at_the_next_word() {
    scenario(
        "esc_suppression_ends_at_the_next_word",
        "",
        steps(vec![
            typed("faccmd con"),
            one("Esc", ESC),
            typed("fig"),
            typed(" "),
            typed("a"),
        ]),
    );
}

#[test]
fn up_from_nothing() {
    scenario(
        "up_from_nothing",
        "",
        steps(vec![
            typed("faccmd "),
            one("Up", UP),
            one("Up", UP),
            // Typing starts the new list with nothing selected.
            one("Down", DOWN),
            typed("b"),
        ]),
    );
}

#[test]
fn history_item() {
    scenario(
        "history_item",
        "",
        steps(vec![
            typed("git c"),
            // History ranks last: Up from nothing selects it.
            one("Up", UP),
            one("Enter (inserts)", ENTER),
            one("Enter (runs)", ENTER),
        ]),
    );
}

#[test]
fn paste_with_newline() {
    scenario(
        "paste_with_newline",
        "",
        steps(vec![
            typed("faccmd "),
            one("paste", b"\x1b[200~echo one\recho two\r\x1b[201~"),
            // zsh keeps a bracketed paste in the buffer: Enter runs it.
            one("Enter (runs the pasted lines)", ENTER),
        ]),
    );
}

#[test]
fn alt_screen_app() {
    scenario(
        "alt_screen_app",
        "",
        steps(vec![
            one(
                "command typed",
                br"printf '\e[?1049h'; read -k2; printf '\e[?1049l'",
            ),
            one("Enter (starts the app)", ENTER),
            one("l (typed inside the app)", b"l"),
            one("x (ends the app)", b"x"),
            one("l (at the prompt again)", b"l"),
        ]),
    );
}

#[test]
fn resize_while_open() {
    scenario(
        "resize_while_open",
        "",
        steps(vec![
            typed("faccmd "),
            vec![resize("resize to 30 rows", 30, 80)],
            typed("b"),
        ]),
    );
}

/// A command without a Fig spec (`faccmd` is a fixture function) keeps
/// phase 1's list: zsh's completions, files and history.
#[test]
fn no_spec_faccmd() {
    scenario(
        "no_spec_faccmd",
        "",
        steps(vec![
            typed("faccmd c"),
            one("Down", DOWN),
            one("Tab", TAB),
            typed("a"),
            one("Tab", TAB),
        ]),
    );
}

/// A parameter name on a line without a spec (`faccmd` is a fixture
/// function) is left to zsh, as in phase 1: this snapshot is the one fac
/// gave before spec suggestions (b5e9dee).
#[test]
fn no_spec_parameter() {
    scenario(
        "no_spec_parameter",
        "",
        steps(vec![one("faccmd $", b"faccmd $"), typed("HO")]),
    );
}

/// `echo`'s spec has a generator (trigger `$`) that lists the shell's
/// exported variables as `$NAME` for its first word, and for a word that
/// starts with `$`.
#[test]
fn echo_env_generator() {
    scenario(
        "echo_env_generator",
        "",
        steps(vec![one("echo $", b"echo $"), typed("HO")]),
    );
}

#[test]
fn smkx_arrows() {
    scenario(
        "smkx_arrows",
        SMKX,
        steps(vec![
            typed("faccmd "),
            one("Down (SS3)", b"\x1bOB"),
            one("Tab", TAB),
        ]),
    );
}

#[test]
fn spaces_in_names() {
    scenario(
        "spaces_in_names",
        "",
        steps(vec![typed("cd My"), one("Tab", TAB)]),
    );
}

#[test]
fn quoted_word() {
    scenario(
        "quoted_word",
        "",
        steps(vec![typed("cd 'fix"), one("Tab", TAB)]),
    );
}

#[test]
fn quoted_dir_with_space() {
    scenario(
        "quoted_dir_with_space",
        "",
        steps(vec![typed("cd 'My"), one("Tab", TAB)]),
    );
}

/// An 8-row terminal (24 columns): the popup has no room for its full
/// height, so it keeps one bottom-line row under its 6 list rows (a card
/// narrower than its minimum, the screen's 22 columns), and Down keeps the
/// selection among them without scrolling the screen.
#[test]
fn short_screen() {
    scenario_sized(
        "short_screen",
        (8, 24),
        "",
        steps(vec![
            typed("git "),
            one("Down", DOWN),
            one("Down", DOWN),
            one("Down", DOWN),
            one("Down", DOWN),
        ]),
        |index, card, screen, colors| {
            let text = render_with(card, screen, colors);
            let state = popup_state_with(card, screen, colors)
                .unwrap_or_else(|| panic!("step {index}: no popup\n{text}"));
            assert!((1..=6).contains(&state.list_rows), "step {index}\n{text}");
            assert_eq!(state.top, 1, "step {index}\n{text}");
            // From `git ` on (described subcommands): the 7 rows below the
            // prompt, one of them the bottom line's, cut from its `D`.
            if index >= 3 {
                assert_eq!(state.bottom, 7, "step {index}\n{text}");
                assert_eq!((state.list_rows, state.bottom_rows), (6, 1), "{text}");
                let card = card.unwrap_or_else(|| panic!("step {index}: no popup"));
                assert!(card.d > 1, "step {index}: {card:?}");
            }
            // Steps 0-3 type `git `; 4-7 are the Downs.
            if index >= 4 {
                let selected = state
                    .selected
                    .unwrap_or_else(|| panic!("step {index}: nothing selected\n{text}"));
                assert!(
                    selected >= state.top && selected < state.top + state.list_rows,
                    "step {index}: the selection is off the popup\n{text}"
                );
            } else {
                assert_eq!(state.selected, None, "step {index}\n{text}");
            }
        },
    );
}

/// A 5-row pane (4 list rows, no room for the bottom line) and a long list:
/// Down scrolls it and keeps the selection on the popup; after a letter
/// (nothing selected again), Up shows and selects the last item.
#[test]
fn short_pane_scrolls() {
    scenario_sized(
        "short_pane_scrolls",
        (5, 80),
        "",
        steps(vec![
            typed("git "),
            one("Down", DOWN),
            one("Down", DOWN),
            one("Down", DOWN),
            one("Down", DOWN),
            one("Down", DOWN),
            one("Down", DOWN),
            one("Down", DOWN),
            typed("c"),
            one("Up", UP),
        ]),
        |index, card, screen, colors| {
            let text = render_with(card, screen, colors);
            let state = popup_state_with(card, screen, colors)
                .unwrap_or_else(|| panic!("step {index}: no popup\n{text}"));
            // Below the prompt, without the bottom line, which has no room.
            assert_eq!(state.top, 1, "step {index}\n{text}");
            assert_eq!(state.bottom_rows, 0, "step {index}\n{text}");
            // From `git ` on, the long list: every row but the prompt's, 4
            // list rows (`gi` and `git` have fewer items).
            if index >= 3 {
                assert_eq!(
                    (state.bottom, state.list_rows),
                    (4, 4),
                    "step {index}\n{text}"
                );
            }
            // Steps 0-3 type `git `; 4-10 are the Downs, 11 is `c`, 12 Up.
            match index {
                4..=10 => {
                    let selected = state
                        .selected
                        .unwrap_or_else(|| panic!("step {index}: nothing selected\n{text}"));
                    assert!(
                        selected >= state.top && selected <= state.bottom,
                        "step {index}: the selection is off the popup\n{text}"
                    );
                }
                12 => {
                    // The last item is on the last row.
                    assert_eq!(state.selected, Some(state.bottom), "{text}");
                }
                _ => assert_eq!(state.selected, None, "step {index}\n{text}"),
            }
        },
    );
}

/// The first list row's label.
fn first_label(s: &Session) -> Option<String> {
    let card = s.card()?;
    let colors: Colors = harness::colors();
    Some(s.with_screen(|screen| harness::label(screen, &card, 0, &colors)))
}

/// What was accepted is remembered across sessions: at `git ch`, an item that
/// is not first is accepted with Tab, the session ends, and in a new session
/// with the same data dir that item is first at `git ch`.
#[test]
fn an_accepted_item_comes_first_in_the_next_session() {
    let data = std::env::temp_dir().join(format!("fac-usage-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data);
    let env = || vec![("FAC_DATA_DIR".to_owned(), data.display().to_string())];

    let (first, accepted) = {
        let (s, _tree) = start_with_env("", 24, 80, env());
        step(&s, "git ch", b"git ch");
        let first = first_label(&s).unwrap_or_else(|| panic!("no popup\n{}", s.screen_text()));
        // The first Down selects the first item, the second the next.
        step(&s, "Down", DOWN);
        step(&s, "Down again", DOWN);
        step(&s, "Tab", TAB);
        let line = s.with_screen(|screen| row_text(screen, 0));
        let accepted = line
            .strip_prefix("% git ")
            .map(str::trim)
            .unwrap_or_else(|| panic!("unexpected prompt line {line:?}"))
            .to_owned();
        assert_ne!(accepted, first, "the second item was accepted");
        step(&s, "Ctrl-U", b"\x15");
        step(&s, "exit", b"exit");
        s.send(ENTER);
        s.wait_exit(harness::TIMEOUT);
        (first, accepted)
    };

    let saved = std::fs::read_to_string(data.join("usage.json")).expect("usage.json is saved");
    assert!(
        saved.contains(&format!("\"git\":{{\"{accepted}\":")),
        "{saved}"
    );
    assert!(!data.join("usage.json.tmp").exists());

    let (s, _tree) = start_with_env("", 24, 80, env());
    step(&s, "git ch", b"git ch");
    let top = first_label(&s).unwrap_or_else(|| panic!("no popup\n{}", s.screen_text()));
    assert_eq!(top, accepted, "was {first:?} before");
    drop(s);
    let _ = std::fs::remove_dir_all(&data);
}

// ---------------------------------------------------------------------
// Phase 2 end to end: the card, the bottom line, generators, sudo, option
// values, recency, short panes, palettes, icons, the popout, 256 colours,
// `cd` in an empty directory and full-screen apps.
// ---------------------------------------------------------------------

const CTRL_K: &[u8] = b"\x0b";
const CTRL_U: &[u8] = b"\x15";

/// The popup fac's log says is on the screen.
fn card_of(s: &Session) -> Card {
    s.card()
        .unwrap_or_else(|| panic!("no popup\n{}", s.screen_text()))
}

/// The labels of the list rows now.
fn labels_now(s: &Session) -> Vec<String> {
    let card = card_of(s);
    s.with_screen(|screen| harness::labels(screen, &card, &harness::colors()))
}

/// The text of the bottom line's drawn rows, joined with a space.
fn bottom_text(s: &Session) -> String {
    let card = card_of(s);
    s.with_screen(|screen| {
        (0..card.lines)
            .map(|line| {
                screen
                    .rows(card.col, card.cols)
                    .nth(usize::from(card.bottom_row() + line))
                    .unwrap_or_default()
                    .trim()
                    .to_owned()
            })
            .collect::<Vec<_>>()
            .join(" ")
    })
}

/// The prompt row's text and the cursor.
fn prompt_line(s: &Session) -> (String, (u16, u16)) {
    s.with_screen(|screen| {
        let (row, col) = screen.cursor_position();
        (row_text(screen, row).trim_end().to_owned(), (row, col))
    })
}

/// The log's `popup drawn items=N` of the last draw.
fn drawn_items(s: &Session) -> usize {
    let log = s.log_text();
    let line = log
        .lines()
        .rfind(|line| line.contains("popup drawn "))
        .unwrap_or_else(|| panic!("no popup was drawn\n{log}"));
    line.split(" items=")
        .nth(1)
        .and_then(|rest| rest.split(' ').next())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("unparsable {line:?}"))
}

/// Make `dir` a git repo on `main` with these other branches.
fn git_repo(dir: &std::path::Path, branches: &[&str]) {
    let mut commands: Vec<Vec<&str>> = vec![
        vec!["init", "-q", "-b", "main"],
        vec![
            "-c",
            "user.name=fac",
            "-c",
            "user.email=fac@example.com",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "init",
        ],
    ];
    commands.extend(branches.iter().map(|branch| vec!["branch", branch]));
    for args in commands {
        let status = std::process::Command::new("/usr/bin/git")
            .args(&args)
            .current_dir(dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .status()
            .expect("running git");
        assert!(status.success(), "git {args:?}");
    }
}

/// The bottom line follows the selection: at `git `, `bisect`'s description
/// wraps, so the bottom line grows from its one idle line to the two lines of
/// the full description when `bisect` is selected, and shrinks back when the
/// next item (`blame`, which has no description of its own) or a typed
/// letter takes the selection away. (At `git ch` every description fits one
/// line, so `D` is 1; see `git_ch_down_keeps_the_screen_still`.) The
/// screen never scrolls: the rows above the popup, the cursor and the
/// card's place stay the same.
#[test]
fn the_bottom_line_grows_and_shrinks_without_scrolling() {
    use std::cell::RefCell;
    // The prompt line and cursor once `git ` is typed, and the cursor's row
    // and the card's place; every later step must match them (the line and
    // the cursor's column change when a letter is typed).
    /// The prompt line, the cursor, and the card's first row and row count.
    type Anchor = (String, (u16, u16), u16, u16);
    let anchor: RefCell<Option<Anchor>> = RefCell::new(None);
    scenario_sized(
        "the_bottom_line_grows_and_shrinks_without_scrolling",
        (24, 80),
        "",
        steps(vec![
            typed("git "),
            // The first Down selects `add`; the fourth, `bisect`.
            one("Down (add)", DOWN),
            one("Down (apply)", DOWN),
            one("Down (archive)", DOWN),
            one("Down (bisect)", DOWN),
            one("Down (blame)", DOWN),
            typed("b"),
        ]),
        |index, card, screen, _| {
            let card = card.unwrap_or_else(|| panic!("step {index}: no popup"));
            let (cursor_row, cursor_col) = screen.cursor_position();
            // Steps 0-3 type `git `, 4-8 are the Downs, 9 is `b`.
            if index == 3 {
                *anchor.borrow_mut() = Some((
                    row_text(screen, 0).trim_end().to_owned(),
                    (cursor_row, cursor_col),
                    card.row,
                    card.rows,
                ));
            }
            if index >= 3 {
                let anchor = anchor.borrow();
                let (line, (row, col), top, rows) = anchor.as_ref().expect("anchored at step 3");
                assert_eq!(
                    (cursor_row, card.row, card.rows),
                    (*row, *top, *rows),
                    "step {index}: the screen moved"
                );
                if index < 9 {
                    assert_eq!(
                        (row_text(screen, 0).trim_end(), cursor_col),
                        (line.as_str(), *col),
                        "step {index}: the prompt moved"
                    );
                }
                assert_eq!(card.d, 2, "step {index}: {card:?}");
                assert!(card.lines <= card.d, "step {index}: {card:?}");
            }
            match index {
                // Idle, and on the one-line descriptions: one line.
                3..=6 | 8 | 9 => assert_eq!(card.lines, 1, "step {index}: {card:?}"),
                // `bisect`: its description wraps to two lines.
                7 => {
                    assert_eq!(card.lines, 2, "step {index}: {card:?}");
                    let text: Vec<String> = (card.bottom_row()..card.bottom_row() + 2)
                        .map(|row| {
                            screen
                                .rows(card.col, card.cols)
                                .nth(usize::from(row))
                                .unwrap_or_default()
                                .trim()
                                .to_owned()
                        })
                        .collect();
                    assert_eq!(
                        text.join(" "),
                        "Use binary search to find the commit that introduced a bug",
                        "step {index}"
                    );
                }
                _ => {}
            }
            // Typing takes the selection away.
            if index == 9 {
                assert_eq!(card.selected, None, "{card:?}");
            }
        },
    );
}

/// `git ch` (every visible description fits one line, so `D` is 1): Down
/// selects the first item and the bottom line shows its description on its
/// one line; nothing moves.
#[test]
fn git_ch_down_keeps_the_screen_still() {
    let (s, _tree) = start("", 24, 80);
    step(&s, "git ch", b"git ch");
    let idle = card_of(&s);
    let (line, cursor) = prompt_line(&s);
    assert_eq!((idle.lines, idle.d), (1, 1), "{idle:?}");
    assert_eq!(idle.selected, None);
    // Idle, the bottom line describes the top item.
    assert_eq!(
        bottom_text(&s),
        "Switch branches or restore working tree files"
    );
    step(&s, "Down", DOWN);
    let down = card_of(&s);
    assert_eq!(down.selected, Some(0));
    assert_eq!((down.row, down.rows, down.lines), (idle.row, idle.rows, 1));
    assert_eq!(prompt_line(&s), (line, cursor));
    assert_eq!(
        bottom_text(&s),
        "Switch branches or restore working tree files"
    );
    step(&s, "Down", DOWN);
    assert_eq!(
        bottom_text(&s),
        "Apply the changes introduced by some existing commits"
    );
}

/// A `git` first on `PATH` (in the fixture's `bin`) that waits for a gate
/// file before it lists branches, so the test decides when they arrive.
/// Everything else goes straight to the real git.
struct GatedGit {
    gate: PathBuf,
}

impl GatedGit {
    fn new(tree: &Tree) -> Self {
        let gate = tree.0.join("gate");
        let git = tree.bin().join("git");
        std::fs::remove_file(&git).expect("removing the git link");
        std::fs::write(
            &git,
            format!(
                "#!/bin/sh\ncase \"$*\" in *branch*) while [ ! -e '{}' ]; do /bin/sleep 0.05; done ;; esac\nexec /usr/bin/git \"$@\"\n",
                gate.display()
            ),
        )
        .expect("writing the gated git");
        std::fs::set_permissions(&git, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .expect("making it executable");
        Self { gate }
    }

    /// Let the branch listing through.
    fn open(&self) {
        std::fs::write(&self.gate, "").expect("opening the gate");
    }
}

/// The selected list row's label, if any.
fn selected_label(s: &Session) -> Option<String> {
    let card = card_of(s);
    let colors = harness::colors();
    s.with_screen(|screen| {
        harness::selected_row(screen, &card, &colors)
            .map(|index| harness::label(screen, &card, index, &colors))
    })
}

/// `git checkout ` in a repo with branches `main`, `feature-one` and
/// `fix/two`: the static items show first, the branches merge in when the
/// generator answers (held back by a gate file), the selection survives the
/// merge, and accepting a branch inserts it with no space after it.
#[test]
fn git_checkout_branches_merge_in_and_accepting_adds_no_space() {
    let (s, tree) = start("", 24, 80);
    let git = GatedGit::new(&tree);
    git_repo(&tree.cwd(), &["feature-one", "fix/two"]);
    s.send(b"git checkout ");
    s.wait_card(
        "the static items",
        |card, _| card.is_some_and(|card| card.list_rows > 0),
        harness::TIMEOUT,
    );
    s.wait_quiet(QUIET, harness::TIMEOUT);
    let before = labels_now(&s);
    assert!(
        !before
            .iter()
            .any(|label| ["main", "feature-one", "fix/two"].contains(&label.as_str())),
        "the branches wait for the generator: {before:?}"
    );
    let before_items = drawn_items(&s);
    // Down, Down: the second row is selected.
    s.send(DOWN);
    s.send(DOWN);
    s.wait_card(
        "the second row selected",
        |card, _| card.is_some_and(|card| card.selected == Some(1)),
        harness::TIMEOUT,
    );
    let selected = selected_label(&s).expect("a selected row");
    assert_eq!(selected, before[1], "{before:?}");

    git.open();
    s.wait_card(
        "the branches",
        |card, screen| {
            card.is_some_and(|card| {
                let labels = harness::labels(screen, card, &harness::colors());
                labels.iter().any(|label| label == "main")
            })
        },
        harness::TIMEOUT,
    );
    settle(&s, "the branches merged in", Instant::now());
    let after = labels_now(&s);
    // Fig ranks them by priority (100 for the current branch, 75 for the
    // others) above the rest.
    assert_eq!(after[..3], ["main", "feature-one", "fix/two"], "{after:?}");
    assert!(
        drawn_items(&s) > before_items,
        "the branches are extra items"
    );
    // The selection is the same item, wherever it moved to.
    assert_eq!(selected_label(&s), Some(selected), "{after:?}");

    // A branch is inserted without a space after it, as in Fig.
    step(&s, "clear", CTRL_U);
    step(&s, "typed", b"git checkout fix/t");
    assert_eq!(labels_now(&s).first().map(String::as_str), Some("fix/two"));
    step(&s, "Tab", TAB);
    let (line, cursor) = prompt_line(&s);
    assert_eq!(line, "% git checkout fix/two");
    assert_eq!(cursor, (0, 22), "a space after the branch");
    assert_eq!(
        card_of(&s).selected,
        None,
        "the popup waits for the next word"
    );
}

/// `sudo git ch` offers what `git ch` does, and accepting `checkout` under
/// `sudo` counts as `git`'s `checkout` in the recency store.
#[test]
fn sudo_git_ch_is_git_ch() {
    let data = std::env::temp_dir().join(format!("fac-sudo-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data);
    let env = vec![("FAC_DATA_DIR".to_owned(), data.display().to_string())];
    {
        let (s, _tree) = start_with_env("", 24, 80, env);
        step(&s, "git ch", b"git ch");
        let plain = (labels_now(&s), drawn_items(&s), bottom_text(&s));
        assert!(plain.0.len() > 3, "{plain:?}");
        step(&s, "clear", CTRL_U);
        step(&s, "sudo git ch", b"sudo git ch");
        let under_sudo = (labels_now(&s), drawn_items(&s), bottom_text(&s));
        assert_eq!(under_sudo, plain);
        // Accept `checkout` (the top item) under sudo.
        step(&s, "Down", DOWN);
        assert_eq!(selected_label(&s).as_deref(), Some("checkout"));
        step(&s, "Tab", TAB);
        assert_eq!(prompt_line(&s).0, "% sudo git checkout");
        step(&s, "Ctrl-U", CTRL_U);
        step(&s, "exit", b"exit");
        s.send(ENTER);
        s.wait_exit(harness::TIMEOUT);
    }
    let saved = std::fs::read_to_string(data.join("usage.json")).expect("usage.json is saved");
    assert!(saved.contains("\"git\":{\"checkout\":"), "{saved}");
    assert!(!saved.contains("\"sudo\""), "{saved}");
    let _ = std::fs::remove_dir_all(&data);
}

/// `ls --color=`: an option that takes `=` and has static suggestions
/// (`always`, `auto`, `never`). The values list once `=` is typed, filter
/// on what follows the `=`, and accepting one replaces only that part.
#[test]
fn option_values_after_equals() {
    let (s, _tree) = start("", 24, 80);
    step(&s, "ls --color=", b"ls --color=");
    assert_eq!(
        labels_now(&s),
        ["always, yes, force", "auto", "never, no, none"]
    );
    // Filtering looks at the value only: `--color` itself does not count.
    step(&s, "n", b"n");
    assert_eq!(
        labels_now(&s).first().map(String::as_str),
        Some("never, no, none")
    );
    assert!(!labels_now(&s).iter().any(|label| label.starts_with('-')));
    step(&s, "Ctrl-U", CTRL_U);
    step(&s, "ls --color=a", b"ls --color=a");
    let labels = labels_now(&s);
    assert!(
        labels.iter().any(|label| label == "always, yes, force")
            && labels.iter().any(|label| label == "auto"),
        "{labels:?}"
    );
    assert!(
        !labels.iter().any(|label| label.starts_with("never")),
        "{labels:?}"
    );
    // Accepting `auto` replaces the `a` after the `=`, nothing before it.
    while selected_label(&s).as_deref() != Some("auto") {
        assert!(card_of(&s).selected.is_none_or(|row| row < 2), "{labels:?}");
        step(&s, "Down", DOWN);
    }
    step(&s, "Tab", TAB);
    assert_eq!(prompt_line(&s).0, "% ls --color=auto");
    // And a value picked right after the `=`, with the popup as it opened.
    step(&s, "Ctrl-U", CTRL_U);
    step(&s, "ls --color=", b"ls --color=");
    step(&s, "Down", DOWN);
    step(&s, "Down", DOWN);
    step(&s, "Down", DOWN);
    step(&s, "Tab", TAB);
    assert_eq!(prompt_line(&s).0, "% ls --color=never");
}

/// What was accepted comes first in the next session, in the card: the
/// third item accepted at `git ch` is the top row after a restart with the
/// same data dir, nothing is selected, and the bottom line describes it.
#[test]
fn recency_puts_the_accepted_item_first_in_the_card() {
    let data = std::env::temp_dir().join(format!("fac-recency-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data);
    let env = || vec![("FAC_DATA_DIR".to_owned(), data.display().to_string())];
    let accepted = {
        let (s, _tree) = start_with_env("", 24, 80, env());
        step(&s, "git ch", b"git ch");
        let before = labels_now(&s);
        for _ in 0..3 {
            step(&s, "Down", DOWN);
        }
        let accepted = selected_label(&s).expect("a selected row");
        assert_eq!(accepted, before[2], "{before:?}");
        assert_ne!(accepted, before[0]);
        step(&s, "Tab", TAB);
        step(&s, "Ctrl-U", CTRL_U);
        step(&s, "exit", b"exit");
        s.send(ENTER);
        s.wait_exit(harness::TIMEOUT);
        accepted
    };
    let (s, _tree) = start_with_env("", 24, 80, env());
    step(&s, "git ch", b"git ch");
    let labels = labels_now(&s);
    assert_eq!(labels.first(), Some(&accepted), "{labels:?}");
    assert_eq!(card_of(&s).selected, None);
    let colors = harness::colors();
    assert!(
        s.with_screen(|screen| harness::selected_row(screen, &card_of(&s), &colors))
            .is_none()
    );
    // The bottom line describes the top item (nothing is selected).
    assert_eq!(accepted, "cherry");
    assert_eq!(bottom_text(&s), "find commits not merged upstream");
    drop(s);
    let _ = std::fs::remove_dir_all(&data);
}

/// One Enter per row leaves the prompt on the last row of a `rows`-row pane
/// (the pane is at the bottom of the screen: the popup must scroll it).
fn prompt_at_the_bottom(s: &Session, rows: u16) {
    for _ in 1..rows {
        s.send(ENTER);
    }
    s.wait_for(
        "the prompt on the last row",
        |screen| screen.cursor_position() == (rows - 1, 2),
        harness::TIMEOUT,
    );
    s.wait_quiet(QUIET, harness::TIMEOUT);
}

/// A pane of 6 rows with the prompt on its last row: the popup scrolls the
/// screen once to make room, then fits what the rows below the prompt allow,
/// dropping in the plan's order (the bottom line's extra rows, then the
/// bottom line, then list rows); no arrow key scrolls it again.
#[test]
fn a_six_row_pane_at_the_bottom_drops_rows_in_order() {
    for line in ["git ", "ls -"] {
        let (s, _tree) = start("", 6, 80);
        prompt_at_the_bottom(&s, 6);
        step(&s, line, line.as_bytes());
        let card = card_of(&s);
        // The prompt scrolled up to the top row, leaving 5 rows below it:
        // 5 list rows and no bottom line, where 6 list rows and a bottom
        // line of `D` rows were wanted.
        assert_eq!(
            s.with_screen(vt100::Screen::cursor_position).0,
            0,
            "{line:?}"
        );
        assert_eq!((card.row, card.last_row()), (1, 5), "{line:?}: {card:?}");
        assert_eq!(
            (card.list_rows, card.bottom, card.lines),
            (5, 0, 0),
            "{line:?}: {card:?}"
        );
        assert!(drawn_items(&s) > 5, "{line:?}: a list longer than the card");
        let still = prompt_line(&s);
        for key in 0..8 {
            step(&s, "Down", DOWN);
            let now = card_of(&s);
            assert_eq!(
                (now.row, now.rows),
                (card.row, card.rows),
                "{line:?} Down {key}"
            );
            assert_eq!(prompt_line(&s), still, "{line:?} Down {key}");
            assert!(now.selected.is_some(), "{line:?} Down {key}: {now:?}");
        }
        step(&s, "Up", UP);
        let now = card_of(&s);
        assert_eq!((now.row, now.rows), (card.row, card.rows), "{line:?} Up");
        assert_eq!(prompt_line(&s), still);
    }
}

/// The order the rows are dropped in, by how many rows lie below the
/// prompt at `git ` (`D` is 2 there, 6 list rows wanted): 7 rows below keep
/// 6 list rows and one bottom line (the extra bottom-line row is dropped
/// first); 6 below drop the bottom line; 5 below drop a list row.
#[test]
fn rows_are_dropped_in_the_plans_order() {
    for (rows, expected) in [
        // (list rows, bottom rows kept, lines drawn idle)
        (9, (6, 2, 1)),
        (8, (6, 1, 1)),
        (7, (6, 0, 0)),
        (6, (5, 0, 0)),
        (5, (4, 0, 0)),
    ] {
        let (s, _tree) = start("", rows, 80);
        step(&s, "git ", b"git ");
        let card = card_of(&s);
        assert_eq!(
            (card.list_rows, card.bottom, card.lines),
            expected,
            "{rows} rows: {card:?}"
        );
        // Down scrolls the list, never the screen, and a selected `bisect`
        // gets no more lines than the room kept.
        for _ in 0..5 {
            step(&s, "Down", DOWN);
            let now = card_of(&s);
            assert_eq!((now.row, now.rows), (card.row, card.rows), "{rows} rows");
            assert!(now.lines <= card.bottom, "{rows} rows: {now:?}");
        }
    }
}

/// What the open popup is painted with: the background of an unselected
/// list row, of the selected row and of the bottom line.
#[derive(Debug, PartialEq, Eq)]
struct Painted {
    card: vt100::Color,
    selected: vt100::Color,
    band: vt100::Color,
}

/// Type `git `, select the first item, and read the three backgrounds.
fn paint_git(s: &Session) -> Painted {
    step(s, "git ", b"git ");
    step(s, "Down", DOWN);
    let card = card_of(s);
    assert!(card.list_rows >= 4 && card.lines >= 1, "{card:?}");
    assert_eq!(card.selected, Some(0), "{card:?}");
    s.with_screen(|screen| {
        let at = |row: u16| {
            screen
                .cell(row, card.col + 1)
                .unwrap_or_else(|| panic!("no cell at row {row}"))
                .bgcolor()
        };
        Painted {
            card: at(card.row + 3),
            selected: at(card.row),
            band: at(card.bottom_row()),
        }
    })
}

/// A config file that sets `theme` (and `extra` lines), in a temp dir of its
/// own; the `FAC_CONFIG` environment that points at it.
struct ConfigFile {
    dir: PathBuf,
}

impl ConfigFile {
    fn new(label: &str, text: &str) -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("fac-config-{label}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("creating the config dir");
        std::fs::write(dir.join("config.toml"), text).expect("writing the config");
        Self { dir }
    }

    fn env(&self) -> Vec<(String, String)> {
        vec![(
            "FAC_CONFIG".to_owned(),
            self.dir.join("config.toml").display().to_string(),
        )]
    }
}

impl Drop for ConfigFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The popup's colours match `expected`: the card, the selected row and the
/// bottom line, and no popup colour outside the card after a step.
fn assert_painted(s: &Session, expected: &Colors, what: &str) {
    let painted = paint_git(s);
    assert_eq!(
        painted,
        Painted {
            card: expected.card,
            selected: expected.selected,
            band: expected.band,
        },
        "{what}"
    );
    check_invariants_with(s, what, 0, expected);
}

/// A light terminal background: the card is a shade darker than it, and the
/// selected row is default's light tint.
#[test]
fn light_palette() {
    let background = (0xfb, 0xfb, 0xf9);
    let roles = harness::roles_for("default", background, Some(harness::ONE_DARK));
    assert!(!roles.dark);
    let light = |rgb| fac::popup::color::to_oklch(rgb).l;
    assert!(light(roles.card) < light(background), "{roles:?}");
    assert!(light(roles.selected) > 0.7, "a light tint: {roles:?}");
    let expected = harness::colors_of(&roles, background);
    let (s, _tree) = start_with("", 24, 80, Vec::new(), |opts| opts.background = background);
    assert_painted(&s, &expected, "the light palette");
    // The card is not the terminal's own background index.
    assert_ne!(
        expected.card,
        vt100::Color::Idx(fac::popup::theme::nearest_256(background))
    );
}

/// The terminal answers OSC 11 but ignores OSC 4: the default theme tints the
/// selected row with the derived accent, `oklch(0.6 0.13 262)` on dark.
#[test]
fn default_without_an_osc_4_answer() {
    let background = SpawnOpts::default().background;
    let roles = harness::roles_for("default", background, None);
    assert!(roles.dark);
    let reported = harness::roles_for("default", background, Some(harness::ONE_DARK));
    assert_ne!(
        roles.selected, reported.selected,
        "the fallback accent is not One Dark's blue"
    );
    let expected = harness::colors_of(&roles, background);
    let (s, _tree) = start_with("", 24, 80, Vec::new(), |opts| opts.palette = None);
    assert_painted(&s, &expected, "default without OSC 4");
}

/// `theme = "default"` paints the same colours as no config, and an
/// unknown theme name gives them too, plus one warning in the log.
#[test]
fn the_theme_through_the_config() {
    let defaults = SpawnOpts::default();
    let expected = harness::colors_for("default", defaults.background, defaults.palette);
    assert_eq!(expected, harness::colors());
    let config = ConfigFile::new("theme-named", "theme = \"default\"\n");
    let (s, _tree) = start_with("", 24, 80, config.env(), |_| {});
    assert_painted(&s, &expected, "default");
    assert!(!s.log_text().contains("using default"), "{}", s.log_text());

    let config = ConfigFile::new("theme-unknown", "theme = \"no-such-theme\"\n");
    let (s, _tree) = start_with("", 24, 80, config.env(), |_| {});
    assert_painted(&s, &expected, "no-such-theme");
    let log = s.log_text();
    let warnings: Vec<&str> = log
        .lines()
        .filter(|line| line.contains("theme \"no-such-theme\""))
        .collect();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("WARN"), "{warnings:?}");
    assert!(warnings[0].contains("using default"), "{warnings:?}");
}

/// A temp `HOME` with `config.toml` (`theme = "mine"`) and the theme file
/// `mine.json` holding `theme_file`.
struct ThemeHome {
    home: PathBuf,
}

impl ThemeHome {
    fn new(theme_file: &str) -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let home = std::env::temp_dir().join(format!("fac-theme-home-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let config = home.join(".config/fac");
        std::fs::create_dir_all(config.join("themes")).expect("creating the themes dir");
        std::fs::write(config.join("config.toml"), "theme = \"mine\"\n").expect("config");
        std::fs::write(config.join("themes/mine.json"), theme_file).expect("theme file");
        Self { home }
    }

    fn env(&self) -> Vec<(String, String)> {
        vec![("HOME".to_owned(), self.home.display().to_string())]
    }
}

impl Drop for ThemeHome {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

/// A Fig v1 theme file: its card and selected colours show on screen.
#[test]
fn a_fig_theme_file() {
    let home = ThemeHome::new(
        r##"{
  "version": "1.0",
  "theme": {
    "backgroundColor": "#302050",
    "textColor": "#e0e0e0",
    "matchBackgroundColor": "#505090",
    "selection": { "backgroundColor": "#803030", "textColor": "#ffffff" },
    "description": { "textColor": "#c0c0c0" }
  }
}"##,
    );
    let (s, _tree) = start_with("", 24, 80, home.env(), |_| {});
    let painted = paint_git(&s);
    let idx = |rgb| vt100::Color::Idx(fac::popup::theme::nearest_256(rgb));
    assert_eq!(painted.card, idx((0x30, 0x20, 0x50)));
    assert_eq!(painted.selected, idx((0x80, 0x30, 0x30)));
    assert!(matches!(painted.band, vt100::Color::Idx(_)));
    assert_ne!(painted.band, painted.card, "the bottom line stands out");
    assert!(!s.log_text().contains("theme \"mine\""), "{}", s.log_text());
}

/// A theme file that is not a usable v1 theme falls back to the default theme
/// and logs one warning.
#[test]
fn a_bad_theme_file_falls_back_with_one_warning() {
    for (what, text) in [
        ("not JSON", "{ nope"),
        ("a v2 theme", r#"{ "version": "2.0", "theme": {} }"#),
        ("a missing colour", r#"{ "version": "1.0", "theme": {} }"#),
    ] {
        let home = ThemeHome::new(text);
        let (s, _tree) = start_with("", 24, 80, home.env(), |_| {});
        let painted = paint_git(&s);
        let expected = harness::colors();
        assert_eq!(
            painted,
            Painted {
                card: expected.card,
                selected: expected.selected,
                band: expected.band,
            },
            "{what}"
        );
        let log = s.log_text();
        let warnings: Vec<&str> = log
            .lines()
            .filter(|line| line.contains("theme \"mine\""))
            .collect();
        assert_eq!(warnings.len(), 1, "{what}: {warnings:?}");
        assert!(warnings[0].contains("WARN"), "{what}: {warnings:?}");
        assert!(
            warnings[0].contains("using default"),
            "{what}: {warnings:?}"
        );
    }
}

/// The first list row's text as drawn, trimmed.
fn first_row(s: &Session) -> String {
    let card = card_of(s);
    s.with_screen(|screen| {
        screen
            .rows(card.col, card.cols)
            .nth(usize::from(card.row))
            .unwrap_or_default()
            .trim()
            .to_owned()
    })
}

/// `icons = "nerd"` draws the Nerd Font glyphs, `icons = "none"` draws no
/// icon column at all, and the default is the plain characters.
#[test]
fn icons_nerd_none_and_plain() {
    for (setting, first) in [
        (None, "$ add [pathspec...]"),
        (Some("plain"), "$ add [pathspec...]"),
        (Some("nerd"), "\u{ea85} add [pathspec...]"),
        (Some("none"), "add [pathspec...]"),
    ] {
        let config = ConfigFile::new(
            "icons",
            &setting.map_or_else(String::new, |icons| format!("icons = \"{icons}\"\n")),
        );
        let (s, _tree) = start_with("", 24, 80, config.env(), |_| {});
        step(&s, "git ", b"git ");
        assert_eq!(first_row(&s), first, "icons = {setting:?}");
        // An option's icon is another one.
        step(&s, "Ctrl-U", CTRL_U);
        step(&s, "ls -", b"ls -");
        let option = first_row(&s);
        let expected = match setting {
            Some("nerd") => "\u{f024} ",
            Some("none") => "-",
            _ => "- ",
        };
        assert!(
            option.starts_with(expected),
            "icons = {setting:?}: {option:?}"
        );
    }
}

/// The text of the popout's rows (its rect from the log), joined with a
/// space; `None` without a popout.
fn popout_text(s: &Session) -> Option<String> {
    let (row, col, rows, cols) = card_of(s).popout?;
    Some(s.with_screen(|screen| {
        (row..row + rows)
            .map(|row| {
                screen
                    .rows(col, cols)
                    .nth(usize::from(row))
                    .unwrap_or_default()
                    .trim()
                    .to_owned()
            })
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    }))
}

/// No text and no colour in `rect` (row, col, rows, cols) apart from the
/// terminal's own: what an erase leaves behind.
fn assert_blank(s: &Session, rect: (u16, u16, u16, u16), what: &str) {
    let (row, col, rows, cols) = rect;
    s.with_screen(|screen| {
        for r in row..row + rows {
            for c in col..col + cols {
                let cell = screen.cell(r, c).expect("a cell");
                assert!(
                    !cell.has_contents() && cell.bgcolor() == vt100::Color::Default,
                    "{what}: ({r}, {c}) holds {cell:?}\n{}",
                    s.screen_text()
                );
            }
        }
    });
}

/// `description_popout_key = "ctrl+k"`: the key opens the popout beside the
/// card, Down moves it to the next description, the key closes it, Esc
/// closes the popout and the card, and the erase leaves nothing behind.
#[test]
fn the_popout_opens_follows_and_erases() {
    let config = ConfigFile::new("popout", "description_popout_key = \"ctrl+k\"\n");
    let (s, _tree) = start_with("", 24, 80, config.env(), |_| {});
    step(&s, "git ", b"git ");
    step(&s, "Down", DOWN);
    assert_eq!(card_of(&s).popout, None);
    let card = card_of(&s);

    let log_before = s.log_text().len();
    let since = Instant::now();
    s.send(CTRL_K);
    settle(&s, "Ctrl-K", since);
    check_invariants(&s, "Ctrl-K", log_before);
    let opened = card_of(&s);
    let rect = opened.popout.expect("the popout opens");
    assert_eq!(
        (opened.row, opened.col, opened.rows, opened.cols),
        (card.row, card.col, card.rows, card.cols),
        "the card stays"
    );
    // Beside the card, not over it.
    assert!(
        rect.1 >= card.col + card.cols || rect.1 + rect.3 <= card.col,
        "{rect:?} {card:?}"
    );
    assert_eq!(
        popout_text(&s).as_deref(),
        Some("Add file contents to the index")
    );
    // The key was not sent to zsh: the line is still `git `.
    assert_eq!(prompt_line(&s).0, "% git");

    step(&s, "Down", DOWN);
    assert_eq!(
        popout_text(&s).as_deref(),
        Some("Apply a patch to files and/or to the index")
    );
    assert!(card_of(&s).popout.is_some());

    step(&s, "Ctrl-K (closes)", CTRL_K);
    assert_eq!(card_of(&s).popout, None);
    assert!(card_of(&s).selected.is_some(), "the card stays open");
    let closed = rect;
    s.with_screen(|screen| {
        for r in closed.0..closed.0 + closed.2 {
            for c in closed.1..closed.1 + closed.3 {
                if c >= card.col + card.cols || c < card.col {
                    let cell = screen.cell(r, c).expect("a cell");
                    assert!(!cell.has_contents(), "popout text left at ({r}, {c})");
                }
            }
        }
    });

    step(&s, "Ctrl-K (reopens)", CTRL_K);
    let rect = card_of(&s).popout.expect("the popout reopens");
    step(&s, "Esc", ESC);
    assert!(s.card().is_none(), "Esc closes the card too");
    s.with_screen(|screen| {
        assert_eq!(harness::popup_cells(screen, &harness::colors()), 0);
    });
    assert_blank(&s, rect, "after Esc, the popout's rect");
    assert_blank(
        &s,
        (card.row, card.col, card.rows, card.cols),
        "after Esc, the card's rect",
    );
}

/// Without `description_popout_key`, Ctrl+K reaches zsh and kills the line.
#[test]
fn ctrl_k_reaches_zsh_without_the_setting() {
    let (s, _tree) = start("", 24, 80);
    step(&s, "typed", b"zzz yyy");
    step(&s, "Ctrl-A", b"\x01");
    assert_eq!(prompt_line(&s).0, "% zzz yyy");
    step(&s, "Ctrl-K", CTRL_K);
    assert_eq!(prompt_line(&s).0, "%", "kill-line ran");
    // Also with a popup open.
    step(&s, "git ", b"git ");
    step(&s, "Down", DOWN);
    step(&s, "Ctrl-A", b"\x01");
    step(&s, "Ctrl-K", CTRL_K);
    assert_eq!(prompt_line(&s).0, "%");
    assert_eq!(s.card().and_then(|card| card.popout), None);
}

/// Without `COLORTERM=truecolor` fac paints in 256 colours: the card, the
/// bottom line and the terminal's background are three different indices,
/// and no true colour sequence is written. With it, the same popup is
/// painted in true colour.
#[test]
fn two_hundred_fifty_six_colours() {
    let background = SpawnOpts::default().background;
    let (s, _tree) = start("", 24, 80);
    let painted = paint_git(&s);
    let terminal = vt100::Color::Idx(fac::popup::theme::nearest_256(background));
    for color in [painted.card, painted.band, terminal] {
        assert!(matches!(color, vt100::Color::Idx(_)), "{color:?}");
    }
    assert_ne!(painted.card, painted.band);
    assert_ne!(painted.card, terminal);
    assert_ne!(painted.band, terminal);
    let raw = s.raw_output();
    assert!(
        !raw.windows(5).any(|window| window == b"48;2;"),
        "a true colour background was written"
    );
    drop(s);

    let env = vec![("COLORTERM".to_owned(), "truecolor".to_owned())];
    let (s, _tree) = start_with_env("", 24, 80, env);
    let painted = paint_git(&s);
    assert!(matches!(painted.card, vt100::Color::Rgb(..)), "{painted:?}");
    assert!(
        matches!(painted.selected, vt100::Color::Rgb(..)),
        "{painted:?}"
    );
    assert!(matches!(painted.band, vt100::Color::Rgb(..)), "{painted:?}");
}

/// `cd ` in an empty directory offers only `../`, and Esc closes it.
#[test]
fn cd_in_an_empty_directory_offers_parent_and_esc_closes() {
    let (s, _tree) = start("/bin/mkdir -p empty-dir && cd empty-dir", 24, 80);
    step(&s, "cd ", b"cd ");
    assert_eq!(labels_now(&s), ["../"]);
    assert_eq!(bottom_text(&s), "folder");
    let card = card_of(&s);
    step(&s, "Esc", ESC);
    assert!(s.card().is_none());
    assert_eq!(prompt_line(&s).0, "% cd");
    assert_blank(&s, (card.row, card.col, card.rows, card.cols), "after Esc");
    s.with_screen(|screen| {
        assert_eq!(harness::popup_cells(screen, &harness::colors()), 0);
    });
}

/// vim runs on the alternate screen: fac draws no popup while it is up (even
/// for text that would open one), and the popup works again after `:q`.
#[test]
fn vim_has_no_popup_and_the_popup_returns_after_quitting() {
    let (s, _tree) = start("", 24, 80);
    s.send(b"/usr/bin/vim -u NONE -n -N\r");
    wait_log(&s, "alt_screen=on");
    s.wait_quiet(QUIET, harness::TIMEOUT);
    // Typed inside vim (insert mode): text that opens a popup at a prompt.
    for chunk in [&b"i"[..], b"git ", b"ls -"] {
        let since = Instant::now();
        s.send(chunk);
        s.wait_quiet(QUIET, harness::TIMEOUT);
        let _ = since;
    }
    s.wait_text("ls -", harness::TIMEOUT);
    s.wait_quiet(QUIET, harness::TIMEOUT);
    assert!(s.card().is_none(), "{:?}", s.card());
    s.with_screen(|screen| {
        assert_eq!(harness::popup_cells(screen, &harness::colors()), 0);
    });
    s.send(b"\x1b:q!\r");
    wait_log(&s, "alt_screen=off");
    wait_prompt(&s);
    s.wait_quiet(QUIET, harness::TIMEOUT);
    s.send(b"git ");
    s.wait_card(
        "the popup again",
        |card, _| card.is_some_and(|card| card.list_rows > 0),
        harness::TIMEOUT,
    );
    step(&s, "settled", b"");
    assert!(labels_now(&s).iter().any(|label| label == "add"));
}

/// A two-line prompt (`tests/fixtures/prompts/two-line.zsh`): the card
/// opens on the row under the line typed on, under the word.
#[test]
fn prompt_two_lines() {
    prompt_scenario(
        "prompt_two_lines",
        "two-line",
        (1, 2),
        steps(vec![
            typed("ls -"),
            one("Down", DOWN),
            one("Esc", ESC),
            one("Ctrl-U", CTRL_U),
            one("Enter (a new prompt)", ENTER),
        ]),
    );
}

/// A right prompt on the line typed on: the card leaves it alone.
#[test]
fn prompt_right() {
    prompt_scenario(
        "prompt_right",
        "rprompt",
        (0, 2),
        steps(vec![
            typed("ls -"),
            one("Down", DOWN),
            one("Esc", ESC),
            one("Ctrl-U", CTRL_U),
            one("Enter (a new prompt)", ENTER),
        ]),
    );
}

/// An early prompt printed before fac's hooks are in, then drawn again.
#[test]
fn prompt_instant() {
    prompt_scenario(
        "prompt_instant",
        "instant-prompt",
        (0, 2),
        steps(vec![typed("ls -"), one("Down", DOWN), one("Esc", ESC)]),
    );
}

/// A transient prompt: Enter draws the finished line's prompt smaller.
#[test]
fn prompt_transient() {
    prompt_scenario(
        "prompt_transient",
        "transient-prompt",
        (1, 2),
        steps(vec![
            typed("ls -"),
            one("Down", DOWN),
            one("Esc", ESC),
            one("Ctrl-U", CTRL_U),
            one("echo hi", b"echo hi"),
            one("Enter (runs)", ENTER),
            typed("ls -"),
            one("Esc", ESC),
        ]),
    );
}

/// The prompt drawn again by `zle reset-prompt` from a `zle -F` handler
/// (up, the prompt, ED) while the popup is open, with no key: the card is
/// drawn again under the line, in the same frame as the shell's redraw.
#[test]
fn prompt_async_reset() {
    prompt_scenario(
        "prompt_async_reset",
        "async-prompt",
        (1, 2),
        steps(vec![
            typed("ls -"),
            vec![fifo("async reset-prompt (no key)", "ed")],
            one("Down", DOWN),
            vec![fifo("async reset-prompt again (no key)", "ed")],
            one("Esc", ESC),
        ]),
    );
}

/// A line of the prompt reprinted by a `zle -F` handler (up, the line, back
/// down; no ED) while the popup is open, with no key.
#[test]
fn prompt_async_reprint() {
    prompt_scenario(
        "prompt_async_reprint",
        "async-prompt",
        (1, 2),
        steps(vec![
            typed("ls -"),
            vec![fifo("async reprint (no key)", "reprint")],
            one("Down", DOWN),
            one("Esc", ESC),
        ]),
    );
}

/// PROMPT_SP: after output without a newline, the prompt starts on the next
/// row, and the card under it.
#[test]
fn prompt_sp() {
    prompt_scenario(
        "prompt_sp",
        "prompt-sp",
        (0, 2),
        steps(vec![
            one("printf x, Enter", b"printf x\r"),
            typed("ls -"),
            one("Down", DOWN),
            one("Esc", ESC),
        ]),
    );
}

/// A prompt rebuilt by a precmd hook that keeps itself last: no 133;A/B
/// reach the terminal, and the popup still opens under the word.
#[test]
fn prompt_rebuilt_without_marks() {
    prompt_scenario(
        "prompt_rebuilt_without_marks",
        "rebuilt-prompt",
        (1, 2),
        steps(vec![
            typed("ls -"),
            one("Down", DOWN),
            one("Esc", ESC),
            one("Ctrl-U", CTRL_U),
            one("Enter (a new prompt)", ENTER),
            typed("ls -"),
        ]),
    );
}

/// A terminal's shell integration on top of fac's
/// (`tests/fixtures/prompts/terminal-integration.zsh`): its own marks around
/// the prompt next to fac's, a precmd and a `zle-line-init` of its own. The
/// scenario also runs under VS Code's profile.
#[test]
fn prompt_terminal_integration() {
    prompt_scenario(
        "prompt_terminal_integration",
        "terminal-integration",
        (0, 2),
        steps(vec![
            typed("git ch"),
            one("Down", DOWN),
            one("Esc", ESC),
            one("Ctrl-U", CTRL_U),
            one("Enter (a new prompt)", ENTER),
            typed("ls -"),
        ]),
    );
}
