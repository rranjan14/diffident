use crate::theme::Theme;
use diffident_model::{LoadState, Review};
use gpui::{IntoElement, ParentElement, SharedString, div, prelude::*, px};

/// Pixels of indent per level of stack nesting.
pub const INDENT: f32 = 14.;

pub fn indent_px(depth: usize) -> f32 {
    depth as f32 * INDENT
}

/// The rail's second line for a review: its number plus whatever its load state
/// can say.
///
/// A failed review keeps its error visible here rather than in a transient
/// toast — the reviewer needs to know which PR in a stack of four is the broken
/// one, and a toast is gone by the time they look.
pub fn rail_label(review: &Review) -> String {
    match &review.state {
        LoadState::Idle => review.subtitle(),
        LoadState::Loading => format!("{}  loading…", review.subtitle()),
        LoadState::Ready { added, removed, .. } => {
            format!("{}  +{added} -{removed}", review.subtitle())
        }
        LoadState::Failed { message } => format!("{}  failed: {message}", review.subtitle()),
    }
}

/// One rail row. `on_click` selects the review; the caller owns what that means.
pub fn rail_row(review: &Review, selected: bool, theme: &Theme) -> impl IntoElement + use<> {
    let connector = if review.depth > 0 { "└ " } else { "" };
    div()
        .flex()
        .flex_col()
        .gap_1()
        .py_2()
        .pr_3()
        .pl(px(12. + indent_px(review.depth)))
        .rounded_md()
        .when(selected, |this| this.bg(theme.row_selected))
        .hover(|this| this.bg(theme.row_hover))
        .child(
            div()
                .text_color(theme.text)
                .child(SharedString::from(format!("{connector}{}", review.branch))),
        )
        .child(
            div()
                .text_sm()
                .text_color(theme.text_muted)
                .child(SharedString::from(rail_label(review))),
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
            state,
        }
    }

    #[test]
    fn an_unloaded_review_shows_no_counts() {
        assert_eq!(rail_label(&review(1, 0, LoadState::Idle)), "#1");
    }

    #[test]
    fn a_loaded_review_shows_its_line_counts() {
        let r = review(
            1,
            0,
            LoadState::Ready {
                head_sha: "abc".into(),
                added: 12,
                removed: 3,
            },
        );
        assert_eq!(rail_label(&r), "#1  +12 -3");
    }

    #[test]
    fn a_loading_review_says_so_rather_than_looking_empty() {
        assert_eq!(rail_label(&review(1, 0, LoadState::Loading)), "#1  loading…");
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
        assert_eq!(rail_label(&r), "#1  failed: gh auth");
    }

    #[test]
    fn indent_grows_with_stack_depth() {
        assert_eq!(indent_px(0), 0.);
        assert!(indent_px(2) > indent_px(1));
    }
}
