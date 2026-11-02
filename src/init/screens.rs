//! What `fac init`, `enable`, `disable` and `uninstall` print: whole
//! screens as strings, each with a blank line around it and a 2-space
//! margin, so they are easy to test. See [`super::style`] for the look.

use std::path::PathBuf;

use super::files::Done;
use super::style::{Style, WIDTH, wrap};

/// The width of the command names in the lists of commands.
const COMMANDS: usize = 13;

/// The details rows for what `done` did, one per kind that has any:
/// changed, created, removed, and the backups written now.
fn done_rows(style: &Style, done: &[Done]) -> Vec<String> {
    let of = |keep: &dyn Fn(&Done) -> bool| -> Vec<PathBuf> {
        done.iter()
            .filter(|item| keep(item))
            .map(|item| item.path.clone())
            .collect()
    };
    let mut rows = Vec::new();
    for (label, paths) in [
        ("Changed", of(&|d| !d.created && !d.deleted)),
        ("Created", of(&|d| d.created && !d.deleted)),
        ("Removed", of(&|d| d.deleted)),
    ] {
        if !paths.is_empty() {
            rows.push(style.row(label, &style.paths(&paths)));
        }
    }
    let backups: Vec<PathBuf> = done.iter().filter_map(|d| d.backup.clone()).collect();
    if !backups.is_empty() {
        rows.push(style.row("Backup", &style.paths(&backups)));
    }
    rows
}

/// `lines`, one per line, then a blank one; with a blank one first.
fn screen(lines: &[String]) -> String {
    let mut out = String::from("\n");
    for line in lines {
        out.push_str(line);
        out.push('\n');
    }
    out.push('\n');
    out
}

/// What `fac init` says when it is done.
pub struct SetUp<'a> {
    pub terminals: &'a str,
    pub done: &'a [Done],
    /// Notes for the yellow lines before "Open a new window".
    pub notes: Vec<String>,
}

pub fn set_up(style: &Style, info: &SetUp) -> String {
    let mut lines = vec![
        style.success("fac is set up"),
        String::new(),
        style.row("Starts in", info.terminals),
    ];
    if info.done.is_empty() {
        lines.push(style.row("Changed", "nothing: it was set up already"));
    } else {
        lines.extend(done_rows(style, info.done));
    }
    lines.push(String::new());
    for note in &info.notes {
        lines.push(style.note(note));
        lines.push(String::new());
    }
    lines.push("  Open a new window to try it.".to_owned());
    lines.push(String::new());
    lines.push(style.command("fac disable", "turn it off for new windows", COMMANDS));
    lines.push(style.command("fac uninstall", "remove it", COMMANDS));
    lines.push(String::new());
    lines.push(style.dim(&format!("  {SAFE_MODE}")));
    screen(&lines)
}

/// The line about safe mode at the end of `fac init`'s screen.
const SAFE_MODE: &str =
    "If fac ever fails to start, new windows open plain and fac doctor shows why.";

/// What `fac enable` and `fac disable` say. `already`: nothing changed.
/// `safe_mode_off`: `fac enable` removed safe mode's file.
pub fn toggled(
    style: &Style,
    on: bool,
    terminals: &str,
    done: &[Done],
    safe_mode_off: bool,
) -> String {
    let already = done.is_empty();
    let title = match (on, already) {
        (true, false) => "fac is on for new windows",
        (true, true) => "fac is on already",
        (false, false) => "fac is off for new windows",
        (false, true) => "fac is off already",
    };
    let mut lines = vec![
        style.success(title),
        String::new(),
        style.row(if on { "Starts in" } else { "Terminals" }, terminals),
    ];
    lines.extend(done_rows(style, done));
    if safe_mode_off {
        lines.push(style.row("Safe mode", "off: new windows start fac again"));
    }
    lines.push(String::new());
    lines.push(if on {
        style.command("fac disable", "turn it off", 0)
    } else {
        style.command("fac enable", "turn it back on", 0)
    });
    screen(&lines)
}

/// What `fac uninstall` says: the files changed, the backups and the
/// folders it keeps.
pub fn removed(style: &Style, done: &[Done], backups: &[PathBuf], folders: &[PathBuf]) -> String {
    let mut lines = vec![style.success(if done.is_empty() {
        "fac was not set up: no file changed"
    } else {
        "fac is removed"
    })];
    let of = |keep: &dyn Fn(&Done) -> bool| -> Vec<String> {
        done.iter()
            .filter(|item| keep(item))
            .map(|item| style.path(&item.path))
            .collect()
    };
    let mut rows = Vec::new();
    rows.extend(style.rows("Changed", &of(&|d| !d.deleted)));
    rows.extend(style.rows("Deleted", &of(&|d| d.deleted)));
    let kept: Vec<String> = backups
        .iter()
        .chain(folders)
        .map(|path| style.path(path))
        .collect();
    rows.extend(style.rows("Kept", &kept));
    if !rows.is_empty() {
        lines.push(String::new());
        lines.extend(rows);
    }
    lines.push(String::new());
    let mut text = String::new();
    if !done.is_empty() {
        text.push_str("New windows start your shell without fac. ");
    }
    text.push_str(
        "fac keeps the backups (your files as they were before fac init) and its folders; delete them yourself if you want them gone.",
    );
    lines.extend(
        wrap(&text, WIDTH, 2)
            .into_iter()
            .map(|line| style.dim(&format!("  {line}"))),
    );
    screen(&lines)
}

/// A refusal or usage error: a red cross and the reason, then what to do.
pub fn problem(style: &Style, reason: &str, hint: Option<&str>) -> String {
    let wrapped = |text: &str| wrap(&style.tilde(text), WIDTH, 4);
    let mut lines = Vec::new();
    for (n, line) in wrapped(reason).iter().enumerate() {
        lines.push(if n == 0 {
            format!("  {} {line}", style.red(style.cross()))
        } else {
            format!("    {line}")
        });
    }
    if let Some(hint) = hint {
        lines.extend(
            wrapped(hint)
                .into_iter()
                .map(|line| style.dim(&format!("    {line}"))),
        );
    }
    screen(&lines)
}

/// A note on its own: `  ! text`.
pub fn note(style: &Style, text: &str) -> String {
    screen(&[style.note(&style.tilde(text))])
}

/// One dim line: the checklist was cancelled, or nothing was ticked.
pub fn dim_line(style: &Style, text: &str) -> String {
    screen(&[style.dim(&format!("  {text}"))])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style() -> Style {
        Style::new(false, true, Some(PathBuf::from("/Users/me")))
    }

    fn done(path: &str, created: bool, deleted: bool, backup: Option<&str>) -> Done {
        Done {
            path: PathBuf::from(path),
            created,
            deleted,
            backup: backup.map(PathBuf::from),
        }
    }

    #[test]
    fn the_success_screen() {
        let done = [
            done(
                "/Users/me/.zshrc",
                false,
                false,
                Some("/Users/me/.zshrc.fac-backup"),
            ),
            done("/Users/me/.zprofile", false, false, None),
        ];
        let text = set_up(
            &style(),
            &SetUp {
                terminals: "Terminal.app, Zed",
                done: &done,
                notes: vec!["/bin/bash is bash 3.2, and fac needs 5.3 or later.".to_owned()],
            },
        );
        let expected = "
  ✓ fac is set up

    Starts in   Terminal.app, Zed
    Changed     ~/.zshrc, ~/.zprofile
    Backup      ~/.zshrc.fac-backup

  ! /bin/bash is bash 3.2, and fac needs 5.3 or later.

  Open a new window to try it.

    fac disable     turn it off for new windows
    fac uninstall   remove it

  If fac ever fails to start, new windows open plain and fac doctor shows why.

";
        assert_eq!(text, expected);
        assert!(text.lines().all(|line| line.chars().count() <= WIDTH));
    }

    #[test]
    fn enable_says_when_safe_mode_went_off() {
        let on = toggled(&style(), true, "Zed", &[], true);
        assert!(
            on.contains("    Safe mode   off: new windows start fac again\n"),
            "{on}"
        );
        assert!(!toggled(&style(), true, "Zed", &[], false).contains("Safe mode"));
    }

    #[test]
    fn an_error_has_a_reason_and_a_hint() {
        let text = problem(&style(), "cannot write /Users/me/.zshrc", Some("Fix it."));
        assert_eq!(text, "\n  ✗ cannot write ~/.zshrc\n    Fix it.\n\n");
    }

    #[test]
    fn uninstall_lists_each_path_on_its_own_line() {
        let done = [done("/Users/me/.zshrc", false, false, None)];
        let text = removed(
            &style(),
            &done,
            &[PathBuf::from("/Users/me/.zshrc.fac-backup")],
            &[PathBuf::from("/Users/me/.config/fac")],
        );
        assert!(text.contains("    Changed     ~/.zshrc\n"), "{text}");
        assert!(
            text.contains("    Kept        ~/.zshrc.fac-backup\n"),
            "{text}"
        );
        assert!(text.contains("                ~/.config/fac\n"), "{text}");
    }
}
