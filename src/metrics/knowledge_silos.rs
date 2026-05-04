use std::collections::HashMap;

use chrono::{DateTime, NaiveDate, TimeZone, Utc};

use crate::analysis::source_filter::is_source_file;
use crate::messages;
use crate::metrics::MetricCollector;
use crate::store::ChangeStore;
use crate::types::{
    Column, LocalizedMessage, MetricEntry, MetricResult, MetricValue, Severity, report_description,
    report_display,
};

const SILO_PCT: u64 = 80;
const IDLE_DAYS: i64 = 180;

/// Only evaluate the top-N most-written files for silo risk; the rest are
/// negligible in line volume and dwarf memory if we load everything.
const TOP_FILES: i64 = 2000;

pub struct KnowledgeSilosCollector;

impl Default for KnowledgeSilosCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl KnowledgeSilosCollector {
    pub fn new() -> Self {
        Self
    }
}

impl MetricCollector for KnowledgeSilosCollector {
    fn name(&self) -> &str {
        "knowledge_silos"
    }

    fn finalize(&mut self) -> MetricResult {
        empty_result()
    }

    fn finalize_from_db(
        &mut self,
        store: &ChangeStore,
        _progress: &crate::metrics::ProgressReporter,
    ) -> Option<MetricResult> {
        let rows = store
            .with_conn(|conn| -> anyhow::Result<Vec<(String, String, u64, i64)>> {
                conn.execute_batch(&format!(
                    "DROP TABLE IF EXISTS __silo_files;
                     CREATE TEMP TABLE __silo_files AS
                       SELECT file_path AS file
                         FROM changes
                        GROUP BY file_path
                        ORDER BY SUM(additions) DESC
                        LIMIT {};",
                    TOP_FILES
                ))?;
                let mut stmt = conn.prepare(
                    "SELECT ch.file_path,
                            ch.email,
                            SUM(ch.additions)   AS added,
                            MAX(ch.commit_ts)   AS last_ts
                       FROM changes ch
                       JOIN __silo_files t ON t.file = ch.file_path
                      GROUP BY ch.file_path, ch.email",
                )?;
                let iter = stmt.query_map([], |row| {
                    let file: String = row.get(0)?;
                    let email: String = row.get(1)?;
                    let added: i64 = row.get(2)?;
                    let last_ts: i64 = row.get(3)?;
                    Ok((file, email, added as u64, last_ts))
                })?;
                let mut out = Vec::new();
                for r in iter {
                    out.push(r?);
                }
                conn.execute("DROP TABLE IF EXISTS __silo_files", [])?;
                Ok(out)
            })
            .ok()?
            .ok()?;

        struct FileAcc {
            lines_per_author: HashMap<String, u64>,
            last_per_author: HashMap<String, i64>,
        }
        let mut per_file: HashMap<String, FileAcc> = HashMap::new();
        for (file, email, added, last_ts) in rows {
            if !is_source_file(&file) {
                continue;
            }
            let acc = per_file.entry(file).or_insert_with(|| FileAcc {
                lines_per_author: HashMap::new(),
                last_per_author: HashMap::new(),
            });
            *acc.lines_per_author.entry(email.clone()).or_insert(0) += added;
            let slot = acc.last_per_author.entry(email).or_insert(last_ts);
            if last_ts > *slot {
                *slot = last_ts;
            }
        }

        let now = Utc::now().timestamp();
        let mut entries: Vec<MetricEntry> = Vec::new();

        for (path, acc) in per_file {
            let total_lines: u64 = acc.lines_per_author.values().sum();
            if total_lines == 0 {
                continue;
            }
            let Some((owner_email, owner_lines)) =
                acc.lines_per_author.iter().max_by_key(|(_, n)| **n)
            else {
                continue;
            };
            let ownership_pct = (owner_lines * 100 / total_lines).min(100);
            let is_silo = ownership_pct >= SILO_PCT;
            if !is_silo {
                continue;
            }
            let owner_last = acc.last_per_author.get(owner_email).copied().unwrap_or(now);
            let owner_idle_days = ((now - owner_last) / 86_400).max(0);
            let owner_inactive = owner_idle_days >= IDLE_DAYS;
            let risk = if owner_inactive {
                LocalizedMessage::code(messages::KNOWLEDGE_SILO_RISK_AT_RISK)
                    .with_severity(Severity::Error)
                    .with_param("idle_days", owner_idle_days)
                    .with_param("ownership_pct", ownership_pct)
            } else {
                LocalizedMessage::code(messages::KNOWLEDGE_SILO_RISK_SINGLE_OWNER)
                    .with_severity(Severity::Warning)
                    .with_param("ownership_pct", ownership_pct)
            };

            let mut values = HashMap::new();
            values.insert("owner".into(), MetricValue::Text(owner_email.clone()));
            values.insert("ownership_pct".into(), MetricValue::Count(ownership_pct));
            values.insert(
                "owner_last_touch".into(),
                MetricValue::Date(ts_to_date(owner_last)),
            );
            values.insert(
                "owner_idle_days".into(),
                MetricValue::Count(owner_idle_days as u64),
            );
            values.insert("total_lines".into(), MetricValue::Count(total_lines));
            values.insert("risk".into(), MetricValue::Message(risk));
            entries.push(MetricEntry { key: path, values });
        }

        entries.sort_by(|a, b| {
            let ra = risk_rank(a);
            let rb = risk_rank(b);
            if ra != rb {
                return rb.cmp(&ra);
            }
            let ia = match a.values.get("owner_idle_days") {
                Some(MetricValue::Count(n)) => *n,
                _ => 0,
            };
            let ib = match b.values.get("owner_idle_days") {
                Some(MetricValue::Count(n)) => *n,
                _ => 0,
            };
            ib.cmp(&ia)
        });
        entries.truncate(150);

        Some(MetricResult {
            name: "knowledge_silos".into(),
            display_name: report_display("knowledge_silos"),
            description: report_description("knowledge_silos")
                .with_param("silo_pct", SILO_PCT)
                .with_param("idle_days", IDLE_DAYS),
            entry_groups: vec![],
            columns: vec![
                Column::in_report("knowledge_silos", "owner"),
                Column::in_report("knowledge_silos", "ownership_pct"),
                Column::in_report("knowledge_silos", "owner_last_touch"),
                Column::in_report("knowledge_silos", "owner_idle_days"),
                Column::in_report("knowledge_silos", "total_lines"),
                Column::in_report("knowledge_silos", "risk"),
            ],
            entries,
        })
    }
}

fn risk_rank(entry: &MetricEntry) -> u8 {
    match entry.values.get("risk") {
        Some(MetricValue::Message(m)) if m.code == messages::KNOWLEDGE_SILO_RISK_AT_RISK => 2,
        Some(MetricValue::Message(m)) if m.code == messages::KNOWLEDGE_SILO_RISK_SINGLE_OWNER => 1,
        _ => 0,
    }
}

fn ts_to_date(ts: i64) -> NaiveDate {
    let dt: DateTime<Utc> = Utc.timestamp_opt(ts, 0).single().unwrap_or_default();
    dt.date_naive()
}

fn empty_result() -> MetricResult {
    MetricResult {
        name: "knowledge_silos".into(),
        display_name: report_display("knowledge_silos"),
        description: report_description("knowledge_silos"),
        entry_groups: vec![],
        columns: vec![],
        entries: vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{CommitInfo, DiffRecord, FileStatus, ParsedChange};
    use chrono::{DateTime, FixedOffset};
    use std::sync::Arc;

    fn make_change_at(
        file: &str,
        oid: &str,
        email: &str,
        added: u32,
        ts: DateTime<FixedOffset>,
    ) -> ParsedChange {
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
                additions: added,
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

    /// Recent timestamp anchored at the current wall clock so the owner
    /// counts as active in `finalize_from_db`'s `IDLE_DAYS` window.
    fn recent() -> DateTime<FixedOffset> {
        Utc::now().with_timezone(&FixedOffset::east_opt(0).unwrap()) - chrono::Duration::days(1)
    }

    /// Timestamp older than `IDLE_DAYS` so the owner counts as inactive.
    fn ancient() -> DateTime<FixedOffset> {
        Utc::now().with_timezone(&FixedOffset::east_opt(0).unwrap())
            - chrono::Duration::days(IDLE_DAYS + 30)
    }

    #[test]
    fn empty_collector_returns_named_result() {
        let mut coll = KnowledgeSilosCollector::new();
        let r = coll.finalize();
        assert_eq!(r.name, "knowledge_silos");
        assert!(r.entries.is_empty());
    }

    #[test]
    fn risk_rank_orders_at_risk_above_single_owner() {
        let make = |code: &str| {
            let mut values = HashMap::new();
            values.insert(
                "risk".into(),
                MetricValue::Message(LocalizedMessage::code(code)),
            );
            MetricEntry {
                key: "k".into(),
                values,
            }
        };
        let at_risk = make(messages::KNOWLEDGE_SILO_RISK_AT_RISK);
        let single = make(messages::KNOWLEDGE_SILO_RISK_SINGLE_OWNER);
        let other = MetricEntry {
            key: "k".into(),
            values: HashMap::new(),
        };
        assert_eq!(risk_rank(&at_risk), 2);
        assert_eq!(risk_rank(&single), 1);
        assert_eq!(risk_rank(&other), 0);
    }

    #[test]
    fn ts_to_date_known_value() {
        // 2024-06-15 00:00:00 UTC = 1718409600
        assert_eq!(
            ts_to_date(1_718_409_600).format("%Y-%m-%d").to_string(),
            "2024-06-15"
        );
    }

    #[test]
    fn finalize_from_db_drops_non_silo_files() {
        // Two authors at 50/50 — ownership_pct = 50, below SILO_PCT (80).
        let store = store_with(&[
            make_change_at("a.rs", "c1", "alice@x", 50, recent()),
            make_change_at("a.rs", "c2", "bob@x", 50, recent()),
        ]);

        let mut coll = KnowledgeSilosCollector::new();
        let r = coll
            .finalize_from_db(&store, &crate::metrics::ProgressReporter::new(None))
            .expect("db result");
        assert!(
            r.entries.is_empty(),
            "files below SILO_PCT must not appear: {:?}",
            r.entries
        );
    }

    #[test]
    fn finalize_from_db_flags_single_owner_silo_when_active() {
        // alice owns 100 of 100 added lines, recently — single_owner risk.
        let store = store_with(&[make_change_at("a.rs", "c1", "alice@x", 100, recent())]);

        let mut coll = KnowledgeSilosCollector::new();
        let r = coll
            .finalize_from_db(&store, &crate::metrics::ProgressReporter::new(None))
            .expect("db result");
        let entry = r.entries.iter().find(|e| e.key == "a.rs").unwrap();
        match entry.values.get("risk") {
            Some(MetricValue::Message(m)) => {
                assert_eq!(m.code, messages::KNOWLEDGE_SILO_RISK_SINGLE_OWNER);
                assert_eq!(m.severity, Some(Severity::Warning));
            }
            other => panic!("expected risk Message, got {other:?}"),
        }
        match entry.values.get("ownership_pct") {
            Some(MetricValue::Count(n)) => assert_eq!(*n, 100),
            other => panic!("expected Count(100), got {other:?}"),
        }
    }

    #[test]
    fn finalize_from_db_flags_at_risk_when_owner_idle() {
        // alice owns everything but her last touch is well past IDLE_DAYS.
        let store = store_with(&[make_change_at("a.rs", "c1", "alice@x", 100, ancient())]);

        let mut coll = KnowledgeSilosCollector::new();
        let r = coll
            .finalize_from_db(&store, &crate::metrics::ProgressReporter::new(None))
            .expect("db result");
        let entry = r.entries.iter().find(|e| e.key == "a.rs").unwrap();
        match entry.values.get("risk") {
            Some(MetricValue::Message(m)) => {
                assert_eq!(m.code, messages::KNOWLEDGE_SILO_RISK_AT_RISK);
                assert_eq!(m.severity, Some(Severity::Error));
            }
            other => panic!("expected at-risk Message, got {other:?}"),
        }
        match entry.values.get("owner_idle_days") {
            Some(MetricValue::Count(n)) => {
                assert!(*n >= IDLE_DAYS as u64, "expected idle >= IDLE_DAYS");
            }
            other => panic!("expected Count, got {other:?}"),
        }
    }

    #[test]
    fn finalize_from_db_filters_non_source_files() {
        let store = store_with(&[
            make_change_at("Cargo.lock", "c1", "alice@x", 100, recent()),
            make_change_at("real.rs", "c2", "alice@x", 100, recent()),
        ]);

        let mut coll = KnowledgeSilosCollector::new();
        let r = coll
            .finalize_from_db(&store, &crate::metrics::ProgressReporter::new(None))
            .expect("db result");
        assert!(r.entries.iter().any(|e| e.key == "real.rs"));
        assert!(!r.entries.iter().any(|e| e.key == "Cargo.lock"));
    }

    #[test]
    fn finalize_from_db_orders_at_risk_before_single_owner() {
        let store = store_with(&[
            // single_owner — alice still active
            make_change_at("active.rs", "c1", "alice@x", 100, recent()),
            // at_risk — alice idle
            make_change_at("idle.rs", "c2", "bob@x", 100, ancient()),
        ]);

        let mut coll = KnowledgeSilosCollector::new();
        let r = coll
            .finalize_from_db(&store, &crate::metrics::ProgressReporter::new(None))
            .expect("db result");
        // at-risk row should come first (rank 2 > rank 1)
        assert_eq!(r.entries[0].key, "idle.rs");
        assert_eq!(r.entries[1].key, "active.rs");
    }
}
