//! The packed spec archive embedded in fac: `specs/fig.zst` holds one zstd
//! frame per entry (a postcard [`Entry`]) plus, for entries with functions, one
//! frame of module source; `specs/index.txt` says where each frame is. Both
//! are written by `fac-pack-specs`. One entry is decoded at a time.

use std::borrow::Cow;
use std::ops::Range;
use std::sync::LazyLock;

use super::model::Entry;

static FIG_ZST: &[u8] = include_bytes!("../../specs/fig.zst");
static INDEX_TXT: &str = include_str!("../../specs/index.txt");

/// The header `fac-pack-specs` writes at the top of `index.txt`.
pub const INDEX_HEADER: &str = "\
# fac spec archive index, written by fac-pack-specs. One line per entry, sorted by name:
#   name kind entry_offset entry_len js_offset js_len
# kind is spec, versioned or version-file. Offsets and lengths are bytes in fig.zst;
# each range is one zstd frame. The entry frame is a postcard Entry; the js frame is
# the module source, present only when the entry has functions (else \"- -\").
";

/// What an entry holds; mirrors the [`Entry`] variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Spec,
    Versioned,
    VersionFile,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Spec => "spec",
            Self::Versioned => "versioned",
            Self::VersionFile => "version-file",
        }
    }

    pub fn of(entry: &Entry) -> Self {
        match entry {
            Entry::Spec { .. } => Self::Spec,
            Entry::Versioned { .. } => Self::Versioned,
            Entry::VersionFile { .. } => Self::VersionFile,
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text {
            "spec" => Some(Self::Spec),
            "versioned" => Some(Self::Versioned),
            "version-file" => Some(Self::VersionFile),
            _ => None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    #[error("spec index line {line}: {reason}")]
    Index { line: usize, reason: String },
    #[error("spec {name}: cannot decompress: {source}")]
    Decompress {
        name: String,
        #[source]
        source: std::io::Error,
    },
    #[error("spec {name}: cannot decode: {source}")]
    Decode {
        name: String,
        #[source]
        source: postcard::Error,
    },
    #[error("spec {name}: the index says {expected}, the entry is {found}")]
    KindMismatch {
        name: String,
        expected: &'static str,
        found: &'static str,
    },
}

#[derive(Debug)]
struct Row {
    name: Box<str>,
    kind: Kind,
    entry: Range<usize>,
    source: Option<Range<usize>>,
}

/// A spec archive: the frames plus their parsed, sorted index.
#[derive(Debug)]
pub struct Archive {
    data: Cow<'static, [u8]>,
    rows: Vec<Row>,
}

static EMBEDDED: LazyLock<Archive> =
    LazyLock::new(|| match Archive::from_parts(FIG_ZST, INDEX_TXT) {
        Ok(archive) => archive,
        Err(err) => {
            tracing::error!("embedded spec archive unusable: {err}");
            Archive {
                data: Cow::Borrowed(&[]),
                rows: Vec::new(),
            }
        }
    });

impl Archive {
    /// The archive built into fac. Its index is parsed on first use.
    pub fn embedded() -> &'static Archive {
        &EMBEDDED
    }

    /// An archive from frame bytes and index text. Every range must lie
    /// within `data`; names must be unique.
    pub fn from_parts(
        data: impl Into<Cow<'static, [u8]>>,
        index: &str,
    ) -> Result<Self, ArchiveError> {
        let data = data.into();
        let mut rows = Vec::new();
        for (number, line) in index.lines().enumerate() {
            let line_no = number + 1;
            let bad = |reason: &str| ArchiveError::Index {
                line: line_no,
                reason: reason.to_owned(),
            };
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let fields: Vec<&str> = line.split(' ').collect();
            let [name, kind, entry_offset, entry_len, js_offset, js_len] = fields[..] else {
                return Err(bad("expected 6 fields"));
            };
            let kind = Kind::parse(kind).ok_or_else(|| bad("unknown kind"))?;
            let in_data = |offset: &str, len: &str| -> Option<Range<usize>> {
                let start: usize = offset.parse().ok()?;
                let end = start.checked_add(len.parse().ok()?)?;
                (end <= data.len()).then_some(start..end)
            };
            let entry = in_data(entry_offset, entry_len)
                .ok_or_else(|| bad("bad or out-of-range entry frame"))?;
            let source = match (js_offset, js_len) {
                ("-", "-") => None,
                _ => Some(
                    in_data(js_offset, js_len)
                        .ok_or_else(|| bad("bad or out-of-range js frame"))?,
                ),
            };
            rows.push(Row {
                name: name.into(),
                kind,
                entry,
                source,
            });
        }
        rows.sort_by(|a, b| a.name.cmp(&b.name));
        if let Some(pair) = rows.windows(2).find(|pair| pair[0].name == pair[1].name) {
            return Err(ArchiveError::Index {
                line: 0,
                reason: format!("duplicate name {}", pair[0].name),
            });
        }
        Ok(Self { data, rows })
    }

    /// Every entry name, sorted.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.rows.iter().map(|row| &*row.name)
    }

    fn row(&self, name: &str) -> Option<&Row> {
        let at = self
            .rows
            .binary_search_by(|row| (*row.name).cmp(name))
            .ok()?;
        self.rows.get(at)
    }

    /// The entry's kind, from the index alone.
    pub fn kind(&self, name: &str) -> Option<Kind> {
        self.row(name).map(|row| row.kind)
    }

    /// Decode one entry. `None` when there is no entry of that name.
    pub fn entry(&self, name: &str) -> Option<Result<Entry, ArchiveError>> {
        let row = self.row(name)?;
        Some(self.decode(row))
    }

    fn decode(&self, row: &Row) -> Result<Entry, ArchiveError> {
        let bytes = self.frame(row, row.entry.clone())?;
        let entry: Entry = postcard::from_bytes(&bytes).map_err(|source| ArchiveError::Decode {
            name: row.name.to_string(),
            source,
        })?;
        let found = Kind::of(&entry);
        if found != row.kind {
            return Err(ArchiveError::KindMismatch {
                name: row.name.to_string(),
                expected: row.kind.as_str(),
                found: found.as_str(),
            });
        }
        Ok(entry)
    }

    /// The module source of an entry with functions. `None` when there is no
    /// entry of that name or it stores no source.
    pub fn source(&self, name: &str) -> Option<Result<Vec<u8>, ArchiveError>> {
        let row = self.row(name)?;
        let range = row.source.clone()?;
        Some(self.frame(row, range))
    }

    fn frame(&self, row: &Row, range: Range<usize>) -> Result<Vec<u8>, ArchiveError> {
        let frame = self.data.get(range).unwrap_or_default();
        zstd::decode_all(frame).map_err(|source| ArchiveError::Decompress {
            name: row.name.to_string(),
            source,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::specs::model::{Arg, CommandOption, Script, Subcommand, Template};

    fn entry(name: &str) -> Entry {
        match Archive::embedded().entry(name) {
            Some(Ok(entry)) => entry,
            Some(Err(err)) => panic!("{name}: {err}"),
            None => panic!("{name}: not in the archive"),
        }
    }

    fn root(name: &str) -> Subcommand {
        match entry(name) {
            Entry::Spec { root, .. } => root,
            other => panic!("{name}: not a plain spec: {:?}", Kind::of(&other)),
        }
    }

    fn named<'a>(subcommand: &'a Subcommand, name: &str) -> &'a Subcommand {
        subcommand
            .subcommands
            .iter()
            .find(|sub| sub.name.iter().any(|n| n == name))
            .unwrap_or_else(|| panic!("no subcommand {name}"))
    }

    fn option<'a>(subcommand: &'a Subcommand, name: &str) -> &'a CommandOption {
        subcommand
            .options
            .iter()
            .find(|opt| opt.name.iter().any(|n| n == name))
            .unwrap_or_else(|| panic!("no option {name}"))
    }

    fn any_template(subcommand: &Subcommand, template: Template) -> bool {
        let in_args = |args: &[Arg]| {
            args.iter().any(|arg| {
                arg.template.contains(&template)
                    || arg
                        .generators
                        .iter()
                        .any(|generator| generator.template.contains(&template))
            })
        };
        in_args(&subcommand.args)
            || subcommand.options.iter().any(|opt| in_args(&opt.args))
            || subcommand
                .subcommands
                .iter()
                .any(|sub| any_template(sub, template))
    }

    #[test]
    fn every_name_decodes() {
        let archive = Archive::embedded();
        let count = archive.names().count();
        assert!(count > 1400, "{count} entries");
        for name in archive.names() {
            let decoded = match archive.entry(name) {
                Some(Ok(entry)) => entry,
                Some(Err(err)) => panic!("{err}"),
                None => panic!("{name}: listed but not found"),
            };
            let source = archive.source(name);
            assert_eq!(
                decoded.has_functions(),
                source.is_some(),
                "{name}: source is stored exactly when the entry has functions"
            );
            if let Some(source) = source {
                let source = source.unwrap_or_else(|err| panic!("{err}"));
                assert!(!source.is_empty(), "{name}: empty source");
            }
        }
    }

    #[test]
    fn unknown_name_is_none() {
        assert!(Archive::embedded().entry("nonexistent").is_none());
        assert!(Archive::embedded().source("nonexistent").is_none());
        assert!(Archive::embedded().kind("nonexistent").is_none());
    }

    #[test]
    fn corrupt_frames_are_errors() {
        let data: Vec<u8> = vec![0x28, 0xb5, 0x2f, 0xfd, 1, 2, 3, 4, 5, 6, 7, 8];
        let index = "a spec 0 12 0 12\nb spec 4 8 - -\n";
        let archive = Archive::from_parts(data, index).expect("index parses");
        assert!(matches!(
            archive.entry("a"),
            Some(Err(ArchiveError::Decompress { .. }))
        ));
        assert!(matches!(
            archive.source("a"),
            Some(Err(ArchiveError::Decompress { .. }))
        ));
        assert!(matches!(archive.entry("b"), Some(Err(_))));

        // A valid frame holding bytes that are not an Entry.
        let frame = zstd::encode_all(&[0xff_u8, 0xff, 0xff][..], 3).expect("encode");
        let index = format!("c spec 0 {} - -\n", frame.len());
        let archive = Archive::from_parts(frame, &index).expect("index parses");
        assert!(matches!(
            archive.entry("c"),
            Some(Err(ArchiveError::Decode { .. }))
        ));

        // A valid entry whose kind disagrees with the index.
        let bytes = postcard::to_allocvec(&Entry::Versioned { fns: vec![] }).expect("encode");
        let frame = zstd::encode_all(&bytes[..], 3).expect("encode");
        let index = format!("d spec 0 {} - -\n", frame.len());
        let archive = Archive::from_parts(frame, &index).expect("index parses");
        assert!(matches!(
            archive.entry("d"),
            Some(Err(ArchiveError::KindMismatch { .. }))
        ));
    }

    #[test]
    fn bad_indexes_are_errors() {
        for index in [
            "a spec 0 1 - -\n",
            "a spec 0 1\n",
            "a weird 0 0 - -\n",
            "a spec x 0 - -\n",
            "a spec 0 0 0 99\n",
            "a spec 0 0 - -\na spec 0 0 - -\n",
            "a spec 18446744073709551615 1 - -\n",
        ] {
            let parsed = Archive::from_parts(Vec::new(), index);
            assert!(parsed.is_err(), "{index:?}");
        }
        let empty = Archive::from_parts(Vec::new(), "# only a comment\n\n").expect("parses");
        assert_eq!(empty.names().count(), 0);
    }

    #[test]
    fn index_is_sorted_with_its_header() {
        assert!(INDEX_TXT.starts_with(INDEX_HEADER));
        let names: Vec<&str> = Archive::embedded().names().collect();
        let listed: Vec<&str> = INDEX_TXT
            .lines()
            .filter(|line| !line.starts_with('#') && !line.is_empty())
            .filter_map(|line| line.split(' ').next())
            .collect();
        assert_eq!(names, listed, "index.txt is sorted by name");
    }

    /// Subcommands and options at every depth, the root excluded.
    fn tree_size(subcommand: &Subcommand) -> (usize, usize) {
        subcommand.subcommands.iter().fold(
            (subcommand.subcommands.len(), subcommand.options.len()),
            |(subs, opts), sub| {
                let (more_subs, more_opts) = tree_size(sub);
                (subs + more_subs, opts + more_opts)
            },
        )
    }

    #[test]
    fn git_has_its_subcommands_and_checkout_b() {
        let git = root("git");
        let (subcommands, options) = tree_size(&git);
        // node counts 93 and 771 for git in the same package.
        assert!(subcommands >= 90, "{subcommands}");
        assert!(options >= 700, "{options}");
        let checkout = named(&git, "checkout");
        assert!(!option(checkout, "-b").args.is_empty());
    }

    #[test]
    fn ls_has_a_filepaths_template() {
        assert!(any_template(&root("ls"), Template::Filepaths));
    }

    #[test]
    fn cd_generators_have_a_custom_function() {
        let Entry::Spec { root, fns } = entry("cd") else {
            panic!("cd is not a plain spec");
        };
        let customs: Vec<_> = root
            .args
            .iter()
            .flat_map(|arg| &arg.generators)
            .filter_map(|generator| generator.custom)
            .collect();
        assert!(!customs.is_empty());
        for custom in customs {
            let path = fns.get(custom.0 as usize).expect("FnRef listed in fns");
            assert!(path.ends_with(".custom"), "{path}");
        }
    }

    #[test]
    fn sudo_arg_is_a_command() {
        assert!(root("sudo").args.iter().any(|arg| arg.is_command));
    }

    #[test]
    fn clang_loads() {
        let clang = root("clang");
        assert!(clang.name.iter().any(|name| name == "clang"));
        assert!(!clang.options.is_empty());
    }

    #[test]
    fn versioned_specs() {
        let Entry::Versioned { fns } = entry("az") else {
            panic!("az is not versioned");
        };
        assert_eq!(fns, [""]);
        assert!(Archive::embedded().source("az").is_some());
        let Entry::VersionFile { versions, .. } = entry("heroku/8.0.0") else {
            panic!("heroku/8.0.0 is not a version file");
        };
        let versions: serde_json::Value = serde_json::from_str(&versions).expect("JSON");
        assert!(versions.as_object().is_some_and(|map| !map.is_empty()));
        assert!(versions.to_string().contains(r#""remove":true"#));
    }

    #[test]
    fn git_generators_keep_their_scripts() {
        assert_eq!(Archive::embedded().kind("git"), Some(Kind::Spec));
        let git = entry("git");
        assert!(git.has_functions());
        let has_argv = git
            .root()
            .into_iter()
            .flat_map(|root| &root.subcommands)
            .flat_map(|sub| &sub.args)
            .flat_map(|arg| &arg.generators)
            .any(|generator| matches!(generator.script, Some(Script::Argv(_))));
        assert!(has_argv);
    }
}
