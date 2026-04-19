use std::collections::HashMap;

use chrono::{DateTime, NaiveDate, TimeZone, Utc};

use crate::metrics::MetricCollector;
use crate::store::ChangeStore;
use crate::types::{
    Column, MetricEntry, MetricResult, MetricValue, report_description, report_display,
};

pub struct AuthorsCollector;

impl Default for AuthorsCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl AuthorsCollector {
    pub fn new() -> Self {
        Self
    }
}

impl MetricCollector for AuthorsCollector {
    fn name(&self) -> &str {
        "authors"
    }

    fn finalize(&mut self) -> MetricResult {
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
                    "SELECT email,
                            COUNT(DISTINCT commit_oid)                          AS commits,
                            SUM(additions)                                      AS lines_added,
                            SUM(deletions)                                      AS lines_deleted,
                            COUNT(DISTINCT date(commit_ts, 'unixepoch'))        AS active_days,
                            MIN(commit_ts)                                      AS first_ts,
                            MAX(commit_ts)                                      AS last_ts
                       FROM changes
                      GROUP BY email
                      ORDER BY commits DESC",
                )?;
                let rows = stmt.query_map([], |row| {
                    let email: String = row.get(0)?;
                    let commits: i64 = row.get(1)?;
                    let added: i64 = row.get(2)?;
                    let deleted: i64 = row.get(3)?;
                    let active_days: i64 = row.get(4)?;
                    let first_ts: i64 = row.get(5)?;
                    let last_ts: i64 = row.get(6)?;
                    Ok((
                        email,
                        commits as u64,
                        added as u64,
                        deleted as u64,
                        active_days as u64,
                        first_ts,
                        last_ts,
                    ))
                })?;

                let mut out = Vec::new();
                for r in rows {
                    let (email, commits, added, deleted, active_days, first_ts, last_ts) = r?;
                    let first: NaiveDate = ts_to_date(first_ts);
                    let last: NaiveDate = ts_to_date(last_ts);
                    let mut values = HashMap::new();
                    values.insert("commits".into(), MetricValue::Count(commits));
                    values.insert("lines_added".into(), MetricValue::Count(added));
                    values.insert("lines_deleted".into(), MetricValue::Count(deleted));
                    values.insert("active_days".into(), MetricValue::Count(active_days));
                    values.insert("first_commit".into(), MetricValue::Date(first));
                    values.insert("last_commit".into(), MetricValue::Date(last));
                    out.push(MetricEntry { key: email, values });
                }
                Ok(out)
            })
            .ok()?
            .ok()?;

        Some(MetricResult {
            name: "authors".into(),
            display_name: report_display("authors"),
            description: report_description("authors"),
            columns: vec![
                Column::in_report("authors", "commits"),
                Column::in_report("authors", "lines_added"),
                Column::in_report("authors", "lines_deleted"),
                Column::in_report("authors", "active_days"),
                Column::in_report("authors", "first_commit"),
                Column::in_report("authors", "last_commit"),
            ],
            entries,
            entry_groups: vec![],
        })
    }
}

fn ts_to_date(ts: i64) -> NaiveDate {
    let dt: DateTime<Utc> = Utc.timestamp_opt(ts, 0).single().unwrap_or_default();
    dt.date_naive()
}

fn empty_result() -> MetricResult {
    MetricResult {
        name: "authors".into(),
        display_name: report_display("authors"),
        description: report_description("authors"),
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

    fn mk(oid: &str, email: &str, file: &str, added: u32, deleted: u32) -> ParsedChange {
        let ts = FixedOffset::east_opt(0)
            .unwrap()
            .with_ymd_and_hms(2025, 1, 15, 12, 0, 0)
            .unwrap();
        ParsedChange {
            diff: Arc::new(DiffRecord {
                commit: Arc::new(CommitInfo {
                    oid: oid.into(),
                    author: email.into(),
                    email: email.into(),
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
        let store = ChangeStore::open_temp().unwrap();
        store.insert_batch(changes).unwrap();
        store.finalize_indexes().unwrap();
        store
    }

    #[test]
    fn groups_by_email_counts_distinct_commits() {
        let store = store_with(&[
            mk("c1", "Alice@x", "a.rs", 10, 2),
            mk("c1", "Alice@x", "b.rs", 5, 1), // same commit, another file
            mk("c2", "Alice@x", "a.rs", 3, 0),
            mk("c3", "Bob@x", "a.rs", 1, 0),
        ]);

        let mut c = AuthorsCollector::new();
        let r = c
            .finalize_from_db(&store, &crate::metrics::ProgressReporter::new(None))
            .expect("db");
        assert_eq!(r.entries.len(), 2);

        let alice = r
            .entries
            .iter()
            .find(|e| e.key == "Alice@x")
            .expect("alice");
        assert!(matches!(
            alice.values.get("commits"),
            Some(MetricValue::Count(2))
        ));
        assert!(matches!(
            alice.values.get("lines_added"),
            Some(MetricValue::Count(18))
        ));
    }
}
