use std::collections::{HashMap, HashSet};

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

/// Upper bound on the number of rows this report emits. Vendored trees can
/// contain tens of thousands of files; without a cap the suspicious-pattern
/// scan produced unbounded output. Directory aggregation collapses most of
/// that, and this cap guards the rest.
const MAX_ENTRIES: usize = 100;

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
            // Vendored/build directory trees are aggregated below into one
            // row per root, so skip their individual files here.
            if matches!(suspicious(path), Some(Suspicion::Directory { .. })) {
                continue;
            }
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

        // Also scan the entire tree for suspicious patterns regardless of
        // size. Directory-type patterns (node_modules/, dist/, ...) are
        // aggregated by their vendored root so a huge tree yields ONE row per
        // root instead of one row per file. Individual suspicious/large files
        // are deduped by path. This whole pass is linear in the file count.
        let existing: HashSet<&str> = entries.iter().map(|e| e.key.as_str()).collect();
        // root -> (summed size, recommendation code), with insertion order kept
        // separately so output is deterministic before the size sort below.
        let mut dir_totals: HashMap<String, (u64, &'static str)> = HashMap::new();
        let mut dir_order: Vec<String> = Vec::new();
        let mut file_seen: HashSet<String> = HashSet::new();
        let mut file_rows: Vec<(String, u64, LocalizedMessage)> = Vec::new();

        for (path, size) in &self.files {
            if existing.contains(path.as_str()) {
                continue;
            }
            if let Some(Suspicion::Directory { root, code }) = suspicious(path) {
                let total = dir_totals.entry(root.clone()).or_insert_with(|| {
                    dir_order.push(root);
                    (0, code)
                });
                total.0 = total.0.saturating_add(*size);
                continue;
            }
            let rec = classify(path, *size);
            if rec.code == messages::BLOAT_RECOMMENDATION_OK {
                continue;
            }
            if file_seen.insert(path.clone()) {
                file_rows.push((path.clone(), *size, rec));
            }
        }

        for root in dir_order {
            let (size, code) = dir_totals[&root];
            let rec = LocalizedMessage::code(code).with_severity(Severity::Warning);
            let mut values = HashMap::new();
            values.insert("size_bytes".into(), MetricValue::Count(size));
            values.insert("size_human".into(), MetricValue::Text(human_size(size)));
            values.insert("recommendation".into(), MetricValue::Message(rec));
            entries.push(MetricEntry { key: root, values });
        }
        for (path, size, rec) in file_rows {
            let mut values = HashMap::new();
            values.insert("size_bytes".into(), MetricValue::Count(size));
            values.insert("size_human".into(), MetricValue::Text(human_size(size)));
            values.insert("recommendation".into(), MetricValue::Message(rec));
            entries.push(MetricEntry { key: path, values });
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

        entries.truncate(MAX_ENTRIES);

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

/// How a path matched a [`SUSPICIOUS_PATTERNS`] entry.
enum Suspicion {
    /// A vendored/build directory tree, identified by its `root` (the path up
    /// to and including the matched segment). Rows for these are aggregated so
    /// the whole tree collapses to a single entry.
    Directory { root: String, code: &'static str },
    /// A standalone suspicious file (e.g. `*.min.js`, `.DS_Store`).
    File { code: &'static str },
}

/// Match a path against the suspicious patterns, anchored to path-segment
/// boundaries: `node_modules/` matches the segment `node_modules`, never a
/// substring like `mynode_modules_backup`. Directory patterns take precedence
/// so an entire vendored tree collapses to one root even when its files also
/// match a file pattern (e.g. `node_modules/app.min.js`).
fn suspicious(path: &str) -> Option<Suspicion> {
    for (pat, code) in SUSPICIOUS_PATTERNS {
        if let Some(seg) = pat.strip_suffix('/') {
            let mut root = String::new();
            for part in path.split('/') {
                if !root.is_empty() {
                    root.push('/');
                }
                root.push_str(part);
                if part == seg {
                    return Some(Suspicion::Directory { root, code });
                }
            }
        }
    }
    let filename = path.rsplit('/').next().unwrap_or(path);
    for (pat, code) in SUSPICIOUS_PATTERNS {
        if !pat.ends_with('/') && filename.ends_with(pat) {
            return Some(Suspicion::File { code });
        }
    }
    None
}

fn classify(path: &str, size: u64) -> LocalizedMessage {
    if let Some(susp) = suspicious(path) {
        let code = match susp {
            Suspicion::Directory { code, .. } | Suspicion::File { code } => code,
        };
        return LocalizedMessage::code(code).with_severity(Severity::Warning);
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

    #[test]
    fn test_suspicious_is_segment_anchored() {
        // Substring, not a real path segment -> no match.
        assert!(suspicious("src/mynode_modules_backup/x").is_none());
        // Real segment -> Directory match with the correct root.
        match suspicious("frontend/node_modules/a/b.js") {
            Some(Suspicion::Directory { root, .. }) => {
                assert_eq!(root, "frontend/node_modules");
            }
            _ => panic!("expected Directory match"),
        }
    }

    fn size_of(entry: &MetricEntry) -> u64 {
        match entry.values.get("size_bytes") {
            Some(MetricValue::Count(n)) => *n,
            _ => 0,
        }
    }

    #[test]
    fn test_directory_aggregation() {
        let mut c = BloatCollector::new();
        // Two distinct vendored roots plus a build tree, many files each.
        c.files = vec![
            ("node_modules/a/x.js".into(), 100),
            ("node_modules/b/y.js".into(), 200),
            ("node_modules/c/z.js".into(), 300),
            ("frontend/node_modules/p.js".into(), 40),
            ("frontend/node_modules/q.js".into(), 60),
            ("target/debug/app".into(), 1000),
            ("target/debug/dep".into(), 500),
            ("src/main.rs".into(), 10),
        ];

        let result = c.finalize();

        // One row per vendored/build root, none per file.
        let roots: Vec<&str> = result.entries.iter().map(|e| e.key.as_str()).collect();
        assert!(roots.contains(&"node_modules"), "roots = {:?}", roots);
        assert!(
            roots.contains(&"frontend/node_modules"),
            "roots = {:?}",
            roots
        );
        assert!(roots.contains(&"target"), "roots = {:?}", roots);
        // No individual file under an aggregated root leaks through.
        assert!(
            !roots.iter().any(|k| k.starts_with("node_modules/")
                || k.starts_with("frontend/node_modules/")
                || k.starts_with("target/")),
            "roots = {:?}",
            roots
        );

        let nm = result
            .entries
            .iter()
            .find(|e| e.key == "node_modules")
            .expect("node_modules root");
        assert_eq!(size_of(nm), 600); // 100 + 200 + 300

        let tgt = result
            .entries
            .iter()
            .find(|e| e.key == "target")
            .expect("target root");
        assert_eq!(size_of(tgt), 1500); // 1000 + 500

        // Columns/schema are unchanged.
        assert_eq!(result.columns.len(), 3);
    }

    #[test]
    fn test_max_entries_cap() {
        let mut c = BloatCollector::new();
        // Far more suspicious files than the cap: distinct .DS_Store paths.
        c.files = (0..(MAX_ENTRIES + 50))
            .map(|i| (format!("dir{i}/.DS_Store"), 10u64))
            .collect();

        let result = c.finalize();
        assert!(result.entries.len() <= MAX_ENTRIES);
    }
}
