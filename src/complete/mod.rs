//! Completion logic: understand the command line around the cursor, quote
//! inserted text, read history and directories, fuzzy-match, and ask the
//! shell's own completion system. Only `history` and `files` touch the file
//! system, and only `zsh_worker` (every shell's worker) runs a process.

pub mod engine;
pub mod files;
pub mod history;
pub mod matcher;
pub mod quoting;
pub mod selection;
pub mod source;
pub mod tokenizer;
pub mod usage;
pub mod zsh_worker;

#[cfg(test)]
pub(crate) mod test_util {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A scratch directory under the system temp dir, removed on drop.
    pub struct TempDir(PathBuf);

    impl TempDir {
        pub fn new(label: &str) -> Self {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("fac-test-{label}-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("create temp dir");
            Self(path)
        }

        pub fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}
