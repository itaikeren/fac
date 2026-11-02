//! The characters `fac.zsh` reports as not drawn by ZLE as themselves (the
//! 7790 report's last field) against what the real zsh draws.
//!
//! The text of a line typed into a real zsh is read back from the screen
//! model, one item at a time, and compared with the text fac builds from
//! the shell's report (`text::drawn_text`).

mod harness;

use std::time::{Duration, Instant};

use fac::popup::text;
use harness::{Direct, Session, SpawnOpts, TIMEOUT, wait_prompt};

/// A zsh without fac, in a 400-column terminal so that a line never wraps.
fn shell(combining: bool) -> Session {
    let s = Session::spawn(SpawnOpts {
        cols: 400,
        direct: Some(Direct::default()),
        ..SpawnOpts::default()
    });
    wait_prompt(&s);
    if combining {
        s.send_line("setopt combiningchars");
        wait_prompt(&s);
    }
    s
}

/// The undrawn indices of the latest 7790 report whose buffer is `buffer`.
fn report_of(s: &Session, buffer: &str) -> Vec<usize> {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let raw = String::from_utf8_lossy(&s.raw_output()).into_owned();
        let found = raw.rsplit("\x1b]7790;").find_map(|message| {
            let message = message.split('\x07').next()?;
            let fields: Vec<&str> = message.split(';').collect();
            (fields.len() >= 7 && fields[6] == buffer).then(|| fields.get(7).copied())
        });
        if let Some(field) = found {
            return field.map_or_else(Vec::new, |field| {
                field
                    .split(',')
                    .flat_map(|run| match run.split_once('-') {
                        Some((from, to)) => from.parse().unwrap()..=to.parse().unwrap(),
                        None => run.parse().unwrap()..=run.parse().unwrap(),
                    })
                    .collect()
            });
        }
        assert!(Instant::now() < deadline, "no report for {buffer:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The line on the cursor's row, once the screen stops changing.
fn settled_line(s: &Session) -> String {
    let snap = || {
        s.with_screen(|screen| {
            let (row, _) = screen.cursor_position();
            screen
                .rows(0, screen.size().1.get())
                .nth(usize::from(row))
                .unwrap_or_default()
        })
    };
    std::thread::sleep(Duration::from_millis(200));
    let mut last = snap();
    let mut same = 0;
    while same < 5 {
        std::thread::sleep(Duration::from_millis(40));
        let now = snap();
        if now == last {
            same += 1;
        } else {
            same = 0;
            last = now;
        }
    }
    last
}

/// Type `~` and then each item followed by `~`, and return the items that
/// do not look the way fac predicts. The `~` after an item is a base for
/// `COMBINING_CHARS`, and a mark to split the drawn line on.
fn check(s: &Session, items: &[String], combining: bool) -> Vec<String> {
    let mut buffer = String::from("~");
    for item in items {
        buffer.push_str(item);
        buffer.push('~');
    }
    s.send(buffer.as_bytes());
    let undrawn = report_of(s, &buffer);
    let line = settled_line(s);
    s.send(b"\x03");
    wait_prompt(s);
    let drawn = line.strip_prefix("% ").unwrap_or(&line);
    let pieces: Vec<&str> = drawn.split('~').skip(1).collect();
    // What follows the last `~` is nothing.
    let pieces = &pieces[..pieces.len().saturating_sub(1)];
    let mut bad = Vec::new();
    let mut first = 1;
    for (item, piece) in items.iter().zip(pieces) {
        // The screen model prints no U+FFFD (vt100's perform.rs).
        let predicted = text::drawn_text(item, first, &undrawn).replace('\u{fffd}', "");
        // With COMBINING_CHARS zsh joins a base, VS16 and a further
        // zero-width character into one cell it counts as 1 wide, while
        // vt100 0.18.1 widens the base to 2 cells at the VS16
        // (screen.rs, `Perform::text`: `!cell_was_wide && cell_is_wide`,
        // then `wrap_widened_cell` moves the cursor on by one). zsh redraws
        // the line with backspaces by its own count, so the model is one
        // column off and a stale cell (a space) is left in front of the item.
        let widened_then_joined = combining
            && item
                .split_once('\u{fe0f}')
                .is_some_and(|(before, after)| !before.is_empty() && !after.is_empty());
        let spaced = widened_then_joined && format!(" {predicted}") == *piece;
        if &predicted != piece && !spaced {
            bad.push(format!(
                "{item:?} ({:x?}): drawn {piece:?}, predicted {predicted:?}",
                item.chars().map(u32::from).collect::<Vec<_>>()
            ));
        }
        first += item.chars().count() + 1;
    }
    if pieces.len() != items.len() {
        bad.push(format!(
            "{} pieces for {} items: {line:?}",
            pieces.len(),
            items.len()
        ));
    }
    bad
}

fn strings(chars: impl IntoIterator<Item = u32>) -> Vec<String> {
    chars
        .into_iter()
        .filter_map(char::from_u32)
        .map(String::from)
        .collect()
}

/// Section 1's rows, and the characters where libc and zsh disagree.
fn table() -> Vec<String> {
    let mut items: Vec<String> = [
        "a",
        "\u{65e5}",
        "\u{2764}",
        "\u{2764}\u{fe0f}",
        "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}",
        "\u{1f44d}\u{1f3fd}",
        "\u{1f1ee}\u{1f1f1}",
        "e\u{301}",
        "\u{301}",
        "a\u{301}\u{302}",
        "\u{65e5}\u{301}",
        "\u{2764}\u{fe0f}\u{301}",
        "1\u{fe0f}\u{20e3}",
        "\u{e01}\u{e31}",
        "\u{2764}\u{fe0e}",
        "\u{1100}\u{1161}",
        "x\u{200b}y",
        "x\u{200d}y",
        "\u{a0}\u{301}",
        "\u{3000}\u{301}",
        "\u{2028}\u{301}",
    ]
    .map(String::from)
    .to_vec();
    items.extend(strings([
        0x2060, 0xe0067, 0x10ffff, 0x1160, 0x1f1ee, 0xad, 0x85, 0xfeff, 0x378, 0xe000, 0xa0,
        0x3000, 0x2028, 0x200b, 0x200d, 0xfe0f, 0x1f3fd, 0xfffd,
    ]));
    items
}

/// Every code point of U+0080..U+10FFFF is one of these or in a range the
/// sample covers: all of U+0080..U+00FF, U+0300..U+036F, U+200B..U+206F and
/// U+FE00..U+FE0F, and 120 evenly spaced in every plane.
fn sample() -> Vec<String> {
    let mut points: Vec<u32> = (0x80..=0xff)
        .chain(0x300..=0x36f)
        .chain(0x200b..=0x206f)
        .chain(0xfe00..=0xfe0f)
        .collect();
    for plane in 0..=16u32 {
        let (start, end) = (plane << 16, (plane << 16) | 0xffff);
        for step in 0..120u32 {
            let point = start + step * (end - start) / 119;
            points.push(if plane == 0 && point < 0x80 {
                point + 0x80
            } else {
                point
            });
        }
    }
    // A fixed-seed spread over everything else.
    let mut state = 0x2545_f491_4f6c_dd1du64;
    for _ in 0..1000 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        points.push(0x80 + u32::try_from(state % (0x11_0000u64 - 0x80)).unwrap());
    }
    points.sort_unstable();
    points.dedup();
    // Surrogates are not characters; `strings` drops them.
    strings(points)
}

fn sweep(combining: bool) {
    let s = shell(combining);
    let mut items = table();
    items.extend(sample());
    println!("checking {} items, combiningchars={combining}", items.len());
    let bad: Vec<String> = items
        .chunks(20)
        .flat_map(|chunk| check(&s, chunk, combining))
        .collect();
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

#[test]
fn the_report_matches_what_zle_draws() {
    sweep(false);
}

#[test]
fn the_report_matches_what_zle_draws_with_combining_chars() {
    sweep(true);
}

/// `_fac_undrawn` and `_fac_run`, cut out of `fac.zsh`, with its cache
/// variables.
fn undrawn_functions() -> String {
    let source = include_str!("../shell/zsh/fac.zsh");
    let from = source.find("typeset -g _fac_u_key").expect("the cache");
    let to = source[from..].find("# 7790:").expect("the report") + from;
    format!("typeset -g _fac_out\n{}", &source[from..to])
}

/// The incremental scan gives what a full scan gives after every step of
/// edits: typing, backspacing, the cursor moving left and right (LBUFFER
/// shrinks and grows) and a paste.
#[test]
fn the_incremental_scan_equals_a_full_scan() {
    let script = format!(
        r#"{functions}
LC_ALL=en_US.UTF-8
RANDOM=7
lines=(
  $'git commit -m "שלום עולם" --all'
  $'café éé á̂ 日́ x'
  $'\U0001f468‍\U0001f469‍\U0001f467 ok ❤️́ 1️⃣'
  $'א́‍️ ́   ͸ \u0085 ­ ﻿'
  $'a\x01b́\x01אב\t́ x​ y'
  $'אבגדהו ז'
  'plain ascii only'
)
integer bad=0 steps=0 n k j
typeset -a cached
# What a full scan says, leaving the cache as it was.
full() {{
  local sk=$_fac_u_key sl=$_fac_u_len sj=$_fac_u_join so=$_fac_u_out
  local -a si=("${{_fac_u_idx[@]}}")
  _fac_u_key= _fac_u_len=0 _fac_u_out= _fac_u_join=0 _fac_u_idx=()
  _fac_undrawn "$@"
  _fac_u_key=$sk _fac_u_len=$sl _fac_u_join=$sj _fac_u_out=$so _fac_u_idx=("${{si[@]}}")
}}
step() {{ # line, length, comb
  local text=${{1[1,$2]}} want
  _fac_undrawn "$text" $3
  full "$text" $3
  want=$_fac_out
  _fac_undrawn "$text" $3   # the cached line again
  if [[ $_fac_out != $want ]]; then
    print -r -- "MISMATCH comb=$3 len=$2: ${{(q+)text}}: $_fac_out != $want"
    (( bad++ ))
  fi
  (( steps++ ))
}}
for comb in 0 1; do
  for line in $lines; do
    n=$#line
    _fac_u_key= _fac_u_len=0 _fac_u_out= _fac_u_join=0 _fac_u_idx=()
    # Typing, then backspacing.
    for (( k = 1; k <= n; k++ )); do step $line $k $comb; done
    for (( k = n; k >= 0; k-- )); do step $line $k $comb; done
    # Typing, the cursor moving left a few and right again, a paste.
    for (( k = 1; k <= n; k++ )); do
      step $line $k $comb
      (( k % 3 == 0 )) && for (( j = k - 1; j >= k - 2 && j >= 0; j-- )); do step $line $j $comb; done
      (( k % 3 == 0 )) && for (( j = k - 1; j <= k; j++ )); do step $line $j $comb; done
    done
    # Jumps to random lengths: pastes, deletes of words, Home and End.
    for (( k = 0; k < 150; k++ )); do step $line $(( RANDOM % (n + 1) )) $comb; done
  done
done
print "DONE steps=$steps bad=$bad"
"#,
        functions = undrawn_functions()
    );
    let dir = std::env::temp_dir().join(format!("fac-undrawn-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("scan.zsh");
    std::fs::write(&path, script).expect("script");
    let out = std::process::Command::new("/bin/zsh")
        .args(["-f"])
        .arg(&path)
        .env("LANG", "en_US.UTF-8")
        .output()
        .expect("zsh");
    let _ = std::fs::remove_dir_all(&dir);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("bad=0") && out.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("DONE steps="), "{stdout}");
}
