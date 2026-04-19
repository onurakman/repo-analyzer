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
//! 3. Filenames matching a content-hash pattern (`<name>.<hex8+>.<ext>`, e.g.
//!    webpack/rollup/vite build output) are rejected.
//! 4. Paths containing a conventional asset/build segment (`/assets/`,
//!    `/dist/`, `/build/`, `/static/`, `/public/`, `/.next/`, `/out/`,
//!    `/coverage/`, `/target/`) are rejected — these directories typically
//!    hold generated, vendored, or non-refactorable content regardless of
//!    file extension. Monorepos still match because the check is substring,
//!    not prefix. Known false positive: Rails' `app/assets/stylesheets/`.
//! 5. Paths matching GitHub Linguist's `vendor.yml` patterns (node_modules/,
//!    vendor/, bower_components/, `*.min.js`, generated protobuf, fixtures, …)
//!    are rejected via [`linguist::is_vendored`].
//! 6. Otherwise, detect the language via [`crate::langs::detect_language_info`].
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

/// Path segments that indicate generated, vendored, or asset-pipeline content
/// regardless of file extension or language detection. Matched as substrings
/// against the path with a synthetic leading `/`, so top-level directories
/// like `assets/` and nested ones like `packages/foo/assets/` both hit.
///
/// Known false positive: Rails projects put authored stylesheets under
/// `app/assets/stylesheets/`; those will be filtered out here. A future
/// CLI `--include-pattern` flag is the intended override.
const NON_CODE_PATH_SEGMENTS: &[&str] = &[
    "/assets/",
    "/static/",
    "/public/",
    "/dist/",
    "/build/",
    "/.next/",
    "/out/",
    "/coverage/",
    "/target/",
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
    "go.mod",
    "go.sum",
    // Ruby
    "Gemfile",
    "Gemfile.lock",
    // PHP
    "composer.json",
    "composer.lock",
    // Elixir
    "mix.lock",
    // Java / JVM — build manifests. Groovy / Kotlin syntax but pure
    // dependency declarations, not refactor targets.
    "build.gradle",
    "build.gradle.kts",
    "settings.gradle",
    "settings.gradle.kts",
    "pom.xml",
    // iOS / CocoaPods
    "Podfile",
    // Version-pin files (mostly extension-less so language detection already
    // rejects them, but listed for clarity).
    ".python-version",
    ".ruby-version",
    ".node-version",
    ".nvmrc",
    ".tool-versions",
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

    if is_content_hashed(name) {
        return false;
    }

    // Substring match against the path with a synthetic leading slash so that
    // both `assets/foo.css` and `packages/x/assets/foo.css` hit `/assets/`.
    let guarded = format!("/{path}");
    if NON_CODE_PATH_SEGMENTS
        .iter()
        .any(|seg| guarded.contains(seg))
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

/// Detects webpack / rollup / vite style content-hash filenames of the shape
/// `<name>.<hex8+>.<ext>`, e.g. `main.4f8b2c9a.js` or
/// `ag.d70f5d9b5da738341efc.svg`. Multi-dot filenames like `my.module.ts` or
/// `Component.stories.tsx` are preserved because their middle segment is not
/// hex.
#[must_use]
fn is_content_hashed(name: &str) -> bool {
    let parts: Vec<&str> = name.rsplitn(3, '.').collect();
    if parts.len() < 3 {
        return false;
    }
    let hash = parts[1];
    hash.len() >= 8 && hash.chars().all(|c| c.is_ascii_hexdigit())
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
    fn build_manifests_across_ecosystems_rejected() {
        // Go modules — detected by linguist as a "Go Module" data language
        // in some versions; pin via exact filename for stability.
        assert!(!is_source_file("go.mod"));
        assert!(!is_source_file("cmd/api/go.mod"));

        // Gradle manifests — Groovy/Kotlin syntax but pure deps.
        assert!(!is_source_file("build.gradle"));
        assert!(!is_source_file("app/build.gradle"));
        assert!(!is_source_file("build.gradle.kts"));
        assert!(!is_source_file("settings.gradle"));
        assert!(!is_source_file("settings.gradle.kts"));

        // Ruby / PHP / Swift manifests.
        assert!(!is_source_file("Gemfile"));
        assert!(!is_source_file("project/Gemfile"));
        assert!(!is_source_file("composer.json"));
        assert!(!is_source_file("Podfile"));
        assert!(!is_source_file("ios/Podfile"));
    }

    #[test]
    fn version_pin_files_rejected() {
        assert!(!is_source_file(".python-version"));
        assert!(!is_source_file(".ruby-version"));
        assert!(!is_source_file(".node-version"));
        assert!(!is_source_file(".nvmrc"));
        assert!(!is_source_file(".tool-versions"));
        assert!(!is_source_file("backend/.python-version"));
    }

    #[test]
    fn build_scripts_with_real_logic_still_code() {
        // Regression guard: after adding manifest exact-match entries above,
        // nearby files that DO carry logic must remain classified as source.
        assert!(is_source_file("Rakefile"));
        assert!(is_source_file("Gulpfile.js"));
        assert!(is_source_file("webpack.config.js"));
        assert!(is_source_file("vite.config.ts"));
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

    #[test]
    fn asset_pipeline_paths_rejected() {
        // Real paths reported by users — vendored CSS/SCSS under
        // conventional asset directories that Linguist's vendor.yml does
        // not catch because the library name is unknown to Linguist.
        assert!(!is_source_file("eom-ui/assets/css/style.css"));
        assert!(!is_source_file(
            "eom-ui/assets/scss/theme/_custom-theme-options.scss"
        ));
        assert!(!is_source_file("eom-ui/assets/scss/icoicon/_icons.scss"));
        assert!(!is_source_file("eom-ui/assets/css/icofont.css"));
        assert!(!is_source_file("eom-ui/assets/scss/theme/_responsive.scss"));
        assert!(!is_source_file("eom-ui/assets/scss/theme/_rtl.scss"));
        assert!(!is_source_file("eom-ui/assets/css/flag-icon.css"));
        assert!(!is_source_file("eom-ui/assets/fonts/flag-icon/eg.svg"));
    }

    #[test]
    fn conventional_build_and_static_folders_rejected() {
        assert!(!is_source_file("web/public/index.html"));
        assert!(!is_source_file("app/dist/bundle.js"));
        assert!(!is_source_file("packages/foo/build/out.js"));
        assert!(!is_source_file("site/.next/server/pages.js"));
        assert!(!is_source_file("proj/out/export.html"));
        assert!(!is_source_file("coverage/lcov-report/index.html"));
        assert!(!is_source_file("target/debug/deps/foo.d"));
        // Monorepo nesting still hits.
        assert!(!is_source_file(
            "apps/frontend/packages/ui/assets/theme.scss"
        ));
    }

    #[test]
    fn content_hash_filenames_rejected() {
        // Webpack / rollup / vite content-hashed output.
        assert!(!is_source_file("ag.d70f5d9b5da738341efc.svg"));
        assert!(!is_source_file("main.4f8b2c9a.js"));
        assert!(!is_source_file("styles.a1b2c3d4e5f6.css"));
        assert!(!is_source_file("web/src/chunk.1234abcd.js"));
    }

    #[test]
    fn non_hash_multi_dot_filenames_still_code() {
        // These have multi-dot filenames but the middle segment is not hex
        // — they must remain classified as source.
        assert!(is_source_file("src/my.module.ts"));
        assert!(is_source_file("web/src/foo.test.ts"));
        assert!(is_source_file("lib.v2.py"));
        assert!(is_source_file("web/src/Component.stories.tsx"));
    }

    #[test]
    fn top_level_asset_dir_also_rejected() {
        // Not nested under another directory; our /-guarded substring
        // match still catches it.
        assert!(!is_source_file("assets/main.css"));
        assert!(!is_source_file("dist/bundle.js"));
        assert!(!is_source_file("public/favicon.ico"));
    }
}
