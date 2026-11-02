//! Fig's generator rules, ported from `packages/autocomplete/src/generators/`
//! in aws/amazon-q-developer-cli-autocomplete (v1.19.7): what a script
//! generator runs, how its output becomes suggestions, what a custom
//! generator's result becomes, and the generator cache. Line references are
//! to those files. The spec thread (`runtime.rs`) runs the processes and the
//! JavaScript; everything here is pure.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Map, Value};

use super::model::{
    Arg, Cache, CacheStrategy, ExecuteCommandInput, LoadSpec, Subcommand, Suggestion,
};
use super::process::Output;

/// Why a generator gave no suggestions. Fig catches every one of these and
/// shows an empty list (`scriptSuggestionsGenerator.ts:88-91`,
/// `customSuggestionsGenerator.ts:52-56`); so should the poll thread.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GenError {
    #[error("the generator ran out of time")]
    Timeout,
    #[error("the command did not start: {0}")]
    Spawn(String),
    #[error("javascript: {0}")]
    Js(String),
    /// The generator's spec was dropped or reloaded while it ran.
    #[error("the generator was cancelled")]
    Cancelled,
}

/// The arg at `path` in a spec's tree: a path as [`CurrentArg::path`] gives
/// it, less its `spec:` prefix (`subcommands.3.args.0`, `options.2.args`,
/// `subcommands.1.loadSpec.args.0`). Args that came from a function's
/// answer (a path with a call in it) are not in the tree.
///
/// [`CurrentArg::path`]: super::parser::CurrentArg::path
pub fn arg_at<'a>(root: &'a Subcommand, path: &str) -> Option<&'a Arg> {
    if path.contains('(') {
        return None;
    }
    enum At<'a> {
        Sub(&'a Subcommand),
        Opt(&'a [Arg]),
        Arg(&'a Arg),
    }
    let mut at = At::Sub(root);
    let mut parts = path.split('.').filter(|part| !part.is_empty()).peekable();
    while let Some(part) = parts.next() {
        let index = |parts: &mut std::iter::Peekable<_>| -> Option<usize> {
            parts.next().and_then(|n: &str| n.parse().ok())
        };
        at = match (at, part) {
            (At::Sub(sub), "subcommands") => At::Sub(sub.subcommands.get(index(&mut parts)?)?),
            (At::Sub(sub), "options") => At::Opt(&sub.options.get(index(&mut parts)?)?.args),
            (At::Sub(sub), "args") => args_at(&sub.args, &mut parts)?,
            (At::Opt(args), "args") => args_at(args, &mut parts)?,
            (At::Sub(sub), "loadSpec") => match &sub.load_spec {
                Some(LoadSpec::Inline(inline)) => At::Sub(inline),
                _ => return None,
            },
            (At::Arg(arg), "loadSpec") => match &arg.load_spec {
                Some(LoadSpec::Inline(inline)) => At::Sub(inline),
                _ => return None,
            },
            _ => return None,
        };
    }
    fn args_at<'a, 'p>(
        args: &'a [Arg],
        parts: &mut std::iter::Peekable<impl Iterator<Item = &'p str>>,
    ) -> Option<At<'a>> {
        // A single args object has no index segment.
        match parts.peek().and_then(|part| part.parse::<usize>().ok()) {
            Some(index) => {
                parts.next();
                args.get(index).map(At::Arg)
            }
            None => args.first().map(At::Arg),
        }
    }
    match at {
        At::Arg(arg) => Some(arg),
        _ => None,
    }
}

/// What a script generator runs: Fig's `ExecuteCommandInput` once built
/// (`scriptSuggestionsGenerator.ts:38-50`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptInput {
    pub command: String,
    pub args: Vec<String>,
    /// Relative to the shell's cwd when relative.
    pub cwd: String,
    pub env: BTreeMap<String, String>,
    pub timeout: Option<u64>,
    /// `JSON.stringify(executeCommandInput)`, the cache key when the
    /// generator gives none. Fig's key order is kept as far as the model
    /// knows it: an argv gives `{command, args, cwd}`, an object
    /// `{cwd, ...object}`.
    pub key: String,
}

/// A script given as an object, or returned as one by a script function.
#[derive(Debug, Deserialize)]
struct CommandObject {
    command: String,
    args: Vec<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    timeout: Option<f64>,
}

impl ScriptInput {
    /// The argv form (`scriptSuggestionsGenerator.ts:39-44`). An empty
    /// argv runs nothing: Fig's command is `undefined` and its run fails.
    pub fn from_argv(argv: &[String], cwd: &str) -> Option<Self> {
        let (command, args) = argv.split_first()?;
        let key = serde_json::json!({"command": command, "args": args, "cwd": cwd});
        let key = ordered(&key, &["command", "args", "cwd"]);
        Some(Self {
            command: command.clone(),
            args: args.to_vec(),
            cwd: cwd.to_owned(),
            env: BTreeMap::new(),
            timeout: None,
            key,
        })
    }

    /// The object form, `{cwd, ...object}` (`scriptSuggestionsGenerator.ts:46-49`).
    pub fn from_object(input: &ExecuteCommandInput, cwd: &str) -> Self {
        let cwd = input.cwd.clone().unwrap_or_else(|| cwd.to_owned());
        let mut key = Map::new();
        key.insert("cwd".to_owned(), Value::from(cwd.as_str()));
        key.insert("command".to_owned(), Value::from(input.command.as_str()));
        key.insert("args".to_owned(), Value::from(input.args.clone()));
        if !input.env.is_empty() {
            key.insert("env".to_owned(), serde_json::json!(input.env));
        }
        if let Some(timeout) = input.timeout {
            key.insert("timeout".to_owned(), Value::from(timeout));
        }
        let key = ordered(
            &Value::Object(key),
            &["cwd", "command", "args", "env", "timeout"],
        );
        Self {
            command: input.command.clone(),
            args: input.args.clone(),
            cwd,
            env: input.env.clone(),
            timeout: input.timeout,
            key,
        }
    }

    /// What a script function returned, as JSON. `Ok(None)` for a falsy
    /// value, which runs nothing (`scriptSuggestionsGenerator.ts:34-36`); an
    /// error for anything Fig's run would throw on (a promise, a string, an
    /// object without `command` and `args`).
    pub fn from_returned(value: &Value, cwd: &str) -> Result<Option<Self>, GenError> {
        if !truthy(value) {
            return Ok(None);
        }
        let bad = || GenError::Js("the script function returned no command".to_owned());
        match value {
            Value::Array(items) => {
                let argv = items
                    .iter()
                    .map(|item| item.as_str().map(str::to_owned))
                    .collect::<Option<Vec<_>>>()
                    .ok_or_else(bad)?;
                Self::from_argv(&argv, cwd).map(Some).ok_or_else(bad)
            }
            Value::Object(_) => {
                let object: CommandObject =
                    serde_json::from_value(value.clone()).map_err(|_| bad())?;
                let input = ExecuteCommandInput {
                    command: object.command,
                    args: object.args,
                    cwd: object.cwd,
                    env: object.env,
                    timeout: object.timeout.map(millis),
                };
                Ok(Some(Self::from_object(&input, cwd)))
            }
            _ => Err(bad()),
        }
    }
}

/// A JavaScript number of milliseconds as whole milliseconds.
pub fn millis(ms: f64) -> u64 {
    if ms.is_finite() && ms > 0.0 {
        // Saturates; a timeout of 2^64 ms is no timeout.
        ms.ceil() as u64
    } else {
        0
    }
}

/// An object's JSON with its keys in `order` (serde_json sorts them).
fn ordered(value: &Value, order: &[&str]) -> String {
    let Value::Object(map) = value else {
        return value.to_string();
    };
    let fields: Vec<String> = order
        .iter()
        .filter_map(|key| {
            let value = map.get(*key)?;
            Some(format!("{}:{value}", Value::from(*key)))
        })
        .collect();
    format!("{{{}}}", fields.join(","))
}

/// The longest of the setting, the generator's `scriptTimeout` and the
/// input's own `timeout` (`scriptSuggestionsGenerator.ts:52-57`).
pub fn script_timeout(
    default: Duration,
    script_timeout: Option<u64>,
    input: Option<u64>,
) -> Duration {
    let longest = script_timeout.unwrap_or(0).max(input.unwrap_or(0));
    default.max(Duration::from_millis(longest))
}

/// JavaScript truthiness of a JSON value.
pub fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|n| n != 0.0 && !n.is_nan()),
        Value::String(text) => !text.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// `String.prototype.trim`: JavaScript's white space and line terminators.
pub fn js_trim(text: &str) -> &str {
    text.trim_matches(|c: char| (c.is_whitespace() && c != '\u{85}') || c == '\u{feff}')
}

/// A plain string from a script, as Fig converts it
/// (`scriptSuggestionsGenerator.ts:83-85`): it is its own insert value. A
/// string with a control character in it (a newline, a tab, ESC) is data
/// that would be typed into the line as it is, and run it at a newline: it
/// is left out.
fn string_suggestion(name: &str, is_dangerous: bool) -> Option<Suggestion> {
    if name.chars().any(char::is_control) {
        return None;
    }
    Some(Suggestion {
        name: vec![name.to_owned()],
        kind: Some(super::model::SuggestionType::Arg),
        insert_value: Some(name.to_owned()),
        is_dangerous,
        ..Suggestion::default()
    })
}

/// An object item, `{...item, type: item.type || "arg"}`, read with the
/// model's JSON form. An item the model cannot read is dropped.
fn object_suggestion(mut item: Map<String, Value>) -> Option<Suggestion> {
    if !item.get("type").is_some_and(truthy) {
        item.insert("type".to_owned(), Value::from("arg"));
    }
    serde_json::from_value(Value::Object(item))
        .inspect_err(
            |err| tracing::debug!(target: "specs", "a generated suggestion was dropped: {err}"),
        )
        .ok()
}

/// `splitOn` (a non-empty one: an empty string is falsy, and Fig goes on to
/// `postProcess`): the trimmed stdout split on it, or nothing when it is
/// blank (`scriptSuggestionsGenerator.ts:69-70`). Every piece is kept, empty
/// ones too, but one with a control character ([`string_suggestion`]).
pub fn split_on(stdout: &str, separator: &str, is_dangerous: bool) -> Vec<Suggestion> {
    let trimmed = js_trim(stdout);
    if trimmed.is_empty() {
        return Vec::new();
    }
    trimmed
        .split(separator)
        .filter_map(|piece| string_suggestion(piece, is_dangerous))
        .collect()
}

/// What `postProcess` returned: it must be an array (Fig calls `.filter` on
/// it); falsy items, and objects whose `name` is falsy, are dropped; strings
/// are converted (`scriptSuggestionsGenerator.ts:71-87`), but for one with
/// a control character ([`string_suggestion`]).
pub fn post_processed(result: Value, is_dangerous: bool) -> Result<Vec<Suggestion>, GenError> {
    let Value::Array(items) = result else {
        return Err(GenError::Js(
            "postProcess did not return an array".to_owned(),
        ));
    };
    Ok(items
        .into_iter()
        .filter_map(|item| match item {
            Value::String(name) if !name.is_empty() => string_suggestion(&name, is_dangerous),
            Value::Object(object) if object.get("name").is_some_and(truthy) => {
                object_suggestion(object)
            }
            _ => None,
        })
        .collect())
}

/// What a custom generator returned, mapped as Fig maps it:
/// `result.map((name) => ({...name, type: name.type || "arg"}))`
/// (`customSuggestionsGenerator.ts:51`). Not an array, or a null item
/// (`name.type` throws), fails the whole result. A string, number or array
/// item spreads into an object with no name.
pub fn custom_items(result: Value) -> Result<Vec<Value>, GenError> {
    let Value::Array(items) = result else {
        return Err(GenError::Js(
            "the custom generator did not return an array".to_owned(),
        ));
    };
    items
        .into_iter()
        .map(|item| {
            let mut object = match item {
                Value::Null => {
                    return Err(GenError::Js(
                        "the custom generator returned a null item".to_owned(),
                    ));
                }
                Value::Object(object) => object,
                _ => Map::new(),
            };
            if !object.get("type").is_some_and(truthy) {
                object.insert("type".to_owned(), Value::from("arg"));
            }
            Ok(Value::Object(object))
        })
        .collect()
}

/// Fig's `isTemplateSuggestion`: an item with its own `context`
/// (`suggestions/index.ts:292-295`). A custom generator's result goes
/// through `filterTemplateSuggestions` when its first item is one
/// (`state/generators.ts:96-113`).
pub fn is_template_suggestion(item: &Value) -> bool {
    item.as_object()
        .is_some_and(|object| object.contains_key("context"))
}

/// Model suggestions from mapped items; an item the model cannot read is
/// dropped.
pub fn suggestions(items: Vec<Value>) -> Vec<Suggestion> {
    items
        .into_iter()
        .filter_map(|item| match item {
            Value::Object(object) => serde_json::from_value(Value::Object(object))
                .inspect_err(|err| {
                    tracing::debug!(target: "specs", "a generated suggestion was dropped: {err}");
                })
                .ok(),
            _ => None,
        })
        .collect()
}

/// A suggestion as Fig's code sees one (for `filterTemplateSuggestions`):
/// a single name is a string, and absent fields are left out.
pub fn fig_suggestion(suggestion: &Suggestion) -> Value {
    let mut value = serde_json::to_value(suggestion).unwrap_or(Value::Null);
    if let Value::Object(object) = &mut value {
        object.retain(|_, field| !field.is_null());
        if let Some(Value::Array(names)) = object.get("name") {
            match names.as_slice() {
                [] => {
                    object.remove("name");
                }
                [one] => {
                    let one = one.clone();
                    object.insert("name".to_owned(), one);
                }
                _ => {}
            }
        }
    }
    value
}

/// `getCWDForFilesAndFolders` (`shared/src/utils.ts:187-201`).
pub fn cwd_for_files_and_folders(cwd: &str, search_term: &str) -> String {
    let dirname = &search_term[..search_term.rfind('/').map_or(0, |at| at + 1)];
    if dirname.is_empty() {
        return if cwd.ends_with('/') {
            cwd.to_owned()
        } else {
            format!("{cwd}/")
        };
    }
    if dirname.starts_with("~/") || dirname.starts_with('/') {
        dirname.to_owned()
    } else {
        format!("{cwd}/{dirname}")
    }
}

/// The cache key (`helpers.ts:45-55`): the directory when `cacheByDirectory`
/// (the files-and-folders directory for a generator with a template), and
/// the given key or else the tokens, joined as `Array.toString` joins them.
pub fn cache_key(
    cache: &Cache,
    has_template: bool,
    cwd: &str,
    search_term: &str,
    key: Option<&str>,
    tokens: &[String],
) -> String {
    let directory = if !cache.cache_by_directory {
        String::new()
    } else if has_template {
        cwd_for_files_and_folders(cwd, search_term)
    } else {
        cwd.to_owned()
    };
    let key = match key {
        Some(key) if !key.is_empty() => key.to_owned(),
        _ => tokens.join(" "),
    };
    format!("{directory},{key}")
}

/// What the generator cache holds: a script's output (post-processing runs
/// on every use, as in Fig) or a custom generator's result as JSON.
#[derive(Debug, Clone, PartialEq)]
pub enum Cached {
    Output(Output),
    Json(String),
}

impl Cached {
    /// The bytes it holds.
    fn size(&self) -> usize {
        match self {
            Cached::Output(output) => output.stdout.len() + output.stderr.len(),
            Cached::Json(json) => json.len(),
        }
    }
}

/// The most keys [`GenCache`] holds.
pub const CACHE_ENTRIES: usize = 128;
/// The most bytes of values [`GenCache`] holds.
pub const CACHE_BYTES: usize = 16 << 20;

/// What to do with a generator that has a `cache`.
#[derive(Debug, Clone, PartialEq)]
pub enum Plan {
    /// Use this value; nothing runs.
    Hit(Cached),
    /// Run, and use what it gives.
    Run,
    /// Wait for the fetch already running for this key.
    Join(u64),
    /// Use this stale value now, run, and use the fresh value too.
    StaleRun(Cached),
    /// Use this stale value now, and the running fetch's value too.
    StaleJoin(Cached, u64),
}

#[derive(Debug, Default)]
struct Entry {
    value: Option<Cached>,
    /// When the last fetch started (Fig sets `lastFetch` before it awaits).
    last_fetch: Option<Instant>,
    /// The fetch running for this key.
    fetching: Option<u64>,
    /// When it was last used, on [`GenCache::clock`].
    used: u64,
}

/// Fig's generator cache (`cache.ts`), keyed as [`cache_key`]. It lives for
/// the session, like Fig's `generatorCache`, but holds at most
/// [`CACHE_ENTRIES`] keys and [`CACHE_BYTES`] of values: past either, the
/// key used longest ago goes first (never one being fetched).
#[derive(Debug, Default)]
pub struct GenCache {
    entries: HashMap<String, Entry>,
    /// The bytes of the values held.
    bytes: usize,
    /// Counts the uses.
    clock: u64,
}

impl GenCache {
    /// `CacheEntry.entry` (`cache.ts:45-56`). A first use runs (or joins the
    /// fetch already running). `max-age` uses the value until `ttl` has
    /// passed since the last fetch started; with no `ttl` it never expires.
    /// `stale-while-revalidate` (the default) treats the value as stale once
    /// `ttl` (default 0) has passed, and then gives it at once while a fresh
    /// one is fetched; Fig's UI keeps old results on screen while a
    /// generator reloads, and the second answer is that reload.
    pub fn plan(&mut self, key: &str, cache: &Cache, now: Instant) -> Plan {
        self.touch(key);
        self.evict(key);
        let Some(entry) = self.entries.get(key) else {
            return Plan::Run;
        };
        let Some(value) = &entry.value else {
            return entry.fetching.map_or(Plan::Run, Plan::Join);
        };
        let ttl = cache.ttl.map(Duration::from_millis);
        let since = entry.last_fetch.map(|at| now.saturating_duration_since(at));
        match cache
            .strategy
            .unwrap_or(CacheStrategy::StaleWhileRevalidate)
        {
            CacheStrategy::MaxAge => {
                let stale = matches!((ttl, since), (Some(ttl), Some(since)) if since > ttl);
                match (stale, entry.fetching) {
                    (false, _) => Plan::Hit(value.clone()),
                    (true, Some(fetch)) => Plan::Join(fetch),
                    (true, None) => Plan::Run,
                }
            }
            CacheStrategy::StaleWhileRevalidate => {
                let max_age = ttl.unwrap_or(Duration::ZERO);
                match entry.fetching {
                    Some(fetch) => Plan::StaleJoin(value.clone(), fetch),
                    None if since.is_none_or(|since| since > max_age) => {
                        Plan::StaleRun(value.clone())
                    }
                    None => Plan::Hit(value.clone()),
                }
            }
        }
    }

    /// Fetch `fetch` started for `key`.
    pub fn started(&mut self, key: &str, fetch: u64, now: Instant) {
        let entry = self.touch(key);
        entry.last_fetch = Some(now);
        entry.fetching = Some(fetch);
        self.evict(key);
    }

    /// Fetch `fetch` for `key` ended: with a value to keep, or none (it
    /// failed or was cancelled). Fig keeps a failed fetch's rejected promise
    /// and so fails every later use of the key; here the next use runs
    /// again.
    pub fn finished(&mut self, key: &str, fetch: u64, value: Option<Cached>) {
        let Some(entry) = self.entries.get_mut(key) else {
            return;
        };
        if entry.fetching == Some(fetch) {
            entry.fetching = None;
        }
        if let Some(value) = value {
            self.bytes += value.size();
            if let Some(old) = entry.value.replace(value) {
                self.bytes = self.bytes.saturating_sub(old.size());
            }
        }
        self.evict(key);
    }

    /// `key`'s entry, made if need be, marked as used now.
    fn touch(&mut self, key: &str) -> &mut Entry {
        self.clock += 1;
        let clock = self.clock;
        let entry = self.entries.entry(key.to_owned()).or_default();
        entry.used = clock;
        entry
    }

    /// Drop the keys used longest ago (not `keep`, and none being fetched)
    /// until the cache is within its caps.
    fn evict(&mut self, keep: &str) {
        while self.entries.len() > CACHE_ENTRIES || self.bytes > CACHE_BYTES {
            let oldest = self
                .entries
                .iter()
                .filter(|(key, entry)| key.as_str() != keep && entry.fetching.is_none())
                .min_by_key(|(_, entry)| entry.used)
                .map(|(key, _)| key.clone());
            let Some(entry) = oldest.and_then(|key| self.entries.remove(&key)) else {
                break;
            };
            self.bytes = self
                .bytes
                .saturating_sub(entry.value.as_ref().map_or(0, Cached::size));
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }
}

/// `semver.clean` (semver 7, non-loose): trim, drop leading `=` and `v`,
/// and accept only a full `major.minor.patch[-pre][+build]` with no leading
/// zeros; the clean form drops the build. Fig runs it on `<cmd> --version`
/// (`autocomplete-parser/src/loadHelpers.ts:173-180`).
pub fn semver_clean(version: &str) -> Option<String> {
    let version = js_trim(version).trim_start_matches(['=', 'v']);
    let version = js_trim(version);
    if version.len() > 256 {
        return None;
    }
    let version = version.strip_prefix('v').unwrap_or(version);
    let (rest, build) = match version.split_once('+') {
        Some((rest, build)) => (rest, Some(build)),
        None => (version, None),
    };
    if let Some(build) = build
        && !build.split('.').all(|part| {
            !part.is_empty() && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
    {
        return None;
    }
    let (core, pre) = match rest.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (rest, None),
    };
    let numeric = |part: &str| {
        !part.is_empty()
            && part.bytes().all(|b| b.is_ascii_digit())
            && (part == "0" || !part.starts_with('0'))
    };
    let parts: Vec<&str> = core.split('.').collect();
    if parts.len() != 3 || !parts.iter().all(|part| numeric(part)) {
        return None;
    }
    // MAX_SAFE_INTEGER.
    if parts
        .iter()
        .any(|part| part.parse::<u64>().map_or(true, |n| n > (1 << 53) - 1))
    {
        return None;
    }
    if let Some(pre) = pre {
        let identifier = |part: &str| {
            numeric(part)
                || (!part.is_empty()
                    && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                    && part.bytes().any(|b| !b.is_ascii_digit()))
        };
        if !pre.split('.').all(identifier) {
            return None;
        }
        return Some(format!("{core}-{pre}"));
    }
    Some(core.to_owned())
}

/// Whether `path` is usable as a working directory string for Fig: Fig's
/// `haveContextForGenerator` wants a non-empty cwd (`helpers.ts:25-26`).
pub fn have_context(cwd: &Path) -> bool {
    !cwd.as_os_str().is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::specs::model::SuggestionType;

    fn names(suggestions: &[Suggestion]) -> Vec<&str> {
        suggestions
            .iter()
            .map(|s| s.name.first().map_or("", String::as_str))
            .collect()
    }

    #[test]
    fn semver_clean_is_semvers() {
        for (input, want) in [
            ("1.2.3", Some("1.2.3")),
            ("  v1.2.3  ", Some("1.2.3")),
            ("=v1.2.3", Some("1.2.3")),
            ("1.2.3-beta.1+build.5", Some("1.2.3-beta.1")),
            ("1.2", None),
            ("01.2.3", None),
            ("1.2.3-01", None),
            ("1.2.3-0a", Some("1.2.3-0a")),
            ("git version 2.39.3", None),
            ("2.53.0\n", Some("2.53.0")),
            ("1.2.3.4", None),
            ("", None),
        ] {
            assert_eq!(semver_clean(input).as_deref(), want, "{input:?}");
        }
    }

    #[test]
    fn arg_paths_find_args() {
        let root: Subcommand = serde_json::from_str(
            r#"{"name": "x", "args": {"name": "root"}, "subcommands": [
                {"name": "a"},
                {"name": "b", "args": [{"name": "b0"}, {"name": "b1"}],
                 "options": [{"name": "-o", "args": {"name": "o"}}],
                 "loadSpec": {"name": "inner", "args": {"name": "in"}}}
            ]}"#,
        )
        .expect("spec");
        let name = |path: &str| arg_at(&root, path).and_then(|arg| arg.name.clone());
        assert_eq!(name("args").as_deref(), Some("root"));
        assert_eq!(name("args.0").as_deref(), Some("root"));
        assert_eq!(name("subcommands.1.args.1").as_deref(), Some("b1"));
        assert_eq!(name("subcommands.1.options.0.args").as_deref(), Some("o"));
        assert_eq!(name("subcommands.1.loadSpec.args").as_deref(), Some("in"));
        assert_eq!(name("subcommands.0.args"), None);
        assert_eq!(name("subcommands.1"), None);
        assert_eq!(name(r#"generateSpec(["x",""]).args.0"#), None);
    }

    #[test]
    fn split_on_and_post_process_follow_fig() {
        let split = split_on("  a\nb\n\nc \n", "\n", true);
        assert_eq!(names(&split), ["a", "b", "", "c"]);
        assert!(split[0].is_dangerous);
        assert_eq!(split[0].kind, Some(SuggestionType::Arg));
        assert_eq!(split[0].insert_value.as_deref(), Some("a"));
        assert!(split_on(" \n ", "\n", false).is_empty());

        let processed = post_processed(
            serde_json::json!(["x", "", null, 5, {"name": ""}, {"description": "no name"},
                {"name": "y", "type": "folder"}, {"name": ["z", "zz"]}]),
            false,
        )
        .expect("an array");
        assert_eq!(names(&processed), ["x", "y", "z"]);
        assert_eq!(processed[1].kind, Some(SuggestionType::Folder));
        assert_eq!(processed[2].kind, Some(SuggestionType::Arg));
        assert!(post_processed(serde_json::json!({"name": "x"}), false).is_err());
    }

    #[test]
    fn custom_items_follow_fig() {
        let items =
            custom_items(serde_json::json!([{"name": "a"}, "str", {"name": "b", "type": ""}]))
                .expect("an array");
        let out = suggestions(items);
        assert_eq!(names(&out), ["a", "", "b"]);
        assert!(out.iter().all(|s| s.kind == Some(SuggestionType::Arg)));
        assert!(custom_items(serde_json::json!([{"name": "a"}, null])).is_err());
        assert!(custom_items(serde_json::json!("a")).is_err());
    }

    #[test]
    fn keys_and_inputs_follow_fig() {
        let argv = ScriptInput::from_argv(&["git".to_owned(), "branch".to_owned()], "/r")
            .expect("an argv");
        assert_eq!(
            argv.key,
            r#"{"command":"git","args":["branch"],"cwd":"/r"}"#
        );
        let object = ScriptInput::from_object(
            &ExecuteCommandInput {
                command: "ls".to_owned(),
                args: vec!["-1".to_owned()],
                timeout: Some(7),
                ..ExecuteCommandInput::default()
            },
            "/r",
        );
        assert_eq!(
            object.key,
            r#"{"cwd":"/r","command":"ls","args":["-1"],"timeout":7}"#
        );
        assert_eq!(ScriptInput::from_returned(&Value::Null, "/r"), Ok(None));
        assert!(ScriptInput::from_returned(&serde_json::json!("ls"), "/r").is_err());
        assert!(ScriptInput::from_returned(&serde_json::json!({"command": "ls"}), "/r").is_err());

        let cache = Cache {
            cache_by_directory: true,
            ..Cache::default()
        };
        let tokens = ["npm".to_owned(), "run".to_owned(), String::new()];
        assert_eq!(
            cache_key(&cache, false, "/r", "", None, &tokens),
            "/r,npm run "
        );
        assert_eq!(
            cache_key(&cache, true, "/r", "src/m", Some("k"), &tokens),
            "/r/src/,k"
        );
        assert_eq!(
            cache_key(&Cache::default(), false, "/r", "", Some(""), &tokens),
            ",npm run "
        );
        assert_eq!(
            script_timeout(Duration::from_millis(5000), Some(9000), Some(100)),
            Duration::from_millis(9000)
        );
    }

    #[test]
    fn the_cache_plans_as_fig() {
        let mut cache = GenCache::default();
        let output = Cached::Json("[]".to_owned());
        let start = Instant::now();
        let max_age = Cache {
            strategy: Some(CacheStrategy::MaxAge),
            ttl: Some(100),
            ..Cache::default()
        };
        assert_eq!(cache.plan("k", &max_age, start), Plan::Run);
        cache.started("k", 1, start);
        assert_eq!(cache.plan("k", &max_age, start), Plan::Join(1));
        cache.finished("k", 1, Some(output.clone()));
        assert_eq!(
            cache.plan("k", &max_age, start + Duration::from_millis(50)),
            Plan::Hit(output.clone())
        );
        assert_eq!(
            cache.plan("k", &max_age, start + Duration::from_millis(150)),
            Plan::Run
        );

        let swr = Cache::default();
        assert_eq!(cache.plan("s", &swr, start), Plan::Run);
        cache.started("s", 2, start);
        cache.finished("s", 2, Some(output.clone()));
        let later = start + Duration::from_millis(1);
        assert_eq!(cache.plan("s", &swr, later), Plan::StaleRun(output.clone()));
        cache.started("s", 3, later);
        assert_eq!(
            cache.plan("s", &swr, later),
            Plan::StaleJoin(output.clone(), 3)
        );
        // A failed fetch keeps the old value and lets the next use run.
        cache.finished("s", 3, None);
        assert_eq!(
            cache.plan("s", &swr, later + Duration::from_millis(1)),
            Plan::StaleRun(output)
        );
    }

    /// A script's string with a control character in it is never an item:
    /// it would be its own insert value.
    #[test]
    fn strings_with_control_characters_are_left_out() {
        let names = |items: Vec<Suggestion>| -> Vec<String> {
            items.into_iter().map(|item| item.name[0].clone()).collect()
        };
        assert_eq!(names(split_on("a\tb\nc\nd\u{1b}e", "\n", false)), ["c"]);
        let processed =
            post_processed(serde_json::json!(["x\r", "y", "z\u{85}"]), false).expect("an array");
        assert_eq!(names(processed), ["y"]);
    }

    #[test]
    fn the_cache_keeps_128_keys_and_16_mib() {
        let mut cache = GenCache::default();
        let start = Instant::now();
        let forever = Cache {
            strategy: Some(CacheStrategy::MaxAge),
            ..Cache::default()
        };
        let fill = |cache: &mut GenCache, key: &str, fetch: u64, value: Cached| {
            assert_eq!(cache.plan(key, &forever, start), Plan::Run);
            cache.started(key, fetch, start);
            cache.finished(key, fetch, Some(value));
        };
        let small = Cached::Json("[]".to_owned());
        // A key being fetched is never evicted.
        assert_eq!(cache.plan("busy", &forever, start), Plan::Run);
        cache.started("busy", 0, start);
        for i in 0..200 {
            fill(&mut cache, &format!("k{i}"), i + 1, small.clone());
            // Using a key keeps it.
            assert!(matches!(cache.plan("k0", &forever, start), Plan::Hit(_)));
        }
        assert_eq!(cache.len(), CACHE_ENTRIES);
        assert_eq!(cache.plan("busy", &forever, start), Plan::Join(0));
        assert!(matches!(cache.plan("k0", &forever, start), Plan::Hit(_)));
        assert!(matches!(cache.plan("k199", &forever, start), Plan::Hit(_)));
        // The oldest went.
        assert_eq!(cache.plan("k1", &forever, start), Plan::Run);

        // 20 values of 1 MiB: the oldest go to keep 16 MiB.
        let mut cache = GenCache::default();
        let big = Cached::Output(Output {
            stdout: "x".repeat(1 << 20),
            stderr: String::new(),
            status: 0,
        });
        for i in 0..20 {
            fill(&mut cache, &format!("big{i}"), i, big.clone());
            assert!(cache.bytes <= CACHE_BYTES);
        }
        assert_eq!(cache.bytes, CACHE_BYTES);
        assert_eq!(cache.len(), 16);
        assert_eq!(cache.plan("big3", &forever, start), Plan::Run);
        assert!(matches!(cache.plan("big4", &forever, start), Plan::Hit(_)));
        // A new value for a key replaces its bytes.
        cache.started("big4", 99, start);
        cache.finished("big4", 99, Some(Cached::Json("[]".to_owned())));
        assert_eq!(cache.bytes, 15 * (1 << 20) + 2);
    }

    #[test]
    fn fig_suggestions_have_string_names() {
        let suggestion = Suggestion {
            name: vec!["a.js".to_owned()],
            kind: Some(SuggestionType::File),
            ..Suggestion::default()
        };
        assert_eq!(
            fig_suggestion(&suggestion),
            serde_json::json!({"name": "a.js", "type": "file", "isDangerous": false, "hidden": false})
        );
    }
}
