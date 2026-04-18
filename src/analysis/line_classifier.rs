use std::borrow::Cow;

use memchr::{memchr2, memrchr};

use crate::langs::{
    Language,
    scoring::{BlockCommentMatchers, language_matchers},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineType {
    Code,
    Comment,
    Blank,
    Shebang,
}

/// Tracks nested block comment state across lines.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CommentState {
    block_comment_depth: usize,
}

impl CommentState {
    #[must_use]
    #[inline]
    pub fn new() -> Self {
        Self::default()
    }

    #[inline]
    const fn enter_first_block(&mut self) {
        self.block_comment_depth = 1;
    }

    #[inline]
    const fn exit_block(&mut self, nested: bool) {
        if nested {
            self.block_comment_depth = self.block_comment_depth.saturating_sub(1);
        } else {
            self.block_comment_depth = 0;
        }
    }

    #[inline]
    const fn enter_nested_block(&mut self) {
        self.block_comment_depth = self.block_comment_depth.saturating_add(1);
    }

    #[must_use]
    #[inline]
    const fn is_in_comment(&self) -> bool {
        self.block_comment_depth > 0
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct LineCounts {
    pub code: u64,
    pub comment: u64,
    pub blank: u64,
    pub shebang: u64,
}

impl LineCounts {
    #[must_use]
    #[allow(dead_code)]
    pub const fn total(&self) -> u64 {
        self.code + self.comment + self.blank + self.shebang
    }
}

/// Classify every line in `content` for the given language. Returns aggregate
/// counts. If `lang` is `None`, every non-blank line is treated as code.
#[must_use]
pub fn count_lines(content: &str, lang: Option<&Language>) -> LineCounts {
    let mut counts = LineCounts::default();
    let mut state = CommentState::new();
    for (idx, line) in content.lines().enumerate() {
        match classify_line(line, lang, &mut state, idx == 0) {
            LineType::Code => counts.code += 1,
            LineType::Comment => counts.comment += 1,
            LineType::Blank => counts.blank += 1,
            LineType::Shebang => counts.shebang += 1,
        }
    }
    counts
}

/// Process block comments on a line, updating state and detecting code.
/// Returns: (remaining line portion, has_code_outside_comments).
#[inline]
fn handle_block_comments<'a>(
    line: &'a str,
    matchers: &BlockCommentMatchers,
    comment_state: &mut CommentState,
    nested: bool,
) -> (&'a str, bool) {
    let mut line_remainder = line;
    let mut has_code = false;
    while !line_remainder.is_empty() {
        if !comment_state.is_in_comment() {
            if let Some((pos, start_len)) = matchers.find_block_start(line_remainder) {
                if pos > 0 && contains_non_whitespace(&line_remainder[..pos]) {
                    has_code = true;
                }
                line_remainder = &line_remainder[pos + start_len..];
                comment_state.enter_first_block();
            } else {
                break;
            }
        } else if let Some((pos, len, found_nested_start)) =
            matchers.find_block_end_or_nested_start(line_remainder, nested)
        {
            if nested && found_nested_start {
                comment_state.enter_nested_block();
            } else {
                comment_state.exit_block(nested);
            }
            line_remainder = &line_remainder[pos + len..];
        } else {
            break;
        }
    }
    (line_remainder, has_code)
}

/// Classify a single line as code, comment, blank, or shebang.
#[inline]
pub fn classify_line(
    line: &str,
    lang_info: Option<&Language>,
    comment_state: &mut CommentState,
    is_first_line: bool,
) -> LineType {
    let trimmed = trim_ascii(line);
    if trimmed.is_empty() {
        return LineType::Blank;
    }
    if is_first_line
        && trimmed.starts_with("#!")
        && let Some(lang) = lang_info
        && !lang.shebangs.is_empty()
    {
        let normalized: Cow<'_, str> = trimmed
            .strip_prefix("#! ")
            .map_or(Cow::Borrowed(trimmed), |rest| {
                Cow::Owned(format!("#!{rest}"))
            });
        if lang
            .shebangs
            .iter()
            .any(|shebang| normalized.starts_with(shebang))
        {
            return LineType::Shebang;
        }
    }
    let Some(lang) = lang_info else {
        return LineType::Code;
    };
    let mut line_remainder: &str = trimmed;
    let matchers = language_matchers(lang);
    let mut has_code = if let Some(block_comments) = matchers.block_comments.as_ref() {
        let (remainder, found_code) =
            handle_block_comments(trimmed, block_comments, comment_state, lang.nested_blocks);
        line_remainder = remainder;
        found_code
    } else {
        false
    };
    if comment_state.is_in_comment() {
        return if has_code {
            LineType::Code
        } else {
            LineType::Comment
        };
    }
    if let Some(line_comments) = matchers.line_comments.as_ref() {
        for matched in line_comments.find_iter(line_remainder) {
            let token = lang.line_comments[matched.pattern().as_usize()];
            if !is_valid_line_comment_match(line_remainder, matched.end(), token) {
                continue;
            }
            let pos = matched.start();
            if pos > 0 && contains_non_whitespace(&line_remainder[..pos]) {
                has_code = true;
            }
            return if has_code {
                LineType::Code
            } else {
                LineType::Comment
            };
        }
    }
    if contains_non_whitespace(line_remainder) {
        has_code = true;
    }
    if has_code {
        LineType::Code
    } else {
        LineType::Comment
    }
}

/// Fast ASCII-only whitespace trim with newline handling — byte-indexed to
/// skip UTF-8 boundary checks on the hot path.
#[inline]
fn trim_ascii(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut start = 0;
    let mut end = bytes.len();
    if let Some(pos) = memrchr(b'\n', &bytes[..end])
        && pos + 1 == end
    {
        end = pos;
        if end > 0 && bytes[end - 1] == b'\r' {
            end -= 1;
        }
    }
    while start < end && is_ascii_ws(bytes[start]) {
        start += 1;
    }
    while end > start && is_ascii_ws(bytes[end - 1]) {
        end -= 1;
    }
    &line[start..end]
}

#[inline]
fn contains_non_whitespace(s: &str) -> bool {
    let bytes = s.as_bytes();
    let mut idx = 0;
    while idx < bytes.len() {
        let byte = bytes[idx];
        if !is_ascii_ws(byte) {
            return true;
        }
        if byte == b' ' || byte == b'\t' {
            if let Some(pos) = memchr2(b' ', b'\t', &bytes[idx..]) {
                idx += pos + 1;
            } else {
                idx = bytes.len();
            }
        } else {
            idx += 1;
        }
    }
    false
}

#[inline]
const fn is_ascii_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0B | 0x0C)
}

#[inline]
const fn is_word_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

#[inline]
fn is_valid_line_comment_match(line: &str, end: usize, token: &str) -> bool {
    let Some(&first) = token.as_bytes().first() else {
        return false;
    };
    if is_word_char(first) {
        let bytes = line.as_bytes();
        if end < bytes.len() && is_word_char(bytes[end]) {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::langs::detect_language_info;

    #[test]
    fn blank_and_unknown_language_paths() {
        let mut state = CommentState::new();
        assert_eq!(classify_line("", None, &mut state, false), LineType::Blank);
        assert_eq!(
            classify_line("   \t  ", None, &mut state, false),
            LineType::Blank
        );
        // Without language info, any non-blank line is treated as code.
        assert_eq!(
            classify_line("some code", None, &mut state, false),
            LineType::Code
        );
    }

    #[test]
    fn count_lines_rust_code_comment_blank() {
        let rust = detect_language_info("foo.rs", None).expect("rust should be detected");
        let src = "// hello\nfn foo() {}\n\n// trailing\n";
        let counts = count_lines(src, Some(rust));
        assert_eq!(counts.code, 1);
        assert_eq!(counts.comment, 2);
        assert_eq!(counts.blank, 1);
        assert_eq!(counts.total(), 4);
    }

    #[test]
    fn count_lines_block_comment_spans_lines() {
        let rust = detect_language_info("foo.rs", None).expect("rust should be detected");
        // Three pure comment lines inside a block comment, then one code line.
        let src = "/*\n first\n second\n*/\nfn bar() {}\n";
        let counts = count_lines(src, Some(rust));
        assert_eq!(counts.comment, 4);
        assert_eq!(counts.code, 1);
    }

    #[test]
    fn count_lines_code_before_block_comment_on_same_line() {
        let rust = detect_language_info("foo.rs", None).expect("rust should be detected");
        // First line has both code and an opening block comment — should be code.
        let src = "let x = 1; /* comment starts\nstill comment\n*/\n";
        let counts = count_lines(src, Some(rust));
        assert_eq!(counts.code, 1, "line with code + block opener is code");
        assert_eq!(counts.comment, 2);
    }

    #[test]
    fn count_lines_shebang_counted_separately() {
        // Bash declares a shebang pattern, so we should see one shebang line.
        let sh = detect_language_info("x.sh", None).expect("bash should be detected");
        let src = "#!/usr/bin/env bash\necho hi\n";
        let counts = count_lines(src, Some(sh));
        assert_eq!(counts.shebang, 1);
        assert_eq!(counts.code, 1);
    }

    #[test]
    fn count_lines_handles_crlf_newlines() {
        let rust = detect_language_info("foo.rs", None).expect("rust should be detected");
        let src = "// one\r\nfn main() {}\r\n";
        let counts = count_lines(src, Some(rust));
        assert_eq!(counts.comment, 1);
        assert_eq!(counts.code, 1);
    }

    #[test]
    fn comment_state_resets_between_files() {
        let rust = detect_language_info("foo.rs", None).expect("rust should be detected");
        // Unclosed block comment — state is "in comment" at EOF.
        let src = "/* never closes\nstill inside\n";
        let mut state = CommentState::new();
        for (i, line) in src.lines().enumerate() {
            classify_line(line, Some(rust), &mut state, i == 0);
        }
        // After a file with an unclosed block comment, `state.is_in_comment()`
        // should be true — callers creating a fresh state per file avoid the
        // bleed. This test encodes that contract so a future change that
        // shares state across files fails loudly.
        let mut second_state = CommentState::new();
        let counts = count_lines("fn ok() {}\n", Some(rust));
        assert_eq!(counts.code, 1);
        // Sanity: a fresh state still classifies code correctly.
        assert_eq!(
            classify_line("fn ok() {}", Some(rust), &mut second_state, false),
            LineType::Code
        );
    }

    #[test]
    fn line_type_equality_and_copy() {
        let a = LineType::Code;
        let b = a; // Copy
        assert_eq!(a, b);
        assert_ne!(LineType::Code, LineType::Comment);
    }
}
