//! The popup's life inside the proxy: when it is shown, when it is painted,
//! what the keys do while it is open, and the apply round trip.
//!
//! The proxy feeds it everything it sees (shell output, the shell's
//! messages, the user's bytes, timers) and writes what it produces. Frame
//! coalescing: once a popup is drawn or a paint is due, shell output is held
//! in the proxy's output buffer, and the paint at the deadline appends the
//! new popup, so one keystroke is one write (one 2026 frame when the
//! terminal supports it): the shell's echo, then the new popup, which
//! erases only the old cells it does not cover, from the model after the
//! echo. Each cell fac touches is written once, so a terminal without
//! synchronized output never shows the card blanked. Output that is not
//! line-local (it can move or overwrite the popup's cells: a line feed, a
//! scroll, a cursor on another row, an alternate screen) erases the old
//! popup first instead, from the model before it (`Fallback`).

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::layout::{self, Item, PopupView};
use super::palette;
use super::render::{self, Area, Painted, Renderer};
use super::text;
use super::theme::Theme;
use crate::complete::engine::{CompsysState, Engine, EngineInput, Source, Suggestion};
use crate::complete::history::History;
use crate::complete::selection::{self, Action, Key, Selection};
use crate::complete::source::{Lookup, QueryKey};
use crate::complete::tokenizer::{self, Dialect};
use crate::complete::usage::{self, Usage};
use crate::complete::zsh_worker::Worker;
use crate::config;
use crate::guards::ShellKind;
use crate::shell::protocol::{BufferReport, ContextValue};
use crate::specs::cache::{SpecCache, SpecState};
use crate::specs::generated::{Generated, Generators, RunEnv};
use crate::specs::needs::{self, Needs, View};
use crate::specs::parser::{Need, Resolved};
use crate::specs::runtime::{Env, Job};
use crate::term::caps::TermCaps;
use crate::term::keys::{KeyDecoder, Token};
use crate::term::scanner::Move;
use crate::term::screen::ScreenModel;

/// A paint is at most this long after it became due.
const PAINT_CAP: Duration = Duration::from_millis(4);
/// A paint waits for the shell's output to be quiet this long. Measured by
/// `tests/echo_timing.rs` (with zsh-syntax-highlighting): the echo comes in
/// one chunk, 99% of them within about 1.0-1.3 ms of the report; at 1.3 ms
/// about 1% of the echoes came after their paint, at 1.4 ms none did.
const PAINT_QUIET: Duration = Duration::from_micros(1400);
/// How long typed bytes wait for the shell to take an apply, or to report
/// the bytes before a held key.
const APPLY_HOLD: Duration = Duration::from_millis(50);
/// Latency samples kept.
const MAX_SAMPLES: usize = 10_000;
/// With `FAC_STATS=1` the latency is logged every this many samples.
const STATS_EVERY: u64 = 100;

/// The word the bottom line's height is decided for: its
/// `selection::word_identity` and compsys context.
type BottomWord = ((String, usize), Option<QueryKey>);

/// Sends an apply to the shell: `(seq, line, lbuffer, rbuffer)`, `line`
/// being the buffer of report `seq`, which the shell must still hold. False
/// when there is no way to (the command pipe is gone).
pub type ApplySender<'a> = dyn FnMut(u64, &str, &str, &str) -> bool + 'a;

/// A paint that is due.
struct Paint {
    first: Instant,
    last_output: Instant,
    /// Due at once (a key moved the selection).
    immediate: bool,
}

impl Paint {
    fn deadline(&self) -> Instant {
        if self.immediate {
            self.first
        } else {
            (self.first + PAINT_CAP).min(self.last_output + PAINT_QUIET)
        }
    }
}

/// When the shell's output came, from the latest report to its paint, for
/// the paint's debug log.
#[derive(Default)]
struct EchoTiming {
    report: Option<Instant>,
    first: Option<Instant>,
    last: Option<Instant>,
    chunks: u32,
    /// The longest gap between two output chunks after the first.
    max_gap: Duration,
    /// A report's paint went out then, and no report came since: output
    /// now is an echo that missed its frame.
    painted: Option<Instant>,
}

impl EchoTiming {
    /// Shell bytes for the screen at `now` (one read can give several).
    fn on_bytes(&mut self, now: Instant) {
        if let Some(painted) = self.painted.take() {
            tracing::debug!(
                "popup late echo after_paint_us={}",
                micros(now.saturating_duration_since(painted))
            );
        }
        if self.report.is_none() || self.last == Some(now) {
            return;
        }
        if let Some(last) = self.last {
            self.max_gap = self.max_gap.max(now.saturating_duration_since(last));
        }
        self.first.get_or_insert(now);
        self.last = Some(now);
        self.chunks += 1;
    }

    /// The paint for the latest report went out at `now`.
    fn on_paint(&mut self, now: Instant) {
        let Some(report) = self.report else {
            return;
        };
        let us = |from: Instant, to: Instant| micros(to.saturating_duration_since(from));
        match (self.first, self.last) {
            (Some(first), Some(last)) => tracing::debug!(
                "popup paint timing report_to_first_echo_us={} last_echo_to_paint_us={} echo_chunks={} max_echo_gap_us={}",
                us(report, first),
                us(last, now),
                self.chunks,
                micros(self.max_gap)
            ),
            _ => tracing::debug!(
                "popup paint timing no_echo report_to_paint_us={}",
                us(report, now)
            ),
        }
        *self = Self {
            painted: Some(now),
            ..Self::default()
        };
    }
}

fn micros(duration: Duration) -> u128 {
    duration.as_micros()
}

/// While a drawn popup's erase waits for the paint, during one read of
/// shell output: what erasing it before that output would write, for when
/// the output turns out to move the screen's rows.
struct Fallback {
    /// The popup's erase, from the model as it was before this read.
    erase: Vec<u8>,
    /// Where this read's output starts in the output buffer.
    at: usize,
    /// The cursor's row and the model's scroll count before this read: the
    /// output is line-local while both stay.
    row: u16,
    scrolls: u64,
    /// The cursor's row before a line feed the next bytes hold.
    line_feed: Option<u16>,
}

/// What the drawn popup's last frame painted, and the model's cells under
/// those cells then: a paint while both still hold leaves alone what still
/// shows (`Renderer::draw_over`).
struct LastPaint {
    painted: Painted,
    under: Vec<((u16, u16), Option<vt100::Cell>)>,
}

impl LastPaint {
    /// Whether the shell has changed none of the cells under the popup since
    /// (so its output did not reach the popup's cells either).
    fn holds(&self, model: &ScreenModel) -> bool {
        self.under
            .iter()
            .all(|((row, col), cell)| model.screen().cell(*row, *col) == cell.as_ref())
    }
}

/// Typed bytes held until the shell reports past `seq`.
struct ApplyHold {
    seq: u64,
    since: Instant,
    queued: Vec<u8>,
    kind: HoldKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HoldKind {
    /// An apply is on its way to the shell; `queued` is what was typed
    /// meanwhile.
    Apply,
    /// A popup key came after bytes the shell has not reported yet, so the
    /// list is stale; `queued` starts with that key.
    Unreported,
}

#[derive(Default)]
struct LatencyStats {
    samples: VecDeque<Duration>,
    total: u64,
    periodic: bool,
}

impl LatencyStats {
    fn record(&mut self, sample: Duration) {
        if self.samples.len() == MAX_SAMPLES {
            self.samples.pop_front();
        }
        self.samples.push_back(sample);
        self.total += 1;
        if self.periodic && self.total.is_multiple_of(STATS_EVERY) {
            self.log();
        }
    }

    /// (p50, p95, max), or None without samples.
    fn summary(&self) -> Option<(Duration, Duration, Duration)> {
        let mut sorted: Vec<Duration> = self.samples.iter().copied().collect();
        sorted.sort_unstable();
        let last = sorted.len().checked_sub(1)?;
        let at = |q: usize| sorted[last * q / 100];
        Some((at(50), at(95), sorted[last]))
    }

    fn log(&self) {
        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        match self.summary() {
            Some((p50, p95, max)) => tracing::info!(
                "popup latency n={} p50_ms={:.2} p95_ms={:.2} max_ms={:.2}",
                self.samples.len(),
                ms(p50),
                ms(p95),
                ms(max)
            ),
            None => tracing::info!("popup latency n=0"),
        }
    }
}

pub struct PopupController {
    engine: Engine,
    selection: Selection,
    renderer: Renderer,
    decoder: KeyDecoder,
    suggestions: Vec<Suggestion>,
    items: Vec<Item>,
    /// The latest buffer report.
    report: Option<BufferReport>,
    /// The report `suggestions` were computed from.
    computed: Option<BufferReport>,
    /// `suggestions` are older than the latest report or compsys answer.
    stale: bool,
    /// Where the popup is drawn on the screen, if it is. Shell output held
    /// for the paint may follow it in the output buffer: the paint and the
    /// teardown erase it from the model after that output.
    rect: Option<Area>,
    /// During a read of shell output that follows a drawn popup.
    fallback: Option<Fallback>,
    /// What the drawn popup's last frame painted. Dropped whenever the
    /// popup's cells may have changed behind fac's back: an erase, a
    /// fallback, a resize, new caps, the alternate screen, a dirty model, or
    /// shell output under the popup. In doubt, the next frame paints all.
    last_paint: Option<LastPaint>,
    /// Frames leave alone the cells that already show what they would
    /// paint. `FAC_FULL_REPAINT=1` turns it off, for the tests that compare
    /// the two.
    diff_paint: bool,
    /// The popup is open for the user: drawn, or erased in held output
    /// that is about to be followed by a new draw. Keys are taken only then.
    open: bool,
    /// The bottom line's rows (`D`). Decided when the popup opens and
    /// again when the word changes; kept while it stays the same, so the
    /// height does not jump as the user types in it or results merge in,
    /// and an arrow key never scrolls the screen. Until compsys has
    /// answered for the word the decision is provisional: it is made again
    /// with that answer, so a word's height does not depend on how fast
    /// the worker is.
    bottom: u16,
    /// The word `bottom` was decided for: its `selection::word_identity` and
    /// its compsys context (`Engine::query_key`; `ls ` and `ls -` are the
    /// same word but a file list and an option list); and whether compsys's
    /// answer for it was in.
    bottom_word: Option<(BottomWord, bool)>,
    /// `suggestions` hold compsys's answer (or compsys is unavailable).
    answered: bool,
    /// The word whose popup Esc closed (`selection::word_identity`).
    suppressed: Option<(String, usize)>,
    /// A paste closed the popup: the next report's word is suppressed.
    suppress_next: bool,
    history: History,
    /// The shell the lines are typed in.
    shell: ShellKind,
    /// What was accepted, for the ranking: read once at startup (`set_usage`),
    /// saved by `sync_jobs` a second after an accept and on exit.
    usage: Usage,
    alt_screen: bool,
    /// A report without `f` came after the last 133;C.
    at_prompt: bool,
    paint: Option<Paint>,
    apply: Option<ApplyHold>,
    /// User bytes went to the shell after the latest report.
    unreported: bool,
    /// Bytes an apply hold let go, to be handled as typed.
    released: Vec<u8>,
    compsys_key: Option<QueryKey>,
    compsys: Lookup,
    /// A report came that the worker has not been asked about.
    lookup_wanted: bool,
    cwd: PathBuf,
    home: PathBuf,
    /// The specs as the spec cache last said, for the names looked up.
    specs: HashMap<String, SpecState>,
    /// The answers to the spec functions the parse calls.
    needs: Needs,
    /// What the latest compute looked up and did not get, for `sync_specs`
    /// to ask for: spec names, and needs (with the load of their spec).
    wanted_specs: Vec<String>,
    wanted_needs: Vec<(Need, u64)>,
    /// What the latest compute's list depends on: a change to one of these
    /// specs (loaded, replaced, failed) or an answer for one of these needs
    /// (one it did not have) makes it stale.
    waiting_specs: Vec<String>,
    waiting_needs: Vec<Need>,
    /// A report came that `sync_specs` has not looked up the spec of.
    spec_wanted: bool,
    /// There is a spec cache (`sync_specs` was called): lines are parsed
    /// against their specs. Without one the list is phase 1's.
    specs_on: bool,
    /// A resize closed the popup: an answer (compsys's, a spec's) that
    /// comes before the next report does not open it again.
    resized: bool,
    /// What the latest compute's accepts are remembered under.
    usage_command: String,
    /// The environment spec functions and generators run commands with:
    /// the shell's exported environment (7791 `env`); until that comes,
    /// fac's own with the shell's `path` as `PATH`.
    spec_env: Env,
    /// `spec_env` is the shell's own.
    shell_env: bool,
    /// The generators of the arg being typed.
    generators: Generators,
    /// The output buffer holds popup bytes or held output, so it goes out
    /// as one synchronized frame.
    wrap: bool,
    /// When the earliest forwarded byte not yet followed by a paint was read.
    unpainted_key: Option<Instant>,
    /// The read time of the bytes the frame being written answers.
    painted_key: Option<Instant>,
    stats: LatencyStats,
    echo: EchoTiming,
    /// The configured palette, resolved once; built into the renderer's
    /// theme with each new set of caps.
    palette: palette::Theme,
    /// Its name; the default's when the configured one was not usable.
    palette_name: String,
    /// The description popout is toggled on (`description_popout_key`).
    popout: bool,
}

impl PopupController {
    /// `shell`: the shell the lines are typed in.
    pub fn new(
        shell: ShellKind,
        caps: TermCaps,
        colorterm: Option<&str>,
        cwd: PathBuf,
        home: PathBuf,
    ) -> Self {
        let configured = &config::global().theme;
        let (palette, warning) = palette::resolve(configured);
        let palette_name = match warning {
            Some(warning) => {
                tracing::warn!("{warning}");
                palette::NAMES[0].to_owned()
            }
            None => configured.clone(),
        };
        let popout_key = config::popout_key_byte(&config::global().description_popout_key)
            .unwrap_or_else(|warning| {
                tracing::warn!("{warning}");
                None
            });
        Self {
            shell,
            engine: Engine::new(),
            selection: Selection::new(),
            renderer: renderer(&palette, &palette_name, caps, colorterm),
            decoder: KeyDecoder::with_popout(popout_key),
            suggestions: Vec::new(),
            items: Vec::new(),
            report: None,
            computed: None,
            stale: false,
            rect: None,
            fallback: None,
            last_paint: None,
            diff_paint: !std::env::var("FAC_FULL_REPAINT").is_ok_and(|value| value == "1"),
            open: false,
            bottom: 0,
            bottom_word: None,
            answered: false,
            suppressed: None,
            suppress_next: false,
            history: History::empty(),
            usage: Usage::default(),
            alt_screen: false,
            at_prompt: false,
            paint: None,
            apply: None,
            unreported: false,
            released: Vec::new(),
            compsys_key: None,
            compsys: Lookup::Pending,
            lookup_wanted: false,
            cwd,
            home,
            specs: HashMap::new(),
            needs: Needs::default(),
            wanted_specs: Vec::new(),
            wanted_needs: Vec::new(),
            waiting_specs: Vec::new(),
            waiting_needs: Vec::new(),
            spec_wanted: false,
            specs_on: false,
            resized: false,
            usage_command: String::new(),
            spec_env: spec_env(None),
            shell_env: false,
            generators: Generators::default(),
            wrap: false,
            unpainted_key: None,
            painted_key: None,
            stats: LatencyStats {
                periodic: std::env::var("FAC_STATS").is_ok_and(|value| value == "1"),
                ..LatencyStats::default()
            },
            echo: EchoTiming::default(),
            palette,
            palette_name,
            popout: false,
        }
    }

    /// The probe's answers came (or stopped coming): the theme is built
    /// again from them.
    pub fn set_caps(&mut self, caps: TermCaps, colorterm: Option<&str>) {
        self.renderer = renderer(&self.palette, &self.palette_name, caps, colorterm);
        self.last_paint = None;
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Where the popup is drawn on the screen, if it is.
    #[cfg(test)]
    pub fn drawn(&self) -> Option<Area> {
        self.rect
    }

    /// Shell output must wait in the output buffer for the paint.
    pub fn holds_output(&self) -> bool {
        self.paint.is_some()
    }

    /// A popup key waits for the shell to report the bytes before it.
    pub fn holds_key(&self) -> bool {
        self.apply
            .as_ref()
            .is_some_and(|hold| hold.kind == HoldKind::Unreported)
    }

    /// The output buffer goes out as one synchronized frame, as a popup
    /// frame does: it holds output that was held back (the shell's answer
    /// to the report trigger, its clear and redraw).
    pub fn wrap_frame(&mut self) {
        self.wrap = true;
    }

    /// When `tick` next has something to do.
    pub fn deadline(&self) -> Option<Instant> {
        let paint = self.paint.as_ref().map(Paint::deadline);
        let hold = self.apply.as_ref().map(|hold| hold.since + APPLY_HOLD);
        let released = (!self.released.is_empty()).then(Instant::now);
        let save = self.usage.save_due();
        let generate = self.generators.deadline();
        [paint, hold, released, save, generate]
            .into_iter()
            .flatten()
            .min()
    }

    /// The usage store read at startup.
    pub fn set_usage(&mut self, usage: Usage) {
        self.usage = usage;
    }

    /// Save the usage store if it has anything unsaved (on exit, before
    /// the spec thread stops): on the spec thread, or here when it is gone.
    pub fn save_usage(&mut self, cache: &mut SpecCache) {
        if self.usage.save_due().is_none() {
            return;
        }
        self.send_usage(cache);
    }

    fn send_usage(&mut self, cache: &mut SpecCache) {
        let Some((path, json)) = self.usage.take_save() else {
            return;
        };
        if let Some(Job::SaveUsage { path, json }) = cache.send(Job::SaveUsage { path, json })
            && let Err(err) = usage::write_file(&path, &json)
        {
            tracing::warn!("usage: saving failed: {}", err.kind());
        }
    }

    /// Send what is due to the spec thread: the generator jobs queued and
    /// the debounced runs that are due, and a usage save a second after
    /// the last accept. Called after the timers and after `sync_specs`.
    pub fn sync_jobs(&mut self, cache: &mut SpecCache, now: Instant) {
        for job in self.generators.take_jobs(now) {
            if let Some(job) = cache.send(job) {
                self.generators.unsent(&job);
            }
        }
        if self.usage.save_due().is_some_and(|due| now >= due) {
            self.send_usage(cache);
        }
    }

    /// Shell output is about to be fed to the model and appended to `out`.
    /// A drawn popup stays on the screen: the output is held for a paint,
    /// which erases what the new popup does not cover, from the model after
    /// the output. Until `end_output` the output is watched (`watching`):
    /// output that moves the screen's rows erases the popup first after all
    /// (`fall_back`), and output that reaches other rows where they are (a
    /// cursor on another row, ED) has the next frame paint every cell
    /// (`touch`).
    pub fn before_output(&mut self, model: &ScreenModel, out: &[u8], now: Instant) {
        self.fallback = None;
        if let Some(rect) = self.rect {
            self.fallback = Some(Fallback {
                erase: self.erase_bytes(rect, model),
                at: out.len(),
                row: model.cursor().0,
                scrolls: model.scrolls(),
                line_feed: None,
            });
            // Still open: drawn again once the output is quiet.
            self.schedule(now);
        }
        if let Some(paint) = &mut self.paint {
            paint.last_output = now;
        }
    }

    /// Whether the read being fed is watched for output that is not
    /// line-local (the scanner's `Moves` events).
    pub fn watching(&self) -> bool {
        self.fallback.is_some()
    }

    /// Shell bytes went to the model and `out`. Rows scrolled off the top
    /// (a wrap, a line feed), or a line feed left the cursor on its row (it
    /// scrolled at a bottom margin): the rows moved. The cursor left its
    /// row: the output may have written on the popup's cells.
    pub fn after_shell_bytes(&mut self, model: &ScreenModel, out: &mut Vec<u8>) {
        let Some(fallback) = &mut self.fallback else {
            return;
        };
        let row = model.cursor().0;
        if model.scrolls() != fallback.scrolls {
            self.fall_back(out, "rows scrolled");
        } else if fallback.line_feed.take() == Some(row) {
            self.fall_back(out, "a line feed scrolled");
        } else if row != fallback.row {
            self.touch("the cursor's row changed");
        }
    }

    /// The scanner's `Moves` event: right after the bytes of `Rows` and
    /// `Erase`, right before those of `Cursor` and `LineFeed`.
    pub fn on_moves(&mut self, kind: Move, model: &ScreenModel, out: &mut Vec<u8>) {
        match kind {
            Move::Rows => self.fall_back(out, "rows moved"),
            Move::Erase => self.touch("erased"),
            Move::LineFeed => {
                if let Some(fallback) = &mut self.fallback {
                    fallback.line_feed = Some(model.cursor().0);
                }
            }
            // The row is compared after the bytes before the move and after
            // the bytes that start with it (`after_shell_bytes`).
            Move::Cursor => {}
        }
    }

    /// The read's output reached other rows where they are: the drawn
    /// popup is kept, but some of its cells may show the shell's now. The
    /// next frame paints every cell of the new popup and erases the rest
    /// of the old one from the model after the output.
    fn touch(&mut self, why: &str) {
        if self.last_paint.take().is_some() {
            tracing::debug!("popup kept, painted whole at the next frame ({why})");
        }
    }

    /// The read's output is all fed.
    pub fn end_output(&mut self) {
        self.fallback = None;
    }

    /// The output of this read is not line-local: the drawn popup is erased
    /// before it, from the model before it, as if it had never been kept.
    fn fall_back(&mut self, out: &mut Vec<u8>, why: &str) {
        let Some(fallback) = self.fallback.take() else {
            return;
        };
        if self.forget_drawn().is_none() {
            return;
        }
        tracing::debug!("popup erased first ({why})");
        let at = fallback.at.min(out.len());
        out.splice(at..at, fallback.erase);
        self.wrap = true;
    }

    /// The drawn popup is no longer on the screen as fac drew it (it is
    /// about to be erased, or the terminal moved it): its area, if it was
    /// drawn.
    fn forget_drawn(&mut self) -> Option<Area> {
        self.last_paint = None;
        self.rect.take()
    }

    /// The bytes that erase a popup drawn at `area`, from `model`.
    fn erase_bytes(&mut self, area: Area, model: &ScreenModel) -> Vec<u8> {
        self.renderer.set_restore_by_cup(model.shell_saved_cursor());
        self.renderer.erase(area, model.screen())
    }

    /// Shell bytes for the screen were read at `now` (for the paint's
    /// timing log).
    pub fn on_shell_bytes(&mut self, now: Instant) {
        self.echo.on_bytes(now);
    }

    /// A 7790 report.
    pub fn on_report(
        &mut self,
        report: &BufferReport,
        model: &ScreenModel,
        out: &mut Vec<u8>,
        now: Instant,
    ) {
        if self
            .apply
            .as_ref()
            .is_some_and(|hold| report.seq > hold.seq)
        {
            self.release_hold("report");
        }
        self.unreported = false;
        self.resized = false;
        // The user typed or moved the cursor: as in Fig, every keystroke's
        // list opens with nothing selected. A report of the same line (a
        // redraw) keeps the selection.
        if self
            .report
            .as_ref()
            .is_some_and(|last| last.buffer != report.buffer || last.cursor != report.cursor)
        {
            self.selection.clear();
        }
        self.report = Some(report.clone());
        self.stale = true;
        self.echo = EchoTiming {
            report: Some(now),
            ..EchoTiming::default()
        };
        if report.flags.finished {
            self.at_prompt = false;
            self.teardown(model, out, "line finished");
            return;
        }
        self.at_prompt = true;
        let identity = selection::word_identity(report, self.shell.into());
        if std::mem::take(&mut self.suppress_next) {
            self.suppressed = Some(identity);
        } else if self
            .suppressed
            .as_ref()
            .is_some_and(|word| *word != identity)
        {
            self.suppressed = None;
        }
        if self.showable(report, model.is_dirty()) {
            self.lookup_wanted = true;
            self.spec_wanted = true;
            self.schedule(now);
        } else {
            self.teardown(model, out, "hidden");
        }
    }

    /// fac's CPR made a dirty model (the width changed) trusted again. The
    /// latest report was not shown for the dirty model: it opens now, if
    /// it came after the resize (a resize alone opens nothing, as before).
    pub fn on_model_clean(&mut self, model: &ScreenModel, now: Instant) {
        if self.resized || self.open {
            return;
        }
        if self
            .report
            .as_ref()
            .is_some_and(|report| self.showable(report, model.is_dirty()))
        {
            self.lookup_wanted = true;
            self.spec_wanted = true;
            self.schedule(now);
        }
    }

    /// OSC 133;C: a command runs.
    pub fn on_command_start(&mut self, model: &ScreenModel, out: &mut Vec<u8>) {
        self.at_prompt = false;
        self.suppressed = None;
        self.suppress_next = false;
        self.teardown(model, out, "command");
    }

    /// A key went to the shell that hands the keys to something of its own
    /// (bash's C-r search, a widget, an editor), with no report until it is
    /// done: the popup closes, as Fig's did, and stays closed (a late
    /// result included) until the shell reports again.
    pub fn on_shell_reads_keys(&mut self, model: &ScreenModel, out: &mut Vec<u8>) {
        self.at_prompt = false;
        self.teardown(model, out, "the shell reads keys");
    }

    /// The alternate screen was entered or left.
    pub fn on_alt_screen(&mut self, on: bool, model: &ScreenModel, out: &mut Vec<u8>) {
        self.alt_screen = on;
        // The model shows the other screen now: a popup kept on the screen
        // is erased before the switch.
        self.fall_back(out, "alternate screen");
        if on {
            self.teardown(model, out, "alternate screen");
        }
    }

    /// A 7791 context value.
    pub fn on_context(&mut self, key: &str, value: &ContextValue) {
        match (key, value) {
            ("path", ContextValue::List(path)) => {
                if !self.shell_env {
                    self.spec_env = spec_env(Some(path));
                }
                return;
            }
            ("env", ContextValue::List(entries)) => {
                // Names and values are never logged.
                self.spec_env = Arc::new(
                    entries
                        .iter()
                        .filter_map(|entry| entry.split_once('='))
                        .map(|(name, value)| (name.to_owned(), value.to_owned()))
                        .collect(),
                );
                self.shell_env = true;
                tracing::debug!("shell env n={}", self.spec_env.len());
                return;
            }
            _ => {}
        }
        let ContextValue::Text(text) = value else {
            return;
        };
        match key {
            "histfile" => {
                self.history = if text.is_empty() {
                    History::empty()
                } else {
                    match History::load(Path::new(text), self.shell.into()) {
                        Ok(history) => history,
                        Err(err) => {
                            // $HISTFILE itself only at debug.
                            tracing::warn!("reading the history file failed: {err}");
                            tracing::debug!("the history file is {text:?}");
                            History::empty()
                        }
                    }
                };
                tracing::debug!("history loaded n={}", self.history.newest_first().count());
            }
            "cwd" if !text.is_empty() => self.cwd = PathBuf::from(text),
            _ => {}
        }
    }

    /// The cwd generators and the worker run in.
    #[cfg(test)]
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// OSC 7, when fac runs the shell with no integration.
    pub fn on_cwd(&mut self, path: &str) {
        if !path.is_empty() {
            self.cwd = PathBuf::from(path);
        }
    }

    /// A 7792: a command about to run, for history.
    pub fn on_command(&mut self, line: &str) {
        // A command ran: the generators run again at the next prompt.
        self.generators.clear();
        if let Err(err) = self.history.refresh() {
            tracing::debug!("refreshing history: {err}");
        }
        self.history.add(line);
    }

    /// Ask the worker about the latest report, and take its answers. When
    /// the answer for the latest report's key is in, a paint is due.
    pub fn sync_compsys(&mut self, worker: Option<&mut Worker>, model: &ScreenModel, now: Instant) {
        let Some(worker) = worker else {
            self.lookup_wanted = false;
            return;
        };
        if std::mem::take(&mut self.lookup_wanted) {
            self.lookup(worker);
        }
        let completed = worker.take_completed();
        let Some(key) = &self.compsys_key else {
            return;
        };
        if !completed.contains(key) {
            return;
        }
        self.lookup(worker);
        self.stale = true;
        if !self.resized
            && let Some(report) = &self.report
            && self.showable(report, model.is_dirty())
        {
            self.schedule(now);
        }
    }

    /// Load the spec of the latest report's line, send what the latest
    /// compute needed and did not have, and take in the specs and answers
    /// that came. When one of them is what the list waits for, a paint is
    /// due. Called after the spec thread's answers are taken in and after
    /// every report.
    pub fn sync_specs(&mut self, cache: &mut SpecCache, model: &ScreenModel, now: Instant) {
        // The list so far was made without specs.
        let mut fresh = !self.specs_on;
        if !self.specs_on {
            self.specs_on = true;
            self.spec_wanted = self.report.is_some();
        }
        for name in cache.take_changed() {
            if !self.specs.contains_key(&name) {
                continue;
            }
            match cache.peek(&name) {
                Some(state) => {
                    self.specs.insert(name.clone(), state);
                }
                None => {
                    self.specs.remove(&name);
                }
            }
            fresh |= self.waiting_specs.contains(&name);
        }
        let mut wanted = std::mem::take(&mut self.wanted_specs);
        if std::mem::take(&mut self.spec_wanted)
            && let Some(name) = self
                .report
                .as_ref()
                .and_then(|report| Engine::spec_name(report, self.shell.into()))
        {
            wanted.push(name);
        }
        for name in wanted {
            let state = cache.get(&name);
            fresh |= matches!(state, SpecState::Ready(_)) && self.waiting_specs.contains(&name);
            self.specs.insert(name, state);
        }
        for (need, generation) in std::mem::take(&mut self.wanted_needs) {
            self.needs
                .request(&need, generation, cache, &self.cwd, &self.spec_env);
        }
        for need in self.needs.take_answers(cache) {
            fresh |= self.waiting_needs.contains(&need);
        }
        let answers = cache.take_gen_answers();
        if !answers.is_empty() {
            let run = RunEnv {
                cwd: &self.cwd,
                home: &self.home,
                env: &self.spec_env,
                history: &self.history,
            };
            fresh |= self.generators.on_answers(answers, &run, now);
        }
        if fresh {
            self.stale = true;
            // An answer is not shell output: a paint already due keeps its
            // deadline, and picks the answer up.
            if self.paint.is_none()
                && !self.resized
                && let Some(report) = &self.report
                && self.showable(report, model.is_dirty())
            {
                self.schedule(now);
            }
        }
        // A word whose generators wait on `trigger` or `getQueryTerm`
        // answers is computed now rather than at the paint, so the answers
        // are asked for while the paint waits for the echo. The wait for a
        // quiet shell starts after it: the echo may have come meanwhile,
        // unread.
        if self.stale
            && self.generators.uses_functions()
            && let Some(paint) = &self.paint
            && !paint.immediate
        {
            self.compute();
            if let Some(paint) = &mut self.paint {
                paint.last_output = Instant::now();
            }
        }
        self.sync_jobs(cache, now);
    }

    fn lookup(&mut self, worker: &mut Worker) {
        let Some(report) = &self.report else {
            return;
        };
        let Some(key) = Engine::query_key(report, &self.cwd, self.shell.into()) else {
            return;
        };
        let (_, rbuffer) = split_buffer(report);
        self.compsys = worker.lookup(&key, rbuffer);
        self.compsys_key = Some(key);
    }

    /// User bytes (after the terminal's replies are taken out). Keys are
    /// taken while the popup is open; everything else, and everything
    /// while it is closed, goes to `outbound` verbatim and in order.
    /// `now`: when the bytes were read.
    pub fn on_user(
        &mut self,
        bytes: &[u8],
        model: &ScreenModel,
        out: &mut Vec<u8>,
        outbound: &mut Vec<u8>,
        apply: &mut ApplySender<'_>,
        now: Instant,
    ) {
        let mut i = 0;
        while i < bytes.len() {
            if let Some(hold) = &mut self.apply {
                hold.queued.extend_from_slice(&bytes[i..]);
                return;
            }
            let (token, len) = self.decoder.next(&bytes[i..], self.open);
            let raw = &bytes[i..i + len];
            i += len;
            match token {
                Token::Bytes => self.forward(raw, outbound, now),
                Token::PasteStart => {
                    self.teardown(model, out, "paste");
                    self.suppress_next = true;
                    self.forward(raw, outbound, now);
                }
                // The release of a key the popup took: not forwarded.
                Token::Release => {}
                Token::Popout => {
                    // Shows or hides the popout beside the open popup; the
                    // key is not forwarded.
                    self.popout = !self.popout;
                    tracing::debug!("popup popout={}", self.popout);
                    self.paint = Some(Paint {
                        first: now,
                        last_output: now,
                        immediate: true,
                    });
                }
                Token::Key(key) if self.unreported => {
                    // It would act on the list for an older line: it waits
                    // (with what is typed after it) for the shell's report.
                    let seq = self.report.as_ref().map_or(0, |report| report.seq);
                    tracing::debug!("held {key:?} until the report after seq={seq}");
                    self.apply = Some(ApplyHold {
                        seq,
                        since: now,
                        queued: raw.to_vec(),
                        kind: HoldKind::Unreported,
                    });
                }
                Token::Key(key) => {
                    if self.on_key(key, model, out, apply, now) {
                        self.forward(raw, outbound, now);
                    }
                }
            }
        }
    }

    /// Bytes an apply hold let go. The caller hands them to `on_user`.
    pub fn take_released(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.released)
    }

    /// Timers: the end of an apply hold, and the paint.
    pub fn tick(&mut self, model: &mut ScreenModel, out: &mut Vec<u8>, now: Instant) {
        if let Some(kind) = self
            .apply
            .as_ref()
            .filter(|hold| now >= hold.since + APPLY_HOLD)
            .map(|hold| hold.kind)
        {
            if kind == HoldKind::Unreported {
                // No report came: the held bytes go to the shell as they
                // are, with the popup closed so none is taken for a key.
                self.teardown(model, out, "no report for a held key");
            }
            self.release_hold("timeout");
        }
        if self
            .paint
            .as_ref()
            .is_some_and(|paint| now >= paint.deadline())
        {
            self.paint(model, out, now);
        }
    }

    /// SIGWINCH, before the model takes the new size. A drawn popup is
    /// erased when the width is the same. After a width change the terminal
    /// has reflowed, so an erase from the model would land in the wrong
    /// place: the popup is only forgotten. zsh's own redraw clears it (zle
    /// moves to the prompt's start and writes ED, zle_refresh.c), and the
    /// redraw's 133;A, with fac's CPR there, clears the model's `dirty`.
    pub fn on_resize(&mut self, model: &ScreenModel, out: &mut Vec<u8>, width_changed: bool) {
        if width_changed && self.forget_drawn().is_some() {
            tracing::info!(
                "width changed with the popup drawn; dropped without an erase, the shell's redraw clears it"
            );
        }
        self.teardown(model, out, "resize");
        self.resized = true;
    }

    /// Before `out` is written: a frame with popup bytes or held output is
    /// wrapped in one 2026 pair when the terminal supports it.
    pub fn seal(&mut self, out: &mut Vec<u8>) {
        if std::mem::take(&mut self.wrap) && self.renderer.sync_output() && !out.is_empty() {
            out.splice(0..0, render::SYNC_BEGIN.iter().copied());
            out.extend_from_slice(render::SYNC_END);
        }
    }

    /// The output buffer was written at `now`.
    pub fn note_written(&mut self, now: Instant) {
        if let Some(read) = self.painted_key.take() {
            self.stats.record(now.saturating_duration_since(read));
        }
    }

    pub fn log_stats(&self) {
        self.stats.log();
    }

    /// How many typed bytes are held (by an apply hold, or let go and not
    /// handled yet).
    pub fn held_input_len(&self) -> usize {
        self.apply.as_ref().map_or(0, |hold| hold.queued.len()) + self.released.len()
    }

    /// After a panic: the erase of a drawn popup (best effort; the caller
    /// catches a second panic) and the first `keep` typed bytes still held.
    pub fn salvage(&mut self, model: &ScreenModel, keep: usize) -> (Vec<u8>, Vec<u8>) {
        self.fallback = None;
        let erase = self
            .forget_drawn()
            .map(|rect| self.erase_bytes(rect, model))
            .unwrap_or_default();
        let mut input = std::mem::take(&mut self.released);
        if let Some(hold) = self.apply.take() {
            input.extend_from_slice(&hold.queued);
        }
        input.truncate(keep);
        (erase, input)
    }

    fn forward(&mut self, raw: &[u8], outbound: &mut Vec<u8>, now: Instant) {
        outbound.extend_from_slice(raw);
        self.unreported = true;
        if self.at_prompt && self.unpainted_key.is_none() {
            self.unpainted_key = Some(now);
        }
    }

    /// A popup key. Returns whether it is forwarded to the shell.
    fn on_key(
        &mut self,
        key: Key,
        model: &ScreenModel,
        out: &mut Vec<u8>,
        apply: &mut ApplySender<'_>,
        now: Instant,
    ) -> bool {
        // The key acts on the suggestions for the shell's latest buffer.
        if self.stale {
            self.compute();
        }
        let action = self.selection.on_key(key);
        tracing::debug!("popup key={key:?} action={action:?}");
        match action {
            Action::Accept(index) => {
                let (Some(suggestion), Some(report)) =
                    (self.suggestions.get(index), self.computed.as_ref())
                else {
                    self.teardown(model, out, "accept without a suggestion");
                    return true;
                };
                if key == Key::Enter && Engine::accept_is_noop(suggestion, report) {
                    self.teardown(model, out, "enter on the typed word");
                    return true;
                }
                let (lbuffer, rbuffer) = Engine::accept(suggestion, report);
                let seq = report.seq;
                if !apply(seq, &report.buffer, &lbuffer, &rbuffer) {
                    self.teardown(model, out, "no command pipe");
                    return true;
                }
                // Remembered for the ranking (history lines are not), under
                // the spec's hand-off command.
                if suggestion.id.source != Source::History {
                    self.usage.record(
                        &self.usage_command,
                        suggestion.usage_name(),
                        usage::now_ms(),
                    );
                }
                tracing::debug!("apply seq={seq} lbuffer={lbuffer:?} rbuffer={rbuffer:?}");
                self.apply = Some(ApplyHold {
                    seq,
                    since: now,
                    queued: Vec::new(),
                    kind: HoldKind::Apply,
                });
                // An insert value that ended in a newline runs the line once
                // the shell has the text: the Enter waits with what is typed
                // next.
                if let Some(hold) = &mut self.apply
                    && suggestion.runs()
                {
                    hold.queued.push(b'\r');
                }
                // The popup stays on the screen until the shell redraws the
                // line; the next level opens with nothing selected.
                self.selection = Selection::new();
                false
            }
            Action::PassThrough => {
                self.teardown(model, out, "pass through");
                true
            }
            Action::Close => {
                if let Some(report) = &self.report {
                    self.suppressed = Some(selection::word_identity(report, self.shell.into()));
                }
                self.teardown(model, out, "esc");
                false
            }
            Action::Redraw => {
                self.paint = Some(Paint {
                    first: now,
                    last_output: now,
                    immediate: true,
                });
                false
            }
        }
    }

    fn release_hold(&mut self, why: &str) {
        if let Some(hold) = self.apply.take() {
            tracing::debug!(
                "apply hold released ({why}) kind={:?} seq={} bytes={}",
                hold.kind,
                hold.seq,
                hold.queued.len()
            );
            self.released.extend_from_slice(&hold.queued);
        }
    }

    /// A paint is due (if one is not already).
    fn schedule(&mut self, now: Instant) {
        match &mut self.paint {
            Some(paint) => paint.last_output = now,
            None => {
                self.paint = Some(Paint {
                    first: now,
                    last_output: now,
                    immediate: false,
                });
            }
        }
    }

    /// Every show rule except "the engine has a suggestion".
    fn showable(&self, report: &BufferReport, dirty: bool) -> bool {
        let flags = report.flags;
        self.at_prompt
            && !self.alt_screen
            && !dirty
            && !(flags.region || flags.isearch || flags.vicmd || flags.finished)
            && !report.buffer.trim().is_empty()
            && self
                .suppressed
                .as_ref()
                .is_none_or(|word| *word != selection::word_identity(report, self.shell.into()))
    }

    /// Close the popup: erase it if it is drawn (from the model as it is
    /// now, after any shell output held for the paint), drop what it showed.
    fn teardown(&mut self, model: &ScreenModel, out: &mut Vec<u8>, why: &str) {
        self.paint = None;
        self.unpainted_key = None;
        self.fallback = None;
        if let Some(rect) = self.forget_drawn() {
            let erase = self.erase_bytes(rect, model);
            out.extend_from_slice(&erase);
            self.wrap = true;
        }
        if self.open {
            tracing::debug!("popup closed ({why})");
        }
        self.open = false;
        self.popout = false;
        self.suggestions.clear();
        self.items.clear();
        self.selection = Selection::new();
    }

    /// The suggestions for the latest report.
    fn compute(&mut self) {
        self.stale = false;
        let Some(report) = self.report.clone() else {
            self.suggestions.clear();
            self.items.clear();
            self.selection.update(&self.suggestions);
            return;
        };
        let current = Engine::query_key(&report, &self.cwd, self.shell.into());
        let compsys = match &self.compsys {
            _ if current.is_none() || current != self.compsys_key => CompsysState::Pending,
            Lookup::Ready(candidates) => CompsysState::Ready(candidates),
            Lookup::Pending => CompsysState::Pending,
            Lookup::Failed => CompsysState::Failed,
            Lookup::Unavailable => CompsysState::Unavailable,
        };
        let compsys_answered = !matches!(compsys, CompsysState::Pending);
        let is_dir = |path: &Path| path.is_dir();
        let view = View::new(&self.specs, &self.needs);
        let generated = self.generators.view(&self.cwd, &self.home, &self.history);
        let computed = self.engine.compute(&EngineInput {
            shell: self.shell,
            report: &report,
            cwd: &self.cwd,
            home: &self.home,
            history: &self.history,
            usage: &self.usage,
            compsys,
            is_dir: &is_dir,
            specs: self.specs_on.then_some(&view as &dyn Resolved),
            generated: self.specs_on.then_some(&generated as &dyn Generated),
        });
        // The generators of the arg it parsed to start, run again or wait.
        let run = RunEnv {
            cwd: &self.cwd,
            home: &self.home,
            env: &self.spec_env,
            history: &self.history,
        };
        self.generators
            .observe(&computed.spec_arg, &run, Instant::now());
        let (missed_specs, missed_needs, looked_up) = view.into_parts();
        // While the spec's parse waits for something that can still come,
        // the list is phase 1's for now, so the bottom line's height for the
        // word waits for it too. A spec that failed or is unavailable will
        // not come: phase 1's list is the answer.
        let spec_pending = match &computed.need {
            None => false,
            Some(Need::Spec { name }) => {
                matches!(self.specs.get(name), None | Some(SpecState::Loading))
            }
            Some(_) => true,
        };
        self.answered = compsys_answered && !spec_pending;
        self.waiting_specs = looked_up;
        self.waiting_needs = missed_needs.iter().map(Needs::key).collect();
        self.wanted_specs = missed_specs;
        // Each with the load of its spec the parse used: a reload taken in
        // before the need is sent gives its function ids to other functions.
        self.wanted_needs = missed_needs
            .into_iter()
            .map(|need| {
                let generation = needs::need_spec(&need)
                    .and_then(|name| match self.specs.get(name) {
                        Some(SpecState::Ready(spec)) => Some(spec.generation),
                        _ => None,
                    })
                    .unwrap_or(0);
                (need, generation)
            })
            .collect();
        self.usage_command = computed.usage_command;
        self.suggestions = computed.suggestions;
        self.items = self.suggestions.iter().map(item).collect();
        self.selection.update(&self.suggestions);
        self.computed = Some(report);
    }

    /// Draw the popup for the latest report, or close it when it has
    /// nothing to show.
    fn paint(&mut self, model: &mut ScreenModel, out: &mut Vec<u8>, now: Instant) {
        self.paint = None;
        let Some(report) = self.report.clone() else {
            self.teardown(model, out, "no report");
            return;
        };
        if !self.showable(&report, model.is_dirty()) {
            self.teardown(model, out, "hidden");
            return;
        }
        if self.stale {
            self.compute();
        }
        if self.suggestions.is_empty() {
            self.teardown(model, out, "no suggestions");
            return;
        }
        let word = (
            selection::word_identity(&report, self.shell.into()),
            Engine::query_key(&report, &self.cwd, self.shell.into()),
        );
        let settled = self.open
            && self
                .bottom_word
                .as_ref()
                .is_some_and(|(decided, answered)| *decided == word && *answered);
        let view = PopupView {
            items: &self.items,
            selected: self.selection.selected(),
            scroll: self.selection.scroll(),
            icons: config::global().icons,
            popout: self.popout,
        };
        let cursor = model.cursor();
        let (rows, cols) = model.screen().size();
        let anchor = anchor_col(&report, cursor.1, self.shell.into());
        let screen = (rows.get(), cols.get());
        let decided = settled.then_some(self.bottom);
        let Some(layout) = layout::compute(&view, cursor, anchor, screen, decided) else {
            self.teardown(model, out, "no room");
            return;
        };
        if !settled {
            self.bottom = layout.decided;
            self.bottom_word = Some((word, self.answered));
        }
        // Keys move the selection within the rows the layout shows.
        self.selection.set_window(usize::from(layout.list_rows));
        let view = PopupView {
            scroll: self.selection.scroll(),
            ..view
        };
        // The old popup is still on the screen (any output since is
        // line-local): the frame erases what the new one does not cover.
        let previous = self.rect.take();
        // What the last frame painted still shows as it left it: kept,
        // unless anything may have touched it.
        let last = self.last_paint.take().filter(|last| {
            self.diff_paint && previous.is_some() && !model.is_dirty() && last.holds(model)
        });
        self.fallback = None;
        self.renderer.set_restore_by_cup(model.shell_saved_cursor());
        let frame = self.renderer.draw_over(
            &view,
            &layout,
            cursor,
            previous,
            last.as_ref().map(|last| &last.painted),
            model.screen(),
        );
        model.process(&frame.scroll);
        out.extend_from_slice(&frame.bytes);
        let under = frame
            .painted
            .positions()
            .map(|(row, col)| ((row, col), model.screen().cell(row, col).cloned()))
            .collect();
        let rect = |r: layout::Rect| format!("{},{},{},{}", r.row, r.col, r.rows, r.cols);
        tracing::debug!(
            "popup drawn items={} at={} rows={} bottom={} lines={} d={} popout={} diff={} selected={:?}",
            self.items.len(),
            rect(frame.area.card),
            layout.list_rows,
            layout.bottom_rows,
            layout.lines,
            layout.decided,
            frame.area.popout.map_or_else(|| "none".to_owned(), rect),
            last.is_some(),
            self.selection.selected()
        );
        self.echo.on_paint(now);
        self.rect = Some(frame.area);
        self.last_paint = Some(LastPaint {
            painted: frame.painted,
            under,
        });
        self.open = true;
        self.wrap = true;
        if let Some(read) = self.unpainted_key.take() {
            self.painted_key = Some(read);
        }
    }
}

fn renderer(
    palette: &palette::Theme,
    name: &str,
    caps: TermCaps,
    colorterm: Option<&str>,
) -> Renderer {
    let depth = Theme::depth_from_env(colorterm, caps.kind);
    let theme = Theme::from_caps(palette, &caps, depth);
    let roles = &theme.roles;
    tracing::debug!(
        "palette {name} built card={} selected={} matched={} dark={} depth={:?}",
        super::color::hex(roles.card),
        super::color::hex(roles.selected),
        super::color::hex(roles.matched),
        roles.dark,
        theme.depth
    );
    Renderer::new(theme, caps.sync_output).unwrapped()
}

fn item(suggestion: &Suggestion) -> Item {
    Item {
        label: suggestion.label.clone(),
        description: suggestion.description.clone(),
        highlights: suggestion.highlights.clone(),
        is_dir: suggestion.is_dir,
        is_history: suggestion.id.source == Source::History,
        kind: suggestion.kind,
        args_hint: suggestion.args_hint.clone(),
        icon: suggestion.icon.clone(),
        arg_name: suggestion.arg_name.clone(),
        arg_description: suggestion.arg_description.clone(),
    }
}

/// What spec functions run their commands with: fac's own environment,
/// with the shell's `path` as `PATH` once it is known.
fn spec_env(path: Option<&[String]>) -> Env {
    let mut env: Vec<(String, String)> = std::env::vars_os()
        .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?)))
        .filter(|(key, _)| path.is_none() || key != "PATH")
        .collect();
    if let Some(path) = path {
        env.push(("PATH".to_owned(), path.join(":")));
    }
    Arc::new(env)
}

/// `(LBUFFER, RBUFFER)`: the buffer split at the cursor (a char index).
fn split_buffer(report: &BufferReport) -> (&str, &str) {
    let at = report
        .buffer
        .char_indices()
        .nth(report.cursor)
        .map_or(report.buffer.len(), |(i, _)| i);
    report.buffer.split_at(at)
}

/// The screen column where the word at the cursor starts: the cursor's
/// column less the width of the word's typed part left of the cursor, or 0
/// when the word started on an earlier row.
fn anchor_col(report: &BufferReport, cursor_col: u16, dialect: Dialect) -> u16 {
    let (lbuffer, rbuffer) = split_buffer(report);
    let ctx = tokenizer::analyze(dialect, &report.prebuffer, lbuffer, rbuffer);
    let start = ctx.current.typed.start.min(lbuffer.len());
    let typed = lbuffer.get(start..).unwrap_or("");
    if typed.contains('\n') {
        return 0;
    }
    // The terminal receives what ZLE draws for the word, not its characters.
    let first = lbuffer[..start].chars().count();
    let drawn = text::drawn_text(typed, first, &report.undrawn);
    cursor_col.saturating_sub(text::typed_width(&drawn))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::complete::source::Candidate;
    use crate::shell::protocol::ReportFlags;

    fn report(seq: u64, buffer: &str) -> BufferReport {
        BufferReport {
            seq,
            context: "start".into(),
            cursor: buffer.chars().count(),
            flags: ReportFlags::default(),
            prebuffer: String::new(),
            buffer: buffer.into(),
            undrawn: Vec::new(),
        }
    }

    struct Fixture {
        popup: PopupController,
        model: ScreenModel,
        out: Vec<u8>,
        outbound: Vec<u8>,
        applies: Vec<(u64, String, String)>,
        now: Instant,
        _dir: crate::complete::test_util::TempDir,
    }

    impl Fixture {
        /// A popup over a cwd with `alpha/`, `beta/` and `file.txt`.
        fn new() -> Self {
            let dir = crate::complete::test_util::TempDir::new("popup-state");
            for sub in ["alpha", "beta"] {
                std::fs::create_dir_all(dir.path().join(sub)).expect("mkdir");
            }
            std::fs::write(dir.path().join("file.txt"), "").expect("write");
            let caps = TermCaps {
                sync_output: true,
                ..TermCaps::default()
            };
            let mut popup = PopupController::new(
                ShellKind::Zsh,
                caps,
                Some("truecolor"),
                dir.path().to_path_buf(),
                dir.path().to_path_buf(),
            );
            // Whole frames, which the checks here look for; frames that
            // leave alone what still shows are `a_frame_leaves_alone_...`'s.
            popup.diff_paint = false;
            let mut model = ScreenModel::new(24, 80);
            model.process(b"% ");
            Self {
                popup,
                model,
                out: Vec::new(),
                outbound: Vec::new(),
                applies: Vec::new(),
                now: Instant::now(),
                _dir: dir,
            }
        }

        /// The shell reports `buffer` and echoes `echo`; then the paint runs.
        fn shell(&mut self, report: BufferReport, echo: &[u8]) {
            self.popup.before_output(&self.model, &self.out, self.now);
            self.popup
                .on_report(&report, &self.model, &mut self.out, self.now);
            self.model.process(echo);
            self.out.extend_from_slice(echo);
            self.settle();
        }

        /// Run the timers past every deadline.
        fn settle(&mut self) {
            while let Some(deadline) = self.popup.deadline() {
                self.now = self.now.max(deadline);
                self.popup.tick(&mut self.model, &mut self.out, self.now);
                let released = self.popup.take_released();
                if !released.is_empty() {
                    self.keys(&released);
                }
            }
            self.popup.seal(&mut self.out);
        }

        /// Run the timers due before a hold would time out.
        fn settle_before_timeout(&mut self) {
            let limit = self.now + APPLY_HOLD - Duration::from_millis(1);
            while let Some(deadline) = self.popup.deadline().filter(|&d| d <= limit) {
                self.now = self.now.max(deadline);
                self.popup.tick(&mut self.model, &mut self.out, self.now);
            }
        }

        fn keys(&mut self, bytes: &[u8]) {
            let applies = &mut self.applies;
            let mut send = |seq: u64, _line: &str, l: &str, r: &str| {
                applies.push((seq, l.to_string(), r.to_string()));
                true
            };
            self.popup.on_user(
                bytes,
                &self.model,
                &mut self.out,
                &mut self.outbound,
                &mut send,
                self.now,
            );
        }

        fn take_out(&mut self) -> Vec<u8> {
            std::mem::take(&mut self.out)
        }
    }

    /// The theme the fixture's popup paints with: the default palette on
    /// an unknown background, in true colour.
    fn fixture_theme() -> Theme {
        let roles = palette::Theme::default().roles(None, &crate::term::caps::Ansi::default());
        Theme::new(roles, None, crate::popup::theme::ColorDepth::TrueColor)
    }

    fn contains(bytes: &[u8], needle: &[u8]) -> bool {
        bytes.windows(needle.len()).any(|w| w == needle)
    }

    /// A list row on the card was drawn.
    fn has_popup(bytes: &[u8]) -> bool {
        contains(
            bytes,
            format!("\x1b[{}m", fixture_theme().card.text).as_bytes(),
        )
    }

    #[test]
    fn shows_for_a_typed_word_with_nothing_selected() {
        let mut f = Fixture::new();
        f.popup.at_prompt = true;
        f.shell(report(1, "ls a"), b"ls a");
        let out = f.take_out();
        // Drawn with the fixture's theme: its card's colours.
        assert!(has_popup(&out));
        assert!(out.starts_with(render::SYNC_BEGIN) && out.ends_with(render::SYNC_END));
        assert!(f.popup.is_open());
        assert_eq!(f.popup.selection.selected(), None);
        assert_eq!(f.popup.items[0].label, "alpha/");
    }

    #[test]
    fn show_rules() {
        let mut f = Fixture::new();
        // No prompt yet (a 7790 without f is what makes one).
        f.popup.on_command_start(&f.model, &mut f.out);
        f.popup.paint = None;
        let r = report(1, "ls a");
        assert!(!f.popup.showable(&r, false));
        f.popup.at_prompt = true;
        assert!(f.popup.showable(&r, false));
        assert!(!f.popup.showable(&r, true), "dirty model");
        assert!(!f.popup.showable(&report(1, "  "), false), "blank");
        for flags in ["r", "i", "v", "f"] {
            let mut flagged = r.clone();
            flagged.flags = ReportFlags {
                region: flags == "r",
                isearch: flags == "i",
                vicmd: flags == "v",
                finished: flags == "f",
            };
            assert!(!f.popup.showable(&flagged, false), "flag {flags}");
        }
        f.popup.alt_screen = true;
        assert!(!f.popup.showable(&r, false), "alternate screen");
        f.popup.alt_screen = false;
        f.popup.suppressed = Some(selection::word_identity(&r, Dialect::Zsh));
        assert!(!f.popup.showable(&r, false), "esc");
        assert!(
            f.popup.showable(&report(1, "ls a b"), false),
            "another word"
        );
    }

    #[test]
    fn closes_on_finish_command_and_alt_screen() {
        for how in ["f", "C", "alt"] {
            let mut f = Fixture::new();
            f.shell(report(1, "ls a"), b"ls a");
            assert!(f.popup.is_open(), "{how}");
            f.take_out();
            // The first output after a draw keeps it on the screen, and the
            // output waits for the paint.
            f.popup.before_output(&f.model, &f.out, f.now);
            assert!(f.out.is_empty(), "{how}");
            assert!(f.popup.holds_output(), "{how}");
            match how {
                "f" => {
                    let mut done = report(2, "ls a");
                    done.flags.finished = true;
                    f.popup.on_report(&done, &f.model, &mut f.out, f.now);
                }
                "C" => f.popup.on_command_start(&f.model, &mut f.out),
                _ => f.popup.on_alt_screen(true, &f.model, &mut f.out),
            }
            assert!(!f.popup.is_open(), "{how}");
            assert!(!f.popup.holds_output(), "{how}");
            // Closing erased it.
            assert!(!f.out.is_empty(), "{how}");
            f.settle();
            assert!(!has_popup(&f.take_out()), "{how}");
            // Typing in a full-screen app reports nothing; a report there
            // (none comes) would not show either.
            if how == "alt" {
                assert!(!f.popup.showable(&report(3, "ls a"), false));
            }
        }
    }

    #[test]
    fn keys_only_while_open() {
        let mut f = Fixture::new();
        f.keys(b"\t\x1b[B\r");
        assert_eq!(f.outbound, b"\t\x1b[B\r");
        f.outbound.clear();
        f.shell(report(1, "ls a"), b"ls a");
        f.keys(b"\x1b[B");
        assert!(f.outbound.is_empty());
        assert_eq!(f.popup.selection.selected(), Some(0));
    }

    #[test]
    fn typing_clears_the_selection() {
        let mut f = Fixture::new();
        f.shell(report(1, "ls "), b"ls ");
        f.keys(b"\x1b[B\x1b[B");
        f.settle();
        assert_eq!(f.popup.selection.selected(), Some(1));
        assert_eq!(f.popup.items[1].label, "beta/");
        // A letter: the new list has nothing selected, even though `beta/`
        // is still in it.
        f.shell(report(2, "ls b"), b"b");
        assert!(f.popup.is_open());
        assert_eq!(f.popup.items[0].label, "beta/");
        assert_eq!(f.popup.selection.selected(), None);
        // So Enter runs the line.
        f.keys(b"\r");
        assert_eq!(f.outbound, b"\r");
        assert!(f.applies.is_empty());

        // Moving the cursor is a new line too.
        let mut f = Fixture::new();
        f.shell(report(1, "ls "), b"ls ");
        f.keys(b"\x1b[B");
        f.settle();
        assert_eq!(f.popup.selection.selected(), Some(0));
        let mut moved = report(2, "ls ");
        moved.cursor = 2;
        f.shell(moved, b"\x08");
        assert_eq!(f.popup.selection.selected(), None);
    }

    #[test]
    fn the_same_line_keeps_the_selection() {
        let mut f = Fixture::new();
        f.shell(report(1, "ls "), b"ls ");
        f.keys(b"\x1b[B\x1b[B");
        f.settle();
        assert_eq!(f.popup.items[1].label, "beta/");
        // The shell reports the same line again (a redraw).
        f.shell(report(2, "ls "), b"");
        assert_eq!(f.popup.selection.selected(), Some(1));
        // compsys's answer merges in for the same report: `beta/` is
        // followed to its new row.
        let report = f.popup.report.clone().expect("a report");
        f.popup.compsys_key = Engine::query_key(&report, &f.popup.cwd, Dialect::Zsh);
        f.popup.compsys = Lookup::Ready(std::sync::Arc::new(vec![
            dir_candidate("aardvark"),
            dir_candidate("alpha"),
            dir_candidate("beta"),
        ]));
        f.popup.stale = true;
        f.popup.schedule(f.now);
        f.settle();
        let labels: Vec<&str> = f.popup.items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, ["aardvark/", "alpha/", "beta/"]);
        assert_eq!(f.popup.selection.selected(), Some(2));
    }

    #[test]
    fn tab_applies_and_holds_typing_until_the_next_report() {
        let mut f = Fixture::new();
        f.shell(report(1, "ls a"), b"ls a");
        f.keys(b"\txy");
        assert_eq!(f.applies, [(1, "ls alpha/".to_string(), String::new())]);
        assert!(f.outbound.is_empty(), "held during the apply");
        // A report with a newer seq lets the bytes go, in order.
        f.shell(report(2, "ls alpha/"), b"lpha/");
        assert_eq!(f.outbound, b"xy");
    }

    /// An insert value that ends in a newline runs the line: the apply
    /// carries the text, and an Enter follows once the shell has it, before
    /// what was typed after it (`state/insertion.ts:209-213`).
    #[test]
    fn a_trailing_newline_sends_enter_after_the_text() {
        let mut f = Fixture::new();
        let spec = Arc::new(crate::specs::runtime::LoadedSpec {
            name: "tool".to_owned(),
            root: serde_json::from_str(
                r#"{"name":"tool","subcommands":[{"name":"runs","insertValue":"runs\n"}]}"#,
            )
            .expect("a spec"),
            fns: Vec::new(),
            has_js: false,
            version: None,
            load_time: Duration::ZERO,
            generation: 0,
        });
        f.popup
            .specs
            .insert("tool".to_owned(), SpecState::Ready(spec));
        f.popup.specs_on = true;
        f.shell(report(1, "tool ru"), b"tool ru");
        assert!(f.popup.is_open());
        f.keys(b"\tx");
        assert_eq!(f.applies, [(1, "tool runs".to_string(), String::new())]);
        assert!(f.outbound.is_empty(), "held during the apply");
        f.shell(report(2, "tool runs"), b"ns");
        assert_eq!(f.outbound, b"\rx");
    }

    #[test]
    fn the_hold_times_out() {
        let mut f = Fixture::new();
        f.shell(report(1, "ls a"), b"ls a");
        f.keys(b"\tz");
        assert!(f.outbound.is_empty());
        f.settle();
        assert_eq!(f.outbound, b"z");
    }

    #[test]
    fn a_key_after_unreported_bytes_waits_for_the_report() {
        let mut f = Fixture::new();
        f.shell(report(1, "cd "), b"cd ");
        assert!(f.popup.is_open());
        // `b` and Tab in one read: the Tab must not act on `cd `'s list.
        f.keys(b"b\t");
        assert_eq!(f.outbound, b"b");
        assert!(f.applies.is_empty());
        f.settle_before_timeout();
        assert!(f.applies.is_empty(), "acted before the report");
        f.shell(report(2, "cd b"), b"b");
        assert_eq!(f.applies, [(2, "cd beta/".to_string(), String::new())]);
        assert_eq!(f.outbound, b"b");

        // Enter too, and what is typed after it waits with it, in order.
        let mut f = Fixture::new();
        f.shell(report(1, "ls a"), b"ls a");
        f.keys(b"x\r");
        f.keys(b"y");
        assert_eq!(f.outbound, b"x");
        f.shell(report(2, "ls ax"), b"x");
        // Nothing selected: the line runs.
        assert_eq!(f.outbound, b"x\ry");
        assert!(f.applies.is_empty());
    }

    #[test]
    fn a_held_key_goes_on_unchanged_without_a_report() {
        let mut f = Fixture::new();
        f.shell(report(1, "cd "), b"cd ");
        f.keys(b"b\t\x1b[B");
        assert_eq!(f.outbound, b"b");
        f.settle();
        assert_eq!(f.outbound, b"b\t\x1b[B");
        assert!(f.applies.is_empty());
        assert!(!f.popup.is_open());
    }

    #[test]
    fn enter_and_esc() {
        let mut f = Fixture::new();
        f.shell(report(1, "ls a"), b"ls a");
        f.take_out();
        // Enter with nothing selected: erase, then the key goes on.
        f.keys(b"\r");
        assert_eq!(f.outbound, b"\r");
        assert!(!f.popup.is_open());
        assert!(!f.take_out().is_empty(), "erased");

        let mut f = Fixture::new();
        f.shell(report(1, "ls a"), b"ls a");
        f.keys(b"\x1b");
        assert!(f.outbound.is_empty(), "Esc is not forwarded");
        assert!(!f.popup.is_open());
        // Suppressed while the word's start is the same.
        f.shell(report(2, "ls al"), b"l");
        assert!(!f.popup.is_open());
        f.shell(report(3, "ls al b"), b" b");
        assert!(f.popup.is_open());
    }

    #[test]
    fn enter_on_a_selection_that_is_the_typed_word_runs_the_line() {
        let mut f = Fixture::new();
        f.shell(report(1, "ls alpha/"), b"ls alpha/");
        // `ls alpha/` lists inside alpha/: nothing; use a word instead.
        f.shell(report(2, "ls file.txt"), b"\rls file.txt");
        assert!(f.popup.is_open());
        f.keys(b"\x1b[B\r");
        assert!(f.applies.is_empty());
        assert_eq!(f.outbound, b"\r");
    }

    #[test]
    fn a_paste_is_forwarded_whole_and_closes() {
        let mut f = Fixture::new();
        f.shell(report(1, "ls a"), b"ls a");
        let paste = b"\x1b[200~x\r\t\x1b[B\x1b[201~";
        f.keys(paste);
        assert_eq!(f.outbound, paste);
        assert!(!f.popup.is_open());
        assert!(f.applies.is_empty());
        // The pasted word stays without a popup.
        f.shell(report(2, "ls ax"), b"x");
        assert!(!f.popup.is_open());
    }

    #[test]
    fn coalesced_frame_holds_erase_echo_and_draw() {
        let mut f = Fixture::new();
        f.shell(report(1, "ls a"), b"ls a");
        f.take_out();
        f.popup.before_output(&f.model, &f.out, f.now);
        f.popup
            .on_report(&report(2, "ls al"), &f.model, &mut f.out, f.now);
        f.model.process(b"l");
        f.out.extend_from_slice(b"l");
        assert!(f.popup.holds_output());
        f.settle();
        let out = f.take_out();
        let begins = out
            .windows(render::SYNC_BEGIN.len())
            .filter(|w| *w == render::SYNC_BEGIN)
            .count();
        assert_eq!(begins, 1, "one frame");
        assert!(out.starts_with(render::SYNC_BEGIN) && out.ends_with(render::SYNC_END));
        assert!(has_popup(&out));
        assert!(out.contains(&b'l'));
    }

    /// What still shows is left alone, while nothing may have touched it.
    #[test]
    fn a_frame_leaves_alone_what_still_shows() {
        let mut f = Fixture::new();
        f.popup.diff_paint = true;
        f.shell(report(1, "ls "), b"ls ");
        assert!(has_popup(&f.take_out()));
        // The same popup again: a frame that writes no cell.
        let redraw = |f: &mut Fixture| {
            f.popup.schedule(f.now);
            f.settle();
            f.take_out()
        };
        let out = redraw(&mut f);
        assert!(
            !out.is_empty() && !has_popup(&out),
            "{:?}",
            String::from_utf8_lossy(&out)
        );
        assert!(f.popup.last_paint.is_some());
        // Down: only the rows that change.
        f.keys(b"\x1b[B");
        f.settle();
        let down = f.take_out();
        assert!(contains(
            &down,
            format!("\x1b[{}m", fixture_theme().selected.text).as_bytes()
        ));
        assert!(
            !contains(&down, b"file.txt"),
            "{:?}",
            String::from_utf8_lossy(&down)
        );
        // The shell wrote under the popup (behind fac's back): all again.
        f.model.process(b"\x1b[3;1HZZZ\x1b[1;4H");
        assert!(has_popup(&redraw(&mut f)));
        assert!(!has_popup(&redraw(&mut f)));
        // New caps: all again.
        f.popup.set_caps(
            TermCaps {
                sync_output: true,
                ..TermCaps::default()
            },
            Some("truecolor"),
        );
        assert!(has_popup(&redraw(&mut f)));
        // Erased (a fallback) and drawn again: all of it.
        f.popup.before_output(&f.model, &f.out, f.now);
        f.popup.on_moves(Move::Rows, &f.model, &mut f.out);
        f.popup.end_output();
        assert!(f.popup.last_paint.is_none() && f.popup.rect.is_none());
        f.settle();
        assert!(has_popup(&f.take_out()));
        // Erased where it is (ED): kept, and drawn again whole.
        f.popup.before_output(&f.model, &f.out, f.now);
        f.popup.on_moves(Move::Erase, &f.model, &mut f.out);
        f.popup.end_output();
        assert!(f.popup.last_paint.is_none() && f.popup.rect.is_some());
        f.settle();
        assert!(has_popup(&f.take_out()));
        // A dirty model (the width changed): closed, nothing kept.
        f.popup.on_resize(&f.model, &mut f.out, true);
        assert!(f.popup.last_paint.is_none() && f.popup.rect.is_none());
        // Off: every frame whole.
        let mut f = Fixture::new();
        f.shell(report(1, "ls "), b"ls ");
        f.take_out();
        assert!(has_popup(&redraw(&mut f)));
    }

    fn has_bottom_line(bytes: &[u8]) -> bool {
        contains(bytes, format!("\x1b[{}m", fixture_theme().band).as_bytes())
    }

    /// compsys answers the latest report with `word -- description`.
    fn answer(f: &mut Fixture, word: &str, description: &str) {
        let report = f.popup.report.clone().expect("a report");
        f.popup.compsys_key = Engine::query_key(&report, &f.popup.cwd, Dialect::Zsh);
        f.popup.compsys = Lookup::Ready(std::sync::Arc::new(vec![described(word, description)]));
        // What `sync_compsys` does when the answer is in.
        f.popup.stale = true;
        f.popup.schedule(f.now);
        f.settle();
    }

    /// compsys's answer for `buffer` is in (cached) before it is reported.
    fn cached(f: &mut Fixture, buffer: &str, word: &str, description: &str) {
        f.popup.compsys_key = Engine::query_key(&report(0, buffer), &f.popup.cwd, Dialect::Zsh);
        f.popup.compsys = Lookup::Ready(std::sync::Arc::new(vec![described(word, description)]));
    }

    /// A directory match from compsys.
    fn dir_candidate(word: &str) -> Candidate {
        Candidate {
            is_file: true,
            suffix: Some("/".into()),
            ..described(word, "")
        }
    }

    fn described(word: &str, description: &str) -> Candidate {
        Candidate {
            word: word.into(),
            display: format!("{word} -- {description}"),
            description: description.into(),
            group: "options".into(),
            suffix: None,
            verbatim: false,
            is_file: false,
            iprefix: String::new(),
            prefix: String::new(),
            apre: String::new(),
            hpre: String::new(),
            hsuf: String::new(),
        }
    }

    /// Wraps to 3 lines on a 24-column card.
    const LONG: &str = "one two three four five six seven eight nine ten eleven twelve";

    #[test]
    fn the_bottom_line_is_decided_when_the_popup_opens() {
        let mut f = Fixture::new();
        // A folder says `folder`: one line, while compsys is pending.
        f.shell(report(1, "ls a"), b"ls a");
        assert!(f.popup.is_open());
        assert_eq!(f.popup.bottom, 1);
        assert!(has_bottom_line(&f.take_out()));
        // compsys's answer for the word has a long description: decided
        // again with it, so the height does not depend on the worker's speed.
        answer(&mut f, "all", LONG);
        assert!(f.popup.is_open());
        assert_eq!(f.popup.items[0].description, LONG);
        assert_eq!(f.popup.bottom, 3);
        // From then on it is kept: a later answer without descriptions...
        answer(&mut f, "all", "");
        assert!(f.popup.is_open());
        assert_eq!(f.popup.bottom, 3);
        assert!(has_bottom_line(&f.take_out()));
        // ... and moving through the list.
        f.keys(b"\x1b[B");
        f.settle();
        assert_eq!(f.popup.bottom, 3);
        assert!(has_bottom_line(&f.take_out()));
        // Closed (a command ran) and opened again: decided anew.
        f.popup.on_command_start(&f.model, &mut f.out);
        assert!(!f.popup.is_open());
        f.take_out();
        f.shell(report(2, "ls a"), b"");
        assert!(f.popup.is_open());
        // compsys's answer, cached, has nothing to describe: no bottom line.
        assert_eq!(f.popup.items[0].label, "all");
        assert_eq!(f.popup.bottom, 0);
        assert!(!has_bottom_line(&f.take_out()));
    }

    #[test]
    fn the_bottom_line_is_decided_again_for_each_word() {
        let mut f = Fixture::new();
        f.shell(report(1, "ls a"), b"ls a");
        assert_eq!(f.popup.bottom, 1);
        // The next word, with a long description: the popup stays open and
        // its bottom line gets 3 rows.
        cached(&mut f, "ls a -", "-l", LONG);
        f.shell(report(2, "ls a -"), b" -");
        assert!(f.popup.is_open());
        assert_eq!(f.popup.bottom, 3);
        // Typing in the word keeps it, whatever the list holds: here only
        // a history line, without a description.
        f.popup.history.add("ls a -zz");
        f.shell(report(3, "ls a -z"), b"z");
        assert!(f.popup.is_open());
        assert_eq!(f.popup.items.len(), 1);
        assert!(f.popup.items[0].is_history);
        assert_eq!(f.popup.bottom, 3);
        f.shell(report(4, "ls a -l"), b"\x08l");
        assert!(f.popup.is_open());
        assert_eq!(f.popup.bottom, 3);
        // So does moving the selection.
        f.take_out();
        f.keys(b"\x1b[B");
        f.settle();
        assert_eq!(f.popup.bottom, 3);
        assert!(has_bottom_line(&f.take_out()));
        // The same word but another compsys context (`ls a -` is options,
        // `ls a ` is files): decided again.
        f.shell(report(5, "ls a "), b"\x08\x08");
        assert!(f.popup.is_open());
        assert_eq!(f.popup.bottom, 1);
        // compsys answers; results merging in after that do not change it.
        answer(&mut f, "file.txt", "");
        let decided = f.popup.bottom;
        assert!(decided < 3);
        answer(&mut f, "file.txt", LONG);
        assert!(f.popup.is_open());
        assert_eq!(f.popup.bottom, decided);
        // A word with short descriptions after one with long ones: the
        // bottom line shrinks.
        cached(&mut f, "ls a -", "-l", LONG);
        f.shell(report(6, "ls a -"), b"-");
        assert_eq!(f.popup.bottom, 3);
        f.shell(report(7, "ls a - "), b" ");
        assert!(f.popup.is_open());
        assert_eq!(f.popup.bottom, 1);
    }

    #[test]
    fn the_popout_key_toggles_the_popout() {
        let mut f = Fixture::new();
        f.popup.decoder = KeyDecoder::with_popout(Some(0x0b));
        f.keys(b"\x0b");
        // Closed: the key goes to the shell (zsh's kill-line).
        assert_eq!(f.outbound, b"\x0b");
        f.outbound.clear();
        cached(&mut f, "ls -", "-l", LONG);
        f.shell(report(1, "ls -"), b"ls -");
        assert!(f.popup.is_open());
        f.take_out();
        f.keys(b"\x0b");
        assert!(f.outbound.is_empty(), "taken while open");
        f.settle();
        assert!(f.popup.popout);
        let popout = f.popup.rect.and_then(|area| area.popout).expect("a popout");
        let card = f.popup.rect.map(|area| area.card).expect("a card");
        assert_eq!(popout.col, card.col + card.cols);
        // Pressed again: closed, and erased.
        f.take_out();
        f.keys(b"\x0b");
        f.settle();
        assert!(!f.popup.popout);
        assert_eq!(f.popup.rect.and_then(|area| area.popout), None);
        // Closing the popup closes it too.
        f.keys(b"\x0b");
        f.settle();
        assert!(f.popup.popout);
        f.keys(b"\x1b");
        assert!(!f.popup.popout);
    }

    /// Take in the spec thread's answers until `done`, syncing the popup
    /// and running its timers each time.
    fn sync_until(
        f: &mut Fixture,
        cache: &mut SpecCache,
        what: &str,
        done: impl Fn(&PopupController) -> bool,
    ) {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            f.popup.sync_specs(cache, &f.model, f.now);
            f.settle();
            if done(&f.popup) {
                return;
            }
            assert!(Instant::now() < deadline, "{what}");
            if let Some(fd) = cache.result_fd() {
                crate::specs::runtime::tests::wait_readable(fd);
            }
            let outcomes = cache.take();
            cache.on_outcomes(outcomes);
        }
    }

    #[test]
    fn a_spec_that_arrives_repaints_the_list() {
        let mut cache = SpecCache::with_thread(Some(
            crate::specs::runtime::SpecThread::start_with_budget(
                None,
                crate::specs::runtime::tests::TEST_BUDGET,
            )
            .expect("spec thread"),
        ));
        let mut f = Fixture::new();
        f.shell(report(1, "ls -"), b"ls -");
        // No spec cache yet: phase 1's list (no compsys either here).
        assert!(!f.popup.is_open());
        sync_until(&mut f, &mut cache, "ls's options", |popup| {
            popup.items.iter().any(|item| item.label == "-l")
        });
        assert!(f.popup.is_open());
        assert!(
            f.popup
                .suggestions
                .iter()
                .all(|s| s.id.source == Source::Spec)
        );
        assert_eq!(f.popup.usage_command, "ls");
        // A command without a spec keeps phase 1's list.
        f.shell(report(2, "faccmd a"), b"\x15faccmd a");
        sync_until(&mut f, &mut cache, "faccmd's spec failed", |popup| {
            matches!(popup.specs.get("faccmd"), Some(SpecState::Failed))
        });
        assert_eq!(f.popup.items[0].label, "alpha/");
        assert_eq!(f.popup.suggestions[0].id.source, Source::File);
        cache.shutdown();
    }

    #[test]
    fn anchor_is_where_the_word_starts() {
        let r = report(1, "git ch");
        assert_eq!(anchor_col(&r, 8, Dialect::Zsh), 6);
        assert_eq!(anchor_col(&report(1, "echo 日本"), 11, Dialect::Zsh), 7);
        // Started on an earlier row.
        assert_eq!(anchor_col(&r, 1, Dialect::Zsh), 0);
        assert_eq!(anchor_col(&report(1, "git "), 6, Dialect::Zsh), 6);
        // The screen model counts a heart with VS16 as 2 cells.
        assert_eq!(
            anchor_col(&report(1, "ls \u{2764}\u{fe0f}"), 5, Dialect::Zsh),
            3
        );
        assert_eq!(
            anchor_col(&report(1, "ls a\u{2764}\u{fe0f}b"), 7, Dialect::Zsh),
            3
        );
        // ZLE draws what the shell reports as `<XXXX>`: `e<0301>` is 7 cells.
        let mut r = report(1, "ls e\u{301}");
        assert_eq!(anchor_col(&r, 4, Dialect::Zsh), 3);
        r.undrawn = vec![4];
        assert_eq!(anchor_col(&r, 10, Dialect::Zsh), 3);
        // Reported characters before the word do not matter.
        r.buffer = "e\u{301} ls e\u{301}".into();
        r.cursor = r.buffer.chars().count();
        r.undrawn = vec![1, 7];
        assert_eq!(anchor_col(&r, 20, Dialect::Zsh), 13);
    }

    #[test]
    fn latency_summary() {
        let mut stats = LatencyStats::default();
        assert!(stats.summary().is_none());
        for ms in 1..=100 {
            stats.record(Duration::from_millis(ms));
        }
        let (p50, p95, max) = stats.summary().expect("samples");
        assert_eq!(
            (p50, p95, max),
            (
                Duration::from_millis(50),
                Duration::from_millis(95),
                Duration::from_millis(100)
            )
        );
    }
}
