use std::collections::HashMap;

use chrono::{NaiveDate, Utc};

use crate::metrics::MetricCollector;
use crate::types::{FileStatus, MetricEntry, MetricResult, MetricValue, ParsedChange};

struct FileAge {
    first_seen: NaiveDate,
    last_modified: NaiveDate,
    change_count: u64,
    deleted: bool,
}

pub struct AgeCollector {
    files: HashMap<String, FileAge>,
}

impl AgeCollector {
    pub fn new() -> Self {
        Self {
            files: HashMap::new(),
        }
    }
}

impl MetricCollector for AgeCollector {
    fn name(&self) -> &str {
        "age"
    }

    fn process(&mut self, change: &ParsedChange) {
        let file = &change.diff.file_path;
        let date = change.diff.commit.timestamp.date_naive();

        let entry = self.files.entry(file.clone()).or_insert(FileAge {
            first_seen: date,
            last_modified: date,
            change_count: 0,
            deleted: false,
        });

        entry.change_count += 1;

        if date < entry.first_seen {
            entry.first_seen = date;
        }
        if date > entry.last_modified {
            entry.last_modified = date;
        }

        if change.diff.status == FileStatus::Deleted {
            entry.deleted = true;
        }
    }

    fn finalize(&mut self) -> MetricResult {
        let today = Utc::now().date_naive();

        let mut entries: Vec<MetricEntry> = self
            .files
            .drain()
            .filter(|(_, age)| !age.deleted)
            .map(|(file, age)| {
                let age_days = (today - age.first_seen).num_days().max(0) as u64;
                let days_since_last_change =
                    (today - age.last_modified).num_days().max(0) as u64;

                let age_years = age_days as f64 / 365.25;
                let changes_per_year = if age_years > 0.0 {
                    age.change_count as f64 / age_years
                } else {
                    age.change_count as f64
                };

                let mut values = HashMap::new();
                values.insert("age_days".into(), MetricValue::Count(age_days));
                values.insert("first_seen".into(), MetricValue::Date(age.first_seen));
                values.insert("last_modified".into(), MetricValue::Date(age.last_modified));
                values.insert(
                    "days_since_last_change".into(),
                    MetricValue::Count(days_since_last_change),
                );
                values.insert("change_count".into(), MetricValue::Count(age.change_count));
                values.insert("changes_per_year".into(), MetricValue::Float(changes_per_year));

                MetricEntry { key: file, values }
            })
            .collect();

        entries.sort_by(|a, b| {
            let sa = match a.values.get("changes_per_year") {
                Some(MetricValue::Float(f)) => *f,
                _ => 0.0,
            };
            let sb = match b.values.get("changes_per_year") {
                Some(MetricValue::Float(f)) => *f,
                _ => 0.0,
            };
            sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
        });

        MetricResult {
            name: "age".into(),
            description: "File age and change frequency analysis".into(),
            columns: vec![
                "age_days".into(),
                "first_seen".into(),
                "last_modified".into(),
                "days_since_last_change".into(),
                "change_count".into(),
                "changes_per_year".into(),
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

    fn make_change(file: &str, status: FileStatus, year: i32, month: u32, day: u32) -> ParsedChange {
        let ts = FixedOffset::east_opt(0)
            .unwrap()
            .with_ymd_and_hms(year, month, day, 12, 0, 0)
            .unwrap();
        ParsedChange {
            diff: DiffRecord {
                commit: CommitInfo {
                    oid: format!("commit_{}_{}_{}_{}", file, year, month, day),
                    author: "dev".into(),
                    email: "dev@test.com".into(),
                    timestamp: ts,
                    message: "test".into(),
                    parent_ids: vec![],
                },
                file_path: file.into(),
                old_path: None,
                status,
                hunks: vec![],
                additions: 5,
                deletions: 2,
            },
            constructs: vec![],
        }
    }

    #[test]
    fn test_age_tracking() {
        let mut collector = AgeCollector::new();
        collector.process(&make_change("lib.rs", FileStatus::Added, 2024, 1, 1));
        collector.process(&make_change("lib.rs", FileStatus::Modified, 2024, 6, 15));

        let result = collector.finalize();
        assert_eq!(result.entries.len(), 1);

        let entry = &result.entries[0];
        assert_eq!(entry.key, "lib.rs");

        match entry.values.get("first_seen") {
            Some(MetricValue::Date(d)) => {
                assert_eq!(*d, NaiveDate::from_ymd_opt(2024, 1, 1).unwrap());
            }
            other => panic!("Expected Date(2024-01-01), got {:?}", other),
        }
        match entry.values.get("last_modified") {
            Some(MetricValue::Date(d)) => {
                assert_eq!(*d, NaiveDate::from_ymd_opt(2024, 6, 15).unwrap());
            }
            other => panic!("Expected Date(2024-06-15), got {:?}", other),
        }
        match entry.values.get("change_count") {
            Some(MetricValue::Count(n)) => assert_eq!(*n, 2),
            other => panic!("Expected Count(2), got {:?}", other),
        }
    }

    #[test]
    fn test_deleted_files_skipped() {
        let mut collector = AgeCollector::new();
        collector.process(&make_change("alive.rs", FileStatus::Added, 2024, 1, 1));
        collector.process(&make_change("dead.rs", FileStatus::Added, 2024, 1, 1));
        collector.process(&make_change("dead.rs", FileStatus::Deleted, 2024, 6, 1));

        let result = collector.finalize();
        assert_eq!(result.entries.len(), 1);
        assert_eq!(result.entries[0].key, "alive.rs");
    }
}
