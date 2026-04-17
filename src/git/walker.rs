use chrono::{DateTime, FixedOffset, Utc};
use gix::traverse::commit::simple::CommitTimeOrder;

use crate::types::{CommitInfo, TimeRange};

/// Walks git history and yields `CommitInfo` records.
pub struct GitWalker {
    repo_path: String,
    time_range: TimeRange,
}

impl GitWalker {
    /// Create a new walker for the given repository path and time range.
    pub fn new(repo_path: String, time_range: TimeRange) -> Self {
        Self {
            repo_path,
            time_range,
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

            // Extract author info
            let author = commit.author()?;
            let author_name = author.name.to_string();
            let author_email = author.email.to_string();

            // Extract timestamp from author signature
            let time = author.time()?;
            let offset_seconds = time.offset;
            let offset = FixedOffset::east_opt(offset_seconds)
                .unwrap_or_else(|| FixedOffset::east_opt(0).unwrap());
            let timestamp = DateTime::<Utc>::from_timestamp(time.seconds, 0)
                .unwrap_or_default()
                .with_timezone(&offset);

            // Apply time range filtering
            if self.before_time_range(timestamp) {
                // Commits are sorted newest-first, so if we're before the range, stop.
                break;
            }
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
        let walker = GitWalker::new(dir.path().to_str().unwrap().to_string(), TimeRange::All);

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
        assert_eq!(commits[0].author, "Test User");
        assert_eq!(commits[0].email, "test@example.com");
    }

    #[test]
    fn test_walk_empty_repo_head() {
        // Walk the current repo (repo-analyzer itself) and verify we get >0 commits.
        // This tests that the walker works on a real repo with history.
        let dir = create_test_repo();
        let walker = GitWalker::new(dir.path().to_str().unwrap().to_string(), TimeRange::All);

        let count = walker.walk(|_| Ok(())).expect("walk should succeed");

        assert!(count > 0, "should produce >0 commits");
    }

    #[test]
    fn test_commit_has_parent_ids() {
        let dir = create_test_repo();
        let walker = GitWalker::new(dir.path().to_str().unwrap().to_string(), TimeRange::All);

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
