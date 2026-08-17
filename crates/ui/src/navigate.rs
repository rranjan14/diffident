use diffident_diff::Row;
use gpui::{KeyBinding, actions};

actions!(
    diffident,
    [
        NextLine,
        PrevLine,
        NextHunk,
        PrevHunk,
        NextFile,
        PrevFile,
        HalfPageDown,
        HalfPageUp,
        Top,
        Bottom,
        ToggleReviewed,
        NextUnreviewed,
        LineComment,
        FileComment,
        ReviewComment,
        ToggleVisual,
        DeleteDraft,
        ClearSelection,
        Submit,
        ToggleResolution,
        NextEvent,
        AdvanceSubmit,
        CancelSubmit,
        SendReview,
        NextReview,
        PrevReview,
        Refresh,
        NextThread,
        PrevThread,
        ToggleResolved,
    ]
);

/// The §8 keymap, restricted to what Phase 2 can service.
///
/// Comment actions (`c`, `C`, `v`, `dd`) are deliberately absent: an action
/// bound to a key but wired to nothing swallows the keystroke silently, which
/// is worse for the user than the key simply not working yet. They arrive in
/// Phase 5 with the comment model behind them.
///
/// Every action declared above must also have an `on_action` handler in
/// `Workspace::render`. The test below guards the binding half; the wiring half
/// is guarded in `workspace.rs`.
///
/// `Diff` scopes the navigation keys so they do not fire while the rail has
/// focus; the two review-switching keys are global on purpose.
pub fn key_bindings() -> Vec<KeyBinding> {
    vec![
        KeyBinding::new("j", NextLine, Some("Diff")),
        KeyBinding::new("k", PrevLine, Some("Diff")),
        KeyBinding::new("]", NextHunk, Some("Diff")),
        KeyBinding::new("[", PrevHunk, Some("Diff")),
        KeyBinding::new("}", NextFile, Some("Diff")),
        KeyBinding::new("{", PrevFile, Some("Diff")),
        KeyBinding::new("ctrl-d", HalfPageDown, Some("Diff")),
        KeyBinding::new("ctrl-u", HalfPageUp, Some("Diff")),
        KeyBinding::new("g", Top, Some("Diff")),
        KeyBinding::new("shift-g", Bottom, Some("Diff")),
        KeyBinding::new("r", ToggleReviewed, Some("Diff")),
        KeyBinding::new("tab", NextUnreviewed, Some("Diff")),
        KeyBinding::new("c", LineComment, Some("Diff")),
        KeyBinding::new("shift-c", FileComment, Some("Diff")),
        // §8 assigns no key for a review-level comment. `s` for "summary" is
        // free and is what GitHub calls the review body.
        KeyBinding::new("s", ReviewComment, Some("Diff")),
        KeyBinding::new("v", ToggleVisual, Some("Diff")),
        // §8 spells this `dd`. A chord needs its own pending-key state machine
        // and this is one keystroke either way, so it is `x`.
        KeyBinding::new("x", DeleteDraft, Some("Diff")),
        KeyBinding::new("escape", ClearSelection, Some("Diff")),
        KeyBinding::new("shift-s", Submit, Some("Diff")),
        // Only meaningful inside the resolver; scoped so they cannot fire while
        // the reviewer is reading a diff.
        KeyBinding::new("space", ToggleResolution, Some("Resolver")),
        KeyBinding::new("enter", AdvanceSubmit, Some("Resolver")),
        KeyBinding::new("escape", CancelSubmit, Some("Resolver")),
        KeyBinding::new("escape", CancelSubmit, Some("Confirm")),
        KeyBinding::new("cmd-enter", SendReview, Some("Confirm")),
        KeyBinding::new("tab", NextEvent, Some("Confirm")),
        KeyBinding::new("ctrl-tab", NextReview, None),
        KeyBinding::new("ctrl-shift-tab", PrevReview, None),
        // Scoped to Diff rather than global: an unscoped binding still fires
        // while the composer has focus, so typing a capital R would refresh.
        KeyBinding::new("shift-r", Refresh, Some("Diff")),
        // §8 assigns no keys for thread navigation — Phase 7 is net-new (§7).
        // `t` for thread; both are free in the Diff context.
        KeyBinding::new("t", NextThread, Some("Diff")),
        KeyBinding::new("shift-t", PrevThread, Some("Diff")),
        // `space` is already the resolver's toggle key; in the Diff context it
        // is free, and "toggle the thing under the cursor" is the same gesture.
        KeyBinding::new("space", ToggleResolved, Some("Diff")),
    ]
}

/// Find the next/previous row matching `pred`, or stay put at either end.
///
/// Clamping rather than wrapping: a reviewer pressing `}` at the last file
/// expects nothing to happen, not to be thrown back to the top of a 400-file
/// diff with no indication anything moved.
fn seek(rows: &[Row], from: usize, forward: bool, pred: impl Fn(&Row) -> bool) -> usize {
    let found = if forward {
        rows.iter()
            .enumerate()
            .skip(from + 1)
            .find(|(_, r)| pred(r))
            .map(|(i, _)| i)
    } else {
        rows.iter()
            .enumerate()
            .take(from)
            .rfind(|(_, r)| pred(r))
            .map(|(i, _)| i)
    };
    found.unwrap_or(from)
}

pub fn next_hunk(rows: &[Row], from: usize) -> usize {
    seek(rows, from, true, |r| matches!(r, Row::HunkHeader { .. }))
}

pub fn prev_hunk(rows: &[Row], from: usize) -> usize {
    seek(rows, from, false, |r| matches!(r, Row::HunkHeader { .. }))
}

pub fn next_file(rows: &[Row], from: usize) -> usize {
    seek(rows, from, true, |r| matches!(r, Row::FileHeader { .. }))
}

pub fn prev_file(rows: &[Row], from: usize) -> usize {
    seek(rows, from, false, |r| matches!(r, Row::FileHeader { .. }))
}

/// The row starting the next unread file after `cursor`, wrapping within this
/// diff.
///
/// `is_unread` is asked about a *file index*, so this stays free of any
/// knowledge of how "read" is tracked.
///
/// Files the cursor is already inside are never a target: you are reviewing
/// that one now, and jumping to its header would be a no-op that looks like a
/// broken key. That is also what makes `None` meaningful — it says "this
/// review has nothing further to offer", which is the caller's cue to move to
/// the next PR in the stack rather than sitting still.
pub fn next_unreviewed_row(
    rows: &[Row],
    cursor: usize,
    is_unread: impl Fn(usize) -> bool,
) -> Option<usize> {
    let here = rows.get(cursor).and_then(Row::file_ix);
    let starts: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, row)| matches!(row, Row::FileHeader { .. }))
        .filter(|(_, row)| {
            let file = row.file_ix();
            file != here && file.is_some_and(&is_unread)
        })
        .map(|(ix, _)| ix)
        .collect();

    starts
        .iter()
        .find(|ix| **ix > cursor)
        .or_else(|| starts.first())
        .copied()
}

/// Move `rows_per_half_page` rows, clamped to the ends of the list.
pub fn half_page(rows: &[Row], from: usize, distance: usize, down: bool) -> usize {
    let last = rows.len().saturating_sub(1);
    if down {
        from.saturating_add(distance).min(last)
    } else {
        from.saturating_sub(distance)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use diffident_diff::{parser::parse, rows::build_rows};

    const TWO_FILES: &str = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,2 +1,2 @@\n ctx\n-old\n+new\n@@ -50,1 +50,2 @@\n k\n+add\ndiff --git a/b.rs b/b.rs\n--- a/b.rs\n+++ b/b.rs\n@@ -1,1 +1,1 @@\n z\n";

    fn rows() -> Vec<Row> {
        build_rows(&parse(TWO_FILES))
    }

    #[test]
    fn next_hunk_walks_forward_and_stops_at_the_last_one() {
        let rows = rows();
        let first = next_hunk(&rows, 0);
        assert!(matches!(rows[first], Row::HunkHeader { hunk_ix: 0, .. }));
        let second = next_hunk(&rows, first);
        assert!(matches!(rows[second], Row::HunkHeader { hunk_ix: 1, .. }));
        let last = next_hunk(&rows, next_hunk(&rows, second));
        assert_eq!(next_hunk(&rows, last), last, "must not run off the end");
    }

    #[test]
    fn prev_file_walks_backward_and_stops_at_the_first() {
        let rows = rows();
        let second = next_file(&rows, 0);
        assert_eq!(rows[second], Row::FileHeader { file_ix: 1 });
        assert_eq!(rows[prev_file(&rows, second)], Row::FileHeader { file_ix: 0 });
        assert_eq!(prev_file(&rows, 0), 0, "must not run off the front");
    }

    #[test]
    fn navigation_on_an_empty_diff_stays_put_rather_than_panicking() {
        assert_eq!(next_hunk(&[], 0), 0);
        assert_eq!(prev_file(&[], 0), 0);
    }

    #[test]
    fn half_page_is_clamped_to_the_last_row() {
        let rows = rows();
        assert_eq!(half_page(&rows, 0, 4, true), 4);
        assert_eq!(half_page(&rows, rows.len() - 1, 40, true), rows.len() - 1);
        assert_eq!(half_page(&rows, 2, 40, false), 0);
    }

    /// a.rs, b.rs, c.rs — one hunk each.
    const THREE_FILES: &str = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,1 +1,1 @@\n-x\n+y\ndiff --git a/b.rs b/b.rs\n--- a/b.rs\n+++ b/b.rs\n@@ -1,1 +1,1 @@\n-x\n+y\ndiff --git a/c.rs b/c.rs\n--- a/c.rs\n+++ b/c.rs\n@@ -1,1 +1,1 @@\n-x\n+y\n";

    fn three_files() -> Vec<Row> {
        build_rows(&parse(THREE_FILES))
    }

    /// Row index of file `ix`'s header.
    fn header_of(rows: &[Row], ix: usize) -> usize {
        rows.iter()
            .position(|r| *r == Row::FileHeader { file_ix: ix })
            .expect("every file has a header")
    }

    #[test]
    fn tab_advances_to_the_next_unread_file_rather_than_standing_still() {
        // The bug this replaced: the target was always the *first* unread row,
        // so with the cursor already there tab abandoned the PR while b.rs and
        // c.rs were still unread.
        let rows = three_files();
        let at = next_unreviewed_row(&rows, 0, |_| true);
        assert_eq!(at, Some(header_of(&rows, 1)), "from a.rs, tab goes to b.rs");
    }

    #[test]
    fn tab_keeps_advancing_on_repeated_presses() {
        let rows = three_files();
        let mut cursor = 0;
        let mut visited = vec![];
        for _ in 0..2 {
            cursor = next_unreviewed_row(&rows, cursor, |_| true).expect("more to visit");
            visited.push(cursor);
        }
        assert_eq!(visited, vec![header_of(&rows, 1), header_of(&rows, 2)]);
    }

    #[test]
    fn tab_skips_files_already_read() {
        let rows = three_files();
        // b.rs read, c.rs not.
        let at = next_unreviewed_row(&rows, 0, |ix| ix != 1);
        assert_eq!(at, Some(header_of(&rows, 2)));
    }

    #[test]
    fn tab_wraps_to_an_earlier_unread_file_from_the_end() {
        let rows = three_files();
        let last = header_of(&rows, 2);
        // Only a.rs is unread; the cursor sits in c.rs.
        let at = next_unreviewed_row(&rows, last, |ix| ix == 0);
        assert_eq!(at, Some(header_of(&rows, 0)));
    }

    #[test]
    fn a_review_with_nothing_left_reports_none_so_the_caller_can_cross_prs() {
        let rows = three_files();
        assert_eq!(next_unreviewed_row(&rows, 0, |_| false), None);
    }

    #[test]
    fn the_file_under_the_cursor_is_never_the_target() {
        // Only a.rs is unread and the cursor is inside it. Jumping to its own
        // header would look like a dead key; None tells the caller to move on.
        let rows = three_files();
        assert_eq!(next_unreviewed_row(&rows, 0, |ix| ix == 0), None);
    }

    #[test]
    fn an_empty_diff_has_nowhere_to_jump() {
        assert_eq!(next_unreviewed_row(&[], 0, |_| true), None);
    }

    #[test]
    fn every_declared_action_has_exactly_one_binding() {
        // An action bound to nothing silently swallows its key. A key bound
        // twice *in the same context* fires whichever the keymap resolves
        // first, which is a coin flip — but the same key in different contexts
        // is how modal keymaps work, so the pair is what must be unique.
        let bindings = key_bindings();
        let mut pairs: Vec<String> = bindings
            .iter()
            .map(|b| {
                let keys = b
                    .keystrokes()
                    .iter()
                    .map(|k| k.unparse())
                    .collect::<Vec<_>>()
                    .join(" ");
                format!("{:?}::{keys}", b.predicate())
            })
            .collect();
        pairs.sort();
        let before = pairs.len();
        pairs.dedup();
        assert_eq!(before, pairs.len(), "a key is bound twice in one context");
        assert_eq!(before, 31, "every action in the actions! set needs a binding");
    }
}
