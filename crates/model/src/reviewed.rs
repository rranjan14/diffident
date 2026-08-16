//! Which files the reviewer has already read.
//!
//! Marks record the content hash they were read at (§7), so a file that
//! changed under them reads as unread again, and `marks`/`restore` carry the
//! set across a restart. This is also what answers "what is left to review,
//! across the whole stack" (§6).

use std::collections::HashMap;

/// Reviewed-file marks for every review in the window.
///
/// Keyed by PR number rather than held on each review's diff, because the diff
/// is evicted from the resident set while the marks must survive — the same
/// reason cursors live outside it.
#[derive(Debug, Default)]
pub struct Reviewed {
    by_review: HashMap<u32, HashMap<String, u64>>,
}

impl Reviewed {
    pub fn new() -> Self {
        Self::default()
    }

    /// Flip the mark on one file, recording the contents it was read at.
    /// Returns the new state.
    pub fn toggle(&mut self, review: u32, path: &str, hash: u64) -> bool {
        let marks = self.by_review.entry(review).or_default();
        if marks.remove(path).is_some() {
            false
        } else {
            marks.insert(path.to_string(), hash);
            true
        }
    }

    /// Whether this file is marked read **at these contents** (§7).
    ///
    /// A mark recorded against different contents reads as unread rather than
    /// being deleted: nothing has to sweep stale marks, and if the author
    /// reverts their change the original mark starts matching again on its own.
    pub fn is_reviewed(&self, review: u32, path: &str, hash: u64) -> bool {
        self.by_review
            .get(&review)
            .and_then(|m| m.get(path))
            .is_some_and(|stored| *stored == hash)
    }

    /// Every mark for one review, for persisting. Path to the hash it was read at.
    pub fn marks(&self, review: u32) -> HashMap<String, u64> {
        self.by_review.get(&review).cloned().unwrap_or_default()
    }

    /// Replace one review's marks, as loaded from disk.
    pub fn restore(&mut self, review: u32, marks: HashMap<String, u64>) {
        self.by_review.insert(review, marks);
    }

    /// How many of `files` are still unread.
    ///
    /// Takes the file list rather than storing it: the marks outlive the diff,
    /// but the set of files a PR touches is only known once its diff has been
    /// fetched. A caller with no file list has no count to show, which is why
    /// this returns a number rather than an `Option` — pass what you know.
    pub fn unreviewed_count(&self, review: u32, files: &[(String, u64)]) -> usize {
        files
            .iter()
            .filter(|(p, h)| !self.is_reviewed(review, p, *h))
            .count()
    }

    /// The first unread file in `files`, or `None` when all are read.
    pub fn first_unreviewed<'a>(
        &self,
        review: u32,
        files: &'a [(String, u64)],
    ) -> Option<&'a String> {
        files
            .iter()
            .find(|(p, h)| !self.is_reviewed(review, p, *h))
            .map(|(p, _)| p)
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    const H: u64 = 111;

    fn files() -> Vec<(String, u64)> {
        vec![("a.rs".into(), H), ("b.rs".into(), H), ("c.rs".into(), H)]
    }

    #[test]
    fn a_file_starts_unreviewed() {
        assert!(!Reviewed::new().is_reviewed(7, "a.rs", H));
    }

    #[test]
    fn toggling_marks_then_unmarks() {
        let mut r = Reviewed::new();
        assert!(r.toggle(7, "a.rs", H), "first toggle marks it read");
        assert!(r.is_reviewed(7, "a.rs", H));
        assert!(!r.toggle(7, "a.rs", H), "second toggle unmarks it");
        assert!(!r.is_reviewed(7, "a.rs", H));
    }

    #[test]
    fn marks_are_scoped_to_one_review() {
        // Two PRs in a stack routinely touch the same path; marking it read in
        // one must not mark it read in the other.
        let mut r = Reviewed::new();
        r.toggle(7, "a.rs", H);
        assert!(r.is_reviewed(7, "a.rs", H));
        assert!(!r.is_reviewed(9, "a.rs", H));
    }

    #[test]
    fn a_mark_recorded_at_different_contents_reads_as_unread() {
        // §7: a changed file drops back to unreviewed. Not deleted — if the
        // author reverts, the original mark starts matching again on its own.
        let mut r = Reviewed::new();
        r.toggle(7, "a.rs", 111);
        assert!(r.is_reviewed(7, "a.rs", 111));
        assert!(!r.is_reviewed(7, "a.rs", 222), "contents changed");
        assert!(r.is_reviewed(7, "a.rs", 111), "and back again if reverted");
    }

    #[test]
    fn marks_round_trip_for_persistence() {
        let mut r = Reviewed::new();
        r.toggle(7, "a.rs", 111);
        let saved = r.marks(7);
        let mut fresh = Reviewed::new();
        fresh.restore(7, saved);
        assert!(fresh.is_reviewed(7, "a.rs", 111));
    }

    #[test]
    fn unreviewed_count_counts_what_is_left() {
        let mut r = Reviewed::new();
        assert_eq!(r.unreviewed_count(7, &files()), 3);
        r.toggle(7, "b.rs", H);
        assert_eq!(r.unreviewed_count(7, &files()), 2);
    }

    #[test]
    fn a_mark_for_a_file_no_longer_in_the_diff_does_not_affect_the_count() {
        // The PR was force-pushed and b.rs is gone. The stale mark must not
        // make the count disagree with what is on screen.
        let mut r = Reviewed::new();
        r.toggle(7, "b.rs", H);
        let now = vec![("a.rs".to_string(), H), ("c.rs".to_string(), H)];
        assert_eq!(r.unreviewed_count(7, &now), 2);
    }

    #[test]
    fn first_unreviewed_skips_what_is_already_read() {
        let mut r = Reviewed::new();
        r.toggle(7, "a.rs", H);
        assert_eq!(r.first_unreviewed(7, &files()), Some(&"b.rs".to_string()));
    }

    #[test]
    fn a_fully_reviewed_pr_has_no_first_unreviewed() {
        let mut r = Reviewed::new();
        for (p, h) in files() {
            r.toggle(7, &p, h);
        }
        assert_eq!(r.first_unreviewed(7, &files()), None);
        assert_eq!(r.unreviewed_count(7, &files()), 0);
    }

}
