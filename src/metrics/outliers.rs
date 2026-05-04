use std::collections::HashMap;

use crate::analysis::source_filter::is_source_file;
use crate::messages;
use crate::metrics::MetricCollector;
use crate::store::ChangeStore;
use crate::types::{
    Column, LocalizedMessage, MetricEntry, MetricResult, MetricValue, Severity, report_description,
    report_display,
};

pub struct OutliersCollector;

impl Default for OutliersCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl OutliersCollector {
    pub fn new() -> Self {
        Self
    }
}

const HIGH_CHURN_THRESHOLD: u64 = 100;
const HIGH_AUTHORS_THRESHOLD: usize = 5;

/// Safety cap on how many candidate rows we pull out of SQL before the
/// Rust-side source-file filter. The SQL already rejects files that don't
/// meet at least one threshold, so in practice this cap is never hit on
/// normal repos — it just prevents a pathological case from unbounded
/// materialization.
const MAX_CANDIDATES: i64 = 5_000;

impl MetricCollector for OutliersCollector {
    fn name(&self) -> &str {
        "outliers"
    }

    fn finalize(&mut self) -> MetricResult {
        empty_result()
    }

    fn finalize_from_db(
        &mut self,
        store: &ChangeStore,
        _progress: &crate::metrics::ProgressReporter,
    ) -> Option<MetricResult> {
        // Push both threshold filters into SQL — any file that fails both is
        // dead weight we don't want to drag into Rust. Status-deleted files
        // are already excluded via the HAVING clause. ORDER BY + LIMIT caps
        // the pull in case a pathological repo has millions of candidate
        // files; the normal path never hits the limit.
        let rows = store
            .with_conn(|conn| -> anyhow::Result<Vec<(String, u64, u64, u64)>> {
                let mut stmt = conn.prepare(
                    "SELECT file_path,
                            COUNT(*)                        AS change_count,
                            COUNT(DISTINCT email)           AS unique_authors,
                            SUM(additions + deletions)     AS total_churn
                       FROM changes
                      GROUP BY file_path
                     HAVING MAX(CASE WHEN status = 2 THEN 1 ELSE 0 END) = 0
                        AND (COUNT(*) >= ?1 OR COUNT(DISTINCT email) >= ?2)
                      ORDER BY change_count DESC
                      LIMIT ?3",
                )?;
                let iter = stmt.query_map(
                    rusqlite::params![
                        HIGH_CHURN_THRESHOLD as i64,
                        HIGH_AUTHORS_THRESHOLD as i64,
                        MAX_CANDIDATES,
                    ],
                    |row| {
                        let file: String = row.get(0)?;
                        let cc: i64 = row.get(1)?;
                        let ua: i64 = row.get(2)?;
                        let tc: i64 = row.get(3)?;
                        Ok((file, cc as u64, ua as u64, tc as u64))
                    },
                )?;
                let mut out = Vec::new();
                for r in iter {
                    out.push(r?);
                }
                Ok(out)
            })
            .ok()?
            .ok()?;

        // SQL already enforced the threshold; only the Rust-only
        // `is_source_file` check remains here.
        let mut entries: Vec<MetricEntry> = rows
            .into_iter()
            .filter(|(file, _, _, _)| is_source_file(file))
            .map(|(file, cc, ua, tc)| {
                let rec = build_recommendation(cc, ua as usize);
                let mut values = HashMap::new();
                values.insert("change_count".into(), MetricValue::Count(cc));
                values.insert("unique_authors".into(), MetricValue::Count(ua));
                values.insert("total_churn".into(), MetricValue::Count(tc));
                values.insert("recommendation".into(), MetricValue::Message(rec));
                MetricEntry { key: file, values }
            })
            .collect();

        entries.sort_by(|a, b| {
            let ca = match a.values.get("change_count") {
                Some(MetricValue::Count(n)) => *n,
                _ => 0,
            };
            let cb = match b.values.get("change_count") {
                Some(MetricValue::Count(n)) => *n,
                _ => 0,
            };
            cb.cmp(&ca)
        });

        Some(MetricResult {
            name: "outliers".into(),
            display_name: report_display("outliers"),
            description: report_description("outliers"),
            entry_groups: vec![],
            columns: vec![
                Column::in_report("outliers", "change_count"),
                Column::in_report("outliers", "unique_authors"),
                Column::in_report("outliers", "total_churn"),
                Column::in_report("outliers", "recommendation"),
            ],
            entries,
        })
    }
}

fn build_recommendation(changes: u64, authors: usize) -> LocalizedMessage {
    let high_churn = changes >= HIGH_CHURN_THRESHOLD;
    let high_authors = authors >= HIGH_AUTHORS_THRESHOLD;
    let (code, severity) = match (high_churn, high_authors) {
        (true, true) => (
            messages::OUTLIERS_RECOMMENDATION_GOD_FILE,
            Some(Severity::Error),
        ),
        (true, false) => (
            messages::OUTLIERS_RECOMMENDATION_HIGH_CHURN,
            Some(Severity::Warning),
        ),
        (false, true) => (
            messages::OUTLIERS_RECOMMENDATION_DIFFUSE_OWNERSHIP,
            Some(Severity::Warning),
        ),
        (false, false) => (messages::OUTLIERS_RECOMMENDATION_OK, None),
    };
    let mut msg = LocalizedMessage::code(code)
        .with_param("changes", changes)
        .with_param("authors", authors as u64);
    if let Some(s) = severity {
        msg = msg.with_severity(s);
    }
    msg
}

fn empty_result() -> MetricResult {
    MetricResult {
        name: "outliers".into(),
        display_name: report_display("outliers"),
        description: report_description("outliers"),
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

    fn make_change(file: &str, oid: &str, email: &str, status: FileStatus) -> ParsedChange {
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
    fn build_recommendation_god_file_when_both_thresholds_breached() {
        let msg = build_recommendation(HIGH_CHURN_THRESHOLD, HIGH_AUTHORS_THRESHOLD);
        assert_eq!(msg.code, messages::OUTLIERS_RECOMMENDATION_GOD_FILE);
        assert_eq!(msg.severity, Some(Severity::Error));
        assert!(msg.params.contains_key("changes"));
        assert!(msg.params.contains_key("authors"));
    }

    #[test]
    fn build_recommendation_high_churn_only() {
        let msg = build_recommendation(HIGH_CHURN_THRESHOLD, 1);
        assert_eq!(msg.code, messages::OUTLIERS_RECOMMENDATION_HIGH_CHURN);
        assert_eq!(msg.severity, Some(Severity::Warning));
    }

    #[test]
    fn build_recommendation_diffuse_ownership_only() {
        let msg = build_recommendation(1, HIGH_AUTHORS_THRESHOLD);
        assert_eq!(
            msg.code,
            messages::OUTLIERS_RECOMMENDATION_DIFFUSE_OWNERSHIP
        );
        assert_eq!(msg.severity, Some(Severity::Warning));
    }

    #[test]
    fn build_recommendation_ok_carries_no_severity() {
        let msg = build_recommendation(1, 1);
        assert_eq!(msg.code, messages::OUTLIERS_RECOMMENDATION_OK);
        assert!(msg.severity.is_none());
    }

    #[test]
    fn empty_collector_returns_named_result() {
        let mut coll = OutliersCollector::new();
        let r = coll.finalize();
        assert_eq!(r.name, "outliers");
        assert!(r.entries.is_empty());
    }

    #[test]
    fn finalize_from_db_returns_empty_for_below_threshold_files() {
        // 5 changes by 2 authors — both well below thresholds.
        let store = store_with(&[
            make_change("a.rs", "c1", "alice@x", FileStatus::Modified),
            make_change("a.rs", "c2", "bob@x", FileStatus::Modified),
            make_change("a.rs", "c3", "alice@x", FileStatus::Modified),
            make_change("a.rs", "c4", "alice@x", FileStatus::Modified),
            make_change("a.rs", "c5", "alice@x", FileStatus::Modified),
        ]);

        let mut coll = OutliersCollector::new();
        let r = coll
            .finalize_from_db(&store, &crate::metrics::ProgressReporter::new(None))
            .expect("db result");
        assert!(
            r.entries.is_empty(),
            "no outliers expected below thresholds"
        );
    }

    #[test]
    fn finalize_from_db_emits_high_churn_outlier() {
        let mut changes = vec![];
        for i in 0..HIGH_CHURN_THRESHOLD {
            changes.push(make_change(
                "noisy.rs",
                &format!("c{i}"),
                "alice@x",
                FileStatus::Modified,
            ));
        }
        let store = store_with(&changes);

        let mut coll = OutliersCollector::new();
        let r = coll
            .finalize_from_db(&store, &crate::metrics::ProgressReporter::new(None))
            .expect("db result");
        let entry = r
            .entries
            .iter()
            .find(|e| e.key == "noisy.rs")
            .expect("noisy.rs row missing");
        match entry.values.get("recommendation") {
            Some(MetricValue::Message(m)) => {
                assert_eq!(m.code, messages::OUTLIERS_RECOMMENDATION_HIGH_CHURN);
            }
            other => panic!("expected high churn recommendation, got {other:?}"),
        }
    }

    #[test]
    fn finalize_from_db_skips_deleted_files() {
        let mut changes = vec![];
        for i in 0..HIGH_CHURN_THRESHOLD {
            changes.push(make_change(
                "gone.rs",
                &format!("c{i}"),
                "alice@x",
                FileStatus::Modified,
            ));
        }
        // Final entry marks the file as deleted — should be filtered out.
        changes.push(make_change(
            "gone.rs",
            "cdel",
            "alice@x",
            FileStatus::Deleted,
        ));
        let store = store_with(&changes);

        let mut coll = OutliersCollector::new();
        let r = coll
            .finalize_from_db(&store, &crate::metrics::ProgressReporter::new(None))
            .expect("db result");
        assert!(
            !r.entries.iter().any(|e| e.key == "gone.rs"),
            "deleted files must not appear in outliers"
        );
    }

    #[test]
    fn finalize_from_db_filters_non_source_files() {
        // Even if package-lock.json crosses thresholds, is_source_file drops it.
        let mut changes = vec![];
        for i in 0..HIGH_CHURN_THRESHOLD {
            changes.push(make_change(
                "package-lock.json",
                &format!("c{i}"),
                "alice@x",
                FileStatus::Modified,
            ));
        }
        for i in 0..HIGH_CHURN_THRESHOLD {
            changes.push(make_change(
                "real.rs",
                &format!("d{i}"),
                "alice@x",
                FileStatus::Modified,
            ));
        }
        let store = store_with(&changes);

        let mut coll = OutliersCollector::new();
        let r = coll
            .finalize_from_db(&store, &crate::metrics::ProgressReporter::new(None))
            .expect("db result");
        assert!(r.entries.iter().any(|e| e.key == "real.rs"));
        assert!(!r.entries.iter().any(|e| e.key == "package-lock.json"));
    }
}
