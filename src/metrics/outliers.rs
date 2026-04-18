use std::collections::HashMap;

use crate::analysis::source_filter::is_source_file;
use crate::metrics::MetricCollector;
use crate::store::ChangeStore;
use crate::types::{MetricEntry, MetricResult, MetricValue};

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
                values.insert("recommendation".into(), MetricValue::Text(rec));
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
            display_name: "Refactor Candidates".into(),
            description: "Files that are unusually risky on two fronts at once: they change very often AND have many different authors. Both signals together almost always mean accumulated bugs, unclear ownership, and high review cost. These are your top refactor / split candidates.".into(),
            entry_groups: vec![],
            column_labels: vec![],
            columns: vec![
                "change_count".into(),
                "unique_authors".into(),
                "total_churn".into(),
                "recommendation".into(),
            ],
            entries,
        })
    }
}

fn build_recommendation(changes: u64, authors: usize) -> String {
    let high_churn = changes >= HIGH_CHURN_THRESHOLD;
    let high_authors = authors >= HIGH_AUTHORS_THRESHOLD;
    match (high_churn, high_authors) {
        (true, true) => "God file + ownership chaos — split responsibilities".into(),
        (true, false) => "High churn — consider refactoring for stability".into(),
        (false, true) => "Diffuse ownership — clarify module owner".into(),
        (false, false) => "OK".into(),
    }
}

fn empty_result() -> MetricResult {
    MetricResult {
        name: "outliers".into(),
        display_name: "Refactor Candidates".into(),
        description: String::new(),
        entry_groups: vec![],
        column_labels: vec![],
        columns: vec![],
        entries: vec![],
    }
}
