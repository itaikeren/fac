//! The poll thread's answers to what the parser needs ([`Need`]): the spec
//! functions it calls (`loadSpec`, `generateSpec`, `parserDirectives.alias`)
//! run on the spec thread as [`Call`]s, and their results are kept here as
//! [`Answer`]s, converted as Fig converts them, in a map of at most
//! [`MAX_ANSWERS`] entries (the oldest goes first). An answer that holds
//! functions keeps them alive on the spec thread; once it is gone from here
//! the spec thread is told to let them go ([`SpecCache::release`]).
//!
//! A pending `generateSpec` does not hold the popup up: until it answers,
//! [`Needs::lookup`] gives the latest answer of the same function, or
//! `Failed` (which keeps the parse's state), as a provisional answer. The
//! other needs are not provisional.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use super::cache::{Call, SpecCache, SpecState};
use super::model::{LoadSpec, Subcommand};
use super::parser::{self, Answer, Need, Resolved};
use super::runtime::{CallError, Env, LoadedSpec, WALKED_FIRST};

/// How many answers are kept.
pub const MAX_ANSWERS: usize = 256;

/// How long a call may take, its promise included.
fn call_budget() -> Duration {
    Duration::from_millis(crate::config::global().generator_timeout_ms)
}

/// What [`Needs::lookup`] found.
#[derive(Debug, Clone, PartialEq)]
pub enum Lookup {
    Answered(Arc<Answer>),
    /// The need is not answered; this stands in for it.
    Provisional(Arc<Answer>),
    Missing,
}

#[derive(Default)]
pub struct Needs {
    answers: HashMap<Need, Arc<Answer>>,
    /// The keys of `answers`, oldest first.
    order: VecDeque<Need>,
    /// The latest answer of each `generateSpec` function.
    latest: HashMap<(String, u32), Arc<Answer>>,
    /// The calls in flight: their need, by call id.
    pending: HashMap<u64, Need>,
    asked: HashSet<Need>,
}

impl Needs {
    /// The key a need's answer is kept under. Fig keeps a parse, and so a
    /// `generateSpec` answer, per line up to its last token (`getCacheKey`,
    /// `parseArguments.ts:767-777`): typing on in the last word asks again
    /// for nothing.
    pub fn key(need: &Need) -> Need {
        match need {
            Need::GenerateSpec {
                spec,
                fn_id,
                tokens,
            } => Need::GenerateSpec {
                spec: spec.clone(),
                fn_id: *fn_id,
                tokens: tokens[..tokens.len().saturating_sub(1)].to_vec(),
            },
            other => other.clone(),
        }
    }

    /// The answer to `need`, or what stands in for it.
    pub fn lookup(&self, need: &Need) -> Lookup {
        let key = Self::key(need);
        if let Some(answer) = self.answers.get(&key) {
            return Lookup::Answered(Arc::clone(answer));
        }
        match need {
            Need::GenerateSpec { spec, fn_id, .. } => Lookup::Provisional(
                self.latest
                    .get(&(spec.clone(), *fn_id))
                    .cloned()
                    .unwrap_or_else(|| Arc::new(Answer::Failed)),
            ),
            _ => Lookup::Missing,
        }
    }

    /// Ask for `need`, unless it is answered or asked already: a spec is
    /// loaded, a function is called with `executeShellCommand`, in `cwd`
    /// with `env`. `generation` is the load of the need's spec the parse
    /// that needs it used (its function ids are that load's). False when
    /// nothing was sent.
    pub fn request(
        &mut self,
        need: &Need,
        generation: u64,
        cache: &mut SpecCache,
        cwd: &Path,
        env: &Env,
    ) -> bool {
        self.forget_reloaded(cache);
        let key = Self::key(need);
        if self.answers.contains_key(&key) || self.asked.contains(&key) {
            return false;
        }
        let (spec, fn_id, args) = match need {
            Need::Spec { name } => {
                cache.get(name);
                return true;
            }
            Need::LoadSpecFn { spec, fn_id, token } | Need::Alias { spec, fn_id, token } => {
                (spec, *fn_id, serde_json::json!([token]))
            }
            Need::GenerateSpec {
                spec,
                fn_id,
                tokens,
            } => (spec, *fn_id, serde_json::json!([tokens])),
        };
        let call = Call {
            spec: spec.clone(),
            generation,
            fn_id,
            args_json: args.to_string(),
            cwd: Some(cwd.to_path_buf()),
            env: Some(Arc::clone(env)),
            with_exec: true,
            budget: call_budget(),
        };
        let Some(id) = cache.call(call) else {
            return false;
        };
        self.pending.insert(id, key.clone());
        self.asked.insert(key);
        true
    }

    /// Take in the calls the spec thread answered: the needs answered.
    pub fn take_answers(&mut self, cache: &mut SpecCache) -> Vec<Need> {
        self.forget_reloaded(cache);
        let mut answered = Vec::new();
        for (id, result) in cache.take_called() {
            let Some(need) = self.pending.remove(&id) else {
                continue;
            };
            self.asked.remove(&need);
            match convert(&need, result) {
                Some(answer) => {
                    for (spec, ids) in self.insert(need.clone(), answer) {
                        cache.release(spec, ids);
                    }
                    answered.push(need);
                }
                // Asked again when the parse needs it next.
                None => tracing::debug!("spec need {need:?} not answered; asked again later"),
            }
        }
        answered
    }

    /// Forget what the specs loaded again (or dropped) since the last look
    /// gave through their old context: the answers that hold its functions,
    /// whose ids the new context does not know (see `runtime::Registry`),
    /// and the calls in flight, whose answers would. Both are asked for
    /// again when a parse needs them. Answers without functions stay.
    fn forget_reloaded(&mut self, cache: &mut SpecCache) {
        for name in cache.take_reloaded() {
            self.forget(&name);
        }
    }

    fn forget(&mut self, name: &str) {
        let of_spec = |need: &Need| need_spec(need) == Some(name);
        let before = self.answers.len();
        self.answers
            .retain(|need, answer| !(of_spec(need) && holds_functions(answer)));
        if self.answers.len() < before {
            let answers = &self.answers;
            self.order.retain(|need| answers.contains_key(need));
        }
        self.latest
            .retain(|(spec, _), answer| !(spec == name && holds_functions(answer)));
        let calls: Vec<u64> = self
            .pending
            .iter()
            .filter(|(_, need)| of_spec(need))
            .map(|(id, _)| *id)
            .collect();
        for id in calls {
            if let Some(need) = self.pending.remove(&id) {
                self.asked.remove(&need);
            }
        }
    }

    /// Keep an answer. What it pushes out (the oldest answer, past
    /// [`MAX_ANSWERS`]; the function's previous latest answer) and nothing
    /// else holds: the spec and the walked function ids of each, for the
    /// spec thread to let go of.
    fn insert(&mut self, need: Need, answer: Answer) -> Vec<(String, Vec<u32>)> {
        let answer = Arc::new(answer);
        let mut gone = Vec::new();
        if let Need::GenerateSpec { spec, fn_id, .. } = &need
            && let Some(previous) = self
                .latest
                .insert((spec.clone(), *fn_id), Arc::clone(&answer))
        {
            gone.push((spec.clone(), previous));
        }
        if let Some(previous) = self.answers.insert(need.clone(), answer) {
            if let Some(spec) = need_spec(&need) {
                gone.push((spec.to_owned(), previous));
            }
        } else {
            self.order.push_back(need);
        }
        while self.order.len() > MAX_ANSWERS {
            if let Some(oldest) = self.order.pop_front()
                && let Some(answer) = self.answers.remove(&oldest)
                && let Some(spec) = need_spec(&oldest)
            {
                gone.push((spec.to_owned(), answer));
            }
        }
        gone.into_iter()
            .filter(|(_, answer)| {
                !self
                    .answers
                    .values()
                    .chain(self.latest.values())
                    .any(|kept| Arc::ptr_eq(kept, answer))
            })
            .map(|(spec, answer)| (spec, walked_ids(&answer)))
            .filter(|(_, ids)| !ids.is_empty())
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.answers.len()
    }

    /// Answer `need` as the spec thread would: what it pushed out, as
    /// [`Needs::insert`] gives it.
    #[cfg(test)]
    pub(crate) fn answer_with(&mut self, need: &Need, answer: Answer) -> Vec<(String, Vec<u32>)> {
        self.insert(Self::key(need), answer)
    }
}

/// The spec whose function a need calls.
pub fn need_spec(need: &Need) -> Option<&str> {
    match need {
        Need::Spec { .. } => None,
        Need::LoadSpecFn { spec, .. }
        | Need::GenerateSpec { spec, .. }
        | Need::Alias { spec, .. } => Some(spec),
    }
}

/// The ids of the functions walked into an answer (`{"$fn": n}` in its
/// JSON form, from [`WALKED_FIRST`] on).
fn walked_ids(answer: &Answer) -> Vec<u32> {
    let Answer::Spec(sub) = answer else {
        return Vec::new();
    };
    let mut ids = Vec::new();
    if let Ok(value) = serde_json::to_value(sub) {
        fn_ids(&value, &mut ids);
    }
    ids.retain(|&id| id >= WALKED_FIRST);
    ids.sort_unstable();
    ids.dedup();
    ids
}

/// Every `{"$fn": n}` in a JSON value.
pub fn fn_ids(value: &serde_json::Value, ids: &mut Vec<u32>) {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(id) = map
                .get("$fn")
                .and_then(serde_json::Value::as_u64)
                .and_then(|id| u32::try_from(id).ok())
            {
                ids.push(id);
            }
            map.values().for_each(|value| fn_ids(value, ids));
        }
        serde_json::Value::Array(items) => items.iter().for_each(|value| fn_ids(value, ids)),
        _ => {}
    }
}

/// Whether an answer holds functions (`{"$fn": n}` in its JSON form): ids
/// walked in the context that answered.
fn holds_functions(answer: &Answer) -> bool {
    fn has_fn(value: &serde_json::Value) -> bool {
        match value {
            serde_json::Value::Object(map) => map.contains_key("$fn") || map.values().any(has_fn),
            serde_json::Value::Array(items) => items.iter().any(has_fn),
            _ => false,
        }
    }
    match answer {
        // A tree that does not convert is kept from no context: drop it.
        Answer::Spec(sub) => serde_json::to_value(sub).map_or(true, |value| has_fn(&value)),
        Answer::Alias(_) | Answer::Failed => false,
    }
}

/// A call's result as the parser's answer, as Fig converts it; `None` when
/// the call did not run to an answer (its spec was dropped or reloaded) and
/// is to be asked again.
pub fn convert(need: &Need, result: Result<String, CallError>) -> Option<Answer> {
    let json = match result {
        Ok(json) => json,
        Err(CallError::Stopped | CallError::NotLoaded) => return None,
        Err(err) => {
            tracing::debug!("spec need {need:?} failed: {err}");
            return Some(Answer::Failed);
        }
    };
    let value: serde_json::Value = match serde_json::from_str(&json) {
        Ok(value) => value,
        Err(_) => return Some(Answer::Failed),
    };
    Some(match need {
        Need::Spec { .. } => return None,
        // A spec object (`parseArguments.ts:671-694`); anything else throws
        // in `convertSubcommand`, which Fig catches.
        Need::GenerateSpec { .. } => match serde_json::from_value::<Subcommand>(value) {
            Ok(spec) if spec_object(&json) => Answer::Spec(spec),
            _ => Answer::Failed,
        },
        Need::LoadSpecFn { .. } => load_spec(value),
        Need::Alias { .. } => match value.as_str().and_then(parser::alias_tokens) {
            Some(words) => Answer::Alias(words),
            None => Answer::Failed,
        },
    })
}

/// Whether a JSON text is an object.
fn spec_object(json: &str) -> bool {
    json.trim_start().starts_with('{')
}

/// A `loadSpec` function's result (`convertLoadSpec` in
/// `@fig/autocomplete-shared`, `specMetadata.js`): a list of spec locations
/// (the first is used), one location (an object with a `type`), or an
/// inline subcommand. A bare string has no `type` to look for and throws.
fn load_spec(value: serde_json::Value) -> Answer {
    let location = match &value {
        serde_json::Value::Array(locations) => locations.first(),
        serde_json::Value::Object(object) if object.contains_key("type") => Some(&value),
        serde_json::Value::Object(_) => {
            return match serde_json::from_value::<Subcommand>(value) {
                Ok(sub) => Answer::Spec(sub),
                Err(_) => Answer::Failed,
            };
        }
        _ => None,
    };
    let Some(location) = location else {
        return Answer::Failed;
    };
    let global = location.get("type").and_then(|kind| kind.as_str()) == Some("global");
    match location.get("name").and_then(|name| name.as_str()) {
        Some(name) if global => Answer::Spec(Subcommand {
            load_spec: Some(LoadSpec::Name(name.to_owned())),
            ..Subcommand::default()
        }),
        // A local (`.fig` folder) spec, which fac does not have.
        _ => Answer::Failed,
    }
}

/// The parser's view of what the poll thread has: the specs it was told
/// about and the answers in a [`Needs`]. It records what the parse asked for
/// and did not get (specs not ready, needs not answered, provisional
/// answers given), for the caller to ask for and wait for.
pub struct View<'a> {
    specs: &'a HashMap<String, SpecState>,
    needs: &'a Needs,
    missed_specs: RefCell<Vec<String>>,
    missed: RefCell<Vec<Need>>,
    /// Every spec name looked up.
    looked_up: RefCell<Vec<String>>,
}

impl<'a> View<'a> {
    pub fn new(specs: &'a HashMap<String, SpecState>, needs: &'a Needs) -> Self {
        Self {
            specs,
            needs,
            missed_specs: RefCell::default(),
            missed: RefCell::default(),
            looked_up: RefCell::default(),
        }
    }

    /// What the parse used: the spec names it asked for and did not get,
    /// the needs it did not get a real answer for, and every spec name it
    /// looked up (a change to any of them changes the parse).
    pub fn into_parts(self) -> (Vec<String>, Vec<Need>, Vec<String>) {
        (
            self.missed_specs.into_inner(),
            self.missed.into_inner(),
            self.looked_up.into_inner(),
        )
    }
}

impl Resolved for View<'_> {
    fn spec(&self, name: &str) -> Option<Arc<LoadedSpec>> {
        self.looked_up.borrow_mut().push(name.to_owned());
        match self.specs.get(name) {
            Some(SpecState::Ready(spec)) => Some(Arc::clone(spec)),
            _ => {
                self.missed_specs.borrow_mut().push(name.to_owned());
                None
            }
        }
    }

    fn answer(&self, need: &Need) -> Option<Arc<Answer>> {
        match self.needs.lookup(need) {
            Lookup::Answered(answer) => Some(answer),
            Lookup::Provisional(answer) => {
                self.missed.borrow_mut().push(need.clone());
                Some(answer)
            }
            Lookup::Missing => {
                self.missed.borrow_mut().push(need.clone());
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::specs::runtime::tests::{TEST_BUDGET, wait_readable};
    use crate::specs::runtime::{Outcome, SpecThread};

    fn generate(tokens: &[&str]) -> Need {
        Need::GenerateSpec {
            spec: "git".to_owned(),
            fn_id: 3,
            tokens: tokens.iter().map(|token| (*token).to_owned()).collect(),
        }
    }

    #[test]
    fn generate_spec_is_kept_per_line_but_its_last_token() {
        let mut needs = Needs::default();
        assert_eq!(
            needs.lookup(&generate(&["git", "c"])),
            Lookup::Provisional(Arc::new(Answer::Failed))
        );
        let spec = Answer::Spec(Subcommand {
            name: vec!["git".to_owned()],
            ..Subcommand::default()
        });
        needs.insert(Needs::key(&generate(&["git", "c"])), spec.clone());
        assert_eq!(
            needs.lookup(&generate(&["git", "ch"])),
            Lookup::Answered(Arc::new(spec.clone()))
        );
        // Another line: the latest answer of the function stands in.
        assert_eq!(
            needs.lookup(&generate(&["git", "x", "y"])),
            Lookup::Provisional(Arc::new(spec))
        );
        let alias = Need::Alias {
            spec: "git".to_owned(),
            fn_id: 4,
            token: "co".to_owned(),
        };
        assert_eq!(needs.lookup(&alias), Lookup::Missing);
    }

    #[test]
    fn the_oldest_answer_goes_first() {
        let mut needs = Needs::default();
        let alias = |n: usize| Need::Alias {
            spec: "git".to_owned(),
            fn_id: 4,
            token: format!("a{n}"),
        };
        for n in 0..=MAX_ANSWERS {
            needs.insert(alias(n), Answer::Failed);
        }
        assert_eq!(needs.len(), MAX_ANSWERS);
        assert_eq!(needs.lookup(&alias(0)), Lookup::Missing);
        assert!(matches!(
            needs.lookup(&alias(MAX_ANSWERS)),
            Lookup::Answered(_)
        ));
    }

    #[test]
    fn results_convert_as_fig_converts_them() {
        let load = Need::LoadSpecFn {
            spec: "x".to_owned(),
            fn_id: 0,
            token: "t".to_owned(),
        };
        let named = |name: &str| {
            Some(Answer::Spec(Subcommand {
                load_spec: Some(LoadSpec::Name(name.to_owned())),
                ..Subcommand::default()
            }))
        };
        let ok = |json: &str| Ok(json.to_owned());
        assert_eq!(
            convert(&load, ok(r#"[{"name":"aws/s3","type":"global"}]"#)),
            named("aws/s3")
        );
        assert_eq!(
            convert(&load, ok(r#"{"name":"aws/s3","type":"global"}"#)),
            named("aws/s3")
        );
        assert_eq!(
            convert(&load, ok(r#"{"name":"run","type":"local"}"#)),
            Some(Answer::Failed)
        );
        assert_eq!(convert(&load, ok(r#""aws/s3""#)), Some(Answer::Failed));
        assert_eq!(convert(&load, ok("[]")), Some(Answer::Failed));
        match convert(
            &load,
            ok(r#"{"name":"inline","subcommands":[{"name":"a"}]}"#),
        ) {
            Some(Answer::Spec(sub)) => {
                assert_eq!(sub.name, ["inline"]);
                assert_eq!(sub.subcommands.len(), 1);
            }
            other => panic!("{other:?}"),
        }
        let generate = generate(&["git", ""]);
        assert!(matches!(
            convert(&generate, ok(r#"{"name":"git","subcommands":[{"name":"x"}]}"#)),
            Some(Answer::Spec(sub)) if sub.subcommands.len() == 1
        ));
        assert_eq!(convert(&generate, ok("null")), Some(Answer::Failed));
        let alias = Need::Alias {
            spec: "git".to_owned(),
            fn_id: 4,
            token: "co".to_owned(),
        };
        assert_eq!(
            convert(&alias, ok(r#""checkout -b""#)),
            Some(Answer::Alias(vec!["checkout".to_owned(), "-b".to_owned()]))
        );
        assert_eq!(convert(&alias, ok("3")), Some(Answer::Failed));
        assert_eq!(
            convert(&alias, Err(CallError::Timeout)),
            Some(Answer::Failed)
        );
        assert_eq!(convert(&alias, Err(CallError::Stopped)), None);
    }

    /// An answer pushed out of the map (the oldest, or replaced) hands its
    /// walked function ids back for release, unless it is still the latest
    /// answer of its `generateSpec`.
    #[test]
    fn answers_let_go_of_hand_back_their_functions() {
        let with_fn = |id: u32| {
            Answer::Spec(
                serde_json::from_str(&format!(
                    r#"{{"name":"git","subcommands":[{{"name":"ext","args":{{"generators":[{{"custom":{{"$fn":{id}}}}},{{"postProcess":{{"$fn":3}}}}]}}}}]}}"#
                ))
                .expect("a spec"),
            )
        };
        let alias = |at: u32| Need::Alias {
            spec: "git".to_owned(),
            fn_id: 4,
            token: format!("t{at}"),
        };
        let generate = |word: &str| Need::GenerateSpec {
            spec: "git".to_owned(),
            fn_id: 3,
            tokens: vec!["git".to_owned(), word.to_owned()],
        };
        let mut needs = Needs::default();
        let first = WALKED_FIRST + 1000;
        assert!(needs.answer_with(&alias(0), with_fn(first)).is_empty());
        for at in 1..MAX_ANSWERS as u32 {
            assert!(needs.answer_with(&alias(at), Answer::Failed).is_empty());
        }
        // The 257th pushes out the first, and its function goes (the
        // spec's own, 3, does not).
        assert_eq!(
            needs.answer_with(&alias(999), Answer::Failed),
            [("git".to_owned(), vec![first])]
        );
        assert_eq!(needs.len(), MAX_ANSWERS);
        // Replaced under its own key.
        assert!(needs.answer_with(&alias(5), with_fn(first + 1)).is_empty());
        assert_eq!(
            needs.answer_with(&alias(5), Answer::Failed),
            [("git".to_owned(), vec![first + 1])]
        );
        // A `generateSpec` answer stays while it is the latest of its
        // function, even once its key is gone.
        let mut needs = Needs::default();
        needs.answer_with(&generate("a"), with_fn(first + 2));
        for at in 0..MAX_ANSWERS as u32 {
            assert!(needs.answer_with(&alias(at), Answer::Failed).is_empty());
        }
        assert!(matches!(
            needs.lookup(&generate("b")),
            Lookup::Provisional(answer) if *answer == with_fn(first + 2)
        ));
        assert_eq!(
            needs.answer_with(&generate("c"), with_fn(first + 3)),
            [("git".to_owned(), vec![first + 2])]
        );
    }

    #[test]
    fn a_need_is_asked_once_and_answered() {
        let mut cache = SpecCache::with_thread(Some(
            SpecThread::start_with_budget(None, TEST_BUDGET).expect("spec thread"),
        ));
        let deadline = Instant::now() + Duration::from_secs(60);
        while !matches!(cache.get("git"), SpecState::Ready(_)) {
            assert!(Instant::now() < deadline, "git did not load");
            wait_readable(cache.result_fd().expect("a live thread"));
            let outcomes = cache.take();
            cache.on_outcomes(outcomes);
        }
        let SpecState::Ready(git) = cache.get("git") else {
            panic!("git is not ready");
        };
        let fn_id = git.root.generate_spec.expect("git's generateSpec").0;
        let need = Need::GenerateSpec {
            spec: "git".to_owned(),
            fn_id,
            tokens: vec!["git".to_owned(), "ch".to_owned()],
        };
        let env: Env = Arc::new(vec![(
            "PATH".to_owned(),
            "/usr/bin:/bin:/usr/sbin:/sbin".to_owned(),
        )]);
        let cwd = std::env::temp_dir();
        let mut needs = Needs::default();
        assert!(needs.request(&need, git.generation, &mut cache, &cwd, &env));
        assert!(
            !needs.request(&need, git.generation, &mut cache, &cwd, &env),
            "asked once"
        );
        let answered = loop {
            assert!(Instant::now() < deadline, "no answer");
            wait_readable(cache.result_fd().expect("a live thread"));
            let outcomes = cache.take();
            cache.on_outcomes(outcomes);
            let answered = needs.take_answers(&mut cache);
            if !answered.is_empty() {
                break answered;
            }
        };
        assert_eq!(answered, [Needs::key(&need)]);
        assert!(matches!(
            needs.lookup(&need),
            Lookup::Answered(answer) if matches!(&*answer, Answer::Spec(sub) if sub.name == ["git"])
        ));
        assert!(
            !needs.request(&need, git.generation, &mut cache, &cwd, &env),
            "answered"
        );
    }

    /// A spec loaded again: its answers that hold functions, and its calls
    /// in flight, are forgotten, so the next parse asks again; answers
    /// without functions stay.
    #[test]
    fn a_reload_forgets_the_answers_with_functions() {
        let mut cache = SpecCache::with_thread(Some(
            SpecThread::start_with_budget(None, TEST_BUDGET).expect("spec thread"),
        ));
        let deadline = Instant::now() + Duration::from_secs(60);
        let git = loop {
            if let SpecState::Ready(git) = cache.get("git") {
                break git;
            }
            assert!(Instant::now() < deadline, "git did not load");
            wait_readable(cache.result_fd().expect("a live thread"));
            let outcomes = cache.take();
            cache.on_outcomes(outcomes);
        };
        cache.take_reloaded();
        let reload = |cache: &mut SpecCache| {
            // What the spec thread sends when it loads the spec again.
            let again = Arc::new(LoadedSpec {
                name: git.name.clone(),
                root: git.root.clone(),
                fns: git.fns.clone(),
                has_js: git.has_js,
                version: None,
                load_time: Duration::ZERO,
                generation: git.generation + 1,
            });
            cache.on_outcomes(vec![Outcome::Loaded {
                name: "git".to_owned(),
                result: Ok(again),
            }]);
        };
        let fn_id = git.root.generate_spec.expect("git's generateSpec").0;
        let generate = |word: &str| Need::GenerateSpec {
            spec: "git".to_owned(),
            fn_id,
            tokens: vec!["git".to_owned(), word.to_owned()],
        };
        let alias = Need::Alias {
            spec: "git".to_owned(),
            fn_id: 4,
            token: "co".to_owned(),
        };
        let with_fn: Subcommand = serde_json::from_str(
            r#"{"name":"git","subcommands":[{"name":"ext","args":{"generators":{"custom":{"$fn":16777216}}}}]}"#,
        )
        .expect("a spec");
        let without_fn: Subcommand =
            serde_json::from_str(r#"{"name":"git","subcommands":[{"name":"ext"}]}"#)
                .expect("a spec");
        let env: Env = Arc::new(vec![(
            "PATH".to_owned(),
            "/usr/bin:/bin:/usr/sbin:/sbin".to_owned(),
        )]);
        let cwd = std::env::temp_dir();

        let mut needs = Needs::default();
        needs.answer_with(&generate("a"), Answer::Spec(with_fn.clone()));
        needs.answer_with(&alias, Answer::Alias(vec!["checkout".to_owned()]));
        let other = Need::GenerateSpec {
            spec: "other".to_owned(),
            fn_id,
            tokens: vec!["other".to_owned(), String::new()],
        };
        needs.answer_with(&other, Answer::Spec(with_fn.clone()));
        assert!(matches!(needs.lookup(&generate("a")), Lookup::Answered(_)));
        reload(&mut cache);
        assert!(needs.take_answers(&mut cache).is_empty());
        // Asked again; meanwhile the stale answer does not stand in.
        assert_eq!(
            needs.lookup(&generate("a")),
            Lookup::Provisional(Arc::new(Answer::Failed))
        );
        assert!(needs.request(&generate("a"), git.generation, &mut cache, &cwd, &env));
        assert!(matches!(needs.lookup(&alias), Lookup::Answered(_)));
        assert!(matches!(needs.lookup(&other), Lookup::Answered(_)));

        // An answer without functions stays across a reload.
        let mut needs = Needs::default();
        needs.answer_with(&generate("b"), Answer::Spec(without_fn));
        reload(&mut cache);
        needs.take_answers(&mut cache);
        assert!(matches!(needs.lookup(&generate("b")), Lookup::Answered(_)));

        // A call in flight across a reload: its answer is not taken, and
        // the need is asked again.
        let mut needs = Needs::default();
        let need = generate("c");
        assert!(needs.request(&need, git.generation, &mut cache, &cwd, &env));
        reload(&mut cache);
        assert!(needs.take_answers(&mut cache).is_empty());
        // Both calls answer: the first `generate("a")` request's, then this
        // one's.
        let mut called = 0;
        while called < 2 {
            assert!(Instant::now() < deadline, "no answer");
            wait_readable(cache.result_fd().expect("a live thread"));
            let outcomes = cache.take();
            called += outcomes
                .iter()
                .filter(|outcome| matches!(outcome, Outcome::Called { .. }))
                .count();
            cache.on_outcomes(outcomes);
            assert!(needs.take_answers(&mut cache).is_empty());
        }
        assert!(!matches!(needs.lookup(&need), Lookup::Answered(_)));
        assert!(
            needs.request(&need, git.generation, &mut cache, &cwd, &env),
            "asked again"
        );
    }
}
