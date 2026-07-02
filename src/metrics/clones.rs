//! AST-based clone detection across the repo.
//!
//! Walks every source file, parses it with tree-sitter, extracts every
//! function-like node, and computes a **normalized AST hash** — a 64-bit
//! hash over node kinds with identifiers / literals collapsed to a single
//! placeholder. Functions with the same hash have the same *shape*
//! regardless of variable names or literal values, which is the classic
//! definition of a Type-2 clone.
//!
//! Memory profile: one fixed-size record per function (hash + file + name +
//! line + size). At ~100 bytes per record, a 10k-function repo uses ~1 MB.
//! Works out of RAM; no SQLite-backed storage needed at this scale.
//!
//! False positives are possible for very short functions that accidentally
//! share shape (getters, one-liners). `MIN_CLONE_LINES` filters those out.
//! False negatives happen on Type-3 clones (modified copies) and on
//! macro-heavy code where tree-sitter collapses the macro.

use std::collections::{HashMap, HashSet, hash_map::DefaultHasher};
use std::hash::{Hash, Hasher};

use tree_sitter::{Language, Node};

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

/// Per-language set of tree-sitter `kind_id`s (u16) for the kind categories we
/// test membership against on every AST node. Precomputing these once per
/// language turns the hot-path checks in `visit` / `hash_walk` from ~32 linear
/// `&str` comparisons per node into a handful of `u16` hash-set lookups.
///
/// Built by scanning every visible node kind in the `Language` and keeping the
/// ids whose kind name is in the corresponding string list. This makes
/// `set.contains(&node.kind_id())` exactly equivalent to the previous
/// `LIST.contains(&node.kind())`, so clone hashes and results are unchanged.
struct KindIds {
    function_kinds: HashSet<u16>,
    comment_kinds: HashSet<u16>,
    normalized_kinds: HashSet<u16>,
}

impl KindIds {
    fn new(language: &Language, spec: &LangSpec) -> Self {
        let ids_for = |targets: &[&str]| -> HashSet<u16> {
            let mut set = HashSet::new();
            for id in 0..language.node_kind_count() as u16 {
                if let Some(kind) = language.node_kind_for_id(id)
                    && targets.contains(&kind)
                {
                    set.insert(id);
                }
            }
            set
        };
        Self {
            function_kinds: ids_for(spec.function_kinds),
            comment_kinds: ids_for(COMMENT_KINDS),
            normalized_kinds: ids_for(NORMALIZED_KINDS),
        }
    }
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
        "function_declaration",
        "method_declaration",
        "local_function_declaration",
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
    /// Per-language `kind_id` sets, cached across files during the shared scan.
    kinds: HashMap<&'static str, KindIds>,
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
            kinds: HashMap::new(),
        }
    }
}

impl MetricCollector for ClonesCollector {
    fn name(&self) -> &str {
        "clones"
    }

    fn process(&mut self, _change: &ParsedChange) {}

    // Driven by the pipeline's shared source scan; `inspect_repo` is a
    // deliberate no-op. (finding #23)
    fn as_source_scanner(&mut self) -> Option<&mut dyn crate::metrics::SourceScanner> {
        Some(self)
    }

    fn finalize(&mut self) -> MetricResult {
        let records = std::mem::take(&mut self.records);
        let mut by_hash: HashMap<u64, Vec<FunctionRecord>> = HashMap::new();
        for r in records {
            by_hash.entry(r.hash).or_default().push(r);
        }

        let mut groups: Vec<Vec<FunctionRecord>> = by_hash
            .into_values()
            .filter(|g| {
                g.len() >= 2
                    && g.first()
                        .map(|r| r.size_lines >= MIN_CLONE_LINES)
                        .unwrap_or(false)
            })
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
                let key = format!(
                    "#{}: {}::{}:{}",
                    idx + 1,
                    first.file,
                    first.name,
                    first.start_line
                );
                let mut values = HashMap::new();
                values.insert("occurrences".into(), MetricValue::Count(group.len() as u64));
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

impl crate::metrics::SourceScanner for ClonesCollector {
    fn scan_file(&mut self, path: &str, source: &str, tree: &tree_sitter::Tree) {
        let Some(spec) = spec_for_path(path) else {
            return;
        };
        let kinds = self
            .kinds
            .entry(spec.name)
            .or_insert_with(|| KindIds::new(&(spec.language)(), spec));
        visit(&tree.root_node(), kinds, path, source, &mut self.records);
    }
}

fn visit(
    node: &Node,
    kinds: &KindIds,
    file_path: &str,
    source: &str,
    out: &mut Vec<FunctionRecord>,
) {
    if kinds.function_kinds.contains(&node.kind_id()) {
        let name = function_name(node, source).unwrap_or_else(|| "<anonymous>".into());
        let start = node.start_position().row as u32 + 1;
        let end = node.end_position().row as u32 + 1;
        let size_lines = end.saturating_sub(start).saturating_add(1);
        if size_lines >= MIN_CLONE_LINES {
            let hash = ast_hash(node, kinds);
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
        visit(&child, kinds, file_path, source, out);
    }
}

fn function_name(node: &Node, source: &str) -> Option<String> {
    // Most grammars expose `name` directly on the function node. Dart's
    // function_declaration / method_declaration instead wrap a
    // function_signature / method_signature child that carries the name, so
    // fall back to that child's name field. (finding #19)
    let name_node = node.child_by_field_name("name").or_else(|| {
        let mut cursor = node.walk();
        node.children(&mut cursor)
            .find(|c| c.kind().ends_with("_signature"))
            .and_then(|sig| sig.child_by_field_name("name"))
    })?;
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
fn ast_hash(node: &Node, kinds: &KindIds) -> u64 {
    let mut hasher = DefaultHasher::new();
    hash_walk(node, kinds, true, &mut hasher);
    hasher.finish()
}

fn hash_walk(node: &Node, kinds: &KindIds, is_root: bool, hasher: &mut DefaultHasher) {
    let kind_id = node.kind_id();
    // Skip nested functions (they get their own record).
    if !is_root && kinds.function_kinds.contains(&kind_id) {
        "<nested_fn>".hash(hasher);
        return;
    }
    if kinds.comment_kinds.contains(&kind_id) {
        return;
    }
    if kinds.normalized_kinds.contains(&kind_id) {
        "_".hash(hasher);
        return;
    }
    // Hash the kind *name* (not the id) so the hash stays stable across
    // grammar-version symbol-numbering changes and identical across languages.
    node.kind().hash(hasher);
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        hash_walk(&child, kinds, false, hasher);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    fn hash_of(spec: &'static LangSpec, src: &str) -> u64 {
        let mut parser = Parser::new();
        let language: Language = (spec.language)();
        let _ = parser.set_language(&language);
        let kinds = KindIds::new(&language, spec);
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
        ast_hash(&func, &kinds)
    }

    #[test]
    fn clones_differ_only_in_identifier_names_share_hash() {
        let a =
            "fn sum(xs: &[i32]) -> i32 { let mut total = 0; for x in xs { total += *x; } total }";
        let b = "fn fold(values: &[i32]) -> i32 { let mut acc = 0; for v in values { acc += *v; } acc }";
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
    fn dart_named_function_clone_detected() {
        // Regression: Dart named functions must be recognized via the
        // `function_declaration` node (which spans the body), not the
        // ~1-line `function_signature`. Two functions differing only in
        // identifier names must share a hash, with real names attached.
        let a = "int add(int a, int b) {\n  var total = a + b;\n  return total;\n}\n";
        let b = "int sumTwo(int x, int y) {\n  var result = x + y;\n  return result;\n}\n";
        assert_eq!(hash_of(&DART, a), hash_of(&DART, b));

        // The declaration node carries a real function name (not <anonymous>).
        let mut parser = Parser::new();
        let _ = parser.set_language(&(DART.language)());
        let tree = parser.parse(a, None).unwrap();
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
        let func = find(&tree.root_node(), &DART).expect("dart function");
        assert_eq!(function_name(&func, a).as_deref(), Some("add"));
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
    fn kind_id_sets_match_string_membership() {
        // The perf fix replaces `LIST.contains(&node.kind())` with
        // `set.contains(&node.kind_id())`. For results to stay byte-identical
        // the id-set membership must agree with the string-list membership for
        // every visible node kind in every supported language.
        for spec in SUPPORTED {
            let language: Language = (spec.language)();
            let kinds = KindIds::new(&language, spec);
            for id in 0..language.node_kind_count() as u16 {
                let Some(name) = language.node_kind_for_id(id) else {
                    continue;
                };
                assert_eq!(
                    kinds.function_kinds.contains(&id),
                    spec.function_kinds.contains(&name),
                    "{}: function_kinds mismatch for id {id} ({name})",
                    spec.name
                );
                assert_eq!(
                    kinds.comment_kinds.contains(&id),
                    COMMENT_KINDS.contains(&name),
                    "{}: comment_kinds mismatch for id {id} ({name})",
                    spec.name
                );
                assert_eq!(
                    kinds.normalized_kinds.contains(&id),
                    NORMALIZED_KINDS.contains(&name),
                    "{}: normalized_kinds mismatch for id {id} ({name})",
                    spec.name
                );
            }
        }
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
