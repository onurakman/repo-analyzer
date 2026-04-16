use std::collections::{HashMap, HashSet};

use crate::metrics::MetricCollector;
use crate::types::{MetricEntry, MetricResult, MetricValue, ParsedChange};

pub struct PatternsCollector {
    /// hour (0-23) -> commit count
    hourly: [u64; 24],
    /// day (0=Mon .. 6=Sun) -> commit count
    daily: [u64; 7],
    /// Deduplication: seen commit ids
    seen_commits: HashSet<String>,
}

impl PatternsCollector {
    pub fn new() -> Self {
        Self {
            hourly: [0; 24],
            daily: [0; 7],
            seen_commits: HashSet::new(),
        }
    }
}

impl MetricCollector for PatternsCollector {
    fn name(&self) -> &str {
        "patterns"
    }

    fn process(&mut self, change: &ParsedChange) {
        let commit_id = &change.diff.commit.oid;

        // Deduplicate: one commit touching many files should count once
        if !self.seen_commits.insert(commit_id.clone()) {
            return;
        }

        let ts = &change.diff.commit.timestamp;
        let hour = ts.format("%H").to_string().parse::<usize>().unwrap_or(0);
        let weekday = ts.format("%u").to_string().parse::<usize>().unwrap_or(1); // 1=Mon..7=Sun

        self.hourly[hour] += 1;
        self.daily[weekday.saturating_sub(1).min(6)] += 1;
    }

    fn finalize(&mut self) -> MetricResult {
        let mut entries: Vec<MetricEntry> = Vec::new();

        // 24 hourly entries
        for h in 0..24 {
            let key = format!("{:02}:00", h);
            let mut values = HashMap::new();
            values.insert("type".into(), MetricValue::Text("hourly".into()));
            values.insert("commits".into(), MetricValue::Count(self.hourly[h]));
            entries.push(MetricEntry { key, values });
        }

        // 7 daily entries
        let day_names = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
        for (i, &name) in day_names.iter().enumerate() {
            let mut values = HashMap::new();
            values.insert("type".into(), MetricValue::Text("daily".into()));
            values.insert("commits".into(), MetricValue::Count(self.daily[i]));
            entries.push(MetricEntry {
                key: name.into(),
                values,
            });
        }

        MetricResult {
            name: "patterns".into(),
            description: "Commit distribution by hour of day and day of week".into(),
            columns: vec!["type".into(), "commits".into()],
            entries,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{CommitInfo, DiffRecord, FileStatus, ParsedChange};
    use chrono::{FixedOffset, TimeZone};

    fn make_change_at(commit_id: &str, hour: u32, file: &str) -> ParsedChange {
        // 2025-01-13 is a Monday
        let ts = FixedOffset::east_opt(0)
            .unwrap()
            .with_ymd_and_hms(2025, 1, 13, hour, 30, 0)
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
    fn test_hourly_pattern() {
        let mut collector = PatternsCollector::new();
        collector.process(&make_change_at("c1", 9, "a.rs"));
        collector.process(&make_change_at("c2", 9, "b.rs"));
        collector.process(&make_change_at("c3", 14, "a.rs"));

        let result = collector.finalize();

        // Find the 09:00 entry
        let h9 = result.entries.iter().find(|e| e.key == "09:00").unwrap();
        match h9.values.get("commits") {
            Some(MetricValue::Count(n)) => assert_eq!(*n, 2),
            other => panic!("Expected Count(2), got {:?}", other),
        }

        // Find the 14:00 entry
        let h14 = result.entries.iter().find(|e| e.key == "14:00").unwrap();
        match h14.values.get("commits") {
            Some(MetricValue::Count(n)) => assert_eq!(*n, 1),
            other => panic!("Expected Count(1), got {:?}", other),
        }
    }

    #[test]
    fn test_deduplicates_commits() {
        let mut collector = PatternsCollector::new();
        // Same commit touching 3 files should count only once
        collector.process(&make_change_at("same_commit", 10, "a.rs"));
        collector.process(&make_change_at("same_commit", 10, "b.rs"));
        collector.process(&make_change_at("same_commit", 10, "c.rs"));

        let result = collector.finalize();
        let h10 = result.entries.iter().find(|e| e.key == "10:00").unwrap();
        match h10.values.get("commits") {
            Some(MetricValue::Count(n)) => assert_eq!(*n, 1),
            other => panic!("Expected Count(1), got {:?}", other),
        }
    }
}
