//! What the user accepted from the popup, remembered across sessions: for
//! each command, the unix time (ms) each suggestion was last accepted. The
//! ranking lifts what was used last (Fig's recency index, `sorting.ts`).
//!
//! The file is read once at startup and written a second after the last
//! accept and on exit, on the spec thread ([`Usage::take_save`] gives it what
//! to write, [`write_file`] writes it); nothing here runs per keystroke.
//! Commands and names are never logged at info or above.

use std::collections::{BTreeMap, HashMap};
use std::ffi::{OsStr, OsString};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// The most names remembered per command.
const MAX_NAMES: usize = 500;
/// The most commands remembered.
const MAX_COMMANDS: usize = 300;
/// A save waits this long after the last accept.
const SAVE_DELAY: Duration = Duration::from_secs(1);
/// The file format's version.
const VERSION: u32 = 1;

/// `{"version":1,"commands":{"git":{"checkout":1792663021000}}}`.
#[derive(Serialize, Deserialize)]
struct UsageFile {
    version: u32,
    commands: BTreeMap<String, BTreeMap<String, u64>>,
}

/// command -> name -> last-used unix ms.
#[derive(Debug, Default)]
pub struct Usage {
    commands: HashMap<String, HashMap<String, u64>>,
    /// Where it is saved; `None` keeps it in memory only.
    path: Option<PathBuf>,
    /// An accept came that is not saved yet.
    dirty: bool,
    /// When the latest accept happened.
    stamp: Option<Instant>,
}

impl Usage {
    /// Read the file. A missing file gives an empty store silently; an
    /// unreadable or corrupt one gives an empty store and one warning that
    /// holds no file contents.
    pub fn load(path: Option<PathBuf>) -> Usage {
        let mut usage = Usage {
            path,
            ..Usage::default()
        };
        let Some(file) = usage.path.clone() else {
            return usage;
        };
        let text = match std::fs::read_to_string(&file) {
            Ok(text) => text,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return usage,
            Err(err) => {
                tracing::warn!("usage: cannot read the usage file: {}", err.kind());
                return usage;
            }
        };
        match serde_json::from_str::<UsageFile>(&text) {
            Ok(parsed) if parsed.version == VERSION => {
                usage.commands = parsed
                    .commands
                    .into_iter()
                    .map(|(command, names)| (command, names.into_iter().collect()))
                    .collect();
                usage.enforce_caps();
            }
            Ok(_) => tracing::warn!("usage: the usage file has an unknown version; ignored"),
            Err(err) => tracing::warn!(
                "usage: the usage file is corrupt ({:?}); starting empty",
                err.classify()
            ),
        }
        usage
    }

    /// Whether anything was accepted for `command`.
    pub fn has_command(&self, command: &str) -> bool {
        self.commands.contains_key(command)
    }

    /// When `name` was last accepted for `command`, in unix ms.
    pub fn last_used(&self, command: &str, name: &str) -> Option<u64> {
        self.commands.get(command)?.get(name).copied()
    }

    /// `name` was accepted for `command` at `now_ms`. Over a cap, the oldest
    /// is evicted (never the one just recorded).
    pub fn record(&mut self, command: &str, name: &str, now_ms: u64) {
        let names = self.commands.entry(command.to_owned()).or_default();
        names.insert(name.to_owned(), now_ms);
        evict_names(names, name);
        if self.commands.len() > MAX_COMMANDS {
            evict_command(&mut self.commands, command);
        }
        // Without a file there is nothing to save.
        if self.path.is_some() {
            self.dirty = true;
            self.stamp = Some(Instant::now());
        }
    }

    /// When a save is due: [`SAVE_DELAY`] after the latest accept, if there
    /// is anything to save.
    pub fn save_due(&self) -> Option<Instant> {
        if !self.dirty {
            return None;
        }
        self.stamp.map(|stamp| stamp + SAVE_DELAY)
    }

    /// What a save writes, for the spec thread to write
    /// ([`write_file`]): the path and the file's bytes. The store counts as
    /// saved from here on. `None` when nothing is due to be saved.
    pub fn take_save(&mut self) -> Option<(PathBuf, Vec<u8>)> {
        if !self.dirty {
            return None;
        }
        self.dirty = false;
        let path = self.path.clone()?;
        match self.json() {
            Ok(json) => Some((path, json)),
            Err(err) => {
                tracing::warn!("usage: saving failed: {}", err.kind());
                None
            }
        }
    }

    fn json(&self) -> io::Result<Vec<u8>> {
        let file = UsageFile {
            version: VERSION,
            commands: self
                .commands
                .iter()
                .map(|(command, names)| {
                    (
                        command.clone(),
                        names.iter().map(|(n, &ms)| (n.clone(), ms)).collect(),
                    )
                })
                .collect(),
        };
        serde_json::to_vec(&file).map_err(io::Error::other)
    }

    /// Take in a file's records: the later time wins.
    fn merge(&mut self, file: UsageFile) {
        for (command, names) in file.commands {
            let known = self.commands.entry(command).or_default();
            for (name, ms) in names {
                let at = known.entry(name).or_insert(ms);
                *at = (*at).max(ms);
            }
        }
    }

    fn enforce_caps(&mut self) {
        for names in self.commands.values_mut() {
            evict_names(names, "");
        }
        while self.commands.len() > MAX_COMMANDS {
            evict_command(&mut self.commands, "");
        }
    }
}

/// Write a store's file (`json`, as [`Usage::take_save`] gives it), merged
/// with what the file holds now: another session (a terminal tab) saves to
/// it too. For each command and name the later time wins, then the caps
/// apply. A missing, unreadable or corrupt file counts as empty. Written
/// atomically: `<path>.<pid>.tmp`, then a rename; creates the directory.
/// Saves in one process take turns.
pub fn write_file(path: &Path, json: &[u8]) -> io::Result<()> {
    static TURN: Mutex<()> = Mutex::new(());
    let _turn = TURN.lock().unwrap_or_else(PoisonError::into_inner);
    let ours: UsageFile = serde_json::from_slice(json).map_err(io::Error::other)?;
    let mut merged = Usage::default();
    merged.merge(ours);
    if let Ok(text) = std::fs::read(path)
        && let Ok(theirs) = serde_json::from_slice::<UsageFile>(&text)
        && theirs.version == VERSION
    {
        merged.merge(theirs);
    }
    merged.enforce_caps();
    let json = merged.json()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp: OsString = path.as_os_str().to_owned();
    tmp.push(format!(".{}.tmp", std::process::id()));
    let tmp = PathBuf::from(tmp);
    let written = std::fs::File::create(&tmp).and_then(|mut f| {
        f.write_all(&json)?;
        f.sync_all()
    });
    if let Err(err) = written.and_then(|()| std::fs::rename(&tmp, path)) {
        let _ = std::fs::remove_file(&tmp);
        return Err(err);
    }
    Ok(())
}

/// Drop the oldest names until `names` fits, keeping `keep`.
fn evict_names(names: &mut HashMap<String, u64>, keep: &str) {
    while names.len() > MAX_NAMES {
        let oldest = names
            .iter()
            .filter(|(name, _)| name.as_str() != keep)
            .min_by(|a, b| a.1.cmp(b.1).then_with(|| a.0.cmp(b.0)))
            .map(|(name, _)| name.clone());
        match oldest {
            Some(name) => names.remove(&name),
            None => break,
        };
    }
}

/// Drop the command whose newest accept is the oldest, keeping `keep`.
fn evict_command(commands: &mut HashMap<String, HashMap<String, u64>>, keep: &str) {
    let oldest = commands
        .iter()
        .filter(|(command, _)| command.as_str() != keep)
        .map(|(command, names)| (names.values().copied().max().unwrap_or(0), command))
        .min()
        .map(|(_, command)| command.clone());
    if let Some(command) = oldest {
        commands.remove(&command);
    }
}

/// The usage file: `$FAC_DATA_DIR/usage.json` when set and non-empty, else
/// `<home>/Library/Application Support/fac/usage.json`.
pub fn path(env_data_dir: Option<&OsStr>, home: Option<&Path>) -> Option<PathBuf> {
    match env_data_dir {
        Some(dir) if !dir.is_empty() => Some(Path::new(dir).join("usage.json")),
        _ => home.map(|home| home.join("Library/Application Support/fac/usage.json")),
    }
}

/// `path` with the real environment.
pub fn env_path() -> Option<PathBuf> {
    let env_data_dir = std::env::var_os("FAC_DATA_DIR");
    let home = std::env::var_os("HOME").map(PathBuf::from);
    path(env_data_dir.as_deref(), home.as_deref())
}

/// The current unix time in ms.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::complete::test_util::TempDir;

    fn store(dir: &TempDir) -> PathBuf {
        dir.path().join("data/usage.json")
    }

    /// Save as the spec thread does: what is due, merged into the file.
    fn save(usage: &mut Usage) -> io::Result<()> {
        match usage.take_save() {
            Some((path, json)) => write_file(&path, &json),
            None => Ok(()),
        }
    }

    fn read(path: &Path) -> UsageFile {
        serde_json::from_slice(&std::fs::read(path).expect("read")).expect("a usage file")
    }

    #[test]
    fn a_missing_file_loads_empty() {
        let dir = TempDir::new("usage-missing");
        let usage = Usage::load(Some(store(&dir)));
        assert_eq!(usage.last_used("git", "checkout"), None);
        assert!(usage.save_due().is_none());
        // No path at all: in memory only.
        let mut memory = Usage::load(None);
        memory.record("git", "add", 5);
        assert_eq!(memory.last_used("git", "add"), Some(5));
        assert!(memory.save_due().is_none());
        assert!(memory.take_save().is_none());
    }

    #[test]
    fn a_corrupt_file_loads_empty() {
        let dir = TempDir::new("usage-corrupt");
        let path = store(&dir);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        for text in [
            "not json at all",
            "{\"version\":1,\"commands\":{\"git\":{\"checkout\":\"yesterday\"}}}",
            "{\"version\":2,\"commands\":{\"git\":{\"checkout\":1}}}",
            "",
        ] {
            std::fs::write(&path, text).expect("write");
            let usage = Usage::load(Some(path.clone()));
            assert_eq!(usage.last_used("git", "checkout"), None, "{text}");
        }
    }

    #[test]
    fn record_and_save_round_trip() {
        let dir = TempDir::new("usage-roundtrip");
        let path = store(&dir);
        let mut usage = Usage::load(Some(path.clone()));
        usage.record("git", "checkout", 1_792_663_021_000);
        usage.record("", "ls", 7);
        assert!(usage.save_due().is_some());
        save(&mut usage).expect("save");
        assert!(usage.save_due().is_none());
        let text = std::fs::read_to_string(&path).expect("read");
        assert_eq!(
            text,
            "{\"version\":1,\"commands\":{\"\":{\"ls\":7},\"git\":{\"checkout\":1792663021000}}}"
        );
        let back = Usage::load(Some(path));
        assert_eq!(back.last_used("git", "checkout"), Some(1_792_663_021_000));
        assert_eq!(back.last_used("", "ls"), Some(7));
        assert_eq!(back.last_used("git", "ls"), None);
        assert_eq!(back.last_used("npm", "checkout"), None);
    }

    #[test]
    fn a_save_leaves_no_tmp_behind() {
        let dir = TempDir::new("usage-tmp");
        let path = store(&dir);
        let mut usage = Usage::load(Some(path.clone()));
        usage.record("git", "add", 1);
        save(&mut usage).expect("save");
        usage.record("git", "add", 2);
        save(&mut usage).expect("save again");
        let names: Vec<String> = std::fs::read_dir(path.parent().expect("parent"))
            .expect("read_dir")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(names, ["usage.json"]);
    }

    #[test]
    fn saves_from_two_sessions_keep_both() {
        let dir = TempDir::new("usage-merge");
        let path = store(&dir);
        let mut first = Usage::load(Some(path.clone()));
        let mut second = Usage::load(Some(path.clone()));
        first.record("git", "checkout", 10);
        first.record("git", "commit", 30);
        second.record("git", "commit", 20);
        second.record("npm", "install", 5);
        save(&mut first).expect("save");
        save(&mut second).expect("save");
        let back = Usage::load(Some(path.clone()));
        assert_eq!(back.last_used("git", "checkout"), Some(10));
        // The later time wins, whoever saves last.
        assert_eq!(back.last_used("git", "commit"), Some(30));
        assert_eq!(back.last_used("npm", "install"), Some(5));
        // The caps hold over what the two give together.
        let mut big = Usage::load(Some(path.clone()));
        for i in 0..MAX_NAMES {
            big.record("git", &format!("n{i:04}"), 1000 + i as u64);
        }
        save(&mut big).expect("save");
        let file = read(&path);
        assert_eq!(file.commands["git"].len(), MAX_NAMES);
        assert!(!file.commands["git"].contains_key("checkout"));
    }

    #[test]
    fn a_corrupt_file_is_replaced_on_save() {
        let dir = TempDir::new("usage-corrupt-save");
        let path = store(&dir);
        let mut usage = Usage::load(Some(path.clone()));
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, "not json").expect("write");
        usage.record("git", "add", 1);
        save(&mut usage).expect("save");
        assert_eq!(read(&path).commands["git"]["add"], 1);
    }

    #[test]
    fn concurrent_saves_never_leave_a_corrupt_file() {
        let dir = TempDir::new("usage-concurrent");
        let path = store(&dir);
        let saver = |command: &'static str| {
            let path = path.clone();
            std::thread::spawn(move || {
                let mut usage = Usage::load(Some(path.clone()));
                for i in 0..50u64 {
                    usage.record(command, &format!("n{i}"), i + 1);
                    save(&mut usage).expect("save");
                    // Whatever the other thread is doing, the file is whole.
                    read(&path);
                }
            })
        };
        let (a, b) = (saver("git"), saver("npm"));
        a.join().expect("thread");
        b.join().expect("thread");
        let file = read(&path);
        assert_eq!(file.commands["git"].len(), 50);
        assert_eq!(file.commands["npm"].len(), 50);
        let names: Vec<String> = std::fs::read_dir(path.parent().expect("parent"))
            .expect("read_dir")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(names, ["usage.json"]);
    }

    #[test]
    fn the_name_cap_evicts_the_oldest() {
        let mut usage = Usage::default();
        for i in 0..=MAX_NAMES {
            usage.record("git", &format!("n{i:04}"), 1000 + i as u64);
        }
        assert_eq!(usage.commands["git"].len(), MAX_NAMES);
        assert_eq!(usage.last_used("git", "n0000"), None);
        assert_eq!(usage.last_used("git", "n0001"), Some(1001));
        assert_eq!(
            usage.last_used("git", &format!("n{MAX_NAMES:04}")),
            Some(1000 + MAX_NAMES as u64)
        );
        // A name recorded with an older time than all the rest is kept.
        usage.record("git", "late", 1);
        assert_eq!(usage.last_used("git", "late"), Some(1));
        assert_eq!(usage.commands["git"].len(), MAX_NAMES);
    }

    #[test]
    fn the_command_cap_evicts_the_oldest() {
        let mut usage = Usage::default();
        for i in 0..=MAX_COMMANDS {
            usage.record(&format!("c{i:03}"), "x", 1000 + i as u64);
        }
        assert_eq!(usage.commands.len(), MAX_COMMANDS);
        assert_eq!(usage.last_used("c000", "x"), None);
        assert_eq!(usage.last_used("c001", "x"), Some(1001));
        // A command's age is its newest name.
        usage.record("c001", "y", 5000);
        usage.record("fresh", "x", 6000);
        assert_eq!(usage.last_used("c001", "x"), Some(1001));
        assert_eq!(usage.last_used("c002", "x"), None);
    }

    #[test]
    fn the_caps_apply_on_load_too() {
        let dir = TempDir::new("usage-caps");
        let path = store(&dir);
        let mut names = BTreeMap::new();
        for i in 0..MAX_NAMES + 10 {
            names.insert(format!("n{i:04}"), 1000 + i as u64);
        }
        let mut commands = BTreeMap::new();
        for i in 0..MAX_COMMANDS + 5 {
            commands.insert(
                format!("c{i:03}"),
                BTreeMap::from([("x".to_owned(), i as u64)]),
            );
        }
        commands.insert("big".to_owned(), names);
        let file = UsageFile {
            version: VERSION,
            commands,
        };
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, serde_json::to_vec(&file).expect("json")).expect("write");
        let usage = Usage::load(Some(path));
        assert!(usage.commands.len() <= MAX_COMMANDS);
        if let Some(big) = usage.commands.get("big") {
            assert!(big.len() <= MAX_NAMES);
            assert_eq!(big.get("n0000"), None);
        }
    }

    #[test]
    fn save_due_timing() {
        let dir = TempDir::new("usage-due");
        let mut usage = Usage::load(Some(store(&dir)));
        assert_eq!(usage.save_due(), None);
        let before = Instant::now();
        usage.record("git", "add", 1);
        let after = Instant::now();
        let due = usage.save_due().expect("due");
        assert!(due >= before + SAVE_DELAY && due <= after + SAVE_DELAY);
        // Another accept pushes it back.
        std::thread::sleep(Duration::from_millis(5));
        usage.record("git", "add", 2);
        assert!(usage.save_due().expect("due") > due);
        save(&mut usage).expect("save");
        assert_eq!(usage.save_due(), None);
    }

    #[test]
    fn path_resolution() {
        let home = Path::new("/home/me");
        assert_eq!(
            path(Some(OsStr::new("/tmp/data")), Some(home)),
            Some(PathBuf::from("/tmp/data/usage.json"))
        );
        let default = Some(PathBuf::from(
            "/home/me/Library/Application Support/fac/usage.json",
        ));
        assert_eq!(path(None, Some(home)), default);
        assert_eq!(path(Some(OsStr::new("")), Some(home)), default);
        assert_eq!(path(None, None), None);
        assert_eq!(
            path(Some(OsStr::new("/tmp/data")), None),
            Some(PathBuf::from("/tmp/data/usage.json"))
        );
    }
}
