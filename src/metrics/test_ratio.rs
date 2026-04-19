use std::collections::HashMap;

use crate::analysis::line_classifier::count_lines;
use crate::analysis::source_filter::is_source_file;
use crate::langs::detect_language_info;
use crate::metrics::MetricCollector;
use crate::types::{
    Column, MetricEntry, MetricResult, MetricValue, report_description, report_display,
};

/// Skip blobs above this size — same threshold as composition.
const MAX_BLOB_BYTES: u64 = 2 * 1024 * 1024;

struct LangStats {
    source_files: u64,
    test_files: u64,
    source_lines: u64,
    test_lines: u64,
}

pub struct TestRatioCollector {
    stats: HashMap<String, LangStats>,
}

impl Default for TestRatioCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl TestRatioCollector {
    pub fn new() -> Self {
        Self {
            stats: HashMap::new(),
        }
    }
}

impl MetricCollector for TestRatioCollector {
    fn name(&self) -> &str {
        "test_ratio"
    }

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
        let mut langs: Vec<(String, LangStats)> = self.stats.drain().collect();
        langs.sort_by(|a, b| {
            (b.1.source_lines + b.1.test_lines).cmp(&(a.1.source_lines + a.1.test_lines))
        });

        let total_source_files: u64 = langs.iter().map(|(_, s)| s.source_files).sum();
        let total_test_files: u64 = langs.iter().map(|(_, s)| s.test_files).sum();
        let total_source_lines: u64 = langs.iter().map(|(_, s)| s.source_lines).sum();
        let total_test_lines: u64 = langs.iter().map(|(_, s)| s.test_lines).sum();

        let mut entries: Vec<MetricEntry> = langs
            .into_iter()
            .filter(|(_, s)| s.source_lines + s.test_lines > 0)
            .map(|(lang, s)| {
                let total_code = s.source_lines + s.test_lines;
                let test_pct = if total_code > 0 {
                    (s.test_lines as f64 / total_code as f64) * 100.0
                } else {
                    0.0
                };
                let mut values = HashMap::new();
                values.insert("source_files".into(), MetricValue::Count(s.source_files));
                values.insert("test_files".into(), MetricValue::Count(s.test_files));
                values.insert("source_lines".into(), MetricValue::Count(s.source_lines));
                values.insert("test_lines".into(), MetricValue::Count(s.test_lines));
                values.insert("test_pct".into(), MetricValue::Float(test_pct));
                MetricEntry { key: lang, values }
            })
            .collect();

        // Summary row at the end.
        let total_code = total_source_lines + total_test_lines;
        let overall_pct = if total_code > 0 {
            (total_test_lines as f64 / total_code as f64) * 100.0
        } else {
            0.0
        };
        let mut summary = HashMap::new();
        summary.insert(
            "source_files".into(),
            MetricValue::Count(total_source_files),
        );
        summary.insert("test_files".into(), MetricValue::Count(total_test_files));
        summary.insert(
            "source_lines".into(),
            MetricValue::Count(total_source_lines),
        );
        summary.insert("test_lines".into(), MetricValue::Count(total_test_lines));
        summary.insert("test_pct".into(), MetricValue::Float(overall_pct));
        entries.push(MetricEntry {
            key: "(total)".into(),
            values: summary,
        });

        MetricResult {
            name: "test_ratio".into(),
            display_name: report_display("test_ratio"),
            description: report_description("test_ratio"),
            entry_groups: vec![],
            columns: vec![
                Column::in_report("test_ratio", "source_files"),
                Column::in_report("test_ratio", "test_files"),
                Column::in_report("test_ratio", "source_lines"),
                Column::in_report("test_ratio", "test_lines"),
                Column::in_report("test_ratio", "test_pct"),
            ],
            entries,
        }
    }
}

fn walk_tree(
    repo: &gix::Repository,
    tree: &gix::Tree,
    prefix: &str,
    coll: &mut TestRatioCollector,
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
            if !is_source_file(&full_path) {
                continue;
            }
            let Ok(obj) = repo.find_object(id) else {
                continue;
            };
            let bytes = obj.data.len() as u64;
            if bytes == 0 || bytes > MAX_BLOB_BYTES {
                continue;
            }
            if obj.data[..obj.data.len().min(8192)].contains(&0) {
                continue;
            }
            let Ok(content) = std::str::from_utf8(&obj.data) else {
                continue;
            };
            let lang_info = match detect_language_info(&full_path, Some(content)) {
                Some(l) => l,
                None => continue,
            };
            let counts = count_lines(content, Some(lang_info));
            let is_test = is_test_file(&full_path);

            let stat = coll
                .stats
                .entry(lang_info.name.to_string())
                .or_insert(LangStats {
                    source_files: 0,
                    test_files: 0,
                    source_lines: 0,
                    test_lines: 0,
                });
            if is_test {
                stat.test_files += 1;
                stat.test_lines += counts.code;
            } else {
                stat.source_files += 1;
                stat.source_lines += counts.code;
            }
        }
    }
}

/// Returns `true` when a file path looks like a test file based on common
/// naming conventions across major language ecosystems.
fn is_test_file(path: &str) -> bool {
    let lower_path = path.to_ascii_lowercase();
    let name = lower_path.rsplit('/').next().unwrap_or(&lower_path);

    // Directory-based: tests/, test/, __tests__/, spec/
    if lower_path.starts_with("tests/")
        || lower_path.starts_with("test/")
        || lower_path.contains("/tests/")
        || lower_path.contains("/test/")
        || lower_path.contains("/__tests__/")
        || lower_path.starts_with("__tests__/")
        || lower_path.starts_with("spec/")
        || lower_path.contains("/spec/")
    {
        return true;
    }

    // Go: *_test.go
    if name.ends_with("_test.go") {
        return true;
    }
    // Python: test_*.py, *_test.py
    if (name.starts_with("test_") && name.ends_with(".py")) || name.ends_with("_test.py") {
        return true;
    }
    // JS/TS: *.test.js, *.test.ts, *.spec.js, *.spec.ts (and tsx/jsx variants)
    if name.contains(".test.") || name.contains(".spec.") {
        return true;
    }
    // Java/Kotlin (case-sensitive check on original name)
    let orig_name = path.rsplit('/').next().unwrap_or(path);
    if orig_name.ends_with("Test.java")
        || orig_name.ends_with("Tests.java")
        || orig_name.ends_with("Spec.java")
        || orig_name.ends_with("Test.kt")
        || orig_name.ends_with("Tests.kt")
    {
        return true;
    }
    // Ruby: *_test.rb, *_spec.rb
    if name.ends_with("_test.rb") || name.ends_with("_spec.rb") {
        return true;
    }
    // C#: *Test.cs, *Tests.cs
    if orig_name.ends_with("Test.cs") || orig_name.ends_with("Tests.cs") {
        return true;
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_file_detection_directories() {
        assert!(is_test_file("tests/unit/foo.rs"));
        assert!(is_test_file("test/main_test.go"));
        assert!(is_test_file("src/__tests__/App.test.tsx"));
        assert!(is_test_file("spec/models/user_spec.rb"));
    }

    #[test]
    fn test_file_detection_suffixes() {
        assert!(is_test_file("pkg/handler_test.go"));
        assert!(is_test_file("src/test_utils.py"));
        assert!(is_test_file("src/utils_test.py"));
        assert!(is_test_file("src/App.test.tsx"));
        assert!(is_test_file("src/App.spec.js"));
        assert!(is_test_file("com/example/UserTest.java"));
        assert!(is_test_file("app/UserTests.java"));
        assert!(is_test_file("lib/user_spec.rb"));
        assert!(is_test_file("Services/PaymentTest.cs"));
    }

    #[test]
    fn non_test_files_rejected() {
        assert!(!is_test_file("src/main.rs"));
        assert!(!is_test_file("src/utils.py"));
        assert!(!is_test_file("src/App.tsx"));
        assert!(!is_test_file("cmd/main.go"));
        assert!(!is_test_file("lib/testing.rb"));
    }

    #[test]
    fn empty_collector_produces_named_result() {
        let mut coll = TestRatioCollector::new();
        let result = coll.finalize();
        assert_eq!(result.name, "test_ratio");
        // Should have at least the summary row.
        assert_eq!(result.entries.len(), 1);
        assert_eq!(result.entries[0].key, "(total)");
    }
}
