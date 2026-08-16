//! Which files the reviewer has already read.
//!
//! In-memory only. Persistence and the `content_hash` invalidation that
//! survives a force-push are Phase 5 (§7) — this is the smaller thing Phase 4
//! needs to answer "what is left to review, across the whole stack" (§6).

use std::collections::{HashMap, HashSet};

/// Reviewed-file marks for every review in the window.
///
/// Keyed by PR number rather than held on each review's diff, because the diff
/// is evicted from the resident set while the marks must survive — the same
/// reason cursors live outside it.
#[derive(Debug, Default)]
pub struct Reviewed {
    by_review: HashMap<u32, HashSet<String>>,
}

impl Reviewed {
    pub fn new() -> Self {
        Self::default()
    }

    /// Flip the mark on one file. Returns the new state.
    pub fn toggle(&mut self, review: u32, path: &str) -> bool {
        let marks = self.by_review.entry(review).or_default();
        if marks.remove(path) {
            false
        } else {
            marks.insert(path.to_string());
            true
        }
    }

    pub fn is_reviewed(&self, review: u32, path: &str) -> bool {
        self.by_review
            .get(&review)
            .is_some_and(|m| m.contains(path))
    }

    /// How many of `paths` are still unread.
    ///
    /// Takes the file list rather than storing it: the marks outlive the diff,
    /// but the set of files a PR touches is only known once its diff has been
    /// fetched. A caller with no file list has no count to show, which is why
    /// this returns a number rather than an `Option` — pass what you know.
    pub fn unreviewed_count(&self, review: u32, paths: &[String]) -> usize {
        paths.iter().filter(|p| !self.is_reviewed(review, p)).count()
    }

    /// The first unread file in `paths`, or `None` when all are read.
    pub fn first_unreviewed<'a>(&self, review: u32, paths: &'a [String]) -> Option<&'a String> {
        paths.iter().find(|p| !self.is_reviewed(review, p))
    }

}

#[cfg(test)]
#[cfg(test)]
mod tests {
    use super::*;

    fn paths() -> Vec<String> {
        vec!["a.rs".into(), "b.rs".into(), "c.rs".into()]
    }

    #[test]
    fn a_file_starts_unreviewed() {
        assert!(!Reviewed::new().is_reviewed(7, "a.rs"));
    }

    #[test]
    fn toggling_marks_then_unmarks() {
        let mut r = Reviewed::new();
        assert!(r.toggle(7, "a.rs"), "first toggle marks it read");
        assert!(r.is_reviewed(7, "a.rs"));
        assert!(!r.toggle(7, "a.rs"), "second toggle unmarks it");
        assert!(!r.is_reviewed(7, "a.rs"));
    }

    #[test]
    fn marks_are_scoped_to_one_review() {
        // Two PRs in a stack routinely touch the same path; marking it read in
        // one must not mark it read in the other.
        let mut r = Reviewed::new();
        r.toggle(7, "a.rs");
        assert!(r.is_reviewed(7, "a.rs"));
        assert!(!r.is_reviewed(9, "a.rs"));
    }

    #[test]
    fn unreviewed_count_counts_what_is_left() {
        let mut r = Reviewed::new();
        assert_eq!(r.unreviewed_count(7, &paths()), 3);
        r.toggle(7, "b.rs");
        assert_eq!(r.unreviewed_count(7, &paths()), 2);
    }

    #[test]
    fn a_mark_for_a_file_no_longer_in_the_diff_does_not_affect_the_count() {
        // The PR was force-pushed and b.rs is gone. The stale mark must not
        // make the count disagree with what is on screen.
        let mut r = Reviewed::new();
        r.toggle(7, "b.rs");
        let now = vec!["a.rs".to_string(), "c.rs".to_string()];
        assert_eq!(r.unreviewed_count(7, &now), 2);
    }

    #[test]
    fn first_unreviewed_skips_what_is_already_read() {
        let mut r = Reviewed::new();
        r.toggle(7, "a.rs");
        assert_eq!(r.first_unreviewed(7, &paths()), Some(&"b.rs".to_string()));
    }

    #[test]
    fn a_fully_reviewed_pr_has_no_first_unreviewed() {
        let mut r = Reviewed::new();
        for p in paths() {
            r.toggle(7, &p);
        }
        assert_eq!(r.first_unreviewed(7, &paths()), None);
        assert_eq!(r.unreviewed_count(7, &paths()), 0);
    }

}
