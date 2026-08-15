//! Root view: a rail of open reviews on the left, the active review on the right.
//!
//! The rail is the whole point of diffident — N reviews resident in ONE window,
//! rather than one OS window per PR.

use diffident_model::Review;
use gpui::{Context, Window, div, prelude::*, px};

use crate::theme::Theme;

pub struct Workspace {
    reviews: Vec<Review>,
    active: usize,
    theme: Theme,
}

impl Workspace {
    pub fn new(reviews: Vec<Review>) -> Self {
        Self {
            reviews,
            active: 0,
            theme: Theme::dark(),
        }
    }

    // `use<>` — Rust 2024 would otherwise capture the `&mut Context` borrow in the
    // returned opaque type, making the render loop below a double-borrow.
    fn render_row(&self, ix: usize, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let review = &self.reviews[ix];
        let theme = &self.theme;
        let selected = ix == self.active;

        div()
            .id(("review", ix))
            .flex()
            .flex_col()
            .gap_1()
            .px_3()
            .py_2()
            .rounded_md()
            .when(selected, |this| this.bg(theme.row_selected))
            .hover(|this| this.bg(theme.row_hover))
            .child(
                div()
                    .flex()
                    .justify_between()
                    .items_center()
                    .gap_2()
                    .child(div().text_color(theme.text).child(review.branch.clone()))
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .text_sm()
                            .child(
                                div()
                                    .text_color(theme.added)
                                    .child(format!("+{}", review.added)),
                            )
                            .child(
                                div()
                                    .text_color(theme.removed)
                                    .child(format!("-{}", review.removed)),
                            ),
                    ),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(theme.text_muted)
                    .child(review.subtitle()),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.active = ix;
                cx.notify();
            }))
    }
}

impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let active_branch = self.reviews[self.active].branch.clone();

        // A loop, not `.map()` — `cx` is a `&mut` that can't escape an `FnMut` closure.
        let mut rows = Vec::with_capacity(self.reviews.len());
        for ix in 0..self.reviews.len() {
            rows.push(self.render_row(ix, cx));
        }

        div()
            .flex()
            .size_full()
            .bg(self.theme.bg)
            .font_family("Zed Plex Mono")
            .text_color(self.theme.text)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .w(px(280.))
                    .h_full()
                    .p_2()
                    .border_r_1()
                    .border_color(self.theme.border)
                    .child(
                        div()
                            .px_3()
                            .py_2()
                            .text_sm()
                            .text_color(self.theme.text_muted)
                            .child("Reviews"),
                    )
                    .children(rows),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .child(div().text_xl().child(active_branch))
                    .child(
                        div()
                            .text_color(self.theme.text_muted)
                            .child("diff goes here — see IMPLEMENTATION_PLAN.md"),
                    ),
            )
    }
}
