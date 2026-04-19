use std::collections::HashMap;

use tree_sitter::{Language, Node, Parser};

use crate::analysis::line_classifier::{CommentState, LineType, classify_line};
use crate::langs::{LANGUAGES, Language as SourceLanguage};
use crate::messages;
use crate::metrics::MetricCollector;
use crate::types::{
    Column, LocalizedMessage, MetricEntry, MetricResult, MetricValue, ParsedChange, Severity,
    report_description, report_display,
};

/// Per-language node-kind tables for cyclomatic, cognitive, and function-shape
/// metric computation.
struct LangSpec {
    name: &'static str,
    language: fn() -> Language,
    function_kinds: &'static [&'static str],
    /// Nodes that contribute to cyclomatic and cognitive complexity. Cognitive
    /// also uses these to grow nesting depth when descended into.
    decision_kinds: &'static [&'static str],
    /// Nodes that count as function exit points for NEXITS. Typically
    /// `return_statement` / `return_expression`; Kotlin uses `jump_expression`
    /// as an umbrella for return/break/continue.
    exit_kinds: &'static [&'static str],
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
    exit_kinds: &["return_expression"],
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
    exit_kinds: &["return_statement"],
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
    exit_kinds: &["return_statement"],
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
    exit_kinds: &["return_statement"],
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
    exit_kinds: &["return_statement"],
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
    exit_kinds: &["return_statement"],
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
    // tree-sitter-kotlin-ng groups return/break/continue under `jump_expression`.
    exit_kinds: &["jump_expression"],
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
    exit_kinds: &["return_statement"],
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
    /// Cognitive complexity (SonarSource spec, simplified). Weights each
    /// decision by `1 + nesting_depth` instead of the flat `+1` that
    /// cyclomatic uses. Better proxy for "hard to read" vs "many paths".
    cognitive: u32,
    /// Number of declared parameters. Derived from the function's
    /// `parameters` / `parameter_list` / `formal_parameters` child.
    nargs: u32,
    /// Number of explicit exit points (return / equivalent). Implicit end-of-
    /// function fall-through is not counted.
    nexits: u32,
    /// Maintainability Index (Microsoft variant, 0-100 scaled and clamped).
    /// Composite of Halstead volume, cyclomatic complexity, and SLOC. Higher
    /// is better; <65 often flagged as hard to maintain.
    mi: u32,
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
                values.insert("cognitive".into(), MetricValue::Count(m.cognitive as u64));
                values.insert("mi".into(), MetricValue::Count(m.mi as u64));
                values.insert("lines".into(), MetricValue::Count(m.line_count as u64));
                values.insert("args".into(), MetricValue::Count(m.nargs as u64));
                values.insert("exits".into(), MetricValue::Count(m.nexits as u64));
                values.insert("language".into(), MetricValue::Text(m.language.into()));
                values.insert(
                    "recommendation".into(),
                    MetricValue::Message(recommendation),
                );
                MetricEntry { key, values }
            })
            .collect();

        MetricResult {
            name: "complexity".into(),
            display_name: report_display("complexity"),
            description: report_description("complexity"),
            entry_groups: vec![],
            columns: vec![
                Column::in_report("complexity", "cyclomatic"),
                Column::in_report("complexity", "cognitive"),
                Column::in_report("complexity", "mi"),
                Column::in_report("complexity", "lines"),
                Column::in_report("complexity", "args"),
                Column::in_report("complexity", "exits"),
                Column::in_report("complexity", "language"),
                Column::in_report("complexity", "recommendation"),
            ],
            entries,
        }
    }
}

fn classify(cc: u32) -> LocalizedMessage {
    match cc {
        0..=5 => LocalizedMessage::code(messages::COMPLEXITY_RECOMMENDATION_SIMPLE),
        6..=10 => LocalizedMessage::code(messages::COMPLEXITY_RECOMMENDATION_OK),
        11..=20 => LocalizedMessage::code(messages::COMPLEXITY_RECOMMENDATION_HIGH)
            .with_severity(Severity::Warning)
            .with_param("cyclomatic", cc),
        _ => LocalizedMessage::code(messages::COMPLEXITY_RECOMMENDATION_VERY_HIGH)
            .with_severity(Severity::Error)
            .with_param("cyclomatic", cc),
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
        // In Dart the signature node is a *sibling* of the body (both children
        // of `declaration` / `class_member`). Walk up so both the span and the
        // decision scan cover the body.
        let scope = scope_for_decisions_and_span(node, spec, kind);
        let cyclomatic = 1 + count_decisions(&scope, spec.decision_kinds, spec.function_kinds);
        let cognitive = count_cognitive(&scope, spec.decision_kinds, spec.function_kinds);
        let nargs = count_params(node);
        let nexits = count_exits(&scope, spec.exit_kinds, spec.function_kinds);
        let start = scope.start_position().row as u32 + 1;
        let end = scope.end_position().row as u32 + 1;
        let line_count = count_code_lines(line_types, start, end);
        let volume = halstead_volume(&scope, source, spec.function_kinds);
        let mi = maintainability_index(volume, cyclomatic, line_count);
        out.push(FunctionMetric {
            file: file_path.to_string(),
            name,
            start_line: start,
            line_count,
            cyclomatic,
            cognitive,
            nargs,
            nexits,
            mi,
            language: spec.name,
        });
        // Continue descending so nested functions/closures are also captured.
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        visit(&child, spec, file_path, source, line_types, out);
    }
}

fn scope_for_decisions_and_span<'a>(node: &Node<'a>, spec: &LangSpec, kind: &str) -> Node<'a> {
    if spec.name == "Dart"
        && matches!(
            kind,
            "function_signature" | "method_signature" | "getter_signature" | "setter_signature"
        )
    {
        return node.parent().unwrap_or(*node);
    }
    *node
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

/// Cognitive complexity (simplified SonarSource rubric). Each decision node
/// contributes `1 + nesting_depth` where depth is the count of ancestor
/// decisions on the path from the function root. This approximates the full
/// spec (which distinguishes nesting-incrementing vs flat-incrementing kinds)
/// well enough to produce meaningful relative signal for a single-column
/// hotspot view. Nested functions are skipped so each gets its own entry.
fn count_cognitive(node: &Node, kinds: &[&str], func_kinds: &[&str]) -> u32 {
    fn walk(node: &Node, kinds: &[&str], func_kinds: &[&str], depth: u32) -> u32 {
        let mut total = 0u32;
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if func_kinds.contains(&child.kind()) {
                continue;
            }
            if kinds.contains(&child.kind()) {
                total = total.saturating_add(1 + depth);
                total = total.saturating_add(walk(&child, kinds, func_kinds, depth + 1));
            } else {
                total = total.saturating_add(walk(&child, kinds, func_kinds, depth));
            }
        }
        total
    }
    walk(node, kinds, func_kinds, 0)
}

/// Count declared parameters for a function node. Finds the first descendant
/// whose kind name contains `parameter` (covers `parameters`,
/// `formal_parameters`, `parameter_list`, `function_value_parameters`,
/// `formal_parameter_list`) and returns its named-child count.
fn count_params(func_node: &Node) -> u32 {
    let mut cursor = func_node.walk();
    for child in func_node.children(&mut cursor) {
        let k = child.kind();
        if k.contains("parameter") || k == "formal_parameters" {
            return child.named_child_count() as u32;
        }
    }
    0
}

/// Count explicit exit points (return / equivalent) inside the function's
/// span, skipping nested functions so their returns don't leak in.
fn count_exits(node: &Node, exit_kinds: &[&str], func_kinds: &[&str]) -> u32 {
    let mut count = 0u32;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if func_kinds.contains(&child.kind()) {
            continue;
        }
        if exit_kinds.contains(&child.kind()) {
            count += 1;
        }
        count += count_exits(&child, exit_kinds, func_kinds);
    }
    count
}

/// Compute Halstead volume for a function scope.
///
/// Halstead separates the token stream into **operators** (keywords,
/// punctuation, binary/unary ops) and **operands** (identifiers, literals).
/// Using tree-sitter's named/unnamed distinction as a generic proxy:
///   - `!is_named()` terminals → operators (covers `if`, `{`, `+`, `==` etc.)
///   - `is_named()` leaves     → operands (identifiers, literals)
///
/// `Volume = (N1 + N2) * log2(n1 + n2)` where N1/N2 are total counts and
/// n1/n2 are unique counts. Returns `0.0` when the scope has effectively
/// no token vocabulary (empty body, parse failure).
///
/// Nested function bodies are skipped so each function owns its own volume.
/// Comment nodes are excluded — Halstead measures semantic token density.
fn halstead_volume(scope_node: &Node, source: &str, func_kinds: &[&str]) -> f64 {
    let mut operators: HashMap<String, u32> = HashMap::new();
    let mut operands: HashMap<String, u32> = HashMap::new();
    let mut cursor = scope_node.walk();
    for child in scope_node.children(&mut cursor) {
        halstead_walk(&child, source, func_kinds, &mut operators, &mut operands);
    }

    let n1 = operators.len();
    let n2 = operands.len();
    let big_n1: u32 = operators.values().sum();
    let big_n2: u32 = operands.values().sum();

    let vocab = n1 + n2;
    let length = big_n1 + big_n2;
    if vocab <= 1 || length == 0 {
        return 0.0;
    }
    (length as f64) * (vocab as f64).log2()
}

fn halstead_walk(
    node: &Node,
    source: &str,
    func_kinds: &[&str],
    operators: &mut HashMap<String, u32>,
    operands: &mut HashMap<String, u32>,
) {
    let kind = node.kind();
    if func_kinds.contains(&kind) {
        // Nested function — its tokens belong to its own metric entry.
        return;
    }
    if is_comment_kind(kind) {
        return;
    }
    if node.child_count() == 0 {
        let bytes = source.as_bytes();
        let start = node.start_byte();
        let end = node.end_byte();
        if start > end || end > bytes.len() {
            return;
        }
        let text = String::from_utf8_lossy(&bytes[start..end]).into_owned();
        if text.is_empty() || text.chars().all(char::is_whitespace) {
            return;
        }
        if node.is_named() {
            *operands.entry(text).or_insert(0) += 1;
        } else {
            *operators.entry(text).or_insert(0) += 1;
        }
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        halstead_walk(&child, source, func_kinds, operators, operands);
    }
}

/// Tree-sitter grammars name comment nodes inconsistently; this covers the
/// common variants across our supported languages.
fn is_comment_kind(kind: &str) -> bool {
    matches!(
        kind,
        "comment"
            | "line_comment"
            | "block_comment"
            | "documentation_comment"
            | "doc_comment"
    )
}

/// Maintainability Index (Microsoft variant), scaled to 0-100.
///
/// ```text
/// raw = 171 - 5.2*ln(volume) - 0.23*cyclomatic - 16.2*ln(sloc)
/// mi  = clamp(raw * 100 / 171, 0, 100)
/// ```
///
/// Interpretation (common convention): ≥85 = highly maintainable, 65-84 =
/// moderate, <65 = hard to maintain. Returns a rounded `u32` for display.
fn maintainability_index(volume: f64, cyclomatic: u32, sloc: u32) -> u32 {
    let effective_sloc = sloc.max(1) as f64;
    let ln_v = if volume > 0.0 { volume.ln() } else { 0.0 };
    let raw =
        171.0 - 5.2 * ln_v - 0.23 * (cyclomatic as f64) - 16.2 * effective_sloc.ln();
    let scaled = (raw * 100.0 / 171.0).clamp(0.0, 100.0);
    scaled.round() as u32
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
        assert_eq!(classify(1).code, messages::COMPLEXITY_RECOMMENDATION_SIMPLE);
        assert_eq!(classify(7).code, messages::COMPLEXITY_RECOMMENDATION_OK);
        assert_eq!(classify(15).code, messages::COMPLEXITY_RECOMMENDATION_HIGH);
        assert_eq!(
            classify(50).code,
            messages::COMPLEXITY_RECOMMENDATION_VERY_HIGH
        );
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

    #[test]
    fn javascript_if_and_ternary() {
        let src = "function f(x) { return x > 0 ? (x > 10 ? 'big' : 'small') : 'neg'; }";
        let m = analyze(&JAVASCRIPT, src);
        let cc = m.iter().find(|x| x.name == "f").unwrap().cyclomatic;
        // 1 base + 2 ternary
        assert!(cc >= 3, "expected >= 3, got {cc}");
    }

    #[test]
    fn javascript_spec_resolves_for_mjs_and_cjs() {
        assert_eq!(spec_for_path("a.js").unwrap().name, "JavaScript");
        assert_eq!(spec_for_path("a.jsx").unwrap().name, "JavaScript");
        assert_eq!(spec_for_path("a.mjs").unwrap().name, "JavaScript");
        assert_eq!(spec_for_path("a.cjs").unwrap().name, "JavaScript");
        // .ts / .tsx should still go to TypeScript, not JavaScript.
        assert_eq!(spec_for_path("a.ts").unwrap().name, "TypeScript");
        assert_eq!(spec_for_path("a.tsx").unwrap().name, "TypeScript");
    }

    #[test]
    fn kotlin_when_branches_count() {
        let src = "fun f(x: Int): Int {\n    return when (x) {\n        0 -> 1\n        1 -> 2\n        else -> 3\n    }\n}\n";
        let m = analyze(&KOTLIN, src);
        let cc = m.iter().find(|x| x.name == "f").unwrap().cyclomatic;
        // 1 base + 3 when_entry = 4
        assert!(cc >= 3, "expected >= 3, got {cc}");
    }

    #[test]
    fn kotlin_spec_resolves_for_kt_and_kts() {
        assert_eq!(spec_for_path("a.kt").unwrap().name, "Kotlin");
        assert_eq!(spec_for_path("build.gradle.kts").unwrap().name, "Kotlin");
    }

    #[test]
    fn dart_if_and_for() {
        let src =
            "int f(List<int> xs) { var n = 0; for (var x in xs) { if (x > 0) n++; } return n; }\n";
        let m = analyze(&DART, src);
        let cc = m.iter().find(|x| x.name == "f").unwrap().cyclomatic;
        // 1 base + for + if = 3
        assert!(cc >= 3, "expected >= 3, got {cc}");
    }

    #[test]
    fn dart_spec_resolves() {
        assert_eq!(spec_for_path("lib/main.dart").unwrap().name, "Dart");
    }

    #[test]
    fn cognitive_nested_decisions_weighted() {
        // Nested if-in-for-in-function:
        //   for { if { ... } }
        // depth 0 decision: for  → +1
        // depth 1 decision: if   → +2
        // cognitive = 3; cyclomatic = 3 (base 1 + for + if).
        let src = "fn f(xs: &[i32]) { for x in xs { if *x > 0 { println!(\"{}\", x); } } }";
        let m = analyze(&RUST, src);
        let entry = m.iter().find(|x| x.name == "f").unwrap();
        assert_eq!(entry.cyclomatic, 3);
        assert_eq!(entry.cognitive, 3);
    }

    #[test]
    fn cognitive_flat_decisions_match_cyclomatic() {
        // Sequential (non-nested) decisions: depth stays 0, so cognitive
        // and (cyclomatic - 1) should match.
        let src = "fn f(x: i32) -> i32 { if x > 0 { return 1; } if x < 0 { return -1; } 0 }";
        let m = analyze(&RUST, src);
        let e = m.iter().find(|x| x.name == "f").unwrap();
        // cyclomatic: 1 + 2 ifs = 3. Cognitive: two flat +1 = 2.
        assert_eq!(e.cyclomatic, 3);
        assert_eq!(e.cognitive, 2);
    }

    #[test]
    fn cognitive_deep_nesting_blows_up() {
        // if { if { if { ... } } } at depths 0, 1, 2 → 1 + 2 + 3 = 6.
        let src = "fn f(a: bool, b: bool, c: bool) { if a { if b { if c { println!(\"x\"); } } } }";
        let m = analyze(&RUST, src);
        let e = m.iter().find(|x| x.name == "f").unwrap();
        assert_eq!(e.cyclomatic, 4); // base + 3 ifs
        assert_eq!(e.cognitive, 6);
    }

    #[test]
    fn nargs_counts_declared_parameters() {
        let rust_src = "fn f(a: i32, b: &str, c: bool) {}";
        let m = analyze(&RUST, rust_src);
        assert_eq!(m.iter().find(|x| x.name == "f").unwrap().nargs, 3);

        let py_src = "def f(self, x, y, *args, **kwargs):\n    pass\n";
        let m = analyze(&PYTHON, py_src);
        assert_eq!(m.iter().find(|x| x.name == "f").unwrap().nargs, 5);

        let go_src = "package p\nfunc f(a int, b string) {}\n";
        let m = analyze(&GO, go_src);
        assert_eq!(m.iter().find(|x| x.name == "f").unwrap().nargs, 2);
    }

    #[test]
    fn nargs_zero_for_noarg_functions() {
        let src = "fn f() {}";
        let m = analyze(&RUST, src);
        assert_eq!(m.iter().find(|x| x.name == "f").unwrap().nargs, 0);
    }

    #[test]
    fn nexits_counts_return_points() {
        let src = "fn f(x: i32) -> i32 { if x > 0 { return 1; } if x < 0 { return -1; } return 0; }";
        let m = analyze(&RUST, src);
        assert_eq!(m.iter().find(|x| x.name == "f").unwrap().nexits, 3);
    }

    #[test]
    fn nexits_zero_when_no_explicit_return() {
        // Implicit Rust expression-value return is not counted.
        let src = "fn f(x: i32) -> i32 { x + 1 }";
        let m = analyze(&RUST, src);
        assert_eq!(m.iter().find(|x| x.name == "f").unwrap().nexits, 0);
    }

    #[test]
    fn nested_function_exits_dont_leak() {
        // The closure's return should not count toward the outer function's
        // exits, mirroring how nested-function decisions are isolated.
        let src = "fn outer() { let inner = || { return 42; }; let _ = inner(); }";
        let m = analyze(&RUST, src);
        let outer = m.iter().find(|x| x.name == "outer").unwrap();
        assert_eq!(outer.nexits, 0);
        let closure = m.iter().find(|x| x.name == "<anonymous>").unwrap();
        assert_eq!(closure.nexits, 1);
    }

    #[test]
    fn maintainability_index_formula_clamps_and_scales() {
        // Pathological inputs: huge volume + CC + sloc must clamp to 0.
        let mi_worst = maintainability_index(1e9, 1000, 10_000);
        assert_eq!(mi_worst, 0);
        // Tiny inputs: should saturate near 100.
        let mi_best = maintainability_index(1.0, 1, 1);
        assert!(mi_best >= 99, "expected near-100, got {mi_best}");
        // Zero-volume edge case (empty body).
        let mi_zero_vol = maintainability_index(0.0, 1, 1);
        assert!(mi_zero_vol >= 99);
    }

    #[test]
    fn mi_simple_function_high() {
        // Empty body → volume 0, cc 1, sloc ~1 → MI should be near 100.
        let src = "fn f() {}";
        let m = analyze(&RUST, src);
        let e = m.iter().find(|x| x.name == "f").unwrap();
        assert!(e.mi >= 90, "expected >= 90 for trivial fn, got {}", e.mi);
    }

    #[test]
    fn mi_complex_function_lower_than_simple() {
        // A trivial function and a branchy function should rank in that
        // order: trivial.mi > complex.mi.
        let trivial = "fn a() {}";
        let complex = "fn b(x: i32) -> i32 {
            if x > 100 { if x > 1000 { if x > 10000 { return 4; } else { return 3; } }
            else { return 2; } } else if x > 10 { return 1; } else if x > 0 { return 0; }
            else if x < -10 { return -1; } else { return -2; }
        }";
        let m_trivial = analyze(&RUST, trivial);
        let m_complex = analyze(&RUST, complex);
        let mi_a = m_trivial.iter().find(|x| x.name == "a").unwrap().mi;
        let mi_b = m_complex.iter().find(|x| x.name == "b").unwrap().mi;
        assert!(mi_a > mi_b, "trivial MI {mi_a} should exceed complex MI {mi_b}");
    }

    #[test]
    fn halstead_volume_non_negative_and_scales_with_length() {
        // Build a minimal Rust AST via parser so we can call the helper directly.
        let src_short = "fn f() { let a = 1; }";
        let src_long = "fn f() { let a = 1; let b = 2; let c = 3; let d = 4; }";
        let mut parsers = HashMap::new();
        parsers.entry("Rust").or_insert_with(|| {
            let mut p = Parser::new();
            let _ = p.set_language(&tree_sitter_rust::LANGUAGE.into());
            p
        });
        let mut vol = |src: &str| -> f64 {
            let parser = parsers.get_mut("Rust").unwrap();
            let tree = parser.parse(src, None).unwrap();
            let func = tree
                .root_node()
                .child(0)
                .expect("source_file should have a child");
            halstead_volume(&func, src, RUST.function_kinds)
        };
        let v_short = vol(src_short);
        let v_long = vol(src_long);
        assert!(v_short > 0.0);
        assert!(
            v_long > v_short,
            "longer body should have larger volume: {v_long} vs {v_short}"
        );
    }

    #[test]
    fn lines_counts_code_only_excluding_comments_and_blanks() {
        // Function with 1 code line, 2 comment lines, 1 blank.
        let src = "fn f() {\n    // comment one\n    // comment two\n\n    let _ = 1;\n}\n";
        let m = analyze(&RUST, src);
        let metric = m.iter().find(|x| x.name == "f").unwrap();
        // Body code lines: `fn f() {`, `let _ = 1;`, `}` => 3 code lines.
        assert_eq!(
            metric.line_count, 3,
            "expected code-only SLOC of 3, got {}",
            metric.line_count
        );
    }
}
