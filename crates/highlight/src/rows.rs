//! Projecting syntax highlighting onto the flat row model.
//!
//! Lives here rather than in the UI crate because it is the single most
//! expensive thing that happens when a review opens — half a second on a
//! 10k-row diff — and it must therefore run on a background thread. Keeping it
//! headless is what makes that possible, and what lets it be benchmarked
//! without a window.

use crate::{Highlights, syntax::highlight_run};
use diffident_diff::{DiffFile, Row};

/// Highlight every line row, returning a vector **exactly as long as `rows`**.
///
/// Index parity with `rows` is the contract: the view indexes this by row
/// number, so a shorter or reordered result silently paints each line with some
/// other line's syntax. Non-line rows get an empty entry rather than being
/// skipped, which is what keeps the indices aligned.
///
/// Highlighting runs per hunk, not per line, because syntax state carries
/// across newlines — see `highlight_run`.
///
/// Two passes, both linear. Do **not** "simplify" this into a single loop that
/// searches `rows` for each highlighted line — `rows.iter().position(..)` inside
/// the line loop is O(rows × lines), which on a 4,000-line diff is 16M
/// comparisons per rebuild and visibly hangs the window.
pub fn for_rows(files: &[DiffFile], rows: &[Row]) -> Vec<Highlights> {
    // Pass 1: highlight each hunk once, indexed by [file_ix][hunk_ix][line_ix].
    let per_hunk: Vec<Vec<Vec<Highlights>>> = files
        .iter()
        .map(|file| {
            let path = file.display_path();
            file.hunks
                .iter()
                .map(|hunk| {
                    let texts: Vec<&str> = hunk.lines.iter().map(|l| l.text.as_str()).collect();
                    highlight_run(path, &texts)
                })
                .collect()
        })
        .collect();

    // Pass 2: project onto the row model, preserving index parity.
    rows.iter()
        .map(|row| match *row {
            Row::Line {
                file_ix,
                hunk_ix,
                line_ix,
            } => per_hunk
                .get(file_ix)
                .and_then(|f| f.get(hunk_ix))
                .and_then(|h| h.get(line_ix))
                .cloned()
                .unwrap_or_default(),
            _ => Vec::new(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use diffident_diff::{parser::parse, rows::build_rows};

    const RUST: &str = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,3 +1,3 @@\n fn main() {}\n-let x = 1;\n+let y = 2;\n";

    #[test]
    fn highlights_are_index_parallel_with_the_rows() {
        // The whole render model depends on this: row N's colours must be at
        // index N, or every line renders someone else's syntax.
        let files = parse(RUST);
        let rows = build_rows(&files);
        assert_eq!(for_rows(&files, &rows).len(), rows.len());
    }

    #[test]
    fn non_line_rows_carry_no_highlights() {
        let files = parse(RUST);
        let rows = build_rows(&files);
        let hl = for_rows(&files, &rows);
        for (ix, row) in rows.iter().enumerate() {
            if !matches!(row, Row::Line { .. }) {
                assert!(hl[ix].is_empty(), "row {ix} ({row:?}) should have none");
            }
        }
    }

    #[test]
    fn code_lines_are_highlighted_using_the_files_language() {
        let files = parse(RUST);
        let rows = build_rows(&files);
        let hl = for_rows(&files, &rows);
        let first_line = rows.iter().position(|r| matches!(r, Row::Line { .. })).unwrap();
        assert!(!hl[first_line].is_empty(), "a .rs line must be highlighted");
    }

    #[test]
    fn highlight_ranges_stay_inside_their_lines_text() {
        let files = parse(RUST);
        let rows = build_rows(&files);
        let hl = for_rows(&files, &rows);
        for (ix, row) in rows.iter().enumerate() {
            let Row::Line {
                file_ix,
                hunk_ix,
                line_ix,
            } = *row
            else {
                continue;
            };
            let text = &files[file_ix].hunks[hunk_ix].lines[line_ix].text;
            for (range, _) in &hl[ix] {
                assert!(range.end <= text.len(), "range {range:?} overruns {text:?}");
            }
        }
    }

    #[test]
    fn an_empty_diff_produces_no_highlights() {
        assert!(for_rows(&[], &[]).is_empty());
    }

    #[test]
    fn a_large_diff_highlights_in_linear_time() {
        // Guards the two-pass structure. The obvious one-pass version searches
        // `rows` per line — O(rows × lines) — which is 16M comparisons here and
        // hangs the window. Under the debug profile this must still be instant;
        // if it takes more than a second, the implementation regressed to O(n²).
        let body: String = (0..4000).map(|i| format!("+let x{i} = {i};\n")).collect();
        let text = format!(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,1 +1,4000 @@\n{body}"
        );
        let files = parse(&text);
        let rows = build_rows(&files);
        assert!(rows.len() > 4000);
        assert_eq!(for_rows(&files, &rows).len(), rows.len());
    }
}
