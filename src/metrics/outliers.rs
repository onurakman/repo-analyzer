use std::collections::{HashMap, HashSet};

use crate::metrics::MetricCollector;
use crate::types::{FileStatus, MetricEntry, MetricResult, MetricValue, ParsedChange};

struct OutlierData {
    change_count: u64,
    authors: HashSet<String>,
    deleted: bool,
    total_churn: u64,
}

pub struct OutliersCollector {
    files: HashMap<String, OutlierData>,
}

impl Default for OutliersCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl OutliersCollector {
    pub fn new() -> Self {
        Self {
            files: HashMap::new(),
        }
    }
}

const HIGH_CHURN_THRESHOLD: u64 = 100;
const HIGH_AUTHORS_THRESHOLD: usize = 5;

impl MetricCollector for OutliersCollector {
    fn name(&self) -> &str {
        "outliers"
    }

    fn process(&mut self, change: &ParsedChange) {
        let file = &change.diff.file_path;
        let entry = self.files.entry(file.clone()).or_insert(OutlierData {
            change_count: 0,
            authors: HashSet::new(),
            deleted: false,
            total_churn: 0,
        });
        entry.change_count += 1;
        entry.authors.insert(change.diff.commit.email.clone());
        entry.total_churn += change.diff.additions as u64 + change.diff.deletions as u64;
        if change.diff.status == FileStatus::Deleted {
            entry.deleted = true;
        }
    }

    fn finalize(&mut self) -> MetricResult {
        let mut entries: Vec<MetricEntry> = self
            .files
            .drain()
            .filter(|(_, d)| {
                !d.deleted
                    && (d.change_count >= HIGH_CHURN_THRESHOLD
                        || d.authors.len() >= HIGH_AUTHORS_THRESHOLD)
            })
            .map(|(file, d)| {
                let author_count = d.authors.len();
                let recommendation = build_recommendation(d.change_count, author_count);

                let mut values = HashMap::new();
                values.insert("change_count".into(), MetricValue::Count(d.change_count));
                values.insert(
                    "unique_authors".into(),
                    MetricValue::Count(author_count as u64),
                );
                values.insert("total_churn".into(), MetricValue::Count(d.total_churn));
                values.insert("recommendation".into(), MetricValue::Text(recommendation));

                MetricEntry { key: file, values }
            })
            .collect();

        entries.sort_by(|a, b| {
            let ca = match a.values.get("change_count") {
                Some(MetricValue::Count(n)) => *n,
                _ => 0,
            };
            let cb = match b.values.get("change_count") {
                Some(MetricValue::Count(n)) => *n,
                _ => 0,
            };
            cb.cmp(&ca)
        });

        MetricResult {
            name: "outliers".into(),
            description:
                "Files that are change hotspots or have diffuse ownership — refactor candidates"
                    .into(),
            entry_groups: vec![],
            columns: vec![
                "change_count".into(),
                "unique_authors".into(),
                "total_churn".into(),
                "recommendation".into(),
            ],
            entries,
        }
    }
}

fn build_recommendation(changes: u64, authors: usize) -> String {
    let high_churn = changes >= HIGH_CHURN_THRESHOLD;
    let high_authors = authors >= HIGH_AUTHORS_THRESHOLD;
    match (high_churn, high_authors) {
        (true, true) => "God file + ownership chaos — split responsibilities".into(),
        (true, false) => "High churn — consider refactoring for stability".into(),
        (false, true) => "Diffuse ownership — clarify module owner".into(),
        (false, false) => "OK".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{CommitInfo, DiffRecord, FileStatus};
    use chrono::{FixedOffset, TimeZone};

    fn make_change(file: &str, email: &str) -> ParsedChange {
        let ts = FixedOffset::east_opt(0)
            .unwrap()
            .with_ymd_and_hms(2025, 1, 1, 12, 0, 0)
            .unwrap();
        ParsedChange {
            diff: DiffRecord {
                commit: CommitInfo {
                    oid: format!("oid_{email}_{file}"),
                    author: email.into(),
                    email: email.into(),
                    timestamp: ts,
                    message: "x".into(),
                    parent_ids: vec![],
                },
                file_path: file.into(),
                old_path: None,
                status: FileStatus::Modified,
                hunks: vec![],
                additions: 1,
                deletions: 1,
            },
            constructs: vec![],
        }
    }

    #[test]
    fn test_outlier_detection_by_churn() {
        let mut c = OutliersCollector::new();
        for i in 0..101 {
            let mut ch = make_change("busy.rs", "a@x");
            ch.diff.commit.oid = format!("oid_{i}");
            c.process(&ch);
        }
        c.process(&make_change("calm.rs", "a@x"));

        let r = c.finalize();
        assert_eq!(r.entries.len(), 1);
        assert_eq!(r.entries[0].key, "busy.rs");
    }

    #[test]
    fn test_outlier_detection_by_authors() {
        let mut c = OutliersCollector::new();
        for e in ["a@x", "b@x", "c@x", "d@x", "e@x"] {
            c.process(&make_change("shared.rs", e));
        }
        c.process(&make_change("owned.rs", "a@x"));

        let r = c.finalize();
        assert_eq!(r.entries.len(), 1);
        assert_eq!(r.entries[0].key, "shared.rs");
    }
}
