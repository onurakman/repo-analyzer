//! Quick filesystem-based language composition.
//!
//! Walks a directory, classifies each file via [`crate::langs::detect_language_info`],
//! counts real code lines via [`crate::analysis::line_classifier::count_lines`],
//! and returns the per-language percentage share.
//!
//! No git, no history — just HEAD-of-working-tree snapshot. Intended for a fast
//! "what is this repo written in?" answer.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use rayon::prelude::*;
use serde::Serialize;

use crate::analysis::line_classifier::count_lines;
use crate::analysis::source_filter::is_source_file;
use crate::langs::detect_language_info;

/// Per-language share of the codebase (by real code lines at HEAD of the
/// working tree). Percentages across the returned Vec sum to ~100.
#[derive(Debug, Clone, Serialize)]
pub struct LanguageShare {
    pub language: String,
    pub percentage: f64,
    pub code_lines: u64,
    pub files: u64,
}

/// Hard cap per file — anything larger is almost certainly minified/generated
/// and would skew the result.
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;

/// Directories always skipped. Conservative list — build outputs, VCS, and
/// vendored deps that would otherwise dominate.
const SKIP_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    "node_modules",
    "target",
    "dist",
    "build",
    "out",
    ".venv",
    "venv",
    "__pycache__",
    ".next",
    ".nuxt",
    ".cache",
    ".gradle",
    ".idea",
    ".vscode",
    "vendor",
    "bower_components",
];

/// Return per-language share of the repo at `root`, sorted descending by
/// percentage. Languages contributing no real code lines are omitted.
///
/// Pure filesystem walk: does not read `.git`, does not consult history.
#[must_use]
pub fn repo_composition(root: &Path) -> Vec<LanguageShare> {
    let files = collect_files(root);

    let classified: Vec<(&'static str, u64)> = files
        .par_iter()
        .filter_map(|path| classify_file(path))
        .collect();

    aggregate(classified)
}

fn classify_file(path: &Path) -> Option<(&'static str, u64)> {
    let path_str = path.to_str()?;
    // Drop lockfiles, manifests, docs, data/markup dialects (YAML/JSON/TOML/
    // Markdown/XML…) — shared with every code-focused metric.
    if !is_source_file(path_str) {
        return None;
    }
    let meta = fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() == 0 || meta.len() > MAX_FILE_BYTES {
        return None;
    }
    let bytes = fs::read(path).ok()?;
    if is_probably_binary(&bytes) {
        return None;
    }
    let content = std::str::from_utf8(&bytes).ok()?;
    let lang = detect_language_info(path_str, Some(content))?;
    let counts = count_lines(content, Some(lang));
    if counts.code == 0 {
        return None;
    }
    Some((lang.name, counts.code))
}

fn aggregate(classified: Vec<(&'static str, u64)>) -> Vec<LanguageShare> {
    let mut buckets: HashMap<&'static str, (u64, u64)> = HashMap::new();
    for (name, code) in classified {
        let entry = buckets.entry(name).or_insert((0, 0));
        entry.0 = entry.0.saturating_add(code);
        entry.1 = entry.1.saturating_add(1);
    }

    let total: u64 = buckets.values().map(|(c, _)| *c).sum();
    if total == 0 {
        return Vec::new();
    }

    let mut out: Vec<LanguageShare> = buckets
        .into_iter()
        .map(|(name, (code, files))| {
            let raw = (code as f64 / total as f64) * 100.0;
            LanguageShare {
                language: name.to_string(),
                percentage: (raw * 100.0).round() / 100.0,
                code_lines: code,
                files,
            }
        })
        .collect();

    out.sort_by(|a, b| {
        b.percentage
            .partial_cmp(&a.percentage)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out
}

fn collect_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if SKIP_DIRS.iter().any(|d| *d == name_str.as_ref()) {
                    continue;
                }
                stack.push(path);
            } else if ft.is_file() {
                out.push(path);
            }
        }
    }
    out
}

/// NUL byte in first 8 KB → binary. Same heuristic as [`crate::metrics::composition`].
fn is_probably_binary(data: &[u8]) -> bool {
    let sample = &data[..data.len().min(8192)];
    sample.contains(&0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{File, create_dir_all};
    use std::io::Write;

    fn write_file(root: &Path, rel: &str, body: &str) {
        let p = root.join(rel);
        if let Some(parent) = p.parent() {
            create_dir_all(parent).unwrap();
        }
        let mut f = File::create(p).unwrap();
        f.write_all(body.as_bytes()).unwrap();
    }

    #[test]
    fn single_rust_file_is_100_percent_rust() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(tmp.path(), "src/main.rs", "fn main() {}\nfn a() {}\n");
        let comp = repo_composition(tmp.path());
        assert_eq!(comp.len(), 1);
        assert_eq!(comp[0].language, "Rust");
        assert!((comp[0].percentage - 100.0).abs() < 0.001);
    }

    #[test]
    fn mixed_repo_sums_to_100_sorted_desc() {
        let tmp = tempfile::tempdir().unwrap();
        // 6 Java code lines
        write_file(
            tmp.path(),
            "App.java",
            "class A {}\nclass B {}\nclass C {}\nclass D {}\nclass E {}\nclass F {}\n",
        );
        // 2 Python code lines
        write_file(tmp.path(), "x.py", "x = 1\ny = 2\n");

        let comp = repo_composition(tmp.path());
        let total_pct: f64 = comp.iter().map(|s| s.percentage).sum();
        // Percentages are rounded to 2 decimals, so sum may drift slightly.
        assert!((total_pct - 100.0).abs() < 0.05);
        assert_eq!(comp[0].language, "Java");
        assert!(comp[0].percentage > comp[1].percentage);
    }

    #[test]
    fn skipped_dirs_are_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(tmp.path(), "src/main.rs", "fn main() {}\n");
        // node_modules / target should NOT count
        write_file(
            tmp.path(),
            "node_modules/pkg/index.js",
            "var a=1;\nvar b=2;\nvar c=3;\n",
        );
        write_file(tmp.path(), "target/debug/foo.rs", "fn junk() {}\n");
        let comp = repo_composition(tmp.path());
        assert_eq!(comp.len(), 1);
        assert_eq!(comp[0].language, "Rust");
    }

    #[test]
    fn empty_dir_returns_empty_vec() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(repo_composition(tmp.path()).is_empty());
    }

    #[test]
    fn binary_files_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(tmp.path(), "src/main.rs", "fn main() {}\n");
        let bin_path = tmp.path().join("blob.rs");
        std::fs::write(&bin_path, b"\x00\x01\x02fake\x00binary\x00").unwrap();
        let comp = repo_composition(tmp.path());
        assert_eq!(comp.len(), 1);
        assert_eq!(comp[0].code_lines, 1);
    }
}
