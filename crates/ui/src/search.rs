//! Finding text in the diff (§8: `/`, `n`, `N`).
//!
//! Pure: rows and a needle in, positions out. No window, no state, no cursor —
//! which is what lets the whole of matching be tested without a app, and what
//! keeps the mode machinery in `workspace.rs` down to "where am I in this list".

use diffident_diff::{DiffFile, Row};

/// One occurrence, as a row and a byte range within that row's text.
///
/// Byte ranges rather than char indices because that is what
/// `StyledText::with_default_highlights` takes (§3), and converting twice is
/// two chances to land mid-character.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Match {
    pub row: usize,
    pub start: usize,
    pub end: usize,
}

/// Whether this needle should be matched case-sensitively.
///
/// Smart case, the `vim` and `ripgrep` rule: an all-lowercase needle matches
/// anything, and the moment you type a capital you meant it. It is the only
/// scheme where the common search needs no flag and the precise one needs no
/// setting.
fn is_case_sensitive(needle: &str) -> bool {
    needle.chars().any(char::is_uppercase)
}

/// Every occurrence of `needle` in the diff's *code* lines, in row order.
///
/// Only line rows are searched. File headers, hunk headers and expanders carry
/// text, but none of it is the code under review — matching `@@` in every hunk
/// header would bury the hits that matter.
///
/// An empty needle matches nothing rather than everything: `/` followed by
/// `enter` is a cancelled search, not a request to highlight the file.
pub fn find(files: &[DiffFile], rows: &[Row], needle: &str) -> Vec<Match> {
    if needle.is_empty() {
        return Vec::new();
    }
    let sensitive = is_case_sensitive(needle);
    let folded_needle = needle.to_lowercase();

    let mut out = Vec::new();
    for (row, r) in rows.iter().enumerate() {
        let Some(line) = r.line(files) else { continue };
        // Fold the haystack only when we are ignoring case. `to_lowercase` can
        // change a string's byte length, so the folded copy is only safe to
        // take offsets from when the needle was folded the same way — which is
        // exactly the insensitive branch.
        let (hay, pat) = if sensitive {
            (line.text.clone(), needle.to_string())
        } else {
            (line.text.to_lowercase(), folded_needle.clone())
        };
        let mut from = 0;
        while let Some(at) = hay[from..].find(&pat) {
            let start = from + at;
            out.push(Match {
                row,
                start,
                end: start + pat.len(),
            });
            // Advance past this hit, never by zero — an empty pattern would
            // otherwise spin here forever, and `find` is called on every
            // keystroke.
            from = start + pat.len().max(1);
            if from >= hay.len() {
                break;
            }
        }
    }
    out
}

/// The index of the next match after `from_row`, wrapping.
///
/// Wrapping rather than clamping, unlike `}` and `{` in `navigate.rs`: a search
/// that stops dead at the last hit reads as "no more matches", which is a
/// different and wrong statement when there are three above you.
///
/// Strictly *after* `from_row` going forward, and strictly before going back,
/// so pressing `n` on a row that already matches moves rather than sitting
/// still.
pub fn step(matches: &[Match], from_row: usize, forward: bool) -> Option<usize> {
    if matches.is_empty() {
        return None;
    }
    if forward {
        matches
            .iter()
            .position(|m| m.row > from_row)
            .or(Some(0))
    } else {
        matches
            .iter()
            .rposition(|m| m.row < from_row)
            .or(Some(matches.len() - 1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use diffident_diff::{parser::parse, rows::build_rows};

    // a.rs: " ctx", "-old Thing", "+new thing"
    const DIFF: &str = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,2 +1,2 @@\n ctx\n-old Thing\n+new thing\n";

    fn fixture() -> (Vec<DiffFile>, Vec<Row>) {
        let files = parse(DIFF);
        let rows = build_rows(&files);
        (files, rows)
    }

    #[test]
    fn a_lowercase_needle_matches_either_case() {
        // Smart case: you did not ask for precision, so you get reach.
        let (files, rows) = fixture();
        assert_eq!(find(&files, &rows, "thing").len(), 2);
    }

    #[test]
    fn a_capital_anywhere_means_you_meant_it() {
        let (files, rows) = fixture();
        let m = find(&files, &rows, "Thing");
        assert_eq!(m.len(), 1, "only the capitalised one");
    }

    #[test]
    fn the_offsets_are_byte_ranges_into_the_line_text() {
        // They are handed straight to `with_default_highlights`, which takes
        // byte ranges on char boundaries (§3).
        let (files, rows) = fixture();
        let m = find(&files, &rows, "Thing")[0];
        let line = rows[m.row].line(&files).unwrap();
        assert_eq!(&line.text[m.start..m.end], "Thing");
    }

    #[test]
    fn only_code_lines_are_searched() {
        // The hunk header contains "@@" and the file header contains "a.rs";
        // matching either would bury the hits that matter.
        let (files, rows) = fixture();
        assert!(find(&files, &rows, "@@").is_empty());
        assert!(find(&files, &rows, "a.rs").is_empty());
    }

    #[test]
    fn an_empty_needle_matches_nothing_rather_than_everything() {
        // `/` then `enter` is a cancelled search, not a request to light up
        // the whole file.
        let (files, rows) = fixture();
        assert!(find(&files, &rows, "").is_empty());
    }

    #[test]
    fn repeated_hits_on_one_line_are_all_found() {
        let files = parse("diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,1 +1,1 @@\n+aa aa aa\n");
        let rows = build_rows(&files);
        assert_eq!(find(&files, &rows, "aa").len(), 3);
    }

    #[test]
    fn a_multibyte_line_yields_ranges_that_slice_cleanly() {
        // Slicing a byte range that lands mid-character panics, and a diff is
        // full of other people's languages.
        let files = parse("diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,1 +1,1 @@\n+héllo wörld héllo\n");
        let rows = build_rows(&files);
        let ms = find(&files, &rows, "héllo");
        assert_eq!(ms.len(), 2);
        for m in ms {
            let line = rows[m.row].line(&files).unwrap();
            assert_eq!(&line.text[m.start..m.end], "héllo");
        }
    }

    #[test]
    fn stepping_forward_goes_to_the_next_match_below_the_cursor() {
        let (files, rows) = fixture();
        let m = find(&files, &rows, "thing");
        let first = m[0].row;
        assert_eq!(step(&m, 0, true), Some(0));
        assert_eq!(step(&m, first, true), Some(1), "strictly after, so `n` moves");
    }

    #[test]
    fn stepping_wraps_at_both_ends() {
        // Stopping dead at the last hit reads as "no more matches", which is a
        // different and wrong statement when there are three above you.
        let (files, rows) = fixture();
        let m = find(&files, &rows, "thing");
        assert_eq!(step(&m, 9999, true), Some(0), "past the end wraps to the first");
        assert_eq!(step(&m, 0, false), Some(m.len() - 1), "before the start wraps to the last");
    }

    #[test]
    fn stepping_with_no_matches_reports_none_rather_than_moving() {
        assert_eq!(step(&[], 0, true), None);
        assert_eq!(step(&[], 0, false), None);
    }
}

/// One run of a line, after matches have been laid over syntax colours.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub range: std::ops::Range<usize>,
    /// The syntax colour underneath, if the run had one.
    pub color: Option<u32>,
    pub is_match: bool,
}

/// Lay match ranges over syntax ranges, keeping the result sorted and
/// non-overlapping.
///
/// `StyledText::with_default_highlights` requires exactly that (§3), so the two
/// sets cannot simply be concatenated — a match inside a keyword overlaps it.
/// Matches win and split the syntax run they land in, because the reason you
/// searched is to find *this*, not to admire the colouring of it.
///
/// Returns only the runs that need styling; the gaps between them inherit the
/// default style, which is what the renderer already does for unhighlighted
/// text.
pub fn overlay(syntax: &[(std::ops::Range<usize>, u32)], matches: &[Match]) -> Vec<Span> {
    if matches.is_empty() {
        return syntax
            .iter()
            .map(|(r, c)| Span {
                range: r.clone(),
                color: Some(*c),
                is_match: false,
            })
            .collect();
    }

    let mut cuts: Vec<usize> = Vec::new();
    for (r, _) in syntax {
        cuts.push(r.start);
        cuts.push(r.end);
    }
    for m in matches {
        cuts.push(m.start);
        cuts.push(m.end);
    }
    cuts.sort_unstable();
    cuts.dedup();

    let colour_at = |p: usize| syntax.iter().find(|(r, _)| r.contains(&p)).map(|(_, c)| *c);
    let matched_at = |p: usize| matches.iter().any(|m| m.start <= p && p < m.end);

    let mut out: Vec<Span> = Vec::new();
    for pair in cuts.windows(2) {
        let (start, end) = (pair[0], pair[1]);
        if start == end {
            continue;
        }
        let (color, is_match) = (colour_at(start), matched_at(start));
        if color.is_none() && !is_match {
            continue; // a plain gap needs no run at all
        }
        // Merge with the previous run when nothing about the style changed —
        // fewer, longer runs are cheaper to lay out and easier to read in a
        // test failure.
        match out.last_mut() {
            Some(prev) if prev.range.end == start && prev.color == color && prev.is_match == is_match => {
                prev.range.end = end;
            }
            _ => out.push(Span { range: start..end, color, is_match }),
        }
    }
    out
}

#[cfg(test)]
mod overlay_tests {
    use super::*;

    fn m(start: usize, end: usize) -> Match {
        Match { row: 0, start, end }
    }

    #[test]
    fn with_no_matches_the_syntax_passes_straight_through() {
        let syn = [(0..4, 0xff0000u32), (5..9, 0x00ff00)];
        let out = overlay(&syn, &[]);
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|s| !s.is_match));
    }

    #[test]
    fn a_match_inside_a_keyword_splits_it_into_three() {
        // The case concatenation gets wrong: the ranges would overlap, which
        // `with_default_highlights` forbids.
        let syn = [(0..10, 0xff0000u32)];
        let out = overlay(&syn, &[m(3, 6)]);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].range, 0..3);
        assert_eq!(out[1].range, 3..6);
        assert!(out[1].is_match, "the middle is the hit");
        assert_eq!(out[1].color, Some(0xff0000), "and it keeps the colour underneath");
        assert_eq!(out[2].range, 6..10);
    }

    #[test]
    fn the_result_is_always_sorted_and_non_overlapping() {
        // The invariant `with_default_highlights` requires (§3).
        let syn = [(2..8, 0xff0000u32), (10..14, 0x00ff00)];
        let out = overlay(&syn, &[m(0, 4), m(12, 20)]);
        for w in out.windows(2) {
            assert!(w[0].range.end <= w[1].range.start, "{:?}", out);
        }
    }

    #[test]
    fn a_match_outside_every_syntax_run_still_gets_a_span() {
        let out = overlay(&[], &[m(3, 6)]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].range, 3..6);
        assert_eq!(out[0].color, None);
        assert!(out[0].is_match);
    }

    #[test]
    fn plain_gaps_produce_no_runs() {
        // Unstyled text needs no entry; the renderer already falls back to the
        // default style for anything not listed.
        let out = overlay(&[(0..2, 1u32), (8..10, 1)], &[]);
        assert_eq!(out.len(), 2, "nothing for the gap between them");
    }

    #[test]
    fn splitting_does_not_leave_abutting_identical_runs_behind() {
        // Cutting at every boundary would turn one syntax run into three
        // wherever a *neighbouring* run's edge falls inside it. Merging on the
        // way out keeps the list as short as the styling actually requires.
        //
        // The no-match path is a deliberate pass-through and does not merge:
        // syntect rarely emits abutting identical runs, and folding them on
        // every render of every row would cost more than it saves.
        let out = overlay(&[(0..10, 7u32)], &[m(20, 22)]);
        assert_eq!(
            out.iter().filter(|s| !s.is_match).count(),
            1,
            "the untouched run stays whole: {out:?}"
        );
    }
}
