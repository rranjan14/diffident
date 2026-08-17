//! Deciding what a review will actually say before it is sent (§7).
//!
//! Every draft is written against the diff as it looked at the time. By submit
//! it may no longer fit: the line moved, the file turned out to be binary, the
//! anchor has no place in GitHub's API at all. Working that out *before* the
//! network call is what lets the reviewer choose what happens rather than
//! watching a 422 come back.
//!
//! Headless on purpose — no gpui here. The modals that present these choices
//! are Phase 6b; everything in this file is decided and tested without a window.

use diffident_diff::{DiffFile, FileKind, LineKind};
use diffident_model::comment::{Comment, CommentScope, Lifecycle, Side};

/// Why a draft cannot be sent as a line comment (§7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unmappable {
    /// The two ends of a range no longer sit on the same side of the diff.
    MixedSideRange,
    /// A whole-file comment. GitHub's create-review call anchors on a line, so
    /// there is nowhere to put it.
    FileLevelNoAnchor,
    /// The file has no text to anchor to.
    BinaryFile,
    /// The file is large enough that GitHub truncates it out of the diff.
    TooLargeFile,
    /// The anchor is simply not in the diff any more.
    LineNotInDiff,
}

impl Unmappable {
    /// One line the reviewer can act on, for the resolver in Phase 6b.
    pub fn reason(&self) -> &'static str {
        match self {
            Unmappable::MixedSideRange => "the range now spans both sides of the diff",
            Unmappable::FileLevelNoAnchor => "GitHub anchors comments on a line, not a file",
            Unmappable::BinaryFile => "the file is binary, so there is no line to anchor to",
            Unmappable::TooLargeFile => "the file is too large for GitHub to show a diff",
            Unmappable::LineNotInDiff => "that line is no longer part of this diff",
        }
    }
}

/// Beyond this many diff lines in one file, GitHub stops rendering the diff and
/// refuses comments on it.
///
/// A heuristic, not a documented limit — GitHub publishes no exact number, and
/// it interacts with total-diff size too. Set high enough that only genuinely
/// enormous files trip it; the cost of guessing low is telling the reviewer a
/// comment is unplaceable when it would have been fine.
pub const TOO_LARGE_LINES: usize = 20_000;

/// What a submit will contain, once every draft has been placed.
#[derive(Debug, Default)]
pub struct Preflight<'a> {
    /// Review-level comments. These become the review body, not line comments.
    pub summary: Vec<&'a Comment>,
    /// Drafts that still anchor cleanly onto the diff.
    pub mappable: Vec<&'a Comment>,
    /// Drafts that do not, and why.
    pub unmappable: Vec<(&'a Comment, Unmappable)>,
}

/// Sort every draft into what can be sent, what cannot, and what is body text.
///
/// Only `LocalDraft` comments are considered: anything GitHub already has is
/// not ours to send again.
///
/// Checks run most-specific-first. A binary file technically also has "no such
/// line", but telling the reviewer *binary* explains the problem while telling
/// them *line not in diff* invites them to go looking for a line that could
/// never have existed.
pub fn preflight<'a>(drafts: &'a [Comment], files: &[DiffFile]) -> Preflight<'a> {
    let mut out = Preflight::default();
    for comment in drafts.iter().filter(|c| c.is_editable()) {
        match &comment.scope {
            CommentScope::Review => out.summary.push(comment),
            CommentScope::File { .. } => {
                out.unmappable.push((comment, Unmappable::FileLevelNoAnchor));
            }
            CommentScope::Line { path, line, side } => {
                match classify(files, path, &[(*line, *side)]) {
                    Some(reason) => out.unmappable.push((comment, reason)),
                    None => out.mappable.push(comment),
                }
            }
            CommentScope::Range {
                path,
                start_line,
                end_line,
                side,
            } => match classify(files, path, &[(*start_line, *side), (*end_line, *side)]) {
                Some(reason) => out.unmappable.push((comment, reason)),
                None => out.mappable.push(comment),
            },
        }
    }
    out
}

/// Why these anchors cannot be used, or `None` when they all still fit.
fn classify(files: &[DiffFile], path: &str, anchors: &[(u32, Side)]) -> Option<Unmappable> {
    // Not `?` — returning None here would mean "maps fine", so a comment on a
    // file that has left the diff entirely would sail through preflight and
    // 422 at submit. The absent file is the most complete form of the anchor
    // being gone.
    let Some(file) = files.iter().find(|f| f.display_path() == path) else {
        return Some(Unmappable::LineNotInDiff);
    };
    if file.kind == FileKind::Binary {
        return Some(Unmappable::BinaryFile);
    }
    let line_count: usize = file.hunks.iter().map(|h| h.lines.len()).sum();
    if line_count > TOO_LARGE_LINES {
        return Some(Unmappable::TooLargeFile);
    }

    let found: Vec<bool> = anchors
        .iter()
        .map(|(line, side)| line_present(file, *line, *side))
        .collect();
    if found.iter().all(|f| *f) {
        return None;
    }
    // One end still present and the other gone means the range straddles a
    // change that landed between writing and submitting, which is exactly the
    // shape GitHub rejects as a mixed-side range.
    if anchors.len() > 1 && found.iter().any(|f| *f) {
        return Some(Unmappable::MixedSideRange);
    }
    Some(Unmappable::LineNotInDiff)
}

/// Whether `line` exists on `side` of this file's diff.
fn line_present(file: &DiffFile, line: u32, side: Side) -> bool {
    file.hunks.iter().flat_map(|h| &h.lines).any(|l| match side {
        Side::Old => l.kind != LineKind::Added && l.old_lineno == Some(line),
        Side::New => l.kind != LineKind::Removed && l.new_lineno == Some(line),
    })
}

/// What the reviewer chose for a draft that would not map (§7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Resolution {
    /// Keep it, appended to the review body. The default — losing a written
    /// comment silently is the worse failure.
    #[default]
    MoveToSummary,
    /// Drop it from this submit.
    Omit,
}

/// The review body: the reviewer's own summary text, plus anything rescued
/// from a failed mapping under an `## Unplaced comments` heading (§7).
///
/// The heading only appears when something was actually moved, so an ordinary
/// review body is not decorated with an empty section.
pub fn review_body(summary: &[&Comment], moved: &[&Comment]) -> String {
    let mut body = summary
        .iter()
        .map(|c| c.body.trim())
        .filter(|b| !b.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");

    if !moved.is_empty() {
        if !body.is_empty() {
            body.push_str("\n\n");
        }
        body.push_str("## Unplaced comments\n");
        for comment in moved {
            body.push_str(&format!("\n**{}**\n\n{}\n", anchor_label(comment), comment.body.trim()));
        }
    }
    body
}

/// How an unplaced comment names the place it was meant for.
///
/// Without this the reviewer reads a list of orphaned paragraphs and cannot
/// tell what any of them referred to.
fn anchor_label(comment: &Comment) -> String {
    match &comment.scope {
        CommentScope::Review => "review".to_string(),
        CommentScope::File { path } => path.clone(),
        CommentScope::Line { path, line, .. } => format!("{path}:{line}"),
        CommentScope::Range {
            path,
            start_line,
            end_line,
            ..
        } => format!("{path}:{start_line}-{end_line}"),
    }
}

/// What kind of review to submit (§7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Comment,
    Approve,
    RequestChanges,
    /// A pending review; GitHub wants the `event` field omitted entirely.
    Draft,
}

impl Event {
    /// The value GitHub expects, or `None` for a draft.
    pub fn api_value(&self) -> Option<&'static str> {
        match self {
            Event::Comment => Some("COMMENT"),
            Event::Approve => Some("APPROVE"),
            Event::RequestChanges => Some("REQUEST_CHANGES"),
            Event::Draft => None,
        }
    }
}

/// Why a submit is not allowed to proceed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// Nothing to say, and not an approval.
    Empty,
}

impl Refused {
    pub fn reason(&self) -> &'static str {
        match self {
            Refused::Empty => "add a comment, or approve without one",
        }
    }
}

/// Whether this review can be sent (§7).
///
/// A bare approve with nothing attached is a real and common review — "looks
/// good" needs no words. Everything else must carry something, because an
/// empty COMMENT review is noise on the PR and almost always a mis-key.
pub fn check(event: Event, body: &str, comment_count: usize) -> Result<(), Refused> {
    if event == Event::Approve {
        return Ok(());
    }
    if body.trim().is_empty() && comment_count == 0 {
        return Err(Refused::Empty);
    }
    Ok(())
}

/// The JSON body for `gh api repos/O/R/pulls/N/reviews --method POST --input -`.
///
/// `commit_id` must be the head the drafts were written against — §5 warns that
/// sending a different one makes GitHub 422 when a strict subset of commits is
/// in play.
///
/// `event` is omitted entirely for a draft rather than sent as null: GitHub
/// treats a present-but-empty event as an error.
pub fn payload(
    commit_id: &str,
    body: &str,
    event: Event,
    comments: &[&Comment],
) -> serde_json::Value {
    let mut root = serde_json::Map::new();
    root.insert("commit_id".into(), commit_id.into());
    root.insert("body".into(), body.into());
    if let Some(value) = event.api_value() {
        root.insert("event".into(), value.into());
    }
    let items: Vec<serde_json::Value> = comments.iter().filter_map(|c| comment_json(c)).collect();
    if !items.is_empty() {
        root.insert("comments".into(), items.into());
    }
    serde_json::Value::Object(root)
}

/// One entry of the `comments` array, or `None` for a scope that has no place
/// there — those went to the body instead.
fn comment_json(comment: &Comment) -> Option<serde_json::Value> {
    let mut item = serde_json::Map::new();
    match &comment.scope {
        CommentScope::Review | CommentScope::File { .. } => return None,
        CommentScope::Line { path, line, side } => {
            item.insert("path".into(), path.clone().into());
            item.insert("line".into(), (*line).into());
            item.insert("side".into(), api_side(*side).into());
        }
        CommentScope::Range {
            path,
            start_line,
            end_line,
            side,
        } => {
            item.insert("path".into(), path.clone().into());
            item.insert("start_line".into(), (*start_line).into());
            item.insert("start_side".into(), api_side(*side).into());
            item.insert("line".into(), (*end_line).into());
            item.insert("side".into(), api_side(*side).into());
        }
    }
    item.insert("body".into(), comment.body.clone().into());
    Some(serde_json::Value::Object(item))
}

/// GitHub names the sides `LEFT` and `RIGHT`, not old and new.
fn api_side(side: Side) -> &'static str {
    match side {
        Side::Old => "LEFT",
        Side::New => "RIGHT",
    }
}

/// A submit in progress: what preflight found, plus what the reviewer chose.
///
/// Separate from `Preflight` because that is a fact about the diff while this
/// is a set of decisions about it. Keeping the decisions in a plain struct is
/// what lets the whole submit be assembled and checked before any modal exists.
#[derive(Debug, Clone)]
pub struct Submission {
    /// What the reviewer typed as the review body, on top of any review-level
    /// drafts they already had.
    pub summary: String,
    pub event: Event,
    /// Only the drafts the reviewer changed their mind about. Anything absent
    /// takes `Resolution::default()`, so the safe choice needs no interaction.
    choices: std::collections::HashMap<uuid::Uuid, Resolution>,
}

impl Submission {
    pub fn new(event: Event) -> Self {
        Self {
            summary: String::new(),
            event,
            choices: std::collections::HashMap::new(),
        }
    }

    pub fn resolution(&self, id: uuid::Uuid) -> Resolution {
        self.choices.get(&id).copied().unwrap_or_default()
    }

    /// Flip one unmappable draft between being rescued and being dropped.
    pub fn toggle(&mut self, id: uuid::Uuid) {
        let next = match self.resolution(id) {
            Resolution::MoveToSummary => Resolution::Omit,
            Resolution::Omit => Resolution::MoveToSummary,
        };
        self.choices.insert(id, next);
    }

    /// The unmappable drafts being rescued into the body.
    pub fn moved<'a>(&self, pre: &Preflight<'a>) -> Vec<&'a Comment> {
        pre.unmappable
            .iter()
            .filter(|(c, _)| self.resolution(c.id) == Resolution::MoveToSummary)
            .map(|(c, _)| *c)
            .collect()
    }

    /// The full review body: review-level drafts, the reviewer's summary, and
    /// anything rescued under `## Unplaced comments`.
    pub fn body(&self, pre: &Preflight) -> String {
        let mut parts: Vec<String> = pre
            .summary
            .iter()
            .map(|c| c.body.trim().to_string())
            .filter(|b| !b.is_empty())
            .collect();
        if !self.summary.trim().is_empty() {
            parts.push(self.summary.trim().to_string());
        }
        let typed = parts.join("\n\n");
        let moved = self.moved(pre);
        if moved.is_empty() {
            return typed;
        }
        // Reuse the same assembly as review_body so the heading and per-anchor
        // formatting cannot drift between the two paths.
        let placeholder = Comment::new_review(&typed);
        if typed.is_empty() {
            review_body(&[], &moved)
        } else {
            review_body(&[&placeholder], &moved)
        }
    }

    /// Whether this submit may be sent (§7).
    pub fn check(&self, pre: &Preflight) -> Result<(), Refused> {
        check(self.event, &self.body(pre), pre.mappable.len())
    }

    /// The create-review JSON.
    pub fn payload(&self, commit_id: &str, pre: &Preflight) -> serde_json::Value {
        payload(commit_id, &self.body(pre), self.event, &pre.mappable)
    }

    /// The drafts this submit actually sends, and which therefore change
    /// lifecycle when it succeeds.
    ///
    /// Omitted drafts are **not** included: they were deliberately held back
    /// and stay editable so the reviewer can send them in a later review.
    pub fn sent_ids(&self, pre: &Preflight) -> Vec<uuid::Uuid> {
        pre.summary
            .iter()
            .chain(pre.mappable.iter())
            .map(|c| c.id)
            .chain(self.moved(pre).iter().map(|c| c.id))
            .collect()
    }

    /// What a sent draft becomes once GitHub has it.
    ///
    /// A pending review leaves them as pushed drafts — GitHub has them but
    /// nobody can see them yet, so they are neither local nor published.
    pub fn landed(&self) -> Lifecycle {
        match self.event {
            Event::Draft => Lifecycle::PushedDraft,
            _ => Lifecycle::Submitted,
        }
    }
}

#[cfg(test)]
mod submission_tests {
    use super::*;
    use diffident_diff::parser::parse;

    const DIFF: &str = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,2 +1,2 @@\n ctx\n-old\n+new\n";

    fn drafts() -> Vec<Comment> {
        vec![
            Comment::new_review("overall fine"),
            Comment::new_line("a.rs", 2, Side::New, "mappable nit"),
            Comment::new_file("a.rs", "file note"), // unmappable
        ]
    }

    #[test]
    fn an_unmappable_draft_is_rescued_by_default() {
        // Silently losing a written comment is the worse failure, so the safe
        // choice needs no interaction.
        let d = drafts();
        let pre = preflight(&d, &parse(DIFF));
        let s = Submission::new(Event::Comment);
        assert_eq!(s.moved(&pre).len(), 1);
        assert!(s.body(&pre).contains("## Unplaced comments"));
    }

    #[test]
    fn toggling_drops_it_from_the_body() {
        let d = drafts();
        let pre = preflight(&d, &parse(DIFF));
        let mut s = Submission::new(Event::Comment);
        s.toggle(pre.unmappable[0].0.id);
        assert!(s.moved(&pre).is_empty());
        assert!(!s.body(&pre).contains("## Unplaced comments"));
    }

    #[test]
    fn toggling_twice_returns_to_rescuing_it() {
        let d = drafts();
        let pre = preflight(&d, &parse(DIFF));
        let mut s = Submission::new(Event::Comment);
        let id = pre.unmappable[0].0.id;
        s.toggle(id);
        s.toggle(id);
        assert_eq!(s.resolution(id), Resolution::MoveToSummary);
    }

    #[test]
    fn the_body_carries_review_drafts_and_the_typed_summary() {
        let d = drafts();
        let pre = preflight(&d, &parse(DIFF));
        let mut s = Submission::new(Event::Comment);
        s.summary = "and one more thought".into();
        let body = s.body(&pre);
        assert!(body.contains("overall fine"), "got: {body}");
        assert!(body.contains("and one more thought"));
    }

    #[test]
    fn an_empty_comment_review_is_refused_but_a_bare_approve_is_not() {
        let empty: Vec<Comment> = Vec::new();
        let pre = preflight(&empty, &parse(DIFF));
        assert_eq!(Submission::new(Event::Comment).check(&pre), Err(Refused::Empty));
        assert_eq!(Submission::new(Event::Approve).check(&pre), Ok(()));
    }

    #[test]
    fn the_payload_sends_only_the_mappable_comments() {
        let d = drafts();
        let pre = preflight(&d, &parse(DIFF));
        let s = Submission::new(Event::Comment);
        let p = s.payload("abc123", &pre);
        assert_eq!(p["commit_id"], "abc123");
        assert_eq!(p["comments"].as_array().unwrap().len(), 1, "one mappable line comment");
    }

    #[test]
    fn everything_sent_changes_lifecycle_and_omitted_drafts_do_not() {
        let d = drafts();
        let pre = preflight(&d, &parse(DIFF));
        let mut s = Submission::new(Event::Comment);
        assert_eq!(s.sent_ids(&pre).len(), 3, "review + mappable + rescued");
        s.toggle(pre.unmappable[0].0.id); // omit the file comment
        assert_eq!(s.sent_ids(&pre).len(), 2, "the omitted one stays a local draft");
    }

    #[test]
    fn a_published_review_marks_its_drafts_submitted() {
        assert_eq!(Submission::new(Event::Comment).landed(), Lifecycle::Submitted);
        assert_eq!(Submission::new(Event::Approve).landed(), Lifecycle::Submitted);
    }

    #[test]
    fn a_pending_review_leaves_its_drafts_pushed_not_published() {
        // GitHub has them but nobody can see them yet.
        assert_eq!(Submission::new(Event::Draft).landed(), Lifecycle::PushedDraft);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use diffident_diff::parser::parse;
    use diffident_model::comment::Lifecycle;

    // a.rs keeps line 1, deletes old line 2, adds new line 2.
    const DIFF: &str = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,2 +1,2 @@\n ctx\n-old\n+new\n";
    const BINARY: &str = "diff --git a/x.png b/x.png\nBinary files a/x.png and b/x.png differ\n";

    fn files() -> Vec<DiffFile> {
        parse(DIFF)
    }

    #[test]
    fn a_line_still_in_the_diff_maps() {
        let drafts = vec![Comment::new_line("a.rs", 2, Side::New, "nit")];
        let p = preflight(&drafts, &files());
        assert_eq!(p.mappable.len(), 1);
        assert!(p.unmappable.is_empty());
    }

    #[test]
    fn a_line_that_left_the_diff_is_unmappable() {
        let drafts = vec![Comment::new_line("a.rs", 99, Side::New, "nit")];
        let p = preflight(&drafts, &files());
        assert_eq!(p.unmappable[0].1, Unmappable::LineNotInDiff);
    }

    #[test]
    fn a_comment_on_a_file_that_vanished_entirely_is_unmappable() {
        let drafts = vec![Comment::new_line("gone.rs", 1, Side::New, "nit")];
        let p = preflight(&drafts, &files());
        assert_eq!(p.unmappable[0].1, Unmappable::LineNotInDiff);
    }

    #[test]
    fn a_deleted_line_maps_on_the_old_side() {
        let drafts = vec![Comment::new_line("a.rs", 2, Side::Old, "why?")];
        let p = preflight(&drafts, &files());
        assert_eq!(p.mappable.len(), 1, "the pre-image line is still commentable");
    }

    #[test]
    fn asking_for_a_deleted_line_on_the_new_side_does_not_map() {
        // Old line 2 was removed; there is no new line 2 with that content.
        let drafts = vec![Comment::new_line("a.rs", 9, Side::Old, "nit")];
        let p = preflight(&drafts, &files());
        assert_eq!(p.unmappable[0].1, Unmappable::LineNotInDiff);
    }

    #[test]
    fn a_binary_file_says_binary_rather_than_line_not_found() {
        // Both are technically true; only one tells the reviewer what to do.
        let drafts = vec![Comment::new_line("x.png", 1, Side::New, "nit")];
        let p = preflight(&drafts, &parse(BINARY));
        assert_eq!(p.unmappable[0].1, Unmappable::BinaryFile);
    }

    #[test]
    fn a_file_level_comment_has_nowhere_to_anchor() {
        let drafts = vec![Comment::new_file("a.rs", "this file needs work")];
        let p = preflight(&drafts, &files());
        assert_eq!(p.unmappable[0].1, Unmappable::FileLevelNoAnchor);
    }

    #[test]
    fn a_review_level_comment_becomes_body_text_not_a_line_comment() {
        let drafts = vec![Comment::new_review("overall fine")];
        let p = preflight(&drafts, &files());
        assert_eq!(p.summary.len(), 1);
        assert!(p.mappable.is_empty() && p.unmappable.is_empty());
    }

    #[test]
    fn a_range_with_both_ends_present_maps() {
        let drafts = vec![Comment::new_range("a.rs", 1, 2, Side::New, "block")];
        let p = preflight(&drafts, &files());
        assert_eq!(p.mappable.len(), 1);
    }

    #[test]
    fn a_range_with_one_end_gone_is_a_mixed_side_range() {
        // It now straddles a change that landed between writing and submitting,
        // which is exactly what GitHub rejects.
        let drafts = vec![Comment::new_range("a.rs", 1, 80, Side::New, "block")];
        let p = preflight(&drafts, &files());
        assert_eq!(p.unmappable[0].1, Unmappable::MixedSideRange);
    }

    #[test]
    fn a_range_with_both_ends_gone_is_simply_not_in_the_diff() {
        let drafts = vec![Comment::new_range("a.rs", 70, 80, Side::New, "block")];
        let p = preflight(&drafts, &files());
        assert_eq!(p.unmappable[0].1, Unmappable::LineNotInDiff);
    }

    #[test]
    fn a_comment_github_already_has_is_never_resubmitted() {
        let mut sent = Comment::new_line("a.rs", 2, Side::New, "already sent");
        sent.lifecycle = Lifecycle::Submitted;
        let drafts = vec![sent];
        let p = preflight(&drafts, &files());
        assert!(p.mappable.is_empty() && p.unmappable.is_empty() && p.summary.is_empty());
    }

    #[test]
    fn the_body_is_just_the_summary_when_nothing_was_moved() {
        let c = Comment::new_review("looks good");
        assert_eq!(review_body(&[&c], &[]), "looks good");
    }

    #[test]
    fn an_empty_review_has_an_empty_body() {
        assert_eq!(review_body(&[], &[]), "");
    }

    #[test]
    fn moved_comments_land_under_an_unplaced_heading_with_their_anchors() {
        let summary = Comment::new_review("overall fine");
        let orphan = Comment::new_line("a.rs", 12, Side::New, "this bit");
        let body = review_body(&[&summary], &[&orphan]);
        assert!(body.starts_with("overall fine"));
        assert!(body.contains("## Unplaced comments"));
        assert!(body.contains("a.rs:12"), "an orphan must say where it was for");
        assert!(body.contains("this bit"));
    }

    #[test]
    fn a_review_with_no_summary_still_gets_the_heading_for_its_orphans() {
        let orphan = Comment::new_file("a.rs", "file note");
        let body = review_body(&[], &[&orphan]);
        assert!(body.starts_with("## Unplaced comments"), "got: {body:?}");
        assert!(body.contains("a.rs"));
    }

    #[test]
    fn a_range_orphan_names_both_ends() {
        let orphan = Comment::new_range("a.rs", 4, 9, Side::New, "block");
        assert!(review_body(&[], &[&orphan]).contains("a.rs:4-9"));
    }

    #[test]
    fn a_bare_approve_needs_no_comment() {
        assert_eq!(check(Event::Approve, "", 0), Ok(()));
    }

    #[test]
    fn an_empty_comment_review_is_refused() {
        // An empty COMMENT review is noise on the PR and almost always a mis-key.
        assert_eq!(check(Event::Comment, "   ", 0), Err(Refused::Empty));
        assert_eq!(check(Event::RequestChanges, "", 0), Err(Refused::Empty));
        assert_eq!(check(Event::Draft, "", 0), Err(Refused::Empty));
    }

    #[test]
    fn a_review_with_only_a_body_is_allowed() {
        assert_eq!(check(Event::Comment, "some thoughts", 0), Ok(()));
    }

    #[test]
    fn a_review_with_only_line_comments_is_allowed() {
        assert_eq!(check(Event::Comment, "", 1), Ok(()));
    }

    #[test]
    fn the_payload_carries_the_commit_body_and_event() {
        let p = payload("abc123", "hello", Event::Approve, &[]);
        assert_eq!(p["commit_id"], "abc123");
        assert_eq!(p["body"], "hello");
        assert_eq!(p["event"], "APPROVE");
    }

    #[test]
    fn a_draft_omits_the_event_field_entirely() {
        // GitHub treats a present-but-empty event as an error.
        let p = payload("abc123", "hello", Event::Draft, &[]);
        assert!(p.get("event").is_none(), "got: {p}");
    }

    #[test]
    fn a_line_comment_uses_githubs_side_names() {
        let c = Comment::new_line("a.rs", 12, Side::Old, "nit");
        let p = payload("abc", "", Event::Comment, &[&c]);
        let item = &p["comments"][0];
        assert_eq!(item["path"], "a.rs");
        assert_eq!(item["line"], 12);
        assert_eq!(item["side"], "LEFT", "Old is LEFT, not 'old'");
        assert_eq!(item["body"], "nit");
    }

    #[test]
    fn a_range_comment_sends_both_ends() {
        let c = Comment::new_range("a.rs", 4, 9, Side::New, "block");
        let p = payload("abc", "", Event::Comment, &[&c]);
        let item = &p["comments"][0];
        assert_eq!(item["start_line"], 4);
        assert_eq!(item["start_side"], "RIGHT");
        assert_eq!(item["line"], 9);
        assert_eq!(item["side"], "RIGHT");
    }

    #[test]
    fn a_review_with_no_line_comments_omits_the_comments_array() {
        // Sending an empty array is accepted but noisy; omitting is cleaner.
        let p = payload("abc", "body only", Event::Comment, &[]);
        assert!(p.get("comments").is_none());
    }

    #[test]
    fn body_scoped_comments_never_leak_into_the_comments_array() {
        let review = Comment::new_review("summary");
        let file = Comment::new_file("a.rs", "file note");
        let p = payload("abc", "", Event::Comment, &[&review, &file]);
        assert!(p.get("comments").is_none(), "got: {p}");
    }

    #[test]
    fn moving_to_summary_is_the_default_resolution() {
        // Losing a written comment silently is the worse failure.
        assert_eq!(Resolution::default(), Resolution::MoveToSummary);
    }

    #[test]
    fn every_unmappable_reason_can_explain_itself() {
        for reason in [
            Unmappable::MixedSideRange,
            Unmappable::FileLevelNoAnchor,
            Unmappable::BinaryFile,
            Unmappable::TooLargeFile,
            Unmappable::LineNotInDiff,
        ] {
            assert!(!reason.reason().is_empty());
        }
    }
}
