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
    /// `Option` so `Drop` can `take()` the connection and `close()` it before
    /// unlinking the file — otherwise the handle stays open across the unlink,
    /// which leaks the temp file on Windows. (finding #44)
    conn: Mutex<Option<Connection>>,
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
            conn: Mutex::new(Some(conn)),
        })
    }

    /// Lock the connection, erroring if the mutex is poisoned or the connection
    /// has already been closed (only happens during `Drop`).
    fn lock_conn(
        &self,
    ) -> anyhow::Result<std::sync::MutexGuard<'_, Option<Connection>>> {
        self.conn
            .lock()
            .map_err(|_| anyhow::anyhow!("ChangeStore mutex poisoned"))
    }

    /// Write a batch of parsed changes to the store inside a single transaction.
    pub fn insert_batch(&self, changes: &[ParsedChange]) -> anyhow::Result<()> {
        if changes.is_empty() {
            return Ok(());
        }
        let mut guard = self.lock_conn()?;
        let conn = guard
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("ChangeStore connection closed"))?;
        let tx = conn.transaction()?;
        {
            let mut insert_change = tx.prepare(
                "INSERT INTO changes
                   (commit_oid, commit_ts, author, email, message, file_path,
                    old_path, status, additions, deletions, parent_count, tz_offset)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
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
                    diff.old_path.as_deref(),
                    status,
                    diff.additions,
                    diff.deletions,
                    commit.parent_ids.len() as i64,
                    commit.timestamp.offset().local_minus_utc(),
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
        let guard = self.lock_conn()?;
        let conn = guard
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("ChangeStore connection closed"))?;
        conn.execute_batch(
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
        let guard = self.lock_conn()?;
        let conn = guard
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("ChangeStore connection closed"))?;
        Ok(f(conn))
    }
}

impl Drop for ChangeStore {
    fn drop(&mut self) {
        // Close the SQLite connection BEFORE unlinking so the backing file has
        // no open handle when removed — otherwise the temp file leaks on
        // Windows, where unlinking an open file fails. (finding #44)
        if let Ok(mut guard) = self.conn.lock()
            && let Some(conn) = guard.take()
        {
            let _ = conn.close();
        }
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
             -- Rename source (the pre-rename path) for status=Renamed rows,
             -- NULL otherwise. Lets a renamed file's history be re-attributed to
             -- its current HEAD name instead of splitting across old/new paths.
             -- (finding #10)
             old_path TEXT,
             status INTEGER NOT NULL,
             additions INTEGER NOT NULL,
             deletions INTEGER NOT NULL,
             parent_count INTEGER NOT NULL,
             -- Author-timezone offset (seconds east of UTC). `commit_ts` is a
             -- UTC epoch, so `commit_ts + tz_offset` reconstructs the author's
             -- local wall-clock — needed to bucket the patterns histograms by
             -- local time-of-day/day-of-week rather than UTC. (finding #41)
             tz_offset INTEGER NOT NULL
         );
         CREATE TABLE constructs (
             change_id INTEGER NOT NULL,
             qualified_name TEXT NOT NULL,
             kind TEXT NOT NULL,
             lines_touched INTEGER NOT NULL
         );",
    )?;
    // Shared non-merge view. The walker visits *every* parent, and each commit
    // is diffed against its first parent only, so a merge commit re-reports the
    // entire side branch — double-counting churn (~2x) and crediting the merger
    // with all the branch work. History-aggregating collectors read from this
    // view instead of `changes` so the filter is defined in exactly one place
    // and can never drift between collectors. Merge rows stay in `changes`
    // itself because `quality` needs them to compute the merge ratio, and
    // `age`/`outliers`/`patterns` intentionally remain merge-inclusive.
    // (audit finding #3)
    conn.execute_batch(
        "CREATE VIEW non_merge_changes AS
             SELECT * FROM changes WHERE parent_count <= 1;",
    )?;
    // Shared "live file" definition: files that currently exist at HEAD, i.e.
    // whose most recent change is NOT a deletion. A file deleted and later
    // re-added is live; one whose last change is a deletion is not. Collectors
    // that describe the current codebase (age, hotspots, churn, ownership,
    // knowledge_silos, succession) filter through this so a since-deleted file
    // can't show up as a top hotspot, and a deleted-then-re-added file isn't
    // wrongly dropped. One definition, no cross-collector drift.
    // (audit findings #12, #13)
    conn.execute_batch(
        "CREATE VIEW live_files AS
             SELECT file_path
               FROM changes
              GROUP BY file_path
             HAVING COALESCE(MAX(CASE WHEN status = 2 THEN commit_ts END), -1) < MAX(commit_ts);",
    )?;
    // Maps every historical (renamed-away) path to its current HEAD name by
    // following rename edges to the end of the chain, so a file renamed
    // A -> B -> C has both A and B resolve to C. Collectors that canonicalize
    // (churn, ownership, age) LEFT JOIN this and group by
    // COALESCE(head_path, file_path), merging a renamed file's history under one
    // name instead of splitting it. Only status=Renamed rows are edges — copies
    // (which diff.rs also records with old_path but status=Modified) are
    // excluded. The depth cap + `to_path <> orig` guard keep a cyclic rename
    // from looping. (finding #10)
    conn.execute_batch(
        "CREATE VIEW canonical_path AS
             WITH RECURSIVE
               edges(from_path, to_path) AS (
                 SELECT from_path, to_path FROM (
                   SELECT old_path AS from_path, file_path AS to_path, MAX(commit_ts) AS ts
                     FROM changes
                    WHERE status = 3 AND old_path IS NOT NULL AND old_path <> file_path
                    GROUP BY old_path
                 )
               ),
               chain(orig, cur, depth) AS (
                 SELECT from_path, to_path, 1 FROM edges
                 UNION ALL
                 SELECT c.orig, e.to_path, c.depth + 1
                   FROM chain c JOIN edges e ON e.from_path = c.cur
                  WHERE c.depth < 64 AND e.to_path <> c.orig
               )
             SELECT orig AS path, cur AS head_path
               FROM chain
              WHERE cur NOT IN (SELECT from_path FROM edges);",
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
    fn non_merge_view_excludes_merge_rows() {
        // A root (0 parents), a normal commit (1 parent) and a merge (2 parents)
        // all touch a.rs. The raw `changes` table keeps every row (quality needs
        // the merge ratio), but the shared `non_merge_changes` view must drop the
        // 2-parent merge so history collectors don't double-count. (finding #3)
        let ts = FixedOffset::east_opt(0)
            .unwrap()
            .with_ymd_and_hms(2025, 1, 1, 0, 0, 0)
            .unwrap();
        let mk = |oid: &str, parents: Vec<String>, adds: u32| ParsedChange {
            diff: Arc::new(DiffRecord {
                commit: Arc::new(CommitInfo {
                    oid: oid.into(),
                    author: "a".into(),
                    email: "a@x".into(),
                    timestamp: ts,
                    message: "m".into(),
                    parent_ids: parents,
                }),
                file_path: "a.rs".into(),
                old_path: None,
                status: FileStatus::Modified,
                hunks: vec![],
                additions: adds,
                deletions: 0,
            }),
            constructs: vec![],
        };
        let store = ChangeStore::open_temp().expect("open");
        store
            .insert_batch(&[
                mk("c1", vec![], 10),
                mk("c2", vec!["c1".into()], 20),
                mk("m1", vec!["c2".into(), "b1".into()], 999),
            ])
            .expect("insert");

        let (all_adds, non_merge_adds): (i64, i64) = store
            .with_conn(|c| {
                let a: i64 = c
                    .query_row("SELECT COALESCE(SUM(additions),0) FROM changes", [], |r| {
                        r.get(0)
                    })
                    .unwrap();
                let n: i64 = c
                    .query_row(
                        "SELECT COALESCE(SUM(additions),0) FROM non_merge_changes",
                        [],
                        |r| r.get(0),
                    )
                    .unwrap();
                (a, n)
            })
            .unwrap();
        assert_eq!(all_adds, 1029, "raw changes keeps the merge row");
        assert_eq!(
            non_merge_adds, 30,
            "non_merge_changes drops the 2-parent merge (10 + 20)"
        );
    }

    #[test]
    fn live_files_view_tracks_head_presence() {
        use chrono::Duration;
        let base = FixedOffset::east_opt(0)
            .unwrap()
            .with_ymd_and_hms(2025, 1, 1, 0, 0, 0)
            .unwrap();
        // (file, status, day-offset) — later day = later commit_ts.
        let mk = |file: &str, status: FileStatus, day: i64| ParsedChange {
            diff: Arc::new(DiffRecord {
                commit: Arc::new(CommitInfo {
                    oid: format!("{file}-{day}"),
                    author: "a".into(),
                    email: "a@x".into(),
                    timestamp: base + Duration::days(day),
                    message: "m".into(),
                    parent_ids: vec!["p".into()],
                }),
                file_path: file.into(),
                old_path: None,
                status,
                hunks: vec![],
                additions: 1,
                deletions: 0,
            }),
            constructs: vec![],
        };
        let store = ChangeStore::open_temp().expect("open");
        store
            .insert_batch(&[
                // never deleted -> live
                mk("alive.rs", FileStatus::Added, 1),
                mk("alive.rs", FileStatus::Modified, 2),
                // deleted last -> NOT live
                mk("gone.rs", FileStatus::Added, 1),
                mk("gone.rs", FileStatus::Deleted, 3),
                // deleted then re-added -> live
                mk("revived.rs", FileStatus::Added, 1),
                mk("revived.rs", FileStatus::Deleted, 2),
                mk("revived.rs", FileStatus::Added, 4),
            ])
            .expect("insert");

        let mut live: Vec<String> = store
            .with_conn(|c| {
                let mut stmt = c.prepare("SELECT file_path FROM live_files ORDER BY 1").unwrap();
                stmt.query_map([], |r| r.get::<_, String>(0))
                    .unwrap()
                    .map(|r| r.unwrap())
                    .collect::<Vec<_>>()
            })
            .unwrap();
        live.sort();
        assert_eq!(
            live,
            vec!["alive.rs".to_string(), "revived.rs".to_string()],
            "live_files = files whose last change is not a deletion"
        );
    }

    #[test]
    fn canonical_path_resolves_rename_chain() {
        use chrono::Duration;
        let base = FixedOffset::east_opt(0)
            .unwrap()
            .with_ymd_and_hms(2025, 1, 1, 0, 0, 0)
            .unwrap();
        let mk = |oid: &str, file: &str, old: Option<&str>, status: FileStatus, day: i64| {
            ParsedChange {
                diff: Arc::new(DiffRecord {
                    commit: Arc::new(CommitInfo {
                        oid: oid.into(),
                        author: "a".into(),
                        email: "a@x".into(),
                        timestamp: base + Duration::days(day),
                        message: "m".into(),
                        parent_ids: vec!["p".into()],
                    }),
                    file_path: file.into(),
                    old_path: old.map(Into::into),
                    status,
                    hunks: vec![],
                    additions: 1,
                    deletions: 0,
                }),
                constructs: vec![],
            }
        };
        let store = ChangeStore::open_temp().expect("open");
        // a.rs -> b.rs -> c.rs. A copy row (old_path set, status Modified) must
        // NOT be treated as a rename edge.
        store
            .insert_batch(&[
                mk("c1", "a.rs", None, FileStatus::Added, 1),
                mk("c2", "b.rs", Some("a.rs"), FileStatus::Renamed, 2),
                mk("c3", "c.rs", Some("b.rs"), FileStatus::Renamed, 3),
                mk("c4", "copy.rs", Some("c.rs"), FileStatus::Modified, 4),
            ])
            .expect("insert");

        let map: Vec<(String, String)> = store
            .with_conn(|c| {
                let mut stmt = c
                    .prepare("SELECT path, head_path FROM canonical_path ORDER BY path")
                    .unwrap();
                stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                    .unwrap()
                    .map(|r| r.unwrap())
                    .collect()
            })
            .unwrap();
        assert_eq!(
            map,
            vec![
                ("a.rs".to_string(), "c.rs".to_string()),
                ("b.rs".to_string(), "c.rs".to_string()),
            ],
            "both a.rs and b.rs resolve to c.rs; the copy is not a rename edge"
        );
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
