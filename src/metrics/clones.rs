//! AST-based clone detection across the repo.
//!
//! Walks every source file, parses it with tree-sitter, extracts every
//! function-like node, and computes a **normalized AST hash** — a 64-bit
//! hash over node kinds with identifiers / literals collapsed to a single
//! placeholder. Functions with the same hash have the same *shape*
//! regardless of variable names or literal values, which is the classic
//! definition of a Type-2 clone.
//!
//! Memory profile: one fixed-size record per function (hash + file + name
//! + line + size). At ~100 bytes per record, a 10k-function repo uses
//! ~1 MB. Works out of RAM; no SQLite-backed storage needed at this scale.
//!
//! False positives are possible for very short functions that accidentally
//! share shape (getters, one-liners). `MIN_CLONE_LINES` filters those out.
//! False negatives happen on Type-3 clones (modified copies) and on
//! macro-heavy code where tree-sitter collapses the macro.

use std::collections::{HashMap, hash_map::DefaultHasher};
use std::hash::{Hash, Hasher};

use tree_sitter::{Language, Node, Parser};

use crate::analysis::source_filter::is_source_file;
use crate::metrics::MetricCollector;
use crate::types::{
    Column, MetricEntry, MetricResult, MetricValue, ParsedChange, report_description,
    report_display,
};

/// Minimum lines in a function for its hash to count toward clone detection.
/// Short functions (getters, one-liners) share shape by coincidence all the
/// time, so reporting them clutters the output.
const MIN_CLONE_LINES: u32 = 10;

/// Cap on clone groups surfaced. Groups are ranked by
/// `size_lines * (occurrences - 1)` descending — the "time you'd save by
/// extracting" heuristic.
const MAX_GROUPS: usize = 100;

/// Node kinds that represent identifiers / literals — collapsed to a fixed
/// placeholder so two clones that differ only in variable names or literal
/// values still share a hash. Covers all eight supported languages; tree-
/// sitter kind names are fairly consistent for these.
const NORMALIZED_KINDS: &[&str] = &[
    "identifier",
    "property_identifier",
    "field_identifier",
    "shorthand_property_identifier",
    "type_identifier",
    "scoped_identifier",
    "integer_literal",
    "integer",
    "string_literal",
    "string",
    "interpolated_string_literal",
    "float_literal",
    "float",
    "number",
    "boolean_literal",
    "true",
    "false",
    "char_literal",
    "character_literal",
    "none",
    "null",
    "nil",
];

/// Comment-like kinds we don't want contributing to structural hashes.
const COMMENT_KINDS: &[&str] = &[
    "comment",
    "line_comment",
    "block_comment",
    "documentation_comment",
    "doc_comment",
];

struct LangSpec {
    name: &'static str,
    language: fn() -> Language,
    function_kinds: &'static [&'static str],
}

const RUST: LangSpec = LangSpec {
    name: "Rust",
    language: || tree_sitter_rust::LANGUAGE.into(),
    function_kinds: &["function_item", "closure_expression"],
};

const PYTHON: LangSpec = LangSpec {
    name: "Python",
    language: || tree_sitter_python::LANGUAGE.into(),
    function_kinds: &["function_definition", "lambda"],
};

const TYPESCRIPT: LangSpec = LangSpec {
    name: "TypeScript",
    language: || tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
    function_kinds: &[
        "function_declaration",
        "method_definition",
        "arrow_function",
        "function_expression",
        "generator_function_declaration",
    ],
};

const JAVA: LangSpec = LangSpec {
    name: "Java",
    language: || tree_sitter_java::LANGUAGE.into(),
    function_kinds: &[
        "method_declaration",
        "constructor_declaration",
        "lambda_expression",
    ],
};

const GO: LangSpec = LangSpec {
    name: "Go",
    language: || tree_sitter_go::LANGUAGE.into(),
    function_kinds: &["function_declaration", "method_declaration", "func_literal"],
};

const JAVASCRIPT: LangSpec = LangSpec {
    name: "JavaScript",
    language: || tree_sitter_javascript::LANGUAGE.into(),
    function_kinds: &[
        "function_declaration",
        "generator_function_declaration",
        "method_definition",
        "arrow_function",
        "function_expression",
    ],
};

const KOTLIN: LangSpec = LangSpec {
    name: "Kotlin",
    language: || tree_sitter_kotlin_ng::LANGUAGE.into(),
    function_kinds: &[
        "function_declaration",
        "anonymous_function",
        "lambda_literal",
    ],
};

const DART: LangSpec = LangSpec {
    name: "Dart",
    language: || tree_sitter_dart::LANGUAGE.into(),
    function_kinds: &[
        "function_signature",
        "method_signature",
        "function_expression",
        "lambda_expression",
    ],
};

const SUPPORTED: &[LangSpec] = &[RUST, PYTHON, TYPESCRIPT, JAVA, GO, JAVASCRIPT, KOTLIN, DART];

fn spec_for_path(path: &str) -> Option<&'static LangSpec> {
    let ext = path.rsplit('.').next()?;
    let name = match ext {
        "rs" => "Rust",
        "py" | "pyi" => "Python",
        "ts" | "tsx" => "TypeScript",
        "js" | "jsx" | "mjs" | "cjs" => "JavaScript",
        "java" => "Java",
        "go" => "Go",
        "kt" | "kts" => "Kotlin",
        "dart" => "Dart",
        _ => return None,
    };
    SUPPORTED.iter().find(|s| s.name == name)
}

#[derive(Clone)]
struct FunctionRecord {
    hash: u64,
    file: String,
    name: String,
    start_line: u32,
    size_lines: u32,
}

pub struct ClonesCollector {
    records: Vec<FunctionRecord>,
}

impl Default for ClonesCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl ClonesCollector {
    pub fn new() -> Self {
        Self {
            records: Vec::new(),
        }
    }
}

impl MetricCollector for ClonesCollector {
    fn name(&self) -> &str {
        "clones"
    }

    fn process(&mut self, _change: &ParsedChange) {}

    fn inspect_repo(
        &mut self,
        repo: &gix::Repository,
        progress: &crate::metrics::ProgressReporter,
    ) -> anyhow::Result<()> {
        let head_commit = match repo.head_commit() {
            Ok(c) => c,
            Err(_) => return Ok(()),
        };
        let tree = head_commit.tree()?;
        let mut parsers: HashMap<&'static str, Parser> = HashMap::new();
        let mut scanned = 0u64;
        walk_tree(
            repo,
            &tree,
            "",
            &mut parsers,
            &mut self.records,
            &mut scanned,
            progress,
        );
        Ok(())
    }

    fn finalize(&mut self) -> MetricResult {
        let records = std::mem::take(&mut self.records);
        let mut by_hash: HashMap<u64, Vec<FunctionRecord>> = HashMap::new();
        for r in records {
            by_hash.entry(r.hash).or_default().push(r);
        }

        let mut groups: Vec<Vec<FunctionRecord>> = by_hash
            .into_values()
            .filter(|g| g.len() >= 2 && g.first().map(|r| r.size_lines >= MIN_CLONE_LINES).unwrap_or(false))
            .collect();

        // Rank by "refactor payoff": size_lines × (occurrences − 1). Extracting
        // a 50-line function cloned 5 times saves 200 lines; 10 lines cloned 2
        // times saves 10. The bigger opportunity sorts first.
        groups.sort_by_key(|g| {
            let size = g.first().map(|r| r.size_lines).unwrap_or(0);
            let payoff = size * (g.len() as u32 - 1);
            std::cmp::Reverse(payoff)
        });
        groups.truncate(MAX_GROUPS);

        let entries: Vec<MetricEntry> = groups
            .into_iter()
            .enumerate()
            .map(|(idx, mut group)| {
                // Stable order inside the group: alphabetical by file, then
                // by line number. First entry becomes the representative.
                group.sort_by(|a, b| a.file.cmp(&b.file).then(a.start_line.cmp(&b.start_line)));
                let first = &group[0];
                let others: Vec<String> = group
                    .iter()
                    .skip(1)
                    .map(|r| format!("{}::{}:{}", r.file, r.name, r.start_line))
                    .collect();
                let key = format!("#{}: {}::{}:{}", idx + 1, first.file, first.name, first.start_line);
                let mut values = HashMap::new();
                values.insert(
                    "occurrences".into(),
                    MetricValue::Count(group.len() as u64),
                );
                values.insert(
                    "size_lines".into(),
                    MetricValue::Count(first.size_lines as u64),
                );
                values.insert(
                    "other_locations".into(),
                    MetricValue::Text(if others.is_empty() {
                        "(none)".into()
                    } else {
                        others.join(" | ")
                    }),
                );
                MetricEntry { key, values }
            })
            .collect();

        MetricResult {
            name: "clones".into(),
            display_name: report_display("clones"),
            description: report_description("clones"),
            entry_groups: vec![],
            columns: vec![
                Column::in_report("clones", "occurrences"),
                Column::in_report("clones", "size_lines"),
                Column::in_report("clones", "other_locations"),
            ],
            entries,
        }
    }
}

fn walk_tree(
    repo: &gix::Repository,
    tree: &gix::Tree,
    prefix: &str,
    parsers: &mut HashMap<&'static str, Parser>,
    records: &mut Vec<FunctionRecord>,
    scanned: &mut u64,
    progress: &crate::metrics::ProgressReporter,
) {
    for entry_res in tree.iter() {
        let Ok(entry) = entry_res else { continue };
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
                walk_tree(repo, &subtree, &full_path, parsers, records, scanned, progress);
            }
        } else if mode.is_blob() {
            let Some(spec) = spec_for_path(&full_path) else {
                continue;
            };
            if !is_source_file(&full_path) {
                continue;
            }
            let Ok(object) = repo.find_object(id) else {
                continue;
            };
            let Ok(blob) = object.try_into_blob() else {
                continue;
            };
            let Ok(source) = std::str::from_utf8(&blob.data) else {
                continue;
            };
            analyze_file(spec, &full_path, source, parsers, records);
            *scanned += 1;
            if scanned.is_multiple_of(200) {
                progress.status(&format!("  clones: {} files parsed...", *scanned));
            }
        }
    }
}

fn analyze_file(
    spec: &'static LangSpec,
    file_path: &str,
    source: &str,
    parsers: &mut HashMap<&'static str, Parser>,
    out: &mut Vec<FunctionRecord>,
) {
    let parser = parsers.entry(spec.name).or_insert_with(|| {
        let mut p = Parser::new();
        let _ = p.set_language(&(spec.language)());
        p
    });
    let Some(tree) = parser.parse(source, None) else {
        return;
    };
    visit(&tree.root_node(), spec, file_path, source, out);
}

fn visit(
    node: &Node,
    spec: &LangSpec,
    file_path: &str,
    source: &str,
    out: &mut Vec<FunctionRecord>,
) {
    if spec.function_kinds.contains(&node.kind()) {
        let name = function_name(node, source).unwrap_or_else(|| "<anonymous>".into());
        let start = node.start_position().row as u32 + 1;
        let end = node.end_position().row as u32 + 1;
        let size_lines = end.saturating_sub(start).saturating_add(1);
        if size_lines >= MIN_CLONE_LINES {
            let hash = ast_hash(node, spec.function_kinds);
            out.push(FunctionRecord {
                hash,
                file: file_path.to_string(),
                name,
                start_line: start,
                size_lines,
            });
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        visit(&child, spec, file_path, source, out);
    }
}

fn function_name(node: &Node, source: &str) -> Option<String> {
    let name_node = node.child_by_field_name("name")?;
    let bytes = source.as_bytes();
    let start = name_node.start_byte();
    let end = name_node.end_byte();
    if start <= end && end <= bytes.len() {
        Some(String::from_utf8_lossy(&bytes[start..end]).into_owned())
    } else {
        None
    }
}

/// Compute a normalized structural hash for a function subtree. Identifier /
/// literal nodes collapse to a single token so renames and literal changes do
/// not break the hash. Comments are skipped. Nested functions are skipped so
/// their body doesn't contaminate the outer hash.
fn ast_hash(node: &Node, func_kinds: &[&str]) -> u64 {
    let mut hasher = DefaultHasher::new();
    hash_walk(node, func_kinds, true, &mut hasher);
    hasher.finish()
}

fn hash_walk(node: &Node, func_kinds: &[&str], is_root: bool, hasher: &mut DefaultHasher) {
    let kind = node.kind();
    // Skip nested functions (they get their own record).
    if !is_root && func_kinds.contains(&kind) {
        "<nested_fn>".hash(hasher);
        return;
    }
    if COMMENT_KINDS.contains(&kind) {
        return;
    }
    if NORMALIZED_KINDS.contains(&kind) {
        "_".hash(hasher);
        return;
    }
    kind.hash(hasher);
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        hash_walk(&child, func_kinds, false, hasher);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash_of(spec: &'static LangSpec, src: &str) -> u64 {
        let mut parser = Parser::new();
        let _ = parser.set_language(&(spec.language)());
        let tree = parser.parse(src, None).unwrap();
        let root = tree.root_node();
        // Find the first function-like node.
        fn find<'a>(node: &Node<'a>, spec: &LangSpec) -> Option<Node<'a>> {
            if spec.function_kinds.contains(&node.kind()) {
                return Some(*node);
            }
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if let Some(found) = find(&child, spec) {
                    return Some(found);
                }
            }
            None
        }
        let func = find(&root, spec).expect("function");
        ast_hash(&func, spec.function_kinds)
    }

    #[test]
    fn clones_differ_only_in_identifier_names_share_hash() {
        let a =
            "fn sum(xs: &[i32]) -> i32 { let mut total = 0; for x in xs { total += *x; } total }";
        let b =
            "fn fold(values: &[i32]) -> i32 { let mut acc = 0; for v in values { acc += *v; } acc }";
        let ha = hash_of(&RUST, a);
        let hb = hash_of(&RUST, b);
        assert_eq!(ha, hb, "renamed variables must not change the hash");
    }

    #[test]
    fn structurally_different_functions_get_different_hashes() {
        let a = "fn f(x: i32) -> i32 { x + 1 }";
        let b = "fn f(x: i32) -> i32 { if x > 0 { 1 } else { 2 } }";
        assert_ne!(hash_of(&RUST, a), hash_of(&RUST, b));
    }

    #[test]
    fn different_literals_still_same_hash() {
        let a = "fn f() -> i32 { 42 }";
        let b = "fn f() -> i32 { 9001 }";
        assert_eq!(hash_of(&RUST, a), hash_of(&RUST, b));
    }

    #[test]
    fn python_clone_detected() {
        let a = "def add(a, b):\n    total = a + b\n    return total\n";
        let b = "def sum_two(x, y):\n    result = x + y\n    return result\n";
        assert_eq!(hash_of(&PYTHON, a), hash_of(&PYTHON, b));
    }

    #[test]
    fn finalize_groups_duplicates_and_ranks_by_payoff() {
        // Seed records manually to avoid needing a real repo. Two groups:
        //   group A: 30-line function cloned twice → payoff 30
        //   group B: 50-line function cloned twice → payoff 50 (ranks higher)
        let mut coll = ClonesCollector::new();
        for i in 0..2 {
            coll.records.push(FunctionRecord {
                hash: 111,
                file: format!("a/f{i}.rs"),
                name: format!("small{i}"),
                start_line: 1,
                size_lines: 30,
            });
            coll.records.push(FunctionRecord {
                hash: 222,
                file: format!("b/f{i}.rs"),
                name: format!("big{i}"),
                start_line: 1,
                size_lines: 50,
            });
        }
        let result = coll.finalize();
        assert_eq!(result.entries.len(), 2);
        // Bigger payoff (50-line group) must rank first.
        let first_key = &result.entries[0].key;
        assert!(
            first_key.contains("b/f0.rs"),
            "expected 50-line group first, got {first_key}"
        );
    }

    #[test]
    fn singleton_hashes_dropped() {
        let mut coll = ClonesCollector::new();
        coll.records.push(FunctionRecord {
            hash: 42,
            file: "only.rs".into(),
            name: "loner".into(),
            start_line: 1,
            size_lines: 50,
        });
        let result = coll.finalize();
        assert!(
            result.entries.is_empty(),
            "singleton function must not produce a clone group"
        );
    }
}
