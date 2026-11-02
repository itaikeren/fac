//! `fac spec new <name> [--js | --json]`: a starter spec in the user spec
//! folder, modelled on Fig's `create-completion-spec`. A TypeScript starter
//! also gets `fig.d.ts` (Fig's `@withfig/autocomplete-types` 1.31.0 and the
//! types of the built-in `@fig/autocomplete-generators`) and a
//! `tsconfig.json`, so an editor completes and checks `Fig.Spec` with no npm
//! install. Neither is ever loaded as a spec (see [`super::user::not_a_spec`]).

use std::io::Write as _;
use std::path::{Path, PathBuf};

use super::user::{self, Format};

/// The editor's types, written as `fig.d.ts`. Its first line is
/// `// fac-types: N`; a file with a lower `N` (or none) is replaced.
pub const FIG_TYPES: &str = include_str!("fig.d.ts");

/// Where Fig's spec fields are described, now that Fig's site is gone.
pub const TYPES_URL: &str =
    "https://github.com/withfig/autocomplete-tools/blob/main/types/index.d.ts";

const TSCONFIG: &str = r#"{
  "compilerOptions": {
    "target": "ES2022",
    "module": "ESNext",
    "moduleResolution": "Bundler",
    "strict": true,
    "noEmit": true,
    "skipLibCheck": true,
    "types": []
  },
  "include": ["**/*.ts"]
}
"#;

/// What [`create`] wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Created {
    pub path: PathBuf,
    /// `fig.d.ts`, when it was written or refreshed.
    pub types: Option<PathBuf>,
    /// `tsconfig.json`, when it was written.
    pub tsconfig: Option<PathBuf>,
}

/// Write the starter spec for `name` into `dir` (created when missing). An
/// existing file is never overwritten. The problem is one line.
pub fn create(dir: &Path, name: &str, format: Format) -> Result<Created, String> {
    if !user::valid_name(name) {
        return Err(format!(
            "{name:?} is not a valid spec name (letters, digits, '.', '_' and '-', with '/' between the parts of a nested name)"
        ));
    }
    if user::not_a_spec(name, format) {
        return Err(format!(
            "{} is the editor's file, not a spec; pick another name",
            user::path(Path::new(""), name, format).display()
        ));
    }
    let path = user::path(dir, name, format);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("cannot create {}: {}", parent.display(), err.kind()))?;
    }
    write_new(&path, &starter(name, format)).map_err(|err| match err.kind() {
        std::io::ErrorKind::AlreadyExists => {
            format!("{} already exists; not overwriting it", path.display())
        }
        kind => format!("cannot write {}: {kind}", path.display()),
    })?;
    let (mut types, mut tsconfig) = (None, None);
    if format == Format::Ts {
        types = write_types(dir)?;
        let config = dir.join("tsconfig.json");
        match write_new(&config, TSCONFIG) {
            Ok(()) => tsconfig = Some(config),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(format!("cannot write {}: {}", config.display(), err.kind())),
        }
    }
    Ok(Created {
        path,
        types,
        tsconfig,
    })
}

/// Create `path` with `text`; fails if it exists.
fn write_new(path: &Path, text: &str) -> std::io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(text.as_bytes())
}

/// The `N` of a `// fac-types: N` first line.
fn types_version(text: &str) -> Option<u32> {
    text.lines()
        .next()?
        .strip_prefix("// fac-types: ")?
        .trim()
        .parse()
        .ok()
}

/// Write `fig.d.ts` when it is missing or older than fac's; the path when
/// it was written.
fn write_types(dir: &Path) -> Result<Option<PathBuf>, String> {
    let path = dir.join("fig.d.ts");
    let ours = types_version(FIG_TYPES);
    match std::fs::read_to_string(&path) {
        Ok(theirs) if types_version(&theirs) >= ours => return Ok(None),
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(format!("cannot read {}: {}", path.display(), err.kind())),
    }
    std::fs::write(&path, FIG_TYPES)
        .map_err(|err| format!("cannot write {}: {}", path.display(), err.kind()))?;
    Ok(Some(path))
}

/// The starter spec for `name` (its command is the last part of a nested
/// name).
pub fn starter(name: &str, format: Format) -> String {
    let command = name.rsplit('/').next().unwrap_or(name);
    let text = match format {
        Format::Ts => TS,
        Format::Js => JS,
        Format::Json => JSON,
    };
    text.replace("fac spec check mytool", &format!("fac spec check {name}"))
        .replace("mytool", command)
}

const TS: &str = r#"// The completion spec for `mytool`: what fac suggests after you type
// `mytool `. Save a change and fac uses it at your next prompt; when
// something looks wrong, `fac spec check mytool` says what.
//
// The fields are Fig's. Your editor completes them and shows their docs
// from fig.d.ts, next to this file; online they are at
// https://github.com/withfig/autocomplete-tools/blob/main/types/index.d.ts
import { filepaths } from "@fig/autocomplete-generators";

const completionSpec: Fig.Spec = {
  name: "mytool",
  description: "What mytool does",
  // The words that can follow `mytool`, each with its own options and args.
  subcommands: [
    {
      name: "open",
      description: "Open a document",
      // `mytool open <file>`: files, as Fig's `filepaths` helper lists them
      // (folders too, so you can go into them).
      args: {
        name: "file",
        generators: filepaths({ extensions: ["md", "txt"] }),
      },
    },
  ],
  // Flags, with every name each one goes by.
  options: [
    {
      // An option with a required argument: `mytool --config <path>`.
      name: ["--config", "-c"],
      description: "Read the settings from this file",
      args: { name: "path", template: "filepaths" },
    },
    {
      // An option without one.
      name: ["--verbose", "-v"],
      description: "Say more about what happens",
    },
  ],
  // `mytool <branch>`: suggestions from a command's output, one per line.
  args: {
    name: "branch",
    isOptional: true,
    generators: {
      script: ["git", "branch", "--format=%(refname:short)"],
      splitOn: "\n",
    },
  },
};

export default completionSpec;
"#;

const JS: &str = r#"// The completion spec for `mytool`: what fac suggests after you type
// `mytool `. Save a change and fac uses it at your next prompt; when
// something looks wrong, `fac spec check mytool` says what.
//
// The fields are Fig's, described in
// https://github.com/withfig/autocomplete-tools/blob/main/types/index.d.ts
import { filepaths } from "@fig/autocomplete-generators";

/** @type {Fig.Spec} */
const completionSpec = {
  name: "mytool",
  description: "What mytool does",
  // The words that can follow `mytool`, each with its own options and args.
  subcommands: [
    {
      name: "open",
      description: "Open a document",
      // `mytool open <file>`: files, as Fig's `filepaths` helper lists them
      // (folders too, so you can go into them).
      args: {
        name: "file",
        generators: filepaths({ extensions: ["md", "txt"] }),
      },
    },
  ],
  // Flags, with every name each one goes by.
  options: [
    {
      // An option with a required argument: `mytool --config <path>`.
      name: ["--config", "-c"],
      description: "Read the settings from this file",
      args: { name: "path", template: "filepaths" },
    },
    {
      // An option without one.
      name: ["--verbose", "-v"],
      description: "Say more about what happens",
    },
  ],
  // `mytool <branch>`: suggestions from a command's output, one per line.
  args: {
    name: "branch",
    isOptional: true,
    generators: {
      script: ["git", "branch", "--format=%(refname:short)"],
      splitOn: "\n",
    },
  },
};

export default completionSpec;
"#;

const JSON: &str = r#"{
  "name": "mytool",
  "description": "What mytool does",
  "subcommands": [
    {
      "name": "open",
      "description": "Open a document",
      "args": { "name": "file", "template": "filepaths" }
    }
  ],
  "options": [
    {
      "name": ["--config", "-c"],
      "description": "Read the settings from this file",
      "args": { "name": "path", "template": "filepaths" }
    },
    {
      "name": ["--verbose", "-v"],
      "description": "Say more about what happens"
    }
  ],
  "args": {
    "name": "branch",
    "isOptional": true,
    "generators": {
      "script": ["git", "branch", "--format=%(refname:short)"],
      "splitOn": "\n"
    }
  }
}
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::specs::user::Prepared;

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("fac-starter-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn every_starter_prepares() {
        for format in Format::ALL {
            let text = starter("tools/deploy", format);
            assert!(
                text.contains("\"deploy\"") && !text.contains("mytool"),
                "{format:?}"
            );
            if format != Format::Json {
                assert!(text.contains("fac spec check tools/deploy"), "{format:?}");
            }
            match user::prepare(&text, format) {
                Ok(Prepared::Json(root)) => {
                    assert_eq!(format, Format::Json);
                    let summary = user::summary(&root);
                    assert_eq!(
                        (
                            summary.subcommands,
                            summary.options,
                            summary.args,
                            summary.generators
                        ),
                        (1, 2, 3, 1)
                    );
                }
                Ok(Prepared::Js { source, .. }) => {
                    assert_ne!(format, Format::Json);
                    assert!(
                        source.contains("@fig/autocomplete-generators"),
                        "{format:?}"
                    );
                }
                Err(err) => panic!("{format:?}: {err}"),
            }
        }
    }

    #[test]
    fn new_writes_once_with_the_editor_files() {
        let dir = temp("new");
        let created = create(&dir, "mytool", Format::Ts).expect("created");
        assert_eq!(created.path, dir.join("mytool.ts"));
        assert_eq!(created.types, Some(dir.join("fig.d.ts")));
        assert_eq!(created.tsconfig, Some(dir.join("tsconfig.json")));
        assert_eq!(
            std::fs::read_to_string(dir.join("fig.d.ts")).expect("types"),
            FIG_TYPES
        );
        // Once only: a second TypeScript spec leaves both alone.
        std::fs::write(dir.join("tsconfig.json"), "{}").expect("edit tsconfig");
        let again = create(&dir, "other", Format::Ts).expect("created");
        assert_eq!((again.types, again.tsconfig), (None, None));
        assert_eq!(
            std::fs::read_to_string(dir.join("tsconfig.json"))
                .ok()
                .as_deref(),
            Some("{}")
        );
        // Older types are replaced.
        std::fs::write(dir.join("fig.d.ts"), "// fac-types: 0\n").expect("old types");
        let refreshed = create(&dir, "third", Format::Ts).expect("created");
        assert_eq!(refreshed.types, Some(dir.join("fig.d.ts")));
        // JavaScript and JSON get no editor files.
        let js = create(&dir, "nested/tool", Format::Js).expect("created");
        assert_eq!(
            (js.path, js.types, js.tsconfig),
            (dir.join("nested/tool.js"), None, None)
        );
        // Never overwritten.
        std::fs::write(dir.join("mytool.ts"), "mine").expect("edit");
        let err = create(&dir, "mytool", Format::Ts).expect_err("exists");
        assert!(
            err.contains("already exists") && !err.contains('\n'),
            "{err}"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("mytool.ts"))
                .ok()
                .as_deref(),
            Some("mine")
        );
        // Bad names, and the editor's own files.
        for (name, format) in [
            ("../evil", Format::Ts),
            ("a b", Format::Js),
            ("", Format::Json),
            ("fig.d", Format::Ts),
            ("tsconfig", Format::Json),
        ] {
            assert!(create(&dir, name, format).is_err(), "{name}");
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}
