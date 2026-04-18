pub(super) mod patterns;
pub mod scoring;

use std::borrow::Cow;

use self::patterns::get_candidates;
use super::data::{LANGUAGES, Language};

const COMMENT_MATCH_SCORE: i32 = 50;
const KEYWORD_MATCH_SCORE: i32 = 10;

#[inline]
fn score_language(lang: &Language, content: &str, tokens: &[&str]) -> i32 {
    if lang.line_comments.is_empty() && lang.block_comments.is_empty() && lang.keywords.is_empty() {
        return 0;
    }
    let mut score: i32 = 0;
    for comment in lang.line_comments {
        if content.contains(comment) {
            score = score.saturating_add(COMMENT_MATCH_SCORE);
        }
    }
    for comment_pair in lang.block_comments {
        if content.contains(comment_pair.0) && content.contains(comment_pair.1) {
            score = score.saturating_add(COMMENT_MATCH_SCORE);
        }
    }
    let mut matched_chars: usize = 0;
    for keyword in lang.keywords {
        let count = if keyword
            .chars()
            .any(|c| !c.is_ascii_alphanumeric() && c != '_')
        {
            let occurrences = content.matches(keyword).count();
            matched_chars = matched_chars.saturating_add(occurrences.saturating_mul(keyword.len()));
            occurrences
        } else {
            tokens
                .iter()
                .filter(|token| token.eq_ignore_ascii_case(keyword))
                .count()
        };
        let clamped_count =
            count.min(usize::try_from(i32::MAX / KEYWORD_MATCH_SCORE).unwrap_or(usize::MAX));
        let count_i32 = clamped_count as i32;
        score = score.saturating_add(count_i32.saturating_mul(KEYWORD_MATCH_SCORE));
    }
    if is_symbol_only_language(lang) && !tokens.is_empty() {
        let non_whitespace = content.chars().filter(|c| !c.is_whitespace()).count();
        if non_whitespace > 0 {
            let matched_chars_u128 = matched_chars as u128;
            let non_whitespace_u128 = non_whitespace as u128;
            if matched_chars_u128.saturating_mul(2) < non_whitespace_u128 {
                return 0;
            }
        }
    }
    score
}

fn is_symbol_only_language(lang: &Language) -> bool {
    !lang.keywords.is_empty()
        && lang
            .keywords
            .iter()
            .all(|kw| kw.chars().all(|c| !c.is_ascii_alphanumeric() && c != '_'))
        && lang.line_comments.is_empty()
        && lang.block_comments.is_empty()
}

#[inline]
fn disambiguate<'a>(candidates: &[&'a Language], content: &str) -> Option<&'a Language> {
    let tokens: Vec<_> = tokenize(content).collect();
    candidates
        .iter()
        .map(|lang| (*lang, score_language(lang, content, &tokens)))
        .max_by_key(|(_, score)| *score)
        .filter(|(_, score)| *score > 0)
        .map(|(lang, _)| lang)
}

#[inline]
fn tokenize(content: &str) -> impl Iterator<Item = &str> {
    content
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .filter(|token| !token.is_empty())
}

#[inline]
fn normalize_shebang(line: &str) -> Cow<'_, str> {
    line.strip_prefix("#! ")
        .map_or(Cow::Borrowed(line), |rest| Cow::Owned(format!("#!{rest}")))
}

#[inline]
fn detect_from_shebang(content: &str) -> Option<&'static Language> {
    let first_line = content.lines().next()?;
    let trimmed = first_line.trim();
    if !trimmed.starts_with("#!") {
        return None;
    }
    let normalized = normalize_shebang(trimmed);
    LANGUAGES.iter().find(|lang| {
        !lang.shebangs.is_empty()
            && lang
                .shebangs
                .iter()
                .any(|shebang| normalized.starts_with(shebang))
    })
}

/// Detect the language for a given filename. If content is provided, it will
/// be used for shebang sniffing and to disambiguate when multiple languages
/// share the filename pattern (e.g. `*.m` = Objective-C or MATLAB).
#[must_use]
pub fn detect_language_info(filename: &str, content: Option<&str>) -> Option<&'static Language> {
    let candidates = get_candidates(filename);
    match candidates.len() {
        0 => content.and_then(detect_from_shebang),
        1 => Some(candidates[0]),
        _ => content.and_then(|file_content| {
            detect_from_shebang(file_content).or_else(|| disambiguate(&candidates, file_content))
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_extension_returns_language() {
        let rust = detect_language_info("src/main.rs", None).expect("rust should be detected");
        assert_eq!(rust.name, "Rust");
    }

    #[test]
    fn literal_filename_match() {
        // Makefile is matched by literal name, not extension.
        let mk = detect_language_info("Makefile", None).expect("Makefile should be detected");
        assert_eq!(mk.name, "Makefile");
    }

    #[test]
    fn case_insensitive_extension() {
        let rust = detect_language_info("LIB.RS", None).expect("case-insensitive glob");
        assert_eq!(rust.name, "Rust");
    }

    #[test]
    fn ambiguous_m_file_picks_objc_over_matlab() {
        let content = "@interface Foo : NSObject\n@end\n";
        let lang = detect_language_info("example.m", Some(content)).expect("keywords win");
        assert_eq!(lang.name, "Objective-C");
    }

    #[test]
    fn ambiguous_m_file_without_signal_returns_none() {
        // No keywords or comments hint at either candidate.
        let lang = detect_language_info("example.m", Some("just plain words"));
        assert!(
            lang.is_none(),
            "should refuse to guess when nothing disambiguates"
        );
    }

    #[test]
    fn shebang_drives_detection_for_unknown_extension() {
        let lang = detect_language_info("script", Some("#!/usr/bin/env python\nprint(1)\n"))
            .expect("shebang fallback");
        assert_eq!(lang.name, "Python");
    }

    #[test]
    fn shebang_with_space_after_bang_normalized() {
        let lang = detect_language_info("script", Some("#! /usr/bin/env bash\necho hi\n"))
            .expect("normalized shebang");
        assert_eq!(lang.name, "Bash");
    }

    #[test]
    fn unknown_extension_without_shebang_returns_none() {
        assert!(detect_language_info("random.xyz", Some("plain text")).is_none());
        assert!(detect_language_info("random.xyz", None).is_none());
    }

    #[test]
    fn no_content_still_resolves_single_candidate() {
        // Clear-cut extension — no content needed.
        let go = detect_language_info("main.go", None).expect("go detected by glob alone");
        assert_eq!(go.name, "Go");
    }

    #[test]
    fn tokenize_splits_on_non_alnum_underscore() {
        let tokens: Vec<&str> = tokenize("hello_world foo-bar __init__").collect();
        assert_eq!(tokens, vec!["hello_world", "foo", "bar", "__init__"]);
    }

    #[test]
    fn tokenize_empty_input() {
        let tokens: Vec<&str> = tokenize("").collect();
        assert!(tokens.is_empty());
    }
}
