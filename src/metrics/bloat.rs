use std::collections::HashMap;

use gix::prelude::HeaderExt;

use crate::messages;
use crate::metrics::MetricCollector;
use crate::metrics::large_sources::is_source_path;
use crate::types::{
    Column, LocalizedMessage, MetricEntry, MetricResult, MetricValue, ParsedChange, Severity,
    report_description, report_display,
};

/// Source files above this are absurd even for auto-generated code and
/// warrant a bloat flag. Below this, source files are the
/// [`large_sources`](crate::metrics::large_sources) report's concern —
/// splitting them is a refactor, not a git-hygiene action.
const SOURCE_BLOAT_THRESHOLD: u64 = 20 * 1024 * 1024;

/// Patterns for files that are commonly committed by mistake.
const SUSPICIOUS_PATTERNS: &[(&str, &str)] = &[
    (".min.js", messages::BLOAT_RECOMMENDATION_MINIFIED_BUNDLE),
    (".min.css", messages::BLOAT_RECOMMENDATION_MINIFIED_BUNDLE),
    (
        "node_modules/",
        messages::BLOAT_RECOMMENDATION_VENDORED_DEPS,
    ),
    ("dist/", messages::BLOAT_RECOMMENDATION_BUILD_OUTPUT),
    ("build/", messages::BLOAT_RECOMMENDATION_BUILD_OUTPUT),
    ("target/", messages::BLOAT_RECOMMENDATION_RUST_BUILD_OUTPUT),
    ("vendor/", messages::BLOAT_RECOMMENDATION_VENDORED_DEPS),
    (".DS_Store", messages::BLOAT_RECOMMENDATION_OS_METADATA),
    (".idea/", messages::BLOAT_RECOMMENDATION_IDE_CONFIG),
    (".vscode/", messages::BLOAT_RECOMMENDATION_IDE_CONFIG),
];

const LARGE_FILE_THRESHOLD: u64 = 500 * 1024; // 500 KB

pub struct BloatCollector {
    files: Vec<(String, u64)>, // (path, size) from HEAD tree
}

impl Default for BloatCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl BloatCollector {
    pub fn new() -> Self {
        Self { files: vec![] }
    }
}

impl MetricCollector for BloatCollector {
    fn name(&self) -> &str {
        "bloat"
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
        // Sort by size descending, take top 30
        self.files.sort_by_key(|f| std::cmp::Reverse(f.1));

        let mut entries: Vec<MetricEntry> = Vec::new();

        for (path, size) in self.files.iter().take(30) {
            let recommendation = classify(path, *size);
            // Skip tiny files unless they match a suspicious pattern
            if *size < LARGE_FILE_THRESHOLD
                && recommendation.code == messages::BLOAT_RECOMMENDATION_OK
            {
                continue;
            }

            let mut values = HashMap::new();
            values.insert("size_bytes".into(), MetricValue::Count(*size));
            values.insert("size_human".into(), MetricValue::Text(human_size(*size)));
            values.insert(
                "recommendation".into(),
                MetricValue::Message(recommendation),
            );
            entries.push(MetricEntry {
                key: path.clone(),
                values,
            });
        }

        // Also scan entire tree for suspicious patterns regardless of size
        for (path, size) in &self.files {
            if entries.iter().any(|e| e.key == *path) {
                continue;
            }
            let rec = classify(path, *size);
            if rec.code != messages::BLOAT_RECOMMENDATION_OK {
                let mut values = HashMap::new();
                values.insert("size_bytes".into(), MetricValue::Count(*size));
                values.insert("size_human".into(), MetricValue::Text(human_size(*size)));
                values.insert("recommendation".into(), MetricValue::Message(rec));
                entries.push(MetricEntry {
                    key: path.clone(),
                    values,
                });
            }
        }

        entries.sort_by(|a, b| {
            let sa = match a.values.get("size_bytes") {
                Some(MetricValue::Count(n)) => *n,
                _ => 0,
            };
            let sb = match b.values.get("size_bytes") {
                Some(MetricValue::Count(n)) => *n,
                _ => 0,
            };
            sb.cmp(&sa)
        });

        MetricResult {
            name: "bloat".into(),
            display_name: report_display("bloat"),
            description: report_description("bloat"),
            entry_groups: vec![],
            columns: vec![
                Column::in_report("bloat", "size_bytes"),
                Column::in_report("bloat", "size_human"),
                Column::in_report("bloat", "recommendation"),
            ],
            entries,
        }
    }
}

fn walk_tree(repo: &gix::Repository, tree: &gix::Tree, prefix: &str, out: &mut Vec<(String, u64)>) {
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
            // Use header to avoid reading the entire blob.
            if let Ok(header) = repo.objects.header(id) {
                let size = header.size();
                // Source files belong in `large_sources`, not bloat — their
                // fix is "split the module", not "rewrite git history".
                // We still catch truly absurd sizes (>20 MB) as likely
                // generated artifacts checked in by accident.
                if is_source_path(&full_path) && size < SOURCE_BLOAT_THRESHOLD {
                    continue;
                }
                out.push((full_path, size));
            }
        }
    }
}

fn classify(path: &str, size: u64) -> LocalizedMessage {
    for (pat, code) in SUSPICIOUS_PATTERNS {
        if path.contains(pat) {
            return LocalizedMessage::code(*code).with_severity(Severity::Warning);
        }
    }
    if size >= 5 * 1024 * 1024 {
        LocalizedMessage::code(messages::BLOAT_RECOMMENDATION_VERY_LARGE_FILE)
            .with_severity(Severity::Warning)
            .with_param("size_bytes", size)
    } else if size >= LARGE_FILE_THRESHOLD {
        LocalizedMessage::code(messages::BLOAT_RECOMMENDATION_LARGE_FILE)
            .with_severity(Severity::Info)
            .with_param("size_bytes", size)
    } else {
        LocalizedMessage::code(messages::BLOAT_RECOMMENDATION_OK)
    }
}

fn human_size(n: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * 1024;
    const GB: u64 = 1024 * 1024 * 1024;
    if n >= GB {
        format!("{:.2} GB", n as f64 / GB as f64)
    } else if n >= MB {
        format!("{:.2} MB", n as f64 / MB as f64)
    } else if n >= KB {
        format!("{:.2} KB", n as f64 / KB as f64)
    } else {
        format!("{} B", n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_human_size() {
        assert_eq!(human_size(100), "100 B");
        assert_eq!(human_size(1500), "1.46 KB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.00 MB");
    }

    #[test]
    fn test_classify() {
        assert_eq!(
            classify("normal.rs", 100).code,
            messages::BLOAT_RECOMMENDATION_OK
        );
        assert_eq!(
            classify("normal.rs", 600 * 1024).code,
            messages::BLOAT_RECOMMENDATION_LARGE_FILE
        );
        assert_eq!(
            classify("big.bin", 10 * 1024 * 1024).code,
            messages::BLOAT_RECOMMENDATION_VERY_LARGE_FILE
        );
        assert_eq!(
            classify("src/app.min.js", 10).code,
            messages::BLOAT_RECOMMENDATION_MINIFIED_BUNDLE
        );
        assert_eq!(
            classify("node_modules/foo", 10).code,
            messages::BLOAT_RECOMMENDATION_VENDORED_DEPS
        );
    }
}
