pub mod scoring;

use std::borrow::Cow;

use super::data::{LANGUAGES, Language};

/// Detect the language for a given filename. If content is provided, it will
/// be used for shebang sniffing and to disambiguate when multiple languages
/// share the filename pattern (e.g. `*.m` = Objective-C or MATLAB).
///
/// Detection is delegated to the [`linguist`] crate (GitHub Linguist data).
/// The returned `Language` is still a reference into the codestats-derived
/// [`LANGUAGES`] table so downstream line classification (comment / blank line
/// counting, complexity scoring) keeps working. Linguist names are mapped to
/// codestats names via [`map_to_codestats`]; mismatches fall back to a small
/// alias table.
#[must_use]
pub fn detect_language_info(filename: &str, content: Option<&str>) -> Option<&'static Language> {
    let mut candidates: Vec<&'static str> = linguist::detect_language_by_filename(filename)
        .unwrap_or_default()
        .into_iter()
        .map(|d| d.name)
        .collect();

    if candidates.is_empty() {
        candidates = linguist::detect_language_by_extension(filename)
            .unwrap_or_default()
            .into_iter()
            .map(|d| d.name)
            .collect();
    }

    // Linguist is case-sensitive on extensions (`.RS` ≠ `.rs`), but source
    // trees routinely ship uppercase extensions (old Windows repos, macros,
    // etc). Retry with a lowercased filename so codestats' case-insensitive
    // behavior is preserved.
    if candidates.is_empty() {
        let lc = filename.to_ascii_lowercase();
        if lc != filename {
            candidates = linguist::detect_language_by_extension(&lc)
                .unwrap_or_default()
                .into_iter()
                .map(|d| d.name)
                .collect();
        }
    }

    let resolved: &'static str = match candidates.len() {
        // No glob/filename match: fall back to codestats' shebang detection
        // for extensionless scripts (`script` with `#!/usr/bin/env python`).
        // Linguist has no shebang-only API.
        0 => return content.and_then(detect_from_shebang),
        1 => candidates[0],
        _ => match content {
            Some(c) => {
                // Prefer shebang when ambiguous extensions ship a hashbang
                // (`foo.pl` vs `#!/usr/bin/env perl`). If the shebang maps
                // to one of the candidates, honor it; otherwise try
                // Linguist's heuristic rules; otherwise refuse to guess —
                // content was provided and nothing matched, so guessing here
                // would be worse than admitting defeat.
                if let Some(lang) = detect_from_shebang(c)
                    && candidates.contains(&lang.name)
                {
                    return Some(lang);
                }
                let disamb = linguist::disambiguate(filename, c).unwrap_or_default();
                match disamb.first() {
                    Some(d) => d.name,
                    None => return None,
                }
            }
            None => {
                // No content means heuristic rules can't fire. Still try
                // `disambiguate(filename, "")` — a handful of rules resolve
                // without reading bytes (e.g. `.h` → C by default). If
                // nothing matches, degrade to the first candidate that has a
                // codestats counterpart; Linguist-only languages (e.g.
                // RenderScript for `.rs`) get skipped in favor of the one
                // codestats can actually line-count.
                let disamb = linguist::disambiguate(filename, "").unwrap_or_default();
                if let Some(d) = disamb.first() {
                    d.name
                } else {
                    return candidates.iter().find_map(|n| map_to_codestats(n));
                }
            }
        },
    };

    map_to_codestats(resolved)
}

/// Map a Linguist language name to the corresponding codestats [`Language`]
/// entry. Direct name match first; small alias table handles the handful of
/// names that differ between the two datasets. Returns `None` if Linguist
/// detects a language codestats doesn't know (rare — accepted regression).
fn map_to_codestats(linguist_name: &str) -> Option<&'static Language> {
    if let Some(lang) = LANGUAGES.iter().find(|l| l.name == linguist_name) {
        return Some(lang);
    }
    let alias = match linguist_name {
        "Shell" => "Bash",
        "TSX" => "TypeScript",
        "JSX" => "JavaScript",
        "Vim Script" => "Vim",
        "Cython" => "Python",
        "HTML+ERB" => "HTML",
        "JavaScript+ERB" => "JavaScript",
        "TypeScript+ERB" => "TypeScript",
        "Ruby+ERB" => "Ruby",
        _ => return None,
    };
    LANGUAGES.iter().find(|l| l.name == alias)
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
        let mk = detect_language_info("Makefile", None).expect("Makefile should be detected");
        assert_eq!(mk.name, "Makefile");
    }

    #[test]
    fn case_insensitive_extension() {
        let rust = detect_language_info("LIB.RS", None).expect("case-insensitive extension");
        assert_eq!(rust.name, "Rust");
    }

    #[test]
    fn ambiguous_m_file_picks_objc_over_matlab() {
        let content = "@interface Foo : NSObject\n@end\n";
        let lang = detect_language_info("example.m", Some(content)).expect("heuristic wins");
        assert_eq!(lang.name, "Objective-C");
    }

    #[test]
    fn ambiguous_m_file_without_signal_returns_none() {
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
        let go = detect_language_info("main.go", None).expect("go detected by glob alone");
        assert_eq!(go.name, "Go");
    }

    #[test]
    fn shell_alias_maps_to_bash() {
        // Linguist calls it "Shell"; codestats calls it "Bash".
        let sh = detect_language_info("script.sh", None).expect("shell → bash alias");
        assert_eq!(sh.name, "Bash");
    }
}
