//! End-to-end tests of the screen model, the startup probe, CPR ownership
//! and the output scanner.

mod harness;

use std::fmt::Write as _;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use harness::{Session, SpawnOpts, TIMEOUT, assert_active, has_line, wait_log, wait_prompt};
use nix::sys::signal::{Signal, kill};

fn start(opts: SpawnOpts) -> Session {
    let s = Session::spawn(opts);
    wait_prompt(&s);
    assert_active(&s);
    s
}

#[test]
fn alt_screen_on_and_off() {
    let s = start(SpawnOpts::default());
    s.send_line(r"printf '\e[?1049h'; sleep 0.2; printf '\e[?1049l'");
    wait_log(&s, "alt_screen=off");
    let log = s.log_text();
    let on = log.find("alt_screen=on").expect("alt_screen=on logged");
    let off = log.find("alt_screen=off").expect("alt_screen=off logged");
    assert!(on < off, "{log}");
}

#[test]
fn shell_cpr_is_not_swallowed() {
    let s = start(SpawnOpts::default());
    // Echo goes off before the query is out: a reply that came before
    // `read -s` turned it off would be echoed by the tty.
    s.send_line(
        r#"stty -echo; printf '\e[6n'; IFS= read -rs -d R pos; stty echo; echo "got${pos#*\[}""#,
    );
    let is_got = |line: &str| {
        line.strip_prefix("got").is_some_and(|rest| {
            rest.split_once(';').is_some_and(|(row, col)| {
                [row, col]
                    .iter()
                    .all(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            })
        })
    };
    s.wait_for("got<row>;<col>", |screen| has_line(screen, is_got), TIMEOUT);
    s.wait_quiet(Duration::from_millis(100), TIMEOUT);
    // One entry per screen row: the command line is longer than one.
    let screen_lines: Vec<String> = s.with_screen(|screen| {
        screen
            .rows(0, screen.size().1.get())
            .map(|line| line.trim_end().to_owned())
            .collect()
    });
    let (row, line) = screen_lines
        .iter()
        .enumerate()
        .find(|(_, line)| is_got(line))
        .expect("got line");
    // Nothing is echoed, so `got` is printed where the cursor was
    // when the shell asked: column 1 of that row.
    assert_eq!(line, &format!("got{};1", row + 1), "{screen_lines:#?}");
    wait_log(&s, "shell cursor query");
}

#[test]
fn prompt_reconcile_after_varied_output() {
    let s = start(SpawnOpts::default());
    for command in [
        r"printf '日本語\tx\n'",
        r"printf '%0200d\n' 0",
        r"printf 'a\e[5A\e[3Bb\n'",
        r"printf '\t\t\tz\r\n'",
        "clear",
        r"printf 'after clear 日本\n'",
    ] {
        s.send_line(command);
        wait_prompt(&s);
    }
    s.send_line(r"printf '\e]133;A\a'");
    wait_prompt(&s);
    wait_log(&s, "cpr ok");
    let log = s.log_text();
    assert!(!log.contains("cpr mismatch"), "{log}");
}

#[test]
fn mid_screen_start_seeds_the_cursor_exactly() {
    let s = start(SpawnOpts {
        prefill_lines: 10,
        ..SpawnOpts::default()
    });
    wait_log(&s, "cpr seed model=(0, 0) real=(10, 0)");
    s.send_line(r"printf '\e]133;A\a'");
    wait_prompt(&s);
    wait_log(&s, "cpr ok");
    let log = s.log_text();
    assert!(!log.contains("cpr mismatch"), "{log}");
}

#[test]
fn caps_dark_with_sync() {
    let s = start(SpawnOpts::default());
    wait_log(&s, "caps sync=true dark=true bg=Some((30, 30, 30))");
}

#[test]
fn caps_light_without_sync() {
    let s = start(SpawnOpts {
        sync: false,
        background: (255, 255, 255),
        ..SpawnOpts::default()
    });
    wait_log(&s, "caps sync=false dark=false bg=Some((255, 255, 255))");
}

/// When the first log line containing `needle` was written: its
/// timestamp's time of day (`2026-10-25T11:08:21.402408Z`).
fn log_time(log: &str, needle: &str) -> Option<Duration> {
    let line = log.lines().find(|line| line.contains(needle))?;
    let time = line.split_whitespace().next()?.split('T').nth(1)?;
    let time = time.strip_suffix('Z')?;
    let mut parts = time.split(':');
    let hours: u64 = parts.next()?.parse().ok()?;
    let minutes: u64 = parts.next()?.parse().ok()?;
    let seconds: f64 = parts.next()?.parse().ok()?;
    Some(Duration::from_secs(hours * 3600 + minutes * 60) + Duration::from_secs_f64(seconds))
}

#[test]
fn caps_without_answers_still_starts() {
    let s = Session::spawn(SpawnOpts {
        answer_queries: false,
        ..SpawnOpts::default()
    });
    wait_prompt(&s);
    wait_log(&s, "prompt mark A");
    // fac's own window, from its log: the first execs from a fresh test
    // process are slow on this machine, before fac runs at all.
    let log = s.log_text();
    let started = format!("fac {} started", env!("CARGO_PKG_VERSION"));
    let started = log_time(&log, &started).expect("the start logged");
    let prompt = log_time(&log, "prompt mark A").expect("the prompt logged");
    // Past midnight the time of day starts again.
    let window = prompt
        .checked_sub(started)
        .unwrap_or_else(|| prompt + Duration::from_secs(24 * 3600) - started);
    assert!(
        window < Duration::from_secs(1),
        "first prompt {window:?} after fac started\n{log}"
    );
    assert_active(&s);
    s.send_line("echo hi");
    s.wait_for(
        "hi",
        |screen| has_line(screen, |line| line == "hi"),
        TIMEOUT,
    );
    wait_log(&s, "caps sync=false dark=true bg=None");
}

#[test]
fn caps_ansi_from_osc4() {
    let s = start(SpawnOpts::default());
    wait_log(
        &s,
        "caps ansi green=#98c379 yellow=#e5c07b blue=#61afef magenta=#c678dd cyan=#56b6c2",
    );
    wait_log(&s, "palette default built");
    // No byte of the replies reached the shell.
    let screen = s.screen_text();
    assert!(!screen.contains("rgb:"), "{screen}");
}

#[test]
fn caps_ansi_keeps_each_index() {
    let s = start(SpawnOpts {
        palette: Some([(1, 2, 3), (4, 5, 6), (7, 8, 9), (10, 11, 12), (13, 14, 15)]),
        ..SpawnOpts::default()
    });
    wait_log(
        &s,
        "caps ansi green=#010203 yellow=#040506 blue=#070809 magenta=#0a0b0c cyan=#0d0e0f",
    );
}

#[test]
fn bad_theme_name_warns_once_and_uses_the_default() {
    let config = std::env::temp_dir().join(format!("fac-test-theme-{}.toml", std::process::id()));
    std::fs::write(&config, "theme = \"../x\"\n").expect("write config");
    let s = start(SpawnOpts {
        extra_env: vec![("FAC_CONFIG".to_owned(), config.display().to_string())],
        ..SpawnOpts::default()
    });
    wait_log(&s, "palette default built");
    let _ = std::fs::remove_file(&config);
    let log = s.log_text();
    let warning = "theme \"../x\": not a valid name";
    assert_eq!(log.matches(warning).count(), 1, "{log}");
    assert!(log.contains("WARN"), "{log}");
}

#[test]
fn caps_without_osc4_answers() {
    let s = start(SpawnOpts {
        palette: None,
        ..SpawnOpts::default()
    });
    wait_log(&s, "caps sync=true dark=true bg=Some((30, 30, 30))");
    wait_log(
        &s,
        "caps ansi green=none yellow=none blue=none magenta=none cyan=none",
    );
    wait_log(&s, "palette default built");
}

#[test]
fn private_osc_is_stripped() {
    let s = start(SpawnOpts::default());
    s.send_line(r"printf 'a\e]7790;1;5;x\ab\n'");
    s.wait_for(
        "ab",
        |screen| has_line(screen, |line| line == "ab"),
        TIMEOUT,
    );
    wait_log(&s, "private osc code=7790");
}

/// A file of numbered lines, every third one coloured, of at least `size`
/// bytes, in the session's home.
fn numbered_lines(s: &Session, size: usize) -> PathBuf {
    let path = s.home().join("lines.txt");
    let mut text = String::new();
    let mut n = 0;
    while text.len() < size {
        if n % 3 == 0 {
            let _ = writeln!(text, "{n:07} \x1b[3{}mcoloured\x1b[0m line 日本", n % 7 + 1);
        } else {
            let _ = writeln!(text, "{n:07} plain line");
        }
        n += 1;
    }
    std::fs::write(&path, text).expect("writing the lines");
    path
}

/// How many prompt CPRs fac has reconciled so far.
fn reconciles(s: &Session) -> usize {
    let log = s.log_text();
    log.matches("cpr ok").count() + log.matches("cpr mismatch").count()
}

/// Run `command` and wait for its prompt, with fac's CPR for that prompt
/// reconciled and the screen quiet.
fn run_to_prompt(s: &Session, command: &str) {
    let before = reconciles(s);
    s.send_line(command);
    let deadline = Instant::now() + TIMEOUT;
    while reconciles(s) <= before {
        assert!(
            Instant::now() < deadline,
            "no cpr reconcile after {command:?}\n--- screen ---\n{}\n--- log ---\n{}",
            s.screen_text(),
            s.log_text()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    wait_prompt(s);
    s.wait_quiet(Duration::from_millis(100), TIMEOUT);
}

/// fac's screen model as its SIGUSR1 dump logs it: `model dump cursor=..
/// rows=[..]`.
fn model_dump(s: &Session) -> String {
    let needle = "model dump ";
    let before = s.log_text().matches(needle).count();
    kill(s.pid(), Signal::SIGUSR1).expect("SIGUSR1 to fac");
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let log = s.log_text();
        if log.matches(needle).count() > before
            && let Some(line) = log.lines().rfind(|line| line.contains(needle))
            && let Some(at) = line.find(needle)
        {
            return line[at..].to_owned();
        }
        assert!(Instant::now() < deadline, "no model dump:\n{log}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The harness's screen in the format of `model_dump`.
fn screen_dump(s: &Session) -> String {
    s.with_screen(|screen| {
        let cols = screen.size().1.get();
        let (row, col) = screen.cursor_position();
        let rows: Vec<String> = screen
            .rows(0, cols)
            .map(|row| row.trim_end().to_owned())
            .collect();
        format!(
            "model dump cursor={:?} rows={rows:?}",
            (row, col.min(cols - 1))
        )
    })
}

/// fac's model agrees with the terminal, cursor and every row's text.
fn assert_model_matches(s: &Session) {
    let (model, screen) = (model_dump(s), screen_dump(s));
    assert_eq!(model, screen, "\n--- log ---\n{}", s.log_text());
}

/// No `cpr mismatch` after the first `skip` reconciles: the model's own
/// cursor was right at every prompt since.
fn assert_no_mismatch_after(s: &Session, skip: usize) {
    let log = s.log_text();
    let mismatches: Vec<&str> = log
        .lines()
        .filter(|line| line.contains("cpr ok") || line.contains("cpr mismatch"))
        .skip(skip)
        .filter(|line| line.contains("cpr mismatch"))
        .collect();
    assert!(mismatches.is_empty(), "{mismatches:#?}\n--- log ---\n{log}");
}

const BULK_SIZE: usize = 2 * 1024 * 1024;

#[test]
fn bulk_output_is_modelled_at_the_prompt() {
    let s = start(SpawnOpts::default());
    let path = numbered_lines(&s, BULK_SIZE);
    let skip = reconciles(&s);
    run_to_prompt(&s, &format!("cat '{}'", path.display()));
    wait_log(&s, "bulk output on");
    assert_model_matches(&s);
    assert_no_mismatch_after(&s, skip);
}

#[test]
fn bulk_output_then_a_popup_erase_restores_the_screen() {
    let s = start(SpawnOpts::default());
    wait_log(&s, "worker ready");
    let path = numbered_lines(&s, BULK_SIZE);
    // The prompt goes back up into the output, so the popup is drawn over
    // the output and its erase repaints it from the model.
    run_to_prompt(
        &s,
        &format!(r"cat '{}'; printf '\e[3;1H\e[K'", path.display()),
    );
    wait_log(&s, "bulk output on");
    let before = screen_dump(&s);
    assert!(before.contains("plain line"), "{before}");
    let colors = harness::colors();
    s.send(b"ls -");
    s.wait_card(
        "the popup",
        |card, screen| card.is_some() && harness::popup_cells(screen, &colors) > 0,
        TIMEOUT,
    );
    s.wait_quiet(Duration::from_millis(100), TIMEOUT);
    s.send(b"\x15");
    s.wait_card(
        "the popup gone",
        |card, screen| card.is_none() && harness::popup_cells(screen, &colors) == 0,
        TIMEOUT,
    );
    s.wait_quiet(Duration::from_millis(150), TIMEOUT);
    assert_eq!(screen_dump(&s), before, "\n--- log ---\n{}", s.log_text());
}

#[test]
fn bulk_output_then_a_full_screen_program() {
    let s = start(SpawnOpts::default());
    let path = numbered_lines(&s, BULK_SIZE);
    run_to_prompt(
        &s,
        &format!(
            r"cat '{}'; printf '\e[?1049h\e[2J\e[Hfull screen\e[10;10Hdrawn'; sleep 0.1; printf '\e[?1049l'",
            path.display()
        ),
    );
    wait_log(&s, "bulk output on");
    wait_log(&s, "alt_screen=off");
    assert_model_matches(&s);
}

#[test]
fn bulk_output_without_a_trailing_newline() {
    // zsh marks the missing newline and the prompt goes on the next row,
    // so the long line's last row stays on the screen.
    let s = start(SpawnOpts {
        zshrc_extra: Some("setopt prompt_sp".to_owned()),
        ..SpawnOpts::default()
    });
    let path = numbered_lines(&s, BULK_SIZE / 2);
    let skip = reconciles(&s);
    // Lines, then one long line: the tail has no newline left in it.
    run_to_prompt(
        &s,
        &format!("cat '{}'; printf '%01000003d' 7", path.display()),
    );
    wait_log(&s, "bulk output on");
    assert_model_matches(&s);
    // Only one long line: never skipped, since it has no newline.
    run_to_prompt(&s, "printf '%02000003d' 7");
    assert_model_matches(&s);
    assert_no_mismatch_after(&s, skip);
}

/// The screen's rows above `row`, and the cursor's row.
fn above_and_cursor(s: &Session, row: u16) -> (Vec<String>, String, (u16, u16)) {
    s.with_screen(|screen| {
        let cols = screen.size().1.get();
        let rows: Vec<String> = screen.rows(0, cols).collect();
        let cursor = screen.cursor_position();
        (
            rows[..usize::from(row)].to_vec(),
            rows[usize::from(cursor.0)].clone(),
            cursor,
        )
    })
}

/// At the bottom of a full screen, the popup scrolls the screen once, when
/// it opens for a word, making room for its list and every row its bottom
/// line may take. Down, Down and Up then leave the cursor's row and every
/// row above the popup as they were, and the card where it was.
#[test]
fn an_arrow_key_never_scrolls_the_screen() {
    let s = start(SpawnOpts {
        prefill_lines: 30,
        ..SpawnOpts::default()
    });
    wait_log(&s, "worker ready");
    let colors = harness::colors();
    s.send(b"ls -");
    s.wait_card(
        "a popup with options",
        |card, screen| {
            card.is_some_and(|card| harness::label(screen, card, 0, &colors).starts_with('-'))
        },
        TIMEOUT,
    );
    s.wait_quiet(Duration::from_millis(150), TIMEOUT);
    let card = s.card().expect("a popup");
    assert_eq!(card.last_row(), 23, "at the bottom: {card:?}");
    assert_eq!(card.bottom, card.d, "{card:?}");
    let before = above_and_cursor(&s, card.row);
    for (key, selected) in [
        (b"\x1b[B", Some(0)),
        (b"\x1b[B", Some(1)),
        (b"\x1b[A", Some(0)),
    ] {
        s.send(key);
        s.wait_card(
            &format!("selected {selected:?}"),
            |now, _| now.is_some_and(|now| now.selected == selected),
            TIMEOUT,
        );
        s.wait_quiet(Duration::from_millis(150), TIMEOUT);
        let now = s.card().expect("a popup");
        assert_eq!(
            (now.row, now.col, now.rows, now.cols),
            (card.row, card.col, card.rows, card.cols),
            "the card moved"
        );
        assert_eq!(
            above_and_cursor(&s, card.row),
            before,
            "after {key:?}\n{}",
            s.screen_text()
        );
    }
}

/// `cargo test --release --test screen -- --ignored throughput --nocapture`
///
/// The same 200 MB `cat` through fac and through `FAC_DISABLE=1` (a plain
/// zsh), the harness draining raw bytes; two rounds each, best of each kept.
#[test]
#[ignore = "throughput measurement"]
fn throughput() {
    const SIZE: usize = 200 * 1024 * 1024;
    let path = std::env::temp_dir().join(format!("fac-throughput-{}.txt", std::process::id()));
    let mut file = std::io::BufWriter::new(std::fs::File::create(&path).expect("create"));
    let mut written = 0;
    let mut n = 0u64;
    while written < SIZE {
        let line = match n % 4 {
            0 => format!("{n:08} the quick brown fox jumps over the lazy dog\n"),
            1 => format!("{n:08} \x1b[1;32mgreen\x1b[0m and \x1b[31mred\x1b[0m text\n"),
            2 => format!("{n:08} 日本語のテキスト, ünïcödé, emoji-free\n"),
            _ => format!("{n:08}\tcol\tcol\tcol\n"),
        };
        file.write_all(line.as_bytes()).expect("write");
        written += line.len();
        n += 1;
    }
    file.flush().expect("flush");
    drop(file);

    let plain = || SpawnOpts {
        extra_env: vec![("FAC_DISABLE".to_owned(), "1".to_owned())],
        ..SpawnOpts::default()
    };
    let mut fac = Vec::new();
    let mut zsh = Vec::new();
    for _ in 0..2 {
        fac.push(time_cat(&start(SpawnOpts::default()), &path, written));
        let s = Session::spawn(plain());
        wait_prompt(&s);
        zsh.push(time_cat(&s, &path, written));
    }
    let _ = std::fs::remove_file(&path);
    let best = |times: &[Duration]| times.iter().min().copied().unwrap_or_default();
    let (fac_best, zsh_best) = (best(&fac), best(&zsh));
    let mb = written as f64 / 1e6;
    println!("throughput: {mb:.1} MB, fac {fac:?}, plain {zsh:?}");
    println!(
        "throughput: fac {:.3} s ({:.1} MB/s), plain {:.3} s ({:.1} MB/s), ratio {:.2}",
        fac_best.as_secs_f64(),
        mb / fac_best.as_secs_f64(),
        zsh_best.as_secs_f64(),
        mb / zsh_best.as_secs_f64(),
        fac_best.as_secs_f64() / zsh_best.as_secs_f64()
    );
}

/// How long `cat` of `path` (`size` bytes) takes to reach the harness, which
/// drains raw bytes meanwhile.
fn time_cat(s: &Session, path: &Path, size: usize) -> Duration {
    s.set_raw_drain(true);
    let before = s.raw_count();
    let started = Instant::now();
    // The echoed command line holds `__E''ND__`; only the output has `__END__`.
    s.send_line(&format!(
        r#"cat '{}'; printf '%s\n' __E''ND__"#,
        path.display()
    ));
    let deadline = started + Duration::from_secs(300);
    loop {
        let done = s.raw_count() - before >= size
            && s.raw_tail()
                .windows(b"__END__".len())
                .any(|window| window == b"__END__");
        if done {
            break;
        }
        assert!(Instant::now() < deadline, "cat did not finish");
        std::thread::sleep(Duration::from_millis(1));
    }
    let elapsed = started.elapsed();
    s.set_raw_drain(false);
    elapsed
}

/// A typed word that holds a heart with VS16: the model (vt100) counts the
/// heart 2 cells, and the card's list starts under the word's first cell, not
/// one column right of it.
#[test]
fn the_card_sits_under_a_typed_word_that_holds_a_heart() {
    let s = start(SpawnOpts {
        zshrc_extra: Some(
            "mkdir -p ~/hearts && cd ~/hearts && : > $'heart-a\\xe2\\x9d\\xa4\\xef\\xb8\\x8fb'"
                .to_owned(),
        ),
        ..SpawnOpts::default()
    });
    wait_log(&s, "worker ready");
    let colors = harness::colors();
    s.send("ls heart-a\u{2764}\u{fe0f}".as_bytes());
    s.wait_card(
        "a popup with the file",
        |card, screen| {
            card.is_some_and(|card| harness::label(screen, card, 0, &colors).starts_with("heart"))
        },
        TIMEOUT,
    );
    s.wait_quiet(Duration::from_millis(150), TIMEOUT);
    let card = s.card().expect("a popup");
    // The column of the first cell holding an `h` in a row.
    let first_h = |row: u16| {
        s.with_screen(|screen| {
            let cols = screen.size().1.get();
            (0..cols).find(|&col| screen.cell(row, col).is_some_and(|c| c.contents() == "h"))
        })
    };
    // The popup opens on the row under the typed line.
    let word = first_h(card.row - 1).expect("the typed word");
    assert_eq!(
        first_h(card.row),
        Some(word),
        "{card:?}\n{}",
        s.screen_text()
    );
}

/// A shell that holds one file named `name` (a shell string), `ls <typed>`
/// typed: the card's list starts under the first cell of the typed word, which
/// starts with `first`. `zshrc` is added to the shell's rc file.
fn the_card_sits_under_the_word(zshrc: &str, name: &str, typed: &str, first: &str, label: &str) {
    let s = start(SpawnOpts {
        zshrc_extra: Some(format!(
            "{zshrc}\nmkdir -p ~/words && cd ~/words && : > {name}"
        )),
        ..SpawnOpts::default()
    });
    wait_log(&s, "worker ready");
    let colors = harness::colors();
    s.send(format!("ls {typed}").as_bytes());
    s.wait_card(
        "a popup with the file",
        |card, screen| {
            card.is_some_and(|card| harness::label(screen, card, 0, &colors).starts_with(label))
        },
        TIMEOUT,
    );
    s.wait_quiet(Duration::from_millis(150), TIMEOUT);
    let card = s.card().expect("a popup");
    // The column of the first cell holding `first` in a row.
    let first_cell = |row: u16| {
        s.with_screen(|screen| {
            let cols = screen.size().1.get();
            (0..cols).find(|&col| screen.cell(row, col).is_some_and(|c| c.contents() == first))
        })
    };
    // The popup opens on the row under the typed line.
    let word = first_cell(card.row - 1).expect("the typed word");
    assert_eq!(
        first_cell(card.row),
        Some(word),
        "{card:?}\n{}",
        s.screen_text()
    );
}

/// ZLE draws a ZWJ as `<200d>` (6 cells); the model counts the one character.
#[test]
fn the_card_sits_under_a_typed_word_that_holds_a_zwj() {
    the_card_sits_under_the_word("", r"$'zwj-a\xe2\x80\x8db'", "zwj-a\u{200d}", "z", "zwj");
}

/// ZLE draws `e` and U+0301 as `e<0301>` unless COMBINING_CHARS is set (macOS's
/// /etc/zshrc sets it), when it draws the one joined cell.
#[test]
fn the_card_sits_under_a_typed_word_that_holds_a_combining_mark() {
    for option in ["unsetopt combiningchars", "setopt combiningchars"] {
        the_card_sits_under_the_word(option, r"$'cafe\xcc\x81.txt'", "cafe\u{301}", "c", "caf");
    }
}

/// Report only (step 6): what happens to the popup's anchor column when the
/// shell echoes characters that terminals draw at different widths.
///
/// fac's model is vt100, which counts the heart with VS16 as 2 cells (it
/// widens on VS16, unlike unicode-width per character), a ZWJ family as the
/// sum of its parts (6), and a skin-tone pair as 4. Terminals draw (step 1b,
/// findings.md fact 8): the heart 1 in Terminal.app, VS Code and Zed and 2 in
/// iTerm2 and Ghostty; the family 8, 6, 6 and 2 in iTerm2 and Ghostty.
///
/// fac anchors the card at `model cursor column - text::typed_width(typed
/// word as ZLE draws it)` (typed_width counts the way the model does, so a
/// heart in the typed word is 2 in both; `text::drawn_text` makes the word the
/// text ZLE draws for it) and positions every card cell and every erase with
/// absolute columns from the model. One effect remains:
/// Characters echoed before the word: the model's columns differ from the
///    terminal's, and the card is drawn that many columns off from the word
///    until the next prompt's CPR resyncs the model. The shell's own cursor
///    is restored by DECRC, so typing is not affected, and the cells erased
///    are the cells drawn (both come from the model's columns), so nothing
///    is left stale beside the card; only its alignment and, near the right
///    edge, its row are off.
#[test]
fn a_shell_echo_the_terminal_draws_at_another_width_drifts_the_models_column() {
    use fac::popup::text;
    use fac::term::screen::ScreenModel;

    // (model cursor column, fac's anchor) after `% <line>`, `word` typed last.
    let after = |line: &str, word: &str| {
        let mut model = ScreenModel::new(24, 80);
        model.process(format!("% {line}").as_bytes());
        let (_, col) = model.cursor();
        (col, col - text::typed_width(word))
    };
    let heart = "\u{2764}\u{fe0f}";
    let family = "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}";

    // 1. The word is the heart: it starts at column 5 in the model and in
    // every terminal, and the card is anchored at 5 (step 6a).
    assert_eq!(after(&format!("ls {heart}"), heart), (7, 5));
    assert_eq!(after("ls ab", "ab"), (7, 5), "a plain word is exact");

    // 2. The word after the heart: the model has it at column 10.
    // iTerm2 and Ghostty agree; Terminal.app, VS Code and Zed draw the heart
    // 1 cell narrower, so the word starts at 9 and the card is 1 right of it.
    assert_eq!(after(&format!("echo {heart} ab"), "ab"), (12, 10));
    // After the family ("% echo " is 7 columns, the family 6, a space) the
    // model has the word at 14. VS Code and Zed agree; Terminal.app draws
    // the family 8 wide (word at 16: the card is 2 left of it); iTerm2 and
    // Ghostty draw it 2 wide (word at 10: the card is 4 right of it).
    assert_eq!(after(&format!("echo {family} ab"), "ab"), (16, 14));
}
