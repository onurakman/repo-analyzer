use std::collections::HashMap;

use crate::metrics::MetricCollector;
use crate::store::ChangeStore;
use crate::types::{MetricEntry, MetricResult, MetricValue};

/// Disk-backed churn collector. No per-change state is kept in RAM — the
/// aggregation runs as a single SQL query against the shared change store
/// in `finalize_from_db`.
pub struct ChurnCollector;

impl Default for ChurnCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl ChurnCollector {
    pub fn new() -> Self {
        Self
    }
}

impl MetricCollector for ChurnCollector {
    fn name(&self) -> &str {
        "churn"
    }

    fn finalize(&mut self) -> MetricResult {
        // In-memory path is unused — empty result is the safety net if the
        // pipeline ever runs without the change store.
        empty_result()
    }

    fn finalize_from_db(
        &mut self,
        store: &ChangeStore,
        _progress: &crate::metrics::ProgressReporter,
    ) -> Option<MetricResult> {
        let entries = store
            .with_conn(|conn| -> anyhow::Result<Vec<MetricEntry>> {
                let mut stmt = conn.prepare(
                    "SELECT file_path,
                            SUM(additions) AS added,
                            SUM(deletions) AS deleted,
                            COUNT(*) AS change_count
                       FROM changes
                      GROUP BY file_path
                      ORDER BY (SUM(additions) + SUM(deletions)) DESC
                      LIMIT 500",
                )?;
                let rows = stmt.query_map([], |row| {
                    let file: String = row.get(0)?;
                    let added: i64 = row.get(1)?;
                    let deleted: i64 = row.get(2)?;
                    let change_count: i64 = row.get(3)?;
                    Ok((file, added as u64, deleted as u64, change_count as u64))
                })?;

                let mut out = Vec::new();
                for r in rows {
                    let (file, added, deleted, change_count) = r?;
                    let total_churn = added + deleted;
                    let net_change = added as i64 - deleted as i64;
                    let churn_rate = if change_count > 0 {
                        total_churn as f64 / change_count as f64
                    } else {
                        0.0
                    };

                    let mut values = HashMap::new();
                    values.insert("lines_added".into(), MetricValue::Count(added));
                    values.insert("lines_deleted".into(), MetricValue::Count(deleted));
                    values.insert("net_change".into(), MetricValue::SignedCount(net_change));
                    values.insert("total_churn".into(), MetricValue::Count(total_churn));
                    values.insert("change_count".into(), MetricValue::Count(change_count));
                    values.insert("churn_rate".into(), MetricValue::Float(churn_rate));
                    out.push(MetricEntry { key: file, values });
                }
                Ok(out)
            })
            .ok()?
            .ok()?;

        Some(MetricResult {
            name: "churn".into(),
            display_name: "File Churn".into(),
            description: "How much each file has been rewritten over time — total lines added, deleted, and net change. High churn means a file keeps getting reworked, usually a sign of unstable design or unclear requirements. Files at the top deserve a closer look.".into(),
            entry_groups: vec![],
            column_labels: vec![],
            columns: vec![
                "lines_added".into(),
                "lines_deleted".into(),
                "net_change".into(),
                "total_churn".into(),
                "change_count".into(),
                "churn_rate".into(),
            ],
            entries,
        })
    }
}

fn empty_result() -> MetricResult {
    MetricResult {
        name: "churn".into(),
        display_name: "File Churn".into(),
        description: String::new(),
        entry_groups: vec![],
        column_labels: vec![],
        columns: vec![],
        entries: vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{CommitInfo, DiffRecord, FileStatus, ParsedChange};
    use chrono::{FixedOffset, TimeZone};
    use std::sync::Arc;

    fn make_change(file: &str, oid: &str, added: u32, deleted: u32) -> ParsedChange {
        let ts = FixedOffset::east_opt(0)
            .unwrap()
            .with_ymd_and_hms(2025, 1, 15, 12, 0, 0)
            .unwrap();
        ParsedChange {
            diff: Arc::new(DiffRecord {
                commit: Arc::new(CommitInfo {
                    oid: oid.into(),
                    author: "dev".into(),
                    email: "dev@test.com".into(),
                    timestamp: ts,
                    message: "test".into(),
                    parent_ids: vec![],
                }),
                file_path: file.into(),
                old_path: None,
                status: FileStatus::Modified,
                hunks: vec![],
                additions: added,
                deletions: deleted,
            }),
            constructs: vec![],
        }
    }

    fn store_with(changes: &[ParsedChange]) -> ChangeStore {
        let store = ChangeStore::open_temp().expect("open store");
        store.insert_batch(changes).expect("insert");
        store.finalize_indexes().expect("index");
        store
    }

    #[test]
    fn test_churn_accumulation() {
        let store = store_with(&[
            make_change("a.rs", "c1", 10, 3),
            make_change("a.rs", "c2", 5, 2),
        ]);

        let mut collector = ChurnCollector::new();
        let result = collector
            .finalize_from_db(&store, &crate::metrics::ProgressReporter::new(None))
            .expect("db result");
        let entry = result.entries.iter().find(|e| e.key == "a.rs").unwrap();

        match entry.values.get("lines_added") {
            Some(MetricValue::Count(n)) => assert_eq!(*n, 15),
            other => panic!("Expected Count(15), got {other:?}"),
        }
        match entry.values.get("lines_deleted") {
            Some(MetricValue::Count(n)) => assert_eq!(*n, 5),
            other => panic!("Expected Count(5), got {other:?}"),
        }
        match entry.values.get("total_churn") {
            Some(MetricValue::Count(n)) => assert_eq!(*n, 20),
            other => panic!("Expected Count(20), got {other:?}"),
        }
        match entry.values.get("change_count") {
            Some(MetricValue::Count(n)) => assert_eq!(*n, 2),
            other => panic!("Expected Count(2), got {other:?}"),
        }
    }

    #[test]
    fn test_sorted_by_total_churn() {
        let store = store_with(&[
            make_change("small.rs", "c1", 2, 1),
            make_change("big.rs", "c2", 100, 50),
        ]);

        let mut collector = ChurnCollector::new();
        let result = collector
            .finalize_from_db(&store, &crate::metrics::ProgressReporter::new(None))
            .expect("db result");
        assert_eq!(result.entries[0].key, "big.rs");
        assert_eq!(result.entries[1].key, "small.rs");
    }
}
