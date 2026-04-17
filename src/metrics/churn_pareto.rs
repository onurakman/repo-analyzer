use std::collections::HashMap;

use crate::metrics::MetricCollector;
use crate::store::ChangeStore;
use crate::types::{MetricEntry, MetricResult, MetricValue};

pub struct ChurnParetoCollector;

impl Default for ChurnParetoCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl ChurnParetoCollector {
    pub fn new() -> Self {
        Self
    }
}

impl MetricCollector for ChurnParetoCollector {
    fn name(&self) -> &str {
        "churn_pareto"
    }

    fn finalize(&mut self) -> MetricResult {
        empty_result()
    }

    fn finalize_from_db(
        &mut self,
        store: &ChangeStore,
        _progress: &crate::metrics::ProgressReporter,
    ) -> Option<MetricResult> {
        let sorted = store
            .with_conn(|conn| -> anyhow::Result<Vec<(String, u64)>> {
                let mut stmt = conn.prepare(
                    "SELECT file_path, SUM(additions + deletions) AS churn
                       FROM changes
                      GROUP BY file_path
                     HAVING churn > 0
                      ORDER BY churn DESC",
                )?;
                let rows = stmt.query_map([], |row| {
                    let p: String = row.get(0)?;
                    let c: i64 = row.get(1)?;
                    Ok((p, c as u64))
                })?;
                let mut out = Vec::new();
                for r in rows {
                    out.push(r?);
                }
                Ok(out)
            })
            .ok()?
            .ok()?;

        let total_files = sorted.len() as u64;
        let total_churn: u64 = sorted.iter().map(|(_, c)| *c).sum();

        let mut entries: Vec<MetricEntry> = Vec::new();

        if total_churn > 0 {
            let p50 = files_to_reach_pct(&sorted, total_churn, 50);
            let p80 = files_to_reach_pct(&sorted, total_churn, 80);
            let p90 = files_to_reach_pct(&sorted, total_churn, 90);
            let p50_pct = pct(p50, total_files);
            let p80_pct = pct(p80, total_files);
            let p90_pct = pct(p90, total_files);

            let mut values = HashMap::new();
            values.insert("rank".into(), MetricValue::Text("—".into()));
            values.insert("churn".into(), MetricValue::Count(total_churn));
            values.insert(
                "pct_of_total".into(),
                MetricValue::Text(format!("{p50}/{total_files} files = {p50_pct}% reach 50%")),
            );
            values.insert(
                "cumulative_pct".into(),
                MetricValue::Text(format!(
                    "{p80}/{total_files} = {p80_pct}% → 80% | {p90}/{total_files} = {p90_pct}% → 90%"
                )),
            );
            entries.push(MetricEntry {
                key: "<summary>".into(),
                values,
            });
        }

        let mut cum: u64 = 0;
        for (rank, (path, churn)) in sorted.iter().enumerate().take(50) {
            cum += *churn;
            let file_pct = (*churn * 100).checked_div(total_churn).unwrap_or(0);
            let cum_pct = (cum * 100).checked_div(total_churn).unwrap_or(0);
            let mut values = HashMap::new();
            values.insert("rank".into(), MetricValue::Count((rank as u64) + 1));
            values.insert("churn".into(), MetricValue::Count(*churn));
            values.insert("pct_of_total".into(), MetricValue::Count(file_pct));
            values.insert("cumulative_pct".into(), MetricValue::Count(cum_pct));
            entries.push(MetricEntry {
                key: path.clone(),
                values,
            });
        }

        Some(MetricResult {
            name: "churn_pareto".into(),
            display_name: "Churn Distribution (Pareto)".into(),
            description: "Distribution of churn across files (the 80/20 rule applied to your codebase). Almost always a small fraction of files takes the lion's share of all changes. The summary row shows exactly how concentrated yours is — that small fraction is where to focus refactoring effort, code review attention, and test coverage.".into(),
            entry_groups: vec![],
            column_labels: vec![],
            columns: vec![
                "rank".into(),
                "churn".into(),
                "pct_of_total".into(),
                "cumulative_pct".into(),
            ],
            entries,
        })
    }
}

fn files_to_reach_pct(sorted: &[(String, u64)], total: u64, target_pct: u64) -> u64 {
    if total == 0 {
        return 0;
    }
    let target = total * target_pct / 100;
    let mut cum = 0u64;
    for (i, (_, c)) in sorted.iter().enumerate() {
        cum += *c;
        if cum >= target {
            return (i as u64) + 1;
        }
    }
    sorted.len() as u64
}

fn pct(part: u64, whole: u64) -> u64 {
    if whole == 0 { 0 } else { part * 100 / whole }
}

fn empty_result() -> MetricResult {
    MetricResult {
        name: "churn_pareto".into(),
        display_name: "Churn Distribution (Pareto)".into(),
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

    fn s(items: &[(&str, u64)]) -> Vec<(String, u64)> {
        items.iter().map(|(p, c)| (p.to_string(), *c)).collect()
    }

    #[test]
    fn pareto_classic_80_20() {
        let sorted = s(&[("a", 80), ("b", 5), ("c", 5), ("d", 5), ("e", 5)]);
        assert_eq!(files_to_reach_pct(&sorted, 100, 50), 1);
        assert_eq!(files_to_reach_pct(&sorted, 100, 80), 1);
        assert_eq!(files_to_reach_pct(&sorted, 100, 90), 3);
    }

    #[test]
    fn pct_handles_zero_whole() {
        assert_eq!(pct(5, 0), 0);
    }
}
