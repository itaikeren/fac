//! The generators of the arg being typed, on the poll side: when each one
//! runs (Fig's `triggerGenerators`, `state/generators.ts:118-198`), what it
//! answered, and what the engine merges in (`getAllSuggestions`,
//! `suggestions/index.ts:213-224`). Line references are to
//! `packages/autocomplete/src` in aws/amazon-q-developer-cli-autocomplete.
//!
//! [`Generators`] keeps a memo for one arg at a time, keyed by the arg's
//! path; each of its generators has a slot, by index. A generator runs when
//! its arg becomes the one typed, and again only when its `trigger` says so
//! (with no trigger, never, unless the arg is debounced). The memo goes when
//! the arg changes (its runs still in flight are cancelled), on a 7792, and
//! when its spec is loaded again.
//!
//! Nothing here blocks or spawns: runs, `trigger` and `getQueryTerm`
//! functions and `filterTemplateSuggestions` are spec-thread jobs, queued in
//! an outbox the controller sends ([`Generators::take_jobs`]). Templates are
//! answered here, from the file system and the history, as the engine asks
//! ([`GenView`]).
//!
//! The view is pure: it decides from the memo alone whether a generator
//! would run again for the word as it is now, so a compute shows what Fig
//! would show for that keystroke before [`Generators::observe`] has acted on
//! it.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::{Duration, Instant};

use super::cache::GenAnswer;
use super::generators::GenError;
use super::model::{FnRef, Generator, StringOrFn, Suggestion, SuggestionType, Template, Trigger};
use super::needs;
use super::parser::{self, CurrentArg, GeneratorRef, Item};
use super::runtime::{Env, Job};
use crate::complete::files;
use crate::complete::history::History;

/// How long a debounced arg's generators wait after the keystroke that
/// triggered them: Fig's default for `debounce: true`
/// (`state/generators.ts:188-193`); the model keeps `debounce` as a flag.
pub const DEBOUNCE: Duration = Duration::from_millis(200);
/// The most values the `history` template offers.
const MAX_HISTORY_VALUES: usize = 100;

/// The arg a compute's parse ended at, and what its generators run with.
#[derive(Debug, Clone, PartialEq)]
pub struct ArgContext {
    pub spec: String,
    /// The spec's load the parse used ([`LoadedSpec::generation`]): the
    /// jobs for its generators carry it.
    ///
    /// [`LoadedSpec::generation`]: super::runtime::LoadedSpec::generation
    pub generation: u64,
    /// The hand-off command (`git` for `sudo git checkout `).
    pub command: String,
    /// Fig's `tokenArray`: from the command word to the word being typed.
    pub tokens: Vec<String>,
    pub arg: CurrentArg,
    /// The parser's search term: the word being typed, from its query
    /// start (after `--opt=`).
    pub search_term: String,
}

/// What a compute's parse said about the arg being typed.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum SpecArg {
    /// No spec parse (no spec, or the parse waits for something): the
    /// generators are left as they are.
    #[default]
    Unknown,
    /// The parse is ready and no arg is being typed.
    None,
    Some(Box<ArgContext>),
}

/// What one generator shows: its items, in order, and the query term they
/// are matched against (the end of the word an accept replaces).
pub struct GenItems<'a> {
    pub items: Cow<'a, [Item]>,
    pub query_term: Cow<'a, str>,
}

/// The generators' items, as the engine reads them.
pub trait Generated {
    /// What `generator` of `ctx`'s arg shows now; `None` while it has
    /// nothing to show (it has not answered, it reloads with a
    /// `getQueryTerm`, or a function's answer is awaited).
    fn items<'s>(&'s self, ctx: &'s ArgContext, generator: &GeneratorRef) -> Option<GenItems<'s>>;
}

/// An arg's generators as Fig runs them: an arg's `template` is a template
/// generator of its own, and its `generators` are then ignored
/// (`initializeArgMeta` in `@fig/autocomplete-shared`).
pub fn arg_generators(arg: &CurrentArg) -> Cow<'_, [GeneratorRef]> {
    if arg.templates.is_empty() {
        Cow::Borrowed(&arg.generators)
    } else {
        Cow::Owned(vec![GeneratorRef {
            index: 0,
            generator: Generator {
                template: arg.templates.clone(),
                ..Generator::default()
            },
        }])
    }
}

/// Whether a generator is the `history` template (and not a files one).
pub fn is_history_template(generator: &Generator) -> bool {
    way(generator) == Way::Template { history: true }
}

/// How a generator is answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Way {
    /// A `filepaths` or `folders` template: Fig runs its `filepaths` or
    /// `folders` helper for it (`createArgState`,
    /// `autocomplete-parser/src/parseArguments.ts:103-128`), whose query
    /// term is the word after its last `/`. Answered here from the file
    /// system.
    Files { dirs_only: bool },
    /// Another template (`history`; `help` gives nothing here).
    Template { history: bool },
    /// A script or custom generator, on the spec thread.
    Run,
}

fn way(generator: &Generator) -> Way {
    let template = &generator.template;
    if template.contains(&Template::Filepaths) {
        Way::Files { dirs_only: false }
    } else if template.contains(&Template::Folders) {
        Way::Files { dirs_only: true }
    } else if !template.is_empty() {
        Way::Template {
            history: template.contains(&Template::History),
        }
    } else {
        Way::Run
    }
}

/// Whether a generator runs again for the word as it is now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decision {
    Run,
    Keep,
    /// A `trigger` function's answer is awaited.
    Unknown,
}

/// `getQueryTermForSuggestion` with a string (`suggestions/helpers.ts:109-112`):
/// the term after the last occurrence of `mark`, sliced one character past
/// where it starts, as Fig slices it; the whole term without one.
pub fn query_term_after<'t>(term: &'t str, mark: &str) -> &'t str {
    match term.rfind(mark) {
        Some(at) => {
            let next = at + term[at..].chars().next().map_or(0, char::len_utf8);
            &term[next..]
        }
        None => term,
    }
}

/// JavaScript's `lastIndexOf`, in chars (`None` for -1).
fn last_index(text: &str, mark: &str) -> Option<usize> {
    text.rfind(mark).map(|at| text[..at].chars().count())
}

/// A string's JavaScript `length`.
fn js_length(text: &str) -> usize {
    text.encode_utf16().count()
}

/// Fig's `trigger` rules for the string and object forms
/// (`state/generators.ts:142-169`): `current` and `previous` are the search
/// terms of this keystroke and the one before.
fn trigger_fires(trigger: &Trigger, current: &str, previous: &str) -> Option<bool> {
    Some(match trigger {
        Trigger::String(mark) => last_index(current, mark) != last_index(previous, mark),
        Trigger::OnThreshold { length } => {
            let length = *length as usize;
            js_length(current) > length && js_length(previous) <= length
        }
        Trigger::OnMatch { string } => {
            let at = |term: &str| string.iter().position(|item| item == term);
            at(current) != at(previous)
        }
        Trigger::OnChange => current != previous,
        Trigger::Fn(_) => return None,
    })
}

/// A function answer awaited or given.
type QueryKey = (String, u32, String);
type TriggerKey = (String, u32, String, String);
/// `filterTemplateSuggestions` answers: the arg path, the generator index
/// and what was listed.
type FilterKey = (String, usize, String);

/// One generator of the memo's arg.
struct Slot {
    index: usize,
    generator: Generator,
    /// Its latest answer.
    items: Option<Vec<Item>>,
    /// A run is due or in flight; the old items stay meanwhile.
    loading: bool,
    /// The run in flight, sent or waiting for its debounce.
    run: Option<u64>,
    /// A debounced run, sent when due.
    due: Option<(Instant, Job)>,
    /// `trigger` function answers awaited: (current, previous).
    awaited: Vec<(String, String)>,
    /// Its last run was cancelled by the spec thread (a reload): it runs at
    /// the next keystroke.
    rerun: bool,
}

/// The arg whose generators run.
struct Memo {
    spec: String,
    path: String,
    /// The search term of the latest compute (Fig's `previousSearchTerm`
    /// for the next one).
    search: String,
    /// The latest context, for runs a `trigger` answer starts.
    ctx: ArgContext,
    slots: Vec<Slot>,
}

/// What a job's answer is for.
enum Awaited {
    Run {
        path: String,
        index: usize,
    },
    Filter(FilterKey),
    Terms {
        trigger: Option<TriggerKey>,
        query: Option<QueryKey>,
    },
}

/// Where runs happen and what they are given.
pub struct RunEnv<'a> {
    pub cwd: &'a Path,
    pub home: &'a Path,
    pub env: &'a Env,
    pub history: &'a History,
}

#[derive(Default)]
pub struct Generators {
    memo: Option<Memo>,
    queries: HashMap<QueryKey, Option<String>>,
    triggers: HashMap<TriggerKey, Option<bool>>,
    filtered: HashMap<FilterKey, Option<Vec<Item>>>,
    awaiting: HashMap<u64, Awaited>,
    /// Ids logged as started and not yet as ended (for the logs the e2e
    /// tests settle on).
    open: HashSet<u64>,
    outbox: Vec<Job>,
    next_id: u64,
}

impl Generators {
    /// After a compute: start, debounce, cancel or keep the runs of the arg
    /// it parsed to, as Fig's `triggerGenerators` does for a new parse.
    pub fn observe(&mut self, spec_arg: &SpecArg, run: &RunEnv, now: Instant) {
        let ctx = match spec_arg {
            SpecArg::Unknown => return,
            SpecArg::None => return self.drop_memo(),
            SpecArg::Some(ctx) => &**ctx,
        };
        // Another load of the spec has other function ids.
        let same = self.memo.as_ref().is_some_and(|memo| {
            memo.spec == ctx.spec
                && memo.path == ctx.arg.path
                && memo.ctx.generation == ctx.generation
        });
        if !same {
            self.drop_memo();
            let slots = arg_generators(&ctx.arg)
                .iter()
                .map(|generator| Slot {
                    index: generator.index,
                    generator: generator.generator.clone(),
                    items: None,
                    loading: false,
                    run: None,
                    due: None,
                    awaited: Vec::new(),
                    rerun: false,
                })
                .collect();
            self.memo = Some(Memo {
                spec: ctx.spec.clone(),
                path: ctx.arg.path.clone(),
                search: ctx.search_term.clone(),
                ctx: ctx.clone(),
                slots,
            });
            let count = self.memo.as_ref().map_or(0, |memo| memo.slots.len());
            for at in 0..count {
                self.start(at, ctx, run, now);
            }
        } else {
            let previous = self
                .memo
                .as_ref()
                .map(|memo| memo.search.clone())
                .unwrap_or_default();
            let count = self.memo.as_ref().map_or(0, |memo| memo.slots.len());
            for at in 0..count {
                let Some(slot) = self.memo.as_ref().and_then(|memo| memo.slots.get(at)) else {
                    continue;
                };
                match self.decide(
                    slot,
                    &ctx.spec,
                    &ctx.search_term,
                    &previous,
                    ctx.arg.debounce,
                ) {
                    Decision::Run => self.start(at, ctx, run, now),
                    Decision::Keep => {}
                    Decision::Unknown => {
                        let Some(Trigger::Fn(function)) = slot.generator.trigger.clone() else {
                            continue;
                        };
                        let key = (
                            ctx.spec.clone(),
                            function.0,
                            ctx.search_term.clone(),
                            previous.clone(),
                        );
                        if let Some(slot) = self.slot_mut(at) {
                            slot.awaited
                                .push((ctx.search_term.clone(), previous.clone()));
                        }
                        if !self.triggers.contains_key(&key) {
                            self.triggers.insert(key.clone(), None);
                            self.send_terms(ctx, Some(key), None);
                        }
                    }
                }
            }
        }
        if let Some(memo) = &mut self.memo {
            memo.search.clone_from(&ctx.search_term);
            memo.ctx = ctx.clone();
        }
        self.ask_terms(ctx, run);
    }

    /// The query terms the arg's `getQueryTerm` functions give for the
    /// word, and the filtered templates for its directory, asked for when
    /// not known.
    fn ask_terms(&mut self, ctx: &ArgContext, run: &RunEnv) {
        let Some(memo) = &self.memo else {
            return;
        };
        let mut queries = Vec::new();
        let mut filters = Vec::new();
        for slot in &memo.slots {
            let generator = &slot.generator;
            if let (Way::Run, Some(StringOrFn::Fn(function))) =
                (way(generator), &generator.get_query_term)
            {
                let key = (ctx.spec.clone(), function.0, ctx.search_term.clone());
                if !self.queries.contains_key(&key) {
                    queries.push(key);
                }
            }
            if let Some(filter) = generator.filter_template_suggestions {
                let key = (
                    memo.path.clone(),
                    slot.index,
                    listing_key(way(generator), &ctx.search_term),
                );
                if matches!(way(generator), Way::Files { .. } | Way::Template { .. })
                    && !self.filtered.contains_key(&key)
                {
                    filters.push((key, filter, way(generator)));
                }
            }
        }
        for key in queries {
            self.queries.insert(key.clone(), None);
            self.send_terms(ctx, None, Some(key));
        }
        for (key, filter, way) in filters {
            let suggestions = template_suggestions(way, ctx, run.cwd, run.home, run.history);
            let id = self.id();
            tracing::debug!("spec generate id={id} filter={}", filter.0);
            self.open.insert(id);
            self.filtered.insert(key.clone(), None);
            self.awaiting.insert(id, Awaited::Filter(key));
            self.outbox.push(Job::FilterTemplate {
                id,
                spec: ctx.spec.clone(),
                generation: ctx.generation,
                fn_id: filter.0,
                suggestions,
            });
        }
    }

    fn send_terms(
        &mut self,
        ctx: &ArgContext,
        trigger: Option<TriggerKey>,
        query: Option<QueryKey>,
    ) {
        let id = self.id();
        tracing::debug!("spec generate id={id} terms");
        self.open.insert(id);
        self.outbox.push(Job::Terms {
            id,
            spec: ctx.spec.clone(),
            generation: ctx.generation,
            trigger: trigger.as_ref().map(|(_, function, current, previous)| {
                (*function, current.clone(), previous.clone())
            }),
            query_term: query
                .as_ref()
                .map(|(_, function, term)| (*function, term.clone())),
        });
        self.awaiting.insert(id, Awaited::Terms { trigger, query });
    }

    fn id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn slot_mut(&mut self, at: usize) -> Option<&mut Slot> {
        self.memo.as_mut().and_then(|memo| memo.slots.get_mut(at))
    }

    /// Whether slot `slot` runs again for `current` after `previous`
    /// (`state/generators.ts:131-178`). Only runs on the spec thread are
    /// decided; templates are answered for every word.
    fn decide(
        &self,
        slot: &Slot,
        spec: &str,
        current: &str,
        previous: &str,
        debounce: bool,
    ) -> Decision {
        if way(&slot.generator) != Way::Run {
            return Decision::Keep;
        }
        if slot.rerun {
            return Decision::Run;
        }
        // The same word again (a redraw, a late answer): not a keystroke.
        if current == previous {
            return Decision::Keep;
        }
        let Some(trigger) = &slot.generator.trigger else {
            // With no trigger it never runs again, unless the arg is
            // debounced, when it runs at every keystroke.
            return if debounce {
                Decision::Run
            } else {
                Decision::Keep
            };
        };
        let fires = match trigger {
            Trigger::Fn(function) => {
                let key = (
                    spec.to_owned(),
                    function.0,
                    current.to_owned(),
                    previous.to_owned(),
                );
                match self.triggers.get(&key) {
                    Some(Some(fires)) => *fires,
                    _ => return Decision::Unknown,
                }
            }
            other => trigger_fires(other, current, previous).unwrap_or(true),
        };
        if fires { Decision::Run } else { Decision::Keep }
    }

    /// Run slot `at` (again): a run in flight is cancelled; a debounced
    /// arg's run waits [`DEBOUNCE`]. The old items stay meanwhile.
    fn start(&mut self, at: usize, ctx: &ArgContext, run: &RunEnv, now: Instant) {
        let Some(slot) = self.memo.as_ref().and_then(|memo| memo.slots.get(at)) else {
            return;
        };
        if way(&slot.generator) != Way::Run {
            return;
        }
        let old = slot.run;
        let was_due = slot.due.is_some();
        if let Some(old) = old {
            self.awaiting.remove(&old);
            if !was_due {
                self.outbox.push(Job::Cancel { id: old });
            }
            self.close(old);
        }
        let id = self.id();
        let Some(slot) = self.slot_mut(at) else {
            return;
        };
        let job = Job::Generate {
            id,
            spec: ctx.spec.clone(),
            generation: ctx.generation,
            arg_path: ctx.arg.path.clone(),
            generator_index: slot.index,
            tokens: ctx.tokens.clone(),
            search_term: ctx.search_term.clone(),
            cwd: run.cwd.to_path_buf(),
            env: run.env.clone(),
            generator: Some(Box::new((slot.generator.clone(), ctx.arg.is_dangerous))),
        };
        slot.loading = true;
        slot.rerun = false;
        slot.run = Some(id);
        let index = slot.index;
        if ctx.arg.debounce {
            slot.due = Some((now + DEBOUNCE, job));
        } else {
            slot.due = None;
            self.outbox.push(job);
        }
        tracing::debug!(
            "spec generate id={id} spec={} index={index} debounce={}",
            ctx.spec,
            ctx.arg.debounce
        );
        self.open.insert(id);
        self.awaiting.insert(
            id,
            Awaited::Run {
                path: ctx.arg.path.clone(),
                index,
            },
        );
    }

    /// The memo goes: its runs in flight are cancelled.
    fn drop_memo(&mut self) {
        let Some(memo) = self.memo.take() else {
            return;
        };
        for slot in memo.slots {
            if let Some(id) = slot.run {
                self.awaiting.remove(&id);
                if slot.due.is_none() {
                    self.outbox.push(Job::Cancel { id });
                }
                self.close(id);
            }
        }
        self.queries.clear();
        self.triggers.clear();
        self.filtered.clear();
    }

    /// A command ran (7792): the memo is cleared, so the arg's generators
    /// run again at the next prompt.
    pub fn clear(&mut self) {
        self.drop_memo();
    }

    fn close(&mut self, id: u64) {
        if self.open.remove(&id) {
            tracing::debug!("spec generated id={id}");
        }
    }

    /// Take in the spec thread's answers. True when what the popup shows
    /// may have changed.
    pub fn on_answers(&mut self, answers: Vec<GenAnswer>, run: &RunEnv, now: Instant) -> bool {
        let mut changed = false;
        for answer in answers {
            match answer {
                GenAnswer::Generated { id, result, stale } => {
                    changed |= self.on_generated(id, result, stale);
                }
                GenAnswer::Terms {
                    id,
                    trigger,
                    query_term,
                } => {
                    changed |= self.on_terms(id, trigger, query_term, run, now);
                }
                GenAnswer::Reloaded { spec } => changed |= self.on_reloaded(&spec),
                GenAnswer::Released { spec, ids } => changed |= self.on_released(&spec, &ids),
            }
        }
        changed
    }

    /// A spec's context changed: the memo of its arg goes, with its runs in
    /// flight, since its generators may hold function ids of the old
    /// context. The next compute builds the memo again from the parse, and
    /// its generators run against the new context.
    fn on_reloaded(&mut self, spec: &str) -> bool {
        if self.memo.as_ref().is_none_or(|memo| memo.spec != spec) {
            return false;
        }
        tracing::debug!("spec generators of {spec} reset: the spec was loaded again");
        self.drop_memo();
        true
    }

    /// Functions of a spec's were let go of: the memo goes if its
    /// generators call one of them, and is built again from the next parse
    /// (whose need, asked again, gives callable ones).
    fn on_released(&mut self, spec: &str, ids: &[u32]) -> bool {
        let Some(memo) = &self.memo else {
            return false;
        };
        if memo.spec != spec {
            return false;
        }
        let mut used = Vec::new();
        for slot in &memo.slots {
            if let Ok(value) = serde_json::to_value(&slot.generator) {
                needs::fn_ids(&value, &mut used);
            }
        }
        if !used.iter().any(|id| ids.contains(id)) {
            return false;
        }
        tracing::debug!("spec generators of {spec} reset: their functions were let go of");
        self.drop_memo();
        true
    }

    fn on_generated(
        &mut self,
        id: u64,
        result: Result<Vec<Suggestion>, GenError>,
        stale: bool,
    ) -> bool {
        let Some(awaited) = self.awaiting.get(&id) else {
            return false;
        };
        let items = match &result {
            Ok(suggestions) => suggestions.iter().map(parser::generated_item).collect(),
            Err(err) => {
                tracing::debug!("spec generated id={id} failed: {err}");
                Vec::new()
            }
        };
        match awaited {
            Awaited::Filter(key) => {
                let key = key.clone();
                self.awaiting.remove(&id);
                self.close(id);
                match self.filtered.get_mut(&key) {
                    Some(slot) => {
                        *slot = Some(items);
                        true
                    }
                    None => false,
                }
            }
            Awaited::Run { path, index } => {
                let (path, index) = (path.clone(), *index);
                if !stale {
                    self.awaiting.remove(&id);
                    self.close(id);
                }
                let Some(slot) = self.memo.as_mut().and_then(|memo| {
                    (memo.path == path)
                        .then(|| memo.slots.iter_mut().find(|slot| slot.index == index))
                        .flatten()
                }) else {
                    return false;
                };
                if slot.run != Some(id) {
                    return false;
                }
                if !stale {
                    slot.loading = false;
                    slot.run = None;
                }
                if matches!(result, Err(GenError::Cancelled)) {
                    // Its spec was reloaded: the old items stay, and it runs
                    // again at the next keystroke.
                    slot.rerun = true;
                    return false;
                }
                slot.items = Some(items);
                true
            }
            Awaited::Terms { .. } => false,
        }
    }

    fn on_terms(
        &mut self,
        id: u64,
        trigger: Option<bool>,
        query_term: Option<String>,
        run: &RunEnv,
        now: Instant,
    ) -> bool {
        let Some(Awaited::Terms {
            trigger: trigger_key,
            query: query_key,
        }) = self.awaiting.remove(&id)
        else {
            return false;
        };
        self.close(id);
        let mut changed = false;
        if let (Some(key), Some(term)) = (query_key, query_term)
            && let Some(slot) = self.queries.get_mut(&key)
        {
            *slot = Some(term);
            changed = true;
        }
        let (Some(key), Some(fires)) = (trigger_key, trigger) else {
            return changed;
        };
        let Some(answer) = self.triggers.get_mut(&key) else {
            return changed;
        };
        *answer = Some(fires);
        let (spec, function, current, previous) = key;
        let Some(memo) = &mut self.memo else {
            return changed;
        };
        if memo.spec != spec {
            return changed;
        }
        let ctx = memo.ctx.clone();
        let mut start = Vec::new();
        for (at, slot) in memo.slots.iter_mut().enumerate() {
            if !matches!(&slot.generator.trigger, Some(Trigger::Fn(FnRef(f))) if *f == function) {
                continue;
            }
            let before = slot.awaited.len();
            slot.awaited
                .retain(|(c, p)| !(*c == current && *p == previous));
            if slot.awaited.len() < before {
                changed = true;
                if fires {
                    start.push(at);
                }
            }
        }
        for at in start {
            self.start(at, &ctx, run, now);
        }
        changed
    }

    /// Whether the arg's generators have `trigger` or `getQueryTerm`
    /// functions, whose answers each keystroke waits for: the caller then
    /// computes as soon as the shell reports, not at the paint, so they are
    /// asked for early.
    pub fn uses_functions(&self) -> bool {
        self.memo.as_ref().is_some_and(|memo| {
            memo.slots.iter().any(|slot| {
                matches!(slot.generator.trigger, Some(Trigger::Fn(_)))
                    || matches!(slot.generator.get_query_term, Some(StringOrFn::Fn(_)))
            })
        })
    }

    /// When the next debounced run is due.
    pub fn deadline(&self) -> Option<Instant> {
        self.memo
            .as_ref()?
            .slots
            .iter()
            .filter_map(|slot| slot.due.as_ref().map(|(at, _)| *at))
            .min()
    }

    /// The jobs to send now: what was queued, and the debounced runs that
    /// are due.
    pub fn take_jobs(&mut self, now: Instant) -> Vec<Job> {
        if let Some(memo) = &mut self.memo {
            for slot in &mut memo.slots {
                if slot.due.as_ref().is_some_and(|(at, _)| now >= *at)
                    && let Some((_, job)) = slot.due.take()
                {
                    self.outbox.push(job);
                }
            }
        }
        std::mem::take(&mut self.outbox)
    }

    /// A job that could not be sent (no spec thread): it is answered as
    /// failed, so nothing waits for it.
    pub fn unsent(&mut self, job: &Job) {
        let id = match job {
            Job::Generate { id, .. } | Job::FilterTemplate { id, .. } | Job::Terms { id, .. } => {
                *id
            }
            _ => return,
        };
        self.awaiting.remove(&id);
        self.close(id);
    }

    /// The engine's view, with where templates are listed from.
    pub fn view<'a>(&'a self, cwd: &'a Path, home: &'a Path, history: &'a History) -> GenView<'a> {
        GenView {
            store: self,
            cwd,
            home,
            history,
        }
    }

    #[cfg(test)]
    pub(crate) fn is_loading(&self, index: usize) -> bool {
        self.memo
            .as_ref()
            .and_then(|memo| memo.slots.iter().find(|slot| slot.index == index))
            .is_some_and(|slot| slot.loading)
    }
}

/// What a template generator's filter was asked about: the directory and
/// whether hidden files were listed, or the history.
fn listing_key(way: Way, search_term: &str) -> String {
    match way {
        Way::Files { .. } => {
            let (dir, partial) = split_dir(search_term);
            format!("{dir}\u{0}{}", partial.starts_with('.'))
        }
        _ => String::new(),
    }
}

/// `dir/` and the part after it.
fn split_dir(term: &str) -> (&str, &str) {
    match term.rfind('/') {
        Some(at) => term.split_at(at + 1),
        None => ("", term),
    }
}

/// A template generator's suggestions, as Fig's `filepaths`/`folders`
/// helpers and `getTemplateSuggestions` give them: a folder's name ends in
/// `/`, and files are left out for `folders`; names sort as V8's
/// `localeCompare` does, dot files after the others, with no `.DS_Store`
/// and with `../` last (`sortFilesAlphabetically`). The history gives the
/// values typed after the command before. Unlike the helpers (`ls -A`), dot
/// files are listed only for a word that starts with `.`, as in phase 1.
fn template_suggestions(
    way: Way,
    ctx: &ArgContext,
    cwd: &Path,
    home: &Path,
    history: &History,
) -> Vec<Suggestion> {
    match way {
        Way::Files { dirs_only } => {
            let listing = files::list(&ctx.search_term, cwd, home);
            let mut names: Vec<(bool, String, bool)> = listing
                .entries
                .iter()
                .filter(|entry| (entry.is_dir || !dirs_only) && entry.name != ".DS_Store")
                .map(|entry| {
                    let name = if entry.is_dir {
                        format!("{}/", entry.name)
                    } else {
                        entry.name.clone()
                    };
                    (entry.is_hidden, name, entry.is_dir)
                })
                .collect();
            names.sort_by(|a, b| {
                a.0.cmp(&b.0)
                    .then_with(|| parser::locale_compare(&a.1, &b.1))
            });
            // `ls` fails on a directory that is not there: no `../`.
            if listing.read {
                names.push((false, "../".to_owned(), true));
            }
            names
                .into_iter()
                .map(|(_, name, is_dir)| Suggestion {
                    kind: Some(if is_dir {
                        SuggestionType::Folder
                    } else {
                        SuggestionType::File
                    }),
                    insert_value: Some(name.clone()),
                    name: vec![name],
                    is_dangerous: ctx.arg.is_dangerous,
                    ..Suggestion::default()
                })
                .collect()
        }
        Way::Template { history: true } => history_values(history, &ctx.command)
            .into_iter()
            .map(|value| Suggestion {
                name: vec![value],
                kind: Some(SuggestionType::Arg),
                ..Suggestion::default()
            })
            .collect(),
        Way::Template { history: false } | Way::Run => Vec::new(),
    }
}

/// The `history` template's values: the words that are not options after
/// `command` in the history lines that run it, newest first, once each.
/// Fig parses each line against the spec and keeps the values of the same
/// kind of arg (`getHistoryArgSuggestions`, `history/index.ts:319-379`);
/// this does not parse.
fn history_values(history: &History, command: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut values = Vec::new();
    for line in history.newest_first() {
        let mut words = line.split_whitespace();
        let Some(first) = words.next() else {
            continue;
        };
        if first.rsplit('/').next() != Some(command) {
            continue;
        }
        for word in words.filter(|word| !word.starts_with('-')) {
            if seen.insert(word.to_owned()) {
                values.push(word.to_owned());
                if values.len() >= MAX_HISTORY_VALUES {
                    return values;
                }
            }
        }
    }
    values
}

/// The engine's view of [`Generators`]: runs from the memo, templates
/// listed as it asks.
pub struct GenView<'a> {
    store: &'a Generators,
    cwd: &'a Path,
    home: &'a Path,
    history: &'a History,
}

impl GenView<'_> {
    /// The query term a generator's items match: its `getQueryTerm`'s
    /// (`suggestions/helpers.ts:102-118`); `None` while a function's answer
    /// is awaited.
    fn query_term<'s>(
        &'s self,
        ctx: &'s ArgContext,
        generator: &Generator,
    ) -> Option<Cow<'s, str>> {
        let term = ctx.search_term.as_str();
        Some(match &generator.get_query_term {
            None => Cow::Borrowed(term),
            Some(StringOrFn::String(mark)) => Cow::Borrowed(query_term_after(term, mark)),
            Some(StringOrFn::Fn(function)) => {
                let key = (ctx.spec.clone(), function.0, term.to_owned());
                Cow::Borrowed(self.store.queries.get(&key)?.as_deref()?)
            }
        })
    }
}

impl Generated for GenView<'_> {
    fn items<'s>(&'s self, ctx: &'s ArgContext, generator: &GeneratorRef) -> Option<GenItems<'s>> {
        let store = self.store;
        let way = way(&generator.generator);
        if let Some(_filter) = generator.generator.filter_template_suggestions
            && way != Way::Run
        {
            let key = (
                ctx.arg.path.clone(),
                generator.index,
                listing_key(way, &ctx.search_term),
            );
            let items = store.filtered.get(&key)?.as_deref()?;
            return Some(GenItems {
                items: Cow::Borrowed(items),
                query_term: template_query_term(way, ctx, &generator.generator, self)?,
            });
        }
        match way {
            Way::Files { .. } | Way::Template { .. } => {
                let items = template_suggestions(way, ctx, self.cwd, self.home, self.history)
                    .iter()
                    .map(parser::generated_item)
                    .collect::<Vec<_>>();
                Some(GenItems {
                    items: Cow::Owned(items),
                    query_term: template_query_term(way, ctx, &generator.generator, self)?,
                })
            }
            Way::Run => {
                let memo = store
                    .memo
                    .as_ref()
                    .filter(|memo| memo.spec == ctx.spec && memo.path == ctx.arg.path)?;
                let slot = memo
                    .slots
                    .iter()
                    .find(|slot| slot.index == generator.index)?;
                if !slot.awaited.is_empty() {
                    return None;
                }
                let loading = slot.loading
                    || match store.decide(
                        slot,
                        &ctx.spec,
                        &ctx.search_term,
                        &memo.search,
                        ctx.arg.debounce,
                    ) {
                        Decision::Run => true,
                        Decision::Keep => false,
                        Decision::Unknown => return None,
                    };
                // Old items are filtered while a reload is on its way, but
                // not with a query term of their own, which they may not
                // fit (`suggestions/index.ts:217-222`).
                if loading && slot.generator.get_query_term.is_some() {
                    return None;
                }
                let items = slot.items.as_deref()?;
                Some(GenItems {
                    items: Cow::Borrowed(items),
                    query_term: self.query_term(ctx, &slot.generator)?,
                })
            }
        }
    }
}

/// A template generator's query term: the word after its last `/` for the
/// files helpers, else its own `getQueryTerm`'s.
fn template_query_term<'s>(
    way: Way,
    ctx: &'s ArgContext,
    generator: &Generator,
    view: &'s GenView<'_>,
) -> Option<Cow<'s, str>> {
    match way {
        Way::Files { .. } => Some(Cow::Borrowed(split_dir(&ctx.search_term).1)),
        _ => view.query_term(ctx, generator),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::specs::model::Script;

    fn script(name: &str) -> Generator {
        Generator {
            script: Some(Script::Argv(vec!["echo".to_owned(), name.to_owned()])),
            split_on: Some("\n".to_owned()),
            ..Generator::default()
        }
    }

    fn arg(path: &str, generators: Vec<Generator>, debounce: bool) -> CurrentArg {
        CurrentArg {
            path: format!("tool:{path}"),
            name: None,
            description: None,
            templates: Vec::new(),
            generators: generators
                .into_iter()
                .enumerate()
                .map(|(index, generator)| GeneratorRef { index, generator })
                .collect(),
            suggest_current_token: false,
            debounce,
            is_variadic: false,
            filter_strategy: None,
            is_dangerous: false,
        }
    }

    fn ctx(arg: &CurrentArg, search: &str) -> SpecArg {
        SpecArg::Some(Box::new(ArgContext {
            spec: "tool".to_owned(),
            generation: 0,
            command: "tool".to_owned(),
            tokens: vec!["tool".to_owned(), search.to_owned()],
            arg: arg.clone(),
            search_term: search.to_owned(),
        }))
    }

    struct Env0 {
        env: Env,
        history: History,
    }

    impl Env0 {
        fn new() -> Self {
            Self {
                env: Arc::new(Vec::new()),
                history: History::empty(),
            }
        }

        fn run(&self) -> RunEnv<'_> {
            RunEnv {
                cwd: Path::new("/"),
                home: Path::new("/"),
                env: &self.env,
                history: &self.history,
            }
        }
    }

    /// What the jobs taken now are: `G<index>` for a run, `C<id>` for a
    /// cancel, `T` for a terms job, `F` for a filter.
    fn jobs(generators: &mut Generators, now: Instant) -> Vec<String> {
        generators
            .take_jobs(now)
            .iter()
            .map(|job| match job {
                Job::Generate {
                    generator_index, ..
                } => format!("G{generator_index}"),
                Job::Cancel { id } => format!("C{id}"),
                Job::Terms { .. } => "T".to_owned(),
                Job::FilterTemplate { .. } => "F".to_owned(),
                _ => "?".to_owned(),
            })
            .collect()
    }

    /// The ids of the runs in the jobs taken now.
    fn run_ids(generators: &mut Generators, now: Instant) -> Vec<u64> {
        generators
            .take_jobs(now)
            .iter()
            .filter_map(|job| match job {
                Job::Generate { id, .. } => Some(*id),
                _ => None,
            })
            .collect()
    }

    fn answer(id: u64, names: &[&str]) -> GenAnswer {
        GenAnswer::Generated {
            id,
            result: Ok(names
                .iter()
                .map(|name| Suggestion {
                    name: vec![(*name).to_owned()],
                    kind: Some(SuggestionType::Arg),
                    ..Suggestion::default()
                })
                .collect()),
            stale: false,
        }
    }

    /// The names a view shows for generator `index`, and its query term.
    fn shown(
        generators: &Generators,
        spec_arg: &SpecArg,
        index: usize,
    ) -> Option<(Vec<String>, String)> {
        let SpecArg::Some(ctx) = spec_arg else {
            return None;
        };
        let history = History::empty();
        let view = generators.view(Path::new("/"), Path::new("/"), &history);
        let generator = ctx.arg.generators.get(index)?;
        let found = view.items(ctx, generator)?;
        Some((
            found
                .items
                .iter()
                .map(|item| item.names[0].clone())
                .collect(),
            found.query_term.into_owned(),
        ))
    }

    /// Whether `trigger` runs the generator again at each word after the
    /// first (`state/generators.ts:131-178`).
    fn reruns(trigger: Option<Trigger>, words: &[&str]) -> Vec<bool> {
        let env = Env0::new();
        let mut generators = Generators::default();
        let generator = Generator {
            trigger,
            ..script("x")
        };
        let arg = arg("args.0", vec![generator], false);
        let now = Instant::now();
        generators.observe(&ctx(&arg, words[0]), &env.run(), now);
        assert_eq!(jobs(&mut generators, now), ["G0"], "the first word runs it");
        words[1..]
            .iter()
            .map(|word| {
                generators.observe(&ctx(&arg, word), &env.run(), now);
                jobs(&mut generators, now).contains(&"G0".to_owned())
            })
            .collect()
    }

    #[test]
    fn without_a_trigger_a_generator_runs_once_per_arg() {
        assert_eq!(reruns(None, &["", "a", "ab", "a"]), [false, false, false]);
        // A new arg runs it again; the old run, unanswered, is cancelled.
        let env = Env0::new();
        let mut generators = Generators::default();
        let now = Instant::now();
        let first = arg("args.0", vec![script("x")], false);
        generators.observe(&ctx(&first, ""), &env.run(), now);
        let ids = run_ids(&mut generators, now);
        let second = arg("args.1", vec![script("x")], false);
        generators.observe(&ctx(&second, ""), &env.run(), now);
        assert_eq!(
            jobs(&mut generators, now),
            [format!("C{}", ids[0]), "G0".to_owned()]
        );
        // The same arg and word (a redraw) runs nothing.
        generators.observe(&ctx(&second, ""), &env.run(), now);
        assert!(jobs(&mut generators, now).is_empty());
    }

    #[test]
    fn a_string_trigger_compares_last_indices() {
        let trigger = Some(Trigger::String("/".to_owned()));
        assert_eq!(
            reruns(
                trigger,
                &["src", "src/", "src/m", "src/ma", "src/ma/", "src/ma", "sr"]
            ),
            [true, false, false, true, true, true]
        );
    }

    #[test]
    fn object_triggers_follow_fig() {
        assert_eq!(
            reruns(Some(Trigger::OnChange), &["", "a", "a", "ab"]),
            [true, false, true]
        );
        // `a.length > length && !(b.length > length)`: only going over it.
        assert_eq!(
            reruns(
                Some(Trigger::OnThreshold { length: 2 }),
                &["a", "ab", "abc", "abcd", "ab", "abc"]
            ),
            [false, true, false, false, true]
        );
        assert_eq!(
            reruns(
                Some(Trigger::OnMatch {
                    string: vec!["x".to_owned(), "y".to_owned()]
                }),
                &["", "x", "xy", "xyz", "y"]
            ),
            [true, true, false, true]
        );
    }

    #[test]
    fn a_trigger_function_waits_for_its_answer() {
        let env = Env0::new();
        let mut generators = Generators::default();
        let generator = Generator {
            trigger: Some(Trigger::Fn(FnRef(7))),
            ..script("x")
        };
        let arg = arg("args.0", vec![generator], false);
        let now = Instant::now();
        generators.observe(&ctx(&arg, "a"), &env.run(), now);
        let first = run_ids(&mut generators, now)[0];
        assert!(generators.on_answers(vec![answer(first, &["one"])], &env.run(), now));
        assert!(shown(&generators, &ctx(&arg, "a"), 0).is_some());
        // A new word asks the function; the items wait for its answer.
        assert!(shown(&generators, &ctx(&arg, "ab"), 0).is_none());
        generators.observe(&ctx(&arg, "ab"), &env.run(), now);
        let terms = generators.take_jobs(now);
        let [
            Job::Terms {
                id,
                trigger: Some((7, current, previous)),
                query_term: None,
                ..
            },
        ] = terms.as_slice()
        else {
            panic!("{terms:?}");
        };
        assert_eq!((current.as_str(), previous.as_str()), ("ab", "a"));
        assert!(shown(&generators, &ctx(&arg, "ab"), 0).is_none());
        // `false`: the old items show again, nothing runs.
        let no = GenAnswer::Terms {
            id: *id,
            trigger: Some(false),
            query_term: None,
        };
        assert!(generators.on_answers(vec![no], &env.run(), now));
        assert!(jobs(&mut generators, now).is_empty());
        assert_eq!(
            shown(&generators, &ctx(&arg, "ab"), 0).map(|(names, _)| names),
            Some(vec!["one".to_owned()])
        );
        // `true`: it runs again.
        generators.observe(&ctx(&arg, "abc"), &env.run(), now);
        let Some(Job::Terms { id, .. }) = generators.take_jobs(now).pop() else {
            panic!("no terms job");
        };
        let yes = GenAnswer::Terms {
            id,
            trigger: Some(true),
            query_term: None,
        };
        generators.on_answers(vec![yes], &env.run(), now);
        assert_eq!(jobs(&mut generators, now), ["G0"]);
    }

    #[test]
    fn debounce_delays_the_run() {
        let env = Env0::new();
        let mut generators = Generators::default();
        let arg = arg("args.0", vec![script("x")], true);
        let now = Instant::now();
        generators.observe(&ctx(&arg, "a"), &env.run(), now);
        assert!(jobs(&mut generators, now).is_empty());
        assert_eq!(generators.deadline(), Some(now + DEBOUNCE));
        // A keystroke before it is due starts the wait again (a debounced
        // arg without a trigger runs at every keystroke).
        let later = now + Duration::from_millis(150);
        generators.observe(&ctx(&arg, "ab"), &env.run(), later);
        assert!(jobs(&mut generators, now + DEBOUNCE).is_empty());
        assert_eq!(generators.deadline(), Some(later + DEBOUNCE));
        assert_eq!(jobs(&mut generators, later + DEBOUNCE), ["G0"]);
        assert_eq!(generators.deadline(), None);
    }

    #[test]
    fn old_items_stay_while_a_run_loads_but_not_with_a_query_term() {
        let env = Env0::new();
        let now = Instant::now();
        for (get_query_term, kept) in [
            (None, true),
            (Some(StringOrFn::String("/".to_owned())), false),
        ] {
            let mut generators = Generators::default();
            let generator = Generator {
                trigger: Some(Trigger::OnChange),
                get_query_term,
                ..script("x")
            };
            let arg = arg("args.0", vec![generator], false);
            generators.observe(&ctx(&arg, "a"), &env.run(), now);
            assert!(
                shown(&generators, &ctx(&arg, "a"), 0).is_none(),
                "nothing yet"
            );
            let id = run_ids(&mut generators, now)[0];
            generators.on_answers(vec![answer(id, &["one"])], &env.run(), now);
            assert!(!generators.is_loading(0));
            // The view sees the run the new word starts before `observe`
            // does, and after it.
            assert_eq!(shown(&generators, &ctx(&arg, "ab"), 0).is_some(), kept);
            generators.observe(&ctx(&arg, "ab"), &env.run(), now);
            assert!(generators.is_loading(0));
            assert_eq!(shown(&generators, &ctx(&arg, "ab"), 0).is_some(), kept);
            let id = run_ids(&mut generators, now)[0];
            // A stale answer shows; the fresh one replaces it.
            let stale = GenAnswer::Generated {
                id,
                result: Ok(Vec::new()),
                stale: true,
            };
            generators.on_answers(vec![stale], &env.run(), now);
            assert!(generators.is_loading(0));
            generators.on_answers(vec![answer(id, &["two"])], &env.run(), now);
            assert_eq!(
                shown(&generators, &ctx(&arg, "ab"), 0).map(|(names, _)| names),
                Some(vec!["two".to_owned()])
            );
        }
    }

    #[test]
    fn an_error_shows_nothing() {
        let env = Env0::new();
        let now = Instant::now();
        let mut generators = Generators::default();
        let arg = arg("args.0", vec![script("x")], false);
        generators.observe(&ctx(&arg, ""), &env.run(), now);
        let id = run_ids(&mut generators, now)[0];
        let failed = GenAnswer::Generated {
            id,
            result: Err(GenError::Timeout),
            stale: false,
        };
        generators.on_answers(vec![failed], &env.run(), now);
        assert_eq!(
            shown(&generators, &ctx(&arg, ""), 0),
            Some((Vec::new(), String::new()))
        );
    }

    #[test]
    fn the_memo_clears_on_a_command() {
        let env = Env0::new();
        let now = Instant::now();
        let mut generators = Generators::default();
        let arg = arg("args.0", vec![script("x")], false);
        generators.observe(&ctx(&arg, ""), &env.run(), now);
        let id = run_ids(&mut generators, now)[0];
        generators.on_answers(vec![answer(id, &["one"])], &env.run(), now);
        generators.observe(&ctx(&arg, "o"), &env.run(), now);
        assert!(jobs(&mut generators, now).is_empty());
        generators.clear();
        assert!(shown(&generators, &ctx(&arg, "o"), 0).is_none());
        generators.observe(&ctx(&arg, "o"), &env.run(), now);
        assert_eq!(jobs(&mut generators, now), ["G0"]);
    }

    /// A reload cancels the runs of its spec's arg and drops the memo, so
    /// the next compute runs its generators again, as the new parse gives
    /// them (with the new context's function ids).
    /// The same arg from another load of the spec (its function ids may
    /// name other functions there) starts over: the old run is cancelled,
    /// and the new one carries the new load.
    #[test]
    fn another_load_of_the_spec_starts_the_arg_over() {
        let env = Env0::new();
        let now = Instant::now();
        let at = |generation: u64| {
            let arg = arg(
                "args.0",
                vec![Generator {
                    custom: Some(FnRef(7)),
                    ..Generator::default()
                }],
                false,
            );
            match ctx(&arg, "a") {
                SpecArg::Some(mut ctx) => {
                    ctx.generation = generation;
                    SpecArg::Some(ctx)
                }
                other => other,
            }
        };
        let sent = |generators: &mut Generators| -> Vec<(String, u64)> {
            generators
                .take_jobs(now)
                .into_iter()
                .map(|job| match job {
                    Job::Generate { id, generation, .. } => (format!("G{id}"), generation),
                    Job::Cancel { id } => (format!("C{id}"), 0),
                    _ => ("?".to_owned(), 0),
                })
                .collect()
        };
        let mut generators = Generators::default();
        generators.observe(&at(1), &env.run(), now);
        let first = sent(&mut generators);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].1, 1);
        // The same load again keeps the run.
        generators.observe(&at(1), &env.run(), now);
        assert!(sent(&mut generators).is_empty());
        generators.observe(&at(2), &env.run(), now);
        let again = sent(&mut generators);
        let old = first[0].0.trim_start_matches('G');
        assert_eq!(again.len(), 2, "{again:?}");
        assert_eq!(again[0].0, format!("C{old}"));
        assert!(again[1].0.starts_with('G'));
        assert_eq!(again[1].1, 2);
    }

    /// Functions let go of: the memo goes only when its generators call one.
    #[test]
    fn released_functions_reset_the_generators_that_call_them() {
        let env = Env0::new();
        let now = Instant::now();
        let custom = Generator {
            custom: Some(FnRef(1 << 24)),
            ..Generator::default()
        };
        let released = |spec: &str, ids: Vec<u32>| GenAnswer::Released {
            spec: spec.to_owned(),
            ids,
        };
        let mut generators = Generators::default();
        let arg = arg("args.0", vec![custom], false);
        generators.observe(&ctx(&arg, "a"), &env.run(), now);
        generators.take_jobs(now);
        assert!(generators.is_loading(0));
        let answers = vec![released("tool", vec![5]), released("other", vec![1 << 24])];
        assert!(!generators.on_answers(answers, &env.run(), now));
        assert!(generators.is_loading(0));
        assert!(generators.on_answers(vec![released("tool", vec![1 << 24])], &env.run(), now));
        assert!(!generators.is_loading(0));
        assert_eq!(jobs(&mut generators, now).len(), 1, "its run is cancelled");
    }

    #[test]
    fn a_reload_cancels_the_runs_and_runs_them_again() {
        let env = Env0::new();
        let now = Instant::now();
        let custom = |id: u32| Generator {
            custom: Some(FnRef(id)),
            ..Generator::default()
        };
        let sent = |generators: &mut Generators| -> Vec<(u64, Option<FnRef>)> {
            generators
                .take_jobs(now)
                .into_iter()
                .filter_map(|job| match job {
                    Job::Generate { id, generator, .. } => {
                        Some((id, generator.and_then(|given| given.0.custom)))
                    }
                    _ => None,
                })
                .collect()
        };
        let reloaded = |spec: &str| GenAnswer::Reloaded {
            spec: spec.to_owned(),
        };

        // The spec thread cancels the run in flight, then the spec loads
        // again.
        let mut generators = Generators::default();
        let old = arg("args.0", vec![custom(100)], false);
        generators.observe(&ctx(&old, "a"), &env.run(), now);
        let [(first, Some(FnRef(100)))] = sent(&mut generators)[..] else {
            panic!("no run");
        };
        let cancelled = GenAnswer::Generated {
            id: first,
            result: Err(GenError::Cancelled),
            stale: false,
        };
        assert!(generators.on_answers(vec![cancelled, reloaded("tool")], &env.run(), now));
        assert!(shown(&generators, &ctx(&old, "a"), 0).is_none());
        let new = arg("args.0", vec![custom(200)], false);
        generators.observe(&ctx(&new, "a"), &env.run(), now);
        let [(second, Some(FnRef(200)))] = sent(&mut generators)[..] else {
            panic!("not run again with the new generator");
        };
        assert_ne!(second, first);
        generators.on_answers(vec![answer(second, &["fresh"])], &env.run(), now);
        assert_eq!(
            shown(&generators, &ctx(&new, "a"), 0).map(|(names, _)| names),
            Some(vec!["fresh".to_owned()])
        );

        // Evicted with a run in flight: the run is cancelled here, and its
        // late answer is not taken.
        let mut generators = Generators::default();
        generators.observe(&ctx(&old, "a"), &env.run(), now);
        let [(first, _)] = sent(&mut generators)[..] else {
            panic!("no run");
        };
        // Another spec's reload changes nothing.
        assert!(!generators.on_answers(vec![reloaded("other")], &env.run(), now));
        assert!(generators.is_loading(0));
        assert!(generators.on_answers(vec![reloaded("tool")], &env.run(), now));
        assert_eq!(jobs(&mut generators, now), [format!("C{first}")]);
        assert!(!generators.on_answers(vec![answer(first, &["late"])], &env.run(), now));
        generators.observe(&ctx(&new, "a"), &env.run(), now);
        assert_eq!(jobs(&mut generators, now), ["G0"]);
    }

    #[test]
    fn query_terms_follow_fig() {
        assert_eq!(query_term_after("src/ma", "/"), "ma");
        assert_eq!(query_term_after("a/b/c", "/"), "c");
        assert_eq!(query_term_after("abc", "/"), "abc");
        // Fig slices one character past where the mark starts.
        assert_eq!(query_term_after("a::b", "::"), ":b");
        assert_eq!(query_term_after("ab", ""), "");
        // A `getQueryTerm` function's answer is waited for.
        let env = Env0::new();
        let now = Instant::now();
        let mut generators = Generators::default();
        let generator = Generator {
            get_query_term: Some(StringOrFn::Fn(FnRef(3))),
            ..script("x")
        };
        let arg = arg("args.0", vec![generator], false);
        generators.observe(&ctx(&arg, "src/m"), &env.run(), now);
        let jobs = generators.take_jobs(now);
        let id = jobs
            .iter()
            .find_map(|job| match job {
                Job::Generate { id, .. } => Some(*id),
                _ => None,
            })
            .expect("a run");
        let terms = jobs
            .iter()
            .find_map(|job| match job {
                Job::Terms {
                    id,
                    query_term: Some((3, term)),
                    ..
                } => Some((*id, term.clone())),
                _ => None,
            })
            .expect("a terms job");
        assert_eq!(terms.1, "src/m");
        generators.on_answers(vec![answer(id, &["main"])], &env.run(), now);
        assert!(shown(&generators, &ctx(&arg, "src/m"), 0).is_none());
        let term = GenAnswer::Terms {
            id: terms.0,
            trigger: None,
            query_term: Some("m".to_owned()),
        };
        generators.on_answers(vec![term], &env.run(), now);
        assert_eq!(
            shown(&generators, &ctx(&arg, "src/m"), 0),
            Some((vec!["main".to_owned()], "m".to_owned()))
        );
    }

    #[test]
    fn the_history_template_offers_values_typed_after_the_command() {
        let mut history = History::empty();
        for line in [
            "ssh -p 22 old.example",
            "ls -la",
            "/usr/bin/ssh new.example",
            "ssh new.example",
        ] {
            history.add(line);
        }
        assert_eq!(
            history_values(&history, "ssh"),
            ["new.example", "22", "old.example"]
        );
        let mut arg = arg("args.0", Vec::new(), false);
        arg.templates = vec![Template::History];
        let SpecArg::Some(ctx) = ctx(&arg, "ne") else {
            unreachable!()
        };
        let ctx = ArgContext {
            command: "ssh".to_owned(),
            ..*ctx
        };
        let generators = Generators::default();
        let view = generators.view(Path::new("/"), Path::new("/"), &history);
        let found = view
            .items(&ctx, &arg_generators(&ctx.arg)[0])
            .expect("templates answer at once");
        let names: Vec<&str> = found
            .items
            .iter()
            .map(|item| item.names[0].as_str())
            .collect();
        assert_eq!(names, ["new.example", "22", "old.example"]);
        assert_eq!(found.query_term, "ne");
    }
}
