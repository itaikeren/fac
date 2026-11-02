//! The startup block `fac init` writes: its text for each shell, and finding
//! it again in a file. The block is all the state there is: which
//! terminals are ticked, whether it is on, and whether fac created the file
//! it is in are read back from its third line.

use std::ops::Range;
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};

use crate::guards::{SHELL_MARK, SHELL_MARK_RUN, ShellKind};
use crate::term::ident::{KNOWN, LC_TERMINAL_ITERM2, TERM_PROGRAMS, TermKind};

/// The block's first line.
pub const BEGIN: &str = "# >>> fac >>>";
/// The block's last line.
pub const END: &str = "# <<< fac <<<";
/// The second line.
const ABOUT: &str =
    "# Added by `fac init`; `fac uninstall` removes it. fac rewrites these lines whole.";
/// The third line starts with this, then the state.
const STATE: &str = "# fac: ";
/// After the state on the third line when fac created the file.
const CREATED: &str = "; fac created this file";
/// Safe mode's file, under `${XDG_CONFIG_HOME:-$HOME/.config}`: fac writes
/// it when it fails to start, and while it exists every block starts the
/// shell plain and says so in one line ([`NO_START_LINE`]). `fac doctor`
/// and `fac enable` remove it.
pub const NO_START: &str = "fac/no-start";
/// What a block prints (dim, to stderr) when it skips because of
/// [`NO_START`]. Its words are safe in double quotes in every shell.
pub const NO_START_LINE: &str =
    "fac didn't start last time, so this window is plain. Run: fac doctor";
/// The shell variable (never exported) a block sets once it printed
/// [`NO_START_LINE`], so a login shell that runs two blocks says it once.
const SAID: &str = "__fac_said_plain";

/// What a block says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    /// Off: the block stays and does nothing (`fac disable`).
    pub on: bool,
    /// The ticked terminals, in [`KNOWN`]'s order.
    pub terminals: Vec<TermKind>,
    /// The file did not exist before fac wrote it: `fac uninstall` deletes
    /// it once nothing but the block is in it.
    pub created: bool,
}

impl Block {
    /// The block's state line, without the leading `# fac: `.
    fn state(&self) -> String {
        let ids: Vec<&str> = self.terminals.iter().map(|kind| kind.id()).collect();
        format!(
            "{} {}{}",
            if self.on { "on" } else { "off" },
            ids.join(","),
            if self.created { CREATED } else { "" }
        )
    }

    /// The state line's contents back into a block.
    fn parse_state(state: &str) -> Option<Block> {
        let (state, created) = match state.strip_suffix(CREATED) {
            Some(state) => (state, true),
            None => (state, false),
        };
        let (on, ids) = state.split_once(' ').unwrap_or((state, ""));
        let on = match on {
            "on" => true,
            "off" => false,
            _ => return None,
        };
        let mut terminals = Vec::new();
        for id in ids.split(',').filter(|id| !id.is_empty()) {
            terminals.push(TermKind::from_id(id)?);
        }
        Some(Block {
            on,
            terminals: sorted(terminals),
            created,
        })
    }
}

/// `terminals` in [`KNOWN`]'s order, each once.
pub fn sorted(terminals: Vec<TermKind>) -> Vec<TermKind> {
    KNOWN
        .into_iter()
        .filter(|kind| terminals.contains(kind))
        .collect()
}

/// `text` as one word for sh, bash and zsh: in single quotes.
pub fn sh_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// `text` as one fish word: in single quotes, with `\` and `'` escaped.
fn fish_quote(text: &str) -> String {
    let mut quoted = String::from("'");
    for c in text.chars() {
        if matches!(c, '\\' | '\'') {
            quoted.push('\\');
        }
        quoted.push(c);
    }
    quoted.push('\'');
    quoted
}

/// The test whether the terminal is a ticked one, for sh, bash and zsh: a
/// `case` command that is true for a ticked terminal, told apart as
/// `ident::detect` does: `TERM_PROGRAM` first, then `LC_TERMINAL`, then
/// `ZED_TERM` (set at all). Each line names its terminal. `indent` is put
/// before every line but the first.
pub fn ticked_test_sh(kind: ShellKind, ticked: &[TermKind], indent: &str) -> String {
    let truth = |term: TermKind| {
        if ticked.contains(&term) {
            "true"
        } else {
            "false"
        }
    };
    let (lc_test, zed_test) = match kind {
        ShellKind::Zsh => (
            format!("[[ ${{LC_TERMINAL-}} == {LC_TERMINAL_ITERM2} ]]"),
            "[[ -n ${ZED_TERM+x} ]]".to_owned(),
        ),
        _ => (
            format!("[ \"${{LC_TERMINAL-}}\" = {LC_TERMINAL_ITERM2} ]"),
            "[ -n \"${ZED_TERM+x}\" ]".to_owned(),
        ),
    };
    let mut out = String::from("case ${TERM_PROGRAM-} in\n");
    for (name, term) in TERM_PROGRAMS {
        out.push_str(&format!(
            "{indent}  {name}) {} ;; # {}\n",
            truth(term),
            term.label()
        ));
    }
    out.push_str(&format!(
        "{indent}  *) if {lc_test}; then {}; elif {zed_test}; then {}; else false; fi ;;\n",
        truth(TermKind::Iterm2),
        truth(TermKind::Zed)
    ));
    out.push_str(&format!("{indent}esac"));
    out
}

/// For fish: lines that set the local `terminal` to the terminal's id (as
/// `ident::detect` tells it), then the test whether it is a ticked one.
/// `indent` is put before every line.
pub fn ticked_test_fish(ticked: &[TermKind], indent: &str) -> (String, String) {
    let mut out = format!("{indent}set -l terminal\n{indent}switch \"$TERM_PROGRAM\"\n");
    for (name, term) in TERM_PROGRAMS {
        out.push_str(&format!(
            "{indent}    case {name}\n{indent}        set terminal {}\n",
            term.id()
        ));
    }
    out.push_str(&format!(
        "{indent}    case '*'\n\
         {indent}        if test \"$LC_TERMINAL\" = {LC_TERMINAL_ITERM2}\n\
         {indent}            set terminal {}\n\
         {indent}        else if set -q ZED_TERM\n\
         {indent}            set terminal {}\n\
         {indent}        end\n\
         {indent}end\n",
        TermKind::Iterm2.id(),
        TermKind::Zed.id()
    ));
    let ids: Vec<&str> = ticked.iter().map(|term| term.id()).collect();
    let test = format!("contains -- \"$terminal\" {}", ids.join(" "))
        .trim_end()
        .to_owned();
    (out, test)
}

/// The whole block for a `kind` shell, every line ending in a newline,
/// starting `fac` (a path).
pub fn render(kind: ShellKind, block: &Block, fac: &Path) -> Result<String> {
    let fac = fac
        .to_str()
        .ok_or_else(|| anyhow!("fac's path is not UTF-8: {}", fac.display()))?;
    if fac.contains('\n') {
        return Err(anyhow!("fac's path has a line break in it"));
    }
    let mut out = format!("{BEGIN}\n{ABOUT}\n{STATE}{}\n", block.state());
    let off = if block.on { "" } else { "false && " };
    let run = SHELL_MARK_RUN;
    let mark = SHELL_MARK;
    match kind {
        ShellKind::Zsh => {
            let fac = sh_quote(fac);
            let ticked = ticked_test_sh(kind, &block.terminals, "  ");
            out.push_str(&format!(
                "if [[ ${{{mark}-}} == {run} || ${{{mark}-}} == plain:$$ ]]; then\n\
                 \x20 typeset -g +x {mark}\n\
                 elif {off}[[ -o interactive && -z ${{ZSH_EXECUTION_STRING+x}} && -t 0 && -t 1 ]] &&\n\
                 \x20 [[ -z ${{ZSH_SCRIPT-}} || $ZSH_SCRIPT != \"${{ZSH_ARGZERO-}}\" ]] &&\n\
                 \x20 [[ ${{FAC_DISABLE-}} != 1 && -z ${{Q_TERM-}} && -z ${{SSH_CONNECTION-}} ]] &&\n\
                 \x20 [[ -z ${{TMUX-}} && -z ${{STY-}} ]] &&\n\
                 \x20 [[ -z ${{FAC_ACTIVE-}} || ${{FAC_ACTIVE#*:}} != $TTY ]] &&\n\
                 \x20 [[ -x {fac} ]] &&\n\
                 \x20 {ticked}\n\
                 then\n\
                 \x20 if [[ -e ${{XDG_CONFIG_HOME:-$HOME/.config}}/{NO_START} ]]; then\n\
                 \x20   if [[ -z ${{{SAID}-}} && -t 2 ]]; then\n\
                 \x20     typeset -g {SAID}=1\n\
                 \x20     if [[ -n ${{NO_COLOR+x}} ]]; then print -ru2 -- \"{line}\"\n\
                 \x20     else print -ru2 -- $'\\e[2m'\"{line}\"$'\\e[0m'; fi\n\
                 \x20   fi\n\
                 \x20 else\n\
                 \x20   [[ ${{SHELL:t}} == zsh ]] || export SHELL=${{commands[zsh]:-/bin/zsh}}\n\
                 \x20   exec {fac}\n\
                 \x20 fi\n\
                 fi\n",
                line = NO_START_LINE
            ));
        }
        ShellKind::Bash => {
            let fac = sh_quote(fac);
            let ticked = ticked_test_sh(kind, &block.terminals, "    ");
            out.push_str(&format!(
                "if [ -n \"${{BASH_VERSION-}}\" ] && case :${{SHELLOPTS-}}: in *:posix:*) false ;; *) true ;; esac; then\n\
                 \x20 if [ \"${{{mark}-}}\" = {run} ] || [ \"${{{mark}-}}\" = \"plain:$$\" ]; then\n\
                 \x20   export -n {mark}\n\
                 \x20 elif {off}case $- in *i*) true ;; *) false ;; esac &&\n\
                 \x20   case ${{0##*/}} in bash | -bash) true ;; *) false ;; esac &&\n\
                 \x20   [ -z \"${{BASH_EXECUTION_STRING+x}}\" ] && [ -t 0 ] && [ -t 1 ] &&\n\
                 \x20   [ \"${{FAC_DISABLE-}}\" != 1 ] && [ -z \"${{Q_TERM-}}\" ] && [ -z \"${{SSH_CONNECTION-}}\" ] &&\n\
                 \x20   [ -z \"${{TMUX-}}\" ] && [ -z \"${{STY-}}\" ] &&\n\
                 \x20   ! {{ [ -n \"${{FAC_ACTIVE-}}\" ] && [ /dev/stdin -ef \"${{FAC_ACTIVE#*:}}\" ]; }} &&\n\
                 \x20   [ -x {fac} ] &&\n\
                 \x20   {ticked}\n\
                 \x20 then\n\
                 \x20   if [ -e \"${{XDG_CONFIG_HOME:-$HOME/.config}}/{NO_START}\" ]; then\n\
                 \x20     if [ -z \"${{{SAID}-}}\" ] && [ -t 2 ]; then\n\
                 \x20       {SAID}=1\n\
                 \x20       if [ -n \"${{NO_COLOR+x}}\" ]; then printf '%s\\n' \"{line}\" >&2\n\
                 \x20       else printf '\\033[2m%s\\033[0m\\n' \"{line}\" >&2; fi\n\
                 \x20     fi\n\
                 \x20   else\n\
                 \x20     case ${{BASH##*/}} in\n\
                 \x20       bash*) SHELL=$BASH ;;\n\
                 \x20       *) case ${{SHELL##*/}} in bash*) ;; *) SHELL=$(type -P bash) ;; esac ;;\n\
                 \x20     esac\n\
                 \x20     export SHELL\n\
                 \x20     exec {fac}\n\
                 \x20   fi\n\
                 \x20 fi\n\
                 fi\n",
                line = NO_START_LINE
            ));
        }
        ShellKind::Fish => {
            let fac = fish_quote(fac);
            let (set_terminal, ticked) = ticked_test_fish(&block.terminals, "    ");
            let off = if block.on { "" } else { "false; and " };
            out.push_str(&format!(
                "if test \"${mark}\" = {run} -o \"${mark}\" = plain:$fish_pid\n\
                 \x20   set -gu {mark} ${mark}\n\
                 else if {off}status is-interactive; and test -t 0 -a -t 1\n\
                 \x20   and test \"$FAC_DISABLE\" != 1 -a -z \"$Q_TERM\" -a -z \"$SSH_CONNECTION\"\n\
                 \x20   and test -z \"$TMUX\" -a -z \"$STY\"\n\
                 \x20   and test -x {fac}\n\
                 \x20   set -l config $HOME/.config\n\
                 \x20   test -n \"$XDG_CONFIG_HOME\"; and set config $XDG_CONFIG_HOME\n\
                 {set_terminal}\
                 \x20   if {ticked}\n\
                 \x20       if test -e \"$config/{NO_START}\"\n\
                 \x20           if not set -q {SAID}; and test -t 2\n\
                 \x20               set -g {SAID} 1\n\
                 \x20               if set -q NO_COLOR\n\
                 \x20                   printf '%s\\n' \"{line}\" >&2\n\
                 \x20               else\n\
                 \x20                   printf '\\e[2m%s\\e[0m\\n' \"{line}\" >&2\n\
                 \x20               end\n\
                 \x20           end\n\
                 \x20       else\n\
                 \x20           string match -qr -- '(^|/)fish$' \"$SHELL\"; or set -gx SHELL (status fish-path)\n\
                 \x20           exec {fac}\n\
                 \x20       end\n\
                 \x20   end\n\
                 end\n",
                line = NO_START_LINE
            ));
        }
    }
    out.push_str(END);
    out.push('\n');
    Ok(out)
}

/// The fac the first block in `text` starts: the path on its `exec` line,
/// unquoted as `kind`'s shell would; `None` without a block or a line fac
/// can read.
pub fn exec_path(kind: ShellKind, text: &[u8]) -> Option<PathBuf> {
    let range = find(text).ok()?.into_iter().next()?;
    let block = std::str::from_utf8(&text[range]).ok()?;
    let word = block
        .lines()
        .find_map(|line| line.trim().strip_prefix("exec "))?;
    let path = match kind {
        ShellKind::Fish => fish_unquote(word)?,
        _ => sh_unquote(word)?,
    };
    Some(PathBuf::from(path))
}

/// [`sh_quote`]'s word back: single-quoted runs and `\'` between them.
fn sh_unquote(word: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = word.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' => loop {
                match chars.next()? {
                    '\'' => break,
                    c => out.push(c),
                }
            },
            '\\' => out.push(chars.next()?),
            _ => return None,
        }
    }
    Some(out)
}

/// [`fish_quote`]'s word back.
fn fish_unquote(word: &str) -> Option<String> {
    let inner = word.strip_prefix('\'')?.strip_suffix('\'')?;
    let mut out = String::new();
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.push(chars.next()?),
            '\'' => return None,
            c => out.push(c),
        }
    }
    Some(out)
}

/// The lines of `text` as byte ranges, each with its newline (the last
/// may have none).
fn lines(text: &[u8]) -> impl Iterator<Item = Range<usize>> + '_ {
    let mut start = 0;
    std::iter::from_fn(move || {
        if start >= text.len() {
            return None;
        }
        let end = text[start..]
            .iter()
            .position(|&byte| byte == b'\n')
            .map_or(text.len(), |at| start + at + 1);
        let line = start..end;
        start = end;
        Some(line)
    })
}

/// A line's bytes without its newline (and a `\r` before it).
fn bare(line: &[u8]) -> &[u8] {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    line.strip_suffix(b"\r").unwrap_or(line)
}

/// Every block in `text`, as byte ranges from the start of its first line
/// to the end of its last (with the newline). An error for a first line
/// with no last line after it.
pub fn find(text: &[u8]) -> Result<Vec<Range<usize>>> {
    let mut found = Vec::new();
    let mut open = None;
    for line in lines(text) {
        let bytes = bare(&text[line.clone()]);
        if bytes == BEGIN.as_bytes() {
            if open.is_some() {
                return Err(anyhow!("fac's block starts twice with no end line between"));
            }
            open = Some(line.start);
        } else if bytes == END.as_bytes() {
            match open.take() {
                Some(start) => found.push(start..line.end),
                None => return Err(anyhow!("fac's block has an end line with no start line")),
            }
        }
    }
    if open.is_some() {
        return Err(anyhow!("fac's block has no end line ({END})"));
    }
    Ok(found)
}

/// `text` without its blocks: byte for byte what it was before the first
/// block went in.
pub fn strip(text: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len());
    let mut at = 0;
    for range in find(text)? {
        out.extend_from_slice(&text[at..range.start]);
        at = range.end;
    }
    out.extend_from_slice(&text[at..]);
    Ok(out)
}

/// What the first block in `text` says; `None` with no block. An error
/// for a block fac cannot read.
pub fn read(text: &[u8]) -> Result<Option<Block>> {
    let Some(range) = find(text)?.into_iter().next() else {
        return Ok(None);
    };
    lines(&text[range.clone()])
        .map(|line| bare(&text[range.start + line.start..range.start + line.end]))
        .find_map(|line| line.strip_prefix(STATE.as_bytes()))
        .and_then(|state| std::str::from_utf8(state).ok())
        .and_then(Block::parse_state)
        .map(Some)
        .ok_or_else(|| anyhow!("fac's block has no state line fac can read"))
}

/// `text` with `block` (its rendered text) at the very top, in place of
/// any block it had.
pub fn put(text: &[u8], block: &str) -> Result<Vec<u8>> {
    let rest = strip(text)?;
    let mut out = Vec::with_capacity(block.len() + rest.len());
    out.extend_from_slice(block.as_bytes());
    out.extend_from_slice(&rest);
    Ok(out)
}

/// Whether `text` holds a marker line's bytes anywhere: a cheap test
/// before any parsing.
pub fn has_marker(text: &[u8]) -> bool {
    [BEGIN, END].iter().any(|marker| {
        text.windows(marker.len())
            .any(|bytes| bytes == marker.as_bytes())
    })
}

/// Whether `text` starts with a block's first line (fish's file).
pub fn starts_with_block(text: &[u8]) -> bool {
    lines(text)
        .next()
        .is_some_and(|line| bare(&text[line]) == BEGIN.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(on: bool, terminals: &[TermKind], created: bool) -> Block {
        Block {
            on,
            terminals: terminals.to_vec(),
            created,
        }
    }

    const FAC: &str = "/opt/homebrew/bin/fac";

    #[test]
    fn state_round_trips() {
        for kind in [ShellKind::Zsh, ShellKind::Bash, ShellKind::Fish] {
            for b in [
                block(true, &[TermKind::Apple, TermKind::Ghostty], false),
                block(false, &[TermKind::Apple, TermKind::Ghostty], true),
                block(true, &KNOWN, false),
                block(false, &[], false),
            ] {
                let text = render(kind, &b, Path::new(FAC)).expect("render");
                assert_eq!(read(text.as_bytes()).expect("read"), Some(b.clone()));
                assert!(text.starts_with(&format!("{BEGIN}\n")), "{text}");
                assert!(text.ends_with(&format!("\n{END}\n")), "{text}");
                assert_eq!(find(text.as_bytes()).expect("find"), vec![0..text.len()]);
            }
        }
    }

    #[test]
    fn put_and_strip_are_byte_exact() {
        let b = render(
            ShellKind::Zsh,
            &block(true, &[TermKind::Apple], false),
            Path::new(FAC),
        )
        .expect("render");
        for original in [
            &b""[..],
            b"export A=1\n",
            b"export A=1",
            b"\n\n",
            b"line\r\nno newline at the end",
            b"\xff\xfe not utf-8\n",
        ] {
            let with = put(original, &b).expect("put");
            assert!(with.starts_with(b.as_bytes()));
            assert_eq!(&with[b.len()..], original);
            assert_eq!(strip(&with).expect("strip"), original);
            // A second put replaces the block, never adds one.
            let again = put(&with, &b).expect("put again");
            assert_eq!(again, with);
            assert_eq!(find(&again).expect("find").len(), 1);
        }
    }

    #[test]
    fn a_block_elsewhere_moves_to_the_top() {
        let b = render(
            ShellKind::Bash,
            &block(true, &[TermKind::Apple], false),
            Path::new(FAC),
        )
        .expect("render");
        let text = format!("# Q pre block\nexport A=1\n{b}export B=2");
        let stripped = strip(text.as_bytes()).expect("strip");
        assert_eq!(stripped, b"# Q pre block\nexport A=1\nexport B=2");
        let moved = put(text.as_bytes(), &b).expect("put");
        assert_eq!(
            moved,
            format!("{b}# Q pre block\nexport A=1\nexport B=2").into_bytes()
        );
    }

    #[test]
    fn broken_blocks_are_errors() {
        for text in [
            format!("{BEGIN}\nx\n"),
            format!("x\n{END}\n"),
            format!("{BEGIN}\n{BEGIN}\n{END}\n"),
        ] {
            assert!(find(text.as_bytes()).is_err(), "{text}");
            assert!(strip(text.as_bytes()).is_err(), "{text}");
        }
        let unreadable = format!("{BEGIN}\n# fac: maybe\n{END}\n");
        assert!(read(unreadable.as_bytes()).is_err());
        let unknown = format!("{BEGIN}\n# fac: on warp\n{END}\n");
        assert!(read(unknown.as_bytes()).is_err());
        assert_eq!(read(b"no block\n").expect("read"), None);
    }

    #[test]
    fn quoting() {
        assert_eq!(sh_quote("/a b/fac"), "'/a b/fac'");
        assert_eq!(sh_quote("/o'neil/fac"), r"'/o'\''neil/fac'");
        assert_eq!(fish_quote(r"/o'neil/a\b"), r"'/o\'neil/a\\b'");
        let odd = render(
            ShellKind::Zsh,
            &block(true, &[TermKind::Apple], false),
            Path::new("/Users/o'neil/bin/fac"),
        )
        .expect("render");
        assert!(odd.contains(r"exec '/Users/o'\''neil/bin/fac'"), "{odd}");
        assert!(render(ShellKind::Zsh, &block(true, &[], false), Path::new("/a\nb")).is_err());
    }

    #[test]
    fn the_exec_path_reads_back() {
        for kind in [ShellKind::Zsh, ShellKind::Bash, ShellKind::Fish] {
            for path in [FAC, "/Users/o'neil/my bin/fac", r"/a\b/fac"] {
                let b = block(true, &KNOWN, false);
                let text = render(kind, &b, Path::new(path)).expect("render");
                let with = put(b"export A=1\n", &text).expect("put");
                assert_eq!(
                    exec_path(kind, &with),
                    Some(PathBuf::from(path)),
                    "{kind:?} {path}"
                );
            }
        }
        assert_eq!(exec_path(ShellKind::Zsh, b"exec /bin/zsh\n"), None);
        assert_eq!(sh_unquote("/no/quotes"), None);
        assert_eq!(fish_unquote("'open"), None);
    }

    #[test]
    fn the_no_start_line_is_printed_once_and_dim() {
        for kind in [ShellKind::Zsh, ShellKind::Bash, ShellKind::Fish] {
            let text = render(kind, &block(true, &KNOWN, false), Path::new(FAC)).expect("render");
            assert_eq!(text.matches(NO_START).count(), 1, "{text}");
            assert_eq!(text.matches(NO_START_LINE).count(), 2, "{text}");
            assert!(text.contains("[2m"), "{text}");
            assert!(text.contains("NO_COLOR"), "{text}");
            assert!(text.contains(SAID), "{text}");
            // The exec is in the other branch of the same test.
            let skip = text.find(NO_START).expect("the file");
            let exec = text.find("exec '").expect("the exec");
            assert!(skip < exec, "{text}");
        }
    }

    #[test]
    fn off_is_a_false_first_test() {
        for kind in [ShellKind::Zsh, ShellKind::Bash, ShellKind::Fish] {
            let on = render(kind, &block(true, &KNOWN, false), Path::new(FAC)).expect("on");
            let off = render(kind, &block(false, &KNOWN, false), Path::new(FAC)).expect("off");
            assert!(!on.contains("elif false") && !on.contains("else if false"));
            let first = match kind {
                ShellKind::Zsh => "elif false && [[ -o interactive",
                ShellKind::Bash => "elif false && case $- in",
                ShellKind::Fish => "else if false; and status is-interactive",
            };
            assert!(off.contains(first), "{off}");
        }
    }

    #[test]
    fn the_fish_test_names_the_ticked() {
        let (_, test) = ticked_test_fish(&[TermKind::Apple, TermKind::Zed], "");
        assert_eq!(test, "contains -- \"$terminal\" terminal zed");
        let (_, none) = ticked_test_fish(&[], "");
        assert_eq!(none, "contains -- \"$terminal\"");
    }
}
