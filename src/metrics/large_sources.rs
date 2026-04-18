use std::collections::HashMap;

use crate::analysis::line_classifier::count_lines;
use crate::langs::detect_language_info;
use crate::metrics::MetricCollector;
use crate::types::{MetricEntry, MetricResult, MetricValue, ParsedChange};

/// Blobs above this size are considered "enormous" — scanning them with the
/// line classifier is still cheap, but bloat catches the truly absurd
/// non-source blobs higher up.
const MAX_BLOB_BYTES: u64 = 20 * 1024 * 1024;

/// Files with fewer code lines than this don't deserve a row — they aren't
/// split candidates. The point of the report is to surface real outliers.
const MIN_CODE_LINES: u64 = 500;

/// Top-N by code-LOC descending. Uncapped output would be noisy in
/// monorepos with lots of moderately-large files.
const MAX_ENTRIES: usize = 100;

/// Languages we consider "source code" for this report. Markup/data formats
/// (JSON, YAML, TOML, Markdown) are intentionally excluded — a 5k-line
/// JSON is a config artifact, not a split candidate.
const SOURCE_LANGUAGE_NAMES: &[&str] = &[
    "Rust",
    "Python",
    "Java",
    "JavaScript",
    "TypeScript",
    "Go",
    "Kotlin",
    "Dart",
    "C",
    "C++",
    "C#",
    "PHP",
    "Ruby",
    "Scala",
    "Swift",
    "Bash",
    "Shell",
    "Objective-C",
];

struct SourceFile {
    path: String,
    bytes: u64,
    code: u64,
    comment: u64,
    blank: u64,
    language: &'static str,
}

pub struct LargeSourcesCollector {
    files: Vec<SourceFile>,
}

impl Default for LargeSourcesCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl LargeSourcesCollector {
    pub fn new() -> Self {
        Self { files: Vec::new() }
    }
}

impl MetricCollector for LargeSourcesCollector {
    fn name(&self) -> &str {
        "large_sources"
    }

    fn process(&mut self, _change: &ParsedChange) {}

    fn inspect_repo(
        &mut self,
        repo: &gix::Repository,
        _progress: &crate::metrics::ProgressReporter,
    ) -> anyhow::Result<()> {
        let head_commit = match repo.head_commit() {
            Ok(c) => c,
            Err(_) => return Ok(()),
        };
        let tree = head_commit.tree()?;
        walk_tree(repo, &tree, "", &mut self.files);
        Ok(())
    }

    fn finalize(&mut self) -> MetricResult {
        let mut files: Vec<SourceFile> = std::mem::take(&mut self.files);
        files.retain(|f| f.code >= MIN_CODE_LINES);
        files.sort_by_key(|f| std::cmp::Reverse(f.code));
        files.truncate(MAX_ENTRIES);

        let entries: Vec<MetricEntry> = files
            .into_iter()
            .map(|f| {
                let total = f.code + f.comment + f.blank;
                let mut values = HashMap::new();
                values.insert("size_bytes".into(), MetricValue::Count(f.bytes));
                values.insert("size_human".into(), MetricValue::Text(human_size(f.bytes)));
                values.insert("code_lines".into(), MetricValue::Count(f.code));
                values.insert("comment_lines".into(), MetricValue::Count(f.comment));
                values.insert("blank_lines".into(), MetricValue::Count(f.blank));
                values.insert("total_lines".into(), MetricValue::Count(total));
                values.insert("language".into(), MetricValue::Text(f.language.into()));
                values.insert(
                    "recommendation".into(),
                    MetricValue::Text(classify(f.code).into()),
                );
                MetricEntry {
                    key: f.path,
                    values,
                }
            })
            .collect();

        MetricResult {
            name: "large_sources".into(),
            display_name: "Large Source Files".into(),
            description: "Source files (handwritten code, not markup or data) sorted by executable code lines. Anything over 1500 code lines is a split candidate — it's hard to navigate, review, and test. An enormous file (5k+ code lines) is usually either auto-generated (consider moving to a build step instead of committing) or a 'god module' that has accreted too many responsibilities.".into(),
            entry_groups: vec![],
            column_labels: vec![],
            columns: vec![
                "size_bytes".into(),
                "size_human".into(),
                "code_lines".into(),
                "comment_lines".into(),
                "blank_lines".into(),
                "total_lines".into(),
                "language".into(),
                "recommendation".into(),
            ],
            entries,
        }
    }
}

fn walk_tree(repo: &gix::Repository, tree: &gix::Tree, prefix: &str, out: &mut Vec<SourceFile>) {
    for entry_res in tree.iter() {
        let entry = match entry_res {
            Ok(e) => e,
            Err(_) => continue,
        };
        let name = entry.filename().to_string();
        let full_path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let id = entry.oid();
        let mode = entry.mode();

        if mode.is_tree() {
            if let Ok(subobj) = repo.find_object(id)
                && let Ok(subtree) = subobj.try_into_tree()
            {
                walk_tree(repo, &subtree, &full_path, out);
            }
        } else if mode.is_blob() {
            let Ok(obj) = repo.find_object(id) else {
                continue;
            };
            let size = obj.data.len() as u64;
            if size == 0 || size > MAX_BLOB_BYTES {
                continue;
            }
            // Binary screen: NUL in first 8 KB → skip.
            let sample = &obj.data[..obj.data.len().min(8192)];
            if sample.contains(&0) {
                continue;
            }
            let Ok(content) = std::str::from_utf8(&obj.data) else {
                continue;
            };
            let Some(lang) = detect_language_info(&full_path, Some(content)) else {
                continue;
            };
            if !is_source_language(lang.name) {
                continue;
            }
            let counts = count_lines(content, Some(lang));
            out.push(SourceFile {
                path: full_path,
                bytes: size,
                code: counts.code,
                comment: counts.comment,
                blank: counts.blank,
                language: lang.name,
            });
        }
    }
}

pub fn is_source_language(name: &str) -> bool {
    SOURCE_LANGUAGE_NAMES.contains(&name)
}

/// Quick extension-based check used by the `bloat` collector to decide
/// whether to treat a file as source code (and defer to this report) or as
/// a potential git-hygiene issue.
pub fn is_source_path(path: &str) -> bool {
    matches!(
        path.rsplit('.').next().unwrap_or(""),
        "rs" | "py"
            | "pyi"
            | "ts"
            | "tsx"
            | "js"
            | "jsx"
            | "mjs"
            | "cjs"
            | "java"
            | "go"
            | "kt"
            | "kts"
            | "cs"
            | "cpp"
            | "cc"
            | "cxx"
            | "hpp"
            | "h"
            | "c"
            | "rb"
            | "php"
            | "scala"
            | "sc"
            | "swift"
            | "dart"
            | "sh"
            | "bash"
            | "m"
            | "mm"
    )
}

fn classify(code_lines: u64) -> &'static str {
    match code_lines {
        0..=1499 => "Sizeable — monitor",
        1500..=4999 => "Very large — consider splitting",
        _ => "Enormous — likely generated or a god module",
    }
}

fn human_size(n: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * 1024;
    if n >= MB {
        format!("{:.2} MB", n as f64 / MB as f64)
    } else if n >= KB {
        format!("{:.1} KB", n as f64 / KB as f64)
    } else {
        format!("{} B", n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_thresholds() {
        assert_eq!(classify(0), "Sizeable — monitor");
        assert_eq!(classify(1499), "Sizeable — monitor");
        assert_eq!(classify(1500), "Very large — consider splitting");
        assert_eq!(classify(4999), "Very large — consider splitting");
        assert_eq!(
            classify(5000),
            "Enormous — likely generated or a god module"
        );
    }

    #[test]
    fn source_path_detection_whitelist() {
        assert!(is_source_path("src/main.rs"));
        assert!(is_source_path("lib/app.dart"));
        assert!(is_source_path("Main.java"));
        assert!(is_source_path("script.mjs"));
        assert!(!is_source_path("README.md"));
        assert!(!is_source_path("package.json"));
        assert!(!is_source_path("data.csv"));
        assert!(!is_source_path("image.png"));
    }

    #[test]
    fn source_language_whitelist_matches_detection_names() {
        // Sanity: the language names here must match what `detect_language_info`
        // returns. Pick a handful to guard against drift.
        let rust = detect_language_info("x.rs", None).unwrap();
        assert!(is_source_language(rust.name));
        let md = detect_language_info("README.md", None).unwrap();
        assert!(!is_source_language(md.name));
    }

    #[test]
    fn human_size_formats() {
        assert_eq!(human_size(100), "100 B");
        assert_eq!(human_size(2048), "2.0 KB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.00 MB");
    }
}
