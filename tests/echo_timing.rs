//! Measures how the shell's echo of a keystroke arrives: fac logs, for every
//! paint that follows a report, the time from the report to the first echo
//! byte, the longest gap between two echo chunks and the time from the last
//! echo byte to the paint; an echo chunk that comes after its paint is
//! logged as a late echo. The fixture `.zshrc` runs with
//! zsh-syntax-highlighting when it is installed. This sets `PAINT_QUIET`
//! and asserts nothing, so it is ignored by default:
//!
//! `cargo test --test echo_timing -- --ignored --nocapture`

mod harness;

use std::path::Path;
use std::time::Duration;

use harness::{Session, SpawnOpts, assert_active, wait_log, wait_prompt};

const HIGHLIGHTING: &[&str] = &[
    "/opt/homebrew/share/zsh-syntax-highlighting/zsh-syntax-highlighting.zsh",
    "/usr/local/share/zsh-syntax-highlighting/zsh-syntax-highlighting.zsh",
];
const COMMANDS: &[&str] = &[
    "git status",
    "ls -la /tmp",
    "echo hello world",
    "cd /usr/local",
    "grep -r needle .",
    "cat README.md",
    "git checkout main",
    "printf '%s' \"quoted $HOME\"",
    "ssh localhost",
    "man zshall",
];
/// `FAC_ECHO_ROUNDS` overrides it.
const ROUNDS: usize = 3;
const KEY_GAP: Duration = Duration::from_millis(60);

fn value(line: &str, key: &str) -> Option<u64> {
    let rest = line.split(&format!("{key}=")).nth(1)?;
    rest.split(|ch: char| !ch.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}

/// The `q`th percentile of `sorted`.
fn percentile(sorted: &[u64], q: usize) -> u64 {
    sorted
        .get((sorted.len().saturating_sub(1)) * q / 100)
        .copied()
        .unwrap_or(0)
}

fn summary(name: &str, mut values: Vec<u64>) -> Vec<u64> {
    values.sort_unstable();
    println!(
        "{name}: n={} p50={} p90={} p95={} p99={} max={} (us)",
        values.len(),
        percentile(&values, 50),
        percentile(&values, 90),
        percentile(&values, 95),
        percentile(&values, 99),
        values.last().copied().unwrap_or(0)
    );
    values
}

#[test]
#[ignore = "a measurement; run with --ignored --nocapture"]
fn echo_gaps() {
    let highlighting = HIGHLIGHTING.iter().find(|path| Path::new(path).exists());
    println!("zsh-syntax-highlighting: {highlighting:?}");
    let extra = highlighting
        .map(|path| format!("source '{path}'"))
        .unwrap_or_default();
    let s = Session::spawn(SpawnOpts {
        zshrc_extra: Some(extra),
        ..SpawnOpts::default()
    });
    wait_prompt(&s);
    assert_active(&s);
    wait_log(&s, "worker ready");

    let rounds = std::env::var("FAC_ECHO_ROUNDS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(ROUNDS);
    let mut keys = 0;
    for _ in 0..rounds {
        for command in COMMANDS {
            for ch in command.chars() {
                s.send(ch.to_string().as_bytes());
                keys += 1;
                std::thread::sleep(KEY_GAP);
            }
            // Ctrl-U clears the line.
            s.send(b"\x15");
            std::thread::sleep(KEY_GAP * 3);
        }
    }
    std::thread::sleep(Duration::from_millis(300));

    let log = s.log_text();
    let (mut first, mut gap, mut to_paint, mut needed, mut chunks) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let (mut no_echo, mut late) = (0, Vec::new());
    for line in log.lines() {
        if line.contains("popup paint timing no_echo") {
            no_echo += 1;
        } else if line.contains("popup paint timing") {
            let get = |key| value(line, key).unwrap_or(0);
            let (f, g) = (get("report_to_first_echo_us"), get("max_echo_gap_us"));
            first.push(f);
            gap.push(g);
            needed.push(f.max(g));
            to_paint.push(get("last_echo_to_paint_us"));
            chunks.push(get("echo_chunks"));
        } else if let Some(us) = value(line, "popup late echo after_paint_us") {
            late.push(us);
        }
    }
    let lines: Vec<&str> = log.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        if line.contains("popup late echo") {
            println!("--- late echo ---");
            for context in &lines[i.saturating_sub(6)..=i] {
                println!("{context}");
            }
        }
    }
    println!("keys sent: {keys}, paints after a report: {}", first.len());
    summary("report -> first echo byte", first);
    summary("max gap between echo chunks", gap);
    summary("last echo byte -> paint", to_paint);
    summary("echo chunks per paint", chunks);
    let needed = summary("quiet needed (max of the first two)", needed);
    println!("paints with no echo: {no_echo}");
    summary("late echo, after its paint", late);
    // The smallest quiet that covers 99% of the paints.
    let at_99 = (needed.len() * 99).div_ceil(100).saturating_sub(1);
    println!(
        "99% of the paints need a quiet of at most {} us",
        needed.get(at_99).copied().unwrap_or(0)
    );
    for quiet in [300, 500, 700, 1000, 1200, 1300, 1400, 1500] {
        let covered = needed.iter().filter(|&&us| us <= quiet).count();
        println!(
            "quiet {quiet} us covers {covered}/{} ({:.1}%)",
            needed.len(),
            100.0 * covered as f64 / needed.len().max(1) as f64
        );
    }
}
