use std::collections::HashMap;

use chrono::{Duration, Utc};

use crate::metrics::MetricCollector;
use crate::store::ChangeStore;
use crate::types::{MetricEntry, MetricResult, MetricValue};

const INACTIVE_DAYS: i64 = 180;

/// Cap the per-file per-author detail pass to the top-N files by total commits.
/// Beyond this the report entry is truncated anyway, and the tail dominates memory.
const TOP_FILES: i64 = 500;

pub struct SuccessionCollector;

impl Default for SuccessionCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl SuccessionCollector {
    pub fn new() -> Self {
        Self
    }
}

impl MetricCollector for SuccessionCollector {
    fn name(&self) -> &str {
        "succession"
    }

    fn finalize(&mut self) -> MetricResult {
        empty_result()
    }

    fn finalize_from_db(
        &mut self,
        store: &ChangeStore,
        progress: &crate::metrics::ProgressReporter,
    ) -> Option<MetricResult> {
        struct AuthorRow {
            commits: u64,
            last_ts: i64,
        }
        struct FileAcc {
            authors: HashMap<String, AuthorRow>,
            original_email: String,
            original_ts: i64,
        }
        type Files = HashMap<String, FileAcc>;
        type GlobalLast = HashMap<String, i64>;

        let (files, global_last) = store
            .with_conn(|conn| -> anyhow::Result<(Files, GlobalLast)> {
                progress.status(&format!(
                    "  succession: picking top {TOP_FILES} active files..."
                ));
                // Top-N files by commit activity. Everything else is dropped.
                conn.execute_batch(&format!(
                    "DROP TABLE IF EXISTS __top_files;
                     CREATE TEMP TABLE __top_files AS
                       SELECT file_path AS file
                         FROM changes
                        GROUP BY file_path
                        ORDER BY COUNT(DISTINCT commit_oid) DESC
                        LIMIT {TOP_FILES};"
                ))?;

                progress.status("  succession: per-file per-author detail...");
                let mut files: Files = HashMap::new();
                {
                    let mut stmt = conn.prepare(
                        "SELECT ch.file_path,
                                ch.email,
                                COUNT(DISTINCT ch.commit_oid) AS commits,
                                MIN(ch.commit_ts)             AS first_ts,
                                MAX(ch.commit_ts)             AS last_ts
                           FROM changes ch
                           JOIN __top_files t ON t.file = ch.file_path
                          GROUP BY ch.file_path, ch.email",
                    )?;
                    let rows = stmt.query_map([], |row| {
                        let file: String = row.get(0)?;
                        let email: String = row.get(1)?;
                        let commits: i64 = row.get(2)?;
                        let first_ts: i64 = row.get(3)?;
                        let last_ts: i64 = row.get(4)?;
                        Ok((file, email, commits as u64, first_ts, last_ts))
                    })?;
                    for r in rows {
                        let (file, email, commits, first_ts, last_ts) = r?;
                        let acc = files.entry(file).or_insert_with(|| FileAcc {
                            authors: HashMap::new(),
                            original_email: email.clone(),
                            original_ts: first_ts,
                        });
                        if first_ts < acc.original_ts {
                            acc.original_ts = first_ts;
                            acc.original_email = email.clone();
                        }
                        acc.authors.insert(email, AuthorRow { commits, last_ts });
                    }
                }

                let mut global_last: GlobalLast = HashMap::new();
                {
                    let mut stmt2 =
                        conn.prepare("SELECT email, MAX(commit_ts) FROM changes GROUP BY email")?;
                    let rows2 = stmt2.query_map([], |row| {
                        let email: String = row.get(0)?;
                        let last: i64 = row.get(1)?;
                        Ok((email, last))
                    })?;
                    for r in rows2 {
                        let (email, last) = r?;
                        global_last.insert(email, last);
                    }
                }

                conn.execute("DROP TABLE IF EXISTS __top_files", [])?;
                Ok((files, global_last))
            })
            .ok()?
            .ok()?;

        let now = Utc::now();
        let active_cutoff = (now - Duration::days(INACTIVE_DAYS)).timestamp();

        let mut entries: Vec<MetricEntry> = files
            .into_iter()
            .map(|(path, fs)| {
                let original_active = global_last
                    .get(&fs.original_email)
                    .map(|t| *t >= active_cutoff)
                    .unwrap_or(false);
                let total_authors = fs.authors.len() as u64;
                let successor_count = fs
                    .authors
                    .iter()
                    .filter(|(email, _)| **email != fs.original_email)
                    .count() as u64;
                let current_top: String = fs
                    .authors
                    .iter()
                    .max_by_key(|(_, t)| (t.commits, t.last_ts))
                    .map(|(e, _)| e.clone())
                    .unwrap_or_else(|| "<unknown>".into());

                let status = classify(original_active, successor_count, total_authors);

                let mut values = HashMap::new();
                values.insert(
                    "original_author".into(),
                    MetricValue::Text(fs.original_email),
                );
                values.insert(
                    "original_active".into(),
                    MetricValue::Text(if original_active {
                        "yes".into()
                    } else {
                        "no".into()
                    }),
                );
                values.insert("current_top".into(), MetricValue::Text(current_top));
                values.insert("total_authors".into(), MetricValue::Count(total_authors));
                values.insert("successors".into(), MetricValue::Count(successor_count));
                values.insert("status".into(), MetricValue::Text(status.into()));

                MetricEntry { key: path, values }
            })
            .collect();

        entries.sort_by_key(|e| std::cmp::Reverse(status_rank(e)));
        entries.truncate(200);

        Some(MetricResult {
            name: "succession".into(),
            display_name: "Author Succession".into(),
            description: "Per-file author succession: was the original author still active recently, and if not, who took over? Files marked 'Orphaned' (no active successor) or 'Knowledge transfer needed' (single successor) have lost their maintainer — if something breaks, no one alive in the project knows the original intent.".into(),
            entry_groups: vec![],
            column_labels: vec![],
            columns: vec![
                "original_author".into(),
                "original_active".into(),
                "current_top".into(),
                "total_authors".into(),
                "successors".into(),
                "status".into(),
            ],
            entries,
        })
    }
}

fn classify(original_active: bool, successors: u64, total_authors: u64) -> &'static str {
    if original_active {
        if total_authors >= 3 {
            "Healthy — original active, multiple authors"
        } else {
            "Owned — original active"
        }
    } else if successors == 0 {
        "Orphaned — original inactive, no successor"
    } else if successors == 1 {
        "Knowledge transfer needed — single successor"
    } else {
        "Handed off — multiple successors"
    }
}

fn status_rank(entry: &MetricEntry) -> u8 {
    match entry.values.get("status") {
        Some(MetricValue::Text(s)) => match s.as_str() {
            "Orphaned — original inactive, no successor" => 4,
            "Knowledge transfer needed — single successor" => 3,
            "Handed off — multiple successors" => 2,
            "Owned — original active" => 1,
            _ => 0,
        },
        _ => 0,
    }
}

fn empty_result() -> MetricResult {
    MetricResult {
        name: "succession".into(),
        display_name: "Author Succession".into(),
        description: String::new(),
        entry_groups: vec![],
        column_labels: vec![],
        columns: vec![],
        entries: vec![],
    }
}
