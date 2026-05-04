use std::collections::HashMap;

use crate::analysis::source_filter::is_source_file;
use crate::messages;
use crate::metrics::MetricCollector;
use crate::store::ChangeStore;
use crate::types::{
    Column, LocalizedMessage, MetricEntry, MetricResult, MetricValue, report_description,
    report_display,
};

pub struct HotspotsCollector;

impl Default for HotspotsCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl HotspotsCollector {
    pub fn new() -> Self {
        Self
    }
}

impl MetricCollector for HotspotsCollector {
    fn name(&self) -> &str {
        "hotspots"
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
                let mut out: Vec<MetricEntry> = Vec::new();

                // File-level
                let mut stmt = conn.prepare(
                    "SELECT file_path,
                            COUNT(*)             AS changes,
                            COUNT(DISTINCT email) AS authors
                       FROM changes
                      GROUP BY file_path",
                )?;
                let rows = stmt.query_map([], |row| {
                    let file: String = row.get(0)?;
                    let changes: i64 = row.get(1)?;
                    let authors: i64 = row.get(2)?;
                    Ok((file, changes as u64, authors as u64))
                })?;
                for r in rows {
                    let (file, changes, authors) = r?;
                    if !is_source_file(&file) {
                        continue;
                    }
                    let score = changes * authors;
                    let mut values = HashMap::new();
                    values.insert(
                        "level".into(),
                        MetricValue::Message(LocalizedMessage::code(messages::HOTSPOT_LEVEL_FILE)),
                    );
                    values.insert("changes".into(), MetricValue::Count(changes));
                    values.insert("unique_authors".into(), MetricValue::Count(authors));
                    values.insert("score".into(), MetricValue::Count(score));
                    out.push(MetricEntry { key: file, values });
                }

                // Construct-level
                let mut stmt2 = conn.prepare(
                    "SELECT ch.file_path,
                            c.qualified_name,
                            c.kind,
                            COUNT(*)                 AS changes,
                            COUNT(DISTINCT ch.email) AS authors
                       FROM constructs c
                       JOIN changes ch ON c.change_id = ch.id
                      GROUP BY ch.file_path, c.qualified_name, c.kind",
                )?;
                let rows2 = stmt2.query_map([], |row| {
                    let file: String = row.get(0)?;
                    let qn: String = row.get(1)?;
                    let kind: String = row.get(2)?;
                    let changes: i64 = row.get(3)?;
                    let authors: i64 = row.get(4)?;
                    Ok((file, qn, kind, changes as u64, authors as u64))
                })?;
                for r in rows2 {
                    let (file, qn, kind, changes, authors) = r?;
                    let score = changes * authors;
                    let key = format!("{file}::{qn}");
                    let mut values = HashMap::new();
                    values.insert(
                        "level".into(),
                        MetricValue::Message(LocalizedMessage::code(
                            messages::HOTSPOT_LEVEL_CONSTRUCT,
                        )),
                    );
                    values.insert("kind".into(), MetricValue::Text(kind));
                    values.insert("file".into(), MetricValue::Text(file));
                    values.insert("changes".into(), MetricValue::Count(changes));
                    values.insert("unique_authors".into(), MetricValue::Count(authors));
                    values.insert("score".into(), MetricValue::Count(score));
                    out.push(MetricEntry { key, values });
                }

                Ok(out)
            })
            .ok()?
            .ok()?;

        let mut entries = entries;
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
        entries.truncate(500);

        Some(MetricResult {
            name: "hotspots".into(),
            display_name: report_display("hotspots"),
            description: report_description("hotspots"),
            entry_groups: vec![],
            columns: vec![
                Column::in_report("hotspots", "level"),
                Column::in_report("hotspots", "kind"),
                Column::in_report("hotspots", "file"),
                Column::in_report("hotspots", "changes"),
                Column::in_report("hotspots", "unique_authors"),
                Column::in_report("hotspots", "score"),
            ],
            entries,
        })
    }
}

fn empty_result() -> MetricResult {
    MetricResult {
        name: "hotspots".into(),
        display_name: report_display("hotspots"),
        description: report_description("hotspots"),
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

    #[test]
    fn empty_collector_returns_named_result() {
        let mut coll = HotspotsCollector::new();
        let r = coll.finalize();
        assert_eq!(r.name, "hotspots");
        assert!(r.entries.is_empty());
    }

    #[test]
    fn finalize_from_db_emits_file_level_rows_with_score() {
        // 3 changes by 2 distinct authors → score = 3 * 2 = 6.
        let store = store_with(&[
            make_change("a.rs", "c1", "alice@x", vec![]),
            make_change("a.rs", "c2", "bob@x", vec![]),
            make_change("a.rs", "c3", "alice@x", vec![]),
        ]);

        let mut coll = HotspotsCollector::new();
        let r = coll
            .finalize_from_db(&store, &crate::metrics::ProgressReporter::new(None))
            .expect("db result");
        let entry = r.entries.iter().find(|e| e.key == "a.rs").unwrap();
        match entry.values.get("changes") {
            Some(MetricValue::Count(n)) => assert_eq!(*n, 3),
            other => panic!("expected Count(3), got {other:?}"),
        }
        match entry.values.get("unique_authors") {
            Some(MetricValue::Count(n)) => assert_eq!(*n, 2),
            other => panic!("expected Count(2), got {other:?}"),
        }
        match entry.values.get("score") {
            Some(MetricValue::Count(n)) => assert_eq!(*n, 6),
            other => panic!("expected Count(6), got {other:?}"),
        }
        match entry.values.get("level") {
            Some(MetricValue::Message(m)) => assert_eq!(m.code, messages::HOTSPOT_LEVEL_FILE),
            other => panic!("expected file-level Message, got {other:?}"),
        }
    }

    #[test]
    fn finalize_from_db_emits_construct_level_rows() {
        let func = |name: &str, s: u32, e: u32| CodeConstruct::Function {
            name: name.into(),
            start_line: s,
            end_line: e,
            enclosing: None,
        };
        let store = store_with(&[
            make_change("a.rs", "c1", "alice@x", vec![func("foo", 1, 10)]),
            make_change("a.rs", "c2", "bob@x", vec![func("foo", 1, 12)]),
        ]);

        let mut coll = HotspotsCollector::new();
        let r = coll
            .finalize_from_db(&store, &crate::metrics::ProgressReporter::new(None))
            .expect("db result");

        let construct = r
            .entries
            .iter()
            .find(|e| e.key.ends_with("::foo"))
            .expect("construct-level hotspot row missing");
        match construct.values.get("level") {
            Some(MetricValue::Message(m)) => assert_eq!(m.code, messages::HOTSPOT_LEVEL_CONSTRUCT),
            other => panic!("expected construct-level Message, got {other:?}"),
        }
        // Construct-level rows always carry kind + file even though file
        // already lives in the key — keeps writers schema-stable.
        assert!(construct.values.contains_key("kind"));
        assert!(construct.values.contains_key("file"));
    }

    #[test]
    fn finalize_from_db_filters_non_source_files_at_file_level() {
        // package-lock.json is dropped at the file level; construct-level
        // rows for it would never exist (no parser touches it).
        let store = store_with(&[
            make_change("package-lock.json", "c1", "alice@x", vec![]),
            make_change("real.rs", "c2", "alice@x", vec![]),
        ]);

        let mut coll = HotspotsCollector::new();
        let r = coll
            .finalize_from_db(&store, &crate::metrics::ProgressReporter::new(None))
            .expect("db result");
        assert!(r.entries.iter().any(|e| e.key == "real.rs"));
        assert!(!r.entries.iter().any(|e| e.key == "package-lock.json"));
    }

    #[test]
    fn finalize_from_db_sorts_by_score_desc() {
        let store = store_with(&[
            // big.rs: 4 changes × 2 authors = 8
            make_change("big.rs", "c1", "alice@x", vec![]),
            make_change("big.rs", "c2", "bob@x", vec![]),
            make_change("big.rs", "c3", "alice@x", vec![]),
            make_change("big.rs", "c4", "bob@x", vec![]),
            // small.rs: 1 change × 1 author = 1
            make_change("small.rs", "c5", "alice@x", vec![]),
        ]);

        let mut coll = HotspotsCollector::new();
        let r = coll
            .finalize_from_db(&store, &crate::metrics::ProgressReporter::new(None))
            .expect("db result");
        assert_eq!(r.entries[0].key, "big.rs");
    }
}
