use std::collections::HashMap;

use crate::analysis::line_classifier::count_lines;
use crate::langs::{Language, detect_language_info};
use crate::metrics::MetricCollector;
use crate::types::{
    Column, MetricEntry, MetricResult, MetricValue, ParsedChange, report_description,
    report_display,
};

/// Skip blobs above this size — they are almost always minified/generated
/// and would dominate the totals while also being the slowest to classify.
const MAX_BLOB_BYTES: u64 = 2 * 1024 * 1024;

struct LangBucket {
    name: &'static str,
    files: u64,
    code: u64,
    comment: u64,
    blank: u64,
    bytes: u64,
}

pub struct CompositionCollector {
    buckets: HashMap<&'static str, LangBucket>,
    unknown_files: u64,
    unknown_bytes: u64,
    binary_files: u64,
}

impl Default for CompositionCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl CompositionCollector {
    pub fn new() -> Self {
        Self {
            buckets: HashMap::new(),
            unknown_files: 0,
            unknown_bytes: 0,
            binary_files: 0,
        }
    }

    fn record(&mut self, lang: &'static Language, bytes: u64, content: &str) {
        let counts = count_lines(content, Some(lang));
        let entry = self.buckets.entry(lang.name).or_insert(LangBucket {
            name: lang.name,
            files: 0,
            code: 0,
            comment: 0,
            blank: 0,
            bytes: 0,
        });
        entry.files += 1;
        entry.code = entry.code.saturating_add(counts.code);
        entry.comment = entry.comment.saturating_add(counts.comment);
        entry.blank = entry.blank.saturating_add(counts.blank);
        entry.bytes = entry.bytes.saturating_add(bytes);
    }
}

impl MetricCollector for CompositionCollector {
    fn name(&self) -> &str {
        "composition"
    }

    fn process(&mut self, _change: &ParsedChange) {}

    fn inspect_repo(
        &mut self,
        repo: &gix::Repository,
        _progress: &crate::metrics::ProgressReporter,
    ) -> anyhow::Result<()> {
        let head_commit = match repo.head_commit() {
            Ok(c) => c,
            Err(_) => return Ok(()),
        };
        let tree = head_commit.tree()?;
        walk_tree(repo, &tree, "", self);
        Ok(())
    }

    fn finalize(&mut self) -> MetricResult {
        let mut buckets: Vec<LangBucket> = self.buckets.drain().map(|(_, v)| v).collect();
        buckets.sort_by_key(|b| std::cmp::Reverse(b.code));

        let total_code: u64 = buckets.iter().map(|b| b.code).sum();

        let mut entries: Vec<MetricEntry> = Vec::with_capacity(buckets.len());
        for b in buckets {
            let total_lines = b.code + b.comment + b.blank;
            let code_pct = if total_code > 0 {
                (b.code as f64 / total_code as f64) * 100.0
            } else {
                0.0
            };
            let comment_ratio = if b.code + b.comment > 0 {
                (b.comment as f64 / (b.code + b.comment) as f64) * 100.0
            } else {
                0.0
            };

            let mut values = HashMap::new();
            values.insert("files".into(), MetricValue::Count(b.files));
            values.insert("code".into(), MetricValue::Count(b.code));
            values.insert("comment".into(), MetricValue::Count(b.comment));
            values.insert("blank".into(), MetricValue::Count(b.blank));
            values.insert("total_lines".into(), MetricValue::Count(total_lines));
            values.insert("code_pct".into(), MetricValue::Float(code_pct));
            values.insert(
                "comment_ratio_pct".into(),
                MetricValue::Float(comment_ratio),
            );
            values.insert("bytes".into(), MetricValue::Count(b.bytes));

            entries.push(MetricEntry {
                key: b.name.to_string(),
                values,
            });
        }

        if self.unknown_files > 0 {
            let mut values = HashMap::new();
            values.insert("files".into(), MetricValue::Count(self.unknown_files));
            values.insert("code".into(), MetricValue::Count(0));
            values.insert("comment".into(), MetricValue::Count(0));
            values.insert("blank".into(), MetricValue::Count(0));
            values.insert("total_lines".into(), MetricValue::Count(0));
            values.insert("code_pct".into(), MetricValue::Float(0.0));
            values.insert("comment_ratio_pct".into(), MetricValue::Float(0.0));
            values.insert("bytes".into(), MetricValue::Count(self.unknown_bytes));
            entries.push(MetricEntry {
                key: "(unknown)".into(),
                values,
            });
        }
        if self.binary_files > 0 {
            let mut values = HashMap::new();
            values.insert("files".into(), MetricValue::Count(self.binary_files));
            values.insert("code".into(), MetricValue::Count(0));
            values.insert("comment".into(), MetricValue::Count(0));
            values.insert("blank".into(), MetricValue::Count(0));
            values.insert("total_lines".into(), MetricValue::Count(0));
            values.insert("code_pct".into(), MetricValue::Float(0.0));
            values.insert("comment_ratio_pct".into(), MetricValue::Float(0.0));
            values.insert("bytes".into(), MetricValue::Count(0));
            entries.push(MetricEntry {
                key: "(binary/skipped)".into(),
                values,
            });
        }

        MetricResult {
            name: "composition".into(),
            display_name: report_display("composition"),
            description: report_description("composition"),
            entry_groups: vec![],
            columns: vec![
                Column::in_report("composition", "files"),
                Column::in_report("composition", "code"),
                Column::in_report("composition", "comment"),
                Column::in_report("composition", "blank"),
                Column::in_report("composition", "total_lines"),
                Column::in_report("composition", "code_pct"),
                Column::in_report("composition", "comment_ratio_pct"),
                Column::in_report("composition", "bytes"),
            ],
            entries,
        }
    }
}

fn walk_tree(
    repo: &gix::Repository,
    tree: &gix::Tree,
    prefix: &str,
    coll: &mut CompositionCollector,
) {
    for entry_res in tree.iter() {
        let entry = match entry_res {
            Ok(e) => e,
            Err(_) => continue,
        };
        let name = entry.filename().to_string();
        let full_path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let id = entry.oid();
        let mode = entry.mode();

        if mode.is_tree() {
            if let Ok(subobj) = repo.find_object(id)
                && let Ok(subtree) = subobj.try_into_tree()
            {
                walk_tree(repo, &subtree, &full_path, coll);
            }
        } else if mode.is_blob() {
            let Ok(obj) = repo.find_object(id) else {
                continue;
            };
            let bytes = obj.data.len() as u64;
            if bytes == 0 {
                continue;
            }
            if bytes > MAX_BLOB_BYTES {
                coll.unknown_files += 1;
                coll.unknown_bytes += bytes;
                continue;
            }
            if is_probably_binary(&obj.data) {
                coll.binary_files += 1;
                continue;
            }
            // UTF-8 only for now — most source repos.
            let Ok(content) = std::str::from_utf8(&obj.data) else {
                coll.binary_files += 1;
                continue;
            };
            match detect_language_info(&full_path, Some(content)) {
                Some(lang) => coll.record(lang, bytes, content),
                None => {
                    coll.unknown_files += 1;
                    coll.unknown_bytes += bytes;
                }
            }
        }
    }
}

/// Cheap binary heuristic: NUL byte in first 8 KB → binary.
fn is_probably_binary(data: &[u8]) -> bool {
    let sample = &data[..data.len().min(8192)];
    sample.contains(&0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::langs::detect_language_info;

    #[test]
    fn binary_heuristic_flags_nul_byte() {
        assert!(is_probably_binary(b"\x7fELF\x02\x01\x01\x00"));
        assert!(is_probably_binary(b"hello\0world"));
    }

    #[test]
    fn binary_heuristic_accepts_plain_text() {
        assert!(!is_probably_binary(b"fn main() { println!(\"hi\"); }\n"));
        assert!(!is_probably_binary(b""));
    }

    #[test]
    fn record_aggregates_per_language_buckets() {
        let rust = detect_language_info("a.rs", None).expect("rust");
        let toml = detect_language_info("Cargo.toml", None).expect("toml");
        let mut coll = CompositionCollector::new();
        coll.record(rust, 100, "fn a() {}\n");
        coll.record(rust, 50, "fn b() {}\n// doc\n\n");
        coll.record(toml, 200, "[package]\nname = \"x\"\n");

        let rust_bucket = coll.buckets.get("Rust").expect("rust bucket");
        assert_eq!(rust_bucket.files, 2);
        assert_eq!(rust_bucket.code, 2);
        assert_eq!(rust_bucket.bytes, 150);

        let toml_bucket = coll.buckets.get("TOML").expect("toml bucket");
        assert_eq!(toml_bucket.files, 1);
        assert_eq!(toml_bucket.code, 2);
    }

    #[test]
    fn finalize_sorts_languages_by_code_desc_and_reports_percentages() {
        let rust = detect_language_info("a.rs", None).expect("rust");
        let toml = detect_language_info("Cargo.toml", None).expect("toml");
        let mut coll = CompositionCollector::new();
        // 10 code lines of Rust, 2 of TOML — Rust should lead.
        coll.record(rust, 1, "fn a() {}\nfn b() {}\nfn c() {}\nfn d() {}\nfn e() {}\nfn f() {}\nfn g() {}\nfn h() {}\nfn i() {}\nfn j() {}\n");
        coll.record(toml, 1, "a = 1\nb = 2\n");

        let result = coll.finalize();
        assert_eq!(result.name, "composition");
        // First entry is the language with the most code.
        assert_eq!(result.entries[0].key, "Rust");
        assert_eq!(result.entries[1].key, "TOML");

        // Percentages should sum to ~100 across languages (ignoring rounding).
        let sum_pct: f64 = result
            .entries
            .iter()
            .filter_map(|e| match e.values.get("code_pct") {
                Some(crate::types::MetricValue::Float(f)) => Some(*f),
                _ => None,
            })
            .sum();
        assert!(
            (sum_pct - 100.0).abs() < 0.001,
            "code_pct should sum to 100, got {sum_pct}"
        );
    }

    #[test]
    fn unknown_and_binary_buckets_surface_in_finalize() {
        let mut coll = CompositionCollector::new();
        coll.unknown_files = 3;
        coll.unknown_bytes = 500;
        coll.binary_files = 2;
        let result = coll.finalize();
        let keys: Vec<&str> = result.entries.iter().map(|e| e.key.as_str()).collect();
        assert!(keys.contains(&"(unknown)"));
        assert!(keys.contains(&"(binary/skipped)"));
    }
}
