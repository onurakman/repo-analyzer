use std::collections::{HashMap, HashSet};

use crate::analysis::source_filter::is_source_file;
use crate::metrics::MetricCollector;
use crate::store::ChangeStore;
use crate::types::{
    Column, MetricEntry, MetricResult, MetricValue, report_description, report_display,
};

const MODULE_DEPTH: usize = 2;
const MIN_CO_CHANGES: u64 = 3;
const MIN_SCORE: f64 = 0.3;

pub struct ModuleCouplingCollector;

impl Default for ModuleCouplingCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl ModuleCouplingCollector {
    pub fn new() -> Self {
        Self
    }
}

impl MetricCollector for ModuleCouplingCollector {
    fn name(&self) -> &str {
        "module_coupling"
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
        // Rather than do the module-level self-join in SQL (SQLite doesn't have
        // a function to truncate paths to N segments), we stream one row per
        // (commit, file) from the DB and aggregate into module pairs in Rust.
        //
        // Rows are streamed `ORDER BY commit_oid` so all rows for a given commit
        // arrive contiguously. `aggregate_streamed` accumulates only the current
        // commit's module set and, at each group boundary, flushes that commit's
        // contribution into the pair counters and per-module totals before
        // clearing the set. Peak memory is one commit's modules, not the whole
        // history.
        let (co_changes, module_totals) = store
            .with_conn(|conn| -> anyhow::Result<(CoChanges, ModuleTotals)> {
                let mut stmt = conn.prepare(
                    "SELECT commit_oid, file_path FROM non_merge_changes ORDER BY commit_oid",
                )?;
                let rows = stmt.query_map([], |row| {
                    let oid: String = row.get(0)?;
                    let file: String = row.get(1)?;
                    Ok((oid, file))
                })?;

                aggregate_streamed(rows.map(|r| r.map_err(anyhow::Error::from)))
            })??;

        let mut entries: Vec<MetricEntry> = co_changes
            .into_iter()
            .filter_map(|((a, b), count)| {
                if count < MIN_CO_CHANGES {
                    return None;
                }
                let ca = module_totals.get(&a).copied().unwrap_or(1);
                let cb = module_totals.get(&b).copied().unwrap_or(1);
                let score = count as f64 / ca.max(cb) as f64;
                if score < MIN_SCORE {
                    return None;
                }
                let key = format!("{a} <-> {b}");
                let mut values = HashMap::new();
                values.insert("module_a".into(), MetricValue::Text(a));
                values.insert("module_b".into(), MetricValue::Text(b));
                values.insert("co_changes".into(), MetricValue::Count(count));
                values.insert("score".into(), MetricValue::Float(score));
                Some(MetricEntry { key, values })
            })
            .collect();

        entries.sort_by(|a, b| {
            let sa = match a.values.get("score") {
                Some(MetricValue::Float(f)) => *f,
                _ => 0.0,
            };
            let sb = match b.values.get("score") {
                Some(MetricValue::Float(f)) => *f,
                _ => 0.0,
            };
            sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
        });
        entries.truncate(100);

        Ok(MetricResult {
            name: "module_coupling".into(),
            display_name: report_display("module_coupling"),
            description: report_description("module_coupling")
                .with_param("module_depth", MODULE_DEPTH as u64),
            entry_groups: vec![],
            columns: vec![
                Column::in_report("module_coupling", "module_a"),
                Column::in_report("module_coupling", "module_b"),
                Column::in_report("module_coupling", "co_changes"),
                Column::in_report("module_coupling", "score"),
            ],
            entries,
        })
        })())
    }
}

type CoChanges = HashMap<(String, String), u64>;
type ModuleTotals = HashMap<String, u64>;

/// Aggregate `(commit_oid, file_path)` rows that arrive grouped by `commit_oid`
/// into unordered module-pair co-change counts and per-module total distinct
/// commit counts.
///
/// Only the current commit's module set is held in memory; at each group
/// boundary the set is flushed into the counters and cleared. This yields the
/// same counts as materializing the whole `commit_oid -> module-set` map first,
/// as long as all rows for a commit are contiguous in the input.
fn aggregate_streamed<I>(rows: I) -> anyhow::Result<(CoChanges, ModuleTotals)>
where
    I: IntoIterator<Item = anyhow::Result<(String, String)>>,
{
    let mut co_changes: CoChanges = HashMap::new();
    let mut totals: ModuleTotals = HashMap::new();

    // Flush one commit's module set into the pair counters and totals.
    fn flush(modules: &HashSet<String>, co_changes: &mut CoChanges, totals: &mut ModuleTotals) {
        for m in modules {
            *totals.entry(m.clone()).or_insert(0) += 1;
        }
        let list: Vec<&String> = modules.iter().collect();
        for i in 0..list.len() {
            for j in (i + 1)..list.len() {
                let (a, b) = if list[i] < list[j] {
                    (list[i].clone(), list[j].clone())
                } else {
                    (list[j].clone(), list[i].clone())
                };
                *co_changes.entry((a, b)).or_insert(0) += 1;
            }
        }
    }

    let mut current_oid: Option<String> = None;
    let mut current_modules: HashSet<String> = HashSet::new();
    for r in rows {
        let (oid, file) = r?;
        if current_oid.as_deref() != Some(oid.as_str()) {
            // Group boundary: flush the previous commit, then reset.
            if current_oid.is_some() {
                flush(&current_modules, &mut co_changes, &mut totals);
            }
            current_modules.clear();
            current_oid = Some(oid);
        }
        if !is_source_file(&file) {
            continue;
        }
        let module = module_of(&file, MODULE_DEPTH);
        current_modules.insert(module);
    }
    // Flush the final commit's group.
    if current_oid.is_some() {
        flush(&current_modules, &mut co_changes, &mut totals);
    }

    Ok((co_changes, totals))
}

fn module_of(path: &str, depth: usize) -> String {
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() <= 1 {
        return "<root>".into();
    }
    let dirs = &parts[..parts.len() - 1];
    let take = dirs.len().min(depth);
    if take == 0 {
        "<root>".into()
    } else {
        dirs[..take].join("/")
    }
}

fn empty_result() -> MetricResult {
    MetricResult {
        name: "module_coupling".into(),
        display_name: report_display("module_coupling"),
        description: report_description("module_coupling"),
        entry_groups: vec![],
        columns: vec![],
        entries: vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_of_truncates_to_depth() {
        assert_eq!(module_of("src/metrics/foo.rs", 2), "src/metrics");
        assert_eq!(module_of("src/metrics/sub/foo.rs", 2), "src/metrics");
        assert_eq!(module_of("README.md", 2), "<root>");
        assert_eq!(module_of("src/lib.rs", 2), "src");
    }

    /// Reference implementation: materialize the whole commit -> module-set map
    /// first (the pre-streaming behaviour), then derive pair and total counts.
    fn aggregate_materialized(
        rows: &[(&str, &str)],
    ) -> (CoChanges, ModuleTotals) {
        let mut commits: HashMap<String, HashSet<String>> = HashMap::new();
        for (oid, file) in rows {
            if !is_source_file(file) {
                continue;
            }
            commits
                .entry((*oid).to_string())
                .or_default()
                .insert(module_of(file, MODULE_DEPTH));
        }
        let mut totals: ModuleTotals = HashMap::new();
        for modules in commits.values() {
            for m in modules {
                *totals.entry(m.clone()).or_insert(0) += 1;
            }
        }
        let mut co_changes: CoChanges = HashMap::new();
        for modules in commits.values() {
            let list: Vec<&String> = modules.iter().collect();
            for i in 0..list.len() {
                for j in (i + 1)..list.len() {
                    let (a, b) = if list[i] < list[j] {
                        (list[i].clone(), list[j].clone())
                    } else {
                        (list[j].clone(), list[i].clone())
                    };
                    *co_changes.entry((a, b)).or_insert(0) += 1;
                }
            }
        }
        (co_changes, totals)
    }

    #[test]
    fn streamed_aggregation_matches_expected_pair_counts() {
        // Rows grouped by commit_oid (as delivered by `ORDER BY commit_oid`).
        // c1: src/a, src/b            -> pair (src/a, src/b)
        // c2: src/a, src/b, src/c     -> pairs a-b, a-c, b-c
        // c3: src/a (README ignored)  -> no pairs
        // c4: src/b, src/c            -> pair b-c
        let rows = [
            ("c1", "src/a/one.rs"),
            ("c1", "src/a/two.rs"),
            ("c1", "src/b/one.rs"),
            ("c2", "src/a/x.rs"),
            ("c2", "src/b/y.rs"),
            ("c2", "src/c/z.rs"),
            ("c3", "src/a/q.rs"),
            ("c3", "README.md"),
            ("c4", "src/b/m.rs"),
            ("c4", "src/c/n.rs"),
        ];

        let (co_changes, totals) = aggregate_streamed(
            rows.iter()
                .map(|(o, f)| Ok(((*o).to_string(), (*f).to_string()))),
        )
        .unwrap();

        let a = "src/a".to_string();
        let b = "src/b".to_string();
        let c = "src/c".to_string();

        assert_eq!(co_changes.get(&(a.clone(), b.clone())), Some(&2)); // c1, c2
        assert_eq!(co_changes.get(&(a.clone(), c.clone())), Some(&1)); // c2
        assert_eq!(co_changes.get(&(b.clone(), c.clone())), Some(&2)); // c2, c4
        assert_eq!(co_changes.len(), 3);

        assert_eq!(totals.get(&a), Some(&3)); // c1, c2, c3
        assert_eq!(totals.get(&b), Some(&3)); // c1, c2, c4
        assert_eq!(totals.get(&c), Some(&2)); // c2, c4

        // Streaming must produce byte-identical counts to materializing first.
        let (ref_co, ref_totals) = aggregate_materialized(&rows);
        assert_eq!(co_changes, ref_co);
        assert_eq!(totals, ref_totals);
    }
}
