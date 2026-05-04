use std::collections::HashMap;

use chrono::{DateTime, NaiveDate, TimeZone, Utc};

use crate::metrics::MetricCollector;
use crate::store::ChangeStore;
use crate::types::{
    Column, MetricEntry, MetricResult, MetricValue, report_description, report_display,
};

pub struct AgeCollector;

impl Default for AgeCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl AgeCollector {
    pub fn new() -> Self {
        Self
    }
}

impl MetricCollector for AgeCollector {
    fn name(&self) -> &str {
        "age"
    }

    fn finalize(&mut self) -> MetricResult {
        empty_result()
    }

    fn finalize_from_db(
        &mut self,
        store: &ChangeStore,
        _progress: &crate::metrics::ProgressReporter,
    ) -> Option<MetricResult> {
        // status=2 means Deleted; skip files that were ever deleted.
        let entries = store
            .with_conn(|conn| -> anyhow::Result<Vec<MetricEntry>> {
                let mut stmt = conn.prepare(
                    "SELECT file_path,
                            MIN(commit_ts)                AS first_ts,
                            MAX(commit_ts)                AS last_ts,
                            COUNT(*)                      AS change_count,
                            MAX(CASE WHEN status = 2 THEN 1 ELSE 0 END) AS ever_deleted
                       FROM changes
                      GROUP BY file_path",
                )?;
                let rows = stmt.query_map([], |row| {
                    let file: String = row.get(0)?;
                    let first_ts: i64 = row.get(1)?;
                    let last_ts: i64 = row.get(2)?;
                    let change_count: i64 = row.get(3)?;
                    let deleted: i64 = row.get(4)?;
                    Ok((file, first_ts, last_ts, change_count as u64, deleted != 0))
                })?;
                let mut out = Vec::new();
                let today = Utc::now().date_naive();
                for r in rows {
                    let (file, first_ts, last_ts, change_count, deleted) = r?;
                    if deleted {
                        continue;
                    }
                    let first_seen = ts_to_date(first_ts);
                    let last_modified = ts_to_date(last_ts);
                    let age_days = (today - first_seen).num_days().max(0) as u64;
                    let days_since_last_change = (today - last_modified).num_days().max(0) as u64;
                    let age_years = age_days as f64 / 365.25;
                    let changes_per_year = if age_years > 0.0 {
                        change_count as f64 / age_years
                    } else {
                        change_count as f64
                    };

                    let mut values = HashMap::new();
                    values.insert("age_days".into(), MetricValue::Count(age_days));
                    values.insert("first_seen".into(), MetricValue::Date(first_seen));
                    values.insert("last_modified".into(), MetricValue::Date(last_modified));
                    values.insert(
                        "days_since_last_change".into(),
                        MetricValue::Count(days_since_last_change),
                    );
                    values.insert("change_count".into(), MetricValue::Count(change_count));
                    values.insert(
                        "changes_per_year".into(),
                        MetricValue::Float(changes_per_year),
                    );
                    out.push(MetricEntry { key: file, values });
                }
                Ok(out)
            })
            .ok()?
            .ok()?;

        let mut entries = entries;
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

        Some(MetricResult {
            name: "age".into(),
            display_name: report_display("age"),
            description: report_description("age"),
            entry_groups: vec![],
            columns: vec![
                Column::in_report("age", "age_days"),
                Column::in_report("age", "first_seen"),
                Column::in_report("age", "last_modified"),
                Column::in_report("age", "days_since_last_change"),
                Column::in_report("age", "change_count"),
                Column::in_report("age", "changes_per_year"),
            ],
            entries,
        })
    }
}

fn ts_to_date(ts: i64) -> NaiveDate {
    let dt: DateTime<Utc> = Utc.timestamp_opt(ts, 0).single().unwrap_or_default();
    dt.date_naive()
}

fn empty_result() -> MetricResult {
    MetricResult {
        name: "age".into(),
        display_name: report_display("age"),
        description: report_description("age"),
        entry_groups: vec![],
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

    fn make_change_at(file: &str, oid: &str, status: FileStatus, ts_year: i32) -> ParsedChange {
        let ts = FixedOffset::east_opt(0)
            .unwrap()
            .with_ymd_and_hms(ts_year, 1, 1, 0, 0, 0)
            .unwrap();
        ParsedChange {
            diff: Arc::new(DiffRecord {
                commit: Arc::new(CommitInfo {
                    oid: oid.into(),
                    author: "dev".into(),
                    email: "dev@test.com".into(),
                    timestamp: ts,
                    message: "m".into(),
                    parent_ids: vec![],
                }),
                file_path: file.into(),
                old_path: None,
                status,
                hunks: vec![],
                additions: 1,
                deletions: 0,
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
    fn empty_collector_finalize_returns_named_result() {
        let mut coll = AgeCollector::new();
        let result = coll.finalize();
        assert_eq!(result.name, "age");
        assert!(result.entries.is_empty());
    }

    #[test]
    fn ts_to_date_handles_negative_timestamp_safely() {
        // Should not panic on out-of-range timestamps; uses default (epoch).
        let date = ts_to_date(i64::MIN);
        // The fallback returns NaiveDate::default() — assert it produced something.
        let _ = date.format("%Y-%m-%d").to_string();
    }

    #[test]
    fn ts_to_date_known_value() {
        // 2024-06-15 00:00:00 UTC = 1718409600
        let date = ts_to_date(1_718_409_600);
        assert_eq!(date.format("%Y-%m-%d").to_string(), "2024-06-15");
    }

    #[test]
    fn finalize_from_db_skips_deleted_files() {
        let store = store_with(&[
            make_change_at("alive.rs", "c1", FileStatus::Added, 2020),
            make_change_at("alive.rs", "c2", FileStatus::Modified, 2021),
            make_change_at("gone.rs", "c3", FileStatus::Added, 2020),
            make_change_at("gone.rs", "c4", FileStatus::Deleted, 2021),
        ]);

        let mut coll = AgeCollector::new();
        let result = coll
            .finalize_from_db(&store, &crate::metrics::ProgressReporter::new(None))
            .expect("db result");
        assert!(result.entries.iter().any(|e| e.key == "alive.rs"));
        assert!(
            !result.entries.iter().any(|e| e.key == "gone.rs"),
            "deleted files must not appear in the age report"
        );
    }

    #[test]
    fn finalize_from_db_sorts_by_changes_per_year_desc() {
        // hot.rs has 3 changes in the same year window; cold.rs has 1.
        // Both share the same first_seen so changes_per_year orders them.
        let store = store_with(&[
            make_change_at("cold.rs", "c1", FileStatus::Added, 2020),
            make_change_at("hot.rs", "c2", FileStatus::Added, 2020),
            make_change_at("hot.rs", "c3", FileStatus::Modified, 2020),
            make_change_at("hot.rs", "c4", FileStatus::Modified, 2021),
        ]);

        let mut coll = AgeCollector::new();
        let result = coll
            .finalize_from_db(&store, &crate::metrics::ProgressReporter::new(None))
            .expect("db result");
        assert!(result.entries.len() >= 2, "expected at least two entries");
        assert_eq!(result.entries[0].key, "hot.rs");
        assert_eq!(result.entries[1].key, "cold.rs");
    }

    #[test]
    fn finalize_from_db_emits_expected_value_keys() {
        let store = store_with(&[make_change_at("a.rs", "c1", FileStatus::Added, 2020)]);

        let mut coll = AgeCollector::new();
        let result = coll
            .finalize_from_db(&store, &crate::metrics::ProgressReporter::new(None))
            .expect("db result");
        let entry = result.entries.iter().find(|e| e.key == "a.rs").unwrap();
        for key in [
            "age_days",
            "first_seen",
            "last_modified",
            "days_since_last_change",
            "change_count",
            "changes_per_year",
        ] {
            assert!(
                entry.values.contains_key(key),
                "missing value key {key} in age entry"
            );
        }
        match entry.values.get("change_count") {
            Some(MetricValue::Count(n)) => assert_eq!(*n, 1),
            other => panic!("Expected Count(1), got {other:?}"),
        }
    }
}
