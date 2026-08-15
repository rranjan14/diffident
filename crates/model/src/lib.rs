//! Shared domain types.
//!
//! Deliberately framework-free: nothing here depends on gpui. That is what lets
//! the headless crates (diff, forge, session, highlight) use these types without
//! pulling a UI toolkit into their build or their tests.

pub mod comment;

/// Identifies a review for the life of the app.
///
/// `(repo, number)` is stable; the head SHA is not, so it lives in `LoadState`
/// where a force-push can move it without invalidating the identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ReviewId {
    /// `owner/name`.
    pub repo: String,
    pub number: u32,
}

/// Where a review is in its fetch lifecycle.
///
/// Modelled as one enum rather than `Option` fields so "loaded but with no
/// counts" cannot be represented.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadState {
    /// Listed in the rail, never opened.
    Idle,
    Loading,
    Ready {
        head_sha: String,
        added: u32,
        removed: u32,
    },
    Failed {
        message: String,
    },
}

/// One open review in the rail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Review {
    pub id: ReviewId,
    pub title: String,
    pub branch: String,
    /// Rail indent from stack detection (§6). 0 for a root PR.
    pub depth: usize,
    pub is_draft: bool,
    pub state: LoadState,
}

impl Review {
    /// How the review is labelled in the rail beneath its branch name.
    pub fn subtitle(&self) -> String {
        format!("#{}", self.id.number)
    }

    /// `Some((added, removed))` once the diff has landed, `None` before.
    pub fn counts(&self) -> Option<(u32, u32)> {
        match self.state {
            LoadState::Ready { added, removed, .. } => Some((added, removed)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn review() -> Review {
        Review {
            id: ReviewId {
                repo: "owner/name".into(),
                number: 142,
            },
            title: "Add search".into(),
            branch: "feat/add-search".into(),
            depth: 0,
            is_draft: false,
            state: LoadState::Idle,
        }
    }

    #[test]
    fn subtitle_shows_the_pr_number() {
        assert_eq!(review().subtitle(), "#142");
    }

    #[test]
    fn counts_are_absent_until_the_diff_lands() {
        assert_eq!(review().counts(), None);
        let loaded = Review {
            state: LoadState::Ready {
                head_sha: "abc".into(),
                added: 12,
                removed: 3,
            },
            ..review()
        };
        assert_eq!(loaded.counts(), Some((12, 3)));
    }
}
