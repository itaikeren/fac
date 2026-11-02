//! What a shell's own completions give, whichever shell answers: the
//! types shared by the completion workers and the engine.

use std::fmt;
use std::sync::Arc;

/// One match the shell's completions added (compsys's names; for bash
/// and fish, the fields they have no answer for stay empty).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// The match.
    pub word: String,
    /// What a listing shows for it (`-d`); the word when there is none.
    pub display: String,
    /// The part of the display after ` -- `, or empty.
    pub description: String,
    /// The first `-X` explanation, else the tag (`$curtag`).
    pub group: String,
    /// The `-S` suffix, when one was given (even empty).
    pub suffix: Option<String>,
    /// `-Q`: the word is inserted as it is, unquoted.
    pub verbatim: bool,
    /// `-f`: the word is a file name.
    pub is_file: bool,
    /// `$IPREFIX` and `$PREFIX` when it was added.
    pub iprefix: String,
    pub prefix: String,
    /// compadd's `-P`: a prefix inserted and shown.
    pub apre: String,
    /// compadd's `-p`: a hidden prefix, inserted but not shown (the
    /// directory part of a path match).
    pub hpre: String,
    /// compadd's `-s`: a hidden suffix.
    pub hsuf: String,
}

impl Candidate {
    /// The text compsys would insert in place of the typed `PREFIX`..`SUFFIX` span (unquoted),
    /// without the `-S` suffix: `apre + hpre + word + hsuf`.
    pub fn insertion(&self) -> String {
        [&self.apre, &self.hpre, &self.word, &self.hsuf]
            .into_iter()
            .map(String::as_str)
            .collect()
    }
}

/// What a query's results are cached under.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct QueryKey {
    pub cwd: String,
    /// LBUFFER cut at the context cut point.
    pub lbuffer: String,
}

impl fmt::Display for QueryKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?} in {:?}", self.lbuffer, self.cwd)
    }
}

#[derive(Debug, Clone)]
pub enum Lookup {
    Ready(Arc<Vec<Candidate>>),
    /// Queued or in flight; `take_completed` names the key when it is done.
    Pending,
    /// The query for this key timed out or failed. Cached like an answer
    /// (a timed-out key is asked again on a later lookup, up to the
    /// worker's `MAX_TRIES`; otherwise not until the cache is
    /// invalidated), but it says nothing about what completes.
    Failed,
    /// compsys completions are off (the worker failed too often).
    Unavailable,
}
