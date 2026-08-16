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
        NextReview,
        PrevReview,
        Refresh,
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
        KeyBinding::new("ctrl-tab", NextReview, None),
        KeyBinding::new("ctrl-shift-tab", PrevReview, None),
        KeyBinding::new("shift-r", Refresh, None),
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

    #[test]
    fn every_declared_action_has_exactly_one_binding() {
        // An action bound to nothing silently swallows its key; a key bound
        // twice fires whichever the keymap resolves first, which is a coin flip.
        let bindings = key_bindings();
        let mut keys: Vec<String> = bindings
            .iter()
            .map(|b| {
                b.keystrokes()
                    .iter()
                    .map(|k| k.unparse())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect();
        keys.sort();
        let before = keys.len();
        keys.dedup();
        assert_eq!(before, keys.len(), "a key is bound twice");
        assert_eq!(before, 15, "every action in the actions! set needs a binding");
    }
}
