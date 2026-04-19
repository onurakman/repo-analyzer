use std::collections::HashMap;
use std::sync::LazyLock;

use chrono::Utc;
use gix::bstr::BStr;
use regex::Regex;

use crate::analysis::line_classifier::{CommentState, LineType, classify_line};
use crate::analysis::source_filter::is_source_file;
use crate::langs::detect_language_info;
use crate::messages;
use crate::metrics::MetricCollector;
use crate::types::{
    Column, LocalizedMessage, MetricEntry, MetricResult, MetricValue, ParsedChange, Severity,
    report_description, report_display,
};

/// Files above this size are skipped — TODO scanning is cheap but blame on a
/// 10-MB vendored file is not worth the latency.
const MAX_BLOB_BYTES: u64 = 500 * 1024;

/// Total markers kept (top-N by age desc). Bounds memory on repos with
/// thousands of TODO comments.
const MAX_MARKERS: usize = 200;

/// When we've collected this many files with markers, stop blaming additional
/// files. Scan still surfaces the marker; age/author are left unknown.
const MAX_BLAME_FILES: usize = 150;

/// Matches debt-marker comments. Captures (marker, optional owner, text).
/// Case-insensitive — `TODO`, `Todo`, `todo` all match. Words these markers
/// also happen to spell don't appear in English prose, so false positives
/// are rare. The marker is uppercased before storage so rollups group.
/// Examples that match:
///   `// TODO: refactor this`           → ("TODO", "", "refactor this")
///   `# fixme(alice) broken on windows` → ("FIXME", "alice", "broken on windows")
///   `/* XXX revisit after merge */`    → ("XXX", "", "revisit after merge */")
static MARKER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(TODO|FIXME|HACK|XXX)\b(?:\(([^)]+)\))?\s*[:\-]?\s*(.*)").unwrap()
});

struct MarkerHit {
    file: String,
    line: u32,
    marker: String,
    owner: Option<String>,
    text: String,
}

struct EnrichedMarker {
    hit: MarkerHit,
    author: String,
    age_days: i64,
}

pub struct DebtMarkersCollector {
    hits: Vec<MarkerHit>,
    /// Populated in [`inspect_repo`] once blame enrichment finishes; drained
    /// in [`finalize`]. Kept separate from `hits` so the scan/enrich phases
    /// stay readable.
    enriched: Vec<EnrichedMarker>,
}

impl Default for DebtMarkersCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl DebtMarkersCollector {
    pub fn new() -> Self {
        Self {
            hits: Vec::new(),
            enriched: Vec::new(),
        }
    }
}

impl MetricCollector for DebtMarkersCollector {
    fn name(&self) -> &str {
        "debt_markers"
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
        let head_id = head_commit.id;
        let tree = head_commit.tree()?;

        let mut scanned = 0u64;
        walk_tree(repo, &tree, "", &mut self.hits, &mut scanned, progress);

        if self.hits.is_empty() {
            return Ok(());
        }

        // Enrich with blame (author + commit age) for the top files by marker
        // count. Uncapped blame on every file would dominate runtime.
        let mut by_file: HashMap<String, Vec<usize>> = HashMap::new();
        for (idx, hit) in self.hits.iter().enumerate() {
            by_file.entry(hit.file.clone()).or_default().push(idx);
        }
        let mut files: Vec<(String, Vec<usize>)> = by_file.into_iter().collect();
        files.sort_by_key(|(_, v)| std::cmp::Reverse(v.len()));
        files.truncate(MAX_BLAME_FILES);

        let now_secs = Utc::now().timestamp();
        let mut enriched: Vec<EnrichedMarker> = Vec::with_capacity(self.hits.len());

        for (blamed_idx, (path, indices)) in files.iter().enumerate() {
            if !is_source_file(path) {
                continue;
            }
            progress.status(&format!(
                "  debt_markers: blame {}/{} {}...",
                blamed_idx + 1,
                files.len(),
                path
            ));
            let opts = gix::repository::blame_file::Options::default();
            let outcome = match repo.blame_file(BStr::new(path.as_bytes()), head_id, opts) {
                Ok(o) => o,
                Err(_) => continue,
            };

            for &hit_idx in indices {
                let hit = &self.hits[hit_idx];
                let (author, age_days) = blame_for_line(repo, &outcome, hit.line, now_secs)
                    .unwrap_or_else(|| {
                        ("<unknown>".into(), -1) // -1 sentinel for "could not determine"
                    });
                enriched.push(EnrichedMarker {
                    hit: MarkerHit {
                        file: hit.file.clone(),
                        line: hit.line,
                        marker: hit.marker.clone(),
                        owner: hit.owner.clone(),
                        text: hit.text.clone(),
                    },
                    author,
                    age_days,
                });
            }
        }

        // Hits in files we skipped blaming go in with "unknown" age.
        for hit in &self.hits {
            if enriched
                .iter()
                .any(|e| e.hit.file == hit.file && e.hit.line == hit.line)
            {
                continue;
            }
            enriched.push(EnrichedMarker {
                hit: MarkerHit {
                    file: hit.file.clone(),
                    line: hit.line,
                    marker: hit.marker.clone(),
                    owner: hit.owner.clone(),
                    text: hit.text.clone(),
                },
                author: "<not-blamed>".into(),
                age_days: -1,
            });
        }

        // Sort oldest first; unknown-age (-1) sinks to bottom.
        enriched.sort_by(|a, b| match (a.age_days, b.age_days) {
            (-1, -1) => std::cmp::Ordering::Equal,
            (-1, _) => std::cmp::Ordering::Greater,
            (_, -1) => std::cmp::Ordering::Less,
            (x, y) => y.cmp(&x),
        });
        enriched.truncate(MAX_MARKERS);

        // Re-pack the enriched view back into `self.hits` keyed by ordering.
        self.hits.clear();
        // Stash enriched on a new storage; we'll rebuild entries in finalize.
        // But `finalize` only sees `self.hits`, so store structured data there.
        // Repurpose: write enriched into a side buffer via a second field? Simpler:
        // stash tuples in `self.hits.text` as a packed string. Ugly.
        //
        // Cleanest: shift to a different internal representation here.
        self.enriched = enriched;
        Ok(())
    }

    fn finalize(&mut self) -> MetricResult {
        let mut entries: Vec<MetricEntry> = self
            .enriched
            .drain(..)
            .map(|e| {
                let mut values = HashMap::new();
                values.insert("marker".into(), MetricValue::Text(e.hit.marker));
                values.insert(
                    "age_days".into(),
                    if e.age_days < 0 {
                        MetricValue::Text("—".into())
                    } else {
                        MetricValue::Count(e.age_days as u64)
                    },
                );
                values.insert("author".into(), MetricValue::Text(e.author));
                values.insert(
                    "owner".into(),
                    MetricValue::Text(e.hit.owner.unwrap_or_default()),
                );
                values.insert(
                    "text".into(),
                    MetricValue::Text(truncate_snippet(&e.hit.text)),
                );
                values.insert(
                    "recommendation".into(),
                    MetricValue::Message(classify(e.age_days)),
                );
                MetricEntry {
                    key: format!("{}:{}", e.hit.file, e.hit.line),
                    values,
                }
            })
            .collect();
        // Already sorted in inspect_repo; keep that order.
        let _ = &mut entries;

        MetricResult {
            name: "debt_markers".into(),
            display_name: report_display("debt_markers"),
            description: report_description("debt_markers"),
            entry_groups: vec![],
            columns: vec![
                Column::in_report("debt_markers", "marker"),
                Column::in_report("debt_markers", "age_days"),
                Column::in_report("debt_markers", "author"),
                Column::in_report("debt_markers", "owner"),
                Column::in_report("debt_markers", "text"),
                Column::in_report("debt_markers", "recommendation"),
            ],
            entries,
        }
    }
}

fn walk_tree(
    repo: &gix::Repository,
    tree: &gix::Tree,
    prefix: &str,
    hits: &mut Vec<MarkerHit>,
    scanned: &mut u64,
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
                walk_tree(repo, &subtree, &full_path, hits, scanned, progress);
            }
        } else if mode.is_blob() {
            let Ok(obj) = repo.find_object(id) else {
                continue;
            };
            let size = obj.data.len() as u64;
            if size == 0 || size > MAX_BLOB_BYTES {
                continue;
            }
            // Binary screen: NUL byte in first 8 KB → skip.
            let sample = &obj.data[..obj.data.len().min(8192)];
            if sample.contains(&0) {
                continue;
            }
            let Ok(content) = std::str::from_utf8(&obj.data) else {
                continue;
            };

            // Only scan files we can classify — otherwise we don't know which
            // lines are comments, and a `TODO` inside a string literal is
            // noise.
            let Some(lang) = detect_language_info(&full_path, Some(content)) else {
                continue;
            };

            scan_file(&full_path, content, lang, hits);
            *scanned += 1;
            if (*scanned).is_multiple_of(500) {
                progress.status(&format!("  debt_markers: scanned {} files...", *scanned));
            }
        }
    }
}

fn scan_file(path: &str, content: &str, lang: &crate::langs::Language, hits: &mut Vec<MarkerHit>) {
    let mut state = CommentState::new();
    for (idx, line) in content.lines().enumerate() {
        let line_type = classify_line(line, Some(lang), &mut state, idx == 0);
        let Some(caps) = MARKER_RE.captures(line) else {
            continue;
        };
        let marker_start = caps.get(0).expect("match group 0 always exists").start();
        let before_marker = &line[..marker_start];

        // Accept when either the whole line is a comment (handled by
        // classify_line, which also tracks nested block-comment state), OR a
        // language comment token appears on this line *before* the marker.
        // That second path catches inline trailing comments like
        //   `x = 1  # TODO: follow up`
        // which classify_line returns as Code because code came first.
        // Edge case: a marker embedded in a string literal on a line that
        // also contains an earlier `//` is a rare false positive we accept.
        let in_comment = matches!(line_type, LineType::Comment);
        let comment_precedes = lang
            .line_comments
            .iter()
            .any(|tok| before_marker.contains(tok))
            || lang
                .block_comments
                .iter()
                .any(|(start, _)| before_marker.contains(start));
        if !(in_comment || comment_precedes) {
            continue;
        }
        // Normalize the marker to upper-case so `Todo` / `todo` / `TODO`
        // don't split rollups into three buckets downstream.
        let marker = caps
            .get(1)
            .map(|m| m.as_str().to_ascii_uppercase())
            .unwrap_or_default();
        let owner = caps.get(2).map(|m| m.as_str().to_string());
        let text = caps
            .get(3)
            .map(|m| m.as_str().trim().to_string())
            .unwrap_or_default();
        hits.push(MarkerHit {
            file: path.to_string(),
            line: (idx as u32) + 1,
            marker,
            owner,
            text,
        });
    }
}

/// Find the commit that introduced `line` (1-based) in the blame outcome,
/// return (author_email, age_in_days).
fn blame_for_line(
    repo: &gix::Repository,
    outcome: &gix::blame::Outcome,
    line: u32,
    now_secs: i64,
) -> Option<(String, i64)> {
    // Blame entries cover a contiguous line range starting at
    // `start_in_blamed_file` (0-based) with `len` lines.
    let target_zero_based = line.saturating_sub(1);
    let entry = outcome.entries.iter().find(|e| {
        let start = e.start_in_blamed_file;
        let end = start + e.len.get();
        target_zero_based >= start && target_zero_based < end
    })?;
    let object = repo.find_object(entry.commit_id).ok()?;
    let commit = object.try_into_commit().ok()?;
    let author = commit.author().ok()?;
    let email = author.email.to_string();
    let time_secs = author.time().ok()?.seconds;
    let age = ((now_secs - time_secs).max(0)) / 86_400;
    Some((email, age))
}

fn classify(age_days: i64) -> LocalizedMessage {
    let (code, severity) = match age_days {
        i64::MIN..=-1 => (messages::DEBT_MARKERS_RECOMMENDATION_AGE_UNKNOWN, None),
        0..=29 => (messages::DEBT_MARKERS_RECOMMENDATION_FRESH, None),
        30..=179 => (
            messages::DEBT_MARKERS_RECOMMENDATION_AGING,
            Some(Severity::Info),
        ),
        180..=364 => (
            messages::DEBT_MARKERS_RECOMMENDATION_STALE,
            Some(Severity::Warning),
        ),
        _ => (
            messages::DEBT_MARKERS_RECOMMENDATION_ROTTEN,
            Some(Severity::Error),
        ),
    };
    let mut msg = LocalizedMessage::code(code).with_param("age_days", age_days.max(0));
    if let Some(s) = severity {
        msg = msg.with_severity(s);
    }
    msg
}

fn truncate_snippet(text: &str) -> String {
    const MAX: usize = 120;
    if text.len() <= MAX {
        text.to_string()
    } else {
        // Respect UTF-8 boundary.
        let mut idx = MAX;
        while idx > 0 && !text.is_char_boundary(idx) {
            idx -= 1;
        }
        format!("{}…", &text[..idx])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_regex_catches_plain_todo() {
        let caps = MARKER_RE.captures("// TODO: fix later").unwrap();
        assert_eq!(&caps[1], "TODO");
        assert_eq!(caps.get(2).map(|m| m.as_str()), None);
        assert_eq!(&caps[3], "fix later");
    }

    #[test]
    fn marker_regex_catches_owner_annotation() {
        let caps = MARKER_RE
            .captures("# FIXME(alice) broken on windows")
            .unwrap();
        assert_eq!(&caps[1], "FIXME");
        assert_eq!(&caps[2], "alice");
        assert_eq!(&caps[3], "broken on windows");
    }

    #[test]
    fn marker_regex_ignores_inline_word() {
        // "TODOS" is not a marker — \b boundary stops it.
        let matched_as_todo = MARKER_RE
            .captures("// TODOS list")
            .map(|c| c.get(1).unwrap().as_str() == "TODO")
            .unwrap_or(false);
        assert!(!matched_as_todo);
    }

    #[test]
    fn classify_age_bands() {
        assert_eq!(
            classify(-1).code,
            messages::DEBT_MARKERS_RECOMMENDATION_AGE_UNKNOWN
        );
        assert_eq!(
            classify(5).code,
            messages::DEBT_MARKERS_RECOMMENDATION_FRESH
        );
        assert_eq!(
            classify(45).code,
            messages::DEBT_MARKERS_RECOMMENDATION_AGING
        );
        assert_eq!(
            classify(200).code,
            messages::DEBT_MARKERS_RECOMMENDATION_STALE
        );
        assert_eq!(
            classify(500).code,
            messages::DEBT_MARKERS_RECOMMENDATION_ROTTEN
        );
        assert_eq!(classify(500).severity, Some(Severity::Error));
    }

    #[test]
    fn truncate_respects_utf8_boundary() {
        let t = "a".repeat(130);
        let out = truncate_snippet(&t);
        assert!(out.ends_with('…'));
        assert!(out.chars().count() < t.chars().count());

        // No truncation for short strings.
        assert_eq!(truncate_snippet("short"), "short");
    }

    #[test]
    fn scan_file_finds_todo_in_comment_not_in_string() {
        let rust = detect_language_info("foo.rs", None).unwrap();
        let src = "fn f() {\n    // TODO: refactor\n    let s = \"TODO: string literal\";\n}\n";
        let mut hits = Vec::new();
        scan_file("foo.rs", src, rust, &mut hits);
        assert_eq!(hits.len(), 1, "should only catch the comment TODO");
        assert_eq!(hits[0].marker, "TODO");
        assert_eq!(hits[0].line, 2);
        assert_eq!(hits[0].text, "refactor");
    }

    #[test]
    fn scan_file_is_case_insensitive_and_normalises_to_uppercase() {
        let rust = detect_language_info("foo.rs", None).unwrap();
        let src = "// todo: lower\n// Todo: title\n// TODO: upper\n";
        let mut hits = Vec::new();
        scan_file("foo.rs", src, rust, &mut hits);
        assert_eq!(hits.len(), 3);
        // All three are recorded as the same marker bucket.
        for h in &hits {
            assert_eq!(h.marker, "TODO");
        }
    }

    #[test]
    fn scan_file_picks_up_hash_comment_style() {
        // Python/Ruby/Bash all use `#` — the language info drives the
        // comment detection, not the marker regex.
        let py = detect_language_info("foo.py", None).unwrap();
        let src = "x = 1  # TODO: follow up\n";
        let mut hits = Vec::new();
        scan_file("foo.py", src, py, &mut hits);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].marker, "TODO");
    }

    #[test]
    fn scan_file_picks_up_block_comment() {
        let rust = detect_language_info("foo.rs", None).unwrap();
        let src = "/*\n * FIXME: broken on arm64\n */\nfn f() {}\n";
        let mut hits = Vec::new();
        scan_file("foo.rs", src, rust, &mut hits);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].marker, "FIXME");
    }

    #[test]
    fn scan_file_picks_up_sql_style_dash_comment() {
        let sql = detect_language_info("q.sql", None).unwrap();
        let src = "SELECT 1; -- HACK: revisit index strategy\n";
        let mut hits = Vec::new();
        scan_file("q.sql", src, sql, &mut hits);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].marker, "HACK");
    }

    #[test]
    fn scan_file_handles_multiple_marker_kinds() {
        let rust = detect_language_info("foo.rs", None).unwrap();
        let src = "// TODO: a\n// FIXME: b\n// HACK: c\n// XXX: d\n// NOTE: ignored\n";
        let mut hits = Vec::new();
        scan_file("foo.rs", src, rust, &mut hits);
        assert_eq!(hits.len(), 4, "NOTE is intentionally excluded");
        let kinds: Vec<&str> = hits.iter().map(|h| h.marker.as_str()).collect();
        assert_eq!(kinds, vec!["TODO", "FIXME", "HACK", "XXX"]);
    }
}
