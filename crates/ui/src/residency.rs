//! What is resident, what is in flight, and where the reviewer left off.
//!
//! Three pieces of state that are meaningless apart: you cannot decide whether
//! to start a fetch without knowing both what is already resident and what is
//! already in flight, and a cursor is only worth remembering for a review whose
//! diff you are about to drop. Keeping them in one type means the whole
//! multi-review policy is testable without opening a window.

/// The resident set for one window: bounded, most-recently-used last.
pub struct Residency<T> {
    /// Most-recently-used last. A `Vec` rather than an LRU crate or a
    /// HashMap+order pair: it holds four things, so a linear scan beats hashing
    /// and the whole policy stays readable at a glance.
    resident: Vec<(u32, T)>,
    /// Keys with a fetch in flight.
    pending: Vec<u32>,
    cap: usize,
}

impl<T> Residency<T> {
    pub fn new(cap: usize) -> Self {
        Self {
            resident: Vec::new(),
            pending: Vec::new(),
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
    pub fn admit(&mut self, key: u32, value: T) {
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

    /// Whether a fetch for `key` is running.
    pub fn is_fetching(&self, key: u32) -> bool {
        self.pending.contains(&key)
    }

    /// Clear the in-flight mark for a fetch that failed, so a retry is possible.
    pub fn abandon_fetch(&mut self, key: u32) {
        self.pending.retain(|k| *k != key);
    }

    /// Resident keys, oldest first. For tests and diagnostics.
    pub fn keys(&self) -> Vec<u32> {
        self.resident.iter().map(|(k, _)| *k).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn residency() -> Residency<char> {
        Residency::new(4)
    }

    #[test]
    fn a_resident_review_is_returned_and_promoted() {
        let mut r = residency();
        r.admit(1, 'a');
        r.admit(2, 'b');
        assert!(r.activate(1));
        assert_eq!(r.keys(), vec![2, 1], "activating must make it most-recent");
        assert_eq!(r.get(1), Some(&'a'));
    }

    #[test]
    fn activating_an_absent_review_reports_a_miss_and_changes_nothing() {
        let mut r = residency();
        r.admit(1, 'a');
        assert!(!r.activate(9));
        assert_eq!(r.keys(), vec![1]);
    }

    #[test]
    fn admitting_beyond_the_cap_evicts_the_least_recently_used() {
        let mut r = residency();
        for (n, c) in [(1, 'a'), (2, 'b'), (3, 'c'), (4, 'd'), (5, 'e'), (6, 'f')] {
            r.admit(n, c);
        }
        assert_eq!(r.keys(), vec![3, 4, 5, 6]);
        assert_eq!(r.get(1), None, "1 was evicted");
    }

    #[test]
    fn a_second_click_while_loading_does_not_start_another_fetch() {
        let mut r = residency();
        assert!(r.begin_fetch(7), "first click starts the fetch");
        assert!(!r.begin_fetch(7), "second click must not");
        assert!(r.is_fetching(7));
    }

    #[test]
    fn an_already_resident_review_never_starts_a_fetch() {
        let mut r = residency();
        r.admit(7, 'a');
        assert!(!r.begin_fetch(7));
    }

    #[test]
    fn admitting_a_result_clears_its_in_flight_mark() {
        let mut r = residency();
        r.begin_fetch(7);
        r.admit(7, 'a');
        assert!(!r.is_fetching(7));
        assert!(!r.begin_fetch(7), "now resident, so still no fetch");
    }

    #[test]
    fn a_failed_fetch_can_be_retried() {
        let mut r = residency();
        r.begin_fetch(7);
        r.abandon_fetch(7);
        assert!(!r.is_fetching(7));
        assert!(r.begin_fetch(7), "a failure must not block retrying forever");
    }
    #[test]
    fn a_late_result_is_kept_even_though_the_reviewer_moved_on() {
        // Switching away must not throw away a fetch that already succeeded —
        // it is correct data for its own review and cost seconds to get.
        let mut r = residency();
        r.begin_fetch(7);
        r.admit(9, 'b'); // reviewer opened 9 meanwhile
        r.admit(7, 'a'); // 7's slow result finally lands
        assert_eq!(r.get(7), Some(&'a'), "kept, so returning to 7 is instant");
        assert_eq!(r.keys(), vec![9, 7]);
    }
    #[test]
    fn re_admitting_replaces_rather_than_duplicating() {
        // A refetch must not leave two entries, or the stale one can be served
        // after the fresh one is evicted.
        let mut r = residency();
        r.admit(1, 'a');
        r.admit(2, 'b');
        r.admit(1, 'z');
        assert_eq!(r.keys(), vec![2, 1]);
        assert_eq!(r.get(1), Some(&'z'));
    }
}
