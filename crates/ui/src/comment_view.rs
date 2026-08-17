//! Drawing a comment, wherever it appears (§7).
//!
//! The same conversation shows up in three places — a local draft, a thread
//! beside the code, and a thread that has nowhere to sit beside. Each used to
//! build its own elements, and the suggestion block was byte-identical in all
//! three while the prose colour quietly differed at each one. That is the drift
//! duplication produces: nothing enforced that a resolved thread and a sent
//! draft looked muted in the same way, so nothing noticed when they stopped.
//!
//! Now the one thing that legitimately varies — whether this comment reads as
//! current or spent — is an argument.

use crate::suggest::{Segment, segments};
use crate::theme::Theme;
use diffident_forge::threads::ReviewThread;
use gpui::{IntoElement, ParentElement, SharedString, div, prelude::*, px};

/// Where a thread stands, in one word.
///
/// Resolved wins over outdated: a thread that has been dealt with is dealt
/// with, whether or not its code moved since.
pub fn status_label(thread: &ReviewThread) -> &'static str {
    if thread.is_resolved {
        "resolved"
    } else if thread.is_outdated {
        "outdated"
    } else {
        "open"
    }
}

/// Who wrote a comment, allowing for the account being gone.
///
/// GraphQL returns a null author rather than dropping the comment, so an empty
/// name means the account was deleted — not that the comment is anonymous.
pub fn author_label(author: &str) -> String {
    if author.is_empty() {
        "(deleted account)".to_string()
    } else {
        author.to_string()
    }
}

/// A comment body, with any ```suggestion fences drawn as proposed edits.
///
/// `muted` is the whole reason this takes a parameter rather than reading
/// state: a resolved thread, a sent draft and an unplaceable thread are all
/// "spent" for different reasons, and each caller is the only one that knows
/// which it is. Passing it makes that judgement visible at the call site.
///
/// The suggestion block ignores `muted` on purpose — a proposed edit is either
/// there or it is not, and dimming one would hide a change still waiting to be
/// accepted.
pub fn comment_body(body: &str, theme: &Theme, muted: bool) -> impl IntoElement {
    let prose = if muted { theme.text_muted } else { theme.text };
    div()
        .flex()
        .flex_col()
        .children(segments(body).into_iter().map(move |seg| match seg {
            Segment::Text(text) => div().text_color(prose).child(SharedString::from(text)),
            Segment::Suggestion(text) => div()
                .px_2()
                .py_1()
                .rounded_md()
                .bg(theme.added_bg)
                .border_l_2()
                .border_color(theme.added)
                .text_color(theme.added)
                .child(SharedString::from(text)),
        }))
}

/// The author line above a comment's body.
pub fn author_line(author: &str, theme: &Theme) -> impl IntoElement {
    div()
        .text_sm()
        .text_color(theme.text_muted)
        .child(SharedString::from(author_label(author)))
}

/// Space between one comment and the next in a thread.
pub const COMMENT_GAP: f32 = 4.0;

/// A whole thread's comments, author line and body each.
pub fn thread_comments(thread: &ReviewThread, theme: &Theme, muted: bool) -> Vec<gpui::AnyElement> {
    thread
        .comments
        .iter()
        .map(|c| {
            div()
                .flex()
                .flex_col()
                .pb(px(COMMENT_GAP))
                .child(author_line(&c.author, theme))
                .child(comment_body(&c.body, theme, muted))
                .into_any_element()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use diffident_forge::threads::ThreadComment;

    fn thread(resolved: bool, outdated: bool) -> ReviewThread {
        ReviewThread {
            id: "PRRT_1".into(),
            path: "a.rs".into(),
            line: Some(1),
            original_line: Some(1),
            on_old_side: false,
            is_resolved: resolved,
            is_outdated: outdated,
            comments: vec![ThreadComment {
                id: "PRRC_1".into(),
                author: String::new(),
                body: "nit".into(),
            }],
        }
    }

    #[test]
    fn an_untouched_thread_reads_as_open() {
        assert_eq!(status_label(&thread(false, false)), "open");
    }

    #[test]
    fn resolved_wins_over_outdated() {
        // A thread that has been dealt with is dealt with, whether or not its
        // code moved afterwards — and it usually did, which is *why* it is
        // outdated. Reporting "outdated" would hide that someone closed it.
        assert_eq!(status_label(&thread(true, true)), "resolved");
        assert_eq!(status_label(&thread(false, true)), "outdated");
    }

    #[test]
    fn a_missing_author_says_the_account_is_gone_rather_than_nothing() {
        // GraphQL nulls the author instead of dropping the comment, so an
        // empty name is a deleted account. Rendering it blank would read as an
        // anonymous comment, which is not a thing GitHub has.
        assert_eq!(author_label(""), "(deleted account)");
        assert_eq!(author_label("octocat"), "octocat");
    }
}
