//! Language detection and classification.
//!
//! Language metadata is generated from `languages.json5` at build time
//! (see `build.rs`). The dataset is MIT-licensed, originally from codestats
//! (<https://github.com/trypsynth/codestats>).
//!
//! ## Detection pipeline
//!
//! 1. Glob the filename against the 460+ language patterns.
//! 2. If exactly one language matches → done.
//! 3. If multiple candidates → try shebang sniffing, then keyword/comment
//!    scoring on the file content.
//! 4. If no candidates but content is available → try shebang alone.

mod data;
mod detection;

pub use data::{LANGUAGES, Language};
pub use detection::{detect_language_info, scoring};
