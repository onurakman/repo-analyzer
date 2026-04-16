use std::collections::{HashMap, HashSet};

use crate::metrics::MetricCollector;
use crate::types::{MetricEntry, MetricResult, MetricValue, ParsedChange};

const MIN_COUPLING_COUNT: u64 = 3;
const MIN_COUPLING_SCORE: f64 = 0.3;

pub struct CouplingCollector {
    /// commit_id -> set of files changed in that commit
    commits: HashMap<String, HashSet<String>>,
    /// file -> total change count
    file_changes: HashMap<String, u64>,
}

impl CouplingCollector {
    pub fn new() -> Self {
        Self {
            commits: HashMap::new(),
            file_changes: HashMap::new(),
        }
    }
}

impl MetricCollector for CouplingCollector {
    fn name(&self) -> &str {
        "coupling"
    }

    fn process(&mut self, change: &ParsedChange) {
        let commit_id = &change.diff.commit.oid;
        let file = &change.diff.file_path;

        self.commits
            .entry(commit_id.clone())
            .or_default()
            .insert(file.clone());

        *self.file_changes.entry(file.clone()).or_insert(0) += 1;
    }

    fn finalize(&mut self) -> MetricResult {
        // Count co-changes for each file pair
        let mut co_changes: HashMap<(String, String), u64> = HashMap::new();

        for files in self.commits.values() {
            let file_list: Vec<&String> = files.iter().collect();
            for i in 0..file_list.len() {
                for j in (i + 1)..file_list.len() {
                    let (a, b) = if file_list[i] < file_list[j] {
                        (file_list[i].clone(), file_list[j].clone())
                    } else {
                        (file_list[j].clone(), file_list[i].clone())
                    };
                    *co_changes.entry((a, b)).or_insert(0) += 1;
                }
            }
        }

        let mut entries: Vec<MetricEntry> = co_changes
            .into_iter()
            .filter_map(|((a, b), count)| {
                if count < MIN_COUPLING_COUNT {
                    return None;
                }

                let changes_a = self.file_changes.get(&a).copied().unwrap_or(1);
                let changes_b = self.file_changes.get(&b).copied().unwrap_or(1);
                let max_changes = changes_a.max(changes_b) as f64;
                let score = count as f64 / max_changes;

                if score < MIN_COUPLING_SCORE {
                    return None;
                }

                let key = format!("{a} <-> {b}");
                let mut values = HashMap::new();
                values.insert("file_a".into(), MetricValue::Text(a));
                values.insert("file_b".into(), MetricValue::Text(b));
                values.insert("co_changes".into(), MetricValue::Count(count));
                values.insert("score".into(), MetricValue::Float(score));

                Some(MetricEntry { key, values })
            })
            .collect();

        entries.sort_by(|a, b| {
            let sa = match a.values.get("score") {
                Some(MetricValue::Float(f)) => *f,
                _ => 0.0,
            };
            let sb = match b.values.get("score") {
                Some(MetricValue::Float(f)) => *f,
                _ => 0.0,
            };
            sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
        });

        MetricResult {
            name: "coupling".into(),
            description: "Temporal coupling between files changed together".into(),
            columns: vec![
                "file_a".into(),
                "file_b".into(),
                "co_changes".into(),
                "score".into(),
            ],
            entries,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{CommitInfo, DiffRecord, FileStatus, ParsedChange};
    use chrono::{FixedOffset, TimeZone};

    fn make_change(commit_id: &str, file: &str) -> ParsedChange {
        let ts = FixedOffset::east_opt(0)
            .unwrap()
            .with_ymd_and_hms(2025, 1, 15, 12, 0, 0)
            .unwrap();
        ParsedChange {
            diff: DiffRecord {
                commit: CommitInfo {
                    oid: commit_id.into(),
                    author: "dev".into(),
                    email: "dev@test.com".into(),
                    timestamp: ts,
                    message: "test".into(),
                    parent_ids: vec![],
                },
                file_path: file.into(),
                old_path: None,
                status: FileStatus::Modified,
                hunks: vec![],
                additions: 5,
                deletions: 2,
            },
            constructs: vec![],
        }
    }

    #[test]
    fn test_coupling_detected() {
        let mut collector = CouplingCollector::new();
        // 4 commits changing a.rs + b.rs together
        for i in 0..4 {
            let cid = format!("commit_{i}");
            collector.process(&make_change(&cid, "a.rs"));
            collector.process(&make_change(&cid, "b.rs"));
        }

        let result = collector.finalize();
        assert_eq!(result.entries.len(), 1);

        let entry = &result.entries[0];
        assert_eq!(entry.key, "a.rs <-> b.rs");

        match entry.values.get("co_changes") {
            Some(MetricValue::Count(n)) => assert_eq!(*n, 4),
            other => panic!("Expected Count(4), got {:?}", other),
        }
        match entry.values.get("score") {
            Some(MetricValue::Float(f)) => assert!((*f - 1.0).abs() < 0.01),
            other => panic!("Expected Float(1.0), got {:?}", other),
        }
    }

    #[test]
    fn test_low_coupling_filtered() {
        let mut collector = CouplingCollector::new();
        // Only 1 co-change — below MIN_COUPLING_COUNT threshold
        collector.process(&make_change("commit_1", "x.rs"));
        collector.process(&make_change("commit_1", "y.rs"));

        let result = collector.finalize();
        assert!(result.entries.is_empty());
    }
}
