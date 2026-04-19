//! Public-API documentation coverage per file.
//!
//! For each supported language, walk top-level items (functions, classes,
//! traits/interfaces, types), decide whether each is "public" (language-
//! specific visibility rule), and whether a documentation comment is attached.
//! Emits one entry per file ranked ascending by coverage%, so files needing
//! the most attention rise to the top.
//!
//! Detection is intentionally lightweight — prev-sibling comment check plus a
//! per-language visibility heuristic. This is not a formal API-surface
//! analyzer. It catches the high-signal gap ("no one wrote anything for this
//! public struct") without trying to validate doc content quality.

use std::collections::HashMap;

use tree_sitter::{Language, Node, Parser};

use crate::analysis::source_filter::is_source_file;
use crate::metrics::MetricCollector;
use crate::types::{
    Column, LocalizedMessage, MetricEntry, MetricResult, MetricValue, ParsedChange, Severity,
    report_description, report_display,
};

/// How many files the report keeps at the top. Coverage-sorted, so this is
/// "worst N". Avoids dumping thousands of rows on a monorepo.
const MAX_FILES: usize = 200;

/// Minimum number of public items needed for a file to appear. Files with 1-2
/// items produce noisy 0% or 50% rows; we want meaningful aggregates.
const MIN_ITEMS: u32 = 3;

#[derive(Clone, Copy)]
enum VisibilityRule {
    /// Rust: child `visibility_modifier` node whose text begins with `pub`.
    RustPub,
    /// Python: the item's `name` field does not start with `_`.
    PythonNotUnderscore,
    /// TypeScript / JavaScript: item is wrapped in `export_statement`, or its
    /// parent is, or its text prefix is literally `export `.
    JsExport,
    /// Go: the item's name begins with an uppercase ASCII letter.
    GoCapitalized,
    /// Java: has a `modifiers` node containing `public`.
    JavaPublicModifier,
    /// Kotlin: treat as public unless `private` / `internal` / `protected`
    /// appears in its modifier list (Kotlin's default visibility is public).
    KotlinDefaultPublic,
    /// Dart: the item's `name` field does not start with `_`.
    DartNotUnderscore,
}

#[derive(Clone, Copy)]
enum DocRule {
    /// Rust / Dart: prev sibling is a `line_comment` beginning with `///` OR a
    /// `block_comment` beginning with `/**` (outer doc).
    TripleSlashOrBlockDoc,
    /// Python: the item's `body` has an opening `expression_statement` whose
    /// child is a `string` literal (the docstring).
    PythonDocstring,
    /// JSDoc / Javadoc / KDoc: prev sibling is `comment` (or `block_comment`)
    /// starting with `/**`.
    BlockDoc,
    /// Go: prev sibling is a `comment` starting with `//` (any `//` prefix).
    GoLineComment,
}

struct DocSpec {
    name: &'static str,
    language: fn() -> Language,
    /// Top-level item node kinds we consider documentation targets.
    item_kinds: &'static [&'static str],
    visibility: VisibilityRule,
    doc: DocRule,
}

const RUST: DocSpec = DocSpec {
    name: "Rust",
    language: || tree_sitter_rust::LANGUAGE.into(),
    item_kinds: &[
        "function_item",
        "struct_item",
        "enum_item",
        "trait_item",
        "type_item",
        "mod_item",
    ],
    visibility: VisibilityRule::RustPub,
    doc: DocRule::TripleSlashOrBlockDoc,
};

const PYTHON: DocSpec = DocSpec {
    name: "Python",
    language: || tree_sitter_python::LANGUAGE.into(),
    item_kinds: &["function_definition", "class_definition"],
    visibility: VisibilityRule::PythonNotUnderscore,
    doc: DocRule::PythonDocstring,
};

const TYPESCRIPT: DocSpec = DocSpec {
    name: "TypeScript",
    language: || tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
    item_kinds: &[
        "function_declaration",
        "class_declaration",
        "interface_declaration",
        "type_alias_declaration",
        "enum_declaration",
    ],
    visibility: VisibilityRule::JsExport,
    doc: DocRule::BlockDoc,
};

const JAVASCRIPT: DocSpec = DocSpec {
    name: "JavaScript",
    language: || tree_sitter_javascript::LANGUAGE.into(),
    item_kinds: &[
        "function_declaration",
        "class_declaration",
        "generator_function_declaration",
    ],
    visibility: VisibilityRule::JsExport,
    doc: DocRule::BlockDoc,
};

const GO: DocSpec = DocSpec {
    name: "Go",
    language: || tree_sitter_go::LANGUAGE.into(),
    item_kinds: &[
        "function_declaration",
        "method_declaration",
        "type_declaration",
    ],
    visibility: VisibilityRule::GoCapitalized,
    doc: DocRule::GoLineComment,
};

const JAVA: DocSpec = DocSpec {
    name: "Java",
    language: || tree_sitter_java::LANGUAGE.into(),
    item_kinds: &[
        "method_declaration",
        "class_declaration",
        "interface_declaration",
        "enum_declaration",
    ],
    visibility: VisibilityRule::JavaPublicModifier,
    doc: DocRule::BlockDoc,
};

const KOTLIN: DocSpec = DocSpec {
    name: "Kotlin",
    language: || tree_sitter_kotlin_ng::LANGUAGE.into(),
    item_kinds: &[
        "function_declaration",
        "class_declaration",
        "object_declaration",
    ],
    visibility: VisibilityRule::KotlinDefaultPublic,
    doc: DocRule::BlockDoc,
};

const DART: DocSpec = DocSpec {
    name: "Dart",
    language: || tree_sitter_dart::LANGUAGE.into(),
    item_kinds: &["function_signature", "method_signature", "class_definition"],
    visibility: VisibilityRule::DartNotUnderscore,
    doc: DocRule::TripleSlashOrBlockDoc,
};

const SUPPORTED: &[DocSpec] = &[RUST, PYTHON, TYPESCRIPT, JAVASCRIPT, GO, JAVA, KOTLIN, DART];

fn spec_for_path(path: &str) -> Option<&'static DocSpec> {
    let ext = path.rsplit('.').next()?;
    let name = match ext {
        "rs" => "Rust",
        "py" | "pyi" => "Python",
        "ts" | "tsx" => "TypeScript",
        "js" | "jsx" | "mjs" | "cjs" => "JavaScript",
        "go" => "Go",
        "java" => "Java",
        "kt" | "kts" => "Kotlin",
        "dart" => "Dart",
        _ => return None,
    };
    SUPPORTED.iter().find(|s| s.name == name)
}

pub struct DocCoverageCollector {
    per_file: Vec<FileRow>,
}

#[derive(Clone)]
struct FileRow {
    file: String,
    language: &'static str,
    public_items: u32,
    documented: u32,
}

impl Default for DocCoverageCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl DocCoverageCollector {
    pub fn new() -> Self {
        Self {
            per_file: Vec::new(),
        }
    }
}

impl MetricCollector for DocCoverageCollector {
    fn name(&self) -> &str {
        "doc_coverage"
    }

    fn process(&mut self, _change: &ParsedChange) {
        // Head-tree walk; no per-commit work.
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
        let mut scanned = 0u64;
        walk_tree(
            repo,
            &tree,
            "",
            &mut parsers,
            &mut self.per_file,
            &mut scanned,
            progress,
        );
        Ok(())
    }

    fn finalize(&mut self) -> MetricResult {
        let mut rows: Vec<FileRow> = self
            .per_file
            .drain(..)
            .filter(|r| r.public_items >= MIN_ITEMS)
            .collect();
        // Coverage asc (worst first); tie-break by more items desc (bigger
        // gaps more important).
        rows.sort_by(|a, b| {
            let ca = coverage_pct(a);
            let cb = coverage_pct(b);
            ca.cmp(&cb)
                .then(b.public_items.cmp(&a.public_items))
                .then(a.file.cmp(&b.file))
        });
        rows.truncate(MAX_FILES);

        let entries: Vec<MetricEntry> = rows
            .into_iter()
            .map(|r| {
                let pct = coverage_pct(&r);
                let recommendation = classify(pct);
                let mut values = HashMap::new();
                values.insert(
                    "public_items".into(),
                    MetricValue::Count(r.public_items as u64),
                );
                values.insert("documented".into(), MetricValue::Count(r.documented as u64));
                values.insert("coverage_pct".into(), MetricValue::Count(pct as u64));
                values.insert("language".into(), MetricValue::Text(r.language.into()));
                values.insert(
                    "recommendation".into(),
                    MetricValue::Message(recommendation),
                );
                MetricEntry {
                    key: r.file,
                    values,
                }
            })
            .collect();

        MetricResult {
            name: "doc_coverage".into(),
            display_name: report_display("doc_coverage"),
            description: report_description("doc_coverage"),
            entry_groups: vec![],
            columns: vec![
                Column::in_report("doc_coverage", "public_items"),
                Column::in_report("doc_coverage", "documented"),
                Column::in_report("doc_coverage", "coverage_pct"),
                Column::in_report("doc_coverage", "language"),
                Column::in_report("doc_coverage", "recommendation"),
            ],
            entries,
        }
    }
}

fn coverage_pct(r: &FileRow) -> u32 {
    if r.public_items == 0 {
        return 100;
    }
    ((r.documented as f64 / r.public_items as f64) * 100.0).round() as u32
}

fn classify(pct: u32) -> LocalizedMessage {
    match pct {
        0..=24 => LocalizedMessage::code(crate::messages::DOC_COVERAGE_RECOMMENDATION_POOR)
            .with_severity(Severity::Error)
            .with_param("pct", pct),
        25..=59 => LocalizedMessage::code(crate::messages::DOC_COVERAGE_RECOMMENDATION_LOW)
            .with_severity(Severity::Warning)
            .with_param("pct", pct),
        60..=84 => LocalizedMessage::code(crate::messages::DOC_COVERAGE_RECOMMENDATION_OK),
        _ => LocalizedMessage::code(crate::messages::DOC_COVERAGE_RECOMMENDATION_GOOD),
    }
}

fn walk_tree(
    repo: &gix::Repository,
    tree: &gix::Tree,
    prefix: &str,
    parsers: &mut HashMap<&'static str, Parser>,
    out: &mut Vec<FileRow>,
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
                walk_tree(repo, &subtree, &full_path, parsers, out, scanned, progress);
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
            if let Some(row) = analyze_file(spec, &full_path, source, parsers) {
                out.push(row);
            }
            *scanned += 1;
            if scanned.is_multiple_of(200) {
                progress.status(&format!("  doc_coverage: {} files parsed...", *scanned));
            }
        }
    }
}

fn analyze_file(
    spec: &'static DocSpec,
    file_path: &str,
    source: &str,
    parsers: &mut HashMap<&'static str, Parser>,
) -> Option<FileRow> {
    let parser = parsers.entry(spec.name).or_insert_with(|| {
        let mut p = Parser::new();
        let _ = p.set_language(&(spec.language)());
        p
    });
    let tree = parser.parse(source, None)?;
    let mut public = 0u32;
    let mut documented = 0u32;
    visit(
        &tree.root_node(),
        spec,
        source,
        &mut public,
        &mut documented,
    );
    Some(FileRow {
        file: file_path.to_string(),
        language: spec.name,
        public_items: public,
        documented,
    })
}

fn visit(node: &Node, spec: &DocSpec, source: &str, public: &mut u32, documented: &mut u32) {
    if spec.item_kinds.contains(&node.kind()) && is_public(node, source, spec.visibility) {
        *public += 1;
        // TS/JS top-level items are often wrapped in `export_statement` or
        // `decorated_definition` (Python decorators). Doc comments attach to
        // the outer wrapper, not the inner declaration, so anchor the
        // previous-sibling lookup there.
        let anchor = outermost_wrapper(*node);
        if has_doc(&anchor, source, spec.doc) {
            *documented += 1;
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        visit(&child, spec, source, public, documented);
    }
}

fn outermost_wrapper(mut anchor: Node<'_>) -> Node<'_> {
    while let Some(parent) = anchor.parent() {
        match parent.kind() {
            "export_statement" | "export_declaration" | "decorated_definition" => {
                anchor = parent;
            }
            _ => break,
        }
    }
    anchor
}

fn node_text<'a>(node: &Node, source: &'a str) -> &'a str {
    let bytes = source.as_bytes();
    let start = node.start_byte();
    let end = node.end_byte();
    if start > end || end > bytes.len() {
        return "";
    }
    std::str::from_utf8(&bytes[start..end]).unwrap_or("")
}

fn is_public(node: &Node, source: &str, rule: VisibilityRule) -> bool {
    match rule {
        VisibilityRule::RustPub => {
            has_child_kind_with_text_prefix(node, "visibility_modifier", source, "pub")
        }
        VisibilityRule::PythonNotUnderscore | VisibilityRule::DartNotUnderscore => {
            match node.child_by_field_name("name") {
                Some(n) => !node_text(&n, source).starts_with('_'),
                None => false,
            }
        }
        VisibilityRule::JsExport => {
            // Walk up to at most 2 levels to find an export wrapper; some
            // grammars insert a `declaration` field node between the function
            // and the `export_statement`.
            let mut cur = node.parent();
            for _ in 0..3 {
                let Some(p) = cur else { break };
                if matches!(p.kind(), "export_statement" | "export_declaration") {
                    return true;
                }
                if matches!(p.kind(), "program" | "source_file" | "module") {
                    break;
                }
                cur = p.parent();
            }
            node_text(node, source).trim_start().starts_with("export ")
        }
        VisibilityRule::GoCapitalized => match node.child_by_field_name("name") {
            Some(n) => node_text(&n, source)
                .chars()
                .next()
                .map(|c| c.is_ascii_uppercase())
                .unwrap_or(false),
            None => false,
        },
        VisibilityRule::JavaPublicModifier => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "modifiers" && node_text(&child, source).contains("public") {
                    return true;
                }
            }
            false
        }
        VisibilityRule::KotlinDefaultPublic => {
            // Default public unless text contains a non-public modifier.
            let text = node_text(node, source);
            // Only look at the declaration header, not the body, to avoid
            // false positives from nested `private` helpers.
            let head: &str = text.split('{').next().unwrap_or(text);
            !(head.contains(" private ")
                || head.starts_with("private ")
                || head.contains(" internal ")
                || head.starts_with("internal ")
                || head.contains(" protected ")
                || head.starts_with("protected "))
        }
    }
}

fn has_child_kind_with_text_prefix(node: &Node, kind: &str, source: &str, prefix: &str) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == kind && node_text(&child, source).trim_start().starts_with(prefix) {
            return true;
        }
    }
    false
}

fn has_doc(node: &Node, source: &str, rule: DocRule) -> bool {
    match rule {
        DocRule::TripleSlashOrBlockDoc => match node.prev_named_sibling() {
            Some(prev) => {
                let text = node_text(&prev, source);
                let k = prev.kind();
                (matches!(k, "line_comment" | "comment" | "documentation_comment")
                    && text.trim_start().starts_with("///"))
                    || (matches!(k, "block_comment" | "comment")
                        && text.trim_start().starts_with("/**"))
            }
            None => false,
        },
        DocRule::BlockDoc => match node.prev_named_sibling() {
            Some(prev) => {
                let k = prev.kind();
                matches!(k, "block_comment" | "comment" | "multiline_comment")
                    && node_text(&prev, source).trim_start().starts_with("/**")
            }
            None => false,
        },
        DocRule::GoLineComment => match node.prev_named_sibling() {
            Some(prev) => {
                prev.kind() == "comment" && node_text(&prev, source).trim_start().starts_with("//")
            }
            None => false,
        },
        DocRule::PythonDocstring => {
            // function_definition / class_definition has a `body` field.
            let body = match node.child_by_field_name("body") {
                Some(b) => b,
                None => return false,
            };
            // First named child is usually an expression_statement wrapping a
            // string literal — that's the docstring.
            let Some(first) = body.named_child(0) else {
                return false;
            };
            if first.kind() != "expression_statement" {
                return false;
            }
            let Some(inner) = first.named_child(0) else {
                return false;
            };
            matches!(inner.kind(), "string" | "concatenated_string")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(spec: &'static DocSpec, src: &str) -> (u32, u32) {
        let mut parsers = HashMap::new();
        let row = analyze_file(spec, "t", src, &mut parsers).expect("row");
        (row.public_items, row.documented)
    }

    #[test]
    fn rust_pub_fn_with_doc_counts_documented() {
        let src = "/// Doc for foo.\npub fn foo() {}\nfn private_bar() {}\n";
        let (public, documented) = scan(&RUST, src);
        assert_eq!(public, 1);
        assert_eq!(documented, 1);
    }

    #[test]
    fn rust_undocumented_pub_item_still_counted_as_public() {
        let src = "pub fn a() {}\npub struct B;\npub fn c() {}\n";
        let (public, documented) = scan(&RUST, src);
        assert_eq!(public, 3);
        assert_eq!(documented, 0);
    }

    #[test]
    fn rust_private_items_not_counted() {
        let src = "fn a() {}\nstruct B;\nfn c() {}\n";
        let (public, documented) = scan(&RUST, src);
        assert_eq!(public, 0);
        assert_eq!(documented, 0);
    }

    #[test]
    fn python_docstring_detected() {
        let src = "def foo():\n    \"\"\"I am a docstring.\"\"\"\n    pass\n\ndef bar():\n    pass\n\ndef _private():\n    pass\n";
        let (public, documented) = scan(&PYTHON, src);
        // foo and bar are public; _private is not.
        assert_eq!(public, 2);
        assert_eq!(documented, 1);
    }

    #[test]
    fn go_capitalized_names_are_public() {
        let src = "package p\n// Foo does foo.\nfunc Foo() {}\n\nfunc bar() {}\n";
        let (public, documented) = scan(&GO, src);
        assert_eq!(public, 1);
        assert_eq!(documented, 1);
    }

    #[test]
    fn typescript_export_with_jsdoc() {
        let src = "/** Adds two numbers. */\nexport function add(a: number, b: number): number { return a + b; }\nfunction helper() {}\n";
        let (public, documented) = scan(&TYPESCRIPT, src);
        assert_eq!(public, 1);
        assert_eq!(documented, 1);
    }

    #[test]
    fn coverage_pct_computes_correctly() {
        let r = FileRow {
            file: "x".into(),
            language: "Rust",
            public_items: 4,
            documented: 1,
        };
        assert_eq!(coverage_pct(&r), 25);
        let r2 = FileRow {
            file: "x".into(),
            language: "Rust",
            public_items: 0,
            documented: 0,
        };
        // Zero-item file returns 100% to avoid "0% coverage" on empty files.
        assert_eq!(coverage_pct(&r2), 100);
    }

    #[test]
    fn classify_threshold_boundaries() {
        assert_eq!(
            classify(10).code,
            crate::messages::DOC_COVERAGE_RECOMMENDATION_POOR
        );
        assert_eq!(
            classify(50).code,
            crate::messages::DOC_COVERAGE_RECOMMENDATION_LOW
        );
        assert_eq!(
            classify(70).code,
            crate::messages::DOC_COVERAGE_RECOMMENDATION_OK
        );
        assert_eq!(
            classify(95).code,
            crate::messages::DOC_COVERAGE_RECOMMENDATION_GOOD
        );
    }
}
