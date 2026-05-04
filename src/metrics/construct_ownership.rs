use std::collections::HashMap;

use crate::analysis::source_filter::is_source_file;
use crate::metrics::MetricCollector;
use crate::store::ChangeStore;
use crate::types::{
    Column, MetricEntry, MetricResult, MetricValue, report_description, report_display,
};

/// Only the top-N constructs (by total lines touched) are materialized for the
/// per-author breakdown. Beyond that, blowing up memory isn't worth it — the
/// tail is mostly noise for a `bus_factor` metric.
const TOP_CONSTRUCTS: i64 = 500;

pub struct ConstructOwnershipCollector;

impl Default for ConstructOwnershipCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl ConstructOwnershipCollector {
    pub fn new() -> Self {
        Self
    }
}

impl MetricCollector for ConstructOwnershipCollector {
    fn name(&self) -> &str {
        "construct_ownership"
    }

    fn finalize(&mut self) -> MetricResult {
        empty_result()
    }

    fn finalize_from_db(
        &mut self,
        store: &ChangeStore,
        progress: &crate::metrics::ProgressReporter,
    ) -> Option<MetricResult> {
        store
            .with_conn(|conn| -> anyhow::Result<MetricResult> {
                progress.status(&format!(
                    "  construct_ownership: selecting top {TOP_CONSTRUCTS} constructs..."
                ));
                // Step 1: materialize top-N (file, qn, kind) by total touches
                // into a temp table. Bounded size — everything below this is
                // the long tail where bus_factor is uninteresting anyway.
                conn.execute_batch(&format!(
                    "DROP TABLE IF EXISTS __top_constructs;
                     CREATE TEMP TABLE __top_constructs AS
                       SELECT ch.file_path AS file, c.qualified_name AS qn, c.kind AS kind
                         FROM constructs c JOIN changes ch ON c.change_id = ch.id
                        GROUP BY ch.file_path, c.qualified_name, c.kind
                        ORDER BY SUM(c.lines_touched) DESC
                        LIMIT {TOP_CONSTRUCTS};"
                ))?;

                progress.status("  construct_ownership: per-author breakdown...");
                // Step 2: per-author breakdown restricted to those top constructs.
                // Max rows ≈ TOP_CONSTRUCTS × authors, which is a tight bound.
                struct Acc {
                    file: String,
                    kind: String,
                    by_author: HashMap<String, u64>,
                }
                let mut per_key: HashMap<String, Acc> = HashMap::new();

                let mut stmt = conn.prepare(
                    "SELECT ch.file_path, c.qualified_name, c.kind, ch.email,
                            SUM(c.lines_touched) AS touches
                       FROM constructs c
                       JOIN changes ch ON c.change_id = ch.id
                       JOIN __top_constructs t
                         ON t.file = ch.file_path
                        AND t.qn   = c.qualified_name
                        AND t.kind = c.kind
                      GROUP BY ch.file_path, c.qualified_name, c.kind, ch.email",
                )?;
                let rows = stmt.query_map([], |row| {
                    let file: String = row.get(0)?;
                    let qn: String = row.get(1)?;
                    let kind: String = row.get(2)?;
                    let email: String = row.get(3)?;
                    let touches: i64 = row.get(4)?;
                    Ok((file, qn, kind, email, touches.max(0) as u64))
                })?;
                for r in rows {
                    let (file, qn, kind, email, touches) = r?;
                    if !is_source_file(&file) {
                        continue;
                    }
                    let key = format!("{file}::{qn}");
                    let acc = per_key.entry(key).or_insert_with(|| Acc {
                        file: file.clone(),
                        kind: kind.clone(),
                        by_author: HashMap::new(),
                    });
                    *acc.by_author.entry(email).or_insert(0) += touches.max(1);
                }
                conn.execute("DROP TABLE IF EXISTS __top_constructs", [])?;

                let mut entries: Vec<MetricEntry> = per_key
                    .into_iter()
                    .map(|(key, a)| {
                        let total: u64 = a.by_author.values().sum();
                        let total_authors = a.by_author.len() as u64;
                        let (top_author, top_lines): (String, u64) = a
                            .by_author
                            .iter()
                            .max_by_key(|(_, n)| **n)
                            .map(|(e, n)| (e.clone(), *n))
                            .unwrap_or_else(|| ("<unknown>".into(), 0));
                        let top_pct = top_lines
                            .saturating_mul(100)
                            .checked_div(total)
                            .unwrap_or(0)
                            .min(100);
                        let bus_factor = compute_bus_factor(&a.by_author, total);

                        let mut values = HashMap::new();
                        values.insert("kind".into(), MetricValue::Text(a.kind));
                        values.insert("file".into(), MetricValue::Text(a.file));
                        values.insert("top_author".into(), MetricValue::Text(top_author));
                        values.insert("top_pct".into(), MetricValue::Count(top_pct));
                        values.insert("total_authors".into(), MetricValue::Count(total_authors));
                        values.insert("bus_factor".into(), MetricValue::Count(bus_factor));
                        values.insert("touches".into(), MetricValue::Count(total));
                        MetricEntry { key, values }
                    })
                    .collect();

                entries.sort_by(|a, b| {
                    let bf_a = match a.values.get("bus_factor") {
                        Some(MetricValue::Count(n)) => *n,
                        _ => 0,
                    };
                    let bf_b = match b.values.get("bus_factor") {
                        Some(MetricValue::Count(n)) => *n,
                        _ => 0,
                    };
                    if bf_a != bf_b {
                        return bf_a.cmp(&bf_b);
                    }
                    let ta = match a.values.get("touches") {
                        Some(MetricValue::Count(n)) => *n,
                        _ => 0,
                    };
                    let tb = match b.values.get("touches") {
                        Some(MetricValue::Count(n)) => *n,
                        _ => 0,
                    };
                    tb.cmp(&ta)
                });

                Ok(MetricResult {
                    name: "construct_ownership".into(),
                    display_name: report_display("construct_ownership"),
                    description: report_description("construct_ownership"),
                    entry_groups: vec![],
                    columns: vec![
                        Column::in_report("construct_ownership", "kind"),
                        Column::in_report("construct_ownership", "file"),
                        Column::in_report("construct_ownership", "top_author"),
                        Column::in_report("construct_ownership", "top_pct"),
                        Column::in_report("construct_ownership", "total_authors"),
                        Column::in_report("construct_ownership", "bus_factor"),
                        Column::in_report("construct_ownership", "touches"),
                    ],
                    entries,
                })
            })
            .ok()?
            .ok()
    }
}

fn compute_bus_factor(authors: &HashMap<String, u64>, total: u64) -> u64 {
    if total == 0 || authors.is_empty() {
        return 0;
    }
    let mut contributions: Vec<u64> = authors.values().copied().collect();
    contributions.sort_by_key(|c| std::cmp::Reverse(*c));
    let half = total / 2;
    let mut acc = 0u64;
    let mut count = 0u64;
    for c in contributions {
        acc += c;
        count += 1;
        if acc > half {
            break;
        }
    }
    count
}

fn empty_result() -> MetricResult {
    MetricResult {
        name: "construct_ownership".into(),
        display_name: report_display("construct_ownership"),
        description: report_description("construct_ownership"),
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
        let mut coll = ConstructOwnershipCollector::new();
        let r = coll.finalize();
        assert_eq!(r.name, "construct_ownership");
        assert!(r.entries.is_empty());
    }

    #[test]
    fn bus_factor_single_author_is_one() {
        let mut authors = HashMap::new();
        authors.insert("alice".into(), 100);
        assert_eq!(compute_bus_factor(&authors, 100), 1);
    }

    #[test]
    fn bus_factor_two_equal_authors_is_two() {
        // Need two authors for either to cross the half mark.
        let mut authors = HashMap::new();
        authors.insert("alice".into(), 50);
        authors.insert("bob".into(), 50);
        assert_eq!(compute_bus_factor(&authors, 100), 2);
    }

    #[test]
    fn bus_factor_dominant_author_is_one() {
        // Top contributor alone exceeds half — single point of failure.
        let mut authors = HashMap::new();
        authors.insert("alice".into(), 80);
        authors.insert("bob".into(), 10);
        authors.insert("carol".into(), 10);
        assert_eq!(compute_bus_factor(&authors, 100), 1);
    }

    #[test]
    fn bus_factor_zero_total_returns_zero() {
        let authors: HashMap<String, u64> = HashMap::new();
        assert_eq!(compute_bus_factor(&authors, 0), 0);
    }

    #[test]
    fn finalize_from_db_attributes_top_author_per_construct() {
        let store = store_with(&[
            // alice owns "foo" with 3 changes vs bob's 1
            make_change("a.rs", "c1", "alice@x", vec![func("foo", 1, 5)]),
            make_change("a.rs", "c2", "alice@x", vec![func("foo", 1, 5)]),
            make_change("a.rs", "c3", "alice@x", vec![func("foo", 1, 5)]),
            make_change("a.rs", "c4", "bob@x", vec![func("foo", 1, 5)]),
        ]);

        let mut coll = ConstructOwnershipCollector::new();
        let r = coll
            .finalize_from_db(&store, &crate::metrics::ProgressReporter::new(None))
            .expect("db result");
        assert!(!r.entries.is_empty());
        let entry = r
            .entries
            .iter()
            .find(|e| e.key.ends_with("::foo"))
            .expect("foo entry");
        match entry.values.get("top_author") {
            Some(MetricValue::Text(s)) => assert_eq!(s, "alice@x"),
            other => panic!("expected Text(alice@x), got {other:?}"),
        }
        match entry.values.get("total_authors") {
            Some(MetricValue::Count(n)) => assert_eq!(*n, 2),
            other => panic!("expected Count(2), got {other:?}"),
        }
        // bus_factor is 1 because alice dominates 3 of 4 lines touched.
        match entry.values.get("bus_factor") {
            Some(MetricValue::Count(n)) => assert_eq!(*n, 1),
            other => panic!("expected Count(1), got {other:?}"),
        }
        match entry.values.get("top_pct") {
            Some(MetricValue::Count(pct)) => assert!((50..=100).contains(pct)),
            other => panic!("expected Count, got {other:?}"),
        }
    }

    #[test]
    fn finalize_from_db_filters_non_source_files() {
        let store = store_with(&[
            make_change("Cargo.lock", "c1", "alice@x", vec![func("x", 1, 1)]),
            make_change("real.rs", "c2", "alice@x", vec![func("y", 1, 1)]),
        ]);

        let mut coll = ConstructOwnershipCollector::new();
        let r = coll
            .finalize_from_db(&store, &crate::metrics::ProgressReporter::new(None))
            .expect("db result");
        assert!(r.entries.iter().any(|e| e.key.starts_with("real.rs")));
        assert!(!r.entries.iter().any(|e| e.key.contains("Cargo.lock")));
    }

    #[test]
    fn finalize_from_db_emits_expected_value_keys() {
        let store = store_with(&[make_change(
            "a.rs",
            "c1",
            "alice@x",
            vec![func("foo", 1, 10)],
        )]);

        let mut coll = ConstructOwnershipCollector::new();
        let r = coll
            .finalize_from_db(&store, &crate::metrics::ProgressReporter::new(None))
            .expect("db result");
        let entry = r.entries.first().unwrap();
        for key in [
            "kind",
            "file",
            "top_author",
            "top_pct",
            "total_authors",
            "bus_factor",
            "touches",
        ] {
            assert!(
                entry.values.contains_key(key),
                "missing value key {key} in construct_ownership entry"
            );
        }
    }

    #[test]
    fn finalize_from_db_sorts_by_bus_factor_then_touches() {
        let store = store_with(&[
            // single-owner construct (bus_factor 1, fewer touches)
            make_change("a.rs", "c1", "alice@x", vec![func("solo", 1, 5)]),
            // two-author construct (bus_factor 2, more touches)
            make_change("b.rs", "c2", "alice@x", vec![func("shared", 1, 5)]),
            make_change("b.rs", "c3", "bob@x", vec![func("shared", 1, 5)]),
            make_change("b.rs", "c4", "alice@x", vec![func("shared", 1, 5)]),
            make_change("b.rs", "c5", "bob@x", vec![func("shared", 1, 5)]),
        ]);

        let mut coll = ConstructOwnershipCollector::new();
        let r = coll
            .finalize_from_db(&store, &crate::metrics::ProgressReporter::new(None))
            .expect("db result");
        // Rows are sorted bus_factor ASC, then touches DESC; lowest bus
        // factor (highest risk) leads the report.
        assert!(r.entries.first().unwrap().key.ends_with("::solo"));
    }
}
