//! Source-file analysis primitives: line classification, encoding heuristics.
//!
//! Imported from codestats (MIT) and adapted. These are language-aware helpers
//! (via [`crate::langs`]) for collectors that need real code/comment/blank
//! breakdowns rather than raw line counts.

pub mod line_classifier;
