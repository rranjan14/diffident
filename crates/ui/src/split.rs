//! Side-by-side rendering (§8), without disturbing the row model.
//!
//! The whole app rests on one invariant: the flat `Vec<Row>` is index-parallel
//! with the rendered list, with `highlights`, with the cursor, and with every
//! anchor computed by `place()` and `scope_for_line` (§3). A split view wants
//! *fewer* rows than a unified one — three removals opposite two additions is
//! three rows, not five — and rebuilding the vector to get them would break
//! every one of those at once.
//!
//! So the vector is left alone and a plan is computed alongside it, one entry
//! per row. A pair is drawn at the removal's row; the addition's row is marked
//! absorbed and draws nothing. The count never changes, so nothing downstream
//! notices.

use diffident_diff::{DiffFile, LineKind, Row};

/// What to draw at one row index, in split mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cell {
    /// Not a code line — a file header, hunk header, expander or spacer. Draws
    /// across both columns, as it does in unified.
    Full,
    /// A code line. Either side may be absent, which is what an unpaired
    /// removal or addition looks like.
    Line {
        left: Option<usize>,
        right: Option<usize>,
    },
    /// This row's text is drawn on an earlier row's right column. It still
    /// exists — the cursor can sit on it, `place()` can anchor to it — it just
    /// has nothing of its own to show.
    Absorbed,
}

/// Plan the split view: one `Cell` per row, in row order.
///
/// Removals and additions pair up in the order they appear within a run, which
/// is what `diff` itself means by them: the nth line replaced by the nth line.
/// A run with more of one than the other leaves the surplus unpaired, opposite
/// a blank.
pub fn plan(files: &[DiffFile], rows: &[Row]) -> Vec<Cell> {
    let mut cells = vec![Cell::Full; rows.len()];

    // Walk runs of consecutive changed lines. A run ends at a context line, at
    // any non-line row, or at the end.
    let mut ix = 0;
    while ix < rows.len() {
        let Some(line) = rows[ix].line(files) else {
            ix += 1;
            continue;
        };
        if line.kind == LineKind::Context {
            cells[ix] = Cell::Line {
                left: Some(ix),
                right: Some(ix),
            };
            ix += 1;
            continue;
        }

        // Collect this run's removals then additions. Unified diffs always emit
        // them in that order within a hunk.
        let mut removed = Vec::new();
        let mut added = Vec::new();
        let mut end = ix;
        while end < rows.len() {
            let Some(l) = rows[end].line(files) else { break };
            match l.kind {
                LineKind::Removed => removed.push(end),
                LineKind::Added => added.push(end),
                LineKind::Context => break,
            }
            end += 1;
        }

        for slot in 0..removed.len().max(added.len()) {
            let (l, r) = (removed.get(slot).copied(), added.get(slot).copied());
            // Draw the pair at whichever side exists first in the file, so a
            // run's rows stay in ascending order and the list never has to
            // draw a cell above the row it belongs to.
            let at = l.or(r).expect("a slot has at least one side");
            cells[at] = Cell::Line { left: l, right: r };
            // The other half draws nothing of its own.
            if let (Some(l), Some(r)) = (l, r) {
                cells[if at == l { r } else { l }] = Cell::Absorbed;
            }
        }
        ix = end;
    }
    cells
}

/// Whether the cursor should skip this row.
///
/// An absorbed row shows nothing, so landing on it with `j` looks like the key
/// stopped working. It is still a real row with a real anchor — only the
/// cursor avoids it, and only while the view is split.
pub fn is_absorbed(cells: &[Cell], row: usize) -> bool {
    matches!(cells.get(row), Some(Cell::Absorbed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use diffident_diff::{parser::parse, rows::build_rows};

    fn plan_of(diff: &str) -> (Vec<DiffFile>, Vec<Row>, Vec<Cell>) {
        let files = parse(diff);
        let rows = build_rows(&files);
        let cells = plan(&files, &rows);
        (files, rows, cells)
    }

    /// One removal opposite one addition — the common case.
    const ONE_FOR_ONE: &str = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,2 +1,2 @@\n ctx\n-old\n+new\n";

    #[test]
    fn the_plan_is_index_parallel_with_the_rows() {
        // The invariant this module exists to protect. A split view wants fewer
        // rows; rebuilding the vector to get them would break `highlights`, the
        // cursor and every anchor at once.
        let (_, rows, cells) = plan_of(ONE_FOR_ONE);
        assert_eq!(cells.len(), rows.len());
    }

    #[test]
    fn a_context_line_shows_on_both_sides() {
        let (files, rows, cells) = plan_of(ONE_FOR_ONE);
        let ctx = rows
            .iter()
            .position(|r| r.line(&files).is_some_and(|l| l.kind == LineKind::Context))
            .unwrap();
        assert_eq!(
            cells[ctx],
            Cell::Line {
                left: Some(ctx),
                right: Some(ctx)
            }
        );
    }

    #[test]
    fn a_removal_and_its_addition_share_one_row() {
        let (files, rows, cells) = plan_of(ONE_FOR_ONE);
        let rm = rows
            .iter()
            .position(|r| r.line(&files).is_some_and(|l| l.kind == LineKind::Removed))
            .unwrap();
        let add = rows
            .iter()
            .position(|r| r.line(&files).is_some_and(|l| l.kind == LineKind::Added))
            .unwrap();
        assert_eq!(
            cells[rm],
            Cell::Line {
                left: Some(rm),
                right: Some(add)
            },
            "drawn together at the removal's row"
        );
        assert_eq!(cells[add], Cell::Absorbed, "and the addition draws nothing");
    }

    #[test]
    fn a_surplus_removal_sits_opposite_a_blank() {
        // Two removals, one addition: the second removal has nothing to pair
        // with, and must still be shown.
        let (files, rows, cells) = plan_of(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,2 +1,1 @@\n-one\n-two\n+won\n",
        );
        let removed: Vec<usize> = rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.line(&files).is_some_and(|l| l.kind == LineKind::Removed))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(removed.len(), 2);
        assert!(matches!(cells[removed[0]], Cell::Line { right: Some(_), .. }));
        assert_eq!(
            cells[removed[1]],
            Cell::Line {
                left: Some(removed[1]),
                right: None
            },
            "nothing replaced it"
        );
    }

    #[test]
    fn a_surplus_addition_sits_opposite_a_blank() {
        let (files, rows, cells) = plan_of(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,1 +1,2 @@\n-one\n+won\n+two\n",
        );
        let added: Vec<usize> = rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.line(&files).is_some_and(|l| l.kind == LineKind::Added))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(added.len(), 2);
        assert_eq!(
            cells[added[1]],
            Cell::Line {
                left: None,
                right: Some(added[1])
            },
            "added with nothing removed for it"
        );
    }

    #[test]
    fn a_pure_addition_never_absorbs_anything() {
        // A new file has no removals at all; every line must still be drawn.
        let (files, rows, cells) = plan_of(
            "diff --git a/a.rs b/a.rs\n--- /dev/null\n+++ b/a.rs\n@@ -0,0 +1,3 @@\n+a\n+b\n+c\n",
        );
        let lines = rows.iter().filter(|r| r.line(&files).is_some()).count();
        assert_eq!(lines, 3);
        assert_eq!(
            cells.iter().filter(|c| matches!(c, Cell::Absorbed)).count(),
            0,
            "nothing to absorb into"
        );
    }

    #[test]
    fn headers_and_expanders_stay_full_width() {
        let (_, rows, cells) = plan_of(ONE_FOR_ONE);
        let header = rows
            .iter()
            .position(|r| matches!(r, Row::FileHeader { .. }))
            .unwrap();
        assert_eq!(cells[header], Cell::Full);
    }

    #[test]
    fn every_line_is_drawn_exactly_once() {
        // The property that matters most: a split view that loses a line is
        // worse than no split view, because the reviewer approves what they
        // did not see.
        let (files, rows, cells) = plan_of(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,5 +1,5 @@\n ctx\n-one\n-two\n-three\n+won\n+too\n k\n",
        );
        let mut drawn: Vec<usize> = Vec::new();
        for cell in &cells {
            if let Cell::Line { left, right } = cell {
                drawn.extend(left);
                drawn.extend(right);
            }
        }
        drawn.sort_unstable();
        drawn.dedup();
        let all_lines: Vec<usize> = rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.line(&files).is_some())
            .map(|(i, _)| i)
            .collect();
        assert_eq!(drawn, all_lines, "every line appears in exactly one cell");
    }

    #[test]
    fn an_empty_diff_plans_nothing() {
        assert!(plan(&[], &[]).is_empty());
    }

    #[test]
    fn absorbed_rows_are_the_ones_the_cursor_skips() {
        let (_, _, cells) = plan_of(ONE_FOR_ONE);
        let absorbed = cells
            .iter()
            .position(|c| matches!(c, Cell::Absorbed))
            .unwrap();
        assert!(is_absorbed(&cells, absorbed));
        assert!(!is_absorbed(&cells, 0));
        assert!(!is_absorbed(&cells, 9999), "past the end is not absorbed");
    }
}
