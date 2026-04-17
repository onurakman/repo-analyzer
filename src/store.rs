//! Disk-backed change store.
//!
//! The pipeline writes every parsed diff record into a temporary SQLite
//! database. Process-based collectors then run SQL queries at finalize time
//! instead of holding in-memory `HashMap`s that grow linearly with the number
//! of commits.
//!
//! The temp file lives under `std::env::temp_dir()`, is named with the current
//! process id, and is deleted when the `ChangeStore` is dropped — including
//! on panic.

use std::path::PathBuf;
use std::process;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use rusqlite::{Connection, params};

use crate::types::{FileStatus, ParsedChange};

/// Temp-file backed SQLite store. Thread-safe via internal `Mutex`.
///
/// Writes are serialized but happen in batches inside a single transaction
/// (see [`insert_batch`](Self::insert_batch)), so contention stays minimal
/// even with rayon-parallel producers.
pub struct ChangeStore {
    path: PathBuf,
    conn: Mutex<Connection>,
}

impl ChangeStore {
    /// Open a fresh temp-file SQLite database and initialize the schema.
    pub fn open_temp() -> anyhow::Result<Self> {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!("repo-analyzer-{}-{seq}.sqlite", process::id()));
        // Remove any leftover from a prior crashed run.
        let _ = std::fs::remove_file(&path);

        let mut conn = Connection::open(&path)?;
        apply_pragmas(&mut conn)?;
        create_schema(&mut conn)?;

        Ok(Self {
            path,
            conn: Mutex::new(conn),
        })
    }

    /// Write a batch of parsed changes to the store inside a single transaction.
    pub fn insert_batch(&self, changes: &[ParsedChange]) -> anyhow::Result<()> {
        if changes.is_empty() {
            return Ok(());
        }
        let mut guard = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("ChangeStore mutex poisoned"))?;
        let tx = guard.transaction()?;
        {
            let mut insert_change = tx.prepare(
                "INSERT INTO changes
                   (commit_oid, commit_ts, author, email, message, file_path,
                    status, additions, deletions, parent_count)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            )?;
            let mut insert_construct = tx.prepare(
                "INSERT INTO constructs
                   (change_id, qualified_name, kind, lines_touched)
                 VALUES (?1, ?2, ?3, ?4)",
            )?;

            for c in changes {
                let diff = c.diff.as_ref();
                let commit = diff.commit.as_ref();
                let status = file_status_to_int(diff.status);
                insert_change.execute(params![
                    commit.oid,
                    commit.timestamp.timestamp(),
                    commit.author.as_ref(),
                    commit.email.as_ref(),
                    commit.message,
                    diff.file_path.as_ref(),
                    status,
                    diff.additions,
                    diff.deletions,
                    commit.parent_ids.len() as i64,
                ])?;
                let change_id = tx.last_insert_rowid();

                for construct in &c.constructs {
                    let (start, end) = construct.line_range();
                    let lines_touched = end.saturating_sub(start).saturating_add(1);
                    insert_construct.execute(params![
                        change_id,
                        construct.qualified_name(),
                        construct.kind_str(),
                        lines_touched,
                    ])?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Called after all inserts complete; builds indexes to speed up the
    /// per-collector SELECT queries. Doing this after the bulk load is much
    /// faster than maintaining indexes during inserts.
    pub fn finalize_indexes(&self) -> anyhow::Result<()> {
        let guard = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("ChangeStore mutex poisoned"))?;
        guard.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_changes_file ON changes(file_path);
             CREATE INDEX IF NOT EXISTS idx_changes_email ON changes(email);
             CREATE INDEX IF NOT EXISTS idx_changes_commit ON changes(commit_oid);
             CREATE INDEX IF NOT EXISTS idx_constructs_qname ON constructs(qualified_name);
             CREATE INDEX IF NOT EXISTS idx_constructs_change ON constructs(change_id);
             ANALYZE;",
        )?;
        Ok(())
    }

    /// Run a read-only closure against the underlying connection.
    /// Used by collectors to execute their aggregation queries at finalize time.
    pub fn with_conn<R>(&self, f: impl FnOnce(&Connection) -> R) -> anyhow::Result<R> {
        let guard = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("ChangeStore mutex poisoned"))?;
        Ok(f(&guard))
    }
}

impl Drop for ChangeStore {
    fn drop(&mut self) {
        // Best-effort: close the connection explicitly (ignored if mutex is
        // poisoned), then remove the backing file.
        let _ = std::fs::remove_file(&self.path);
    }
}

fn apply_pragmas(conn: &mut Connection) -> anyhow::Result<()> {
    // Keep the page cache small and disable mmap so SQLite doesn't bloat the
    // resident set on a long-running ingest. temp_store goes to FILE (not
    // MEMORY) so any GROUP BY spill lands on disk instead of RAM.
    conn.execute_batch(
        "PRAGMA journal_mode = OFF;
         PRAGMA synchronous = OFF;
         PRAGMA temp_store = FILE;
         PRAGMA cache_size = -8192;
         PRAGMA mmap_size = 0;
         PRAGMA locking_mode = EXCLUSIVE;",
    )?;
    Ok(())
}

fn create_schema(conn: &mut Connection) -> anyhow::Result<()> {
    // `file_path`, `author`, `email` are stored as plain TEXT (not interned via
    // a lookup table) — SQLite dedupes at the page level and query planning
    // stays simple. If memory pressure returns we can switch to a star schema.
    conn.execute_batch(
        "CREATE TABLE changes (
             id INTEGER PRIMARY KEY,
             commit_oid TEXT NOT NULL,
             commit_ts INTEGER NOT NULL,
             author TEXT NOT NULL,
             email TEXT NOT NULL,
             message TEXT,
             file_path TEXT NOT NULL,
             status INTEGER NOT NULL,
             additions INTEGER NOT NULL,
             deletions INTEGER NOT NULL,
             parent_count INTEGER NOT NULL
         );
         CREATE TABLE constructs (
             change_id INTEGER NOT NULL,
             qualified_name TEXT NOT NULL,
             kind TEXT NOT NULL,
             lines_touched INTEGER NOT NULL
         );",
    )?;
    Ok(())
}

fn file_status_to_int(s: FileStatus) -> i64 {
    match s {
        FileStatus::Added => 0,
        FileStatus::Modified => 1,
        FileStatus::Deleted => 2,
        FileStatus::Renamed => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{CodeConstruct, CommitInfo, DiffRecord, ParsedChange};
    use chrono::{FixedOffset, TimeZone};
    use std::sync::Arc;

    fn sample_change(oid: &str, file: &str, email: &str) -> ParsedChange {
        let ts = FixedOffset::east_opt(0)
            .unwrap()
            .with_ymd_and_hms(2025, 1, 1, 0, 0, 0)
            .unwrap();
        ParsedChange {
            diff: Arc::new(DiffRecord {
                commit: Arc::new(CommitInfo {
                    oid: oid.into(),
                    author: email.into(),
                    email: email.into(),
                    timestamp: ts,
                    message: "m".into(),
                    parent_ids: vec![],
                }),
                file_path: file.into(),
                old_path: None,
                status: FileStatus::Modified,
                hunks: vec![],
                additions: 10,
                deletions: 2,
            }),
            constructs: vec![CodeConstruct::Function {
                name: "foo".into(),
                start_line: 1,
                end_line: 10,
                enclosing: None,
            }],
        }
    }

    #[test]
    fn insert_and_query_roundtrip() {
        let store = ChangeStore::open_temp().expect("open");
        store
            .insert_batch(&[sample_change("c1", "a.rs", "alice@x")])
            .expect("insert");
        store.finalize_indexes().expect("indexes");

        let count: i64 = store
            .with_conn(|c| {
                c.query_row("SELECT COUNT(*) FROM changes", [], |r| r.get(0))
                    .unwrap()
            })
            .unwrap();
        assert_eq!(count, 1);

        let constructs: i64 = store
            .with_conn(|c| {
                c.query_row("SELECT COUNT(*) FROM constructs", [], |r| r.get(0))
                    .unwrap()
            })
            .unwrap();
        assert_eq!(constructs, 1);
    }

    #[test]
    fn temp_file_is_deleted_on_drop() {
        let path = {
            let store = ChangeStore::open_temp().expect("open");
            store.path.clone()
        };
        assert!(
            !path.exists(),
            "temp file should be removed when ChangeStore is dropped"
        );
    }
}
