//! Each popup cell is written once per frame: the frame byte budget, the
//! late echo, and the shell's own saved cursor.

mod harness;
mod suite;

use std::time::Instant;

use harness::frames;
use harness::{Colors, Profile, Session};
use suite::{settle, spawn_in_tree};

const DOWN: &[u8] = b"\x1b[B";
const ESC: &[u8] = b"\x1b";
const ENTER: &[u8] = b"\r";

/// fac in the fixture tree, `rows` x 80, ready at the top of the screen.
fn start(rows: u16, truecolor: bool) -> (Session, suite::Tree) {
    let (s, tree) = spawn_in_tree("", rows, 80, Vec::new(), |opts| {
        if truecolor {
            // Ghostty's replies (with synchronized output), in true colour
            // by its kind.
            opts.set_profile(Profile::Ghostty);
        }
    });
    harness::wait_prompt(&s);
    suite::ready(&s);
    (s, tree)
}

/// What fac wrote in one 2026 frame: its own bytes (the echo excluded),
/// all of the frame's, and whether it scrolls to make room.
struct Sizes {
    fac: usize,
    all: usize,
    scroll: bool,
}

/// The most of its own bytes fac may write in a typical keystroke's frame:
/// macOS PTYs hand a terminal about 1 KB per read, so a bigger frame shows
/// in pieces where there is no synchronized output.
const BUDGET: usize = 1024;

/// Send `bytes`, settle, and size each 2026 frame in what came.
fn frame_sizes(s: &Session, rows: u16, name: &str, bytes: &[u8]) -> Vec<Sizes> {
    let before = s.raw_output().len();
    let since = Instant::now();
    s.send(bytes);
    settle(s, name, since);
    let raw = s.raw_output();
    frames::sync_frames(&raw[before..])
        .into_iter()
        .map(|frame| {
            let (fac, all) = frames::sizes(frame, rows);
            Sizes {
                fac,
                all,
                scroll: frames::scrolls(frame, rows),
            }
        })
        .collect()
}

/// The frames of `git ch` (one key at a time), Down, a letter after it,
/// Esc, and the frames that scroll to make room as `git ch` is typed on the
/// last row, as (step, typical, frame sizes).
fn budget_steps(truecolor: bool) -> Vec<(String, bool, Vec<Sizes>)> {
    let mut steps = Vec::new();
    let (s, _tree) = start(24, truecolor);
    for ch in "git ch".chars() {
        let name = format!("{ch:?}");
        let sizes = frame_sizes(&s, 24, &name, ch.to_string().as_bytes());
        // A letter typed with the popup open: from `c` on (`git ` opens
        // it; `c` shrinks the card from git's whole list).
        steps.push((name, ch == 'h', sizes));
    }
    steps.push(("Down".to_owned(), true, frame_sizes(&s, 24, "Down", DOWN)));
    steps.push(("'e'".to_owned(), true, frame_sizes(&s, 24, "e", b"e")));
    steps.push(("Esc".to_owned(), true, frame_sizes(&s, 24, "Esc", ESC)));
    drop(s);

    let (s, _tree) = start(24, truecolor);
    for _ in 1..24 {
        s.send(ENTER);
    }
    s.wait_for(
        "the prompt on the last row",
        |screen| screen.cursor_position() == (23, 2),
        harness::TIMEOUT,
    );
    s.wait_quiet(suite::QUIET, harness::TIMEOUT);
    let mut sizes = Vec::new();
    for ch in "git ch".chars() {
        sizes.extend(frame_sizes(
            &s,
            24,
            "git ch at the bottom",
            ch.to_string().as_bytes(),
        ));
    }
    let scroll: Vec<Sizes> = sizes.into_iter().filter(|frame| frame.scroll).collect();
    assert!(!scroll.is_empty(), "no frame scrolls");
    steps.push((
        "frames that scroll (`git ch` on the last row)".to_owned(),
        false,
        scroll,
    ));
    steps
}

/// The frame sizes per step (fac's own bytes, the echo excluded, and the
/// whole frame; `scroll` marks the frame that scrolls to make room), in 256
/// colours and in true colour. Typical keystroke frames stay within
/// `BUDGET` in both.
#[test]
fn frame_byte_budget() {
    let mut table = String::from(
        "| step | 256 colours: fac / frame | true colour: fac / frame |\n|---|---|---|\n",
    );
    let ansi = budget_steps(false);
    let truecolor = budget_steps(true);
    let show = |sizes: &[Sizes]| {
        sizes
            .iter()
            .map(|frame| {
                let scroll = if frame.scroll { " (scroll)" } else { "" };
                format!("{} / {}{scroll}", frame.fac, frame.all)
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    for ((name, _, a), (_, _, t)) in ansi.iter().zip(&truecolor) {
        table.push_str(&format!("| {name} | {} | {} |\n", show(a), show(t)));
    }
    println!("{table}");
    for (depth, steps) in [("256 colours", &ansi), ("true colour", &truecolor)] {
        for (name, typical, sizes) in steps {
            if *typical {
                assert!(!sizes.is_empty(), "{name}: no frame\n{table}");
                for frame in sizes {
                    assert!(
                        frame.fac < BUDGET,
                        "{name}: over budget in {depth}\n{table}"
                    );
                }
            }
        }
    }
}

/// A zle hook that runs after fac's report on every redraw and holds the
/// echo back 200 ms, so the paint (at most 4 ms after the report) goes out
/// first and the echo comes after it: phase 1's late echo, every time.
/// Added at the second prompt, once fac's own hooks are in.
const SLOW_ECHO: &str = r#"
zmodload zsh/zselect
_slow_echo() { zselect -t 20 }
_slow_echo_init() {
  (( $+functions[_fac_init] )) && return
  precmd_functions=(${precmd_functions:#_slow_echo_init})
  autoload -Uz add-zle-hook-widget
  zle -N _slow_echo
  add-zle-hook-widget line-pre-redraw _slow_echo
}
precmd_functions+=(_slow_echo_init)
"#;

/// The cells painted with the popup's colours.
fn popup_cells(screen: &vt100::Screen, colors: &Colors) -> Vec<(u16, u16)> {
    let (rows, cols) = (screen.size().0.get(), screen.size().1.get());
    (0..rows)
        .flat_map(|row| (0..cols).map(move |col| (row, col)))
        .filter(|&(row, col)| {
            screen
                .cell(row, col)
                .is_some_and(|cell| colors.is_popup(cell))
        })
        .collect()
}

/// Replay `raw` up to `from` into a fresh terminal, then the rest a byte at
/// a time: a cell that is the popup's before and after never shows
/// anything else in between (a blank flash). Fails with the first such
/// cell and how far into the output it was.
fn no_flash(raw: &[u8], from: usize, rows: u16, cols: u16, colors: &Colors) -> Result<(), String> {
    let size = |n| std::num::NonZeroU16::new(n).expect("not zero");
    let mut whole = vt100::Parser::new(size(rows), size(cols), 0);
    whole.process(raw);
    let mut terminal = vt100::Parser::new(size(rows), size(cols), 0);
    terminal.process(&raw[..from]);
    let before = popup_cells(terminal.screen(), colors);
    let after = popup_cells(whole.screen(), colors);
    let kept: Vec<(u16, u16)> = before
        .into_iter()
        .filter(|cell| after.contains(cell))
        .collect();
    if kept.is_empty() {
        return Err("no popup before and after".to_owned());
    }
    for at in from..raw.len() {
        terminal.process(&raw[at..=at]);
        if let Some(&(row, col)) = kept.iter().find(|&&(row, col)| {
            !terminal
                .screen()
                .cell(row, col)
                .is_some_and(|cell| colors.is_popup(cell))
        }) {
            return Err(format!(
                "({row}, {col}) left the popup {} bytes in: {:?}",
                at + 1 - from,
                String::from_utf8_lossy(&raw[from..=at])
            ));
        }
    }
    Ok(())
}

/// An echo that comes after its paint (forced here with a 30 ms delay in
/// the shell) does not blank the card: under synchronized output and
/// without it, no popup cell shows anything else on its way to the next
/// paint.
#[test]
fn a_late_echo_does_not_blank_the_card() {
    for profile in [None, Some(Profile::Apple), Some(Profile::Vscode104)] {
        let (s, _tree) = spawn_in_tree(SLOW_ECHO, 24, 80, Vec::new(), |opts| {
            if let Some(profile) = profile {
                opts.set_profile(profile);
            }
        });
        harness::wait_prompt(&s);
        suite::ready(&s);
        let colors = profile.map_or_else(harness::colors, harness::profile_colors);
        // Each echo takes 200 ms, longer than `settle`'s quiet: wait for
        // what each step shows.
        let since = Instant::now();
        s.send(b"ls -");
        s.wait_card(
            "the popup for `ls -`",
            |card, screen| card.is_some() && suite::row_text(screen, 0).trim_end() == "% ls -",
            harness::TIMEOUT,
        );
        settle(&s, "ls -", since);
        let late = s.log_text().matches("popup late echo").count();
        let draws = s.log_text().matches("popup drawn").count();
        let key = s.raw_output().len();
        let since = Instant::now();
        s.send(b"l");
        // The paint for the report goes out before the echo (its popup
        // placed for the cursor before the echo): the late echo and the next
        // paint start after it.
        let deadline = Instant::now() + harness::TIMEOUT;
        while s.log_text().matches("popup drawn").count() == draws
            || frames::segments(&s.raw_output()[key..], 24).is_empty()
        {
            assert!(Instant::now() < deadline, "{profile:?}: no paint");
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        s.wait_quiet(std::time::Duration::from_millis(40), harness::TIMEOUT);
        let echo = s.raw_output().len();
        s.wait_card(
            "the popup for `ls -l`",
            |card, screen| card.is_some() && suite::row_text(screen, 0).trim_end() == "% ls -l",
            harness::TIMEOUT,
        );
        settle(&s, "l", since);
        let log = s.log_text();
        assert!(
            log.matches("popup late echo").count() > late,
            "{profile:?}: the echo was not late\n{log}"
        );
        let raw = s.raw_output();
        assert!(
            frames::find(&raw[echo..], b"l").is_some(),
            "{profile:?}: the echo came before the paint"
        );
        if let Err(err) = no_flash(&raw, echo, 24, 80, &colors) {
            panic!("{profile:?}: {err}");
        }
        if let Err(err) = suite::frame_errors(&s, "l", key) {
            panic!("{profile:?}: {err}");
        }
    }
}

/// A prompt that saves the cursor (DECSC) where it starts.
const SAVING_PROMPT: &str = "PS1=$'%{\\e7%}%% '";

/// The shell saved its cursor and has not restored it: fac's frames never
/// save their own over it (they restore by CUP), so the shell's DECRC finds
/// the cursor it saved.
#[test]
fn a_cursor_the_shell_saved_stays_its_own() {
    let (s, _tree) = spawn_in_tree(SAVING_PROMPT, 24, 80, Vec::new(), |_| {});
    harness::wait_prompt(&s);
    suite::ready(&s);
    let key = s.raw_output().len();
    let since = Instant::now();
    s.send(b"ls -");
    settle(&s, "ls -", since);
    assert!(s.card().is_some(), "no popup\n{}", s.screen_text());
    assert!(s.log_text().contains("shell cursor saved=true"));
    let raw = s.raw_output();
    let typed = &raw[key..];
    assert!(
        frames::find(typed, b"\x1b7").is_none() && frames::find(typed, b"\x1b8").is_none(),
        "{:?}",
        String::from_utf8_lossy(typed)
    );
    if let Err(err) = suite::frame_errors(&s, "ls -", key) {
        panic!("{err}");
    }
    // The shell's DECRC goes back to where its prompt saved the cursor,
    // and two newlines keep the next prompt off that row.
    let since = Instant::now();
    s.send(b"\x15printf '\\e8X\\n\\n'");
    settle(&s, "printf", since);
    let row = s.with_screen(|screen| screen.cursor_position().0);
    let since = Instant::now();
    s.send(ENTER);
    settle(&s, "Enter", since);
    let line = s.with_screen(|screen| suite::row_text(screen, row));
    assert!(line.starts_with('X'), "{line:?}\n{}", s.screen_text());
}
