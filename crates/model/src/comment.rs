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

/// Every local draft in the window, per review.
///
/// Held outside the diff for the same reason reviewed marks are: the diff is
/// evicted from the resident set while a draft the reviewer spent a minute
/// writing must not be.
#[derive(Debug, Default)]
pub struct Drafts {
    by_review: std::collections::HashMap<u32, Vec<Comment>>,
}

impl Drafts {
    pub fn new() -> Self {
        Self::default()
    }

    /// Newest last, so the UI reads them in the order they were written.
    pub fn add(&mut self, review: u32, comment: Comment) {
        self.by_review.entry(review).or_default().push(comment);
    }

    pub fn for_review(&self, review: u32) -> &[Comment] {
        self.by_review.get(&review).map_or(&[], Vec::as_slice)
    }

    pub fn count(&self, review: u32) -> usize {
        self.for_review(review).len()
    }

    /// Drop one draft, reporting whether it was there.
    ///
    /// Refuses anything GitHub already knows about: that copy is a mirror, so
    /// deleting it here would only hide a comment that still exists on the PR.
    pub fn remove(&mut self, review: u32, id: Uuid) -> bool {
        let Some(drafts) = self.by_review.get_mut(&review) else {
            return false;
        };
        let before = drafts.len();
        drafts.retain(|c| c.id != id || !c.is_editable());
        drafts.len() != before
    }

    /// Replace one review's drafts, as loaded from disk.
    pub fn restore(&mut self, review: u32, comments: Vec<Comment>) {
        self.by_review.insert(review, comments);
    }

    /// Mark these drafts as having reached GitHub.
    ///
    /// Ids not present are ignored rather than reported: the caller computed
    /// them from the same set a moment ago, and failing a whole submit because
    /// one draft was deleted mid-flight would be worse than quietly skipping it.
    pub fn mark(&mut self, review: u32, ids: &[Uuid], lifecycle: Lifecycle) {
        let Some(list) = self.by_review.get_mut(&review) else {
            return;
        };
        for comment in list.iter_mut().filter(|c| ids.contains(&c.id)) {
            comment.lifecycle = lifecycle;
        }
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

#[cfg(test)]
mod draft_tests {
    use super::*;

    #[test]
    fn a_review_starts_with_no_drafts() {
        let d = Drafts::new();
        assert_eq!(d.count(7), 0);
        assert!(d.for_review(7).is_empty());
    }

    #[test]
    fn drafts_are_kept_in_authoring_order() {
        let mut d = Drafts::new();
        d.add(7, Comment::new_review("first"));
        d.add(7, Comment::new_review("second"));
        let bodies: Vec<&str> = d.for_review(7).iter().map(|c| c.body.as_str()).collect();
        assert_eq!(bodies, ["first", "second"]);
    }

    #[test]
    fn drafts_are_scoped_to_one_review() {
        // Two PRs in a stack are reviewed side by side; a draft written on one
        // must not show up on the other.
        let mut d = Drafts::new();
        d.add(7, Comment::new_review("on seven"));
        assert_eq!(d.count(7), 1);
        assert_eq!(d.count(9), 0);
    }

    #[test]
    fn a_local_draft_can_be_deleted() {
        let mut d = Drafts::new();
        let c = Comment::new_review("oops");
        let id = c.id;
        d.add(7, c);
        assert!(d.remove(7, id));
        assert_eq!(d.count(7), 0);
    }

    #[test]
    fn deleting_an_unknown_draft_reports_nothing_removed() {
        let mut d = Drafts::new();
        d.add(7, Comment::new_review("keep"));
        assert!(!d.remove(7, Uuid::new_v4()));
        assert!(!d.remove(9, Uuid::new_v4()), "and not for an unknown review");
        assert_eq!(d.count(7), 1);
    }

    #[test]
    fn a_comment_github_already_has_cannot_be_deleted_locally() {
        // The local copy is a mirror of what is published. Dropping it here
        // would hide a comment that still exists on the PR.
        let mut d = Drafts::new();
        let mut c = Comment::new_review("published");
        c.lifecycle = Lifecycle::Submitted;
        let id = c.id;
        d.add(7, c);
        assert!(!d.remove(7, id));
        assert_eq!(d.count(7), 1);
    }

    #[test]
    fn drafts_round_trip_for_persistence() {
        let mut d = Drafts::new();
        d.add(7, Comment::new_review("written before the restart"));
        let saved = d.for_review(7).to_vec();
        let mut fresh = Drafts::new();
        fresh.restore(7, saved.clone());
        assert_eq!(fresh.for_review(7), saved.as_slice());
    }

    #[test]
    fn marking_drafts_sent_makes_them_uneditable() {
        let mut d = Drafts::new();
        let c = Comment::new_review("sent");
        let id = c.id;
        d.add(7, c);
        d.mark(7, &[id], Lifecycle::Submitted);
        assert!(!d.for_review(7)[0].is_editable());
    }

    #[test]
    fn marking_leaves_drafts_that_were_not_sent_alone() {
        // The omitted ones must stay editable for a later review.
        let mut d = Drafts::new();
        let sent = Comment::new_review("sent");
        let kept = Comment::new_review("kept");
        let sent_id = sent.id;
        d.add(7, sent);
        d.add(7, kept);
        d.mark(7, &[sent_id], Lifecycle::Submitted);
        assert!(d.for_review(7).iter().any(|c| c.is_editable()));
        assert!(d.for_review(7).iter().any(|c| !c.is_editable()));
    }

    #[test]
    fn marking_an_id_that_is_gone_is_not_an_error() {
        let mut d = Drafts::new();
        d.add(7, Comment::new_review("here"));
        d.mark(7, &[Uuid::new_v4()], Lifecycle::Submitted);
        assert_eq!(d.count(7), 1);
    }
}
