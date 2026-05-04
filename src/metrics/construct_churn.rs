use std::collections::HashMap;

use chrono::{DateTime, NaiveDate, TimeZone, Utc};

use crate::analysis::source_filter::is_source_file;
use crate::metrics::MetricCollector;
use crate::store::ChangeStore;
use crate::types::{
    Column, MetricEntry, MetricResult, MetricValue, report_description, report_display,
};

pub struct ConstructChurnCollector;

impl Default for ConstructChurnCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl ConstructChurnCollector {
    pub fn new() -> Self {
        Self
    }
}

impl MetricCollector for ConstructChurnCollector {
    fn name(&self) -> &str {
        "construct_churn"
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
                    "SELECT ch.file_path,
                            c.qualified_name,
                            c.kind,
                            COUNT(*)                  AS changes,
                            SUM(c.lines_touched)      AS lines_touched,
                            COUNT(DISTINCT ch.email)  AS unique_authors,
                            MAX(ch.commit_ts)         AS last_ts
                       FROM constructs c
                       JOIN changes ch ON c.change_id = ch.id
                      GROUP BY ch.file_path, c.qualified_name, c.kind
                      ORDER BY changes DESC
                      LIMIT 500",
                )?;
                let rows = stmt.query_map([], |row| {
                    let file: String = row.get(0)?;
                    let qn: String = row.get(1)?;
                    let kind: String = row.get(2)?;
                    let changes: i64 = row.get(3)?;
                    let lines: i64 = row.get(4)?;
                    let authors: i64 = row.get(5)?;
                    let last_ts: i64 = row.get(6)?;
                    Ok((
                        file,
                        qn,
                        kind,
                        changes as u64,
                        lines as u64,
                        authors as u64,
                        last_ts,
                    ))
                })?;
                let mut out = Vec::new();
                for r in rows {
                    let (file, qn, kind, changes, lines, authors, last_ts) = r?;
                    if !is_source_file(&file) {
                        continue;
                    }
                    let mut values = HashMap::new();
                    values.insert("kind".into(), MetricValue::Text(kind));
                    values.insert("file".into(), MetricValue::Text(file.clone()));
                    values.insert("changes".into(), MetricValue::Count(changes));
                    values.insert("lines_touched".into(), MetricValue::Count(lines));
                    values.insert("unique_authors".into(), MetricValue::Count(authors));
                    values.insert(
                        "last_modified".into(),
                        MetricValue::Date(ts_to_date(last_ts)),
                    );
                    out.push(MetricEntry {
                        key: format!("{file}::{qn}"),
                        values,
                    });
                }
                Ok(out)
            })
            .ok()?
            .ok()?;

        Some(MetricResult {
            name: "construct_churn".into(),
            display_name: report_display("construct_churn"),
            description: report_description("construct_churn"),
            entry_groups: vec![],
            columns: vec![
                Column::in_report("construct_churn", "kind"),
                Column::in_report("construct_churn", "file"),
                Column::in_report("construct_churn", "changes"),
                Column::in_report("construct_churn", "lines_touched"),
                Column::in_report("construct_churn", "unique_authors"),
                Column::in_report("construct_churn", "last_modified"),
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
        name: "construct_churn".into(),
        display_name: report_display("construct_churn"),
        description: report_description("construct_churn"),
        entry_groups: vec![],
        columns: vec![],
        entries: vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{CodeConstruct, CommitInfo, DiffRecord, FileStatus, ParsedChange};
    use chrono::{FixedOffset, TimeZone};
    use std::sync::Arc;

    fn make_change(
        file: &str,
        oid: &str,
        email: &str,
        constructs: Vec<CodeConstruct>,
    ) -> ParsedChange {
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
            constructs,
        }
    }

    fn store_with(changes: &[ParsedChange]) -> ChangeStore {
        let store = ChangeStore::open_temp().expect("open store");
        store.insert_batch(changes).expect("insert");
        store.finalize_indexes().expect("index");
        store
    }

    fn func(name: &str, start: u32, end: u32) -> CodeConstruct {
        CodeConstruct::Function {
            name: name.into(),
            start_line: start,
            end_line: end,
            enclosing: None,
        }
    }

    #[test]
    fn empty_collector_returns_named_result() {
        let mut coll = ConstructChurnCollector::new();
        let r = coll.finalize();
        assert_eq!(r.name, "construct_churn");
        assert!(r.entries.is_empty());
    }

    #[test]
    fn ts_to_date_known_value() {
        // 2024-06-15 00:00:00 UTC = 1718409600
        let date = ts_to_date(1_718_409_600);
        assert_eq!(date.format("%Y-%m-%d").to_string(), "2024-06-15");
    }

    #[test]
    fn finalize_from_db_groups_per_file_and_qualified_name() {
        let store = store_with(&[
            make_change("a.rs", "c1", "alice@x", vec![func("foo", 1, 10)]),
            make_change("a.rs", "c2", "alice@x", vec![func("foo", 1, 12)]),
            make_change("a.rs", "c3", "bob@x", vec![func("bar", 20, 30)]),
        ]);

        let mut coll = ConstructChurnCollector::new();
        let r = coll
            .finalize_from_db(&store, &crate::metrics::ProgressReporter::new(None))
            .expect("db result");
        assert_eq!(r.entries.len(), 2, "two distinct constructs");
        let foo = r.entries.iter().find(|e| e.key.ends_with("::foo")).unwrap();
        match foo.values.get("changes") {
            Some(MetricValue::Count(n)) => assert_eq!(*n, 2),
            other => panic!("expected Count(2), got {other:?}"),
        }
        // foo only modified by alice -> unique_authors == 1
        match foo.values.get("unique_authors") {
            Some(MetricValue::Count(n)) => assert_eq!(*n, 1),
            other => panic!("expected Count(1), got {other:?}"),
        }
    }

    #[test]
    fn finalize_from_db_excludes_non_source_files() {
        // package-lock.json is filtered by is_source_file.
        let store = store_with(&[
            make_change("package-lock.json", "c1", "alice@x", vec![func("x", 1, 1)]),
            make_change("real.rs", "c2", "alice@x", vec![func("y", 1, 1)]),
        ]);

        let mut coll = ConstructChurnCollector::new();
        let r = coll
            .finalize_from_db(&store, &crate::metrics::ProgressReporter::new(None))
            .expect("db result");
        assert!(r.entries.iter().any(|e| e.key.starts_with("real.rs")));
        assert!(
            !r.entries.iter().any(|e| e.key.contains("package-lock")),
            "non-source file should have been filtered"
        );
    }

    #[test]
    fn finalize_from_db_orders_by_changes_desc() {
        let store = store_with(&[
            make_change("a.rs", "c1", "alice@x", vec![func("once", 1, 1)]),
            make_change("a.rs", "c2", "bob@x", vec![func("often", 1, 1)]),
            make_change("a.rs", "c3", "bob@x", vec![func("often", 1, 1)]),
            make_change("a.rs", "c4", "bob@x", vec![func("often", 1, 1)]),
        ]);

        let mut coll = ConstructChurnCollector::new();
        let r = coll
            .finalize_from_db(&store, &crate::metrics::ProgressReporter::new(None))
            .expect("db result");
        assert!(r.entries.first().unwrap().key.ends_with("::often"));
    }

    #[test]
    fn finalize_from_db_emits_expected_value_keys() {
        let store = store_with(&[make_change(
            "a.rs",
            "c1",
            "alice@x",
            vec![func("foo", 1, 10)],
        )]);

        let mut coll = ConstructChurnCollector::new();
        let r = coll
            .finalize_from_db(&store, &crate::metrics::ProgressReporter::new(None))
            .expect("db result");
        let entry = r.entries.first().unwrap();
        for key in [
            "kind",
            "file",
            "changes",
            "lines_touched",
            "unique_authors",
            "last_modified",
        ] {
            assert!(
                entry.values.contains_key(key),
                "missing value key {key} in construct_churn entry"
            );
        }
    }
}
