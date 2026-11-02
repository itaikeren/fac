//! Changing a user's startup file safely: through a symlink to the file it
//! names (the link stays), keeping its permissions, with a backup before
//! the first change, and through a temp file and a rename, so the file is
//! never half-written. Every change is prepared (read, computed, its temp
//! file written) before any file is replaced.

use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use nix::unistd::{AccessFlags, access};

/// The backup of `path`: `<path>.fac-backup`, next to the name the user
/// knows (a symlink's, not its target's).
pub fn backup_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".fac-backup");
    PathBuf::from(name)
}

/// A startup file as read: where it really is and what is in it.
#[derive(Debug, Clone)]
pub struct Current {
    /// The name the user knows (`~/.zshrc`).
    pub path: PathBuf,
    /// The file it names: itself, or the end of its symlinks.
    pub real: PathBuf,
    /// `None`: the file does not exist.
    pub bytes: Option<Vec<u8>>,
    /// Whether `path` is a symlink.
    pub link: bool,
}

/// Read `path` through its symlinks. A missing file is `bytes: None`; an
/// unreadable one, or a symlink to nothing, is an error.
pub fn read(path: &Path) -> Result<Current> {
    let link = std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink());
    let real = if link {
        std::fs::canonicalize(path).with_context(|| {
            format!(
                "{} is a symlink to a file that does not exist",
                path.display()
            )
        })?
    } else {
        path.to_path_buf()
    };
    let bytes = match std::fs::read(&real) {
        Ok(bytes) => Some(bytes),
        Err(err) if err.kind() == ErrorKind::NotFound => None,
        Err(err) => return Err(err).with_context(|| format!("cannot read {}", path.display())),
    };
    Ok(Current {
        path: path.to_path_buf(),
        real,
        bytes,
        link,
    })
}

/// What becomes of a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum After {
    Write(Vec<u8>),
    /// Removed (a file fac created, with nothing else left in it).
    Delete,
}

/// One planned change.
#[derive(Debug, Clone)]
pub struct Change {
    pub current: Current,
    pub after: After,
    /// What to back the file up as (to [`backup_path`]) before the change,
    /// unless a backup is there already: the file without fac's block, so
    /// a restored backup never brings the block's exec back. `None`: no
    /// backup (a file fac created).
    pub backup: Option<Vec<u8>>,
}

/// What happened to one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Done {
    pub path: PathBuf,
    pub created: bool,
    pub deleted: bool,
    /// The backup written now.
    pub backup: Option<PathBuf>,
}

/// In unit tests, never touch the real home (the user's passwd home).
#[cfg(test)]
fn guard(path: &Path) {
    if let Some(home) = crate::init::passwd_home() {
        assert!(
            !path.starts_with(&home),
            "a test was about to write {} in the real home",
            path.display()
        );
    }
}

#[cfg(not(test))]
fn guard(_: &Path) {}

/// A temp file next to `target`, written whole, with `mode`; removed on
/// drop unless it was put in place.
struct Temp {
    /// `None` once committed.
    path: Option<PathBuf>,
    target: PathBuf,
}

impl Temp {
    fn write(target: &Path, bytes: &[u8], mode: Option<u32>) -> Result<Temp> {
        guard(target);
        let dir = target
            .parent()
            .ok_or_else(|| anyhow!("{} has no folder", target.display()))?;
        let name = target
            .file_name()
            .ok_or_else(|| anyhow!("{} has no file name", target.display()))?;
        let mut temp_name = std::ffi::OsString::from(".");
        temp_name.push(name);
        temp_name.push(format!(".fac-tmp-{}", std::process::id()));
        let path = dir.join(temp_name);
        let write = || -> std::io::Result<()> {
            let mut file: File = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(mode.unwrap_or(0o644))
                .open(&path)?;
            file.write_all(bytes)?;
            if let Some(mode) = mode {
                // Exactly the old mode, whatever the umask.
                file.set_permissions(std::fs::Permissions::from_mode(mode))?;
            }
            file.sync_all()
        };
        let _ = std::fs::remove_file(&path);
        if let Err(err) = write() {
            let _ = std::fs::remove_file(&path);
            return Err(err).with_context(|| format!("cannot write in {}", dir.display()));
        }
        Ok(Temp {
            path: Some(path),
            target: target.to_path_buf(),
        })
    }

    fn commit(mut self) -> Result<()> {
        let Some(path) = self.path.take() else {
            return Ok(());
        };
        if let Err(err) = std::fs::rename(&path, &self.target) {
            let _ = std::fs::remove_file(&path);
            return Err(err).with_context(|| format!("cannot replace {}", self.target.display()));
        }
        Ok(())
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        if let Some(path) = &self.path {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Make `changes`: first every temp file (and backup) is written and every
/// file checked writable, so an error leaves every file as it was; then
/// each is put in place.
pub fn apply(changes: &[Change]) -> Result<Vec<Done>> {
    struct Ready {
        temp: Option<Temp>,
        backup: Option<Temp>,
        delete: Option<PathBuf>,
        done: Done,
    }
    let mut ready = Vec::new();
    for change in changes {
        let current = &change.current;
        let shown = current.path.display();
        let mode = match std::fs::metadata(&current.real) {
            Ok(meta) => Some(meta.permissions().mode() & 0o7777),
            Err(err) if err.kind() == ErrorKind::NotFound => None,
            Err(err) => return Err(err).with_context(|| format!("cannot read {shown}")),
        };
        if current.bytes.is_some() && access(&current.real, AccessFlags::W_OK).is_err() {
            return Err(anyhow!("cannot write {shown}: it is read-only for you"));
        }
        let backup_to = backup_path(&current.path);
        let backup = match (&current.bytes, &change.backup) {
            (Some(_), Some(bytes)) if backup_to.symlink_metadata().is_err() => {
                Some(Temp::write(&backup_to, bytes, mode)?)
            }
            _ => None,
        };
        let (temp, delete) = match &change.after {
            After::Write(bytes) => {
                if let (None, Some(dir)) = (&current.bytes, current.real.parent()) {
                    // A new file's folder (fish's conf.d) may be missing.
                    guard(dir);
                    std::fs::create_dir_all(dir)
                        .with_context(|| format!("cannot create {}", dir.display()))?;
                }
                (Some(Temp::write(&current.real, bytes, mode)?), None)
            }
            After::Delete => {
                guard(&current.real);
                (None, Some(current.real.clone()))
            }
        };
        ready.push(Ready {
            temp,
            backup,
            delete,
            done: Done {
                path: current.path.clone(),
                created: current.bytes.is_none(),
                deleted: matches!(change.after, After::Delete),
                backup: None,
            },
        });
    }
    let mut done = Vec::new();
    for mut item in ready {
        if let Some(backup) = item.backup.take() {
            let to = backup.target.clone();
            backup.commit()?;
            item.done.backup = Some(to);
        }
        if let Some(temp) = item.temp.take() {
            temp.commit()?;
        }
        if let Some(path) = item.delete.take() {
            std::fs::remove_file(&path)
                .with_context(|| format!("cannot remove {}", item.done.path.display()))?;
        }
        done.push(item.done);
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    struct Dir(PathBuf);

    impl Dir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "fac-init-files-{name}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("mkdir");
            Self(dir)
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write_change(path: &Path, bytes: &[u8]) -> Done {
        let current = read(path).expect("read");
        let current_bytes = current.bytes.clone();
        apply(&[Change {
            current,
            after: After::Write(bytes.to_vec()),
            backup: current_bytes,
        }])
        .expect("apply")
        .remove(0)
    }

    #[test]
    fn a_symlink_stays_a_symlink() {
        let dir = Dir::new("link");
        let target = dir.0.join("dotfiles-zshrc");
        std::fs::write(&target, "export A=1\n").expect("write");
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).expect("chmod");
        let link = dir.0.join(".zshrc");
        symlink(&target, &link).expect("symlink");
        let done = write_change(&link, b"new\nexport A=1\n");
        assert!(
            std::fs::symlink_metadata(&link)
                .expect("link")
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            std::fs::read(&target).expect("target"),
            b"new\nexport A=1\n"
        );
        let mode = std::fs::metadata(&target)
            .expect("meta")
            .permissions()
            .mode()
            & 0o7777;
        assert_eq!(mode, 0o600);
        // The backup sits next to the link, with the old contents.
        assert_eq!(done.backup.as_deref(), Some(backup_path(&link).as_path()));
        assert_eq!(
            std::fs::read(backup_path(&link)).expect("backup"),
            b"export A=1\n"
        );
        // No temp file is left behind.
        let names: Vec<_> = std::fs::read_dir(&dir.0)
            .expect("ls")
            .map(|entry| entry.expect("entry").file_name())
            .collect();
        assert_eq!(names.len(), 3, "{names:?}");
    }

    #[test]
    fn a_backup_is_made_once() {
        let dir = Dir::new("backup");
        let file = dir.0.join(".bashrc");
        std::fs::write(&file, "one\n").expect("write");
        let first = write_change(&file, b"two\n");
        assert!(first.backup.is_some());
        let second = write_change(&file, b"three\n");
        assert_eq!(second.backup, None);
        assert_eq!(std::fs::read(backup_path(&file)).expect("backup"), b"one\n");
        // A missing file is created, with no backup.
        let new = dir.0.join(".profile");
        let done = write_change(&new, b"block\n");
        assert!(done.created && done.backup.is_none());
        assert!(!backup_path(&new).exists());
    }

    #[test]
    fn a_read_only_file_is_an_error_and_nothing_is_written() {
        let dir = Dir::new("readonly");
        let ok = dir.0.join(".bash_profile");
        let locked = dir.0.join(".bashrc");
        std::fs::write(&ok, "ok\n").expect("write");
        std::fs::write(&locked, "locked\n").expect("write");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o444)).expect("chmod");
        let changes = [
            Change {
                current: read(&ok).expect("read"),
                after: After::Write(b"changed\n".to_vec()),
                backup: Some(b"ok\n".to_vec()),
            },
            Change {
                current: read(&locked).expect("read"),
                after: After::Write(b"changed\n".to_vec()),
                backup: Some(b"locked\n".to_vec()),
            },
        ];
        let err = apply(&changes).expect_err("read-only");
        assert!(err.to_string().contains("read-only"), "{err:#}");
        assert_eq!(std::fs::read(&ok).expect("ok"), b"ok\n");
        assert!(!backup_path(&ok).exists());
        let names: Vec<_> = std::fs::read_dir(&dir.0).expect("ls").collect();
        assert_eq!(names.len(), 2);
    }

    #[test]
    fn a_dangling_symlink_is_an_error() {
        let dir = Dir::new("dangling");
        let link = dir.0.join(".zshrc");
        symlink(dir.0.join("missing"), &link).expect("symlink");
        assert!(read(&link).is_err());
    }

    #[test]
    fn delete_removes_the_file() {
        let dir = Dir::new("delete");
        let file = dir.0.join("fac.fish");
        std::fs::write(&file, "x\n").expect("write");
        let done = apply(&[Change {
            current: read(&file).expect("read"),
            after: After::Delete,
            backup: None,
        }])
        .expect("apply");
        assert!(done[0].deleted);
        assert!(!file.exists());
    }
}
