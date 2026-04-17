use std::collections::HashMap;

use chrono::{Duration, Utc};
use gix::bstr::BStr;

use crate::metrics::MetricCollector;
use crate::types::{MetricEntry, MetricResult, MetricValue, ParsedChange};

/// Files larger than this (in bytes) are skipped — blame on huge files is
/// extremely expensive, often allocating GBs per file in long histories.
const MAX_BLOB_BYTES: u64 = 50 * 1024;

/// Maximum number of files to blame. gix blame on a single file in a long-history
/// repository can consume hundreds of MB; capping at 50 keeps the metric usable
/// on huge codebases without OOM.
const MAX_FILES: usize = 50;

/// Number of months considered "ancient" — lines older than this count as surviving original code.
const ANCIENT_MONTHS: i64 = 6;

const SOURCE_EXTENSIONS: &[&str] = &[
    "rs", "py", "pyi", "ts", "tsx", "js", "jsx", "java", "go", "cpp", "cc", "cxx", "hpp", "h",
    "cs", "kt", "kts", "php", "rb", "scala", "sc", "swift", "sh", "bash",
];

struct FileHalfLife {
    total_lines: u64,
    ancient_lines: u64,
    oldest_age_days: i64,
}

pub struct HalfLifeCollector {
    files: HashMap<String, FileHalfLife>,
}

impl Default for HalfLifeCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl HalfLifeCollector {
    pub fn new() -> Self {
        Self {
            files: HashMap::new(),
        }
    }
}

impl MetricCollector for HalfLifeCollector {
    fn name(&self) -> &str {
        "half_life"
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
        let head_id = head_commit.id;
        let tree = head_commit.tree()?;

        let cutoff_secs = (Utc::now() - Duration::days(ANCIENT_MONTHS * 30)).timestamp();
        let now_secs = Utc::now().timestamp();

        let mut paths: Vec<(String, u64)> = vec![];
        collect_source_blobs(repo, &tree, "", &mut paths);

        // Cap files to bound memory: blame is O(history size × file size) per file
        // and gix-blame can momentarily hold the entire file content + diff state
        // for every parent. 50 files × ~200 MB peak = a manageable ceiling.
        paths.sort_by_key(|(p, _)| p.clone());
        paths.truncate(MAX_FILES);

        // Stop blame from walking past the ancient cutoff — anything older than
        // that already counts as "ancient" wholesale, and the deeper traversal
        // is the dominant memory cost.
        let blame_since = gix::date::Time::new(cutoff_secs, 0);

        let total_files = paths.len();
        for (idx, (path, _size)) in paths.into_iter().enumerate() {
            progress.status(&format!(
                "  half_life: blame {}/{total_files} {path}...",
                idx + 1
            ));
            let opts = gix::repository::blame_file::Options {
                since: Some(blame_since),
                ..Default::default()
            };
            let outcome = match repo.blame_file(BStr::new(path.as_bytes()), head_id, opts) {
                Ok(o) => o,
                Err(_) => continue,
            };

            let mut total_lines: u64 = 0;
            let mut ancient_lines: u64 = 0;
            let mut oldest_secs: i64 = now_secs;

            for entry in &outcome.entries {
                let len = entry.len.get() as u64;
                total_lines += len;

                let commit_time = match commit_timestamp(repo, entry.commit_id) {
                    Some(t) => t,
                    None => continue,
                };
                if commit_time < oldest_secs {
                    oldest_secs = commit_time;
                }
                if commit_time <= cutoff_secs {
                    ancient_lines += len;
                }
            }

            if total_lines == 0 {
                continue;
            }

            let oldest_age_days = ((now_secs - oldest_secs).max(0)) / 86_400;

            self.files.insert(
                path,
                FileHalfLife {
                    total_lines,
                    ancient_lines,
                    oldest_age_days,
                },
            );
        }

        Ok(())
    }

    fn finalize(&mut self) -> MetricResult {
        let mut entries: Vec<MetricEntry> = self
            .files
            .drain()
            .map(|(path, f)| {
                let pct = f
                    .ancient_lines
                    .saturating_mul(100)
                    .checked_div(f.total_lines)
                    .unwrap_or(0)
                    .min(100);
                let recommendation = classify(pct);
                let mut values = HashMap::new();
                values.insert("total_lines".into(), MetricValue::Count(f.total_lines));
                values.insert("ancient_lines".into(), MetricValue::Count(f.ancient_lines));
                values.insert("ancient_pct".into(), MetricValue::Count(pct));
                values.insert(
                    "oldest_age_days".into(),
                    MetricValue::Count(f.oldest_age_days as u64),
                );
                values.insert(
                    "recommendation".into(),
                    MetricValue::Text(recommendation.into()),
                );
                MetricEntry { key: path, values }
            })
            .collect();

        // Sort by ancient_pct ascending (hot zones first — most actionable)
        entries.sort_by(|a, b| {
            let pa = match a.values.get("ancient_pct") {
                Some(MetricValue::Count(n)) => *n,
                _ => 0,
            };
            let pb = match b.values.get("ancient_pct") {
                Some(MetricValue::Count(n)) => *n,
                _ => 0,
            };
            pa.cmp(&pb)
        });

        MetricResult {
            name: "half_life".into(),
            display_name: "Code Half-Life".into(),
            description: format!(
                "Per-file code longevity: what fraction of the current code was written more than {ANCIENT_MONTHS} months ago. High percentage = stable, mature code that has stood the test of time. Low percentage = file is constantly rewritten, handle changes here with extra care."
            ),
            entry_groups: vec![],
            column_labels: vec![],
            columns: vec![
                "total_lines".into(),
                "ancient_lines".into(),
                "ancient_pct".into(),
                "oldest_age_days".into(),
                "recommendation".into(),
            ],
            entries,
        }
    }
}

fn classify(pct: u64) -> &'static str {
    match pct {
        0..=20 => "Hot zone — frequent rewrites",
        21..=50 => "Aging steadily",
        51..=80 => "Mostly stable",
        _ => "Stable core",
    }
}

fn commit_timestamp(repo: &gix::Repository, oid: gix::ObjectId) -> Option<i64> {
    let object = repo.find_object(oid).ok()?;
    let commit = object.try_into_commit().ok()?;
    let author = commit.author().ok()?;
    Some(author.time().ok()?.seconds)
}

fn collect_source_blobs(
    repo: &gix::Repository,
    tree: &gix::Tree,
    prefix: &str,
    out: &mut Vec<(String, u64)>,
) {
    use gix::prelude::HeaderExt;

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
                collect_source_blobs(repo, &subtree, &full_path, out);
            }
        } else if mode.is_blob()
            && is_source(&full_path)
            && let Ok(header) = repo.objects.header(id)
            && header.size() <= MAX_BLOB_BYTES
        {
            out.push((full_path, header.size()));
        }
    }
}

fn is_source(path: &str) -> bool {
    let ext = match path.rsplit('.').next() {
        Some(e) => e,
        None => return false,
    };
    SOURCE_EXTENSIONS.contains(&ext)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_thresholds() {
        assert_eq!(classify(0), "Hot zone — frequent rewrites");
        assert_eq!(classify(15), "Hot zone — frequent rewrites");
        assert_eq!(classify(35), "Aging steadily");
        assert_eq!(classify(70), "Mostly stable");
        assert_eq!(classify(95), "Stable core");
    }

    #[test]
    fn is_source_recognizes_rust() {
        assert!(is_source("src/main.rs"));
        assert!(is_source("foo.py"));
        assert!(!is_source("README.md"));
        assert!(!is_source("data.json"));
    }

    #[test]
    fn classify_boundaries_exact() {
        // boundary exactly on inclusive end of "Hot zone" (20)
        assert_eq!(classify(20), "Hot zone — frequent rewrites");
        assert_eq!(classify(21), "Aging steadily");
        assert_eq!(classify(50), "Aging steadily");
        assert_eq!(classify(51), "Mostly stable");
        assert_eq!(classify(80), "Mostly stable");
        assert_eq!(classify(81), "Stable core");
    }

    #[test]
    fn is_source_handles_extensionless_files() {
        assert!(!is_source("Makefile"));
        assert!(!is_source("LICENSE"));
        assert!(!is_source("path/without/dot"));
    }

    #[test]
    fn is_source_handles_multi_dot_files() {
        // Should pick the LAST extension after final dot.
        assert!(is_source("foo.bar.rs"));
        assert!(!is_source("foo.rs.bak"));
    }
}
