//! Root view: a rail of open reviews on the left, the active review on the right.
//!
//! The rail is the whole point of diffident (§1) — N reviews resident in ONE
//! window, rather than one OS window per PR.

use crate::diff_view::DiffView;
use crate::file_list::{file_entries, status_glyph};
use crate::loader::{LoadedReview, list_reviews, load_review};
use crate::navigate::*;
use crate::rail::rail_row;
use crate::theme::Theme;
use diffident_forge::{PrFilter, Repo, gh::Gh, github::GitHub};
use diffident_model::{LoadState, Review};
use gpui::{
    Context, Entity, FocusHandle, Focusable, IntoElement, ParentElement, Render, SharedString,
    Window, div, prelude::*, px,
};

pub struct Workspace {
    repo: Repo,
    reviews: Vec<Review>,
    active: Option<usize>,
    diff: Option<Entity<DiffView>>,
    theme: Theme,
    focus: FocusHandle,
    error: Option<String>,
}

impl Workspace {
    pub fn new(repo: Repo, open_pr: Option<u32>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            repo: repo.clone(),
            reviews: Vec::new(),
            active: None,
            diff: None,
            theme: Theme::dark(),
            focus: cx.focus_handle(),
            error: None,
        };
        this.refresh(open_pr, cx);
        this
    }

    /// Fetch the PR list off the main thread, then land it back on this entity.
    ///
    /// `gh` is a blocking subprocess, so it must not run on the foreground
    /// executor — a 400ms list call there freezes the window.
    fn refresh(&mut self, open_pr: Option<u32>, cx: &mut Context<Self>) {
        let repo = self.repo.clone();
        cx.spawn(async move |this, cx| {
            let listed = cx
                .background_executor()
                .spawn(async move { list_reviews(&GitHub::new(Gh), &repo, PrFilter::AllOpen) })
                .await;
            this.update(cx, |this, cx| {
                match listed {
                    Ok(reviews) => {
                        this.reviews = reviews;
                        this.error = None;
                        if let Some(number) = open_pr
                            && let Some(ix) = this.reviews.iter().position(|r| r.id.number == number)
                        {
                            this.select(ix, cx);
                        }
                    }
                    Err(e) => this.error = Some(e.to_string()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Open a review, fetching its diff if it is not already resident.
    fn select(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(review) = self.reviews.get_mut(ix) else {
            return;
        };
        self.active = Some(ix);
        review.state = LoadState::Loading;
        let (repo, number) = (self.repo.clone(), review.id.number);
        cx.notify();

        cx.spawn(async move |this, cx| {
            let loaded = cx
                .background_executor()
                .spawn(async move { load_review(&GitHub::new(Gh), &repo, number) })
                .await;
            this.update(cx, |this, cx| {
                match loaded {
                    Ok(loaded) => this.apply(number, loaded, cx),
                    Err(e) => {
                        if let Some(r) = this.reviews.iter_mut().find(|r| r.id.number == number) {
                            r.state = LoadState::Failed {
                                message: e.to_string(),
                            };
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Land a fetched diff, unless the reviewer has moved on (§5).
    fn apply(&mut self, number: u32, loaded: LoadedReview, cx: &mut Context<Self>) {
        let current = self.active.and_then(|ix| self.reviews.get(ix));
        if current.map(|r| r.id.number) != Some(number) {
            return; // stale: the reviewer switched away while this was in flight
        }
        if let Some(r) = self.reviews.iter_mut().find(|r| r.id.number == number) {
            r.state = LoadState::Ready {
                head_sha: loaded.request.head_sha.clone(),
                added: loaded.added,
                removed: loaded.removed,
            };
        }
        let theme = self.theme.clone();
        self.diff = Some(cx.new(|_| DiffView::new(loaded.files, loaded.rows, theme)));
    }

    fn move_cursor(&mut self, f: impl Fn(&[diffident_diff::Row], usize) -> usize, cx: &mut Context<Self>) {
        let Some(diff) = self.diff.clone() else {
            return;
        };
        diff.update(cx, |view, cx| {
            let next = f(view.rows(), view.cursor);
            view.scroll_to(next);
            cx.notify();
        });
    }
}

impl Focusable for Workspace {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme.clone();

        let mut rail = Vec::with_capacity(self.reviews.len());
        for ix in 0..self.reviews.len() {
            let selected = self.active == Some(ix);
            rail.push(
                div()
                    .id(("review", ix))
                    .child(rail_row(&self.reviews[ix], selected, &theme))
                    .on_click(cx.listener(move |this, _, _, cx| this.select(ix, cx))),
            );
        }

        // The file panel. Built here rather than inside the diff view because
        // clicking an entry scrolls that view — the parent owns the wiring
        // between the two panes.
        let mut file_rows = Vec::new();
        if let Some(diff) = self.diff.clone() {
            let entries = {
                let view = diff.read(cx);
                file_entries(view.files(), view.rows())
            };
            for entry in entries {
                let (row_ix, diff) = (entry.row_ix, diff.clone());
                file_rows.push(
                    div()
                        .id(("file", row_ix))
                        .flex()
                        .justify_between()
                        .gap_2()
                        .px_2()
                        .py_1()
                        .text_sm()
                        .rounded_md()
                        .hover(|this| this.bg(theme.row_hover))
                        .child(div().text_color(theme.text_muted).child(SharedString::from(
                            format!("{} {}", status_glyph(&entry.status), entry.path),
                        )))
                        .child(
                            div()
                                .flex()
                                .gap_1()
                                .child(
                                    div()
                                        .text_color(theme.added)
                                        .child(SharedString::from(format!("+{}", entry.added))),
                                )
                                .child(
                                    div()
                                        .text_color(theme.removed)
                                        .child(SharedString::from(format!("-{}", entry.removed))),
                                ),
                        )
                        .on_click(move |_, _, cx| {
                            diff.update(cx, |view, cx| {
                                view.scroll_to(row_ix);
                                cx.notify();
                            });
                        }),
                );
            }
        }

        div()
            .key_context("Diff")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &NextLine, _, cx| {
                this.move_cursor(|_, ix| ix + 1, cx)
            }))
            .on_action(cx.listener(|this, _: &PrevLine, _, cx| {
                this.move_cursor(|_, ix| ix.saturating_sub(1), cx)
            }))
            .on_action(cx.listener(|this, _: &NextHunk, _, cx| this.move_cursor(next_hunk, cx)))
            .on_action(cx.listener(|this, _: &PrevHunk, _, cx| this.move_cursor(prev_hunk, cx)))
            .on_action(cx.listener(|this, _: &NextFile, _, cx| this.move_cursor(next_file, cx)))
            .on_action(cx.listener(|this, _: &PrevFile, _, cx| this.move_cursor(prev_file, cx)))
            .on_action(cx.listener(|this, _: &Top, _, cx| this.move_cursor(|_, _| 0, cx)))
            .on_action(cx.listener(|this, _: &Bottom, _, cx| {
                this.move_cursor(|rows, _| rows.len().saturating_sub(1), cx)
            }))
            .flex()
            .size_full()
            .bg(theme.bg)
            .font_family(theme.font_family)
            .text_color(theme.text)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .w(px(300.))
                    .h_full()
                    .p_2()
                    .border_r_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .px_3()
                            .py_2()
                            .text_sm()
                            .text_color(theme.text_muted)
                            .child(SharedString::from(format!(
                                "{} — {} reviews",
                                self.repo.slug(),
                                self.reviews.len()
                            ))),
                    )
                    .children(self.error.clone().map(|e| {
                        div()
                            .px_3()
                            .text_sm()
                            .text_color(theme.removed)
                            .child(SharedString::from(e))
                    }))
                    .children(rail),
            )
            .children((!file_rows.is_empty()).then(|| {
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .w(px(320.))
                    .h_full()
                    .p_2()
                    .border_r_1()
                    .border_color(theme.border)
                    .children(file_rows)
            }))
            .child(match self.diff.clone() {
                Some(diff) => div().flex_1().h_full().child(diff).into_any_element(),
                None => div()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .text_color(theme.text_muted)
                    .child("select a review")
                    .into_any_element(),
            })
    }
}
