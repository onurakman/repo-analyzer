use std::collections::{HashMap, HashSet};

use chrono::NaiveDate;

use crate::metrics::MetricCollector;
use crate::types::{MetricEntry, MetricResult, MetricValue, ParsedChange};

struct AuthorStats {
    commits: u64,
    lines_added: u64,
    lines_deleted: u64,
    active_days: HashSet<String>,
    first_commit: NaiveDate,
    last_commit: NaiveDate,
}

pub struct AuthorsCollector {
    authors: HashMap<String, AuthorStats>,
}

impl Default for AuthorsCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl AuthorsCollector {
    pub fn new() -> Self {
        Self {
            authors: HashMap::new(),
        }
    }
}

impl MetricCollector for AuthorsCollector {
    fn name(&self) -> &str {
        "authors"
    }

    fn process(&mut self, change: &ParsedChange) {
        let commit = &change.diff.commit;
        let email = &commit.email;
        let date = commit.timestamp.date_naive();
        let day_str = date.format("%Y-%m-%d").to_string();

        let stats = self
            .authors
            .entry(email.clone())
            .or_insert_with(|| AuthorStats {
                commits: 0,
                lines_added: 0,
                lines_deleted: 0,
                active_days: HashSet::new(),
                first_commit: date,
                last_commit: date,
            });

        stats.commits += 1;
        stats.lines_added += change.diff.additions as u64;
        stats.lines_deleted += change.diff.deletions as u64;
        stats.active_days.insert(day_str);

        if date < stats.first_commit {
            stats.first_commit = date;
        }
        if date > stats.last_commit {
            stats.last_commit = date;
        }
    }

    fn finalize(&mut self) -> MetricResult {
        let mut entries: Vec<MetricEntry> = self
            .authors
            .drain()
            .map(|(email, stats)| {
                let mut values = HashMap::new();
                values.insert("commits".into(), MetricValue::Count(stats.commits));
                values.insert("lines_added".into(), MetricValue::Count(stats.lines_added));
                values.insert(
                    "lines_deleted".into(),
                    MetricValue::Count(stats.lines_deleted),
                );
                values.insert(
                    "active_days".into(),
                    MetricValue::Count(stats.active_days.len() as u64),
                );
                values.insert("first_commit".into(), MetricValue::Date(stats.first_commit));
                values.insert("last_commit".into(), MetricValue::Date(stats.last_commit));
                MetricEntry {
                    key: email,
                    values,
                }
            })
            .collect();

        entries.sort_by(|a, b| {
            let ca = match a.values.get("commits") {
                Some(MetricValue::Count(n)) => *n,
                _ => 0,
            };
            let cb = match b.values.get("commits") {
                Some(MetricValue::Count(n)) => *n,
                _ => 0,
            };
            cb.cmp(&ca)
        });

        MetricResult {
            name: "authors".into(),
            description: "Per-author contribution statistics".into(),
            columns: vec![
                "commits".into(),
                "lines_added".into(),
                "lines_deleted".into(),
                "active_days".into(),
                "first_commit".into(),
                "last_commit".into(),
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

    fn make_change(author: &str, file: &str, added: u32, deleted: u32) -> ParsedChange {
        let ts = FixedOffset::east_opt(0)
            .unwrap()
            .with_ymd_and_hms(2025, 1, 15, 12, 0, 0)
            .unwrap();
        ParsedChange {
            diff: DiffRecord {
                commit: CommitInfo {
                    oid: "abc123".into(),
                    author: author.into(),
                    email: format!("{author}@test.com"),
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
    fn test_author_commit_count() {
        let mut collector = AuthorsCollector::new();
        collector.process(&make_change("Alice", "a.rs", 10, 2));
        collector.process(&make_change("Alice", "b.rs", 5, 1));
        collector.process(&make_change("Bob", "a.rs", 3, 0));

        let result = collector.finalize();
        assert_eq!(result.entries.len(), 2);

        let alice = result.entries.iter().find(|e| e.key == "Alice@test.com").unwrap();
        match alice.values.get("commits") {
            Some(MetricValue::Count(n)) => assert_eq!(*n, 2),
            other => panic!("Expected Count(2), got {:?}", other),
        }
        match alice.values.get("lines_added") {
            Some(MetricValue::Count(n)) => assert_eq!(*n, 15),
            other => panic!("Expected Count(15), got {:?}", other),
        }

        let bob = result.entries.iter().find(|e| e.key == "Bob@test.com").unwrap();
        match bob.values.get("commits") {
            Some(MetricValue::Count(n)) => assert_eq!(*n, 1),
            other => panic!("Expected Count(1), got {:?}", other),
        }
    }

    #[test]
    fn test_groups_by_email() {
        let mut collector = AuthorsCollector::new();

        let ts = FixedOffset::east_opt(0)
            .unwrap()
            .with_ymd_and_hms(2025, 1, 15, 12, 0, 0)
            .unwrap();

        // Same email, different author names
        collector.process(&ParsedChange {
            diff: DiffRecord {
                commit: CommitInfo {
                    oid: "c1".into(),
                    author: "Alice".into(),
                    email: "alice@test.com".into(),
                    timestamp: ts,
                    message: "test".into(),
                    parent_ids: vec![],
                },
                file_path: "a.rs".into(),
                old_path: None,
                status: FileStatus::Modified,
                hunks: vec![],
                additions: 10,
                deletions: 0,
            },
            constructs: vec![],
        });
        collector.process(&ParsedChange {
            diff: DiffRecord {
                commit: CommitInfo {
                    oid: "c2".into(),
                    author: "alice".into(),
                    email: "alice@test.com".into(),
                    timestamp: ts,
                    message: "test".into(),
                    parent_ids: vec![],
                },
                file_path: "b.rs".into(),
                old_path: None,
                status: FileStatus::Modified,
                hunks: vec![],
                additions: 5,
                deletions: 0,
            },
            constructs: vec![],
        });

        let result = collector.finalize();
        // Should be grouped into 1 entry (same email)
        assert_eq!(result.entries.len(), 1);
        match result.entries[0].values.get("commits") {
            Some(MetricValue::Count(n)) => assert_eq!(*n, 2),
            other => panic!("Expected Count(2), got {:?}", other),
        }
    }

    #[test]
    fn test_sorted_by_commits_desc() {
        let mut collector = AuthorsCollector::new();
        collector.process(&make_change("Alice", "a.rs", 1, 0));
        collector.process(&make_change("Alice", "b.rs", 1, 0));
        collector.process(&make_change("Alice", "c.rs", 1, 0));
        collector.process(&make_change("Bob", "a.rs", 1, 0));

        let result = collector.finalize();
        assert_eq!(result.entries[0].key, "Alice@test.com");
        assert_eq!(result.entries[1].key, "Bob@test.com");
    }
}
