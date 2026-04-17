use std::sync::Arc;

use gix::bstr::ByteSlice;
use gix::object::tree::diff::Change;

use crate::types::{CommitInfo, DiffRecord, FileStatus, Hunk};

/// Extracts file-level diff information from git commits using `gix` (native, no subprocess).
pub struct DiffExtractor {
    repo: Arc<gix::ThreadSafeRepository>,
}

impl DiffExtractor {
    pub fn new(repo: Arc<gix::ThreadSafeRepository>) -> Self {
        Self { repo }
    }

    /// Extract diff records for a single commit by comparing its tree to its parent's tree.
    /// For initial commits (no parent), compares against the empty tree.
    pub fn extract(&self, commit: &CommitInfo) -> anyhow::Result<Vec<DiffRecord>> {
        let repo = self.repo.to_thread_local();

        let new_commit_id = gix::ObjectId::from_hex(commit.oid.as_bytes())?;
        let new_tree = repo.find_object(new_commit_id)?.try_into_commit()?.tree()?;

        let old_tree = if let Some(parent_str) = commit.parent_ids.first() {
            let parent_id = gix::ObjectId::from_hex(parent_str.as_bytes())?;
            repo.find_object(parent_id)?.try_into_commit()?.tree()?
        } else {
            repo.empty_tree()
        };

        let mut records: Vec<DiffRecord> = Vec::new();
        let mut resource_cache = repo.diff_resource_cache_for_tree_diff()?;

        let mut platform = old_tree.changes()?;

        platform.for_each_to_obtain_tree(&new_tree, |change| {
            // Extract path + status. Skip non-blob entries (e.g., tree-to-tree changes).
            let parsed = match &change {
                Change::Addition {
                    location,
                    entry_mode,
                    ..
                } => {
                    if !entry_mode.is_blob() {
                        return Ok::<_, std::convert::Infallible>(std::ops::ControlFlow::Continue(
                            (),
                        ));
                    }
                    Some((bstr_to_string(location), None, FileStatus::Added))
                }
                Change::Deletion {
                    location,
                    entry_mode,
                    ..
                } => {
                    if !entry_mode.is_blob() {
                        return Ok(std::ops::ControlFlow::Continue(()));
                    }
                    Some((bstr_to_string(location), None, FileStatus::Deleted))
                }
                Change::Modification {
                    location,
                    entry_mode,
                    ..
                } => {
                    if !entry_mode.is_blob() {
                        return Ok(std::ops::ControlFlow::Continue(()));
                    }
                    Some((bstr_to_string(location), None, FileStatus::Modified))
                }
                Change::Rewrite {
                    location,
                    source_location,
                    copy,
                    entry_mode,
                    ..
                } => {
                    if !entry_mode.is_blob() {
                        return Ok(std::ops::ControlFlow::Continue(()));
                    }
                    let st = if *copy {
                        FileStatus::Modified
                    } else {
                        FileStatus::Renamed
                    };
                    Some((
                        bstr_to_string(location),
                        Some(bstr_to_string(source_location)),
                        st,
                    ))
                }
            };

            let Some((file_path, old_path, status)) = parsed else {
                return Ok(std::ops::ControlFlow::Continue(()));
            };

            let (additions, deletions, is_binary) = match change
                .diff(&mut resource_cache)
                .ok()
                .and_then(|mut p| p.line_counts().ok())
                .flatten()
            {
                Some(c) => (c.insertions, c.removals, false),
                None => (0, 0, true),
            };

            resource_cache.clear_resource_cache_keep_allocation();

            let hunks = if is_binary {
                vec![]
            } else {
                vec![Hunk {
                    old_start: 1,
                    old_lines: deletions,
                    new_start: 1,
                    new_lines: additions,
                    content: String::new(),
                }]
            };

            records.push(DiffRecord {
                commit: commit.clone(),
                file_path,
                old_path,
                status,
                hunks,
                additions,
                deletions,
            });

            Ok::<_, std::convert::Infallible>(std::ops::ControlFlow::Continue(()))
        })?;

        Ok(records)
    }
}

fn bstr_to_string(b: &gix::bstr::BStr) -> String {
    b.to_str_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::walker::GitWalker;
    use crate::types::TimeRange;
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

        // First commit: add file.txt
        std::fs::write(path.join("file.txt"), "hello\n").expect("write failed");
        run(&["add", "file.txt"]);
        run(&["commit", "-m", "Initial commit"]);

        // Small delay to ensure different timestamps
        std::thread::sleep(std::time::Duration::from_millis(1100));

        // Second commit: modify file.txt
        std::fs::write(path.join("file.txt"), "hello world\nline two\n").expect("write failed");
        run(&["add", "file.txt"]);
        run(&["commit", "-m", "Second commit"]);

        dir
    }

    /// Collect commits from a repo using GitWalker.
    fn collect_commits(repo_path: &str) -> Vec<CommitInfo> {
        let walker = GitWalker::new(repo_path.to_string(), TimeRange::All);
        let mut commits = Vec::new();
        walker
            .walk(|ci| {
                commits.push(ci);
                Ok(())
            })
            .expect("walk failed");
        commits
    }

    fn make_extractor(repo_path: &str) -> DiffExtractor {
        let repo = Arc::new(gix::ThreadSafeRepository::open(repo_path).expect("open repo"));
        DiffExtractor::new(repo)
    }

    #[test]
    fn test_extract_diff_records() {
        let dir = create_test_repo();
        let repo_path = dir.path().to_str().unwrap().to_string();
        let commits = collect_commits(&repo_path);

        assert_eq!(commits.len(), 2);

        let extractor = make_extractor(&repo_path);

        // Most recent commit (index 0) modified file.txt
        let diffs = extractor
            .extract(&commits[0])
            .expect("extract failed for second commit");
        assert_eq!(diffs.len(), 1, "second commit should touch 1 file");
        assert_eq!(diffs[0].file_path, "file.txt");
        assert_eq!(diffs[0].status, FileStatus::Modified);

        // First commit (index 1) added file.txt
        let diffs = extractor
            .extract(&commits[1])
            .expect("extract failed for initial commit");
        assert_eq!(diffs.len(), 1, "initial commit should touch 1 file");
        assert_eq!(diffs[0].file_path, "file.txt");
        assert_eq!(diffs[0].status, FileStatus::Added);
    }

    #[test]
    fn test_diff_has_line_counts() {
        let dir = create_test_repo();
        let repo_path = dir.path().to_str().unwrap().to_string();
        let commits = collect_commits(&repo_path);

        let extractor = make_extractor(&repo_path);

        // Second commit modifies file.txt: should have non-zero additions and/or deletions
        let diffs = extractor.extract(&commits[0]).expect("extract failed");
        assert_eq!(diffs.len(), 1);

        let record = &diffs[0];
        let total_added = record.additions;
        let total_deleted = record.deletions;

        assert!(
            total_added > 0 || total_deleted > 0,
            "modification commit should have non-zero line counts, got +{total_added} -{total_deleted}"
        );

        // Hunks should also be present
        assert!(
            !record.hunks.is_empty(),
            "modification diff should have at least one hunk"
        );

        // Verify hunk line counts are consistent with record totals
        let hunk_added: u32 = record.hunks.iter().map(|h| h.new_lines).sum();
        let hunk_deleted: u32 = record.hunks.iter().map(|h| h.old_lines).sum();
        assert_eq!(
            hunk_added, total_added,
            "hunk additions should match record total"
        );
        assert_eq!(
            hunk_deleted, total_deleted,
            "hunk deletions should match record total"
        );
    }
}
