//! Root view: a rail of open reviews on the left, the active review on the right.
//!
//! The rail is the whole point of diffident (§1) — N reviews resident in ONE
//! window, rather than one OS window per PR.

use crate::diff_view::DiffView;
use crate::file_list::{file_entries, reviewed_marker, status_glyph};
use crate::loader::{LoadedReview, list_reviews, load_review};
use crate::navigate::*;
use crate::rail::rail_row;
use crate::composer::{Key, TextBuffer, key_action, scope_for_file, scope_for_line, scope_for_range};
use crate::residency::Residency;
use crate::theme::Theme;
use diffident_forge::{Repo, gh::Gh, github::GitHub};
use diffident_model::{LoadState, Review};
use diffident_model::comment::{Comment, CommentScope, Drafts, Side};
use diffident_model::reviewed::Reviewed;
use diffident_session::store::{Session, SessionKey, Store, default_root};
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
    /// Where review progress is persisted. One file per PR (§7).
    store: Store,
    /// Local draft comments, per PR (§7). Outlives the diffs in `residency`.
    drafts: Drafts,
    /// The comment being written, if any.
    composing: Option<Composing>,
    /// Where a `v` visual selection began. `c` turns anchor..cursor into a
    /// range comment.
    visual_anchor: Option<usize>,
    /// Focus for the composer, so typing reaches it and not the diff.
    composer_focus: FocusHandle,
    theme: Theme,
    focus: FocusHandle,
    error: Option<String>,
}

/// A comment being written.
struct Composing {
    /// What it will attach to, decided when the composer opened rather than
    /// when it closes — the cursor is free to move underneath.
    scope: CommentScope,
    buffer: TextBuffer,
}

impl Workspace {
    pub fn new(repo: Repo, open_pr: Option<u32>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            repo: repo.clone(),
            reviews: Vec::new(),
            active: None,
            residency: Residency::new(RESIDENT),
            reviewed: Reviewed::new(),
            store: Store::new(default_root()),
            drafts: Drafts::new(),
            composing: None,
            visual_anchor: None,
            composer_focus: cx.focus_handle(),
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
                        let mut moved = Vec::new();
                        this.reviews = listed
                            .into_iter()
                            .map(|mut fresh| {
                                // Carry over what the listing does not know, and
                                // flag a head that moved under a loaded review.
                                if let Some(old) =
                                    this.reviews.iter().find(|r| r.id == fresh.id)
                                {
                                    if old.head_sha != fresh.head_sha {
                                        moved.push(fresh.id.number);
                                    }
                                    fresh.rebased =
                                        old.rebased || old.head_sha != fresh.head_sha;
                                    fresh.state = old.state.clone();
                                }
                                fresh
                            })
                            .collect();

                        // A cached diff for a moved head is stale, and `select`
                        // serves the cache without refetching — so leaving it
                        // there would show old code with no way to reload it.
                        // The badge tells the reviewer; this makes it actionable.
                        for number in moved {
                            this.residency.forget(number);
                        }
                        // Reload whatever is on screen, or the pane goes blank
                        // until the reviewer clicks the rail again.
                        if let Some(ix) = this.active
                            && this.diff().is_none()
                        {
                            this.select(ix, cx);
                        }
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
            LoadState::Ready { files, .. } => {
                self.reviewed.unreviewed_count(review.id.number, files)
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
                files: loaded
                    .files
                    .iter()
                    .map(|f| (f.display_path().to_string(), f.content_hash()))
                    .collect(),
                head_sha: loaded.head_sha.clone(),
            };
        }
        // Reattach saved progress. Marks whose hash no longer matches simply
        // read as unread, so nothing needs filtering here (§7).
        let saved = self.store.load(&self.session_key(number));
        // Drafts reattach only at the head they were written on — otherwise
        // their line anchors point into a diff that no longer exists (§7).
        self.drafts
            .restore(number, saved.comments_at(&loaded.head_sha).to_vec());
        self.reviewed.restore(number, saved.reviewed);

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
        let mark = {
            let view = diff.read(cx);
            view.rows()
                .get(view.cursor)
                .and_then(|row| row.file_ix())
                .and_then(|ix| view.files().get(ix))
                .map(|f| (f.display_path().to_string(), f.content_hash()))
        };
        if let Some((path, hash)) = mark {
            self.reviewed.toggle(number, &path, hash);
            self.persist(number);
            cx.notify();
        }
    }

    /// Open the composer for `scope`, seeded from any draft already on it.
    ///
    /// Re-opening the same anchor edits that draft rather than stacking a
    /// second one on the same line, which is almost never what is meant.
    fn compose(&mut self, scope: CommentScope, window: &mut Window, cx: &mut Context<Self>) {
        let existing = self
            .active_number()
            .map(|n| self.drafts.for_review(n))
            .unwrap_or_default()
            .iter()
            .find(|c| c.scope == scope && c.is_editable())
            .map(|c| c.body.clone());
        self.composing = Some(Composing {
            buffer: existing
                .map(|b| TextBuffer::from_text(&b))
                .unwrap_or_default(),
            scope,
        });
        self.visual_anchor = None;
        self.composer_focus.focus(window, cx);
        cx.notify();
    }

    /// `c` — line comment, or a range comment when a `v` selection is open.
    fn comment_on_line(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(diff) = self.diff() else { return };
        let scope = {
            let view = diff.read(cx);
            match self.visual_anchor {
                Some(anchor) => scope_for_range(view.files(), view.rows(), anchor, view.cursor),
                None => scope_for_line(view.files(), view.rows(), view.cursor),
            }
        };
        if let Some(scope) = scope {
            self.compose(scope, window, cx);
        }
    }

    /// `C` — a comment on the whole file the cursor is in.
    fn comment_on_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(diff) = self.diff() else { return };
        let scope = {
            let view = diff.read(cx);
            scope_for_file(view.files(), view.rows(), view.cursor)
        };
        if let Some(scope) = scope {
            self.compose(scope, window, cx);
        }
    }

    /// `v` — start or clear a visual selection at the cursor.
    fn toggle_visual(&mut self, cx: &mut Context<Self>) {
        self.visual_anchor = match self.visual_anchor {
            Some(_) => None,
            None => self.diff().map(|d| d.read(cx).cursor),
        };
        cx.notify();
    }

    /// `x` — delete the newest draft anchored where the cursor is.
    fn delete_draft(&mut self, cx: &mut Context<Self>) {
        let (Some(number), Some(diff)) = (self.active_number(), self.diff()) else {
            return;
        };
        let here = {
            let view = diff.read(cx);
            scope_for_line(view.files(), view.rows(), view.cursor)
                .or_else(|| scope_for_file(view.files(), view.rows(), view.cursor))
        };
        let Some(here) = here else { return };
        let target = self
            .drafts
            .for_review(number)
            .iter()
            .rev()
            .find(|c| c.scope == here && c.is_editable())
            .map(|c| c.id);
        if let Some(id) = target {
            self.drafts.remove(number, id);
            self.persist(number);
            cx.notify();
        }
    }

    /// A keystroke while the composer has focus.
    ///
    /// Takes `window` because closing the composer has to hand focus back.
    /// GPUI dispatches actions along the focus chain, so leaving focus on a
    /// handle whose element has just been removed silently kills every diff
    /// binding — the app looks alive and answers no keys at all.
    fn composer_key(&mut self, key: &Key, window: &mut Window, cx: &mut Context<Self>) {
        let Some(composing) = self.composing.as_mut() else {
            return;
        };
        match key {
            Key::Insert(text) => composing.buffer.insert(text),
            Key::Newline => composing.buffer.newline(),
            Key::Backspace => composing.buffer.backspace(),
            Key::Delete => composing.buffer.delete(),
            Key::Left => composing.buffer.left(),
            Key::Right => composing.buffer.right(),
            Key::Up => composing.buffer.up(),
            Key::Down => composing.buffer.down(),
            Key::Home => composing.buffer.home(),
            Key::End => composing.buffer.end(),
            Key::Cancel => {
                self.composing = None;
                self.focus.focus(window, cx);
            }
            Key::Save => {
                self.save_draft();
                self.focus.focus(window, cx);
            }
            Key::Ignore => return,
        }
        cx.notify();
    }

    /// Turn the composer's contents into a draft and close it.
    ///
    /// A blank comment closes without saving rather than storing an empty
    /// draft the reviewer would then have to delete.
    fn save_draft(&mut self) {
        let Some(composing) = self.composing.take() else {
            return;
        };
        let Some(number) = self.active_number() else {
            return;
        };
        if composing.buffer.is_blank() {
            return;
        }
        let body = composing.buffer.text();
        // Replace any draft already on this exact anchor — `compose` seeded the
        // buffer from it, so keeping both would duplicate the text.
        if let Some(old) = self
            .drafts
            .for_review(number)
            .iter()
            .find(|c| c.scope == composing.scope && c.is_editable())
            .map(|c| c.id)
        {
            self.drafts.remove(number, old);
        }
        let comment = match composing.scope {
            CommentScope::Review => Comment::new_review(&body),
            CommentScope::File { ref path } => Comment::new_file(path, &body),
            CommentScope::Line {
                ref path,
                line,
                side,
            } => Comment::new_line(path, line, side, &body),
            CommentScope::Range {
                ref path,
                start_line,
                end_line,
                side,
            } => Comment::new_range(path, start_line, end_line, side, &body),
        };
        self.drafts.add(number, comment);
        self.persist(number);
    }

    /// The composer panel: what it attaches to, the text, and how to finish.
    fn render_composer(&self, composing: &Composing, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme.clone();
        let (cur_line, cur_col) = composing.buffer.cursor();

        let mut lines = Vec::new();
        for (ix, text) in composing.buffer.lines().iter().enumerate() {
            let row = div().flex().h(px(theme.line_height));
            lines.push(if ix == cur_line {
                // The caret is a thin div between two spans rather than a
                // character spliced into the text: inline layout puts it in
                // exactly the right place with no font measurement, and it
                // does not shift what the reviewer typed.
                let (before, after) = text.split_at(cur_col);
                row.child(SharedString::from(before.to_string()))
                    .child(div().w(px(1.5)).h(px(theme.line_height)).bg(theme.text))
                    .child(SharedString::from(after.to_string()))
            } else {
                row.child(SharedString::from(text.clone()))
            });
        }

        div()
            .id("composer")
            .track_focus(&self.composer_focus)
            .key_context("Composer")
            .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, window, cx| {
                let k = &ev.keystroke;
                let action = key_action(
                    &k.key,
                    k.key_char.as_deref(),
                    k.modifiers.platform,
                    k.modifiers.control,
                );
                this.composer_key(&action, window, cx);
            }))
            .flex()
            .flex_col()
            .gap_1()
            .w_full()
            .p_2()
            .border_t_1()
            .border_color(theme.border)
            .bg(theme.header_bg)
            .child(
                div()
                    .text_sm()
                    .text_color(theme.text_muted)
                    .child(SharedString::from(format!(
                        "comment on {}",
                        scope_label(&composing.scope)
                    ))),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_h(px(theme.line_height * 3.))
                    .children(lines),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(theme.text_muted)
                    .child("cmd-enter save · esc cancel"),
            )
    }

    /// The drafts written so far on the active review.
    fn render_drafts(&self) -> Vec<gpui::AnyElement> {
        let theme = &self.theme;
        let Some(number) = self.active_number() else {
            return Vec::new();
        };
        self.drafts
            .for_review(number)
            .iter()
            .map(|c| {
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .px_2()
                    .py_1()
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.added)
                            .child(SharedString::from(scope_label(&c.scope))),
                    )
                    .child(
                        div()
                            .text_color(theme.text)
                            .child(SharedString::from(c.body.clone())),
                    )
                    .into_any_element()
            })
            .collect()
    }

    /// The storage key for one review.
    fn session_key(&self, number: u32) -> SessionKey {
        SessionKey {
            repo: self.repo.slug(),
            pr: number,
        }
    }

    /// Write this review's progress to disk.
    ///
    /// Called on every toggle rather than on quit: a review app that loses an
    /// hour of marks because it was force-quit is worse than one that writes a
    /// few KB more often, and the write is atomic so a crash mid-save cannot
    /// corrupt what was already there.
    ///
    /// A failed write is surfaced but not fatal — the marks are still correct
    /// in memory, and refusing to continue the review over a disk error would
    /// be the larger harm.
    fn persist(&mut self, number: u32) {
        let session = Session {
            // The head the *diff* was fetched at, not the one from the last
            // listing: a listing can move under us, and writing that head
            // would claim the drafts were authored against code never seen.
            head_sha: self
                .reviews
                .iter()
                .find(|r| r.id.number == number)
                .and_then(|r| match &r.state {
                    LoadState::Ready { head_sha, .. } => Some(head_sha.clone()),
                    _ => None,
                })
                .unwrap_or_default(),
            comments: self.drafts.for_review(number).to_vec(),
            reviewed: self.reviewed.marks(number),
        };
        if let Err(e) = self.store.save(&self.session_key(number), &session) {
            self.error = Some(format!("could not save review progress: {e}"));
        }
    }

    /// `tab`: go to the next unread file, crossing into the next PR of this
    /// stack when the current one is fully read.
    ///
    /// Wraps within the stack and stops if it comes all the way round, so a
    /// fully-read stack does nothing rather than looping forever. Never leaves
    /// the stack: being done here must not drag the reviewer into an unrelated
    /// PR.
    fn next_unreviewed(&mut self, cx: &mut Context<Self>) {
        if let (Some(number), Some(diff)) = (self.active_number(), self.diff()) {
            let target = {
                let view = diff.read(cx);
                next_unreviewed_row(view.rows(), view.cursor, |file_ix| {
                    view.files().get(file_ix).is_some_and(|f| {
                        !self.reviewed.is_reviewed(number, f.display_path(), f.content_hash())
                    })
                })
            };
            if let Some(row) = target {
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


/// A one-line description of what a comment is attached to.
///
/// Shown in the composer header and beside each draft: a list of comment
/// bodies with no anchors is unreadable once there is more than one.
fn scope_label(scope: &CommentScope) -> String {
    match scope {
        CommentScope::Review => "whole review".to_string(),
        CommentScope::File { path } => path.clone(),
        CommentScope::Line { path, line, side } => format!("{path}:{line}{}", side_mark(side)),
        CommentScope::Range {
            path,
            start_line,
            end_line,
            side,
        } => format!("{path}:{start_line}-{end_line}{}", side_mark(side)),
    }
}

/// Marks an anchor on the pre-image, where the line no longer exists on the new
/// side. Blank for the common case so it does not add noise.
fn side_mark(side: &Side) -> &'static str {
    match side {
        Side::Old => " (old)",
        Side::New => "",
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
            for (ix, entry) in entries.into_iter().enumerate() {
                let entry_file = diff.read(cx).files().get(ix);
                let is_read = self.active_number().is_some_and(|n| {
                    entry_file
                        .map(|f| self.reviewed.is_reviewed(n, &entry.path, f.content_hash()))
                        .unwrap_or(false)
                });
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
            // Dropped while composing, so `j` types a j instead of moving the
            // cursor. Key contexts match along the whole focus chain, so one
            // left on an ancestor would still fire the diff bindings.
            .when(self.composing.is_none(), |this| this.key_context("Diff"))
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
            .on_action(cx.listener(|this, _: &LineComment, window, cx| {
                this.comment_on_line(window, cx)
            }))
            .on_action(cx.listener(|this, _: &FileComment, window, cx| {
                this.comment_on_file(window, cx)
            }))
            .on_action(cx.listener(|this, _: &ReviewComment, window, cx| {
                this.compose(CommentScope::Review, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleVisual, _, cx| this.toggle_visual(cx)))
            .on_action(cx.listener(|this, _: &DeleteDraft, _, cx| this.delete_draft(cx)))
            .on_action(cx.listener(|this, _: &ClearSelection, _, cx| {
                this.visual_anchor = None;
                cx.notify();
            }))
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
            .children({
                let drafts = self.render_drafts();
                (!drafts.is_empty()).then(|| {
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .w(px(320.))
                        .h_full()
                        .p_2()
                        .border_l_1()
                        .border_color(theme.border)
                        .child(
                            div()
                                .px_2()
                                .text_sm()
                                .text_color(theme.text_muted)
                                .child("drafts"),
                        )
                        .children(drafts)
                })
            })
            .child(match self.diff() {
                Some(diff) => div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .h_full()
                    .child(div().flex_1().min_h(px(0.)).child(diff))
                    .children(
                        self.composing
                            .as_ref()
                            .map(|c| self.render_composer(c, cx).into_any_element()),
                    )
                    .into_any_element(),
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
