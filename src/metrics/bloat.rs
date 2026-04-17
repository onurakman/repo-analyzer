use std::collections::HashMap;

use gix::prelude::HeaderExt;

use crate::metrics::MetricCollector;
use crate::types::{MetricEntry, MetricResult, MetricValue, ParsedChange};

/// Patterns for files that are commonly committed by mistake.
const SUSPICIOUS_PATTERNS: &[(&str, &str)] = &[
    (".min.js", "Minified bundle — should be build artifact"),
    (".min.css", "Minified bundle — should be build artifact"),
    ("node_modules/", "Vendored dependencies — add to .gitignore"),
    ("dist/", "Build output — should be generated, not committed"),
    (
        "build/",
        "Build output — should be generated, not committed",
    ),
    ("target/", "Rust build output — should be in .gitignore"),
    ("vendor/", "Vendored deps — review if needed in repo"),
    (".DS_Store", "OS metadata — add to global gitignore"),
    (".idea/", "IDE config — usually not checked in"),
    (".vscode/", "IDE config — usually not checked in"),
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

    fn inspect_repo(&mut self, repo: &gix::Repository) -> anyhow::Result<()> {
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
        self.files.sort_by(|a, b| b.1.cmp(&a.1));

        let mut entries: Vec<MetricEntry> = Vec::new();

        for (path, size) in self.files.iter().take(30) {
            let recommendation = classify(path, *size);
            // Skip tiny files unless they match a suspicious pattern
            if *size < LARGE_FILE_THRESHOLD && recommendation == "OK" {
                continue;
            }

            let mut values = HashMap::new();
            values.insert("size_bytes".into(), MetricValue::Count(*size));
            values.insert("size_human".into(), MetricValue::Text(human_size(*size)));
            values.insert(
                "recommendation".into(),
                MetricValue::Text(recommendation.into()),
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
            if rec != "OK" {
                let mut values = HashMap::new();
                values.insert("size_bytes".into(), MetricValue::Count(*size));
                values.insert("size_human".into(), MetricValue::Text(human_size(*size)));
                values.insert("recommendation".into(), MetricValue::Text(rec.into()));
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
            description: "Large files and suspicious committed artifacts in HEAD".into(),
            entry_groups: vec![],
            columns: vec![
                "size_bytes".into(),
                "size_human".into(),
                "recommendation".into(),
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
            // Use header to avoid reading the entire blob
            if let Ok(header) = repo.objects.header(id) {
                out.push((full_path, header.size()));
            }
        }
    }
}

fn classify(path: &str, size: u64) -> &'static str {
    for (pat, rec) in SUSPICIOUS_PATTERNS {
        if path.contains(pat) {
            return rec;
        }
    }
    if size >= 5 * 1024 * 1024 {
        "Very large — investigate; use Git LFS if needed"
    } else if size >= LARGE_FILE_THRESHOLD {
        "Large — verify intentional, consider LFS"
    } else {
        "OK"
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
        assert_eq!(classify("normal.rs", 100), "OK");
        assert_eq!(
            classify("normal.rs", 600 * 1024),
            "Large — verify intentional, consider LFS"
        );
        assert_eq!(
            classify("big.bin", 10 * 1024 * 1024),
            "Very large — investigate; use Git LFS if needed"
        );
        assert!(classify("src/app.min.js", 10).contains("Minified"));
        assert!(classify("node_modules/foo", 10).contains("Vendored"));
    }
}
