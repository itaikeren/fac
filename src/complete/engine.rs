//! The completion engine: merges spec, compsys, file and history
//! candidates into one ranked list for the word at the cursor, and computes
//! the exact new command line when a suggestion is accepted.
//!
//! Pure and synchronous. The only I/O is `files::list` and the injected
//! `is_dir`. The line is parsed against the command's Fig spec through the
//! caller's [`Resolved`]; what the parse needs and does not have is handed
//! back ([`Computed::need`]), and until it has it the list is phase 1's.
//!
//! Adaptation to the worker's cut-context queries: the span a compsys
//! candidate replaces never starts inside `iprefix` (typed text compsys
//! leaves alone), so `echo $HO` + `HOME` (`iprefix` `$`) gives `echo $HOME `.
//!
//! Spec items follow Fig's UI (`packages/autocomplete/src` in
//! aws/amazon-q-developer-cli-autocomplete): matching each name on its own
//! (`suggestions/index.ts:393-519`, `helpers.ts:40-100`), the label
//! (`components/Suggestion.tsx:35-50`) and the insertion
//! (`state/insertion.ts:25-224`).

use std::borrow::Cow;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::files;
use super::history::History;
use super::matcher::FuzzyMatcher;
use super::quoting::quote_for;
use super::source::{Candidate, QueryKey};
use super::tokenizer::{self, Dialect, LineContext, Quote, Word};
use super::usage::Usage;
use crate::config::{self, Sort};
use crate::guards::ShellKind;
use crate::shell::protocol::BufferReport;
use crate::specs::generated::{
    ArgContext, GenItems, Generated, SpecArg, arg_generators, is_history_template,
};
use crate::specs::model::{FilterStrategy, SuggestionType};
use crate::specs::parser::{self, Fnv, Item, ItemKind, Need, ParseResult, Resolved, Suggest};

/// The most suggestions returned.
const MAX_SUGGESTIONS: usize = 50;
/// The most history lines offered.
const MAX_HISTORY: usize = 3;
/// Commands compsys completes only directories for: until it answers, the
/// file fast path offers directories only too, so the list does not lose
/// rows (and the selection) when compsys's answer replaces it.
const DIRECTORY_COMMANDS: &[&str] = &["cd", "pushd", "rmdir"];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Source {
    Spec,
    Compsys,
    File,
    History,
}

impl Source {
    /// Ranking step 4: spec, then compsys, then files; history is ranked
    /// apart, after everything.
    fn priority(self) -> u8 {
        match self {
            Source::Spec => 0,
            Source::Compsys => 1,
            Source::File => 2,
            Source::History => 3,
        }
    }
}

/// What a suggestion is, for its icon. A spec's `mixin` is shown as
/// `Special`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Subcommand,
    Option,
    Arg,
    Folder,
    File,
    History,
    Special,
    Shortcut,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SuggestionId {
    pub source: Source,
    /// The full unquoted word (or line, for history) after accepting.
    pub text: String,
}

#[derive(Clone, Debug)]
pub struct Suggestion {
    pub id: SuggestionId,
    /// What the popup shows: the word, or a file name with a trailing `/`
    /// for directories, or the history line's first line; a spec item's
    /// `displayName`, or its names joined by `", "`.
    pub label: String,
    pub description: String,
    /// Char indices into `label`.
    pub highlights: Vec<u32>,
    pub is_dir: bool,
    pub kind: Kind,
    /// A spec item's args as Fig shows them after the name; empty if none.
    pub args_hint: String,
    /// A spec item's `icon`.
    pub icon: Option<String>,
    /// The arg being typed, when the spec parse has one: its `name` and
    /// `description`, which the popup's bottom line shows for an item
    /// without a description of its own (Fig's `Description.tsx`). Set on
    /// every item offered while there is one, whatever its kind.
    pub arg_name: Option<String>,
    pub arg_description: Option<String>,
    /// Accepting it runs the line: its insert value ended in a newline
    /// (`state/insertion.ts:209-213`).
    runs: bool,
    /// What an accept is remembered under: a spec item's first name (Fig's
    /// `name[0]`), else `id.text`.
    usage_name: String,
    accept: AcceptPlan,
}

impl Suggestion {
    /// The name an accept of it is remembered under, for its command.
    pub fn usage_name(&self) -> &str {
        &self.usage_name
    }

    /// Whether accepting it runs the line, as Enter would, once the text
    /// is in.
    pub fn runs(&self) -> bool {
        self.runs
    }
}

/// How to build the new buffer when a suggestion is accepted.
#[derive(Clone, Debug)]
enum AcceptPlan {
    /// Replace the unquoted span `from..end of word` with `inserted` (a
    /// backspace in it takes back the character before it), then append
    /// `suffix`. With `rest` (an insert value's `{cursor}`), the cursor
    /// stays after `inserted`, and `rest` and the suffix go after it.
    /// `dialect`: the rules `from` was counted by.
    Replace {
        from: usize,
        inserted: String,
        rest: Option<String>,
        suffix: String,
        dialect: Dialect,
    },
    /// Replace the whole line (history).
    Line(String),
}

pub enum CompsysState<'a> {
    Ready(&'a [Candidate]),
    Pending,
    /// The query for this context timed out or failed: like `Unavailable`
    /// for this word.
    Failed,
    Unavailable,
}

pub struct EngineInput<'a> {
    /// The shell the line is typed in: its rules read and quote the line,
    /// and the zsh-only filters apply to zsh alone.
    pub shell: ShellKind,
    pub report: &'a BufferReport,
    pub cwd: &'a Path,
    pub home: &'a Path,
    pub history: &'a History,
    /// What was accepted before, for the recency boost.
    pub usage: &'a Usage,
    pub compsys: CompsysState<'a>,
    /// Injected for tests; the proxy passes a real stat.
    pub is_dir: &'a dyn Fn(&Path) -> bool,
    /// The specs and answers the caller has: the line is parsed against
    /// its command's spec through it. `None`: no specs, phase 1's list.
    pub specs: Option<&'a dyn Resolved>,
    /// The generators' items for the arg being typed; `None`: none.
    pub generated: Option<&'a dyn Generated>,
}

/// What [`Engine::compute`] gives.
#[derive(Debug, Default)]
pub struct Computed {
    /// Ranked, at most 50; empty when nothing should be shown.
    pub suggestions: Vec<Suggestion>,
    /// What the parse stopped at (a spec not loaded, a function not
    /// answered): the list is phase 1's until it is there.
    pub need: Option<Need>,
    /// The command accepts are remembered under: the spec's hand-off
    /// command (`git` for `sudo git ch`), else the line's first word, or
    /// `""` at command position.
    pub usage_command: String,
    /// The arg the spec's parse ended at, for its generators.
    pub spec_arg: SpecArg,
}

pub struct Engine {
    matcher: FuzzyMatcher,
    indices: Vec<u32>,
    /// A text put together for a usage lookup.
    scratch: String,
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

/// How well a candidate matches the typed word, best first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Tier {
    /// Equal (`helpers.ts:47`).
    Exact,
    /// Equal but for case (`helpers.ts:48`): the tier-1 exact match with the
    /// case-sensitive one first, as Fig lists them.
    ExactIgnoringCase,
    /// Starts with the needle (`helpers.ts:49`).
    CaseSensitivePrefix,
    /// Starts with the needle, ignoring case (`helpers.ts:50`).
    CaseInsensitivePrefix,
    Fuzzy,
}

/// The settings ranking reads (`config::global()` in production).
#[derive(Clone, Copy, Debug)]
struct RankSettings {
    fuzzy: bool,
    sort: Sort,
    /// History lines, and the `history` template's values, are offered.
    history: bool,
    /// `disable_for_commands`.
    disabled: &'static [String],
}

impl RankSettings {
    fn global() -> Self {
        let config = config::global();
        Self {
            fuzzy: config.fuzzy,
            sort: config.sort,
            history: config.history,
            disabled: &config.disable_for_commands,
        }
    }

    /// With a spec's `filterStrategy`: `prefix` drops the fuzzy tier,
    /// `fuzzy` keeps it whatever the setting (`state/index.ts:367-381`).
    fn with_strategy(self, strategy: Option<FilterStrategy>) -> Self {
        let fuzzy = match strategy {
            Some(FilterStrategy::Prefix) => false,
            Some(FilterStrategy::Fuzzy) => true,
            Some(FilterStrategy::Default) | None => self.fuzzy,
        };
        Self { fuzzy, ..self }
    }

    fn is_disabled(&self, command: &str) -> bool {
        self.disabled.iter().any(|disabled| disabled == command)
    }
}

/// The priority of a suggestion that declares none (Fig: `priority || 50`).
const DEFAULT_PRIORITY: u8 = 50;
/// Fig's recency divisor (`sorting.ts:69`).
const RECENCY_DIVISOR: f64 = 10_000_000_000_000.0;

/// What ranking reads of a candidate. Candidates are ranked on this alone,
/// and only the ones shown are built into a [`Suggestion`] ([`Build`]).
#[derive(Clone, Debug)]
struct RankKey {
    tier: Tier,
    /// The fuzzy score; 0 outside the fuzzy tier, where it does not rank.
    score: u32,
    /// The priority the source declares; `None` for compsys, files and
    /// history, which declare none.
    declared_priority: Option<f64>,
    /// When it was last accepted for this command, in unix ms.
    last_used: Option<u64>,
    source: Source,
    /// Among equals the shorter text ranks first (`cd` above `chdir` for
    /// `c`): the text's length in chars, or 0 to keep the source's order (an
    /// empty needle, which scores nothing).
    length: usize,
    /// The label is `../`, which recency does not lift.
    is_parent_dir: bool,
    /// The label, for `sort = "alphabetical"`; empty otherwise.
    sort_label: String,
}

/// Port of Fig's `updatePriorities` (`sorting.ts:52-83`) for one suggestion.
fn effective_priority(
    declared: Option<f64>,
    last_used: Option<u64>,
    is_parent_dir: bool,
    sort: Sort,
) -> f64 {
    // `sorting.ts:56-62`: the recency index is used unless `alphabetical`.
    let recency = match sort {
        Sort::Recent => last_used.filter(|&ms| ms != 0),
        Sort::Alphabetical => None,
    };
    // `sorting.ts:68`: `../` is exempt.
    let recency = recency.filter(|_| !is_parent_dir);
    // `sorting.ts:69`.
    let boost = recency.map_or(0.0, |ms| ms as f64 / RECENCY_DIVISOR);
    // `sorting.ts:71-74`: `priority || 50` (0 and NaN are falsy), clamped
    // to 0..=100.
    let priority = match declared {
        Some(p) if p != 0.0 && !p.is_nan() => p.clamp(0.0, 100.0),
        _ => f64::from(DEFAULT_PRIORITY),
    };
    // `sorting.ts:76-80`: a used item at 50..=75 is lifted to 75.
    if recency.is_some() && (50.0..=75.0).contains(&priority) {
        75.0 + boost
    } else {
        priority + boost
    }
}

/// The best `keep` of `keys` in order, as indices, by the plan's list
/// (`phase-2-plan.md`, Engine and ranking): tier, fuzzy score, effective
/// priority, source, shorter text (or alphabetical), then input order. The
/// same as a stable sort of them all, cut to `keep`.
fn rank(keys: &[RankKey], sort: Sort, keep: usize) -> Vec<usize> {
    let priorities: Vec<f64> = keys
        .iter()
        .map(|key| {
            effective_priority(
                key.declared_priority,
                key.last_used,
                key.is_parent_dir,
                sort,
            )
        })
        .collect();
    let order = |&a: &usize, &b: &usize| {
        let (x, y) = (&keys[a], &keys[b]);
        x.tier
            .cmp(&y.tier)
            .then(y.score.cmp(&x.score))
            .then(priorities[b].total_cmp(&priorities[a]))
            .then(x.source.priority().cmp(&y.source.priority()))
            .then_with(|| match sort {
                Sort::Recent => x.length.cmp(&y.length),
                Sort::Alphabetical => alphabetical(&x.sort_label, &y.sort_label),
            })
            .then(a.cmp(&b))
    };
    let mut order_of: Vec<usize> = (0..keys.len()).collect();
    if keep == 0 {
        return Vec::new();
    }
    if keep < order_of.len() {
        order_of.select_nth_unstable_by(keep - 1, order);
        order_of.truncate(keep);
    }
    order_of.sort_unstable_by(order);
    order_of
}

fn alphabetical(a: &str, b: &str) -> std::cmp::Ordering {
    a.to_lowercase()
        .cmp(&b.to_lowercase())
        .then_with(|| a.cmp(b))
}

/// How to build a candidate's suggestion, from what it borrows.
enum Build<'a> {
    /// A spec item matched against `needle`, replacing the word from
    /// unquoted byte `from`.
    Spec {
        item: &'a Item,
        needle: &'a str,
        from: usize,
    },
    Compsys {
        candidate: &'a Candidate,
        needle: &'a str,
        from: usize,
        /// The match as shown and matched (unquoted for a `-Q` one).
        word: Cow<'a, str>,
        /// What it inserts, unquoted.
        plain: Cow<'a, str>,
        is_dir: bool,
        subcommand_position: bool,
    },
    File {
        entry: &'a files::FileEntry,
        needle: &'a str,
        from: usize,
    },
}

/// The candidates that matched, in source order.
#[derive(Default)]
struct Found<'a> {
    keys: Vec<RankKey>,
    builds: Vec<Build<'a>>,
}

impl<'a> Found<'a> {
    fn push(&mut self, key: RankKey, build: Build<'a>) {
        self.keys.push(key);
        self.builds.push(build);
    }

    fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
}

/// The texts offered so far; the first source to offer a text wins. Every
/// text is the start of the word being typed, up to unquoted byte `head`,
/// and then `rest`. It is kept split where it stops agreeing with the word,
/// a place that depends on the text alone, so equal texts get equal keys
/// and a text is looked up without being put together.
struct Seen<'a> {
    word: &'a str,
    texts: HashSet<(usize, Cow<'a, str>), Fnv>,
}

impl<'a> Seen<'a> {
    fn new(word: &'a str) -> Self {
        Self {
            word,
            texts: HashSet::default(),
        }
    }

    /// Add `word[..head]` + `rest`; false when it was there already.
    fn insert(&mut self, head: usize, rest: Cow<'a, str>) -> bool {
        let tail = self.word.as_bytes().get(head..).unwrap_or_default();
        let mut same = tail
            .iter()
            .zip(rest.as_bytes())
            .take_while(|(a, b)| a == b)
            .count();
        while !rest.is_char_boundary(same) {
            same -= 1;
        }
        let rest = match rest {
            Cow::Borrowed(rest) => Cow::Borrowed(rest.get(same..).unwrap_or_default()),
            Cow::Owned(mut rest) => {
                rest.drain(..same);
                Cow::Owned(rest)
            }
        };
        self.texts.insert((head + same, rest))
    }
}

impl Engine {
    pub fn new() -> Self {
        Self {
            matcher: FuzzyMatcher::new(),
            indices: Vec::new(),
            scratch: String::new(),
        }
    }

    /// The worker query for this report: the cwd and LBUFFER cut at the
    /// context cut point, or just past the opening quote mark when that is
    /// further right. `None` for an empty (or all-blank) buffer.
    pub fn query_key(report: &BufferReport, cwd: &Path, dialect: Dialect) -> Option<QueryKey> {
        if report.buffer.trim().is_empty() {
            return None;
        }
        let (lbuffer, rbuffer) = split_buffer(report);
        let ctx = tokenizer::analyze(dialect, &report.prebuffer, lbuffer, rbuffer);
        let w = &ctx.current;
        let mut cut = ctx.cut.min(w.cursor);
        // compsys offers options only for a word that starts with `-`, so
        // the context keeps the dash: `ls -l` asks for `ls -`.
        if cut == 0 && w.cursor > 0 && w.unquoted.starts_with('-') {
            cut = 1;
        }
        let at = if cut == 0 {
            w.typed.start
        } else {
            w.map.get(cut).copied().unwrap_or(w.typed.end)
        };
        // Inside an open quote compsys completes for that quote (`cd '` is
        // not `cd `: its `-Q` names are quoted differently), so the quote
        // mark stays in the context.
        let at = at.max(w.quote_open.unwrap_or(0)).min(lbuffer.len());
        let cut_lbuffer = lbuffer.get(..at).unwrap_or(lbuffer);
        Some(QueryKey {
            cwd: cwd.to_string_lossy().into_owned(),
            lbuffer: cut_lbuffer.to_string(),
        })
    }

    /// The suggestions, what the spec parse still needs, and the command
    /// accepts are remembered under.
    pub fn compute(&mut self, input: &EngineInput) -> Computed {
        self.compute_with(input, RankSettings::global())
    }

    /// The name of the spec for a report's line: its command word's (a
    /// path's base name), or `None` at command position.
    pub fn spec_name(report: &BufferReport, dialect: Dialect) -> Option<String> {
        let (lbuffer, rbuffer) = split_buffer(report);
        let ctx = tokenizer::analyze(dialect, &report.prebuffer, lbuffer, rbuffer);
        spec_command(&ctx).map(str::to_owned)
    }

    fn compute_with(&mut self, input: &EngineInput, settings: RankSettings) -> Computed {
        let report = input.report;
        if report.buffer.trim().is_empty() {
            return Computed::default();
        }
        let (lbuffer, rbuffer) = split_buffer(report);
        let ctx = tokenizer::analyze(input.shell.into(), &report.prebuffer, lbuffer, rbuffer);
        let w = &ctx.current;
        let mut computed = Computed {
            usage_command: ctx.words_before.first().cloned().unwrap_or_default(),
            ..Computed::default()
        };
        let command = spec_command(&ctx);
        if command.is_none() {
            // Command position: no arg, so no generators (Fig's are `[]`).
            computed.spec_arg = SpecArg::None;
        }
        if command.is_some_and(|command| settings.is_disabled(command)) {
            return computed;
        }

        // The spec's parse, when the spec and what it needs are there.
        let mut parsed = None;
        if let (Some(command), Some(specs)) = (command, input.specs) {
            match specs.spec(command) {
                None => {
                    computed.need = Some(Need::Spec {
                        name: command.to_owned(),
                    });
                }
                Some(root) => {
                    let current = w.unquoted.get(..w.cursor).unwrap_or("");
                    match parser::parse(&root, &ctx.words_before[1..], current, specs) {
                        ParseResult::Needs(need) => computed.need = Some(need),
                        ParseResult::Ready(suggest) => parsed = Some(suggest),
                    }
                }
            }
        }
        if let Some(suggest) = &parsed {
            if settings.is_disabled(&suggest.command) {
                return computed;
            }
            computed.usage_command.clone_from(&suggest.command);
        }

        let settings = match &parsed {
            Some(suggest) => settings.with_strategy(match &suggest.arg {
                Some(arg) => arg.filter_strategy,
                None => suggest.filter_strategy,
            }),
            None => settings,
        };
        let scope = Scope {
            settings,
            command: &computed.usage_command,
            usage: input.usage,
        };
        // What the candidates borrow, so they are built only once ranked.
        let current = w.unquoted.get(..w.cursor).unwrap_or("");
        let arg = parsed.as_ref().and_then(|suggest| {
            suggest.arg.as_ref().map(|arg| ArgContext {
                spec: suggest.spec.clone(),
                generation: suggest.generation,
                command: suggest.command.clone(),
                tokens: suggest.tokens.clone(),
                arg: arg.clone(),
                search_term: current
                    .get(floor_boundary(current, suggest.query_start)..)
                    .unwrap_or("")
                    .to_owned(),
            })
        });
        let generated = match (&arg, input.generated) {
            (Some(arg), Some(generated)) => generated_lists(arg, generated, current, settings),
            _ => Vec::new(),
        };
        let listing;
        let mut found = Found::default();
        let mut seen = Seen::new(&w.unquoted);

        match &parsed {
            Some(suggest) => {
                self.spec_candidates(&ctx, &scope, suggest, &generated, &mut seen, &mut found);
                // zsh's completions fill in what the spec lacks.
                if let CompsysState::Ready(candidates) = &input.compsys {
                    self.compsys_candidates(
                        input,
                        &ctx,
                        &scope,
                        candidates,
                        subcommand_position(suggest, current),
                        &mut seen,
                        &mut found,
                    );
                }
                // Where the spec has nothing for an arg yet (its generators
                // have not answered), the file fast path stands in for
                // compsys until it answers, as in phase 1, so the list does
                // not go empty in between (`cd fix`).
                let start = floor_boundary(&w.unquoted, suggest.query_start.min(w.cursor));
                listing = (suggest.arg.is_some()
                    && found.is_empty()
                    && !matches!(input.compsys, CompsysState::Ready(_)))
                .then(|| list_files(input, w, start));
                if let Some(listing) = &listing {
                    let dirs_only = directories_only(input, &ctx);
                    self.file_candidates(
                        listing, &scope, w, start, dirs_only, &mut seen, &mut found,
                    );
                }
            }
            None => {
                if let CompsysState::Ready(candidates) = &input.compsys {
                    self.compsys_candidates(
                        input, &ctx, &scope, candidates, false, &mut seen, &mut found,
                    );
                }
                // The file fast path stands in for compsys until it answers;
                // once it has, its answer is the list (`cd` takes
                // directories only).
                listing = (!ctx.command_position
                    && !matches!(input.compsys, CompsysState::Ready(_)))
                .then(|| list_files(input, w, 0));
                if let Some(listing) = &listing {
                    let dirs_only = directories_only(input, &ctx);
                    self.file_candidates(listing, &scope, w, 0, dirs_only, &mut seen, &mut found);
                }
            }
        }

        // History lines are never ranked among the rest: up to 3, newest
        // first, after everything, with room reserved in the cap.
        let history = if settings.history && !lbuffer.is_empty() {
            history_suggestions(input.history, lbuffer)
        } else {
            Vec::new()
        };
        let shown = rank(&found.keys, settings.sort, MAX_SUGGESTIONS - history.len());
        let mut suggestions = Vec::with_capacity(shown.len() + history.len());
        for at in shown {
            if let Some(build) = found.builds.get(at)
                && let Some(suggestion) = self.build(build, &ctx, settings)
            {
                suggestions.push(suggestion);
            }
        }
        suggestions.extend(history);
        computed.suggestions = suggestions;
        // Every item offered while an arg is being typed carries it.
        if let Some(arg) = parsed.as_ref().and_then(|suggest| suggest.arg.as_ref()) {
            let text = |text: Option<&str>| {
                text.map(collapse_spaces)
                    .filter(|text| !text.trim().is_empty())
            };
            let (name, description) = (text(arg.name.as_deref()), text(arg.description.as_deref()));
            for suggestion in &mut computed.suggestions {
                suggestion.arg_name.clone_from(&name);
                suggestion.arg_description.clone_from(&description);
            }
        }
        // The candidates borrow the arg's generators' items.
        drop(seen);
        drop(found);
        drop(generated);
        computed.spec_arg = match (&parsed, arg) {
            (Some(_), Some(arg)) => SpecArg::Some(Box::new(arg)),
            (Some(_), None) => SpecArg::None,
            (None, _) => computed.spec_arg,
        };
        computed
    }

    /// The spec's items that match the query term (from `query_start`),
    /// each by its best-matching name; a hidden item only on an exact
    /// match. Their texts (every name of every item) go into `seen`, so
    /// compsys does not offer them again.
    ///
    /// The generators' items go where Fig puts them (`getAllSuggestions`,
    /// `suggestions/index.ts:208-231`): after the subcommands and the arg's
    /// own suggestions, before `additionalSuggestions` and the options,
    /// each generator's matched against its own query term.
    fn spec_candidates<'a>(
        &mut self,
        ctx: &'a LineContext,
        scope: &Scope,
        suggest: &'a Suggest,
        generated: &'a [GenList<'a>],
        seen: &mut Seen<'a>,
        found: &mut Found<'a>,
    ) {
        let w = &ctx.current;
        let current = w.unquoted.get(..w.cursor).unwrap_or("");
        let start = floor_boundary(current, suggest.query_start);
        let query = current.get(start..).unwrap_or("");
        let at = suggest
            .items
            .iter()
            .position(|item| matches!(item.kind, ItemKind::Additional | ItemKind::Option))
            .unwrap_or(suggest.items.len());
        let (before, after) = suggest.items.split_at(at);
        for item in before {
            self.spec_candidate(w, scope, suggest, item, query, start, seen, found);
        }
        for list in generated {
            let needle = list.items.query_term.as_ref();
            for item in list.items.items.iter() {
                // In an option chain only options and args are kept
                // (`suggestions/index.ts:242-244`).
                if suggest.in_option_chain
                    && !matches!(
                        item.suggestion_type,
                        Some(SuggestionType::Arg | SuggestionType::Option)
                    )
                {
                    continue;
                }
                self.spec_candidate(w, scope, suggest, item, needle, list.from, seen, found);
            }
        }
        for item in after {
            self.spec_candidate(w, scope, suggest, item, query, start, seen, found);
        }
    }

    /// One spec item, matched against `query`, which starts at unquoted
    /// byte `start` of the word. An item that would insert a name with a
    /// control character is left out (an insert value goes in as the spec
    /// wrote it).
    #[allow(clippy::too_many_arguments)]
    fn spec_candidate<'a>(
        &mut self,
        w: &'a Word,
        scope: &Scope,
        suggest: &Suggest,
        item: &'a Item,
        query: &'a str,
        start: usize,
        seen: &mut Seen<'a>,
        found: &mut Found<'a>,
    ) {
        if item.names.is_empty() {
            return;
        }
        // In an option chain an arg is matched against an empty query
        // and inserted after the chain (`suggestions/index.ts:266-269`).
        let chain_arg =
            suggest.in_option_chain && matches!(item.suggestion_type, Some(SuggestionType::Arg));
        let (needle, from) = if chain_arg {
            ("", w.cursor)
        } else {
            (query, start)
        };
        let Some(matched) = self.match_item(item, needle, scope.settings, false) else {
            return;
        };
        let text = insert_text(item, matched.name);
        if !text.verbatim && text.text.chars().any(char::is_control) {
            return;
        }
        let plain = text.plain();
        let length = if needle.is_empty() {
            0
        } else {
            char_len(w.unquoted.get(..from).unwrap_or("")) + char_len(&plain)
        };
        if !seen.insert(from, plain) {
            return;
        }
        for name in &item.names {
            seen.insert(from, Cow::Borrowed(name));
            // A folder's name ends in `/`; compsys's word for it does not.
            if let Some(bare) = name.strip_suffix('/') {
                seen.insert(from, Cow::Borrowed(bare));
            }
        }
        let is_parent_dir = match &item.display_name {
            Some(display) => display == "../",
            None => item.names.len() == 1 && item.names[0] == "../",
        };
        let sort_label = match scope.settings.sort {
            Sort::Recent => String::new(),
            Sort::Alphabetical => item
                .display_name
                .clone()
                .unwrap_or_else(|| item.names.join(", ")),
        };
        found.push(
            RankKey {
                tier: matched.tier,
                score: matched.score,
                declared_priority: item.priority,
                last_used: scope.last_used(&item.names[0]),
                source: Source::Spec,
                length,
                is_parent_dir,
                sort_label,
            },
            Build::Spec { item, needle, from },
        );
    }

    /// How `item` matches `needle`: each name on its own, the best tier
    /// (then score) winning, its highlight placed where that name is in
    /// the label; a `displayName` is the label and is matched too, though
    /// never as an exact match (`suggestions/index.ts:442-489`). A hidden
    /// item matches only exactly. Without `full`, the label and highlights
    /// are left empty: ranking needs only the tier, the score and the name.
    fn match_item(
        &mut self,
        item: &Item,
        needle: &str,
        settings: RankSettings,
        full: bool,
    ) -> Option<ItemMatch> {
        // Better first: the tier, then the higher score.
        let rank = |tier: Tier, score: u32| (tier, std::cmp::Reverse(score));
        let mut best: Option<(Tier, u32, Vec<u32>, usize)> = None;
        let mut offset = 0u32;
        for (i, name) in item.names.iter().enumerate() {
            if i > 0 {
                offset += 2;
            }
            if let Some((tier, score)) = self.classify(needle, name, settings)
                && (!item.hidden || matches!(tier, Tier::Exact | Tier::ExactIgnoringCase))
                && best.as_ref().is_none_or(|(best_tier, best_score, ..)| {
                    rank(tier, score) < rank(*best_tier, *best_score)
                })
            {
                let highlights = if full {
                    self.indices.iter().map(|at| at + offset).collect()
                } else {
                    Vec::new()
                };
                best = Some((tier, score, highlights, i));
            }
            offset += char_count(name);
        }
        if item.hidden && best.is_none() {
            return None;
        }
        let Some(display) = &item.display_name else {
            let (tier, score, highlights, name) = best?;
            return Some(ItemMatch {
                tier,
                score,
                label: if full {
                    item.names.join(", ")
                } else {
                    String::new()
                },
                highlights,
                name: Some(name),
            });
        };
        // The label is the display name: a name's match is not in it.
        let by_display = self
            .classify(needle, display, settings)
            .map(|(tier, score)| {
                let tier = match tier {
                    Tier::Exact | Tier::ExactIgnoringCase => Tier::CaseSensitivePrefix,
                    other => other,
                };
                let highlights = if full {
                    self.indices.clone()
                } else {
                    Vec::new()
                };
                (tier, score, highlights)
            });
        let (tier, score, highlights, name) = match (best, by_display) {
            (Some((tier, score, _, name)), Some(display)) => {
                if rank(display.0, display.1) <= rank(tier, score) {
                    (display.0, display.1, display.2, Some(name))
                } else {
                    (tier, score, display.2, Some(name))
                }
            }
            (Some((tier, score, _, name)), None) => (tier, score, Vec::new(), Some(name)),
            (None, Some((tier, score, highlights))) => (tier, score, highlights, None),
            (None, None) => return None,
        };
        Some(ItemMatch {
            tier,
            score,
            label: if full { display.clone() } else { String::new() },
            highlights,
            name,
        })
    }

    /// zsh's candidates for the word. `subcommand_position`: the spec's
    /// parse says the word is a subcommand there, so a name zsh offers is
    /// one (git's `cherry` at `git ch`). A match that is or inserts a name
    /// with a control character is left out.
    #[allow(clippy::too_many_arguments)]
    fn compsys_candidates<'a>(
        &mut self,
        input: &EngineInput,
        ctx: &'a LineContext,
        scope: &Scope,
        candidates: &'a [Candidate],
        subcommand_position: bool,
        seen: &mut Seen<'a>,
        found: &mut Found<'a>,
    ) {
        let w = &ctx.current;
        let cut = ctx.cut.min(w.cursor);
        let fish = ctx.dialect == Dialect::Fish;
        for c in candidates {
            if fish {
                self.fish_candidate(input, ctx, scope, c, subcommand_position, seen, found);
                continue;
            }
            // `PREFIX` is in typed form (`my\ docs/`); `cut` counts unquoted bytes.
            let prefix_len = tokenizer::unquote(ctx.dialect, &c.prefix, w.quote)
                .map_or(c.prefix.len(), |p| p.len());
            let from = floor_boundary(
                &w.unquoted,
                cut.saturating_sub(prefix_len)
                    .max(c.iprefix.len())
                    .min(w.cursor),
            );
            let needle_from = floor_boundary(&w.unquoted, cut.max(c.iprefix.len()).min(w.cursor));
            let needle = w.unquoted.get(needle_from..w.cursor).unwrap_or("");
            // A `-Q` match arrives quoted (`My\ Documents`): it is shown,
            // matched and stat'ed unquoted, and inserted as it is.
            let word = if c.verbatim {
                Cow::Owned(
                    tokenizer::unquote(ctx.dialect, &c.word, w.quote)
                        .unwrap_or_else(|| c.word.clone()),
                )
            } else {
                Cow::Borrowed(c.word.as_str())
            };
            if input.shell == ShellKind::Zsh
                && ctx.command_position
                && is_zsh_internal(c, &word, needle)
            {
                continue;
            }
            let Some((tier, score)) = self.classify(needle, &word, scope.settings) else {
                continue;
            };
            let plain = if c.verbatim {
                let insertion = c.insertion();
                Cow::Owned(
                    tokenizer::unquote(ctx.dialect, &insertion, w.quote).unwrap_or(insertion),
                )
            } else if c.apre.is_empty() && c.hpre.is_empty() && c.hsuf.is_empty() {
                Cow::Borrowed(c.word.as_str())
            } else {
                Cow::Owned(c.insertion())
            };
            if word.chars().any(char::is_control) || plain.chars().any(char::is_control) {
                continue;
            }
            if !seen.insert(from, plain.clone()) {
                continue;
            }
            let is_dir = c.suffix.as_deref() == Some("/")
                || (c.is_file && (input.is_dir)(&resolve(&plain, input.cwd, input.home)));
            let head = w.unquoted.get(..from).unwrap_or("");
            let length = if needle.is_empty() {
                0
            } else {
                char_len(head) + char_len(&plain)
            };
            let last_used = self.last_used_text(scope, head, &plain);
            let sort_label = match scope.settings.sort {
                Sort::Recent => String::new(),
                Sort::Alphabetical if is_dir => format!("{word}/"),
                Sort::Alphabetical => word.clone().into_owned(),
            };
            found.push(
                RankKey {
                    tier,
                    score,
                    declared_priority: None,
                    last_used,
                    source: Source::Compsys,
                    length,
                    is_parent_dir: is_dir && word == "..",
                    sort_label,
                },
                Build::Compsys {
                    candidate: c,
                    needle,
                    from,
                    word,
                    plain,
                    is_dir,
                    subcommand_position,
                },
            );
        }
    }

    /// One of fish's matches. `complete -C` gives the whole token, unquoted
    /// (`src/main.rs` for `src/m`): when it starts with the word up to the
    /// cut point it replaces the word from there, as zsh's would, else the
    /// whole word (a match fish found ignoring case, a variable's `$HOME`).
    #[allow(clippy::too_many_arguments)]
    fn fish_candidate<'a>(
        &mut self,
        input: &EngineInput,
        ctx: &'a LineContext,
        scope: &Scope,
        c: &'a Candidate,
        subcommand_position: bool,
        seen: &mut Seen<'a>,
        found: &mut Found<'a>,
    ) {
        let w = &ctx.current;
        let cut = floor_boundary(&w.unquoted, ctx.cut.min(w.cursor));
        let head = w.unquoted.get(..cut).unwrap_or("");
        let from = if c.word.starts_with(head) { cut } else { 0 };
        let word = c.word.get(from..).unwrap_or("");
        let needle = w.unquoted.get(from..w.cursor).unwrap_or("");
        if word.is_empty() || word.chars().any(char::is_control) {
            return;
        }
        let Some((tier, score)) = self.classify(needle, word, scope.settings) else {
            return;
        };
        if !seen.insert(from, Cow::Borrowed(word)) {
            return;
        }
        let is_dir = c.suffix.as_deref() == Some("/")
            || (c.is_file && (input.is_dir)(&resolve(word, input.cwd, input.home)));
        let head = w.unquoted.get(..from).unwrap_or("");
        let length = if needle.is_empty() {
            0
        } else {
            char_len(head) + char_len(word)
        };
        let last_used = self.last_used_text(scope, head, word);
        let sort_label = match scope.settings.sort {
            Sort::Recent => String::new(),
            Sort::Alphabetical if is_dir => format!("{word}/"),
            Sort::Alphabetical => word.to_owned(),
        };
        found.push(
            RankKey {
                tier,
                score,
                declared_priority: None,
                last_used,
                source: Source::Compsys,
                length,
                is_parent_dir: is_dir && word == "..",
                sort_label,
            },
            Build::Compsys {
                candidate: c,
                needle,
                from,
                word: Cow::Borrowed(word),
                plain: Cow::Borrowed(word),
                is_dir,
                subcommand_position,
            },
        );
    }

    /// The files for the word's path from unquoted byte `start` on (after
    /// an option's `=` for a spec's option arg), from `listing`.
    #[allow(clippy::too_many_arguments)]
    fn file_candidates<'a>(
        &mut self,
        listing: &'a files::Listing,
        scope: &Scope,
        w: &'a Word,
        start: usize,
        dirs_only: bool,
        seen: &mut Seen<'a>,
        found: &mut Found<'a>,
    ) {
        let before_cursor = w.unquoted.get(start..w.cursor).unwrap_or("");
        let at = listing.dir_prefix.len().min(before_cursor.len());
        let from = start + at;
        let needle = before_cursor.get(at..).unwrap_or("");
        let head = w.unquoted.get(..from).unwrap_or("");
        for entry in listing
            .entries
            .iter()
            .filter(|entry| entry.is_dir || !dirs_only)
        {
            let Some((tier, score)) = self.classify(needle, &entry.name, scope.settings) else {
                continue;
            };
            // Compsys (already in `seen`) wins over a file with the same text.
            if !seen.insert(from, Cow::Borrowed(&entry.name)) {
                continue;
            }
            let length = if needle.is_empty() {
                0
            } else {
                char_len(head) + char_len(&entry.name)
            };
            let last_used = self.last_used_text(scope, head, &entry.name);
            let sort_label = match scope.settings.sort {
                Sort::Recent => String::new(),
                Sort::Alphabetical if entry.is_dir => format!("{}/", entry.name),
                Sort::Alphabetical => entry.name.clone(),
            };
            found.push(
                RankKey {
                    tier,
                    score,
                    declared_priority: None,
                    last_used,
                    source: Source::File,
                    length,
                    is_parent_dir: entry.is_dir && entry.name == "..",
                    sort_label,
                },
                Build::File {
                    entry,
                    needle,
                    from,
                },
            );
        }
    }

    /// When the text `head` + `rest` was last accepted, put together in a
    /// reused buffer only when the command has any.
    fn last_used_text(&mut self, scope: &Scope, head: &str, rest: &str) -> Option<u64> {
        if !scope.usage.has_command(scope.command) {
            return None;
        }
        self.scratch.clear();
        self.scratch.push_str(head);
        self.scratch.push_str(rest);
        scope.last_used(&self.scratch)
    }

    /// The suggestion a ranked candidate shows.
    fn build(
        &mut self,
        build: &Build,
        ctx: &LineContext,
        settings: RankSettings,
    ) -> Option<Suggestion> {
        let w = &ctx.current;
        match build {
            Build::Spec { item, needle, from } => {
                self.build_spec(item, needle, *from, w, ctx.dialect, settings)
            }
            Build::Compsys {
                candidate: c,
                needle,
                from,
                word,
                plain,
                is_dir,
                subcommand_position,
            } => {
                let is_dir = *is_dir;
                let suffix = match &c.suffix {
                    Some(s) => s.clone(),
                    None if is_dir => "/".to_string(),
                    None => " ".to_string(),
                };
                let close = w.quote != Quote::None && suffix != "/";
                // fish's match is `plain` itself; a variable's or a home
                // dir's at the word's start keeps its `$` or `~`.
                let (insertion, verbatim) = if ctx.dialect == Dialect::Fish {
                    (
                        Cow::Borrowed(plain.as_ref()),
                        fish_verbatim(*from, plain, w),
                    )
                } else {
                    (Cow::Owned(c.insertion()), c.verbatim)
                };
                let mut inserted = quote_for(ctx.dialect, &insertion, w.quote, verbatim, close);
                // A `-Q` match is left as it is, but it still ends the open quote.
                if verbatim && close {
                    inserted.push_str(closing_mark(ctx.dialect, w.quote));
                }
                let text = format!("{}{plain}", w.unquoted.get(..*from).unwrap_or(""));
                // The highlights of the match, as ranking found it.
                self.classify(needle, word, settings)?;
                let label = if is_dir {
                    format!("{word}/")
                } else {
                    word.clone().into_owned()
                };
                // A command name (a command, function, alias or builtin) at
                // command position, or a name at the spec's subcommand
                // position, is drawn as a subcommand.
                let kind = if is_dir {
                    Kind::Folder
                } else if c.is_file {
                    Kind::File
                } else if ctx.command_position || *subcommand_position {
                    Kind::Subcommand
                } else {
                    Kind::Arg
                };
                Some(Suggestion {
                    usage_name: text.clone(),
                    id: SuggestionId {
                        source: Source::Compsys,
                        text,
                    },
                    label,
                    description: c.description.clone(),
                    highlights: self.indices.clone(),
                    is_dir,
                    kind,
                    args_hint: String::new(),
                    icon: None,
                    arg_name: None,
                    arg_description: None,
                    runs: false,
                    accept: AcceptPlan::Replace {
                        from: *from,
                        inserted,
                        rest: None,
                        suffix,
                        dialect: ctx.dialect,
                    },
                })
            }
            Build::File {
                entry,
                needle,
                from,
            } => {
                self.classify(needle, &entry.name, settings)?;
                let text = format!("{}{}", w.unquoted.get(..*from).unwrap_or(""), entry.name);
                let suffix = if entry.is_dir { "/" } else { " " };
                let close = w.quote != Quote::None && suffix != "/";
                let inserted = quote_for(ctx.dialect, &entry.name, w.quote, false, close);
                let label = if entry.is_dir {
                    format!("{}/", entry.name)
                } else {
                    entry.name.clone()
                };
                Some(Suggestion {
                    usage_name: text.clone(),
                    id: SuggestionId {
                        source: Source::File,
                        text,
                    },
                    label,
                    description: String::new(),
                    highlights: self.indices.clone(),
                    is_dir: entry.is_dir,
                    kind: if entry.is_dir {
                        Kind::Folder
                    } else {
                        Kind::File
                    },
                    args_hint: String::new(),
                    icon: None,
                    arg_name: None,
                    arg_description: None,
                    runs: false,
                    accept: AcceptPlan::Replace {
                        from: *from,
                        inserted,
                        rest: None,
                        suffix: suffix.to_string(),
                        dialect: ctx.dialect,
                    },
                })
            }
        }
    }

    /// A spec item's suggestion: its label and highlights as it matched,
    /// and what accepting it inserts.
    fn build_spec(
        &mut self,
        item: &Item,
        needle: &str,
        from: usize,
        w: &Word,
        dialect: Dialect,
        settings: RankSettings,
    ) -> Option<Suggestion> {
        let matched = self.match_item(item, needle, settings, true)?;
        let insertion = insertion(item, matched.name);
        let id = SuggestionId {
            source: Source::Spec,
            text: format!(
                "{}{}",
                w.unquoted.get(..from).unwrap_or(""),
                insertion.plain
            ),
        };
        let close =
            w.quote != Quote::None && insertion.rest.is_none() && !insertion.plain.ends_with('/');
        let (inserted, rest) = if insertion.verbatim {
            (insertion.before, insertion.rest)
        } else {
            (
                quote_for(dialect, &insertion.before, w.quote, false, close),
                insertion.rest,
            )
        };
        let kind = item_kind(item);
        Some(Suggestion {
            id,
            label: matched.label,
            description: item
                .description
                .as_deref()
                .map(collapse_spaces)
                .unwrap_or_default(),
            highlights: matched.highlights,
            is_dir: kind == Kind::Folder,
            kind,
            args_hint: item.args_hint.clone(),
            icon: item.icon.clone(),
            arg_name: None,
            arg_description: None,
            runs: insertion.runs,
            usage_name: item.names.first().cloned().unwrap_or_default(),
            accept: AcceptPlan::Replace {
                from,
                inserted,
                rest,
                suffix: insertion.suffix,
                dialect,
            },
        })
    }

    /// The tier of `needle` against `word` and, in the fuzzy tier, the
    /// fuzzy score; leaves the highlighted char indices in `self.indices`.
    /// `None` drops the candidate: it does not match, or only fuzzily while
    /// `fuzzy` is off. An empty needle puts everything in the
    /// case-sensitive prefix tier.
    fn classify(
        &mut self,
        needle: &str,
        word: &str,
        settings: RankSettings,
    ) -> Option<(Tier, u32)> {
        self.indices.clear();
        if needle.is_empty() {
            return Some((Tier::CaseSensitivePrefix, 0));
        }
        // `helpers.ts:47-50`; the label of a directory may end in `/`.
        let plain = word.strip_suffix('/').unwrap_or(word);
        let tier = if plain == needle {
            Some(Tier::Exact)
        } else if lower(plain).eq(lower(needle)) {
            Some(Tier::ExactIgnoringCase)
        } else if word.starts_with(needle) {
            Some(Tier::CaseSensitivePrefix)
        } else if starts_with_ci(word, needle) {
            Some(Tier::CaseInsensitivePrefix)
        } else {
            None
        };
        if let Some(tier) = tier {
            // Highlight the typed prefix.
            self.indices.extend(0..char_count(needle));
            return Some((tier, 0));
        }
        if !settings.fuzzy {
            return None;
        }
        self.matcher
            .score_with_indices(needle, word, &mut self.indices)
            .map(|score| (Tier::Fuzzy, score))
    }

    /// The new (LBUFFER, RBUFFER) for accepting `s` against the report it
    /// was computed from.
    pub fn accept(s: &Suggestion, report: &BufferReport) -> (String, String) {
        build_accept(s, report, true)
    }

    /// Whether accepting `s` would leave the buffer unchanged (ignoring the
    /// trailing suffix).
    pub fn accept_is_noop(s: &Suggestion, report: &BufferReport) -> bool {
        let (lbuffer, rbuffer) = split_buffer(report);
        let (new_l, new_r) = build_accept(s, report, false);
        new_l == lbuffer && new_r == rbuffer
    }
}

/// One generator's items for the arg being typed, and where its query term
/// starts in the word.
struct GenList<'a> {
    items: GenItems<'a>,
    from: usize,
}

/// What the arg's generators show now, in order. With `history = false`
/// the `history` template shows nothing.
fn generated_lists<'a>(
    arg: &'a ArgContext,
    generated: &'a dyn Generated,
    current: &str,
    settings: RankSettings,
) -> Vec<GenList<'a>> {
    let mut lists = Vec::new();
    for generator in arg_generators(&arg.arg).iter() {
        if !settings.history && is_history_template(&generator.generator) {
            continue;
        }
        let Some(items) = generated.items(arg, generator) else {
            continue;
        };
        // Fig deletes the query term's length and types the text.
        let from = floor_boundary(
            current,
            current.len().saturating_sub(items.query_term.len()),
        );
        lists.push(GenList { items, from });
    }
    lists
}

/// The file fast path's listing for the word's path from unquoted byte
/// `start` on.
fn list_files(input: &EngineInput, w: &Word, start: usize) -> files::Listing {
    let before_cursor = w.unquoted.get(start..w.cursor).unwrap_or("");
    files::list(before_cursor, input.cwd, input.home)
}

/// What ranking one call needs besides the candidates.
struct Scope<'a> {
    settings: RankSettings,
    /// The command accepts are remembered under (`""` at command position).
    command: &'a str,
    usage: &'a Usage,
}

impl Scope<'_> {
    fn last_used(&self, name: &str) -> Option<u64> {
        self.usage.last_used(self.command, name)
    }
}

/// Up to 3 history lines that start with `lbuffer`, newest first.
fn history_suggestions(history: &History, lbuffer: &str) -> Vec<Suggestion> {
    history
        .newest_first()
        .filter(|e| e.len() > lbuffer.len() && e.starts_with(lbuffer))
        .take(MAX_HISTORY)
        .map(|entry| {
            let mut lines = entry.lines();
            let first = lines.next().unwrap_or("");
            let label = if lines.next().is_some() {
                format!("{first}…")
            } else {
                first.to_string()
            };
            Suggestion {
                id: SuggestionId {
                    source: Source::History,
                    text: entry.to_string(),
                },
                label,
                description: String::new(),
                highlights: Vec::new(),
                is_dir: false,
                kind: Kind::History,
                args_hint: String::new(),
                icon: None,
                arg_name: None,
                arg_description: None,
                runs: false,
                usage_name: entry.to_string(),
                accept: AcceptPlan::Line(entry.to_string()),
            }
        })
        .collect()
}

fn build_accept(s: &Suggestion, report: &BufferReport, with_suffix: bool) -> (String, String) {
    let (lbuffer, rbuffer) = split_buffer(report);
    let (from, inserted, rest, suffix, dialect) = match &s.accept {
        AcceptPlan::Line(line) => return (line.clone(), String::new()),
        AcceptPlan::Replace {
            from,
            inserted,
            rest,
            suffix,
            dialect,
        } => (*from, inserted, rest, suffix, *dialect),
    };
    let ctx = tokenizer::analyze(dialect, &report.prebuffer, lbuffer, rbuffer);
    let range = tokenizer::replace_range(&ctx.current, from);
    let line = format!("{lbuffer}{rbuffer}");
    let (Some(head), Some(tail)) = (line.get(..range.start), line.get(range.end..)) else {
        return (lbuffer.to_string(), rbuffer.to_string());
    };
    let mut new_l = head.to_string();
    // A backspace takes back the character before it, as it does when Fig
    // types the text into the terminal.
    for ch in inserted.chars() {
        if ch == BACKSPACE {
            new_l.pop();
        } else {
            new_l.push(ch);
        }
    }
    let suffix = if with_suffix && !(suffix == " " && tail.starts_with(' ')) {
        suffix.as_str()
    } else {
        ""
    };
    match rest {
        None => {
            new_l.push_str(suffix);
            (new_l, tail.to_string())
        }
        Some(rest) => (new_l, format!("{rest}{suffix}{tail}")),
    }
}

/// Where an insert value puts the cursor.
const CURSOR: &str = "{cursor}";
const BACKSPACE: char = '\u{8}';

/// How a spec item matched: its tier and score, and its label with the
/// matched characters.
struct ItemMatch {
    tier: Tier,
    score: u32,
    label: String,
    highlights: Vec<u32>,
    /// The name that matched best; none when only the display name did.
    name: Option<usize>,
}

/// What accepting a spec item inserts.
struct Insertion {
    /// The text before the cursor.
    before: String,
    /// The text after the cursor, when `{cursor}` placed it.
    rest: Option<String>,
    /// The whole text, for the item's identity.
    plain: String,
    /// An insert value goes in as it is; a name is quoted.
    verbatim: bool,
    suffix: String,
    /// The insert value ended in a newline: the line runs once it is in.
    runs: bool,
}

/// The text accepting a spec item inserts, before `{cursor}` splits it.
struct InsertText<'a> {
    /// The insert value without its newlines at the end, or the name.
    text: &'a str,
    /// An option's separator, which the cursor follows (never with an
    /// insert value).
    separator: Option<&'a str>,
    /// An insert value goes in as it is; a name is quoted.
    verbatim: bool,
    /// The insert value ended in a newline.
    runs: bool,
}

impl<'a> InsertText<'a> {
    /// The text with the separator and the cursor after it.
    fn full(&self) -> Cow<'a, str> {
        match self.separator {
            Some(separator) => Cow::Owned(format!("{}{separator}{CURSOR}", self.text)),
            None => Cow::Borrowed(self.text),
        }
    }

    /// The whole text, as the item's identity: without its `{cursor}`.
    fn plain(&self) -> Cow<'a, str> {
        match self.full() {
            Cow::Borrowed(text) if !text.contains(CURSOR) => Cow::Borrowed(text),
            full => Cow::Owned(match full.split_once(CURSOR) {
                Some((before, after)) => format!("{before}{after}"),
                None => full.into_owned(),
            }),
        }
    }
}

/// What accepting `item` inserts (`getFullInsertion`,
/// `state/insertion.ts:165-191`): its `insertValue` as it is (but for a file
/// or folder), else the name that matched best (`name`, an index into its
/// names; the first when only its display name matched) and, for an option
/// that needs one, its separator with the cursor after it.
///
/// `makeInsertTextForItem` (`state/insertion.ts:193-224`): newlines at the
/// end of an insert value collapse to one, which runs the line once the
/// text is in (a newline elsewhere stays in the text). Only an insert value
/// does that: it is what the spec asked for, while a name is data (a file
/// name, a generator's output), and one with a control character in it is
/// never inserted (see [`Engine::spec_candidate`]).
fn insert_text(item: &Item, name: Option<usize>) -> InsertText<'_> {
    let file_or_folder = matches!(
        item.suggestion_type,
        Some(SuggestionType::File | SuggestionType::Folder)
    );
    match &item.insert_value {
        Some(value) if !file_or_folder => {
            let text = value.trim_end_matches('\n');
            InsertText {
                text,
                separator: None,
                verbatim: true,
                runs: text.len() < value.len(),
            }
        }
        _ => InsertText {
            text: name
                .and_then(|at| item.names.get(at))
                .or(item.names.first())
                .map_or("", String::as_str),
            separator: item.separator.as_deref(),
            verbatim: false,
            runs: false,
        },
    }
}

/// [`insert_text`], split around the cursor: `{cursor}` (its first
/// occurrence) splits it. A space follows when the item says so
/// (`shouldAddSpace`) and the line does not run, after the text and its
/// `{cursor}` part, with the cursor before it.
fn insertion(item: &Item, name: Option<usize>) -> Insertion {
    let text = insert_text(item, name);
    let full = text.full();
    let (before, rest) = match full.split_once(CURSOR) {
        Some((before, after)) => (before.to_owned(), Some(after.to_owned())),
        None => (full.clone().into_owned(), None),
    };
    let suffix = if item.should_add_space && !text.runs {
        " "
    } else {
        ""
    };
    Insertion {
        before,
        rest,
        plain: text.plain().into_owned(),
        verbatim: text.verbatim,
        suffix: suffix.to_owned(),
        runs: text.runs,
    }
}

/// Whether the file fast path, standing in for compsys, offers directories
/// only: for a command compsys completes directories for, until it answers.
/// zsh only.
fn directories_only(input: &EngineInput, ctx: &LineContext) -> bool {
    input.shell == ShellKind::Zsh
        && matches!(input.compsys, CompsysState::Pending)
        && ctx
            .words_before
            .first()
            .is_some_and(|command| DIRECTORY_COMMANDS.contains(&command.as_str()))
}

/// A spec's description as Fig shows it, in HTML: every run of white
/// space (a template literal's line breaks too) is one space.
fn collapse_spaces(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The kind of a spec item, by its type (`mixin` shows as `special`); an
/// untyped one is an arg, or, among `additionalSuggestions`, special.
fn item_kind(item: &Item) -> Kind {
    match &item.suggestion_type {
        Some(SuggestionType::Subcommand) => Kind::Subcommand,
        Some(SuggestionType::Option) => Kind::Option,
        Some(SuggestionType::Arg) => Kind::Arg,
        Some(SuggestionType::Folder) => Kind::Folder,
        Some(SuggestionType::File) => Kind::File,
        Some(SuggestionType::Special | SuggestionType::Mixin) => Kind::Special,
        Some(SuggestionType::Shortcut) => Kind::Shortcut,
        Some(SuggestionType::Other(_)) | None if item.kind == ItemKind::Additional => Kind::Special,
        Some(SuggestionType::Other(_)) | None => Kind::Arg,
    }
}

/// Whether the parse puts the word being typed (`current`, up to the
/// cursor) at a subcommand: it is not an option and the parse offers
/// subcommands for it (a failed parse offers none). An optional arg beside
/// them (git's `alias`) does not change that.
fn subcommand_position(suggest: &Suggest, current: &str) -> bool {
    !suggest.in_option_chain
        && !current.starts_with('-')
        && suggest
            .items
            .iter()
            .any(|item| item.kind == ItemKind::Subcommand)
}

/// The command whose spec a line is parsed against: the first word's base
/// name (`/usr/bin/git` is `git`), or `None` at command position.
fn spec_command(ctx: &LineContext) -> Option<&str> {
    if ctx.command_position {
        return None;
    }
    let first = ctx.words_before.first()?;
    let base = first.rsplit('/').next().unwrap_or(first);
    (!base.is_empty()).then_some(base)
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

/// compsys builtins: zsh's completion machinery, not commands to run.
const COMPSYS_BUILTINS: &[&str] = &[
    "compadd",
    "compset",
    "compctl",
    "comparguments",
    "compcall",
    "compdescribe",
    "compfiles",
    "compgroups",
    "compquote",
    "comptags",
    "comptry",
    "compvalues",
];

/// compsys's own functions: shown at command position only when typed in full.
const COMPSYS_FUNCTIONS: &[&str] = &[
    "compinit",
    "compdef",
    "compdump",
    "compaudit",
    "compinstall",
];

/// A command-position match Fig and Warp would not show: a completion
/// function or other `_` name (unless the user is typing one), a compsys
/// builtin, one of compsys's own functions (unless typed in full), or a
/// parameter. `word` is the match as shown.
///
/// The parameter group is the `parameters` tag, or with a user's
/// `descriptions` format style the formatted `parameter` explanation.
fn is_zsh_internal(c: &Candidate, word: &str, needle: &str) -> bool {
    (word.starts_with('_') && !needle.starts_with('_'))
        || COMPSYS_BUILTINS.contains(&word)
        || (COMPSYS_FUNCTIONS.contains(&word) && needle != word)
        || c.group == "parameters"
        || c.group
            .split(|ch: char| !ch.is_alphanumeric())
            .any(|part| part == "parameter")
}

/// Whether fish's match `plain`, replacing the word from unquoted byte
/// `from`, goes in as it is: a variable (`$HOME`) or a home dir (`~`,
/// `~user`) that fish expanded the word's start to, typed unescaped, which
/// quoting would make literal. Their names need no quoting.
fn fish_verbatim(from: usize, plain: &str, w: &Word) -> bool {
    let name = |rest: &str, extra: &str| {
        rest.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || extra.contains(c))
    };
    // The word's first character took one typed byte: not `\$` or `\~`.
    let unescaped = matches!(w.map.as_slice(), [first, second, ..] if *second == first + 1);
    from == 0
        && w.quote == Quote::None
        && unescaped
        && match (plain.chars().next(), w.unquoted.chars().next()) {
            (Some('$'), Some('$')) => plain.len() > 1 && name(&plain[1..], ""),
            (Some('~'), Some('~')) => name(&plain[1..], ".-"),
            _ => false,
        }
}

/// The mark that ends an open `quote`, in `dialect`.
fn closing_mark(dialect: Dialect, quote: Quote) -> &'static str {
    match dialect {
        Dialect::Zsh | Dialect::Bash => closing_mark_zsh(quote),
        // fish has no `$'...'`: a single quote stands in for it.
        Dialect::Fish => match quote {
            Quote::None => "",
            Quote::Single | Quote::Dollar => "'",
            Quote::Double => "\"",
        },
    }
}

/// [`closing_mark`] by zsh's rules.
fn closing_mark_zsh(quote: Quote) -> &'static str {
    match quote {
        Quote::None => "",
        Quote::Single | Quote::Dollar => "'",
        Quote::Double => "\"",
    }
}

fn char_len(s: &str) -> usize {
    s.chars().count()
}

/// `s` lowercased, char by char (no allocation).
fn lower(s: &str) -> impl Iterator<Item = char> + '_ {
    s.chars().flat_map(char::to_lowercase)
}

/// Whether `haystack` starts with `needle`, ignoring case.
fn starts_with_ci(haystack: &str, needle: &str) -> bool {
    let mut hay = lower(haystack);
    lower(needle).all(|c| hay.next() == Some(c))
}

fn char_count(s: &str) -> u32 {
    u32::try_from(s.chars().count()).unwrap_or(u32::MAX)
}

fn floor_boundary(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// A candidate's path as the file system sees it.
fn resolve(path: &str, cwd: &Path, home: &Path) -> PathBuf {
    if path == "~" {
        home.to_path_buf()
    } else if let Some(rest) = path.strip_prefix("~/") {
        home.join(rest)
    } else {
        cwd.join(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::complete::test_util::TempDir;
    use crate::shell::protocol::ReportFlags;
    use crate::specs::generated::Generators;
    use std::fs;

    /// The config defaults: fuzzy on, most recent first.
    const DEFAULTS: RankSettings = RankSettings {
        fuzzy: true,
        sort: Sort::Recent,
        history: true,
        disabled: &[],
    };

    /// A report for `text` with the cursor at `|` (or at the end).
    fn report(text: &str) -> BufferReport {
        let (buffer, cursor) = match text.find('|') {
            Some(i) => (text.replacen('|', "", 1), text[..i].chars().count()),
            None => (text.to_string(), text.chars().count()),
        };
        BufferReport {
            seq: 1,
            context: "start".to_string(),
            cursor,
            flags: ReportFlags::default(),
            prebuffer: String::new(),
            buffer,
            undrawn: Vec::new(),
        }
    }

    fn cand(word: &str) -> Candidate {
        Candidate {
            word: word.to_string(),
            display: word.to_string(),
            description: String::new(),
            group: String::new(),
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

    struct Fixture {
        root: TempDir,
        history: History,
        /// The shell the lines are typed in; zsh unless a test says.
        shell: ShellKind,
    }

    impl Fixture {
        fn new() -> Self {
            let root = TempDir::new("engine");
            let p = root.path();
            fs::create_dir_all(p.join("cwd/My Documents")).expect("mkdir");
            fs::create_dir_all(p.join("cwd/my docs")).expect("mkdir");
            fs::create_dir_all(p.join("cwd/src")).expect("mkdir");
            fs::write(p.join("cwd/my file.txt"), "").expect("write");
            fs::write(p.join("cwd/a.txt"), "").expect("write");
            fs::create_dir_all(p.join("home/Dev")).expect("mkdir");
            fs::create_dir_all(p.join("home/Documents")).expect("mkdir");
            Self {
                root,
                history: History::empty(),
                shell: ShellKind::Zsh,
            }
        }

        fn cwd(&self) -> PathBuf {
            self.root.path().join("cwd")
        }

        fn home(&self) -> PathBuf {
            self.root.path().join("home")
        }

        fn run(&self, text: &str, compsys: CompsysState) -> Vec<Suggestion> {
            self.run_with(text, compsys, &Usage::default(), DEFAULTS)
        }

        fn run_with(
            &self,
            text: &str,
            compsys: CompsysState,
            usage: &Usage,
            settings: RankSettings,
        ) -> Vec<Suggestion> {
            self.compute(text, compsys, None, usage, settings)
                .suggestions
        }

        fn compute(
            &self,
            text: &str,
            compsys: CompsysState,
            specs: Option<&dyn Resolved>,
            usage: &Usage,
            settings: RankSettings,
        ) -> Computed {
            self.compute_generated(text, compsys, specs, None, usage, settings)
        }

        fn compute_generated(
            &self,
            text: &str,
            compsys: CompsysState,
            specs: Option<&dyn Resolved>,
            generated: Option<&dyn Generated>,
            usage: &Usage,
            settings: RankSettings,
        ) -> Computed {
            let r = report(text);
            let (cwd, home) = (self.cwd(), self.home());
            let is_dir = |p: &Path| p.is_dir();
            Engine::new().compute_with(
                &EngineInput {
                    shell: self.shell,
                    report: &r,
                    cwd: &cwd,
                    home: &home,
                    history: &self.history,
                    usage,
                    compsys,
                    is_dir: &is_dir,
                    specs,
                    generated,
                },
                settings,
            )
        }
    }

    fn labels(s: &[Suggestion]) -> Vec<&str> {
        s.iter().map(|s| s.label.as_str()).collect()
    }

    fn pick<'a>(s: &'a [Suggestion], label: &str) -> &'a Suggestion {
        s.iter()
            .find(|s| s.label == label)
            .unwrap_or_else(|| panic!("no suggestion {label:?} in {:?}", labels(s)))
    }

    fn accepted(text: &str, label: &str, compsys: CompsysState) -> (String, String) {
        let f = Fixture::new();
        let s = f.run(text, compsys);
        Engine::accept(pick(&s, label), &report(text))
    }

    #[test]
    fn query_keys() {
        let cwd = Path::new("/w");
        let key = |t: &str| Engine::query_key(&report(t), cwd, Dialect::Zsh).map(|k| k.lbuffer);
        assert_eq!(key("git ch").as_deref(), Some("git "));
        assert_eq!(key("ls --color=al").as_deref(), Some("ls --color="));
        assert_eq!(key("cd ~/De").as_deref(), Some("cd ~/"));
        assert_eq!(key("ls /usr/lo").as_deref(), Some("ls /usr/"));
        assert_eq!(key("ls -").as_deref(), Some("ls -"));
        assert_eq!(key("ls -l").as_deref(), Some("ls -"));
        assert_eq!(key("ls --col").as_deref(), Some("ls -"));
        assert_eq!(key("ls '-l").as_deref(), Some("ls '-"));
        // An open quote stays in the context: `cd '` is not `cd `.
        assert_eq!(key("cd '").as_deref(), Some("cd '"));
        assert_eq!(key("cd 'fix").as_deref(), Some("cd '"));
        assert_eq!(key("cd 'my docs/sr").as_deref(), Some("cd 'my docs/"));
        assert_eq!(key("cat \"my").as_deref(), Some("cat \""));
        assert_eq!(key("cat $'my").as_deref(), Some("cat $'"));
        assert_eq!(key("cd ab'cd").as_deref(), Some("cd ab'"));
        assert_eq!(key("cd 'my d|ocs'").as_deref(), Some("cd '"));
        // A closed quote does not.
        assert_eq!(key("cd 'a b'").as_deref(), Some("cd "));
        assert_eq!(key("").as_deref(), None);
        assert_eq!(key("   ").as_deref(), None);
        assert_eq!(
            Engine::query_key(&report("git ch"), cwd, Dialect::Zsh).map(|k| k.cwd),
            Some("/w".to_string())
        );
    }

    #[test]
    fn accept_compsys_word() {
        let mut c = cand("checkout");
        c.description = "Switch branches".into();
        let got = accepted("git ch", "checkout", CompsysState::Ready(&[c]));
        assert_eq!(got, ("git checkout ".to_string(), String::new()));
    }

    #[test]
    fn accept_option_argument() {
        let mut c = cand("always");
        c.iprefix = "--color=".into();
        let got = accepted("ls --color=al", "always", CompsysState::Ready(&[c]));
        assert_eq!(got.0, "ls --color=always ");
    }

    #[test]
    fn accept_path_with_suffix() {
        let mut c = cand("local");
        c.hpre = "/usr/".into();
        c.prefix = "/usr/lo".into();
        c.is_file = true;
        c.suffix = Some("/".into());
        let got = accepted("ls /usr/lo", "local/", CompsysState::Ready(&[c]));
        assert_eq!(got.0, "ls /usr/local/");
    }

    #[test]
    fn accept_compsys_path_in_cut_context() {
        // The worker was asked `ls /usr/`; the typed dir is the ignored prefix.
        let mut c = cand("local");
        c.iprefix = "/usr/".into();
        c.is_file = true;
        c.suffix = Some("/".into());
        let got = accepted("ls /usr/lo", "local/", CompsysState::Ready(&[c]));
        assert_eq!(got.0, "ls /usr/local/");
    }

    #[test]
    fn accept_variable() {
        let mut c = cand("HOME");
        c.iprefix = "$".into();
        c.prefix = "HO".into();
        let got = accepted("echo $HO", "HOME", CompsysState::Ready(&[c]));
        assert_eq!(got.0, "echo $HOME ");
    }

    #[test]
    fn accept_files_with_quoting() {
        let none = || CompsysState::Unavailable;
        assert_eq!(
            accepted("cd My\\ Do", "My Documents/", none()).0,
            "cd My\\ Documents/"
        );
        assert_eq!(accepted("cd 'my do", "my docs/", none()).0, "cd 'my docs/");
        assert_eq!(
            accepted("cat 'my fi", "my file.txt", none()).0,
            "cat 'my file.txt' "
        );
        assert_eq!(
            accepted("cat my\\ fi", "my file.txt", none()).0,
            "cat my\\ file.txt "
        );
    }

    #[test]
    fn accept_mid_word_drops_suffix_before_space() {
        let c = cand("checkout");
        let text = "git chec|kout main";
        let f = Fixture::new();
        let s = f.run(text, CompsysState::Ready(&[c]));
        let got = Engine::accept(pick(&s, "checkout"), &report(text));
        assert_eq!(got, ("git checkout".to_string(), " main".to_string()));
    }

    #[test]
    fn accept_history_replaces_the_line() {
        let mut f = Fixture::new();
        f.history.add("git commit -m x");
        let s = f.run("git c", CompsysState::Ready(&[]));
        assert_eq!(labels(&s), ["git commit -m x"]);
        assert_eq!(s[0].id.source, Source::History);
        assert_eq!(
            Engine::accept(&s[0], &report("git c")),
            ("git commit -m x".to_string(), String::new())
        );
    }

    #[test]
    fn compsys_custom_suffix_and_close_quote() {
        let mut c = cand("name");
        c.suffix = Some("=".into());
        let got = accepted("cmd 'na", "name", CompsysState::Ready(&[c]));
        assert_eq!(got.0, "cmd 'name'=");
    }

    #[test]
    fn noop_ignores_suffix() {
        let f = Fixture::new();
        let r = report("ls");
        let s = f.run("ls", CompsysState::Ready(&[cand("ls"), cand("lsof")]));
        assert!(Engine::accept_is_noop(pick(&s, "ls"), &r));
        assert!(!Engine::accept_is_noop(pick(&s, "lsof"), &r));
    }

    #[test]
    fn nothing_for_blank_buffer() {
        let mut f = Fixture::new();
        f.history.add("ls");
        assert!(f.run("", CompsysState::Pending).is_empty());
        assert!(f.run("  ", CompsysState::Pending).is_empty());
    }

    #[test]
    fn ranking_exact_then_source_then_score() {
        let mut f = Fixture::new();
        f.history.add("ls src extra");
        let cands = [cand("srcfoo"), cand("sxrxc"), cand("src")];
        let s = f.run("ls src", CompsysState::Ready(&cands));
        // exact first; then the other compsys words; then files; then history.
        assert_eq!(s[0].label, "src");
        assert_eq!(s[0].id.source, Source::Compsys);
        let sources: Vec<Source> = s.iter().map(|s| s.id.source).collect();
        let mut sorted = sources.clone();
        sorted.sort_by_key(|s| s.priority());
        assert_eq!(sources[1..], sorted[1..]);
        assert_eq!(s.last().map(|s| s.id.source), Some(Source::History));
    }

    #[test]
    fn shorter_text_wins_a_tie() {
        let f = Fixture::new();
        let mut m = FuzzyMatcher::new();
        let mut indices = Vec::new();
        let mut score = |needle: &str, word: &str| m.score_with_indices(needle, word, &mut indices);
        // What Fig shows: the shorter name first when the match is as good.
        assert_eq!(score("ec", "echotc"), score("ec", "echo"));
        let cands = [cand("echotc"), cand("echoti"), cand("echo")];
        let s = f.run("ec", CompsysState::Ready(&cands));
        assert_eq!(labels(&s), ["echo", "echotc", "echoti"]);
        assert_eq!(score("c", "chdir"), score("c", "cd"));
        let s = f.run("c", CompsysState::Ready(&[cand("chdir"), cand("cd")]));
        assert_eq!(labels(&s), ["cd", "chdir"]);
        // A better score still wins over a shorter text.
        assert!(score("che", "checkout") > score("che", "chxe"));
        let s = f.run(
            "git che",
            CompsysState::Ready(&[cand("chxe"), cand("checkout")]),
        );
        assert_eq!(labels(&s), ["checkout", "chxe"]);
        // Files too, within their source.
        let s = f.run("ls a", CompsysState::Pending);
        assert_eq!(labels(&s), ["a.txt"]);
        // An empty needle scores nothing: the source's order stays.
        let s = f.run("git ", CompsysState::Ready(&[cand("checkout"), cand("am")]));
        assert_eq!(labels(&s), ["checkout", "am"]);
    }

    #[test]
    fn exact_file_match_beats_compsys() {
        let f = Fixture::new();
        let cands = [cand("srcfoo")];
        // `.`-less word, compsys pending: files only; `src` is exact.
        let s = f.run("ls sr", CompsysState::Ready(&cands));
        assert!(s.iter().all(|s| s.id.source == Source::Compsys));
        let s = f.run("ls src", CompsysState::Pending);
        assert_eq!(s[0].label, "src/");
    }

    #[test]
    fn ties_keep_order_and_cap_is_50() {
        let f = Fixture::new();
        let cands: Vec<Candidate> = (0..80).map(|i| cand(&format!("w{i:02}"))).collect();
        let s = f.run("x ", CompsysState::Ready(&cands));
        assert_eq!(s.len(), 50);
        assert_eq!(s[0].label, "w00");
        assert_eq!(s[49].label, "w49");
    }

    #[test]
    fn compsys_duplicates_are_deduped() {
        let f = Fixture::new();
        let mut c = cand("src");
        c.is_file = true;
        // The same match from two tags (`cd` is both a command and a builtin).
        let s = f.run("ls s", CompsysState::Ready(&[c.clone(), c]));
        assert_eq!(labels(&s), ["src/"]);
        assert_eq!(s[0].id.source, Source::Compsys);
        assert!(s[0].is_dir, "is_file candidate stat'ed as a directory");
    }

    #[test]
    fn files_only_until_compsys_answers() {
        let f = Fixture::new();
        fs::create_dir_all(f.cwd().join("src/inner-b")).expect("mkdir");
        fs::write(f.cwd().join("src/inner-a"), "").expect("write");
        // Ready compsys, plain word: no files.
        let s = f.run("ls a", CompsysState::Ready(&[]));
        assert!(s.is_empty());
        // Ready compsys, path word: compsys's answer is the list, so `cd`
        // (directories only) does not get the file `inner-a`.
        let mut dir = cand("inner-b");
        dir.hpre = "src/".into();
        dir.prefix = "src/".into();
        dir.is_file = true;
        dir.verbatim = true;
        let s = f.run("cd src/", CompsysState::Ready(std::slice::from_ref(&dir)));
        assert_eq!(labels(&s), ["inner-b/"]);
        assert_eq!(s[0].id.source, Source::Compsys);
        assert!(f.run("ls ./a", CompsysState::Ready(&[])).is_empty());
        assert!(f.run("ls ~/Dev", CompsysState::Ready(&[])).is_empty());
        // Pending and Unavailable: the fast path's files, path words too.
        assert_eq!(
            labels(&f.run("ls src/", CompsysState::Pending)),
            ["inner-a", "inner-b/"]
        );
        assert_eq!(labels(&f.run("ls ./a", CompsysState::Pending)), ["a.txt"]);
        // `~/` expands to home.
        assert_eq!(labels(&f.run("ls ~/Dev", CompsysState::Pending)), ["Dev/"]);
        assert_eq!(labels(&f.run("ls a", CompsysState::Pending)), ["a.txt"]);
        assert_eq!(labels(&f.run("ls a", CompsysState::Unavailable)), ["a.txt"]);
        assert_eq!(labels(&f.run("ls a", CompsysState::Failed)), ["a.txt"]);
        assert_eq!(
            labels(&f.run("cd src/", CompsysState::Failed)),
            ["inner-a", "inner-b/"]
        );
        // Command position: never files.
        assert!(f.run("./s", CompsysState::Pending).is_empty());
        assert!(f.run("a", CompsysState::Unavailable).is_empty());
        assert!(f.run("a", CompsysState::Failed).is_empty());
    }

    #[test]
    fn directory_commands_get_directories_until_compsys_answers() {
        let f = Fixture::new();
        fs::create_dir_all(f.cwd().join("src/inner-b")).expect("mkdir");
        fs::write(f.cwd().join("src/inner-a"), "").expect("write");
        for command in ["cd", "pushd", "rmdir"] {
            let line = format!("{command} src/");
            assert_eq!(
                labels(&f.run(&line, CompsysState::Pending)),
                ["inner-b/"],
                "{command}"
            );
            assert_eq!(
                labels(&f.run(&format!("{command} "), CompsysState::Pending)),
                ["My Documents/", "my docs/", "src/"],
                "{command}"
            );
        }
        // Other commands get files too.
        assert_eq!(
            labels(&f.run("cat src/", CompsysState::Pending)),
            ["inner-a", "inner-b/"]
        );
        // Without compsys the fast path is the whole answer: as before.
        assert_eq!(
            labels(&f.run("cd src/", CompsysState::Failed)),
            ["inner-a", "inner-b/"]
        );
    }

    #[test]
    fn a_failed_query_leaves_files_and_history() {
        let mut f = Fixture::new();
        f.history.add("slowcmd a.txt --flag");
        let s = f.run("slowcmd a", CompsysState::Failed);
        assert_eq!(labels(&s), ["a.txt", "slowcmd a.txt --flag"]);
        assert_eq!(s[0].id.source, Source::File);
        assert_eq!(s[1].id.source, Source::History);
        // An answer with nothing in it is the list: no files.
        let s = f.run("slowcmd a", CompsysState::Ready(&[]));
        assert_eq!(labels(&s), ["slowcmd a.txt --flag"]);
    }

    /// A `-Q` match as `_path_files` adds it: already quoted for the word.
    fn quoted_dir(word: &str) -> Candidate {
        let mut c = cand(word);
        c.verbatim = true;
        c.is_file = true;
        c.group = "local-directories".into();
        c
    }

    #[test]
    fn verbatim_words_are_shown_and_stated_unquoted() {
        let f = Fixture::new();
        let mut c = quoted_dir("My\\ Documents");
        c.prefix = "My".into();
        let s = f.run("cd My", CompsysState::Ready(std::slice::from_ref(&c)));
        assert_eq!(labels(&s), ["My Documents/"]);
        assert!(s[0].is_dir);
        assert_eq!(s[0].id.text, "My Documents");
        // Inserted as compsys gave it, with the directory's slash.
        assert_eq!(
            Engine::accept(&s[0], &report("cd My")),
            ("cd My\\ Documents/".to_string(), String::new())
        );
        // Matched unquoted: the typed `My\ D` is the needle `My D`.
        c.prefix = "My\\ D".into();
        let s = f.run("cd My\\ D", CompsysState::Ready(std::slice::from_ref(&c)));
        assert_eq!(labels(&s), ["My Documents/"]);
        assert_eq!(s[0].highlights, vec![0, 1, 2, 3]);
        assert_eq!(
            Engine::accept(&s[0], &report("cd My\\ D")).0,
            "cd My\\ Documents/"
        );
    }

    #[test]
    fn verbatim_words_from_an_open_quote_context() {
        // What compsys gives for `cd '` and `cat '` (zsh 5.9): `-Q` names
        // quoted for the open quote (nothing to escape), `PREFIX` empty.
        let f = Fixture::new();
        let dir = quoted_dir("My Documents");
        let s = f.run("cd 'My", CompsysState::Ready(std::slice::from_ref(&dir)));
        assert_eq!(labels(&s), ["My Documents/"]);
        // A directory leaves the quote open.
        assert_eq!(
            Engine::accept(&s[0], &report("cd 'My")),
            ("cd 'My Documents/".to_string(), String::new())
        );
        let mut file = cand("my file.txt");
        file.verbatim = true;
        file.is_file = true;
        file.group = "globbed-files".into();
        let s = f.run("cat 'my", CompsysState::Ready(std::slice::from_ref(&file)));
        assert_eq!(labels(&s), ["my file.txt"]);
        assert!(!s[0].is_dir);
        // A file closes it.
        assert_eq!(
            Engine::accept(&s[0], &report("cat 'my")).0,
            "cat 'my file.txt' "
        );
        let s = f.run("cat \"my", CompsysState::Ready(std::slice::from_ref(&file)));
        assert_eq!(
            Engine::accept(&s[0], &report("cat \"my")).0,
            "cat \"my file.txt\" "
        );
        let s = f.run("cat $'my", CompsysState::Ready(std::slice::from_ref(&file)));
        assert_eq!(
            Engine::accept(&s[0], &report("cat $'my")).0,
            "cat $'my file.txt' "
        );
    }

    #[test]
    fn the_replaced_span_counts_prefix_unquoted() {
        // compsys leaves `x=` alone and reports `PREFIX` typed:
        // `my\ docs/` is 9 bytes typed, 8 unquoted. The span starts at
        // unquoted byte 10 - 8 = 2; with the typed length it would start
        // at 1 and eat the `=`.
        let f = Fixture::new();
        fs::create_dir_all(f.cwd().join("my docs/src")).expect("mkdir");
        let mut c = quoted_dir("src");
        c.hpre = "my\\ docs/".into();
        c.prefix = "my\\ docs/".into();
        assert_ne!(
            c.prefix.len(),
            tokenizer::unquote(Dialect::Zsh, &c.prefix, Quote::None).map_or(0, |p| p.len())
        );
        let text = "ls x=my\\ docs/";
        let s = f.run(text, CompsysState::Ready(std::slice::from_ref(&c)));
        assert_eq!(labels(&s), ["src/"]);
        assert_eq!(s[0].id.text, "x=my docs/src");
        assert_eq!(
            Engine::accept(&s[0], &report(text)).0,
            "ls x=my\\ docs/src/"
        );
        // A `PREFIX` that does not unquote counts as typed.
        let mut odd = cand("value");
        odd.prefix = "a b".into();
        odd.iprefix = "--opt=".into();
        let s = f.run(
            "cmd --opt=",
            CompsysState::Ready(std::slice::from_ref(&odd)),
        );
        assert_eq!(
            Engine::accept(&s[0], &report("cmd --opt=")).0,
            "cmd --opt=value "
        );
    }

    #[test]
    fn verbatim_words_inside_an_open_quote() {
        let f = Fixture::new();
        // In `cd 'M`, `_path_files` quotes for the open single quote.
        let mut c = quoted_dir("My Documents");
        c.prefix = "M".into();
        let s = f.run("cd 'M", CompsysState::Ready(std::slice::from_ref(&c)));
        assert_eq!(labels(&s), ["My Documents/"]);
        assert_eq!(
            Engine::accept(&s[0], &report("cd 'M")).0,
            "cd 'My Documents/"
        );
        // A path inside a directory: the hidden prefix is unquoted too.
        let mut c = quoted_dir("src");
        c.hpre = "my\\ docs/".into();
        c.prefix = "my\\ docs/".into();
        fs::create_dir_all(f.cwd().join("my docs/src")).expect("mkdir");
        let s = f.run(
            "ls my\\ docs/",
            CompsysState::Ready(std::slice::from_ref(&c)),
        );
        assert_eq!(labels(&s), ["src/"]);
        assert_eq!(s[0].id.text, "my docs/src");
        assert_eq!(
            Engine::accept(&s[0], &report("ls my\\ docs/")).0,
            "ls my\\ docs/src/"
        );
    }

    /// A compsys match at command position, shaped like what the worker
    /// reported for `c` and `cd` (zsh 5.9): `group` is the tag.
    fn command(word: &str, group: &str, verbatim: bool) -> Candidate {
        let mut c = cand(word);
        c.group = group.into();
        c.verbatim = verbatim;
        c
    }

    fn command_candidates() -> Vec<Candidate> {
        vec![
            command("cd", "commands", false),
            command("comm", "commands", false),
            command("compfiles", "builtins", true),
            command("comptags", "builtins", true),
            command("compadd", "builtins", true),
            command("chdir", "builtins", true),
            command("cd", "builtins", true),
            command("compadd", "functions", false),
            command("compinit", "functions", false),
            command("compdef", "functions", false),
            command("compdump", "functions", false),
            command("compaudit", "functions", false),
            command("compinstall", "functions", false),
            command("_cdr", "functions", false),
            command("_cd", "functions", false),
            command("_fac_encode_controls", "functions", false),
            command("cdx", "aliases", true),
            command("case", "reserved-words", true),
            command("cdpath", "parameters", false),
            command("CDPATH", "parameters", false),
            command("compprefuncs", "parameters", false),
        ]
    }

    /// bash's and fish's completions have no compsys internals to hide: a
    /// `_` name or a parameter there is a real command or function.
    #[test]
    fn only_zsh_hides_zsh_internals() {
        let cands = command_candidates();
        for shell in [ShellKind::Bash, ShellKind::Fish] {
            let f = Fixture {
                shell,
                ..Fixture::new()
            };
            let s = f.run("comp", CompsysState::Ready(&cands));
            assert!(
                labels(&s).contains(&"compadd"),
                "{shell:?}: {:?}",
                labels(&s)
            );
            assert!(
                labels(&s).contains(&"compinit"),
                "{shell:?}: {:?}",
                labels(&s)
            );
            let s = f.run("c", CompsysState::Ready(&cands));
            assert!(
                labels(&s).contains(&"cdpath"),
                "{shell:?}: {:?}",
                labels(&s)
            );
        }
    }

    /// fish's `complete -C` gives whole tokens, unquoted: each is cut at the
    /// word's cut point (`src/` of `src/main.rs`, `--color=`), inserted by
    /// fish's quoting, and a variable or `~` at the word's start keeps its
    /// `$` or `~`.
    #[test]
    fn fish_matches_are_whole_tokens() {
        let f = Fixture {
            shell: ShellKind::Fish,
            ..Fixture::new()
        };
        let run =
            |text: &str, c: &Candidate| f.run(text, CompsysState::Ready(std::slice::from_ref(c)));
        let accept = |text: &str, c: Candidate, label: &str| {
            let s = run(text, &c);
            Engine::accept(pick(&s, label), &report(text)).0
        };
        let file = || {
            let mut c = cand("src/main.rs");
            c.is_file = true;
            c
        };
        assert_eq!(labels(&run("facx src/m", &file())), ["main.rs"]);
        assert_eq!(accept("facx src/m", file(), "main.rs"), "facx src/main.rs ");
        // Found ignoring case: the whole word goes.
        assert_eq!(
            accept("facx SRC/m", file(), "src/main.rs"),
            "facx src/main.rs "
        );
        let dir = || {
            let mut c = cand("my docs");
            c.suffix = Some("/".into());
            c
        };
        assert_eq!(accept("facx my", dir(), "my docs/"), "facx my\\ docs/");
        assert_eq!(accept("facx 'my", dir(), "my docs/"), "facx 'my docs/");
        assert_eq!(
            accept("facx it", cand("it's.txt"), "it's.txt"),
            "facx it\\'s.txt "
        );
        assert_eq!(
            accept("facx \"it", cand("it's \"q\".txt"), "it's \"q\".txt"),
            "facx \"it's \\\"q\\\".txt\" "
        );
        assert_eq!(
            accept("facx 'it", cand("it's.txt"), "it's.txt"),
            "facx 'it\\'s.txt' "
        );
        assert_eq!(
            accept("facx --color=a", cand("--color=always"), "always"),
            "facx --color=always "
        );
        // A variable and a home dir, as fish expanded them.
        let home = || {
            let mut c = cand("$HOME");
            c.suffix = Some("/".into());
            c
        };
        assert_eq!(accept("facx $HO", home(), "$HOME/"), "facx $HOME/");
        assert_eq!(accept("facx ~ro", cand("~root"), "~root"), "facx ~root ");
        // A file named with a `$` the user escaped stays escaped.
        assert_eq!(accept("facx \\$", cand("$x"), "$x"), "facx \\$x ");
        assert_eq!(accept("facx \\~", cand("~x"), "~x"), "facx \\~x ");
    }

    /// Until the shell's completions answer, `cd` lists directories only in
    /// zsh (compsys's `cd` completes directories); bash and fish list files
    /// too.
    #[test]
    fn only_zsh_lists_directories_only_for_cd() {
        for (shell, file) in [
            (ShellKind::Zsh, false),
            (ShellKind::Bash, true),
            (ShellKind::Fish, true),
        ] {
            let f = Fixture {
                shell,
                ..Fixture::new()
            };
            let s = f.run("cd ", CompsysState::Pending);
            assert!(labels(&s).contains(&"src/"), "{shell:?}: {:?}", labels(&s));
            assert_eq!(
                labels(&s).contains(&"a.txt"),
                file,
                "{shell:?}: {:?}",
                labels(&s)
            );
        }
    }

    /// bash and fish read and quote lines by zsh's rules for now, and an
    /// accept re-reads the line by the rules it was made with.
    #[test]
    fn bash_and_fish_quote_as_zsh_for_now() {
        for shell in [ShellKind::Bash, ShellKind::Fish] {
            let f = Fixture {
                shell,
                ..Fixture::new()
            };
            let s = f.run("cat my", CompsysState::Pending);
            assert_eq!(
                Engine::accept(pick(&s, "my file.txt"), &report("cat my")).0,
                "cat my\\ file.txt ",
                "{shell:?}"
            );
            let s = f.run("cat 'my", CompsysState::Pending);
            assert_eq!(
                Engine::accept(pick(&s, "my file.txt"), &report("cat 'my")).0,
                "cat 'my file.txt' ",
                "{shell:?}"
            );
            let key = Engine::query_key(&report("git ch"), &f.cwd(), shell.into());
            assert_eq!(key.map(|key| key.lbuffer).as_deref(), Some("git "));
        }
    }

    #[test]
    fn command_position_hides_zsh_internals() {
        let f = Fixture::new();
        let cands = command_candidates();
        let s = f.run("c", CompsysState::Ready(&cands));
        // Equal scores: shorter first.
        assert_eq!(labels(&s), ["cd", "cdx", "comm", "case", "chdir"]);
        let s = f.run("cd", CompsysState::Ready(&cands));
        assert_eq!(labels(&s), ["cd", "cdx", "chdir"]);
        // compsys's own functions only when typed in full.
        for name in [
            "compinit",
            "compdef",
            "compdump",
            "compaudit",
            "compinstall",
        ] {
            let s = f.run(name, CompsysState::Ready(&cands));
            assert_eq!(labels(&s), [name]);
            let partial = &name[..name.len() - 1];
            let s = f.run(partial, CompsysState::Ready(&cands));
            assert!(!labels(&s).contains(&name), "{partial}: {:?}", labels(&s));
        }
        // Typing `_` asks for them.
        let s = f.run("_c", CompsysState::Ready(&cands));
        assert_eq!(labels(&s), ["_cd", "_cdr", "_fac_encode_controls"]);
        // With a `descriptions` format style, the group is the formatted
        // explanation.
        let styled = [
            command("cdpath", "%B[parameter]%b", false),
            command("cd", "%B[builtin command]%b", true),
        ];
        let s = f.run("cd", CompsysState::Ready(&styled));
        assert_eq!(labels(&s), ["cd"]);
    }

    /// Command names at command position (commands, functions, aliases,
    /// builtins) are drawn as subcommands; the same words as arguments stay
    /// args, and history keeps its own kind.
    #[test]
    fn command_names_are_subcommands_at_command_position() {
        let mut f = Fixture::new();
        f.history.add("cd somewhere");
        let cands = command_candidates();
        let s = f.run("c", CompsysState::Ready(&cands));
        let kinds: Vec<(&str, Kind)> = s.iter().map(|s| (s.label.as_str(), s.kind)).collect();
        assert!(
            kinds
                .iter()
                .filter(|(label, _)| *label != "cd somewhere")
                .all(|(_, kind)| *kind == Kind::Subcommand),
            "{kinds:?}"
        );
        assert!(
            kinds.contains(&("cd somewhere", Kind::History)),
            "{kinds:?}"
        );
        let s = f.run("which c", CompsysState::Ready(&cands));
        assert!(
            s.iter()
                .filter(|s| s.id.source == Source::Compsys)
                .all(|s| s.kind == Kind::Arg),
            "{:?}",
            labels(&s)
        );
    }

    #[test]
    fn arguments_keep_what_command_position_hides() {
        let f = Fixture::new();
        let mut home = command("HOME", "parameters", false);
        home.iprefix = "$".into();
        home.prefix = "HO".into();
        let s = f.run("echo $HO", CompsysState::Ready(std::slice::from_ref(&home)));
        assert_eq!(labels(&s), ["HOME"]);
        let s = f.run("which _c", CompsysState::Ready(&command_candidates()));
        assert!(labels(&s).contains(&"_cdr"));
        let s = f.run("which c", CompsysState::Ready(&command_candidates()));
        assert!(labels(&s).contains(&"compfiles") && labels(&s).contains(&"_cd"));
    }

    #[test]
    fn history_rules() {
        let mut f = Fixture::new();
        for h in ["git a", "git b", "git c", "git d", "other", "git d"] {
            f.history.add(h);
        }
        f.history.add("git multi\nline");
        let s = f.run("git ", CompsysState::Ready(&[]));
        // newest first, at most 3; multi-line ends in an ellipsis.
        assert_eq!(labels(&s), ["git multi…", "git d", "git c"]);
        // Equal to LBUFFER is not longer: not offered.
        let s = f.run("git d", CompsysState::Ready(&[]));
        assert!(s.is_empty());
        // Cursor mid-line: only LBUFFER counts.
        let s = f.run("git |zzz", CompsysState::Ready(&[]));
        assert_eq!(s.len(), 3);
    }

    #[test]
    fn history_survives_the_cap() {
        let mut f = Fixture::new();
        let cands: Vec<Candidate> = (0..141).map(|i| cand(&format!("c{i:03}"))).collect();
        let without = f.run("git c", CompsysState::Ready(&cands));
        assert_eq!(without.len(), MAX_SUGGESTIONS);
        f.history.add("git c-old");
        f.history.add("git c-new");
        let s = f.run("git c", CompsysState::Ready(&cands));
        assert_eq!(s.len(), MAX_SUGGESTIONS);
        // Newest history first, at the end.
        assert_eq!(labels(&s)[48..], ["git c-new", "git c-old"]);
        // The compsys part is the best-ranked prefix of what it was before.
        assert_eq!(labels(&s)[..48], labels(&without)[..48]);
    }

    #[test]
    fn highlights_are_label_char_indices() {
        let f = Fixture::new();
        let s = f.run("git chk", CompsysState::Ready(&[cand("checkout")]));
        assert_eq!(s[0].highlights, vec![0, 1, 4]);
    }

    #[test]
    fn fuzzy_filters_compsys() {
        let f = Fixture::new();
        let cands = [cand("checkout"), cand("commit"), cand("cherry-pick")];
        let s = f.run("git ch", CompsysState::Ready(&cands));
        assert_eq!(labels(&s), ["checkout", "cherry-pick"]);
    }

    /// A ranked item for `rank` tests: nothing but what ranking reads.
    fn item(label: &str, source: Source, tier: Tier) -> RankKey {
        RankKey {
            tier,
            score: 0,
            declared_priority: None,
            last_used: None,
            source,
            length: label.chars().count(),
            is_parent_dir: label == "../",
            sort_label: label.to_string(),
        }
    }

    fn ranked_labels(ranked: Vec<RankKey>, sort: Sort) -> Vec<String> {
        rank(&ranked, sort, MAX_SUGGESTIONS)
            .into_iter()
            .map(|at| ranked[at].sort_label.clone())
            .collect()
    }

    const USED: u64 = 1_792_663_021_000;

    #[test]
    fn each_tier_beats_the_next() {
        let f = Fixture::new();
        let cands = [
            cand("acbh"),
            cand("CHy"),
            cand("chx"),
            cand("CH"),
            cand("ch"),
        ];
        let s = f.run("git ch", CompsysState::Ready(&cands));
        // exact (the same case first), case-sensitive prefix,
        // case-insensitive prefix, fuzzy.
        assert_eq!(labels(&s), ["ch", "CH", "chx", "CHy", "acbh"]);
        // A directory's slash does not stop it being exact.
        let s = f.run("ls src", CompsysState::Pending);
        assert_eq!(s[0].label, "src/");
        let s = f.run("ls SRC", CompsysState::Pending);
        assert_eq!(labels(&s), ["src/"]);
    }

    #[test]
    fn prefix_matches_highlight_the_typed_prefix() {
        let f = Fixture::new();
        let s = f.run("git CH", CompsysState::Ready(&[cand("checkout")]));
        assert_eq!(labels(&s), ["checkout"]);
        assert_eq!(s[0].highlights, vec![0, 1]);
    }

    #[test]
    fn the_fuzzy_score_orders_within_the_fuzzy_tier() {
        let f = Fixture::new();
        let mut m = FuzzyMatcher::new();
        let mut idx = Vec::new();
        let better = m.score_with_indices("ch", "xch", &mut idx);
        let worse = m.score_with_indices("ch", "xcxxxh", &mut idx);
        assert!(better > worse, "{better:?} {worse:?}");
        let s = f.run(
            "git ch",
            CompsysState::Ready(&[cand("xcxxxh"), cand("xch")]),
        );
        assert_eq!(labels(&s), ["xch", "xcxxxh"]);
        // A prefix match beats a better fuzzy score.
        let s = f.run(
            "git ch",
            CompsysState::Ready(&[cand("xch"), cand("choooooooooooooo")]),
        );
        assert_eq!(labels(&s), ["choooooooooooooo", "xch"]);
    }

    #[test]
    fn priority_orders_within_a_tier() {
        let mut high = item("high", Source::Compsys, Tier::CaseSensitivePrefix);
        high.declared_priority = Some(90.0);
        let mut low = item("low", Source::Compsys, Tier::CaseSensitivePrefix);
        low.declared_priority = Some(10.0);
        let plain = item("plain", Source::Compsys, Tier::CaseSensitivePrefix);
        assert_eq!(
            ranked_labels(vec![low, plain, high], Sort::Recent),
            ["high", "plain", "low"]
        );
        // The tier still comes first.
        let mut fuzzy_high = item("fuzzy", Source::Compsys, Tier::Fuzzy);
        fuzzy_high.declared_priority = Some(100.0);
        let prefix = item("prefix", Source::Compsys, Tier::CaseSensitivePrefix);
        assert_eq!(
            ranked_labels(vec![fuzzy_high, prefix], Sort::Recent),
            ["prefix", "fuzzy"]
        );
    }

    #[test]
    fn figs_priority_rules() {
        let boost = USED as f64 / 1e13;
        let p = |declared, last_used, parent| {
            effective_priority(declared, last_used, parent, Sort::Recent)
        };
        // `priority || 50`, clamped.
        assert_eq!(p(None, None, false), 50.0);
        assert_eq!(p(Some(0.0), None, false), 50.0);
        assert_eq!(p(Some(200.0), None, false), 100.0);
        // A used item at 50..=75 is lifted to 75, plus the fraction.
        assert_eq!(p(None, Some(USED), false), 75.0 + boost);
        assert_eq!(p(Some(75.0), Some(USED), false), 75.0 + boost);
        // Outside it the fraction is just added.
        assert_eq!(p(Some(49.0), Some(USED), false), 49.0 + boost);
        assert_eq!(p(Some(76.0), Some(USED), false), 76.0 + boost);
        // `../` is exempt; so is a time of 0.
        assert_eq!(p(None, Some(USED), true), 50.0);
        assert_eq!(p(None, Some(0), false), 50.0);
        // Alphabetical: no boost at all.
        assert_eq!(
            effective_priority(None, Some(USED), false, Sort::Alphabetical),
            50.0
        );
    }

    #[test]
    fn recency_lifts_an_item_and_beats_74() {
        // An item at 50 that was used before beats an unused one at 74.
        let mut used = item("used", Source::Compsys, Tier::CaseSensitivePrefix);
        used.last_used = Some(USED);
        let mut unused = item("unused", Source::Compsys, Tier::CaseSensitivePrefix);
        unused.declared_priority = Some(74.0);
        assert_eq!(
            ranked_labels(vec![unused, used], Sort::Recent),
            ["used", "unused"]
        );
        // A more recent use beats an older one.
        let mut older = item("older", Source::Compsys, Tier::CaseSensitivePrefix);
        older.last_used = Some(USED - 5000);
        let mut newer = item("newer", Source::Compsys, Tier::CaseSensitivePrefix);
        newer.last_used = Some(USED);
        assert_eq!(
            ranked_labels(vec![older, newer], Sort::Recent),
            ["newer", "older"]
        );
        // `../` is not lifted.
        let mut parent = item("../", Source::File, Tier::CaseSensitivePrefix);
        parent.last_used = Some(USED);
        let plain = item("a/", Source::File, Tier::CaseSensitivePrefix);
        assert_eq!(
            ranked_labels(vec![plain, parent], Sort::Recent),
            ["a/", "../"]
        );
    }

    #[test]
    fn usage_lifts_the_accepted_name_for_its_command() {
        let f = Fixture::new();
        let cands = [cand("alpha"), cand("beta"), cand("gamma")];
        let mut usage = Usage::default();
        usage.record("git", "gamma", USED);
        usage.record("npm", "beta", USED);
        let s = f.run_with("git ", CompsysState::Ready(&cands), &usage, DEFAULTS);
        assert_eq!(labels(&s), ["gamma", "alpha", "beta"]);
        // Another command's use does not count.
        let s = f.run_with("npm ", CompsysState::Ready(&cands), &usage, DEFAULTS);
        assert_eq!(labels(&s), ["beta", "alpha", "gamma"]);
        // Command position is the command `""`.
        let mut usage = Usage::default();
        usage.record("", "gamma", USED);
        let cands = [cand("gx"), cand("gamma")];
        let s = f.run_with("g", CompsysState::Ready(&cands), &usage, DEFAULTS);
        assert_eq!(labels(&s), ["gamma", "gx"]);
        // The tier still wins over recency.
        let cands = [cand("xaz"), cand("abc")];
        usage.record("git", "xaz", USED);
        let s = f.run_with("git a", CompsysState::Ready(&cands), &usage, DEFAULTS);
        assert_eq!(labels(&s), ["abc", "xaz"]);
    }

    #[test]
    fn the_usage_command_is_the_first_word() {
        let f = Fixture::new();
        let command = |t: &str| {
            f.compute(t, CompsysState::Pending, None, &Usage::default(), DEFAULTS)
                .usage_command
        };
        assert_eq!(command("git ch"), "git");
        assert_eq!(command("git commit -m x"), "git");
        assert_eq!(command("gi"), "");
        assert_eq!(command(""), "");
    }

    #[test]
    fn source_order_breaks_ties() {
        let compsys = item("same", Source::Compsys, Tier::CaseSensitivePrefix);
        let file = item("same", Source::File, Tier::CaseSensitivePrefix);
        let keys = vec![file, compsys];
        let ranked = rank(&keys, Sort::Recent, MAX_SUGGESTIONS);
        let sources: Vec<Source> = ranked.iter().map(|&at| keys[at].source).collect();
        assert_eq!(sources, [Source::Compsys, Source::File]);
        // Source comes before length.
        let long_compsys = item("longer-name", Source::Compsys, Tier::CaseSensitivePrefix);
        let short_file = item("s", Source::File, Tier::CaseSensitivePrefix);
        assert_eq!(
            ranked_labels(vec![short_file, long_compsys], Sort::Recent),
            ["longer-name", "s"]
        );
    }

    #[test]
    fn shorter_beats_longer_and_an_empty_needle_keeps_input_order() {
        let f = Fixture::new();
        let s = f.run("c", CompsysState::Ready(&[cand("chdir"), cand("cd")]));
        assert_eq!(labels(&s), ["cd", "chdir"]);
        let cands = [cand("zz"), cand("a"), cand("mmm")];
        let s = f.run("git ", CompsysState::Ready(&cands));
        assert_eq!(labels(&s), ["zz", "a", "mmm"]);
    }

    #[test]
    fn without_fuzzy_only_prefix_matches_stay() {
        let f = Fixture::new();
        let off = RankSettings {
            fuzzy: false,
            ..DEFAULTS
        };
        let none = Usage::default();
        let cands = [cand("acbh"), cand("Chx"), cand("chy"), cand("CH")];
        let s = f.run_with("git ch", CompsysState::Ready(&cands), &none, off);
        assert_eq!(labels(&s), ["CH", "chy", "Chx"]);
        let s = f.run_with("git ch", CompsysState::Ready(&cands), &none, DEFAULTS);
        assert_eq!(labels(&s), ["CH", "chy", "Chx", "acbh"]);
        // Files too.
        let s = f.run_with("ls a.", CompsysState::Pending, &none, off);
        assert_eq!(labels(&s), ["a.txt"]);
        let s = f.run_with("ls a.xt", CompsysState::Pending, &none, off);
        assert!(s.is_empty());
        let s = f.run_with("ls a.xt", CompsysState::Pending, &none, DEFAULTS);
        assert_eq!(labels(&s), ["a.txt"]);
    }

    #[test]
    fn alphabetical_gives_no_boost_and_orders_alphabetically() {
        let f = Fixture::new();
        let alphabetical = RankSettings {
            sort: Sort::Alphabetical,
            ..DEFAULTS
        };
        let cands = [cand("zeta"), cand("Beta"), cand("alpha"), cand("omega")];
        let mut usage = Usage::default();
        usage.record("git", "omega", USED);
        let s = f.run_with("git ", CompsysState::Ready(&cands), &usage, alphabetical);
        assert_eq!(labels(&s), ["alpha", "Beta", "omega", "zeta"]);
        let s = f.run_with("git ", CompsysState::Ready(&cands), &usage, DEFAULTS);
        assert_eq!(labels(&s), ["omega", "zeta", "Beta", "alpha"]);
        // Alphabetical replaces shorter-first, not the tiers.
        let cands = [cand("cbb"), cand("ca"), cand("Cz"), cand("xc")];
        let s = f.run_with("git c", CompsysState::Ready(&cands), &usage, alphabetical);
        assert_eq!(labels(&s), ["ca", "cbb", "Cz", "xc"]);
    }

    #[test]
    fn history_stays_last_whatever_the_sort() {
        let mut f = Fixture::new();
        f.history.add("git ab-old");
        f.history.add("git ab-new");
        let cands = [cand("zz"), cand("ab")];
        let mut usage = Usage::default();
        usage.record("git", "zz", USED);
        for sort in [Sort::Recent, Sort::Alphabetical] {
            let settings = RankSettings { sort, ..DEFAULTS };
            let s = f.run_with("git ab", CompsysState::Ready(&cands), &usage, settings);
            assert_eq!(labels(&s), ["ab", "git ab-new", "git ab-old"], "{sort:?}");
            assert_eq!(s[1].id.source, Source::History);
        }
        // History gets no boost from the usage store.
        usage.record("git", "git ab-old", USED);
        let s = f.run_with("git ab", CompsysState::Ready(&cands), &usage, DEFAULTS);
        assert_eq!(labels(&s), ["ab", "git ab-new", "git ab-old"]);
    }

    /// The specs a test has, loaded from the archive (or given inline), and
    /// the answers to their functions.
    struct Specs {
        states: std::collections::HashMap<String, crate::specs::cache::SpecState>,
        needs: crate::specs::needs::Needs,
    }

    impl Specs {
        fn new(names: &[&str]) -> Self {
            let mut specs = Self {
                states: std::collections::HashMap::new(),
                needs: crate::specs::needs::Needs::default(),
            };
            for name in names {
                specs.add(archived(name));
            }
            specs
        }

        fn add(&mut self, spec: std::sync::Arc<crate::specs::runtime::LoadedSpec>) {
            self.states.insert(
                spec.name.clone(),
                crate::specs::cache::SpecState::Ready(spec),
            );
        }

        fn view(&self) -> crate::specs::needs::View<'_> {
            crate::specs::needs::View::new(&self.states, &self.needs)
        }
    }

    /// A spec from the archive, as the spec thread hands it over.
    fn archived(name: &str) -> std::sync::Arc<crate::specs::runtime::LoadedSpec> {
        use crate::specs::model::Entry;
        match crate::specs::archive::Archive::embedded().entry(name) {
            Some(Ok(Entry::Spec { root, fns })) => {
                std::sync::Arc::new(crate::specs::runtime::LoadedSpec {
                    name: name.to_owned(),
                    has_js: !fns.is_empty(),
                    root,
                    fns,
                    version: None,
                    load_time: std::time::Duration::ZERO,
                    generation: 0,
                })
            }
            other => panic!("{name}: {other:?}"),
        }
    }

    fn inline(name: &str, json: &str) -> std::sync::Arc<crate::specs::runtime::LoadedSpec> {
        std::sync::Arc::new(crate::specs::runtime::LoadedSpec {
            name: name.to_owned(),
            root: serde_json::from_str(json).unwrap_or_else(|err| panic!("{err}")),
            fns: Vec::new(),
            has_js: false,
            version: None,
            load_time: std::time::Duration::ZERO,
            generation: 0,
        })
    }

    impl Fixture {
        /// `text` parsed against `specs`, with the default settings.
        fn spec_run(&self, text: &str, compsys: CompsysState, specs: &Specs) -> Computed {
            self.spec_run_with(text, compsys, specs, &Usage::default(), DEFAULTS)
        }

        fn spec_run_with(
            &self,
            text: &str,
            compsys: CompsysState,
            specs: &Specs,
            usage: &Usage,
            settings: RankSettings,
        ) -> Computed {
            let generators = Generators::default();
            self.spec_run_full(text, compsys, specs, &generators, usage, settings)
        }

        /// `text` parsed against `specs`, with `generators`' items (and
        /// templates listed from the fixture's cwd).
        fn spec_run_full(
            &self,
            text: &str,
            compsys: CompsysState,
            specs: &Specs,
            generators: &Generators,
            usage: &Usage,
            settings: RankSettings,
        ) -> Computed {
            let view = specs.view();
            let (cwd, home) = (self.cwd(), self.home());
            let generated = generators.view(&cwd, &home, &self.history);
            self.compute_generated(
                text,
                compsys,
                Some(&view),
                Some(&generated),
                usage,
                settings,
            )
        }
    }

    fn sources(s: &[Suggestion]) -> Vec<Source> {
        s.iter().map(|s| s.id.source).collect()
    }

    /// The description the archive's git spec gives subcommand `name`.
    fn git_description(name: &str) -> String {
        archived("git")
            .root
            .subcommands
            .iter()
            .find(|sub| sub.name.iter().any(|n| n == name))
            .and_then(|sub| sub.description.as_deref().map(str::to_owned))
            .unwrap_or_else(|| panic!("no git {name}"))
    }

    #[test]
    fn spec_subcommands_come_with_their_hint_and_description() {
        let f = Fixture::new();
        let specs = Specs::new(&["git"]);
        let mut usage = Usage::default();
        usage.record("git", "checkout", USED);
        let computed = f.spec_run_with("git ch", CompsysState::Pending, &specs, &usage, DEFAULTS);
        assert_eq!(computed.need, None);
        assert_eq!(computed.usage_command, "git");
        let s = &computed.suggestions;
        assert_eq!(s[0].label, "checkout");
        assert!(
            s.iter().all(|s| s.id.source == Source::Spec),
            "{:?}",
            sources(s)
        );
        // The prefix matches are subcommands (options match fuzzily below).
        let prefixed: Vec<&Suggestion> = s.iter().filter(|s| s.label.starts_with("ch")).collect();
        assert_eq!(prefixed.len(), 2, "{:?}", labels(s));
        assert!(prefixed.iter().all(|s| s.kind == Kind::Subcommand));
        assert!(!s[0].args_hint.is_empty());
        assert_eq!(s[0].description, git_description("checkout"));
        assert_eq!(s[0].highlights, vec![0, 1]);
        assert_eq!(s[0].usage_name(), "checkout");
        // As in Fig, a subcommand whose first arg is optional, and that
        // has no subcommands, is not followed by a space.
        assert_eq!(
            Engine::accept(&s[0], &report("git ch")),
            ("git checkout".to_string(), String::new())
        );
        // Another record puts its item first.
        let mut usage = Usage::default();
        usage.record("git", "cherry-pick", USED);
        let s = f
            .spec_run_with("git ch", CompsysState::Pending, &specs, &usage, DEFAULTS)
            .suggestions;
        assert_eq!(labels(&s)[..2], ["cherry-pick", "checkout"]);
    }

    #[test]
    fn sudo_hands_off_to_the_command_spec() {
        let f = Fixture::new();
        let specs = Specs::new(&["git", "sudo"]);
        let direct = f.spec_run("git ch", CompsysState::Pending, &specs);
        let sudo = f.spec_run("sudo git ch", CompsysState::Pending, &specs);
        assert_eq!(sudo.need, None);
        assert_eq!(sudo.usage_command, "git");
        assert_eq!(labels(&sudo.suggestions), labels(&direct.suggestions));
        // Without the hand-off spec, phase 1's list, and the need for it.
        let only_sudo = Specs::new(&["sudo"]);
        let waiting = f.spec_run("sudo git ch", CompsysState::Pending, &only_sudo);
        assert_eq!(
            waiting.need,
            Some(Need::Spec {
                name: "git".to_owned()
            })
        );
        assert!(waiting.suggestions.is_empty());
    }

    #[test]
    fn spec_options_and_joined_names() {
        let f = Fixture::new();
        let specs = Specs::new(&["ls", "git"]);
        let s = f
            .spec_run("ls -", CompsysState::Pending, &specs)
            .suggestions;
        assert!(s.len() > 20, "{:?}", labels(&s));
        assert!(
            s.iter()
                .all(|s| s.kind == Kind::Option && s.id.source == Source::Spec)
        );
        assert!(labels(&s).contains(&"-l"));
        let l = pick(&s, "-l");
        // An option without args gets no space (Fig's `shouldAddSpace`).
        assert_eq!(Engine::accept(l, &report("ls -")).0, "ls -l");
        // A description's line breaks are spaces, as in Fig's HTML.
        let thousands = &pick(&s, "-,").description;
        assert!(
            thousands.contains("returned by localeconv(3)"),
            "{thousands}"
        );
        // An item with several names shows them joined.
        let s = f
            .spec_run("git commit -", CompsysState::Pending, &specs)
            .suggestions;
        let message = pick(&s, "-m, --message");
        assert_eq!(message.kind, Kind::Option);
        // Matched by its best name, highlighted where that name is.
        let s = f
            .spec_run("git commit --mess", CompsysState::Pending, &specs)
            .suggestions;
        let message = pick(&s, "-m, --message");
        assert_eq!(message.highlights, vec![4, 5, 6, 7, 8, 9]);
        // The name that matches is inserted.
        assert_eq!(
            Engine::accept(message, &report("git commit --mess")).0,
            "git commit --message "
        );
        let s = f
            .spec_run("git commit -m", CompsysState::Pending, &specs)
            .suggestions;
        assert_eq!(
            Engine::accept(pick(&s, "-m, --message"), &report("git commit -m")).0,
            "git commit -m "
        );
    }

    #[test]
    fn compsys_fills_in_what_the_spec_lacks() {
        let f = Fixture::new();
        let specs = Specs::new(&["git"]);
        let mut extra = cand("chextra");
        extra.description = "from zsh".into();
        let cands = [cand("checkout"), extra];
        let off = RankSettings {
            fuzzy: false,
            ..DEFAULTS
        };
        let s = f
            .spec_run_with(
                "git ch",
                CompsysState::Ready(&cands),
                &specs,
                &Usage::default(),
                off,
            )
            .suggestions;
        // `checkout` is the spec's; zsh's own `chextra` comes after every
        // spec item.
        assert_eq!(pick(&s, "checkout").id.source, Source::Spec);
        assert_eq!(labels(&s).iter().filter(|l| **l == "checkout").count(), 1);
        let at = s
            .iter()
            .position(|s| s.label == "chextra")
            .expect("chextra");
        assert_eq!(s[at].id.source, Source::Compsys);
        // At git's subcommand position zsh's extra name is a subcommand.
        assert_eq!(s[at].kind, Kind::Subcommand);
        assert!(s[..at].iter().all(|s| s.id.source == Source::Spec));
        assert_eq!(at, s.len() - 1);
    }

    #[test]
    fn compsys_names_are_subcommands_only_at_a_subcommand_position() {
        let f = Fixture::new();
        let specs = Specs::new(&["git"]);
        let off = RankSettings {
            fuzzy: false,
            ..DEFAULTS
        };
        let kind_of = |text: &str, word: &str| {
            let cands = [cand(word)];
            let s = f
                .spec_run_with(
                    text,
                    CompsysState::Ready(&cands),
                    &specs,
                    &Usage::default(),
                    off,
                )
                .suggestions;
            let found = s
                .iter()
                .find(|s| s.label == word && s.id.source == Source::Compsys)
                .unwrap_or_else(|| panic!("no compsys {word:?} at {text:?}: {:?}", labels(&s)));
            found.kind
        };
        // `git ch`: no arg is being typed, git offers subcommands there.
        assert_eq!(kind_of("git ch", "cherry"), Kind::Subcommand);
        assert_eq!(kind_of("git check-a", "check-attr"), Kind::Subcommand);
        // `git checkout fe`: the word is checkout's arg.
        assert_eq!(kind_of("git checkout fe", "feature-x"), Kind::Arg);
        // `git --ex`: an option, not a subcommand.
        assert_eq!(kind_of("git --ex", "--extra"), Kind::Arg);
    }

    #[test]
    fn files_only_where_the_template_asks() {
        let f = Fixture::new();
        let mut specs = Specs::new(&["ls", "git", "cd"]);
        // `ls` takes file paths: its template's files are spec items.
        let s = f
            .spec_run("ls a", CompsysState::Pending, &specs)
            .suggestions;
        assert_eq!(s[0].label, "a.txt");
        assert_eq!(s[0].id.source, Source::Spec);
        assert_eq!(s[0].kind, Kind::File);
        let s = f
            .spec_run("ls s", CompsysState::Pending, &specs)
            .suggestions;
        assert_eq!(s[0].label, "src/");
        assert_eq!(s[0].kind, Kind::Folder);
        // `git ` and `cd ` have no template: no files once compsys has
        // answered.
        for line in ["git s", "cd s"] {
            let s = f
                .spec_run(line, CompsysState::Ready(&[]), &specs)
                .suggestions;
            assert!(
                !sources(&s).contains(&Source::File),
                "{line}: {:?}",
                labels(&s)
            );
        }
        // Until it answers, the file fast path stands in for it where the
        // spec has nothing for the arg (`cd` takes directories), not where
        // it has items (`git s`) or there is no arg (`ls -`).
        let s = f
            .spec_run("cd s", CompsysState::Pending, &specs)
            .suggestions;
        // (`cd`'s arg is matched fuzzily.)
        assert_eq!(labels(&s), ["src/", "my docs/", "My Documents/"]);
        assert!(s.iter().all(|s| s.kind == Kind::Folder));
        let s = f.spec_run("cd ", CompsysState::Pending, &specs).suggestions;
        assert_eq!(labels(&s), ["My Documents/", "my docs/", "src/"]);
        let s = f
            .spec_run("git s", CompsysState::Pending, &specs)
            .suggestions;
        assert!(!sources(&s).contains(&Source::File), "{:?}", labels(&s));
        let s = f
            .spec_run("ls -", CompsysState::Unavailable, &specs)
            .suggestions;
        assert!(!sources(&s).contains(&Source::File), "{:?}", labels(&s));
        // Folders only.
        specs.add(inline(
            "dirs",
            r#"{"name":"dirs","args":{"template":"folders"}}"#,
        ));
        let s = f
            .spec_run("dirs ", CompsysState::Pending, &specs)
            .suggestions;
        assert_eq!(labels(&s), ["my docs/", "My Documents/", "src/", "../"]);
    }

    #[test]
    fn cursor_splits_the_insertion() {
        let f = Fixture::new();
        let specs = Specs::new(&["git"]);
        let s = f
            .spec_run("git config ali", CompsysState::Pending, &specs)
            .suggestions;
        let alias = pick(&s, "alias.*");
        assert_eq!(
            Engine::accept(alias, &report("git config ali")),
            ("git config alias.".to_string(), String::new())
        );
        // A space follows an option with a mandatory arg, after the text,
        // with the cursor before it when `{cursor}` (or an option's
        // separator) placed it (`state/insertion.ts:39-44,215-221`).
        let mut specs = Specs::new(&[]);
        specs.add(inline(
            "tool",
            r#"{"name":"tool","options":[
                {"name":"--message","insertValue":"--message '{cursor}'","args":{"name":"msg"}},
                {"name":"--color","requiresSeparator":true,"args":{"name":"when"}},
                {"name":"--back","insertValue":"\b\b-x"}
            ]}"#,
        ));
        let s = f
            .spec_run("tool --me", CompsysState::Pending, &specs)
            .suggestions;
        assert_eq!(
            Engine::accept(pick(&s, "--message"), &report("tool --me")),
            ("tool --message '".to_string(), "' ".to_string())
        );
        let s = f
            .spec_run("tool --co", CompsysState::Pending, &specs)
            .suggestions;
        assert_eq!(
            Engine::accept(pick(&s, "--color"), &report("tool --co")),
            ("tool --color=".to_string(), " ".to_string())
        );
        // A backspace takes back the character before it, as when Fig
        // types it in after deleting the query: here the space and the `l`.
        let s = f
            .spec_run("tool --ba", CompsysState::Pending, &specs)
            .suggestions;
        assert_eq!(
            Engine::accept(pick(&s, "--back"), &report("tool --ba")).0,
            "too-x"
        );
    }

    #[test]
    fn a_hidden_item_shows_only_on_an_exact_match() {
        let f = Fixture::new();
        let specs = Specs::new(&["git"]);
        let s = f
            .spec_run("git config add.", CompsysState::Pending, &specs)
            .suggestions;
        assert!(labels(&s).contains(&"add.interactive.useBuiltin"));
        assert!(!labels(&s).contains(&"add.ignore-errors"));
        let s = f
            .spec_run(
                "git config add.ignore-errors",
                CompsysState::Pending,
                &specs,
            )
            .suggestions;
        assert_eq!(s[0].label, "add.ignore-errors");
        // Case aside.
        let s = f
            .spec_run(
                "git config ADD.IGNORE-ERRORS",
                CompsysState::Pending,
                &specs,
            )
            .suggestions;
        assert!(labels(&s).contains(&"add.ignore-errors"));
    }

    #[test]
    fn disabled_commands_get_no_popup() {
        let f = Fixture::new();
        let specs = Specs::new(&["git", "sudo", "ls"]);
        let disabled: &'static [String] = vec!["git".to_owned(), "faccmd".to_owned()].leak();
        let settings = RankSettings {
            disabled,
            ..DEFAULTS
        };
        let run = |text: &str| {
            f.spec_run_with(
                text,
                CompsysState::Pending,
                &specs,
                &Usage::default(),
                settings,
            )
            .suggestions
        };
        assert!(run("git ch").is_empty());
        // The hand-off command too.
        assert!(run("sudo git ch").is_empty());
        // A command without a spec.
        assert!(
            f.compute(
                "faccmd a",
                CompsysState::Pending,
                None,
                &Usage::default(),
                settings
            )
            .suggestions
            .is_empty()
        );
        assert!(!run("ls a").is_empty());
    }

    #[test]
    fn a_spec_still_loading_gives_phase_1s_list() {
        let mut f = Fixture::new();
        f.history.add("git checkout main");
        let mut specs = Specs::new(&[]);
        specs
            .states
            .insert("git".to_owned(), crate::specs::cache::SpecState::Loading);
        let cands = [cand("checkout"), cand("cherry-pick")];
        let phase_1 = f.run("git ch", CompsysState::Ready(&cands));
        let loading = f.spec_run("git ch", CompsysState::Ready(&cands), &specs);
        assert_eq!(
            loading.need,
            Some(Need::Spec {
                name: "git".to_owned()
            })
        );
        assert_eq!(labels(&loading.suggestions), labels(&phase_1));
        assert_eq!(sources(&loading.suggestions), sources(&phase_1));
        assert_eq!(
            sources(&phase_1),
            [Source::Compsys, Source::Compsys, Source::History]
        );
        // Files too, while compsys is pending.
        let phase_1 = f.run("git a", CompsysState::Pending);
        let loading = f.spec_run("git a", CompsysState::Pending, &specs);
        assert_eq!(labels(&loading.suggestions), labels(&phase_1));
        assert_eq!(labels(&phase_1), ["a.txt"]);
    }

    #[test]
    fn a_pending_generate_spec_gives_the_static_items_then_its_own() {
        let f = Fixture::new();
        let mut specs = Specs::new(&["git"]);
        let s = f
            .spec_run("git ext", CompsysState::Pending, &specs)
            .suggestions;
        assert!(!labels(&s).contains(&"extcmd"));
        let view = specs.view();
        let computed = f.compute(
            "git ext",
            CompsysState::Pending,
            Some(&view),
            &Usage::default(),
            DEFAULTS,
        );
        assert_eq!(computed.need, None, "provisional: the parse went on");
        let (_, missed, looked_up) = view.into_parts();
        assert_eq!(looked_up, ["git"]);
        let fn_id = archived("git").root.generate_spec.expect("generateSpec").0;
        let need = Need::GenerateSpec {
            spec: "git".to_owned(),
            fn_id,
            tokens: vec!["git".to_owned(), "ext".to_owned()],
        };
        assert_eq!(missed, std::slice::from_ref(&need));
        // `git help -a` answers.
        specs.needs.answer_with(
            &need,
            crate::specs::parser::Answer::Spec(
                serde_json::from_str(
                    r#"{"name":"git","subcommands":[{"name":"extcmd","description":"Run git-extcmd"}]}"#,
                )
                .expect("a spec"),
            ),
        );
        let s = f
            .spec_run("git ext", CompsysState::Pending, &specs)
            .suggestions;
        assert_eq!(s[0].label, "extcmd");
        assert_eq!(s[0].description, "Run git-extcmd");
        assert!(labels(&s).len() > 1, "the static items stay");
    }

    #[test]
    fn filter_strategy_overrides_the_setting() {
        let f = Fixture::new();
        let mut specs = Specs::new(&["ls"]);
        let off = RankSettings {
            fuzzy: false,
            ..DEFAULTS
        };
        let none = Usage::default();
        // `ls`'s arg is `fuzzy`: fuzzy file matches with the setting off.
        let s = f.spec_run_with("ls atx", CompsysState::Pending, &specs, &none, off);
        assert_eq!(labels(&s.suggestions), ["a.txt"]);
        // `prefix` turns it off with the setting on.
        specs.add(inline(
            "pick",
            r#"{"name":"pick","args":{"filterStrategy":"prefix","suggestions":["alpha","xalpha"]}}"#,
        ));
        let s = f
            .spec_run("pick lph", CompsysState::Pending, &specs)
            .suggestions;
        assert!(s.is_empty(), "{:?}", labels(&s));
        let s = f
            .spec_run("pick al", CompsysState::Pending, &specs)
            .suggestions;
        assert_eq!(labels(&s), ["alpha"]);
        specs.add(inline(
            "fuzz",
            r#"{"name":"fuzz","args":{"suggestions":["alpha","xalpha"]}}"#,
        ));
        let s = f
            .spec_run("fuzz lph", CompsysState::Pending, &specs)
            .suggestions;
        assert_eq!(labels(&s), ["alpha", "xalpha"]);
        let s = f.spec_run_with("fuzz lph", CompsysState::Pending, &specs, &none, off);
        assert!(s.suggestions.is_empty());
    }

    #[test]
    fn an_option_arg_is_queried_and_replaced_after_its_equals() {
        let f = Fixture::new();
        let specs = Specs::new(&["ls"]);
        let s = f
            .spec_run("ls --color=", CompsysState::Pending, &specs)
            .suggestions;
        assert_eq!(
            labels(&s),
            ["always, yes, force", "auto", "never, no, none"]
        );
        assert!(s.iter().all(|s| s.kind == Kind::Arg));
        let s = f
            .spec_run("ls --color=n", CompsysState::Pending, &specs)
            .suggestions;
        assert_eq!(labels(&s)[0], "never, no, none");
        assert_eq!(s[0].highlights, vec![0]);
        assert_eq!(s[0].id.text, "--color=never");
        // Only the part after `=` is replaced; an arg value gets no space.
        assert_eq!(
            Engine::accept(&s[0], &report("ls --color=n")),
            ("ls --color=never".to_string(), String::new())
        );
        // The name that matched best is inserted.
        let s = f
            .spec_run("ls --color=y", CompsysState::Pending, &specs)
            .suggestions;
        assert_eq!(
            Engine::accept(&s[0], &report("ls --color=y")).0,
            "ls --color=yes"
        );
        // A compsys answer for the same value is the spec's row.
        let mut always = cand("always");
        always.iprefix = "--color=".into();
        let s = f
            .spec_run("ls --color=al", CompsysState::Ready(&[always]), &specs)
            .suggestions;
        assert_eq!(labels(&s), ["always, yes, force"]);
    }

    // Spec items with generators, and Fig's insertion rules (step 8).
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use crate::complete::selection::{Key, Selection};
    use crate::specs::cache::GenAnswer;
    use crate::specs::generated::RunEnv;
    use crate::specs::model::Suggestion as SpecSuggestion;
    use crate::specs::runtime::Job;

    const TOOL: &str = r#"{"name":"tool",
        "subcommands":[{"name":"sub"}],
        "args":{"name":"thing","suggestions":["static"],
                "generators":{"script":["list"],"splitOn":"\n"}},
        "additionalSuggestions":["extra"],
        "options":[{"name":"--opt"}]}"#;

    /// `text` against the spec, with `generators`.
    fn run(f: &Fixture, specs: &Specs, generators: &Generators, text: &str) -> Computed {
        f.spec_run_full(
            text,
            CompsysState::Pending,
            specs,
            generators,
            &Usage::default(),
            DEFAULTS,
        )
    }

    /// Observe `computed`'s arg and answer its first run with `names`.
    fn answer(
        f: &Fixture,
        generators: &mut Generators,
        computed: &Computed,
        names: &[&str],
        query: Option<&str>,
    ) {
        let suggestions = names
            .iter()
            .map(|name| SpecSuggestion {
                name: vec![(*name).to_owned()],
                kind: Some(SuggestionType::Arg),
                ..SpecSuggestion::default()
            })
            .collect();
        answer_with(f, generators, computed, suggestions, query);
    }

    /// [`answer`] with these suggestions.
    fn answer_with(
        f: &Fixture,
        generators: &mut Generators,
        computed: &Computed,
        suggestions: Vec<SpecSuggestion>,
        query: Option<&str>,
    ) {
        let env = Arc::new(Vec::new());
        let history = History::empty();
        let (cwd, home) = (f.cwd(), f.home());
        let run = RunEnv {
            cwd: &cwd,
            home: &home,
            env: &env,
            history: &history,
        };
        let now = Instant::now();
        generators.observe(&computed.spec_arg, &run, now);
        let jobs = generators.take_jobs(now);
        let id = jobs
            .iter()
            .find_map(|job| match job {
                Job::Generate { id, .. } => Some(*id),
                _ => None,
            })
            .expect("a run");
        let mut answers = vec![GenAnswer::Generated {
            id,
            result: Ok(suggestions),
            stale: false,
        }];
        if let (Some(query), Some(Job::Terms { id, .. })) = (
            query,
            jobs.iter().find(|job| matches!(job, Job::Terms { .. })),
        ) {
            answers.push(GenAnswer::Terms {
                id: *id,
                trigger: None,
                query_term: Some(query.to_owned()),
            });
        }
        generators.on_answers(answers, &run, now);
    }

    #[test]
    fn generator_items_go_where_fig_puts_them() {
        let f = Fixture::new();
        let mut specs = Specs::new(&[]);
        specs.add(inline("tool", TOOL));
        let mut generators = Generators::default();
        let before = run(&f, &specs, &generators, "tool ");
        assert_eq!(
            labels(&before.suggestions),
            ["sub", "static", "extra", "--opt"]
        );
        answer(&f, &mut generators, &before, &["gen-a", "gen-b"], None);
        let after = run(&f, &specs, &generators, "tool ");
        assert_eq!(
            labels(&after.suggestions),
            ["sub", "static", "gen-a", "gen-b", "extra", "--opt"]
        );
        let found = pick(&after.suggestions, "gen-a");
        assert_eq!(found.id.source, Source::Spec);
        assert_eq!(found.kind, Kind::Arg);
        // An arg value gets no space.
        assert_eq!(
            Engine::accept(found, &report("tool ")),
            ("tool gen-a".to_owned(), String::new())
        );
    }

    /// Every item offered while an arg is being typed carries the arg's
    /// name and description (whatever its kind, as Fig uses `currentArg`),
    /// for the popup's bottom line; with no arg being typed, none does.
    #[test]
    fn items_carry_the_current_arg() {
        let f = Fixture::new();
        let mut specs = Specs::new(&[]);
        specs.add(inline(
            "tooldesc",
            r#"{"name":"tooldesc",
                "subcommands":[{"name":"sub"}],
                "args":{"name":"thing","description":"The  thing to use",
                        "suggestions":["static"],
                        "generators":{"script":["list"],"splitOn":"\n"}},
                "options":[{"name":"--opt"}]}"#,
        ));
        let mut generators = Generators::default();
        let before = run(&f, &specs, &generators, "tooldesc ");
        answer(&f, &mut generators, &before, &["gen-a"], None);
        let after = run(&f, &specs, &generators, "tooldesc ");
        assert_eq!(
            labels(&after.suggestions),
            ["sub", "static", "gen-a", "--opt"]
        );
        for s in &after.suggestions {
            assert_eq!(s.arg_name.as_deref(), Some("thing"), "{}", s.label);
            assert_eq!(
                s.arg_description.as_deref(),
                Some("The thing to use"),
                "{}",
                s.label
            );
        }
        // A name without a description.
        let mut specs = Specs::new(&[]);
        specs.add(inline("tool", TOOL));
        let s = run(&f, &specs, &generators, "tool ");
        assert!(
            s.suggestions
                .iter()
                .all(|s| s.arg_name.as_deref() == Some("thing") && s.arg_description.is_none())
        );
        // No arg being typed: after a subcommand without args, and without
        // a spec at all.
        let s = run(&f, &specs, &generators, "tool sub ");
        assert!(s.suggestions.iter().all(|s| s.arg_name.is_none()));
        let cands = [cand("alpha")];
        let s = f.run("faccmd ", CompsysState::Ready(&cands));
        assert_eq!(labels(&s), ["alpha"]);
        assert!(
            s.iter()
                .all(|s| s.arg_name.is_none() && s.arg_description.is_none())
        );
    }

    #[test]
    fn a_merge_keeps_the_selection_by_identity() {
        let f = Fixture::new();
        let mut specs = Specs::new(&[]);
        specs.add(inline("tool", TOOL));
        let mut generators = Generators::default();
        let before = run(&f, &specs, &generators, "tool ");
        let mut selection = Selection::new();
        selection.update(&before.suggestions);
        // Down, Down, Down: `extra`.
        for _ in 0..3 {
            selection.on_key(Key::Down);
        }
        assert_eq!(
            before.suggestions[selection.selected().expect("selected")].label,
            "extra"
        );
        answer(&f, &mut generators, &before, &["gen-a", "gen-b"], None);
        let after = run(&f, &specs, &generators, "tool ");
        selection.update(&after.suggestions);
        let at = selection.selected().expect("still selected");
        assert_eq!(after.suggestions[at].label, "extra");
        assert_eq!(at, 4);
    }

    #[test]
    fn a_query_term_filters_and_is_all_an_accept_replaces() {
        let f = Fixture::new();
        let mut specs = Specs::new(&[]);
        specs.add(inline(
            "tool",
            r#"{"name":"tool","args":{"name":"path",
                "generators":{"script":["list"],"splitOn":"\n","getQueryTerm":"/"}}}"#,
        ));
        let mut generators = Generators::default();
        let first = run(&f, &specs, &generators, "tool src/ma");
        answer(
            &f,
            &mut generators,
            &first,
            &["main.rs", "mod.rs", "lib.rs"],
            None,
        );
        let s = run(&f, &specs, &generators, "tool src/ma").suggestions;
        assert_eq!(labels(&s)[0], "main.rs");
        assert!(!labels(&s).contains(&"lib.rs"));
        assert_eq!(s[0].highlights, vec![0, 1]);
        assert_eq!(s[0].id.text, "src/main.rs");
        assert_eq!(
            Engine::accept(&s[0], &report("tool src/ma")),
            ("tool src/main.rs".to_owned(), String::new())
        );
    }

    #[test]
    fn files_and_folders_come_before_options_on_an_empty_word() {
        let f = Fixture::new();
        let specs = Specs::new(&["ls"]);
        let generators = Generators::default();
        let s = run(&f, &specs, &generators, "ls ").suggestions;
        let first_option = s
            .iter()
            .position(|s| s.kind == Kind::Option)
            .expect("options");
        assert_eq!(
            labels(&s)[..first_option],
            // As V8's `localeCompare` sorts them, then `../`.
            [
                "a.txt",
                "my docs/",
                "My Documents/",
                "my file.txt",
                "src/",
                "../"
            ]
        );
        assert!(
            s[..first_option]
                .iter()
                .all(|s| s.id.source == Source::Spec)
        );
        // A file is not followed by a space inside a spec, nor a folder.
        assert_eq!(
            Engine::accept(pick(&s, "a.txt"), &report("ls ")).0,
            "ls a.txt"
        );
        assert_eq!(
            Engine::accept(pick(&s, "src/"), &report("ls ")).0,
            "ls src/"
        );
        // In a folder, only the part after its `/` is matched and replaced.
        let s = run(&f, &specs, &generators, "ls ../cwd/sr").suggestions;
        assert_eq!(s[0].label, "src/");
        assert_eq!(
            Engine::accept(&s[0], &report("ls ../cwd/sr")).0,
            "ls ../cwd/src/"
        );
    }

    /// What accepting `label` at `text` gives, with the spec `json`.
    fn spec_accepted(json: &str, text: &str, label: &str) -> (String, String) {
        let f = Fixture::new();
        let mut specs = Specs::new(&[]);
        specs.add(inline("tool", json));
        let s = run(&f, &specs, &Generators::default(), text).suggestions;
        Engine::accept(pick(&s, label), &report(text))
    }

    #[test]
    fn a_space_follows_only_where_fig_adds_one() {
        let spec = r#"{"name":"tool",
            "subcommands":[
                {"name":"mandatory","args":{"name":"x"}},
                {"name":"optional","args":{"name":"x","isOptional":true}},
                {"name":"required","requiresSubcommand":true,"args":{"name":"x","isOptional":true}},
                {"name":"parent","subcommands":[{"name":"child"}]},
                {"name":"leaf"},
                {"name":"cursor","insertValue":"cursor '{cursor}'","args":{"name":"x"}},
                {"name":"runs","insertValue":"runs\n\n"}
            ],
            "options":[{"name":"--value","args":{"name":"v"}},{"name":"--flag"}],
            "additionalSuggestions":["more"]}"#;
        let line = |text: &str| (text.to_owned(), String::new());
        // The first arg is mandatory: a space.
        assert_eq!(
            spec_accepted(spec, "tool man", "mandatory"),
            line("tool mandatory ")
        );
        // Optional, and no subcommands: none.
        assert_eq!(
            spec_accepted(spec, "tool opt", "optional"),
            line("tool optional")
        );
        // `requiresSubcommand: true` wins over an optional arg.
        assert_eq!(
            spec_accepted(spec, "tool req", "required"),
            line("tool required ")
        );
        // Subcommands of its own: a space.
        assert_eq!(
            spec_accepted(spec, "tool par", "parent"),
            line("tool parent ")
        );
        assert_eq!(spec_accepted(spec, "tool lea", "leaf"), line("tool leaf"));
        // Options: a mandatory arg, or none.
        assert_eq!(
            spec_accepted(spec, "tool --v", "--value"),
            line("tool --value ")
        );
        assert_eq!(
            spec_accepted(spec, "tool --f", "--flag"),
            line("tool --flag")
        );
        // `additionalSuggestions` are values.
        assert_eq!(spec_accepted(spec, "tool mo", "more"), line("tool more"));
        // With `{cursor}` the space goes after the text, the cursor before it.
        assert_eq!(
            spec_accepted(spec, "tool cur", "cursor"),
            ("tool cursor '".to_owned(), "' ".to_owned())
        );
    }

    #[test]
    fn a_trailing_newline_runs_the_line() {
        let f = Fixture::new();
        let mut specs = Specs::new(&[]);
        specs.add(inline(
            "tool",
            r#"{"name":"tool","subcommands":[
                {"name":"runs","insertValue":"runs\n\n","args":{"name":"x"}},
                {"name":"inner","insertValue":"a\nb"}]}"#,
        ));
        let s = run(&f, &specs, &Generators::default(), "tool ru").suggestions;
        let runs = pick(&s, "runs");
        assert!(runs.runs());
        // The newlines are not inserted, and no space is.
        assert_eq!(runs.id.text, "runs");
        assert_eq!(
            Engine::accept(runs, &report("tool ru")),
            ("tool runs".to_owned(), String::new())
        );
        // A newline that is not at the end stays in the text.
        let s = run(&f, &specs, &Generators::default(), "tool in").suggestions;
        let inner = pick(&s, "inner");
        assert!(!inner.runs());
        assert_eq!(Engine::accept(inner, &report("tool in")).0, "tool a\nb");
        // The one spec that has one: `nr -` runs the last command.
        let specs = Specs::new(&["nr"]);
        let s = run(&f, &specs, &Generators::default(), "nr -").suggestions;
        let last = pick(&s, "-");
        assert!(last.runs());
        assert_eq!(Engine::accept(last, &report("nr -")).0, "nr -");
    }

    /// A `displayName` is the label, is matched (never as an exact match),
    /// and is highlighted; the name is what goes in.
    #[test]
    fn a_display_name_is_shown_and_matched_and_its_name_inserted() {
        let spec = r#"{"name":"tool","subcommands":[
            {"name":"co","displayName":"checkout"},
            {"name":"checkout"},
            {"name":"zz","displayName":"Zap it"}]}"#;
        let f = Fixture::new();
        let mut specs = Specs::new(&[]);
        specs.add(inline("tool", spec));
        let s = |text: &str| run(&f, &specs, &Generators::default(), text).suggestions;
        // Matched on the display name, highlighted there, the name inserted.
        let got = s("tool che");
        assert_eq!(labels(&got), ["checkout", "checkout"]);
        let shown = got.iter().find(|s| s.id.text == "co").expect("co");
        assert_eq!(shown.highlights, [0, 1, 2]);
        assert_eq!(Engine::accept(shown, &report("tool che")).0, "tool co");
        // Typed in full, the display name is not an exact match: the item
        // whose name it is comes first.
        let got = s("tool checkout");
        assert_eq!(
            got.iter().map(|s| s.id.text.as_str()).collect::<Vec<_>>(),
            ["checkout", "co"]
        );
        // Matched on the name: the label is still the display name, with
        // the typed letters highlighted in it where they are (`c…o`).
        let got = s("tool co");
        assert_eq!(got[0].label, "checkout");
        assert_eq!(got[0].id.text, "co");
        assert_eq!(got[0].highlights, [0, 5]);
        // A name match that is not in the label highlights nothing.
        let got = s("tool zz");
        assert_eq!(labels(&got), ["Zap it"]);
        assert!(got[0].highlights.is_empty());
        assert_eq!(Engine::accept(&got[0], &report("tool zz")).0, "tool zz");
        let got = s("tool zap");
        assert_eq!(labels(&got), ["Zap it"]);
        assert_eq!(got[0].highlights, [0, 1, 2]);
    }

    /// With `history = false` there are no history lines, and the `history`
    /// template gives nothing.
    #[test]
    fn history_off_offers_no_history() {
        let mut f = Fixture::new();
        f.history.add("tool alpha");
        f.history.add("tool beta");
        let off = RankSettings {
            history: false,
            ..DEFAULTS
        };
        let none = Usage::default();
        // History lines.
        let lines = |settings| f.run_with("tool ", CompsysState::Ready(&[]), &none, settings);
        assert_eq!(labels(&lines(DEFAULTS)), ["tool beta", "tool alpha"]);
        assert!(lines(off).is_empty());
        // The template.
        let mut specs = Specs::new(&[]);
        specs.add(inline(
            "tool",
            r#"{"name":"tool","args":{"name":"x","template":"history"}}"#,
        ));
        let template = |settings| {
            f.spec_run_full(
                "tool ",
                CompsysState::Ready(&[]),
                &specs,
                &Generators::default(),
                &none,
                settings,
            )
            .suggestions
        };
        let on = template(DEFAULTS);
        assert_eq!(
            sources(&on),
            [Source::Spec, Source::Spec, Source::History, Source::History]
        );
        assert_eq!(labels(&on)[..2], ["beta", "alpha"]);
        assert!(template(off).is_empty());
    }

    /// What every suggestion accepted at `text` would put in the line, and
    /// whether it would run it.
    fn accepts(s: &[Suggestion], text: &str) -> Vec<(String, bool)> {
        s.iter()
            .map(|s| {
                let (lbuffer, rbuffer) = Engine::accept(s, &report(text));
                (format!("{lbuffer}{rbuffer}"), s.runs())
            })
            .collect()
    }

    #[test]
    fn a_file_name_with_a_newline_never_runs_the_line() {
        let f = Fixture::new();
        fs::write(f.cwd().join("important"), "").expect("write");
        fs::write(f.cwd().join("important\n"), "").expect("write");
        // A spec's template, the file fast path, and zsh's completions.
        let specs = Specs::new(&["rm"]);
        let spec = run(&f, &specs, &Generators::default(), "rm imp").suggestions;
        let plain = f.run("faccmd imp", CompsysState::Pending);
        let mut c = cand("important\n");
        c.is_file = true;
        let mut quoted = cand("important$'\\n'");
        quoted.verbatim = true;
        quoted.is_file = true;
        let zsh = f.run(
            "faccmd imp",
            CompsysState::Ready(&[c, cand("important"), quoted]),
        );
        for (s, text) in [
            (&spec, "rm imp"),
            (&plain, "faccmd imp"),
            (&zsh, "faccmd imp"),
        ] {
            assert_eq!(labels(s), ["important"], "{text}");
            for (line, runs) in accepts(s, text) {
                assert!(!runs, "{line:?}");
                assert!(!line.chars().any(char::is_control), "{line:?}");
            }
        }
    }

    #[test]
    fn a_name_zsh_prints_as_octal_escapes_is_the_same_file() {
        // zsh quotes a character the locale cannot print (here U+200D, bytes
        // e2 80 8d) as `$'\342\200\215'` (`addunprintable`, utils.c).
        let name = "hello\u{1f468}\u{200d}\u{1f469}.txt";
        let f = Fixture::new();
        fs::write(f.cwd().join(name), "").expect("write");
        let mut quoted = cand("hello\u{1f468}$'\\342\\200\\215'\u{1f469}.txt");
        quoted.verbatim = true;
        quoted.is_file = true;
        quoted.group = "globbed-files".into();
        let s = f.run(
            "faccmd hel",
            CompsysState::Ready(std::slice::from_ref(&quoted)),
        );
        assert_eq!(labels(&s), [name]);
        assert_eq!(s[0].id.text, name);
        for (line, runs) in accepts(&s, "faccmd hel") {
            assert!(!runs, "{line:?}");
            assert!(line.starts_with("faccmd hello"), "{line:?}");
        }
        // Without the file on disk only zsh's item is left, still by its
        // real name, inserted as zsh quoted it.
        let f = Fixture::new();
        let s = f.run(
            "faccmd hel",
            CompsysState::Ready(std::slice::from_ref(&quoted)),
        );
        assert_eq!(labels(&s), [name]);
        assert_eq!(
            accepts(&s, "faccmd hel")[0].0,
            format!("faccmd {} ", quoted.word)
        );
    }

    #[test]
    fn a_generated_name_with_a_newline_never_runs_the_line() {
        let f = Fixture::new();
        let mut specs = Specs::new(&[]);
        specs.add(inline(
            "tool",
            r#"{"name":"tool","args":{"name":"x",
                "generators":{"script":["list"],"splitOn":","}}}"#,
        ));
        let mut generators = Generators::default();
        let first = run(&f, &specs, &generators, "tool ");
        // A script's pieces are their own insert values: the ones with a
        // control character are left out as the output is split.
        let split = crate::specs::generators::split_on("b,a\n,a\u{1b}x,ok", ",", false);
        let names: Vec<&str> = split.iter().map(|s| s.name[0].as_str()).collect();
        assert_eq!(names, ["b", "ok"]);
        answer_with(&f, &mut generators, &first, split, None);
        let s = run(&f, &specs, &generators, "tool ").suggestions;
        assert_eq!(labels(&s), ["b", "ok"]);
        for (line, runs) in accepts(&s, "tool ") {
            assert!(!runs, "{line:?}");
        }
        // A name with one and no insert value (a `postProcess` object) is
        // left out by the engine.
        let mut generators = Generators::default();
        answer(&f, &mut generators, &first, &["c", "a\n", "d\t"], None);
        let s = run(&f, &specs, &generators, "tool ").suggestions;
        assert_eq!(labels(&s), ["c"]);
        assert!(!s[0].runs());
        // An insert value that ends in a newline still runs the line.
        let specs = Specs::new(&["nr"]);
        let s = run(&f, &specs, &Generators::default(), "nr -").suggestions;
        assert!(pick(&s, "-").runs());
    }

    /// Synthetic generator names, as `brew formulae` and `brew casks` give
    /// them: lowercase words, digits and dashes, 3 to 18 characters.
    fn synthetic_names(count: usize, seed: u64) -> Vec<String> {
        const LETTERS: &[u8] = b"abcdefghijklmnopqrstuvwxyz";
        let mut state = seed;
        let mut next = move || {
            // xorshift64
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        (0..count)
            .map(|i| {
                let len = 3 + (next() % 16) as usize;
                let mut name: String = (0..len)
                    .map(|_| LETTERS[(next() % 26) as usize] as char)
                    .collect();
                match next() % 5 {
                    0 => name.insert(len / 2, '-'),
                    1 => name.push_str(&format!("@{}", next() % 20)),
                    _ => {}
                }
                format!("{name}{i}")
            })
            .collect()
    }

    /// Answer every run and query term `computed`'s arg asks for: `per_run`
    /// synthetic names each.
    fn answer_all(f: &Fixture, generators: &mut Generators, computed: &Computed, per_run: usize) {
        let env = Arc::new(Vec::new());
        let history = History::empty();
        let (cwd, home) = (f.cwd(), f.home());
        let run = RunEnv {
            cwd: &cwd,
            home: &home,
            env: &env,
            history: &history,
        };
        let now = Instant::now();
        generators.observe(&computed.spec_arg, &run, now);
        let jobs = generators.take_jobs(now + Duration::from_secs(1));
        let runs = jobs
            .iter()
            .filter(|job| matches!(job, Job::Generate { .. }))
            .count();
        println!("{runs} generator runs answered, {per_run} items each");
        assert!(runs > 0, "no generator runs");
        let mut answers = Vec::new();
        for (at, job) in jobs.iter().enumerate() {
            match job {
                Job::Generate { id, .. } => answers.push(GenAnswer::Generated {
                    id: *id,
                    result: Ok(synthetic_names(per_run, 0x9E37_79B9 + at as u64)
                        .into_iter()
                        .map(|name| SpecSuggestion {
                            name: vec![name],
                            kind: Some(SuggestionType::Arg),
                            ..SpecSuggestion::default()
                        })
                        .collect()),
                    stale: false,
                }),
                Job::Terms {
                    id,
                    trigger,
                    query_term,
                    ..
                } => answers.push(GenAnswer::Terms {
                    id: *id,
                    trigger: trigger.as_ref().map(|_| false),
                    query_term: query_term.as_ref().map(|(_, term)| term.clone()),
                }),
                _ => {}
            }
        }
        generators.on_answers(answers, &run, now);
    }

    /// One full compute of `text` against `specs` and `generators`, with a
    /// reused engine: (p50, p95) over many runs, and the suggestions.
    fn time_compute(
        f: &Fixture,
        specs: &Specs,
        generators: &Generators,
        text: &str,
    ) -> (Duration, Duration, usize) {
        let view = specs.view();
        let (cwd, home) = (f.cwd(), f.home());
        let generated = generators.view(&cwd, &home, &f.history);
        let r = report(text);
        let usage = Usage::default();
        let is_dir = |p: &Path| p.is_dir();
        let input = EngineInput {
            shell: ShellKind::Zsh,
            report: &r,
            cwd: &cwd,
            home: &home,
            history: &f.history,
            usage: &usage,
            compsys: CompsysState::Pending,
            is_dir: &is_dir,
            specs: Some(&view),
            generated: Some(&generated),
        };
        let mut engine = Engine::new();
        let shown = engine.compute_with(&input, DEFAULTS).suggestions.len();
        for _ in 0..20 {
            std::hint::black_box(engine.compute_with(&input, DEFAULTS));
        }
        let mut times: Vec<Duration> = (0..200)
            .map(|_| {
                let started = Instant::now();
                std::hint::black_box(engine.compute_with(&input, DEFAULTS));
                started.elapsed()
            })
            .collect();
        times.sort_unstable();
        (times[times.len() / 2], times[times.len() * 95 / 100], shown)
    }

    /// One full compute against the real archive, with 16,000 generator
    /// items for `brew install`, as p50 and p95; the targets are on p50.
    /// `cargo test --release --lib compute_timing -- --ignored --nocapture`.
    #[test]
    #[ignore = "timing; run in release"]
    fn compute_timing() {
        let f = Fixture::new();
        let specs = Specs::new(&["aws", "aws/ec2", "git", "ls", "brew"]);
        let mut generators = Generators::default();
        let brew = "brew install fo";
        let first = run(&f, &specs, &generators, brew);
        answer_all(&f, &mut generators, &first, 8_000);
        let shown = run(&f, &specs, &generators, "brew install ").suggestions;
        assert_eq!(
            shown.len(),
            MAX_SUGGESTIONS,
            "the generators' items are shown"
        );
        let targets = [
            ("aws ", Some(Duration::from_micros(500))),
            ("aws ec2 ", None),
            ("git ", None),
            ("ls -", None),
            (brew, Some(Duration::from_micros(1500))),
            ("brew install ", None),
        ];
        for (text, target) in targets {
            let empty = Generators::default();
            let generators = if text.starts_with("brew") {
                &generators
            } else {
                &empty
            };
            let (p50, p95, shown) = time_compute(&f, &specs, generators, text);
            println!(
                "{text:?}: p50 {:.3} ms, p95 {:.3} ms ({shown} shown)",
                p50.as_secs_f64() * 1e3,
                p95.as_secs_f64() * 1e3
            );
            if let Some(target) = target {
                assert!(p50 < target, "{text:?}: {p50:?}");
            }
        }
    }
}
