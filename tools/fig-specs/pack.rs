//! fac-pack-specs: packs Fig's compiled completion specs into the archive fac
//! embeds (`specs/fig.zst` and `specs/index.txt`), plus a report.
//!
//! Usage: fac-pack-specs <build-dir> <overrides-dir> <out-dir>
//!
//! Each `.js` module is evaluated once in a fresh QuickJS context. Its default
//! export goes through the shared walker (`src/specs/walk.js`), which replaces
//! functions with numbered `{"$fn": n}` references; the JSON is read into the
//! spec model, stored with postcard and compressed as its own zstd frame. A
//! module with functions also keeps its source, as a second frame, for the
//! spec thread to evaluate later. A module in the overrides dir replaces the
//! vendored module of the same name.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context as _, Result, anyhow, bail};
use fac::specs::WALK_JS;
use fac::specs::archive::{INDEX_HEADER, Kind};
use fac::specs::model::{Entry, LoadSpec, StaticSuggestion, Subcommand, SuggestionType};
use rquickjs::{CatchResultExt, Context, Ctx, Function, Module, Object, Runtime, Value};
use serde::Deserialize;
use serde_json::Value as Json;

/// zstd level for every frame.
const LEVEL: i32 = 19;

/// Findings (see [`Finding`]) met in the vendored specs, each with why it is
/// fine. The pack fails on a finding without a reason here: fix the model,
/// fix the spec with an override, or explain it.
const KNOWN_FINDINGS: &[(&str, &str)] = &[
    (
        "subcommands.prority",
        "fisher misspells priority; Fig ignores the key too",
    ),
    (
        "suggestions.args",
        "netlify gives suggestions subcommand fields, which a suggestion lacks; Fig ignores them",
    ),
    (
        "suggestions.options",
        "netlify gives suggestions subcommand fields, which a suggestion lacks; Fig ignores them",
    ),
    ("subcommands.0", HASURA_SPREAD),
    ("subcommands.1", HASURA_SPREAD),
    ("subcommands.2", HASURA_SPREAD),
    ("subcommands.3", HASURA_SPREAD),
    ("subcommands.4", HASURA_SPREAD),
    ("subcommands.5", HASURA_SPREAD),
    (
        "default (number -> string)",
        "ionic gives some arg defaults as numbers; kept as the text JS shows",
    ),
];

const HASURA_SPREAD: &str =
    "hasura spreads an array of subcommands into one object; Fig sees a nameless subcommand";

/// Modules that are part of the package but are not specs.
const NOT_SPECS: &[(&str, &str)] = &[
    ("index.js", "the package's list of spec names, not a spec"),
    (
        "dynamic/index.js",
        "a table of import() loaders for every spec, not a spec",
    ),
];

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [build, overrides, out] = &args[..] else {
        eprintln!("usage: fac-pack-specs <build-dir> <overrides-dir> <out-dir>");
        return ExitCode::from(2);
    };
    match run(Path::new(build), Path::new(overrides), Path::new(out)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("fac-pack-specs: {err:#}");
            ExitCode::FAILURE
        }
    }
}

/// What one module turned into.
enum Outcome {
    Packed(Box<Packed>),
    Skipped(String),
}

struct Packed {
    name: String,
    entry: Entry,
    source: Vec<u8>,
}

/// Category counts and findings, for the report.
#[derive(Default)]
struct Tally {
    files: usize,
    spec_plain: usize,
    spec_fns: usize,
    versioned: usize,
    version_file: usize,
    skipped: Vec<(String, String)>,
    findings: Findings,
    /// Suggestion `type` names outside Fig's list → count.
    other_types: BTreeMap<String, usize>,
}

/// What the model did not keep as it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Finding {
    /// A key the model has no field for; dropped.
    Unknown,
    /// A value of another JSON type than Fig's types say; converted.
    Changed,
}

/// (finding, `parent.key`) → (count, first example).
type Findings = BTreeMap<(Finding, String), (usize, String)>;

fn run(build: &Path, overrides: &Path, out: &Path) -> Result<()> {
    let runtime = Runtime::new().context("cannot start QuickJS")?;
    let mut tally = Tally::default();
    let mut packed: BTreeMap<String, Box<Packed>> = BTreeMap::new();

    let vendored = js_files(build).with_context(|| format!("cannot list {}", build.display()))?;
    for rel in &vendored {
        tally.files += 1;
        match pack_file(&runtime, build, rel, &mut tally.findings)? {
            Outcome::Skipped(reason) => tally.skipped.push((rel.clone(), reason)),
            Outcome::Packed(item) => {
                match &item.entry {
                    Entry::Spec { fns, .. } if fns.is_empty() => tally.spec_plain += 1,
                    Entry::Spec { .. } => tally.spec_fns += 1,
                    Entry::Versioned { .. } => tally.versioned += 1,
                    Entry::VersionFile { .. } => tally.version_file += 1,
                }
                if packed.contains_key(&item.name) {
                    bail!("{rel}: a second module named {}", item.name);
                }
                packed.insert(item.name.clone(), item);
            }
        }
    }

    let mut override_lines = Vec::new();
    let override_files =
        js_files(overrides).with_context(|| format!("cannot list {}", overrides.display()))?;
    for rel in &override_files {
        let item = match pack_file(&runtime, overrides, rel, &mut tally.findings)? {
            Outcome::Packed(item) => item,
            Outcome::Skipped(reason) => bail!("override {rel}: {reason}"),
        };
        let action = if packed.contains_key(&item.name) {
            "replaces"
        } else {
            "adds"
        };
        override_lines.push(format!("  {rel} {action} {}", item.name));
        packed.insert(item.name.clone(), item);
    }

    let unexplained: Vec<String> = tally
        .findings
        .iter()
        .filter(|((_, key), _)| !KNOWN_FINDINGS.iter().any(|(known, _)| known == key))
        .map(|((_, key), (count, example))| format!("{key}: {count}, e.g. {example}"))
        .collect();
    if !unexplained.is_empty() {
        bail!(
            "the model dropped or converted values with no recorded reason \
             (fix the model, add an override, or explain them in KNOWN_FINDINGS):\n  {}",
            unexplained.join("\n  ")
        );
    }

    let mut data = Vec::new();
    let mut index = String::from(INDEX_HEADER);
    let (mut entry_bytes, mut entry_raw, mut js_bytes, mut js_raw, mut js_count) = (0, 0, 0, 0, 0);
    for (name, item) in &packed {
        if let Some(root) = item.entry.root() {
            other_types(root, &mut tally.other_types);
        }
        let raw =
            postcard::to_allocvec(&item.entry).with_context(|| format!("{name}: postcard"))?;
        let frame = zstd::encode_all(&raw[..], LEVEL).with_context(|| format!("{name}: zstd"))?;
        let entry_at = data.len();
        data.extend_from_slice(&frame);
        entry_bytes += frame.len();
        entry_raw += raw.len();
        let js = if item.entry.has_functions() {
            let frame = zstd::encode_all(&item.source[..], LEVEL)
                .with_context(|| format!("{name}: zstd"))?;
            let at = data.len();
            data.extend_from_slice(&frame);
            js_bytes += frame.len();
            js_raw += item.source.len();
            js_count += 1;
            format!("{at} {}", frame.len())
        } else {
            "- -".to_owned()
        };
        let kind = Kind::of(&item.entry).as_str();
        writeln!(index, "{name} {kind} {entry_at} {} {js}", frame.len())?;
    }

    let mut report = String::new();
    let r = &mut report;
    writeln!(r, "fac-pack-specs report")?;
    writeln!(r)?;
    writeln!(
        r,
        "Source modules (.js files in the build dir): {}",
        tally.files
    )?;
    writeln!(r, "  spec without functions: {}", tally.spec_plain)?;
    writeln!(r, "  spec with functions:    {}", tally.spec_fns)?;
    writeln!(r, "  versioned:              {}", tally.versioned)?;
    writeln!(r, "  version file:           {}", tally.version_file)?;
    writeln!(r, "  skipped:                {}", tally.skipped.len())?;
    for (rel, reason) in &tally.skipped {
        writeln!(r, "    {rel}: {reason}")?;
    }
    writeln!(r)?;
    writeln!(r, "Overrides: {}", override_lines.len())?;
    for line in &override_lines {
        writeln!(r, "{line}")?;
    }
    writeln!(r)?;
    for (finding, title) in [
        (Finding::Unknown, "Unknown fields (dropped)"),
        (Finding::Changed, "Values of another type (converted)"),
    ] {
        let rows: Vec<_> = tally
            .findings
            .iter()
            .filter(|((kind, _), _)| *kind == finding)
            .collect();
        writeln!(r, "{title}: {}", rows.len())?;
        for ((_, key), (count, example)) in rows {
            let reason = KNOWN_FINDINGS
                .iter()
                .find(|(known, _)| known == key)
                .map_or("", |(_, reason)| reason);
            writeln!(r, "  {key}: {count}, e.g. {example}")?;
            writeln!(r, "    {reason}")?;
        }
    }
    writeln!(
        r,
        "Suggestion types outside Fig's list (kept as written): {}",
        tally.other_types.len()
    )?;
    for (name, count) in &tally.other_types {
        writeln!(r, "  {name}: {count}")?;
    }
    writeln!(r)?;
    writeln!(r, "Entries: {}", packed.len())?;
    writeln!(r, "Size of fig.zst: {} bytes", data.len())?;
    writeln!(
        r,
        "  entry frames: {entry_bytes} bytes ({entry_raw} bytes of postcard)"
    )?;
    writeln!(
        r,
        "  js frames:    {js_bytes} bytes ({js_count} modules, {js_raw} bytes of source)"
    )?;

    std::fs::create_dir_all(out).with_context(|| format!("cannot create {}", out.display()))?;
    write(&out.join("fig.zst"), &data)?;
    write(&out.join("index.txt"), index.as_bytes())?;
    write(&out.join("pack-report.txt"), report.as_bytes())?;
    print!("{report}");
    Ok(())
}

/// Count the suggestion types outside Fig's list in a spec tree.
fn other_types(subcommand: &Subcommand, out: &mut BTreeMap<String, usize>) {
    let mut count = |suggestions: &[StaticSuggestion]| {
        for item in suggestions {
            if let StaticSuggestion::Suggestion(suggestion) = item
                && let Some(SuggestionType::Other(name)) = &suggestion.kind
            {
                *out.entry(name.clone()).or_default() += 1;
            }
        }
    };
    count(&subcommand.additional_suggestions);
    let args = subcommand
        .args
        .iter()
        .chain(subcommand.options.iter().flat_map(|option| &option.args));
    let mut inline = Vec::new();
    for arg in args {
        count(&arg.suggestions);
        if let Some(LoadSpec::Inline(spec)) = &arg.load_spec {
            inline.push(spec);
        }
    }
    if let Some(LoadSpec::Inline(spec)) = &subcommand.load_spec {
        inline.push(spec);
    }
    for spec in inline {
        other_types(spec, out);
    }
    for sub in &subcommand.subcommands {
        other_types(sub, out);
    }
}

fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    std::fs::write(path, bytes).with_context(|| format!("cannot write {}", path.display()))
}

/// Every `.js` file under `root`, as sorted `/`-separated relative paths. A
/// missing directory has none.
fn js_files(root: &Path) -> Result<Vec<String>> {
    fn visit(root: &Path, dir: &Path, out: &mut Vec<String>) -> Result<()> {
        for item in std::fs::read_dir(dir)? {
            let path = item?.path();
            if path.is_dir() {
                visit(root, &path, out)?;
            } else if path.extension().is_some_and(|ext| ext == "js") {
                let rel = path.strip_prefix(root)?;
                let parts: Vec<&str> = rel
                    .iter()
                    .map(|part| {
                        part.to_str()
                            .ok_or_else(|| anyhow!("non-UTF-8 path {rel:?}"))
                    })
                    .collect::<Result<_>>()?;
                out.push(parts.join("/"));
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    if root.exists() {
        visit(root, root, &mut out)?;
    }
    out.sort();
    Ok(out)
}

/// The entry name for a module path: the path without `.js`, and a versioned
/// spec's `<name>/index.js` is `<name>`.
fn entry_name(rel: &str) -> String {
    let stem = rel.strip_suffix(".js").unwrap_or(rel);
    stem.strip_suffix("/index").unwrap_or(stem).to_owned()
}

/// What evaluating a module gave.
enum Evaluated {
    NoDefault,
    /// The default export is neither a function nor a plain object: a
    /// helper module.
    NotSpec(&'static str),
    Function {
        json: String,
        fns: Vec<String>,
    },
    Object {
        json: String,
        fns: Vec<String>,
        /// A version table's JSON; its functions are in `fns`.
        versions: Option<String>,
    },
}

fn pack_file(runtime: &Runtime, dir: &Path, rel: &str, findings: &mut Findings) -> Result<Outcome> {
    if let Some((_, reason)) = NOT_SPECS.iter().find(|(path, _)| *path == rel) {
        return Ok(Outcome::Skipped((*reason).to_owned()));
    }
    let path: PathBuf = dir.join(rel);
    let source = std::fs::read(&path).with_context(|| format!("cannot read {rel}"))?;
    let evaluated = evaluate(runtime, rel, &source).with_context(|| rel.to_owned())?;
    let name = entry_name(rel);
    let entry = match evaluated {
        Evaluated::NoDefault => return Ok(Outcome::Skipped("no default export".to_owned())),
        Evaluated::NotSpec(kind) => {
            return Ok(Outcome::Skipped(format!(
                "the default export is {kind}, not a spec"
            )));
        }
        Evaluated::Function { json, fns } => {
            if json != r#"{"$fn":0}"# || fns != [""] {
                bail!("{rel}: unexpected walk of a function default: {json}");
            }
            Entry::Versioned { fns }
        }
        Evaluated::Object {
            json,
            fns,
            versions,
        } => {
            let root = spec_tree(&json, rel, findings)?;
            match versions {
                None => Entry::Spec { root, fns },
                Some(versions) => {
                    // The table stays opaque JSON text; parsing it here only
                    // checks it is JSON.
                    let _: Json = serde_json::from_str(&versions)
                        .with_context(|| format!("{rel}: bad versions JSON"))?;
                    Entry::VersionFile {
                        root,
                        fns,
                        versions,
                    }
                }
            }
        }
    };
    Ok(Outcome::Packed(Box::new(Packed {
        name,
        entry,
        source,
    })))
}

/// Read walked JSON into the model, and record every key the model dropped.
fn spec_tree(json: &str, rel: &str, findings: &mut Findings) -> Result<Subcommand> {
    let value: Json = serde_json::from_str(json).with_context(|| format!("{rel}: bad JSON"))?;
    let root = Subcommand::deserialize(&value).with_context(|| format!("{rel}: model"))?;
    let back = serde_json::to_value(&root).with_context(|| format!("{rel}: model to JSON"))?;
    dropped_keys(&value, &back, "(root)", "", &mut |finding, key, path| {
        let slot = findings
            .entry((finding, key))
            .or_insert((0, format!("{rel}: {path}")));
        slot.0 += 1;
    });
    Ok(root)
}

/// Compare walked JSON with the model's JSON form. Report each object key of
/// `original` missing from `back` as `parent.key`, and each value whose JSON
/// type changed as `parent.key (from -> to)`, where parent is the nearest
/// non-index path segment; the full path comes along as an example. Single
/// values normalised to one-element lists are not changes.
fn dropped_keys(
    original: &Json,
    back: &Json,
    parent: &str,
    path: &str,
    found: &mut impl FnMut(Finding, String, String),
) {
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
                    Some(back) => dropped_keys(value, back, key, &join(key), found),
                    None => found(Finding::Unknown, format!("{parent}.{key}"), join(key)),
                }
            }
        }
        (Json::Array(original), Json::Array(back)) => {
            for (index, (value, back)) in original.iter().zip(back).enumerate() {
                dropped_keys(value, back, parent, &join(&index.to_string()), found);
            }
        }
        (_, Json::Array(back)) if back.len() == 1 => {
            dropped_keys(original, &back[0], parent, path, found);
        }
        _ => {
            let (from, to) = (json_type(original), json_type(back));
            if from != to {
                found(
                    Finding::Changed,
                    format!("{parent} ({from} -> {to})"),
                    path.to_owned(),
                );
            }
        }
    }
}

fn json_type(value: &Json) -> &'static str {
    match value {
        Json::Null => "null",
        Json::Bool(_) => "boolean",
        Json::Number(_) => "number",
        Json::String(_) => "string",
        Json::Array(_) => "array",
        Json::Object(_) => "object",
    }
}

fn evaluate(runtime: &Runtime, rel: &str, source: &[u8]) -> Result<Evaluated> {
    let context = Context::full(runtime).context("cannot create a QuickJS context")?;
    context.with(|ctx| evaluate_in(&ctx, rel, source))
}

fn evaluate_in<'js>(ctx: &Ctx<'js>, rel: &str, source: &[u8]) -> Result<Evaluated> {
    let walk: Function = js(ctx, ctx.eval(WALK_JS))?;
    let module = js(ctx, Module::declare(ctx.clone(), rel, source.to_vec()))?;
    let (module, promise) = js(ctx, module.eval())?;
    js(ctx, promise.finish::<()>())?;
    let namespace = js(ctx, module.namespace())?;
    if !js(ctx, namespace.contains_key("default"))? {
        return Ok(Evaluated::NoDefault);
    }
    let default: Value = js(ctx, namespace.get("default"))?;
    if default.is_function() {
        let (json, fns) = run_walk(ctx, &walk, default, None)?;
        return Ok(Evaluated::Function { json, fns });
    }
    if default.is_array() {
        return Ok(Evaluated::NotSpec("an array"));
    }
    if !default.is_object() {
        return Ok(Evaluated::NotSpec(default.type_name()));
    }
    let (json, mut fns) = run_walk(ctx, &walk, default, None)?;
    let versions = if js(ctx, namespace.contains_key("versions"))? {
        if fns.iter().any(|path| path.starts_with("versions.")) {
            bail!("a spec path starts with versions., which the version table's paths use");
        }
        let versions: Value = js(ctx, namespace.get("versions"))?;
        let (versions, version_fns) = run_walk(ctx, &walk, versions, Some(fns.len()))?;
        fns.extend(version_fns);
        Some(versions)
    } else {
        None
    };
    Ok(Evaluated::Object {
        json,
        fns,
        versions,
    })
}

/// Call the walker on a value: its JSON text and function paths. With
/// `first`, the value is a version table: paths start with `versions.` and
/// ids start at `first`.
fn run_walk<'js>(
    ctx: &Ctx<'js>,
    walk: &Function<'js>,
    value: Value<'js>,
    first: Option<usize>,
) -> Result<(String, Vec<String>)> {
    let walked: Object = match first {
        None => js(ctx, walk.call((value,)))?,
        Some(first) => js(ctx, walk.call((value, "versions", first)))?,
    };
    let json: String = js(ctx, walked.get("json"))?;
    let fns: Vec<String> = js(ctx, walked.get("fns"))?;
    Ok((json, fns))
}

/// A QuickJS result, with a thrown exception's message and stack as the error.
fn js<'js, T>(ctx: &Ctx<'js>, result: rquickjs::Result<T>) -> Result<T> {
    result.catch(ctx).map_err(|err| anyhow!("{err}"))
}
