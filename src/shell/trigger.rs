//! The report trigger. bash and fish have no hook that runs on every edit,
//! so after the user's keys fac types the shell's private trigger key, and
//! the shell's binding for it sends a 7790 report (`protocol`, "bash and
//! fish"). No trigger goes before a bash or fish hook announces its
//! trigger, though the accept keys typed meanwhile, while the shell
//! starts, are typed ahead (see "Typeahead"); zsh never announces one, and
//! for zsh nothing here runs at all.
//!
//! Per read of stdin, the trigger goes out once, after the last key it may
//! follow (`feed`, `end_read`), and only while the shell's line editor
//! reads keys: armed by `armed` for the shell's pid, between 133;B and
//! 133;C. It never follows an accept key, nor anything after one in that
//! read (that disarms it until the next `armed`); never goes inside a key,
//! a character or a bracketed paste; never follows a terminal's reply to
//! the shell's own query (fish asks OSC 11 and DA1 at every prompt), nor a
//! key that answers a question bash's line editor asks itself
//! (completion's `(y or n)`, its `--More--` pager); waits out the shell's
//! escape delay after a lone ESC, which is then Escape and no prefix (but
//! a `w` ESC stays one: bash's readline waits on it for good); and
//! keeps to the shell's `defer` list (idle keys get theirs after 150 ms
//! without a key, or before a popup key that waits for it). The line's
//! first trigger goes at 133;B or `armed`. No trigger goes at all while a
//! line typed ahead may still be queued for the shell (see "Typeahead").
//! Two missed triggers turn it off until the shell's next `armed`. Output
//! the shell writes while it answers is held (`holds_output`) so the
//! redraw its binding makes reaches the terminal in one write. An apply's
//! trigger goes once the command pipe has the whole apply.
//!
//! # Typeahead
//!
//! An accept key (Enter) that goes to the shell while its line editor may
//! not be reading (the gate closed: a command runs, the prompt is not
//! drawn yet, or a line typed ahead may be queued) may end a line that is
//! still queued when the next prompt comes. A trigger then, at the line's
//! start or after the user's first key, would land after that line, in
//! whatever it runs. So from such a key on, no trigger goes, for the rest
//! of the line and the lines after it, until fac knows the shell holds no
//! line typed ahead. No timer decides that, and no report (which needs a
//! trigger).
//!
//! Accept keys are fac's own (Enter, C-j, C-o, `keys::is_accept`) and
//! those the hook names (`a` in `defer`: a key bound to accept-line or to
//! fish's execute). While suspended at the prompt (a search, a widget),
//! the first accept key may end the line or only the search, and every one
//! after it, until the suspension ends, is typed ahead.
//!
//! fac counts the bytes it writes to the shell (`written`), and while such
//! a key is not settled, writes `ta` on the command pipe, which asks the
//! hook to look at its input at its next prompt. There, before its line
//! editor reads anything, the hook sends `look` and waits; fac counts the
//! bytes of the shell's input no one has read yet, from its end of the PTY
//! (`pty::input_queued`, which sees the line being typed too, not only
//! whole lines), and answers with the count and `written` as the *mark*.
//! The hook sends both back (`unread`), and that settles keys:
//! - with nothing unread, every accept key that ends at or before the
//!   mark has been read, by a program or by the shell's line editor;
//! - with some unread, the unread bytes are the last ones written, as long
//!   as the line discipline took none out: it deletes bytes only for the
//!   control characters `deletes` names (an erase, a kill, Ctrl-C; for
//!   other settings fac's count says only whether any are unread), and it
//!   drops what is past its room (MAX_INPUT) of one line, so only while
//!   fewer than `QUEUE_ROOM` bytes were written since the input was last
//!   empty (`empty_at`: a look with nothing unread, or a report of a
//!   trigger with nothing written after it). Then an accept key with no
//!   such control character after it, ending at or before `mark -
//!   unread`, has been read.
//!
//! The count is taken while the hook waits, when the shell reads nothing;
//! a count fac takes after the hook stopped waiting (fish's, after about a
//! second) never comes back as `unread`. bash's waits for good, since fac
//! changes its terminal's modes to count (`pty::input_queued`).
//!
//! bash's readline reads a byte at a time and no further than the line it
//! accepts, so a key it read is done with. fish reads ahead: at a prompt it
//! reads all its input until the reply to its query (DA1) and keeps what
//! it has not used in a buffer of its own, which no look at the input sees.
//! For fish, fac counts the lines it may hold (`held`): every accept key
//! that may have been read at a prompt (still unread at that prompt's look,
//! or with no look, or typed before fish has its query's reply) is one,
//! and every line it takes (133;C, or a new prompt with none: an empty
//! line) that was not ended at the open gate takes one off. The trigger
//! comes back once no accept key is unsettled and fish holds none. While
//! it is off, each prompt asks for the next look (`ta`), so it cannot stay
//! off for good.
use std::time::{Duration, Instant};

use crate::guards::ShellKind;
use crate::shell::protocol::{BufferReport, DeferClass, DeferKey, HookKey};
use crate::term::keys::{self, KeySplitter, Piece};

/// After an idle key (`i`), the trigger goes out once no key has come for
/// this long.
pub const IDLE: Duration = Duration::from_millis(150);
/// The shell's escape delay when it does not say (fish's default).
pub const ESCAPE_DELAY: Duration = Duration::from_millis(30);
/// The longest escape delay taken from a shell.
const MAX_ESCAPE_DELAY: Duration = Duration::from_secs(2);
/// A trigger with no 7790 this long after it is missed. A second missed
/// trigger, sent after the first was found missed and with no 7790 in
/// between, turns the trigger off for the line (a shell that stalled, a
/// hook that died): the next `armed` from the shell's pid turns it on
/// again. A nested or exec'd shell never sends one, so for it the trigger
/// stays off.
pub const MISS: Duration = Duration::from_millis(30);
/// The output hold ends once the report came and the output has been quiet
/// this long,
pub const QUIET: Duration = Duration::from_millis(1);
/// or this long after the first byte it held.
pub const HOLD_CAP: Duration = Duration::from_millis(30);
/// The trigger and the apply trigger each hook sends, the only ones taken.
const TRIGGERS: [(ShellKind, &[u8], &[u8]); 2] = [
    (ShellKind::Bash, b"\x1b[9999~", b"\x1b[9998~"),
    (
        ShellKind::Fish,
        "\u{10fffd}".as_bytes(),
        "\u{10fffc}".as_bytes(),
    ),
];
/// The longest key sequence matched against `defer`.
const MAX_SEQ: usize = 32;
/// The questions bash's line editor asks itself and reads the answer to:
/// completion's `Display all N possibilities? (y or n)` and its pager's
/// `--More--`, as the output ends with them.
const QUESTIONS: [&[u8]; 2] = [b"(y or n)", b"--More--"];
/// The end of the output kept to find a question split across reads.
const QUESTION_TAIL: usize = 8;
/// What XNU's line discipline holds of a line being typed (MAX_INPUT is
/// 1024; past it, it drops what comes). Fewer bytes written since the
/// shell's input was last empty, and nothing was dropped.
const QUEUE_ROOM: u64 = 1000;

/// The shell's `defer` list, with what is looked up per key worked out
/// once. Index 0 is every mode but vi command mode, 1 is vi command mode.
struct Defer {
    keys: Vec<DeferKey>,
    /// The first bytes of the elements of each mode: a key whose (legacy)
    /// first byte is not among them is matched no further.
    first: [[bool; 256]; 2],
    /// Whether an element of the mode starts with a printable byte, so that
    /// typed text is looked at character by character.
    printable: [bool; 2],
    /// The keys that enter vi command mode (`m`), kept out of `keys`.
    vi_command_keys: Vec<Vec<u8>>,
}

impl Default for Defer {
    fn default() -> Self {
        Defer::new(Vec::new())
    }
}

enum Match {
    None,
    /// A proper prefix of an element.
    Prefix,
    Exact(DeferClass),
}

impl Defer {
    fn new(keys: Vec<DeferKey>) -> Self {
        let (vi_command_keys, keys): (Vec<_>, Vec<_>) = keys
            .into_iter()
            .partition(|key| key.class == DeferClass::ViCommand);
        let mut defer = Defer {
            keys,
            first: [[false; 256]; 2],
            printable: [false; 2],
            vi_command_keys: vi_command_keys.into_iter().map(|key| key.keys).collect(),
        };
        for key in &defer.keys {
            let byte = key.keys[0];
            let printable = byte >= 0x20 && byte != 0x7f;
            for mode in [0, 1] {
                if mode == 1 || !key.vi_command {
                    defer.first[mode][usize::from(byte)] = true;
                    defer.printable[mode] |= printable;
                }
            }
        }
        defer
    }

    /// `seq` (legacy bytes) in mode `vi`. A prefix of a longer element wins
    /// over an element of its own: the key after it decides. But not over
    /// one that accepts the line: the shell runs a prefix's own binding
    /// when no sequence goes on from it, or after its key timeout, so the
    /// key may end the line.
    fn lookup(&self, seq: &[u8], vi: bool) -> Match {
        let mut exact = None;
        let mut prefix = false;
        for key in self.keys.iter().filter(|key| vi || !key.vi_command) {
            if key.keys.len() > seq.len() && key.keys.starts_with(seq) {
                prefix = true;
            }
            if key.keys == seq {
                exact = Some(key.class);
            }
        }
        match exact {
            Some(DeferClass::Accept) => Match::Exact(DeferClass::Accept),
            _ if prefix => Match::Prefix,
            Some(class) => Match::Exact(class),
            None => Match::None,
        }
    }

    /// When `key` (legacy bytes, outside vi command mode) enters vi command
    /// mode: what of it is left to judge in that mode (nothing for the mode
    /// key itself, the character of an Alt key). Not for a key that is an
    /// element of its own, nor for the mode key with a CSI or an SS3 after
    /// it (an arrow).
    fn vi_command_rest<'k>(&self, key: &'k [u8]) -> Option<&'k [u8]> {
        let mode_key = self
            .vi_command_keys
            .iter()
            .find(|mode_key| key.starts_with(mode_key))?;
        let rest = &key[mode_key.len()..];
        if matches!(rest.first(), Some(b'[' | b'O')) {
            return None;
        }
        if !rest.is_empty() && matches!(self.lookup(key, false), Match::Exact(_)) {
            return None;
        }
        Some(rest)
    }
}

/// What a key means for the trigger.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    /// The trigger may follow it.
    Safe,
    /// Not right after it (a prefix, a pending key); the next key decides.
    Wait,
    /// After 150 ms with no key.
    Idle,
    /// Not until the shell reports or re-arms.
    Never,
    /// It accepts the line: no trigger after it, disarmed.
    Accept,
    /// No key to the shell (a kitty key release).
    Ignore,
}

/// Judges keys against `defer`, joining a prefix key with the keys after it.
#[derive(Default)]
struct Keys {
    defer: Defer,
    /// The latest report has `v`.
    vi: bool,
    /// The prefix keys so far.
    seq: [u8; MAX_SEQ],
    seq_len: usize,
}

impl Keys {
    /// One whole key from the splitter.
    fn key(&mut self, key: &[u8]) -> Outcome {
        if keys::is_accept(key) {
            self.seq_len = 0;
            return Outcome::Accept;
        }
        let mut buf = [0u8; 8];
        match keys::legacy_bytes(key, &mut buf) {
            Some(legacy) => self.judge(legacy),
            None => Outcome::Ignore,
        }
    }

    /// One key in its legacy bytes.
    fn judge(&mut self, legacy: &[u8]) -> Outcome {
        if !self.vi
            && self.seq_len == 0
            && let Some(rest) = self.defer.vi_command_rest(legacy)
        {
            // From here on the shell reads keys in vi command mode.
            self.vi = true;
            if rest.is_empty() {
                return Outcome::Safe;
            }
            return self.judge(rest);
        }
        let mode = usize::from(self.vi);
        let Some(&first) = legacy.first() else {
            return Outcome::Safe;
        };
        if self.seq_len == 0 && !self.defer.first[mode][usize::from(first)] {
            return Outcome::Safe;
        }
        let len = self.seq_len + legacy.len();
        if len > MAX_SEQ {
            self.seq_len = 0;
            return Outcome::Safe;
        }
        let mut joined = [0u8; MAX_SEQ];
        joined[..self.seq_len].copy_from_slice(&self.seq[..self.seq_len]);
        joined[self.seq_len..len].copy_from_slice(legacy);
        self.seq_len = 0;
        match self.defer.lookup(&joined[..len], self.vi) {
            Match::None => Outcome::Safe,
            Match::Prefix => {
                self.seq[..len].copy_from_slice(&joined[..len]);
                self.seq_len = len;
                Outcome::Wait
            }
            Match::Exact(DeferClass::Pending) => Outcome::Wait,
            // A prefix, whatever comes after it.
            Match::Exact(DeferClass::Wait) => {
                self.seq[..len].copy_from_slice(&joined[..len]);
                self.seq_len = len;
                Outcome::Wait
            }
            Match::Exact(DeferClass::Idle) => Outcome::Idle,
            Match::Exact(DeferClass::Never) => Outcome::Never,
            Match::Exact(DeferClass::Accept) => Outcome::Accept,
            // Kept out of `keys`.
            Match::Exact(DeferClass::ViCommand) => Outcome::Safe,
        }
    }

    /// A key no key may join any more (a lone ESC past the escape delay:
    /// the shell has taken it as Escape): an element only when it is one
    /// itself, not the prefix of a longer one (Alt-x is `ESC x`). But a
    /// `w` key stays a prefix: the shell takes the next key with it.
    fn key_alone(&mut self, key: &[u8]) -> Outcome {
        match self.key(key) {
            Outcome::Wait if self.seq_len > 0 => {
                let len = std::mem::take(&mut self.seq_len);
                let seq = self.seq;
                let class = self
                    .defer
                    .keys
                    .iter()
                    .filter(|element| self.vi || !element.vi_command)
                    .rfind(|element| element.keys == seq[..len])
                    .map(|element| element.class);
                match class {
                    None => Outcome::Safe,
                    Some(DeferClass::Wait) => {
                        self.seq_len = len;
                        Outcome::Wait
                    }
                    Some(DeferClass::Pending) => Outcome::Wait,
                    Some(DeferClass::Idle) => Outcome::Idle,
                    Some(DeferClass::Never) => Outcome::Never,
                    Some(DeferClass::Accept) => Outcome::Accept,
                    // Kept out of `keys`.
                    Some(DeferClass::ViCommand) => Outcome::Safe,
                }
            }
            other => other,
        }
    }

    /// Whether typed text must be looked at character by character.
    fn text_matters(&self) -> bool {
        self.seq_len > 0 || self.defer.printable[usize::from(self.vi)]
    }
}

/// When a trigger may go out.
#[derive(Default)]
struct Gate {
    /// A bash or fish shell (never zsh).
    shell: bool,
    /// The shell's trigger bytes, once announced.
    trigger: Option<Vec<u8>>,
    off: bool,
    armed: bool,
    /// Between 133;B and 133;C.
    prompt: bool,
    /// After a `never` key, until a report or `armed`.
    suspended: bool,
    alt_screen: bool,
    /// bash's line editor asks a question of its own (`QUESTIONS`): keys
    /// answer it, and no trigger goes.
    question: bool,
    /// An accept key went to the shell while the gate was closed (typed
    /// ahead, or typed into a program), and it is not settled yet: a line
    /// may still be queued (see "Typeahead").
    typeahead: bool,
    /// While suspended at the prompt, an accept key came: it may have
    /// ended the line (an Enter in bash's search) or only the search or
    /// widget, so every accept key after it is typed ahead, until the
    /// suspension ends.
    accepted_in_suspension: bool,
}

impl Gate {
    fn open(&self) -> bool {
        self.open_but_for_suspension() && !self.suspended
    }

    fn open_but_for_suspension(&self) -> bool {
        self.trigger.is_some()
            && !self.off
            && self.armed
            && self.prompt
            && !self.alt_screen
            && !self.typeahead
    }
}

/// How the keys of the read being fed ended.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Last {
    #[default]
    Safe,
    Wait,
    Idle,
    /// Inside a key or a character.
    Cut,
    Paste,
    /// After an accept or a `never` key.
    Stop,
}

/// The read being fed.
#[derive(Default)]
struct Read {
    /// User bytes went to the shell in it.
    fed: bool,
    /// Where in `outbound` the trigger goes: after the last key it may follow.
    safe_at: Option<usize>,
    /// An accept or `never` key came: nothing later in the read counts.
    stop: bool,
    /// A `never` key came.
    suspended: bool,
    /// The accept keys that came with the gate closed.
    typeahead: u32,
    /// An accept key came with the gate open: it ends the line.
    accepted: bool,
    last: Last,
}

impl Read {
    fn note(&mut self, gate: &mut Gate, outcome: Outcome, end: usize) {
        // A key that answers the shell's own question (an Enter goes on a
        // line in bash's pager): the trigger waits until the line is back.
        if gate.question && !gate.suspended {
            if outcome != Outcome::Ignore {
                self.last = Last::Idle;
            }
            return;
        }
        // An Enter after the one that ended the line, or with the gate
        // closed, may end a line still queued for the shell, also before
        // the hook has announced its trigger (typed while the shell
        // starts) and while a full-screen program runs (typed in the same
        // write as the key that quits it, it is the shell's). While
        // suspended at the prompt, the first accept key ends the line
        // (bash's search) or only the search or widget, and the ones after
        // it are typed ahead.
        if outcome == Outcome::Accept && gate.shell {
            let first_in_suspension = gate.suspended
                && !self.stop
                && !gate.accepted_in_suspension
                && gate.open_but_for_suspension();
            if first_in_suspension {
                gate.accepted_in_suspension = true;
            } else if self.stop || !gate.open() {
                self.typeahead += 1;
            }
        }
        // While suspended the keys are not the line editor's (they go to
        // the search, widget or program): nothing else of them counts.
        if gate.suspended {
            if outcome == Outcome::Accept {
                self.stop = true;
            }
            return;
        }
        if self.stop {
            return;
        }
        match outcome {
            Outcome::Safe => {
                if gate.open() {
                    self.safe_at = Some(end);
                }
                self.last = Last::Safe;
            }
            Outcome::Wait => self.last = Last::Wait,
            Outcome::Idle => self.last = Last::Idle,
            Outcome::Never => {
                if gate.open() {
                    tracing::debug!("trigger suspended until the shell reports");
                }
                gate.suspended = true;
                self.suspended = true;
                self.stop = true;
                self.last = Last::Stop;
            }
            Outcome::Accept => {
                self.accepted |= gate.open();
                gate.armed = false;
                self.stop = true;
                self.last = Last::Stop;
            }
            Outcome::Ignore => {}
        }
    }
}

/// Output held while the shell answers a trigger.
struct Hold {
    /// When the trigger went out.
    since: Instant,
    /// The first held byte's read.
    first: Option<Instant>,
    /// The latest read of output.
    last: Instant,
    /// Reports still to come, one per trigger sent while holding.
    awaiting: u32,
}

impl Hold {
    fn deadline(&self) -> Instant {
        let cap = self.first.unwrap_or(self.since) + HOLD_CAP;
        if self.awaiting == 0 {
            cap.min(self.last + QUIET)
        } else {
            cap
        }
    }
}

pub struct Trigger {
    kind: ShellKind,
    gate: Gate,
    apply_trigger: Option<Vec<u8>>,
    escape_delay: Duration,
    /// The pid `armed` must name; any while unknown (unit tests).
    shell_pid: Option<u32>,
    splitter: KeySplitter,
    keys: Keys,
    read: Read,
    /// A trigger due once no key comes (after an idle key).
    deferred: Option<Instant>,
    /// A trigger due once the escape delay passes (after a lone ESC, or a
    /// key cut by the end of a read).
    esc_hold: Option<Instant>,
    /// The line's first trigger, due after this read of output (133;B or
    /// `armed` opened the gate).
    start_due: bool,
    /// An apply was queued: its trigger goes out once it is all written.
    apply_waiting: bool,
    hold: Option<Hold>,
    /// The hold just ended: the output goes out as one frame.
    released: bool,
    /// A `never` key went to the shell since the last `take_suspended`.
    just_suspended: bool,
    /// The oldest trigger not answered, and whether one was missed already
    /// (both since the latest 7790).
    unanswered: Option<Instant>,
    missed: bool,
    /// Triggers sent and not answered yet.
    in_flight: u32,
    /// bash: whether its questions are looked for, the end of its latest
    /// output, and whether an idle trigger came due during a question.
    questions: bool,
    tail: Vec<u8>,
    after_question: bool,
    /// While suspended: the reports still to come for triggers sent before
    /// the `never` key, which do not end the suspension.
    owed: u32,
    /// The bytes written to the shell so far (see "Typeahead").
    written: u64,
    /// Where in the shell's input each unsettled accept key ends, at most.
    ahead: Vec<u64>,
    /// fish: how many lines it may hold read ahead.
    held: u64,
    /// fish: between `armed` and 133;C or 133;D.
    at_prompt: bool,
    /// fish: from `armed` until its query's reply (DA1) is forwarded to it:
    /// what it gets meanwhile it may read ahead.
    reading_ahead: bool,
    /// fish: the DA1 reply went to it in the read being fed.
    da1_in_read: bool,
    /// fish: the line of this prompt was ended by a key at the open gate,
    /// so it was none of those it may hold.
    line_taken: bool,
    /// A `ta` was written that the hook has not looked for yet (no `look`
    /// since): no other is written meanwhile.
    ta_waiting: bool,
    /// Every byte before this was read at a time when no byte after it was
    /// written yet: the shell's input was empty then.
    empty_at: u64,
    /// Just past the last byte written that the line discipline may delete
    /// (`deletes`).
    clean_from: u64,
    /// Where the latest trigger sent ends in the shell's input.
    last_sent_end: u64,
}

impl Trigger {
    /// For a `shell` shell; inert for zsh.
    pub fn new(shell: ShellKind) -> Self {
        Self {
            kind: shell,
            gate: Gate {
                shell: shell != ShellKind::Zsh,
                ..Gate::default()
            },
            apply_trigger: None,
            escape_delay: ESCAPE_DELAY,
            shell_pid: None,
            splitter: KeySplitter::default(),
            keys: Keys::default(),
            read: Read::default(),
            deferred: None,
            esc_hold: None,
            start_due: false,
            apply_waiting: false,
            hold: None,
            released: false,
            just_suspended: false,
            unanswered: None,
            missed: false,
            in_flight: 0,
            questions: shell == ShellKind::Bash,
            tail: Vec::new(),
            after_question: false,
            owed: 0,
            written: 0,
            ahead: Vec::new(),
            held: 0,
            at_prompt: false,
            reading_ahead: false,
            da1_in_read: false,
            line_taken: false,
            ta_waiting: false,
            empty_at: 0,
            clean_from: 0,
            last_sent_end: 0,
        }
    }

    /// The pid `armed` must name.
    pub fn set_shell_pid(&mut self, pid: u32) {
        self.shell_pid = Some(pid);
    }

    /// Whether the shell announced its trigger: from then on fac asks for
    /// its reports.
    fn announced(&self) -> bool {
        self.gate.trigger.is_some()
    }

    /// Whether applies to this shell carry the line fac expects and need
    /// the apply trigger.
    pub fn wants_apply_trigger(&self) -> bool {
        self.gate.shell && self.apply_trigger.is_some()
    }

    /// A 7791 trigger key from the hook. The trigger and the apply trigger
    /// are taken once per session, and only as the shell's hook sends them
    /// (`TRIGGERS`): a key fac types is never one the shell named anew.
    pub fn on_hook_key(&mut self, key: HookKey) {
        if !self.gate.shell {
            return;
        }
        let fixed = TRIGGERS
            .iter()
            .find(|(shell, ..)| *shell == self.kind)
            .map(|&(_, trigger, apply)| (trigger, apply));
        match key {
            HookKey::Trigger(bytes)
                if self.gate.trigger.is_none()
                    && fixed.is_some_and(|(trigger, _)| bytes == trigger) =>
            {
                tracing::info!("the shell's report trigger is on");
                self.gate.trigger = Some(bytes);
            }
            HookKey::ApplyTrigger(bytes)
                if self.apply_trigger.is_none()
                    && fixed.is_some_and(|(_, apply)| bytes == apply) =>
            {
                self.apply_trigger = Some(bytes);
            }
            HookKey::Trigger(bytes) | HookKey::ApplyTrigger(bytes) => {
                tracing::debug!("a trigger of {} bytes ignored", bytes.len());
            }
            HookKey::Defer(defer) => {
                tracing::debug!("trigger defer n={}", defer.len());
                self.keys.defer = Defer::new(defer);
                self.keys.seq_len = 0;
            }
            HookKey::Armed(pid) => self.on_armed(pid),
            // fac's proxy answers it; the hook read the `ta` that asked.
            HookKey::Look => self.ta_waiting = false,
            HookKey::Unread { mark, bytes } => self.on_unread(mark, bytes),
            HookKey::EscapeDelay(delay) => self.escape_delay = delay.min(MAX_ESCAPE_DELAY),
        }
    }

    fn on_armed(&mut self, pid: u32) {
        if self.shell_pid.is_some_and(|shell| shell != pid) {
            tracing::debug!("armed for another pid; ignored");
            return;
        }
        let was_open = self.gate.open();
        if self.kind == ShellKind::Fish {
            // A new prompt with no 133;C since the last: fish took an empty
            // line (its fish_prompt event comes once per prompt, not on a
            // repaint).
            if self.at_prompt {
                self.line_done();
            }
            self.line_taken = false;
            // With no look (its `unread` comes before `armed`), all that is
            // unsettled may be read ahead at this prompt.
            self.held += self.ahead.len() as u64;
            self.ahead.clear();
            self.at_prompt = true;
            self.reading_ahead = true;
            self.settle_typeahead();
        }
        if self.gate.off {
            tracing::info!("the shell's report trigger is on again at the shell's prompt");
            self.gate.off = false;
        }
        self.gate.armed = true;
        self.gate.suspended = false;
        self.gate.accepted_in_suspension = false;
        self.end_question();
        // A new prompt: the triggers of the lines before it were answered
        // by now, or never will be (an interrupt flushes the input).
        self.unanswered = None;
        self.missed = false;
        self.in_flight = 0;
        self.owed = 0;
        if self.gate.open() && !was_open {
            self.start_due = true;
        }
    }

    /// `unread`: fac's look found `bytes` of the shell's input unread when
    /// it had written `mark` bytes to it (see "Typeahead").
    fn on_unread(&mut self, mark: u64, bytes: u64) {
        let mark = mark.min(self.written);
        let before = self.ahead.len();
        if bytes == 0 {
            // Every byte written was read (or dropped).
            self.empty_at = self.empty_at.max(mark);
            self.ahead.retain(|&end| end > mark);
        } else if mark.saturating_sub(self.empty_at) < QUEUE_ROOM {
            // Nothing was dropped since the input was last empty: the
            // unread bytes are the last ones written, but for what the
            // line discipline deleted, and it deletes only for the bytes
            // `deletes` names. A key with none of them after it was read
            // if it ends at or before `mark - bytes`.
            let read_to = mark.saturating_sub(bytes);
            let clean_from = self.clean_from;
            self.ahead.retain(|&end| end > read_to || end < clean_from);
        }
        let settled = before - self.ahead.len();
        if self.kind == ShellKind::Fish {
            // What is unread is fish's to read ahead at this prompt.
            self.held += self.ahead.len() as u64;
            self.ahead.clear();
        }
        tracing::debug!("typeahead looked at: settled={settled} held={}", self.held);
        self.settle_typeahead();
    }

    /// Whether a line typed ahead may still be queued.
    fn settle_typeahead(&mut self) {
        let queued = !self.ahead.is_empty() || self.held > 0;
        if self.gate.typeahead && !queued {
            tracing::debug!("no line typed ahead is queued");
        }
        self.gate.typeahead = queued;
    }

    /// An accept key that ends at `end` in the shell's input went to it
    /// while its line editor may not read it.
    fn typed_ahead(&mut self, end: u64) {
        if !self.gate.typeahead {
            tracing::debug!("trigger off: a line typed ahead may be queued");
        }
        if self.kind == ShellKind::Fish && self.reading_ahead {
            // Typed before fish has its query's reply: read ahead, maybe.
            self.held += 1;
        } else {
            self.ahead.push(end);
        }
        self.gate.typeahead = true;
    }

    /// `bytes` went to the shell, next after what went before.
    pub fn on_written(&mut self, bytes: &[u8]) {
        if self.gate.shell
            && let Some(at) = bytes.iter().rposition(|&byte| deletes(byte))
        {
            self.clean_from = self.written + at as u64 + 1;
        }
        self.written += bytes.len() as u64;
    }

    /// Whether to write `ta` on the command pipe, which asks the hook to
    /// look at its input at its next prompt: while a line typed ahead may
    /// be queued, and no `ta` waits for the hook (its `look` says it read
    /// one). So the pipe holds at most one, however much is typed into a
    /// program meanwhile, and the next one goes once the hook has looked
    /// (at that prompt, when its look did not settle everything).
    pub fn take_mark(&mut self) -> bool {
        if !self.gate.typeahead || self.ta_waiting {
            return false;
        }
        self.ta_waiting = true;
        true
    }

    /// The shell this is for.
    pub fn shell(&self) -> ShellKind {
        self.kind
    }

    /// The bytes written to the shell so far: a look's mark.
    pub fn written(&self) -> u64 {
        self.written
    }

    /// OSC 133;B: the line editor starts.
    pub fn on_prompt(&mut self) {
        let was_open = self.gate.open();
        self.gate.prompt = true;
        if self.gate.open() && !was_open {
            self.start_due = true;
        }
    }

    /// OSC 133;C: a line runs (before `on_command`).
    pub fn on_line_run(&mut self) {
        if self.kind == ShellKind::Fish && self.at_prompt {
            self.line_done();
        }
    }

    /// fish took a line (it runs, or it was empty): one it held, unless the
    /// user ended it at the open gate (fish holds nothing then, as the gate
    /// was open, unless typed after it, which it takes later).
    fn line_done(&mut self) {
        if !std::mem::take(&mut self.line_taken) && self.held > 0 {
            self.held -= 1;
            self.settle_typeahead();
        }
    }

    /// fish: the reply to its DA1 query went to it: what fac writes after
    /// it, fish reads only as it goes, and no further than the line it
    /// takes.
    pub fn on_device_attrs_forwarded(&mut self) {
        self.da1_in_read = true;
    }

    /// OSC 133;C: the line was accepted and a command runs. Also 133;D,
    /// which ends a line that had no 133;C (bash's Ctrl-C): no trigger
    /// until the next prompt's 133;B, since the shell's `armed` comes
    /// before the prompt, while the terminal still echoes what it gets.
    pub fn on_command(&mut self) {
        self.at_prompt = false;
        self.reading_ahead = false;
        self.gate.armed = false;
        self.gate.prompt = false;
        self.gate.suspended = false;
        self.gate.accepted_in_suspension = false;
        self.end_question();
        self.keys.vi = false;
        self.keys.seq_len = 0;
        self.deferred = None;
        self.esc_hold = None;
        self.start_due = false;
        self.apply_waiting = false;
        self.owed = 0;
        // The command's output is not held.
        if self.hold.take().is_some() {
            self.released = true;
        }
    }

    pub fn on_alt_screen(&mut self, on: bool) {
        self.gate.alt_screen = on;
        if on {
            self.deferred = None;
            self.esc_hold = None;
        }
    }

    /// A 7790 report.
    pub fn on_report(&mut self, report: &BufferReport, now: Instant) {
        if !self.announced() {
            return;
        }
        self.keys.vi = report.flags.vicmd;
        if report.flags.finished {
            self.gate.armed = false;
            // Sent as the line is accepted (fish: from fish_preexec, after
            // 133;C), not for a trigger: it says nothing of what the shell
            // read since, so a line typed ahead may still be queued.
            return;
        }
        // It read every byte up to the trigger it answers, and no byte came
        // after that trigger: the shell's input was empty then.
        if self.in_flight == 1 && self.written == self.last_sent_end {
            self.empty_at = self.empty_at.max(self.last_sent_end);
        }
        self.in_flight = self.in_flight.saturating_sub(1);
        // The shell's line editor reads keys again, unless the report
        // answers a trigger sent before the key that suspended it.
        if self.owed > 0 {
            self.owed -= 1;
        } else {
            self.gate.suspended = false;
            self.gate.accepted_in_suspension = false;
        }
        self.unanswered = None;
        self.missed = false;
        if let Some(hold) = &mut self.hold {
            hold.awaiting = hold.awaiting.saturating_sub(1);
            hold.last = now;
        }
    }

    /// A read of the shell's output came at `now`, before it is scanned.
    pub fn on_output(&mut self, now: Instant) {
        if let Some(hold) = &mut self.hold {
            hold.first.get_or_insert(now);
            hold.last = now;
        }
    }

    /// bash: a read of the shell's output, as it came. While it ends with
    /// a question of the line editor's own (`QUESTIONS`), keys answer it
    /// and no trigger goes; once the line is back, the trigger that waited
    /// goes after the idle delay.
    pub fn on_shell_text(&mut self, bytes: &[u8], now: Instant) {
        if !self.questions || !self.gate.prompt || bytes.is_empty() || !self.announced() {
            return;
        }
        if bytes.len() >= QUESTION_TAIL {
            self.tail.clear();
            self.tail
                .extend_from_slice(&bytes[bytes.len() - QUESTION_TAIL..]);
        } else {
            self.tail.extend_from_slice(bytes);
            let excess = self.tail.len().saturating_sub(QUESTION_TAIL);
            self.tail.drain(..excess);
        }
        let asking = QUESTIONS
            .iter()
            .any(|question| self.tail.ends_with(question));
        if asking == self.gate.question {
            return;
        }
        self.gate.question = asking;
        if asking {
            tracing::debug!("trigger waits: the shell asks a question of its own");
        } else if std::mem::take(&mut self.after_question) && self.gate.open() {
            self.deferred = Some(now + IDLE);
        }
    }

    fn end_question(&mut self) {
        self.gate.question = false;
        self.after_question = false;
        self.tail.clear();
    }

    /// Whether a `never` key (a search, a widget, a program) went to the
    /// shell since the last call: the shell reads keys of its own now, and
    /// no report comes until it is done.
    pub fn take_suspended(&mut self) -> bool {
        std::mem::take(&mut self.just_suspended)
    }

    /// Whether the shell's output waits for the report and its redraw.
    pub fn holds_output(&self) -> bool {
        self.hold.is_some()
    }

    /// Whether the hold ended since the last call: what it held goes out
    /// as one frame.
    pub fn take_released(&mut self) -> bool {
        std::mem::take(&mut self.released)
    }

    /// A read of stdin starts.
    pub fn begin_read(&mut self) {
        self.read = Read::default();
    }

    /// User bytes of the read that went to the shell: `bytes` is
    /// `outbound[at..]`, appended by this read. Before the hook has
    /// announced its trigger they are looked at too, for their accept keys:
    /// typed while the shell starts, they are typed ahead.
    pub fn feed(&mut self, bytes: &[u8], at: usize) {
        if !self.gate.shell || bytes.is_empty() {
            return;
        }
        let Trigger {
            gate,
            splitter,
            keys,
            read,
            deferred,
            esc_hold,
            ..
        } = self;
        let mut i = 0;
        while i < bytes.len() {
            let split = splitter.next(&bytes[i..]);
            let start = at + i;
            i += split.len;
            let end = at + i;
            // A terminal's reply to the shell's query (fish asks OSC 11 and
            // DA1 at every prompt) is no key: no trigger follows it.
            if split.piece == (Piece::Key { whole: true }) && keys::is_reply(split.key) {
                continue;
            }
            if !read.fed {
                read.fed = true;
                // A key came: what waited for one is decided by it.
                *deferred = None;
                *esc_hold = None;
            }
            match split.piece {
                Piece::Key { whole: true } if split.key.is_empty() => {
                    // A key too long to keep: nothing to match.
                    read.note(gate, Outcome::Safe, end);
                }
                Piece::Key { whole: true } => {
                    let outcome = keys.key(split.key);
                    read.note(gate, outcome, end);
                }
                Piece::Key { whole: false } => {
                    if !read.stop {
                        read.last = Last::Cut;
                    }
                }
                Piece::Paste { ends } => {
                    keys.seq_len = 0;
                    if ends {
                        read.note(gate, Outcome::Safe, end);
                    } else if !read.stop {
                        read.last = Last::Paste;
                    }
                }
                Piece::Text { whole } => {
                    let text = &bytes[i - split.len..i];
                    text_keys(text, whole, start, keys, gate, read);
                }
            }
        }
    }

    /// The read is over: the trigger goes where it may, and what waits for
    /// a timer is set. `key_held`: a popup key waits for the shell's report,
    /// so a deferred trigger goes now.
    pub fn end_read(&mut self, outbound: &mut Vec<u8>, now: Instant, key_held: bool) {
        if !self.gate.shell {
            return;
        }
        let read = std::mem::take(&mut self.read);
        if read.fed && self.gate.open() {
            match read.last {
                Last::Idle => self.deferred = Some(now + IDLE),
                Last::Cut => self.esc_hold = Some(now + self.escape_delay),
                Last::Safe | Last::Wait | Last::Paste | Last::Stop => {}
            }
        }
        if let Some(at) = read.safe_at {
            tracing::debug!("trigger sent after a key");
            self.insert(outbound, at, now);
        } else if key_held && self.gate.open() && self.deferred.take().is_some() {
            tracing::debug!("deferred trigger sent before a popup key");
            self.insert(outbound, outbound.len(), now);
        }
        if read.suspended {
            self.owed = self.in_flight;
            self.just_suspended = true;
        }
        for _ in 0..read.typeahead {
            self.typed_ahead(self.written + outbound.len() as u64);
        }
        if read.accepted {
            self.line_taken = true;
        }
        // (The keys of the read before fish's DA1 reply, and after it, are
        // all taken as before it: at most more it may hold.)
        if std::mem::take(&mut self.da1_in_read) {
            self.reading_ahead = false;
        }
    }

    /// After a read of output: the line's first trigger, if 133;B or
    /// `armed` opened the gate.
    pub fn inject_due(&mut self, outbound: &mut Vec<u8>, now: Instant) {
        if !std::mem::take(&mut self.start_due) {
            return;
        }
        // Not inside a key the user is typing (the next key asks), nor
        // while a line typed ahead may be queued (the gate is closed).
        let quiet = !self.splitter.is_cut() && !self.splitter.in_paste() && self.keys.seq_len == 0;
        if !self.gate.open() || !quiet {
            return;
        }
        tracing::debug!("trigger sent at the line's start");
        self.insert(outbound, outbound.len(), now);
    }

    /// An apply was queued on the command pipe.
    pub fn on_apply_queued(&mut self) {
        if self.wants_apply_trigger() {
            self.apply_waiting = true;
        }
    }

    /// The command pipe has written everything queued: a waiting apply's
    /// trigger goes out.
    pub fn on_commands_written(&mut self, outbound: &mut Vec<u8>, now: Instant) {
        if !std::mem::take(&mut self.apply_waiting) || !self.gate.open() {
            return;
        }
        let Some(apply) = &self.apply_trigger else {
            return;
        };
        tracing::debug!("apply trigger sent");
        outbound.extend_from_slice(apply);
        let end = self.written + outbound.len() as u64;
        self.sent(now, end);
    }

    /// The timers: the escape delay, the idle trigger, a missed trigger and
    /// the end of the hold.
    pub fn tick(&mut self, outbound: &mut Vec<u8>, now: Instant) {
        if !self.announced() {
            return;
        }
        if self.esc_hold.is_some_and(|at| now >= at) {
            self.esc_hold = None;
            let Trigger {
                gate,
                splitter,
                keys,
                ..
            } = self;
            // Nothing more came in time: what was cut is the whole key, and
            // nothing joins it.
            let cut = splitter.finish_cut();
            let outcome = if cut.is_empty() {
                Outcome::Safe
            } else {
                keys.key_alone(cut)
            };
            let mut read = Read::default();
            read.note(gate, outcome, outbound.len());
            if read.last == Last::Idle && gate.open() {
                self.deferred = Some(now + IDLE);
            }
            if let Some(at) = read.safe_at {
                tracing::debug!("trigger sent after the escape delay");
                self.insert(outbound, at, now);
            }
            if read.suspended {
                self.owed = self.in_flight;
                self.just_suspended = true;
            }
            for _ in 0..read.typeahead {
                self.typed_ahead(self.written + outbound.len() as u64);
            }
        }
        if self.deferred.is_some_and(|at| now >= at) {
            self.deferred = None;
            if self.gate.question {
                self.after_question = true;
            } else if self.gate.open() {
                tracing::debug!("trigger sent after an idle key");
                self.insert(outbound, outbound.len(), now);
            }
        }
        self.check_missed(now);
        self.settle(now);
    }

    /// End the hold if it is over.
    pub fn settle(&mut self, now: Instant) {
        if self
            .hold
            .as_ref()
            .is_some_and(|hold| now >= hold.deadline())
        {
            self.hold = None;
            self.released = true;
        }
    }

    /// When `tick` next has something to do.
    pub fn deadline(&self) -> Option<Instant> {
        if !self.announced() {
            return None;
        }
        let missed = self.unanswered.map(|sent| sent + MISS);
        let hold = self.hold.as_ref().map(Hold::deadline);
        [self.esc_hold, self.deferred, missed, hold]
            .into_iter()
            .flatten()
            .min()
    }

    /// Put the trigger into `outbound` at `at`.
    fn insert(&mut self, outbound: &mut Vec<u8>, at: usize, now: Instant) {
        let Some(trigger) = &self.gate.trigger else {
            return;
        };
        let at = at.min(outbound.len());
        outbound.extend_from_slice(trigger);
        outbound[at..].rotate_right(trigger.len());
        let end = self.written + (at + trigger.len()) as u64;
        self.sent(now, end);
    }

    /// A trigger that ends at `end` in the shell's input went to it at
    /// `now`: its output is held for the report, which is waited for.
    fn sent(&mut self, now: Instant, end: u64) {
        self.last_sent_end = end;
        match &mut self.hold {
            Some(hold) => hold.awaiting += 1,
            None => {
                self.hold = Some(Hold {
                    since: now,
                    first: None,
                    last: now,
                    awaiting: 1,
                });
            }
        }
        self.unanswered.get_or_insert(now);
        self.in_flight += 1;
    }

    fn check_missed(&mut self, now: Instant) {
        if self.unanswered.is_none_or(|sent| now < sent + MISS) {
            return;
        }
        self.unanswered = None;
        if !std::mem::replace(&mut self.missed, true) {
            return;
        }
        tracing::info!("the shell's report trigger is off for the line: two went unanswered");
        self.gate.off = true;
        self.deferred = None;
        self.esc_hold = None;
        if self.hold.take().is_some() {
            self.released = true;
        }
    }
}

/// The keys of a run of text: every character is one.
fn text_keys(
    text: &[u8],
    whole: bool,
    at: usize,
    keys: &mut Keys,
    gate: &mut Gate,
    read: &mut Read,
) {
    // The bytes of a character cut by the end of the read are left out.
    let complete = if whole {
        text.len()
    } else {
        last_char_start(text)
    };
    if complete > 0 {
        if keys.text_matters() {
            let mut i = 0;
            // A character's continuation bytes from an earlier read.
            while i < complete && (0x80..0xc0).contains(&text[i]) {
                i += 1;
            }
            if i > 0 {
                read.note(gate, Outcome::Safe, at + i);
            }
            while i < complete && !read.stop {
                let len = char_len(text[i]).min(complete - i);
                let outcome = keys.judge(&text[i..i + len]);
                i += len;
                read.note(gate, outcome, at + i);
            }
        } else {
            read.note(gate, Outcome::Safe, at + complete);
        }
    }
    if !whole && !read.stop {
        read.last = Last::Cut;
    }
}

/// Where the last character of `text` starts.
fn last_char_start(text: &[u8]) -> usize {
    let mut at = text.len();
    while at > 0 && (0x80..0xc0).contains(&text[at - 1]) && text.len() - at < 3 {
        at -= 1;
    }
    at.saturating_sub(1)
}

/// Whether the line discipline may delete `byte` from the shell's input,
/// or the byte before it: its erase, kill, word-erase, reprint,
/// literal-next, discard, signal and flow-control characters are control
/// characters (for any other setting fac's look counts none,
/// `pty::only_controls_delete`). Tab, NL, CR and ESC it never takes.
fn deletes(byte: u8) -> bool {
    (byte < 0x20 && !matches!(byte, b'\t' | b'\n' | b'\r' | 0x1b)) || byte == 0x7f
}

/// The bytes of the UTF-8 character that starts with `lead`.
fn char_len(lead: u8) -> usize {
    match lead {
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::protocol::ReportFlags;

    const T: &[u8] = b"\x1b[9999~";
    const A: &[u8] = b"\x1b[9998~";
    /// fish's trigger and apply trigger.
    const FT: &[u8] = "\u{10fffd}".as_bytes();
    const FA: &[u8] = "\u{10fffc}".as_bytes();
    const PID: u32 = 4242;

    /// A bash with the trigger on, at a prompt, and what fac sent it.
    struct Fix {
        t: Trigger,
        out: Vec<u8>,
        now: Instant,
        seq: u64,
    }

    fn defer(element: &str) -> DeferKey {
        DeferKey::parse(element.as_bytes()).expect("a defer element")
    }

    impl Fix {
        fn new() -> Self {
            Self::with_defer(&[])
        }

        fn with_defer(elements: &[&str]) -> Self {
            Self::shell(ShellKind::Bash, elements)
        }

        /// A fish with the trigger on, at a prompt.
        fn fish() -> Self {
            Self::shell(ShellKind::Fish, &[])
        }

        fn shell(kind: ShellKind, elements: &[&str]) -> Self {
            let (trigger, apply) = match kind {
                ShellKind::Fish => (FT, FA),
                _ => (T, A),
            };
            let mut t = Trigger::new(kind);
            t.set_shell_pid(PID);
            t.on_hook_key(HookKey::Trigger(trigger.to_vec()));
            t.on_hook_key(HookKey::ApplyTrigger(apply.to_vec()));
            t.on_hook_key(HookKey::Defer(elements.iter().map(|e| defer(e)).collect()));
            let mut fix = Fix {
                t,
                out: Vec::new(),
                now: Instant::now(),
                seq: 0,
            };
            fix.prompt();
            assert_eq!(fix.take(), trigger, "the line's first trigger");
            fix.report("");
            // Its hold ends.
            fix.wait(1);
            assert!(!fix.t.holds_output());
            fix
        }

        /// The hook's look at its input, before `armed` (its `look`, and
        /// fac's answer sent back): `bytes` unread when fac had written
        /// `mark` bytes.
        fn unread(&mut self, mark: u64, bytes: u64) {
            self.t.on_hook_key(HookKey::Look);
            self.t.on_hook_key(HookKey::Unread { mark, bytes });
        }

        /// `armed`, 133;B, and what follows the read of output.
        fn prompt(&mut self) {
            self.t.on_hook_key(HookKey::Armed(PID));
            self.t.on_prompt();
            self.t.inject_due(&mut self.out, self.now);
        }

        /// One read of stdin, all of it forwarded (the popup is closed).
        fn read(&mut self, bytes: &[u8]) -> Vec<u8> {
            self.read_held(bytes, false)
        }

        fn read_held(&mut self, bytes: &[u8], key_held: bool) -> Vec<u8> {
            self.t.begin_read();
            let before = self.out.len();
            self.out.extend_from_slice(bytes);
            self.t.feed(&self.out[before..], before);
            self.t.end_read(&mut self.out, self.now, key_held);
            self.take()
        }

        /// What went to the shell since the last call, all of it written.
        fn take(&mut self) -> Vec<u8> {
            self.t.on_written(&self.out);
            std::mem::take(&mut self.out)
        }

        fn report_with(&mut self, flags: ReportFlags) {
            self.seq += 1;
            let report = BufferReport {
                seq: self.seq,
                context: "start".into(),
                cursor: 0,
                flags,
                prebuffer: String::new(),
                buffer: String::new(),
                undrawn: Vec::new(),
            };
            self.t.on_output(self.now);
            self.t.on_report(&report, self.now);
            self.t.settle(self.now);
        }

        fn report(&mut self, _line: &str) {
            self.report_with(ReportFlags::default());
        }

        /// A read of the shell's output, with no report in it.
        fn shell_text(&mut self, bytes: &[u8]) {
            self.t.on_output(self.now);
            self.t.on_shell_text(bytes, self.now);
        }

        /// `ms` pass and the timers run: what they sent.
        fn wait(&mut self, ms: u64) -> Vec<u8> {
            self.now += Duration::from_millis(ms);
            self.t.tick(&mut self.out, self.now);
            self.take()
        }
    }

    fn with_t(bytes: &[u8]) -> Vec<u8> {
        [bytes, T].concat()
    }

    #[test]
    fn one_trigger_per_read_after_its_last_key() {
        let mut f = Fix::new();
        assert_eq!(f.read(b"a"), with_t(b"a"));
        f.report("a");
        assert_eq!(f.read(b"bcd"), with_t(b"bcd"));
        f.report("abcd");
        // Arrows, function keys, Backspace, kitty keys and text together.
        let keys = "\x1b[D\x1b[3~\x7f\x1b[97;5ué ש 🙂".as_bytes();
        assert_eq!(f.read(keys), with_t(keys));
        // Every key of a read, then one trigger.
        let out = f.read(b"xyz\x1b[A\x1bb");
        assert_eq!(out.windows(T.len()).filter(|w| *w == T).count(), 1);
        assert!(out.ends_with(T));
    }

    /// bash's ESC in vi-insert (`m`): the keys after it are vi command
    /// mode's at once, before any report says so.
    #[test]
    fn a_vi_command_key_judges_what_follows_in_vi_command_mode() {
        let elements = [
            "m\x1b",
            "i\x1b[3;5~",
            "n\x1bc",
            "P\x1b",
            "Pd",
            "Pf",
            "P2",
            "NR",
        ];
        let mut f = Fix::with_defer(&elements);
        // A lone ESC, once the escape delay has passed: the trigger may
        // follow it; then `d` waits for its motion, `w` ends it.
        assert_eq!(f.read(b"\x1b"), b"\x1b");
        assert_eq!(f.wait(31), T);
        assert_eq!(f.read(b"d"), b"d");
        assert_eq!(f.read(b"w"), with_t(b"w"));
        // ESC and `d` in one key (typed fast): `d` in vi command mode, and
        // so is the count after it.
        f.report("");
        assert_eq!(f.read(b"\x1bd"), b"\x1bd");
        assert_eq!(f.read(b"2"), b"2");
        assert_eq!(f.read(b"f"), b"f");
        assert_eq!(f.read(b"x"), with_t(b"x"));
        // R (replace mode, which has no trigger binding) after ESC.
        f.report("");
        assert_eq!(f.read(b"\x1b"), b"\x1b");
        assert_eq!(f.wait(31), T);
        assert_eq!(f.read(b"R"), b"R");
        assert_eq!(f.read(b"xy"), b"xy");
        assert_eq!(f.wait(200), b"");
        // An arrow is no mode key, nor is a key with an element of its own.
        f.prompt();
        f.take();
        f.report("");
        assert_eq!(f.read(b"\x1b[A"), with_t(b"\x1b[A"));
        f.report("");
        assert_eq!(f.read(b"d"), with_t(b"d"), "text in vi-insert");
        f.report("");
        assert_eq!(f.read(b"\x1b[3;5~"), b"\x1b[3;5~");
        assert_eq!(f.wait(150), T, "idle");
        f.report("");
        assert_eq!(f.read(b"\x1bc"), b"\x1bc", "a widget");
        assert_eq!(f.read(b"a"), b"a");
    }

    /// A `never` key is told once (the popup closes on it), from a read or
    /// from the escape delay.
    #[test]
    fn a_never_key_is_told_once() {
        let mut f = Fix::with_defer(&["n\x12", "n\x1bx"]);
        assert!(!f.t.take_suspended());
        assert_eq!(f.read(b"a"), with_t(b"a"));
        f.report("a");
        assert!(!f.t.take_suspended());
        assert_eq!(f.read(b"\x12"), b"\x12");
        assert!(f.t.take_suspended());
        assert!(!f.t.take_suspended());
        f.report("");
        assert_eq!(f.read(b"\x1bx"), b"\x1bx");
        assert!(f.t.take_suspended());
    }

    #[test]
    fn zsh_never_gets_a_trigger() {
        let mut t = Trigger::new(ShellKind::Zsh);
        t.on_hook_key(HookKey::Trigger(T.to_vec()));
        t.on_hook_key(HookKey::Armed(PID));
        t.on_prompt();
        let mut out = Vec::new();
        t.inject_due(&mut out, Instant::now());
        t.begin_read();
        out.extend_from_slice(b"ls\x1b");
        t.feed(&out, 0);
        t.end_read(&mut out, Instant::now(), true);
        t.tick(&mut out, Instant::now() + Duration::from_secs(1));
        assert_eq!(out, b"ls\x1b");
        assert!(!t.wants_apply_trigger());
        assert!(!t.holds_output());
        assert_eq!(t.deadline(), None);
    }

    /// Only the bytes each hook sends, and only the first time: a trigger
    /// announced anew (another message that got through) changes nothing.
    #[test]
    fn the_triggers_are_taken_once_and_only_as_the_hook_sends_them() {
        let typed = |t: &mut Trigger| {
            t.on_hook_key(HookKey::Armed(PID));
            t.on_prompt();
            let mut out = Vec::new();
            t.inject_due(&mut out, Instant::now());
            out
        };
        // Not the hook's bytes: ignored, and the real ones still count.
        let mut t = Trigger::new(ShellKind::Bash);
        t.set_shell_pid(PID);
        t.on_hook_key(HookKey::Trigger(b"touch PWNED\r".to_vec()));
        t.on_hook_key(HookKey::ApplyTrigger(b"x".to_vec()));
        assert_eq!(typed(&mut t), b"");
        assert!(!t.wants_apply_trigger());
        t.on_hook_key(HookKey::Trigger(T.to_vec()));
        t.on_hook_key(HookKey::ApplyTrigger(A.to_vec()));
        t.on_command();
        assert_eq!(typed(&mut t), T);
        assert!(t.wants_apply_trigger());
        // Once: fish's bytes, or bash's again, are not taken now.
        t.on_hook_key(HookKey::Trigger("\u{10fffd}".as_bytes().to_vec()));
        t.on_hook_key(HookKey::ApplyTrigger(b"\x1b[9997~".to_vec()));
        assert_eq!(t.gate.trigger.as_deref(), Some(T));
        assert_eq!(t.apply_trigger.as_deref(), Some(A));
        // Each shell its own: bash's bytes are no fish trigger.
        let mut fish = Trigger::new(ShellKind::Fish);
        fish.set_shell_pid(PID);
        fish.on_hook_key(HookKey::Trigger(T.to_vec()));
        assert_eq!(typed(&mut fish), b"");
        fish.on_hook_key(HookKey::Trigger("\u{10fffd}".as_bytes().to_vec()));
        fish.on_hook_key(HookKey::ApplyTrigger("\u{10fffc}".as_bytes().to_vec()));
        fish.on_command();
        assert_eq!(typed(&mut fish), "\u{10fffd}".as_bytes());
        assert!(fish.wants_apply_trigger());
    }

    #[test]
    fn nothing_before_the_hook_announces_its_trigger() {
        let mut t = Trigger::new(ShellKind::Fish);
        t.on_hook_key(HookKey::Armed(PID));
        t.on_prompt();
        let mut out = Vec::new();
        t.inject_due(&mut out, Instant::now());
        t.begin_read();
        out.extend_from_slice(b"ls");
        t.feed(&out, 0);
        t.end_read(&mut out, Instant::now(), false);
        assert_eq!(out, b"ls");
    }

    /// Keys typed while the shell starts, before its hook announces the
    /// trigger: an Enter among them may end a line still queued at the
    /// first prompt, so that prompt sends no trigger until a look finds the
    /// Enter read, as after any line typed ahead.
    #[test]
    fn an_enter_typed_before_the_hook_announces_is_typed_ahead() {
        for (kind, trigger, apply) in [(ShellKind::Bash, T, A), (ShellKind::Fish, FT, FA)] {
            let now = Instant::now();
            let mut t = Trigger::new(kind);
            t.set_shell_pid(PID);
            let mut out = b"cat >o1\rabc".to_vec();
            t.begin_read();
            t.feed(&out, 0);
            t.end_read(&mut out, now, false);
            assert_eq!(out, b"cat >o1\rabc", "{kind:?}: no trigger yet");
            t.on_written(&out);
            out.clear();
            assert!(t.take_mark(), "{kind:?}: the hook is asked to look");
            t.on_hook_key(HookKey::Trigger(trigger.to_vec()));
            t.on_hook_key(HookKey::ApplyTrigger(apply.to_vec()));
            t.on_hook_key(HookKey::Defer(Vec::new()));
            // The first prompt: `cat >o1` is still queued.
            t.on_hook_key(HookKey::Armed(PID));
            t.on_prompt();
            t.inject_due(&mut out, now);
            assert_eq!(out, b"", "{kind:?}: a line may be queued");
            // cat runs and reads `abc`; the next prompt's look finds the
            // Enter read.
            t.on_line_run();
            t.on_command();
            let mark = t.written();
            t.on_hook_key(HookKey::Look);
            t.on_hook_key(HookKey::Unread { mark, bytes: 0 });
            t.on_hook_key(HookKey::Armed(PID));
            t.on_prompt();
            t.inject_due(&mut out, now);
            assert_eq!(out, trigger, "{kind:?}: the trigger is back");
        }
    }

    /// However much is typed into a program after an Enter typed ahead,
    /// one `ta` waits on the pipe for the hook; the next goes once the
    /// hook has looked and found something still queued.
    #[test]
    fn one_ta_waits_for_the_hook_at_a_time() {
        let mut f = Fix::new();
        f.t.on_command();
        assert_eq!(f.read(b"cat\r"), b"cat\r");
        assert!(f.t.take_mark(), "the first asks for a look");
        for _ in 0..300 {
            assert_eq!(f.read(b"abcdefghi\r"), b"abcdefghi\r");
            assert!(!f.t.take_mark(), "one waits already");
        }
        // The prompt's look finds a line still queued: the next prompt
        // looks again.
        f.unread(f.t.written, 4);
        f.prompt();
        assert!(f.t.take_mark());
        assert!(!f.t.take_mark());
        f.t.on_command();
        f.unread(f.t.written, 0);
        f.prompt();
        assert_eq!(f.take(), T, "settled");
        assert!(!f.t.take_mark());
    }

    /// A key the hook names as one that accepts the line (`a`: bash's C-x
    /// bound to accept-line, fish's ctrl-x bound to execute) is an Enter:
    /// no trigger after it, and typed ahead it may end a queued line.
    #[test]
    fn a_key_that_accepts_the_line_is_an_enter() {
        // C-x is also the prefix of C-x C-e: it may still accept.
        let mut f = Fix::with_defer(&["a\x18", "n\x18\x05", "A\x0e"]);
        assert_eq!(
            f.read(b"cat\x18abc"),
            [b"cat".as_slice(), T, b"\x18abc"].concat()
        );
        assert_eq!(f.read(b"more"), b"more", "disarmed");
        assert!(!f.t.take_mark(), "the line's own");
        // Typed while a command runs: a line may be queued.
        f.t.on_command();
        assert_eq!(f.read(b"cat >o1\x18abc"), b"cat >o1\x18abc");
        assert!(f.t.take_mark(), "typed ahead");
        f.unread(f.t.written, 11);
        f.prompt();
        assert_eq!(f.take(), b"", "the look finds it unread");
        // vi command mode's own counts only there.
        let mut g = Fix::with_defer(&["A\x0e"]);
        assert_eq!(g.read(b"\x0e"), with_t(b"\x0e"));
    }

    /// A look with bytes unread settles the keys typed ahead that end at
    /// or before `mark - unread`, while that count is exact: no byte the
    /// line discipline may delete came after the key, and fewer than
    /// `QUEUE_ROOM` bytes were written since the shell's input was last
    /// empty (it drops what is past its room). Otherwise only a look with
    /// nothing unread settles them.
    #[test]
    fn a_look_settles_what_its_count_proves_read() {
        let typed_ahead = |rest: &[u8]| {
            let mut f = Fix::new();
            f.t.on_command();
            assert_eq!(f.read(b"cat\r"), b"cat\r");
            assert_eq!(f.read(rest), rest);
            f.t.on_command();
            f
        };
        // The text after the Enter is all that is unread: it was read.
        let mut f = typed_ahead(b"git ch");
        f.unread(f.t.written, 6);
        f.prompt();
        assert_eq!(f.take(), T, "settled");
        // A count of more than that: not read.
        let mut f = typed_ahead(b"git ch");
        f.unread(f.t.written, 7);
        f.prompt();
        assert_eq!(f.take(), b"");
        // An erase after the Enter may have taken bytes out of the count.
        let mut f = typed_ahead(b"gix\x7ft ch");
        f.unread(f.t.written, 6);
        f.prompt();
        assert_eq!(f.take(), b"", "an erase after it");
        // A count fac could not take (u64::MAX), or one taken with the
        // line discipline deleting for other bytes.
        let mut f = typed_ahead(b"git ch");
        f.unread(f.t.written, u64::MAX);
        f.prompt();
        assert_eq!(f.take(), b"");
        // Too much written since the input was last known empty: the line
        // discipline may have dropped some.
        let mut f = typed_ahead(&[b'x'; QUEUE_ROOM as usize]);
        f.unread(f.t.written, 6);
        f.prompt();
        assert_eq!(f.take(), b"", "past the queue's room");
        // Nothing unread settles everything, erases and all.
        let mut f = typed_ahead(b"gix\x7ft ch");
        f.unread(f.t.written, 0);
        f.prompt();
        assert_eq!(f.take(), T);
        // A report of a trigger with nothing written after it, and a look
        // with nothing unread, are where the input was empty.
        let mut f = Fix::new();
        let after_first = f.t.empty_at;
        assert!(after_first > 0, "the first report's trigger");
        assert_eq!(f.read(b"x"), with_t(b"x"));
        f.report("x");
        assert_eq!(f.t.empty_at, f.t.written);
        assert_eq!(f.read(b"y"), with_t(b"y"));
        f.t.on_written(b"z");
        f.report("xy");
        assert!(
            f.t.empty_at < f.t.written - 1,
            "a byte came after its trigger"
        );
    }

    /// While suspended at the prompt (bash's C-r), the first Enter ends
    /// the line or only the search, so it is not typed ahead; every Enter
    /// after it until the suspension ends is, in the same read or later.
    #[test]
    fn enters_after_the_first_in_a_search_are_typed_ahead() {
        let mut f = Fix::with_defer(&["n\x12"]);
        assert_eq!(f.read(b"\x12"), b"\x12");
        assert_eq!(f.read(b"ech\r"), b"ech\r");
        assert!(!f.t.take_mark(), "the first Enter is the line's");
        assert_eq!(f.read(b"cat\r"), b"cat\r");
        assert!(f.t.take_mark(), "typed ahead");
        let mut f = Fix::with_defer(&["n\x12"]);
        assert_eq!(f.read(b"\x12ech\rcat\r"), b"\x12ech\rcat\r");
        assert!(f.t.take_mark(), "in one read");
        // A widget's report ends the suspension: its Enter was the
        // widget's, and the line goes on.
        let mut f = Fix::with_defer(&["n\x14"]);
        assert_eq!(f.read(b"\x14"), b"\x14");
        assert_eq!(f.read(b"\r"), b"\r");
        f.report("ls foo");
        assert_eq!(f.read(b"x"), with_t(b"x"));
        assert!(!f.t.take_mark());
    }

    #[test]
    fn an_accept_key_ends_the_triggers_until_the_next_armed() {
        for enter in [
            &b"\r"[..],
            b"\n",
            b"\x0f",
            b"\x1bOM",
            b"\x1b[13u",
            b"\x1b[13;2u",
            b"\x1b[13;1:1u",
            b"\x1b[57414u",
            b"\x1b[106;5u",
            b"\x1b[109;5u",
            b"\x1b[111;5u",
            b"\x1b[27;5;106~",
            b"\x1b[27;2;13~",
        ] {
            let mut f = Fix::new();
            // The trigger may go before Enter; never after it, nor after
            // what comes later in the read (that is for `cat`).
            let read = [b"cat".as_slice(), enter, b"hello"].concat();
            let expected = [b"cat".as_slice(), T, enter, b"hello"].concat();
            assert_eq!(f.read(&read), expected, "{enter:?}");
            assert_eq!(f.read(b"more"), b"more", "{enter:?}");
            // Nothing after Enter alone.
            let mut f = Fix::new();
            assert_eq!(f.read(enter), enter, "{enter:?}");
            assert_eq!(f.read(b"x"), b"x", "{enter:?}");
            // The next prompt arms it again.
            f.t.on_command();
            assert_eq!(f.read(b"typeahead"), b"typeahead");
            f.prompt();
            assert_eq!(f.take(), T, "{enter:?}");
            f.report("typeahead");
            assert_eq!(f.read(b"y"), with_t(b"y"), "{enter:?}");
        }
        // `armed` again on the same line (fish redraws its prompt) re-arms.
        let mut f = Fix::new();
        assert_eq!(f.read(b"\r"), b"\r");
        f.prompt();
        assert_eq!(f.take(), T);
    }

    /// A line typed ahead, then a key (Up) typed before the next prompt,
    /// while the terminal still echoes: once the hook's look finds the
    /// marked key read, that prompt's first trigger goes at once, so the
    /// key is reported. Until then no timer, and no key, brings a trigger.
    #[test]
    fn a_key_typed_ahead_of_the_prompt_after_a_line_typed_ahead_is_reported() {
        let up = b"\x1b[A";
        let mut f = Fix::new();
        // Enter, and the next line typed while the empty line's prompt is
        // not drawn yet: a mark, once it is written.
        assert_eq!(f.read(b"\r"), b"\r");
        assert!(!f.t.take_mark(), "an Enter at the prompt is its own");
        assert_eq!(f.read(b"echo a\r"), b"echo a\r");
        assert!(f.t.take_mark(), "the hook is asked to look");
        assert!(!f.t.take_mark(), "once");
        // The empty line's prompt: its look finds `echo a` unread.
        f.unread(f.t.written, 7);
        f.prompt();
        assert_eq!(f.take(), b"", "a line may still be queued");
        assert_eq!(f.wait(1000), b"", "no timer brings it");
        assert!(f.t.take_mark(), "the next prompt looks again");
        // The queued line runs; Up comes before the prompt after it, whose
        // look finds no whole line unread.
        f.t.on_command();
        assert_eq!(f.read(up), up);
        f.t.on_command();
        f.unread(f.t.written, up.len() as u64);
        f.prompt();
        assert_eq!(f.take(), T, "the line's first trigger");
        f.report("echo a");
        assert_eq!(f.read(b"x"), with_t(b"x"));
        assert!(!f.t.take_mark(), "no looks once settled");
    }

    /// The reviewer's case: `sleep 1` runs, `cat` and Enter, then `abc`,
    /// are typed meanwhile, and the shell is slow between 133;B and 133;C
    /// (a PS0 that takes 500 ms). However long it takes, and whatever is
    /// typed at that prompt, no trigger goes until a look finds the marked
    /// keys read: cat reads only what was typed.
    #[test]
    fn no_trigger_while_a_line_typed_ahead_may_be_queued() {
        let mut f = Fix::new();
        assert_eq!(
            f.read(b"sleep 1\r"),
            [b"sleep 1".as_slice(), T, b"\r"].concat()
        );
        f.report("sleep 1");
        f.t.on_command();
        assert_eq!(f.read(b"cat\r"), b"cat\r");
        let cat = f.t.written;
        assert_eq!(f.read(b"abc"), b"abc");
        // sleep ends; the prompt's look finds `cat` unread: no `unread`.
        f.prompt();
        assert_eq!(f.take(), b"");
        assert_eq!(f.wait(300), b"", "no timed trigger");
        assert_eq!(f.read(b"x"), b"x", "nor one after a key");
        assert_eq!(f.wait(500), b"");
        // A look that finds its Enter unread settles nothing.
        f.unread(cat, 4);
        assert_eq!(f.read(b"y"), b"y");
        // cat runs at last; what is typed into it gets nothing either, and
        // its Enter is unsettled too.
        f.t.on_command();
        assert_eq!(f.read(b"\r\x04"), b"\r\x04");
        // A look from before that Enter settles only the keys before it.
        f.unread(cat, 0);
        f.prompt();
        assert_eq!(f.take(), b"", "cat's Enter is not settled");
        f.t.on_command();
        assert!(f.t.take_mark());
        f.unread(f.t.written, 0);
        f.prompt();
        assert_eq!(f.take(), T, "the fallback ends at the next settled prompt");
        f.report("");
        assert_eq!(f.read(b"l"), with_t(b"l"));
        // A mark is no more than fac wrote.
        let mut g = Fix::new();
        assert_eq!(g.read(b"\r"), b"\r");
        g.t.on_command();
        assert_eq!(g.read(b"cat\r"), b"cat\r");
        g.unread(u64::MAX, 0);
        g.prompt();
        assert_eq!(g.take(), T, "everything fac wrote was read");
    }

    /// fish reads ahead: at a prompt it reads all its input until its
    /// query's reply and keeps what it did not use, where no look sees it.
    /// fac counts the lines fish may hold, one less for each line it runs.
    #[test]
    fn fish_holds_lines_it_read_ahead_until_it_runs_them() {
        let mut f = Fix::fish();
        assert_eq!(
            f.read(b"sleep 1\r"),
            [b"sleep 1".as_slice(), FT, b"\r"].concat()
        );
        f.report("sleep 1");
        f.t.on_command();
        // Two lines typed while sleep runs.
        assert_eq!(f.read(b"ls\rcat\r"), b"ls\rcat\r");
        assert!(f.t.take_mark());
        let mark = f.t.written;
        // The prompt after sleep: both unread, so fish reads both ahead.
        f.unread(mark, 7);
        f.prompt();
        assert_eq!(f.take(), b"");
        assert_eq!(f.t.held, 2);
        // It runs `ls`; at the next prompt its input is empty, but it
        // still holds `cat`.
        f.t.on_line_run();
        f.t.on_command();
        f.unread(mark, 0);
        f.prompt();
        assert_eq!(f.take(), b"", "fish still holds a line");
        f.t.on_line_run();
        f.t.on_command();
        // A line typed into cat: unsettled, then read by cat.
        assert_eq!(f.read(b"hello\r"), b"hello\r");
        assert_eq!(f.t.held, 0);
        // The prompt after cat: what is unread now (fish's query replies,
        // written after the keys) is no line.
        assert!(f.t.take_mark());
        let mark = f.t.written;
        assert_eq!(f.read(b"\x1b[?62;22c"), b"\x1b[?62;22c");
        f.unread(f.t.written, f.t.written - mark);
        f.prompt();
        assert_eq!(f.take(), FT, "on again");
        // At an off prompt, an Enter fish reads there is one line it may
        // hold until it runs one.
        let mut f = Fix::fish();
        f.t.on_command();
        assert_eq!(f.read(b"cat\r"), b"cat\r");
        f.prompt();
        assert_eq!(
            f.take(),
            b"",
            "no look: what is unsettled may be read ahead"
        );
        assert_eq!(f.t.held, 1);
        assert_eq!(f.read(b"x\r"), b"x\r");
        assert_eq!(f.t.held, 2);
        f.t.on_line_run();
        f.t.on_command();
        f.prompt();
        assert_eq!(f.take(), b"", "one may still be held");
        // Empty lines run nothing: a new prompt with no 133;C takes one,
        // but not the line the user ended at the open gate.
        let mut g = Fix::fish();
        assert_eq!(g.read(b"\r"), b"\r");
        assert_eq!(g.read(b"\r"), b"\r");
        assert_eq!(g.t.held, 1, "typed before fish's query reply");
        g.prompt();
        assert_eq!(g.take(), b"", "the first Enter's prompt: one held");
        g.prompt();
        assert_eq!(g.take(), FT, "fish took the second");
        // What comes after fish's DA1 reply, fish reads only as it goes:
        // an Enter then is in its input, for the next look.
        let mut h = Fix::fish();
        assert_eq!(h.read(b"\r"), b"\r");
        h.t.on_command();
        assert_eq!(h.read(b"cat\r"), b"cat\r");
        h.prompt();
        assert_eq!(h.take(), b"");
        assert_eq!(h.t.held, 1);
        h.t.begin_read();
        h.t.on_device_attrs_forwarded();
        h.t.end_read(&mut h.out, h.now, false);
        assert_eq!(h.read(b"ls\r"), b"ls\r");
        assert_eq!((h.t.held, h.t.ahead.len()), (1, 1));
        f.t.on_line_run();
        f.t.on_command();
        // Both ran, and no Enter is unsettled: nothing is left to look at.
        assert!(!f.t.take_mark());
        f.prompt();
        assert_eq!(f.take(), FT);
    }

    /// fish asks OSC 11 and DA1 at every prompt: their replies come after
    /// 133;B, as user bytes, and are no keys. No trigger follows them, and
    /// a line typed ahead keeps the first trigger back.
    #[test]
    fn a_terminals_reply_is_no_key() {
        let mut f = Fix::new();
        let replies = b"\x1b]11;rgb:2828/2c2c/3434\x1b\\\x1b[?62;22c";
        assert_eq!(f.read(replies), replies);
        assert_eq!(f.wait(200), b"");
        // A key and a reply in one read: the trigger after the key.
        let read = [&b"x"[..], replies].concat();
        assert_eq!(f.read(&read), [b"x".as_slice(), T, replies].concat());
        f.report("x");
        // Typed ahead: the prompt's replies do not ask for the first trigger.
        assert_eq!(f.read(b"\rcat\r"), b"\rcat\r");
        f.t.on_command();
        f.prompt();
        assert_eq!(f.take(), b"");
        assert_eq!(f.read(replies), replies);
        assert_eq!(f.wait(200), b"");
    }

    /// fish's `f` report comes from fish_preexec, after 133;C: it answers
    /// no trigger, so a line typed ahead still holds back the next prompt's
    /// first trigger.
    #[test]
    fn a_finished_report_answers_no_trigger() {
        let mut f = Fix::new();
        assert_eq!(
            f.read(b"ls\rcat\r"),
            [b"ls".as_slice(), T, b"\rcat\r"].concat()
        );
        f.report("ls");
        f.t.on_command();
        f.report_with(ReportFlags {
            finished: true,
            ..ReportFlags::default()
        });
        f.prompt();
        assert_eq!(f.take(), b"");
        // Nor does it arm or open the gate.
        let mut g = Fix::new();
        g.report_with(ReportFlags {
            finished: true,
            ..ReportFlags::default()
        });
        assert_eq!(g.read(b"x"), b"x");
    }

    #[test]
    fn no_trigger_after_a_line_typed_ahead_until_its_keys_are_read() {
        // One read with two lines: the second is typed ahead.
        let mut g = Fix::new();
        assert_eq!(
            g.read(b"ls\rcat\r"),
            [b"ls".as_slice(), T, b"\rcat\r"].concat()
        );
        assert!(g.t.take_mark());
        g.report("ls");
        g.t.on_command();
        g.prompt();
        assert_eq!(g.take(), b"");
        let mut f = Fix::new();
        assert_eq!(f.read(b"ls\r"), [b"ls".as_slice(), T, b"\r"].concat());
        // Typed before the next prompt: `cat` and its Enter, and text.
        assert_eq!(f.read(b"cat\rhello"), b"cat\rhello");
        f.report("ls");
        f.t.on_command();
        // The prompt after `ls`: a trigger now would come after `cat`'s
        // Enter in the shell's input, so it would be cat's.
        f.prompt();
        assert_eq!(f.take(), b"");
        // Nor after the first key there: it may come before the queued
        // line is read (a slow PS0).
        assert_eq!(f.read(b"x"), b"x");
        f.t.on_command();
        // Lines typed into cat while it runs.
        assert_eq!(f.read(b"\rworld\r\x04"), b"\rworld\r\x04");
        assert!(f.t.take_mark());
        f.unread(f.t.written, 0);
        f.prompt();
        assert_eq!(f.take(), T);
        // Text typed ahead without an Enter is the next line's: the first
        // trigger follows it, with nothing to settle.
        f.report("");
        assert_eq!(f.read(b"\r"), b"\r");
        f.t.on_command();
        assert_eq!(f.read(b"ls"), b"ls");
        assert!(!f.t.take_mark());
        f.prompt();
        assert_eq!(f.take(), T);
        // A full-screen program's Enters are typed ahead too: the shell may
        // get some (written with the key that quits the program). It
        // normally reads them all, so the next prompt's look finds them
        // read.
        let mut f = Fix::new();
        assert_eq!(f.read(b"vi\r"), [b"vi".as_slice(), T, b"\r"].concat());
        f.report("vi");
        f.t.on_command();
        f.t.on_alt_screen(true);
        assert_eq!(f.read(b"ihello\r\x1b:wq\r"), b"ihello\r\x1b:wq\r");
        f.t.on_alt_screen(false);
        assert!(f.t.take_mark());
        f.unread(f.t.written, 0);
        f.prompt();
        assert_eq!(f.take(), T);
        // `q` to less and the next line in one write: less reads only `q`.
        let mut f = Fix::new();
        f.t.on_command();
        f.t.on_alt_screen(true);
        assert_eq!(f.read(b"qcat >o1\rabc"), b"qcat >o1\rabc");
        f.t.on_alt_screen(false);
        assert!(f.t.take_mark());
        f.unread(f.t.written, 11);
        f.prompt();
        assert_eq!(f.take(), b"", "cat >o1 is queued");
        // Reports (a widget's, fish's `f`) settle nothing: only a look.
        let mut f = Fix::new();
        assert_eq!(f.read(b"\r"), b"\r");
        f.t.on_command();
        assert_eq!(f.read(b"cat\r"), b"cat\r");
        f.prompt();
        f.report("");
        assert_eq!(f.read(b"z"), b"z");
    }

    #[test]
    fn a_lone_esc_waits_for_the_escape_delay() {
        let mut f = Fix::new();
        assert_eq!(f.read(b"\x1b"), b"\x1b");
        assert_eq!(f.wait(29), b"");
        assert_eq!(f.wait(1), T);
        f.report("");
        // The next byte in time makes it Alt with that key.
        assert_eq!(f.read(b"\x1b"), b"\x1b");
        assert_eq!(f.wait(10), b"");
        assert_eq!(f.read(b"b"), with_t(b"b"));
        assert_eq!(f.wait(100), b"", "nothing more");
        f.report("");
        // Or the rest of a sequence cut by the read.
        assert_eq!(f.read(b"\x1b["), b"\x1b[");
        assert_eq!(f.read(b"1;5D"), with_t(b"1;5D"));
        f.report("");
        // The shell's own delay, from its context.
        f.t.on_hook_key(HookKey::EscapeDelay(Duration::from_millis(80)));
        assert_eq!(f.read(b"x\x1b"), [b"x".as_slice(), T, b"\x1b"].concat());
        f.report("x");
        assert_eq!(f.wait(79), b"");
        assert_eq!(f.wait(1), T);
    }

    /// fish's Alt keys (`ESC h` is man) make ESC a prefix: a lone ESC is
    /// still Escape once the delay is over, and the key after it is a key
    /// of its own, not Alt with it.
    #[test]
    fn a_lone_esc_past_the_delay_is_no_prefix() {
        let mut f = Fix::with_defer(&["n\x1bh", "N\x1be", "Pd"]);
        assert_eq!(f.read(b"\x1b"), b"\x1b");
        assert_eq!(f.wait(30), T);
        f.report("");
        assert_eq!(f.read(b"h"), with_t(b"h"));
        f.report("h");
        // In time, it is Alt-h: never.
        assert_eq!(f.read(b"\x1b"), b"\x1b");
        assert_eq!(f.read(b"h"), b"h");
        assert_eq!(f.wait(200), b"");
        // An element that is ESC itself still counts.
        let mut g = Fix::with_defer(&["n\x1b", "n\x1bh"]);
        assert_eq!(g.read(b"\x1b"), b"\x1b");
        assert_eq!(g.wait(30), b"");
        assert_eq!(g.read(b"x"), b"x");
    }

    /// bash's emacs ESC has no binding of its own (`w`): readline waits for
    /// the key after it however long the pause, so that key is judged with
    /// it, past the escape delay too (M-. after a second, M-x).
    #[test]
    fn a_w_esc_is_judged_with_the_key_after_it_however_late() {
        let mut f = Fix::with_defer(&["w\x1b", "i\x1b.", "n\x1bx", "p\x1b1"]);
        assert_eq!(f.read(b"\x1b"), b"\x1b");
        assert_eq!(f.wait(30), b"", "not after the escape delay");
        assert_eq!(f.wait(1000), b"", "nor a second later");
        // yank-last-arg, an idle key: its trigger once no key came.
        assert_eq!(f.read(b"."), b".");
        assert_eq!(f.wait(149), b"");
        assert_eq!(f.wait(1), T);
        f.report("");
        // M-x, typed a second apart: execute-named-command reads keys of
        // its own, so none.
        assert_eq!(f.read(b"\x1b"), b"\x1b");
        assert_eq!(f.wait(1000), b"");
        assert_eq!(f.read(b"x"), b"x");
        assert!(f.t.take_suspended());
        assert_eq!(f.read(b"ls\r"), b"ls\r");
        f.report("");
        // A key with no element of its own after it: the trigger follows.
        assert_eq!(f.read(b"\x1b"), b"\x1b");
        assert_eq!(f.wait(1000), b"");
        assert_eq!(f.read(b"f"), with_t(b"f"));
        f.report("");
        // In one key, as ever.
        assert_eq!(f.read(b"\x1b."), b"\x1b.");
        assert_eq!(f.wait(150), T);
        f.report("");
        // vi command mode's own (`W`) counts only there.
        let mut g = Fix::with_defer(&["W\x1b", "Nx"]);
        assert_eq!(g.read(b"\x1b"), b"\x1b");
        assert_eq!(g.wait(30), T, "outside vi command mode, Escape");
        g.report_with(ReportFlags {
            vicmd: true,
            ..ReportFlags::default()
        });
        assert_eq!(g.read(b"\x1b"), b"\x1b");
        assert_eq!(g.wait(1000), b"");
        assert_eq!(g.read(b"x"), with_t(b"x"), "ESC x is no element");
    }

    #[test]
    fn a_character_cut_by_the_read_is_finished_first() {
        let mut f = Fix::new();
        let e = "é".as_bytes();
        assert_eq!(
            f.read(&[b"a".as_slice(), &e[..1]].concat()),
            [b"a".as_slice(), T, &e[..1]].concat()
        );
        f.report("a");
        assert_eq!(f.read(&e[1..]), with_t(&e[1..]));
        f.report("aé");
        let smile = "🙂".as_bytes();
        assert_eq!(f.read(&smile[..2]), &smile[..2]);
        assert_eq!(f.read(&smile[2..3]), &smile[2..3]);
        assert_eq!(f.read(&smile[3..]), with_t(&smile[3..]));
    }

    #[test]
    fn defer_prefixes_and_keys() {
        let mut f = Fix::with_defer(&["n\x18\x05", "p\x16", "p\x1b1", "Pf", "N/"]);
        // A prefix: nothing after it; the next key decides with it.
        assert_eq!(f.read(b"a\x18"), [b"a".as_slice(), T, b"\x18"].concat());
        f.report("a");
        assert_eq!(f.read(b"\x15"), with_t(b"\x15"));
        f.report("");
        // C-x C-e opens a program: nothing until the shell reports.
        assert_eq!(f.read(b"\x18\x05"), b"\x18\x05");
        assert_eq!(
            f.read(b"typed in the editor\x1b"),
            b"typed in the editor\x1b"
        );
        assert_eq!(f.wait(100), b"");
        f.report("");
        assert_eq!(f.read(b"x"), with_t(b"x"));
        f.report("x");
        // The same key in kitty's form.
        assert_eq!(f.read(b"\x1b[120;5u\x1b[101;5u"), b"\x1b[120;5u\x1b[101;5u");
        assert_eq!(f.read(b"y"), b"y");
        f.report("x");
        // C-v and M-1: nothing right after them; the next key may.
        assert_eq!(f.read(b"\x16"), b"\x16");
        assert_eq!(f.read(b"\x01"), with_t(b"\x01"));
        f.report("x");
        assert_eq!(f.read(b"\x1b1"), b"\x1b1");
        assert_eq!(f.read(b"\x1b[B"), with_t(b"\x1b[B"));
        f.report("x");
        // vi command mode's own keys only count there.
        assert_eq!(f.read(b"fb"), with_t(b"fb"));
        f.report_with(ReportFlags {
            vicmd: true,
            ..ReportFlags::default()
        });
        assert_eq!(f.read(b"f"), b"f");
        assert_eq!(f.read(b"x"), with_t(b"x"));
        f.report_with(ReportFlags {
            vicmd: true,
            ..ReportFlags::default()
        });
        assert_eq!(f.read(b"3fxw"), with_t(b"3fxw"));
        f.report_with(ReportFlags {
            vicmd: true,
            ..ReportFlags::default()
        });
        assert_eq!(f.read(b"wf"), [b"w".as_slice(), T, b"f"].concat());
        assert_eq!(f.read(b"x"), with_t(b"x"));
        f.report_with(ReportFlags {
            vicmd: true,
            ..ReportFlags::default()
        });
        f.report_with(ReportFlags {
            vicmd: true,
            ..ReportFlags::default()
        });
        // A search reads keys of its own.
        assert_eq!(f.read(b"/abc"), b"/abc");
        assert_eq!(f.read(b"\x7fd"), b"\x7fd");
        f.report_with(ReportFlags::default());
        assert_eq!(f.read(b"a/"), with_t(b"a/"));
    }

    #[test]
    fn a_widget_key_gets_no_trigger_even_after_keys_typed_into_it() {
        let mut f = Fix::with_defer(&["n\x14"]);
        assert_eq!(f.read(b"ls \x14"), [b"ls ".as_slice(), T, b"\x14"].concat());
        f.report("ls ");
        for keys in [&b"foo"[..], b"\x1b[B", b"\t", b"\x7f"] {
            assert_eq!(f.read(keys), keys);
        }
        assert_eq!(f.read(b"\r"), b"\r");
        assert_eq!(f.wait(500), b"");
        // The widget's wrapper reports when it returns.
        f.report("ls foo");
        assert_eq!(f.read(b"x"), with_t(b"x"));
    }

    #[test]
    fn a_paste_split_across_three_reads() {
        let mut f = Fix::new();
        assert_eq!(
            f.read(b"x\x1b[200~echo \r"),
            [b"x".as_slice(), T, b"\x1b[200~echo \r"].concat()
        );
        f.report("x");
        assert_eq!(f.read(b"one\ttwo\x1b"), b"one\ttwo\x1b");
        assert_eq!(f.wait(100), b"", "no escape delay inside a paste");
        assert_eq!(f.read(b"[Athree\x1b[201~y"), with_t(b"[Athree\x1b[201~y"));
        f.report("");
        // Its markers cut too.
        assert_eq!(f.read(b"\x1b[20"), b"\x1b[20");
        assert_eq!(f.read(b"0~ab\r\x1b[2"), b"0~ab\r\x1b[2");
        assert_eq!(f.read(b"01"), b"01");
        assert_eq!(f.read(b"~"), with_t(b"~"));
        f.report("");
        assert_eq!(f.read(b"z"), with_t(b"z"), "armed: the Enter was pasted");
    }

    #[test]
    fn an_idle_key_waits_for_150_ms_without_keys() {
        let mut f = Fix::with_defer(&["i\t", "i\x17", "i\x19"]);
        assert_eq!(f.read(b"\t"), b"\t");
        assert_eq!(f.wait(149), b"");
        assert_eq!(f.wait(1), T);
        f.report("");
        // Tab Tab: nothing between them; each Tab starts the wait again.
        assert_eq!(f.read(b"\t"), b"\t");
        assert_eq!(f.wait(100), b"");
        assert_eq!(f.read(b"\t"), b"\t");
        assert_eq!(f.wait(100), b"");
        assert_eq!(f.wait(50), T);
        f.report("");
        // C-w C-w in one read, then C-y.
        assert_eq!(f.read(b"\x17\x17"), b"\x17\x17");
        assert_eq!(f.read(b"\x19"), b"\x19");
        assert_eq!(f.wait(150), T);
        f.report("");
        // A key after it gets its own trigger instead.
        assert_eq!(f.read(b"\t"), b"\t");
        assert_eq!(f.read(b"x"), with_t(b"x"));
        assert_eq!(f.wait(500), b"");
        f.report("");
        // The keys before it in a read get theirs.
        assert_eq!(f.read(b"ab\t"), [b"ab".as_slice(), T, b"\t"].concat());
        assert_eq!(f.wait(150), T);
    }

    #[test]
    fn a_popup_key_sends_a_deferred_trigger_first() {
        let mut f = Fix::with_defer(&["i\x17"]);
        assert_eq!(f.read(b"\x17"), b"\x17");
        // Down, held by the popup for the report: nothing forwarded.
        assert_eq!(f.read_held(b"", true), T);
        assert_eq!(f.wait(500), b"", "it went once");
        f.report("");
        // Without a deferred trigger, a held key sends nothing.
        assert_eq!(f.read_held(b"", true), b"");
    }

    #[test]
    fn armed_for_the_shells_pid_and_disarmed_at_133_c() {
        let mut t = Trigger::new(ShellKind::Bash);
        t.set_shell_pid(PID);
        t.on_hook_key(HookKey::Trigger(T.to_vec()));
        let mut out = Vec::new();
        let now = Instant::now();
        let read = |t: &mut Trigger, out: &mut Vec<u8>, bytes: &[u8]| {
            t.begin_read();
            let before = out.len();
            out.extend_from_slice(bytes);
            t.feed(&out[before..], before);
            t.end_read(out, now, false);
            std::mem::take(out)
        };
        // At the prompt but not armed: nothing.
        t.on_prompt();
        t.inject_due(&mut out, now);
        assert_eq!(read(&mut t, &mut out, b"a"), b"a");
        // Another process's `armed` (a nested shell) is not this shell's.
        t.on_hook_key(HookKey::Armed(PID + 1));
        t.inject_due(&mut out, now);
        assert_eq!(read(&mut t, &mut out, b"b"), b"b");
        // Armed at the prompt: the line's first trigger, then per key.
        t.on_hook_key(HookKey::Armed(PID));
        t.inject_due(&mut out, now);
        assert_eq!(std::mem::take(&mut out), T);
        assert_eq!(read(&mut t, &mut out, b"c"), with_t(b"c"));
        // A command runs: nothing, typeahead included.
        t.on_command();
        assert_eq!(read(&mut t, &mut out, b"ls"), b"ls");
        // `armed` comes before 133;B (bash's PROMPT_COMMAND): the first
        // trigger goes at 133;B.
        t.on_hook_key(HookKey::Armed(PID));
        t.inject_due(&mut out, now);
        assert_eq!(std::mem::take(&mut out), b"");
        t.on_prompt();
        t.inject_due(&mut out, now);
        assert_eq!(std::mem::take(&mut out), T);
        // In a full-screen program: nothing.
        t.on_alt_screen(true);
        assert_eq!(read(&mut t, &mut out, b"j"), b"j");
        t.on_alt_screen(false);
        assert_eq!(read(&mut t, &mut out, b"k"), with_t(b"k"));
    }

    /// bash's completion asks `Display all 200 possibilities? (y or n)`,
    /// and its pager shows `--More--`: readline reads the answer itself, so
    /// no trigger goes until the line is back.
    #[test]
    fn no_trigger_into_bashs_own_questions() {
        let mut f = Fix::with_defer(&["i\t"]);
        // The second Tab: the question comes before its idle trigger.
        assert_eq!(f.read(b"\t"), b"\t");
        f.shell_text(b"\r\nDisplay all 200 possibilities? (y or n)");
        assert_eq!(f.wait(200), b"", "no trigger into the question");
        // y, then Enter and space in the pager: each an answer, Enter no
        // accept.
        assert_eq!(f.read(b"y"), b"y");
        f.shell_text(b"\r\nfile-1.txt  file-2.txt\r\n--More--");
        assert_eq!(f.wait(200), b"");
        assert_eq!(f.read(b"\r"), b"\r");
        f.shell_text(b"\r\x1b[Kfile-3.txt\r\n--Mo");
        f.shell_text(b"re--");
        assert_eq!(f.wait(200), b"", "a question split across reads");
        assert_eq!(f.read(b" "), b" ");
        assert_eq!(f.wait(200), b"");
        // q: the line is back, and the trigger goes once no key came.
        assert_eq!(f.read(b"q"), b"q");
        f.shell_text(b"\r\x1b[K% ls file-");
        assert_eq!(f.wait(150), T);
        f.report("ls file-");
        assert_eq!(f.read(b"x"), with_t(b"x"), "still armed");
        // n, at the question: the same.
        f.report("ls file-x");
        assert_eq!(f.read(b"\t"), b"\t");
        f.shell_text(b"\r\nDisplay all 200 possibilities? (y or n)");
        assert_eq!(f.wait(200), b"");
        assert_eq!(f.read(b"n"), b"n");
        assert_eq!(f.wait(200), b"", "still the question's line");
        f.shell_text(b"\r\n% ls file-x");
        assert_eq!(f.wait(150), T, "the line is back");
    }

    #[test]
    fn off_after_two_triggers_go_unanswered() {
        let mut f = Fix::new();
        assert_eq!(f.read(b"a"), with_t(b"a"));
        assert_eq!(f.wait(29), b"");
        // Answered late: still on.
        f.report("a");
        assert_eq!(f.read(b"b"), with_t(b"b"));
        assert_eq!(f.wait(30), b"");
        // One missed; triggers sent before it was found missed count with it.
        assert_eq!(f.read(b"c"), with_t(b"c"));
        assert_eq!(f.wait(29), b"");
        assert!(!f.t.gate.off);
        assert_eq!(f.wait(1), b"");
        assert!(f.t.gate.off, "the second miss");
        assert_eq!(f.read(b"d"), b"d", "off for the rest of the line");
        f.report("abcd");
        assert_eq!(f.read(b"e"), b"e", "a late report does not turn it on");
        // A nested or exec'd shell's `armed` leaves it off.
        f.t.on_hook_key(HookKey::Armed(PID + 1));
        f.t.on_prompt();
        f.t.inject_due(&mut f.out, f.now);
        assert_eq!(f.take(), b"", "off for another pid");
        // The hook's shell at its next prompt: on again, missed count reset.
        f.t.on_command();
        f.prompt();
        assert_eq!(f.take(), T, "on again at the shell's next prompt");
        f.report("");
        assert_eq!(f.read(b"f"), with_t(b"f"));
        assert_eq!(f.wait(30), b"");
        assert!(!f.t.gate.off, "one miss after the prompt is the first");
        // A report in between starts the count again.
        let mut f = Fix::new();
        assert_eq!(f.read(b"a"), with_t(b"a"));
        assert_eq!(f.wait(30), b"");
        f.report("a");
        assert_eq!(f.read(b"b"), with_t(b"b"));
        assert_eq!(f.wait(30), b"");
        assert!(!f.t.gate.off);
        // Two triggers within one stall are one miss.
        let mut f = Fix::new();
        assert_eq!(f.read(b"a"), with_t(b"a"));
        f.now += Duration::from_millis(5);
        assert_eq!(f.read(b"b"), with_t(b"b"));
        assert_eq!(f.wait(40), b"");
        assert!(!f.t.gate.off);
    }

    #[test]
    fn the_hold_ends_after_the_report_and_1_ms_of_quiet_or_at_30_ms() {
        let mut f = Fix::new();
        assert!(!f.t.holds_output());
        f.read(b"a");
        assert!(f.t.holds_output());
        // The echo, the clear, the report and the redraw, 0.4 ms apart.
        let step = Duration::from_micros(400);
        f.now += step;
        f.t.on_output(f.now);
        f.now += step;
        f.report("a");
        f.now += step;
        f.t.on_output(f.now);
        f.t.settle(f.now);
        assert!(f.t.holds_output());
        assert_eq!(f.t.deadline(), Some(f.now + QUIET));
        f.now += QUIET - Duration::from_micros(1);
        f.t.tick(&mut f.out, f.now);
        assert!(f.t.holds_output());
        f.now += Duration::from_micros(1);
        f.t.tick(&mut f.out, f.now);
        assert!(!f.t.holds_output());
        assert!(f.t.take_released());
        assert!(!f.t.take_released());
        // No report: 30 ms after the first held byte.
        f.read(b"b");
        f.now += Duration::from_millis(5);
        f.t.on_output(f.now);
        let first = f.now;
        f.now += Duration::from_millis(20);
        f.t.on_output(f.now);
        f.t.settle(f.now);
        assert!(f.t.holds_output());
        f.wait(0);
        assert_eq!(
            f.t.hold.as_ref().map(Hold::deadline),
            Some(first + HOLD_CAP)
        );
        f.now = first + HOLD_CAP;
        f.t.tick(&mut f.out, f.now);
        assert!(!f.t.holds_output());
        // Two triggers: both reports are waited for.
        let mut f = Fix::new();
        f.read(b"a");
        f.read(b"b");
        f.report("a");
        f.wait(2);
        assert!(f.t.holds_output());
        f.report("ab");
        f.wait(2);
        assert!(!f.t.holds_output());
        // A command's output is never held.
        f.read(b"c");
        f.t.on_command();
        assert!(!f.t.holds_output());
    }

    #[test]
    fn the_apply_trigger_follows_the_written_apply() {
        let mut f = Fix::new();
        assert!(f.t.wants_apply_trigger());
        f.t.on_apply_queued();
        assert_eq!(f.take(), b"");
        f.t.on_commands_written(&mut f.out, f.now);
        assert_eq!(f.take(), A);
        assert!(f.t.holds_output());
        // Only once per apply.
        f.t.on_commands_written(&mut f.out, f.now);
        assert_eq!(f.take(), b"");
        // A shell without an apply trigger gets none.
        let mut t = Trigger::new(ShellKind::Bash);
        t.on_hook_key(HookKey::Trigger(T.to_vec()));
        assert!(!t.wants_apply_trigger());
    }

    #[test]
    fn a_typical_read_takes_well_under_10_us() {
        let elements: Vec<String> = (0..40)
            .map(|i| format!("n\x18{}", char::from(b'a' + i % 26)))
            .chain(["i\t".into(), "i\x17".into(), "Pf".into(), "Pt".into()])
            .collect();
        let elements: Vec<&str> = elements.iter().map(String::as_str).collect();
        let mut f = Fix::with_defer(&elements);
        let reads: [&[u8]; 4] = [b"a", b"git checkout ", "é".as_bytes(), b"\x1b[A"];
        let rounds = 20_000;
        let mut out = Vec::with_capacity(64);
        let started = Instant::now();
        for i in 0..rounds {
            let bytes = reads[i % reads.len()];
            out.clear();
            f.t.begin_read();
            out.extend_from_slice(bytes);
            f.t.feed(&out, 0);
            f.t.end_read(&mut out, f.now, false);
            // Answered at once, so it stays on and the hold ends.
            f.t.unanswered = None;
            f.t.hold = None;
        }
        let each = started.elapsed() / rounds as u32;
        assert!(each < Duration::from_micros(10), "{each:?} per read");
    }
}
