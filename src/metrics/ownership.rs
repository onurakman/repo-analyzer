use std::collections::HashMap;

use crate::analysis::source_filter::is_source_file;
use crate::metrics::MetricCollector;
use crate::store::ChangeStore;
use crate::types::{
    Column, MetricEntry, MetricResult, MetricValue, report_description, report_display,
};

pub struct OwnershipCollector;

impl Default for OwnershipCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl OwnershipCollector {
    pub fn new() -> Self {
        Self
    }
}

impl MetricCollector for OwnershipCollector {
    fn name(&self) -> &str {
        "ownership"
    }

    fn finalize(&mut self) -> MetricResult {
        empty_result()
    }

    fn finalize_from_db(
        &mut self,
        store: &ChangeStore,
        _progress: &crate::metrics::ProgressReporter,
    ) -> Option<anyhow::Result<MetricResult>> {
        Some((|| -> anyhow::Result<MetricResult> {
            // Pull (file, email, lines_added) per author, then group in Rust to
            // compute top_author and bus_factor (which require the full author
            // distribution per file — hard to do in plain SQL without window funcs).
            let rows =
                store.with_conn(|conn| -> anyhow::Result<Vec<(String, String, u64)>> {
                    let mut stmt = conn.prepare(
                        // Canonicalize to the HEAD name so a renamed file's
                        // ownership isn't split across old/new paths. (finding #10)
                        "SELECT COALESCE(cp.head_path, ch.file_path) AS canon,
                            ch.email,
                            SUM(ch.additions) AS added
                       FROM non_merge_changes ch
                       LEFT JOIN canonical_path cp ON cp.path = ch.file_path
                      GROUP BY canon, ch.email
                     HAVING canon IN (SELECT file_path FROM live_files)",
                    )?;
                    let iter = stmt.query_map([], |row| {
                        let file: String = row.get(0)?;
                        let email: String = row.get(1)?;
                        let added: i64 = row.get(2)?;
                        Ok((file, email, added as u64))
                    })?;
                    let mut out = Vec::new();
                    for r in iter {
                        out.push(r?);
                    }
                    Ok(out)
                })??;

            // file → (email → lines_added). Keyed by String; SQLite already interns
            // pages so memory pressure here is only from the in-flight row set.
            let mut files: HashMap<String, HashMap<String, u64>> = HashMap::new();
            for (file, email, added) in rows {
                if !is_source_file(&file) {
                    continue;
                }
                *files.entry(file).or_default().entry(email).or_insert(0) += added;
            }

            let mut entries: Vec<MetricEntry> = files
                .into_iter()
                .map(|(file, authors)| {
                    let total_lines: u64 = authors.values().sum();
                    let total_authors = authors.len() as u64;
                    // Pick the author with the most lines; break ties
                    // deterministically by (lines DESC, email ASC). `max_by`
                    // yields the greatest under this total order: compare by
                    // lines, then reverse email so the smallest email wins.
                    let top_author: String = authors
                        .iter()
                        .max_by(|(email_a, lines_a), (email_b, lines_b)| {
                            lines_a.cmp(lines_b).then_with(|| email_b.cmp(email_a))
                        })
                        .map(|(name, _)| name.clone())
                        .unwrap_or_default();
                    let bus_factor = compute_bus_factor(&authors, total_lines);

                    let mut values = HashMap::new();
                    values.insert("total_authors".into(), MetricValue::Count(total_authors));
                    values.insert("bus_factor".into(), MetricValue::Count(bus_factor));
                    values.insert("top_author".into(), MetricValue::Text(top_author));
                    values.insert("total_lines".into(), MetricValue::Count(total_lines));
                    MetricEntry { key: file, values }
                })
                .collect();

            // Deterministic ordering: bus_factor ASC (highest risk first),
            // then total_lines DESC, then key ASC. Without this the order
            // followed HashMap iteration and varied across runs.
            entries.sort_by(|a, b| {
                let count = |e: &MetricEntry, k: &str| match e.values.get(k) {
                    Some(MetricValue::Count(n)) => *n,
                    _ => u64::MAX,
                };
                count(a, "bus_factor")
                    .cmp(&count(b, "bus_factor"))
                    .then_with(|| count(b, "total_lines").cmp(&count(a, "total_lines")))
                    .then_with(|| a.key.cmp(&b.key))
            });

            // Bound output: ownership can otherwise emit one entry per live
            // source file, which is unbounded. The sort puts highest-risk
            // (lowest bus_factor) first, so keeping the leading 500 retains the
            // most-at-risk files.
            entries.truncate(500);

            Ok(MetricResult {
                name: "ownership".into(),
                display_name: report_display("ownership"),
                description: report_description("ownership"),
                entry_groups: vec![],
                columns: vec![
                    Column::in_report("ownership", "total_authors"),
                    Column::in_report("ownership", "bus_factor"),
                    Column::in_report("ownership", "top_author"),
                    Column::in_report("ownership", "total_lines"),
                ],
                entries,
            })
        })())
    }
}

fn compute_bus_factor(authors: &HashMap<String, u64>, total_lines: u64) -> u64 {
    if total_lines == 0 {
        return 0;
    }
    let threshold = total_lines as f64 * 0.5;
    let mut contributions: Vec<u64> = authors.values().copied().collect();
    contributions.sort_unstable_by(|a, b| b.cmp(a));
    let mut accumulated = 0u64;
    for (i, &contrib) in contributions.iter().enumerate() {
        accumulated += contrib;
        if accumulated as f64 > threshold {
            return (i + 1) as u64;
        }
    }
    contributions.len() as u64
}

fn empty_result() -> MetricResult {
    MetricResult {
        name: "ownership".into(),
        display_name: report_display("ownership"),
        description: report_description("ownership"),
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

    fn make_change(file: &str, oid: &str, email: &str, added: u32) -> ParsedChange {
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

    #[test]
    fn empty_collector_returns_named_result() {
        let mut coll = OwnershipCollector::new();
        let r = coll.finalize();
        assert_eq!(r.name, "ownership");
        assert!(r.entries.is_empty());
    }

    #[test]
    fn bus_factor_zero_total_is_zero() {
        let authors: HashMap<String, u64> = HashMap::new();
        assert_eq!(compute_bus_factor(&authors, 0), 0);
    }

    #[test]
    fn bus_factor_single_author_is_one() {
        let mut authors = HashMap::new();
        authors.insert("alice".into(), 100);
        assert_eq!(compute_bus_factor(&authors, 100), 1);
    }

    #[test]
    fn bus_factor_dominant_author_is_one() {
        // 80% from alice — alone she crosses the 50% mark.
        let mut authors = HashMap::new();
        authors.insert("alice".into(), 80);
        authors.insert("bob".into(), 10);
        authors.insert("carol".into(), 10);
        assert_eq!(compute_bus_factor(&authors, 100), 1);
    }

    #[test]
    fn bus_factor_two_equal_authors_is_two() {
        let mut authors = HashMap::new();
        authors.insert("alice".into(), 50);
        authors.insert("bob".into(), 50);
        // alice alone is exactly 50%, not strictly > 50%, so we need both.
        assert_eq!(compute_bus_factor(&authors, 100), 2);
    }

    #[test]
    fn bus_factor_three_balanced_authors_is_two() {
        // Top two combined exceed 50%, top alone does not.
        let mut authors = HashMap::new();
        authors.insert("alice".into(), 40);
        authors.insert("bob".into(), 40);
        authors.insert("carol".into(), 20);
        assert_eq!(compute_bus_factor(&authors, 100), 2);
    }

    #[test]
    fn finalize_from_db_picks_top_author_by_lines_added() {
        let store = store_with(&[
            make_change("a.rs", "c1", "alice@x", 100),
            make_change("a.rs", "c2", "bob@x", 10),
        ]);

        let mut coll = OwnershipCollector::new();
        let r = coll
            .finalize_from_db(&store, &crate::metrics::ProgressReporter::new(None))
            .and_then(|r| r.ok())
            .expect("db result");
        let entry = r.entries.iter().find(|e| e.key == "a.rs").unwrap();
        match entry.values.get("top_author") {
            Some(MetricValue::Text(s)) => assert_eq!(s, "alice@x"),
            other => panic!("expected Text(alice@x), got {other:?}"),
        }
        match entry.values.get("total_authors") {
            Some(MetricValue::Count(n)) => assert_eq!(*n, 2),
            other => panic!("expected Count(2), got {other:?}"),
        }
        match entry.values.get("total_lines") {
            Some(MetricValue::Count(n)) => assert_eq!(*n, 110),
            other => panic!("expected Count(110), got {other:?}"),
        }
    }

    #[test]
    fn finalize_from_db_filters_non_source_files() {
        let store = store_with(&[
            make_change("Cargo.lock", "c1", "alice@x", 100),
            make_change("real.rs", "c2", "alice@x", 100),
        ]);

        let mut coll = OwnershipCollector::new();
        let r = coll
            .finalize_from_db(&store, &crate::metrics::ProgressReporter::new(None))
            .and_then(|r| r.ok())
            .expect("db result");
        assert!(r.entries.iter().any(|e| e.key == "real.rs"));
        assert!(!r.entries.iter().any(|e| e.key == "Cargo.lock"));
    }

    #[test]
    fn finalize_from_db_sorts_by_bus_factor_asc() {
        let store = store_with(&[
            // a.rs: bus_factor 2 (alice 50, bob 50)
            make_change("a.rs", "c1", "alice@x", 50),
            make_change("a.rs", "c2", "bob@x", 50),
            // b.rs: bus_factor 1 (alice 100)
            make_change("b.rs", "c3", "alice@x", 100),
        ]);

        let mut coll = OwnershipCollector::new();
        let r = coll
            .finalize_from_db(&store, &crate::metrics::ProgressReporter::new(None))
            .and_then(|r| r.ok())
            .expect("db result");
        // Highest risk (lowest bus_factor) leads.
        assert_eq!(r.entries[0].key, "b.rs");
        assert_eq!(r.entries[1].key, "a.rs");
    }

    #[test]
    fn finalize_from_db_is_deterministic_for_tied_inputs() {
        // Three files that all tie on bus_factor and total_lines, plus a file
        // with two equal-line authors that ties on top_author. Every ordering
        // and tie-break must resolve identically on each run, so re-running
        // against a fresh store yields byte-identical entry keys and values.
        let build = || {
            let store = store_with(&[
                // Tie on (bus_factor 1, total_lines 100); resolved by key ASC.
                make_change("c.rs", "c1", "zoe@x", 100),
                make_change("a.rs", "c2", "amy@x", 100),
                make_change("b.rs", "c3", "kim@x", 100),
                // Two authors with equal lines: top_author tie-broken by email ASC.
                make_change("d.rs", "c4", "zoe@x", 50),
                make_change("d.rs", "c5", "amy@x", 50),
            ]);
            let mut coll = OwnershipCollector::new();
            coll.finalize_from_db(&store, &crate::metrics::ProgressReporter::new(None))
                .and_then(|r| r.ok())
                .expect("db result")
        };

        let first = build();
        let keys: Vec<String> = first.entries.iter().map(|e| e.key.clone()).collect();
        // a.rs, b.rs, c.rs share (bus_factor 1, total_lines 100) → key ASC;
        // d.rs has bus_factor 2 so it sorts last.
        assert_eq!(keys, vec!["a.rs", "b.rs", "c.rs", "d.rs"]);

        let d = first.entries.iter().find(|e| e.key == "d.rs").unwrap();
        match d.values.get("top_author") {
            // amy@x < zoe@x, so the email-ascending tie-break selects amy@x.
            Some(MetricValue::Text(s)) => assert_eq!(s, "amy@x"),
            other => panic!("expected Text(amy@x), got {other:?}"),
        }

        // Re-running from an independent store must reproduce the same order.
        for _ in 0..5 {
            let again = build();
            let again_keys: Vec<String> = again.entries.iter().map(|e| e.key.clone()).collect();
            assert_eq!(again_keys, keys);
        }
    }

    #[test]
    fn finalize_from_db_caps_entries_at_500() {
        // Build 600 source files. Files 0..=299 get a single author
        // (bus_factor 1, highest risk); files 300..=599 get two equal-line
        // authors (bus_factor 2). The 500-entry cap must keep the 300
        // bus_factor-1 files plus the 200 lowest-key bus_factor-2 files, and
        // drop the rest — never a bus_factor-1 file.
        let mut changes = Vec::new();
        for i in 0..600 {
            let file = format!("f{i:04}.rs");
            if i < 300 {
                changes.push(make_change(&file, &format!("c{i}a"), "alice@x", 100));
            } else {
                changes.push(make_change(&file, &format!("c{i}a"), "alice@x", 50));
                changes.push(make_change(&file, &format!("c{i}b"), "bob@x", 50));
            }
        }

        let store = store_with(&changes);
        let mut coll = OwnershipCollector::new();
        let r = coll
            .finalize_from_db(&store, &crate::metrics::ProgressReporter::new(None))
            .and_then(|r| r.ok())
            .expect("db result");

        assert_eq!(r.entries.len(), 500);
        // Every retained bus_factor-1 file must survive the cap.
        for i in 0..300 {
            let key = format!("f{i:04}.rs");
            assert!(
                r.entries.iter().any(|e| e.key == key),
                "expected {key} (bus_factor 1) to be retained"
            );
        }
        // The retained set is exactly the lowest-bus_factor files: no entry may
        // have a higher bus_factor than any dropped one would.
        let bus_of = |e: &MetricEntry| match e.values.get("bus_factor") {
            Some(MetricValue::Count(n)) => *n,
            _ => u64::MAX,
        };
        assert!(r.entries.iter().all(|e| bus_of(e) <= 2));
    }
}
