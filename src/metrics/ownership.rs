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
    ) -> Option<MetricResult> {
        // Pull (file, email, lines_added) per author, then group in Rust to
        // compute top_author and bus_factor (which require the full author
        // distribution per file — hard to do in plain SQL without window funcs).
        let rows = store
            .with_conn(|conn| -> anyhow::Result<Vec<(String, String, u64)>> {
                let mut stmt = conn.prepare(
                    "SELECT file_path, email, SUM(additions) AS added
                       FROM changes
                      GROUP BY file_path, email",
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
            })
            .ok()?
            .ok()?;

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
                let top_author: String = authors
                    .iter()
                    .max_by_key(|(_, v)| **v)
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

        entries.sort_by(|a, b| {
            let fa = match a.values.get("bus_factor") {
                Some(MetricValue::Count(n)) => *n,
                _ => u64::MAX,
            };
            let fb = match b.values.get("bus_factor") {
                Some(MetricValue::Count(n)) => *n,
                _ => u64::MAX,
            };
            fa.cmp(&fb)
        });

        Some(MetricResult {
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
