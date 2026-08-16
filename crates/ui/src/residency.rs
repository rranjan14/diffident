//! What is resident, what is in flight, and where the reviewer left off.
//!
//! Three pieces of state that are meaningless apart: you cannot decide whether
//! to start a fetch without knowing both what is already resident and what is
//! already in flight, and a cursor is only worth remembering for a review whose
//! diff you are about to drop. Keeping them in one type means the whole
//! multi-review policy is testable without opening a window.

use std::collections::HashMap;

/// The resident set for one window: bounded, most-recently-used last.
pub struct Residency<T> {
    /// Most-recently-used last. A `Vec` rather than an LRU crate or a
    /// HashMap+order pair: it holds four things, so a linear scan beats hashing
    /// and the whole policy stays readable at a glance.
    resident: Vec<(u32, T)>,
    /// Keys with a fetch in flight.
    pending: Vec<u32>,
    /// Last cursor row per key, kept after the diff itself is evicted — one
    /// integer per review is free, and it means returning to a review whose
    /// diff was dropped still lands where the reviewer left off.
    cursors: HashMap<u32, usize>,
    /// Head SHA each key was last admitted at, so a force-push can invalidate
    /// the remembered cursor rather than restoring it into a different diff.
    heads: HashMap<u32, String>,
    cap: usize,
}

impl<T> Residency<T> {
    pub fn new(cap: usize) -> Self {
        Self {
            resident: Vec::new(),
            pending: Vec::new(),
            cursors: HashMap::new(),
            heads: HashMap::new(),
            cap,
        }
    }

    /// The value for `key`, if resident.
    pub fn get(&self, key: u32) -> Option<&T> {
        self.resident.iter().find(|(k, _)| *k == key).map(|(_, v)| v)
    }

    /// Promote `key` to most-recently-used. Returns whether it was resident.
    pub fn activate(&mut self, key: u32) -> bool {
        match self.resident.iter().position(|(k, _)| *k == key) {
            Some(pos) => {
                let entry = self.resident.remove(pos);
                self.resident.push(entry);
                true
            }
            None => false,
        }
    }

    /// Record a completed fetch: insert as most-recently-used, evict past the
    /// cap, and clear the in-flight mark.
    ///
    /// Deliberately unconditional. A result that arrives after the reviewer has
    /// switched away is still correct data for *its own* review, and throwing
    /// it out would refetch the same seconds of work on the way back. What the
    /// reviewer is looking at is decided by `get`, not by who finished last.
    ///
    /// `head` is the commit the diff was fetched at. When it differs from the
    /// last admission the remembered cursor is dropped: the reviewer was on row
    /// 900 of a diff that no longer exists, and restoring that position would
    /// land them somewhere unrelated.
    pub fn admit(&mut self, key: u32, value: T, head: &str) {
        if self
            .heads
            .insert(key, head.to_string())
            .is_some_and(|old| old != head)
        {
            self.cursors.remove(&key);
        }
        self.pending.retain(|k| *k != key);
        self.resident.retain(|(k, _)| *k != key);
        self.resident.push((key, value));
        while self.resident.len() > self.cap {
            self.resident.remove(0);
        }
    }

    /// Whether the caller should start a fetch for `key`.
    ///
    /// False when the value is already resident or a fetch is already running —
    /// clicking a still-loading review must not start a second identical fetch,
    /// which costs seconds and races the first one to land.
    pub fn begin_fetch(&mut self, key: u32) -> bool {
        if self.get(key).is_some() || self.pending.contains(&key) {
            return false;
        }
        self.pending.push(key);
        true
    }


    /// Clear the in-flight mark for a fetch that failed, so a retry is possible.
    pub fn abandon_fetch(&mut self, key: u32) {
        self.pending.retain(|k| *k != key);
    }

    pub fn remember_cursor(&mut self, key: u32, row: usize) {
        self.cursors.insert(key, row);
    }

    /// Where to put the cursor when `key` is opened, given how many rows the
    /// diff has *now*.
    ///
    /// Clamped, because the diff can be shorter than when the cursor was
    /// remembered — a force-push between visits is exactly the case that would
    /// otherwise index past the end.
    pub fn recall_cursor(&self, key: u32, row_count: usize) -> usize {
        match self.cursors.get(&key) {
            Some(&row) => row.min(row_count.saturating_sub(1)),
            None => 0,
        }
    }

    /// Resident keys, oldest first. For tests and diagnostics.
    pub fn keys(&self) -> Vec<u32> {
        self.resident.iter().map(|(k, _)| *k).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEAD: &str = "abc123";

    fn residency() -> Residency<char> {
        Residency::new(4)
    }

    #[test]
    fn a_resident_review_is_returned_and_promoted() {
        let mut r = residency();
        r.admit(1, 'a', HEAD);
        r.admit(2, 'b', HEAD);
        assert!(r.activate(1));
        assert_eq!(r.keys(), vec![2, 1], "activating must make it most-recent");
        assert_eq!(r.get(1), Some(&'a'));
    }

    #[test]
    fn activating_an_absent_review_reports_a_miss_and_changes_nothing() {
        let mut r = residency();
        r.admit(1, 'a', HEAD);
        assert!(!r.activate(9));
        assert_eq!(r.keys(), vec![1]);
    }

    #[test]
    fn admitting_beyond_the_cap_evicts_the_least_recently_used() {
        let mut r = residency();
        for (n, c) in [(1, 'a'), (2, 'b'), (3, 'c'), (4, 'd'), (5, 'e'), (6, 'f')] {
            r.admit(n, c, HEAD);
        }
        assert_eq!(r.keys(), vec![3, 4, 5, 6]);
        assert_eq!(r.get(1), None, "1 was evicted");
    }

    #[test]
    fn a_second_click_while_loading_does_not_start_another_fetch() {
        let mut r = residency();
        assert!(r.begin_fetch(7), "first click starts the fetch");
        assert!(!r.begin_fetch(7), "second click must not");
    }

    #[test]
    fn an_already_resident_review_never_starts_a_fetch() {
        let mut r = residency();
        r.admit(7, 'a', HEAD);
        assert!(!r.begin_fetch(7));
    }

    #[test]
    fn admitting_a_result_clears_its_in_flight_mark() {
        let mut r = residency();
        r.begin_fetch(7);
        r.admit(7, 'a', HEAD);
        assert!(!r.begin_fetch(7), "now resident, so still no fetch");
    }

    #[test]
    fn a_force_push_forgets_the_remembered_cursor() {
        // Row 42 of the old diff is not row 42 of the new one, so restoring it
        // would drop the reviewer somewhere unrelated.
        let mut r = residency();
        r.admit(1, 'a', "head-one");
        r.remember_cursor(1, 42);
        r.admit(1, 'b', "head-two");
        assert_eq!(r.recall_cursor(1, 100), 0);
    }

    #[test]
    fn re_admitting_at_the_same_head_keeps_the_cursor() {
        let mut r = residency();
        r.admit(1, 'a', HEAD);
        r.remember_cursor(1, 42);
        r.admit(1, 'b', HEAD);
        assert_eq!(r.recall_cursor(1, 100), 42);
    }

    #[test]
    fn re_activating_after_each_admit_keeps_the_on_screen_review_resident() {
        // Open one review, then let four other fetches land at once. Without
        // re-activating the one being looked at, those four admissions evict it
        // and the diff pane goes blank. `Workspace::apply` re-activates the
        // active review after every admit for exactly this reason.
        let mut r = residency();
        r.admit(1, 'a', HEAD);
        for (n, c) in [(2, 'b'), (3, 'c'), (4, 'd'), (5, 'e')] {
            r.admit(n, c, HEAD);
            r.activate(1);
        }
        assert!(r.get(1).is_some(), "the on-screen review must survive");
        assert_eq!(*r.keys().last().unwrap(), 1, "and stay most-recently-used");
    }

    #[test]
    fn without_re_activating_the_on_screen_review_is_evicted() {
        // The bug this guards against, stated as a fact about the cache: four
        // admissions past the cap will drop whatever was there first.
        let mut r = residency();
        r.admit(1, 'a', HEAD);
        for (n, c) in [(2, 'b'), (3, 'c'), (4, 'd'), (5, 'e')] {
            r.admit(n, c, HEAD);
        }
        assert!(r.get(1).is_none());
    }

    #[test]
    fn an_unseen_review_opens_at_the_top() {
        assert_eq!(residency().recall_cursor(7, 100), 0);
    }

    #[test]
    fn a_remembered_cursor_survives_eviction_of_its_diff() {
        let mut r = residency();
        r.admit(1, 'a', HEAD);
        r.remember_cursor(1, 42);
        for (n, c) in [(2, 'b'), (3, 'c'), (4, 'd'), (5, 'e')] {
            r.admit(n, c, HEAD);
        }
        assert_eq!(r.get(1), None, "the diff itself is gone");
        assert_eq!(r.recall_cursor(1, 100), 42, "but the position is not");
    }

    #[test]
    fn a_remembered_cursor_is_clamped_to_a_diff_that_shrank() {
        // The author force-pushed a smaller diff between visits.
        let mut r = residency();
        r.remember_cursor(1, 900);
        assert_eq!(r.recall_cursor(1, 10), 9);
    }

    #[test]
    fn a_remembered_cursor_on_an_empty_diff_is_zero_not_a_panic() {
        let mut r = residency();
        r.remember_cursor(1, 900);
        assert_eq!(r.recall_cursor(1, 0), 0);
    }
    #[test]
    fn a_failed_fetch_can_be_retried() {
        let mut r = residency();
        r.begin_fetch(7);
        r.abandon_fetch(7);
        assert!(r.begin_fetch(7), "a failure must not block retrying forever");
    }
    #[test]
    fn a_late_result_is_kept_even_though_the_reviewer_moved_on() {
        // Switching away must not throw away a fetch that already succeeded —
        // it is correct data for its own review and cost seconds to get.
        let mut r = residency();
        r.begin_fetch(7);
        r.admit(9, 'b', HEAD); // reviewer opened 9 meanwhile
        r.admit(7, 'a', HEAD); // 7's slow result finally lands
        assert_eq!(r.get(7), Some(&'a'), "kept, so returning to 7 is instant");
        assert_eq!(r.keys(), vec![9, 7]);
    }
    #[test]
    fn re_admitting_replaces_rather_than_duplicating() {
        // A refetch must not leave two entries, or the stale one can be served
        // after the fresh one is evicted.
        let mut r = residency();
        r.admit(1, 'a', HEAD);
        r.admit(2, 'b', HEAD);
        r.admit(1, 'z', HEAD);
        assert_eq!(r.keys(), vec![2, 1]);
        assert_eq!(r.get(1), Some(&'z'));
    }
}
