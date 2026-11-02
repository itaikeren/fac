//! The wire format between fac and its shell hooks (`fac.zsh`, `fac.bash`,
//! `fac.fish`), both directions.
//!
//! Shell to fac: `ESC ] 779N ; 1:<token> ; <seq> ; <field> … BEL`, found and
//! split on `;` by the output scanner. Fields are percent-encoded (`%`, `;`,
//! every byte below 0x20 and 0x7f); UTF-8 passes raw. A list is its elements
//! encoded, each also with `,` as `%2C`, joined with `,`.
//!
//! # The session token
//!
//! Any program the shell runs can print an OSC 779x, so only the hook may
//! be believed: fac makes a [`Token`] per shell session (128 bits from the
//! OS RNG, as 32 lowercase hex digits) and gives it to the hook alone. zsh
//! and bash get it in `FAC_TOKEN`, which the hook (zsh: fac's `.zshenv`
//! shim; bash: the top of `fac.bash`, before the login files) copies into
//! an unexported shell variable and unsets before anything else runs. fish
//! runs its init command only after config.fish, so its programs would
//! inherit an env var: it gets `token <token>` as the first line on the
//! command pipe instead, read as `fac.fish` loads. Every 7790 to 7793 the
//! hook sends carries it after the version, in the version's field, as
//! `1:<token>` (the field's place keeps every other field where it was); a
//! hook that was given none sends the bare `1`, which fac drops the same.
//! fac drops a message whose token is missing or wrong before it decodes
//! anything of it ([`authenticate`]); what fac forwards to the completion
//! worker comes only from messages that passed. The token is never logged,
//! and it is on no screen: fac strips every 779x from the output.
//!
//! fac to shell: one line per command on the pipe at fd 63,
//! `apply <seq> <LBUFFER> <RBUFFER>\n`, both buffers percent-encoded (`%`,
//! `\`, space, every byte below 0x20 and 0x7f) so zsh can decode them with
//! `printf %b` and split the line on spaces. To bash and fish (a shell that
//! announced `apply_trigger`) the apply has a fifth field, `<LINE>`, encoded
//! the same way: the line fac expects the shell to hold, the buffer of the
//! report `<seq>`. zsh's reader takes exactly four fields, so zsh never gets
//! it; zsh's applies are as before. bash and fish also get `ta\n`, which
//! asks the hook to look at its input at its next prompt, and `looked
//! <seq> <mark> <bytes>\n`, fac's answer to the hook's `look` (7791 `<seq>`):
//! fac had written `mark` bytes to the shell, and `bytes` of its input
//! were unread (`shell::trigger`, "Typeahead"). fish gets `token
//! <token>\n` first (see below).
//!
//! fac to the completion worker (`worker.zsh`, and bash's and fish's):
//! lines on its input pipe, each field encoded like the apply buffers plus
//! `;` (`encode_word`); a list is its elements, each also with `,` as `%2C`,
//! joined with `,`. The worker's replies are fields encoded like 779x
//! fields plus space, read with `percent_decode`: `ready`, then per query
//! `c` (one candidate), `d` (done) or `err`, for every shell alike; fields
//! a shell has no answer for stay empty.
//!
//! # zsh
//!
//! 7790 `<seq> ; <CONTEXT> ; <CURSOR> ; <flags> ; <PREBUFFER> ; <BUFFER> [;
//! <undrawn>]`, from ZLE's hooks on every edit. 7791 context keys: `path`,
//! `fpath`, `histfile`, `compdump`, `aliases`, `functions`, `options`,
//! `cwd`, `env`, `zstyles`.
//!
//! # bash and fish
//!
//! Neither shell has a hook that runs on every edit. After a user key fac
//! injects the shell's private *trigger* sequence, which the shell binds to
//! a function that sends the report (bash: `bind -x` with
//! `READLINE_LINE`/`READLINE_POINT`; fish: `bind` with `commandline -b` and
//! `commandline -C`). One trigger at each 133;B (or at `armed`, when that
//! comes after it) gives the line's first report, except after a line typed
//! ahead (an Enter that reached the shell before its prompt): a trigger
//! then could land in what that line runs, so none goes until a look at
//! the shell's input settles it (`look`, `unread`). `shell::trigger` has
//! every rule of where a trigger may go.
//!
//! 7790, seven fields (no `undrawn`):
//! - `<CONTEXT>`: `start`; bash sends `cont` on its PS2 lines, with the
//!   lines before as `<PREBUFFER>`. fish edits a multi-line command as one
//!   buffer, so its `<PREBUFFER>` is empty.
//! - `<CURSOR>`: in chars, into `<BUFFER>`.
//! - `<flags>`: `v` while vi command mode is on (bash: the `vi-command`
//!   keymap; fish, with vi key bindings: `fish_bind_mode` is not
//!   `insert`), and `f` from the report sent as the line is accepted
//!   (fish: from `fish_preexec`, after its 133;C; it answers no trigger;
//!   bash has no hook there, and its 133;C ends the line). fish sends `i`
//!   while its own pager (its Tab completions, its history search) is
//!   open, so fac's popup stays hidden under it. No `r`. bash sends a
//!   report only from a binding fac's key ran (the trigger's, the apply
//!   trigger's, or at the end of a user's `bind -x` widget, which the hook
//!   adds), never on its own.
//!
//! 7791 keys of both shells, besides `path`, `cwd`, `env` and `histfile`:
//! - `trigger`: the bytes fac injects to ask for a report (bash
//!   `\e[9999~`, fish `U+10FFFD`), percent-encoded like any field.
//! - `apply_trigger`: the bytes that make the shell read an apply from
//!   fd 63. fac types them only once the apply is all written to the pipe.
//!   The binding reads every apply line waiting (with a timeout where the
//!   shell's `read` has one; fish's has none, so its end of the pipe is
//!   non-blocking), applies the one whose `<seq>` is its latest
//!   report and whose `<LINE>` is its line, drops the others as stale, and
//!   sends its 7790 report from the same binding, applied or not.
//! - `defer` (list): keys after which fac must not inject a trigger at
//!   once, from the live keymap. See "The defer list".
//! - `armed`: the shell's pid, sent at every prompt; it arms the trigger
//!   for that prompt.
//! - `look` (empty): at a prompt where the hook read a `ta`, before
//!   `armed` and before the line editor reads, the hook asks fac to look
//!   at its input, and waits for fac's `looked` on the pipe. Meanwhile the
//!   shell reads no input, so fac's count of the unread bytes, from its
//!   end of the PTY (`pty::input_queued`), is the count at the hook's
//!   look. bash's terminal is in cooked mode there, and fac takes it out
//!   for an instant to count, so bash waits with no time limit (`read`
//!   ends at the pipe's end if fac is gone, and a degraded fac closes the
//!   pipe): its line editor must not set the modes meanwhile. fish is out
//!   of cooked mode at its look, so fac counts without touching the modes,
//!   and fish polls its non-blocking pipe for about a second at most.
//! - `unread` (list): `<mark>,<bytes>`, the numbers of the `looked` that
//!   answered the hook's `look`, sent once the hook has it. Only this
//!   message settles anything: a `looked` the hook stopped waiting for is
//!   never sent back, so a count fac took after the hook went on is never
//!   used. With bytes unread, the hook looks again at its next prompt
//!   whether or not a `ta` asks (fac's `ta` may come after that prompt's
//!   drain of the pipe).
//! - `escape_delay`: how long, in ms, the shell waits after a lone ESC
//!   before it takes it as Escape (fish's `fish_escape_delay_ms`); 30 when
//!   not sent.
//!
//! ## The defer list
//!
//! Each element is one class letter, then the key's bytes (one key, or a
//! sequence of keys such as C-x C-e). Keys are in their legacy bytes (C-x
//! is `\x18`, Alt-e is `ESC e`); a key the terminal sends in kitty's
//! `CSI … u` or xterm's `CSI 27 ; … ~` form is matched by its legacy bytes.
//! - `p` (pending): no trigger right after the key; the key after it is
//!   judged on its own. Count keys (M-digit, vi counts), vi operators and
//!   pending keys (`f t F T r m` and `` ` ``), C-v, a lone ESC that the
//!   shell takes as a prefix, and bash's completion keys (Tab), whose
//!   second press lists only with no trigger in between.
//! - `i` (idle): the trigger goes out once no key has come for 150 ms, or
//!   at once before a popup key that waits for the report. The kill and
//!   yank keys (bash), which act differently right after a trigger.
//! - `n` (never): no trigger after the key until the shell reports or sends
//!   `armed` again. Keys that open a program (C-x C-e, fish's Alt-E and
//!   Alt-H), keys bound to user widgets that read the terminal (fzf), and
//!   searches that read keys of their own (bash's C-r, vi `/ ?`).
//! - `w` (wait): a prefix key the shell joins with the key after it
//!   however long the pause: bash's ESC where it has no binding of its own
//!   (emacs, vi-command), which readline waits on for good (its
//!   keyseq-timeout is only for a prefix that has one). The key after it
//!   is matched together with it even past the escape delay, which ends a
//!   lone ESC for every other prefix.
//! - `a` (accept): the key accepts the line, as Enter does: no trigger
//!   after it, and none until the next `armed`. Typed while the line
//!   editor may not be reading, it is an Enter typed ahead ("Typeahead").
//!   bash: keys bound to `accept-line` and the other functions that accept
//!   the line (`operate-and-get-next`, `insert-comment`), and macros that
//!   end a line; fish: keys bound to `execute` (also from `commandline -f`)
//!   or to fish's functions that run it. Enter, C-j and C-o fac knows as
//!   accept keys itself.
//! - `P`, `I`, `N`, `W`, `A`: the same, only in vi command mode (the
//!   latest report has `v`).
//! - `m` (mode): the key enters vi command mode (bash's ESC in vi-insert),
//!   so the keys after it are judged as in vi command mode until a report
//!   says otherwise; the trigger may follow it. Alt with a character (ESC
//!   and a character in one key) is the key and then the character, in vi
//!   command mode, unless the whole key is an element of its own; ESC
//!   with a CSI or an SS3 (an arrow) is not the mode key. Only outside vi
//!   command mode; `M` is ignored.
//!
//! Every proper prefix of an element (C-x of C-x C-e) is a prefix key: no
//! trigger right after it, and the key after it is matched together with
//! it. An element with another letter, or with no key, is ignored.
//!
//! fish's keys: `fish_function_path` and `fish_complete_path` (lists), for
//! its worker's `complete -C`. bash's keys: `bash_completion`, the source
//! file of the bash-completion loader (`_comp_load`), and `complete`
//! (list), the `complete -p` lines of the user's own specs.
//!
//! [`HookKey::decode`] reads the trigger keys.
//!
//! fish 4.3+ sends OSC 133 A/B/C/D itself, ST-terminated and with
//! parameters (`A;click_events=1`, `C;cmdline_url=<the command line>`); the
//! scanner takes them as fac's own marks and keeps nothing of the
//! parameters, so the command line in `cmdline_url` is never logged.

use std::fmt;
use std::time::Duration;

/// The protocol version in every message.
const VERSION: &[u8] = b"1";

/// The length of a [`Token`] in hex digits.
const TOKEN_LEN: usize = 32;

/// The session token (see "The session token"): only messages that carry
/// it are the hook's. Its `Debug` shows nothing of it.
#[derive(Clone, PartialEq, Eq)]
pub struct Token([u8; TOKEN_LEN]);

impl Token {
    /// A new token: 128 bits from the OS RNG.
    pub fn generate() -> Result<Token, getrandom::Error> {
        let mut random = [0u8; TOKEN_LEN / 2];
        getrandom::fill(&mut random)?;
        Ok(Token::from_random(random))
    }

    fn from_random(random: [u8; TOKEN_LEN / 2]) -> Token {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut hex = [0u8; TOKEN_LEN];
        for (i, byte) in random.iter().enumerate() {
            hex[2 * i] = HEX[usize::from(byte >> 4)];
            hex[2 * i + 1] = HEX[usize::from(byte & 15)];
        }
        Token(hex)
    }

    /// The hex digits, as the hook sends them.
    pub fn as_str(&self) -> &str {
        // Only ASCII hex digits are ever stored.
        std::str::from_utf8(&self.0).unwrap_or_default()
    }

    /// A fixed token, for tests.
    #[cfg(test)]
    pub(crate) fn for_tests() -> Token {
        Token::from_random([0x5a; TOKEN_LEN / 2])
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(..)")
    }
}

/// Whether a private OSC's `fields` (as the scanner split them) carry
/// `token`: the first field must be exactly `1:<token>`. If so, it becomes
/// the bare version, so [`decode`] reads the message as it always has.
/// `None` (no token for this session) passes nothing. The comparison takes
/// the same time wherever the first difference is.
pub fn authenticate(fields: &mut [Vec<u8>], token: Option<&Token>) -> bool {
    let (Some(token), Some(first)) = (token, fields.first_mut()) else {
        return false;
    };
    let Some(sent) = first
        .strip_prefix(VERSION)
        .and_then(|rest| rest.strip_prefix(b":"))
    else {
        return false;
    };
    if sent.len() != TOKEN_LEN {
        return false;
    }
    let differ = sent
        .iter()
        .zip(token.0.iter())
        .fold(0u8, |acc, (a, b)| acc | (a ^ b));
    if differ != 0 {
        return false;
    }
    first.truncate(VERSION.len());
    true
}

pub const BUFFER: u16 = 7790;
pub const CONTEXT: u16 = 7791;
pub const COMMAND: u16 = 7792;
pub const DIAGNOSTIC: u16 = 7793;

/// Context keys whose value is a list.
const LIST_KEYS: &[&str] = &[
    "path",
    "fpath",
    "options",
    "functions",
    "env",
    "defer",
    "fish_function_path",
    "fish_complete_path",
    "complete",
    "unread",
];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProtocolError {
    #[error("unknown message code {0}")]
    UnknownCode(u16),
    #[error("protocol version {0:?}, expected \"1\"")]
    Version(String),
    #[error("message {code} has {got} fields, expected {expected}")]
    FieldCount {
        code: u16,
        expected: usize,
        got: usize,
    },
    #[error("bad percent escape at byte {0}")]
    BadEscape(usize),
    #[error("bad number {0:?}")]
    BadNumber(String),
    #[error("context key {0} has the wrong kind of value")]
    BadValue(String),
}

/// What the shell's hook reported about the command line (7790): zsh's
/// from ZLE's hooks on every edit, bash's and fish's from the bindings of
/// their trigger keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BufferReport {
    pub seq: u64,
    /// Where the line is edited: zsh's `$CONTEXT` (`start`, `cont`,
    /// `select` or `vared`); bash's `start` or `cont` (a PS2 line); fish's
    /// `start`.
    pub context: String,
    /// The cursor, in chars into `buffer`.
    pub cursor: usize,
    pub flags: ReportFlags,
    pub prebuffer: String,
    pub buffer: String,
    /// Sorted indices (in chars, into `buffer`) of the characters left of
    /// the cursor that ZLE does not draw as themselves; empty when the
    /// shell did not say (an all-ASCII buffer, or an older `fac.zsh`).
    pub undrawn: Vec<usize>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReportFlags {
    /// `r`: a region is active.
    pub region: bool,
    /// `i`: an incremental history search is active (zsh), or fish's own
    /// pager is open.
    pub isearch: bool,
    /// `v`: vi command mode is on (zsh's vicmd keymap, bash's vi-command
    /// keymap, fish's bind mode other than insert).
    pub vicmd: bool,
    /// `f`: sent as the line is accepted (zsh's `zle-line-finish`, fish's
    /// `fish_preexec`); the line is done.
    pub finished: bool,
}

impl ReportFlags {
    /// Unknown letters are ignored, so the shell side can add flags.
    fn parse(letters: &[u8]) -> Self {
        let mut flags = Self::default();
        for letter in letters {
            match letter {
                b'r' => flags.region = true,
                b'i' => flags.isearch = true,
                b'v' => flags.vicmd = true,
                b'f' => flags.finished = true,
                _ => {}
            }
        }
        flags
    }
}

impl fmt::Display for ReportFlags {
    /// The letters as the shell sends them.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (on, letter) in [
            (self.region, "r"),
            (self.isearch, "i"),
            (self.vicmd, "v"),
            (self.finished, "f"),
        ] {
            if on {
                f.write_str(letter)?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextValue {
    Text(String),
    /// `path`, `fpath`, `options`, `functions`, `env` (`NAME=value`);
    /// bash's and fish's `defer`, `fish_function_path`,
    /// `fish_complete_path` and `complete`.
    List(Vec<String>),
}

/// What a `defer` element says about its key (see "The defer list").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeferClass {
    /// `p`: no trigger right after it; the next key is judged on its own.
    Pending,
    /// `i`: the trigger goes out after 150 ms with no key.
    Idle,
    /// `n`: no trigger until the shell reports or re-arms.
    Never,
    /// `m`: the key enters vi command mode.
    ViCommand,
    /// `w`: a prefix the shell joins with the next key, however late.
    Wait,
    /// `a`: the key accepts the line, as Enter does.
    Accept,
}

/// One `defer` element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeferKey {
    pub class: DeferClass,
    /// Only in vi command mode (`P`, `I`, `N`, `W`, `A`).
    pub vi_command: bool,
    /// The key sequence, in legacy bytes; not empty.
    pub keys: Vec<u8>,
}

impl DeferKey {
    /// An element as the shell sent it; `None` for an unknown class letter
    /// or no key.
    pub fn parse(element: &[u8]) -> Option<DeferKey> {
        let (&letter, keys) = element.split_first()?;
        if keys.is_empty() {
            return None;
        }
        let class = match letter.to_ascii_lowercase() {
            b'p' => DeferClass::Pending,
            b'i' => DeferClass::Idle,
            b'n' => DeferClass::Never,
            b'w' => DeferClass::Wait,
            b'a' => DeferClass::Accept,
            b'm' if letter == b'm' => DeferClass::ViCommand,
            _ => return None,
        };
        Some(DeferKey {
            class,
            vi_command: letter.is_ascii_uppercase(),
            keys: keys.to_vec(),
        })
    }
}

/// A 7791 key that drives the report trigger (bash and fish), decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookKey {
    /// `trigger`: the bytes fac injects after a key to ask for a report.
    Trigger(Vec<u8>),
    /// `apply_trigger`: the bytes that make the shell read an apply.
    ApplyTrigger(Vec<u8>),
    /// `defer`: keys after which no trigger goes out at once; elements that
    /// do not parse are left out.
    Defer(Vec<DeferKey>),
    /// `armed`: the shell's pid.
    Armed(u32),
    /// `look`: the hook asks fac to look at its input, and waits for the
    /// answer on the command pipe.
    Look,
    /// `unread`: fac's answer to the hook's `look`, as the hook got it:
    /// `bytes` of the shell's input were unread when fac had written
    /// `mark` bytes to it.
    Unread { mark: u64, bytes: u64 },
    /// `escape_delay`: how long the shell waits after a lone ESC.
    EscapeDelay(Duration),
}

impl HookKey {
    /// The typed value of `key`; `None` when `key` is not a trigger key.
    pub fn decode(key: &str, value: &ContextValue) -> Option<Result<HookKey, ProtocolError>> {
        let text = || match value {
            ContextValue::Text(text) => Ok(text.as_str()),
            ContextValue::List(_) => Err(ProtocolError::BadValue(key.to_owned())),
        };
        let bytes = || text().map(|text| text.as_bytes().to_vec());
        let decoded = match key {
            "trigger" => bytes().map(HookKey::Trigger),
            "apply_trigger" => bytes().map(HookKey::ApplyTrigger),
            "defer" => match value {
                ContextValue::List(items) => Ok(HookKey::Defer(
                    items
                        .iter()
                        .filter_map(|item| DeferKey::parse(item.as_bytes()))
                        .collect(),
                )),
                ContextValue::Text(_) => Err(ProtocolError::BadValue(key.to_owned())),
            },
            "armed" => text().and_then(|text| {
                u32::try_from(number(text.as_bytes())?)
                    .map(HookKey::Armed)
                    .map_err(|_| ProtocolError::BadNumber(text.to_owned()))
            }),
            "look" => Ok(HookKey::Look),
            "unread" => match value {
                ContextValue::List(items) => match &items[..] {
                    [mark, bytes] => number(mark.as_bytes()).and_then(|mark| {
                        number(bytes.as_bytes()).map(|bytes| HookKey::Unread { mark, bytes })
                    }),
                    _ => Err(ProtocolError::BadValue(key.to_owned())),
                },
                ContextValue::Text(_) => Err(ProtocolError::BadValue(key.to_owned())),
            },
            "escape_delay" => text().and_then(|text| {
                number(text.as_bytes()).map(|ms| HookKey::EscapeDelay(Duration::from_millis(ms)))
            }),
            _ => return None,
        };
        Some(decoded)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellMessage {
    /// 7790.
    Buffer(BufferReport),
    /// 7791: one context key.
    Context {
        seq: u64,
        key: String,
        value: ContextValue,
    },
    /// 7792: a command about to run, from preexec.
    Command { seq: u64, line: String },
    /// 7793: something went wrong on the shell side.
    Diagnostic { seq: u64, code: String },
}

/// Decode one private OSC from the scanner: `code` is 7790..=7793, `fields`
/// everything after it, split on `;` and still percent-encoded.
pub fn decode(code: u16, fields: &[Vec<u8>]) -> Result<ShellMessage, ProtocolError> {
    let expected = match code {
        BUFFER if fields.len() == 8 => 8,
        BUFFER => 7,
        CONTEXT => 4,
        COMMAND | DIAGNOSTIC => 3,
        _ => return Err(ProtocolError::UnknownCode(code)),
    };
    // The version is checked first: another version may have other fields.
    if let Some(version) = fields.first()
        && version != VERSION
    {
        return Err(ProtocolError::Version(
            String::from_utf8_lossy(version).into_owned(),
        ));
    }
    if fields.len() != expected {
        return Err(ProtocolError::FieldCount {
            code,
            expected,
            got: fields.len(),
        });
    }
    let seq = number(&fields[1])?;
    let message = match code {
        BUFFER => {
            let buffer = text(&fields[6])?;
            let chars = buffer.chars().count();
            ShellMessage::Buffer(BufferReport {
                seq,
                context: text(&fields[2])?,
                cursor: usize::try_from(number(&fields[3])?)
                    .map_err(|_| ProtocolError::BadNumber(lossy(&fields[3])))?,
                flags: ReportFlags::parse(&fields[4]),
                prebuffer: text(&fields[5])?,
                undrawn: fields
                    .get(7)
                    .map_or(Ok(Vec::new()), |field| runs(field, chars))?,
                buffer,
            })
        }
        CONTEXT => {
            let key = text(&fields[2])?;
            let value = if LIST_KEYS.contains(&key.as_str()) {
                ContextValue::List(list(&fields[3])?)
            } else {
                ContextValue::Text(text(&fields[3])?)
            };
            ShellMessage::Context { seq, key, value }
        }
        COMMAND => ShellMessage::Command {
            seq,
            line: text(&fields[2])?,
        },
        _ => ShellMessage::Diagnostic {
            seq,
            code: text(&fields[2])?,
        },
    };
    Ok(message)
}

fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn number(field: &[u8]) -> Result<u64, ProtocolError> {
    if field.is_empty() || !field.iter().all(u8::is_ascii_digit) {
        return Err(ProtocolError::BadNumber(lossy(field)));
    }
    std::str::from_utf8(field)
        .ok()
        .and_then(|digits| digits.parse().ok())
        .ok_or_else(|| ProtocolError::BadNumber(lossy(field)))
}

/// The most characters one run in an `undrawn` field may span.
const MAX_RUN: usize = 1 << 16;

/// A field of char indices: numbers and `a-b` runs joined with `,`, into a
/// buffer of `chars` characters. Every index must be in the buffer, and the
/// runs together may not span more characters than it has, so a field read
/// from any output never expands past the buffer it came with.
fn runs(field: &[u8], chars: usize) -> Result<Vec<usize>, ProtocolError> {
    let bad = || ProtocolError::BadNumber(lossy(field));
    let mut out = Vec::new();
    let mut spanned = 0usize;
    for run in field
        .split(|&byte| byte == b',')
        .filter(|run| !run.is_empty())
    {
        let (from, to) = match run.iter().position(|&byte| byte == b'-') {
            Some(dash) => (&run[..dash], &run[dash + 1..]),
            None => (run, run),
        };
        let (from, to) = (index(from).ok_or_else(bad)?, index(to).ok_or_else(bad)?);
        if to < from || to - from >= MAX_RUN || to >= chars {
            return Err(bad());
        }
        spanned += to - from + 1;
        if spanned > chars {
            return Err(bad());
        }
        out.extend(from..=to);
    }
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

fn index(digits: &[u8]) -> Option<usize> {
    std::str::from_utf8(digits).ok()?.parse().ok()
}

/// A percent-decoded field as a string; invalid UTF-8 is replaced.
fn text(field: &[u8]) -> Result<String, ProtocolError> {
    let bytes = percent_decode(field)?;
    Ok(match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(err) => {
            tracing::debug!("field is not UTF-8; decoded lossily");
            String::from_utf8_lossy(err.as_bytes()).into_owned()
        }
    })
}

/// A list field. The empty field is the empty list.
fn list(field: &[u8]) -> Result<Vec<String>, ProtocolError> {
    if field.is_empty() {
        return Ok(Vec::new());
    }
    field.split(|&byte| byte == b',').map(text).collect()
}

/// Strict percent-decoding: every `%` must start a two-hex-digit escape.
pub fn percent_decode(bytes: &[u8]) -> Result<Vec<u8>, ProtocolError> {
    let hex = |byte: u8| char::from(byte).to_digit(16);
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let (Some(high), Some(low)) = (
                bytes.get(i + 1).copied().and_then(hex),
                bytes.get(i + 2).copied().and_then(hex),
            ) else {
                return Err(ProtocolError::BadEscape(i));
            };
            // Two hex digits make one byte.
            decoded.push((high * 16 + low) as u8);
            i += 3;
        } else {
            decoded.push(bytes[i]);
            i += 1;
        }
    }
    Ok(decoded)
}

fn percent_encode(bytes: &[u8], extra: &[u8]) -> Vec<u8> {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = Vec::with_capacity(bytes.len());
    for &byte in bytes {
        if byte == b'%' || byte < 0x20 || byte == 0x7f || extra.contains(&byte) {
            encoded.extend_from_slice(&[
                b'%',
                HEX[usize::from(byte >> 4)],
                HEX[usize::from(byte & 15)],
            ]);
        } else {
            encoded.push(byte);
        }
    }
    encoded
}

/// `apply <seq> <L> <R>\n`: set the command line to `lbuffer` + `rbuffer`
/// with the cursor between them, if `seq` is still the latest report.
/// `expected`: for bash and fish, the line the shell must still hold (the
/// buffer of report `seq`), as a fifth field; `None` for zsh, whose apply
/// stays as it was.
pub fn encode_apply(seq: u64, lbuffer: &str, rbuffer: &str, expected: Option<&str>) -> Vec<u8> {
    let mut line = format!("apply {seq} ").into_bytes();
    line.extend_from_slice(&percent_encode(lbuffer.as_bytes(), b"\\ "));
    line.push(b' ');
    line.extend_from_slice(&percent_encode(rbuffer.as_bytes(), b"\\ "));
    if let Some(expected) = expected {
        line.push(b' ');
        line.extend_from_slice(&percent_encode(expected.as_bytes(), b"\\ "));
    }
    line.push(b'\n');
    line
}

/// Encode a fac-to-worker field: `%`, `\`, `;`, space, every byte below
/// 0x20 and 0x7f, so `worker.zsh` can split its line on spaces and decode
/// the field with `printf %b`.
pub fn encode_word(bytes: &[u8]) -> Vec<u8> {
    percent_encode(bytes, b"\\ ;")
}

/// Encode a fac-to-worker list field: `encode_word` of each element, also
/// with `,` as `%2C`, joined with `,`.
pub fn encode_word_list<T: AsRef<[u8]>>(items: &[T]) -> Vec<u8> {
    let encoded: Vec<Vec<u8>> = items
        .iter()
        .map(|item| percent_encode(item.as_ref(), b"\\ ;,"))
        .collect();
    encoded.join(&b","[..])
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    /// Encode a shell-to-fac field, as `fac.zsh` does.
    fn encode_field(bytes: &[u8]) -> Vec<u8> {
        percent_encode(bytes, b";")
    }

    /// Encode a shell-to-fac list field, as `fac.zsh` does.
    fn encode_list<T: AsRef<[u8]>>(items: &[T]) -> Vec<u8> {
        let encoded: Vec<Vec<u8>> = items
            .iter()
            .map(|item| percent_encode(item.as_ref(), b";,"))
            .collect();
        encoded.join(&b","[..])
    }

    fn fields(parts: &[&str]) -> Vec<Vec<u8>> {
        parts.iter().map(|part| part.as_bytes().to_vec()).collect()
    }

    /// A message passes only with exactly `1:<the session's token>` first,
    /// which then reads as before; any other first field is dropped.
    #[test]
    fn only_messages_with_the_sessions_token_pass() {
        let token = Token::for_tests();
        let other = Token::from_random([0xa5; TOKEN_LEN / 2]);
        let with = |first: &[u8]| {
            let mut parts = fields(&["", "4", "cwd", "/tmp"]);
            parts[0] = first.to_vec();
            parts
        };
        let good = format!("1:{}", token.as_str());
        let mut message = with(good.as_bytes());
        assert!(authenticate(&mut message, Some(&token)));
        assert_eq!(
            decode(CONTEXT, &message),
            Ok(ShellMessage::Context {
                seq: 4,
                key: "cwd".into(),
                value: ContextValue::Text("/tmp".into()),
            })
        );
        let wrong = format!("1:{}", other.as_str());
        let short = &good[..good.len() - 1];
        let long = format!("{good}0");
        let upper = good.to_uppercase();
        let version_2 = format!("2:{}", token.as_str());
        for first in [
            "1",
            "1:",
            wrong.as_str(),
            short,
            long.as_str(),
            upper.as_str(),
            version_2.as_str(),
            token.as_str(),
            "",
        ] {
            let mut message = with(first.as_bytes());
            assert!(!authenticate(&mut message, Some(&token)), "{first:?}");
            assert_eq!(message[0], first.as_bytes(), "left as it was");
        }
        // No fields, or no token for the session: nothing passes.
        assert!(!authenticate(&mut [], Some(&token)));
        assert!(!authenticate(&mut with(good.as_bytes()), None));
    }

    #[test]
    fn tokens_are_32_hex_digits_and_differ() {
        let one = Token::generate().expect("a token");
        let two = Token::generate().expect("a token");
        assert_ne!(one, two);
        for token in [&one, &two] {
            assert_eq!(token.as_str().len(), 32);
            assert!(
                token
                    .as_str()
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            );
        }
        assert_eq!(format!("{one:?}"), "Token(..)");
    }

    #[test]
    fn buffer_report() {
        let message = decode(
            BUFFER,
            &fields(&["1", "12", "cont", "3", "rf", "echo \"a%0A", "a%3Bb %25"]),
        );
        assert_eq!(
            message,
            Ok(ShellMessage::Buffer(BufferReport {
                seq: 12,
                context: "cont".into(),
                cursor: 3,
                flags: ReportFlags {
                    region: true,
                    finished: true,
                    ..ReportFlags::default()
                },
                prebuffer: "echo \"a\n".into(),
                buffer: "a;b %".into(),
                undrawn: Vec::new(),
            }))
        );
    }

    #[test]
    fn buffer_report_with_undrawn_characters() {
        let parse = |undrawn: &str| match decode(
            BUFFER,
            &fields(&[
                "1",
                "12",
                "start",
                "12",
                "",
                "",
                "caf\u{e9} au lait",
                undrawn,
            ]),
        ) {
            Ok(ShellMessage::Buffer(report)) => Ok(report.undrawn),
            other => Err(other),
        };
        assert_eq!(parse("3"), Ok(vec![3]));
        assert_eq!(parse("1-3,7,9-10"), Ok(vec![1, 2, 3, 7, 9, 10]));
        assert_eq!(parse(""), Ok(vec![]));
        assert_eq!(parse("11"), Ok(vec![11]));
        assert_eq!(parse("0-11"), Ok((0..12).collect()));
        for bad in ["x", "3-", "-3", "5-2", "1-99999", "1,,x"] {
            assert!(matches!(parse(bad), Err(Err(_))), "{bad:?}");
        }
        // A report without the field, as an older fac.zsh sends it.
        let Ok(ShellMessage::Buffer(report)) =
            decode(BUFFER, &fields(&["1", "12", "start", "3", "", "", "abc"]))
        else {
            panic!("an old report");
        };
        assert!(report.undrawn.is_empty());
    }

    /// A 7790 report of `buffer` with the undrawn field `undrawn`.
    fn undrawn_of(buffer: &str, undrawn: &[u8]) -> Result<Vec<usize>, ProtocolError> {
        let mut parts = fields(&["1", "12", "start", "0", "", "", buffer]);
        parts.push(undrawn.to_vec());
        match decode(BUFFER, &parts)? {
            ShellMessage::Buffer(report) => Ok(report.undrawn),
            other => panic!("not a buffer report: {other:?}"),
        }
    }

    #[test]
    fn undrawn_indices_must_be_in_the_buffer() {
        assert_eq!(undrawn_of("caf\u{e9}", b"3"), Ok(vec![3]));
        for bad in ["4", "2-4", "0,9", "65535"] {
            assert!(
                matches!(
                    undrawn_of("caf\u{e9}", bad.as_bytes()),
                    Err(ProtocolError::BadNumber(_))
                ),
                "{bad:?}"
            );
        }
        assert!(matches!(
            undrawn_of("", b"0"),
            Err(ProtocolError::BadNumber(_))
        ));
    }

    #[test]
    fn undrawn_runs_never_span_more_than_the_buffer() {
        // Overlapping runs, each in range, may not add up past the buffer.
        assert_eq!(undrawn_of("abcd", b"0-1,2-3"), Ok(vec![0, 1, 2, 3]));
        assert!(matches!(
            undrawn_of("abcd", b"0-3,0"),
            Err(ProtocolError::BadNumber(_))
        ));
        // A crafted report as big as the scanner lets through, with the
        // widest run repeated over a buffer that has that many characters:
        // rejected after one run instead of expanding to billions of entries.
        let buffer = "x".repeat(1 << 16);
        let field = b"0-65535,".repeat((1 << 20) / 8);
        let started = std::time::Instant::now();
        assert!(matches!(
            undrawn_of(&buffer, &field),
            Err(ProtocolError::BadNumber(_))
        ));
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        // And over a one-character buffer, rejected at the first index.
        let started = std::time::Instant::now();
        assert!(matches!(
            undrawn_of("x", &field),
            Err(ProtocolError::BadNumber(_))
        ));
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }

    #[test]
    fn flags_display_and_unknown_letters() {
        let flags = ReportFlags::parse(b"vxifr");
        assert_eq!(flags.to_string(), "rivf");
        assert_eq!(ReportFlags::parse(b"").to_string(), "");
    }

    #[test]
    fn context_text_and_lists() {
        assert_eq!(
            decode(CONTEXT, &fields(&["1", "3", "path", "/usr/bin,/a%2Cb,"])),
            Ok(ShellMessage::Context {
                seq: 3,
                key: "path".into(),
                value: ContextValue::List(vec!["/usr/bin".into(), "/a,b".into(), String::new()]),
            })
        );
        assert_eq!(
            decode(CONTEXT, &fields(&["1", "4", "fpath", ""])),
            Ok(ShellMessage::Context {
                seq: 4,
                key: "fpath".into(),
                value: ContextValue::List(Vec::new()),
            })
        );
        assert_eq!(
            decode(
                CONTEXT,
                &fields(&["1", "5", "aliases", "alias a=b%0Aalias c=d,e"])
            ),
            Ok(ShellMessage::Context {
                seq: 5,
                key: "aliases".into(),
                value: ContextValue::Text("alias a=b\nalias c=d,e".into()),
            })
        );
        assert_eq!(
            decode(
                CONTEXT,
                &fields(&["1", "6", "functions", "faccmd,mk%2Cdir"])
            ),
            Ok(ShellMessage::Context {
                seq: 6,
                key: "functions".into(),
                value: ContextValue::List(vec!["faccmd".into(), "mk,dir".into()]),
            })
        );
    }

    #[test]
    fn bash_and_fish_reports() {
        // fish: one buffer, no prebuffer, no undrawn field; `v` in vi
        // command mode.
        assert_eq!(
            decode(
                BUFFER,
                &fields(&["1", "7", "start", "2", "v", "", "git ch%0Aecho"])
            ),
            Ok(ShellMessage::Buffer(BufferReport {
                seq: 7,
                context: "start".into(),
                cursor: 2,
                flags: ReportFlags {
                    vicmd: true,
                    ..ReportFlags::default()
                },
                prebuffer: String::new(),
                buffer: "git ch\necho".into(),
                undrawn: Vec::new(),
            }))
        );
        // bash: a PS2 line, the lines before it as the prebuffer.
        assert_eq!(
            decode(
                BUFFER,
                &fields(&["1", "8", "cont", "3", "f", "echo 'a%0A", "b' c"])
            ),
            Ok(ShellMessage::Buffer(BufferReport {
                seq: 8,
                context: "cont".into(),
                cursor: 3,
                flags: ReportFlags {
                    finished: true,
                    ..ReportFlags::default()
                },
                prebuffer: "echo 'a\n".into(),
                buffer: "b' c".into(),
                undrawn: Vec::new(),
            }))
        );
    }

    #[test]
    fn bash_and_fish_context_keys() {
        let context = |key: &str, field: &[u8]| match decode(
            CONTEXT,
            &[
                b"1".to_vec(),
                b"9".to_vec(),
                key.as_bytes().to_vec(),
                field.to_vec(),
            ],
        ) {
            Ok(ShellMessage::Context { value, .. }) => value,
            other => panic!("not a context message: {other:?}"),
        };
        let list =
            |items: &[&str]| ContextValue::List(items.iter().map(|&item| item.into()).collect());
        let text = |text: &str| ContextValue::Text(text.into());
        assert_eq!(
            context(
                "defer",
                &encode_list(&[b"\x18".as_slice(), b"\x1bx", b"a,b"])
            ),
            list(&["\x18", "\x1bx", "a,b"])
        );
        assert_eq!(
            context("fish_function_path", b"/a/functions,/b%2Cc"),
            list(&["/a/functions", "/b,c"])
        );
        assert_eq!(
            context("fish_complete_path", b"/a/completions"),
            list(&["/a/completions"])
        );
        assert_eq!(
            context(
                "complete",
                &encode_list(&[
                    "complete -F _aws aws",
                    "complete -o nospace -F __start_kubectl kubectl"
                ])
            ),
            list(&[
                "complete -F _aws aws",
                "complete -o nospace -F __start_kubectl kubectl"
            ])
        );
        assert_eq!(
            context(
                "bash_completion",
                b"/opt/homebrew/share/bash-completion/bash_completion"
            ),
            text("/opt/homebrew/share/bash-completion/bash_completion")
        );
        assert_eq!(
            context("trigger", &encode_field(b"\x1b[9999~")),
            text("\x1b[9999~")
        );
        assert_eq!(
            context("trigger", "\u{10fffd}".as_bytes()),
            text("\u{10fffd}")
        );
        assert_eq!(context("armed", b"4242"), text("4242"));
        assert_eq!(context("escape_delay", b"30"), text("30"));
    }

    #[test]
    fn hook_keys_decode() {
        let text = |text: &str| ContextValue::Text(text.into());
        let decode = |key: &str, value: &ContextValue| HookKey::decode(key, value);
        assert_eq!(
            decode("trigger", &text("\x1b[9999~")),
            Some(Ok(HookKey::Trigger(b"\x1b[9999~".to_vec())))
        );
        assert_eq!(
            decode("trigger", &text("\u{10fffd}")),
            Some(Ok(HookKey::Trigger("\u{10fffd}".as_bytes().to_vec())))
        );
        assert_eq!(
            decode("apply_trigger", &text("\x1b[9998~")),
            Some(Ok(HookKey::ApplyTrigger(b"\x1b[9998~".to_vec())))
        );
        let defer = |class, vi_command, keys: &[u8]| DeferKey {
            class,
            vi_command,
            keys: keys.to_vec(),
        };
        assert_eq!(
            decode(
                "defer",
                &ContextValue::List(vec![
                    "p\x18".into(),
                    "i\t".into(),
                    "n\x18\x05".into(),
                    "Pf".into(),
                    "N/".into(),
                    "I\x17".into(),
                    "m\x1b".into(),
                    "w\x1b".into(),
                    "W\x1b".into(),
                    "a\x18".into(),
                    "A\r".into(),
                    // Unknown letters, `M` and keyless elements are left
                    // out.
                    "x\x18".into(),
                    "M\x1b".into(),
                    "p".into(),
                    String::new(),
                ])
            ),
            Some(Ok(HookKey::Defer(vec![
                defer(DeferClass::Pending, false, b"\x18"),
                defer(DeferClass::Idle, false, b"\t"),
                defer(DeferClass::Never, false, b"\x18\x05"),
                defer(DeferClass::Pending, true, b"f"),
                defer(DeferClass::Never, true, b"/"),
                defer(DeferClass::Idle, true, b"\x17"),
                defer(DeferClass::ViCommand, false, b"\x1b"),
                defer(DeferClass::Wait, false, b"\x1b"),
                defer(DeferClass::Wait, true, b"\x1b"),
                defer(DeferClass::Accept, false, b"\x18"),
                defer(DeferClass::Accept, true, b"\r"),
            ])))
        );
        assert_eq!(
            decode("armed", &text("4242")),
            Some(Ok(HookKey::Armed(4242)))
        );
        assert_eq!(
            decode("escape_delay", &text("30")),
            Some(Ok(HookKey::EscapeDelay(Duration::from_millis(30))))
        );
        assert_eq!(decode("look", &text("")), Some(Ok(HookKey::Look)));
        let list = |items: &[&str]| ContextValue::List(items.iter().map(|&i| i.into()).collect());
        assert_eq!(
            decode("unread", &list(&["1700", "0"])),
            Some(Ok(HookKey::Unread {
                mark: 1700,
                bytes: 0
            }))
        );
        for bad in [&["1"][..], &["1", "2", "3"], &["x", "1"], &["1", ""]] {
            assert!(
                matches!(decode("unread", &list(bad)), Some(Err(_))),
                "{bad:?}"
            );
        }
        assert!(matches!(
            decode("unread", &text("1")),
            Some(Err(ProtocolError::BadValue(_)))
        ));
        for (key, bad) in [
            ("armed", "x"),
            ("armed", ""),
            ("armed", "-1"),
            ("armed", "99999999999"),
            ("escape_delay", "3.5"),
        ] {
            assert!(
                matches!(
                    decode(key, &text(bad)),
                    Some(Err(ProtocolError::BadNumber(_)))
                ),
                "{key}={bad:?}"
            );
        }
        assert!(matches!(
            decode("defer", &text("x")),
            Some(Err(ProtocolError::BadValue(_)))
        ));
        assert!(matches!(
            decode("trigger", &ContextValue::List(Vec::new())),
            Some(Err(ProtocolError::BadValue(_)))
        ));
        assert_eq!(decode("path", &text("/bin")), None);
        assert_eq!(decode("cwd", &text("/")), None);
    }

    #[test]
    fn env_is_a_list_of_assignments() {
        assert_eq!(
            decode(
                CONTEXT,
                &fields(&["1", "7", "env", "HOME=/Users/me,A=b%2Cc=d%3B%0Ae,EMPTY="])
            ),
            Ok(ShellMessage::Context {
                seq: 7,
                key: "env".into(),
                value: ContextValue::List(vec![
                    "HOME=/Users/me".into(),
                    "A=b,c=d;\ne".into(),
                    "EMPTY=".into()
                ]),
            })
        );
    }

    #[test]
    fn command_and_diagnostic() {
        assert_eq!(
            decode(COMMAND, &fields(&["1", "9", "ls %3B echo"])),
            Ok(ShellMessage::Command {
                seq: 9,
                line: "ls ; echo".into()
            })
        );
        assert_eq!(
            decode(DIAGNOSTIC, &fields(&["1", "10", "cmd_fd_closed"])),
            Ok(ShellMessage::Diagnostic {
                seq: 10,
                code: "cmd_fd_closed".into()
            })
        );
    }

    #[test]
    fn version_mismatch() {
        assert_eq!(
            decode(COMMAND, &fields(&["2", "9", "ls"])),
            Err(ProtocolError::Version("2".into()))
        );
        assert_eq!(
            decode(BUFFER, &fields(&["2", "1"])),
            Err(ProtocolError::Version("2".into()))
        );
    }

    #[test]
    fn wrong_field_counts() {
        assert_eq!(
            decode(COMMAND, &fields(&["1", "9", "ls", "extra"])),
            Err(ProtocolError::FieldCount {
                code: COMMAND,
                expected: 3,
                got: 4
            })
        );
        assert_eq!(
            decode(BUFFER, &fields(&["1", "5", "x"])),
            Err(ProtocolError::FieldCount {
                code: BUFFER,
                expected: 7,
                got: 3
            })
        );
        assert_eq!(
            decode(DIAGNOSTIC, &[]),
            Err(ProtocolError::FieldCount {
                code: DIAGNOSTIC,
                expected: 3,
                got: 0
            })
        );
    }

    #[test]
    fn bad_numbers_and_codes() {
        assert!(matches!(
            decode(COMMAND, &fields(&["1", "x", "ls"])),
            Err(ProtocolError::BadNumber(_))
        ));
        assert!(matches!(
            decode(BUFFER, &fields(&["1", "1", "start", "-1", "", "", ""])),
            Err(ProtocolError::BadNumber(_))
        ));
        assert_eq!(decode(7794, &[]), Err(ProtocolError::UnknownCode(7794)));
    }

    #[test]
    fn bad_percent_escapes() {
        for (input, at) in [("%", 0), ("a%2", 1), ("%zz", 0), ("ok%25%G1", 5)] {
            assert_eq!(
                percent_decode(input.as_bytes()),
                Err(ProtocolError::BadEscape(at)),
                "{input}"
            );
        }
        assert!(matches!(
            decode(COMMAND, &fields(&["1", "1", "100%"])),
            Err(ProtocolError::BadEscape(3))
        ));
    }

    #[test]
    fn invalid_utf8_is_lossy() {
        assert_eq!(
            decode(COMMAND, &fields(&["1", "1", "a%FFb"])),
            Ok(ShellMessage::Command {
                seq: 1,
                line: "a\u{fffd}b".into()
            })
        );
    }

    #[test]
    fn field_encoding() {
        assert_eq!(
            encode_field("a;b%\n\x1b\x7f✅".as_bytes()),
            "a%3Bb%25%0A%1B%7F✅".as_bytes()
        );
        assert_eq!(encode_list(&["a,b", "c;"]), b"a%2Cb,c%3B");
    }

    #[test]
    fn word_encoding() {
        assert_eq!(
            encode_word("a b;c\\d%\n✅".as_bytes()),
            "a%20b%3Bc%5Cd%25%0A✅".as_bytes()
        );
        assert_eq!(encode_word_list(&["/a b", "c,d", ""]), b"/a%20b,c%2Cd,");
    }

    #[test]
    fn apply_encoding() {
        assert_eq!(
            encode_apply(7, "echo ✅ 100%", "\\x\n", None),
            "apply 7 echo%20✅%20100%25 %5Cx%0A\n".as_bytes()
        );
        assert_eq!(encode_apply(0, "", "", None), b"apply 0  \n");
        // bash and fish: the line the shell must hold, as a fifth field.
        assert_eq!(
            encode_apply(7, "git checkout ", "", Some("git ch")),
            b"apply 7 git%20checkout%20  git%20ch\n"
        );
        assert_eq!(encode_apply(0, "", "", Some("")), b"apply 0   \n");
    }

    /// fac.zsh's reader takes an apply only with exactly four words: zsh
    /// must never get the fifth field, or every apply would be dropped.
    #[test]
    fn zsh_drops_an_apply_with_a_line() {
        let zsh = include_str!("../../shell/zsh/fac.zsh");
        assert!(zsh.contains("[[ $words[1] == apply && $#words == 4"));
        assert_eq!(
            zsh_decode_apply(&encode_apply(3, "a", "b", Some("x"))),
            None
        );
        assert!(zsh_decode_apply(&encode_apply(3, "a", "b", None)).is_some());
    }

    /// What `fac.zsh` does with an apply line: split on spaces keeping empty
    /// words, then `printf -v var '%b' "${word//\%/\\x}"`.
    fn zsh_decode_apply(line: &[u8]) -> Option<(u64, Vec<u8>, Vec<u8>)> {
        let line = line.strip_suffix(b"\n")?;
        // `read -r` stops at the first newline.
        if line.contains(&b'\n') {
            return None;
        }
        let words: Vec<&[u8]> = line.split(|&byte| byte == b' ').collect();
        let [b"apply", seq, left, right] = words[..] else {
            return None;
        };
        let seq = std::str::from_utf8(seq).ok()?.parse().ok()?;
        Some((
            seq,
            printf_b(&backslash_x(left)),
            printf_b(&backslash_x(right)),
        ))
    }

    /// `${word//\%/\\x}`
    fn backslash_x(word: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        for &byte in word {
            if byte == b'%' {
                out.extend_from_slice(b"\\x");
            } else {
                out.push(byte);
            }
        }
        out
    }

    /// zsh's `printf %b`: backslash escapes, `\x` with up to two hex digits,
    /// `\0` with up to three octal digits, `\c` ends the output.
    fn printf_b(input: &[u8]) -> Vec<u8> {
        let hex = |byte: u8| char::from(byte).to_digit(16);
        let mut out = Vec::new();
        let mut i = 0;
        while i < input.len() {
            if input[i] != b'\\' || i + 1 == input.len() {
                out.push(input[i]);
                i += 1;
                continue;
            }
            let escape = input[i + 1];
            i += 2;
            match escape {
                b'x' => {
                    let mut value = 0u32;
                    let mut digits = 0;
                    while digits < 2 && i < input.len() {
                        let Some(digit) = hex(input[i]) else { break };
                        value = value * 16 + digit;
                        digits += 1;
                        i += 1;
                    }
                    if digits == 0 {
                        out.extend_from_slice(b"\\x");
                    } else {
                        out.push(value as u8);
                    }
                }
                b'0' => {
                    let mut value = 0u32;
                    let mut digits = 0;
                    while digits < 3 && i < input.len() && (b'0'..=b'7').contains(&input[i]) {
                        value = value * 8 + u32::from(input[i] - b'0');
                        digits += 1;
                        i += 1;
                    }
                    out.push(value as u8);
                }
                b'c' => return out,
                b'a' => out.push(0x07),
                b'b' => out.push(0x08),
                b'e' | b'E' => out.push(0x1b),
                b'f' => out.push(0x0c),
                b'n' => out.push(b'\n'),
                b'r' => out.push(b'\r'),
                b't' => out.push(b'\t'),
                b'v' => out.push(0x0b),
                b'\\' => out.push(b'\\'),
                other => out.extend_from_slice(&[b'\\', other]),
            }
        }
        out
    }

    #[test]
    fn printf_b_emulation_would_mangle_raw_backslashes() {
        assert_eq!(printf_b(b"a\\nb\\x41\\cdrop"), b"a\nbA");
    }

    proptest! {
        #[test]
        fn field_round_trip(s in any::<String>()) {
            let encoded = encode_field(s.as_bytes());
            prop_assert!(!encoded.iter().any(|&b| b == b';' || b < 0x20 || b == 0x7f));
            prop_assert_eq!(percent_decode(&encoded), Ok(s.clone().into_bytes()));
            let message = decode(COMMAND, &[b"1".to_vec(), b"1".to_vec(), encoded]);
            prop_assert_eq!(message, Ok(ShellMessage::Command { seq: 1, line: s }));
        }

        #[test]
        fn list_round_trip(items in prop::collection::vec(any::<String>(), 0..8)) {
            // The empty field is the empty list, so `[""]` reads back as `[]`.
            prop_assume!(items != [String::new()]);
            let encoded = encode_list(&items);
            prop_assert!(!encoded.iter().any(|&b| b == b';' || b < 0x20 || b == 0x7f));
            let message = decode(CONTEXT, &[b"1".to_vec(), b"2".to_vec(), b"path".to_vec(), encoded]);
            prop_assert_eq!(message, Ok(ShellMessage::Context {
                seq: 2,
                key: "path".into(),
                value: ContextValue::List(items),
            }));
        }

        #[test]
        fn word_round_trip(s in any::<String>()) {
            let encoded = encode_word(s.as_bytes());
            prop_assert!(!encoded.iter().any(|&b| b == b' ' || b == b';' || b < 0x20 || b == 0x7f));
            prop_assert_eq!(printf_b(&backslash_x(&encoded)), s.into_bytes());
        }

        #[test]
        fn word_list_round_trip(items in prop::collection::vec(any::<String>(), 1..8)) {
            // What worker.zsh does: split on `,`, then decode each element.
            let encoded = encode_word_list(&items);
            prop_assert!(!encoded.iter().any(|&b| b == b' ' || b == b';' || b < 0x20 || b == 0x7f));
            let decoded: Vec<Vec<u8>> = encoded
                .split(|&b| b == b',')
                .map(|item| printf_b(&backslash_x(item)))
                .collect();
            let expected: Vec<Vec<u8>> = items.into_iter().map(String::into_bytes).collect();
            prop_assert_eq!(decoded, expected);
        }

        #[test]
        fn apply_round_trip(seq in any::<u64>(), left in any::<String>(), right in any::<String>()) {
            let line = encode_apply(seq, &left, &right, None);
            let decoded = zsh_decode_apply(&line);
            prop_assert_eq!(decoded, Some((seq, left.into_bytes(), right.into_bytes())));
        }

        #[test]
        fn apply_with_a_line_round_trip(
            seq in any::<u64>(),
            left in any::<String>(),
            right in any::<String>(),
            expected in any::<String>(),
        ) {
            // What the bash and fish readers do: split on spaces keeping
            // empty words, then decode each.
            let line = encode_apply(seq, &left, &right, Some(&expected));
            let line = line.strip_suffix(b"\n").expect("a line");
            prop_assert!(!line.contains(&b'\n'));
            let words: Vec<&[u8]> = line.split(|&byte| byte == b' ').collect();
            prop_assert_eq!(words.len(), 5);
            let seq = seq.to_string();
            prop_assert_eq!(words[1], seq.as_bytes());
            let decoded: Vec<Vec<u8>> = words[2..]
                .iter()
                .map(|word| printf_b(&backslash_x(word)))
                .collect();
            prop_assert_eq!(
                decoded,
                vec![left.into_bytes(), right.into_bytes(), expected.into_bytes()]
            );
        }
    }
}
