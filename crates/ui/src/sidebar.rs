//! The sidebar: reviews above, the active review's files below.
//!
//! Stacked in one column rather than side by side because a review's files are
//! meaningless except under that review — the hierarchy is real, so the layout
//! shows it.

use crate::theme::Theme;
use gpui::{IntoElement, ParentElement, SharedString, div, prelude::*, px};

/// A sticky section label with its count right-aligned.
pub fn section_header(label: &str, count: usize, theme: &Theme) -> impl IntoElement {
    div()
        .flex()
        .justify_between()
        .items_center()
        .px(px(theme.s3))
        .py(px(theme.s2))
        .bg(theme.surface)
        .border_b_1()
        .border_color(theme.border_subtle)
        .text_size(px(theme.ui_xs))
        .text_color(theme.text_tertiary)
        .child(SharedString::from(label.to_uppercase()))
        .child(SharedString::from(count.to_string()))
}
