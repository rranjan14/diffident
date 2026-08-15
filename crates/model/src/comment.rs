use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Which side of the diff a comment anchors to.
///
/// Maps directly onto GitHub's `LEFT`/`RIGHT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side {
    Old,
    New,
}

/// What a comment is attached to (spec §7).
///
/// One enum rather than four optional fields, so an impossible state — a
/// review-level comment carrying a line number — cannot be represented.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommentScope {
    /// Applies to the whole review.
    Review,
    File {
        path: String,
    },
    Line {
        path: String,
        line: u32,
        side: Side,
    },
    Range {
        path: String,
        /// Always <= `end_line`; constructors normalise.
        start_line: u32,
        end_line: u32,
        side: Side,
    },
}

impl CommentScope {
    /// The file this comment belongs to, if any.
    pub fn path(&self) -> Option<&str> {
        match self {
            CommentScope::Review => None,
            CommentScope::File { path }
            | CommentScope::Line { path, .. }
            | CommentScope::Range { path, .. } => Some(path),
        }
    }
}

/// How far a comment has travelled toward GitHub.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Lifecycle {
    /// Local only. The only editable state.
    LocalDraft,
    /// Submitted as part of a GitHub PENDING review.
    PushedDraft,
    /// Published.
    Submitted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comment {
    pub id: Uuid,
    pub body: String,
    pub scope: CommentScope,
    pub lifecycle: Lifecycle,
    /// Set once GitHub has assigned one.
    pub remote_id: Option<String>,
}

impl Comment {
    fn draft(scope: CommentScope, body: &str) -> Self {
        Self {
            id: Uuid::new_v4(),
            body: body.to_string(),
            scope,
            lifecycle: Lifecycle::LocalDraft,
            remote_id: None,
        }
    }

    pub fn new_review(body: &str) -> Self {
        Self::draft(CommentScope::Review, body)
    }

    pub fn new_file(path: &str, body: &str) -> Self {
        Self::draft(
            CommentScope::File {
                path: path.to_string(),
            },
            body,
        )
    }

    pub fn new_line(path: &str, line: u32, side: Side, body: &str) -> Self {
        Self::draft(
            CommentScope::Line {
                path: path.to_string(),
                line,
                side,
            },
            body,
        )
    }

    /// Bounds are normalised, so a bottom-to-top selection anchors identically
    /// to a top-to-bottom one. GitHub 422s when `start_line > line`.
    pub fn new_range(path: &str, a: u32, b: u32, side: Side, body: &str) -> Self {
        Self::draft(
            CommentScope::Range {
                path: path.to_string(),
                start_line: a.min(b),
                end_line: a.max(b),
                side,
            },
            body,
        )
    }

    /// Only local drafts may be edited or deleted — once GitHub knows about a
    /// comment, local edits would diverge from what is actually published.
    pub fn is_editable(&self) -> bool {
        self.lifecycle == Lifecycle::LocalDraft
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_comment_is_an_editable_local_draft() {
        let c = Comment::new_review("looks good");
        assert_eq!(c.lifecycle, Lifecycle::LocalDraft);
        assert!(c.is_editable());
    }

    #[test]
    fn a_submitted_comment_is_locked() {
        let mut c = Comment::new_review("looks good");
        c.lifecycle = Lifecycle::Submitted;
        assert!(!c.is_editable());
    }

    #[test]
    fn a_pushed_draft_is_also_locked() {
        // GitHub owns it once it is a PENDING review comment; editing locally
        // would silently diverge from what the reviewer will actually publish.
        let mut c = Comment::new_review("x");
        c.lifecycle = Lifecycle::PushedDraft;
        assert!(!c.is_editable());
    }

    #[test]
    fn every_comment_gets_a_distinct_id() {
        assert_ne!(Comment::new_review("a").id, Comment::new_review("a").id);
    }

    #[test]
    fn a_line_comment_carries_its_anchor() {
        let c = Comment::new_line("src/main.rs", 12, Side::New, "nit");
        assert_eq!(
            c.scope,
            CommentScope::Line {
                path: "src/main.rs".into(),
                line: 12,
                side: Side::New
            }
        );
    }

    #[test]
    fn a_range_comment_normalises_reversed_bounds() {
        // Selecting bottom-to-top must anchor the same as top-to-bottom;
        // GitHub rejects start_line > line.
        let c = Comment::new_range("a.rs", 20, 10, Side::New, "block");
        assert_eq!(
            c.scope,
            CommentScope::Range {
                path: "a.rs".into(),
                start_line: 10,
                end_line: 20,
                side: Side::New
            }
        );
    }

    #[test]
    fn scope_path_is_none_only_for_review_level_comments() {
        assert!(Comment::new_review("x").scope.path().is_none());
        assert_eq!(Comment::new_file("a.rs", "x").scope.path(), Some("a.rs"));
    }

    #[test]
    fn a_comment_round_trips_through_json() {
        let c = Comment::new_line("a.rs", 3, Side::Old, "why?");
        let back: Comment = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert_eq!(back, c);
    }
}
