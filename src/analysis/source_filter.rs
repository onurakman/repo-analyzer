//! Classifies file paths as "source code" vs configuration / data / markup.
//!
//! Used by code-focused metrics (hotspots, churn, coupling, outliers, module
//! coupling) to drop paths like `pom.xml`, `package-lock.json`, or README
//! files that are not refactorable source code and would otherwise dominate
//! rankings.
//!
//! Classification rules (conservative: "unknown = not code"):
//! 1. Paths ending in `.lock` are always data.
//! 2. Explicit non-source filenames (lockfiles, CI state, docs) are rejected.
//! 3. Paths matching GitHub Linguist's `vendor.yml` patterns (node_modules/,
//!    vendor/, bower_components/, `*.min.js`, generated protobuf, fixtures, …)
//!    are rejected via [`linguist::is_vendored`].
//! 4. Otherwise, detect the language via [`crate::langs::detect_language_info`].
//!    If the language name is in [`NON_CODE_LANGUAGES`], it's not source code.
//!    If no language is detected, treat it as not source code.

use crate::langs::detect_language_info;

/// Language names (as they appear in `languages.json5`) treated as non-code:
/// data formats, markup, docs, and configuration dialects without logic.
const NON_CODE_LANGUAGES: &[&str] = &[
    "Apache Config",
    "AsciiDoc",
    "BibTeX",
    "CSV",
    "Dotenv",
    "EditorConfig",
    "INI",
    "JSON",
    "JSON5",
    "Markdown",
    "Nginx Config",
    "Org",
    "reStructuredText",
    "Roff",
    "SVG",
    "Textile",
    "TOML",
    "WiX",
    "XML",
    "YAML",
];

/// Exact filenames that are never source code regardless of extension-based
/// language detection (e.g. `pom.xml` would be detected as XML, but we also
/// list it here for clarity / to override any future XML reclassification).
const NON_CODE_FILENAMES: &[&str] = &[
    // JS/TS ecosystem
    "package.json",
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "bun.lockb",
    "npm-shrinkwrap.json",
    // Rust
    "Cargo.lock",
    // Python
    "Pipfile",
    "Pipfile.lock",
    "poetry.lock",
    "uv.lock",
    "requirements.txt",
    "requirements-dev.txt",
    // Go
    "go.sum",
    // Ruby
    "Gemfile.lock",
    // PHP
    "composer.lock",
    // Elixir
    "mix.lock",
    // Java / JVM (handled by XML classifier, listed for safety)
    "pom.xml",
    // Docs / legal
    "LICENSE",
    "LICENCE",
    "COPYING",
    "NOTICE",
    "AUTHORS",
    "CONTRIBUTORS",
    "CHANGELOG",
    "CHANGES",
    "README",
    "HISTORY",
    // Git / tooling metadata
    ".gitignore",
    ".gitattributes",
    ".gitmodules",
    ".editorconfig",
    ".dockerignore",
    ".npmignore",
    ".prettierignore",
    ".eslintignore",
];

/// Returns `true` when the path looks like human-maintained source code that
/// would be a plausible refactor / architecture target. Config, data, docs,
/// and lockfiles all return `false`.
#[must_use]
pub fn is_source_file(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);

    // Any *.lock file is a generated artifact, regardless of language detection.
    if name.ends_with(".lock") {
        return false;
    }

    if NON_CODE_FILENAMES
        .iter()
        .any(|n| name.eq_ignore_ascii_case(n))
    {
        return false;
    }

    // Linguist's curated vendor.yml — catches `node_modules/`, `vendor/`,
    // `bower_components/`, minified bundles (`*.min.js`), generated protobuf,
    // test fixtures, etc. Failure-open: treat a regex error as "not vendored"
    // so a broken upstream pattern never blocks legitimate source files.
    if linguist::is_vendored(path).unwrap_or(false) {
        return false;
    }

    match detect_language_info(path, None) {
        Some(lang) => !NON_CODE_LANGUAGES.contains(&lang.name),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_go_python_are_source() {
        assert!(is_source_file("src/main.rs"));
        assert!(is_source_file("cmd/api/main.go"));
        assert!(is_source_file("app/models.py"));
    }

    #[test]
    fn frontend_sources_are_code() {
        assert!(is_source_file("web/src/App.tsx"));
        assert!(is_source_file("web/src/styles/main.scss"));
        assert!(is_source_file("web/index.html"));
    }

    #[test]
    fn build_scripts_with_logic_are_code() {
        // Makefile / Dockerfile / CMake have logic — keep as code.
        assert!(is_source_file("Makefile"));
        assert!(is_source_file("Dockerfile"));
        assert!(is_source_file("CMakeLists.txt"));
    }

    #[test]
    fn config_files_are_not_code() {
        assert!(!is_source_file("config.yaml"));
        assert!(!is_source_file(".github/workflows/ci.yml"));
        assert!(!is_source_file("pyproject.toml"));
        assert!(!is_source_file("tsconfig.json"));
    }

    #[test]
    fn manifest_and_lockfiles_are_not_code() {
        assert!(!is_source_file("pom.xml"));
        assert!(!is_source_file("web/package.json"));
        assert!(!is_source_file("web/package-lock.json"));
        assert!(!is_source_file("Cargo.lock"));
        assert!(!is_source_file("vendor/foo.lock"));
        assert!(!is_source_file("go.sum"));
    }

    #[test]
    fn docs_are_not_code() {
        assert!(!is_source_file("README.md"));
        assert!(!is_source_file("docs/guide.rst"));
        assert!(!is_source_file("LICENSE"));
        assert!(!is_source_file("CHANGELOG.md"));
    }

    #[test]
    fn unknown_extensions_are_not_code() {
        assert!(!is_source_file("assets/logo.png"));
        assert!(!is_source_file("data/dump.bin"));
        assert!(!is_source_file("some.randomext"));
    }

    #[test]
    fn nested_paths_resolve_correctly() {
        assert!(is_source_file("a/b/c/d.rs"));
        assert!(!is_source_file("a/b/c/config.yaml"));
    }

    #[test]
    fn case_insensitive_filename_match() {
        assert!(!is_source_file("license"));
        assert!(!is_source_file("LICENSE.TXT".to_lowercase().as_str()));
    }

    #[test]
    fn vendored_paths_dropped_by_linguist() {
        // Patterns that weren't in our hardcoded list but Linguist's
        // vendor.yml covers. Regression guard: if the linguist crate's
        // vendor patterns change in a way that stops catching these, the
        // signal for code-focused metrics regresses silently.
        assert!(!is_source_file("node_modules/react/index.js"));
        assert!(!is_source_file("vendor/bundle/gems/rails.rb"));
        assert!(!is_source_file("bower_components/jquery/jquery.js"));
        assert!(!is_source_file("web/dist/app.min.js"));
    }
}
