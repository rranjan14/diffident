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
    cap: usize,
}

impl<T> Residency<T> {
    pub fn new(cap: usize) -> Self {
        Self {
            resident: Vec::new(),
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

    /// Insert as most-recently-used, evicting from the front past the cap.
    pub fn admit(&mut self, key: u32, value: T) {
        self.resident.retain(|(k, _)| *k != key);
        self.resident.push((key, value));
        while self.resident.len() > self.cap {
            self.resident.remove(0);
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
