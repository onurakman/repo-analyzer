use std::collections::{HashMap, HashSet};

use crate::analysis::source_filter::is_source_file;
use crate::metrics::MetricCollector;
use crate::store::ChangeStore;
use crate::types::{MetricEntry, MetricResult, MetricValue};

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
    ) -> Option<MetricResult> {
        type ModuleCommits = HashMap<String, HashSet<String>>;
        type ModuleTotals = HashMap<String, u64>;

        // Rather than do the module-level self-join in SQL (SQLite doesn't have
        // a function to truncate paths to N segments), we stream one row per
        // (commit, file) from the DB and aggregate into module pairs in Rust.
        let (commit_modules, module_totals) = store
            .with_conn(|conn| -> anyhow::Result<(ModuleCommits, ModuleTotals)> {
                let mut stmt = conn.prepare("SELECT commit_oid, file_path FROM changes")?;
                let rows = stmt.query_map([], |row| {
                    let oid: String = row.get(0)?;
                    let file: String = row.get(1)?;
                    Ok((oid, file))
                })?;

                let mut commits: HashMap<String, HashSet<String>> = HashMap::new();
                for r in rows {
                    let (oid, file) = r?;
                    if !is_source_file(&file) {
                        continue;
                    }
                    let module = module_of(&file, MODULE_DEPTH);
                    commits.entry(oid).or_default().insert(module);
                }

                // Per-module total distinct commits (for scoring).
                let mut totals: HashMap<String, u64> = HashMap::new();
                for modules in commits.values() {
                    for m in modules {
                        *totals.entry(m.clone()).or_insert(0) += 1;
                    }
                }
                Ok((commits, totals))
            })
            .ok()?
            .ok()?;

        let mut co_changes: HashMap<(String, String), u64> = HashMap::new();
        for modules in commit_modules.values() {
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

        Some(MetricResult {
            name: "module_coupling".into(),
            display_name: "Module Coupling".into(),
            description: format!(
                "Like 'coupling' but at the directory/module level (depth {MODULE_DEPTH}) instead of per-file. Strong coupling between modules that 'shouldn't' be related (e.g. billing ↔ auth) reveals architectural smells: a leaky abstraction, a shared concern that should be extracted, or a piece of logic that lives in the wrong place."
            ),
            entry_groups: vec![],
            column_labels: vec![],
            columns: vec![
                "module_a".into(),
                "module_b".into(),
                "co_changes".into(),
                "score".into(),
            ],
            entries,
        })
    }
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
        display_name: "Module Coupling".into(),
        description: String::new(),
        entry_groups: vec![],
        column_labels: vec![],
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
}
