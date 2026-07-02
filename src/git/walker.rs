use std::sync::Arc;

use chrono::{DateTime, FixedOffset, Utc};
use gix::bstr::ByteSlice;
use gix::traverse::commit::simple::CommitTimeOrder;

use crate::interner::Interner;
use crate::types::{CommitInfo, TimeRange};

/// Walks git history and yields `CommitInfo` records.
pub struct GitWalker {
    repo_path: String,
    time_range: TimeRange,
    interner: Arc<Interner>,
}

impl GitWalker {
    /// Create a new walker for the given repository path and time range.
    pub fn new(repo_path: String, time_range: TimeRange, interner: Arc<Interner>) -> Self {
        Self {
            repo_path,
            time_range,
            interner,
        }
    }

    /// Walk the commit history, calling `callback` for each commit that falls within the
    /// configured time range. Returns the total number of matching commits.
    ///
    /// Commits are traversed in reverse chronological order (newest first).
    pub fn walk<F>(&self, mut callback: F) -> anyhow::Result<u64>
    where
        F: FnMut(CommitInfo) -> anyhow::Result<()>,
    {
        let repo = gix::open(&self.repo_path)?;

        // A freshly `git init`ed repository has an unborn HEAD (a symbolic ref
        // pointing at a branch that has no commits yet). `head_commit()` errors
        // in that case, so short-circuit to zero commits — the downstream
        // `total == 0` path already produces clean empty output.
        if repo.head()?.is_unborn() {
            return Ok(0);
        }

        let head_commit = repo.head_commit()?;

        let walk = head_commit
            .id()
            .ancestors()
            .sorting(gix::revision::walk::Sorting::ByCommitTime(
                CommitTimeOrder::NewestFirst,
            ))
            .use_commit_graph(true)
            .all()?;

        let mut count = 0u64;

        for info_result in walk {
            let info = info_result?;
            let commit = info.object()?;

            // Extract author info — interned because authors recur heavily across commits.
            let author = commit.author()?;
            let author_name = self.interner.intern(&author.name.to_str_lossy());
            let author_email = self.interner.intern(&author.email.to_str_lossy());

            // Extract timestamp from author signature
            let time = author.time()?;
            let offset_seconds = time.offset;
            let offset = FixedOffset::east_opt(offset_seconds)
                .unwrap_or_else(|| FixedOffset::east_opt(0).unwrap());
            let timestamp = DateTime::<Utc>::from_timestamp(time.seconds, 0)
                .unwrap_or_default()
                .with_timezone(&offset);

            // Apply time range filtering.
            //
            // The traversal is ordered by COMMITTER time (`ByCommitTime`), so
            // early termination must key off committer time — not the author
            // time used for `timestamp`. On rebased/cherry-picked commits the
            // author date can be far older than the committer date; such a
            // commit surfaces near HEAD, and breaking on its old author date
            // would silently drop every remaining in-range commit deeper in
            // history. Committer time >= author time in the normal case, so
            // this only ever terminates *later*, never dropping in-range work.
            let committer_ts = DateTime::<Utc>::from_timestamp(info.commit_time(), 0)
                .unwrap_or_default()
                .fixed_offset();
            if self.before_time_range(committer_ts) {
                break;
            }
            // Membership still uses author time (the stored `timestamp`) so the
            // set of commits attributed to a range matches their author dates.
            if !self.in_time_range(timestamp) {
                continue;
            }

            // Extract message
            let message = commit.message_raw_sloppy().to_string();

            // Extract parent IDs
            let parent_ids: Vec<String> = info.parent_ids().map(|id| id.to_string()).collect();

            let commit_info = CommitInfo {
                oid: info.id().to_string(),
                author: author_name,
                email: author_email,
                timestamp,
                message,
                parent_ids,
            };

            callback(commit_info)?;
            count += 1;
        }

        Ok(count)
    }

    /// Count matching commits WITHOUT decoding each commit object. The full
    /// `walk` decodes author/message/parents for every commit just to count
    /// them up front; here we only need the total for the progress bar, so we
    /// lean on the commit-graph's `commit_time` and never touch the object db.
    /// For `TimeRange::All` this is a pure oid count; for filtered ranges we
    /// bucket by committer time (decode-free), which matches the early-break
    /// logic and is exact enough for a progress total. (finding #31)
    pub fn count(&self) -> anyhow::Result<u64> {
        let repo = gix::open(&self.repo_path)?;
        if repo.head()?.is_unborn() {
            return Ok(0);
        }
        let head_commit = repo.head_commit()?;
        let walk = head_commit
            .id()
            .ancestors()
            .sorting(gix::revision::walk::Sorting::ByCommitTime(
                CommitTimeOrder::NewestFirst,
            ))
            .use_commit_graph(true)
            .all()?;

        let mut count = 0u64;
        for info_result in walk {
            let info = info_result?;
            if matches!(self.time_range, TimeRange::All) {
                count += 1;
                continue;
            }
            let ts = DateTime::<Utc>::from_timestamp(info.commit_time(), 0)
                .unwrap_or_default()
                .fixed_offset();
            if self.before_time_range(ts) {
                break;
            }
            if self.in_time_range(ts) {
                count += 1;
            }
        }
        Ok(count)
    }

    /// Returns `true` if the given timestamp falls within the configured time range.
    fn in_time_range(&self, ts: DateTime<FixedOffset>) -> bool {
        match &self.time_range {
            TimeRange::All => true,
            TimeRange::Since(duration) => {
                let cutoff = Utc::now() - *duration;
                ts >= cutoff
            }
            TimeRange::Between { from, to } => {
                let date = ts.date_naive();
                date >= *from && date <= *to
            }
        }
    }

    /// Returns `true` if the given timestamp is before (older than) the start of the time range.
    /// Used for early termination since commits are sorted newest-first.
    fn before_time_range(&self, ts: DateTime<FixedOffset>) -> bool {
        match &self.time_range {
            TimeRange::All => false,
            TimeRange::Since(duration) => {
                let cutoff = Utc::now() - *duration;
                ts < cutoff
            }
            TimeRange::Between { from, .. } => {
                let date = ts.date_naive();
                date < *from
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    /// Helper: create a temporary git repo with 2 commits.
    fn create_test_repo() -> TempDir {
        let dir = TempDir::new().expect("failed to create temp dir");
        let path = dir.path();

        let run = |args: &[&str]| {
            let status = Command::new("git")
                .args(args)
                .current_dir(path)
                .env("GIT_AUTHOR_NAME", "Test User")
                .env("GIT_AUTHOR_EMAIL", "test@example.com")
                .env("GIT_COMMITTER_NAME", "Test User")
                .env("GIT_COMMITTER_EMAIL", "test@example.com")
                .output()
                .expect("failed to run git command");
            assert!(
                status.status.success(),
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&status.stderr)
            );
        };

        run(&["init"]);
        run(&["config", "user.name", "Test User"]);
        run(&["config", "user.email", "test@example.com"]);

        // First commit
        std::fs::write(path.join("file.txt"), "hello").expect("write failed");
        run(&["add", "file.txt"]);
        run(&["commit", "-m", "Initial commit"]);

        // Small delay to ensure different timestamps
        std::thread::sleep(std::time::Duration::from_millis(1100));

        // Second commit
        std::fs::write(path.join("file.txt"), "hello world").expect("write failed");
        run(&["add", "file.txt"]);
        run(&["commit", "-m", "Second commit"]);

        dir
    }

    #[test]
    fn test_walk_all_commits() {
        let dir = create_test_repo();
        let walker = GitWalker::new(
            dir.path().to_str().unwrap().to_string(),
            TimeRange::All,
            Arc::new(Interner::new()),
        );

        let mut commits = Vec::new();
        let count = walker
            .walk(|ci| {
                commits.push(ci);
                Ok(())
            })
            .expect("walk failed");

        assert_eq!(count, 2, "should have exactly 2 commits");
        assert_eq!(commits.len(), 2);

        // Most recent first
        assert_eq!(commits[0].message.trim(), "Second commit");
        assert_eq!(commits[1].message.trim(), "Initial commit");

        // Author info
        assert_eq!(&*commits[0].author, "Test User");
        assert_eq!(&*commits[0].email, "test@example.com");
    }

    #[test]
    fn test_walk_empty_repo_head() {
        // Walk the current repo (repo-analyzer itself) and verify we get >0 commits.
        // This tests that the walker works on a real repo with history.
        let dir = create_test_repo();
        let walker = GitWalker::new(
            dir.path().to_str().unwrap().to_string(),
            TimeRange::All,
            Arc::new(Interner::new()),
        );

        let count = walker.walk(|_| Ok(())).expect("walk should succeed");

        assert!(count > 0, "should produce >0 commits");
    }

    /// Helper: create a temporary git repo with no commits (unborn HEAD).
    fn create_empty_test_repo() -> TempDir {
        let dir = TempDir::new().expect("failed to create temp dir");
        let path = dir.path();

        let status = Command::new("git")
            .args(["init"])
            .current_dir(path)
            .output()
            .expect("failed to run git init");
        assert!(
            status.status.success(),
            "git init failed: {}",
            String::from_utf8_lossy(&status.stderr)
        );

        dir
    }

    #[test]
    fn test_walk_unborn_head_repo() {
        // A freshly `git init`ed repo has no commits (unborn HEAD). The walk
        // should yield zero commits without erroring.
        let dir = create_empty_test_repo();
        let walker = GitWalker::new(
            dir.path().to_str().unwrap().to_string(),
            TimeRange::All,
            Arc::new(Interner::new()),
        );

        let mut called = false;
        let count = walker
            .walk(|_| {
                called = true;
                Ok(())
            })
            .expect("walk should succeed on a repo with no commits");

        assert_eq!(count, 0, "empty repo should walk to 0 commits");
        assert!(!called, "callback should not fire for a repo with no commits");
    }

    #[test]
    fn test_commit_has_parent_ids() {
        let dir = create_test_repo();
        let walker = GitWalker::new(
            dir.path().to_str().unwrap().to_string(),
            TimeRange::All,
            Arc::new(Interner::new()),
        );

        let mut commits = Vec::new();
        walker
            .walk(|ci| {
                commits.push(ci);
                Ok(())
            })
            .expect("walk failed");

        assert_eq!(commits.len(), 2);

        // Second commit (newest, index 0) has 1 parent
        assert_eq!(
            commits[0].parent_ids.len(),
            1,
            "second commit should have 1 parent"
        );

        // First commit (oldest, index 1) has 0 parents
        assert_eq!(
            commits[1].parent_ids.len(),
            0,
            "initial commit should have 0 parents"
        );

        // The parent of the second commit should be the first commit's oid
        assert_eq!(
            commits[0].parent_ids[0], commits[1].oid,
            "second commit's parent should be the first commit"
        );
    }
}
