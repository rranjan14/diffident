use crate::theme::Theme;
use diffident_model::{LoadState, Review};
use gpui::{IntoElement, ParentElement, SharedString, div, prelude::*, px};

/// Pixels of indent per level of stack nesting.
pub const INDENT: f32 = 14.;

pub fn indent_px(depth: usize) -> f32 {
    depth as f32 * INDENT
}

/// The rail's second line for a review: its number, plus whatever its load
/// state can say, plus how much is left to read.
///
/// `unreviewed` is only meaningful once the review is `Ready` — before that we
/// do not know the file list, so the caller passes 0 and nothing is shown.
///
/// A failed review keeps its error visible here rather than in a transient
/// toast — the reviewer needs to know which PR in a stack of four is the broken
/// one, and a toast is gone by the time they look.
pub fn rail_label(review: &Review, unreviewed: usize) -> String {
    match &review.state {
        LoadState::Idle => review.subtitle(),
        LoadState::Loading => format!("{}  loading…", review.subtitle()),
        LoadState::Ready { added, removed, .. } => {
            let left = if unreviewed == 0 {
                "done".to_string()
            } else {
                format!("{unreviewed} left")
            };
            let rebased = if review.rebased { "  rebased" } else { "" };
            format!("{}  +{added} -{removed}  {left}{rebased}", review.subtitle())
        }
        LoadState::Failed { message } => format!("{}  failed: {message}", review.subtitle()),
    }
}

/// One rail row. `on_click` selects the review; the caller owns what that means.
pub fn rail_row(
    review: &Review,
    selected: bool,
    unreviewed: usize,
    theme: &Theme,
) -> impl IntoElement + use<> {
    let connector = if review.depth > 0 { "└ " } else { "" };
    div()
        .flex()
        .flex_col()
        .gap_1()
        .py_2()
        .pr_3()
        .pl(px(12. + indent_px(review.depth)))
        .rounded_md()
        .when(selected, |this| this.bg(theme.accent_soft))
        .hover(|this| this.bg(theme.surface_raised))
        .child(
            div()
                .text_color(theme.text_primary)
                .child(SharedString::from(format!("{connector}{}", review.branch))),
        )
        .child(
            div()
                .text_sm()
                .text_color(theme.text_secondary)
                .child(SharedString::from(rail_label(review, unreviewed))),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use diffident_model::ReviewId;

    fn review(number: u32, depth: usize, state: LoadState) -> Review {
        Review {
            id: ReviewId {
                repo: "o/r".into(),
                number,
            },
            title: "t".into(),
            branch: format!("branch-{number}"),
            depth,
            is_draft: false,
            head_sha: String::new(),
            rebased: false,
            state,
        }
    }

    #[test]
    fn an_unloaded_review_shows_no_counts() {
        assert_eq!(rail_label(&review(1, 0, LoadState::Idle), 0), "#1");
    }

    #[test]
    fn a_loaded_review_shows_its_line_counts() {
        let r = review(
            1,
            0,
            LoadState::Ready {
                added: 12,
                removed: 3,
                files: Vec::new(),
                head_sha: "abc".into(),
            },
        );
        assert_eq!(rail_label(&r, 0), "#1  +12 -3  done");
    }

    #[test]
    fn a_loading_review_says_so_rather_than_looking_empty() {
        assert_eq!(rail_label(&review(1, 0, LoadState::Loading), 0), "#1  loading…");
    }

    #[test]
    fn a_failed_review_surfaces_its_error_in_the_rail() {
        let r = review(
            1,
            0,
            LoadState::Failed {
                message: "gh auth".into(),
            },
        );
        assert_eq!(rail_label(&r, 0), "#1  failed: gh auth");
    }

    #[test]
    fn a_loaded_review_shows_how_many_files_are_left() {
        let r = review(
            1,
            0,
            LoadState::Ready {
                added: 12,
                removed: 3,
                files: vec![("a.rs".into(), 1), ("b.rs".into(), 1), ("c.rs".into(), 1)],
                head_sha: "abc".into(),
            },
        );
        assert_eq!(rail_label(&r, 2), "#1  +12 -3  2 left");
    }

    #[test]
    fn a_fully_reviewed_pr_says_so_rather_than_showing_zero() {
        let r = review(
            1,
            0,
            LoadState::Ready {
                added: 12,
                removed: 3,
                files: vec![("a.rs".into(), 1)],
                head_sha: "abc".into(),
            },
        );
        assert_eq!(rail_label(&r, 0), "#1  +12 -3  done");
    }

    #[test]
    fn an_unloaded_review_shows_no_count_because_we_do_not_know_its_files() {
        // gh's list API caps file lists at 100 per PR, so a pre-fetched count
        // would silently undercount. No badge beats a wrong badge.
        assert_eq!(rail_label(&review(1, 0, LoadState::Idle), 0), "#1");
    }

    #[test]
    fn a_rebased_review_is_flagged_in_the_rail() {
        let mut r = review(
            1,
            0,
            LoadState::Ready {
                added: 12,
                removed: 3,
                files: vec![("a.rs".into(), 1)],
                head_sha: "abc".into(),
            },
        );
        r.rebased = true;
        assert_eq!(rail_label(&r, 1), "#1  +12 -3  1 left  rebased");
    }

    #[test]
    fn an_unrebased_review_says_nothing_extra() {
        let r = review(
            1,
            0,
            LoadState::Ready {
                added: 12,
                removed: 3,
                files: vec![("a.rs".into(), 1)],
                head_sha: "abc".into(),
            },
        );
        assert!(!rail_label(&r, 1).contains("rebased"));
    }

    #[test]
    fn indent_grows_with_stack_depth() {
        assert_eq!(indent_px(0), 0.);
        assert!(indent_px(2) > indent_px(1));
    }
}
