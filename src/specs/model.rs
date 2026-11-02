//! The completion spec tree, covering every field of `@withfig/autocomplete-types`
//! 1.31.0. A spec's root is a [`Subcommand`].
//!
//! Every type has two serde forms:
//! - **Fig's JSON** (human-readable formats, e.g. `serde_json`): the shape the
//!   walker (`walk.js`) produces from a spec module, with unions untagged and
//!   functions as `{"$fn": n}`. Single values in "one or many" fields are
//!   accepted and normalised to a `Vec`; they serialize back as arrays.
//! - **The stored form** (compact formats, e.g. postcard): plain derived
//!   structs and externally tagged enums, which a non-self-describing format
//!   can read back.
//!
//! The union types pick their form with `is_human_readable`; untagged parsing
//! happens only on the JSON side. Struct fields are never skipped on
//! serialization, because postcard reads fields by position: an absent
//! `Option` costs one byte instead.

use std::collections::BTreeMap;
use std::fmt;
use std::marker::PhantomData;
use std::sync::Arc;

use serde::de::value::MapAccessDeserializer;
use serde::de::{self, IntoDeserializer, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A function in the spec's module, by its walk order: `fns[id]` in the
/// owning [`Entry`] is the function's path from the spec root.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(remote = "Self")]
pub struct FnRef(pub u32);

/// Opaque JSON, kept as text (`_internal`, which specs must not use).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(remote = "Self")]
pub struct JsonText(pub String);

/// A command or subcommand; also a spec's root.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Subcommand {
    #[serde(deserialize_with = "one_or_many")]
    pub name: Vec<String>,
    pub display_name: Option<String>,
    pub insert_value: Option<String>,
    pub replace_value: Option<String>,
    #[serde(with = "shared_text")]
    pub description: Option<Arc<str>>,
    pub icon: Option<String>,
    pub is_dangerous: bool,
    pub priority: Option<f64>,
    pub hidden: bool,
    pub deprecated: Option<Deprecated>,
    pub preview_component: Option<String>,
    #[serde(rename = "_internal")]
    pub internal: Option<JsonText>,
    pub subcommands: Vec<Subcommand>,
    /// `None` when the spec does not say: Fig then guesses from
    /// `subcommands`, while an explicit `false` is final.
    pub requires_subcommand: Option<bool>,
    pub options: Vec<CommandOption>,
    #[serde(deserialize_with = "one_or_many")]
    pub args: Vec<Arg>,
    pub filter_strategy: Option<FilterStrategy>,
    pub additional_suggestions: Vec<StaticSuggestion>,
    pub load_spec: Option<LoadSpec>,
    pub generate_spec: Option<FnRef>,
    pub generate_spec_cache_key: Option<StringOrFn>,
    pub parser_directives: Option<SubcommandDirectives>,
    pub cache: bool,
}

/// An option (flag). Fig's `Option`, renamed to keep `std::option::Option`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CommandOption {
    #[serde(deserialize_with = "one_or_many")]
    pub name: Vec<String>,
    pub display_name: Option<String>,
    pub insert_value: Option<String>,
    pub replace_value: Option<String>,
    #[serde(with = "shared_text")]
    pub description: Option<Arc<str>>,
    pub icon: Option<String>,
    pub is_dangerous: bool,
    pub priority: Option<f64>,
    pub hidden: bool,
    pub deprecated: Option<Deprecated>,
    pub preview_component: Option<String>,
    #[serde(rename = "_internal")]
    pub internal: Option<JsonText>,
    #[serde(deserialize_with = "one_or_many")]
    pub args: Vec<Arg>,
    pub is_persistent: bool,
    pub is_required: bool,
    pub requires_equals: bool,
    pub requires_separator: Option<RequiresSeparator>,
    pub is_repeatable: Option<Repeatable>,
    pub exclusive_on: Vec<String>,
    pub depends_on: Vec<String>,
}

/// An argument of a subcommand or an option.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Arg {
    pub name: Option<String>,
    #[serde(with = "shared_text")]
    pub description: Option<Arc<str>>,
    pub is_dangerous: bool,
    pub suggestions: Vec<StaticSuggestion>,
    #[serde(deserialize_with = "one_or_many")]
    pub template: Vec<Template>,
    #[serde(deserialize_with = "one_or_many")]
    pub generators: Vec<Generator>,
    pub filter_strategy: Option<FilterStrategy>,
    pub suggest_current_token: bool,
    pub is_variadic: bool,
    /// `None` means Fig's default, `true`.
    pub options_can_break_variadic_arg: Option<bool>,
    pub is_optional: bool,
    pub is_command: bool,
    pub is_script: bool,
    pub is_module: Option<String>,
    pub debounce: bool,
    /// A few specs give a number or boolean; it is kept as JS would print it.
    #[serde(deserialize_with = "scalar_string")]
    pub default: Option<String>,
    pub load_spec: Option<LoadSpec>,
    pub parser_directives: Option<ArgDirectives>,
}

/// A suggestion, static or returned by a generator.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Suggestion {
    #[serde(deserialize_with = "one_or_many")]
    pub name: Vec<String>,
    #[serde(rename = "type")]
    pub kind: Option<SuggestionType>,
    pub display_name: Option<String>,
    pub insert_value: Option<String>,
    pub replace_value: Option<String>,
    #[serde(with = "shared_text")]
    pub description: Option<Arc<str>>,
    pub icon: Option<String>,
    pub is_dangerous: bool,
    pub priority: Option<f64>,
    pub hidden: bool,
    pub deprecated: Option<Deprecated>,
    pub preview_component: Option<String>,
    #[serde(rename = "_internal")]
    pub internal: Option<JsonText>,
}

/// The object form of `deprecated`: Fig's `Omit<BaseSuggestion, "deprecated">`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DeprecatedInfo {
    pub display_name: Option<String>,
    pub insert_value: Option<String>,
    pub replace_value: Option<String>,
    pub description: Option<String>,
    pub icon: Option<String>,
    pub is_dangerous: bool,
    pub priority: Option<f64>,
    pub hidden: bool,
    pub preview_component: Option<String>,
    #[serde(rename = "_internal")]
    pub internal: Option<JsonText>,
}

/// A way to compute suggestions for an argument.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Generator {
    #[serde(deserialize_with = "one_or_many")]
    pub template: Vec<Template>,
    pub filter_template_suggestions: Option<FnRef>,
    pub script: Option<Script>,
    pub script_timeout: Option<u64>,
    pub post_process: Option<FnRef>,
    pub split_on: Option<String>,
    pub trigger: Option<Trigger>,
    pub get_query_term: Option<StringOrFn>,
    pub custom: Option<FnRef>,
    pub cache: Option<Cache>,
}

/// A command for a generator to run.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ExecuteCommandInput {
    pub command: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub env: BTreeMap<String, String>,
    pub timeout: Option<u64>,
}

/// How a generator's results are cached.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Cache {
    /// `None` means Fig's default, stale-while-revalidate.
    pub strategy: Option<CacheStrategy>,
    pub ttl: Option<u64>,
    pub cache_by_directory: bool,
    pub cache_key: Option<String>,
}

/// `parserDirectives` on a subcommand.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SubcommandDirectives {
    pub flags_are_posix_noncompliant: bool,
    pub options_must_precede_arguments: bool,
    #[serde(deserialize_with = "one_or_many")]
    pub option_arg_separators: Vec<String>,
}

/// `parserDirectives` on an argument.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ArgDirectives {
    pub alias: Option<StringOrFn>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FilterStrategy {
    Fuzzy,
    Prefix,
    Default,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Template {
    Filepaths,
    Folders,
    History,
    Help,
}

/// A suggestion's `type`. Some specs use names outside Fig's list (e.g.
/// `"argument"`); Fig shows those like an untyped suggestion, and they are
/// kept as `Other`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(remote = "Self")]
pub enum SuggestionType {
    Folder,
    File,
    Arg,
    Subcommand,
    Option,
    Special,
    Mixin,
    Shortcut,
    Other(String),
}

impl SuggestionType {
    const NAMED: [(&str, SuggestionType); 8] = [
        ("folder", Self::Folder),
        ("file", Self::File),
        ("arg", Self::Arg),
        ("subcommand", Self::Subcommand),
        ("option", Self::Option),
        ("special", Self::Special),
        ("mixin", Self::Mixin),
        ("shortcut", Self::Shortcut),
    ];

    pub fn as_str(&self) -> &str {
        match self {
            Self::Other(name) => name,
            known => Self::NAMED
                .iter()
                .find(|(_, kind)| kind == known)
                .map_or("", |(name, _)| name),
        }
    }

    pub fn from_name(name: &str) -> Self {
        Self::NAMED
            .iter()
            .find(|(known, _)| *known == name)
            .map_or_else(|| Self::Other(name.to_owned()), |(_, kind)| kind.clone())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CacheStrategy {
    MaxAge,
    StaleWhileRevalidate,
}

/// An item of `suggestions` or `additionalSuggestions`. Fig treats a bare
/// string differently from an object (it inherits the parent's
/// `isDangerous`), so the two stay apart.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(remote = "Self")]
pub enum StaticSuggestion {
    Name(String),
    Suggestion(Box<Suggestion>),
}

/// `deprecated`: a flag, or an object describing the replacement.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(remote = "Self")]
pub enum Deprecated {
    Flag(bool),
    Info(Box<DeprecatedInfo>),
}

/// `isRepeatable`: a flag, or the most times the option may appear.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(remote = "Self")]
pub enum Repeatable {
    Flag(bool),
    Times(u32),
}

/// `requiresSeparator`: a flag (meaning `=`), or the separator itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(remote = "Self")]
pub enum RequiresSeparator {
    Flag(bool),
    Separator(String),
}

/// A string or a function, as in `getQueryTerm`, `generateSpecCacheKey` and
/// `parserDirectives.alias`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(remote = "Self")]
pub enum StringOrFn {
    String(String),
    Fn(FnRef),
}

/// `loadSpec`: a spec name, an inline subcommand, or a function.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(remote = "Self")]
pub enum LoadSpec {
    Name(String),
    Inline(Box<Subcommand>),
    Fn(FnRef),
}

/// A generator's `script`: an argv, a full command, or a function returning
/// either.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(remote = "Self")]
pub enum Script {
    Argv(Vec<String>),
    Command(ExecuteCommandInput),
    Fn(FnRef),
}

/// A generator's `trigger`: when to run it again while a token is edited.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(remote = "Self")]
pub enum Trigger {
    /// Regenerate when the count or position of this string in the token
    /// changes.
    String(String),
    Fn(FnRef),
    OnChange,
    OnThreshold {
        length: u32,
    },
    OnMatch {
        string: Vec<String>,
    },
}

/// One spec in the archive. An entry "has functions" when `fns` is non-empty;
/// its module source is then stored too, for the spec thread to evaluate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Entry {
    /// A spec. `fns[id]` is the path of function `id`, e.g.
    /// `"subcommands.3.args.0.generators.1.postProcess"`.
    Spec { root: Subcommand, fns: Vec<String> },
    /// A versioned spec's `index.js`, whose default export is a function of
    /// the CLI version; it is evaluated at runtime (`fns` is `[""]`).
    Versioned { fns: Vec<String> },
    /// One version of a versioned spec, with its diff table as JSON text.
    /// The table's functions follow the root's in `fns`, with paths that
    /// start with `versions.` (see `walk.js`).
    VersionFile {
        root: Subcommand,
        fns: Vec<String>,
        versions: String,
    },
}

impl Entry {
    /// The function paths, indexed by [`FnRef`].
    pub fn fns(&self) -> &[String] {
        match self {
            Self::Spec { fns, .. } | Self::Versioned { fns } | Self::VersionFile { fns, .. } => fns,
        }
    }

    pub fn has_functions(&self) -> bool {
        !self.fns().is_empty()
    }

    /// The spec tree, when the entry has one.
    pub fn root(&self) -> Option<&Subcommand> {
        match self {
            Self::Spec { root, .. } | Self::VersionFile { root, .. } => Some(root),
            Self::Versioned { .. } => None,
        }
    }
}

/// The Fig JSON form of a type whose stored form is derived.
trait FigForm: Sized {
    fn serialize_fig<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error>;
    fn deserialize_fig<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error>;
}

/// `Serialize` and `Deserialize` that use the Fig form for human-readable
/// formats and the derived (`remote = "Self"`) form otherwise.
macro_rules! two_forms {
    ($($ty:ident),* $(,)?) => {$(
        impl Serialize for $ty {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                if serializer.is_human_readable() {
                    self.serialize_fig(serializer)
                } else {
                    $ty::serialize(self, serializer)
                }
            }
        }

        impl<'de> Deserialize<'de> for $ty {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                if deserializer.is_human_readable() {
                    Self::deserialize_fig(deserializer)
                } else {
                    $ty::deserialize(deserializer)
                }
            }
        }
    )*};
}

two_forms!(
    FnRef,
    JsonText,
    SuggestionType,
    StaticSuggestion,
    Deprecated,
    Repeatable,
    RequiresSeparator,
    StringOrFn,
    LoadSpec,
    Script,
    Trigger,
);

/// `{"$fn": n}`, and nothing else.
#[derive(Serialize)]
struct FnRefWire {
    #[serde(rename = "$fn")]
    id: u32,
}

struct FnRefVisitor;

impl<'de> Visitor<'de> for FnRefVisitor {
    type Value = FnRef;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str(r#"a function, {"$fn": n}"#)
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<FnRef, A::Error> {
        let mut id = None;
        while let Some(key) = map.next_key::<String>()? {
            if key != "$fn" || id.is_some() {
                return Err(de::Error::unknown_field(&key, &["$fn"]));
            }
            id = Some(map.next_value()?);
        }
        id.map(FnRef).ok_or_else(|| de::Error::missing_field("$fn"))
    }
}

impl FigForm for FnRef {
    fn serialize_fig<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        FnRefWire { id: self.0 }.serialize(serializer)
    }

    fn deserialize_fig<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(FnRefVisitor)
    }
}

impl FigForm for JsonText {
    fn serialize_fig<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match serde_json::from_str::<serde_json::Value>(&self.0) {
            Ok(value) => value.serialize(serializer),
            Err(_) => self.0.serialize(serializer),
        }
    }

    fn deserialize_fig<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        Ok(Self(value.to_string()))
    }
}

impl FigForm for SuggestionType {
    fn serialize_fig<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.as_str().serialize(serializer)
    }

    fn deserialize_fig<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        Ok(Self::from_name(&name))
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum StaticSuggestionWire {
    Name(String),
    Suggestion(Box<Suggestion>),
}

impl FigForm for StaticSuggestion {
    fn serialize_fig<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Name(name) => name.serialize(serializer),
            Self::Suggestion(suggestion) => suggestion.serialize(serializer),
        }
    }

    fn deserialize_fig<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match StaticSuggestionWire::deserialize(deserializer)? {
            StaticSuggestionWire::Name(name) => Self::Name(name),
            StaticSuggestionWire::Suggestion(suggestion) => Self::Suggestion(suggestion),
        })
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum DeprecatedWire {
    Flag(bool),
    Info(Box<DeprecatedInfo>),
}

impl FigForm for Deprecated {
    fn serialize_fig<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Flag(flag) => flag.serialize(serializer),
            Self::Info(info) => info.serialize(serializer),
        }
    }

    fn deserialize_fig<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match DeprecatedWire::deserialize(deserializer)? {
            DeprecatedWire::Flag(flag) => Self::Flag(flag),
            DeprecatedWire::Info(info) => Self::Info(info),
        })
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RepeatableWire {
    Flag(bool),
    Times(u32),
}

impl FigForm for Repeatable {
    fn serialize_fig<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Flag(flag) => flag.serialize(serializer),
            Self::Times(times) => times.serialize(serializer),
        }
    }

    fn deserialize_fig<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match RepeatableWire::deserialize(deserializer)? {
            RepeatableWire::Flag(flag) => Self::Flag(flag),
            RepeatableWire::Times(times) => Self::Times(times),
        })
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RequiresSeparatorWire {
    Flag(bool),
    Separator(String),
}

impl FigForm for RequiresSeparator {
    fn serialize_fig<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Flag(flag) => flag.serialize(serializer),
            Self::Separator(separator) => separator.serialize(serializer),
        }
    }

    fn deserialize_fig<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match RequiresSeparatorWire::deserialize(deserializer)? {
            RequiresSeparatorWire::Flag(flag) => Self::Flag(flag),
            RequiresSeparatorWire::Separator(separator) => Self::Separator(separator),
        })
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum StringOrFnWire {
    String(String),
    Fn(FnRef),
}

impl FigForm for StringOrFn {
    fn serialize_fig<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::String(string) => string.serialize(serializer),
            Self::Fn(id) => id.serialize(serializer),
        }
    }

    fn deserialize_fig<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match StringOrFnWire::deserialize(deserializer)? {
            StringOrFnWire::String(string) => Self::String(string),
            StringOrFnWire::Fn(id) => Self::Fn(id),
        })
    }
}

/// `Fn` comes before `Inline`: a function is a map with `$fn` alone, so a
/// subcommand object never parses as one.
#[derive(Deserialize)]
#[serde(untagged)]
enum LoadSpecWire {
    Name(String),
    Fn(FnRef),
    Inline(Box<Subcommand>),
}

impl FigForm for LoadSpec {
    fn serialize_fig<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Name(name) => name.serialize(serializer),
            Self::Inline(subcommand) => subcommand.serialize(serializer),
            Self::Fn(id) => id.serialize(serializer),
        }
    }

    fn deserialize_fig<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match LoadSpecWire::deserialize(deserializer)? {
            LoadSpecWire::Name(name) => Self::Name(name),
            LoadSpecWire::Fn(id) => Self::Fn(id),
            LoadSpecWire::Inline(subcommand) => Self::Inline(subcommand),
        })
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ScriptWire {
    Argv(Vec<String>),
    Fn(FnRef),
    Command(ExecuteCommandInput),
}

impl FigForm for Script {
    fn serialize_fig<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Argv(argv) => argv.serialize(serializer),
            Self::Command(input) => input.serialize(serializer),
            Self::Fn(id) => id.serialize(serializer),
        }
    }

    fn deserialize_fig<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match ScriptWire::deserialize(deserializer)? {
            ScriptWire::Argv(argv) => Self::Argv(argv),
            ScriptWire::Fn(id) => Self::Fn(id),
            ScriptWire::Command(input) => Self::Command(input),
        })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "on", rename_all = "lowercase", deny_unknown_fields)]
enum TriggerOn {
    Change,
    Threshold {
        length: u32,
    },
    Match {
        #[serde(deserialize_with = "one_or_many")]
        string: Vec<String>,
    },
}

#[derive(Deserialize)]
#[serde(untagged)]
enum TriggerWire {
    String(String),
    Fn(FnRef),
    On(TriggerOn),
}

impl FigForm for Trigger {
    fn serialize_fig<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::String(string) => string.serialize(serializer),
            Self::Fn(id) => id.serialize(serializer),
            Self::OnChange => TriggerOn::Change.serialize(serializer),
            Self::OnThreshold { length } => {
                TriggerOn::Threshold { length: *length }.serialize(serializer)
            }
            Self::OnMatch { string } => TriggerOn::Match {
                string: string.clone(),
            }
            .serialize(serializer),
        }
    }

    fn deserialize_fig<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match TriggerWire::deserialize(deserializer)? {
            TriggerWire::String(string) => Self::String(string),
            TriggerWire::Fn(id) => Self::Fn(id),
            TriggerWire::On(TriggerOn::Change) => Self::OnChange,
            TriggerWire::On(TriggerOn::Threshold { length }) => Self::OnThreshold { length },
            TriggerWire::On(TriggerOn::Match { string }) => Self::OnMatch { string },
        })
    }
}

/// A string field that some specs fill with a number or a boolean: in Fig's
/// JSON any scalar is accepted and kept as its JS string form.
/// A description as `Option<String>` on the wire, kept as an `Arc<str>`
/// (serde's `rc` feature is off): the parse hands it to every item that
/// shows it without copying it.
mod shared_text {
    use std::sync::Arc;

    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(
        text: &Option<Arc<str>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        text.as_deref().serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Arc<str>>, D::Error> {
        Ok(Option::<String>::deserialize(deserializer)?.map(Arc::from))
    }
}

fn scalar_string<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    if !deserializer.is_human_readable() {
        return Option::deserialize(deserializer);
    }
    match serde_json::Value::deserialize(deserializer)? {
        serde_json::Value::Null => Ok(None),
        serde_json::Value::String(string) => Ok(Some(string)),
        serde_json::Value::Bool(flag) => Ok(Some(flag.to_string())),
        serde_json::Value::Number(number) => Ok(Some(number.to_string())),
        other => Err(de::Error::invalid_type(
            de::Unexpected::Other(if other.is_array() {
                "an array"
            } else {
                "an object"
            }),
            &"a string, number or boolean",
        )),
    }
}

/// A "one or many" field: in Fig's JSON a single value or an array, always a
/// `Vec` here; the stored form is the plain `Vec`.
fn one_or_many<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    if deserializer.is_human_readable() {
        deserializer.deserialize_any(OneOrMany(PhantomData))
    } else {
        Vec::deserialize(deserializer)
    }
}

struct OneOrMany<T>(PhantomData<T>);

impl<'de, T: Deserialize<'de>> Visitor<'de> for OneOrMany<T> {
    type Value = Vec<T>;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a value or an array of values")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut items = Vec::with_capacity(seq.size_hint().unwrap_or(0));
        while let Some(item) = seq.next_element()? {
            items.push(item);
        }
        Ok(items)
    }

    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
        T::deserialize(MapAccessDeserializer::new(map)).map(|item| vec![item])
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
        T::deserialize(value.into_deserializer()).map(|item| vec![item])
    }

    fn visit_string<E: de::Error>(self, value: String) -> Result<Self::Value, E> {
        T::deserialize(value.into_deserializer()).map(|item| vec![item])
    }

    fn visit_bool<E: de::Error>(self, value: bool) -> Result<Self::Value, E> {
        T::deserialize(value.into_deserializer()).map(|item| vec![item])
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
        T::deserialize(value.into_deserializer()).map(|item| vec![item])
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
        T::deserialize(value.into_deserializer()).map(|item| vec![item])
    }

    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Self::Value, E> {
        T::deserialize(value.into_deserializer()).map(|item| vec![item])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn from_json(json: &str) -> Subcommand {
        match serde_json::from_str(json) {
            Ok(spec) => spec,
            Err(err) => panic!("{json}: {err}"),
        }
    }

    fn postcard_round_trip(spec: &Subcommand) -> Subcommand {
        let bytes = postcard::to_allocvec(spec).expect("serialize");
        postcard::from_bytes(&bytes).expect("deserialize")
    }

    const UNIONS: &str = r#"{
        "name": "tool",
        "deprecated": {"description": "use other"},
        "loadSpec": {"$fn": 2},
        "generateSpecCacheKey": "key",
        "parserDirectives": {"optionArgSeparators": "="},
        "additionalSuggestions": ["a", {"name": ["b", "c"], "type": "shortcut", "deprecated": true}],
        "subcommands": [
            {"name": "inline", "loadSpec": {"name": "inner", "args": {"name": "x"}}},
            {"name": "named", "loadSpec": "other", "priority": 75.5}
        ],
        "options": [
            {"name": "-a", "isRepeatable": 3, "requiresSeparator": ":"},
            {"name": ["-b", "--bee"], "isRepeatable": true, "requiresSeparator": false,
             "args": [{"name": "one"}, {"name": "two", "optionsCanBreakVariadicArg": false}]}
        ],
        "args": {
            "template": "filepaths",
            "suggestions": ["s"],
            "parserDirectives": {"alias": {"$fn": 0}},
            "loadSpec": "x",
            "generators": [
                {"script": ["git", "branch"], "postProcess": {"$fn": 1}, "trigger": "/",
                 "cache": {"strategy": "max-age", "ttl": 1000, "cacheByDirectory": true}},
                {"script": {"command": "ls", "args": ["-1"], "env": {"A": "1"}, "timeout": 5},
                 "trigger": {"on": "threshold", "length": 2}, "getQueryTerm": "/"},
                {"script": {"$fn": 3}, "trigger": {"$fn": 4}, "getQueryTerm": {"$fn": 5},
                 "template": ["folders", "history"], "filterTemplateSuggestions": {"$fn": 6}},
                {"custom": {"$fn": 7}, "trigger": {"on": "match", "string": "a"}},
                {"trigger": {"on": "match", "string": ["a", "b"]}, "splitOn": "\n"},
                {"trigger": {"on": "change"}, "scriptTimeout": 100}
            ]
        }
    }"#;

    #[test]
    fn unions_parse_from_fig_json() {
        let spec = from_json(UNIONS);
        assert_eq!(spec.name, ["tool"]);
        assert!(
            matches!(&spec.deprecated, Some(Deprecated::Info(info)) if info.description.as_deref() == Some("use other"))
        );
        assert_eq!(spec.load_spec, Some(LoadSpec::Fn(FnRef(2))));
        assert_eq!(
            spec.generate_spec_cache_key,
            Some(StringOrFn::String("key".into()))
        );
        assert_eq!(
            spec.parser_directives
                .as_ref()
                .map(|d| d.option_arg_separators.clone()),
            Some(vec!["=".to_owned()])
        );
        assert_eq!(
            spec.additional_suggestions[0],
            StaticSuggestion::Name("a".into())
        );
        let StaticSuggestion::Suggestion(b) = &spec.additional_suggestions[1] else {
            panic!("{:?}", spec.additional_suggestions[1]);
        };
        assert_eq!(b.name, ["b", "c"]);
        assert_eq!(b.kind, Some(SuggestionType::Shortcut));
        assert_eq!(b.deprecated, Some(Deprecated::Flag(true)));

        let Some(LoadSpec::Inline(inner)) = &spec.subcommands[0].load_spec else {
            panic!("{:?}", spec.subcommands[0].load_spec);
        };
        assert_eq!(inner.name, ["inner"]);
        assert_eq!(inner.args.len(), 1);
        assert_eq!(
            spec.subcommands[1].load_spec,
            Some(LoadSpec::Name("other".into()))
        );
        assert_eq!(spec.subcommands[1].priority, Some(75.5));

        let [a, b] = &spec.options[..] else {
            panic!("{:?}", spec.options);
        };
        assert_eq!(a.is_repeatable, Some(Repeatable::Times(3)));
        assert_eq!(
            a.requires_separator,
            Some(RequiresSeparator::Separator(":".into()))
        );
        assert_eq!(b.name, ["-b", "--bee"]);
        assert_eq!(b.is_repeatable, Some(Repeatable::Flag(true)));
        assert_eq!(b.requires_separator, Some(RequiresSeparator::Flag(false)));
        assert_eq!(b.args.len(), 2);
        assert_eq!(b.args[1].options_can_break_variadic_arg, Some(false));

        let arg = &spec.args[0];
        assert_eq!(arg.template, [Template::Filepaths]);
        assert_eq!(arg.suggestions, [StaticSuggestion::Name("s".into())]);
        assert_eq!(
            arg.parser_directives.as_ref().and_then(|d| d.alias.clone()),
            Some(StringOrFn::Fn(FnRef(0)))
        );
        let g = &arg.generators;
        assert_eq!(g.len(), 6);
        assert_eq!(
            g[0].script,
            Some(Script::Argv(vec!["git".into(), "branch".into()]))
        );
        assert_eq!(g[0].post_process, Some(FnRef(1)));
        assert_eq!(g[0].trigger, Some(Trigger::String("/".into())));
        assert_eq!(
            g[0].cache,
            Some(Cache {
                strategy: Some(CacheStrategy::MaxAge),
                ttl: Some(1000),
                cache_by_directory: true,
                cache_key: None,
            })
        );
        let Some(Script::Command(input)) = &g[1].script else {
            panic!("{:?}", g[1].script);
        };
        assert_eq!(input.command, "ls");
        assert_eq!(input.env.get("A").map(String::as_str), Some("1"));
        assert_eq!(input.timeout, Some(5));
        assert_eq!(g[1].trigger, Some(Trigger::OnThreshold { length: 2 }));
        assert_eq!(g[2].script, Some(Script::Fn(FnRef(3))));
        assert_eq!(g[2].trigger, Some(Trigger::Fn(FnRef(4))));
        assert_eq!(g[2].get_query_term, Some(StringOrFn::Fn(FnRef(5))));
        assert_eq!(g[2].template, [Template::Folders, Template::History]);
        assert_eq!(g[2].filter_template_suggestions, Some(FnRef(6)));
        assert_eq!(g[3].custom, Some(FnRef(7)));
        assert_eq!(
            g[3].trigger,
            Some(Trigger::OnMatch {
                string: vec!["a".into()]
            })
        );
        assert_eq!(
            g[4].trigger,
            Some(Trigger::OnMatch {
                string: vec!["a".into(), "b".into()]
            })
        );
        assert_eq!(g[5].trigger, Some(Trigger::OnChange));
        assert_eq!(g[5].script_timeout, Some(100));
    }

    #[test]
    fn both_forms_round_trip() {
        let spec = from_json(UNIONS);
        assert_eq!(postcard_round_trip(&spec), spec);
        let json = serde_json::to_string(&spec).expect("to json");
        assert_eq!(from_json(&json), spec);
    }

    #[test]
    fn odd_values_are_kept() {
        let spec = from_json(
            r#"{"name": "x", "args": {"default": 8100, "suggestions": [{"name": "a", "type": "argument"}]}}"#,
        );
        assert_eq!(spec.args[0].default.as_deref(), Some("8100"));
        let StaticSuggestion::Suggestion(a) = &spec.args[0].suggestions[0] else {
            panic!("{:?}", spec.args[0].suggestions);
        };
        assert_eq!(a.kind, Some(SuggestionType::Other("argument".into())));
        assert_eq!(SuggestionType::from_name("mixin"), SuggestionType::Mixin);
        assert_eq!(SuggestionType::Mixin.as_str(), "mixin");
        assert_eq!(postcard_round_trip(&spec), spec);
        let json = serde_json::to_string(&spec).expect("to json");
        assert_eq!(from_json(&json), spec);
    }

    #[test]
    fn internal_is_opaque_json_text() {
        let spec = from_json(r#"{"name": "x", "_internal": {"a": [1, {"b": null}]}}"#);
        assert_eq!(
            spec.internal,
            Some(JsonText(r#"{"a":[1,{"b":null}]}"#.into()))
        );
        assert_eq!(postcard_round_trip(&spec), spec);
    }

    #[test]
    fn bad_union_values_are_errors() {
        for json in [r#"{"on": "sometimes"}"#, r#"{"on": "threshold"}"#, "3"] {
            let parsed = serde_json::from_str::<Trigger>(json);
            assert!(parsed.is_err(), "{json}: {parsed:?}");
        }
        for json in ["true", "[1]"] {
            let parsed = serde_json::from_str::<LoadSpec>(json);
            assert!(parsed.is_err(), "{json}: {parsed:?}");
        }
    }

    #[test]
    fn entry_round_trips_through_postcard() {
        let entry = Entry::VersionFile {
            root: from_json(UNIONS),
            fns: vec!["args.0.parserDirectives.alias".into()],
            versions: r#"{"1.0.0":{}}"#.into(),
        };
        let bytes = postcard::to_allocvec(&entry).expect("serialize");
        let back: Entry = postcard::from_bytes(&bytes).expect("deserialize");
        assert_eq!(back, entry);
        assert!(back.has_functions());
        assert_eq!(Entry::Versioned { fns: vec![] }.root(), None);
    }
}
