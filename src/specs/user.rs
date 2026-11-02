//! The user's own specs, in `~/.config/fac/specs/`: for a command `<name>`
//! (or a nested name like `aws/ec2`, as a folder), `<name>.ts` (TypeScript in
//! Fig's format), `<name>.js` (an ES module) or `<name>.json` (the spec as
//! plain JSON, in Fig's field names). When more than one exists the order is
//! `.ts`, `.js`, `.json`. A user spec replaces fac's spec of the same name.
//!
//! [`prepare`] turns a file's text into what the spec thread evaluates: a
//! TypeScript file has its types stripped with oxc (only TypeScript is
//! transformed: nothing is lowered), a JavaScript file is parsed by oxc too,
//! so a syntax error has its line and column; the only import either may
//! have is fac's built-in `@fig/autocomplete-generators`. A JSON file is read
//! straight into the model, with no JavaScript, and a key the model does not
//! know is an error. [`stamp`] is what the live reload compares. [`summary`]
//! is `fac spec check`'s account of a loaded spec.
//!
//! An error's [`SpecError::summary`] never quotes the file, so it can be
//! logged; its `Display` can, and is for the user's own terminal only.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use oxc_allocator::Allocator;
use oxc_ast::ast::{
    ImportDeclarationSpecifier, ImportOrExportKind, Program, Statement, StringLiteral,
};
use oxc_codegen::{Codegen, CodegenOptions};
use oxc_diagnostics::Diagnostics;
use oxc_parser::Parser;
use oxc_semantic::SemanticBuilder;
use oxc_span::SourceType;
use oxc_transformer::{TransformOptions, Transformer};
use serde_json::Value as Json;

use super::model::{Arg, Generator, LoadSpec, Script, StringOrFn, Subcommand, Trigger};

/// Where the user specs are, under `HOME`.
pub const DIR: &str = ".config/fac/specs";

/// The one module a user spec may import.
pub const GENERATORS: &str = "@fig/autocomplete-generators";

/// [`GENERATORS`]'s source.
pub const GENERATORS_JS: &str = include_str!("autocomplete-generators.js");

/// Turns generators that are functions with fields (`filepaths`) into plain
/// objects; see the file.
pub const PLAIN_GENERATORS_JS: &str = include_str!("plain-generators.js");

/// Fig's keys whose value can only be a function, so a JSON spec has none.
const FUNCTION_KEYS: [&str; 4] = [
    "generateSpec",
    "postProcess",
    "custom",
    "filterTemplateSuggestions",
];

/// The user spec folder under `home`.
pub fn dir(home: &Path) -> PathBuf {
    home.join(DIR)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    Ts,
    Js,
    Json,
}

impl Format {
    /// In the order fac picks them.
    pub const ALL: [Format; 3] = [Format::Ts, Format::Js, Format::Json];

    pub fn extension(self) -> &'static str {
        match self {
            Format::Ts => "ts",
            Format::Js => "js",
            Format::Json => "json",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Format::Ts => "TypeScript",
            Format::Js => "JavaScript",
            Format::Json => "JSON",
        }
    }

    /// The format of a file, by its extension.
    pub fn of(path: &Path) -> Option<Format> {
        let extension = path.extension()?.to_str()?;
        Format::ALL
            .into_iter()
            .find(|format| format.extension() == extension)
    }

    fn index(self) -> usize {
        match self {
            Format::Ts => 0,
            Format::Js => 1,
            Format::Json => 2,
        }
    }
}

/// `[A-Za-z0-9._-]+`, plus `/` between such parts for a nested name; no
/// part is `.` or `..`, so a name never leaves the folder.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        })
}

/// The files in the folder that are not specs: an editor's `*.d.ts` types
/// (`fig.d.ts`) and `tsconfig.json`.
pub fn not_a_spec(name: &str, format: Format) -> bool {
    let last = name.rsplit('/').next().unwrap_or(name);
    match format {
        Format::Ts => last.ends_with(".d"),
        Format::Js => false,
        Format::Json => last == "tsconfig",
    }
}

/// `<dir>/<name>.<extension>`.
pub fn path(dir: &Path, name: &str, format: Format) -> PathBuf {
    dir.join(format!("{name}.{}", format.extension()))
}

/// The file fac uses for a name, and the other formats it ignores.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub path: PathBuf,
    pub format: Format,
    pub unused: Vec<PathBuf>,
}

/// The user spec for `name`, when there is one.
pub fn find(dir: &Path, name: &str) -> Option<Found> {
    if !valid_name(name) {
        return None;
    }
    let mut present = Format::ALL
        .into_iter()
        .filter(|&format| !not_a_spec(name, format))
        .map(|format| (format, path(dir, name, format)))
        .filter(|(_, path)| path.is_file());
    let (format, path) = present.next()?;
    Some(Found {
        path,
        format,
        unused: present.map(|(_, path)| path).collect(),
    })
}

/// What a name's files looked like: each format's modification time and
/// size, `None` where there is no file. A change of any of them (a file
/// added, edited or removed) is a change of the spec.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stamp([Option<(SystemTime, u64)>; 3]);

pub fn stamp(dir: &Path, name: &str) -> Stamp {
    let mut stamp = Stamp::default();
    if !valid_name(name) {
        return stamp;
    }
    for format in Format::ALL {
        if not_a_spec(name, format) {
            continue;
        }
        stamp.0[format.index()] = std::fs::metadata(path(dir, name, format))
            .ok()
            .filter(std::fs::Metadata::is_file)
            .map(|meta| {
                (
                    meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                    meta.len(),
                )
            });
    }
    stamp
}

/// A name with user spec files, and their formats in fac's order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    pub name: String,
    pub formats: Vec<Format>,
}

/// Every user spec in the folder, by name. Hidden files and `node_modules`
/// are skipped.
pub fn list(dir: &Path) -> Vec<Listed> {
    fn visit(root: &Path, dir: &Path, out: &mut Vec<(String, Format)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let file_name = entry.file_name();
            let Some(file_name) = file_name.to_str() else {
                continue;
            };
            if file_name.starts_with('.') || file_name == "node_modules" {
                continue;
            }
            if path.is_dir() {
                visit(root, &path, out);
                continue;
            }
            let Some(format) = Format::of(&path) else {
                continue;
            };
            let Some(name) = path
                .strip_prefix(root)
                .ok()
                .and_then(|relative| relative.with_extension("").to_str().map(str::to_owned))
            else {
                continue;
            };
            if valid_name(&name) && !not_a_spec(&name, format) {
                out.push((name, format));
            }
        }
    }
    let mut files = Vec::new();
    visit(dir, dir, &mut files);
    files.sort_by_key(|(name, format)| (name.clone(), format.index()));
    let mut listed: Vec<Listed> = Vec::new();
    for (name, format) in files {
        match listed.last_mut() {
            Some(last) if last.name == name => last.formats.push(format),
            _ => listed.push(Listed {
                name,
                formats: vec![format],
            }),
        }
    }
    listed
}

/// What kind of problem a spec file has; [`SpecError::summary`] says it in
/// words that never come from the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    Read,
    Syntax,
    Import,
    UnknownKey,
    Function,
    Field,
}

impl ErrorKind {
    fn words(self) -> &'static str {
        match self {
            ErrorKind::Read => "the file cannot be read",
            ErrorKind::Syntax => "a syntax error",
            ErrorKind::Import => "an import fac does not have",
            ErrorKind::UnknownKey => "a key fac does not know",
            ErrorKind::Function => "a function in a JSON spec",
            ErrorKind::Field => "a field of the wrong type",
        }
    }
}

/// Why a spec file was not loaded, before any JavaScript ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecError {
    pub kind: ErrorKind,
    /// Line and column, from 1, when the problem is at one place.
    pub at: Option<(usize, usize)>,
    /// What is wrong, in full; it may quote the file.
    pub detail: String,
}

impl SpecError {
    /// The kind and place only, for the log.
    pub fn summary(&self) -> String {
        match self.at {
            Some((line, column)) => format!("{line}:{column}: {}", self.kind.words()),
            None => self.kind.words().to_owned(),
        }
    }
}

impl fmt::Display for SpecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.at {
            Some((line, column)) => write!(f, "{line}:{column}: {}", self.detail),
            None => f.write_str(&self.detail),
        }
    }
}

impl std::error::Error for SpecError {}

/// A spec file, ready for the spec thread.
#[derive(Debug)]
pub enum Prepared {
    /// A module's source, for QuickJS; for TypeScript, with the way back
    /// from its lines to the file's.
    Js {
        source: String,
        lines: Option<Lines>,
    },
    /// A JSON spec's tree, already in the model.
    Json(Box<Subcommand>),
}

/// Where the stripped JavaScript's positions were in the TypeScript file,
/// from the code generator's source map: `(line, column)` pairs, from 0,
/// sorted by the JavaScript position.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Lines(Vec<((u32, u32), (u32, u32))>);

impl Lines {
    /// The TypeScript file's line and column (from 1) for the JavaScript's.
    fn original(&self, line: u32, column: u32) -> Option<(u32, u32)> {
        let at = (line.checked_sub(1)?, column.saturating_sub(1));
        let index = self.0.partition_point(|(js, _)| *js <= at).checked_sub(1)?;
        let ((js_line, _), (ts_line, ts_column)) = self.0[index];
        (js_line == at.0).then_some((ts_line + 1, ts_column + 1))
    }

    /// QuickJS's message with every `(file:line:column)` of its stack put
    /// back at the TypeScript file's place.
    pub fn remap(&self, message: &str, file: &str) -> String {
        let marker = format!("({file}:");
        let mut out = String::with_capacity(message.len());
        let mut rest = message;
        while let Some(at) = rest.find(&marker) {
            let after = &rest[at + marker.len()..];
            let end = after.find(')').unwrap_or(after.len());
            let mut parts = after[..end].split(':').map(str::parse::<u32>);
            let place = match (parts.next(), parts.next(), parts.next()) {
                (Some(Ok(line)), Some(Ok(column)), None) => self.original(line, column),
                _ => None,
            };
            out.push_str(&rest[..at + marker.len()]);
            match place {
                Some((line, column)) => out.push_str(&format!("{line}:{column}")),
                None => out.push_str(&after[..end]),
            }
            rest = &after[end..];
        }
        out.push_str(rest);
        out
    }
}

/// Read a spec file as UTF-8.
pub fn read(path: &Path) -> Result<String, SpecError> {
    let bytes = std::fs::read(path).map_err(|err| SpecError {
        kind: ErrorKind::Read,
        at: None,
        detail: format!("cannot read the file: {}", err.kind()),
    })?;
    String::from_utf8(bytes).map_err(|_| SpecError {
        kind: ErrorKind::Read,
        at: None,
        detail: "the file is not UTF-8 text".to_owned(),
    })
}

/// A spec file's text as the spec thread takes it.
pub fn prepare(text: &str, format: Format) -> Result<Prepared, SpecError> {
    match format {
        Format::Ts | Format::Js => {
            module_source(text, format).map(|(source, lines)| Prepared::Js { source, lines })
        }
        Format::Json => from_json(text).map(|root| Prepared::Json(Box::new(root))),
    }
}

/// Line and column, from 1, of a byte offset; the column counts characters.
fn position(text: &str, offset: usize) -> (usize, usize) {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    let before = &text[..offset];
    let line = before.matches('\n').count() + 1;
    let start = before.rfind('\n').map_or(0, |at| at + 1);
    (line, before[start..].chars().count() + 1)
}

/// The first error among oxc's diagnostics, as a syntax error in `text`.
fn first_error(diagnostics: &Diagnostics, text: &str) -> Option<SpecError> {
    let error = diagnostics.errors().next()?;
    // The primary label is where the problem is; another may say where,
    // say, the brace it is in was opened.
    let offset = error
        .labels
        .iter()
        .find(|label| label.primary())
        .or_else(|| error.labels.first())
        .map(|label| label.offset() as usize);
    Some(SpecError {
        kind: ErrorKind::Syntax,
        at: offset.map(|offset| position(text, offset)),
        detail: match &error.help {
            Some(help) => format!("{}; {help}", error.message),
            None => error.message.to_string(),
        },
    })
}

/// The module source of a TypeScript or JavaScript spec: TypeScript has its
/// types stripped (and its lines mapped); both are checked for syntax errors
/// and imports.
fn module_source(text: &str, format: Format) -> Result<(String, Option<Lines>), SpecError> {
    let allocator = Allocator::default();
    let source_type = match format {
        Format::Ts => SourceType::ts().with_module(true),
        _ => SourceType::mjs(),
    };
    let parsed = Parser::new(&allocator, text, source_type).parse();
    if let Some(error) = first_error(&parsed.diagnostics, text) {
        return Err(error);
    }
    let mut program = parsed.program;
    let semantic = SemanticBuilder::new()
        .with_check_syntax_error(true)
        .build(&program);
    if let Some(error) = first_error(&semantic.diagnostics, text) {
        return Err(error);
    }
    check_imports(&program, text)?;
    if format != Format::Ts {
        return Ok((text.to_owned(), None));
    }
    let scoping = semantic.semantic.into_scoping();
    // The defaults transform TypeScript only; nothing is lowered.
    let transformed = Transformer::new(
        &allocator,
        Path::new("spec.ts"),
        &TransformOptions::default(),
    )
    .build_with_scoping(scoping, &mut program);
    if let Some(error) = first_error(&transformed.diagnostics, text) {
        return Err(error);
    }
    let generated = Codegen::new()
        .with_options(CodegenOptions {
            source_map_path: Some(PathBuf::from("spec.ts")),
            ..CodegenOptions::default()
        })
        .build(&program);
    let lines = generated.map.map(|map| {
        let mut pairs: Vec<_> = map
            .get_tokens()
            .map(|token| {
                (
                    (token.get_dst_line(), token.get_dst_col()),
                    (token.get_src_line(), token.get_src_col()),
                )
            })
            .collect();
        pairs.sort_unstable();
        Lines(pairs)
    });
    Ok((generated.code, lines))
}

/// Every module the program imports or re-exports values from must be
/// [`GENERATORS`]; what is imported for its types only goes with the types.
/// Checked before the types are stripped, which also drops imports whose
/// bindings are never used: an unknown module is an error even then.
fn check_imports(program: &Program<'_>, text: &str) -> Result<(), SpecError> {
    let is_type = |kind: &ImportOrExportKind| matches!(kind, ImportOrExportKind::Type);
    let sources = program.body.iter().filter_map(|statement| match statement {
        Statement::ImportDeclaration(import) => {
            let types_only = is_type(&import.import_kind)
                || import.specifiers.as_ref().is_some_and(|specifiers| {
                    !specifiers.is_empty()
                        && specifiers.iter().all(|specifier| {
                            matches!(specifier, ImportDeclarationSpecifier::ImportSpecifier(one)
                                if is_type(&one.import_kind))
                        })
                });
            (!types_only).then_some(&import.source)
        }
        Statement::ExportAllDeclaration(export) => {
            (!is_type(&export.export_kind)).then_some(&export.source)
        }
        Statement::ExportFromDeclaration(export) => {
            (!is_type(&export.export_kind)).then_some(&export.source)
        }
        _ => None,
    });
    for source in sources {
        let name = literal(source);
        if name != GENERATORS {
            return Err(SpecError {
                kind: ErrorKind::Import,
                at: Some(position(text, source.span.start as usize)),
                detail: format!(
                    "it imports {name:?}, which fac does not have; the only module a spec can import is {GENERATORS:?}"
                ),
            });
        }
    }
    Ok(())
}

fn literal(source: &StringLiteral<'_>) -> String {
    source.value.as_str().unwrap_or_default().to_owned()
}

/// A JSON spec, read into the model: a syntax error has its line and
/// column, a field of the wrong type its path, and a key the model does not
/// know is an error that names it.
pub fn from_json(text: &str) -> Result<Subcommand, SpecError> {
    let value: Json = serde_json::from_str(text).map_err(|err| SpecError {
        kind: ErrorKind::Syntax,
        at: Some((err.line(), err.column())),
        detail: without_position(&err),
    })?;
    if let Some(problem) = json_functions(&value, "") {
        return Err(problem);
    }
    let mut reader = serde_json::Deserializer::from_str(text);
    let root: Subcommand = serde_path_to_error::deserialize(&mut reader).map_err(|err| {
        let path = err.path().to_string();
        let inner = err.into_inner();
        let mut message = without_position(&inner);
        // A field with several forms (a flag or a number, a string or an
        // object) names fac's own type; say it in Fig's terms.
        if message.contains("did not match any variant of untagged enum") {
            message =
                "not a form Fig allows for this field (see its type in Fig's types)".to_owned();
        }
        SpecError {
            kind: ErrorKind::Field,
            at: Some((inner.line(), inner.column())),
            detail: format!("{path}: {message}"),
        }
    })?;
    let back = serde_json::to_value(&root).map_err(|err| SpecError {
        kind: ErrorKind::Field,
        at: None,
        detail: err.to_string(),
    })?;
    match unknown_keys(&value, &back).first() {
        Some(key) => Err(SpecError {
            kind: ErrorKind::UnknownKey,
            at: None,
            detail: format!("unknown key {key}"),
        }),
        None => Ok(root),
    }
}

/// serde_json's message without its " at line L column C".
fn without_position(err: &serde_json::Error) -> String {
    let message = err.to_string();
    match message.rsplit_once(" at line ") {
        Some((message, _)) => message.to_owned(),
        None => message,
    }
}

/// A key that needs a function (`postProcess`), or a `{"$fn": n}`: JSON has
/// no functions.
fn json_functions(value: &Json, path: &str) -> Option<SpecError> {
    let join = |key: &str| {
        if path.is_empty() {
            key.to_owned()
        } else {
            format!("{path}.{key}")
        }
    };
    match value {
        Json::Object(map) => map.iter().find_map(|(key, item)| {
            if FUNCTION_KEYS.contains(&key.as_str()) || key == "$fn" {
                Some(SpecError {
                    kind: ErrorKind::Function,
                    at: None,
                    detail: format!(
                        "{} needs a function, and a JSON spec has none: use a .ts or .js spec, \
                         or a generator's `script` with `splitOn`",
                        join(key)
                    ),
                })
            } else {
                json_functions(item, &join(key))
            }
        }),
        Json::Array(items) => items
            .iter()
            .enumerate()
            .find_map(|(index, item)| json_functions(item, &join(&index.to_string()))),
        _ => None,
    }
}

/// The keys of `original` (as `path.key`) that the model's JSON form of it,
/// `back`, does not have: the model does not know them. A single value the
/// model made a one-element list is the same value.
pub fn unknown_keys(original: &Json, back: &Json) -> Vec<String> {
    fn visit(original: &Json, back: &Json, path: &str, out: &mut Vec<String>) {
        let join = |key: &str| {
            if path.is_empty() {
                key.to_owned()
            } else {
                format!("{path}.{key}")
            }
        };
        match (original, back) {
            (Json::Object(original), Json::Object(back)) => {
                for (key, value) in original {
                    match back.get(key) {
                        Some(back) => visit(value, back, &join(key), out),
                        None => out.push(join(key)),
                    }
                }
            }
            (Json::Array(original), Json::Array(back)) => {
                for (index, (value, back)) in original.iter().zip(back).enumerate() {
                    visit(value, back, &join(&index.to_string()), out);
                }
            }
            (_, Json::Array(back)) if back.len() == 1 => visit(original, &back[0], path, out),
            _ => {}
        }
    }
    let mut out = Vec::new();
    visit(original, back, "", &mut out);
    out
}

/// What `fac spec check` says about a loaded spec.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Summary {
    /// Below the root, at any depth.
    pub subcommands: usize,
    pub options: usize,
    pub args: usize,
    pub generators: usize,
    /// Generators with a function in them, which run JavaScript.
    pub js_generators: usize,
    /// What will run commands: `(path, what)`.
    pub commands: Vec<(String, String)>,
    /// Generators that run a script but turn none of its output into
    /// suggestions (no `splitOn`, no `postProcess`).
    pub silent: Vec<String>,
}

/// Count a spec's parts and find what runs commands.
pub fn summary(root: &Subcommand) -> Summary {
    let mut summary = Summary::default();
    subcommand(root, "", &mut summary);
    summary
}

fn join(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_owned()
    } else {
        format!("{path}.{key}")
    }
}

fn subcommand(sub: &Subcommand, path: &str, out: &mut Summary) {
    if sub.generate_spec.is_some() {
        out.commands.push((
            join(path, "generateSpec"),
            "a function that gets executeShellCommand and may run commands".to_owned(),
        ));
    }
    load_spec(sub.load_spec.as_ref(), &join(path, "loadSpec"), out);
    for (index, child) in sub.subcommands.iter().enumerate() {
        out.subcommands += 1;
        subcommand(child, &join(path, &format!("subcommands.{index}")), out);
    }
    for (index, option) in sub.options.iter().enumerate() {
        out.options += 1;
        let option_path = join(path, &format!("options.{index}"));
        args(&option.args, &option_path, out);
    }
    args(&sub.args, path, out);
}

fn load_spec(load: Option<&LoadSpec>, path: &str, out: &mut Summary) {
    match load {
        Some(LoadSpec::Fn(_)) => out.commands.push((
            path.to_owned(),
            "a function that gets executeShellCommand and may run commands".to_owned(),
        )),
        Some(LoadSpec::Inline(sub)) => subcommand(sub, path, out),
        Some(LoadSpec::Name(_)) | None => {}
    }
}

fn args(args: &[Arg], path: &str, out: &mut Summary) {
    for (index, arg) in args.iter().enumerate() {
        out.args += 1;
        let arg_path = join(path, &format!("args.{index}"));
        if let Some(directives) = &arg.parser_directives
            && let Some(StringOrFn::Fn(_)) = directives.alias
        {
            out.commands.push((
                join(&arg_path, "parserDirectives.alias"),
                "a function that gets executeShellCommand and may run commands".to_owned(),
            ));
        }
        load_spec(arg.load_spec.as_ref(), &join(&arg_path, "loadSpec"), out);
        for (index, generator) in arg.generators.iter().enumerate() {
            let generator_path = join(&arg_path, &format!("generators.{index}"));
            self::generator(generator, &generator_path, out);
        }
    }
}

fn generator(generator: &Generator, path: &str, out: &mut Summary) {
    out.generators += 1;
    let has_fn = matches!(generator.script, Some(Script::Fn(_)))
        || generator.post_process.is_some()
        || generator.custom.is_some()
        || generator.filter_template_suggestions.is_some()
        || matches!(generator.trigger, Some(Trigger::Fn(_)))
        || matches!(generator.get_query_term, Some(StringOrFn::Fn(_)));
    if has_fn {
        out.js_generators += 1;
    }
    match &generator.script {
        Some(Script::Argv(argv)) => out
            .commands
            .push((join(path, "script"), format!("runs `{}`", argv.join(" ")))),
        Some(Script::Command(input)) => {
            let mut argv = vec![input.command.clone()];
            argv.extend(input.args.iter().cloned());
            out.commands
                .push((join(path, "script"), format!("runs `{}`", argv.join(" "))));
        }
        Some(Script::Fn(_)) => out.commands.push((
            join(path, "script"),
            "a function that picks a command to run".to_owned(),
        )),
        None => {}
    }
    if generator.script.is_some()
        && generator.split_on.is_none()
        && generator.post_process.is_none()
    {
        out.silent.push(path.to_owned());
    }
    if generator.custom.is_some() {
        out.commands.push((
            join(path, "custom"),
            "a function that gets executeShellCommand and may run commands".to_owned(),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("fac-user-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    fn js(text: &str, format: Format) -> Result<String, SpecError> {
        match prepare(text, format)? {
            Prepared::Js { source, .. } => Ok(source),
            Prepared::Json(_) => panic!("a module gave a tree"),
        }
    }

    #[test]
    fn typescript_lines_map_back() {
        let source = "type A = string;\ninterface B {\n  x: number;\n}\nconst f = (): A => {\n  throw new Error(\"x\");\n};\nexport default { name: f() };\n";
        let Ok(Prepared::Js {
            source: out,
            lines: Some(lines),
        }) = prepare(source, Format::Ts)
        else {
            panic!("no lines");
        };
        // `throw` is on line 6, column 3 of the file.
        let (js_line, js_column) = out
            .lines()
            .enumerate()
            .find_map(|(index, line)| line.find("throw").map(|at| (index + 1, at + 1)))
            .expect("a throw");
        let message = format!(
            "Error: x\n    at f (tool.ts:{js_line}:{js_column})\n    at <x> (other.js:1:1)"
        );
        assert_eq!(
            lines.remap(&message, "tool.ts"),
            "Error: x\n    at f (tool.ts:6:3)\n    at <x> (other.js:1:1)"
        );
        assert_eq!(
            lines.remap("no place (tool.ts:x)", "tool.ts"),
            "no place (tool.ts:x)"
        );
    }

    #[test]
    fn names_stay_inside_the_folder() {
        for name in ["git", "aws/ec2", "my-tool_2.x", "a/b/c"] {
            assert!(valid_name(name), "{name}");
        }
        for name in [
            "", "../x", "a/../b", "/abs", "a//b", "a/", ".", "x y", "a\\b", "é",
        ] {
            assert!(!valid_name(name), "{name}");
        }
        assert!(not_a_spec("fig.d", Format::Ts));
        assert!(not_a_spec("aws/types.d", Format::Ts));
        assert!(not_a_spec("tsconfig", Format::Json));
        assert!(!not_a_spec("tsconfig", Format::Ts));
        assert!(!not_a_spec("fig", Format::Ts));
    }

    #[test]
    fn ts_wins_then_js_then_json() {
        let dir = temp("order");
        std::fs::create_dir_all(dir.join("aws")).expect("mkdir");
        for file in [
            "tool.json",
            "tool.js",
            "aws/ec2.json",
            "fig.d.ts",
            "tsconfig.json",
        ] {
            std::fs::write(dir.join(file), "{}").expect("write");
        }
        let found = find(&dir, "tool").expect("found");
        assert_eq!((found.format, found.unused.len()), (Format::Js, 1));
        std::fs::write(dir.join("tool.ts"), "").expect("write");
        let found = find(&dir, "tool").expect("found");
        assert_eq!(found.format, Format::Ts);
        assert_eq!(found.unused, [dir.join("tool.js"), dir.join("tool.json")]);
        assert_eq!(find(&dir, "aws/ec2").map(|f| f.format), Some(Format::Json));
        assert_eq!(find(&dir, "fig.d"), None, "types are not a spec");
        assert_eq!(find(&dir, "tsconfig"), None, "nor is tsconfig.json");
        assert_eq!(find(&dir, "../order/tool"), None);
        let listed = list(&dir);
        let names: Vec<(&str, &[Format])> = listed
            .iter()
            .map(|listed| (listed.name.as_str(), listed.formats.as_slice()))
            .collect();
        assert_eq!(
            names,
            [
                ("aws/ec2", &[Format::Json][..]),
                ("tool", &[Format::Ts, Format::Js, Format::Json][..]),
            ]
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_stamp_changes_with_any_file_of_the_name() {
        let dir = temp("stamp");
        let empty = stamp(&dir, "tool");
        assert_eq!(empty, Stamp::default());
        std::fs::write(dir.join("tool.json"), "{}").expect("write");
        let json = stamp(&dir, "tool");
        assert_ne!(json, empty);
        std::fs::write(dir.join("tool.json"), "{ }").expect("write");
        let grown = stamp(&dir, "tool");
        assert_ne!(grown, json, "a new size");
        std::fs::write(dir.join("tool.ts"), "").expect("write");
        assert_ne!(stamp(&dir, "tool"), grown);
        std::fs::remove_file(dir.join("tool.ts")).expect("rm");
        assert_eq!(stamp(&dir, "tool"), grown);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn typescript_loses_its_types_only() {
        let source = r#"import type { Thing } from "./elsewhere";
import { filepaths, type FilepathsOptions } from "@fig/autocomplete-generators";
interface Extra { depth: number }
type Kind = "a" | "b";
const kinds = ["a", "b"] as const;
const options: FilepathsOptions = { extensions: ["ts"] };
const extra = { depth: 1 } satisfies Extra;
const completionSpec: Fig.Spec = {
  name: "tool",
  args: { generators: filepaths(options) },
  subcommands: kinds.map((kind: Kind): Fig.Subcommand => ({ name: kind, priority: extra.depth })),
};
export default completionSpec;
"#;
        let out = js(source, Format::Ts).expect("strips");
        for gone in [
            "interface",
            "type ",
            "as const",
            "satisfies",
            "Fig.",
            "./elsewhere",
        ] {
            assert!(!out.contains(gone), "{gone} is left in:\n{out}");
        }
        assert!(
            out.contains(r#"import { filepaths } from "@fig/autocomplete-generators";"#),
            "{out}"
        );
        assert!(out.contains("export default completionSpec"), "{out}");
    }

    #[test]
    fn a_syntax_error_has_its_line_and_column() {
        let err = js(
            "const spec = {\n  name: \"x\"\n  args: {},\n};\n",
            Format::Ts,
        )
        .expect_err("a missing comma");
        assert_eq!((err.kind, err.at), (ErrorKind::Syntax, Some((3, 3))));
        assert_eq!(err.summary(), "3:3: a syntax error");
        let err = js("export default {\n  name: \"x\",\n  é: (,\n};", Format::Js)
            .expect_err("JavaScript too");
        assert_eq!(
            (err.kind, err.at.map(|at| at.0)),
            (ErrorKind::Syntax, Some(3))
        );
        // Types are not JavaScript.
        let err = js("const x: number = 1;\nexport default {};", Format::Js).expect_err("types");
        assert_eq!(err.at.map(|at| at.0), Some(1), "{err}");
    }

    #[test]
    fn an_unknown_import_names_the_module() {
        let err = js(
            "const a = 1;\nimport { x } from \"lodash\";\nexport default { name: \"x\" };",
            Format::Ts,
        )
        .expect_err("lodash");
        assert_eq!((err.kind, err.at), (ErrorKind::Import, Some((2, 19))));
        assert!(err.to_string().contains("\"lodash\""), "{err}");
        assert!(
            !err.summary().contains("lodash"),
            "the log never quotes the file"
        );
        let err = js(
            "export * from \"./mine.js\";\nexport default {};",
            Format::Js,
        )
        .expect_err("re-export");
        assert!(err.to_string().contains("./mine.js"), "{err}");
        // A type-only import goes with the types.
        for types in [
            "import type { X } from \"lodash\";",
            "import { type X, type Y } from \"lodash\";",
            "export type { X } from \"lodash\";",
        ] {
            let source = format!("{types}\nexport default {{}};");
            assert!(js(&source, Format::Ts).is_ok(), "{types}");
        }
    }

    #[test]
    fn json_reads_into_the_model() {
        let root = from_json(
            r#"{"name": "tool", "subcommands": [{"name": "run", "args": {"name": "x",
              "generators": {"script": ["ls"], "splitOn": "\n"}}}],
              "options": [{"name": ["-v", "--verbose"]}]}"#,
        )
        .expect("a spec");
        assert_eq!(root.name, ["tool"]);
        assert_eq!(
            root.subcommands[0].args[0].generators[0]
                .split_on
                .as_deref(),
            Some("\n")
        );
        assert_eq!(root.options[0].name, ["-v", "--verbose"]);
    }

    #[test]
    fn json_problems_say_where_they_are() {
        let err = from_json(
            "{\n  \"name\": \"tool\",\n  \"subcommands\": [{\"name\": \"a\", \"hiden\": true}]\n}",
        )
        .expect_err("a typo");
        assert_eq!(err.kind, ErrorKind::UnknownKey);
        assert_eq!(err.to_string(), "unknown key subcommands.0.hiden");
        assert!(!err.summary().contains("hiden"));
        let err = from_json("{\"name\": \"tool\",\n \"args\": {\"isOptional\": \"yes\"}}")
            .expect_err("a string for a bool");
        assert_eq!(
            (err.kind, err.at.map(|at| at.0)),
            (ErrorKind::Field, Some(2))
        );
        assert!(err.detail.starts_with("args.isOptional: "), "{err}");
        let err = from_json("{\"name\": \"tool\",\n\n  \"args\": [}").expect_err("syntax");
        assert_eq!((err.kind, err.at), (ErrorKind::Syntax, Some((3, 12))));
        let err = from_json(
            r#"{"name": "t", "args": {"generators": {"script": ["ls"], "postProcess": "x"}}}"#,
        )
        .expect_err("a function key");
        assert_eq!(err.kind, ErrorKind::Function);
        assert!(
            err.detail.starts_with("args.generators.postProcess "),
            "{err}"
        );
        let err = from_json(r#"{"name": "t", "loadSpec": {"$fn": 0}}"#).expect_err("$fn");
        assert_eq!(err.kind, ErrorKind::Function);
    }

    #[test]
    fn a_summary_counts_and_names_what_runs() {
        let root = from_json(
            r#"{"name": "tool",
              "subcommands": [{"name": "a", "subcommands": [{"name": "b"}],
                "args": {"generators": [{"script": ["git", "branch"], "splitOn": "\n"},
                                        {"script": {"command": "ls", "args": ["-1"]}}]}}],
              "options": [{"name": "-x", "args": {"name": "f", "template": "filepaths"}}, {"name": "-y"}]}"#,
        )
        .expect("a spec");
        let summary = summary(&root);
        assert_eq!(
            (
                summary.subcommands,
                summary.options,
                summary.args,
                summary.generators
            ),
            (2, 2, 2, 2)
        );
        assert_eq!(summary.js_generators, 0);
        assert_eq!(
            summary.commands,
            [
                (
                    "subcommands.0.args.0.generators.0.script".to_owned(),
                    "runs `git branch`".to_owned()
                ),
                (
                    "subcommands.0.args.0.generators.1.script".to_owned(),
                    "runs `ls -1`".to_owned()
                ),
            ]
        );
        assert_eq!(summary.silent, ["subcommands.0.args.0.generators.1"]);
    }
}
