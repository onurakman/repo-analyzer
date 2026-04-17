use std::collections::HashMap;

use crate::metrics::MetricCollector;
use crate::store::ChangeStore;
use crate::types::{MetricEntry, MetricResult, MetricValue};

pub struct HotspotsCollector;

impl Default for HotspotsCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl HotspotsCollector {
    pub fn new() -> Self {
        Self
    }
}

impl MetricCollector for HotspotsCollector {
    fn name(&self) -> &str {
        "hotspots"
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
                let mut out: Vec<MetricEntry> = Vec::new();

                // File-level
                let mut stmt = conn.prepare(
                    "SELECT file_path,
                            COUNT(*)             AS changes,
                            COUNT(DISTINCT email) AS authors
                       FROM changes
                      GROUP BY file_path",
                )?;
                let rows = stmt.query_map([], |row| {
                    let file: String = row.get(0)?;
                    let changes: i64 = row.get(1)?;
                    let authors: i64 = row.get(2)?;
                    Ok((file, changes as u64, authors as u64))
                })?;
                for r in rows {
                    let (file, changes, authors) = r?;
                    let score = changes * authors;
                    let mut values = HashMap::new();
                    values.insert("level".into(), MetricValue::Text("file".into()));
                    values.insert("changes".into(), MetricValue::Count(changes));
                    values.insert("unique_authors".into(), MetricValue::Count(authors));
                    values.insert("score".into(), MetricValue::Count(score));
                    out.push(MetricEntry { key: file, values });
                }

                // Construct-level
                let mut stmt2 = conn.prepare(
                    "SELECT ch.file_path,
                            c.qualified_name,
                            c.kind,
                            COUNT(*)                 AS changes,
                            COUNT(DISTINCT ch.email) AS authors
                       FROM constructs c
                       JOIN changes ch ON c.change_id = ch.id
                      GROUP BY ch.file_path, c.qualified_name, c.kind",
                )?;
                let rows2 = stmt2.query_map([], |row| {
                    let file: String = row.get(0)?;
                    let qn: String = row.get(1)?;
                    let kind: String = row.get(2)?;
                    let changes: i64 = row.get(3)?;
                    let authors: i64 = row.get(4)?;
                    Ok((file, qn, kind, changes as u64, authors as u64))
                })?;
                for r in rows2 {
                    let (file, qn, kind, changes, authors) = r?;
                    let score = changes * authors;
                    let key = format!("{file}::{qn}");
                    let mut values = HashMap::new();
                    values.insert("level".into(), MetricValue::Text("construct".into()));
                    values.insert("kind".into(), MetricValue::Text(kind));
                    values.insert("file".into(), MetricValue::Text(file));
                    values.insert("changes".into(), MetricValue::Count(changes));
                    values.insert("unique_authors".into(), MetricValue::Count(authors));
                    values.insert("score".into(), MetricValue::Count(score));
                    out.push(MetricEntry { key, values });
                }

                Ok(out)
            })
            .ok()?
            .ok()?;

        let mut entries = entries;
        entries.sort_by(|a, b| {
            let sa = match a.values.get("score") {
                Some(MetricValue::Count(n)) => *n,
                _ => 0,
            };
            let sb = match b.values.get("score") {
                Some(MetricValue::Count(n)) => *n,
                _ => 0,
            };
            sb.cmp(&sa)
        });
        entries.truncate(500);

        Some(MetricResult {
            name: "hotspots".into(),
            display_name: "Hotspots".into(),
            description: "Files and code constructs that change very often AND are touched by many different people. Frequent change × many authors = high bug risk and reviewer fatigue. The top entries are your most likely refactor candidates — fixing one of these usually pays back across the team.".into(),
            entry_groups: vec![],
            column_labels: vec![],
            columns: vec![
                "level".into(),
                "kind".into(),
                "file".into(),
                "changes".into(),
                "unique_authors".into(),
                "score".into(),
            ],
            entries,
        })
    }
}

fn empty_result() -> MetricResult {
    MetricResult {
        name: "hotspots".into(),
        display_name: "Hotspots".into(),
        description: String::new(),
        entry_groups: vec![],
        column_labels: vec![],
        columns: vec![],
        entries: vec![],
    }
}
