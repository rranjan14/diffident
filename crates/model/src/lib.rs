//! Shared domain types.
//!
//! Deliberately framework-free: nothing here depends on gpui. That is what lets the
//! headless crates (diff, forge, session) use these types without pulling a UI toolkit
//! into their build or their tests.

pub mod comment;

/// One open review. Everything a PR needs lives here, so N of them coexist in one window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Review {
    /// `None` for a local source (working tree, staged, commit range).
    pub number: Option<u32>,
    pub branch: String,
    pub added: u32,
    pub removed: u32,
}

impl Review {
    /// How the review is labelled in the rail beneath its branch name.
    pub fn subtitle(&self) -> String {
        match self.number {
            Some(n) => format!("#{n}"),
            None => "local".to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subtitle_distinguishes_pr_from_local() {
        let pr = Review {
            number: Some(142),
            branch: "feat/add-search".into(),
            added: 2367,
            removed: 19,
        };
        assert_eq!(pr.subtitle(), "#142");

        let local = Review {
            number: None,
            ..pr
        };
        assert_eq!(local.subtitle(), "local");
    }
}
