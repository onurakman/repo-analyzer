use std::sync::LazyLock;

use aho_corasick::{AhoCorasick, AhoCorasickBuilder, MatchKind};

use super::super::data::{LANGUAGES, Language};

#[derive(Debug)]
pub struct LanguageMatchers {
    pub(crate) line_comments: Option<AhoCorasick>,
    pub(crate) block_comments: Option<BlockCommentMatchers>,
}

#[derive(Debug)]
pub struct BlockCommentMatchers {
    start_automaton: AhoCorasick,
    end_automaton: AhoCorasick,
}

impl BlockCommentMatchers {
    /// Locate the leftmost block-comment *start* on `line`. Returns
    /// `(position, length, pair_index)` where `pair_index` identifies which
    /// `(start, end)` pair from the language definition matched. Callers must
    /// remember this index so the matching end can be found later — see
    /// [`Self::find_block_end_or_nested_start`].
    #[inline]
    pub(crate) fn find_block_start(&self, line: &str) -> Option<(usize, usize, usize)> {
        self.start_automaton
            .find(line)
            .map(|m| (m.start(), m.len(), m.pattern().as_usize()))
    }

    /// Scan for the end of an already-open block comment (or, when `nested`,
    /// for a nested start that deepens it). `pair` is the start-pattern index
    /// recorded by [`Self::find_block_start`] when the comment was opened.
    ///
    /// Only end (and nested-start) tokens belonging to the *same* `pair` are
    /// accepted, so a stray delimiter from a different block-comment pair
    /// (e.g. `-->` inside a `/* … */` comment) cannot spuriously close it. For
    /// the common single-pair language this is a no-op — every token has index
    /// `0` — so behavior is unchanged.
    #[inline]
    pub(crate) fn find_block_end_or_nested_start(
        &self,
        line: &str,
        nested: bool,
        pair: usize,
    ) -> Option<(usize, usize, bool)> {
        if nested {
            let start_match = find_pair_match(&self.start_automaton, line, pair);
            let end_match = find_pair_match(&self.end_automaton, line, pair);
            match (start_match, end_match) {
                (Some(s), Some(e)) if s.0 < e.0 => Some((s.0, s.1, true)),
                (Some(s), None) => Some((s.0, s.1, true)),
                (_, Some(e)) => Some((e.0, e.1, false)),
                (None, None) => None,
            }
        } else {
            find_pair_match(&self.end_automaton, line, pair).map(|(pos, len)| (pos, len, false))
        }
    }
}

/// Leftmost non-overlapping match on `line` whose pattern index equals `pair`,
/// returned as `(position, length)`. Matches belonging to other pairs are
/// skipped so mismatched delimiters cannot close the wrong comment.
#[inline]
fn find_pair_match(automaton: &AhoCorasick, line: &str, pair: usize) -> Option<(usize, usize)> {
    automaton
        .find_iter(line)
        .find(|m| m.pattern().as_usize() == pair)
        .map(|m| (m.start(), m.len()))
}

static LANGUAGE_MATCHERS: LazyLock<Vec<LanguageMatchers>> =
    LazyLock::new(|| LANGUAGES.iter().map(build_language_matchers).collect());

#[inline]
pub fn language_matchers(lang: &Language) -> &'static LanguageMatchers {
    &LANGUAGE_MATCHERS[lang.index]
}

fn build_language_matchers(lang: &Language) -> LanguageMatchers {
    let line_comments = if lang.line_comments.is_empty() {
        None
    } else {
        Some(
            AhoCorasickBuilder::new()
                .match_kind(MatchKind::LeftmostFirst)
                .build(lang.line_comments)
                .expect("AhoCorasick should never fail to build with valid line comment patterns"),
        )
    };
    let block_comments = if lang.block_comments.is_empty() {
        None
    } else {
        let mut start_patterns = Vec::with_capacity(lang.block_comments.len());
        let mut end_patterns = Vec::with_capacity(lang.block_comments.len());
        for (start, end) in lang.block_comments {
            start_patterns.push(*start);
            end_patterns.push(*end);
        }
        let start_automaton = AhoCorasickBuilder::new()
            .match_kind(MatchKind::LeftmostFirst)
            .build(start_patterns)
            .expect(
                "AhoCorasick should never fail to build with valid block comment start patterns",
            );
        let end_automaton = AhoCorasickBuilder::new()
            .match_kind(MatchKind::LeftmostFirst)
            .build(end_patterns)
            .expect("AhoCorasick should never fail to build with valid block comment end patterns");
        Some(BlockCommentMatchers {
            start_automaton,
            end_automaton,
        })
    };
    LanguageMatchers {
        line_comments,
        block_comments,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_matchers(pairs: &[(&str, &str)]) -> BlockCommentMatchers {
        let starts: Vec<&str> = pairs.iter().map(|(s, _)| *s).collect();
        let ends: Vec<&str> = pairs.iter().map(|(_, e)| *e).collect();
        let start_automaton = AhoCorasickBuilder::new()
            .match_kind(MatchKind::LeftmostFirst)
            .build(starts)
            .unwrap();
        let end_automaton = AhoCorasickBuilder::new()
            .match_kind(MatchKind::LeftmostFirst)
            .build(ends)
            .unwrap();
        BlockCommentMatchers {
            start_automaton,
            end_automaton,
        }
    }

    #[test]
    fn find_block_start_reports_matching_pair_index() {
        let m = build_matchers(&[("/*", "*/"), ("<!--", "-->")]);
        assert_eq!(m.find_block_start("code /* x"), Some((5, 2, 0)));
        assert_eq!(m.find_block_start("code <!-- x"), Some((5, 4, 1)));
    }

    #[test]
    fn block_end_only_accepts_same_pair() {
        let m = build_matchers(&[("/*", "*/"), ("<!--", "-->")]);
        // A different pair's end token appears before the correct one; it must
        // be skipped so the `/* … */` comment (pair 0) is closed by `*/`.
        let line = "still comment --> not the end */ trailing";
        let star = line.find("*/").unwrap();
        let arrow = line.find("-->").unwrap();
        assert_eq!(
            m.find_block_end_or_nested_start(line, false, 0),
            Some((star, 2, false))
        );
        // For pair 1 the `-->` token is the correct terminator.
        assert_eq!(
            m.find_block_end_or_nested_start(line, false, 1),
            Some((arrow, 3, false))
        );
    }

    #[test]
    fn single_pair_behavior_preserved() {
        let m = build_matchers(&[("/*", "*/")]);
        assert_eq!(m.find_block_start("a /* b"), Some((2, 2, 0)));
        assert_eq!(
            m.find_block_end_or_nested_start("x */ y", false, 0),
            Some((2, 2, false))
        );
        assert_eq!(
            m.find_block_end_or_nested_start("no terminator here", false, 0),
            None
        );
    }

    #[test]
    fn nested_scan_stays_within_pair() {
        let m = build_matchers(&[("/*", "*/"), ("<!--", "-->")]);
        // A nested same-pair start deepens the comment.
        assert_eq!(
            m.find_block_end_or_nested_start("inner /* deeper", true, 0),
            Some((6, 2, true))
        );
        // A foreign pair's start is ignored; the same-pair end wins.
        let line = "text <!-- foreign */ end";
        let star = line.find("*/").unwrap();
        assert_eq!(
            m.find_block_end_or_nested_start(line, true, 0),
            Some((star, 2, false))
        );
    }
}
