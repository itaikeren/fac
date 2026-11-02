//! Fuzzy matching with `nucleo-matcher`.
//!
//! Needles are matched literally: spaces, `^`, `$`, `!` and `'` are ordinary
//! characters (an `Atom` of kind `Fuzzy` is built directly instead of parsing
//! a `Pattern`). Case is smart: a needle without capitals ignores case.

use nucleo_matcher::pattern::{Atom, AtomKind, CaseMatching, Normalization};
use nucleo_matcher::{Config, Matcher, Utf32Str};

/// A reused matcher plus scratch buffers.
pub struct FuzzyMatcher {
    matcher: Matcher,
    haystack_buf: Vec<char>,
    /// The last needle's atom: one needle is matched against every
    /// candidate of a list.
    atom: Option<(String, Atom)>,
}

impl Default for FuzzyMatcher {
    fn default() -> Self {
        Self::new()
    }
}

fn literal_atom(needle: &str) -> Atom {
    Atom::new(
        needle,
        CaseMatching::Smart,
        Normalization::Smart,
        AtomKind::Fuzzy,
        false,
    )
}

impl FuzzyMatcher {
    pub fn new() -> Self {
        Self {
            matcher: Matcher::new(Config::DEFAULT),
            haystack_buf: Vec::new(),
            atom: None,
        }
    }

    /// Literal fuzzy match (no pattern syntax: spaces, ^, $, !, ' are ordinary characters), smart case,
    /// filling `indices` with the matched char indices of `haystack`, sorted and deduplicated.
    pub fn score_with_indices(
        &mut self,
        needle: &str,
        haystack: &str,
        indices: &mut Vec<u32>,
    ) -> Option<u32> {
        indices.clear();
        let atom = match &mut self.atom {
            Some((last, atom)) if last == needle => atom,
            slot => &mut slot.insert((needle.to_owned(), literal_atom(needle))).1,
        };
        let hay = Utf32Str::new(haystack, &mut self.haystack_buf);
        let score = atom.indices(hay, &mut self.matcher, indices);
        if score.is_none() {
            indices.clear();
            return None;
        }
        indices.sort_unstable();
        indices.dedup();
        score.map(u32::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    impl FuzzyMatcher {
        /// `score_with_indices` without the indices.
        fn score(&mut self, needle: &str, haystack: &str) -> Option<u32> {
            self.score_with_indices(needle, haystack, &mut Vec::new())
        }
    }

    #[test]
    fn fuzzy_subsequence_matches() {
        let mut m = FuzzyMatcher::new();
        assert!(m.score("gco", "git-checkout").is_some());
        assert!(m.score("chk", "checkout").is_some());
        assert!(m.score("xyz", "checkout").is_none());
        assert!(m.score("tuokcehc", "checkout").is_none());
    }

    #[test]
    fn pattern_syntax_characters_are_literal() {
        let mut m = FuzzyMatcher::new();
        // `^` is not a prefix anchor.
        assert!(m.score("^foo", "foo").is_none());
        assert!(m.score("^foo", "a^foo").is_some());
        // `$` is not a postfix anchor.
        assert!(m.score("foo$", "foo").is_none());
        assert!(m.score("foo$", "x foo$").is_some());
        // `'` is not an exact-substring marker.
        assert!(m.score("'foo", "foo").is_none());
        assert!(m.score("it's", "it's").is_some());
        // `!` is not negation.
        assert!(m.score("!foo", "bar").is_none());
        assert!(m.score("!foo", "!foo").is_some());
        // A space is a character, not an atom separator.
        assert!(m.score("my doc", "my doc").is_some());
        assert!(m.score("my doc", "mydoc").is_none());
        assert!(m.score("a b", "a  b").is_some());
        // Backslash is not an escape.
        assert!(m.score("a\\ b", "a b").is_none());
        assert!(m.score("a\\ b", "a\\ b").is_some());
    }

    #[test]
    fn smart_case() {
        let mut m = FuzzyMatcher::new();
        assert!(m.score("readme", "README.md").is_some());
        assert!(m.score("README", "README.md").is_some());
        assert!(m.score("README", "readme.md").is_none());
        assert!(m.score("Read", "Readme.md").is_some());
        assert!(m.score("Read", "README.md").is_none());
        assert!(m.score("Read", "bread").is_none());
        assert!(m.score("é", "École").is_some());
    }

    #[test]
    fn indices_are_sorted_and_deduplicated() {
        let mut m = FuzzyMatcher::new();
        let mut idx = Vec::new();
        let score = m.score_with_indices("gco", "git-checkout", &mut idx);
        assert!(score.is_some());
        assert_eq!(idx.len(), 3);
        assert!(idx.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(idx[0], 0);
        assert_eq!(score, m.score("gco", "git-checkout"));

        // Exact prefix: contiguous.
        let score = m.score_with_indices("che", "checkout", &mut idx);
        assert!(score.is_some());
        assert_eq!(idx, [0, 1, 2]);

        // Char (not byte) indices.
        let score = m.score_with_indices("cf", "é-café", &mut idx);
        assert!(score.is_some());
        assert_eq!(idx, [2, 4]);

        // No match clears the vector.
        assert!(m.score_with_indices("zzz", "checkout", &mut idx).is_none());
        assert!(idx.is_empty());

        // Previous contents are dropped.
        idx.push(99);
        m.score_with_indices("c", "abc", &mut idx);
        assert_eq!(idx, [2]);
    }

    #[test]
    fn prefix_matches_score_higher() {
        let mut m = FuzzyMatcher::new();
        assert!(m.score("che", "checkout") > m.score("che", "xcheckout"));
        assert_eq!(m.score("che", "status"), None);
    }

    #[test]
    fn matcher_is_reusable_across_calls() {
        let mut m = FuzzyMatcher::new();
        for _ in 0..100 {
            assert!(m.score("ab", "xaxb").is_some());
            assert!(m.score("ab", "ba").is_none());
        }
    }

    /// `cargo test --release -- --ignored filter_10k --nocapture`
    #[test]
    #[ignore]
    fn filter_10k() {
        use std::time::Instant;
        let verbs = [
            "git", "cargo", "docker", "kubectl", "npm", "ssh", "cd", "ls",
        ];
        let subs = [
            "checkout",
            "build",
            "run",
            "get pods",
            "install",
            "commit -m",
        ];
        let items: Vec<String> = (0..10_000)
            .map(|i| {
                format!(
                    "{} {} --flag-{} ~/Development/project-{}/src/file{}.rs",
                    verbs[i % verbs.len()],
                    subs[i % subs.len()],
                    i % 97,
                    i % 53,
                    i
                )
            })
            .collect();
        // What the engine does per candidate: score with the indices.
        let mut m = FuzzyMatcher::new();
        let mut indices = Vec::new();
        let mut times = Vec::new();
        let mut hits = 0;
        for _ in 0..20 {
            let start = Instant::now();
            hits = items
                .iter()
                .filter(|item| m.score_with_indices("gco", item, &mut indices).is_some())
                .count();
            times.push(start.elapsed());
        }
        times.sort();
        let median = times[times.len() / 2];
        println!("filter 10k items, needle \"gco\": median {median:?} (hits {hits})");
        assert!(median.as_micros() < 1000, "median {median:?}");
    }
}
