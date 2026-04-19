use std::collections::HashMap;

use chrono::{DateTime, NaiveDate, TimeZone, Utc};

use crate::metrics::MetricCollector;
use crate::store::ChangeStore;
use crate::types::{
    Column, MetricEntry, MetricResult, MetricValue, report_description, report_display,
};

pub struct ConstructChurnCollector;

impl Default for ConstructChurnCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl ConstructChurnCollector {
    pub fn new() -> Self {
        Self
    }
}

impl MetricCollector for ConstructChurnCollector {
    fn name(&self) -> &str {
        "construct_churn"
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
                let mut stmt = conn.prepare(
                    "SELECT ch.file_path,
                            c.qualified_name,
                            c.kind,
                            COUNT(*)                  AS changes,
                            SUM(c.lines_touched)      AS lines_touched,
                            COUNT(DISTINCT ch.email)  AS unique_authors,
                            MAX(ch.commit_ts)         AS last_ts
                       FROM constructs c
                       JOIN changes ch ON c.change_id = ch.id
                      GROUP BY ch.file_path, c.qualified_name, c.kind
                      ORDER BY changes DESC
                      LIMIT 500",
                )?;
                let rows = stmt.query_map([], |row| {
                    let file: String = row.get(0)?;
                    let qn: String = row.get(1)?;
                    let kind: String = row.get(2)?;
                    let changes: i64 = row.get(3)?;
                    let lines: i64 = row.get(4)?;
                    let authors: i64 = row.get(5)?;
                    let last_ts: i64 = row.get(6)?;
                    Ok((
                        file,
                        qn,
                        kind,
                        changes as u64,
                        lines as u64,
                        authors as u64,
                        last_ts,
                    ))
                })?;
                let mut out = Vec::new();
                for r in rows {
                    let (file, qn, kind, changes, lines, authors, last_ts) = r?;
                    let mut values = HashMap::new();
                    values.insert("kind".into(), MetricValue::Text(kind));
                    values.insert("file".into(), MetricValue::Text(file.clone()));
                    values.insert("changes".into(), MetricValue::Count(changes));
                    values.insert("lines_touched".into(), MetricValue::Count(lines));
                    values.insert("unique_authors".into(), MetricValue::Count(authors));
                    values.insert(
                        "last_modified".into(),
                        MetricValue::Date(ts_to_date(last_ts)),
                    );
                    out.push(MetricEntry {
                        key: format!("{file}::{qn}"),
                        values,
                    });
                }
                Ok(out)
            })
            .ok()?
            .ok()?;

        Some(MetricResult {
            name: "construct_churn".into(),
            display_name: report_display("construct_churn"),
            description: report_description("construct_churn"),
            entry_groups: vec![],
            columns: vec![
                Column::in_report("construct_churn", "kind"),
                Column::in_report("construct_churn", "file"),
                Column::in_report("construct_churn", "changes"),
                Column::in_report("construct_churn", "lines_touched"),
                Column::in_report("construct_churn", "unique_authors"),
                Column::in_report("construct_churn", "last_modified"),
            ],
            entries,
        })
    }
}

fn ts_to_date(ts: i64) -> NaiveDate {
    let dt: DateTime<Utc> = Utc.timestamp_opt(ts, 0).single().unwrap_or_default();
    dt.date_naive()
}

fn empty_result() -> MetricResult {
    MetricResult {
        name: "construct_churn".into(),
        display_name: report_display("construct_churn"),
        description: report_description("construct_churn"),
        entry_groups: vec![],
        columns: vec![],
        entries: vec![],
    }
}
