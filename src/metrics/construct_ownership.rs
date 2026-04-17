use std::collections::HashMap;

use crate::metrics::MetricCollector;
use crate::store::ChangeStore;
use crate::types::{MetricEntry, MetricResult, MetricValue};

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
                        let top_pct = if total == 0 {
                            0
                        } else {
                            (top_lines * 100 / total).min(100)
                        };
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
                    display_name: "Function Ownership".into(),
                    description: "Function-level ownership and bus factor — finer than file-level ownership. Even if a file has many contributors, individual functions inside it may have only one author. Bus factor of 1 on a critical function (e.g. payment processing) is a real risk even when the surrounding file looks healthy.".into(),
                    entry_groups: vec![],
                    column_labels: vec![],
                    columns: vec![
                        "kind".into(),
                        "file".into(),
                        "top_author".into(),
                        "top_pct".into(),
                        "total_authors".into(),
                        "bus_factor".into(),
                        "touches".into(),
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
    contributions.sort_by(|a, b| b.cmp(a));
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
        display_name: "Function Ownership".into(),
        description: String::new(),
        entry_groups: vec![],
        column_labels: vec![],
        columns: vec![],
        entries: vec![],
    }
}
