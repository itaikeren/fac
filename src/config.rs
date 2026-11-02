//! The user's settings, read once at startup from `~/.config/fac/config.toml`
//! (or the file named by `FAC_CONFIG`). A bad file or a bad key falls back to
//! the defaults with a warning; fac never refuses to start over its config.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, OnceLock};

use toml::{Table, Value};

/// Which icon set the popup rows use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icons {
    /// Coloured plain ASCII characters; needs no special font.
    Plain,
    /// Nerd Font glyphs.
    Nerd,
    /// No icons.
    None,
}

/// How suggestions with equal match quality are ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sort {
    /// Most recently used first.
    Recent,
    /// Alphabetical.
    Alphabetical,
}

/// Every setting fac reads from its config file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Name of the colour theme.
    pub theme: String,
    /// Icon set for the popup rows.
    pub icons: Icons,
    /// Whether fuzzy matching runs below the starts-with matches.
    pub fuzzy: bool,
    /// Order of equally good suggestions.
    pub sort: Sort,
    /// Whether history lines are suggested.
    pub history: bool,
    /// Key that opens the full description popout; empty means off.
    pub description_popout_key: String,
    /// How long a generator may run, in milliseconds (100..=60000).
    pub generator_timeout_ms: u64,
    /// Commands for which fac stays out of the way.
    pub disable_for_commands: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            theme: "default".to_owned(),
            icons: Icons::Plain,
            fuzzy: true,
            sort: Sort::Recent,
            history: true,
            description_popout_key: String::new(),
            generator_timeout_ms: 5000,
            disable_for_commands: Vec::new(),
        }
    }
}

/// Accepted range of `generator_timeout_ms`.
const TIMEOUT_RANGE: std::ops::RangeInclusive<i64> = 100..=60000;

/// The config file path: `FAC_CONFIG` when set and non-empty, else
/// `<home>/.config/fac/config.toml`.
pub fn path(env_config: Option<&OsStr>, home: Option<&Path>) -> Option<PathBuf> {
    match env_config {
        Some(path) if !path.is_empty() => Some(PathBuf::from(path)),
        _ => home.map(|home| home.join(".config/fac/config.toml")),
    }
}

/// `path` with the real environment.
pub fn env_path() -> Option<PathBuf> {
    let env_config = std::env::var_os("FAC_CONFIG");
    let home = std::env::var_os("HOME").map(PathBuf::from);
    path(env_config.as_deref(), home.as_deref())
}

/// Parse config text. Returns the settings and the warnings. A warning names
/// a key or an error, never a value from the file.
pub fn parse(text: &str) -> (Config, Vec<String>) {
    let mut config = Config::default();
    let mut warnings = Vec::new();
    let table: Table = match text.parse() {
        Ok(table) => table,
        Err(err) => {
            let line = err
                .span()
                .map(|span| text[..span.start.min(text.len())].matches('\n').count() + 1);
            warnings.push(match line {
                Some(line) => format!("invalid TOML at line {line}: {}", err.message()),
                None => format!("invalid TOML: {}", err.message()),
            });
            return (config, warnings);
        }
    };
    for (key, value) in &table {
        match key.as_str() {
            "theme" => match value.as_str() {
                Some(theme) => config.theme = theme.to_owned(),
                None => wrong_type(&mut warnings, key, "a string"),
            },
            "icons" => match value.as_str() {
                Some("plain") => config.icons = Icons::Plain,
                Some("nerd") => config.icons = Icons::Nerd,
                Some("none") => config.icons = Icons::None,
                Some(_) => bad_value(&mut warnings, key, "plain, nerd or none"),
                None => wrong_type(&mut warnings, key, "a string"),
            },
            "fuzzy" => match value.as_bool() {
                Some(fuzzy) => config.fuzzy = fuzzy,
                None => wrong_type(&mut warnings, key, "a boolean"),
            },
            "sort" => match value.as_str() {
                Some("recent") => config.sort = Sort::Recent,
                Some("alphabetical") => config.sort = Sort::Alphabetical,
                Some(_) => bad_value(&mut warnings, key, "recent or alphabetical"),
                None => wrong_type(&mut warnings, key, "a string"),
            },
            "history" => match value.as_bool() {
                Some(history) => config.history = history,
                None => wrong_type(&mut warnings, key, "a boolean"),
            },
            "description_popout_key" => match value.as_str() {
                Some(key_name) => config.description_popout_key = key_name.to_owned(),
                None => wrong_type(&mut warnings, key, "a string"),
            },
            "generator_timeout_ms" => match value.as_integer() {
                Some(ms) if TIMEOUT_RANGE.contains(&ms) => {
                    config.generator_timeout_ms = ms.unsigned_abs();
                }
                Some(_) => bad_value(&mut warnings, key, "a number from 100 to 60000"),
                None => wrong_type(&mut warnings, key, "an integer"),
            },
            "disable_for_commands" => match string_array(value) {
                Some(commands) => config.disable_for_commands = commands,
                None => wrong_type(&mut warnings, key, "an array of strings"),
            },
            _ => warnings.push(format!("unknown key `{key}` ignored")),
        }
    }
    (config, warnings)
}

fn string_array(value: &Value) -> Option<Vec<String>> {
    value
        .as_array()?
        .iter()
        .map(|item| item.as_str().map(str::to_owned))
        .collect()
}

fn wrong_type(warnings: &mut Vec<String>, key: &str, expected: &str) {
    warnings.push(format!("`{key}` must be {expected}; using the default"));
}

fn bad_value(warnings: &mut Vec<String>, key: &str, expected: &str) {
    warnings.push(format!("`{key}` must be {expected}; using the default"));
}

/// Read the config file at `path`, logging nothing: `None` when there is
/// no file; else the settings and the warnings (an unreadable file gives
/// the defaults and one).
pub fn read(path: &Path) -> Option<(Config, Vec<String>)> {
    match std::fs::read_to_string(path) {
        Ok(text) => Some(parse(&text)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => Some((
            Config::default(),
            vec![format!("cannot read the config file: {}", err.kind())],
        )),
    }
}

/// Read the config file. A missing file gives the defaults silently; an
/// unreadable or bad file gives the defaults plus warnings. Every warning is
/// logged.
pub fn load() -> Config {
    let Some(path) = env_path() else {
        tracing::warn!("config path unknown (HOME is not set); using the defaults");
        return Config::default();
    };
    let (config, warnings) = read(&path).unwrap_or_default();
    for warning in &warnings {
        tracing::warn!("config: {warning}");
    }
    tracing::info!(
        "config loaded path={} warnings={}",
        path.display(),
        warnings.len()
    );
    config
}

/// The byte `description_popout_key` sends: `ctrl+<letter>` (any case).
/// Empty: the popout is off. Anything else is an error (the popout stays
/// off), and so are Ctrl+I, Ctrl+J and Ctrl+M, which are Tab and Enter.
/// The error never repeats the value.
pub fn popout_key_byte(name: &str) -> Result<Option<u8>, String> {
    if name.is_empty() {
        return Ok(None);
    }
    let letter = name
        .to_ascii_lowercase()
        .strip_prefix("ctrl+")
        .and_then(|rest| match rest.as_bytes() {
            [letter] if letter.is_ascii_lowercase() => Some(*letter),
            _ => None,
        });
    match letter {
        Some(b'i' | b'j' | b'm') => Err(
            "description_popout_key: Ctrl+I, Ctrl+J and Ctrl+M are Tab and Enter; the description popout is off"
                .to_owned(),
        ),
        Some(letter) => Ok(Some(letter - b'a' + 1)),
        None => Err(
            "description_popout_key: only ctrl+<letter> is supported; the description popout is off"
                .to_owned(),
        ),
    }
}

static CONFIG: OnceLock<Config> = OnceLock::new();
static DEFAULT_CONFIG: LazyLock<Config> = LazyLock::new(Config::default);

/// Set the process-wide config. Only the first call has an effect.
pub fn init(config: Config) {
    let _ = CONFIG.set(config);
}

/// The process-wide config; the defaults until `init` runs.
pub fn global() -> &'static Config {
    CONFIG.get().unwrap_or(&DEFAULT_CONFIG)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one_warning(text: &str) -> (Config, String) {
        let (config, warnings) = parse(text);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        (config, warnings.into_iter().collect())
    }

    #[test]
    fn empty_text_gives_the_defaults() {
        let (config, warnings) = parse("");
        assert_eq!(config, Config::default());
        assert!(warnings.is_empty());
    }

    #[test]
    fn every_key_set() {
        let (config, warnings) = parse(
            r#"
theme = "mine"
icons = "nerd"
fuzzy = false
sort = "alphabetical"
history = false
description_popout_key = "ctrl+k"
generator_timeout_ms = 250
disable_for_commands = ["ssh", "vim"]
"#,
        );
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            config,
            Config {
                theme: "mine".to_owned(),
                icons: Icons::Nerd,
                fuzzy: false,
                sort: Sort::Alphabetical,
                history: false,
                description_popout_key: "ctrl+k".to_owned(),
                generator_timeout_ms: 250,
                disable_for_commands: vec!["ssh".to_owned(), "vim".to_owned()],
            }
        );
        assert_eq!(parse("icons = \"none\"").0.icons, Icons::None);
    }

    #[test]
    fn invalid_toml_gives_the_defaults_and_one_warning() {
        let (config, warning) = one_warning("theme = = \"x\"");
        assert_eq!(config, Config::default());
        assert!(warning.starts_with("invalid TOML"), "{warning}");
    }

    #[test]
    fn wrong_type_keeps_that_default() {
        for (line, key) in [
            ("theme = 1", "theme"),
            ("icons = 1", "icons"),
            ("fuzzy = \"yes\"", "fuzzy"),
            ("sort = 1", "sort"),
            ("history = 1", "history"),
            ("description_popout_key = 1", "description_popout_key"),
            ("generator_timeout_ms = \"5\"", "generator_timeout_ms"),
            ("disable_for_commands = \"ssh\"", "disable_for_commands"),
        ] {
            let (config, warning) = one_warning(line);
            assert_eq!(config, Config::default(), "{line}");
            assert!(warning.contains(key), "{line}: {warning}");
        }
    }

    #[test]
    fn unknown_key_is_ignored_with_a_warning() {
        let (config, warning) = one_warning("colour = \"red\"\nfuzzy = false");
        assert!(!config.fuzzy);
        assert!(warning.contains("colour"), "{warning}");
    }

    #[test]
    fn unknown_icons_value_keeps_the_default() {
        let (config, warning) = one_warning("icons = \"emoji\"");
        assert_eq!(config.icons, Icons::Plain);
        assert!(warning.contains("icons"), "{warning}");
        let (config, warning) = one_warning("sort = \"random\"");
        assert_eq!(config.sort, Sort::Recent);
        assert!(warning.contains("sort"), "{warning}");
    }

    #[test]
    fn generator_timeout_range_edges() {
        for (ms, ok) in [(99, false), (100, true), (60000, true), (60001, false)] {
            let (config, warnings) = parse(&format!("generator_timeout_ms = {ms}"));
            if ok {
                assert!(warnings.is_empty(), "{ms}: {warnings:?}");
                assert_eq!(config.generator_timeout_ms, ms as u64);
            } else {
                assert_eq!(warnings.len(), 1, "{ms}: {warnings:?}");
                assert!(warnings[0].contains("generator_timeout_ms"));
                assert_eq!(config.generator_timeout_ms, 5000);
            }
        }
    }

    #[test]
    fn mixed_disable_for_commands_is_a_wrong_type() {
        let (config, warning) = one_warning("disable_for_commands = [\"ssh\", 3]");
        assert!(config.disable_for_commands.is_empty());
        assert!(warning.contains("disable_for_commands"), "{warning}");
    }

    #[test]
    fn path_resolution() {
        let home = Path::new("/home/me");
        assert_eq!(
            path(Some(OsStr::new("/tmp/c.toml")), Some(home)),
            Some(PathBuf::from("/tmp/c.toml"))
        );
        let default = Some(PathBuf::from("/home/me/.config/fac/config.toml"));
        assert_eq!(path(None, Some(home)), default);
        assert_eq!(path(Some(OsStr::new("")), Some(home)), default);
        assert_eq!(path(None, None), None);
    }

    #[test]
    fn warnings_never_contain_file_values() {
        let texts = [
            "mystery = \"secret-value\"",
            "icons = \"secret-value\"",
            "sort = \"secret-value\"",
            "fuzzy = \"secret-value\"",
            "generator_timeout_ms = \"secret-value\"",
            "disable_for_commands = [\"secret-value\", 3]",
            "theme = \"secret-value\" \"secret-value\"",
            "generator_timeout_ms = 77777777",
        ];
        for text in texts {
            let (_, warnings) = parse(text);
            assert!(!warnings.is_empty(), "{text}");
            for warning in warnings {
                assert!(!warning.contains("secret-value"), "{text}: {warning}");
                assert!(!warning.contains("77777777"), "{text}: {warning}");
            }
        }
    }

    #[test]
    fn popout_keys() {
        assert_eq!(popout_key_byte(""), Ok(None));
        assert_eq!(popout_key_byte("ctrl+k"), Ok(Some(0x0b)));
        assert_eq!(popout_key_byte("Ctrl+K"), Ok(Some(0x0b)));
        assert_eq!(popout_key_byte("ctrl+a"), Ok(Some(0x01)));
        assert_eq!(popout_key_byte("ctrl+z"), Ok(Some(0x1a)));
        for bad in [
            "ctrl-k",
            "ctrl+",
            "ctrl+kk",
            "ctrl+1",
            "alt+k",
            "k",
            "ctrl+i",
            "ctrl+j",
            "ctrl+m",
            "secret-value",
        ] {
            let warning = popout_key_byte(bad).expect_err(bad);
            assert!(warning.contains("description_popout_key"), "{warning}");
            assert!(!warning.contains("secret-value"), "{warning}");
        }
    }

    #[test]
    fn global_is_the_default_before_init() {
        assert_eq!(global(), &Config::default());
    }
}
