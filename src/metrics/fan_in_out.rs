use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use regex::Regex;

use crate::analysis::source_filter::is_source_file;
use crate::messages;
use crate::metrics::MetricCollector;
use crate::types::{
    Column, LocalizedMessage, MetricEntry, MetricResult, MetricValue, ParsedChange,
    report_description, report_display,
};

/// Skip files larger than this when scanning for imports.
const MAX_BLOB_BYTES: u64 = 200 * 1024;

#[derive(Default, Clone, Copy)]
struct Counts {
    fan_in: u64,
    fan_out: u64,
}

pub struct FanInOutCollector {
    counts: HashMap<String, Counts>,
}

impl Default for FanInOutCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl FanInOutCollector {
    pub fn new() -> Self {
        Self {
            counts: HashMap::new(),
        }
    }
}

impl MetricCollector for FanInOutCollector {
    fn name(&self) -> &str {
        "fan_in_out"
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

        progress.status("  fan_in_out: pass 1/2 collecting source paths...");
        // Pass 1: collect all candidate source paths in the repo.
        let mut all_paths: Vec<(String, gix::ObjectId, u64)> = vec![];
        collect_blobs(repo, &tree, "", &mut all_paths);
        let path_set: HashSet<String> = all_paths.iter().map(|(p, _, _)| p.clone()).collect();

        // Pass 2: for each source file, extract imports and resolve them against the path set.
        let total = all_paths.len();
        for (idx, (path, oid, size)) in all_paths.iter().enumerate() {
            if idx.is_multiple_of(200) {
                progress.status(&format!(
                    "  fan_in_out: pass 2/2 {}/{total} files...",
                    idx + 1
                ));
            }
            if !is_source_file(path) {
                continue;
            }
            if *size > MAX_BLOB_BYTES {
                continue;
            }
            let Some(lang) = detect_lang(path) else {
                continue;
            };
            let Ok(object) = repo.find_object(*oid) else {
                continue;
            };
            let Ok(blob) = object.try_into_blob() else {
                continue;
            };
            let Ok(source) = std::str::from_utf8(&blob.data) else {
                continue;
            };

            let imports = extract_imports(lang, source);
            for raw in imports {
                if let Some(target) = resolve_import(lang, &raw, path, &path_set) {
                    if target == *path {
                        continue; // self-import shouldn't happen, but guard
                    }
                    self.counts.entry(target).or_default().fan_in += 1;
                    self.counts.entry(path.clone()).or_default().fan_out += 1;
                }
            }
            // Make sure every source file appears even if it imports nothing or is unimported.
            self.counts.entry(path.clone()).or_default();
        }

        Ok(())
    }

    fn finalize(&mut self) -> MetricResult {
        let mut entries: Vec<MetricEntry> = self
            .counts
            .drain()
            .filter(|(_, c)| c.fan_in + c.fan_out > 0)
            .map(|(path, c)| {
                let total = c.fan_in + c.fan_out;
                let instability = c
                    .fan_out
                    .saturating_mul(100)
                    .checked_div(total)
                    .unwrap_or(0);
                let role = classify(c.fan_in, c.fan_out);
                let mut values = HashMap::new();
                values.insert("fan_in".into(), MetricValue::Count(c.fan_in));
                values.insert("fan_out".into(), MetricValue::Count(c.fan_out));
                values.insert("instability_pct".into(), MetricValue::Count(instability));
                values.insert("role".into(), MetricValue::Message(role));
                MetricEntry { key: path, values }
            })
            .collect();

        // Sort by fan_in desc — critical files first.
        entries.sort_by(|a, b| {
            let ia = match a.values.get("fan_in") {
                Some(MetricValue::Count(n)) => *n,
                _ => 0,
            };
            let ib = match b.values.get("fan_in") {
                Some(MetricValue::Count(n)) => *n,
                _ => 0,
            };
            ib.cmp(&ia)
        });
        entries.truncate(150);

        MetricResult {
            name: "fan_in_out".into(),
            display_name: report_display("fan_in_out"),
            description: report_description("fan_in_out"),
            entry_groups: vec![],
            columns: vec![
                Column::in_report("fan_in_out", "fan_in"),
                Column::in_report("fan_in_out", "fan_out"),
                Column::in_report("fan_in_out", "instability_pct"),
                Column::in_report("fan_in_out", "role"),
            ],
            entries,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Lang {
    Rust,
    Python,
    TypeScript,
}

pub(crate) fn detect_lang(path: &str) -> Option<Lang> {
    let ext = path.rsplit('.').next()?;
    match ext {
        "rs" => Some(Lang::Rust),
        "py" | "pyi" => Some(Lang::Python),
        "ts" | "tsx" | "js" | "jsx" => Some(Lang::TypeScript),
        _ => None,
    }
}

fn classify(fan_in: u64, fan_out: u64) -> LocalizedMessage {
    let code = if fan_in >= 5 && fan_out <= 2 {
        messages::FAN_IN_OUT_ROLE_HUB
    } else if fan_out >= 5 && fan_in <= 1 {
        messages::FAN_IN_OUT_ROLE_ORCHESTRATOR
    } else if fan_in == 0 && fan_out > 0 {
        messages::FAN_IN_OUT_ROLE_LEAF
    } else if fan_out == 0 && fan_in > 0 {
        messages::FAN_IN_OUT_ROLE_PURE_DEP
    } else {
        messages::FAN_IN_OUT_ROLE_MIXED
    };
    LocalizedMessage::code(code)
        .with_param("fan_in", fan_in)
        .with_param("fan_out", fan_out)
}

pub(crate) fn collect_blobs(
    repo: &gix::Repository,
    tree: &gix::Tree,
    prefix: &str,
    out: &mut Vec<(String, gix::ObjectId, u64)>,
) {
    use gix::prelude::HeaderExt;
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
                collect_blobs(repo, &subtree, &full_path, out);
            }
        } else if mode.is_blob() && detect_lang(&full_path).is_some() {
            let size = repo.objects.header(id).map(|h| h.size()).unwrap_or(0);
            out.push((full_path, id.into(), size));
        }
    }
}

// --- Import extraction (regex) ----------------------------------------------------

fn rust_use_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    // Capture the whole path body of a `use ...;` statement (everything up to
    // the terminating `;`), so brace groups can be expanded downstream.
    R.get_or_init(|| Regex::new(r"^[ \t]*(?:pub\s+)?use\s+([^;]+);").unwrap())
}

/// Expand a Rust `use` path body into individual import paths, so a grouped
/// import like `crate::foo::{bar, baz}` yields `crate::foo::bar` and
/// `crate::foo::baz` (previously only the parent module resolved, undercounting
/// fan-in). One level of braces is handled — the common case. (finding #15)
fn expand_rust_use(body: &str) -> Vec<String> {
    let body = body.trim();
    let Some(open) = body.find('{') else {
        // No group: a plain `use a::b::c`. Drop any `as alias`.
        let head = body.split(" as ").next().unwrap_or(body).trim();
        return if head.is_empty() {
            vec![]
        } else {
            vec![head.to_string()]
        };
    };
    let prefix = &body[..open]; // includes the trailing `::`
    let close = body.rfind('}').unwrap_or(body.len());
    let group = &body[open + 1..close.min(body.len())];
    let mut out = Vec::new();
    for item in split_top_level_commas(group) {
        // Only the head segment matters for file resolution; ignore any nested
        // `::{...}` and `as alias`.
        let head = item
            .split("::{")
            .next()
            .unwrap_or(item)
            .split(" as ")
            .next()
            .unwrap_or(item)
            .trim();
        if head.is_empty() {
            continue;
        }
        if head == "self" || head == "*" {
            // `self`/glob refer to the module named by the prefix itself.
            out.push(prefix.trim_end_matches("::").to_string());
        } else {
            out.push(format!("{prefix}{head}"));
        }
    }
    out
}

/// Split on commas that are not nested inside `{...}` (one-level brace depth).
fn split_top_level_commas(s: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut start = 0usize;
    for (i, ch) in s.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => depth -= 1,
            ',' if depth <= 0 => {
                parts.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&s[start..]);
    parts
}

fn rust_mod_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^[ \t]*(?:pub\s+)?mod\s+([A-Za-z_]\w*)\s*;").unwrap())
}

fn python_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^[ \t]*(?:from\s+([\w.]+)\s+import|import\s+([\w.]+))").unwrap())
}

fn ts_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r#"(?:from|require\()\s*['"]([^'"]+)['"]"#).unwrap())
}

pub(crate) fn extract_imports(lang: Lang, source: &str) -> Vec<String> {
    let mut out = vec![];
    match lang {
        Lang::Rust => {
            for line in source.lines() {
                if let Some(c) = rust_use_re().captures(line)
                    && let Some(m) = c.get(1)
                {
                    out.extend(expand_rust_use(m.as_str()));
                }
                if let Some(c) = rust_mod_re().captures(line)
                    && let Some(m) = c.get(1)
                {
                    // record as a "mod foo" hint with marker prefix "mod:"
                    out.push(format!("mod:{}", m.as_str()));
                }
            }
        }
        Lang::Python => {
            for line in source.lines() {
                if let Some(c) = python_re().captures(line) {
                    if let Some(m) = c.get(1) {
                        out.push(m.as_str().to_string());
                    } else if let Some(m) = c.get(2) {
                        out.push(m.as_str().to_string());
                    }
                }
            }
        }
        Lang::TypeScript => {
            for c in ts_re().captures_iter(source) {
                if let Some(m) = c.get(1) {
                    out.push(m.as_str().to_string());
                }
            }
        }
    }
    out
}

// --- Import resolution ------------------------------------------------------------

pub(crate) fn resolve_import(
    lang: Lang,
    raw: &str,
    importer: &str,
    paths: &HashSet<String>,
) -> Option<String> {
    match lang {
        Lang::Rust => resolve_rust(raw, importer, paths),
        Lang::Python => resolve_python(raw, paths),
        Lang::TypeScript => resolve_ts(raw, importer, paths),
    }
}

/// Split a repo path into `(dir, stem)`, e.g. `src/metrics/foo.rs` ->
/// `("src/metrics", "foo")`.
fn split_dir_stem(path: &str) -> (&str, &str) {
    let (dir, file) = path.rsplit_once('/').unwrap_or(("", path));
    (dir, file.strip_suffix(".rs").unwrap_or(file))
}

fn join_dir(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

const MOD_FILE_STEMS: [&str; 3] = ["mod", "lib", "main"];

/// Try to resolve a Rust import path like `crate::foo::bar`, `self::x`,
/// `super::y`, or `mod:foo` to a file in the repo. `super::`/`self::` are
/// resolved relative to the importer's module directory rather than the crate
/// root — the old code treated them like `crate::` and missed them. (finding #15)
fn resolve_rust(raw: &str, importer: &str, paths: &HashSet<String>) -> Option<String> {
    let (importer_dir, importer_stem) = split_dir_stem(importer);
    let importer_is_mod = MOD_FILE_STEMS.contains(&importer_stem);

    if let Some(name) = raw.strip_prefix("mod:") {
        // `mod foo;` — a `mod.rs`/`lib.rs`/`main.rs` declares siblings in its
        // own dir; `foo.rs` declares submodules in `foo/`. Try both.
        let mut bases = vec![importer_dir.to_string()];
        if !importer_is_mod {
            bases.push(join_dir(importer_dir, importer_stem));
        }
        for base in &bases {
            for cand in candidate_rust_paths(base, name) {
                if paths.contains(&cand) {
                    return Some(cand);
                }
            }
        }
        return None;
    }

    // Base directories to resolve the path against + the remainder after the
    // leading keyword.
    let (bases, rest): (Vec<String>, &str) = if let Some(r) = raw.strip_prefix("crate::") {
        (vec!["src".to_string()], r)
    } else if let Some(r) = raw.strip_prefix("self::") {
        // `self` = the importer's own module; submodules of `foo.rs` live in `foo/`.
        let mut b = vec![importer_dir.to_string()];
        if !importer_is_mod {
            b.push(join_dir(importer_dir, importer_stem));
        }
        (b, r)
    } else {
        // `super` = the parent module: the importer's dir, or its parent when
        // the importer itself is a `mod.rs`/`lib.rs`/`main.rs`. Anything not
        // starting with a path keyword is an external crate -> None.
        let r = raw.strip_prefix("super::")?;
        let parent = if importer_is_mod {
            split_dir_stem(importer_dir).0.to_string()
        } else {
            importer_dir.to_string()
        };
        (vec![parent], r)
    };

    let parts: Vec<&str> = rest.split("::").filter(|s| !s.is_empty()).collect();
    if parts.is_empty() {
        return None;
    }
    // Longest prefix down to shortest: `a::b::c` -> a/b/c.rs, a/b/c/mod.rs, a/b…
    for take in (1..=parts.len()).rev() {
        let joined = parts[..take].join("/");
        for base in &bases {
            let prefix = if base.is_empty() {
                String::new()
            } else {
                format!("{base}/")
            };
            for cand in [
                format!("{prefix}{joined}.rs"),
                format!("{prefix}{joined}/mod.rs"),
            ] {
                if paths.contains(&cand) {
                    return Some(cand);
                }
            }
        }
    }
    None
}

fn candidate_rust_paths(dir: &str, name: &str) -> Vec<String> {
    let prefix = if dir.is_empty() {
        String::new()
    } else {
        format!("{dir}/")
    };
    vec![
        format!("{prefix}{name}.rs"),
        format!("{prefix}{name}/mod.rs"),
    ]
}

fn resolve_python(raw: &str, paths: &HashSet<String>) -> Option<String> {
    let dotted = raw.trim_start_matches('.');
    if dotted.is_empty() {
        return None;
    }
    let slashed = dotted.replace('.', "/");
    let candidates = [
        format!("{slashed}.py"),
        format!("{slashed}/__init__.py"),
        format!("src/{slashed}.py"),
        format!("src/{slashed}/__init__.py"),
    ];
    candidates.into_iter().find(|c| paths.contains(c))
}

fn resolve_ts(raw: &str, importer: &str, paths: &HashSet<String>) -> Option<String> {
    if !raw.starts_with('.') {
        return None; // bare/external module
    }
    let importer_dir = importer.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
    let combined = if importer_dir.is_empty() {
        raw.to_string()
    } else {
        format!("{importer_dir}/{raw}")
    };
    let normalized = normalize_path(&combined);

    let exts = ["ts", "tsx", "js", "jsx"];
    for ext in &exts {
        let cand = format!("{normalized}.{ext}");
        if paths.contains(&cand) {
            return Some(cand);
        }
    }
    for ext in &exts {
        let cand = format!("{normalized}/index.{ext}");
        if paths.contains(&cand) {
            return Some(cand);
        }
    }
    if paths.contains(&normalized) {
        return Some(normalized);
    }
    None
}

/// Collapse `./` and `../` segments in a slash-separated path.
fn normalize_path(p: &str) -> String {
    let mut out: Vec<&str> = vec![];
    for seg in p.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(items: &[&str]) -> HashSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn rust_use_crate_resolves() {
        let p = paths(&["src/foo/bar.rs", "src/lib.rs"]);
        let r = resolve_rust("crate::foo::bar::Thing", "src/lib.rs", &p);
        assert_eq!(r.as_deref(), Some("src/foo/bar.rs"));
    }

    #[test]
    fn rust_external_use_skipped() {
        let p = paths(&["src/lib.rs"]);
        assert!(resolve_rust("anyhow::Result", "src/lib.rs", &p).is_none());
    }

    #[test]
    fn rust_mod_decl_resolves_sibling() {
        let p = paths(&["src/parser/registry.rs", "src/parser/mod.rs"]);
        let r = resolve_rust("mod:registry", "src/parser/mod.rs", &p);
        assert_eq!(r.as_deref(), Some("src/parser/registry.rs"));
    }

    #[test]
    fn python_dotted_resolves() {
        let p = paths(&["a/b.py", "a/__init__.py"]);
        assert_eq!(resolve_python("a.b", &p).as_deref(), Some("a/b.py"));
    }

    #[test]
    fn ts_relative_resolves_with_extension() {
        let p = paths(&["src/lib/foo.ts", "src/index.ts"]);
        let r = resolve_ts("./lib/foo", "src/index.ts", &p);
        assert_eq!(r.as_deref(), Some("src/lib/foo.ts"));
    }

    #[test]
    fn ts_external_skipped() {
        let p = paths(&["src/index.ts"]);
        assert!(resolve_ts("react", "src/index.ts", &p).is_none());
    }

    #[test]
    fn extract_rust_uses() {
        let src = "use crate::a::b::C;\nuse std::collections::HashMap;\nmod foo;\n";
        let imps = extract_imports(Lang::Rust, src);
        assert!(imps.iter().any(|s| s == "crate::a::b::C"));
        assert!(imps.iter().any(|s| s == "std::collections::HashMap"));
        assert!(imps.iter().any(|s| s == "mod:foo"));
    }

    #[test]
    fn rust_brace_group_expands_to_each_item() {
        // `use crate::foo::{bar, baz}` must resolve BOTH foo/bar.rs and
        // foo/baz.rs, not just the parent module. (finding #15)
        let imps = expand_rust_use("crate::foo::{bar, baz as qux, self}");
        assert!(imps.contains(&"crate::foo::bar".to_string()));
        assert!(imps.contains(&"crate::foo::baz".to_string()));
        assert!(imps.contains(&"crate::foo".to_string())); // `self`

        let p = paths(&["src/foo/bar.rs", "src/foo/baz.rs", "src/lib.rs"]);
        assert_eq!(
            resolve_rust("crate::foo::bar", "src/lib.rs", &p).as_deref(),
            Some("src/foo/bar.rs")
        );
        assert_eq!(
            resolve_rust("crate::foo::baz", "src/lib.rs", &p).as_deref(),
            Some("src/foo/baz.rs")
        );
    }

    #[test]
    fn rust_super_resolves_relative_to_parent_module() {
        // `super::helper` from src/metrics/foo.rs must resolve src/metrics/helper.rs,
        // not src/helper.rs (the old crate-root behavior). (finding #15)
        let p = paths(&["src/metrics/foo.rs", "src/metrics/helper.rs", "src/lib.rs"]);
        assert_eq!(
            resolve_rust("super::helper::Thing", "src/metrics/foo.rs", &p).as_deref(),
            Some("src/metrics/helper.rs")
        );
    }

    #[test]
    fn rust_self_resolves_into_own_module_dir() {
        // `self::sub` from src/metrics/foo.rs -> src/metrics/foo/sub.rs. (finding #15)
        let p = paths(&["src/metrics/foo.rs", "src/metrics/foo/sub.rs"]);
        assert_eq!(
            resolve_rust("self::sub::Thing", "src/metrics/foo.rs", &p).as_deref(),
            Some("src/metrics/foo/sub.rs")
        );
    }

    #[test]
    fn classify_hub_vs_orchestrator() {
        assert_eq!(classify(10, 1).code, messages::FAN_IN_OUT_ROLE_HUB);
        assert_eq!(classify(0, 8).code, messages::FAN_IN_OUT_ROLE_ORCHESTRATOR);
        assert_eq!(classify(0, 0).code, messages::FAN_IN_OUT_ROLE_MIXED);
    }

    #[test]
    fn python_plain_import_resolves() {
        let p = paths(&["pkg/mod.py", "pkg/__init__.py"]);
        assert_eq!(resolve_python("pkg.mod", &p).as_deref(), Some("pkg/mod.py"));
    }

    #[test]
    fn python_init_module_resolves() {
        let p = paths(&["pkg/__init__.py"]);
        assert_eq!(
            resolve_python("pkg", &p).as_deref(),
            Some("pkg/__init__.py")
        );
    }

    #[test]
    fn ts_deep_relative_resolves() {
        // importer src/a/b/x.ts; import "../../c/leaf" → src/c/leaf.ts
        let p = paths(&["src/c/leaf.ts", "src/a/b/x.ts"]);
        assert_eq!(
            resolve_ts("../../c/leaf", "src/a/b/x.ts", &p).as_deref(),
            Some("src/c/leaf.ts")
        );
    }

    #[test]
    fn ts_index_file_resolves() {
        let p = paths(&["lib/utils/index.ts"]);
        assert_eq!(
            resolve_ts("./utils", "lib/main.ts", &p).as_deref(),
            Some("lib/utils/index.ts")
        );
    }

    #[test]
    fn normalize_path_collapses_dots() {
        assert_eq!(normalize_path("a/./b"), "a/b");
        assert_eq!(normalize_path("a/b/../c"), "a/c");
        assert_eq!(normalize_path("./a"), "a");
    }

    #[test]
    fn extract_python_imports_both_forms() {
        let src = "from a.b import x\nimport c.d\n# import not_a_real comment\n";
        let imps = extract_imports(Lang::Python, src);
        assert!(imps.iter().any(|s| s == "a.b"));
        assert!(imps.iter().any(|s| s == "c.d"));
    }

    #[test]
    fn extract_ts_handles_require_and_import() {
        let src = "import x from './foo';\nconst y = require('./bar');\n";
        let imps = extract_imports(Lang::TypeScript, src);
        assert!(imps.iter().any(|s| s == "./foo"));
        assert!(imps.iter().any(|s| s == "./bar"));
    }

    #[test]
    fn rust_use_picks_longest_resolvable_prefix() {
        // `crate::a::b::Symbol` should resolve to src/a/b.rs even though Symbol isn't a file
        let p = paths(&["src/a/b.rs", "src/a.rs"]);
        let r = resolve_rust("crate::a::b::Symbol", "src/lib.rs", &p);
        assert_eq!(r.as_deref(), Some("src/a/b.rs"));
    }
}
