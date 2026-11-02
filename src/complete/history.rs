//! Shell history: parse the history file (in its shell's format), keep the
//! newest unique entries, and follow the file as the shell appends to it.
//!
//! zsh's file is split into lines on the raw bytes and each line is unmetafied
//! on its own (equivalent to unmetafying the whole file, since a metafied
//! operand is never `\n` or `\`), so offsets into the file stay exact for
//! incremental reads.

use std::collections::{HashSet, VecDeque};
use std::fs;
use std::io::{self, Read, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::guards::ShellKind;

/// The most entries kept.
pub const MAX_ENTRIES: usize = 10_000;

/// How a shell writes its history file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryFormat {
    /// Metafied lines, `\`-continued, with optional `: <time>:<secs>;`
    /// headers.
    Zsh,
    /// Plain lines, with `#<epoch>` lines when HISTTIMEFORMAT is set.
    Bash,
    /// `- cmd:` / `when:` / `paths:` records.
    Fish,
}

impl From<ShellKind> for HistoryFormat {
    fn from(kind: ShellKind) -> Self {
        match kind {
            ShellKind::Zsh => HistoryFormat::Zsh,
            ShellKind::Bash => HistoryFormat::Bash,
            ShellKind::Fish => HistoryFormat::Fish,
        }
    }
}

/// What identifies a version of the history file. The inode is part of it
/// because zsh rewrites the file by renaming a new one over it.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Stamp {
    mtime: SystemTime,
    len: u64,
    ino: u64,
}

impl Stamp {
    fn of(meta: &fs::Metadata) -> io::Result<Self> {
        Ok(Self {
            mtime: meta.modified()?,
            len: meta.len(),
            ino: meta.ino(),
        })
    }
}

pub struct History {
    format: HistoryFormat,
    /// Newest last; unique; at most `MAX_ENTRIES`.
    entries: VecDeque<String>,
    path: Option<PathBuf>,
    stamp: Option<Stamp>,
    /// File offset just past the last entry that was read completely.
    committed: u64,
}

impl History {
    pub fn empty() -> Self {
        Self {
            format: HistoryFormat::Zsh,
            entries: VecDeque::new(),
            path: None,
            stamp: None,
            committed: 0,
        }
    }

    /// The history in `path`, written in `format`. A missing file gives an
    /// empty history (and `refresh` picks the file up when it appears).
    pub fn load(path: &Path, format: HistoryFormat) -> io::Result<Self> {
        let mut history = Self::empty();
        history.format = format;
        history.path = Some(path.to_path_buf());
        history.reload()?;
        Ok(history)
    }

    /// Re-read if (mtime, len) changed: if the file grew, parse only the new bytes (from the last complete entry);
    /// if it shrank or was replaced, reload fully. Returns whether anything changed.
    pub fn refresh(&mut self) -> io::Result<bool> {
        let Some(path) = self.path.clone() else {
            return Ok(false);
        };
        let stamp = match fs::metadata(&path) {
            Ok(meta) => Stamp::of(&meta)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let had_data = self.stamp.is_some() || !self.entries.is_empty();
                self.entries.clear();
                self.stamp = None;
                self.committed = 0;
                return Ok(had_data);
            }
            Err(e) => return Err(e),
        };
        let Some(old) = self.stamp else {
            return self.reload();
        };
        if old == stamp {
            return Ok(false);
        }
        let grew = stamp.ino == old.ino && stamp.len > old.len && stamp.len >= self.committed;
        if grew {
            self.read_tail(&path, stamp)
        } else {
            self.reload()
        }
    }

    /// From the shell's preexec report; moves an existing equal entry to newest.
    pub fn add(&mut self, command: &str) {
        if command.is_empty() {
            return;
        }
        self.push(command.to_string());
    }

    pub fn newest_first(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().rev().map(String::as_str)
    }

    fn push(&mut self, entry: String) {
        if let Some(i) = self.entries.iter().rposition(|e| *e == entry) {
            self.entries.remove(i);
        }
        self.entries.push_back(entry);
        while self.entries.len() > MAX_ENTRIES {
            self.entries.pop_front();
        }
    }

    /// Read the whole file again.
    fn reload(&mut self) -> io::Result<bool> {
        let path = match &self.path {
            Some(path) => path.clone(),
            None => return Ok(false),
        };
        let mut file = match fs::File::open(&path) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let had_data = !self.entries.is_empty();
                self.entries.clear();
                self.stamp = None;
                self.committed = 0;
                return Ok(had_data);
            }
            Err(e) => return Err(e),
        };
        let stamp = Stamp::of(&file.metadata()?)?;
        let mut bytes = Vec::with_capacity(usize::try_from(stamp.len).unwrap_or(0));
        file.read_to_end(&mut bytes)?;
        let (parsed, consumed) = parse_as(self.format, &bytes, true);
        self.entries = newest_unique(parsed);
        self.stamp = Some(stamp);
        // A final line without a newline is kept, but read again next time
        // in case the shell is still writing it.
        self.committed = parse_as(self.format, &bytes, false).1 as u64;
        debug_assert!(self.committed <= consumed as u64);
        Ok(true)
    }

    /// Parse only what was appended since the last complete entry.
    fn read_tail(&mut self, path: &Path, stamp: Stamp) -> io::Result<bool> {
        let mut file = fs::File::open(path)?;
        file.seek(SeekFrom::Start(self.committed))?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        let (parsed, consumed) = parse_as(self.format, &bytes, false);
        self.committed += consumed as u64;
        self.stamp = Some(stamp);
        let changed = !parsed.is_empty();
        for entry in parsed {
            self.push(entry);
        }
        Ok(changed)
    }
}

/// The newest `MAX_ENTRIES` unique entries of `parsed`, newest last.
fn newest_unique(parsed: Vec<String>) -> VecDeque<String> {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut keep = Vec::new();
    for (i, entry) in parsed.iter().enumerate().rev() {
        if keep.len() == MAX_ENTRIES {
            break;
        }
        if seen.insert(entry.as_str()) {
            keep.push(i);
        }
    }
    keep.iter().rev().map(|&i| parsed[i].clone()).collect()
}

/// All entries of a history file, in file order (duplicates kept).
#[cfg(test)]
fn parse(bytes: &[u8]) -> Vec<String> {
    parse_chunk(bytes, true).0
}

/// [`parse_chunk`] in `format`.
fn parse_as(format: HistoryFormat, bytes: &[u8], flush: bool) -> (Vec<String>, usize) {
    match format {
        HistoryFormat::Zsh => parse_chunk(bytes, flush),
        HistoryFormat::Bash => parse_bash(bytes, flush),
        HistoryFormat::Fish => parse_fish(bytes, flush),
    }
}

/// Parse fish entries from `bytes`, which start at a line boundary. An
/// entry is its `- cmd: ` line (fish 2.0's format, written by fish since);
/// the `  when:`, `  paths:` and `    - <path>` lines after it are skipped.
/// Returns the entries and how many bytes they cover. Unless `flush`, a
/// last line without its newline is left out and not counted.
fn parse_fish(bytes: &[u8], flush: bool) -> (Vec<String>, usize) {
    let mut entries = Vec::new();
    let mut consumed = 0;
    let mut pos = 0;
    while pos < bytes.len() {
        let (line_end, next) = match memchr::memchr(b'\n', &bytes[pos..]) {
            Some(i) => (pos + i, pos + i + 1),
            None if flush => (bytes.len(), bytes.len()),
            None => break,
        };
        if let Some(command) = bytes[pos..line_end].strip_prefix(b"- cmd: ") {
            push_entry(&mut entries, Some(unescape_fish(command)));
        }
        consumed = next;
        pos = next;
    }
    (entries, consumed)
}

/// fish's history escapes: `\\` is a backslash and `\n` a newline; any
/// other backslash stands for itself.
fn unescape_fish(command: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(command.len());
    let mut i = 0;
    while i < command.len() {
        let byte = command[i];
        let escaped = match (byte, command.get(i + 1)) {
            (b'\\', Some(b'\\')) => Some(b'\\'),
            (b'\\', Some(b'n')) => Some(b'\n'),
            _ => None,
        };
        match escaped {
            Some(byte) => {
                out.push(byte);
                i += 2;
            }
            None => {
                out.push(byte);
                i += 1;
            }
        }
    }
    out
}

/// Parse zsh entries from `bytes`, which start at an entry boundary.
/// Returns the entries and how many bytes they cover. Unless `flush`, an
/// entry that is not complete yet (last line missing its newline, or a
/// continuation still open) is left out and not counted.
fn parse_chunk(bytes: &[u8], flush: bool) -> (Vec<String>, usize) {
    let mut entries = Vec::new();
    let mut consumed = 0;
    let mut pos = 0;
    let mut current: Option<Vec<u8>> = None;
    while pos < bytes.len() {
        let (line_end, next, terminated) = match memchr::memchr(b'\n', &bytes[pos..]) {
            Some(i) => (pos + i, pos + i + 1, true),
            None => (bytes.len(), bytes.len(), false),
        };
        if !terminated && !flush {
            break;
        }
        let mut content = unmetafy(&bytes[pos..line_end]);
        if current.is_none() {
            strip_header(&mut content);
        }
        let continues = content.last() == Some(&b'\\');
        if continues {
            content.pop();
        }
        match current.as_mut() {
            Some(entry) => {
                entry.push(b'\n');
                entry.extend_from_slice(&content);
            }
            None => current = Some(content),
        }
        if !continues {
            push_entry(&mut entries, current.take());
            consumed = next;
        }
        pos = next;
    }
    if flush {
        push_entry(&mut entries, current.take());
        consumed = bytes.len();
    }
    (entries, consumed)
}

/// Parse bash entries from `bytes`, which start at an entry boundary, the
/// way bash reads its history file (`read_history_range` in readline's
/// histfile.c): a line that starts with `#` and a digit is a timestamp
/// (HISTTIMEFORMAT), never an entry. In a file that starts with one, the
/// lines up to the next timestamp are one entry (a multi-line command,
/// `lithist`); otherwise each line is an entry. Unless `flush`, a line
/// without its newline is left out.
fn parse_bash(bytes: &[u8], flush: bool) -> (Vec<String>, usize) {
    let is_stamp =
        |line: &[u8]| line.first() == Some(&b'#') && line.get(1).is_some_and(u8::is_ascii_digit);
    let first_line = memchr::memchr(b'\n', bytes).map_or(bytes, |end| &bytes[..end]);
    let stamped = is_stamp(first_line);
    let mut entries = Vec::new();
    let mut consumed = 0;
    let mut pos = 0;
    let mut current: Option<Vec<u8>> = None;
    while pos < bytes.len() {
        let (line_end, next, terminated) = match memchr::memchr(b'\n', &bytes[pos..]) {
            Some(i) => (pos + i, pos + i + 1, true),
            None => (bytes.len(), bytes.len(), false),
        };
        if !terminated && !flush {
            break;
        }
        let line = &bytes[pos..line_end];
        if is_stamp(line) {
            push_entry(&mut entries, current.take());
            // In a timestamped file the next entry starts at its timestamp.
            consumed = if stamped { pos } else { next };
        } else if stamped {
            match current.as_mut() {
                Some(entry) => {
                    entry.push(b'\n');
                    entry.extend_from_slice(line);
                }
                None => current = Some(line.to_vec()),
            }
        } else {
            push_entry(&mut entries, Some(line.to_vec()));
            consumed = next;
        }
        pos = next;
    }
    // bash writes an entry whole (`history -a`, or at exit): one that ends
    // with the data's newline is complete.
    if flush || (current.is_some() && bytes.ends_with(b"\n")) {
        push_entry(&mut entries, current.take());
        consumed = bytes.len();
    }
    (entries, consumed)
}

fn push_entry(entries: &mut Vec<String>, entry: Option<Vec<u8>>) {
    if let Some(bytes) = entry
        && !bytes.is_empty()
    {
        entries.push(String::from_utf8_lossy(&bytes).into_owned());
    }
}

/// zsh's history encoding: `0x83` marks that the next byte is XORed with `0x20`.
fn unmetafy(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut iter = bytes.iter().copied();
    while let Some(b) = iter.next() {
        if b == 0x83 {
            match iter.next() {
                Some(next) => out.push(next ^ 0x20),
                None => out.push(b),
            }
        } else {
            out.push(b);
        }
    }
    out
}

/// Remove an EXTENDED_HISTORY header, `: <digits>:<digits>;`.
fn strip_header(line: &mut Vec<u8>) {
    let Some(rest) = line.strip_prefix(b": ") else {
        return;
    };
    let digits = |s: &[u8]| s.iter().take_while(|b| b.is_ascii_digit()).count();
    let first = digits(rest);
    if first == 0 || rest.get(first) != Some(&b':') {
        return;
    }
    let rest = &rest[first + 1..];
    let second = digits(rest);
    if second == 0 || rest.get(second) != Some(&b';') {
        return;
    }
    let header_len = line.len() - rest.len() + second + 1;
    line.drain(..header_len);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::complete::test_util::TempDir;
    use std::io::Write;

    fn strings(history: &History) -> Vec<&str> {
        history.newest_first().collect()
    }

    #[test]
    fn extended_and_plain_lines() {
        let bytes = b": 1700000000:0;ls -la\nplain command\n: 1700000001:12;git status\n";
        assert_eq!(parse(bytes), ["ls -la", "plain command", "git status"]);
    }

    #[test]
    fn header_lookalikes_are_not_stripped() {
        let bytes = b": x:0;a\n: 12;b\n: 1:;c\n:1:2;d\n: 1:2:e\n";
        assert_eq!(
            parse(bytes),
            [": x:0;a", ": 12;b", ": 1:;c", ":1:2;d", ": 1:2:e"]
        );
    }

    #[test]
    fn multi_line_entries() {
        let bytes = b": 1:0;echo a\\\nb\\\nc\n: 2:0;next\n";
        assert_eq!(parse(bytes), ["echo a\nb\nc", "next"]);
    }

    #[test]
    fn a_double_backslash_keeps_one() {
        // Entry `curl x \` + newline + `  -X POST`, as zsh writes it.
        let bytes = b": 1:0;curl x \\\\\n  -X POST\n: 2:0;ls\n";
        assert_eq!(parse(bytes), ["curl x \\\n  -X POST", "ls"]);
    }

    #[test]
    fn a_header_inside_a_continuation_is_text() {
        let bytes = b": 1:0;echo \\\n: 2:0;not a header\n";
        assert_eq!(parse(bytes), ["echo \n: 2:0;not a header"]);
    }

    #[test]
    fn metafied_bytes_are_decoded() {
        // ✅ is e2 9c 85, stored as e2 83 bc 83 a5; √ is e2 88 9a, stored as e2 83 a8 83 ba.
        let bytes = b": 1:0;echo \xe2\x83\xbc\x83\xa5 \xe2\x83\xa8\x83\xba\n";
        assert_eq!(parse(bytes), ["echo ✅ √"]);
        // A metafied marker byte and NUL.
        let bytes = b"a\x83\xa3b\x83\x20c\n";
        assert_eq!(parse(bytes), ["a\u{fffd}b\0c"]);
    }

    #[test]
    fn invalid_utf8_is_decoded_lossily() {
        assert_eq!(parse(b"ok \xff\xfe end\n"), ["ok \u{fffd}\u{fffd} end"]);
        // A lone trailing marker byte survives.
        assert_eq!(parse(b"x\x83\n").len(), 1);
    }

    #[test]
    fn empty_entries_are_skipped_and_nothing_is_trimmed() {
        let bytes = b"\n: 1:0;\n  padded  \n\n: 2:0; \n";
        assert_eq!(parse(bytes), ["  padded  ", " "]);
    }

    #[test]
    fn a_last_line_without_newline_is_parsed() {
        assert_eq!(parse(b"a\nb"), ["a", "b"]);
        assert_eq!(parse(b""), Vec::<String>::new());
        // An unfinished continuation at the end of a static file is kept.
        assert_eq!(parse(b"a \\\nb \\"), ["a \nb "]);
    }

    #[test]
    fn partial_chunks_leave_the_unfinished_entry() {
        let (entries, used) = parse_chunk(b"a\nb\nc", false);
        assert_eq!(entries, ["a", "b"]);
        assert_eq!(used, 4);
        let (entries, used) = parse_chunk(b"a\nb \\\nc\n", false);
        assert_eq!(entries, ["a", "b \nc"]);
        assert_eq!(used, 8);
        let (entries, used) = parse_chunk(b"a\nb \\\n", false);
        assert_eq!(entries, ["a"]);
        assert_eq!(used, 2);
    }

    /// Plain lines are entries to zsh and bash, and none to fish (no
    /// `- cmd:` records); each shell gets its own format.
    #[test]
    fn each_shell_reads_its_own_format() {
        let dir = TempDir::new("history-formats");
        let path = dir.path().join("hist");
        write_file(&path, b"ls -la\ngit status\n");
        let entries = |format| {
            let history = History::load(&path, format).expect("load");
            history
                .newest_first()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        assert_eq!(entries(HistoryFormat::Zsh), ["git status", "ls -la"]);
        assert_eq!(entries(HistoryFormat::Bash), ["git status", "ls -la"]);
        assert!(entries(HistoryFormat::Fish).is_empty());
        assert_eq!(HistoryFormat::from(ShellKind::Bash), HistoryFormat::Bash);
        assert_eq!(HistoryFormat::from(ShellKind::Fish), HistoryFormat::Fish);
        assert_eq!(HistoryFormat::from(ShellKind::Zsh), HistoryFormat::Zsh);
    }

    /// A fish history file as fish 4.9.3 writes it.
    const FISH: &[u8] = b"- cmd: ls -la\n  when: 1700000000\n\
        - cmd: cat 'a b.txt'\n  when: 1700000100\n  paths:\n    - a b.txt\n\
        - cmd: echo one\\ntwo\n  when: 1700000200\n\
        - cmd: echo back\\\\slash \\\\n \\q\n  when: 1700000300\n\
        - cmd: ls -la\n  when: 1700000400\n";

    #[test]
    fn fish_records() {
        let (entries, used) = parse_fish(FISH, false);
        assert_eq!(
            entries,
            [
                "ls -la",
                "cat 'a b.txt'",
                "echo one\ntwo",
                "echo back\\slash \\n \\q",
                "ls -la"
            ]
        );
        assert_eq!(used, FISH.len());
    }

    #[test]
    fn fish_partial_tail() {
        // A `- cmd:` line not finished yet is left for the next read; the
        // lines of a record after its `- cmd:` are skipped, finished or not.
        let (entries, used) = parse_fish(b"- cmd: a\n  when: 1\n- cmd: b", false);
        assert_eq!(entries, ["a"]);
        assert_eq!(used, 19);
        let (entries, used) = parse_fish(b"- cmd: a\n  wh", false);
        assert_eq!(entries, ["a"]);
        assert_eq!(used, 9);
        let (entries, used) = parse_fish(b"- cmd: a\n- cmd: b", true);
        assert_eq!(entries, ["a", "b"]);
        assert_eq!(used, 17);
        // An empty command is no entry.
        assert!(parse_fish(b"- cmd: \n", false).0.is_empty());
    }

    #[test]
    fn fish_history_loads_and_follows_the_file() {
        let dir = TempDir::new("history-fish");
        let path = dir.path().join("fish_history");
        write_file(&path, FISH);
        let mut history = History::load(&path, HistoryFormat::Fish).expect("load");
        assert_eq!(
            strings(&history),
            [
                "ls -la",
                "echo back\\slash \\n \\q",
                "echo one\ntwo",
                "cat 'a b.txt'"
            ]
        );
        // fish appends a record: only the new bytes are read.
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("open");
        file.write_all(b"- cmd: git status\n  when: 1700000500\n")
            .expect("append");
        drop(file);
        assert!(history.refresh().expect("refresh"));
        assert_eq!(strings(&history)[0], "git status");
        // A line that is not fish's is skipped, as zsh's would not be.
        write_file(&path, b"plain line\n- cmd: only\n");
        assert!(history.refresh().expect("refresh"));
        assert_eq!(strings(&history), ["only"]);
    }

    fn write_file(path: &Path, bytes: &[u8]) {
        fs::write(path, bytes).expect("write");
    }

    fn append(path: &Path, bytes: &[u8]) {
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(path)
            .expect("open");
        file.write_all(bytes).expect("append");
    }

    /// Sleep past the file system's mtime granularity is not needed: the
    /// length changes with every write below, and replacements change the inode.
    #[test]
    fn load_dedupes_keeping_the_newest() {
        let dir = TempDir::new("history-dedupe");
        let path = dir.path().join("hist");
        write_file(&path, b"a\nb\na\nc\nb\n");
        let h = History::load(&path, HistoryFormat::Zsh).expect("load");
        assert_eq!(strings(&h), ["b", "c", "a"]);
        assert_eq!(h.entries.len(), 3);
    }

    #[test]
    fn missing_file_is_empty_and_picked_up_later() {
        let dir = TempDir::new("history-missing");
        let path = dir.path().join("hist");
        let mut h = History::load(&path, HistoryFormat::Zsh).expect("load");
        assert_eq!(h.entries.len(), 0);
        assert!(!h.refresh().expect("refresh"));
        write_file(&path, b": 1:0;ls\n");
        assert!(h.refresh().expect("refresh"));
        assert_eq!(strings(&h), ["ls"]);
    }

    #[test]
    fn empty_history_never_refreshes() {
        let mut h = History::empty();
        assert!(!h.refresh().expect("refresh"));
        assert_eq!(h.entries.len(), 0);
        h.add("ls");
        assert_eq!(strings(&h), ["ls"]);
    }

    #[test]
    fn add_moves_an_equal_entry_to_newest() {
        let mut h = History::empty();
        h.add("a");
        h.add("b");
        h.add("c");
        h.add("a");
        assert_eq!(strings(&h), ["a", "c", "b"]);
        h.add("");
        assert_eq!(h.entries.len(), 3);
    }

    #[test]
    fn the_cap_drops_the_oldest() {
        let mut h = History::empty();
        for i in 0..MAX_ENTRIES + 5 {
            h.add(&format!("cmd {i}"));
        }
        assert_eq!(h.entries.len(), MAX_ENTRIES);
        assert_eq!(h.newest_first().next(), Some("cmd 10004"));
        assert_eq!(h.newest_first().last(), Some("cmd 5"));

        let dir = TempDir::new("history-cap");
        let path = dir.path().join("hist");
        let mut bytes = Vec::new();
        for i in 0..MAX_ENTRIES + 500 {
            bytes.extend_from_slice(format!(": 1:0;cmd {i}\n").as_bytes());
        }
        write_file(&path, &bytes);
        let h = History::load(&path, HistoryFormat::Zsh).expect("load");
        assert_eq!(h.entries.len(), MAX_ENTRIES);
        assert_eq!(h.newest_first().next(), Some("cmd 10499"));
        assert_eq!(h.newest_first().last(), Some("cmd 500"));
    }

    #[test]
    fn refresh_reads_only_appended_entries() {
        let dir = TempDir::new("history-tail");
        let path = dir.path().join("hist");
        write_file(&path, b": 1:0;one\n: 2:0;two\n");
        let mut h = History::load(&path, HistoryFormat::Zsh).expect("load");
        assert!(!h.refresh().expect("refresh"));
        assert_eq!(h.committed, 20);

        append(&path, b": 3:0;three\n: 4:0;one\n");
        assert!(h.refresh().expect("refresh"));
        assert_eq!(strings(&h), ["one", "three", "two"]);
        assert!(!h.refresh().expect("refresh"));

        // Entries added from the shell are not lost by the next refresh.
        h.add("typed");
        append(&path, b"five\n");
        assert!(h.refresh().expect("refresh"));
        assert_eq!(strings(&h), ["five", "typed", "one", "three", "two"]);
    }

    #[test]
    fn refresh_keeps_a_partial_last_line_for_later() {
        let dir = TempDir::new("history-partial");
        let path = dir.path().join("hist");
        write_file(&path, b"first\n");
        let mut h = History::load(&path, HistoryFormat::Zsh).expect("load");

        append(&path, b"sec");
        assert!(!h.refresh().expect("refresh"));
        assert_eq!(strings(&h), ["first"]);

        append(&path, b"ond\nthi");
        assert!(h.refresh().expect("refresh"));
        assert_eq!(strings(&h), ["second", "first"]);

        append(&path, b"rd\n");
        assert!(h.refresh().expect("refresh"));
        assert_eq!(strings(&h), ["third", "second", "first"]);
    }

    #[test]
    fn refresh_waits_for_an_unfinished_continuation() {
        let dir = TempDir::new("history-continuation");
        let path = dir.path().join("hist");
        write_file(&path, b": 1:0;a\n");
        let mut h = History::load(&path, HistoryFormat::Zsh).expect("load");
        append(&path, b": 2:0;echo x \\\n");
        assert!(!h.refresh().expect("refresh"));
        append(&path, b"  y\n");
        assert!(h.refresh().expect("refresh"));
        assert_eq!(strings(&h), ["echo x \n  y", "a"]);
    }

    #[test]
    fn a_partial_last_line_at_load_is_kept_and_not_duplicated() {
        let dir = TempDir::new("history-load-partial");
        let path = dir.path().join("hist");
        write_file(&path, b"a\nb");
        let mut h = History::load(&path, HistoryFormat::Zsh).expect("load");
        assert_eq!(strings(&h), ["b", "a"]);
        append(&path, b"c\n");
        assert!(h.refresh().expect("refresh"));
        assert_eq!(strings(&h), ["bc", "b", "a"]);
    }

    #[test]
    fn truncation_reloads_fully() {
        let dir = TempDir::new("history-truncate");
        let path = dir.path().join("hist");
        write_file(&path, b"one\ntwo\nthree\n");
        let mut h = History::load(&path, HistoryFormat::Zsh).expect("load");
        assert_eq!(h.entries.len(), 3);

        write_file(&path, b"x\n");
        assert!(h.refresh().expect("refresh"));
        assert_eq!(strings(&h), ["x"]);
        assert!(!h.refresh().expect("refresh"));

        // Emptied.
        write_file(&path, b"");
        assert!(h.refresh().expect("refresh"));
        assert_eq!(h.entries.len(), 0);

        // Removed.
        write_file(&path, b"y\n");
        assert!(h.refresh().expect("refresh"));
        fs::remove_file(&path).expect("remove");
        assert!(h.refresh().expect("refresh"));
        assert_eq!(h.entries.len(), 0);
        assert!(!h.refresh().expect("refresh"));
    }

    #[test]
    fn a_replaced_larger_file_reloads_fully() {
        let dir = TempDir::new("history-replace");
        let path = dir.path().join("hist");
        write_file(&path, b"old1\nold2\n");
        let mut h = History::load(&path, HistoryFormat::Zsh).expect("load");

        // zsh writes a new file and renames it over the old one.
        let tmp = dir.path().join("hist.new");
        write_file(&tmp, b"new1\nnew2\nnew3\nnew4\n");
        fs::rename(&tmp, &path).expect("rename");
        assert!(h.refresh().expect("refresh"));
        assert_eq!(strings(&h), ["new4", "new3", "new2", "new1"]);
    }

    fn bash(bytes: &[u8]) -> Vec<String> {
        parse_bash(bytes, true).0
    }

    #[test]
    fn bash_plain_lines_are_entries() {
        assert_eq!(
            bash(b"ls -la\nmake test\n\necho \xc3\xa9 \\\n#comment\nlast"),
            ["ls -la", "make test", "echo é \\", "#comment", "last"]
        );
        // A timestamp-like line in a plain file is skipped, as bash does.
        assert_eq!(bash(b"ls\n#1700000000\npwd\n"), ["ls", "pwd"]);
    }

    #[test]
    fn bash_timestamped_entries_span_lines() {
        let file = b"#1700000000\nls -la\n#1700000100\nfor i in 1 2\ndo echo $i\ndone\n\
                     #1700000200\n#not a stamp\n";
        assert_eq!(
            bash(file),
            ["ls -la", "for i in 1 2\ndo echo $i\ndone", "#not a stamp"]
        );
        // Not flushed: every entry that ends with a newline is complete.
        let (entries, consumed) = parse_bash(file, false);
        assert_eq!(entries.len(), 3);
        assert_eq!(consumed, file.len());
        // A last line without its newline waits, and so does its entry.
        let cut = b"#1\nls\n#2\nmake te";
        let (entries, consumed) = parse_bash(cut, false);
        assert_eq!(entries, ["ls"]);
        assert_eq!(&cut[consumed..], b"#2\nmake te");
    }

    #[test]
    fn bash_refresh_reads_appended_entries() {
        let dir = TempDir::new("history-bash");
        let path = dir.path().join("hist");
        write_file(&path, b"#1\none\n#2\ntwo\n");
        let mut h = History::load(&path, HistoryFormat::Bash).expect("load");
        assert_eq!(strings(&h), ["two", "one"]);
        assert!(!h.refresh().expect("refresh"));

        append(&path, b"#3\nthree\n#4\non");
        assert!(h.refresh().expect("refresh"));
        assert_eq!(strings(&h), ["three", "two", "one"]);
        append(&path, b"e\n");
        assert!(h.refresh().expect("refresh"));
        assert_eq!(strings(&h), ["one", "three", "two"]);

        let plain = dir.path().join("plain");
        write_file(&plain, b"one\ntwo\n");
        let mut h = History::load(&plain, HistoryFormat::Bash).expect("load");
        append(&plain, b"three\n");
        assert!(h.refresh().expect("refresh"));
        assert_eq!(strings(&h), ["three", "two", "one"]);
    }
}
