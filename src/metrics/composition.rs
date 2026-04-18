use std::collections::HashMap;

use crate::analysis::line_classifier::count_lines;
use crate::langs::{Language, detect_language_info};
use crate::metrics::MetricCollector;
use crate::types::{MetricEntry, MetricResult, MetricValue, ParsedChange};

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
        buckets.sort_by(|a, b| b.code.cmp(&a.code));

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
            display_name: "Code Composition".into(),
            description: "Language breakdown at HEAD. Real code vs comment vs blank lines (not raw line counts) — block comments, nested comments, and shebangs are classified correctly via a 460-language knowledge base. Useful for sizing the codebase and spotting unexpected dominance (e.g. vendored JS crowding out Rust).".into(),
            entry_groups: vec![],
            column_labels: vec![],
            columns: vec![
                "files".into(),
                "code".into(),
                "comment".into(),
                "blank".into(),
                "total_lines".into(),
                "code_pct".into(),
                "comment_ratio_pct".into(),
                "bytes".into(),
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
