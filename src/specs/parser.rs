//! The command line parsed against a spec: a port of Fig's argument parser,
//! `parseArguments.ts` in aws/amazon-q-developer-cli v1.10.1
//! (`packages/autocomplete-parser/src`), plus the part of the suggestion list
//! that comes from the parse (`getAllSuggestions`, `packages/autocomplete/src/
//! suggestions/index.ts`). Line references in comments are to those files.
//!
//! [`parse`] is pure: it never loads a spec or calls a function itself. What
//! it needs and does not have yet comes back as a [`Need`]; the caller gets
//! it answered (on the spec thread), keeps the answer, and parses again.
//!
//! Where Fig's parser throws, the whole parse fails and Fig shows no
//! suggestions from the spec; [`parse`] then returns a [`Suggest`] with no
//! items and no arg. Those cases:
//! - a token before the cursor that no state can consume (`cd a b `);
//! - a lone `-` before the cursor where an option could go (`cat - x`): Fig
//!   reads its empty option chain and throws a `TypeError`;
//! - a `parserDirectives.alias` that fails (its function throws, or the
//!   substitution is not one simple command), unless an alias was
//!   substituted just before; then the parse fails only if the token loads
//!   no spec. A second substitution in one spec's part of the line always
//!   fails;
//! - a `loadSpec` function that fails;
//! - a hand-off to a spec that does not exist, or to a local (`.fig`
//!   folder) spec, which fac does not have: an `isScript` arg, a command
//!   given as a path (`sudo ./run`), `?` and `+`.
//!
//! Where Fig catches the error and moves on, so does this parser; see
//! [`update_state`].
//!
//! Deliberate gaps:
//! - Fig looks names up in plain objects, so a token such as `constructor`
//!   finds `Object.prototype`'s members; here it finds nothing.
//! - Fig walks option chains and offsets by UTF-16 code unit; here by char.
//! - Nested hand-offs stop after [`MAX_DEPTH`] (Fig would recurse forever).

use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use super::model::{
    Arg, CommandOption, Deprecated, FilterStrategy, Generator, LoadSpec, Repeatable,
    RequiresSeparator, StaticSuggestion, StringOrFn, Subcommand, SubcommandDirectives,
    SuggestionType, Template,
};
use super::runtime::LoadedSpec;

/// How many specs a line may hand off through (`sudo time git ...`).
pub const MAX_DEPTH: usize = 16;

/// The priority Fig gives an option that a passed option depends on and
/// that is not passed yet (`suggestions/index.ts:156`).
const DEPENDED_ON_PRIORITY: f64 = 75.0;

/// What the caller has loaded and answered so far.
pub trait Resolved {
    /// A spec loaded by name (for isCommand / isScript hand-off and loadSpec
    /// strings); None = not loaded yet.
    fn spec(&self, name: &str) -> Option<Arc<LoadedSpec>>;
    /// The cached answer of a function need (loadSpec fn, generateSpec,
    /// alias), keyed as the Need says; None = not answered yet.
    fn answer(&self, need: &Need) -> Option<Arc<Answer>>;
}

#[derive(Debug, Clone, PartialEq)]
pub enum ParseResult {
    Ready(Suggest),
    Needs(Need),
}

/// Something the parse cannot go on without.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Need {
    /// Load this spec. When it does not exist Fig gives up on the line, and
    /// so should the caller (no spec suggestions).
    Spec { name: String },
    /// Call `loadSpec` function `fn_id` of `spec` with `(token, exec)`.
    LoadSpecFn {
        spec: String,
        fn_id: u32,
        token: String,
    },
    /// Call `generateSpec` function `fn_id` of `spec` with `(tokens, exec)`:
    /// every token from the spec's command word to the word being typed.
    /// Fig keeps its parse, and so this answer, per line up to the last token
    /// (`getCacheKey`, `parseArguments.ts:767-777`), so a caller may reuse an
    /// answer for the same `tokens` but the last.
    GenerateSpec {
        spec: String,
        fn_id: u32,
        tokens: Vec<String>,
    },
    /// Call `parserDirectives.alias` function `fn_id` of `spec` with
    /// `(token, exec)`.
    Alias {
        spec: String,
        fn_id: u32,
        token: String,
    },
}

/// What a function need resolves to.
// Answers live behind an `Arc`, so the variants' sizes do not matter.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum Answer {
    /// A `loadSpec` function's result or a `generateSpec` result, as a
    /// subcommand. A `loadSpec` result that is a spec location (or a list of
    /// them: Fig uses the first) is given as a subcommand with no name whose
    /// `load_spec` is that location's [`LoadSpec::Name`]; the parse then
    /// hands off to that spec. A local location is [`Answer::Failed`], and so
    /// is a bare string (Fig's `convertLoadSpec` throws on one).
    Spec(Subcommand),
    /// An alias's replacement tokens; see [`alias_tokens`].
    Alias(Vec<String>),
    /// The function threw or timed out.
    Failed,
}

/// Everything the engine needs at the cursor.
#[derive(Debug, Clone, PartialEq)]
pub struct Suggest {
    /// The spec whose state the parse ended in (for function calls).
    pub spec: String,
    /// Its load ([`LoadedSpec::generation`]).
    pub generation: u64,
    /// The hand-off command name (recency key), e.g. "git" for `sudo git ch`.
    pub command: String,
    /// The tokens from the spec's command word to the word being typed, as
    /// Fig hands them to a generator (`tokenArray`,
    /// `state/generators.ts:29-33`).
    pub tokens: Vec<String>,
    /// Subcommands, options not yet used, arg suggestions and
    /// `additionalSuggestions`, in Fig's order: subcommands, arg suggestions,
    /// additional suggestions, options, each group but the arg suggestions
    /// sorted by first name as JavaScript's `localeCompare` does. Hidden
    /// items are kept; the engine shows them only on an exact match. The
    /// generators' results go after the arg suggestions (the engine adds
    /// them).
    pub items: Vec<Item>,
    /// The arg being typed, if any.
    pub arg: Option<CurrentArg>,
    pub filter_strategy: Option<FilterStrategy>,
    /// Byte offset into `current` where the query term starts by default
    /// (after `--opt=`, or after the options of `-pnf10`), before any
    /// `getQueryTerm`.
    pub query_start: usize,
    /// The word being typed is an option chain (`-la`): Fig's rule
    /// (`suggestions/index.ts:233-287`) has been applied to `items` (only
    /// options and args are left, short options renamed to extend the
    /// chain); generator results still need it: keep only those of type
    /// `arg` or `option`, and match `arg` ones (static ones in `items` too)
    /// against an empty query.
    pub in_option_chain: bool,
}

impl Suggest {
    /// What Fig shows when its parse throws: nothing from the spec.
    fn failed(spec: &LoadedSpec) -> Self {
        Self {
            spec: spec.name.clone(),
            generation: spec.generation,
            command: spec.name.clone(),
            tokens: Vec::new(),
            items: Vec::new(),
            arg: None,
            filter_strategy: None,
            query_start: 0,
            in_option_chain: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    Subcommand,
    Option,
    ArgSuggestion,
    /// A generator's or a template's result (merged by the engine).
    Generated,
    Additional,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub kind: ItemKind,
    pub names: Vec<String>,
    pub display_name: Option<String>,
    pub insert_value: Option<String>,
    /// Shared with the spec's tree: a parse does not copy it.
    pub description: Option<Arc<str>>,
    pub icon: Option<String>,
    pub priority: Option<f64>,
    pub hidden: bool,
    pub deprecated: bool,
    pub is_dangerous: bool,
    pub suggestion_type: Option<SuggestionType>,
    /// The item's own args as Fig shows them (`Suggestion.tsx:226-232`):
    /// `<name> ` (with its trailing space) for a required arg, `[name]` for
    /// an optional one, `...` after a variadic arg's name, joined by spaces;
    /// args without a name are left out. Empty if none.
    pub args_hint: String,
    /// Fig's `separatorToAdd` (`suggestions/index.ts:160-179`): what an
    /// option that needs its separator (`requiresSeparator`,
    /// `requiresEquals`) is inserted with, the cursor after it.
    pub separator: Option<String>,
    /// Fig's `shouldAddSpace` (`suggestions/index.ts:45-68`): a subcommand
    /// or option is followed by a space when its first arg is mandatory,
    /// else as its `requiresSubcommand` says when it gives one, else when it
    /// has subcommands. Arg values never are.
    pub should_add_space: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CurrentArg {
    /// Unique: spec name + tree path of the Arg, e.g.
    /// "git:subcommands.12.args.0" (identity for generator memo). The path
    /// is the one `walk.js` gives the arg's functions; an args list given as
    /// a single object has no index segment when its functions show that,
    /// and an index otherwise. Args from an answer carry the function call
    /// in their path, e.g. `generateSpec(["fig", ""])`.
    pub path: String,
    pub name: Option<String>,
    pub description: Option<Arc<str>>,
    pub templates: Vec<Template>,
    /// Empty when the arg has a `template`: Fig then ignores its
    /// `generators` (`initializeArgMeta` in `@fig/autocomplete-shared`).
    pub generators: Vec<GeneratorRef>,
    pub suggest_current_token: bool,
    pub debounce: bool,
    pub is_variadic: bool,
    pub filter_strategy: Option<FilterStrategy>,
    /// The arg's `isDangerous`, handed to its generators.
    pub is_dangerous: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GeneratorRef {
    /// The generator's index in the arg's `generators`.
    pub index: usize,
    /// A clone; FnRefs inside are this spec's ids.
    pub generator: Generator,
}

/// Parse a command line against `root`, the spec of its command word.
///
/// `words` are the words between the command word and the one being typed,
/// unquoted (the tokenizer's `words_before` without its first word); empty
/// when only the command name was typed. `current` is the word being typed,
/// up to the cursor. Fig's token list is the spec's name, `words`, then
/// `current`.
pub fn parse(
    root: &LoadedSpec,
    words: &[String],
    current: &str,
    resolved: &dyn Resolved,
) -> ParseResult {
    match outcome(root, words, current, resolved) {
        Ok(suggest) => ParseResult::Ready(suggest),
        Err(Stop::Needs(need)) => ParseResult::Needs(need),
        Err(Stop::Fail) => ParseResult::Ready(Suggest::failed(root)),
    }
}

fn outcome(
    root: &LoadedSpec,
    words: &[String],
    current: &str,
    resolved: &dyn Resolved,
) -> Result<Suggest, Stop> {
    let mut tokens = Vec::with_capacity(words.len() + 2);
    tokens.push(root.name.clone());
    tokens.extend(words.iter().cloned());
    tokens.push(current.to_owned());
    Parser { resolved }.frame(root, tokens, root.name.clone(), 0)
}

/// The words an alias stands for, split the way Fig's shell parser splits
/// the line once the alias is put in (`substituteAlias`,
/// `shell-parser/src/command.ts`): on unquoted blanks, with quotes and
/// backslashes removed. None when Fig's substitution fails: the alias is
/// empty, or it is not one simple command (an unquoted `;`, `&`, `|` or
/// newline).
pub fn alias_tokens(alias: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = alias.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            '\n' | ';' | '&' | '|' => return None,
            '\\' => {
                in_word = true;
                if let Some(escaped) = chars.next() {
                    word.push(escaped);
                }
            }
            '\'' => {
                in_word = true;
                for quoted in chars.by_ref() {
                    if quoted == '\'' {
                        break;
                    }
                    word.push(quoted);
                }
            }
            // `$'...'`: Fig's parser keeps the text between the marks as is.
            '$' if chars.peek() == Some(&'\'') => {
                in_word = true;
                chars.next();
                for quoted in chars.by_ref() {
                    if quoted == '\'' {
                        break;
                    }
                    word.push(quoted);
                }
            }
            '"' => {
                in_word = true;
                while let Some(quoted) = chars.next() {
                    match quoted {
                        '"' => break,
                        '\\' => match chars.peek() {
                            Some(&next) if "$`\"\\\n".contains(next) => {
                                word.push(next);
                                chars.next();
                            }
                            _ => word.push('\\'),
                        },
                        other => word.push(other),
                    }
                }
            }
            other => {
                in_word = true;
                word.push(other);
            }
        }
    }
    if in_word {
        words.push(word);
    }
    (!words.is_empty()).then_some(words)
}

/// Why a parse stopped early.
#[derive(Debug, Clone, PartialEq)]
enum Stop {
    Needs(Need),
    /// Fig's parse threw.
    Fail,
}

/// Fig's `UpdateStateError`: this token cannot be read this way. Always
/// caught by a caller that tries the next way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Reject;

/// `TokenType` (`parseArguments.ts:42-51`), without `Composite`, which is
/// [`Ann::Composite`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TokenType {
    None,
    Subcommand,
    Option,
    OptionArg,
    SubcommandArg,
}

/// The last annotation; Fig reads no other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ann {
    Basic(TokenType),
    /// An option chain or an option with its arg in one token. Its last
    /// subtoken, `Option` or `OptionArg`, with the byte offset where that
    /// subtoken's text starts in the token; None for the empty chain of a
    /// lone `-`.
    Composite(Option<(TokenType, usize)>),
}

/// Where a spec location points (`getSpecPath`, `loadSpec.ts:59-101`).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Location {
    Global(String),
    /// A spec in a `.fig` folder; fac has none.
    Local,
}

/// One source of subcommands and options: a subcommand, or a generated spec
/// merged into one.
#[derive(Debug, Clone)]
struct Layer<'a> {
    sub: &'a Subcommand,
    path: Rc<str>,
}

/// Fig's `completionObj` (an `Internal.Subcommand`) as the parse sees it.
#[derive(Debug)]
struct Node<'a> {
    /// The subcommand itself: its name, `loadSpec`, `generateSpec`,
    /// `additionalSuggestions`, `filterStrategy` and `isDangerous`.
    sub: &'a Subcommand,
    path: Rc<str>,
    /// `parserDirectives`, own or inherited.
    directives: Option<Cow<'a, SubcommandDirectives>>,
    /// Where `subcommands` and the non-persistent `options` come from, later
    /// layers winning: the subcommand, then any generated specs.
    layers: Vec<Layer<'a>>,
    /// Where `persistentOptions` come from, later layers winning: the
    /// subcommand, its ancestors' (`Object.assign`, `parseArguments.ts:
    /// 293-296`), then any generated specs.
    persistent: Vec<Layer<'a>>,
    /// `args`: the subcommand's own, or a generated spec's.
    args: Option<ArgList<'a>>,
}

#[derive(Debug, Clone)]
struct ArgList<'a> {
    args: &'a [Arg],
    /// Tree path of the `args` field, e.g. `subcommands.3.args`.
    base: Rc<str>,
}

/// `ArgArrayState` (`parseArguments.ts:36-40`).
#[derive(Debug, Clone, Default)]
struct ArgState<'a> {
    list: Option<ArgList<'a>>,
    index: usize,
    /// `variadicCount`; 0 for undefined.
    variadic: u32,
}

impl<'a> ArgState<'a> {
    /// `createArgState` (`parseArguments.ts:102-134`).
    fn new(args: &'a [Arg], base: Rc<str>) -> Self {
        Self {
            list: (!args.is_empty()).then_some(ArgList { args, base }),
            index: 0,
            variadic: 0,
        }
    }

    fn from_list(list: Option<&ArgList<'a>>) -> Self {
        Self {
            list: list.cloned(),
            index: 0,
            variadic: 0,
        }
    }

    /// `getCurrentArg` (`parseArguments.ts:178-179`).
    fn current(&self) -> Option<&'a Arg> {
        let list = self.list.as_ref()?;
        let args: &'a [Arg] = list.args;
        args.get(self.index)
    }

    fn len(&self) -> usize {
        self.list.as_ref().map_or(0, |list| list.args.len())
    }

    /// `updateArgState` (`parseArguments.ts:163-176`): consume an arg.
    fn advance(&self) -> Self {
        if let Some(list) = &self.list
            && let Some(arg) = list.args.get(self.index)
        {
            if arg.is_variadic {
                return Self {
                    list: self.list.clone(),
                    index: self.index,
                    variadic: self.variadic + 1,
                };
            }
            if self.index < list.args.len() - 1 {
                return Self {
                    list: self.list.clone(),
                    index: self.index + 1,
                    variadic: 0,
                };
            }
        }
        Self::default()
    }
}

/// `ArgumentParserState` (`parseArguments.ts:77-89`), keeping only the last
/// annotation.
#[derive(Debug, Clone)]
struct State<'a> {
    node: Rc<Node<'a>>,
    option_args: ArgState<'a>,
    subcommand_args: ArgState<'a>,
    last: Ann,
    passed: Vec<&'a CommandOption>,
    entered_subcommand_args: bool,
    end_of_options: bool,
}

/// `isMandatoryOrVariadic` (`parseArguments.ts:181-182`).
fn mandatory_or_variadic(arg: Option<&Arg>) -> bool {
    arg.is_some_and(|arg| arg.is_variadic || !arg.is_optional)
}

/// `preferOptionArg` (`parseArguments.ts:184-186`).
fn prefer_option_arg(state: &State) -> bool {
    mandatory_or_variadic(state.option_args.current()) || state.subcommand_args.current().is_none()
}

/// `getArgState` (`parseArguments.ts:188-189`).
fn arg_state<'s, 'a>(state: &'s State<'a>) -> &'s ArgState<'a> {
    if prefer_option_arg(state) {
        &state.option_args
    } else {
        &state.subcommand_args
    }
}

/// `canConsumeOptions` (`parseArguments.ts:191-238`).
fn can_consume_options(state: &State) -> bool {
    let must_precede = state
        .node
        .directives
        .as_ref()
        .is_some_and(|directives| directives.options_must_precede_arguments);
    if state.entered_subcommand_args && must_precede {
        return false;
    }
    if state.end_of_options {
        return false;
    }
    let subcommand_arg = state.subcommand_args.current();
    let option_arg = state.option_args.current();
    if mandatory_or_variadic(option_arg) {
        // Options may still break a variadic option arg once it has one value.
        return option_arg.is_some_and(|arg| {
            arg.is_variadic
                && state.option_args.variadic > 0
                && arg.options_can_break_variadic_arg != Some(false)
        });
    }
    if let Some(arg) = subcommand_arg
        && state.subcommand_args.variadic > 0
        && arg.options_can_break_variadic_arg == Some(false)
    {
        return false;
    }
    true
}

/// `canConsumeSubcommands` (`parseArguments.ts:538-540`).
fn can_consume_subcommands(state: &State) -> bool {
    !mandatory_or_variadic(state.option_args.current()) && !state.entered_subcommand_args
}

/// `a.b` from a tree path and a tail; the root's path is empty.
fn join(base: &str, tail: &str) -> Rc<str> {
    if base.is_empty() {
        Rc::from(tail)
    } else {
        Rc::from(format!("{base}.{tail}"))
    }
}

/// `findSubcommand` (`parseArguments.ts:251-260`) over the merged
/// `subcommands` map: the last subcommand to claim a name owns it.
fn find_subcommand<'a>(node: &Node<'a>, token: &str) -> Option<(&'a Subcommand, Rc<str>)> {
    node.layers.iter().rev().find_map(|layer| {
        let subs: &'a [Subcommand] = &layer.sub.subcommands;
        let (at, sub) = subs
            .iter()
            .enumerate()
            .rev()
            .find(|(_, sub)| sub.name.iter().any(|name| name == token))?;
        Some((sub, join(&layer.path, &format!("subcommands.{at}"))))
    })
}

/// `findOption` (`parseArguments.ts:240-249`): `options`, then
/// `persistentOptions`.
fn find_option<'a>(node: &Node<'a>, token: &str) -> Option<(&'a CommandOption, Rc<str>)> {
    let find = |layers: &[Layer<'a>], persistent: bool| {
        layers.iter().rev().find_map(|layer| {
            let options: &'a [CommandOption] = &layer.sub.options;
            let (at, option) = options.iter().enumerate().rev().find(|(_, option)| {
                option.is_persistent == persistent && option.name.iter().any(|name| name == token)
            })?;
            Some((option, join(&layer.path, &format!("options.{at}"))))
        })
    };
    find(&node.layers, false).or_else(|| find(&node.persistent, true))
}

/// `optionsAreEqual` (`parseArguments.ts:151-152`).
fn options_are_equal(a: &CommandOption, b: &CommandOption) -> bool {
    a.name.iter().any(|name| b.name.contains(name))
}

/// `countEqualOptions` (`parseArguments.ts:154-161`).
fn count_equal_options(option: &CommandOption, passed: &[&CommandOption]) -> u32 {
    passed
        .iter()
        .filter(|other| options_are_equal(option, other))
        .count() as u32
}

/// `updateStateForSubcommand` (`parseArguments.ts:262-307`).
fn update_for_subcommand<'a>(
    state: &State<'a>,
    token: &str,
    is_final: bool,
) -> Result<State<'a>, Reject> {
    if state.entered_subcommand_args {
        return Err(Reject);
    }
    let (child, path) = find_subcommand(&state.node, token).ok_or(Reject)?;
    let mut next = state.clone();
    next.last = Ann::Basic(TokenType::Subcommand);
    if is_final {
        return Ok(next);
    }
    let parent = &state.node;
    // parserDirectives and persistent options carry down (lines 287-296).
    let directives = match &child.parser_directives {
        Some(own) => Some(Cow::Borrowed(own)),
        None => parent.directives.clone(),
    };
    let own = Layer {
        sub: child,
        path: Rc::clone(&path),
    };
    let mut persistent = Vec::with_capacity(parent.persistent.len() + 1);
    persistent.push(own.clone());
    persistent.extend(parent.persistent.iter().cloned());
    let args = (!child.args.is_empty()).then(|| ArgList {
        args: &child.args,
        base: join(&path, "args"),
    });
    next.subcommand_args = ArgState::from_list(args.as_ref());
    next.node = Rc::new(Node {
        sub: child,
        path,
        directives,
        layers: vec![own],
        persistent,
        args,
    });
    next.passed.clear();
    next.option_args = ArgState::default();
    Ok(next)
}

/// `updateStateForOption` (`parseArguments.ts:309-344`).
fn update_for_option<'a>(
    state: &State<'a>,
    token: &str,
    is_final: bool,
) -> Result<State<'a>, Reject> {
    let (option, path) = find_option(&state.node, token).ok_or(Reject)?;
    let limit = match option.is_repeatable {
        None | Some(Repeatable::Flag(true)) => None,
        Some(Repeatable::Flag(false)) => Some(1),
        Some(Repeatable::Times(times)) => Some(times),
    };
    if let Some(limit) = limit
        && count_equal_options(option, &state.passed) >= limit
    {
        return Err(Reject);
    }
    let mut next = state.clone();
    next.last = Ann::Basic(TokenType::Option);
    if is_final {
        return Ok(next);
    }
    next.passed.push(option);
    next.option_args = ArgState::new(&option.args, join(&path, "args"));
    Ok(next)
}

/// `updateStateForOptionArg` (`parseArguments.ts:346-369`).
fn update_for_option_arg<'a>(state: &State<'a>, is_final: bool) -> Result<State<'a>, Reject> {
    if state.option_args.current().is_none() {
        return Err(Reject);
    }
    let mut next = state.clone();
    next.last = Ann::Basic(TokenType::OptionArg);
    if !is_final {
        next.option_args = state.option_args.advance();
    }
    Ok(next)
}

/// `updateStateForSubcommandArg` (`parseArguments.ts:371-396`).
fn update_for_subcommand_arg<'a>(state: &State<'a>, is_final: bool) -> Result<State<'a>, Reject> {
    if state.subcommand_args.current().is_none() {
        return Err(Reject);
    }
    let mut next = state.clone();
    next.last = Ann::Basic(TokenType::SubcommandArg);
    if !is_final {
        next.subcommand_args = state.subcommand_args.advance();
        next.entered_subcommand_args = true;
    }
    Ok(next)
}

/// `updateStateForChainedOptionToken` (`parseArguments.ts:398-536`): `--`,
/// a long option (`--num=10` too), or a chain of short ones (`-abc`,
/// `-pnf10`).
fn update_for_option_token<'a>(
    state: &State<'a>,
    token: &str,
    is_final: bool,
) -> Result<State<'a>, Reject> {
    if is_final && (token == "-" || token == "--") {
        return Err(Reject);
    }
    if token == "--" {
        let mut next = state.clone();
        next.end_of_options = true;
        next.last = Ann::Basic(TokenType::Option);
        next.option_args = ArgState::default();
        return Ok(next);
    }
    let directives = state.node.directives.as_deref();
    let long = directives.is_some_and(|directives| directives.flags_are_posix_noncompliant)
        || token.starts_with("--")
        || !token.starts_with('-');
    if long {
        // `new Set(parserDirectives?.optionArgSeparators || "=")`, matched
        // one character at a time (`firstMatchingToken`). An empty list reads
        // as absent: the model cannot tell them apart.
        let separators = directives
            .map(|directives| directives.option_arg_separators.as_slice())
            .filter(|separators| !separators.is_empty());
        let is_separator = |c: char| match separators {
            Some(separators) => separators.iter().any(|separator| {
                let mut chars = separator.chars();
                chars.next() == Some(c) && chars.next().is_none()
            }),
            None => c == '=',
        };
        if let Some((at, separator)) = token.char_indices().find(|&(_, c)| is_separator(c)) {
            // `--num=10`: the option, then its arg (lines 435-471). The option
            // is always consumed, even in the last token.
            let option_state = update_for_option(state, &token[..at], false)?;
            if option_state.option_args.len() > 1 {
                return Err(Reject);
            }
            let mut next = update_for_option_arg(&option_state, is_final)?;
            next.last = Ann::Composite(Some((TokenType::OptionArg, at + separator.len_utf8())));
            return Ok(next);
        }
        let mut next = update_for_option(state, token, is_final)?;
        let takes_separator = find_option(&state.node, token).is_some_and(|(option, _)| {
            option.requires_equals
                || match &option.requires_separator {
                    Some(RequiresSeparator::Flag(flag)) => *flag,
                    Some(RequiresSeparator::Separator(separator)) => !separator.is_empty(),
                    None => false,
                }
        });
        if takes_separator {
            next.option_args = ArgState::default();
        }
        return Ok(next);
    }

    // A chain of short options (lines 482-535), consumed even in the last
    // token.
    let mut option_state = state.clone();
    let mut option_arg = None;
    let mut last_option = None;
    let mut passed = state.passed.clone();
    for (at, c) in token.char_indices().skip(1) {
        passed.clone_from(&option_state.passed);
        match update_for_option(&option_state, &format!("-{c}"), false) {
            Ok(next) => option_state = next,
            // A character that is no option starts the last option's arg.
            Err(_) if at > 1 => {
                option_arg = Some(at);
                break;
            }
            Err(reject) => return Err(reject),
        }
        last_option = Some(at);
        if mandatory_or_variadic(option_state.option_args.current()) {
            option_arg = Some(at + c.len_utf8());
            break;
        }
    }
    let mut last = last_option.map(|at| (TokenType::Option, at));
    if let Some(at) = option_arg.filter(|&at| at < token.len()) {
        if option_state.option_args.len() > 1 {
            return Err(Reject);
        }
        option_state = update_for_option_arg(&option_state, is_final)?;
        passed.clone_from(&option_state.passed);
        last = Some((TokenType::OptionArg, at));
    }
    if !is_final {
        passed = std::mem::take(&mut option_state.passed);
    }
    option_state.passed = passed;
    option_state.last = Ann::Composite(last);
    Ok(option_state)
}

/// The parser's state machine, `updateState` (`parseArguments.ts:543-573`):
/// a subcommand, else an option, else an option arg, else a subcommand arg;
/// the first that takes the token wins, and each one's errors are caught.
/// Only the last way's error reaches the caller.
fn update_state<'a>(state: &State<'a>, token: &str, is_final: bool) -> Result<State<'a>, Reject> {
    if can_consume_subcommands(state)
        && let Ok(next) = update_for_subcommand(state, token, is_final)
    {
        return Ok(next);
    }
    if can_consume_options(state)
        && let Ok(next) = update_for_option_token(state, token, is_final)
    {
        return Ok(next);
    }
    if prefer_option_arg(state)
        && let Ok(next) = update_for_option_arg(state, is_final)
    {
        return Ok(next);
    }
    update_for_subcommand_arg(state, is_final)
}

/// `getInitialState` (`parseArguments.ts:575-594`).
fn initial_state(root: &Subcommand) -> State<'_> {
    let path: Rc<str> = Rc::from("");
    let args = (!root.args.is_empty()).then(|| ArgList {
        args: &root.args,
        base: Rc::from("args"),
    });
    let layer = Layer {
        sub: root,
        path: Rc::clone(&path),
    };
    State {
        subcommand_args: ArgState::from_list(args.as_ref()),
        node: Rc::new(Node {
            sub: root,
            path,
            directives: root.parser_directives.as_ref().map(Cow::Borrowed),
            layers: vec![layer.clone()],
            persistent: vec![layer],
            args,
        }),
        option_args: ArgState::default(),
        last: Ann::Basic(TokenType::Subcommand),
        passed: Vec::new(),
        entered_subcommand_args: false,
        end_of_options: false,
    }
}

/// A `loadSpec` subcommand taking over (`parseArguments.ts:883-898`). Its
/// `parserDirectives` are merged over the current ones; the model cannot
/// tell an explicit `false` from an absent flag, so a flag set on either
/// side holds.
fn inline<'a>(state: &State<'a>, sub: &'a Subcommand, path: Rc<str>) -> State<'a> {
    let directives = match (&state.node.directives, &sub.parser_directives) {
        (None, None) => None,
        (Some(parent), None) => Some(parent.clone()),
        (None, Some(own)) => Some(Cow::Borrowed(own)),
        (Some(parent), Some(own)) => Some(Cow::Owned(SubcommandDirectives {
            flags_are_posix_noncompliant: own.flags_are_posix_noncompliant
                || parent.flags_are_posix_noncompliant,
            options_must_precede_arguments: own.options_must_precede_arguments
                || parent.options_must_precede_arguments,
            option_arg_separators: if own.option_arg_separators.is_empty() {
                parent.option_arg_separators.clone()
            } else {
                own.option_arg_separators.clone()
            },
        })),
    };
    let args = (!sub.args.is_empty()).then(|| ArgList {
        args: &sub.args,
        base: join(&path, "args"),
    });
    let layer = Layer {
        sub,
        path: Rc::clone(&path),
    };
    State {
        subcommand_args: ArgState::from_list(args.as_ref()),
        node: Rc::new(Node {
            sub,
            path,
            directives,
            layers: vec![layer.clone()],
            persistent: vec![layer],
            args,
        }),
        option_args: ArgState::default(),
        passed: Vec::new(),
        entered_subcommand_args: false,
        last: state.last,
        end_of_options: state.end_of_options,
    }
}

/// A generated spec merged in (`generateSpecForState`,
/// `parseArguments.ts:665-682`): its subcommands and options are added, its
/// args used only when the subcommand has none.
fn generate_merge<'a>(state: &State<'a>, generated: &'a Subcommand, path: Rc<str>) -> State<'a> {
    let node = &state.node;
    let layer = Layer {
        sub: generated,
        path: Rc::clone(&path),
    };
    let mut layers = node.layers.clone();
    layers.push(layer.clone());
    let mut persistent = node.persistent.clone();
    persistent.push(layer);
    let keep_args = node.args.is_some();
    let args = if keep_args {
        node.args.clone()
    } else {
        (!generated.args.is_empty()).then(|| ArgList {
            args: &generated.args,
            base: join(&path, "args"),
        })
    };
    let mut next = state.clone();
    if !keep_args {
        next.subcommand_args = ArgState::from_list(args.as_ref());
    }
    next.node = Rc::new(Node {
        sub: node.sub,
        path: Rc::clone(&node.path),
        directives: node.directives.clone(),
        layers,
        persistent,
        args,
    });
    next
}

/// `getSpecPath` for an arg's token (`loadSpec.ts:59-101`, `isScript`
/// given). `+` is Fig's default personal shortcuts token.
fn spec_path(token: &str, is_script: bool) -> Location {
    if token == "?" || token == "+" {
        return Location::Local;
    }
    let dir = &token[..token.rfind('/').map_or(0, |at| at + 1)];
    if !is_script
        && !["/", "./", "~/"]
            .iter()
            .any(|prefix| dir.starts_with(prefix))
    {
        Location::Global(token.to_owned())
    } else {
        Location::Local
    }
}

/// The spec named by a location-form `loadSpec` answer (see
/// [`Answer::Spec`]).
fn location_name(sub: &Subcommand) -> Option<String> {
    match &sub.load_spec {
        Some(LoadSpec::Name(name)) if sub.name.is_empty() => Some(name.clone()),
        _ => None,
    }
}

/// The path segment of element `index` of an args list at `base` with `len`
/// elements, as `walk.js` gives it: a one-element list may have been a
/// single object (no index); the spec's function paths tell.
fn element_path(fns: &[String], base: &str, index: usize, len: usize) -> String {
    if len == 1 {
        let single = fns.iter().any(|path| {
            path.strip_prefix(base)
                .and_then(|rest| rest.strip_prefix('.'))
                .is_some_and(|rest| !rest.starts_with(|c: char| c.is_ascii_digit()))
        });
        if single {
            return base.to_owned();
        }
    }
    format!("{base}.{index}")
}

/// What to load after a token (`parseArguments.ts:971-1001`).
enum Target<'a> {
    Spec(&'a LoadSpec, Rc<str>),
    Location(Location, Option<String>),
}

/// One `parseArgumentsCached` call: a spec and the tokens from its command
/// word on.
struct Frame<'s> {
    spec: &'s LoadedSpec,
    tokens: Vec<String>,
    command: String,
    depth: usize,
    /// An alias was substituted in this frame: Fig's `currentCommand` is no
    /// longer its `command`.
    substituted: bool,
    substituted_aliases: Vec<String>,
    alias_error: bool,
}

struct Parser<'r> {
    resolved: &'r dyn Resolved,
}

impl Parser<'_> {
    fn answer(&self, need: Need) -> Result<Arc<Answer>, Stop> {
        self.resolved.answer(&need).ok_or(Stop::Needs(need))
    }

    /// `parseArgumentsCached` (`parseArguments.ts:780-1024`), then the last
    /// token (`parseArguments.ts:1131-1143`).
    fn frame(
        &self,
        spec: &LoadedSpec,
        tokens: Vec<String>,
        command: String,
        depth: usize,
    ) -> Result<Suggest, Stop> {
        if depth > MAX_DEPTH {
            return Err(Stop::Fail);
        }
        let mut frame = Frame {
            spec,
            tokens,
            command,
            depth,
            substituted: false,
            substituted_aliases: Vec::new(),
            alias_error: false,
        };
        let state = initial_state(&spec.root);
        // The root's own loadSpec, with no token (line 903): a function is
        // not called.
        match &spec.root.load_spec {
            Some(LoadSpec::Name(name)) => {
                self.hand_off(&frame, Location::Global(name.clone()), 0, None)
            }
            Some(LoadSpec::Inline(sub)) => {
                let state = inline(&state, sub, Rc::from("loadSpec"));
                self.run(&mut frame, state, 1, false)
            }
            Some(LoadSpec::Fn(_)) | None => self.run(&mut frame, state, 1, false),
        }
    }

    /// Parse a spec location's spec from token `index` on, that token being
    /// its command word (lines 870-881).
    fn hand_off(
        &self,
        frame: &Frame,
        location: Location,
        index: usize,
        command: Option<String>,
    ) -> Result<Suggest, Stop> {
        let Location::Global(name) = location else {
            return Err(Stop::Fail);
        };
        let Some(spec) = self.resolved.spec(&name) else {
            return Err(Stop::Needs(Need::Spec { name }));
        };
        let tokens = frame.tokens.get(index..).unwrap_or_default().to_vec();
        let command = command.unwrap_or_else(|| frame.command.clone());
        self.frame(&spec, tokens, command, frame.depth + 1)
    }

    /// The token loop (lines 907-1014) from token `i`, with the generated
    /// spec for token `i` already merged when `generated`.
    fn run<'b>(
        &self,
        frame: &mut Frame<'_>,
        mut state: State<'b>,
        mut i: usize,
        mut generated: bool,
    ) -> Result<Suggest, Stop> {
        while i < frame.tokens.len() {
            if !generated && let Some(id) = state.node.sub.generate_spec {
                let need = Need::GenerateSpec {
                    spec: frame.spec.name.clone(),
                    fn_id: id.0,
                    tokens: frame.tokens.clone(),
                };
                let answer = self.answer(need)?;
                // A failed generateSpec is logged and the state kept (lines
                // 683-693).
                if let Answer::Spec(spec) = &*answer {
                    let before_last = &frame.tokens[..frame.tokens.len() - 1];
                    let path = join(&state.node.path, &format!("generateSpec({before_last:?})"));
                    let merged = generate_merge(&state, spec, path);
                    return self.run(frame, merged, i, true);
                }
            }
            generated = false;
            if i == frame.tokens.len() - 1 {
                // The last token is read in `finish`.
                break;
            }
            let token = frame.tokens[i].clone();
            let last_list = arg_state(&state);
            let last_arg = last_list
                .current()
                .map(|arg| (arg, last_list.list.clone(), last_list.index));
            let last_arg_type = if prefer_option_arg(&state) {
                TokenType::OptionArg
            } else {
                TokenType::SubcommandArg
            };
            let last_state = state.clone();
            state = update_state(&state, &token, false).map_err(|_| Stop::Fail)?;
            let last_type = match state.last {
                Ann::Basic(kind) | Ann::Composite(Some((kind, _))) => kind,
                // Fig reads the last subtoken of an empty chain and throws.
                Ann::Composite(None) => return Err(Stop::Fail),
            };

            // parserDirectives.alias (lines 942-969).
            if last_type == last_arg_type
                && let Some((arg, ..)) = &last_arg
                && let Some(alias) = arg
                    .parser_directives
                    .as_ref()
                    .and_then(|d| d.alias.as_ref())
                    .filter(|alias| !matches!(alias, StringOrFn::String(text) if text.is_empty()))
                && !frame.substituted_aliases.contains(&token)
            {
                match self.alias(frame, alias, &token)? {
                    Some(words) => {
                        frame.tokens.splice(i..=i, words);
                        frame.substituted = true;
                        frame.substituted_aliases.push(token);
                        state = last_state;
                        continue;
                    }
                    None if frame.substituted_aliases.is_empty() => return Err(Stop::Fail),
                    None => frame.alias_error = true,
                }
            }

            // loadSpec, or an arg that names a command (lines 971-1005).
            let mut target = None;
            let sub: &'b Subcommand = state.node.sub;
            if last_type == TokenType::Subcommand
                && let Some(load_spec) = &sub.load_spec
            {
                target = Some(Target::Spec(load_spec, Rc::clone(&state.node.path)));
            }
            if last_type == last_arg_type
                && let Some((arg, list, index)) = &last_arg
            {
                if let Some(load_spec) = &arg.load_spec {
                    let path = list.as_ref().map_or_else(String::new, |list| {
                        element_path(&frame.spec.fns, &list.base, *index, list.args.len())
                    });
                    target = Some(Target::Spec(load_spec, Rc::from(path)));
                } else if arg.is_command || arg.is_script {
                    target = Some(Target::Location(
                        spec_path(&token, arg.is_script),
                        Some(token.clone()),
                    ));
                } else if let Some(module) = arg.is_module.as_deref().filter(|m| !m.is_empty()) {
                    target = Some(Target::Location(
                        Location::Global(format!("{module}{token}")),
                        None,
                    ));
                }
            }
            match target {
                None => {}
                Some(Target::Spec(LoadSpec::Inline(sub), base)) => {
                    state = inline(&state, sub, join(&base, "loadSpec"));
                }
                Some(Target::Spec(LoadSpec::Name(name), _)) => {
                    return self.hand_off(frame, Location::Global(name.clone()), i, None);
                }
                Some(Target::Location(location, command)) => {
                    return self.hand_off(frame, location, i, command);
                }
                Some(Target::Spec(LoadSpec::Fn(id), base)) => {
                    let answer = self.answer(Need::LoadSpecFn {
                        spec: frame.spec.name.clone(),
                        fn_id: id.0,
                        token: token.clone(),
                    })?;
                    // A loadSpec function that throws fails the parse.
                    let Answer::Spec(sub) = &*answer else {
                        return Err(Stop::Fail);
                    };
                    if let Some(name) = location_name(sub) {
                        return self.hand_off(frame, Location::Global(name), i, None);
                    }
                    let state = inline(&state, sub, join(&base, &format!("loadSpec({token:?})")));
                    if frame.alias_error {
                        return Err(Stop::Fail);
                    }
                    frame.substituted_aliases.clear();
                    return self.run(frame, state, i + 1, false);
                }
            }
            // An alias that failed and was not used by a loadSpec (line 1009).
            if frame.alias_error {
                return Err(Stop::Fail);
            }
            frame.substituted_aliases.clear();
            i += 1;
        }
        Ok(self.finish(frame, state))
    }

    /// An alias's words, or None when Fig's substitution fails. Fig hands
    /// `substituteAlias` the frame's original command, which holds none of
    /// the tokens of an already substituted one, so a second substitution in
    /// a frame always fails (line 952).
    fn alias(
        &self,
        frame: &Frame,
        alias: &StringOrFn,
        token: &str,
    ) -> Result<Option<Vec<String>>, Stop> {
        if frame.substituted {
            return Ok(None);
        }
        Ok(match alias {
            StringOrFn::String(text) => alias_tokens(text),
            StringOrFn::Fn(id) => {
                let answer = self.answer(Need::Alias {
                    spec: frame.spec.name.clone(),
                    fn_id: id.0,
                    token: token.to_owned(),
                })?;
                match &*answer {
                    Answer::Alias(words) if !words.is_empty() => Some(words.clone()),
                    _ => None,
                }
            }
        })
    }

    /// The last token (`parseArguments.ts:1131-1143`) and what follows from
    /// the final state.
    fn finish(&self, frame: &Frame, state: State) -> Suggest {
        let current = frame.tokens.last().map_or("", String::as_str);
        let state = match update_state(&state, current, true) {
            Ok(next) => next,
            Err(_) => State {
                last: Ann::Basic(TokenType::None),
                ..state
            },
        };
        suggest(frame, &state, current)
    }
}

/// `getResultFromState` (`parseArguments.ts:696-744`) and `getAllSuggestions`
/// (`suggestions/index.ts:193-290`), without generator results.
fn suggest(frame: &Frame, state: &State, current: &str) -> Suggest {
    let mut args = arg_state(state);
    let mut query_start = 0;
    let mut only_args = state.end_of_options;
    let mut in_option_chain = false;
    if let Ann::Composite(last) = state.last {
        args = &state.option_args;
        match last {
            Some((TokenType::OptionArg, at)) => {
                query_start = at;
                only_args = true;
            }
            Some((TokenType::Option, _)) => in_option_chain = true,
            _ => {}
        }
    }
    let with_subcommands = !only_args && can_consume_subcommands(state);
    let with_options = !only_args && can_consume_options(state);
    let node = &state.node;

    let mut items = Vec::new();
    if with_subcommands {
        let names = node
            .layers
            .iter()
            .flat_map(|layer| &layer.sub.subcommands)
            .map(|sub| sub.name.len())
            .sum();
        let mut subcommands = NamedMap::with_capacity(names);
        for layer in &node.layers {
            for sub in &layer.sub.subcommands {
                for name in &sub.name {
                    subcommands.insert(name, sub);
                }
            }
        }
        let mut subcommands: Vec<Item> = subcommands
            .values()
            .into_iter()
            .map(subcommand_item)
            .collect();
        sort_by_name(&mut subcommands);
        items.extend(subcommands);
    }

    let arg = args.current();
    if let Some(arg) = arg {
        items.extend(arg.suggestions.iter().map(|suggestion| match suggestion {
            StaticSuggestion::Name(name) => named_item(
                ItemKind::ArgSuggestion,
                name,
                Some(SuggestionType::Arg),
                arg.is_dangerous,
            ),
            StaticSuggestion::Suggestion(suggestion) => suggestion_item(
                ItemKind::ArgSuggestion,
                suggestion,
                Some(SuggestionType::Arg),
            ),
        }));
    }

    let mut additional: Vec<Item> = node
        .sub
        .additional_suggestions
        .iter()
        .map(|suggestion| match suggestion {
            StaticSuggestion::Name(name) => {
                named_item(ItemKind::Additional, name, None, node.sub.is_dangerous)
            }
            StaticSuggestion::Suggestion(suggestion) => {
                suggestion_item(ItemKind::Additional, suggestion, None)
            }
        })
        .collect();
    sort_by_name(&mut additional);
    items.extend(additional);

    if with_options || in_option_chain {
        let mut options = option_items(node, &state.passed);
        let last_option = current.chars().last().map(|c| format!("-{c}"));
        // Without options, the chain's last option is still shown.
        let exact = (in_option_chain && !with_options)
            .then(|| {
                options
                    .iter()
                    .find(|option| short_name(&option.names) == last_option.as_deref())
                    .cloned()
            })
            .flatten();
        if with_options {
            items.append(&mut options);
        }
        if in_option_chain {
            option_chain(&mut items, exact, current, last_option.as_deref());
        }
    }

    Suggest {
        spec: frame.spec.name.clone(),
        generation: frame.spec.generation,
        command: frame.command.clone(),
        tokens: frame.tokens.clone(),
        items,
        arg: arg.and_then(|arg| current_arg(frame.spec, args, arg)),
        filter_strategy: node.sub.filter_strategy,
        query_start,
        in_option_chain,
    }
}

fn current_arg(spec: &LoadedSpec, state: &ArgState, arg: &Arg) -> Option<CurrentArg> {
    let list = state.list.as_ref()?;
    let path = element_path(&spec.fns, &list.base, state.index, list.args.len());
    let generators = if arg.template.is_empty() {
        arg.generators
            .iter()
            .enumerate()
            .map(|(index, generator)| GeneratorRef {
                index,
                generator: generator.clone(),
            })
            .collect()
    } else {
        Vec::new()
    };
    Some(CurrentArg {
        path: format!("{}:{path}", spec.name),
        name: arg.name.clone(),
        description: arg.description.clone(),
        templates: arg.template.clone(),
        generators,
        suggest_current_token: arg.suggest_current_token,
        debounce: arg.debounce,
        is_variadic: arg.is_variadic,
        filter_strategy: arg.filter_strategy,
        is_dangerous: arg.is_dangerous,
    })
}

/// The options to suggest (`getStaticSuggestions`,
/// `suggestions/index.ts:125-181`), sorted: `options` then
/// `persistentOptions`, without excluded ones and ones used up.
fn option_items(node: &Node, passed: &[&CommandOption]) -> Vec<Item> {
    let mut excluded = HashSet::new();
    let mut unmet = Vec::new();
    for option in passed {
        excluded.extend(option.exclusive_on.iter().map(String::as_str));
        unmet.extend(option.depends_on.iter().map(String::as_str));
    }
    for option in passed {
        unmet.retain(|name| !option.name.iter().any(|own| own == name));
    }

    let names = |layers: &[Layer], is_persistent: bool| {
        layers
            .iter()
            .flat_map(|layer| &layer.sub.options)
            .filter(|option| option.is_persistent == is_persistent)
            .map(|option| option.name.len())
            .sum()
    };
    let mut options = NamedMap::with_capacity(names(&node.layers, false));
    let mut persistent = NamedMap::with_capacity(names(&node.persistent, true));
    for (layers, map, is_persistent) in [
        (&node.layers, &mut options, false),
        (&node.persistent, &mut persistent, true),
    ] {
        for layer in layers {
            for option in &layer.sub.options {
                if option.is_persistent == is_persistent {
                    for name in &option.name {
                        map.insert(name, option);
                    }
                }
            }
        }
    }
    let mut seen = HashSet::new();
    let mut items: Vec<Item> = options
        .values()
        .into_iter()
        .chain(persistent.values())
        .filter(|option| seen.insert(std::ptr::from_ref(*option)))
        .filter(|option| {
            option
                .name
                .iter()
                .all(|name| !excluded.contains(name.as_str()))
        })
        .filter(|option| match option.is_repeatable {
            Some(Repeatable::Flag(true)) => true,
            Some(Repeatable::Times(times)) if times > 0 => {
                count_equal_options(option, passed) < times
            }
            _ => count_equal_options(option, passed) < 1,
        })
        .map(|option| {
            let mut item = option_item(option, node.directives.as_deref());
            if option
                .name
                .iter()
                .any(|name| unmet.contains(&name.as_str()))
            {
                item.priority = Some(DEPENDED_ON_PRIORITY);
            }
            item
        })
        .collect();
    sort_by_name(&mut items);
    items
}

/// The option chain rule (`suggestions/index.ts:233-287`): only options and
/// args, and short options renamed to extend the chain `prefix`, whose last
/// option is `last_option`.
fn option_chain(
    items: &mut Vec<Item>,
    exact: Option<Item>,
    prefix: &str,
    last_option: Option<&str>,
) {
    let is = |item: &Item, kind: &str| {
        item.suggestion_type
            .as_ref()
            .is_some_and(|ty| ty.as_str() == kind)
    };
    items.retain(|item| is(item, "option") || is(item, "arg"));
    items.extend(exact);
    for item in items.iter_mut() {
        if is(item, "arg") {
            continue;
        }
        let Some(short) = short_name(&item.names).map(str::to_owned) else {
            continue;
        };
        if Some(short.as_str()) == last_option {
            for name in &mut item.names {
                if *name == short {
                    *name = prefix.to_owned();
                }
            }
        } else if let Some(letter) = short.chars().nth(1) {
            let name = format!("{prefix}{letter}");
            item.insert_value = Some(name.clone());
            item.names = vec![name];
        }
    }
}

/// A short option's name: `-` and one character.
fn short_name(names: &[String]) -> Option<&str> {
    names
        .iter()
        .find(|name| {
            name.starts_with('-') && !name.starts_with("--") && name.encode_utf16().count() == 2
        })
        .map(String::as_str)
}

fn is_deprecated(deprecated: &Option<Deprecated>) -> bool {
    match deprecated {
        None => false,
        Some(Deprecated::Flag(flag)) => *flag,
        Some(Deprecated::Info(_)) => true,
    }
}

/// `Suggestion.tsx:226-232`.
fn args_hint(args: &[Arg]) -> String {
    let mut hint = String::new();
    for arg in args {
        let Some(name) = arg.name.as_deref().filter(|name| !name.is_empty()) else {
            continue;
        };
        if !hint.is_empty() {
            hint.push(' ');
        }
        hint.push(if arg.is_optional { '[' } else { '<' });
        hint.push_str(name);
        if arg.is_variadic {
            hint.push_str("...");
        }
        hint.push_str(if arg.is_optional { "]" } else { "> " });
    }
    hint
}

fn subcommand_item(sub: &Subcommand) -> Item {
    Item {
        kind: ItemKind::Subcommand,
        names: sub.name.clone(),
        display_name: sub.display_name.clone(),
        insert_value: sub.insert_value.clone(),
        description: sub.description.clone(),
        icon: sub.icon.clone(),
        priority: sub.priority,
        hidden: sub.hidden,
        deprecated: is_deprecated(&sub.deprecated),
        is_dangerous: sub.is_dangerous,
        suggestion_type: Some(SuggestionType::Subcommand),
        args_hint: args_hint(&sub.args),
        separator: None,
        should_add_space: mandatory_first(&sub.args)
            || sub
                .requires_subcommand
                .unwrap_or(!sub.subcommands.is_empty()),
    }
}

/// Whether an item's first arg is mandatory (`suggestions/index.ts:48-49`).
fn mandatory_first(args: &[Arg]) -> bool {
    args.first().is_some_and(|arg| !arg.is_optional)
}

/// `separatorToAdd` (`suggestions/index.ts:160-179`): never when the first
/// arg is optional; else the option's own separator, or its subcommand's
/// first `optionArgSeparators` (`=` by default) for `requiresSeparator:
/// true`, or `=` for `requiresEquals`.
fn separator(option: &CommandOption, directives: Option<&SubcommandDirectives>) -> Option<String> {
    if option.args.first().is_some_and(|arg| arg.is_optional) {
        return None;
    }
    match &option.requires_separator {
        Some(RequiresSeparator::Separator(separator)) if !separator.is_empty() => {
            return Some(separator.clone());
        }
        Some(RequiresSeparator::Flag(true)) => {
            let first = directives.and_then(|directives| directives.option_arg_separators.first());
            return Some(first.map_or_else(|| "=".to_owned(), Clone::clone));
        }
        _ => {}
    }
    option.requires_equals.then(|| "=".to_owned())
}

fn option_item(option: &CommandOption, directives: Option<&SubcommandDirectives>) -> Item {
    Item {
        kind: ItemKind::Option,
        names: option.name.clone(),
        display_name: option.display_name.clone(),
        insert_value: option.insert_value.clone(),
        description: option.description.clone(),
        icon: option.icon.clone(),
        priority: option.priority,
        hidden: option.hidden,
        deprecated: is_deprecated(&option.deprecated),
        is_dangerous: option.is_dangerous,
        suggestion_type: Some(SuggestionType::Option),
        args_hint: args_hint(&option.args),
        separator: separator(option, directives),
        should_add_space: mandatory_first(&option.args),
    }
}

/// A bare string suggestion (`convertStringsToSuggestions`,
/// `suggestions/index.ts:70-85`): it takes its owner's `isDangerous`.
fn named_item(
    kind: ItemKind,
    name: &str,
    suggestion_type: Option<SuggestionType>,
    is_dangerous: bool,
) -> Item {
    Item {
        kind,
        names: vec![name.to_owned()],
        display_name: None,
        insert_value: None,
        description: None,
        icon: None,
        priority: None,
        hidden: false,
        deprecated: false,
        is_dangerous,
        suggestion_type,
        args_hint: String::new(),
        separator: None,
        should_add_space: false,
    }
}

fn suggestion_item(
    kind: ItemKind,
    suggestion: &super::model::Suggestion,
    default_type: Option<SuggestionType>,
) -> Item {
    Item {
        kind,
        names: suggestion.name.clone(),
        display_name: suggestion.display_name.clone(),
        insert_value: suggestion.insert_value.clone(),
        description: suggestion.description.clone(),
        icon: suggestion.icon.clone(),
        priority: suggestion.priority,
        hidden: suggestion.hidden,
        deprecated: is_deprecated(&suggestion.deprecated),
        is_dangerous: suggestion.is_dangerous,
        suggestion_type: suggestion.kind.clone().or(default_type),
        args_hint: String::new(),
        separator: None,
        should_add_space: false,
    }
}

/// A generator's (or template's) suggestion as an item. Its `type` is set
/// already (`arg` when it gave none).
pub fn generated_item(suggestion: &super::model::Suggestion) -> Item {
    suggestion_item(ItemKind::Generated, suggestion, Some(SuggestionType::Arg))
}

/// A plain JavaScript object used as a map from name to item, as Fig's
/// `makeNamedMap` builds one (`@fig/autocomplete-shared`, `convert.js`): a
/// later item takes over a name in place.
struct NamedMap<'a, T> {
    entries: Vec<(&'a str, &'a T)>,
    slots: HashMap<&'a str, usize, Fnv>,
    /// Some key is an array index, so property order is not insertion order.
    indexed: bool,
}

impl<T> Default for NamedMap<'_, T> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            slots: HashMap::default(),
            indexed: false,
        }
    }
}

impl<'a, T> NamedMap<'a, T> {
    fn with_capacity(names: usize) -> Self {
        Self {
            entries: Vec::with_capacity(names),
            slots: HashMap::with_capacity_and_hasher(names, Fnv::default()),
            indexed: false,
        }
    }

    fn insert(&mut self, name: &'a str, value: &'a T) {
        match self.slots.get(name) {
            Some(&at) => self.entries[at].1 = value,
            None => {
                self.indexed |= array_index(name).is_some();
                self.slots.insert(name, self.entries.len());
                self.entries.push((name, value));
            }
        }
    }

    /// `[...new Set(Object.values(map))]`: property order puts integer-like
    /// keys first, in numeric order, then the rest in insertion order.
    fn values(&self) -> Vec<&'a T> {
        let mut order: Vec<usize> = (0..self.entries.len()).collect();
        if self.indexed {
            order.sort_by_key(|&at| match array_index(self.entries[at].0) {
                Some(index) => (false, index, 0),
                None => (true, 0, at),
            });
        }
        let mut seen = HashSet::with_capacity_and_hasher(order.len(), Fnv::default());
        order
            .into_iter()
            .map(|at| self.entries[at].1)
            .filter(|value| seen.insert(std::ptr::from_ref(*value) as usize))
            .collect()
    }
}

/// FNV-1a, for the parse's small maps of short names: much cheaper than the
/// default SipHash, and no untrusted input picks the keys' collisions to
/// any effect.
#[derive(Clone, Copy)]
pub(crate) struct FnvHasher(u64);

impl Default for FnvHasher {
    fn default() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
}

impl std::hash::Hasher for FnvHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.0 = (self.0 ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3);
        }
    }
}

pub(crate) type Fnv = std::hash::BuildHasherDefault<FnvHasher>;

/// A property key that is an array index: a canonical number below
/// 2^32 - 1.
fn array_index(key: &str) -> Option<u32> {
    if key != "0" && (key.starts_with('0') || key.is_empty()) {
        return None;
    }
    if !key.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    key.parse::<u32>().ok().filter(|&index| index < u32::MAX)
}

/// Sort by first name, stably, as `compareNamedObjectsAlphabetically`
/// does with `localeCompare`.
fn sort_by_name(items: &mut [Item]) {
    items.sort_by(|a, b| {
        locale_compare(
            a.names.first().map_or("", String::as_str),
            b.names.first().map_or("", String::as_str),
        )
    });
}

/// The punctuation, symbols and digits in their order under ICU's root
/// collation (what `localeCompare` uses), after the control characters it
/// ignores and before the letters.
const COLLATION_ORDER: &[u8] = b"\t\n\x0b\x0c\r _-,;:!?.'\"()[]{}@*/\\&#%`^+<=>|~$0123456789";

/// Each ASCII character's base weight under ICU's root collation, from 1;
/// 0 for the control characters it ignores. Upper and lower case share one.
const ASCII_WEIGHTS: [u16; 128] = {
    let mut weights = [0u16; 128];
    let mut at = 0;
    while at < COLLATION_ORDER.len() {
        weights[COLLATION_ORDER[at] as usize] = at as u16 + 1;
        at += 1;
    }
    let mut letter = 0;
    while letter < 26 {
        let weight = (COLLATION_ORDER.len() + 1 + letter) as u16;
        weights[b'a' as usize + letter] = weight;
        weights[b'A' as usize + letter] = weight;
        letter += 1;
    }
    weights
};

/// A character's base weight (0: ignored) and case (lowercase first).
/// Exact for ASCII; other characters sort after the letters by code point.
fn collation_weight(c: char) -> (u32, bool) {
    match u8::try_from(c) {
        Ok(byte) if byte.is_ascii() => (
            u32::from(ASCII_WEIGHTS[usize::from(byte)]),
            byte.is_ascii_uppercase(),
        ),
        _ if c.is_control() => (0, false),
        _ => (COLLATION_ORDER.len() as u32 + 27 + u32::from(c), false),
    }
}

/// Compare two names as JavaScript's `localeCompare` does under ICU's root
/// collation, which Fig's sort uses: first by base character (punctuation,
/// then digits, then letters, case aside; control characters ignored), then
/// lowercase before uppercase.
pub(crate) fn locale_compare(a: &str, b: &str) -> Ordering {
    fn weights(text: &str) -> impl Iterator<Item = (u32, bool)> + '_ {
        text.chars()
            .map(collation_weight)
            .filter(|&(base, _)| base != 0)
    }
    match weights(a)
        .map(|(base, _)| base)
        .cmp(weights(b).map(|(base, _)| base))
    {
        Ordering::Equal => weights(a)
            .map(|(_, upper)| upper)
            .cmp(weights(b).map(|(_, upper)| upper)),
        unequal => unequal,
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::complete::tokenizer::{Dialect, analyze};
    use crate::specs::archive::Archive;
    use crate::specs::model::Entry;

    /// A plain spec from the archive.
    fn load(name: &str) -> Option<Arc<LoadedSpec>> {
        match Archive::embedded().entry(name)? {
            Ok(Entry::Spec { root, fns }) => Some(Arc::new(LoadedSpec {
                name: name.to_owned(),
                has_js: !fns.is_empty(),
                root,
                fns,
                version: None,
                load_time: Duration::ZERO,
                generation: 0,
            })),
            Ok(_) => None,
            Err(err) => panic!("{name}: {err}"),
        }
    }

    fn inline_spec(name: &str, json: &str) -> Arc<LoadedSpec> {
        let root: Subcommand = serde_json::from_str(json).unwrap_or_else(|err| panic!("{err}"));
        Arc::new(LoadedSpec {
            name: name.to_owned(),
            root,
            fns: Vec::new(),
            has_js: false,
            version: None,
            load_time: Duration::ZERO,
            generation: 0,
        })
    }

    /// The test's `Resolved`: specs loaded from the archive when asked for;
    /// function needs answered from `answers`, else by `otherwise`, which
    /// by default fails them all (as Fig's functions do with a shell that
    /// runs nothing). Every need asked is recorded.
    struct Specs {
        loaded: RefCell<HashMap<String, Arc<LoadedSpec>>>,
        answers: HashMap<Need, Arc<Answer>>,
        /// None leaves the need pending.
        otherwise: fn(&Need) -> Option<Answer>,
        asked: RefCell<Vec<Need>>,
        record: bool,
    }

    impl Default for Specs {
        fn default() -> Self {
            Self {
                loaded: RefCell::default(),
                answers: HashMap::new(),
                otherwise: |_| Some(Answer::Failed),
                asked: RefCell::default(),
                record: true,
            }
        }
    }

    impl Resolved for Specs {
        fn spec(&self, name: &str) -> Option<Arc<LoadedSpec>> {
            self.loaded.borrow().get(name).cloned()
        }

        fn answer(&self, need: &Need) -> Option<Arc<Answer>> {
            if self.record {
                self.asked.borrow_mut().push(need.clone());
            }
            self.answers
                .get(need)
                .cloned()
                .or_else(|| (self.otherwise)(need).map(Arc::new))
        }
    }

    impl Specs {
        fn with(specs: &[Arc<LoadedSpec>]) -> Self {
            let this = Self::default();
            for spec in specs {
                this.loaded
                    .borrow_mut()
                    .insert(spec.name.clone(), Arc::clone(spec));
            }
            this
        }

        /// Load a spec the parse asked for; false when there is none.
        fn fetch(&self, name: &str) -> bool {
            let Some(spec) = load(name) else {
                return false;
            };
            self.loaded.borrow_mut().insert(name.to_owned(), spec);
            true
        }
    }

    /// Leaves alias needs pending and fails the rest.
    fn pending_alias(need: &Need) -> Option<Answer> {
        (!matches!(need, Need::Alias { .. })).then_some(Answer::Failed)
    }

    /// What Fig's functions give in the fixture, where every shell command
    /// fails with no output: git's generateSpec finds no external commands
    /// in `git help -a` and returns a bare `git`; aliases throw.
    fn fig_stub(need: &Need) -> Option<Answer> {
        Some(match need {
            Need::GenerateSpec { spec, .. } if spec == "git" => Answer::Spec(Subcommand {
                name: vec!["git".into()],
                ..Subcommand::default()
            }),
            Need::Alias { spec, .. } if spec == "git" => Answer::Failed,
            other => panic!("the fixture has no answer for {other:?}"),
        })
    }

    /// A line typed with the cursor at its end, through the tokenizer.
    struct Line {
        command: String,
        words: Vec<String>,
        current: String,
    }

    fn line(text: &str) -> Line {
        let context = analyze(Dialect::Zsh, "", text, "");
        let mut words = context.words_before.into_iter();
        let command = words.next().unwrap_or_default();
        Line {
            command,
            words: words.collect(),
            current: context.current.unquoted,
        }
    }

    /// A parse's outcome, or the function need it stopped at.
    type Ran = Result<Result<Suggest, Stop>, Need>;

    /// The parse of `text`, loading every spec it asks for; the specs asked
    /// for are recorded. Err(need) for any other need.
    fn run(specs: &Specs, text: &str) -> (Ran, Vec<String>) {
        let line = line(text);
        let mut fetched = Vec::new();
        if specs.spec(&line.command).is_none() {
            assert!(specs.fetch(&line.command), "no spec {}", line.command);
        }
        loop {
            let Some(root) = specs.spec(&line.command) else {
                panic!("{} is gone", line.command);
            };
            match outcome(&root, &line.words, &line.current, specs) {
                Err(Stop::Needs(Need::Spec { name })) if !fetched.contains(&name) => {
                    fetched.push(name.clone());
                    if !specs.fetch(&name) {
                        return (Ok(Err(Stop::Fail)), fetched);
                    }
                }
                Err(Stop::Needs(need)) => return (Err(need), fetched),
                other => return (Ok(other), fetched),
            }
        }
    }

    /// The parse of `text`, which must succeed.
    fn ready(specs: &Specs, text: &str) -> Suggest {
        match run(specs, text).0 {
            Ok(Ok(suggest)) => suggest,
            other => panic!("{text:?}: {other:?}"),
        }
    }

    fn names(suggest: &Suggest, kind: ItemKind) -> Vec<&str> {
        suggest
            .items
            .iter()
            .filter(|item| item.kind == kind)
            .filter_map(|item| item.names.first().map(String::as_str))
            .collect()
    }

    fn has(suggest: &Suggest, kind: ItemKind, name: &str) -> bool {
        suggest
            .items
            .iter()
            .any(|item| item.kind == kind && item.names.iter().any(|n| n == name))
    }

    fn item<'s>(suggest: &'s Suggest, name: &str) -> &'s Item {
        suggest
            .items
            .iter()
            .find(|item| item.names.iter().any(|n| n == name))
            .unwrap_or_else(|| panic!("no item {name}"))
    }

    fn arg_name(suggest: &Suggest) -> Option<&str> {
        suggest.arg.as_ref().and_then(|arg| arg.name.as_deref())
    }

    #[test]
    fn git_checkout_b_waits_for_its_branch() {
        let s = ready(&Specs::default(), "git checkout -b ");
        assert_eq!(s.spec, "git");
        assert_eq!(arg_name(&s), Some("New Branch"));
        // A mandatory option arg: nothing but its suggestions.
        assert!(names(&s, ItemKind::Subcommand).is_empty());
        assert!(names(&s, ItemKind::Option).is_empty());
        let arg = s.arg.as_ref().expect("an arg");
        assert!(arg.path.starts_with("git:subcommands."), "{}", arg.path);
        assert!(arg.path.contains(".options."), "{}", arg.path);
    }

    #[test]
    fn git_c_takes_a_dir_then_subcommands() {
        let s = ready(&Specs::default(), "git -C dir sta");
        assert!(has(&s, ItemKind::Subcommand, "status"));
        assert!(has(&s, ItemKind::Subcommand, "stash"));
        assert_eq!(arg_name(&s), Some("alias"));
        assert_eq!(s.query_start, 0);
    }

    #[test]
    fn ls_la_then_more_options() {
        let specs = Specs::default();
        let s = ready(&specs, "ls -la ");
        // -l and -a are used up; the rest are offered.
        assert!(!has(&s, ItemKind::Option, "-l"));
        assert!(!has(&s, ItemKind::Option, "-a"));
        assert!(has(&s, ItemKind::Option, "-h"));
        assert!(!s.in_option_chain);
        // Typing the chain itself: options extend it.
        let s = ready(&specs, "ls -la");
        assert!(s.in_option_chain);
        assert_eq!(item(&s, "-la").kind, ItemKind::Option);
        let h = item(&s, "-lah");
        assert_eq!(h.insert_value.as_deref(), Some("-lah"));
        assert!(!has(&s, ItemKind::Option, "-lal"), "-l is passed");
    }

    #[test]
    fn tar_xzf_is_not_a_chain() {
        // tar sets flagsArePosixNoncompliant, so Fig reads `-xzf` as one
        // long option, finds none, and cannot place the token: the parse
        // fails. Typed as the last word, it is nothing Fig knows.
        let specs = Specs::default();
        assert_eq!(run(&specs, "tar -xzf ").0, Ok(Err(Stop::Fail)));
        let s = ready(&specs, "tar -xzf");
        assert!(!s.in_option_chain);
        assert_eq!(s.arg, None);
        // A chain in a spec without that directive: `f` takes the rest, and
        // with nothing left its arg is next.
        let spec = inline_spec(
            "tarlike",
            r#"{"name": "tarlike", "options": [
                {"name": "-x"}, {"name": "-z"}, {"name": "-f", "args": {"name": "archive"}}
            ]}"#,
        );
        let specs = Specs::with(&[spec]);
        let s = ready(&specs, "tarlike -xzf ");
        assert_eq!(arg_name(&s), Some("archive"));
        assert!(s.items.is_empty());
        let s = ready(&specs, "tarlike -xzfa.tgz");
        assert_eq!(arg_name(&s), Some("archive"));
        assert_eq!(s.query_start, 4);
    }

    #[test]
    fn docker_run_rm_it() {
        let s = ready(&Specs::default(), "docker run --rm -it ");
        assert_eq!(arg_name(&s), Some("image"));
        assert!(!has(&s, ItemKind::Option, "--rm"));
        assert!(!has(&s, ItemKind::Option, "-i"));
        assert!(!has(&s, ItemKind::Option, "-t"));
        assert!(has(&s, ItemKind::Option, "--name"));
    }

    #[test]
    fn npm_run_has_a_script_generator() {
        let s = ready(&Specs::default(), "npm run ");
        let arg = s.arg.as_ref().expect("an arg");
        assert_eq!(arg.name.as_deref(), Some("script"));
        assert!(!arg.generators.is_empty());
        // Its generator's functions sit under the arg's path.
        let npm = load("npm").expect("npm");
        let (_, path) = arg.path.split_once(':').expect("spec:path");
        let generator = &arg.generators[0].generator;
        let fn_ids = [generator.custom, generator.post_process]
            .into_iter()
            .flatten()
            .chain(match &generator.script {
                Some(crate::specs::model::Script::Fn(id)) => Some(*id),
                _ => None,
            });
        let mut any = false;
        for id in fn_ids {
            any = true;
            let fn_path = &npm.fns[id.0 as usize];
            assert!(
                fn_path.starts_with(&format!("{path}.")),
                "{fn_path} vs {path}"
            );
        }
        assert!(any, "the generator has functions");
    }

    #[test]
    fn sudo_hands_off_to_git() {
        let specs = Specs::default();
        let (outcome, fetched) = run(&specs, "sudo git ch");
        assert_eq!(fetched, ["git"]);
        let Ok(Ok(s)) = outcome else {
            panic!("{outcome:?}");
        };
        assert_eq!(s.command, "git");
        assert_eq!(s.spec, "git");
        assert!(has(&s, ItemKind::Subcommand, "checkout"));
        assert!(has(&s, ItemKind::Subcommand, "cherry-pick"));
        // Before the command word is complete, sudo's own state.
        let s = ready(&specs, "sudo gi");
        assert_eq!(s.command, "sudo");
        assert_eq!(arg_name(&s), Some("command"));
    }

    #[test]
    fn kubectl_get_pods_n() {
        // Fig's kubectl spec gives `-n` (namespace) no args.
        let s = ready(&Specs::default(), "kubectl get pods -n ");
        assert_eq!(s.arg, None);
        assert!(!has(&s, ItemKind::Option, "-n"));
        assert!(has(&s, ItemKind::Option, "--chunk-size"));
    }

    #[test]
    fn aws_s3_ls_loads_its_specs_by_name() {
        let specs = Specs::default();
        let (outcome, fetched) = run(&specs, "aws s3 ls ");
        assert_eq!(fetched, ["aws/s3"]);
        let Ok(Ok(s)) = outcome else {
            panic!("{outcome:?}");
        };
        assert_eq!(s.spec, "aws/s3");
        assert_eq!(s.command, "aws");
        assert_eq!(arg_name(&s), Some("paths"));
        assert!(has(&s, ItemKind::Option, "--recursive"));
        // The first parse stops at the need.
        let fresh = Specs::default();
        assert!(fresh.fetch("aws"));
        let aws = fresh.spec("aws").expect("aws");
        let line = line("aws s3 ls ");
        assert_eq!(
            parse(&aws, &line.words, &line.current, &fresh),
            ParseResult::Needs(Need::Spec {
                name: "aws/s3".into()
            })
        );
    }

    #[test]
    fn git_commit_message_then_am() {
        let s = ready(&Specs::default(), r#"git commit -m "msg" --am"#);
        assert!(has(&s, ItemKind::Option, "--amend"));
        assert!(!has(&s, ItemKind::Option, "-m"), "-m is passed");
        assert_eq!(arg_name(&s), Some("pathspec"));
        let s = ready(&Specs::default(), "git commit --amend --am");
        assert!(!has(&s, ItemKind::Option, "--amend"), "not repeatable");
    }

    #[test]
    fn an_option_value_after_the_separator() {
        // Fig's git spec has no `log --format`, so `--format=` is no option
        // there: the whole word is the query, against log's own state.
        let s = ready(&Specs::default(), "git log --format=");
        assert_eq!(s.query_start, 0);
        assert_eq!(arg_name(&s), Some("since"));
        assert!(has(&s, ItemKind::Option, "--oneline"));
        // An option that has args: only they are offered, queried after `=`.
        let s = ready(&Specs::default(), "git push --force-with-lease=ma");
        assert_eq!(s.query_start, "--force-with-lease=".len());
        assert_eq!(arg_name(&s), Some("refname[:expect]"));
        assert!(names(&s, ItemKind::Option).is_empty());
        assert!(names(&s, ItemKind::Subcommand).is_empty());
    }

    #[test]
    fn after_double_dash_a_dash_word_is_an_arg() {
        let specs = Specs::default();
        let s = ready(&specs, "rm -- -x");
        assert!(s.items.is_empty());
        assert!(s.arg.is_some());
        let s = ready(&specs, "rm -- -x ");
        assert!(s.items.is_empty());
        assert!(s.arg.as_ref().is_some_and(|arg| arg.is_variadic));
    }

    #[test]
    fn rm_is_variadic() {
        let s = ready(&Specs::default(), "rm a b ");
        let arg = s.arg.as_ref().expect("an arg");
        assert!(arg.is_variadic);
        // rm has no functions to tell whether its args were one object.
        assert_eq!(arg.path, "rm:args.0");
        // Options can still break in.
        assert!(has(&s, ItemKind::Option, "-r"));
    }

    #[test]
    fn exclusive_on_and_depends_on() {
        // chown's -H is exclusive with -L and -P and depends on -R.
        let s = ready(&Specs::default(), "chown -H ");
        assert!(!has(&s, ItemKind::Option, "-L"));
        assert!(!has(&s, ItemKind::Option, "-P"));
        assert!(!has(&s, ItemKind::Option, "-H"));
        assert_eq!(item(&s, "-R").priority, Some(75.0));
        assert_eq!(item(&s, "-f").priority, None);
    }

    #[test]
    fn requires_subcommand_changes_nothing_in_the_parse() {
        // Fig reads requiresSubcommand only when inserting (shouldAddSpace).
        let s = ready(&Specs::default(), "yalc ");
        assert!(has(&s, ItemKind::Subcommand, "add"));
        assert_eq!(s.arg, None);
    }

    #[test]
    fn an_explicit_requires_subcommand_is_final() {
        // `suggestions/index.ts:45-68`: a mandatory first arg, else an
        // explicit boolean, else whether there are subcommands.
        let spec = inline_spec(
            "tool",
            r#"{"name": "tool", "subcommands": [
                {"name": "off", "requiresSubcommand": false, "subcommands": [{"name": "x"}]},
                {"name": "on", "requiresSubcommand": true},
                {"name": "guess-some", "subcommands": [{"name": "x"}]},
                {"name": "guess-none"},
                {"name": "arg-wins", "requiresSubcommand": false, "args": {"name": "a"}}
            ]}"#,
        );
        let specs = Specs::with(&[spec]);
        let s = ready(&specs, "tool ");
        for (name, space) in [
            ("off", false),
            ("on", true),
            ("guess-some", true),
            ("guess-none", false),
            ("arg-wins", true),
        ] {
            assert_eq!(item(&s, name).should_add_space, space, "{name}");
        }
    }

    #[test]
    fn options_must_precede_arguments() {
        let specs = Specs::default();
        let s = ready(&specs, "fold ");
        assert!(has(&s, ItemKind::Option, "-w"));
        let s = ready(&specs, "fold file ");
        assert!(names(&s, ItemKind::Option).is_empty());
        assert_eq!(arg_name(&s), Some("file"));
    }

    #[test]
    fn flags_are_posix_noncompliant() {
        let spec = inline_spec(
            "nc",
            r#"{"name": "nc", "parserDirectives": {"flagsArePosixNoncompliant": true},
                "options": [{"name": "-ab", "args": {"name": "value"}}, {"name": "-a"}, {"name": "-b"}]}"#,
        );
        let specs = Specs::with(&[spec]);
        // `-ab` is one option, with a mandatory arg.
        let s = ready(&specs, "nc -ab ");
        assert_eq!(arg_name(&s), Some("value"));
        let s = ready(&specs, "nc -ab");
        assert!(!s.in_option_chain);
    }

    #[test]
    fn a_string_alias_is_substituted() {
        let spec = inline_spec(
            "tool",
            r#"{"name": "tool",
                "args": {"name": "command", "isOptional": true, "parserDirectives": {"alias": "go --fast"}},
                "subcommands": [
                    {"name": "go", "options": [{"name": "--fast"}, {"name": "--slow"}],
                     "args": {"name": "target"}},
                    {"name": "stop"}
                ]}"#,
        );
        let specs = Specs::with(&[spec]);
        let s = ready(&specs, "tool g ");
        assert_eq!(arg_name(&s), Some("target"));
        assert!(has(&s, ItemKind::Option, "--slow"));
        assert!(!has(&s, ItemKind::Option, "--fast"), "passed by the alias");
        assert_eq!(
            alias_tokens(r#"log --format="%h %s" -n\ 1"#),
            Some(vec!["log".into(), "--format=%h %s".into(), "-n 1".into()])
        );
        assert_eq!(alias_tokens(""), None);
        assert_eq!(alias_tokens("a; b"), None);
        assert_eq!(alias_tokens("a | b"), None);
    }

    #[test]
    fn a_function_alias_is_a_need() {
        let specs = Specs {
            otherwise: pending_alias,
            ..Specs::default()
        };
        let (outcome, _) = run(&specs, "git co -b ");
        let Err(Need::Alias { spec, fn_id, token }) = outcome else {
            panic!("{outcome:?}");
        };
        assert_eq!((spec.as_str(), token.as_str()), ("git", "co"));
        let mut specs = Specs::default();
        specs.answers.insert(
            Need::Alias {
                spec: "git".into(),
                fn_id,
                token: "co".into(),
            },
            Arc::new(Answer::Alias(vec!["checkout".into()])),
        );
        let s = ready(&specs, "git co -b ");
        assert_eq!(arg_name(&s), Some("New Branch"));
        // An alias that fails (no such git alias) fails the parse, as Fig's
        // throw does.
        assert_eq!(run(&Specs::default(), "git co -b ").0, Ok(Err(Stop::Fail)));
    }

    #[test]
    fn generate_spec_is_a_need_and_its_answer_is_merged() {
        let spec = inline_spec(
            "gen",
            r#"{"name": "gen", "subcommands": [
                {"name": "run", "generateSpec": {"$fn": 0}, "options": [{"name": "--own"}]}
            ]}"#,
        );
        let mut specs = Specs::with(&[spec]);
        specs.otherwise = |_| None;
        let need = Need::GenerateSpec {
            spec: "gen".into(),
            fn_id: 0,
            tokens: vec!["gen".into(), "run".into(), "".into()],
        };
        assert_eq!(run(&specs, "gen run ").0, Err(need.clone()));
        let generated: Subcommand = serde_json::from_str(
            r#"{"name": "run", "subcommands": [{"name": "made"}],
                "options": [{"name": "--made"}], "args": {"name": "thing"}}"#,
        )
        .expect("json");
        specs
            .answers
            .insert(need, Arc::new(Answer::Spec(generated)));
        let s = ready(&specs, "gen run ");
        assert!(has(&s, ItemKind::Subcommand, "made"));
        assert!(has(&s, ItemKind::Option, "--made"));
        assert!(has(&s, ItemKind::Option, "--own"));
        let arg = s.arg.as_ref().expect("the generated arg");
        assert_eq!(arg.name.as_deref(), Some("thing"));
        assert_eq!(
            arg.path,
            r#"gen:subcommands.0.generateSpec(["gen", "run"]).args.0"#
        );
        // A failed generateSpec keeps the state.
        let specs = Specs::with(&[specs.spec("gen").expect("gen")]);
        let s = ready(&specs, "gen run ");
        assert!(has(&s, ItemKind::Option, "--own"));
        assert!(!has(&s, ItemKind::Option, "--made"));
    }

    #[test]
    fn a_load_spec_function_is_a_need() {
        let spec = inline_spec(
            "make",
            r#"{"name": "make", "subcommands": [
                {"name": "create", "args": {"name": "kit", "isCommand": false, "loadSpec": {"$fn": 3}}}
            ]}"#,
        );
        let mut specs = Specs::with(&[spec]);
        specs.otherwise = |_| None;
        let need = Need::LoadSpecFn {
            spec: "make".into(),
            fn_id: 3,
            token: "vite".into(),
        };
        assert_eq!(run(&specs, "make create vite ").0, Err(need.clone()));
        specs.otherwise = |_| Some(Answer::Failed);
        // A location: hand off to that spec, whose command word is the token.
        let location: Subcommand =
            serde_json::from_str(r#"{"name": [], "loadSpec": "git"}"#).expect("json");
        specs
            .answers
            .insert(need.clone(), Arc::new(Answer::Spec(location)));
        let s = ready(&specs, "make create vite ");
        assert_eq!(s.spec, "git");
        assert!(has(&s, ItemKind::Subcommand, "commit"));
        // An inline subcommand: used in place.
        let inline: Subcommand =
            serde_json::from_str(r#"{"name": "kit", "options": [{"name": "--template"}]}"#)
                .expect("json");
        specs.answers.insert(need, Arc::new(Answer::Spec(inline)));
        let s = ready(&specs, "make create vite ");
        assert_eq!(s.spec, "make");
        assert!(has(&s, ItemKind::Option, "--template"));
    }

    #[test]
    fn only_the_command_name() {
        let s = ready(&Specs::default(), "git ");
        assert!(has(&s, ItemKind::Subcommand, "commit"));
        assert!(has(&s, ItemKind::Option, "--version"));
        assert_eq!(arg_name(&s), Some("alias"));
        let git = load("git").expect("git");
        let ParseResult::Ready(direct) = parse(&git, &[], "", &Specs::default()) else {
            panic!("git alone needs nothing");
        };
        assert_eq!(direct, s);
    }

    #[test]
    fn unknown_tokens_go_the_way_fig_sends_them() {
        let specs = Specs::default();
        // git's root arg has a function alias: an unknown subcommand is
        // looked up as an alias first, and with no such alias the parse
        // fails.
        let pending = Specs {
            otherwise: pending_alias,
            ..Specs::default()
        };
        assert!(matches!(
            run(&pending, "git foo ").0,
            Err(Need::Alias { .. })
        ));
        assert_eq!(run(&specs, "git foo ").0, Ok(Err(Stop::Fail)));
        // cd takes one arg; a second fails the parse.
        assert_eq!(run(&specs, "cd a b ").0, Ok(Err(Stop::Fail)));
        // A spec without args: an unknown subcommand fails the parse.
        assert_eq!(run(&specs, "yalc nope ").0, Ok(Err(Stop::Fail)));
        // As the word being typed, it is only a filter.
        let s = ready(&specs, "yalc nope");
        assert!(has(&s, ItemKind::Subcommand, "add"));
        // A lone `-` before the cursor: Fig's empty chain throws.
        assert_eq!(run(&specs, "cat - ").0, Ok(Err(Stop::Fail)));
        // `parse` turns a failure into an empty suggestion.
        let cd = load("cd").expect("cd");
        let ParseResult::Ready(s) = parse(&cd, &["a".into(), "b".into()], "", &specs) else {
            panic!("cd needs nothing");
        };
        assert!(s.items.is_empty() && s.arg.is_none());
    }

    #[test]
    fn a_dash_while_an_arg_is_pending() {
        // A mandatory arg takes `-` as its value: only its suggestions.
        let s = ready(&Specs::default(), "git checkout -b -");
        assert_eq!(arg_name(&s), Some("New Branch"));
        assert!(names(&s, ItemKind::Option).is_empty());
        // An optional subcommand arg leaves options on.
        let s = ready(&Specs::default(), "ls -");
        assert!(has(&s, ItemKind::Option, "-l"));
    }

    #[test]
    fn repeatable_persistent_and_unbreakable_variadic() {
        let spec = inline_spec(
            "tool",
            r#"{"name": "tool",
                "options": [{"name": "--global", "isPersistent": true}, {"name": "-v", "isRepeatable": 2}],
                "subcommands": [{"name": "sub", "options": [{"name": "--own"}],
                    "args": {"name": "files", "isVariadic": true, "optionsCanBreakVariadicArg": false}}],
                "additionalSuggestions": ["extra"]}"#,
        );
        let specs = Specs::with(&[spec]);
        assert!(has(&ready(&specs, "tool -v "), ItemKind::Option, "-v"));
        assert!(!has(&ready(&specs, "tool -v -v "), ItemKind::Option, "-v"));
        // A third -v is no option, and tool takes no args.
        assert_eq!(run(&specs, "tool -v -v -v ").0, Ok(Err(Stop::Fail)));
        // Persistent options carry into subcommands; the rest stay behind.
        let s = ready(&specs, "tool sub ");
        assert!(has(&s, ItemKind::Option, "--global"));
        assert!(has(&s, ItemKind::Option, "--own"));
        assert!(!has(&s, ItemKind::Option, "-v"));
        assert!(!has(&s, ItemKind::Additional, "extra"));
        assert!(has(&ready(&specs, "tool "), ItemKind::Additional, "extra"));
        // Once the variadic arg has a value, options cannot break it.
        let s = ready(&specs, "tool sub a ");
        assert!(names(&s, ItemKind::Option).is_empty());
        assert_eq!(arg_name(&s), Some("files"));
        assert!(has(
            &ready(&specs, "tool sub --global "),
            ItemKind::Option,
            "--own"
        ));
    }

    #[test]
    fn hidden_items_are_kept_and_modules_load_by_name() {
        let spec = inline_spec(
            "py",
            r#"{"name": "py", "subcommands": [{"name": "secret", "hidden": true}, {"name": "open"}],
                "options": [{"name": "-m", "args": {"name": "module", "isModule": "py/"}}]}"#,
        );
        let specs = Specs::with(&[spec]);
        let s = ready(&specs, "py ");
        assert!(item(&s, "secret").hidden);
        assert!(!item(&s, "open").hidden);
        let (outcome, fetched) = run(&specs, "py -m http.server x");
        assert_eq!(fetched, ["py/http.server"]);
        assert_eq!(outcome, Ok(Err(Stop::Fail)), "no such spec");
    }

    #[test]
    fn templates_drop_the_generators() {
        let spec = inline_spec(
            "t",
            r#"{"name": "t", "args": {"name": "file", "template": "filepaths",
                "generators": {"script": ["ls"]}}}"#,
        );
        let s = ready(&Specs::with(&[spec]), "t ");
        let arg = s.arg.as_ref().expect("an arg");
        assert_eq!(arg.templates, [Template::Filepaths]);
        assert!(arg.generators.is_empty());
    }

    #[test]
    fn args_hint_follows_fig() {
        let args: Vec<Arg> = serde_json::from_str(
            r#"[{"name": "a"}, {"name": "b", "isOptional": true}, {"name": "c", "isVariadic": true},
                {"name": "d", "isVariadic": true, "isOptional": true}, {}]"#,
        )
        .expect("json");
        assert_eq!(args_hint(&args), "<a>  [b] <c...>  [d...]");
        assert_eq!(args_hint(&[]), "");
    }

    #[test]
    fn named_maps_order_like_javascript_objects() {
        let subs: Vec<Subcommand> = serde_json::from_str(
            r#"[{"name": ["b", "x"]}, {"name": "10"}, {"name": "2"}, {"name": "b"}, {"name": "a"}]"#,
        )
        .expect("json");
        let mut map = NamedMap::default();
        for sub in &subs {
            for name in &sub.name {
                map.insert(name, sub);
            }
        }
        let order: Vec<&str> = map
            .values()
            .into_iter()
            .map(|sub| sub.name[0].as_str())
            .collect();
        // Integer keys first; `b` was taken over by the later `b` in place.
        assert_eq!(order, ["2", "10", "b", "b", "a"]);
        assert_eq!(array_index("01"), None);
        assert_eq!(array_index("4294967295"), None);
        assert_eq!(array_index("4294967294"), Some(4_294_967_294));
    }

    #[test]
    fn collation_matches_icu_for_ascii() {
        // Node's `[...ascii].sort((a, b) => a.localeCompare(b))`, controls
        // (ignored) left out.
        let node = "\t\n\u{b}\u{c}\r _-,;:!?.'\"()[]{}@*/\\&#%`^+<=>|~$0123456789aAbBcCdDeEfFgGhHiIjJkKlLmMnNoOpPqQrRsStTuUvVwWxXyYzZ";
        let mut chars: Vec<String> = (0x09u8..0x7f)
            .filter(|b| !(0x0e..0x20).contains(b))
            .map(|b| char::from(b).to_string())
            .collect();
        chars.sort_by(|a, b| locale_compare(a, b));
        assert_eq!(chars.concat(), node);
        let mut words = vec![
            "-a", "-A", "--a", "-B", "-b", "-1", "-a1", "-ab", "-aB", "-Ab",
        ];
        words.sort_by(|a, b| locale_compare(a, b));
        assert_eq!(
            words,
            [
                "--a", "-1", "-a", "-A", "-a1", "-ab", "-aB", "-Ab", "-b", "-B"
            ]
        );
        let mut words = vec![
            "a", "A", "b", "B", "ab", "Ab", "aB", "a-b", "ab-", "a_b", "A-b",
        ];
        words.sort_by(|a, b| locale_compare(a, b));
        assert_eq!(
            words,
            [
                "a", "A", "a_b", "a-b", "A-b", "ab", "aB", "Ab", "ab-", "b", "B"
            ]
        );
    }

    #[test]
    fn hand_off_to_a_local_spec_fails() {
        assert_eq!(spec_path("git", false), Location::Global("git".into()));
        assert_eq!(
            spec_path("foo/bar", false),
            Location::Global("foo/bar".into())
        );
        assert_eq!(spec_path("./run", false), Location::Local);
        assert_eq!(spec_path("/usr/bin/git", false), Location::Local);
        assert_eq!(spec_path("~/bin/x", false), Location::Local);
        assert_eq!(spec_path("x.js", true), Location::Local);
        assert_eq!(spec_path("?", false), Location::Local);
        assert_eq!(
            run(&Specs::default(), "sudo ./run x").0,
            Ok(Err(Stop::Fail))
        );
    }

    /// One line's summary, in the shape `make-expected.mjs` prints.
    fn summary(suggest: &Suggest, current: &str) -> serde_json::Value {
        let items: Vec<serde_json::Value> = suggest
            .items
            .iter()
            .map(|item| {
                let mut value = serde_json::json!({
                    "type": item.suggestion_type.as_ref().map(SuggestionType::as_str),
                    "names": item.names,
                });
                if let Some(priority) = item.priority {
                    value["priority"] = serde_json::json!(priority);
                }
                if let Some(insert) = &item.insert_value {
                    value["insertValue"] = serde_json::json!(insert);
                }
                let is_arg = item
                    .suggestion_type
                    .as_ref()
                    .is_some_and(|ty| ty.as_str() == "arg");
                if suggest.in_option_chain && is_arg {
                    value["query"] = serde_json::json!("");
                }
                if item.hidden {
                    value["hidden"] = serde_json::json!(true);
                }
                value
            })
            .collect();
        serde_json::json!({
            "arg": suggest.arg.as_ref().map(|arg| arg.name.clone().unwrap_or_default()),
            "search": current.get(suggest.query_start..).unwrap_or_default(),
            "suggestions": items,
        })
    }

    /// JSON numbers as floats, as fac's priorities are.
    fn numbers_as_floats(value: serde_json::Value) -> serde_json::Value {
        use serde_json::Value;
        match value {
            Value::Number(number) => number
                .as_f64()
                .map_or(Value::Null, |f| serde_json::json!(f)),
            Value::Array(items) => Value::Array(items.into_iter().map(numbers_as_floats).collect()),
            Value::Object(map) => Value::Object(
                map.into_iter()
                    .map(|(key, value)| (key, numbers_as_floats(value)))
                    .collect(),
            ),
            other => other,
        }
    }

    /// Lines where fac's parse may differ from Fig's, with why. None yet.
    const DIFFERENCES: &[(&str, &str)] = &[];

    #[test]
    fn matches_figs_own_parser() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/fixtures/fig-parse/expected.json"))
                .expect("fixture JSON");
        let cases = fixture["cases"].as_array().expect("cases");
        assert!(cases.len() >= 25, "{} cases", cases.len());
        let mut mismatches = Vec::new();
        for case in cases {
            let text = case["line"].as_str().expect("line");
            if DIFFERENCES.iter().any(|(line, _)| *line == text) {
                continue;
            }
            let specs = Specs {
                otherwise: fig_stub,
                ..Specs::default()
            };
            let (outcome, _) = run(&specs, text);
            let got = match outcome {
                Ok(Ok(suggest)) => summary(&suggest, &line(text).current),
                Ok(Err(Stop::Fail)) => serde_json::json!({"error": true}),
                other => panic!("{text:?}: {other:?}"),
            };
            let mut want = numbers_as_floats(case.clone());
            if let Some(object) = want.as_object_mut() {
                object.remove("line");
                if object.contains_key("error") {
                    object.insert("error".into(), serde_json::json!(true));
                }
            }
            if got != want {
                mismatches.push(format!("{text:?}\n  fig: {want}\n  fac: {got}"));
            }
        }
        assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
    }

    fn time_parse(specs: &Specs, text: &str) -> Duration {
        let line = line(text);
        let root = specs.spec(&line.command).expect("loaded");
        let parse_once = || match parse(&root, &line.words, &line.current, specs) {
            ParseResult::Ready(suggest) => suggest,
            ParseResult::Needs(need) => panic!("{text:?} needs {need:?}"),
        };
        for _ in 0..100 {
            std::hint::black_box(parse_once());
        }
        const RUNS: u32 = 2000;
        let started = Instant::now();
        for _ in 0..RUNS {
            std::hint::black_box(parse_once());
        }
        started.elapsed() / RUNS
    }

    /// `cargo test --release --lib parse_timing -- --ignored --nocapture`.
    #[test]
    #[ignore = "timing; run in release"]
    fn parse_timing() {
        let specs = Specs {
            record: false,
            ..Specs::default()
        };
        for name in ["git", "aws", "aws/ec2"] {
            assert!(specs.fetch(name), "{name}");
        }
        for text in ["git commit -m x --am", "aws ec2 describe-instances --"] {
            let each = time_parse(&specs, text);
            println!("{text:?}: {:.1} µs", each.as_secs_f64() * 1e6);
            assert!(each < Duration::from_micros(50), "{text:?}: {each:?}");
        }
    }
}
