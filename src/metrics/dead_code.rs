//! File-level dead code detection.
//!
//! A file is flagged as "potentially dead" when:
//! 1. it is a source file (passes `is_source_file`),
//! 2. the import graph has zero fan-in (nobody in the repo imports it),
//! 3. it is not a known entry-point filename (`main.rs`, `lib.rs`,
//!    `__init__.py`, `index.ts`, …), and
//! 4. it is not a test file (`*_test.*`, `*.spec.*`, inside `tests/`,
//!    `__tests__/`, etc.).
//!
//! Reuses `fan_in_out`'s regex-based import extraction. This catches the
//! high-confidence case (an isolated module file) without trying to be a
//! symbol-level unused-analysis — that would need a full language-server
//! resolver. Orphan files in dynamic languages (Python eval, JS require()
//! via variable) may generate false positives; user should verify before
//! deleting.

use std::collections::{HashMap, HashSet};

use crate::analysis::source_filter::is_source_file;
use crate::metrics::MetricCollector;
use crate::metrics::fan_in_out::{collect_blobs, detect_lang, extract_imports, resolve_import};
use crate::types::{
    Column, MetricEntry, MetricResult, MetricValue, ParsedChange, report_description,
    report_display,
};

const MAX_BLOB_BYTES: u64 = 200 * 1024;

/// Top-N cap on the per-file orphan list. Monorepos with test-generated
/// fixtures can produce hundreds of orphans; the table stays readable.
const MAX_ORPHANS: usize = 200;

/// Filenames that count as entry points. Zero imports from other code is
/// expected for these — they are the roots of the graph, not orphans.
const ENTRY_POINT_FILENAMES: &[&str] = &[
    // Rust
    "main.rs",
    "lib.rs",
    "build.rs",
    // Python
    "__main__.py",
    "__init__.py",
    "conftest.py",
    "setup.py",
    // JS / TS
    "index.ts",
    "index.tsx",
    "index.js",
    "index.jsx",
    "index.mjs",
    "index.cjs",
    "main.ts",
    "main.js",
    "main.tsx",
    "main.jsx",
    "server.ts",
    "server.js",
    // Go
    "main.go",
    // Java / Kotlin
    "Main.java",
    "Main.kt",
    "main.kt",
    // Dart
    "main.dart",
];

/// Path segments that indicate the file is test code — tests are often
/// entry-pointed by a framework via reflection rather than via imports,
/// so zero fan-in there doesn't mean "dead".
const TEST_PATH_SEGMENTS: &[&str] = &[
    "/tests/",
    "/__tests__/",
    "/spec/",
    "/__mocks__/",
    "/e2e/",
    "/test/",
];

pub struct DeadCodeCollector {
    orphans: Vec<String>,
}

impl Default for DeadCodeCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl DeadCodeCollector {
    pub fn new() -> Self {
        Self {
            orphans: Vec::new(),
        }
    }
}

impl MetricCollector for DeadCodeCollector {
    fn name(&self) -> &str {
        "dead_code"
    }

    fn process(&mut self, _change: &ParsedChange) {}

    fn inspect_repo(
        &mut self,
        repo: &gix::Repository,
        progress: &crate::metrics::ProgressReporter,
    ) -> anyhow::Result<()> {
        let head_commit = match repo.head_commit() {
            Ok(c) => c,
            Err(_) => return Ok(()),
        };
        let tree = head_commit.tree()?;

        progress.status("  dead_code: collecting source paths...");
        let mut all_paths: Vec<(String, gix::ObjectId, u64)> = Vec::new();
        collect_blobs(repo, &tree, "", &mut all_paths);
        let path_set: HashSet<String> = all_paths.iter().map(|(p, _, _)| p.clone()).collect();

        // Build a fan-in map (path → number of files that import it). We only
        // need to know >0 vs 0, but keeping counts makes debugging easier if
        // ever needed.
        let mut fan_in: HashMap<String, u64> = HashMap::new();
        let total = all_paths.len();
        for (idx, (path, oid, size)) in all_paths.iter().enumerate() {
            if idx.is_multiple_of(200) {
                progress.status(&format!(
                    "  dead_code: {}/{total} files scanned...",
                    idx + 1
                ));
            }
            if !is_source_file(path) {
                continue;
            }
            if *size > MAX_BLOB_BYTES {
                continue;
            }
            let Some(lang) = detect_lang(path) else {
                continue;
            };
            let Ok(object) = repo.find_object(*oid) else {
                continue;
            };
            let Ok(blob) = object.try_into_blob() else {
                continue;
            };
            let Ok(source) = std::str::from_utf8(&blob.data) else {
                continue;
            };
            for raw in extract_imports(lang, source) {
                if let Some(target) = resolve_import(lang, &raw, path, &path_set)
                    && target != *path
                {
                    *fan_in.entry(target).or_insert(0) += 1;
                }
            }
        }

        for (path, _, _) in &all_paths {
            if !is_source_file(path) {
                continue;
            }
            if detect_lang(path).is_none() {
                // Only languages with import extraction can be judged. SVG,
                // CSS, Markdown, etc. have no import graph — skip, not
                // "dead" in any meaningful sense.
                continue;
            }
            if is_entry_point(path) {
                continue;
            }
            if is_test_path(path) {
                continue;
            }
            if fan_in.get(path).copied().unwrap_or(0) == 0 {
                self.orphans.push(path.clone());
            }
        }

        Ok(())
    }

    fn finalize(&mut self) -> MetricResult {
        let mut list: Vec<String> = self.orphans.drain(..).collect();
        list.sort();
        list.truncate(MAX_ORPHANS);

        let entries: Vec<MetricEntry> = list
            .into_iter()
            .map(|path| {
                let ext = path.rsplit('.').next().unwrap_or("").to_string();
                let mut values = HashMap::new();
                values.insert("extension".into(), MetricValue::Text(ext));
                MetricEntry { key: path, values }
            })
            .collect();

        MetricResult {
            name: "dead_code".into(),
            display_name: report_display("dead_code"),
            description: report_description("dead_code"),
            entry_groups: vec![],
            columns: vec![Column::in_report("dead_code", "extension")],
            entries,
        }
    }
}

fn is_entry_point(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    ENTRY_POINT_FILENAMES.contains(&name)
}

fn is_test_path(path: &str) -> bool {
    let guarded = format!("/{path}");
    if TEST_PATH_SEGMENTS.iter().any(|seg| guarded.contains(seg)) {
        return true;
    }
    let name = path.rsplit('/').next().unwrap_or(path);
    // Common test filename patterns across ecosystems.
    name.contains("_test.")
        || name.contains(".test.")
        || name.contains(".spec.")
        || name.starts_with("test_")
        || name.ends_with("Test.java")
        || name.ends_with("Tests.java")
        || name.ends_with("Spec.kt")
        || name.ends_with("Test.kt")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_points_not_flagged() {
        assert!(is_entry_point("src/main.rs"));
        assert!(is_entry_point("src/lib.rs"));
        assert!(is_entry_point("web/src/index.ts"));
        assert!(is_entry_point("pkg/foo/__init__.py"));
        assert!(is_entry_point("cmd/api/main.go"));
        // Not entry points.
        assert!(!is_entry_point("src/utils.rs"));
        assert!(!is_entry_point("pkg/foo/helpers.py"));
    }

    #[test]
    fn test_paths_detected() {
        assert!(is_test_path("tests/integration.rs"));
        assert!(is_test_path("src/foo_test.go"));
        assert!(is_test_path("web/__tests__/component.ts"));
        assert!(is_test_path("app/spec/api_spec.rb"));
        assert!(is_test_path("src/FooTest.java"));
        assert!(is_test_path("src/FooSpec.kt"));
        assert!(is_test_path("web/src/App.test.tsx"));
        assert!(is_test_path("web/src/utils.spec.ts"));
        assert!(is_test_path("tests/test_auth.py"));
        // Not tests.
        assert!(!is_test_path("src/main.rs"));
        assert!(!is_test_path("src/handlers.go"));
    }

    #[test]
    fn orphan_list_truncated_and_sorted() {
        let mut coll = DeadCodeCollector::new();
        // Seed 300 orphans in reverse-alphabetical order to exercise sort +
        // truncation in finalize().
        for i in (0..300u32).rev() {
            coll.orphans.push(format!("src/{i:04}.rs"));
        }
        let result = coll.finalize();
        assert_eq!(result.entries.len(), MAX_ORPHANS);
        let keys: Vec<&str> = result.entries.iter().map(|e| e.key.as_str()).collect();
        assert_eq!(keys[0], "src/0000.rs");
        assert!(keys[1] < keys[2]); // confirm ascending sort
    }
}
