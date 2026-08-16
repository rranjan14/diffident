//! Shared domain types.
//!
//! Deliberately framework-free: nothing here depends on gpui. That is what lets
//! the headless crates (diff, forge, session, highlight) use these types without
//! pulling a UI toolkit into their build or their tests.

pub mod reviewed;

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
        added: u32,
        removed: u32,
        /// Every path the PR touches, in diff order.
        ///
        /// Lives here rather than on the diff because the diff is evicted from
        /// the resident set while the rail badge and cross-stack navigation
        /// still need to know what the PR contains. That "we have paths" and
        /// "we have loaded it" are the same condition is deliberate: an
        /// unloaded PR has no count to show, rather than a guessed one.
        paths: Vec<String>,
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
    /// The commit this review was listed at.
    pub head_sha: String,
    /// The head moved since we loaded this review's diff (§6).
    ///
    /// A flag, not an action: the reviewed marks are deliberately left alone.
    /// Phase 5's `content_hash` can tell which files genuinely changed; until
    /// then, telling the reviewer beats silently discarding their progress.
    pub rebased: bool,
    pub state: LoadState,
}

impl Review {
    /// How the review is labelled in the rail beneath its branch name.
    pub fn subtitle(&self) -> String {
        format!("#{}", self.id.number)
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
            head_sha: String::new(),
            rebased: false,
            state: LoadState::Idle,
        }
    }

    #[test]
    fn subtitle_shows_the_pr_number() {
        assert_eq!(review().subtitle(), "#142");
    }

}
