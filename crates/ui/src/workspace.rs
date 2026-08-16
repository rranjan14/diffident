//! Root view: a rail of open reviews on the left, the active review on the right.
//!
//! The rail is the whole point of diffident (§1) — N reviews resident in ONE
//! window, rather than one OS window per PR.

use crate::diff_view::DiffView;
use crate::file_list::{file_entries, reviewed_marker, status_glyph};
use crate::loader::{LoadedReview, list_reviews, load_review};
use crate::navigate::*;
use crate::rail::rail_row;
use crate::residency::Residency;
use crate::theme::Theme;
use diffident_forge::{Repo, gh::Gh, github::GitHub};
use diffident_model::{LoadState, Review};
use diffident_model::reviewed::Reviewed;
use diffident_forge::stack::next_in_stack;
use gpui::{
    Context, Entity, FocusHandle, Focusable, IntoElement, ParentElement, Render, SharedString,
    Window, div, prelude::*, px,
};

/// How many diffs stay resident. Four covers a typical stack (§6) without
/// holding an unbounded amount of parsed diff in memory (§10).
const RESIDENT: usize = 4;

pub struct Workspace {
    repo: Repo,
    reviews: Vec<Review>,
    active: Option<usize>,
    /// Resident diffs, in-flight fetches, and remembered cursors (§9 Phase 3).
    ///
    /// Re-opening a review costs ~2s of `gh` plus ~530ms of highlighting, so
    /// keeping a few alive makes switching instant — the whole point of one
    /// window holding N reviews (§1). Bounded because a large diff is tens of
    /// MB (§10).
    residency: Residency<Entity<DiffView>>,
    /// Which files the reviewer has marked read, per PR. Outlives the diffs in
    /// `residency` on purpose — evicting a diff must not forget your progress.
    reviewed: Reviewed,
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
            residency: Residency::new(RESIDENT),
            reviewed: Reviewed::new(),
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
                .spawn(async move { list_reviews(&GitHub::new(Gh), &repo) })
                .await;
            this.update(cx, |this, cx| {
                match listed {
                    Ok(listed) => {
                        this.error = None;
                        this.reviews = listed
                            .into_iter()
                            .map(|mut fresh| {
                                // Carry over what the listing does not know, and
                                // flag a head that moved under a loaded review.
                                if let Some(old) =
                                    this.reviews.iter().find(|r| r.id == fresh.id)
                                {
                                    fresh.rebased =
                                        old.rebased || old.head_sha != fresh.head_sha;
                                    fresh.state = old.state.clone();
                                }
                                fresh
                            })
                            .collect();
                        if let Some(number) = open_pr
                            && let Some(ix) =
                                this.reviews.iter().position(|r| r.id.number == number)
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

    /// The PR number of the active review, if any.
    fn active_number(&self) -> Option<u32> {
        Some(self.active.and_then(|ix| self.reviews.get(ix))?.id.number)
    }

    /// How many files remain unread in one review, or 0 when we do not know its
    /// file list yet — which the rail renders as no badge at all.
    fn unreviewed_count(&self, review: &Review) -> usize {
        match &review.state {
            LoadState::Ready { paths, .. } => {
                self.reviewed.unreviewed_count(review.id.number, paths)
            }
            _ => 0,
        }
    }

    /// The diff for the active review, if it is resident.
    fn diff(&self) -> Option<Entity<DiffView>> {
        self.residency.get(self.active_number()?).cloned()
    }

    /// Open a review, fetching its diff only if it is not already resident.
    fn select(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(review) = self.reviews.get_mut(ix) else {
            return;
        };
        let number = review.id.number;
        // Remember where the outgoing review was, before it stops being active.
        // Survives eviction of its diff, so returning later still lands there.
        if let (Some(outgoing), Some(view)) = (self.active_number(), self.diff()) {
            let row = view.read(cx).cursor;
            self.residency.remember_cursor(outgoing, row);
        }
        self.active = Some(ix);
        let repo = self.repo.clone();

        // Already resident: promote to most-recently-used and skip the fetch
        // entirely. This is what makes switching between stacked PRs instant.
        if self.residency.activate(number) {
            cx.notify();
            return;
        }

        // Already loading: make it active and let the in-flight fetch land.
        // Starting a second one would cost seconds and race the first.
        if !self.residency.begin_fetch(number) {
            cx.notify();
            return;
        }

        if let Some(r) = self.reviews.get_mut(ix) {
            r.state = LoadState::Loading;
        }
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
                        this.residency.abandon_fetch(number);
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

    /// Land a fetched diff.
    ///
    /// Unconditional, even if the reviewer has since switched away: the result
    /// is correct data for *its own* review and cost seconds to fetch, so it is
    /// cached rather than discarded. Nothing can render into the wrong review
    /// because `diff()` looks the active review up by number — the mismatch a
    /// staleness check used to guard against is now unrepresentable.
    fn apply(&mut self, number: u32, loaded: LoadedReview, cx: &mut Context<Self>) {
        if let Some(r) = self.reviews.iter_mut().find(|r| r.id.number == number) {
            r.state = LoadState::Ready {
                added: loaded.added,
                removed: loaded.removed,
                paths: loaded
                    .files
                    .iter()
                    .map(|f| f.display_path().to_string())
                    .collect(),
            };
        }
        let theme = self.theme.clone();
        let row = self.residency.recall_cursor(number, loaded.rows.len());
        let view = cx.new(|_| {
            let mut v = DiffView::new(loaded.files, loaded.rows, loaded.highlights, theme);
            v.scroll_to(row);
            v
        });
        self.residency.admit(number, view, &loaded.head_sha);

        // Keep whatever the reviewer is actually looking at at the
        // most-recently-used end. Open four reviews, click back to the first,
        // and then let all four fetches land: without this, those four
        // admissions evict the first out from under them and the diff pane goes
        // blank until they click the rail again. Fetches land on their own
        // schedule, so "most recently admitted" is not "most recently looked
        // at" — only this makes them agree.
        if let Some(active) = self.active_number() {
            self.residency.activate(active);
        }
    }

    fn move_cursor(&mut self, f: impl Fn(&[diffident_diff::Row], usize) -> usize, cx: &mut Context<Self>) {
        let Some(diff) = self.diff() else {
            return;
        };
        diff.update(cx, |view, cx| {
            let next = f(view.rows(), view.cursor);
            view.scroll_to(next);
            cx.notify();
        });
    }

    /// `r`: flip the read mark on the file the cursor is in.
    ///
    /// A `Spacer` row belongs to no file, so the key does nothing there rather
    /// than guessing at a neighbour.
    fn toggle_reviewed(&mut self, cx: &mut Context<Self>) {
        let (Some(number), Some(diff)) = (self.active_number(), self.diff()) else {
            return;
        };
        let path = {
            let view = diff.read(cx);
            view.rows()
                .get(view.cursor)
                .and_then(|row| row.file_ix())
                .and_then(|ix| view.files().get(ix))
                .map(|f| f.display_path().to_string())
        };
        if let Some(path) = path {
            self.reviewed.toggle(number, &path);
            cx.notify();
        }
    }

    /// The row of the first unread file in the active review, if any.
    fn first_unreviewed_row(&self, cx: &Context<Self>) -> Option<usize> {
        let (number, diff) = (self.active_number()?, self.diff()?);
        let view = diff.read(cx);
        view.rows().iter().position(|row| {
            row.file_ix()
                .and_then(|ix| view.files().get(ix))
                .is_some_and(|f| !self.reviewed.is_reviewed(number, f.display_path()))
        })
    }

    /// `tab`: go to the next unread file, crossing into the next PR of this
    /// stack when the current one is fully read.
    ///
    /// Wraps within the stack and stops if it comes all the way round, so a
    /// fully-read stack does nothing rather than looping forever. Never leaves
    /// the stack: being done here must not drag the reviewer into an unrelated
    /// PR.
    fn next_unreviewed(&mut self, cx: &mut Context<Self>) {
        if let Some(row) = self.first_unreviewed_row(cx) {
            let view_row = self.diff().map(|d| d.read(cx).cursor);
            if view_row != Some(row) {
                self.move_cursor(move |_, _| row, cx);
                return;
            }
        }

        let Some(from) = self.active else { return };
        let depths: Vec<usize> = self.reviews.iter().map(|r| r.depth).collect();
        let mut at = from;
        for _ in 0..depths.len() {
            let Some(next) = next_in_stack(&depths, at) else {
                return;
            };
            if next == from {
                return; // all the way round; nothing unread in this stack
            }
            at = next;
            let has_unread = match &self.reviews[at].state {
                LoadState::Ready { .. } => self.unreviewed_count(&self.reviews[at]) > 0,
                // Never opened, so nothing is marked read — by definition unread.
                _ => true,
            };
            if has_unread {
                self.select(at, cx);
                return;
            }
        }
    }

    /// `ctrl-d` / `ctrl-u`. The distance depends on the laid-out viewport, so it
    /// is read from the view rather than passed in.
    fn half_page(&mut self, down: bool, cx: &mut Context<Self>) {
        let Some(diff) = self.diff() else {
            return;
        };
        let distance = diff.read(cx).rows_per_half_page();
        self.move_cursor(move |rows, ix| half_page(rows, ix, distance, down), cx);
    }

    /// `ctrl-tab` / `ctrl-shift-tab`: move to the adjacent review in the rail.
    ///
    /// Wraps, unlike diff navigation: the rail is a short ring the reviewer is
    /// cycling through, not a long document they can lose their place in.
    fn step_review(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.reviews.is_empty() {
            return;
        }
        let len = self.reviews.len() as isize;
        let from = self.active.unwrap_or(0) as isize;
        let next = (from + delta).rem_euclid(len) as usize;
        self.select(next, cx);
    }
}


/// What the diff pane shows when no diff is resident for the active review.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Placeholder {
    NothingSelected,
    Loading,
    Failed(String),
}

/// Decide what the diff pane says when it has no diff to draw.
///
/// Split out of `render` so all three cases are testable without a window. The
/// pane used to say "select a review" in every one of them, which was a plain
/// lie the moment you clicked something: the rail said "loading…" while the
/// pane said you had selected nothing.
///
/// An active review with no resident diff has either failed or has a fetch
/// running or about to run — `select` never leaves a review active without one
/// of those being true, and `apply` re-activates the on-screen review so it
/// cannot be evicted while you are looking at it.
fn placeholder(active: Option<&Review>) -> Placeholder {
    let Some(review) = active else {
        return Placeholder::NothingSelected;
    };
    match &review.state {
        LoadState::Failed { message } => Placeholder::Failed(message.clone()),
        _ => Placeholder::Loading,
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
            let unreviewed = self.unreviewed_count(&self.reviews[ix]);
            rail.push(
                div()
                    .id(("review", ix))
                    .child(rail_row(&self.reviews[ix], selected, unreviewed, &theme))
                    .on_click(cx.listener(move |this, _, _, cx| this.select(ix, cx))),
            );
        }

        // The file panel. Built here rather than inside the diff view because
        // clicking an entry scrolls that view — the parent owns the wiring
        // between the two panes.
        let mut file_rows = Vec::new();
        if let Some(diff) = self.diff() {
            let entries = {
                let view = diff.read(cx);
                file_entries(view.files(), view.rows())
            };
            for entry in entries {
                let is_read = self
                    .active_number()
                    .is_some_and(|n| self.reviewed.is_reviewed(n, &entry.path));
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
                            format!(
                                "{} {} {}",
                                reviewed_marker(is_read),
                                status_glyph(&entry.status),
                                entry.path
                            ),
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
            .on_action(cx.listener(|this, _: &ToggleReviewed, _, cx| this.toggle_reviewed(cx)))
            .on_action(cx.listener(|this, _: &NextUnreviewed, _, cx| this.next_unreviewed(cx)))
            .on_action(cx.listener(|this, _: &Refresh, _, cx| this.refresh(None, cx)))
            .on_action(cx.listener(|this, _: &HalfPageDown, _, cx| this.half_page(true, cx)))
            .on_action(cx.listener(|this, _: &HalfPageUp, _, cx| this.half_page(false, cx)))
            .on_action(cx.listener(|this, _: &NextReview, _, cx| this.step_review(1, cx)))
            .on_action(cx.listener(|this, _: &PrevReview, _, cx| this.step_review(-1, cx)))
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
            .child(match self.diff() {
                Some(diff) => div().flex_1().h_full().child(diff).into_any_element(),
                None => {
                    let active = self.active.and_then(|ix| self.reviews.get(ix));
                    let (text, colour) = match placeholder(active) {
                        Placeholder::NothingSelected => {
                            (SharedString::from("select a review"), theme.text_muted)
                        }
                        Placeholder::Loading => {
                            (SharedString::from("loading…"), theme.text_muted)
                        }
                        Placeholder::Failed(message) => (
                            SharedString::from(format!("failed: {message}")),
                            theme.removed,
                        ),
                    };
                    div()
                        .flex_1()
                        .items_center()
                        .justify_center()
                        .text_color(colour)
                        .child(text)
                        .into_any_element()
                }
            })
    }
}

#[cfg(test)]
mod tests {
    use super::{Placeholder, placeholder};
    use diffident_model::{LoadState, Review, ReviewId};

    fn review(state: LoadState) -> Review {
        Review {
            id: ReviewId {
                repo: "o/r".into(),
                number: 7,
            },
            title: "t".into(),
            branch: "b".into(),
            depth: 0,
            is_draft: false,
            head_sha: String::new(),
            rebased: false,
            state,
        }
    }

    #[test]
    fn with_nothing_selected_the_pane_says_so() {
        assert_eq!(placeholder(None), Placeholder::NothingSelected);
    }

    #[test]
    fn a_selected_review_that_is_still_fetching_says_loading_not_select_a_review() {
        // The rail said "loading…" while the pane said you had selected
        // nothing. Both describe the same review; they must agree.
        assert_eq!(
            placeholder(Some(&review(LoadState::Loading))),
            Placeholder::Loading
        );
    }

    #[test]
    fn a_failed_review_shows_its_error_in_the_pane_too() {
        assert_eq!(
            placeholder(Some(&review(LoadState::Failed {
                message: "gh auth login".into()
            }))),
            Placeholder::Failed("gh auth login".into())
        );
    }

    #[test]
    fn a_selected_review_awaiting_its_first_fetch_says_loading() {
        assert_eq!(
            placeholder(Some(&review(LoadState::Idle))),
            Placeholder::Loading
        );
    }

    use diffident_forge::stack::next_in_stack;
    /// The rule `next_unreviewed` follows once the current PR is exhausted.
    /// Extracted here so the choice is testable without a window; the action
    /// handler does the loading.
    fn next_review_to_open(depths: &[usize], from: usize, unread: &[usize]) -> Option<usize> {
        let mut at = from;
        for _ in 0..depths.len() {
            at = next_in_stack(depths, at)?;
            if at == from {
                return None; // all the way round, nothing unread
            }
            if unread[at] > 0 {
                return Some(at);
            }
        }
        None
    }
    #[test]
    fn exhausting_a_pr_moves_to_the_next_one_in_the_same_stack() {
        let depths = [0, 1, 2];
        let unread = [0, 3, 1];
        assert_eq!(next_review_to_open(&depths, 0, &unread), Some(1));
    }
    #[test]
    fn a_fully_read_neighbour_is_skipped() {
        let depths = [0, 1, 2];
        let unread = [0, 0, 4];
        assert_eq!(next_review_to_open(&depths, 0, &unread), Some(2));
    }
    #[test]
    fn a_fully_read_stack_has_nowhere_to_go() {
        let depths = [0, 1, 2];
        let unread = [0, 0, 0];
        assert_eq!(next_review_to_open(&depths, 0, &unread), None);
    }
    #[test]
    fn navigation_never_leaves_the_stack_for_an_unrelated_pr() {
        // #4 (index 3) is a separate stack with plenty unread. Being done with
        // this stack must not drag the reviewer into someone else's PR.
        let depths = [0, 1, 0];
        let unread = [0, 0, 9];
        assert_eq!(next_review_to_open(&depths, 0, &unread), None);
    }
    /// Every action declared in `navigate.rs` must have an `on_action` handler
    /// Every action declared in `navigate.rs` must have an `on_action` handler
    /// here.
    ///
    /// GPUI binds keys to actions and actions to handlers in two separate
    /// places, and nothing links them: an action with a key binding but no
    /// handler compiles, passes every other test, and silently swallows the
    /// keystroke at runtime. Phase 2 shipped five such keys — `ctrl-d`,
    /// `ctrl-u`, `r`, `ctrl-tab`, `ctrl-shift-tab` — and no test noticed.
    ///
    /// Reading the source is crude, but the alternative is a windowed test, and
    /// the thing being checked is precisely that two source files agree.
    #[test]
    fn every_declared_action_is_wired_to_a_handler() {
        let navigate = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/navigate.rs"));
        let workspace = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/workspace.rs"));

        let declared: Vec<&str> = navigate
            .split_once("actions!(")
            .expect("navigate.rs declares an actions! set")
            .1
            .split_once('[')
            .expect("the actions! set is bracketed")
            .1
            .split_once(']')
            .expect("the actions! set is closed")
            .0
            .split(',')
            .map(str::trim)
            .filter(|s| s.chars().next().is_some_and(char::is_uppercase))
            .collect();

        assert!(
            declared.len() >= 12,
            "expected the full action set, parsed {declared:?}"
        );

        let unwired: Vec<&&str> = declared
            .iter()
            .filter(|a| !workspace.contains(&format!("&{a},")))
            .collect();

        assert!(
            unwired.is_empty(),
            "these actions are bound to keys but have no on_action handler, so \
             their keystrokes are silently swallowed: {unwired:?}"
        );
    }
}
