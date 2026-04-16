use std::collections::HashMap;

use crate::metrics::MetricCollector;
use crate::types::{MetricEntry, MetricResult, MetricValue, ParsedChange};

struct ChurnData {
    lines_added: u64,
    lines_deleted: u64,
    change_count: u64,
}

pub struct ChurnCollector {
    files: HashMap<String, ChurnData>,
}

impl ChurnCollector {
    pub fn new() -> Self {
        Self {
            files: HashMap::new(),
        }
    }
}

impl MetricCollector for ChurnCollector {
    fn name(&self) -> &str {
        "churn"
    }

    fn process(&mut self, change: &ParsedChange) {
        let file = &change.diff.file_path;
        let entry = self.files.entry(file.clone()).or_insert(ChurnData {
            lines_added: 0,
            lines_deleted: 0,
            change_count: 0,
        });
        entry.lines_added += change.diff.additions as u64;
        entry.lines_deleted += change.diff.deletions as u64;
        entry.change_count += 1;
    }

    fn finalize(&mut self) -> MetricResult {
        let mut entries: Vec<MetricEntry> = self
            .files
            .drain()
            .map(|(file, data)| {
                let total_churn = data.lines_added + data.lines_deleted;
                let net_change = data.lines_added as i64 - data.lines_deleted as i64;
                let churn_rate = if data.change_count > 0 {
                    total_churn as f64 / data.change_count as f64
                } else {
                    0.0
                };

                let mut values = HashMap::new();
                values.insert("lines_added".into(), MetricValue::Count(data.lines_added));
                values.insert("lines_deleted".into(), MetricValue::Count(data.lines_deleted));
                values.insert("net_change".into(), MetricValue::Count(net_change as u64));
                values.insert("total_churn".into(), MetricValue::Count(total_churn));
                values.insert("change_count".into(), MetricValue::Count(data.change_count));
                values.insert("churn_rate".into(), MetricValue::Float(churn_rate));

                MetricEntry { key: file, values }
            })
            .collect();

        entries.sort_by(|a, b| {
            let ca = match a.values.get("total_churn") {
                Some(MetricValue::Count(n)) => *n,
                _ => 0,
            };
            let cb = match b.values.get("total_churn") {
                Some(MetricValue::Count(n)) => *n,
                _ => 0,
            };
            cb.cmp(&ca)
        });

        MetricResult {
            name: "churn".into(),
            description: "File-level code churn statistics".into(),
            columns: vec![
                "lines_added".into(),
                "lines_deleted".into(),
                "net_change".into(),
                "total_churn".into(),
                "change_count".into(),
                "churn_rate".into(),
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

    fn make_change(file: &str, added: u32, deleted: u32) -> ParsedChange {
        let ts = FixedOffset::east_opt(0)
            .unwrap()
            .with_ymd_and_hms(2025, 1, 15, 12, 0, 0)
            .unwrap();
        ParsedChange {
            diff: DiffRecord {
                commit: CommitInfo {
                    oid: "abc123".into(),
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
                additions: added,
                deletions: deleted,
            },
            constructs: vec![],
        }
    }

    #[test]
    fn test_churn_accumulation() {
        let mut collector = ChurnCollector::new();
        collector.process(&make_change("a.rs", 10, 3));
        collector.process(&make_change("a.rs", 5, 2));

        let result = collector.finalize();
        let entry = result.entries.iter().find(|e| e.key == "a.rs").unwrap();

        match entry.values.get("lines_added") {
            Some(MetricValue::Count(n)) => assert_eq!(*n, 15),
            other => panic!("Expected Count(15), got {:?}", other),
        }
        match entry.values.get("lines_deleted") {
            Some(MetricValue::Count(n)) => assert_eq!(*n, 5),
            other => panic!("Expected Count(5), got {:?}", other),
        }
        match entry.values.get("total_churn") {
            Some(MetricValue::Count(n)) => assert_eq!(*n, 20),
            other => panic!("Expected Count(20), got {:?}", other),
        }
        match entry.values.get("change_count") {
            Some(MetricValue::Count(n)) => assert_eq!(*n, 2),
            other => panic!("Expected Count(2), got {:?}", other),
        }
    }

    #[test]
    fn test_sorted_by_total_churn() {
        let mut collector = ChurnCollector::new();
        collector.process(&make_change("small.rs", 2, 1)); // churn=3
        collector.process(&make_change("big.rs", 100, 50)); // churn=150

        let result = collector.finalize();
        assert_eq!(result.entries[0].key, "big.rs");
        assert_eq!(result.entries[1].key, "small.rs");
    }
}
