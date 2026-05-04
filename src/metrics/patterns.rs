use std::collections::HashMap;

use crate::metrics::MetricCollector;
use crate::store::ChangeStore;
use crate::types::{
    Column, EntryGroup, MetricEntry, MetricResult, MetricValue, report_description, report_display,
};

pub struct PatternsCollector;

impl Default for PatternsCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl PatternsCollector {
    pub fn new() -> Self {
        Self
    }
}

impl MetricCollector for PatternsCollector {
    fn name(&self) -> &str {
        "patterns"
    }

    fn finalize(&mut self) -> MetricResult {
        empty_result()
    }

    fn finalize_from_db(
        &mut self,
        store: &ChangeStore,
        _progress: &crate::metrics::ProgressReporter,
    ) -> Option<MetricResult> {
        // SQLite %w is 0=Sun..6=Sat. Remap to 0=Mon..6=Sun so Monday is at index 0,
        // matching the original `%u` (1=Mon..7=Sun) / day_names layout.
        let (hourly, daily) = store
            .with_conn(|conn| -> anyhow::Result<([u64; 24], [u64; 7])> {
                let mut hourly = [0u64; 24];
                let mut daily = [0u64; 7];

                let mut stmt = conn.prepare(
                    "SELECT
                        CAST(strftime('%H', datetime(commit_ts, 'unixepoch')) AS INTEGER) AS hour,
                        CAST(strftime('%w', datetime(commit_ts, 'unixepoch')) AS INTEGER) AS dow,
                        COUNT(*) AS commits
                       FROM (
                          SELECT commit_oid, MIN(commit_ts) AS commit_ts
                            FROM changes GROUP BY commit_oid
                       )
                      GROUP BY hour, dow",
                )?;
                let rows = stmt.query_map([], |row| {
                    let hour: i64 = row.get(0)?;
                    let dow: i64 = row.get(1)?;
                    let cnt: i64 = row.get(2)?;
                    Ok((hour, dow, cnt as u64))
                })?;
                for r in rows {
                    let (hour, dow, cnt) = r?;
                    let h = hour.clamp(0, 23) as usize;
                    hourly[h] += cnt;
                    // SQLite 0=Sun..6=Sat. Our output slots 0=Mon..6=Sun.
                    let day_idx = match dow {
                        0 => 6,                // Sun → slot 6
                        n => (n - 1) as usize, // Mon..Sat → 0..5
                    };
                    daily[day_idx] += cnt;
                }
                Ok((hourly, daily))
            })
            .ok()?
            .ok()?;

        let hourly_entries: Vec<MetricEntry> = (0..24)
            .map(|h| {
                let key = format!("{:02}:00", h);
                let mut values = HashMap::new();
                values.insert("order".into(), MetricValue::Count(h as u64));
                values.insert("commits".into(), MetricValue::Count(hourly[h]));
                MetricEntry { key, values }
            })
            .collect();

        let day_names = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
        let daily_entries: Vec<MetricEntry> = day_names
            .iter()
            .enumerate()
            .map(|(i, &name)| {
                let mut values = HashMap::new();
                values.insert("order".into(), MetricValue::Count((i + 1) as u64));
                values.insert("commits".into(), MetricValue::Count(daily[i]));
                MetricEntry {
                    key: name.into(),
                    values,
                }
            })
            .collect();

        Some(MetricResult {
            name: "patterns".into(),
            display_name: report_display("patterns"),
            description: report_description("patterns"),
            columns: vec![
                Column::in_report("patterns", "order"),
                Column::in_report("patterns", "commits"),
            ],
            entries: vec![],
            entry_groups: vec![
                EntryGroup {
                    name: "hourly".into(),
                    label: "Hourly".into(),
                    entries: hourly_entries,
                },
                EntryGroup {
                    name: "daily".into(),
                    label: "Daily".into(),
                    entries: daily_entries,
                },
            ],
        })
    }
}

fn empty_result() -> MetricResult {
    MetricResult {
        name: "patterns".into(),
        display_name: report_display("patterns"),
        description: report_description("patterns"),
        columns: vec![],
        entries: vec![],
        entry_groups: vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{CommitInfo, DiffRecord, FileStatus, ParsedChange};
    use chrono::{FixedOffset, TimeZone};
    use std::sync::Arc;

    fn make_change_at(
        file: &str,
        oid: &str,
        year: i32,
        month: u32,
        day: u32,
        hour: u32,
    ) -> ParsedChange {
        let ts = FixedOffset::east_opt(0)
            .unwrap()
            .with_ymd_and_hms(year, month, day, hour, 0, 0)
            .unwrap();
        ParsedChange {
            diff: Arc::new(DiffRecord {
                commit: Arc::new(CommitInfo {
                    oid: oid.into(),
                    author: "dev".into(),
                    email: "dev@x".into(),
                    timestamp: ts,
                    message: "m".into(),
                    parent_ids: vec![],
                }),
                file_path: file.into(),
                old_path: None,
                status: FileStatus::Modified,
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

    fn group<'a>(r: &'a MetricResult, name: &str) -> &'a EntryGroup {
        r.entry_groups
            .iter()
            .find(|g| g.name == name)
            .unwrap_or_else(|| panic!("group {name} missing"))
    }

    fn count_at(group: &EntryGroup, key: &str) -> u64 {
        let entry = group
            .entries
            .iter()
            .find(|e| e.key == key)
            .unwrap_or_else(|| panic!("entry {key} missing in group {}", group.name));
        match entry.values.get("commits") {
            Some(MetricValue::Count(n)) => *n,
            other => panic!("expected commits Count, got {other:?}"),
        }
    }

    #[test]
    fn empty_collector_returns_named_result() {
        let mut coll = PatternsCollector::new();
        let r = coll.finalize();
        assert_eq!(r.name, "patterns");
        assert!(r.entries.is_empty());
        assert!(r.entry_groups.is_empty());
    }

    #[test]
    fn finalize_from_db_emits_24_hourly_and_7_daily_slots() {
        let store = store_with(&[]);
        let mut coll = PatternsCollector::new();
        let r = coll
            .finalize_from_db(&store, &crate::metrics::ProgressReporter::new(None))
            .expect("db result");
        assert_eq!(group(&r, "hourly").entries.len(), 24);
        assert_eq!(group(&r, "daily").entries.len(), 7);
        // Day order is Mon..Sun, locked down by the writer column layout.
        let names: Vec<_> = group(&r, "daily")
            .entries
            .iter()
            .map(|e| e.key.clone())
            .collect();
        assert_eq!(names, vec!["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"]);
    }

    #[test]
    fn finalize_from_db_buckets_hour_correctly() {
        // 2025-01-13 14:00 UTC = Mon at hour 14
        let store = store_with(&[make_change_at("a.rs", "c1", 2025, 1, 13, 14)]);
        let mut coll = PatternsCollector::new();
        let r = coll
            .finalize_from_db(&store, &crate::metrics::ProgressReporter::new(None))
            .expect("db result");
        assert_eq!(count_at(group(&r, "hourly"), "14:00"), 1);
        assert_eq!(count_at(group(&r, "hourly"), "13:00"), 0);
        assert_eq!(count_at(group(&r, "hourly"), "15:00"), 0);
    }

    #[test]
    fn finalize_from_db_remaps_sunday_to_last_slot() {
        // 2025-01-12 is a Sunday — slot should be 6 (Sun), not 0.
        let store = store_with(&[make_change_at("a.rs", "csun", 2025, 1, 12, 10)]);
        let mut coll = PatternsCollector::new();
        let r = coll
            .finalize_from_db(&store, &crate::metrics::ProgressReporter::new(None))
            .expect("db result");
        assert_eq!(count_at(group(&r, "daily"), "Sun"), 1);
        assert_eq!(count_at(group(&r, "daily"), "Mon"), 0);
    }

    #[test]
    fn finalize_from_db_remaps_weekdays_starting_at_monday() {
        // 2025-01-13 is a Monday → slot 0 (Mon).
        // 2025-01-18 is a Saturday → slot 5 (Sat).
        let store = store_with(&[
            make_change_at("a.rs", "cmon", 2025, 1, 13, 9),
            make_change_at("b.rs", "csat", 2025, 1, 18, 9),
        ]);
        let mut coll = PatternsCollector::new();
        let r = coll
            .finalize_from_db(&store, &crate::metrics::ProgressReporter::new(None))
            .expect("db result");
        assert_eq!(count_at(group(&r, "daily"), "Mon"), 1);
        assert_eq!(count_at(group(&r, "daily"), "Sat"), 1);
        assert_eq!(count_at(group(&r, "daily"), "Sun"), 0);
    }

    #[test]
    fn finalize_from_db_dedupes_per_commit_across_files() {
        // Two file rows under the same commit_oid must count once in
        // both hourly and daily totals (the SQL groups by commit_oid first).
        let store = store_with(&[
            make_change_at("a.rs", "c1", 2025, 1, 13, 9),
            make_change_at("b.rs", "c1", 2025, 1, 13, 9),
        ]);
        let mut coll = PatternsCollector::new();
        let r = coll
            .finalize_from_db(&store, &crate::metrics::ProgressReporter::new(None))
            .expect("db result");
        assert_eq!(count_at(group(&r, "hourly"), "09:00"), 1);
        assert_eq!(count_at(group(&r, "daily"), "Mon"), 1);
    }

    #[test]
    fn finalize_from_db_carries_order_value() {
        let store = store_with(&[]);
        let mut coll = PatternsCollector::new();
        let r = coll
            .finalize_from_db(&store, &crate::metrics::ProgressReporter::new(None))
            .expect("db result");
        // Hourly: order = 0..23 (used by writers to keep the table sorted by hour).
        let hourly = group(&r, "hourly");
        for (i, entry) in hourly.entries.iter().enumerate() {
            match entry.values.get("order") {
                Some(MetricValue::Count(n)) => assert_eq!(*n, i as u64),
                other => panic!("expected hourly order Count({i}), got {other:?}"),
            }
        }
        // Daily: order = 1..7 so ascending sort matches Mon..Sun.
        let daily = group(&r, "daily");
        for (i, entry) in daily.entries.iter().enumerate() {
            match entry.values.get("order") {
                Some(MetricValue::Count(n)) => assert_eq!(*n, (i + 1) as u64),
                other => panic!("expected daily order Count({}), got {other:?}", i + 1),
            }
        }
    }
}
