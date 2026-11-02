//! List directory entries for a path word.
//!
//! `~user/...` forms are not supported: they list nothing. A bare `~` (no
//! slash) is an ordinary name fragment and lists the working directory.
//! Environment variables are not expanded.

use std::fs;
use std::path::{Path, PathBuf};

/// The most entries a listing returns.
pub const MAX_ENTRIES: usize = 2_000;

pub struct FileEntry {
    /// Final component.
    pub name: String,
    /// Follows symlinks.
    pub is_dir: bool,
    pub is_hidden: bool,
}

pub struct Listing {
    /// The typed (unquoted) part up to and including the last '/', e.g. "~/Dev/" or "".
    pub dir_prefix: String,
    pub entries: Vec<FileEntry>,
    /// The directory was read (it may be empty).
    pub read: bool,
}

/// List the directory for the unquoted word left of the cursor. `~` and `~/...` expand to `home`; relative paths resolve against `cwd`.
/// Hidden entries only when the part after the last '/' starts with '.'. Sorted by name, at most 2_000 entries. Unreadable dir → empty.
/// A name with a control character (a newline, a tab, ESC, a C1 control) is left out: it is never inserted into the line.
pub fn list(word_before_cursor: &str, cwd: &Path, home: &Path) -> Listing {
    list_with_cap(word_before_cursor, cwd, home, MAX_ENTRIES)
}

fn list_with_cap(word: &str, cwd: &Path, home: &Path, cap: usize) -> Listing {
    let (dir_prefix, partial) = match word.rfind('/') {
        Some(i) => word.split_at(i + 1),
        None => ("", word),
    };
    let mut listing = Listing {
        dir_prefix: dir_prefix.to_string(),
        entries: Vec::new(),
        read: false,
    };
    let Some(dir) = resolve_dir(dir_prefix, cwd, home) else {
        return listing;
    };
    let Ok(read_dir) = fs::read_dir(&dir) else {
        return listing;
    };
    listing.read = true;
    let show_hidden = partial.starts_with('.');
    let mut found: Vec<(String, fs::FileType)> = Vec::new();
    for entry in read_dir.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if !show_hidden && name.starts_with('.') {
            continue;
        }
        if name.chars().any(char::is_control) {
            continue;
        }
        if let Ok(file_type) = entry.file_type() {
            found.push((name, file_type));
        }
    }
    found.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    found.truncate(cap);
    listing.entries = found
        .into_iter()
        .map(|(name, file_type)| {
            let is_dir = if file_type.is_symlink() {
                fs::metadata(dir.join(&name)).is_ok_and(|m| m.is_dir())
            } else {
                file_type.is_dir()
            };
            FileEntry {
                is_hidden: name.starts_with('.'),
                name,
                is_dir,
            }
        })
        .collect();
    listing
}

/// The directory a typed prefix such as `~/Dev/` or `../x/` names.
fn resolve_dir(dir_prefix: &str, cwd: &Path, home: &Path) -> Option<PathBuf> {
    if dir_prefix.is_empty() {
        return Some(cwd.to_path_buf());
    }
    if let Some(rest) = dir_prefix.strip_prefix("~/") {
        return Some(home.join(rest));
    }
    if dir_prefix.starts_with('~') {
        return None;
    }
    Some(cwd.join(dir_prefix))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::complete::test_util::TempDir;
    use std::os::unix::fs::symlink;

    fn names(listing: &Listing) -> Vec<&str> {
        listing.entries.iter().map(|e| e.name.as_str()).collect()
    }

    struct Tree {
        root: TempDir,
    }

    impl Tree {
        fn new() -> Self {
            let root = TempDir::new("files");
            let p = root.path();
            fs::create_dir_all(p.join("home/Documents")).expect("mkdir");
            fs::create_dir_all(p.join("home/Dev/fac")).expect("mkdir");
            fs::create_dir_all(p.join("home/.config")).expect("mkdir");
            fs::write(p.join("home/.zshrc"), "").expect("write");
            fs::write(p.join("home/notes.txt"), "").expect("write");
            fs::create_dir_all(p.join("cwd/src")).expect("mkdir");
            fs::create_dir_all(p.join("cwd/My Docs")).expect("mkdir");
            fs::write(p.join("cwd/a.txt"), "").expect("write");
            fs::write(p.join("cwd/B.txt"), "").expect("write");
            fs::write(p.join("cwd/.hidden"), "").expect("write");
            fs::write(p.join("cwd/my file.txt"), "").expect("write");
            symlink(p.join("cwd/src"), p.join("cwd/link-to-dir")).expect("symlink");
            symlink(p.join("cwd/a.txt"), p.join("cwd/link-to-file")).expect("symlink");
            symlink(p.join("cwd/missing"), p.join("cwd/broken")).expect("symlink");
            Self { root }
        }

        fn cwd(&self) -> PathBuf {
            self.root.path().join("cwd")
        }

        fn home(&self) -> PathBuf {
            self.root.path().join("home")
        }

        fn list(&self, word: &str) -> Listing {
            list(word, &self.cwd(), &self.home())
        }
    }

    #[test]
    fn relative_listing_is_sorted_and_hides_dotfiles() {
        let t = Tree::new();
        let l = t.list("");
        assert_eq!(l.dir_prefix, "");
        assert_eq!(
            names(&l),
            [
                "B.txt",
                "My Docs",
                "a.txt",
                "broken",
                "link-to-dir",
                "link-to-file",
                "my file.txt",
                "src"
            ]
        );
        // The part after the last slash does not filter.
        assert_eq!(names(&t.list("zzz")), names(&l));
    }

    #[test]
    fn names_with_control_characters_are_left_out() {
        let root = TempDir::new("files-controls");
        let p = root.path();
        for name in [
            "important",
            "important\n",
            "tab\there",
            "cr\r",
            "esc\u{1b}[31m",
            "c1\u{85}",
            "del\u{7f}",
        ] {
            fs::write(p.join(name), "").expect("write");
        }
        fs::create_dir_all(p.join("dir\n")).expect("mkdir");
        let l = list("", p, p);
        assert_eq!(names(&l), ["important"]);
    }

    #[test]
    fn directories_follow_symlinks() {
        let t = Tree::new();
        let l = t.list("");
        let get = |n: &str| l.entries.iter().find(|e| e.name == n).expect(n);
        assert!(get("src").is_dir);
        assert!(get("My Docs").is_dir);
        assert!(!get("a.txt").is_dir);
        assert!(get("link-to-dir").is_dir);
        assert!(!get("link-to-file").is_dir);
        assert!(!get("broken").is_dir);
        assert!(!get("a.txt").is_hidden);
    }

    #[test]
    fn dot_prefix_shows_hidden_entries() {
        let t = Tree::new();
        let l = t.list(".");
        assert!(l.entries.iter().any(|e| e.name == ".hidden" && e.is_hidden));
        assert!(l.entries.iter().any(|e| e.name == "a.txt"));
        let l = t.list(".h");
        assert!(l.entries.iter().any(|e| e.name == ".hidden"));
        // Hidden entries of a subdirectory need the dot there too.
        assert!(!names(&t.list("~/")).contains(&".zshrc"));
        assert!(names(&t.list("~/.")).contains(&".zshrc"));
        assert!(names(&t.list("~/.")).contains(&".config"));
    }

    #[test]
    fn subdirectories_and_prefixes() {
        let t = Tree::new();
        let l = t.list("src/");
        assert_eq!(l.dir_prefix, "src/");
        assert!(l.entries.is_empty());
        let l = t.list("My Docs/x");
        assert_eq!(l.dir_prefix, "My Docs/");
        let l = t.list("../home/Do");
        assert_eq!(l.dir_prefix, "../home/");
        assert!(names(&l).contains(&"Documents"));
        let l = t.list("./");
        assert_eq!(l.dir_prefix, "./");
        assert!(names(&l).contains(&"a.txt"));
        let l = t.list("link-to-dir/");
        assert_eq!(l.dir_prefix, "link-to-dir/");
    }

    #[test]
    fn tilde_expands_to_home() {
        let t = Tree::new();
        let l = t.list("~/De");
        assert_eq!(l.dir_prefix, "~/");
        assert_eq!(names(&l), ["Dev", "Documents", "notes.txt"]);
        let l = t.list("~/Dev/");
        assert_eq!(l.dir_prefix, "~/Dev/");
        assert_eq!(names(&l), ["fac"]);
        assert!(l.entries[0].is_dir);
    }

    #[test]
    fn bare_tilde_and_tilde_user_are_not_listed_as_home() {
        let t = Tree::new();
        // A bare `~` is a name fragment: the working directory is listed.
        let l = t.list("~");
        assert_eq!(l.dir_prefix, "");
        assert!(names(&l).contains(&"a.txt"));
        // `~user/` is unsupported.
        let l = t.list("~root/");
        assert_eq!(l.dir_prefix, "~root/");
        assert!(l.entries.is_empty());
        let l = t.list("~root/etc/x");
        assert!(l.entries.is_empty());
    }

    #[test]
    fn absolute_paths() {
        let t = Tree::new();
        let abs = format!("{}/", t.home().display());
        let l = t.list(&abs);
        assert_eq!(l.dir_prefix, abs);
        assert_eq!(names(&l), ["Dev", "Documents", "notes.txt"]);
        // A cwd that does not exist matters not for absolute words.
        let l = list(
            &abs,
            Path::new("/nonexistent-fac-dir"),
            Path::new("/nonexistent"),
        );
        assert_eq!(names(&l), ["Dev", "Documents", "notes.txt"]);
    }

    #[test]
    fn missing_and_unreadable_directories_list_nothing() {
        let t = Tree::new();
        let l = t.list("no-such-dir/x");
        assert_eq!(l.dir_prefix, "no-such-dir/");
        assert!(l.entries.is_empty());
        // A file is not a directory.
        assert!(t.list("a.txt/").entries.is_empty());

        use std::os::unix::fs::PermissionsExt;
        let locked = t.cwd().join("locked");
        fs::create_dir(&locked).expect("mkdir");
        fs::write(locked.join("x"), "").expect("write");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).expect("chmod");
        let is_root = fs::read_dir(&locked).is_ok();
        let listed = t.list("locked/").entries.len();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).expect("chmod");
        if !is_root {
            assert_eq!(listed, 0);
        }
    }

    #[test]
    fn listing_is_capped_after_sorting() {
        let t = Tree::new();
        let l = list_with_cap("", &t.cwd(), &t.home(), 3);
        assert_eq!(names(&l), ["B.txt", "My Docs", "a.txt"]);
        let l = list_with_cap("", &t.cwd(), &t.home(), 0);
        assert!(l.entries.is_empty());

        let big = TempDir::new("files-big");
        for i in 0..MAX_ENTRIES + 5 {
            fs::write(big.path().join(format!("f{i:05}")), "").expect("write");
        }
        let l = list("", big.path(), big.path());
        assert_eq!(l.entries.len(), MAX_ENTRIES);
        assert_eq!(l.entries[0].name, "f00000");
    }
}
