use std::sync::Arc;

use gix::bstr::ByteSlice;
use gix::object::tree::diff::Change;

use crate::interner::Interner;
use crate::types::{CommitInfo, DiffRecord, FileStatus, Hunk};

/// Extracts file-level diff information from git commits using `gix` (native, no subprocess).
pub struct DiffExtractor {
    interner: Arc<Interner>,
}

impl DiffExtractor {
    pub fn new(interner: Arc<Interner>) -> Self {
        Self { interner }
    }

    /// Extract diff records for a single commit by comparing its tree to its parent's tree.
    /// For initial commits (no parent), compares against the empty tree.
    ///
    /// `repo` is the caller's per-worker thread-local repository, reused across
    /// commits so its object cache survives between them (finding #24). Each
    /// record is paired with the new blob's `ObjectId` (finding #25) so the
    /// parser can fetch size/content directly instead of re-walking the tree.
    pub fn extract(
        &self,
        repo: &gix::Repository,
        commit: &Arc<CommitInfo>,
    ) -> anyhow::Result<Vec<(DiffRecord, Option<gix::ObjectId>)>> {
        let new_commit_id = gix::ObjectId::from_hex(commit.oid.as_bytes())?;
        let new_tree = repo.find_object(new_commit_id)?.try_into_commit()?.tree()?;

        let old_tree = if let Some(parent_str) = commit.parent_ids.first() {
            let parent_id = gix::ObjectId::from_hex(parent_str.as_bytes())?;
            repo.find_object(parent_id)?.try_into_commit()?.tree()?
        } else {
            repo.empty_tree()
        };

        let mut records: Vec<(DiffRecord, Option<gix::ObjectId>)> = Vec::new();
        let mut resource_cache = repo.diff_resource_cache_for_tree_diff()?;

        let mut platform = old_tree.changes()?;

        platform.for_each_to_obtain_tree(&new_tree, |change| {
            // Extract path + status + new-blob oid. Skip non-blob entries.
            let parsed = match &change {
                Change::Addition {
                    location,
                    entry_mode,
                    id,
                    ..
                } => {
                    if !entry_mode.is_blob() {
                        return Ok::<_, std::convert::Infallible>(std::ops::ControlFlow::Continue(
                            (),
                        ));
                    }
                    (
                        bstr_to_string(location),
                        None,
                        FileStatus::Added,
                        Some(id.detach()),
                    )
                }
                Change::Deletion {
                    location,
                    entry_mode,
                    ..
                } => {
                    if !entry_mode.is_blob() {
                        return Ok(std::ops::ControlFlow::Continue(()));
                    }
                    // No new content for a deletion — no blob to parse.
                    (bstr_to_string(location), None, FileStatus::Deleted, None)
                }
                Change::Modification {
                    location,
                    entry_mode,
                    id,
                    ..
                } => {
                    if !entry_mode.is_blob() {
                        return Ok(std::ops::ControlFlow::Continue(()));
                    }
                    (
                        bstr_to_string(location),
                        None,
                        FileStatus::Modified,
                        Some(id.detach()),
                    )
                }
                Change::Rewrite {
                    location,
                    source_location,
                    copy,
                    entry_mode,
                    id,
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
                    (
                        bstr_to_string(location),
                        Some(bstr_to_string(source_location)),
                        st,
                        Some(id.detach()),
                    )
                }
            };

            let (file_path, old_path, status, blob_oid) = parsed;

            let (additions, deletions, is_binary) = match change
                .diff(&mut resource_cache)
                .ok()
                .and_then(|mut p| p.line_counts().ok())
                .flatten()
            {
                Some(c) => (c.insertions, c.removals, false),
                None => (0, 0, true),
            };

            // Real per-hunk line ranges. The `line_counts()` call above already
            // loaded both blob resources into `resource_cache` and forced the
            // internal diff, so we re-drive the prepared diff to read
            // imara-diff's true `(before, after)` ranges. The old code
            // fabricated a single `(1, additions)` hunk, so every edit that
            // wasn't at the top of the file was attributed to the wrong
            // construct (construct_churn / construct_ownership / hotspots).
            // `Platform::lines()` only exposes hunk *content*, not offsets, so
            // we go one level lower. (finding #1)
            let hunks = if is_binary {
                Vec::new()
            } else {
                real_hunks(&mut resource_cache)
            };

            resource_cache.clear_resource_cache_keep_allocation();

            // Intern paths so repeated file paths across commits share one allocation.
            let file_path: Arc<str> = self.interner.intern(&file_path);
            let old_path: Option<Arc<str>> = old_path.map(|p| self.interner.intern(&p));

            records.push((
                DiffRecord {
                    commit: Arc::clone(commit),
                    file_path,
                    old_path,
                    status,
                    hunks,
                    additions,
                    deletions,
                },
                blob_oid,
            ));

            Ok::<_, std::convert::Infallible>(std::ops::ControlFlow::Continue(()))
        })?;

        Ok(records)
    }
}

fn bstr_to_string(b: &gix::bstr::BStr) -> String {
    b.to_str_lossy().into_owned()
}

/// Extract real per-hunk line ranges from a blob diff whose resources are
/// already loaded into `resource_cache` (by a preceding `change.diff(...)`).
/// Returns 1-based inclusive [`Hunk`]s. Empty when the diff can't be performed
/// internally (external diff driver, binary), matching the binary fallback.
fn real_hunks(resource_cache: &mut gix::diff::blob::Platform) -> Vec<Hunk> {
    use gix::diff::blob::platform::prepare_diff::Operation;

    let Ok(prep) = resource_cache.prepare_diff() else {
        return Vec::new();
    };
    let Operation::InternalDiff { algorithm } = prep.operation else {
        return Vec::new();
    };
    let input = prep.interned_input();
    let diff = gix::diff::blob::diff_with_slider_heuristics(algorithm, &input);
    diff.hunks()
        .map(|h| Hunk {
            // imara-diff ranges are 0-based half-open; the construct filter
            // (registry.rs) expects 1-based inclusive line numbers.
            old_start: h.before.start + 1,
            old_lines: h.before.end - h.before.start,
            new_start: h.after.start + 1,
            new_lines: h.after.end - h.after.start,
        })
        .collect()
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
    fn collect_commits(repo_path: &str) -> Vec<Arc<CommitInfo>> {
        let walker = GitWalker::new(
            repo_path.to_string(),
            TimeRange::All,
            Arc::new(crate::interner::Interner::new()),
        );
        let mut commits = Vec::new();
        walker
            .walk(|ci| {
                commits.push(Arc::new(ci));
                Ok(())
            })
            .expect("walk failed");
        commits
    }

    fn make_extractor(repo_path: &str) -> (DiffExtractor, gix::Repository) {
        let repo = gix::ThreadSafeRepository::open(repo_path)
            .expect("open repo")
            .to_thread_local();
        let extractor = DiffExtractor::new(Arc::new(crate::interner::Interner::new()));
        (extractor, repo)
    }

    #[test]
    fn test_extract_diff_records() {
        let dir = create_test_repo();
        let repo_path = dir.path().to_str().unwrap().to_string();
        let commits = collect_commits(&repo_path);

        assert_eq!(commits.len(), 2);

        let (extractor, repo) = make_extractor(&repo_path);

        // Most recent commit (index 0) modified file.txt
        let diffs = extractor
            .extract(&repo, &commits[0])
            .expect("extract failed for second commit");
        assert_eq!(diffs.len(), 1, "second commit should touch 1 file");
        assert_eq!(&*diffs[0].0.file_path, "file.txt");
        assert_eq!(diffs[0].0.status, FileStatus::Modified);
        assert!(diffs[0].1.is_some(), "modification carries a new blob oid");

        // First commit (index 1) added file.txt
        let diffs = extractor
            .extract(&repo, &commits[1])
            .expect("extract failed for initial commit");
        assert_eq!(diffs.len(), 1, "initial commit should touch 1 file");
        assert_eq!(&*diffs[0].0.file_path, "file.txt");
        assert_eq!(diffs[0].0.status, FileStatus::Added);
    }

    /// A modification confined to the middle of a file must produce a hunk that
    /// *starts at that line*, not a fabricated `(1, additions)` range. This is
    /// the core of finding #1 — construct attribution depends on real offsets.
    fn create_middle_edit_repo() -> TempDir {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path();
        let run = |args: &[&str]| {
            let out = Command::new("git")
                .args(args)
                .current_dir(path)
                .env("GIT_AUTHOR_NAME", "Test User")
                .env("GIT_AUTHOR_EMAIL", "test@example.com")
                .env("GIT_COMMITTER_NAME", "Test User")
                .env("GIT_COMMITTER_EMAIL", "test@example.com")
                .output()
                .expect("git");
            assert!(out.status.success(), "git {args:?} failed");
        };
        run(&["init"]);
        run(&["config", "user.name", "Test User"]);
        run(&["config", "user.email", "test@example.com"]);
        std::fs::write(path.join("f.txt"), "a\nb\nc\nd\ne\nf\ng\nh\n").expect("write");
        run(&["add", "f.txt"]);
        run(&["commit", "-m", "base"]);
        std::thread::sleep(std::time::Duration::from_millis(1100));
        // Edit only line 5 ("e" -> "EEE"); lines 1-4 and 6-8 are unchanged.
        std::fs::write(path.join("f.txt"), "a\nb\nc\nd\nEEE\nf\ng\nh\n").expect("write");
        run(&["add", "f.txt"]);
        run(&["commit", "-m", "edit middle"]);
        dir
    }

    #[test]
    fn test_hunks_capture_real_positions() {
        let dir = create_middle_edit_repo();
        let repo_path = dir.path().to_str().unwrap().to_string();
        let commits = collect_commits(&repo_path);
        let (extractor, repo) = make_extractor(&repo_path);

        let diffs = extractor.extract(&repo, &commits[0]).expect("extract");
        assert_eq!(diffs.len(), 1);
        let rec = &diffs[0].0;

        assert!(!rec.hunks.is_empty(), "modification must have a hunk");
        // The edit is on line 5 — no hunk may claim to start at line 1.
        assert!(
            rec.hunks
                .iter()
                .all(|h| h.new_start >= 5 && h.old_start >= 5),
            "hunk must start at the real edit line, got {:?}",
            rec.hunks
        );
        // Hunk line totals must still reconcile with the record totals so
        // construct_churn's line accounting stays consistent.
        let ha: u32 = rec.hunks.iter().map(|h| h.new_lines).sum();
        let hd: u32 = rec.hunks.iter().map(|h| h.old_lines).sum();
        assert_eq!(ha, rec.additions, "hunk additions reconcile with total");
        assert_eq!(hd, rec.deletions, "hunk deletions reconcile with total");
    }

    #[test]
    fn test_diff_has_line_counts() {
        let dir = create_test_repo();
        let repo_path = dir.path().to_str().unwrap().to_string();
        let commits = collect_commits(&repo_path);

        let (extractor, repo) = make_extractor(&repo_path);

        // Second commit modifies file.txt: should have non-zero additions and/or deletions
        let diffs = extractor
            .extract(&repo, &commits[0])
            .expect("extract failed");
        assert_eq!(diffs.len(), 1);

        let record = &diffs[0].0;
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
