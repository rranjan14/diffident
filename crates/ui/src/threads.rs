//! Putting other people's review threads onto our row model (§7).

use diffident_diff::{DiffFile, LineKind, Row};
use diffident_forge::threads::ReviewThread;

/// A thread, and where it landed.
#[derive(Debug, Clone, PartialEq)]
pub struct Placed<'a> {
    pub thread: &'a ReviewThread,
    /// Index of the diff row this thread sits under, or `None` when the code it
    /// referred to is not in this diff.
    pub row: Option<usize>,
}

impl Placed<'_> {
    /// Whether this thread has somewhere to render inline.
    pub fn is_anchored(&self) -> bool {
        self.row.is_some()
    }
}

/// Place every thread against the current diff.
///
/// Order is preserved, and every input thread comes back — an unplaceable one
/// is returned with `row: None` rather than dropped. A thread silently missing
/// from the UI is worse than one shown out of place: the reviewer would answer
/// a question they never saw.
///
/// **The result is index-parallel with `threads`, and that is load-bearing**
/// (§3's rows/highlights parity, in miniature): the pane highlights `placed[i]`
/// while `space` and `a` act on `threads[i]` through [`selected`]. Sorting here
/// — as [`by_row`] does, one function below — would point the highlight at one
/// conversation and resolve or reply to another. `placement_is_index_parallel_with_its_input`
/// is the guard.
pub fn place<'a>(
    threads: &'a [ReviewThread],
    files: &[DiffFile],
    rows: &[Row],
) -> Vec<Placed<'a>> {
    threads
        .iter()
        .map(|thread| Placed {
            thread,
            row: row_for(thread, files, rows),
        })
        .collect()
}

/// The row index a thread anchors to, if the diff still contains its line.
fn row_for(thread: &ReviewThread, files: &[DiffFile], rows: &[Row]) -> Option<usize> {
    // An outdated thread's only anchor is where it was originally left, which
    // by definition is not in this diff. Trying to place it would land it on
    // whatever line happens to carry that number now — a different piece of
    // code entirely.
    if thread.is_outdated {
        return None;
    }
    let line = thread.line?;
    rows.iter().position(|row| {
        let Row::Line {
            file_ix,
            hunk_ix,
            line_ix,
        } = *row
        else {
            return false;
        };
        let Some(file) = files.get(file_ix) else {
            return false;
        };
        if file.display_path() != thread.path {
            return false;
        }
        let Some(diff_line) = file.hunks.get(hunk_ix).and_then(|h| h.lines.get(line_ix)) else {
            return false;
        };
        if thread.on_old_side {
            diff_line.kind != LineKind::Added && diff_line.old_lineno == Some(line)
        } else {
            diff_line.kind != LineKind::Removed && diff_line.new_lineno == Some(line)
        }
    })
}

/// Threads grouped by the row they sit under, for a renderer walking rows.
///
/// Returns pairs rather than a map so the caller can iterate in row order
/// without sorting, which is what a virtualised list wants.
pub fn by_row<'a>(placed: &[Placed<'a>]) -> Vec<(usize, Vec<&'a ReviewThread>)> {
    let mut out: Vec<(usize, Vec<&'a ReviewThread>)> = Vec::new();
    for p in placed {
        let Some(row) = p.row else { continue };
        match out.iter_mut().find(|(r, _)| *r == row) {
            Some((_, list)) => list.push(p.thread),
            None => out.push((row, vec![p.thread])),
        }
    }
    out.sort_by_key(|(row, _)| *row);
    out
}

/// How many threads could not be placed — the count the UI needs to tell the
/// reviewer that conversations exist which it cannot show beside the code.
pub fn unanchored(placed: &[Placed<'_>]) -> usize {
    placed.iter().filter(|p| !p.is_anchored()).count()
}
/// The thread `delta` steps away, wrapping, with a stale cursor clamped first.
///
/// Wrapping rather than clamping is deliberate and differs from diff
/// navigation: this is a short selector list, not a 10,000-row document.
pub fn step(count: usize, cursor: usize, delta: isize) -> usize {
    if count == 0 {
        return 0;
    }
    let here = cursor.min(count - 1) as isize;
    (here + delta).rem_euclid(count as isize) as usize
}

/// The thread under the cursor, clamped.
///
/// Clamped rather than bounds-checked because threads can vanish under the
/// cursor when a review reloads, and doing nothing at all in that case reads
/// as a broken key.
pub fn selected(threads: &[ReviewThread], cursor: usize) -> Option<&ReviewThread> {
    threads.get(cursor.min(threads.len().saturating_sub(1)))
}

/// Threads grouped by the diff row they render under, as owned copies.
///
/// `by_row` returns borrows, which cannot outlive `Workspace`'s map; the view
/// is a separate entity and needs its own copies. Threads number in the tens,
/// so cloning them on every sync is not worth avoiding.
pub fn inline_groups(
    threads: &[ReviewThread],
    files: &[DiffFile],
    rows: &[Row],
) -> Vec<(usize, Vec<ReviewThread>)> {
    by_row(&place(threads, files, rows))
        .into_iter()
        .map(|(row, ts)| (row, ts.into_iter().cloned().collect()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use diffident_diff::{parser::parse, rows::build_rows};
    use diffident_forge::threads::{ReviewThread, ThreadComment};

    // a.rs: line 1 context, old line 2 removed, new line 2 added.
    const DIFF: &str = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,2 +1,2 @@\n ctx\n-old\n+new\n";

    fn thread(path: &str, line: Option<u32>, old_side: bool) -> ReviewThread {
        ReviewThread {
            id: "PRRT_1".into(),
            path: path.into(),
            line,
            original_line: line,
            on_old_side: old_side,
            is_resolved: false,
            is_outdated: false,
            comments: vec![ThreadComment {
                id: "PRRC_1".into(),
                author: "octocat".into(),
                body: "nit".into(),
            }],
        }
    }

    fn fixture() -> (Vec<DiffFile>, Vec<Row>) {
        let files = parse(DIFF);
        let rows = build_rows(&files);
        (files, rows)
    }

    #[test]
    fn a_thread_on_an_added_line_lands_on_that_row() {
        let (files, rows) = fixture();
        let t = [thread("a.rs", Some(2), false)];
        let placed = place(&t, &files, &rows);
        let row = placed[0].row.expect("should anchor");
        assert!(matches!(rows[row], Row::Line { .. }));
        assert!(placed[0].is_anchored());
    }

    #[test]
    fn a_thread_on_the_old_side_lands_on_the_removed_line() {
        // Old line 2 was deleted; a thread left on it still belongs there.
        let (files, rows) = fixture();
        let t = [thread("a.rs", Some(2), true)];
        let placed = place(&t, &files, &rows);
        let row = placed[0].row.expect("should anchor to the pre-image line");
        let Row::Line { file_ix, hunk_ix, line_ix } = rows[row] else {
            panic!("expected a line row");
        };
        assert_eq!(files[file_ix].hunks[hunk_ix].lines[line_ix].kind, LineKind::Removed);
    }

    #[test]
    fn a_thread_on_a_line_not_in_the_diff_is_returned_unplaced_not_dropped() {
        // Dropping it would mean the reviewer answers a question they never saw.
        let (files, rows) = fixture();
        let t = [thread("a.rs", Some(999), false)];
        let placed = place(&t, &files, &rows);
        assert_eq!(placed.len(), 1);
        assert_eq!(placed[0].row, None);
        assert_eq!(unanchored(&placed), 1);
    }

    #[test]
    fn a_thread_on_a_file_not_in_the_diff_is_unplaced() {
        let (files, rows) = fixture();
        let t = [thread("elsewhere.rs", Some(1), false)];
        assert_eq!(place(&t, &files, &rows)[0].row, None);
    }

    #[test]
    fn an_outdated_thread_is_never_placed_even_when_the_number_still_exists() {
        // Its only anchor is where it *was*; that line number now carries
        // different code, and putting the thread there would misattribute it.
        let (files, rows) = fixture();
        let mut t = thread("a.rs", None, false);
        t.original_line = Some(1);
        t.is_outdated = true;
        let threads = vec![t];
        let placed = place(&threads, &files, &rows);
        assert_eq!(placed[0].row, None);
    }

    #[test]
    fn the_old_and_new_sides_of_one_line_number_are_different_places() {
        let (files, rows) = fixture();
        let new_t = vec![thread("a.rs", Some(2), false)];
        let old_t = vec![thread("a.rs", Some(2), true)];
        let new_side = place(&new_t, &files, &rows)[0].row;
        let old_side = place(&old_t, &files, &rows)[0].row;
        assert_ne!(new_side, old_side, "side must disambiguate the anchor");
    }

    #[test]
    fn every_thread_comes_back_in_the_order_it_arrived() {
        let (files, rows) = fixture();
        let t = [
            thread("a.rs", Some(999), false),
            thread("a.rs", Some(2), false),
            thread("nope.rs", Some(1), false),
        ];
        let placed = place(&t, &files, &rows);
        assert_eq!(placed.len(), 3);
        assert_eq!(unanchored(&placed), 2);
    }

    #[test]
    fn placement_is_index_parallel_with_its_input() {
        // Load-bearing since Phase 7c: the pane highlights `placed[i]` while
        // `space` and `a` act on `threads[i]` via `selected`. Sorting or
        // filtering here would point the highlight at one conversation and
        // resolve or reply to a different one — silently, on someone else's
        // pull request. `by_row` immediately below *does* sort, so this is one
        // plausible edit away rather than a hypothetical.
        let (files, rows) = fixture();
        let t = [
            // Unanchored first: any row-order sort would move it last.
            thread("a.rs", Some(999), false),
            thread("a.rs", Some(2), false),
            thread("a.rs", Some(1), false),
        ];
        let placed = place(&t, &files, &rows);
        assert_eq!(placed.len(), t.len(), "every thread comes back");
        for (ix, p) in placed.iter().enumerate() {
            assert!(
                std::ptr::eq(p.thread, &t[ix]),
                "placed[{ix}] is not threads[{ix}] — the cursor now lies"
            );
        }
    }

    #[test]
    fn several_threads_on_one_line_are_grouped_under_it() {
        let (files, rows) = fixture();
        let t = [thread("a.rs", Some(2), false), thread("a.rs", Some(2), false)];
        let grouped = by_row(&place(&t, &files, &rows));
        assert_eq!(grouped.len(), 1, "one row");
        assert_eq!(grouped[0].1.len(), 2, "both threads under it");
    }

    #[test]
    fn grouping_comes_back_in_row_order() {
        // A renderer walking rows top to bottom must not have to sort.
        let (files, rows) = fixture();
        let t = [thread("a.rs", Some(2), false), thread("a.rs", Some(1), false)];
        let grouped = by_row(&place(&t, &files, &rows));
        assert!(grouped.windows(2).all(|w| w[0].0 < w[1].0));
    }

    #[test]
    fn unplaced_threads_are_left_out_of_the_grouping() {
        let (files, rows) = fixture();
        let t = [thread("a.rs", Some(999), false)];
        assert!(by_row(&place(&t, &files, &rows)).is_empty());
    }

    #[test]
    fn stepping_moves_one_thread_at_a_time() {
        assert_eq!(step(3, 0, 1), 1);
        assert_eq!(step(3, 1, -1), 0);
    }

    #[test]
    fn stepping_wraps_at_both_ends() {
        // Unlike diff navigation, which clamps: a 400-file diff makes wrapping
        // disorienting, but a list of three threads is a selector, and a
        // selector that stops dead at the end feels broken.
        assert_eq!(step(3, 2, 1), 0);
        assert_eq!(step(3, 0, -1), 2);
    }

    #[test]
    fn stepping_over_no_threads_stays_at_zero_rather_than_dividing_by_zero() {
        assert_eq!(step(0, 0, 1), 0);
        assert_eq!(step(0, 5, -1), 0);
    }

    #[test]
    fn a_cursor_past_the_end_is_clamped_rather_than_wrapped_when_stepping() {
        // Threads can disappear under the cursor on reload. Wrapping a stale
        // cursor would land somewhere unrelated; clamping first keeps the next
        // press predictable.
        assert_eq!(step(3, 99, 1), 0, "clamp to 2, then step to 0");
    }

    #[test]
    fn the_selected_thread_is_the_one_under_the_cursor() {
        let t = [thread("a.rs", Some(1), false), thread("b.rs", Some(2), false)];
        assert_eq!(selected(&t, 1).map(|t| t.path.as_str()), Some("b.rs"));
    }

    #[test]
    fn a_stale_cursor_selects_the_last_thread_rather_than_nothing() {
        let t = [thread("a.rs", Some(1), false)];
        assert_eq!(selected(&t, 99).map(|t| t.path.as_str()), Some("a.rs"));
    }

    #[test]
    fn nothing_is_selected_when_there_are_no_threads() {
        assert!(selected(&[], 0).is_none());
    }

    #[test]
    fn no_threads_places_nothing_and_reports_nothing_unplaced() {
        let (files, rows) = fixture();
        let placed = place(&[], &files, &rows);
        assert!(placed.is_empty());
        assert_eq!(unanchored(&placed), 0);
    }

    #[test]
    fn inline_groups_are_owned_copies_keyed_by_row() {
        let (files, rows) = fixture();
        let t = [thread("a.rs", Some(2), false)];
        let groups = inline_groups(&t, &files, &rows);
        assert_eq!(groups.len(), 1);
        let (row, threads) = &groups[0];
        assert!(matches!(rows[*row], Row::Line { .. }));
        assert_eq!(threads[0].path, "a.rs");
    }

    #[test]
    fn several_threads_on_one_line_arrive_as_one_group() {
        let (files, rows) = fixture();
        let t = [thread("a.rs", Some(2), false), thread("a.rs", Some(2), false)];
        let groups = inline_groups(&t, &files, &rows);
        assert_eq!(groups.len(), 1, "one row");
        assert_eq!(groups[0].1.len(), 2, "both threads under it");
    }

    #[test]
    fn groups_come_back_in_row_order_so_the_renderer_need_not_sort() {
        let (files, rows) = fixture();
        let t = [thread("a.rs", Some(2), false), thread("a.rs", Some(1), false)];
        let groups = inline_groups(&t, &files, &rows);
        assert!(groups.windows(2).all(|w| w[0].0 < w[1].0));
    }

    #[test]
    fn a_thread_with_no_line_in_this_diff_is_in_no_group() {
        // It has nowhere to render inline. The right-hand pane keeps it —
        // dropping it entirely would hide a conversation the reviewer is
        // expected to answer.
        let (files, rows) = fixture();
        let t = [thread("a.rs", Some(999), false)];
        assert!(inline_groups(&t, &files, &rows).is_empty());
    }
}
