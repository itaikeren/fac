//! Which terminal fac runs in, from the environment it was started with.
//! Warp is not here: `guards.rs` steps aside for it before anything runs.

/// A terminal whose answers and quirks were measured in phase 3
/// (`docs/plans/phase-3-live/probe/findings.md`), or `Unknown`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TermKind {
    /// Terminal.app.
    Apple,
    Iterm2,
    /// VS Code's integrated terminal.
    Vscode,
    Zed,
    /// Ghostty, and terminals built on it.
    Ghostty,
    Unknown,
}

/// Each measured terminal's `TERM_PROGRAM`. One table for [`detect`] and
/// for the shell test that `fac init` writes into the startup files
/// (`src/init/block.rs`), so the two tell terminals apart the same way.
pub const TERM_PROGRAMS: [(&str, TermKind); 5] = [
    ("Apple_Terminal", TermKind::Apple),
    ("iTerm.app", TermKind::Iterm2),
    ("vscode", TermKind::Vscode),
    ("zed", TermKind::Zed),
    ("ghostty", TermKind::Ghostty),
];

/// `LC_TERMINAL` in iTerm2 (which also survives in some of its children).
pub const LC_TERMINAL_ITERM2: &str = "iTerm2";

/// The terminals `fac init` lists, in its order.
pub const KNOWN: [TermKind; 5] = [
    TermKind::Apple,
    TermKind::Iterm2,
    TermKind::Ghostty,
    TermKind::Vscode,
    TermKind::Zed,
];

impl TermKind {
    /// The name `fac init --terminals` takes, and its startup block lists.
    pub fn id(self) -> &'static str {
        match self {
            TermKind::Apple => "terminal",
            TermKind::Iterm2 => "iterm2",
            TermKind::Ghostty => "ghostty",
            TermKind::Vscode => "vscode",
            TermKind::Zed => "zed",
            TermKind::Unknown => "unknown",
        }
    }

    /// The terminal's name as people know it.
    pub fn label(self) -> &'static str {
        match self {
            TermKind::Apple => "Terminal.app",
            TermKind::Iterm2 => "iTerm2",
            TermKind::Ghostty => "Ghostty",
            TermKind::Vscode => "VS Code",
            TermKind::Zed => "Zed",
            TermKind::Unknown => "an unknown terminal",
        }
    }

    /// The known terminal named `id` (see [`TermKind::id`]).
    pub fn from_id(id: &str) -> Option<TermKind> {
        KNOWN.into_iter().find(|kind| kind.id() == id)
    }
}

/// The kind from `TERM_PROGRAM`, then `LC_TERMINAL` (iTerm2 sets it to
/// `iTerm2`), then `ZED_TERM` (Zed sets it), each as measured in
/// findings.md.
pub fn detect(
    term_program: Option<&str>,
    lc_terminal: Option<&str>,
    zed_term: Option<&str>,
) -> TermKind {
    if let Some((_, kind)) = TERM_PROGRAMS
        .iter()
        .find(|(name, _)| term_program == Some(*name))
    {
        return *kind;
    }
    if lc_terminal == Some(LC_TERMINAL_ITERM2) {
        TermKind::Iterm2
    } else if zed_term.is_some() {
        TermKind::Zed
    } else {
        TermKind::Unknown
    }
}

/// The kind of the terminal fac was started in, read from its environment.
pub fn from_env() -> TermKind {
    let var = |name: &str| std::env::var(name).ok();
    detect(
        var("TERM_PROGRAM").as_deref(),
        var("LC_TERMINAL").as_deref(),
        var("ZED_TERM").as_deref(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn term_program_names_each_measured_terminal() {
        for (name, kind) in [
            ("Apple_Terminal", TermKind::Apple),
            ("iTerm.app", TermKind::Iterm2),
            ("vscode", TermKind::Vscode),
            ("zed", TermKind::Zed),
            ("ghostty", TermKind::Ghostty),
        ] {
            assert_eq!(detect(Some(name), None, None), kind, "{name}");
        }
    }

    #[test]
    fn term_program_comes_first() {
        // VS Code started from iTerm2 inherits LC_TERMINAL.
        assert_eq!(
            detect(Some("vscode"), Some("iTerm2"), None),
            TermKind::Vscode
        );
        assert_eq!(
            detect(Some("Apple_Terminal"), None, Some("true")),
            TermKind::Apple
        );
        assert_eq!(
            detect(Some("ghostty"), Some("iTerm2"), Some("true")),
            TermKind::Ghostty
        );
    }

    #[test]
    fn then_lc_terminal_then_zed_term() {
        assert_eq!(detect(None, Some("iTerm2"), None), TermKind::Iterm2);
        assert_eq!(detect(Some("tmux"), Some("iTerm2"), None), TermKind::Iterm2);
        assert_eq!(detect(None, Some("iTerm2"), Some("true")), TermKind::Iterm2);
        assert_eq!(detect(None, None, Some("true")), TermKind::Zed);
        // Set at all is enough.
        assert_eq!(detect(None, None, Some("")), TermKind::Zed);
        assert_eq!(detect(Some("tmux"), None, Some("1")), TermKind::Zed);
    }

    #[test]
    fn anything_else_is_unknown() {
        assert_eq!(detect(None, None, None), TermKind::Unknown);
        for name in [
            "",
            "WarpTerminal",
            "tmux",
            "Apple_terminal",
            "iTerm2",
            "Ghostty",
        ] {
            assert_eq!(detect(Some(name), None, None), TermKind::Unknown, "{name}");
        }
        assert_eq!(detect(None, Some("iTerm.app"), None), TermKind::Unknown);
        assert_eq!(detect(None, Some(""), None), TermKind::Unknown);
    }
}
