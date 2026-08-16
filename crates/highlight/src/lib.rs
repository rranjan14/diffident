//! Syntax highlighting as plain data.
//!
//! Deliberately gpui-free: this returns packed RGB integers, and the UI converts
//! them to `HighlightStyle` at the view boundary. That keeps `syntect` — which
//! loads a multi-megabyte syntax dump — out of the UI crate's test build.

pub mod syntax;

use std::ops::Range;

/// Byte ranges into one line of source, each with a packed `0x00RRGGBB` colour.
///
/// Ranges are sorted, non-overlapping, and land on char boundaries — exactly
/// what `StyledText::with_default_highlights` requires. They are relative to
/// the start of *that line*, not the file.
pub type Highlights = Vec<(Range<usize>, u32)>;
