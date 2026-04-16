use std::process::Command;

use crate::types::{CommitInfo, DiffRecord, FileStatus, Hunk};

/// Extracts file-level diff information from git commits.
pub struct DiffExtractor {
    repo_path: String,
}

impl DiffExtractor {
    pub fn new(repo_path: String) -> Self {
        Self { repo_path }
    }

    /// Extract diff records for a single commit by comparing its tree to its parent's tree.
    /// For initial commits (no parent), compares against the empty tree.
    pub fn extract(&self, commit: &CommitInfo) -> anyhow::Result<Vec<DiffRecord>> {
        let oid = &commit.oid;

        // Use git diff-tree to get file-level stats.
        // For root commits (no parent), use --root flag which diffs against empty tree.
        let numstat_output = if commit.parent_ids.is_empty() {
            let output = Command::new("git")
                .args(["diff-tree", "--root", "--numstat", "-r", "-z", "-M", oid])
                .current_dir(&self.repo_path)
                .output()?;
            if !output.status.success() {
                anyhow::bail!(
                    "git diff-tree failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            String::from_utf8_lossy(&output.stdout).to_string()
        } else {
            let parent = &commit.parent_ids[0];
            let output = Command::new("git")
                .args(["diff-tree", "--numstat", "-r", "-z", "-M", parent, oid])
                .current_dir(&self.repo_path)
                .output()?;
            if !output.status.success() {
                anyhow::bail!(
                    "git diff-tree failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            String::from_utf8_lossy(&output.stdout).to_string()
        };

        // Also get the raw status output for detecting renames/adds/deletes/modifies.
        let raw_output = if commit.parent_ids.is_empty() {
            let output = Command::new("git")
                .args([
                    "diff-tree", "--root", "-r", "-z", "-M", "--diff-filter=AMDRT",
                    "--name-status", oid,
                ])
                .current_dir(&self.repo_path)
                .output()?;
            if !output.status.success() {
                anyhow::bail!(
                    "git diff-tree (name-status) failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            String::from_utf8_lossy(&output.stdout).to_string()
        } else {
            let parent = &commit.parent_ids[0];
            let output = Command::new("git")
                .args([
                    "diff-tree", "-r", "-z", "-M", "--diff-filter=AMDRT",
                    "--name-status", parent, oid,
                ])
                .current_dir(&self.repo_path)
                .output()?;
            if !output.status.success() {
                anyhow::bail!(
                    "git diff-tree (name-status) failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            String::from_utf8_lossy(&output.stdout).to_string()
        };

        // Parse name-status output into a map of file_path -> (status, old_path)
        let status_map = parse_name_status(&raw_output);

        // Parse numstat output into diff records
        let records = parse_numstat(&numstat_output, commit, &status_map);

        Ok(records)
    }
}

/// Parsed file status entry from --name-status output.
struct FileEntry {
    status: FileStatus,
    file_path: String,
    old_path: Option<String>,
}

/// Parse NUL-delimited --name-status output.
/// Format for non-renames: status\0path\0
/// Format for renames: Rxx\0old_path\0new_path\0
/// The first field for --root diffs may include the commit hash, skip it.
fn parse_name_status(raw: &str) -> Vec<FileEntry> {
    let mut entries = Vec::new();
    // Split on NUL, filter empty
    let parts: Vec<&str> = raw.split('\0').filter(|s| !s.is_empty()).collect();

    let mut i = 0;
    while i < parts.len() {
        let field = parts[i].trim();

        // Skip commit hash lines (40-char hex)
        if field.len() == 40 && field.chars().all(|c| c.is_ascii_hexdigit()) {
            i += 1;
            continue;
        }

        // Determine status character
        let status_char = field.chars().next().unwrap_or('M');

        match status_char {
            'A' => {
                if i + 1 < parts.len() {
                    entries.push(FileEntry {
                        status: FileStatus::Added,
                        file_path: parts[i + 1].to_string(),
                        old_path: None,
                    });
                    i += 2;
                } else {
                    i += 1;
                }
            }
            'D' => {
                if i + 1 < parts.len() {
                    entries.push(FileEntry {
                        status: FileStatus::Deleted,
                        file_path: parts[i + 1].to_string(),
                        old_path: None,
                    });
                    i += 2;
                } else {
                    i += 1;
                }
            }
            'R' => {
                // Rename: Rxx\0old_path\0new_path
                if i + 2 < parts.len() {
                    entries.push(FileEntry {
                        status: FileStatus::Renamed,
                        file_path: parts[i + 2].to_string(),
                        old_path: Some(parts[i + 1].to_string()),
                    });
                    i += 3;
                } else {
                    i += 1;
                }
            }
            'M' | 'T' | _ => {
                if i + 1 < parts.len() {
                    entries.push(FileEntry {
                        status: FileStatus::Modified,
                        file_path: parts[i + 1].to_string(),
                        old_path: None,
                    });
                    i += 2;
                } else {
                    i += 1;
                }
            }
        }
    }

    entries
}

/// Parse NUL-delimited --numstat output and combine with status info.
/// numstat format: added\tdeleted\tpath (or for renames: added\tdeleted\told\0new)
fn parse_numstat(
    raw: &str,
    commit: &CommitInfo,
    status_entries: &[FileEntry],
) -> Vec<DiffRecord> {
    let mut records = Vec::new();

    // Split on NUL
    let parts: Vec<&str> = raw.split('\0').filter(|s| !s.is_empty()).collect();

    let mut i = 0;
    while i < parts.len() {
        let line = parts[i].trim();

        // Skip commit hash lines
        if line.len() == 40 && line.chars().all(|c| c.is_ascii_hexdigit()) {
            i += 1;
            continue;
        }

        // Each numstat line: "added\tdeleted\tpath"
        // For binary files: "-\t-\tpath"
        // For renames: "added\tdeleted\t" followed by old_path\0new_path
        let tab_parts: Vec<&str> = line.splitn(3, '\t').collect();
        if tab_parts.len() < 3 {
            i += 1;
            continue;
        }

        let additions_str = tab_parts[0];
        let deletions_str = tab_parts[1];
        let path_part = tab_parts[2];

        let is_binary = additions_str == "-" && deletions_str == "-";
        let additions: u32 = additions_str.parse().unwrap_or(0);
        let deletions: u32 = deletions_str.parse().unwrap_or(0);

        // Determine file path — for renames, path_part is empty and next two NUL parts are old/new
        let (file_path, old_path, consumed_extra) = if path_part.is_empty() && i + 2 < parts.len()
        {
            // Rename case: numstat gives empty path, then old\0new
            let old = parts[i + 1].to_string();
            let new = parts[i + 2].to_string();
            (new, Some(old), 2)
        } else {
            (path_part.to_string(), None, 0)
        };

        // Look up status from name-status parsing
        let (status, final_old_path) = find_status_for_path(
            &file_path,
            &old_path,
            status_entries,
            additions,
            deletions,
        );

        // Build a single hunk representing the whole-file change
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
            old_path: final_old_path,
            status,
            hunks,
            additions,
            deletions,
        });

        i += 1 + consumed_extra;
    }

    records
}

/// Find the file status from the name-status entries, falling back to heuristic.
fn find_status_for_path(
    file_path: &str,
    numstat_old_path: &Option<String>,
    status_entries: &[FileEntry],
    additions: u32,
    deletions: u32,
) -> (FileStatus, Option<String>) {
    // Try to find exact match in status entries
    for entry in status_entries {
        if entry.file_path == file_path {
            return (entry.status, entry.old_path.clone());
        }
    }

    // If numstat gave us an old_path (rename), use that
    if let Some(old) = numstat_old_path {
        return (FileStatus::Renamed, Some(old.clone()));
    }

    // Fallback heuristic based on line counts
    if deletions == 0 && additions > 0 {
        (FileStatus::Added, None)
    } else if additions == 0 && deletions > 0 {
        (FileStatus::Deleted, None)
    } else {
        (FileStatus::Modified, None)
    }
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

    #[test]
    fn test_extract_diff_records() {
        let dir = create_test_repo();
        let repo_path = dir.path().to_str().unwrap().to_string();
        let commits = collect_commits(&repo_path);

        assert_eq!(commits.len(), 2);

        let extractor = DiffExtractor::new(repo_path);

        // Most recent commit (index 0) modified file.txt
        let diffs = extractor.extract(&commits[0]).expect("extract failed for second commit");
        assert_eq!(diffs.len(), 1, "second commit should touch 1 file");
        assert_eq!(diffs[0].file_path, "file.txt");
        assert_eq!(diffs[0].status, FileStatus::Modified);

        // First commit (index 1) added file.txt
        let diffs = extractor.extract(&commits[1]).expect("extract failed for initial commit");
        assert_eq!(diffs.len(), 1, "initial commit should touch 1 file");
        assert_eq!(diffs[0].file_path, "file.txt");
        assert_eq!(diffs[0].status, FileStatus::Added);
    }

    #[test]
    fn test_diff_has_line_counts() {
        let dir = create_test_repo();
        let repo_path = dir.path().to_str().unwrap().to_string();
        let commits = collect_commits(&repo_path);

        let extractor = DiffExtractor::new(repo_path);

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
        assert_eq!(hunk_added, total_added, "hunk additions should match record total");
        assert_eq!(hunk_deleted, total_deleted, "hunk deletions should match record total");
    }
}
