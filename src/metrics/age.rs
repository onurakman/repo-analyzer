use std::collections::HashMap;

use chrono::{DateTime, NaiveDate, TimeZone, Utc};

use crate::metrics::MetricCollector;
use crate::store::ChangeStore;
use crate::types::{
    Column, MetricEntry, MetricResult, MetricValue, report_description, report_display,
};

pub struct AgeCollector;

impl Default for AgeCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl AgeCollector {
    pub fn new() -> Self {
        Self
    }
}

impl MetricCollector for AgeCollector {
    fn name(&self) -> &str {
        "age"
    }

    fn finalize(&mut self) -> MetricResult {
        empty_result()
    }

    fn finalize_from_db(
        &mut self,
        store: &ChangeStore,
        _progress: &crate::metrics::ProgressReporter,
    ) -> Option<MetricResult> {
        // status=2 means Deleted; skip files that were ever deleted.
        let entries = store
            .with_conn(|conn| -> anyhow::Result<Vec<MetricEntry>> {
                let mut stmt = conn.prepare(
                    "SELECT file_path,
                            MIN(commit_ts)                AS first_ts,
                            MAX(commit_ts)                AS last_ts,
                            COUNT(*)                      AS change_count,
                            MAX(CASE WHEN status = 2 THEN 1 ELSE 0 END) AS ever_deleted
                       FROM changes
                      GROUP BY file_path",
                )?;
                let rows = stmt.query_map([], |row| {
                    let file: String = row.get(0)?;
                    let first_ts: i64 = row.get(1)?;
                    let last_ts: i64 = row.get(2)?;
                    let change_count: i64 = row.get(3)?;
                    let deleted: i64 = row.get(4)?;
                    Ok((file, first_ts, last_ts, change_count as u64, deleted != 0))
                })?;
                let mut out = Vec::new();
                let today = Utc::now().date_naive();
                for r in rows {
                    let (file, first_ts, last_ts, change_count, deleted) = r?;
                    if deleted {
                        continue;
                    }
                    let first_seen = ts_to_date(first_ts);
                    let last_modified = ts_to_date(last_ts);
                    let age_days = (today - first_seen).num_days().max(0) as u64;
                    let days_since_last_change = (today - last_modified).num_days().max(0) as u64;
                    let age_years = age_days as f64 / 365.25;
                    let changes_per_year = if age_years > 0.0 {
                        change_count as f64 / age_years
                    } else {
                        change_count as f64
                    };

                    let mut values = HashMap::new();
                    values.insert("age_days".into(), MetricValue::Count(age_days));
                    values.insert("first_seen".into(), MetricValue::Date(first_seen));
                    values.insert("last_modified".into(), MetricValue::Date(last_modified));
                    values.insert(
                        "days_since_last_change".into(),
                        MetricValue::Count(days_since_last_change),
                    );
                    values.insert("change_count".into(), MetricValue::Count(change_count));
                    values.insert(
                        "changes_per_year".into(),
                        MetricValue::Float(changes_per_year),
                    );
                    out.push(MetricEntry { key: file, values });
                }
                Ok(out)
            })
            .ok()?
            .ok()?;

        let mut entries = entries;
        entries.sort_by(|a, b| {
            let sa = match a.values.get("changes_per_year") {
                Some(MetricValue::Float(f)) => *f,
                _ => 0.0,
            };
            let sb = match b.values.get("changes_per_year") {
                Some(MetricValue::Float(f)) => *f,
                _ => 0.0,
            };
            sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
        });

        Some(MetricResult {
            name: "age".into(),
            display_name: report_display("age"),
            description: report_description("age"),
            entry_groups: vec![],
            columns: vec![
                Column::in_report("age", "age_days"),
                Column::in_report("age", "first_seen"),
                Column::in_report("age", "last_modified"),
                Column::in_report("age", "days_since_last_change"),
                Column::in_report("age", "change_count"),
                Column::in_report("age", "changes_per_year"),
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
        name: "age".into(),
        display_name: report_display("age"),
        description: report_description("age"),
        entry_groups: vec![],
        columns: vec![],
        entries: vec![],
    }
}
