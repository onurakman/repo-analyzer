use std::collections::HashMap;

use crate::analysis::source_filter::is_source_file;
use crate::metrics::MetricCollector;
use crate::store::ChangeStore;
use crate::types::{
    Column, MetricEntry, MetricResult, MetricValue, report_description, report_display,
};

const MIN_COUPLING_COUNT: u64 = 3;
const MIN_COUPLING_SCORE: f64 = 0.3;

/// Cap the self-join to the top-N most-active files. Without this bound the
/// coupling query on a 30k-commit repository can materialize millions of
/// (file_a, file_b) pairs and explode SQLite's working set.
const TOP_FILES: i64 = 500;

pub struct CouplingCollector;

impl Default for CouplingCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl CouplingCollector {
    pub fn new() -> Self {
        Self
    }
}

impl MetricCollector for CouplingCollector {
    fn name(&self) -> &str {
        "coupling"
    }

    fn finalize(&mut self) -> MetricResult {
        empty_result()
    }

    fn finalize_from_db(
        &mut self,
        store: &ChangeStore,
        progress: &crate::metrics::ProgressReporter,
    ) -> Option<MetricResult> {
        store
            .with_conn(|conn| -> anyhow::Result<MetricResult> {
                progress.status(&format!(
                    "  coupling: picking top {TOP_FILES} active source files..."
                ));
                // Step 1: pick the top-N most-active *source* files (config,
                // lockfiles, docs are excluded up front so non-code pairs
                // don't crowd out real architectural coupling).
                conn.execute_batch(
                    "DROP TABLE IF EXISTS __coupling_files;
                     DROP TABLE IF EXISTS __coupling_changes;
                     CREATE TEMP TABLE __coupling_files (file TEXT PRIMARY KEY, cnt INTEGER);",
                )?;
                let top_files: Vec<(String, i64)> = {
                    let mut stmt = conn.prepare(
                        "SELECT file_path, COUNT(*) AS cnt
                           FROM changes
                          GROUP BY file_path
                          ORDER BY cnt DESC",
                    )?;
                    let rows = stmt.query_map([], |row| {
                        let f: String = row.get(0)?;
                        let n: i64 = row.get(1)?;
                        Ok((f, n))
                    })?;
                    let mut out: Vec<(String, i64)> = Vec::new();
                    for r in rows {
                        let (f, n) = r?;
                        if !is_source_file(&f) {
                            continue;
                        }
                        out.push((f, n));
                        if out.len() as i64 == TOP_FILES {
                            break;
                        }
                    }
                    out
                };
                let mut ins =
                    conn.prepare("INSERT INTO __coupling_files(file, cnt) VALUES (?1, ?2)")?;
                for (f, n) in &top_files {
                    ins.execute(rusqlite::params![f, n])?;
                }
                drop(ins);

                progress.status("  coupling: materializing filtered changes...");
                // Step 2: materialize a narrow (commit_oid, file_path) view
                // restricted to those top files. Add a covering index so the
                // self-join can probe matching commits in O(log n) per row
                // instead of scanning the full changes table twice.
                conn.execute_batch(
                    "CREATE TEMP TABLE __coupling_changes AS
                       SELECT ch.commit_oid, ch.file_path
                         FROM changes ch
                         JOIN __coupling_files t ON t.file = ch.file_path;
                     CREATE INDEX __idx_coupling_commit
                         ON __coupling_changes(commit_oid, file_path);",
                )?;
                progress.status("  coupling: self-join on filtered set...");

                // Pre-load per-file total commit counts (needed for scoring).
                let mut per_file: HashMap<String, u64> = HashMap::new();
                let mut stmt_totals = conn.prepare("SELECT file, cnt FROM __coupling_files")?;
                let totals = stmt_totals.query_map([], |row| {
                    let p: String = row.get(0)?;
                    let n: i64 = row.get(1)?;
                    Ok((p, n as u64))
                })?;
                for r in totals {
                    let (p, n) = r?;
                    per_file.insert(p, n);
                }

                // Self-join on the filtered, indexed set. The `file_path < file_path`
                // condition eliminates duplicate (b,a) vs (a,b) pairs.
                // LIMIT caps memory in case pairs explode anyway.
                let mut entries: Vec<MetricEntry> = Vec::new();
                let mut stmt = conn.prepare(
                    "SELECT a.file_path AS fa,
                            b.file_path AS fb,
                            COUNT(*)    AS co
                       FROM __coupling_changes a
                       JOIN __coupling_changes b
                         ON a.commit_oid = b.commit_oid
                        AND a.file_path < b.file_path
                      GROUP BY a.file_path, b.file_path
                     HAVING co >= ?1
                      ORDER BY co DESC
                      LIMIT 1000",
                )?;
                let rows = stmt.query_map([MIN_COUPLING_COUNT as i64], |row| {
                    let fa: String = row.get(0)?;
                    let fb: String = row.get(1)?;
                    let co: i64 = row.get(2)?;
                    Ok((fa, fb, co as u64))
                })?;
                for r in rows {
                    let (fa, fb, co) = r?;
                    let ca = per_file.get(&fa).copied().unwrap_or(1);
                    let cb = per_file.get(&fb).copied().unwrap_or(1);
                    let max_changes = ca.max(cb) as f64;
                    let score = co as f64 / max_changes;
                    if score < MIN_COUPLING_SCORE {
                        continue;
                    }
                    let key = format!("{fa} <-> {fb}");
                    let mut values = HashMap::new();
                    values.insert("file_a".into(), MetricValue::Text(fa));
                    values.insert("file_b".into(), MetricValue::Text(fb));
                    values.insert("co_changes".into(), MetricValue::Count(co));
                    values.insert("score".into(), MetricValue::Float(score));
                    entries.push(MetricEntry { key, values });
                }

                conn.execute_batch(
                    "DROP INDEX IF EXISTS __idx_coupling_commit;
                     DROP TABLE IF EXISTS __coupling_changes;
                     DROP TABLE IF EXISTS __coupling_files;",
                )?;

                Ok(MetricResult {
                    name: "coupling".into(),
                    display_name: report_display("coupling"),
                    description: report_description("coupling"),
                    entry_groups: vec![],
                    columns: vec![
                        Column::in_report("coupling", "file_a"),
                        Column::in_report("coupling", "file_b"),
                        Column::in_report("coupling", "co_changes"),
                        Column::in_report("coupling", "score"),
                    ],
                    entries,
                })
            })
            .ok()?
            .ok()
    }
}

fn empty_result() -> MetricResult {
    MetricResult {
        name: "coupling".into(),
        display_name: report_display("coupling"),
        description: report_description("coupling"),
        entry_groups: vec![],
        columns: vec![],
        entries: vec![],
    }
}
