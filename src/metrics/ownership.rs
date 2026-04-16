use std::collections::HashMap;

use crate::metrics::MetricCollector;
use crate::types::{MetricEntry, MetricResult, MetricValue, ParsedChange};

pub struct OwnershipCollector {
    /// file -> (author -> lines_added)
    files: HashMap<String, HashMap<String, u64>>,
}

impl Default for OwnershipCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl OwnershipCollector {
    pub fn new() -> Self {
        Self {
            files: HashMap::new(),
        }
    }
}

impl MetricCollector for OwnershipCollector {
    fn name(&self) -> &str {
        "ownership"
    }

    fn process(&mut self, change: &ParsedChange) {
        let file = &change.diff.file_path;
        let email = &change.diff.commit.email;
        let added = change.diff.additions as u64;

        let authors = self.files.entry(file.clone()).or_default();
        *authors.entry(email.clone()).or_insert(0) += added;
    }

    fn finalize(&mut self) -> MetricResult {
        let mut entries: Vec<MetricEntry> = self
            .files
            .drain()
            .map(|(file, authors)| {
                let total_lines: u64 = authors.values().sum();
                let total_authors = authors.len() as u64;

                // Find top author
                let top_author = authors
                    .iter()
                    .max_by_key(|(_, v)| **v)
                    .map(|(name, _)| name.clone())
                    .unwrap_or_default();

                // Compute bus factor: minimum authors to reach >50%
                let bus_factor = compute_bus_factor(&authors, total_lines);

                let mut values = HashMap::new();
                values.insert("total_authors".into(), MetricValue::Count(total_authors));
                values.insert("bus_factor".into(), MetricValue::Count(bus_factor));
                values.insert("top_author".into(), MetricValue::Text(top_author));
                values.insert("total_lines".into(), MetricValue::Count(total_lines));

                MetricEntry { key: file, values }
            })
            .collect();

        entries.sort_by(|a, b| {
            let fa = match a.values.get("bus_factor") {
                Some(MetricValue::Count(n)) => *n,
                _ => u64::MAX,
            };
            let fb = match b.values.get("bus_factor") {
                Some(MetricValue::Count(n)) => *n,
                _ => u64::MAX,
            };
            fa.cmp(&fb)
        });

        MetricResult {
            name: "ownership".into(),
            description: "File ownership distribution and bus factor analysis".into(),
            entry_groups: vec![],
            columns: vec![
                "total_authors".into(),
                "bus_factor".into(),
                "top_author".into(),
                "total_lines".into(),
            ],
            entries,
        }
    }
}

fn compute_bus_factor(authors: &HashMap<String, u64>, total_lines: u64) -> u64 {
    if total_lines == 0 {
        return 0;
    }

    let threshold = total_lines as f64 * 0.5;
    let mut contributions: Vec<u64> = authors.values().copied().collect();
    contributions.sort_unstable_by(|a, b| b.cmp(a)); // descending

    let mut accumulated = 0u64;
    for (i, &contrib) in contributions.iter().enumerate() {
        accumulated += contrib;
        if accumulated as f64 > threshold {
            return (i + 1) as u64;
        }
    }

    contributions.len() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{CommitInfo, DiffRecord, FileStatus, ParsedChange};
    use chrono::{FixedOffset, TimeZone};

    fn make_change(author: &str, file: &str, added: u32) -> ParsedChange {
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
                deletions: 0,
            },
            constructs: vec![],
        }
    }

    #[test]
    fn test_bus_factor() {
        // Alice: 90 lines, Bob: 10 lines => bus_factor = 1
        let mut collector = OwnershipCollector::new();
        collector.process(&make_change("Alice", "lib.rs", 90));
        collector.process(&make_change("Bob", "lib.rs", 10));

        let result = collector.finalize();
        let entry = result.entries.iter().find(|e| e.key == "lib.rs").unwrap();

        match entry.values.get("bus_factor") {
            Some(MetricValue::Count(n)) => assert_eq!(*n, 1),
            other => panic!("Expected Count(1), got {:?}", other),
        }
    }

    #[test]
    fn test_bus_factor_distributed() {
        // Alice: 33, Bob: 33, Carol: 34 => bus_factor = 2
        let mut collector = OwnershipCollector::new();
        collector.process(&make_change("Alice", "lib.rs", 33));
        collector.process(&make_change("Bob", "lib.rs", 33));
        collector.process(&make_change("Carol", "lib.rs", 34));

        let result = collector.finalize();
        let entry = result.entries.iter().find(|e| e.key == "lib.rs").unwrap();

        match entry.values.get("bus_factor") {
            Some(MetricValue::Count(n)) => assert_eq!(*n, 2),
            other => panic!("Expected Count(2), got {:?}", other),
        }
    }
}
