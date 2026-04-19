use std::collections::HashMap;

use crate::metrics::MetricCollector;
use crate::store::ChangeStore;
use crate::types::{
    Column, EntryGroup, MetricEntry, MetricResult, MetricValue, report_description, report_display,
};

pub struct CommitVelocityCollector;

impl Default for CommitVelocityCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl CommitVelocityCollector {
    pub fn new() -> Self {
        Self
    }
}

impl MetricCollector for CommitVelocityCollector {
    fn name(&self) -> &str {
        "commit_velocity"
    }

    fn finalize(&mut self) -> MetricResult {
        empty_result()
    }

    fn finalize_from_db(
        &mut self,
        store: &ChangeStore,
        _progress: &crate::metrics::ProgressReporter,
    ) -> Option<MetricResult> {
        let (weekly, monthly) = store
            .with_conn(
                |conn| -> anyhow::Result<(Vec<MetricEntry>, Vec<MetricEntry>)> {
                    // Deduplicate commits first (one commit has many change rows),
                    // then group by time bucket.
                    let mut weekly = Vec::new();
                    let mut stmt = conn.prepare(
                        "SELECT strftime('%Y-W%W', datetime(ts, 'unixepoch')) AS week,
                            COUNT(*)  AS commits,
                            SUM(lines) AS lines_changed
                       FROM (
                           SELECT commit_oid,
                                  MIN(commit_ts) AS ts,
                                  SUM(additions + deletions) AS lines
                             FROM changes
                            GROUP BY commit_oid
                       )
                      GROUP BY week
                      ORDER BY week",
                    )?;
                    let rows = stmt.query_map([], |row| {
                        let week: String = row.get(0)?;
                        let commits: i64 = row.get(1)?;
                        let lines: i64 = row.get(2)?;
                        Ok((week, commits as u64, lines as u64))
                    })?;
                    for r in rows {
                        let (week, commits, lines) = r?;
                        let mut values = HashMap::new();
                        values.insert("commits".into(), MetricValue::Count(commits));
                        values.insert("lines_changed".into(), MetricValue::Count(lines));
                        weekly.push(MetricEntry { key: week, values });
                    }

                    let mut monthly = Vec::new();
                    let mut stmt = conn.prepare(
                        "SELECT strftime('%Y-%m', datetime(ts, 'unixepoch')) AS month,
                            COUNT(*)  AS commits,
                            SUM(lines) AS lines_changed
                       FROM (
                           SELECT commit_oid,
                                  MIN(commit_ts) AS ts,
                                  SUM(additions + deletions) AS lines
                             FROM changes
                            GROUP BY commit_oid
                       )
                      GROUP BY month
                      ORDER BY month",
                    )?;
                    let rows = stmt.query_map([], |row| {
                        let month: String = row.get(0)?;
                        let commits: i64 = row.get(1)?;
                        let lines: i64 = row.get(2)?;
                        Ok((month, commits as u64, lines as u64))
                    })?;
                    for r in rows {
                        let (month, commits, lines) = r?;
                        let mut values = HashMap::new();
                        values.insert("commits".into(), MetricValue::Count(commits));
                        values.insert("lines_changed".into(), MetricValue::Count(lines));
                        monthly.push(MetricEntry { key: month, values });
                    }

                    Ok((weekly, monthly))
                },
            )
            .ok()?
            .ok()?;

        Some(MetricResult {
            name: "commit_velocity".into(),
            display_name: report_display("commit_velocity"),
            description: report_description("commit_velocity"),
            columns: vec![
                Column::in_report("commit_velocity", "commits"),
                Column::in_report("commit_velocity", "lines_changed"),
            ],
            entries: vec![],
            entry_groups: vec![
                EntryGroup {
                    name: "weekly".into(),
                    label: "Weekly".into(),
                    entries: weekly,
                },
                EntryGroup {
                    name: "monthly".into(),
                    label: "Monthly".into(),
                    entries: monthly,
                },
            ],
        })
    }
}

fn empty_result() -> MetricResult {
    MetricResult {
        name: "commit_velocity".into(),
        display_name: report_display("commit_velocity"),
        description: report_description("commit_velocity"),
        columns: vec![],
        entries: vec![],
        entry_groups: vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_collector_produces_named_result() {
        let mut coll = CommitVelocityCollector::new();
        let result = coll.finalize();
        assert_eq!(result.name, "commit_velocity");
    }
}
