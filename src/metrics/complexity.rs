use std::collections::HashMap;

use tree_sitter::{Language, Node, Parser};

use crate::analysis::line_classifier::{CommentState, LineType, classify_line};
use crate::langs::{LANGUAGES, Language as SourceLanguage};
use crate::metrics::MetricCollector;
use crate::types::{MetricEntry, MetricResult, MetricValue, ParsedChange};

/// Per-language node-kind tables for cyclomatic complexity computation.
struct LangSpec {
    name: &'static str,
    language: fn() -> Language,
    function_kinds: &'static [&'static str],
    decision_kinds: &'static [&'static str],
}

const RUST: LangSpec = LangSpec {
    name: "Rust",
    language: || tree_sitter_rust::LANGUAGE.into(),
    function_kinds: &["function_item", "closure_expression"],
    decision_kinds: &[
        "if_expression",
        "match_arm",
        "while_expression",
        "loop_expression",
        "for_expression",
        "try_expression",
    ],
};

const PYTHON: LangSpec = LangSpec {
    name: "Python",
    language: || tree_sitter_python::LANGUAGE.into(),
    function_kinds: &["function_definition", "lambda"],
    decision_kinds: &[
        "if_statement",
        "elif_clause",
        "while_statement",
        "for_statement",
        "except_clause",
        "case_clause",
        "conditional_expression",
    ],
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
    decision_kinds: &[
        "if_statement",
        "while_statement",
        "do_statement",
        "for_statement",
        "for_in_statement",
        "switch_case",
        "catch_clause",
        "ternary_expression",
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
    decision_kinds: &[
        "if_statement",
        "while_statement",
        "do_statement",
        "for_statement",
        "enhanced_for_statement",
        "switch_label",
        "catch_clause",
        "ternary_expression",
    ],
};

const GO: LangSpec = LangSpec {
    name: "Go",
    language: || tree_sitter_go::LANGUAGE.into(),
    function_kinds: &["function_declaration", "method_declaration", "func_literal"],
    decision_kinds: &[
        "if_statement",
        "for_statement",
        "expression_case",
        "default_case",
        "type_case",
        "communication_case",
    ],
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
    decision_kinds: &[
        "if_statement",
        "while_statement",
        "do_statement",
        "for_statement",
        "for_in_statement",
        "switch_case",
        "catch_clause",
        "ternary_expression",
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
    decision_kinds: &[
        "if_expression",
        "when_entry",
        "while_statement",
        "do_while_statement",
        "for_statement",
        "catch_block",
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
    decision_kinds: &[
        "if_statement",
        "while_statement",
        "do_statement",
        "for_statement",
        "switch_statement_case",
        "switch_expression_case",
        "catch_clause",
        "conditional_expression",
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

struct FunctionMetric {
    file: String,
    name: String,
    start_line: u32,
    line_count: u32,
    cyclomatic: u32,
    language: &'static str,
}

/// Top-N bound. Keeping just the highest-complexity functions in a min-heap
/// caps memory at O(N) regardless of how many files the repo has — for big
/// monorepos this used to reach hundreds of MB.
const MAX_METRICS: usize = 200;

pub struct ComplexityCollector {
    /// Min-heap keyed by cyclomatic so we can cheaply drop the weakest entry
    /// whenever a stronger one comes in.
    metrics: std::collections::BinaryHeap<std::cmp::Reverse<HeapEntry>>,
}

#[derive(PartialEq, Eq)]
struct HeapEntry {
    cyclomatic: u32,
    metric: FunctionMetric,
}

impl Ord for HeapEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.cyclomatic.cmp(&other.cyclomatic)
    }
}
impl PartialOrd for HeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for FunctionMetric {
    fn eq(&self, other: &Self) -> bool {
        self.file == other.file && self.name == other.name && self.start_line == other.start_line
    }
}
impl Eq for FunctionMetric {}

impl Default for ComplexityCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl ComplexityCollector {
    pub fn new() -> Self {
        Self {
            metrics: std::collections::BinaryHeap::with_capacity(MAX_METRICS + 1),
        }
    }

    /// Insert a metric while keeping the heap at `MAX_METRICS` entries.
    fn push_bounded(&mut self, metric: FunctionMetric) {
        use std::cmp::Reverse;
        let cc = metric.cyclomatic;
        if self.metrics.len() < MAX_METRICS {
            self.metrics.push(Reverse(HeapEntry {
                cyclomatic: cc,
                metric,
            }));
        } else if let Some(min) = self.metrics.peek()
            && cc > min.0.cyclomatic
        {
            self.metrics.pop();
            self.metrics.push(Reverse(HeapEntry {
                cyclomatic: cc,
                metric,
            }));
        }
    }
}

impl MetricCollector for ComplexityCollector {
    fn name(&self) -> &str {
        "complexity"
    }

    fn process(&mut self, _change: &ParsedChange) {
        // Operates on HEAD tree, not per-commit diffs.
    }

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
        let mut files_scanned = 0u64;
        walk_tree(
            repo,
            &tree,
            "",
            &mut parsers,
            self,
            &mut files_scanned,
            progress,
        );
        Ok(())
    }

    fn finalize(&mut self) -> MetricResult {
        // Drain the min-heap and sort descending by cyclomatic.
        let mut list: Vec<FunctionMetric> = self.metrics.drain().map(|r| r.0.metric).collect();
        list.sort_by_key(|m| std::cmp::Reverse(m.cyclomatic));

        let entries: Vec<MetricEntry> = list
            .into_iter()
            .map(|m| {
                let recommendation = classify(m.cyclomatic);
                let key = format!("{}::{}:{}", m.file, m.name, m.start_line);
                let mut values = HashMap::new();
                values.insert("cyclomatic".into(), MetricValue::Count(m.cyclomatic as u64));
                values.insert("lines".into(), MetricValue::Count(m.line_count as u64));
                values.insert("language".into(), MetricValue::Text(m.language.into()));
                values.insert(
                    "recommendation".into(),
                    MetricValue::Text(recommendation.into()),
                );
                MetricEntry { key, values }
            })
            .collect();

        MetricResult {
            name: "complexity".into(),
            display_name: "Cyclomatic Complexity".into(),
            description: "Cyclomatic complexity per function — roughly, how many independent execution paths it has. CC under 5 is simple, 6-10 is OK, 11-20 is hard to test and reason about, 21+ is hard to even read safely. Functions at the top of this list are the best candidates to split into smaller pieces. `lines` counts executable code only (blanks and comments excluded) so a docstring-heavy function isn't flagged for length alone.".into(),
            entry_groups: vec![],
            column_labels: vec![],
            columns: vec![
                "cyclomatic".into(),
                "lines".into(),
                "language".into(),
                "recommendation".into(),
            ],
            entries,
        }
    }
}

fn classify(cc: u32) -> &'static str {
    match cc {
        0..=5 => "Simple",
        6..=10 => "OK",
        11..=20 => "High — refactor candidate",
        _ => "Very high — split urgently",
    }
}

fn walk_tree(
    repo: &gix::Repository,
    tree: &gix::Tree,
    prefix: &str,
    parsers: &mut HashMap<&'static str, Parser>,
    collector: &mut ComplexityCollector,
    files_scanned: &mut u64,
    progress: &crate::metrics::ProgressReporter,
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
                walk_tree(
                    repo,
                    &subtree,
                    &full_path,
                    parsers,
                    collector,
                    files_scanned,
                    progress,
                );
            }
        } else if mode.is_blob() {
            let Some(spec) = spec_for_path(&full_path) else {
                continue;
            };
            let Ok(object) = repo.find_object(id) else {
                continue;
            };
            let Ok(blob) = object.try_into_blob() else {
                continue;
            };
            let Ok(source) = std::str::from_utf8(&blob.data) else {
                continue;
            };
            // Parse into a per-file local Vec, then drain into the bounded
            // heap so per-file allocation drops before we move on.
            let mut local: Vec<FunctionMetric> = Vec::new();
            analyze_source(spec, &full_path, source, parsers, &mut local);
            for m in local.drain(..) {
                collector.push_bounded(m);
            }
            *files_scanned += 1;
            if (*files_scanned).is_multiple_of(200) {
                progress.status(&format!("  complexity: {} files parsed...", *files_scanned));
            }
        }
    }
}

fn analyze_source(
    spec: &'static LangSpec,
    file_path: &str,
    source: &str,
    parsers: &mut HashMap<&'static str, Parser>,
    out: &mut Vec<FunctionMetric>,
) {
    let parser = parsers.entry(spec.name).or_insert_with(|| {
        let mut p = Parser::new();
        let _ = p.set_language(&(spec.language)());
        p
    });
    let Some(tree) = parser.parse(source, None) else {
        return;
    };
    let root = tree.root_node();
    let line_types = classify_file_lines(source, spec.name);
    visit(&root, spec, file_path, source, &line_types, out);
}

/// Pre-classify every line of the file so each function metric can count
/// *code* lines in its span (blanks and comment lines excluded). A single
/// pass is needed for the whole file because block-comment state spans lines.
fn classify_file_lines(source: &str, lang_name: &'static str) -> Vec<LineType> {
    let lang = codestats_lang(lang_name);
    let mut state = CommentState::new();
    source
        .lines()
        .enumerate()
        .map(|(idx, line)| classify_line(line, lang, &mut state, idx == 0))
        .collect()
}

fn codestats_lang(name: &str) -> Option<&'static SourceLanguage> {
    LANGUAGES.iter().find(|l| l.name == name)
}

/// Count code-only lines in the 1-based inclusive range `[start, end]`.
fn count_code_lines(line_types: &[LineType], start: u32, end: u32) -> u32 {
    let s = start.saturating_sub(1) as usize;
    let e = (end as usize).min(line_types.len());
    if s >= e {
        return 0;
    }
    line_types[s..e]
        .iter()
        .filter(|t| matches!(t, LineType::Code))
        .count() as u32
}

/// Recursively find function-like nodes and compute their cyclomatic complexity.
fn visit(
    node: &Node,
    spec: &'static LangSpec,
    file_path: &str,
    source: &str,
    line_types: &[LineType],
    out: &mut Vec<FunctionMetric>,
) {
    let kind = node.kind();
    if spec.function_kinds.contains(&kind) {
        let name = function_name(node, source).unwrap_or_else(|| "<anonymous>".into());
        let cyclomatic = 1 + count_decisions(node, spec.decision_kinds, spec.function_kinds);
        let start = node.start_position().row as u32 + 1;
        let end = node.end_position().row as u32 + 1;
        out.push(FunctionMetric {
            file: file_path.to_string(),
            name,
            start_line: start,
            line_count: count_code_lines(line_types, start, end),
            cyclomatic,
            language: spec.name,
        });
        // Continue descending so nested functions/closures are also captured.
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        visit(&child, spec, file_path, source, line_types, out);
    }
}

/// Count decision-point nodes inside `node`, skipping subtrees rooted at nested
/// function-like nodes (those get their own complexity entries).
fn count_decisions(node: &Node, kinds: &[&str], func_kinds: &[&str]) -> u32 {
    let mut count = 0u32;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if func_kinds.contains(&child.kind()) {
            continue;
        }
        if kinds.contains(&child.kind()) {
            count += 1;
        }
        count += count_decisions(&child, kinds, func_kinds);
    }
    count
}

/// Best-effort function name extraction via the `name` field, if present.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn analyze(spec: &'static LangSpec, source: &str) -> Vec<FunctionMetric> {
        let mut parsers = HashMap::new();
        let mut out = vec![];
        analyze_source(spec, "test", source, &mut parsers, &mut out);
        out
    }

    #[test]
    fn rust_simple_function_cc_1() {
        let src = "fn foo() {}";
        let m = analyze(&RUST, src);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].cyclomatic, 1);
        assert_eq!(m[0].name, "foo");
    }

    #[test]
    fn rust_if_else_cc_2() {
        let src = "fn foo(x: i32) -> i32 { if x > 0 { 1 } else { 2 } }";
        let m = analyze(&RUST, src);
        assert_eq!(m.iter().find(|x| x.name == "foo").unwrap().cyclomatic, 2);
    }

    #[test]
    fn rust_match_arms_count() {
        let src = "fn foo(x: i32) -> i32 { match x { 0 => 1, 1 => 2, _ => 3 } }";
        let m = analyze(&RUST, src);
        // 1 base + 3 match arms = 4
        assert_eq!(m.iter().find(|x| x.name == "foo").unwrap().cyclomatic, 4);
    }

    #[test]
    fn python_if_elif_cc() {
        let src = "def foo(x):\n    if x > 0:\n        return 1\n    elif x < 0:\n        return -1\n    return 0\n";
        let m = analyze(&PYTHON, src);
        // 1 base + 1 if + 1 elif = 3
        assert_eq!(m.iter().find(|x| x.name == "foo").unwrap().cyclomatic, 3);
    }

    #[test]
    fn typescript_for_and_if() {
        let src =
            "function foo(xs: number[]) { for (const x of xs) { if (x > 0) console.log(x); } }";
        let m = analyze(&TYPESCRIPT, src);
        // 1 + for_in_statement (for-of) + if = 3
        let cc = m.iter().find(|x| x.name == "foo").unwrap().cyclomatic;
        assert!(cc >= 2, "expected >= 2, got {cc}");
    }

    #[test]
    fn classify_thresholds() {
        assert_eq!(classify(1), "Simple");
        assert_eq!(classify(7), "OK");
        assert_eq!(classify(15), "High — refactor candidate");
        assert_eq!(classify(50), "Very high — split urgently");
    }

    #[test]
    fn unsupported_extension_returns_none() {
        assert!(spec_for_path("foo.scala").is_none());
        assert!(spec_for_path("foo.rs").is_some());
    }

    #[test]
    fn java_method_with_branches() {
        let src = "class C { int f(int x) { if (x > 0) return 1; for (int i = 0; i < x; i++) {} return 0; } }";
        let m = analyze(&JAVA, src);
        let cc = m.iter().find(|x| x.name == "f").unwrap().cyclomatic;
        // 1 base + if + for = 3
        assert!(cc >= 3, "expected >= 3, got {cc}");
    }

    #[test]
    fn go_switch_cases_count() {
        let src = "package p\nfunc f(x int) int {\n  switch x {\n  case 0: return 1\n  case 1: return 2\n  default: return 3\n  }\n}\n";
        let m = analyze(&GO, src);
        let cc = m.iter().find(|x| x.name == "f").unwrap().cyclomatic;
        // 1 base + 2 expression_case + 1 default_case = 4
        assert!(cc >= 3, "expected >= 3, got {cc}");
    }

    #[test]
    fn nested_function_captured_separately() {
        let src = "fn outer() { let inner = || { if true { 1 } else { 2 } }; }";
        let m = analyze(&RUST, src);
        // outer (CC=1) + closure (CC=2)
        let outer = m.iter().find(|x| x.name == "outer").unwrap();
        assert_eq!(outer.cyclomatic, 1);
        let closure = m.iter().find(|x| x.name == "<anonymous>").unwrap();
        assert_eq!(closure.cyclomatic, 2);
    }

    #[test]
    fn unsupported_source_yields_no_metrics() {
        // Garbage source for an unsupported file path: spec_for_path returns None
        assert!(spec_for_path("foo.cobol").is_none());
    }
}
