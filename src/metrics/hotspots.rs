use std::collections::{HashMap, HashSet};

use crate::metrics::MetricCollector;
use crate::types::{MetricEntry, MetricResult, MetricValue, ParsedChange};

struct HotspotData {
    change_count: u64,
    authors: HashSet<String>,
}

impl HotspotData {
    fn new() -> Self {
        Self {
            change_count: 0,
            authors: HashSet::new(),
        }
    }

    fn score(&self) -> u64 {
        self.change_count * self.authors.len() as u64
    }
}

pub struct HotspotsCollector {
    file_hotspots: HashMap<String, HotspotData>,
    construct_hotspots: HashMap<String, ConstructHotspot>,
}

struct ConstructHotspot {
    data: HotspotData,
    kind: String,
    file: String,
}

impl HotspotsCollector {
    pub fn new() -> Self {
        Self {
            file_hotspots: HashMap::new(),
            construct_hotspots: HashMap::new(),
        }
    }
}

impl MetricCollector for HotspotsCollector {
    fn name(&self) -> &str {
        "hotspots"
    }

    fn process(&mut self, change: &ParsedChange) {
        let file = &change.diff.file_path;
        let author = &change.diff.commit.author;

        // File-level hotspot
        let file_data = self
            .file_hotspots
            .entry(file.clone())
            .or_insert_with(HotspotData::new);
        file_data.change_count += 1;
        file_data.authors.insert(author.clone());

        // Construct-level hotspots
        for construct in &change.constructs {
            let key = format!("{}::{}", file, construct.qualified_name());
            let entry = self
                .construct_hotspots
                .entry(key)
                .or_insert_with(|| ConstructHotspot {
                    data: HotspotData::new(),
                    kind: construct.kind_str().to_string(),
                    file: file.clone(),
                });
            entry.data.change_count += 1;
            entry.data.authors.insert(author.clone());
        }
    }

    fn finalize(&mut self) -> MetricResult {
        let mut entries: Vec<MetricEntry> = Vec::new();

        // File-level entries
        for (file, data) in self.file_hotspots.drain() {
            let mut values = HashMap::new();
            values.insert("level".into(), MetricValue::Text("file".into()));
            values.insert("changes".into(), MetricValue::Count(data.change_count));
            values.insert(
                "unique_authors".into(),
                MetricValue::Count(data.authors.len() as u64),
            );
            values.insert("score".into(), MetricValue::Count(data.score()));
            entries.push(MetricEntry { key: file, values });
        }

        // Construct-level entries
        for (key, hotspot) in self.construct_hotspots.drain() {
            let mut values = HashMap::new();
            values.insert("level".into(), MetricValue::Text("construct".into()));
            values.insert("kind".into(), MetricValue::Text(hotspot.kind));
            values.insert("file".into(), MetricValue::Text(hotspot.file));
            values.insert(
                "changes".into(),
                MetricValue::Count(hotspot.data.change_count),
            );
            values.insert(
                "unique_authors".into(),
                MetricValue::Count(hotspot.data.authors.len() as u64),
            );
            values.insert("score".into(), MetricValue::Count(hotspot.data.score()));
            entries.push(MetricEntry { key, values });
        }

        entries.sort_by(|a, b| {
            let sa = match a.values.get("score") {
                Some(MetricValue::Count(n)) => *n,
                _ => 0,
            };
            let sb = match b.values.get("score") {
                Some(MetricValue::Count(n)) => *n,
                _ => 0,
            };
            sb.cmp(&sa)
        });

        MetricResult {
            name: "hotspots".into(),
            description: "Files and constructs with high change frequency and author diversity".into(),
            columns: vec![
                "level".into(),
                "kind".into(),
                "file".into(),
                "changes".into(),
                "unique_authors".into(),
                "score".into(),
            ],
            entries,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{CodeConstruct, CommitInfo, DiffRecord, FileStatus, ParsedChange};
    use chrono::{FixedOffset, TimeZone};

    fn make_change(
        author: &str,
        file: &str,
        constructs: Vec<CodeConstruct>,
    ) -> ParsedChange {
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
                additions: 10,
                deletions: 2,
            },
            constructs,
        }
    }

    #[test]
    fn test_file_hotspot() {
        let mut collector = HotspotsCollector::new();
        collector.process(&make_change("Alice", "src/lib.rs", vec![]));
        collector.process(&make_change("Bob", "src/lib.rs", vec![]));

        let result = collector.finalize();
        let entry = result
            .entries
            .iter()
            .find(|e| e.key == "src/lib.rs")
            .unwrap();

        match entry.values.get("changes") {
            Some(MetricValue::Count(n)) => assert_eq!(*n, 2),
            other => panic!("Expected Count(2), got {:?}", other),
        }
        match entry.values.get("unique_authors") {
            Some(MetricValue::Count(n)) => assert_eq!(*n, 2),
            other => panic!("Expected Count(2), got {:?}", other),
        }
        // score = 2 changes * 2 authors = 4
        match entry.values.get("score") {
            Some(MetricValue::Count(n)) => assert_eq!(*n, 4),
            other => panic!("Expected Count(4), got {:?}", other),
        }
    }

    #[test]
    fn test_construct_hotspot() {
        let func = CodeConstruct::Function {
            name: "do_stuff".into(),
            start_line: 10,
            end_line: 20,
            enclosing: None,
        };
        let mut collector = HotspotsCollector::new();
        collector.process(&make_change("Alice", "src/lib.rs", vec![func.clone()]));
        collector.process(&make_change("Bob", "src/lib.rs", vec![func]));

        let result = collector.finalize();
        let entry = result
            .entries
            .iter()
            .find(|e| e.key == "src/lib.rs::do_stuff")
            .unwrap();

        match entry.values.get("level") {
            Some(MetricValue::Text(s)) => assert_eq!(s, "construct"),
            other => panic!("Expected Text(construct), got {:?}", other),
        }
        match entry.values.get("score") {
            Some(MetricValue::Count(n)) => assert_eq!(*n, 4),
            other => panic!("Expected Count(4), got {:?}", other),
        }
    }
}
