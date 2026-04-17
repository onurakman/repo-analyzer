use std::collections::HashMap;

use chrono::{DateTime, NaiveDate, TimeZone, Utc};

use crate::metrics::MetricCollector;
use crate::store::ChangeStore;
use crate::types::{MetricEntry, MetricResult, MetricValue};

const SILO_PCT: u64 = 80;
const IDLE_DAYS: i64 = 180;

/// Only evaluate the top-N most-written files for silo risk; the rest are
/// negligible in line volume and dwarf memory if we load everything.
const TOP_FILES: i64 = 2000;

pub struct KnowledgeSilosCollector;

impl Default for KnowledgeSilosCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl KnowledgeSilosCollector {
    pub fn new() -> Self {
        Self
    }
}

impl MetricCollector for KnowledgeSilosCollector {
    fn name(&self) -> &str {
        "knowledge_silos"
    }

    fn finalize(&mut self) -> MetricResult {
        empty_result()
    }

    fn finalize_from_db(
        &mut self,
        store: &ChangeStore,
        _progress: &crate::metrics::ProgressReporter,
    ) -> Option<MetricResult> {
        let rows = store
            .with_conn(|conn| -> anyhow::Result<Vec<(String, String, u64, i64)>> {
                conn.execute_batch(&format!(
                    "DROP TABLE IF EXISTS __silo_files;
                     CREATE TEMP TABLE __silo_files AS
                       SELECT file_path AS file
                         FROM changes
                        GROUP BY file_path
                        ORDER BY SUM(additions) DESC
                        LIMIT {};",
                    TOP_FILES
                ))?;
                let mut stmt = conn.prepare(
                    "SELECT ch.file_path,
                            ch.email,
                            SUM(ch.additions)   AS added,
                            MAX(ch.commit_ts)   AS last_ts
                       FROM changes ch
                       JOIN __silo_files t ON t.file = ch.file_path
                      GROUP BY ch.file_path, ch.email",
                )?;
                let iter = stmt.query_map([], |row| {
                    let file: String = row.get(0)?;
                    let email: String = row.get(1)?;
                    let added: i64 = row.get(2)?;
                    let last_ts: i64 = row.get(3)?;
                    Ok((file, email, added as u64, last_ts))
                })?;
                let mut out = Vec::new();
                for r in iter {
                    out.push(r?);
                }
                conn.execute("DROP TABLE IF EXISTS __silo_files", [])?;
                Ok(out)
            })
            .ok()?
            .ok()?;

        struct FileAcc {
            lines_per_author: HashMap<String, u64>,
            last_per_author: HashMap<String, i64>,
        }
        let mut per_file: HashMap<String, FileAcc> = HashMap::new();
        for (file, email, added, last_ts) in rows {
            let acc = per_file.entry(file).or_insert_with(|| FileAcc {
                lines_per_author: HashMap::new(),
                last_per_author: HashMap::new(),
            });
            *acc.lines_per_author.entry(email.clone()).or_insert(0) += added;
            let slot = acc.last_per_author.entry(email).or_insert(last_ts);
            if last_ts > *slot {
                *slot = last_ts;
            }
        }

        let now = Utc::now().timestamp();
        let mut entries: Vec<MetricEntry> = Vec::new();

        for (path, acc) in per_file {
            let total_lines: u64 = acc.lines_per_author.values().sum();
            if total_lines == 0 {
                continue;
            }
            let Some((owner_email, owner_lines)) =
                acc.lines_per_author.iter().max_by_key(|(_, n)| **n)
            else {
                continue;
            };
            let ownership_pct = (owner_lines * 100 / total_lines).min(100);
            let is_silo = ownership_pct >= SILO_PCT;
            if !is_silo {
                continue;
            }
            let owner_last = acc.last_per_author.get(owner_email).copied().unwrap_or(now);
            let owner_idle_days = ((now - owner_last) / 86_400).max(0);
            let owner_inactive = owner_idle_days >= IDLE_DAYS;
            let risk = if owner_inactive {
                "At risk — silo + owner idle"
            } else {
                "Single-owner — bus factor 1"
            };

            let mut values = HashMap::new();
            values.insert("owner".into(), MetricValue::Text(owner_email.clone()));
            values.insert("ownership_pct".into(), MetricValue::Count(ownership_pct));
            values.insert(
                "owner_last_touch".into(),
                MetricValue::Date(ts_to_date(owner_last)),
            );
            values.insert(
                "owner_idle_days".into(),
                MetricValue::Count(owner_idle_days as u64),
            );
            values.insert("total_lines".into(), MetricValue::Count(total_lines));
            values.insert("risk".into(), MetricValue::Text(risk.into()));
            entries.push(MetricEntry { key: path, values });
        }

        entries.sort_by(|a, b| {
            let ra = risk_rank(a);
            let rb = risk_rank(b);
            if ra != rb {
                return rb.cmp(&ra);
            }
            let ia = match a.values.get("owner_idle_days") {
                Some(MetricValue::Count(n)) => *n,
                _ => 0,
            };
            let ib = match b.values.get("owner_idle_days") {
                Some(MetricValue::Count(n)) => *n,
                _ => 0,
            };
            ib.cmp(&ia)
        });
        entries.truncate(150);

        Some(MetricResult {
            name: "knowledge_silos".into(),
            display_name: "Knowledge Silos".into(),
            description: format!(
                "Files where one person wrote at least {SILO_PCT}% of the code, AND that person hasn't touched the file in the last {IDLE_DAYS} days. If they go on vacation or leave the company, no one else can confidently change this code. Treat 'At risk' files as urgent knowledge-transfer items — pair-program a change, write docs, or split the file."
            ),
            entry_groups: vec![],
            column_labels: vec![],
            columns: vec![
                "owner".into(),
                "ownership_pct".into(),
                "owner_last_touch".into(),
                "owner_idle_days".into(),
                "total_lines".into(),
                "risk".into(),
            ],
            entries,
        })
    }
}

fn risk_rank(entry: &MetricEntry) -> u8 {
    match entry.values.get("risk") {
        Some(MetricValue::Text(s)) if s.starts_with("At risk") => 2,
        Some(MetricValue::Text(s)) if s.starts_with("Single-owner") => 1,
        _ => 0,
    }
}

fn ts_to_date(ts: i64) -> NaiveDate {
    let dt: DateTime<Utc> = Utc.timestamp_opt(ts, 0).single().unwrap_or_default();
    dt.date_naive()
}

fn empty_result() -> MetricResult {
    MetricResult {
        name: "knowledge_silos".into(),
        display_name: "Knowledge Silos".into(),
        description: String::new(),
        entry_groups: vec![],
        column_labels: vec![],
        columns: vec![],
        entries: vec![],
    }
}
