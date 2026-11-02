//! The poll loop that copies bytes between the terminal and the shell's PTY.

use std::cell::Cell;
use std::io::Read;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use nix::errno::Errno;
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::sys::signal::{Signal, kill};
use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::Pid;

use crate::complete::usage::{self, Usage};
use crate::complete::zsh_worker::{Worker, WorkerSpec};
use crate::guards::ShellKind;
use crate::popup::color;
use crate::popup::state::PopupController;
use crate::pty::{self, ShellProcess};
use crate::shell::protocol::{self, BufferReport, HookKey, ShellMessage, Token};
use crate::shell::trigger::Trigger;
use crate::specs::cache::SpecCache;
use crate::term::bulk::Bulk;
use crate::term::caps::{self, Probe, TermCaps};
use crate::term::cpr::{CprOwner, CprQueue};
use crate::term::ident::{self, TermKind};
use crate::term::input::{InputItem, InputTokenizer, TermReply};
use crate::term::replies::{Owner, ReplyQueues};
use crate::term::scanner::{OutputEvent, OutputScanner, PromptMark, Segment};
use crate::term::screen::ScreenModel;
use crate::wake::{self, WakeTimer};

const READ_CHUNK: usize = 64 * 1024;
/// Past this much unsent input, stdin is not read until the queue drains.
const MAX_OUTBOUND: usize = 1024 * 1024;
const HUP_GRACE: Duration = Duration::from_secs(2);
const KILL_GRACE: Duration = Duration::from_millis(500);
/// How often `waitpid` is retried once the slave side is gone, in case the
/// SIGCHLD came before the handler was registered.
const REAP_RETRY: u16 = 50;
/// How long a partial sequence on stdin waits for the rest before it is
/// passed on as typed.
const PENDING_FLUSH: Duration = Duration::from_millis(10);
/// Past this much held output the startup hold ends early.
const MAX_HELD: usize = 1024 * 1024;
/// fac's own cursor position query.
const CPR_QUERY: &[u8] = b"\x1b[6n";
/// After the shell exits, what is left in the master is forwarded for at
/// most this long or this many bytes: a process the shell left behind can
/// keep the master readable.
const DRAIN_TIME: Duration = Duration::from_millis(500);
const DRAIN_BYTES: usize = 64 * 1024 * 1024;
/// The common specs load once no key has come for this long, after the
/// first buffer report or prompt.
const PREWARM_IDLE: Duration = Duration::from_millis(300);

/// Set when a guarded handler panicked; the session is pure passthrough from then on.
static DEGRADED: AtomicBool = AtomicBool::new(false);
thread_local! {
    /// True while a guarded handler runs on this thread, so the panic hook
    /// (which runs on the panicking thread) leaves raw mode alone.
    static IN_GUARDED: Cell<bool> = const { Cell::new(false) };
    /// True on a background thread (the spec thread): a panic there must not
    /// touch the terminal at all.
    static IN_BACKGROUND: Cell<bool> = const { Cell::new(false) };
}

/// Whether a guarded handler has panicked in this process.
pub fn degraded() -> bool {
    DEGRADED.load(Ordering::Relaxed)
}

/// Whether a guarded handler is running right now (read by the panic hook).
pub fn in_guarded() -> bool {
    IN_GUARDED.get()
}

/// Whether this thread is a background thread (read by the panic hook).
pub fn in_background() -> bool {
    IN_BACKGROUND.get()
}

/// Mark this thread as a background thread, for good.
pub fn mark_background() {
    IN_BACKGROUND.set(true);
}

/// What the panic hook does to the terminal for a panic on this thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PanicPlan {
    /// Write the restore buffer (sync off, popup erase, cursor on) to fd 1.
    pub restore_screen: bool,
    /// Put fd 0's saved termios back.
    pub restore_termios: bool,
}

/// The panic hook's plan for this thread: a background thread leaves the
/// terminal alone; a guarded handler's panic is caught and the session goes
/// on, so raw mode stays on.
pub fn panic_plan() -> PanicPlan {
    let background = in_background();
    PanicPlan {
        restore_screen: !background,
        restore_termios: !background && !in_guarded(),
    }
}

/// A panic payload's message. It may quote what the user typed, so it is
/// only logged at debug.
pub fn panic_message(payload: &(dyn std::any::Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("non-string panic payload")
}

/// Run one per-event handler. A panic in it is logged and turns on degraded
/// mode instead of unwinding out of the proxy. Where it panicked is logged
/// by the panic hook.
fn guarded<R>(what: &str, f: impl FnOnce() -> R) -> Option<R> {
    let outer = IN_GUARDED.replace(true);
    let result = catch_unwind(AssertUnwindSafe(f));
    IN_GUARDED.set(outer);
    match result {
        Ok(value) => Some(value),
        Err(payload) => {
            tracing::error!("{what} panicked; degrading to passthrough");
            tracing::debug!("{what} panic payload: {}", panic_message(&*payload));
            DEGRADED.store(true, Ordering::Relaxed);
            None
        }
    }
}

/// The signals the loop reacts to, each with its own flag; one self-pipe
/// wakes `poll` for all of them.
struct Signals {
    wake: UnixStream,
    winch: Arc<AtomicBool>,
    chld: Arc<AtomicBool>,
    hup: Arc<AtomicBool>,
    term: Arc<AtomicBool>,
    /// SIGUSR1: log the screen model.
    usr1: Arc<AtomicBool>,
}

impl Signals {
    fn register() -> Result<Self> {
        let (wake, notify) = UnixStream::pair().context("signal self-pipe")?;
        wake.set_nonblocking(true)
            .context("non-blocking signal pipe")?;
        let flag = |signal: libc::c_int| -> Result<Arc<AtomicBool>> {
            let flag = Arc::new(AtomicBool::new(false));
            signal_hook::flag::register(signal, Arc::clone(&flag))
                .with_context(|| format!("registering signal {signal}"))?;
            signal_hook::low_level::pipe::register(signal, notify.try_clone()?)
                .with_context(|| format!("registering signal {signal} on the pipe"))?;
            Ok(flag)
        };
        Ok(Self {
            winch: flag(libc::SIGWINCH)?,
            chld: flag(libc::SIGCHLD)?,
            hup: flag(libc::SIGHUP)?,
            term: flag(libc::SIGTERM)?,
            usr1: flag(libc::SIGUSR1)?,
            wake,
        })
    }

    fn drain(&self) {
        let mut buf = [0u8; 64];
        while matches!((&self.wake).read(&mut buf), Ok(n) if n > 0) {}
    }
}

/// Everything fac knows about the terminal and the shell's output stream.
struct Term {
    scanner: OutputScanner,
    model: ScreenModel,
    /// Whether a command's output skips the model, and its tail meanwhile.
    bulk: Bulk,
    tokenizer: InputTokenizer,
    cprs: CprQueue,
    /// Who waits for each DECRPM, OSC 11/10/4 and DA1 reply.
    replies: ReplyQueues,
    probe: Probe,
    /// The shell's output, held back (not forwarded, not modelled) while the
    /// startup probe waits for its replies. `None` once released.
    held: Option<Vec<u8>>,
    /// Whether the probe's end (DA1 or the consume deadline) was handled.
    probe_done: bool,
    /// Whether the caps were logged with every reply in.
    caps_logged: bool,
    /// What goes to stdout next, in one write.
    out: Vec<u8>,
    /// When the tokenizer's partial sequence started waiting.
    pending_since: Option<Instant>,
    /// Context and command messages, for the compsys worker.
    worker_messages: Vec<ShellMessage>,
    popup: Popup,
    /// The caps the popup was last given.
    popup_caps: Option<TermCaps>,
    colorterm: Option<String>,
    prewarm: Prewarm,
    /// A prompt started (OSC 133;A, or the line editor's start without
    /// one) since the spec cache last heard of one.
    prompted: bool,
    /// The prompt being drawn or edited has had its reconcile point: a
    /// 133;A, or fac's CPR at the line editor's start. Cleared by 133;D.
    prompt_marked: bool,
    /// bash only (`None` for the other shells): the prompt's 133;B came.
    /// bash redraws the prompt after every `bind -x` (each report), and
    /// its B with it; only the first B of a prompt counts.
    bash_prompt_b: Option<bool>,
    /// Whether the terminal's ANSI colours were logged, once the probe's
    /// answers were in.
    ansi_logged: bool,
    /// The report trigger (bash and fish; inert for zsh).
    trigger: Trigger,
    /// The session token: only a 779x that carries it is the hook's. `None`
    /// (no shell integration) believes none.
    token: Option<Token>,
    /// The seqs of the hook's `look`s not answered yet.
    looks: Vec<u64>,
}

/// When to load the common specs: once, [`PREWARM_IDLE`] after the first
/// buffer report or prompt, pushed back by every key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Prewarm {
    Waiting,
    At(Instant),
    Done,
}

impl Prewarm {
    /// A buffer report or a prompt came.
    fn arm(&mut self, now: Instant) {
        if *self == Prewarm::Waiting {
            *self = Prewarm::At(now + PREWARM_IDLE);
        }
    }

    /// A key came.
    fn on_key(&mut self, now: Instant) {
        if matches!(self, Prewarm::At(_)) {
            *self = Prewarm::At(now + PREWARM_IDLE);
        }
    }

    fn deadline(&self) -> Option<Instant> {
        match self {
            Prewarm::At(at) => Some(*at),
            Prewarm::Waiting | Prewarm::Done => None,
        }
    }

    /// Whether it is due at `now`; true once.
    fn take_due(&mut self, now: Instant) -> bool {
        let due = matches!(self, Prewarm::At(at) if now >= *at);
        if due {
            *self = Prewarm::Done;
        }
        due
    }
}

/// The popup controller and what it reaches the shell with.
struct Popup {
    /// `None` once it panicked: off for the rest of the session.
    controller: Option<PopupController>,
    /// fac's end of the command pipe to the shell.
    commands: Option<CommandChannel>,
    /// Typed bytes the controller held when it was turned off, for the shell.
    orphaned: Vec<u8>,
}

impl Popup {
    /// Run `f` on the controller inside `guarded`. A panic turns the popup
    /// off (and the session to passthrough).
    fn call<R>(
        &mut self,
        model: &mut ScreenModel,
        out: &mut Vec<u8>,
        f: impl FnOnce(&mut PopupController, &mut ScreenModel, &mut Vec<u8>) -> R,
    ) -> Option<R> {
        let controller = self.controller.as_mut()?;
        let result = guarded("popup", || f(controller, model, out));
        if result.is_none() {
            self.disable(model, out, usize::MAX);
        }
        result
    }

    /// User bytes from stdin (or let go by an apply hold), read at `now`.
    /// `with_line`: an apply carries the line fac expects (bash, fish).
    /// Returns whether an apply was queued on the command pipe.
    fn on_user(
        &mut self,
        bytes: &[u8],
        model: &mut ScreenModel,
        out: &mut Vec<u8>,
        outbound: &mut Vec<u8>,
        with_line: bool,
        now: Instant,
    ) -> bool {
        let Some(controller) = self.controller.as_mut() else {
            outbound.extend_from_slice(bytes);
            return false;
        };
        let before = outbound.len();
        let held = controller.held_input_len();
        let commands = &mut self.commands;
        let mut queued = false;
        let mut send = |seq: u64, line: &str, lbuffer: &str, rbuffer: &str| match commands {
            Some(channel) => {
                channel.send_apply(seq, lbuffer, rbuffer, with_line.then_some(line));
                queued = true;
                true
            }
            None => false,
        };
        let handled = guarded("popup input", || {
            controller.on_user(bytes, model, out, outbound, &mut send, now);
        });
        if handled.is_none() {
            // How far it got is unknown: these bytes go again, whole.
            outbound.truncate(before);
            self.disable(model, out, held);
            outbound.append(&mut self.orphaned);
            outbound.extend_from_slice(bytes);
        }
        queued
    }

    /// Whether a popup key waits for the shell's report.
    fn holds_key(&self) -> bool {
        self.controller
            .as_ref()
            .is_some_and(PopupController::holds_key)
    }

    /// Turn the controller off: erase its popup if it is drawn (best
    /// effort) and keep the first `keep` bytes it held for the shell.
    fn disable(&mut self, model: &ScreenModel, out: &mut Vec<u8>, keep: usize) {
        let Some(mut controller) = self.controller.take() else {
            return;
        };
        tracing::error!("the popup is off for the rest of the session");
        if let Some((erase, input)) = guarded("popup salvage", || controller.salvage(model, keep)) {
            out.extend_from_slice(&erase);
            self.orphaned.extend_from_slice(&input);
        }
    }

    fn holds_output(&self) -> bool {
        self.controller
            .as_ref()
            .is_some_and(PopupController::holds_output)
    }
}

/// What the shell's messages and markers go to.
struct Listeners<'a> {
    worker_messages: &'a mut Vec<ShellMessage>,
    popup: &'a mut Popup,
    trigger: &'a mut Trigger,
    prewarm: &'a mut Prewarm,
    prompted: &'a mut bool,
    prompt_marked: &'a mut bool,
    bash_prompt_b: &'a mut Option<bool>,
    token: Option<&'a Token>,
    looks: &'a mut Vec<u64>,
}

impl Term {
    /// `caps::probe_for(kind)` is written to the terminal at `now`.
    /// `shell`: the shell fac runs. `commands`: fac's end of the command
    /// pipe, when the shell integration is on.
    fn new(
        rows: u16,
        cols: u16,
        kind: TermKind,
        shell: ShellKind,
        now: Instant,
        commands: Option<CommandChannel>,
    ) -> Self {
        let mut cprs = CprQueue::new();
        cprs.push(CprOwner::Startup, now);
        let mut replies = ReplyQueues::new();
        replies.push_ours_for(caps::probe_for(kind));
        let colorterm = std::env::var("COLORTERM").ok();
        let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from);
        let cwd = std::env::current_dir().unwrap_or_else(|_| home.clone());
        let caps = TermCaps {
            kind,
            ..TermCaps::default()
        };
        let controller = PopupController::new(shell, caps, colorterm.as_deref(), cwd, home);
        Self {
            scanner: OutputScanner::new(),
            model: ScreenModel::new(rows, cols),
            bulk: Bulk::new(),
            tokenizer: InputTokenizer::new(),
            cprs,
            replies,
            probe: Probe::new(now, kind),
            held: Some(Vec::new()),
            probe_done: false,
            caps_logged: false,
            out: Vec::new(),
            pending_since: None,
            worker_messages: Vec::new(),
            popup: Popup {
                controller: Some(controller),
                commands,
                orphaned: Vec::new(),
            },
            popup_caps: None,
            colorterm,
            prewarm: Prewarm::Waiting,
            prompted: false,
            prompt_marked: false,
            bash_prompt_b: (shell == ShellKind::Bash).then_some(false),
            ansi_logged: false,
            trigger: Trigger::new(shell),
            // The unit tests' hook messages carry the tests' token.
            #[cfg(test)]
            token: Some(Token::for_tests()),
            #[cfg(not(test))]
            token: None,
            looks: Vec::new(),
        }
    }

    /// One read from the master: held during the startup probe, else scanned
    /// into `out`. `inject`: whether fac may add its own CPR queries.
    fn on_output(&mut self, bytes: &[u8], now: Instant, inject: bool) {
        if let Some(held) = &mut self.held {
            held.extend_from_slice(bytes);
            if held.len() > MAX_HELD {
                self.release(now, inject);
            }
            return;
        }
        self.trigger.on_output(now);
        self.trigger.on_shell_text(bytes, now);
        let Term {
            scanner,
            model,
            bulk,
            cprs,
            replies,
            out,
            worker_messages,
            popup,
            prewarm,
            prompted,
            prompt_marked,
            bash_prompt_b,
            trigger,
            token,
            looks,
            ..
        } = self;
        // In bulk mode the popup is idle: nothing of it runs per read.
        if !bulk.active() {
            // A drawn popup stays; its erase may need the model as it is
            // before this read.
            popup.call(model, out, |controller, model, out| {
                controller.before_output(model, out, now);
            });
        }
        // Watched while a drawn popup waits for its paint: output that is
        // not line-local erases it first.
        scanner.set_watch(
            popup
                .controller
                .as_ref()
                .is_some_and(PopupController::watching),
        );
        let mut listeners = Listeners {
            worker_messages,
            popup,
            trigger,
            prewarm,
            prompted,
            prompt_marked,
            bash_prompt_b,
            token: token.as_ref(),
            looks,
        };
        scanner.feed(bytes, &mut |segment| match segment {
            Segment::Bytes(bytes) => {
                let mut idle = true;
                if let Some(controller) = &mut listeners.popup.controller
                    && !bulk.active()
                {
                    controller.on_shell_bytes(now);
                    idle = !controller.is_open() && !controller.holds_output();
                }
                bulk.process(bytes, model, idle, now);
                out.extend_from_slice(bytes);
                if let Some(controller) = &mut listeners.popup.controller
                    && controller.watching()
                {
                    controller.after_shell_bytes(model, out);
                }
            }
            Segment::Event(event) => {
                // The model is current before anything reads it.
                bulk.flush(model);
                bulk.on_event(&event);
                on_event(
                    event,
                    model,
                    cprs,
                    replies,
                    &mut listeners,
                    out,
                    now,
                    inject,
                );
            }
        });
        scanner.set_watch(false);
        if let Some(controller) = &mut popup.controller {
            controller.end_output();
        }
        self.trigger.settle(now);
        self.frame_released();
    }

    /// The trigger's output hold ended: what it held goes out as one frame.
    fn frame_released(&mut self) {
        if self.trigger.take_released() && !self.out.is_empty() {
            self.popup
                .call(&mut self.model, &mut self.out, |controller, _, _| {
                    controller.wrap_frame();
                });
        }
    }

    /// After a read of output: the trigger that starts a line, if one is
    /// due, and a typeahead mark a prompt asked for, if nothing waits to be
    /// written to the shell.
    fn after_output(&mut self, outbound: &mut Vec<u8>, now: Instant) {
        self.trigger.inject_due(outbound, now);
        if outbound.is_empty() {
            self.send_mark(outbound, now);
        }
    }

    /// The command pipe wrote everything queued.
    fn on_commands_written(&mut self, outbound: &mut Vec<u8>, now: Instant) {
        self.trigger.on_commands_written(outbound, now);
    }

    /// An apply was queued for a shell that reads it on its apply trigger:
    /// the pipe is written now, so the trigger can follow it at once.
    fn on_apply_queued(&mut self, outbound: &mut Vec<u8>, now: Instant) {
        if !self.trigger.wants_apply_trigger() {
            return;
        }
        self.trigger.on_apply_queued();
        self.flush_commands(outbound, now);
    }

    /// Bytes were written to the shell, and `outbound` is what is left;
    /// with nothing left to write, the typeahead mark, if one is due
    /// (`shell::trigger`, "Typeahead").
    fn on_input_written(&mut self, outbound: &mut Vec<u8>, now: Instant) {
        if outbound.is_empty() {
            self.send_mark(outbound, now);
        }
    }

    /// The typeahead mark on the command pipe, if one is due. Only once
    /// everything for the shell is written.
    fn send_mark(&mut self, outbound: &mut Vec<u8>, now: Instant) {
        if !self.trigger.take_mark() {
            return;
        }
        let Some(channel) = &mut self.popup.commands else {
            return;
        };
        channel.send_mark();
        self.flush_commands(outbound, now);
    }

    /// Answer the hook's `look`s (`shell::trigger`, "Typeahead"): the
    /// bytes written to the shell so far, and how many of its input are
    /// unread, which `measure` counts. The hook waits for the answer, so
    /// the shell reads nothing meanwhile.
    fn answer_looks(
        &mut self,
        outbound: &mut Vec<u8>,
        now: Instant,
        mut measure: impl FnMut() -> Option<u64>,
    ) {
        if self.looks.is_empty() {
            return;
        }
        let looks = std::mem::take(&mut self.looks);
        let Some(channel) = &mut self.popup.commands else {
            return;
        };
        for seq in looks {
            let unread = measure();
            if unread.is_none() {
                tracing::debug!("the shell's input could not be counted");
            }
            channel.send_looked(seq, self.trigger.written(), unread.unwrap_or(u64::MAX));
        }
        self.flush_commands(outbound, now);
    }

    /// Write what the command pipe takes now; once it has everything, an
    /// apply's trigger may follow.
    fn flush_commands(&mut self, outbound: &mut Vec<u8>, now: Instant) {
        let Some(channel) = &mut self.popup.commands else {
            return;
        };
        match channel.flush() {
            Ok(()) if !channel.pending() => self.trigger.on_commands_written(outbound, now),
            Ok(()) => {}
            Err(err) => {
                tracing::info!("command pipe closed ({err}); no more commands to the shell");
                self.popup.commands = None;
            }
        }
    }

    /// User bytes from a timer (a partial sequence that waited, or what an
    /// apply hold let go): handled as a read of their own.
    fn timer_user(&mut self, bytes: &[u8], outbound: &mut Vec<u8>, now: Instant) {
        self.trigger.begin_read();
        let before = outbound.len();
        let with_line = self.trigger.wants_apply_trigger();
        let queued = self.popup.on_user(
            bytes,
            &mut self.model,
            &mut self.out,
            outbound,
            with_line,
            now,
        );
        self.trigger.feed(&outbound[before..], before);
        if queued {
            self.on_apply_queued(outbound, now);
        }
        let key_held = self.popup.holds_key();
        self.trigger.end_read(outbound, now, key_held);
        self.after_suspend();
    }

    /// bash and fish: a key that suspended the trigger hands the keys to a
    /// search, a widget or a program, and no report comes until it is done
    /// (in fish, until the next prompt), so the popup closes now.
    fn after_suspend(&mut self) {
        if self.trigger.take_suspended() {
            let Term {
                popup, model, out, ..
            } = self;
            popup.call(model, out, |controller, model, out| {
                controller.on_shell_reads_keys(model, out);
            });
        }
    }

    /// One read from stdin: user bytes and forwarded CPR replies go to `outbound`.
    fn on_input(&mut self, bytes: &[u8], now: Instant, outbound: &mut Vec<u8>) {
        // Replies and keys read the model.
        self.bulk.flush(&mut self.model);
        self.cprs.expire(now);
        if !self.probe.consuming(now) {
            // The tick that ends the window may not have run yet.
            self.replies.expire_ours();
        }
        // Only while the probe is outstanding, so a partial sequence is
        // held no more often than before reply kinds were told apart.
        let expect = self.replies.expect();
        let cpr_outstanding = !self.cprs.is_empty();
        let partial_keys = self
            .popup
            .controller
            .as_ref()
            .is_some_and(PopupController::is_open);
        self.trigger.begin_read();
        let with_line = self.trigger.wants_apply_trigger();
        let mut queued = false;
        let Term {
            tokenizer,
            model,
            cprs,
            replies,
            probe,
            popup,
            out,
            prewarm,
            trigger,
            ..
        } = self;
        tokenizer.feed(
            bytes,
            &expect,
            cpr_outstanding,
            partial_keys,
            &mut |item| match item {
                InputItem::User(bytes) => {
                    prewarm.on_key(now);
                    let before = outbound.len();
                    queued |= popup.on_user(bytes, model, out, outbound, with_line, now);
                    trigger.feed(&outbound[before..], before);
                }
                InputItem::Reply { reply, raw } => {
                    let device_attrs = matches!(reply, TermReply::DeviceAttrs);
                    let before = outbound.len();
                    let trusted = on_reply(reply, raw, model, cprs, replies, probe, outbound, now);
                    if device_attrs && outbound.len() > before {
                        trigger.on_device_attrs_forwarded();
                    }
                    if trusted {
                        popup.call(model, out, |controller, model, _| {
                            controller.on_model_clean(model, now);
                        });
                    }
                }
            },
        );
        if queued {
            self.on_apply_queued(outbound, now);
        }
        let key_held = self.popup.holds_key();
        self.trigger.end_read(outbound, now, key_held);
        self.after_suspend();
        self.pending_since = self.tokenizer.has_pending().then_some(now);
        self.sync_caps();
    }

    /// Timers: the end of the startup hold, the probe's end, the flush of
    /// a partial sequence on stdin and of quiet bulk output.
    fn tick(&mut self, now: Instant, outbound: &mut Vec<u8>) {
        if self.bulk.deadline().is_some_and(|deadline| now >= deadline) {
            self.bulk.flush(&mut self.model);
        }
        if self.held.is_some() && !self.probe.holding(now) {
            self.release(now, true);
        }
        if !self.probe_done && !self.probe.consuming(now) {
            self.probe_done = true;
            if self.cprs.remove_startup() {
                tracing::debug!("startup cpr never answered");
            }
            self.replies.expire_ours();
            if !self.caps_logged && self.probe.answered() {
                // The hold ended before the replies came.
                self.caps_logged = true;
                log_caps(&self.probe);
            }
        }
        if self
            .pending_since
            .is_some_and(|since| now >= since + PENDING_FLUSH)
        {
            self.pending_since = None;
            if let Some(pending) = self.tokenizer.take_pending() {
                // A lone ESC that waited this long is the Esc key.
                self.timer_user(&pending, outbound, now);
            }
        }
        self.cprs.expire(now);
        let Term {
            popup, model, out, ..
        } = self;
        let released = popup.call(model, out, |controller, model, out| {
            controller.tick(model, out, now);
            controller.take_released()
        });
        if let Some(released) = released.filter(|released| !released.is_empty()) {
            self.timer_user(&released, outbound, now);
        }
        self.trigger.tick(outbound, now);
        self.after_suspend();
        // The startup hold's release may have opened the trigger's gate.
        self.trigger.inject_due(outbound, now);
        self.frame_released();
        self.sync_caps();
    }

    /// Give the popup the probe's answers when they change; it builds its
    /// theme from them.
    fn sync_caps(&mut self) {
        let caps = self.probe.caps;
        if !self.ansi_logged && (self.probe.answered() || self.probe_done) {
            self.ansi_logged = true;
            log_ansi(&caps);
        }
        if self.popup_caps == Some(caps) {
            return;
        }
        self.popup_caps = Some(caps);
        let colorterm = self.colorterm.as_deref();
        self.popup
            .call(&mut self.model, &mut self.out, |controller, _, _| {
                controller.set_caps(caps, colorterm);
            });
    }

    /// Ask the worker about the latest buffer and take its answers.
    fn sync_compsys(&mut self, worker: Option<&mut Worker>) {
        if self.bulk.active() {
            return;
        }
        self.popup
            .call(&mut self.model, &mut self.out, |controller, model, _| {
                controller.sync_compsys(worker, model, Instant::now());
            });
    }

    /// Hand the popup the spec cache: what the latest report and compute
    /// need is asked for, and what came is taken in. A prompt since the last
    /// time is passed on first, so user specs are rechecked at their next ask.
    fn sync_specs(&mut self, specs: &mut SpecCache) {
        if std::mem::take(&mut self.prompted) {
            specs.on_prompt();
        }
        if self.bulk.active() {
            return;
        }
        self.popup
            .call(&mut self.model, &mut self.out, |controller, model, _| {
                controller.sync_specs(specs, model, Instant::now());
            });
    }

    /// Send the popup's due jobs to the spec thread (debounced generator
    /// runs, the usage save). Not held back by bulk output: a save that is
    /// due is a deadline that must be met.
    fn sync_jobs(&mut self, specs: &mut SpecCache) {
        self.popup
            .call(&mut self.model, &mut self.out, |controller, _, _| {
                controller.sync_jobs(specs, Instant::now());
            });
    }

    /// The terminal's new size, before the model takes it.
    fn on_resize(&mut self, rows: u16, cols: u16) {
        self.bulk.flush(&mut self.model);
        let width_changed = self.model.screen().size().1.get() != cols.max(1);
        self.popup
            .call(&mut self.model, &mut self.out, |controller, model, out| {
                controller.on_resize(model, out, width_changed);
            });
        self.model.resize(rows, cols);
    }

    /// Whether `out` waits for the popup's paint, or for the shell's answer
    /// to the report trigger.
    fn holds_output(&self) -> bool {
        !degraded() && (self.popup.holds_output() || self.trigger.holds_output())
    }

    /// End the startup hold: log what the probe found and pass the held
    /// output on through the normal path.
    fn release(&mut self, now: Instant, inject: bool) {
        let Some(held) = self.held.take() else {
            return;
        };
        self.caps_logged = self.probe.answered();
        log_caps(&self.probe);
        self.on_output(&held, now, inject);
    }

    /// When `tick` next has something to do.
    fn deadline(&self) -> Option<Instant> {
        let hold = self.held.is_some().then(|| self.probe.hold_deadline());
        let probe = (!self.probe_done).then(|| self.probe.consume_deadline());
        let pending = self.pending_since.map(|since| since + PENDING_FLUSH);
        let bulk = self.bulk.deadline();
        let popup = self
            .popup
            .controller
            .as_ref()
            .and_then(PopupController::deadline);
        let prewarm = self.prewarm.deadline();
        let trigger = self.trigger.deadline();
        [hold, probe, pending, bulk, popup, prewarm, trigger]
            .into_iter()
            .flatten()
            .min()
    }

    /// Degraded (after a panic): the popup is turned off (its erase goes
    /// out, best effort), then the output fac still holds goes out raw.
    /// `out` must hold only whole reads.
    fn degrade_output(&mut self, stdout: BorrowedFd<'_>) -> nix::Result<()> {
        self.popup.disable(&self.model, &mut self.out, usize::MAX);
        self.close_commands();
        let out = std::mem::take(&mut self.out);
        write_all(stdout, &out)?;
        match self.held.take() {
            Some(held) => write_all(stdout, &held),
            None => Ok(()),
        }
    }

    /// Degraded, fac reads no more of the hook's messages: the command
    /// pipe closes, so a hook waiting on it for an answer (`look`) reads
    /// its end instead of waiting for good.
    fn close_commands(&mut self) {
        if self.popup.commands.take().is_some() {
            tracing::info!("command pipe closed: degraded");
        }
    }

    /// Whether the common specs are due to load now; true once.
    fn take_prewarm(&mut self, now: Instant) -> bool {
        self.prewarm.take_due(now)
    }

    /// Feed the model the bulk output it skipped.
    fn flush_bulk(&mut self) {
        self.bulk.flush(&mut self.model);
    }

    /// SIGUSR1: log the screen model (at debug), after a bulk flush.
    fn dump_model(&mut self) {
        self.flush_bulk();
        let screen = self.model.screen();
        let rows: Vec<String> = screen
            .rows(0, screen.size().1.get())
            .map(|row| row.trim_end().to_owned())
            .collect();
        // Screen contents: debug only.
        tracing::debug!("model dump cursor={:?} rows={rows:?}", self.model.cursor());
    }

    /// Degraded (after a panic): input fac still holds goes to the shell.
    fn degrade_input(&mut self, outbound: &mut Vec<u8>) {
        self.popup.disable(&self.model, &mut self.out, usize::MAX);
        self.close_commands();
        outbound.append(&mut self.popup.orphaned);
        self.pending_since = None;
        if let Some(pending) = self.tokenizer.take_pending() {
            outbound.extend_from_slice(&pending);
        }
    }
}

/// The terminal's ANSI colours, as the probe found them.
fn log_ansi(caps: &TermCaps) {
    let ansi = caps.ansi;
    let show = |rgb: Option<(u8, u8, u8)>| rgb.map_or_else(|| "none".to_owned(), color::hex);
    tracing::debug!(
        "caps ansi green={} yellow={} blue={} magenta={} cyan={}",
        show(ansi.green),
        show(ansi.yellow),
        show(ansi.blue),
        show(ansi.magenta),
        show(ansi.cyan)
    );
}

fn log_caps(probe: &Probe) {
    let caps = probe.caps;
    tracing::info!(
        "caps sync={} dark={} bg={:?}",
        caps.sync_output,
        caps.dark,
        caps.background
    );
}

#[expect(
    clippy::too_many_arguments,
    reason = "the parts of `Term` an event touches"
)]
fn on_event(
    event: OutputEvent,
    model: &mut ScreenModel,
    cprs: &mut CprQueue,
    replies: &mut ReplyQueues,
    listeners: &mut Listeners<'_>,
    out: &mut Vec<u8>,
    now: Instant,
    inject: bool,
) {
    let popup = &mut *listeners.popup;
    if let (OutputEvent::Prompt(mark), Some(seen)) = (&event, listeners.bash_prompt_b.as_mut()) {
        if *mark == PromptMark::B {
            if *seen {
                tracing::trace!("prompt mark B again (bash's redraw); ignored");
                return;
            }
            *seen = true;
        } else {
            *seen = false;
        }
    }
    match event {
        OutputEvent::Private { code, mut fields } => {
            // Before anything of it is read: any program can print one.
            if !protocol::authenticate(&mut fields, listeners.token) {
                tracing::debug!("private osc code={code} dropped: not the hook's");
                return;
            }
            match protocol::decode(code, &fields) {
                Ok(message) => {
                    log_message(&message);
                    match &message {
                        ShellMessage::Buffer(report) => listeners.trigger.on_report(report, now),
                        ShellMessage::Context { seq, key, value } => {
                            match HookKey::decode(key, value) {
                                Some(Ok(HookKey::Look)) => {
                                    tracing::debug!("the hook asks for a look at its input");
                                    listeners.looks.push(*seq);
                                    listeners.trigger.on_hook_key(HookKey::Look);
                                }
                                Some(Ok(hook)) => listeners.trigger.on_hook_key(hook),
                                Some(Err(err)) => {
                                    tracing::debug!("context key={key} ignored: {err}");
                                }
                                None => {}
                            }
                        }
                        _ => {}
                    }
                    if let ShellMessage::Buffer(report) = &message {
                        listeners.prewarm.arm(now);
                        let line_start = line_start(report, listeners.prompt_marked);
                        if line_start {
                            *listeners.prompted = true;
                        }
                        if inject && (line_start || model.is_dirty()) && !cprs.has_ours() {
                            out.extend_from_slice(CPR_QUERY);
                            cprs.push(
                                CprOwner::Ours {
                                    model_cursor_at_query: model.cursor(),
                                },
                                now,
                            );
                        }
                    }
                    popup.call(model, out, |controller, model, out| match &message {
                        ShellMessage::Buffer(report) => {
                            controller.on_report(report, model, out, now);
                        }
                        ShellMessage::Context { key, value, .. } => {
                            controller.on_context(key, value);
                        }
                        ShellMessage::Command { line, .. } => controller.on_command(line),
                        ShellMessage::Diagnostic { .. } => {}
                    });
                    if matches!(
                        message,
                        ShellMessage::Context { .. } | ShellMessage::Command { .. }
                    ) {
                        listeners.worker_messages.push(message);
                    }
                }
                Err(err) => tracing::debug!("private osc code={code} ignored: {err}"),
            }
        }
        OutputEvent::Prompt(PromptMark::A) => {
            tracing::debug!("prompt mark A");
            listeners.prewarm.arm(now);
            *listeners.prompted = true;
            *listeners.prompt_marked = true;
            if inject {
                // Right after the marker, so the reply is compared with the
                // model's cursor at this exact byte.
                out.extend_from_slice(CPR_QUERY);
                cprs.push(
                    CprOwner::Ours {
                        model_cursor_at_query: model.cursor(),
                    },
                    now,
                );
            }
        }
        OutputEvent::Prompt(mark) => {
            tracing::debug!("prompt mark {mark:?}");
            if matches!(mark, PromptMark::D(_)) {
                *listeners.prompt_marked = false;
                // A line can end with no 133;C (bash's Ctrl-C): the line
                // editor is done with it all the same.
                listeners.trigger.on_command();
            }
            if mark == PromptMark::B {
                listeners.trigger.on_prompt();
            }
            if mark == PromptMark::C {
                listeners.trigger.on_line_run();
                listeners.trigger.on_command();
                popup.call(model, out, |controller, model, out| {
                    controller.on_command_start(model, out);
                });
            }
        }
        OutputEvent::ShellCursorQuery => {
            tracing::debug!("shell cursor query");
            cprs.push(CprOwner::Shell, now);
        }
        OutputEvent::ShellQueries(kinds) => {
            // Pushed as the scanner meets them, which is the order they are
            // written in: held output is only scanned when it is released,
            // and `out` goes to the terminal first in, first out.
            for kind in kinds {
                tracing::debug!(?kind, "shell query");
                replies.push_shell(kind);
            }
        }
        OutputEvent::AltScreen(on) => {
            tracing::info!("alt_screen={}", if on { "on" } else { "off" });
            listeners.trigger.on_alt_screen(on);
            popup.call(model, out, |controller, model, out| {
                controller.on_alt_screen(on, model, out);
            });
        }
        OutputEvent::Cwd(path) => {
            // Any program can print an OSC 7: with the shell integration
            // on, fac's cwd is the hook's token-checked `cwd` key alone.
            // The OSC still reaches the terminal as it came.
            if listeners.token.is_some() {
                tracing::debug!("osc 7 passed on; the hook's cwd is fac's");
            } else {
                tracing::debug!("cwd from osc 7");
                popup.call(model, out, |controller, _, _| controller.on_cwd(&path));
            }
        }
        OutputEvent::ShellCursorSaved(saved) => {
            tracing::debug!("shell cursor saved={saved}");
            model.set_shell_saved_cursor(saved);
        }
        OutputEvent::Moves(kind) => {
            popup.call(model, out, |controller, model, out| {
                controller.on_moves(kind, model, out);
            });
        }
    }
}

/// Whether `report` is the line editor's start for a prompt drawn without
/// fac's 133;A (it is then marked). A report sits at its place in the
/// stream: every byte before it went to the model and to the terminal
/// alike, as at a 133;A, so fac's CPR there reconciles the model just the
/// same, and the prompt counts as one for the spec cache.
///
/// A theme that rebuilds PS1 after fac wrapped it (in a precmd hook that
/// keeps itself last) draws its prompts without fac's 133;A. Its line
/// editor's first report (from zle-line-init, which zsh runs after drawing
/// the prompt and flushing it: `zleread`, zle_main.c:1342-1346, and the end
/// of `zrefresh`, zle_refresh.c:1804) stands in for the mark. Besides, while
/// the model is dirty (the width changed), any report asks, marks or not.
/// One of fac's CPRs at a time: a prompt that had its 133;A sends nothing
/// more.
fn line_start(report: &BufferReport, prompt_marked: &mut bool) -> bool {
    if *prompt_marked || report.context != "start" || report.flags.finished {
        return false;
    }
    tracing::debug!("line editor started without a prompt mark");
    *prompt_marked = true;
    true
}

fn log_message(message: &ShellMessage) {
    match message {
        ShellMessage::Buffer(report) => tracing::debug!(
            "report seq={} ctx={} cursor={} flags={} buffer={:?} prebuffer={:?}",
            report.seq,
            report.context,
            report.cursor,
            report.flags,
            report.buffer,
            report.prebuffer
        ),
        ShellMessage::Context { key, value, .. } => {
            // Chars of a text, elements of a list.
            let len = match &value {
                protocol::ContextValue::Text(text) => text.chars().count(),
                protocol::ContextValue::List(items) => items.len(),
            };
            tracing::debug!("context key={key} len={len}");
        }
        ShellMessage::Command { line, .. } => tracing::debug!("command {line:?}"),
        ShellMessage::Diagnostic { code, .. } => tracing::debug!("diagnostic {code}"),
    }
}

/// A terminal reply; `raw`: its bytes as they came, which a reply to the
/// shell's query is forwarded as. Returns whether the reply made a dirty
/// model trusted again.
#[expect(
    clippy::too_many_arguments,
    reason = "the parts of `Term` a reply touches"
)]
fn on_reply(
    reply: TermReply,
    raw: &[u8],
    model: &mut ScreenModel,
    cprs: &mut CprQueue,
    replies: &mut ReplyQueues,
    probe: &mut Probe,
    outbound: &mut Vec<u8>,
    now: Instant,
) -> bool {
    match reply {
        TermReply::CursorPos { row, col } => match cprs.pop(now) {
            Some(CprOwner::Ours {
                model_cursor_at_query,
            }) => reconcile(
                model,
                cprs,
                model_cursor_at_query,
                (row, col),
                "cpr mismatch",
            ),
            // The model started at the origin before any output.
            Some(CprOwner::Startup) => reconcile(model, cprs, (0, 0), (row, col), "cpr seed"),
            Some(CprOwner::Shell) | None => {
                tracing::debug!(row, col, "cpr forwarded to the shell");
                outbound.extend_from_slice(raw);
                false
            }
        },
        reply => {
            let kind = reply.kind();
            let owner = match kind {
                Some(kind) => replies.pop(kind),
                None => replies.pop_palette(),
            };
            if owner == Some(Owner::Ours) {
                on_our_reply(reply, cprs, probe);
            } else {
                tracing::debug!(?kind, "reply forwarded to the shell");
                outbound.extend_from_slice(raw);
            }
            false
        }
    }
}

/// A reply to fac's own probe.
fn on_our_reply(reply: TermReply, cprs: &mut CprQueue, probe: &mut Probe) {
    match reply {
        // Handled by `on_reply`.
        TermReply::CursorPos { .. } => {}
        TermReply::Mode { mode: 2026, ps } => probe.caps.sync_output = caps::sync_supported(ps),
        TermReply::Mode { .. } => {
            tracing::debug!(kind = ?reply.kind(), "reply to fac not used");
        }
        TermReply::Background(rgb) => probe.caps.set_background(rgb),
        TermReply::Palette(index, rgb) => probe.caps.set_palette(index, rgb),
        // Consumed so it never reaches the shell; nothing in it to use.
        TermReply::Malformed(_) => tracing::debug!("malformed osc 4 reply dropped"),
        TermReply::DeviceAttrs => {
            probe.on_device_attrs();
            // Terminals answer in order: a CPR not answered by now never will be.
            if cprs.remove_startup() {
                tracing::debug!("startup cpr not answered before DA1");
            }
        }
    }
}

/// Compare a CPR reply (1-based) with the model's cursor when the query was
/// written. On a difference, shift the model's cursor by the same amount,
/// and the snapshots of fac's queries still in `cprs` with it: they were
/// taken from the model before the correction. Returns whether the model
/// was dirty (it is trusted now).
fn reconcile(
    model: &mut ScreenModel,
    cprs: &mut CprQueue,
    at_query: (u16, u16),
    reply: (u16, u16),
    what: &str,
) -> bool {
    let real = (reply.0.saturating_sub(1), reply.1.saturating_sub(1));
    if real == at_query {
        tracing::debug!("cpr ok");
    } else {
        let (rows, cols) = model.screen().size();
        let (row, col) = model.cursor();
        let shift = |now: u16, real: u16, then: u16, len: u16| {
            let moved = i32::from(now) + i32::from(real) - i32::from(then);
            // Clamped into 0..len, so it fits.
            moved.clamp(0, i32::from(len) - 1) as u16
        };
        let moved = (
            shift(row, real.0, at_query.0, rows.get()),
            shift(col, real.1, at_query.1, cols.get()),
        );
        model.set_cursor(moved.0, moved.1);
        cprs.shift_ours(
            (
                i32::from(moved.0) - i32::from(row),
                i32::from(moved.1) - i32::from(col),
            ),
            (rows.get(), cols.get()),
        );
        tracing::info!("{what} model={at_query:?} real={real:?}");
    }
    let was_dirty = model.is_dirty();
    model.clear_dirty();
    was_dirty
}

/// fac's end of the command pipe to the shell (fd 63 there), with what is
/// still to be written. The fd is non-blocking.
pub struct CommandChannel {
    fd: OwnedFd,
    queue: Vec<u8>,
}

impl CommandChannel {
    pub fn new(fd: OwnedFd) -> Self {
        Self {
            fd,
            queue: Vec::new(),
        }
    }

    /// Queue `apply`: the shell sets its command line to `lbuffer` +
    /// `rbuffer` if `seq` is still its latest report (and, for bash and
    /// fish, `expected` still its line).
    pub fn send_apply(&mut self, seq: u64, lbuffer: &str, rbuffer: &str, expected: Option<&str>) {
        self.queue
            .extend_from_slice(&protocol::encode_apply(seq, lbuffer, rbuffer, expected));
    }

    /// Queue `ta`: the hook looks at its input at its next prompt
    /// (`shell::trigger`, "Typeahead").
    pub fn send_mark(&mut self) {
        self.queue.extend_from_slice(b"ta\n");
    }

    /// Queue the answer to the hook's `look` `seq`: `mark` bytes were
    /// written to the shell, and `unread` of its input were unread.
    pub fn send_looked(&mut self, seq: u64, mark: u64, unread: u64) {
        self.queue
            .extend_from_slice(format!("looked {seq} {mark} {unread}\n").as_bytes());
    }

    /// Whether anything waits to be written.
    pub fn pending(&self) -> bool {
        !self.queue.is_empty()
    }

    /// Write what the pipe takes now. An error means the channel is dead.
    fn flush(&mut self) -> nix::Result<()> {
        while !self.queue.is_empty() {
            match nix::unistd::write(&self.fd, &self.queue) {
                Ok(n) => {
                    self.queue.drain(..n);
                }
                Err(Errno::EINTR) => {}
                Err(Errno::EAGAIN) => return Ok(()),
                Err(err) => return Err(err),
            }
        }
        Ok(())
    }
}

/// What ends the loop.
enum Exit {
    /// The shell exited with this status.
    Shell(i32),
    /// fac must take the shell down and exit with this status.
    Shutdown(i32),
}

/// Copy bytes both ways until the shell exits or the terminal goes away,
/// probing the terminal first and modelling the screen as output passes.
/// `shell` is a `shell_kind` shell. `cmd_writer`: fac's end of the command
/// pipe, `token`: the session token, and `worker`: the shell's completion
/// worker, when the shell integration is on. Returns the exit status fac
/// should exit with.
pub fn run(
    shell: ShellProcess,
    shell_kind: ShellKind,
    cmd_writer: Option<OwnedFd>,
    token: Option<Token>,
    worker: Option<WorkerSpec>,
) -> Result<i32> {
    let signals = Signals::register()?;
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let stdin = stdin.as_fd();
    let stdout = stdout.as_fd();
    let master = shell.master.as_fd();

    let size = pty::window_size(stdin);
    let commands = cmd_writer.map(CommandChannel::new);
    // The environment is read once, here; only the kind's name is logged.
    let kind = ident::from_env();
    tracing::info!("terminal kind={kind:?}");
    let mut term = Term::new(
        size.ws_row,
        size.ws_col,
        kind,
        shell_kind,
        Instant::now(),
        commands,
    );
    if let Ok(pid) = u32::try_from(shell.pid.as_raw()) {
        term.trigger.set_shell_pid(pid);
    }
    term.token = token;
    // Read once, here; written by the popup's timer and on exit below.
    if let Some(controller) = &mut term.popup.controller {
        controller.set_usage(Usage::load(usage::env_path()));
    }
    let mut outbound: Vec<u8> = Vec::new();
    let mut buf = vec![0u8; READ_CHUNK];
    let mut slave_gone = false;
    // Started once the shell has reported what its spec's start gate
    // names (zsh: PATH, fpath and the dump).
    let mut worker = worker.map(Worker::new);
    // Starts the spec thread; without one, fac runs without specs.
    let mut specs = SpecCache::new(shell_kind);
    // Without it, timers fall back to poll's millisecond timeout.
    let wake_timer = WakeTimer::new()
        .inspect_err(|err| tracing::info!("no microsecond timer ({err}); timers round to ms"))
        .ok();

    let exit = 'run: {
        if let Err(err) = write_all(stdout, caps::probe_for(kind)) {
            tracing::info!("writing the probe failed ({err}); terminal lost");
            break 'run Exit::Shutdown(129);
        }
        loop {
            if let Some(exit) =
                handle_signals(&signals, &shell, &mut term, &mut worker, slave_gone)?
            {
                break 'run exit;
            }
            if let Err(err) = tick(&mut term, stdout, &mut outbound) {
                tracing::info!("writing stdout failed ({err}); terminal lost");
                break 'run Exit::Shutdown(129);
            }
            // The startup hold's end may have let a `look` through.
            answer_looks(&mut term, master, &mut outbound);
            sync_jobs(&mut term, &mut specs);
            feed_worker(&mut term, &mut worker);
            prewarm_specs(&mut term, &mut specs);

            let read_stdin = outbound.len() < MAX_OUTBOUND;
            let mut master_events = PollFlags::POLLIN;
            if !outbound.is_empty() {
                master_events |= PollFlags::POLLOUT;
            }
            let stdin_events = if read_stdin {
                PollFlags::POLLIN
            } else {
                PollFlags::empty()
            };
            let mut fds = vec![
                PollFd::new(signals.wake.as_fd(), PollFlags::POLLIN),
                PollFd::new(stdin, stdin_events),
                PollFd::new(master, master_events),
            ];
            // The command pipe is polled only while something waits for it.
            let mut cmd_index = None;
            if let Some(channel) = term
                .popup
                .commands
                .as_ref()
                .filter(|channel| channel.pending())
            {
                cmd_index = Some(fds.len());
                fds.push(PollFd::new(channel.fd.as_fd(), PollFlags::POLLOUT));
            }
            let worker_fds = worker_poll_fds(&worker);
            let worker_index = fds.len();
            for &(fd, events) in &worker_fds {
                // SAFETY: the worker owns these fds and is not touched until
                // `fds` is dropped after poll, so they stay open meanwhile.
                fds.push(PollFd::new(unsafe { BorrowedFd::borrow_raw(fd) }, events));
            }
            let spec_fd = spec_poll_fd(&specs);
            let spec_index = fds.len();
            if let Some(fd) = spec_fd {
                // SAFETY: the spec cache owns this fd and is not touched until
                // `fds` is dropped after poll, so it stays open meanwhile.
                let fd = unsafe { BorrowedFd::borrow_raw(fd) };
                fds.push(PollFd::new(fd, PollFlags::POLLIN));
            }
            let deadline = [term_deadline(&term), worker_deadline(&worker)]
                .into_iter()
                .flatten()
                .min();
            // The microsecond timer wakes poll at the deadline; poll's own
            // timeout stays as a fallback. Whatever wakes it, the deadline
            // checks decide what fires.
            if let Some(timer) = &wake_timer {
                if let Err(err) = timer.set(deadline) {
                    tracing::debug!("setting the wake timer failed ({err})");
                }
                fds.push(PollFd::new(timer.as_fd(), PollFlags::POLLIN));
            }
            let timer = deadline.map(|deadline| {
                wake::poll_millis(deadline.saturating_duration_since(Instant::now()))
            });
            // Once the slave is gone the master would report POLLHUP forever, so
            // it leaves the set and waitpid is retried on a timer instead.
            let (polled, timeout) = if slave_gone {
                let millis = timer.map_or(REAP_RETRY, |timer| timer.min(REAP_RETRY));
                (&mut fds[..2], PollTimeout::from(millis))
            } else {
                (
                    &mut fds[..],
                    timer.map_or(PollTimeout::NONE, PollTimeout::from),
                )
            };
            match poll(polled, timeout) {
                Ok(_) | Err(Errno::EINTR) => {}
                Err(err) => return Err(err).context("poll"),
            }
            let revents = |i: usize| {
                fds.get(i)
                    .and_then(PollFd::revents)
                    .unwrap_or(PollFlags::empty())
            };
            let (wake_ev, stdin_ev, mut master_ev) = (revents(0), revents(1), revents(2));
            let cmd_ev = cmd_index.map_or(PollFlags::empty(), revents);
            let worker_ev: Vec<(RawFd, PollFlags)> = worker_fds
                .iter()
                .enumerate()
                .map(|(i, &(fd, _))| (fd, revents(worker_index + i)))
                .collect();
            let spec_ev = spec_fd.map_or(PollFlags::empty(), |_| revents(spec_index));
            drop(fds);
            if slave_gone {
                master_ev = PollFlags::empty();
            }

            if cmd_ev.intersects(PollFlags::POLLOUT | PollFlags::POLLHUP | PollFlags::POLLERR)
                && let Some(channel) = &mut term.popup.commands
            {
                match channel.flush() {
                    Ok(()) => {
                        if !channel.pending() {
                            commands_written(&mut term, &mut outbound);
                        }
                    }
                    Err(err) => {
                        tracing::info!(
                            "command pipe closed ({err}); no more commands to the shell"
                        );
                        term.popup.commands = None;
                    }
                }
            }

            if !wake_ev.is_empty() {
                signals.drain();
                // Signals first, so a resize lands before input typed after it.
                if let Some(exit) =
                    handle_signals(&signals, &shell, &mut term, &mut worker, slave_gone)?
                {
                    break 'run exit;
                }
            }

            if stdin_ev.intersects(PollFlags::POLLNVAL) {
                tracing::info!("stdin is invalid; terminal lost");
                break 'run Exit::Shutdown(129);
            }
            if stdin_ev.intersects(PollFlags::POLLIN | PollFlags::POLLHUP | PollFlags::POLLERR) {
                match nix::unistd::read(stdin, &mut buf) {
                    Ok(0) | Err(Errno::EIO) => {
                        tracing::info!("stdin closed; terminal lost");
                        break 'run Exit::Shutdown(129);
                    }
                    Ok(n) => on_input(&mut term, &buf[..n], &mut outbound),
                    Err(Errno::EAGAIN | Errno::EINTR) => {}
                    Err(err) => return Err(err).context("reading stdin"),
                }
                // A DA1 reply ends the startup hold right away.
                if let Err(err) = tick(&mut term, stdout, &mut outbound) {
                    tracing::info!("writing stdout failed ({err}); terminal lost");
                    break 'run Exit::Shutdown(129);
                }
            }

            if master_ev.intersects(PollFlags::POLLIN | PollFlags::POLLHUP | PollFlags::POLLERR) {
                match nix::unistd::read(master, &mut buf) {
                    Ok(0) | Err(Errno::EIO) => {
                        tracing::debug!("slave side closed; waiting for the shell to exit");
                        slave_gone = true;
                        outbound.clear();
                    }
                    Ok(n) => {
                        if let Err(err) = on_output(&mut term, stdout, &buf[..n], true) {
                            tracing::info!("writing stdout failed ({err}); terminal lost");
                            break 'run Exit::Shutdown(129);
                        }
                        after_output(&mut term, &mut outbound);
                        answer_looks(&mut term, master, &mut outbound);
                        sync_compsys(&mut term, &mut worker);
                        sync_specs(&mut term, &mut specs);
                    }
                    Err(Errno::EAGAIN | Errno::EINTR) => {}
                    Err(err) => return Err(err).context("reading the PTY master"),
                }
            }

            if !slave_gone && !outbound.is_empty() && master_ev.intersects(PollFlags::POLLOUT) {
                match nix::unistd::write(master, &outbound) {
                    Ok(n) => {
                        note_written(&mut term, &outbound[..n]);
                        outbound.drain(..n);
                        input_written(&mut term, &mut outbound);
                    }
                    Err(Errno::EAGAIN | Errno::EINTR) => {}
                    Err(Errno::EIO) => {
                        tracing::debug!("slave side closed while writing input");
                        slave_gone = true;
                        outbound.clear();
                    }
                    Err(err) => return Err(err).context("writing the PTY master"),
                }
            }

            on_worker(&mut worker, &worker_ev);
            sync_compsys(&mut term, &mut worker);
            on_specs(&mut specs, spec_ev);
            sync_specs(&mut term, &mut specs);
        }
    };

    if !degraded() {
        guarded("bulk", || term.flush_bulk());
    }
    if let Some(worker) = &mut worker {
        worker.shutdown();
    }
    // Sent before the spec thread stops, which writes it on its way out.
    if !degraded()
        && let Some(controller) = &mut term.popup.controller
    {
        guarded("usage", || controller.save_usage(&mut specs));
    }
    specs.shutdown();
    if let Some(controller) = &term.popup.controller {
        controller.log_stats();
    }

    match exit {
        Exit::Shell(status) => {
            drain_master(master, stdout, &mut buf, &mut term);
            tracing::info!(status, "shell exited");
            Ok(status)
        }
        Exit::Shutdown(status) => {
            shutdown(shell.pid, master);
            tracing::info!(status, "shell taken down");
            Ok(status)
        }
    }
}

/// Hand one master read to `term` and write the result to stdout in one
/// write; pure passthrough once degraded.
fn on_output(
    term: &mut Term,
    stdout: BorrowedFd<'_>,
    bytes: &[u8],
    inject: bool,
) -> nix::Result<()> {
    if !degraded() {
        // `out` may hold earlier reads waiting for a paint.
        let committed = term.out.len();
        if guarded("output", || term.on_output(bytes, Instant::now(), inject)).is_some() {
            return flush(term, stdout);
        }
        term.out.truncate(committed);
    }
    term.degrade_output(stdout)?;
    write_all(stdout, bytes)
}

/// Hand one stdin read to `term`; pure passthrough once degraded.
fn on_input(term: &mut Term, bytes: &[u8], outbound: &mut Vec<u8>) {
    let before = outbound.len();
    if !degraded() && guarded("input", || term.on_input(bytes, Instant::now(), outbound)).is_some()
    {
        return;
    }
    outbound.truncate(before);
    term.degrade_input(outbound);
    outbound.extend_from_slice(bytes);
}

/// Run `term`'s timers and write whatever they produced.
fn tick(term: &mut Term, stdout: BorrowedFd<'_>, outbound: &mut Vec<u8>) -> nix::Result<()> {
    if !degraded() && guarded("timers", || term.tick(Instant::now(), outbound)).is_some() {
        return flush(term, stdout);
    }
    term.degrade_input(outbound);
    term.degrade_output(stdout)
}

/// The trigger a read of output made due; nothing once degraded.
fn after_output(term: &mut Term, outbound: &mut Vec<u8>) {
    if !degraded() {
        guarded("trigger", || term.after_output(outbound, Instant::now()));
    }
}

/// Answer the hook's `look`s with a count of the shell's unread input,
/// taken at `master`; nothing once degraded (the hook stops waiting).
fn answer_looks(term: &mut Term, master: BorrowedFd<'_>, outbound: &mut Vec<u8>) {
    if !degraded() {
        guarded("trigger", || {
            // Only bash waits for the answer however long it takes, so only
            // for bash may the count change the terminal's modes (see
            // `pty::input_queued`); fish is out of cooked mode at its look.
            let modes = term.trigger.shell() == ShellKind::Bash;
            term.answer_looks(outbound, Instant::now(), || {
                // A count past what `shell::trigger` can work with is
                // only whether there is any.
                pty::input_queued(master, modes).map(|unread| {
                    if unread > 0 && !pty::only_controls_delete(master) {
                        u64::MAX
                    } else {
                        unread
                    }
                })
            });
        });
    }
}

/// Everything for the shell is written; nothing once degraded.
fn input_written(term: &mut Term, outbound: &mut Vec<u8>) {
    if !degraded() {
        guarded("trigger", || {
            term.on_input_written(outbound, Instant::now())
        });
    }
}

/// `written` went to the shell; nothing once degraded.
fn note_written(term: &mut Term, written: &[u8]) {
    if !degraded() {
        guarded("trigger", || term.trigger.on_written(written));
    }
}

/// The command pipe wrote everything queued; nothing once degraded.
fn commands_written(term: &mut Term, outbound: &mut Vec<u8>) {
    if !degraded() {
        guarded("trigger", || {
            term.on_commands_written(outbound, Instant::now());
        });
    }
}

fn term_deadline(term: &Term) -> Option<Instant> {
    if degraded() { None } else { term.deadline() }
}

/// Write `term.out` to stdout, unless it waits for the popup's paint.
fn flush(term: &mut Term, stdout: BorrowedFd<'_>) -> nix::Result<()> {
    if term.holds_output() {
        return Ok(());
    }
    write_out(term, stdout)
}

/// Write `term.out` to stdout in one write, as one synchronized frame when
/// it holds popup bytes.
fn write_out(term: &mut Term, stdout: BorrowedFd<'_>) -> nix::Result<()> {
    if term.out.is_empty() {
        return Ok(());
    }
    let Term {
        popup, model, out, ..
    } = term;
    popup.call(model, out, |controller, _, out| controller.seal(out));
    let result = write_all(stdout, &term.out);
    term.out.clear();
    let written = Instant::now();
    let Term {
        popup, model, out, ..
    } = term;
    popup.call(model, out, |controller, _, _| {
        controller.note_written(written)
    });
    result
}

/// Ask the worker about the latest buffer report and take its answers.
fn sync_compsys(term: &mut Term, worker: &mut Option<Worker>) {
    if degraded() {
        return;
    }
    term.sync_compsys(worker.as_mut());
}

/// Send the popup's due jobs; nothing once degraded.
fn sync_jobs(term: &mut Term, specs: &mut SpecCache) {
    if degraded() {
        return;
    }
    term.sync_jobs(specs);
}

/// Hand the popup the spec cache; nothing once degraded.
fn sync_specs(term: &mut Term, specs: &mut SpecCache) {
    if degraded() {
        return;
    }
    term.sync_specs(specs);
}

/// The worker's fds to poll; none once degraded.
fn worker_poll_fds(worker: &Option<Worker>) -> Vec<(RawFd, PollFlags)> {
    match worker {
        Some(worker) if !degraded() => worker.poll_fds(),
        _ => Vec::new(),
    }
}

fn worker_deadline(worker: &Option<Worker>) -> Option<Instant> {
    match worker {
        Some(worker) if !degraded() => worker.next_deadline(),
        _ => None,
    }
}

/// Hand the shell's context and command messages to the worker, and start
/// it once it has what it needs.
fn feed_worker(term: &mut Term, worker: &mut Option<Worker>) {
    let messages = std::mem::take(&mut term.worker_messages);
    let Some(worker) = worker.as_mut().filter(|_| !degraded()) else {
        return;
    };
    if messages.is_empty() {
        return;
    }
    guarded("worker", || {
        for message in messages {
            match message {
                ShellMessage::Context { key, value, .. } => worker.set_context(&key, &value),
                // What a command did may change what completes.
                ShellMessage::Command { .. } => worker.invalidate(),
                _ => {}
            }
        }
        worker.start_if_ready();
    });
}

/// The worker's fds after poll, then its timers.
fn on_worker(worker: &mut Option<Worker>, ready: &[(RawFd, PollFlags)]) {
    let Some(worker) = worker.as_mut().filter(|_| !degraded()) else {
        return;
    };
    guarded("worker", || {
        for &(fd, revents) in ready {
            worker.on_ready(fd, revents);
        }
        worker.on_timer(Instant::now());
    });
}

/// The spec thread's result pipe to poll; none once degraded.
fn spec_poll_fd(specs: &SpecCache) -> Option<RawFd> {
    if degraded() { None } else { specs.result_fd() }
}

/// Take in the spec thread's answers; the popup takes them in `sync_specs`.
fn on_specs(specs: &mut SpecCache, revents: PollFlags) {
    if degraded()
        || !revents.intersects(PollFlags::POLLIN | PollFlags::POLLHUP | PollFlags::POLLERR)
    {
        return;
    }
    guarded("specs", || {
        let outcomes = specs.take();
        specs.on_outcomes(outcomes);
    });
}

/// Load the common specs once the first prompt has been idle a while.
fn prewarm_specs(term: &mut Term, specs: &mut SpecCache) {
    if degraded() {
        return;
    }
    guarded("specs", || {
        if term.take_prewarm(Instant::now()) {
            specs.prewarm();
        }
    });
}

/// `waitpid` on the worker's processes; an exited one is reported to it.
fn reap_worker(worker: &mut Worker) {
    for pid in worker.pids() {
        let status = match waitpid(pid, Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::Exited(_, code)) => code,
            Ok(WaitStatus::Signaled(_, signal, _)) => 128 + signal as i32,
            _ => continue,
        };
        worker.on_child_exit(pid, status);
    }
}

/// React to the signal flags. Returns how the loop ends, if it does. Only
/// the shell's exit ends it; a worker that exited is restarted.
fn handle_signals(
    signals: &Signals,
    shell: &ShellProcess,
    term: &mut Term,
    worker: &mut Option<Worker>,
    slave_gone: bool,
) -> Result<Option<Exit>> {
    let chld = signals.chld.swap(false, Ordering::Relaxed);
    if chld && let Some(worker) = worker.as_mut() {
        guarded("worker", || reap_worker(worker));
    }
    if (chld || slave_gone)
        && let Some(status) = reap(shell.pid, Some(WaitPidFlag::WNOHANG))?
    {
        return Ok(Some(Exit::Shell(status)));
    }
    if signals.hup.swap(false, Ordering::Relaxed) {
        tracing::info!("SIGHUP");
        return Ok(Some(Exit::Shutdown(129)));
    }
    if signals.term.swap(false, Ordering::Relaxed) {
        tracing::info!("SIGTERM");
        return Ok(Some(Exit::Shutdown(143)));
    }
    if signals.usr1.swap(false, Ordering::Relaxed) && !degraded() {
        guarded("dump", || term.dump_model());
    }
    if signals.winch.swap(false, Ordering::Relaxed) {
        let size = pty::window_size(std::io::stdin().as_fd());
        tracing::debug!(rows = size.ws_row, cols = size.ws_col, "resize");
        // The shell gets its new size even if the model panics. The
        // popup's erase still goes into `out` before anything the shell
        // draws at the new size: the master is read only after this.
        if let Err(err) = pty::resize(shell.master.as_fd(), size) {
            tracing::warn!("resizing the PTY failed: {err}");
        }
        guarded("resize", || term.on_resize(size.ws_row, size.ws_col));
    }
    Ok(None)
}

/// `waitpid` on the shell. `Some(status)` once it has exited.
fn reap(pid: Pid, flags: Option<WaitPidFlag>) -> Result<Option<i32>> {
    loop {
        match waitpid(pid, flags) {
            Ok(WaitStatus::Exited(_, code)) => return Ok(Some(code)),
            Ok(WaitStatus::Signaled(_, signal, _)) => return Ok(Some(128 + signal as i32)),
            Ok(_) => return Ok(None),
            Err(Errno::EINTR) => {}
            Err(err) => return Err(err).context("waitpid on the shell"),
        }
    }
}

/// SIGHUP the shell, give it time to exit, then SIGKILL it, and reap it.
/// The master is read (and what comes is dropped) all the while: the shell
/// may write on its way out, and on macOS a process that exits with unread
/// output on its terminal waits in its last close for it to drain. SIGKILL
/// does not end that wait: the shell was seen still exiting 500 ms after it.
fn shutdown(pid: Pid, master: BorrowedFd<'_>) {
    if let Err(err) = kill(pid, Signal::SIGHUP) {
        tracing::warn!("SIGHUP to the shell failed: {err}");
    }
    let mut drain = MasterDrain::new(master);
    if wait_for_exit(pid, HUP_GRACE, &mut drain) {
        return;
    }
    tracing::warn!("shell did not exit after SIGHUP; sending SIGKILL");
    if let Err(err) = kill(pid, Signal::SIGKILL) {
        tracing::warn!("SIGKILL to the shell failed: {err}");
    }
    if !wait_for_exit(pid, KILL_GRACE, &mut drain) {
        tracing::warn!("shell was not reaped after SIGKILL");
    }
}

/// Reads and drops what the shell writes while fac takes it down.
struct MasterDrain<'a> {
    master: BorrowedFd<'a>,
    /// Once the slave side is gone (EIO), there is nothing left to read.
    gone: bool,
    buf: Vec<u8>,
}

impl<'a> MasterDrain<'a> {
    fn new(master: BorrowedFd<'a>) -> Self {
        Self {
            master,
            gone: false,
            buf: vec![0u8; READ_CHUNK],
        }
    }

    /// Wait up to `wait` for output and drop what came.
    fn drain(&mut self, wait: Duration) {
        if self.gone {
            std::thread::sleep(wait);
            return;
        }
        let mut fds = [PollFd::new(self.master, PollFlags::POLLIN)];
        let timeout = PollTimeout::from(wake::poll_millis(wait));
        match poll(&mut fds, timeout) {
            Ok(0) | Err(Errno::EINTR) => return,
            Ok(_) => {}
            Err(_) => {
                self.gone = true;
                return;
            }
        }
        // One read per call, so a job that keeps writing cannot hold fac
        // past its deadline.
        match nix::unistd::read(self.master, &mut self.buf) {
            Ok(0) | Err(Errno::EIO) => self.gone = true,
            Ok(_) | Err(Errno::EINTR | Errno::EAGAIN) => {}
            Err(err) => {
                tracing::debug!("reading the master while taking the shell down: {err}");
                self.gone = true;
            }
        }
    }
}

fn wait_for_exit(pid: Pid, grace: Duration, drain: &mut MasterDrain<'_>) -> bool {
    let deadline = Instant::now() + grace;
    loop {
        match reap(pid, Some(WaitPidFlag::WNOHANG)) {
            Ok(Some(_)) => return true,
            Ok(None) => {}
            Err(err) => {
                tracing::warn!("{err:#}");
                return false;
            }
        }
        let now = Instant::now();
        if now >= deadline {
            return false;
        }
        drain.drain(Duration::from_millis(10).min(deadline - now));
    }
}

/// Forward whatever the shell left in the master before it exited, through
/// the same filtering, but without fac's own queries: nobody would read the
/// replies. Bounded by `DRAIN_TIME` and `DRAIN_BYTES`, since a job the shell
/// left running can keep writing.
fn drain_master(master: BorrowedFd<'_>, stdout: BorrowedFd<'_>, buf: &mut [u8], term: &mut Term) {
    let committed = term.out.len();
    let released =
        !degraded() && guarded("release", || term.release(Instant::now(), false)).is_some();
    let flushed = if released {
        flush(term, stdout)
    } else {
        term.out.truncate(committed);
        term.degrade_output(stdout)
    };
    if flushed.is_err() {
        return;
    }
    let deadline = Instant::now() + DRAIN_TIME;
    let mut drained = 0;
    loop {
        if Instant::now() >= deadline || drained >= DRAIN_BYTES {
            tracing::info!(
                bytes = drained,
                "the shell's output is still coming; not drained"
            );
            break;
        }
        match nix::unistd::read(master, buf) {
            Ok(0) => break,
            Ok(n) => {
                drained += n;
                if on_output(term, stdout, &buf[..n], false).is_err() {
                    return;
                }
            }
            Err(Errno::EINTR) => {}
            Err(_) => break,
        }
    }
    if !degraded() {
        guarded("bulk", || term.flush_bulk());
    }
    // Output still waiting for a paint goes out now.
    let _ = write_out(term, stdout);
}

/// Blocking write of all of `bytes`. fd 1 is never made non-blocking by fac,
/// but it shares its file description with others, so EAGAIN waits for POLLOUT.
fn write_all(fd: BorrowedFd<'_>, mut bytes: &[u8]) -> nix::Result<()> {
    while !bytes.is_empty() {
        match nix::unistd::write(fd, bytes) {
            Ok(n) => bytes = &bytes[n..],
            Err(Errno::EINTR) => {}
            Err(Errno::EAGAIN) => {
                let mut fds = [PollFd::new(fd, PollFlags::POLLOUT)];
                match poll(&mut fds, PollTimeout::NONE) {
                    Ok(_) | Err(Errno::EINTR) => {}
                    Err(err) => return Err(err),
                }
            }
            Err(err) => return Err(err),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guarded_passes_values_through_and_catches_panics() {
        assert_eq!(guarded("ok", || 7), Some(7));
        assert!(!in_guarded());

        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let caught = guarded("boom", || -> i32 { panic!("boom") });
        std::panic::set_hook(previous);

        assert_eq!(caught, None);
        assert!(degraded());
        assert!(!in_guarded());
    }

    /// Answer the oldest query in `cprs` with a 1-based `(row, col)`.
    fn cpr_reply(model: &mut ScreenModel, cprs: &mut CprQueue, reply: (u16, u16), now: Instant) {
        let mut probe = Probe::new(now, TermKind::Unknown);
        let mut outbound = Vec::new();
        let (row, col) = reply;
        let raw = format!("\x1b[{row};{col}R");
        on_reply(
            TermReply::CursorPos { row, col },
            raw.as_bytes(),
            model,
            cprs,
            &mut ReplyQueues::new(),
            &mut probe,
            &mut outbound,
            now,
        );
        assert!(outbound.is_empty(), "a reply of ours went to the shell");
    }

    #[test]
    fn a_late_startup_seed_moves_queued_snapshots() {
        let now = Instant::now();
        let mut model = ScreenModel::new(24, 80);
        let mut cprs = CprQueue::new();
        cprs.push(CprOwner::Startup, now);
        // The hold timed out: the prompt's 133;A came with the model still
        // at the origin (really row 10), then the prompt.
        cprs.push(
            CprOwner::Ours {
                model_cursor_at_query: model.cursor(),
            },
            now,
        );
        model.process(b"% ");
        cpr_reply(&mut model, &mut cprs, (11, 1), now);
        assert_eq!(model.cursor(), (10, 2));
        // The prompt's query, taken before the seed, now agrees.
        cpr_reply(&mut model, &mut cprs, (11, 1), now);
        assert_eq!(model.cursor(), (10, 2));
        assert!(cprs.is_empty());
    }

    #[test]
    fn a_correction_moves_later_snapshots() {
        let now = Instant::now();
        let mut model = ScreenModel::new(24, 80);
        let mut cprs = CprQueue::new();
        model.process(b"\r\n\r\n");
        // Two prompt queries, the model two rows off at both.
        for _ in 0..2 {
            cprs.push(
                CprOwner::Ours {
                    model_cursor_at_query: model.cursor(),
                },
                now,
            );
            model.process(b"% \r\n");
        }
        // Really rows 4 and 5: only the first reply moves the model.
        cpr_reply(&mut model, &mut cprs, (5, 1), now);
        assert_eq!(model.cursor(), (6, 0));
        cpr_reply(&mut model, &mut cprs, (6, 1), now);
        assert_eq!(model.cursor(), (6, 0));
    }

    #[test]
    fn the_shells_cpr_reply_is_forwarded_verbatim() {
        let now = Instant::now();
        let mut model = ScreenModel::new(24, 80);
        let mut cprs = CprQueue::new();
        let mut probe = Probe::new(now, TermKind::Unknown);
        let mut outbound = Vec::new();
        // DECXCPR, for the shell's `CSI ? 6n`: the `?` and the page stay.
        for raw in [&b"\x1b[?5;10;1R"[..], b"\x1b[?5;10R", b"\x1b[5;10R"] {
            cprs.push(CprOwner::Shell, now);
            on_reply(
                TermReply::CursorPos { row: 5, col: 10 },
                raw,
                &mut model,
                &mut cprs,
                &mut ReplyQueues::new(),
                &mut probe,
                &mut outbound,
                now,
            );
            assert_eq!(outbound, raw);
            outbound.clear();
        }
        assert!(cprs.is_empty());
    }

    /// The shell asked OSC 11 after fac; fac's reply came and the shell's
    /// passed raw, unrecognised, leaving its entry behind. A later query of
    /// fac's gets its own reply: consumed, nothing typed into the shell.
    #[test]
    fn a_stale_shell_entry_never_takes_facs_next_reply() {
        use crate::term::replies::ReplyKind;

        let now = Instant::now();
        let mut model = ScreenModel::new(24, 80);
        let mut cprs = CprQueue::new();
        let mut probe = Probe::new(now, TermKind::Unknown);
        let mut replies = ReplyQueues::new();
        let mut outbound = Vec::new();
        let query = b"\x1b]11;?\x07";
        let raw = b"\x1b]11;rgb:1e1e/1e1e/1e1e\x07";
        let mut answer = |replies: &mut ReplyQueues, outbound: &mut Vec<u8>| {
            on_reply(
                TermReply::Background((30, 30, 30)),
                raw,
                &mut model,
                &mut cprs,
                replies,
                &mut probe,
                outbound,
                now,
            );
        };
        replies.push_ours_for(query);
        replies.push_shell(ReplyKind::Background);
        answer(&mut replies, &mut outbound);
        assert!(outbound.is_empty());
        // The shell's reply is not expected any more, so it passes raw
        // without popping anything.
        assert!(!replies.expect().expects(ReplyKind::Background));
        replies.push_ours_for(query);
        answer(&mut replies, &mut outbound);
        assert!(outbound.is_empty(), "fac's reply typed into the shell");
        assert!(!replies.expect().any());
    }

    /// What a terminal answers `PROBE` with, DA1 last.
    const PROBE_ANSWER: &[u8] = b"\x1b[?2026;2$y\x1b]11;rgb:1e1e/1e1e/1e1e\x1b\\\
\x1b]4;2;rgb:98/c3/79\x1b\\\x1b]4;3;rgb:e5/c0/7b\x1b\\\x1b]4;4;rgb:61/af/ef\x1b\\\
\x1b]4;5;rgb:c6/78/dd\x1b\\\x1b]4;6;rgb:56/b6/c2\x1b\\\x1b[1;1R\x1b[?62;22c";

    #[test]
    fn shell_queries_released_by_the_hold_get_their_replies() {
        let start = Instant::now();
        let mut term = Term::new(24, 80, TermKind::Unknown, ShellKind::Zsh, start, None);
        let mut outbound = Vec::new();
        // The shell asks while its output is held.
        let queries = b"\x1b]11;?\x1b\\\x1b]10;?\x07\x1b]4;3;?\x07\x1b[c";
        term.on_output(queries, start + Duration::from_millis(5), false);
        assert!(term.out.is_empty());
        // The hold runs out before DA1: the queries go to the terminal.
        term.tick(start + caps::HOLD, &mut outbound);
        assert!(term.held.is_none());
        assert_eq!(term.out, queries);
        assert!(term.probe.consuming(start + caps::HOLD));
        // fac's replies and then the shell's, in one read. The shell's
        // background differs only so it can't pass for fac's.
        let shells = b"\x1b]11;rgb:ffff/ffff/ffff\x1b\\\x1b]10;rgb:0/0/0\x07\
\x1b]4;3;rgb:1/2/3\x07\x1b[?62;22c";
        let input = [PROBE_ANSWER, shells].concat();
        term.on_input(&input, start + caps::HOLD, &mut outbound);
        assert_eq!(outbound, shells);
        assert!(term.probe.answered());
        assert_eq!(term.probe.caps.background, Some((30, 30, 30)));
        assert_eq!(term.probe.caps.ansi.yellow, Some((0xe5, 0xc0, 0x7b)));
        assert!(term.probe.caps.sync_output);
        assert!(!term.tokenizer.has_pending());
    }

    /// fac never asks OSC 10: the shell's replies to it pass through raw,
    /// in either terminator, split anywhere, among fac's probe replies
    /// (while it expects OSC replies) and after them.
    #[test]
    fn the_shells_osc10_replies_pass_raw() {
        let (probe_replies, da1) = PROBE_ANSWER.split_at(PROBE_ANSWER.len() - 9);
        assert_eq!(da1, b"\x1b[?62;22c");
        for raw in [
            &b"\x1b]10;rgb:0/0/0\x07"[..],
            b"\x1b]10;rgb:e6e6/e6e6/eaea\x1b\\",
        ] {
            for input in [
                [probe_replies, raw, da1].concat(),
                [PROBE_ANSWER, raw].concat(),
            ] {
                for split in 0..=input.len() {
                    let start = Instant::now();
                    let mut term =
                        Term::new(24, 80, TermKind::Unknown, ShellKind::Zsh, start, None);
                    let mut outbound = Vec::new();
                    term.on_output(b"\x1b]10;?\x07", start + Duration::from_millis(5), false);
                    term.tick(start + caps::HOLD, &mut outbound);
                    assert_eq!(term.out, b"\x1b]10;?\x07");
                    let (a, b) = input.split_at(split);
                    term.on_input(a, start + caps::HOLD, &mut outbound);
                    term.on_input(b, start + caps::HOLD, &mut outbound);
                    assert_eq!(outbound, raw, "{input:?} split at {split}");
                    assert!(term.probe.answered());
                    assert!(!term.tokenizer.has_pending());
                }
            }
        }
    }

    #[test]
    fn a_lone_esc_waits_only_while_the_probe_is_out() {
        let start = Instant::now();
        let mut term = Term::new(24, 80, TermKind::Unknown, ShellKind::Zsh, start, None);
        let mut outbound = Vec::new();
        // As before: held while fac's replies are expected.
        term.on_input(b"\x1b", start, &mut outbound);
        assert!(term.tokenizer.has_pending());
        let mut outbound = Vec::new();
        term.on_input(&PROBE_ANSWER[1..], start, &mut outbound);
        assert!(outbound.is_empty());
        assert!(term.probe.answered());
        // After DA1 nothing is outstanding: it goes at once.
        term.on_input(b"\x1b", start, &mut outbound);
        assert_eq!(outbound, b"\x1b");
        assert!(!term.tokenizer.has_pending());
        // fac's reply kinds stop being expected the moment the consume
        // window ends, even before its tick (which also drops the startup
        // CPR, the other reason to hold).
        let mut term = Term::new(24, 80, TermKind::Unknown, ShellKind::Zsh, start, None);
        let mut outbound = Vec::new();
        assert!(term.replies.expect().any());
        term.on_input(b"", start + caps::CONSUME, &mut outbound);
        assert!(!term.replies.expect().any());
        // And once the window ended with no DA1.
        term.tick(start + caps::CONSUME, &mut outbound);
        assert!(!term.replies.expect().any());
        term.on_input(b"\x1b", start + caps::CONSUME, &mut outbound);
        assert_eq!(outbound, b"\x1b");
        assert!(!term.tokenizer.has_pending());
        // A late reply to fac is then the shell's, untouched.
        let mut outbound = Vec::new();
        term.on_input(
            b"\x1b]11;rgb:1e/1e/1e\x07",
            start + caps::CONSUME,
            &mut outbound,
        );
        assert_eq!(outbound, b"\x1b]11;rgb:1e/1e/1e\x07");
        assert_eq!(term.probe.caps.background, None);
    }

    #[test]
    fn the_drain_after_the_shell_exits_is_bounded() {
        // A master that never runs dry: /dev/zero never ends, never EAGAIN.
        let source = std::fs::File::open("/dev/zero").expect("/dev/zero");
        let sink = std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/null")
            .expect("/dev/null");
        let mut term = Term::new(
            24,
            80,
            TermKind::Unknown,
            ShellKind::Zsh,
            Instant::now(),
            None,
        );
        let mut buf = vec![0u8; READ_CHUNK];
        let started = Instant::now();
        drain_master(source.as_fd(), sink.as_fd(), &mut buf, &mut term);
        let took = started.elapsed();
        assert!(took < DRAIN_TIME + Duration::from_secs(2), "{took:?}");
    }

    /// A 7790 report of `buffer` with the cursor at its end.
    fn report(seq: u64, buffer: &str) -> Vec<u8> {
        signed(
            format!(
                "\x1b]7790;1;{seq};start;{};;;{buffer}\x07",
                buffer.chars().count()
            )
            .as_bytes(),
        )
    }

    /// The hook's `cwd` key for `dir` (fac's cwd comes from it, not from
    /// OSC 7).
    fn cwd_context(dir: &std::path::Path) -> Vec<u8> {
        signed(format!("\x1b]7791;1;0;cwd;{}\x07", dir.display()).as_bytes())
    }

    /// `bytes` with the tests' token in each hook message, as the hook
    /// sends it: `ESC ] 779N ; 1 ;` becomes `ESC ] 779N ; 1:<token> ;`.
    fn signed(bytes: &[u8]) -> Vec<u8> {
        let token = format!(";1:{};", Token::for_tests().as_str());
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            let rest = &bytes[i..];
            if rest.len() >= 9 && rest.starts_with(b"\x1b]779") && &rest[6..9] == b";1;" {
                out.extend_from_slice(&rest[..6]);
                out.extend_from_slice(token.as_bytes());
                i += 9;
            } else {
                out.push(bytes[i]);
                i += 1;
            }
        }
        out
    }

    /// A terminal for the popup tests: fac's output goes to `terminal`, a
    /// vt100 standing in for the real one.
    struct Screen {
        term: Term,
        terminal: vt100::Parser,
        now: Instant,
        _dir: crate::complete::test_util::TempDir,
    }

    impl Screen {
        /// The probe answered, a `% ` prompt on row 0 in a folder with
        /// `alpha/`, `beta/` and `file.txt`, and the popup drawn for `ls a`.
        fn with_popup() -> Self {
            let dir = crate::complete::test_util::TempDir::new("proxy-paint");
            for sub in ["alpha", "beta"] {
                std::fs::create_dir_all(dir.path().join(sub)).expect("mkdir");
            }
            std::fs::write(dir.path().join("file.txt"), "").expect("write");
            let start = Instant::now();
            let mut term = Term::new(24, 80, TermKind::Unknown, ShellKind::Zsh, start, None);
            let mut outbound = Vec::new();
            term.on_input(PROBE_ANSWER, start, &mut outbound);
            term.tick(start, &mut outbound);
            let size = |n| std::num::NonZeroU16::new(n).expect("not zero");
            let mut screen = Self {
                term,
                terminal: vt100::Parser::new(size(24), size(80), 0),
                now: start,
                _dir: dir,
            };
            let mut first = cwd_context(screen._dir.path());
            first.extend_from_slice(
                format!("\x1b]7;file://h{}\x07% ", screen._dir.path().display()).as_bytes(),
            );
            first.extend_from_slice(&report(1, "ls a"));
            first.extend_from_slice(b"ls a");
            screen.output(&first);
            screen.paint();
            assert!(
                screen
                    .term
                    .popup
                    .controller
                    .as_ref()
                    .is_some_and(|c| c.is_open())
            );
            screen
        }

        /// One read of shell output; returns what it put in fac's output
        /// buffer (held for the paint, or written).
        fn output(&mut self, bytes: &[u8]) -> Vec<u8> {
            self.now += Duration::from_millis(1);
            self.term.on_output(bytes, self.now, false);
            let held = self.term.out.clone();
            self.write();
            held
        }

        /// Run the timers past the paint and write the frame.
        fn paint(&mut self) {
            self.now += Duration::from_millis(20);
            let mut outbound = Vec::new();
            self.term.tick(self.now, &mut outbound);
            self.write();
        }

        /// What `write_out` does, to `terminal`.
        fn write(&mut self) {
            if self.term.holds_output() || self.term.out.is_empty() {
                return;
            }
            let Term {
                popup, model, out, ..
            } = &mut self.term;
            popup.call(model, out, |controller, _, out| controller.seal(out));
            self.terminal.process(&self.term.out);
            self.term.out.clear();
        }

        /// Every cell of the drawn card shows the card's background.
        fn card_is_whole(&self, what: &str) {
            let card = self
                .term
                .popup
                .controller
                .as_ref()
                .and_then(PopupController::drawn)
                .expect("drawn")
                .card;
            let screen = self.terminal.screen();
            for row in card.row..card.row + card.rows {
                for col in card.col..card.col + card.cols {
                    let cell = screen.cell(row, col).expect("on the screen");
                    assert_ne!(
                        cell.bgcolor(),
                        vt100::Color::Default,
                        "{what}: ({row}, {col})\n{}",
                        screen.contents()
                    );
                }
            }
        }

        /// The terminal shows the model, but where the popup is drawn.
        fn matches_the_model(&self, what: &str) {
            let area = self
                .term
                .popup
                .controller
                .as_ref()
                .and_then(PopupController::drawn);
            let inside = |row: u16, col: u16| {
                area.iter()
                    .flat_map(|area| std::iter::once(area.card).chain(area.popout))
                    .any(|r| {
                        (r.row..r.row + r.rows).contains(&row)
                            && (r.col..r.col + r.cols).contains(&col)
                    })
            };
            let (terminal, model) = (self.terminal.screen(), self.term.model.screen());
            for row in 0..24 {
                for col in (0..80).filter(|&col| !inside(row, col)) {
                    assert_eq!(
                        terminal.cell(row, col),
                        model.cell(row, col),
                        "{what}: ({row}, {col})\nterminal:\n{}\nmodel:\n{}",
                        terminal.contents(),
                        model.contents()
                    );
                }
            }
            assert_eq!(
                terminal.cursor_position(),
                model.cursor_position(),
                "{what}"
            );
        }
    }

    /// Echo bytes that move the screen's rows erase the drawn popup before
    /// them.
    #[test]
    fn output_that_moves_rows_erases_the_popup_first() {
        let moving: Vec<(&str, &[u8], &[u8])> = vec![
            ("IND", b"", b"\x1bD"),
            ("NEL", b"", b"\x1bE"),
            ("RI", b"", b"\x1bM"),
            ("IL", b"", b"\x1b[L"),
            ("DL", b"", b"\x1b[M"),
            ("SU", b"", b"\x1b[S"),
            ("SD", b"", b"\x1b[T"),
            ("DECSTBM", b"", b"\x1b[1;20r"),
            ("alternate screen", b"", b"\x1b[?1049h"),
            // A scroll region of rows 1-2 set before (the cursor is kept):
            // the second line feed scrolls at its bottom margin.
            ("LF at a bottom margin", b"\x1b7\x1b[1;2r\x1b8", b"\n\n"),
        ];
        for (what, before, seq) in moving {
            let mut s = Screen::with_popup();
            s.term.model.process(before);
            s.terminal.process(before);
            let mut read = report(2, "ls al");
            read.extend_from_slice(b"l");
            read.extend_from_slice(seq);
            let held = s.output(&read);
            // fac's erase, then all of the echo.
            assert!(held.starts_with(b"\x1b7"), "{what}: {held:?}");
            let echo = [b"l".as_slice(), seq].concat();
            assert!(held.ends_with(&echo), "{what}: {held:?}");
            s.paint();
            s.matches_the_model(what);
        }
    }

    /// Echo bytes that reach other rows where they are (a cursor on another
    /// row, ED) leave the drawn popup to the paint, which erases what the
    /// new popup does not cover from the model after them and paints every
    /// cell of the new one.
    #[test]
    fn output_on_other_rows_keeps_the_popup_for_the_paint() {
        let long = "x".repeat(80);
        let in_place: Vec<(&str, Vec<u8>)> = vec![
            ("LF", b"\n".to_vec()),
            ("CUP to another row", b"\x1b[5;1Hz".to_vec()),
            ("VPA to another row", b"\x1b[6d".to_vec()),
            ("CUU and back", b"\x1b[3Bz\x1b[3A".to_vec()),
            ("ED", b"\x1b[J".to_vec()),
            (
                "ED below the popup's first row",
                b"\x1b[3;1H\x1b[J\x1b[1;6H".to_vec(),
            ),
            ("a wrap", long.clone().into_bytes()),
            // As zsh-autosuggestions draws a suggestion that wraps onto the
            // popup's row, then goes back.
            (
                "an autosuggestion onto the popup's row",
                format!("\x1b[90m{long}\x1b[0m\x1b[A\x1b[6G").into_bytes(),
            ),
            // As zsh's reset-prompt redraws a two-line prompt: up, the
            // prompt again, ED, the line.
            (
                "a two-line prompt redrawn",
                b"\r\x1b[A% top\x1b[K\r\n% \x1b[Jls al".to_vec(),
            ),
        ];
        for (what, seq) in in_place {
            let mut s = Screen::with_popup();
            let mut read = report(2, "ls al");
            read.extend_from_slice(b"l");
            read.extend_from_slice(&seq);
            let held = s.output(&read);
            let echo = [b"l".as_slice(), &seq].concat();
            assert_eq!(held, echo, "{what}");
            s.paint();
            s.matches_the_model(what);
            s.card_is_whole(what);
        }
    }

    #[test]
    fn line_local_output_keeps_the_popup_for_the_paint() {
        for (what, echo) in [
            ("a letter", &b"l"[..]),
            ("CUP on the same row", b"\x1b[1;6Hl"),
            ("VPA on the same row", b"\x1b[1dl"),
            ("erase in line", b"l\x1b[K"),
            ("columns", b"\rl\x1b[5C\x08\x1b[4G\x1b[2P"),
        ] {
            let mut s = Screen::with_popup();
            let mut read = report(2, "ls al");
            read.extend_from_slice(echo);
            let held = s.output(&read);
            assert_eq!(held, echo, "{what}");
            s.paint();
            s.matches_the_model(what);
            s.card_is_whole(what);
        }
    }

    #[test]
    fn a_late_echo_after_the_paint_keeps_the_card() {
        let mut s = Screen::with_popup();
        // The report, and its paint before the echo came.
        s.output(&report(2, "ls al"));
        s.paint();
        let card = s
            .term
            .popup
            .controller
            .as_ref()
            .and_then(PopupController::drawn)
            .expect("drawn")
            .card;
        let cells = |s: &Screen| {
            (card.row..card.row + card.rows)
                .flat_map(|row| (card.col..card.col + card.cols).map(move |col| (row, col)))
                .map(|(row, col)| s.terminal.screen().cell(row, col).cloned())
                .collect::<Vec<_>>()
        };
        let before = cells(&s);
        // The late echo is held for a paint, with no erase before it.
        assert_eq!(s.output(b"l"), b"l");
        assert_eq!(cells(&s), before, "nothing reached the terminal yet");
        s.paint();
        s.matches_the_model("late echo");
    }

    /// fac's CPRs in `bytes`.
    fn cprs_in(bytes: &[u8]) -> usize {
        bytes.windows(4).filter(|w| *w == CPR_QUERY).count()
    }

    /// A read of shell output into `term`, with fac's CPRs allowed: the
    /// CPRs it put in `out`, which is then cleared.
    fn read(term: &mut Term, bytes: &[u8], now: Instant) -> usize {
        term.on_output(bytes, now, true);
        let n = cprs_in(&term.out);
        term.out.clear();
        n
    }

    /// At reports, fac asks only where a prompt had no 133;A (at the line
    /// editor's start, once) or the model is dirty, and never with one of
    /// its own CPRs waiting.
    #[test]
    fn cprs_at_reports() {
        let now = Instant::now();
        let mut term = Term::new(24, 80, TermKind::Unknown, ShellKind::Zsh, now, None);
        let mut outbound = Vec::new();
        term.on_input(PROBE_ANSWER, now, &mut outbound);
        term.tick(now, &mut outbound);
        term.out.clear();
        let answer = |term: &mut Term, reply: &[u8]| {
            let mut outbound = Vec::new();
            term.on_input(reply, now, &mut outbound);
            assert!(!term.cprs.has_ours());
        };
        // A prompt with fac's marks: the CPR at 133;A, none at the reports.
        assert_eq!(read(&mut term, b"\x1b]133;A\x07% \x1b]133;B\x07", now), 1);
        answer(&mut term, b"\x1b[1;3R");
        assert_eq!(read(&mut term, &report(1, ""), now), 0);
        assert_eq!(read(&mut term, &report(2, "l"), now), 0);
        // The next prompt without them: one CPR at the line editor's start.
        term.prompted = false;
        assert_eq!(read(&mut term, b"\x1b]133;D;0\x07\r\n% ", now), 0);
        assert_eq!(read(&mut term, &report(3, ""), now), 1);
        assert!(term.prompted, "it counts as a prompt");
        answer(&mut term, b"\x1b[2;3R");
        assert_eq!(read(&mut term, &report(4, "l"), now), 0);
        // A finished line is not a line editor's start.
        assert_eq!(read(&mut term, b"\x1b]133;D;0\x07\r\n% ", now), 0);
        let finished = signed(b"\x1b]7790;1;5;start;0;f;;\x07");
        assert_eq!(read(&mut term, &finished, now), 0);
        // A dirty model: the next report asks, while none of fac's waits.
        term.on_resize(24, 100);
        assert_eq!(read(&mut term, &report(6, ""), now), 1, "the line start");
        assert_eq!(read(&mut term, &report(7, "l"), now), 0, "one waits");
        answer(&mut term, b"\x1b[3;3R");
        assert!(!term.model.is_dirty());
        term.on_resize(24, 80);
        assert_eq!(read(&mut term, &report(8, "ls"), now), 1, "dirty");
        assert_eq!(read(&mut term, &report(9, "ls "), now), 0, "one waits");
    }

    /// bash redraws its prompt, 133;B with it, after every report: fac
    /// takes the first B of a prompt only. zsh's marks are taken as they
    /// come.
    #[test]
    fn bash_takes_one_prompt_mark_b_per_prompt() {
        let now = Instant::now();
        let zsh = Term::new(24, 80, TermKind::Unknown, ShellKind::Zsh, now, None);
        assert_eq!(zsh.bash_prompt_b, None);
        let mut term = Term::new(24, 80, TermKind::Unknown, ShellKind::Bash, now, None);
        let mut outbound = Vec::new();
        term.on_input(PROBE_ANSWER, now, &mut outbound);
        term.tick(now, &mut outbound);
        assert_eq!(read(&mut term, b"\x1b]133;A\x07% \x1b]133;B\x07", now), 1);
        assert_eq!(term.bash_prompt_b, Some(true));
        // The redraws after reports: their B's are left out.
        for line in ["", "l", "ls"] {
            let redraw = [&b"\r\x1b[K\r% "[..], b"\x1b]133;B\x07", line.as_bytes()].concat();
            assert_eq!(read(&mut term, &redraw, now), 0);
            assert_eq!(term.bash_prompt_b, Some(true));
        }
        // The command, and the next prompt's B counts again.
        read(&mut term, b"\x1b]133;C\x07", now);
        assert_eq!(term.bash_prompt_b, Some(false));
        read(
            &mut term,
            b"\x1b]133;D;0\x07\x1b]133;A\x07% \x1b]133;B\x07",
            now,
        );
        assert_eq!(term.bash_prompt_b, Some(true));
    }

    /// One step of a recorded session: a read from stdin, a read from the
    /// shell, or the timers, each `ms` after the previous step.
    enum Step {
        Input(u64, &'static [u8]),
        Output(u64, Vec<u8>),
        Tick(u64),
    }

    /// A zsh session as fac sees it: the probe's answer, prompts with their
    /// marks, reports, typing (ASCII, é, Hebrew, emoji), the popup opened,
    /// moved, accepted with keys typed while the apply waits, a paste, Esc,
    /// kitty keys, a CPR the shell asked for, Enter, typeahead while a
    /// command runs, and the next prompt.
    fn zsh_session(dir: &std::path::Path) -> Vec<Step> {
        use Step::{Input, Output, Tick};
        let prompt = b"\x1b]133;A\x07% \x1b]133;B\x07".to_vec();
        let with = |echo: &[u8], seq: u64, buffer: &str| {
            let mut bytes = echo.to_vec();
            bytes.extend_from_slice(&report(seq, buffer));
            bytes
        };
        let mut first = cwd_context(dir);
        first.extend_from_slice(format!("\x1b]7;file://h{}\x07", dir.display()).as_bytes());
        first.extend_from_slice(&prompt);
        first.extend_from_slice(&report(1, ""));
        vec![
            Input(0, PROBE_ANSWER),
            Tick(1),
            Output(5, first),
            Input(30, b"l"),
            Output(1, with(b"l", 2, "l")),
            Input(30, b"s "),
            Output(1, with(b"s ", 3, "ls ")),
            Input(30, b"a"),
            Output(1, with(b"a", 4, "ls a")),
            Tick(20),
            // The popup is open: Down moves, Tab applies, and what is typed
            // meanwhile waits for the apply's report.
            Input(30, b"\x1b[B"),
            Tick(20),
            Input(30, b"\t"),
            Input(1, b"x"),
            Output(2, with(b"\r% ls alpha/", 5, "ls alpha/")),
            Tick(1),
            Output(1, with(b"x", 6, "ls alpha/x")),
            Tick(20),
            // A key typed before the report of the bytes before it waits.
            Input(30, "é\t".as_bytes()),
            Output(1, with("é".as_bytes(), 7, "ls alpha/xé")),
            Tick(20),
            Input(30, "שלום 🙂".as_bytes()),
            Output(1, with("שלום 🙂".as_bytes(), 8, "ls alpha/xéשלום 🙂")),
            Tick(20),
            Input(30, b"\x1b[200~echo pasted\r\t\x1b[A\x1b[201~"),
            Output(1, with(b"echo pasted", 9, "ls alpha/xéשלום 🙂echo pasted")),
            Tick(20),
            Input(30, b"\x1b"),
            Tick(20),
            Input(30, b"\x1b[97;5u\x1b[13;2u\x1b[9u"),
            Tick(20),
            Output(1, b"\x1b[6n".to_vec()),
            Input(1, b"\x1b[5;10R"),
            Input(30, b"\x15ls a"),
            Output(1, with(b"\rls a", 10, "ls a")),
            Tick(20),
            Input(30, b"\x1b[B\x1b[A\x1b[Z"),
            Tick(20),
            Input(30, b"\r"),
            Output(1, b"\x1b]133;C\x07alpha:\r\nbeta:\r\n".to_vec()),
            Input(5, b"typed ahead\x1b[A"),
            Output(5, b"\x1b]133;D;0\x07\r\n".to_vec()),
            Output(1, [prompt.as_slice(), &report(11, "typed ahead")].concat()),
            Tick(20),
            Input(30, b"\x03"),
            Output(1, [prompt.as_slice(), &report(12, "")].concat()),
            Input(30, b"echo done\r"),
            Output(1, b"echo done\r\n\x1b]133;C\x07done\r\n".to_vec()),
        ]
    }

    /// `zsh_session` through `Term`: what went to the PTY and what went to
    /// the command pipe.
    fn replay(steps: Vec<Step>) -> (Vec<u8>, Vec<u8>) {
        let (reader, writer) = nix::unistd::pipe().expect("pipe");
        nix::fcntl::fcntl(
            &reader,
            nix::fcntl::FcntlArg::F_SETFL(nix::fcntl::OFlag::O_NONBLOCK),
        )
        .expect("non-blocking");
        let mut now = Instant::now();
        let mut term = Term::new(
            24,
            80,
            TermKind::Unknown,
            ShellKind::Zsh,
            now,
            Some(CommandChannel::new(writer)),
        );
        let mut outbound = Vec::new();
        for step in steps {
            match step {
                Step::Input(ms, bytes) => {
                    now += Duration::from_millis(ms);
                    term.on_input(bytes, now, &mut outbound);
                }
                Step::Output(ms, bytes) => {
                    now += Duration::from_millis(ms);
                    term.on_output(&bytes, now, false);
                }
                Step::Tick(ms) => {
                    now += Duration::from_millis(ms);
                    term.tick(now, &mut outbound);
                }
            }
            term.out.clear();
            if let Some(channel) = &mut term.popup.commands {
                channel.flush().expect("flush");
            }
        }
        let mut commands = Vec::new();
        let mut buf = [0u8; 4096];
        while let Ok(n) = nix::unistd::read(&reader, &mut buf) {
            if n == 0 {
                break;
            }
            commands.extend_from_slice(&buf[..n]);
        }
        (outbound, commands)
    }

    /// zsh never announces a trigger, so what fac sends zsh is the same
    /// as before the trigger existed: byte for byte, both to the PTY and to
    /// the command pipe. The golden file was written by this test at the
    /// commit before the trigger (`FAC_WRITE_GOLDEN=1`).
    #[test]
    fn zsh_gets_the_same_bytes_as_before_the_trigger() {
        let dir = crate::complete::test_util::TempDir::new("zsh-golden");
        for sub in ["alpha", "beta"] {
            std::fs::create_dir_all(dir.path().join(sub)).expect("mkdir");
        }
        std::fs::write(dir.path().join("file.txt"), "").expect("write");
        let (outbound, commands) = replay(zsh_session(dir.path()));
        let mut got = outbound;
        got.extend_from_slice(b"\n--- command pipe ---\n");
        got.extend_from_slice(&commands);
        let golden = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/trigger/zsh-session.golden");
        if std::env::var_os("FAC_WRITE_GOLDEN").is_some() {
            std::fs::create_dir_all(golden.parent().expect("a parent")).expect("mkdir");
            std::fs::write(&golden, &got).expect("writing the golden file");
        }
        let expected = std::fs::read(&golden).expect("the golden file");
        assert_eq!(
            String::from_utf8_lossy(&got),
            String::from_utf8_lossy(&expected)
        );
        assert_eq!(got, expected);
    }

    /// What `write_out` would write now, or `None` while the output waits.
    /// (`Term::holds_output`, but for `degraded`, which another test of
    /// this binary may have set.)
    fn written(term: &mut Term) -> Option<Vec<u8>> {
        if term.popup.holds_output() || term.trigger.holds_output() {
            return None;
        }
        let Term {
            popup, model, out, ..
        } = term;
        popup.call(model, out, |controller, _, out| controller.seal(out));
        Some(std::mem::take(&mut term.out))
    }

    /// Only the hook's messages are believed: a 779x with no token, or with
    /// another session's, is dropped before it is read. No trigger is
    /// taken, nothing reaches the worker, and the same messages with the
    /// session's token pass. A session with no token believes none.
    #[test]
    fn a_message_without_the_sessions_token_is_dropped() {
        let forged = |first: &str| {
            format!(
                "\x1b]7791;{first};1;trigger;%1B[9999~\x07\
\x1b]7791;{first};2;aliases;touch PWNED\x07\x1b]7791;{first};3;armed;77\x07\
\x1b]7792;{first};4;touch PWNED\x07\x1b]7793;{first};5;x\x07\
\x1b]133;A\x07$ \x1b]133;B\x07\x1b]7790;{first};6;start;6;;;git ch\x07"
            )
        };
        let other = format!("1:{}", "0".repeat(32));
        let start = |token: Option<Token>| {
            let now = Instant::now();
            let mut term = Term::new(24, 80, TermKind::Unknown, ShellKind::Bash, now, None);
            term.trigger.set_shell_pid(77);
            term.token = token;
            let mut outbound = Vec::new();
            term.on_input(PROBE_ANSWER, now, &mut outbound);
            term.tick(now, &mut outbound);
            let _ = written(&mut term);
            (term, now)
        };
        let typed = |term: &mut Term, now: Instant, output: &[u8]| {
            let mut outbound = Vec::new();
            term.on_output(output, now, false);
            term.after_output(&mut outbound, now);
            term.on_input(b"x", now, &mut outbound);
            outbound
        };
        let (mut term, now) = start(Some(Token::for_tests()));
        for first in ["1", "1:", other.as_str(), Token::for_tests().as_str()] {
            assert_eq!(typed(&mut term, now, forged(first).as_bytes()), b"x");
            assert!(term.worker_messages.is_empty(), "{first:?}");
            assert!(!term.trigger.wants_apply_trigger());
        }
        let outbound = typed(&mut term, now, &signed(forged("1").as_bytes()));
        assert_eq!(outbound, b"\x1b[9999~x\x1b[9999~", "the hook's own pass");
        assert_eq!(term.worker_messages.len(), 4, "the context and the command");
        let (mut term, now) = start(None);
        assert_eq!(typed(&mut term, now, &signed(forged("1").as_bytes())), b"x");
        assert!(term.worker_messages.is_empty());
    }

    /// A bash with the trigger fixture's announcements: fac types the
    /// trigger at the prompt and after keys, holds bash's clear and redraw
    /// until the report and 1 ms of quiet and writes them as one frame, and
    /// sends applies with the line they expect, then the apply trigger once
    /// the pipe has them.
    #[test]
    fn bash_gets_triggers_held_frames_and_applies_with_the_line() {
        const T: &[u8] = b"\x1b[9999~";
        const A: &[u8] = b"\x1b[9998~";
        let dir = crate::complete::test_util::TempDir::new("proxy-trigger");
        for sub in ["alpha", "beta"] {
            std::fs::create_dir_all(dir.path().join(sub)).expect("mkdir");
        }
        let (reader, writer) = nix::unistd::pipe().expect("pipe");
        nix::fcntl::fcntl(
            &reader,
            nix::fcntl::FcntlArg::F_SETFL(nix::fcntl::OFlag::O_NONBLOCK),
        )
        .expect("non-blocking");
        let mut now = Instant::now();
        let mut term = Term::new(
            24,
            80,
            TermKind::Unknown,
            ShellKind::Bash,
            now,
            Some(CommandChannel::new(writer)),
        );
        term.trigger.set_shell_pid(77);
        let mut outbound = Vec::new();
        term.on_input(PROBE_ANSWER, now, &mut outbound);
        term.tick(now, &mut outbound);
        assert!(term.probe.caps.sync_output);
        let _ = written(&mut term);
        let mut start = cwd_context(dir.path());
        start.extend_from_slice(format!("\x1b]7;file://h{}\x07", dir.path().display()).as_bytes());
        start.extend_from_slice(&signed(
            b"\x1b]7791;1;1;trigger;%1B[9999~\x07\x1b]7791;1;2;apply_trigger;%1B[9998~\x07\
\x1b]7791;1;3;defer;i%09\x07\x1b]7791;1;4;armed;77\x07\x1b]133;A\x07$ \x1b]133;B\x07",
        ));
        term.on_output(&start, now, false);
        assert!(written(&mut term).is_some());
        term.after_output(&mut outbound, now);
        assert_eq!(outbound, T, "the line's first trigger");
        outbound.clear();
        // bash's answer: clear, report, redraw, a little apart. Held until
        // the report and 1 ms of quiet, then one frame.
        let redraw = |seq: u64, line: &str| {
            [
                b"\r\x1b[K\r".as_slice(),
                &report(seq, line),
                b"$ ",
                line.as_bytes(),
            ]
            .concat()
        };
        now += Duration::from_micros(200);
        term.on_output(&redraw(1, "")[..4], now, false);
        assert_eq!(written(&mut term), None);
        now += Duration::from_micros(200);
        term.on_output(&redraw(1, "")[4..], now, false);
        assert_eq!(written(&mut term), None);
        now += Duration::from_micros(999);
        term.tick(now, &mut outbound);
        assert_eq!(written(&mut term), None);
        now += Duration::from_micros(1);
        term.tick(now, &mut outbound);
        let frame = written(&mut term).expect("released");
        assert!(
            frame.starts_with(crate::popup::render::SYNC_BEGIN),
            "{frame:?}"
        );
        assert!(frame.ends_with(crate::popup::render::SYNC_END), "{frame:?}");
        assert!(frame.windows(4).any(|w| w == b"\r\x1b[K"), "{frame:?}");
        // A key, its trigger after it; the popup opens on the report.
        now += Duration::from_millis(30);
        term.on_input(b"ls a", now, &mut outbound);
        assert_eq!(outbound, [b"ls a".as_slice(), T].concat());
        outbound.clear();
        now += Duration::from_millis(1);
        term.on_output(
            &[b"ls a".as_slice(), &redraw(2, "ls a")].concat(),
            now,
            false,
        );
        now += Duration::from_millis(20);
        term.tick(now, &mut outbound);
        assert!(term.popup.controller.as_ref().is_some_and(|c| c.is_open()));
        let _ = written(&mut term);
        // Tab: the apply, with the line it expects, then its trigger.
        now += Duration::from_millis(30);
        term.on_input(b"\t", now, &mut outbound);
        assert_eq!(outbound, A);
        let mut buf = [0u8; 256];
        let n = nix::unistd::read(&reader, &mut buf).expect("the apply");
        assert_eq!(&buf[..n], b"apply 2 ls%20alpha/  ls%20a\n");
        // Enter: no trigger after it.
        outbound.clear();
        now += Duration::from_millis(1);
        term.on_output(&redraw(3, "ls alpha/"), now, false);
        now += Duration::from_millis(30);
        term.tick(now, &mut outbound);
        let _ = written(&mut term);
        term.on_input(b"\x1b\x1b", now, &mut outbound);
        outbound.clear();
        now += Duration::from_millis(30);
        term.tick(now, &mut outbound);
        outbound.clear();
        term.on_input(b"x\rcat", now, &mut outbound);
        assert_eq!(outbound, [b"x".as_slice(), T, b"\rcat"].concat());
    }

    /// A line that ends with no 133;C (bash's Ctrl-C): 133;D ends it, so
    /// the next prompt's `armed`, sent from PROMPT_COMMAND while the
    /// terminal still echoes, brings no trigger; 133;B does.
    #[test]
    fn a_line_ended_by_133_d_gets_its_first_trigger_at_133_b() {
        const T: &[u8] = b"\x1b[9999~";
        let now = Instant::now();
        let mut term = Term::new(24, 80, TermKind::Unknown, ShellKind::Bash, now, None);
        term.trigger.set_shell_pid(77);
        let mut outbound = Vec::new();
        term.on_input(PROBE_ANSWER, now, &mut outbound);
        term.tick(now, &mut outbound);
        let _ = written(&mut term);
        let prompt = b"\x1b]7791;1;9;armed;77\x07\x1b]133;A\x07$ \x1b]133;B\x07";
        term.on_output(
            &signed(
                &[
                    b"\x1b]7791;1;1;trigger;%1B[9999~\x07\x1b]7791;1;2;defer;n%03\x07".as_slice(),
                    prompt.as_slice(),
                ]
                .concat(),
            ),
            now,
            false,
        );
        term.after_output(&mut outbound, now);
        assert_eq!(outbound, T, "the line's first trigger");
        outbound.clear();
        term.on_output(&report(2, ""), now, false);
        term.on_input(b"ls", now, &mut outbound);
        assert_eq!(outbound, [b"ls".as_slice(), T].concat());
        outbound.clear();
        term.on_output(&report(3, "ls"), now, false);
        // Ctrl-C (`n`: no trigger after it): bash prints ^C, and
        // PROMPT_COMMAND sends 133;D and `armed` before the prompt is drawn.
        term.on_input(b"\x03", now, &mut outbound);
        assert_eq!(outbound, b"\x03");
        outbound.clear();
        term.on_output(
            &signed(b"^C\r\n\x1b]133;D;130\x07\x1b]7791;1;4;armed;77\x07"),
            now,
            false,
        );
        term.after_output(&mut outbound, now);
        assert_eq!(outbound, b"", "no trigger before the prompt");
        term.on_output(b"\x1b]133;A\x07$ \x1b]133;B\x07", now, false);
        term.after_output(&mut outbound, now);
        assert_eq!(outbound, T, "the next line's first trigger");
    }

    /// The hook's `look` (with the token) is answered on the command pipe
    /// with its seq, the bytes written to the shell so far and the count
    /// fac took; one without the token, or after fac degraded, is not.
    #[test]
    fn a_look_is_answered_on_the_command_pipe() {
        let (reader, writer) = nix::unistd::pipe().expect("pipe");
        nix::fcntl::fcntl(
            &reader,
            nix::fcntl::FcntlArg::F_SETFL(nix::fcntl::OFlag::O_NONBLOCK),
        )
        .expect("non-blocking");
        let now = Instant::now();
        let mut term = Term::new(
            24,
            80,
            TermKind::Unknown,
            ShellKind::Bash,
            now,
            Some(CommandChannel::new(writer)),
        );
        let mut outbound = Vec::new();
        term.on_input(PROBE_ANSWER, now, &mut outbound);
        term.tick(now, &mut outbound);
        term.trigger.on_written(b"typed\r");
        let pipe = || {
            let mut buf = [0u8; 256];
            match nix::unistd::read(&reader, &mut buf) {
                Ok(n) => buf[..n].to_vec(),
                Err(_) => Vec::new(),
            }
        };
        let _ = pipe();
        // Forged: no token.
        term.on_output(b"\x1b]7791;1;8;look;\x07", now, false);
        term.answer_looks(&mut outbound, now, || Some(3));
        assert_eq!(pipe(), b"");
        term.on_output(&signed(b"\x1b]7791;1;9;look;\x07"), now, false);
        let mut counted = 0;
        term.answer_looks(&mut outbound, now, || {
            counted += 1;
            Some(3)
        });
        assert_eq!(counted, 1);
        assert_eq!(pipe(), b"looked 9 6 3\n");
        // A count fac could not take.
        term.on_output(&signed(b"\x1b]7791;1;10;look;\x07"), now, false);
        term.answer_looks(&mut outbound, now, || None);
        assert_eq!(pipe(), format!("looked 10 6 {}\n", u64::MAX).as_bytes());
        // Nothing asked: nothing counted.
        term.answer_looks(&mut outbound, now, || panic!("no look waits"));
    }

    /// The reviewer's case: any program can print an OSC 7. With the
    /// shell integration on, fac's cwd comes from the hook's `cwd` key
    /// alone (the OSC still goes to the terminal); with none, OSC 7 is all
    /// there is.
    #[test]
    fn osc_7_sets_facs_cwd_only_with_no_integration() {
        let cwd = |term: &Term| {
            term.popup
                .controller
                .as_ref()
                .map(|controller| controller.cwd().to_path_buf())
        };
        let forged = b"\x1b]7;file://x/private/tmp/FORGEDDIR\x07";
        let now = Instant::now();
        let mut term = Term::new(24, 80, TermKind::Unknown, ShellKind::Bash, now, None);
        let mut outbound = Vec::new();
        term.on_input(PROBE_ANSWER, now, &mut outbound);
        term.tick(now, &mut outbound);
        term.on_output(&cwd_context(std::path::Path::new("/hook/dir")), now, false);
        assert_eq!(cwd(&term), Some("/hook/dir".into()));
        term.out.clear();
        term.on_output(forged, now, false);
        assert_eq!(cwd(&term), Some("/hook/dir".into()));
        assert!(
            term.out.windows(forged.len()).any(|w| w == forged),
            "passed on to the terminal"
        );
        // No integration (no token): OSC 7 is believed, as before.
        let mut term = Term::new(24, 80, TermKind::Unknown, ShellKind::Bash, now, None);
        term.token = None;
        term.on_input(PROBE_ANSWER, now, &mut outbound);
        term.tick(now, &mut outbound);
        term.on_output(forged, now, false);
        assert_eq!(cwd(&term), Some("/private/tmp/FORGEDDIR".into()));
    }

    /// bash's hook waits on the command pipe for the answer to its `look`
    /// with no time limit: degraded, fac answers none, so it closes the
    /// pipe and the hook reads its end.
    #[test]
    fn degrading_closes_the_command_pipe() {
        let (reader, writer) = nix::unistd::pipe().expect("pipe");
        let now = Instant::now();
        let mut term = Term::new(
            24,
            80,
            TermKind::Unknown,
            ShellKind::Bash,
            now,
            Some(CommandChannel::new(writer)),
        );
        let mut outbound = Vec::new();
        term.degrade_input(&mut outbound);
        assert!(term.popup.commands.is_none());
        let mut buf = [0u8; 8];
        assert_eq!(
            nix::unistd::read(&reader, &mut buf).expect("read"),
            0,
            "EOF"
        );
    }

    #[test]
    fn send_apply_queues_the_encoded_line() {
        let (reader, writer) = nix::unistd::pipe().expect("pipe");
        let mut channel = CommandChannel::new(writer);
        assert!(!channel.pending());
        channel.send_apply(4, "git ch", "eckout", None);
        channel.send_apply(5, "a b", "", None);
        assert_eq!(channel.queue, b"apply 4 git%20ch eckout\napply 5 a%20b \n");
        channel.flush().expect("flush");
        assert!(!channel.pending());
        let mut buf = [0u8; 64];
        let n = nix::unistd::read(&reader, &mut buf).expect("read");
        assert_eq!(&buf[..n], b"apply 4 git%20ch eckout\napply 5 a%20b \n");
    }
}
