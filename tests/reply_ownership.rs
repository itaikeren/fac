//! Every terminal reply goes to whoever asked for it: the shell's queries,
//! written while fac's startup probe is still out or later on the alternate
//! screen, get their replies byte for byte, and fac's own still set its caps.

mod harness;

use std::time::Duration;

use harness::{Profile, RED, Session, SpawnOpts, TIMEOUT, has_line, wait_log, wait_prompt};

/// The fake terminal answers nothing until the shell has asked DA1 too, or
/// this long: past `caps::HOLD` (250 ms), so the shell's output is released
/// with its queries while fac's probe is still out, and inside
/// `caps::CONSUME` (1 s), so fac still takes its own replies.
const SLOW_START: Duration = Duration::from_millis(800);

/// `OSC 11 ; ?`, `OSC 10 ; ?` and DA1 from `.zshrc`, as a colorsaurus-style
/// tool asks; the replies are read raw until DA1's, with a timeout, and saved
/// byte for byte in `$HOME/replies`.
const COLORSAURUS: &str = r#"
() {
  local saved=$(stty -g) reply= c
  stty raw -echo
  print -rn -- $'\e]11;?\e\\\e]10;?\e\\\e[c'
  while read -r -t 5 -k 1 -u 0 c; do
    reply+=$c
    [[ $reply =~ $'\e\\[\\?[0-9;]*c$' ]] && break
  done
  stty $saved
  print -rn -- $reply > $HOME/replies
}
"#;

/// An nvim-style burst on the alternate screen: DA1, DECRQM 2026, OSC 11,
/// OSC 4;1 and CPR (at the home position), read raw until CPR's reply.
const NVIMISH: &str = r#"
nvimish() {
  local saved=$(stty -g) reply= c
  stty raw -echo
  print -rn -- $'\e[?1049h\e[H\e[c\e[?2026$p\e]11;?\e\\\e]4;1;?\e\\\e[6n'
  while read -r -t 5 -k 1 -u 0 c; do
    reply+=$c
    [[ $reply == *R ]] && break
  done
  print -rn -- $'\e[?1049l'
  stty $saved
  print -rn -- $reply > $HOME/replies
  print nvimish done
}
"#;

/// `rgb:rrrr/gggg/bbbb`, as the fake terminal writes it.
fn rgb((r, g, b): (u8, u8, u8)) -> String {
    format!("rgb:{r:02x}{r:02x}/{g:02x}{g:02x}/{b:02x}{b:02x}")
}

/// The OSC reply for an ST-terminated query to the default profile
/// (Ghostty mirrors the terminator).
fn osc(body: &str) -> Vec<u8> {
    format!("\x1b]{body}\x1b\\").into_bytes()
}

fn replies(s: &Session) -> Vec<u8> {
    std::fs::read(s.home().join("replies")).unwrap_or_default()
}

/// The `report seq=` lines whose buffer is not empty.
fn typed_reports(s: &Session) -> Vec<String> {
    s.reports()
        .into_iter()
        .filter(|line| !line.contains(" buffer=\"\" "))
        .collect()
}

/// fac's last `caps sync=` line, from `caps` on.
fn last_caps(log: &str) -> String {
    log.lines()
        .rev()
        .find_map(|line| line.find("caps sync=").map(|at| line[at..].trim_end()))
        .unwrap_or_default()
        .to_owned()
}

/// The attempts `a_colorsaurus_query_at_startup_gets_its_replies` gets at
/// its timing: zsh must ask within `SLOW_START` of fac's probe, which a cold
/// or loaded machine sometimes misses (zsh took 1.9 s once).
const ATTEMPTS: usize = 3;

#[test]
fn a_colorsaurus_query_at_startup_gets_its_replies() {
    for attempt in 1..=ATTEMPTS {
        if colorsaurus_at_startup() {
            return;
        }
        eprintln!("attempt {attempt}: zsh asked after fac's probe was answered");
    }
    panic!("zsh never asked while fac's probe was out, in {ATTEMPTS} attempts");
}

/// One run of the colorsaurus-style `.zshrc`, every reply checked; whether
/// zsh asked while fac's probe was still out, the case this is about.
fn colorsaurus_at_startup() -> bool {
    let profile = Profile::default();
    let s = Session::spawn(SpawnOpts {
        zshrc_extra: Some(COLORSAURUS.to_owned()),
        slow_start: Some(SLOW_START),
        ..SpawnOpts::default()
    });
    // Nothing reached the command line: the first prompt is bare.
    wait_prompt(&s);
    let expected = [
        osc(&format!("11;{}", rgb(profile.background()))),
        osc(&format!("10;{}", rgb(profile.foreground()))),
        profile.da1().to_vec(),
    ]
    .concat();
    let got = replies(&s);
    assert_eq!(
        got,
        expected,
        "{:?}\n--- log ---\n{}",
        String::from_utf8_lossy(&got),
        s.log_text()
    );
    // fac's own replies still set its caps.
    wait_log(&s, "caps ansi");
    let log = s.log_text();
    let (r, g, b) = profile.background();
    assert_eq!(
        last_caps(&log),
        format!("caps sync=true dark=true bg=Some(({r}, {g}, {b}))"),
        "{log}"
    );
    assert_eq!(typed_reports(&s), Vec::<String>::new(), "{log}");
    // The shell asked while fac's probe was out when its replies came while
    // fac's were still expected, and were told apart from them.
    let forwarded = |kind: &str| {
        log.lines().any(|line| {
            line.contains("reply forwarded to the shell") && line.contains(&format!("kind={kind}"))
        })
    };
    forwarded("Some(Background)") && forwarded("Some(DeviceAttrs)")
}

#[test]
fn an_nvim_style_burst_on_the_alt_screen_gets_every_reply_in_order() {
    let profile = Profile::default();
    let s = Session::spawn(SpawnOpts {
        zshrc_extra: Some(NVIMISH.to_owned()),
        ..SpawnOpts::default()
    });
    wait_prompt(&s);
    s.send_line("nvimish");
    s.wait_for(
        "nvimish done",
        |screen| has_line(screen, |line| line == "nvimish done"),
        TIMEOUT,
    );
    wait_prompt(&s);
    let expected = [
        profile.da1().to_vec(),
        b"\x1b[?2026;2$y".to_vec(),
        osc(&format!("11;{}", rgb(profile.background()))),
        osc(&format!("4;1;{}", rgb(RED))),
        b"\x1b[1;1R".to_vec(),
    ]
    .concat();
    let got = replies(&s);
    assert_eq!(
        got,
        expected,
        "{:?}\n--- log ---\n{}",
        String::from_utf8_lossy(&got),
        s.log_text()
    );
    // Nothing of them was typed at the prompt after it.
    s.send_line("echo after");
    s.wait_for(
        "after",
        |screen| has_line(screen, |line| line == "after"),
        TIMEOUT,
    );
    let screen = s.screen_text();
    for residue in ["rgb:", "$y", "^["] {
        assert!(!screen.contains(residue), "{residue:?} in\n{screen}");
    }
}
