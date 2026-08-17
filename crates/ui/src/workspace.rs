//! Root view: a rail of open reviews on the left, the active review on the right.
//!
//! The rail is the whole point of diffident (§1) — N reviews resident in ONE
//! window, rather than one OS window per PR.

use crate::diff_view::DiffView;
use crate::file_list::{file_entries, file_row};
use crate::loader::{LoadedReview, ReviewData, list_reviews, load_review};
use crate::navigate::*;
use crate::rail::rail_row;
use crate::composer::{Key, TextBuffer, key_action, scope_for_file, scope_for_line, scope_for_range};
use crate::residency::Residency;
use crate::submit::{Event, Resolution, Submission, preflight};
use crate::theme::Theme;
use diffident_forge::{Forge, Repo};
use std::sync::Arc;
use diffident_model::{LoadState, Review};
use diffident_model::comment::{Comment, CommentScope, Drafts, Side};
use diffident_model::reviewed::Reviewed;
use diffident_session::store::{Session, SessionKey, Store, default_root};
use diffident_forge::stack::next_in_stack;
use gpui::{
    Context, Entity, FocusHandle, Focusable, IntoElement, ParentElement, Render, SharedString,
    Window, div, prelude::*, px, uniform_list,
};

/// How many diffs stay resident. Four covers a typical stack (§6) without
/// holding an unbounded amount of parsed diff in memory (§10).
const RESIDENT: usize = 4;

/// What the window is currently doing.
#[derive(Default)]
enum Mode {
    /// Reading the diff. Diff keys live here.
    #[default]
    Browsing,
    /// Writing a comment.
    Composing(Composing),
    /// Choosing what happens to drafts that will not map onto the diff.
    Resolving(Submission),
    /// Choosing the review kind, and sending.
    Confirming(Submission),
}

impl Mode {
    /// The key context this mode listens in. `None` while a mode owns the
    /// keyboard for raw text, so no action bindings fire underneath it.
    fn key_context(&self) -> Option<&'static str> {
        match self {
            Mode::Browsing => Some("Diff"),
            Mode::Composing(_) => None,
            Mode::Resolving(_) => Some("Resolver"),
            Mode::Confirming(_) => Some("Confirm"),
        }
    }
}

pub struct Workspace {
    /// The code host, injected rather than constructed.
    ///
    /// Every `gh`-touching path used to name `GitHub::new(Gh)` inline, which
    /// meant no test could ever build a `Workspace` — the reason two guard
    /// tests in this file resort to reading their own source. `Arc<dyn>`
    /// because the background executor needs to move a handle into a task and
    /// the trait is deliberately object-safe (see `Forge`'s doc).
    forge: Arc<dyn Forge + Send + Sync>,
    repo: Repo,
    reviews: Vec<Review>,
    active: Option<usize>,
    /// Resident diffs, in-flight fetches, and remembered cursors (§9 Phase 3).
    ///
    /// Re-opening a review costs ~2s of `gh` plus ~530ms of highlighting, so
    /// keeping a few alive makes switching instant — the whole point of one
    /// window holding N reviews (§1). Bounded because a large diff is tens of
    /// MB (§10).
    /// The parsed diff and its view, evicted together.
    ///
    /// The `Arc<ReviewData>` lives *here* rather than in a map of its own so
    /// that eviction still frees it — a large diff is tens of MB (§10), and a
    /// clone parked anywhere longer-lived would make this LRU decorative.
    residency: Residency<(Arc<ReviewData>, Entity<DiffView>)>,
    /// Which files the reviewer has marked read, per PR. Outlives the diffs in
    /// `residency` on purpose — evicting a diff must not forget your progress.
    reviewed: Reviewed,
    /// Where review progress is persisted. One file per PR (§7).
    store: Store,
    /// Local draft comments, per PR (§7). Outlives the diffs in `residency`.
    drafts: Drafts,
    /// Threads already on each PR, or why they are missing. Outlives the diff
    /// in `residency` for the same reason drafts do.
    ///
    /// One map rather than two, because "no threads" and "we could not find
    /// out" are one fact with two values, and splitting them meant every
    /// reader had to consult the other map to know which it was looking at.
    /// The error stays out of `error` (the rail's) because it is about one
    /// pane, not the app.
    threads: std::collections::HashMap<u32, Result<Vec<diffident_forge::threads::ReviewThread>, String>>,
    /// Which thread in the right-hand pane the reviewer is acting on.
    ///
    /// Not per-PR: switching reviews resets it, because carrying "thread 3"
    /// across to a PR with one thread would silently point somewhere else.
    thread_cursor: usize,
    /// What the window is doing. One value, so two full-window states cannot
    /// both be active — which three separate `Option`s would eventually allow.
    mode: Mode,
    /// Where a `v` visual selection began. `c` turns anchor..cursor into a
    /// range comment.
    visual_anchor: Option<usize>,
    /// Focus for the composer, so typing reaches it and not the diff.
    composer_focus: FocusHandle,
    theme: Theme,
    focus: FocusHandle,
    error: Option<String>,
}

/// Where the composer's text goes when it is saved.
///
/// `Clone` so `commit_composer` can lift the destination out of `self.mode`
/// before it needs `&mut self` — see the comment there.
#[derive(Clone)]
enum Destination {
    /// A local draft on this anchor, batched into the next submit (§7).
    Draft(CommentScope),
    /// A reply to a thread already on the PR. Posted the moment it is saved
    /// rather than batched: §7 gives replies their own mutation, and
    /// `create_review` has no field that would carry one.
    Reply {
        thread_id: String,
        /// `path:line`, for the composer's header. Carried rather than looked
        /// up so rendering needs no access to the thread list.
        on: String,
    },
}

/// A comment being written.
struct Composing {
    /// Where it will go, decided when the composer opened rather than when it
    /// closes — the cursor and the thread selection are free to move
    /// underneath.
    dest: Destination,
    buffer: TextBuffer,
}

impl Workspace {
    pub fn new(
        forge: Arc<dyn Forge + Send + Sync>,
        repo: Repo,
        open_pr: Option<u32>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut this = Self {
            forge,
            repo: repo.clone(),
            reviews: Vec::new(),
            active: None,
            residency: Residency::new(RESIDENT),
            reviewed: Reviewed::new(),
            store: Store::new(default_root()),
            drafts: Drafts::new(),
            threads: std::collections::HashMap::new(),
            thread_cursor: 0,
            mode: Mode::Browsing,
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
        let forge = self.forge.clone();
        cx.spawn(async move |this, cx| {
            let listed = cx
                .background_executor()
                .spawn(async move { list_reviews(forge.as_ref(), &repo) })
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
        self.residency
            .get(self.active_number()?)
            .map(|(_, view)| view.clone())
    }

    /// Say why a keystroke did nothing.
    ///
    /// The four comment keys all resolve the cursor to a comment scope, and a
    /// header, hunk header, spacer or expander has none. Returning silently
    /// there is indistinguishable from a broken key — and `p` already proved
    /// the point by reporting one of its two failures and swallowing the
    /// other, four lines apart.
    fn refuse(&mut self, why: &str, cx: &mut Context<Self>) {
        self.error = Some(why.to_string());
        cx.notify();
    }

    /// The threads on `number`, or none when the fetch failed.
    ///
    /// Callers that act on a thread — select, resolve, reply — cannot do
    /// anything useful with the failure, so this flattens it away rather than
    /// making each of them decide. Only `render_threads`, which has to tell the
    /// reviewer *why* the pane is empty, looks at the `Result` itself.
    fn threads_of(&self, number: u32) -> Vec<diffident_forge::threads::ReviewThread> {
        self.threads
            .get(&number)
            .and_then(|t| t.as_ref().ok())
            .cloned()
            .unwrap_or_default()
    }

    /// The active review's parsed diff, without borrowing the view.
    ///
    /// This is the point of sharing it: reading files or rows no longer needs a
    /// `Context` just to reach through a GPUI entity, so the callers below are
    /// plain `&self` functions that a test can call directly.
    fn data(&self) -> Option<&Arc<ReviewData>> {
        self.residency
            .get(self.active_number()?)
            .map(|(data, _)| data)
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
        self.thread_cursor = 0;
        // Before the early returns below, not after: both of them leave a
        // resident view holding the *previous* review's selection, and one of
        // them (`activate`) is the ordinary path back to an already-loaded
        // review. A first load finds nothing resident here and is synced by
        // `apply` instead.
        self.sync_threads(cx);
        let repo = self.repo.clone();
        let forge = self.forge.clone();

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
                .spawn(async move { load_review(forge.as_ref(), &repo, number) })
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
                    .data
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
        self.threads.insert(
            number,
            match &loaded.threads_error {
                Some(why) => Err(why.clone()),
                None => Ok(loaded.threads.clone()),
            },
        );

        let theme = self.theme.clone();
        let row = self.residency.recall_cursor(number, loaded.data.rows.len());
        let data = loaded.data.clone();
        let view = cx.new(|_| {
            let mut v = DiffView::new(data, theme);
            v.scroll_to(row);
            v
        });
        self.residency
            .admit(number, (loaded.data, view), &loaded.head_sha);

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
        self.sync_threads_for(number, cx);
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
        self.compose_with(scope, None, window, cx);
    }

    /// `compose`, with `extra` appended to whatever was already there.
    ///
    /// Appending rather than replacing: Task 7 seeds a suggestion fence, and
    /// silently discarding a paragraph the reviewer had already typed on that
    /// line is not a recoverable mistake.
    fn compose_with(
        &mut self,
        scope: CommentScope,
        extra: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let existing = self
            .active_number()
            .map(|n| self.drafts.for_review(n))
            .unwrap_or_default()
            .iter()
            .find(|c| c.scope == scope && c.is_editable())
            .map(|c| c.body.clone());
        let seed = match (existing, extra) {
            (Some(body), Some(extra)) => format!("{body}\n{extra}"),
            (Some(body), None) => body,
            (None, Some(extra)) => extra,
            (None, None) => String::new(),
        };
        self.visual_anchor = None;
        self.enter_mode(
            Mode::Composing(Composing {
                buffer: TextBuffer::from_text(&seed),
                dest: Destination::Draft(scope),
            }),
            window,
            cx,
        );
    }

    /// `a` — answer the selected thread.
    fn reply(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(number) = self.active_number() else {
            return;
        };
        let Some((thread_id, on)) = crate::threads::selected(
            &self.threads_of(number),
            self.thread_cursor,
        )
            .map(|t| {
                (
                    t.id.clone(),
                    format!("{}:{}", t.path, t.anchor_line().unwrap_or(0)),
                )
            })
        else {
            return;
        };
        self.visual_anchor = None;
        self.enter_mode(
            Mode::Composing(Composing {
                // Never seeded from a draft: a reply is not a draft, and there
                // is nothing on disk that belongs to it.
                buffer: TextBuffer::default(),
                dest: Destination::Reply { thread_id, on },
            }),
            window,
            cx,
        );
    }

    /// Switch modes, moving focus with the switch.
    ///
    /// **Every mode change goes through here.** GPUI dispatches actions along
    /// the focus chain, so a mode that takes focus and never returns it leaves
    /// focus on an element that no longer exists and silently kills every
    /// binding in the app — which is exactly what Phase 5b shipped, with a
    /// fully green test suite. Keeping the focus move welded to the mode
    /// change is what stops that recurring.
    fn enter_mode(&mut self, mode: Mode, window: &mut Window, cx: &mut Context<Self>) {
        let takes_keyboard = matches!(mode, Mode::Composing(_));
        self.mode = mode;
        if takes_keyboard {
            self.composer_focus.focus(window, cx);
        } else {
            self.focus.focus(window, cx);
        }
        cx.notify();
    }

    /// Return to reading the diff.
    fn leave_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.enter_mode(Mode::Browsing, window, cx);
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
        match scope {
            Some(scope) => self.compose(scope, window, cx),
            None => self.refuse("the cursor is not inside a file", cx),
        }
    }

    /// `p` — comment on the selected line(s), pre-filled with a suggestion
    /// fence containing what is there now.
    ///
    /// Seeding with the real source is the whole value: a suggestion has to
    /// match the file exactly for GitHub to offer a commit button, and
    /// retyping a line from a rendered diff is how that goes wrong.
    fn suggest(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(diff) = self.diff() else { return };
        let (scope, lines) = {
            let view = diff.read(cx);
            let anchor = self.visual_anchor.unwrap_or(view.cursor);
            let scope = match self.visual_anchor {
                Some(a) => scope_for_range(view.files(), view.rows(), a, view.cursor),
                None => scope_for_line(view.files(), view.rows(), view.cursor),
            };
            (
                scope,
                crate::suggest::source_lines(view.files(), view.rows(), anchor, view.cursor),
            )
        };
        let Some(scope) = scope else {
            return self.refuse("there is no line here to suggest a change to", cx);
        };
        // Nothing on the new side to replace — a removed-lines-only selection.
        // GitHub would reject the suggestion, so it is better not to offer one.
        if lines.is_empty() {
            return self.refuse("a suggestion needs lines on the new side of the diff", cx);
        }
        self.compose_with(scope, Some(crate::suggest::fence(&lines)), window, cx);
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
        let Some(here) = here else {
            return self.refuse("there is no draft here to delete", cx);
        };
        let at_cursor: Vec<&Comment> = self
            .drafts
            .for_review(number)
            .iter()
            .rev()
            .filter(|c| c.scope == here)
            .collect();
        let target = at_cursor.iter().find(|c| c.is_editable()).map(|c| c.id);
        match target {
            Some(id) => {
                self.drafts.remove(number, id);
                self.persist(number);
                self.error = None;
            }
            // There is a comment here, but GitHub has it. Saying so beats a
            // key that silently does nothing, which reads as a broken app.
            None if !at_cursor.is_empty() => {
                self.error = Some("that comment is already on GitHub — it cannot be deleted here".into());
            }
            None => return,
        }
        cx.notify();
    }

    /// A keystroke while the composer has focus.
    ///
    /// Takes `window` because closing the composer has to hand focus back.
    /// GPUI dispatches actions along the focus chain, so leaving focus on a
    /// handle whose element has just been removed silently kills every diff
    /// binding — the app looks alive and answers no keys at all.
    fn composer_key(&mut self, key: &Key, window: &mut Window, cx: &mut Context<Self>) {
        match key {
            Key::Cancel => {
                if matches!(self.mode, Mode::Composing(_)) {
                    self.leave_mode(window, cx);
                }
                return;
            }
            Key::Save => {
                if matches!(self.mode, Mode::Composing(_)) {
                    self.commit_composer(window, cx);
                }
                return;
            }
            Key::Ignore => return,
            _ => {}
        }
        let Mode::Composing(composing) = &mut self.mode else {
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
            Key::Cancel | Key::Save | Key::Ignore => return,
        }
        cx.notify();
    }

    /// Finish composing: save a draft, or post a reply, then close.
    ///
    /// A blank body closes without doing either, rather than storing an empty
    /// draft the reviewer would then have to delete or posting an empty reply
    /// that GitHub would reject.
    ///
    /// The destination and body are lifted out of `self.mode` into owned values
    /// first. Every branch below needs `&mut self`, and holding a borrow of
    /// `self.mode` across `save_draft`/`post_reply`/`leave_mode` does not
    /// compile.
    fn commit_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pending = match &self.mode {
            Mode::Composing(c) if !c.buffer.is_blank() => Some((c.dest.clone(), c.buffer.text())),
            Mode::Composing(_) => None,
            _ => return,
        };
        match pending {
            Some((Destination::Draft(scope), body)) => self.save_draft(scope, body),
            Some((Destination::Reply { thread_id, .. }, body)) => {
                self.post_reply(thread_id, body, cx)
            }
            None => {}
        }
        self.leave_mode(window, cx);
    }

    /// Store a local draft on `scope`, replacing any already there.
    fn save_draft(&mut self, scope: CommentScope, body: String) {
        let Some(number) = self.active_number() else {
            return;
        };
        // Replace any draft already on this exact anchor — `compose_with`
        // seeded the buffer from it, so keeping both would duplicate the text.
        if let Some(old) = self
            .drafts
            .for_review(number)
            .iter()
            .find(|c| c.scope == scope && c.is_editable())
            .map(|c| c.id)
        {
            self.drafts.remove(number, old);
        }
        let comment = match scope {
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

    /// Post a reply and, only if GitHub took it, show it in the pane.
    ///
    /// The comment comes back from the mutation itself, so the reply renders
    /// with the author and id GitHub assigned rather than a local guess, and
    /// costs no second fetch.
    fn post_reply(&mut self, thread_id: String, body: String, cx: &mut Context<Self>) {
        let Some(number) = self.active_number() else {
            return;
        };
        let target = thread_id.clone();
        let forge = self.forge.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { forge.reply(&thread_id, &body) })
                .await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(comment) => {
                        if let Some(t) = this
                            .threads
                            .get_mut(&number)
                            .and_then(|ts| ts.as_mut().ok())
                            .and_then(|ts| ts.iter_mut().find(|t| t.id == target))
                        {
                            t.comments.push(comment);
                        }
                        this.error = None;
                        this.sync_threads(cx);
                    }
                    // The text is gone from the composer and was never a draft.
                    // Saying so is the only thing standing between the reviewer
                    // and a reply they believe they sent.
                    Err(e) => this.error = Some(format!("reply failed: {e}")),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
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
                    .child(div().w(px(1.5)).h(px(theme.line_height)).bg(theme.text_primary))
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
            .border_color(theme.border_subtle)
            .bg(theme.surface_raised)
            .child(
                div()
                    .text_sm()
                    .text_color(theme.text_secondary)
                    .child(SharedString::from(match &composing.dest {
                        Destination::Draft(scope) => format!("comment on {}", scope_label(scope)),
                        Destination::Reply { on, .. } => format!("reply to {on}"),
                    })),
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
                    .text_color(theme.text_secondary)
                    .child(SharedString::from(match &composing.dest {
                        Destination::Draft(_) => "cmd-enter save · esc cancel",
                        // Not "save": this one leaves the machine the moment it
                        // is pressed, and the reviewer should know that before
                        // pressing it, not after.
                        Destination::Reply { .. } => "cmd-enter post to GitHub · esc cancel",
                    })),
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
                            .flex()
                            .justify_between()
                            .gap_2()
                            .text_sm()
                            .child(
                                div()
                                    .text_color(if c.is_editable() {
                                        theme.added_fg
                                    } else {
                                        theme.text_secondary
                                    })
                                    .child(SharedString::from(scope_label(&c.scope))),
                            )
                            // Without this a sent comment looks exactly like an
                            // unsent one, and the only signal that a submit
                            // worked is the absence of an error.
                            .child(
                                div()
                                    .text_color(theme.text_secondary)
                                    .child(SharedString::from(c.lifecycle.label())),
                            ),
                    )
                    .child(
                        div()
                            .child(crate::comment_view::comment_body(
                                &c.body,
                                theme,
                                !c.is_editable(),
                            )),
                    )
                    .into_any_element()
            })
            .collect()
    }

    /// Existing conversations on this PR, grouped by the line they sit on.
    ///
    /// Threads that could not be anchored are still listed, under a heading
    /// that says so — a conversation the reviewer cannot see is one they will
    /// answer twice or not at all.
    fn render_threads(&self, cx: &Context<Self>) -> Vec<gpui::AnyElement> {
        let theme = &self.theme;
        let (Some(number), Some(diff)) = (self.active_number(), self.diff()) else {
            return Vec::new();
        };
        // The notice must render even with no threads: an empty pane reads as
        // "nobody has reviewed this", which is a different and wrong statement.
        let (threads, failed) = match self.threads.get(&number) {
            Some(Ok(t)) => (t.as_slice(), None),
            Some(Err(why)) => (&[][..], Some(why.clone())),
            None => return Vec::new(),
        };
        let view = diff.read(cx);
        let placed = crate::threads::place(threads, view.files(), view.rows());

        let mut out: Vec<gpui::AnyElement> = Vec::new();
        let mut heading_shown = false;
        for (ix, p) in placed.iter().enumerate() {
            // Anchored threads render inline against their code (Task 3).
            // Repeating them here would make the reviewer check two places for
            // the same conversation.
            if p.is_anchored() {
                continue;
            }
            if !heading_shown {
                out.push(
                    div()
                        .px_2()
                        .text_sm()
                        .text_color(theme.text_tertiary)
                        .child("threads not in this diff")
                        .into_any_element(),
                );
                heading_shown = true;
            }
            let is_selected = ix == self.thread_cursor.min(placed.len().saturating_sub(1));
            let t = p.thread;
            // Only unanchored threads reach here, so there is no line to show.
            let where_ = format!("{} (not in this diff)", t.path);
            let status = crate::comment_view::status_label(t);
            out.push(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .when(is_selected, |this| {
                        this.bg(theme.accent_soft).border_l_2().border_color(theme.text_primary)
                    })
                    .px_2()
                    .py_1()
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .gap_2()
                            .text_sm()
                            .child(
                                div()
                                    .text_color(theme.text_secondary)
                                    .child(SharedString::from(where_)),
                            )
                            .child(
                                div()
                                    .text_color(theme.text_tertiary)
                                    .child(SharedString::from(status.to_string())),
                            ),
                    )
                    .children(crate::comment_view::thread_comments(t, theme, true))
                    .into_any_element(),
            );
        }

        if let Some(why) = failed {
            out.push(
                div()
                    .px_2()
                    .py_1()
                    .text_sm()
                    .text_color(theme.removed_fg)
                    .child(SharedString::from(format!("could not load threads: {why}")))
                    .into_any_element(),
            );
        }

        // Shown whenever the review has any conversation at all — the keys act
        // on threads drawn inline just as much as on the ones listed here.
        if !placed.is_empty() {
            out.push(
                div()
                    .px_2()
                    .py_1()
                    .text_sm()
                    .text_color(theme.text_secondary)
                    .child("t/T select · shift-space resolve · a reply")
                    .into_any_element(),
            );
        }
        out
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

    /// Push review `number`'s threads into *its own* diff view.
    ///
    /// **Call this from every place that changes `threads` or `thread_cursor`.**
    /// The view holds its own copies, and a stale copy means the reviewer is
    /// looking at a conversation that has already been resolved or replied to.
    ///
    /// Explicitly numbered rather than "whatever is active", because `apply`
    /// deliberately lands results for reviews the reviewer has since switched
    /// away from. Keying off the active review meant those diffs kept an empty
    /// thread list forever, and since the side pane stopped listing anchored
    /// threads that hid every conversation on them without a trace.
    fn sync_threads_for(&mut self, number: u32, cx: &mut Context<Self>) {
        let Some((data, diff)) = self.residency.get(number).cloned() else {
            return;
        };
        let threads = self.threads_of(number);
        // The cursor belongs to the review on screen; a background review has
        // no selection to draw.
        let selected = (self.active_number() == Some(number))
            .then(|| crate::threads::selected(&threads, self.thread_cursor))
            .flatten()
            .map(|t| t.id.clone());
        // Grouping reads the diff from the shared handle, not from the view, so
        // the only thing left inside the update is the write itself.
        let groups = crate::threads::inline_groups(&threads, &data.files, &data.rows);
        diff.update(cx, |view, cx| {
            view.set_threads(groups, selected);
            cx.notify();
        });
    }

    /// [`Self::sync_threads_for`] the review on screen.
    fn sync_threads(&mut self, cx: &mut Context<Self>) {
        if let Some(number) = self.active_number() {
            self.sync_threads_for(number, cx);
        }
    }

    /// `t` / `T` — move between the conversations on this PR.
    fn step_thread(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(number) = self.active_number() else {
            return;
        };
        let count = self.threads_of(number).len();
        self.thread_cursor = crate::threads::step(count, self.thread_cursor, delta);
        self.sync_threads(cx);
        // Scroll the newly selected conversation into view. Since anchored
        // threads left the side pane, moving the selection onto a row that is
        // off screen otherwise changes nothing visible and the key reads as
        // broken. An unanchored thread has no row to scroll to — it is listed
        // in the pane instead, so leave the diff where it is.
        let cursor = self.thread_cursor;
        let threads = self.threads_of(number);
        if let Some(diff) = self.diff() {
            diff.update(cx, |view, cx| {
                let placed = crate::threads::place(&threads, view.files(), view.rows());
                if let Some(row) = placed.get(cursor).and_then(|p| p.row) {
                    view.scroll_to(row);
                    cx.notify();
                }
            });
        }
        cx.notify();
    }

    /// `space` — resolve the selected thread, or unresolve it if it is already
    /// resolved.
    ///
    /// Nothing local changes until GitHub confirms. §9 requires a failed write
    /// to leave state untouched, and one landing closure that only mutates on
    /// `Ok` is the whole of that guarantee.
    fn toggle_resolved(&mut self, cx: &mut Context<Self>) {
        let Some(number) = self.active_number() else {
            return;
        };
        let Some((id, want)) = crate::threads::selected(
            &self.threads_of(number),
            self.thread_cursor,
        )
        .map(|t| (t.id.clone(), !t.is_resolved))
        else {
            return;
        };
        let target = id.clone();
        let forge = self.forge.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { forge.set_resolved(&id, want) })
                .await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(()) => {
                        // Found by id, not by index: the cursor may have moved
                        // during the round trip.
                        if let Some(t) = this
                            .threads
                            .get_mut(&number)
                            .and_then(|ts| ts.as_mut().ok())
                            .and_then(|ts| ts.iter_mut().find(|t| t.id == target))
                        {
                            t.is_resolved = want;
                        }
                        this.error = None;
                        this.sync_threads(cx);
                    }
                    Err(e) => this.error = Some(format!("could not update the thread: {e}")),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// `ctrl-tab` / `ctrl-shift-tab`: move to the adjacent review in the rail.
    ///
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

    /// `S` — start a submit.
    ///
    /// Goes straight to the confirm step when everything maps: making the
    /// reviewer dismiss an empty resolver would be a dialog that says
    /// "nothing to decide".
    fn begin_submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(number), Some(data)) = (self.active_number(), self.data().cloned()) else {
            return;
        };
        let drafts = self.drafts.for_review(number).to_vec();
        let has_unmappable = !preflight(&drafts, &data.files).unmappable.is_empty();
        let submission = Submission::new(Event::Comment);
        let next = if has_unmappable {
            Mode::Resolving(submission)
        } else {
            Mode::Confirming(submission)
        };
        self.enter_mode(next, window, cx);
    }

    /// `space` in the resolver — flip the highlighted draft between being
    /// rescued into the body and being left behind.
    fn toggle_resolution(&mut self, cx: &mut Context<Self>) {
        let (Some(number), Some(data)) = (self.active_number(), self.data().cloned()) else {
            return;
        };
        let drafts = self.drafts.for_review(number).to_vec();
        let first_unmappable = preflight(&drafts, &data.files)
            .unmappable
            .first()
            .map(|(c, _)| c.id);
        if let (Mode::Resolving(sub), Some(id)) = (&mut self.mode, first_unmappable) {
            sub.toggle(id);
            cx.notify();
        }
    }

    /// `tab` in the confirm step — cycle COMMENT → APPROVE → REQUEST_CHANGES →
    /// draft.
    ///
    /// Starts at COMMENT and requires a deliberate keystroke to reach APPROVE:
    /// approving by accident is the one outcome here that cannot be taken back
    /// without another visible action on the PR.
    fn next_event(&mut self, cx: &mut Context<Self>) {
        if let Mode::Confirming(sub) = &mut self.mode {
            sub.event = match sub.event {
                Event::Comment => Event::Approve,
                Event::Approve => Event::RequestChanges,
                Event::RequestChanges => Event::Draft,
                Event::Draft => Event::Comment,
            };
            cx.notify();
        }
    }

    /// `enter` in the resolver — accept the choices and move to confirm.
    fn advance_submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Mode::Resolving(sub) = &self.mode {
            let sub = sub.clone();
            self.enter_mode(Mode::Confirming(sub), window, cx);
        }
    }

    /// `cmd-enter` in the confirm step — post the review.
    ///
    /// The lifecycle change happens in the landing closure and nowhere else,
    /// only on `Ok`. §9 requires that a failed submit leave every draft at
    /// `LocalDraft`, and the only way to be sure of that is to have exactly one
    /// place that can change it.
    fn send_review(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Mode::Confirming(sub) = &self.mode else {
            return;
        };
        let (Some(number), Some(data)) = (self.active_number(), self.data().cloned()) else {
            return;
        };
        let sub = sub.clone();
        let drafts = self.drafts.for_review(number).to_vec();

        let (json, sent, landed) = {
            let pre = preflight(&drafts, &data.files);
            if sub.check(&pre).is_err() {
                return; // the confirm step is already showing why
            }
            // The head the diff was fetched at. §5: sending a different
            // commit_id makes GitHub 422 when a strict subset of commits is in
            // play.
            let head = self
                .reviews
                .iter()
                .find(|r| r.id.number == number)
                .and_then(|r| match &r.state {
                    LoadState::Ready { head_sha, .. } => Some(head_sha.clone()),
                    _ => None,
                })
                .unwrap_or_default();
            (
                sub.payload(&head, &pre).to_string(),
                sub.sent_ids(&pre),
                sub.landed(),
            )
        };

        self.leave_mode(window, cx);
        let repo = self.repo.clone();
        let forge = self.forge.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { forge.create_review(&repo, number, &json) })
                .await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(()) => {
                        this.drafts.mark(number, &sent, landed);
                        this.persist(number);
                        this.error = None;
                    }
                    // Nothing changes. The drafts are still local, still
                    // editable, still on disk — the reviewer can fix whatever
                    // GitHub objected to and try again.
                    Err(e) => this.error = Some(format!("submit failed: {e}")),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The resolver: every draft that will not map, why, and what happens to it.
    fn render_resolver(&self, sub: &Submission) -> impl IntoElement {
        let theme = self.theme.clone();
        let mut rows = Vec::new();
        if let (Some(number), Some(data)) = (self.active_number(), self.data()) {
            let drafts = self.drafts.for_review(number).to_vec();
            for (comment, reason) in preflight(&drafts, &data.files).unmappable {
                let kept = sub.resolution(comment.id) == Resolution::MoveToSummary;
                rows.push(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .px_2()
                        .py_1()
                        .child(
                            div()
                                .text_color(if kept { theme.added_fg } else { theme.text_secondary })
                                .child(SharedString::from(format!(
                                    "{} — {}",
                                    if kept { "move to summary" } else { "omit" },
                                    reason.reason()
                                ))),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(theme.text_secondary)
                                .child(SharedString::from(comment.body.clone())),
                        ),
                );
            }
        }

        div()
            .flex()
            .flex_col()
            .gap_1()
            .w_full()
            .p_2()
            .border_t_1()
            .border_color(theme.border_subtle)
            .bg(theme.surface_raised)
            .child(
                div()
                    .text_color(theme.text_primary)
                    .child("these comments no longer fit the diff"),
            )
            .children(rows)
            .child(
                div()
                    .text_sm()
                    .text_color(theme.text_secondary)
                    .child("space toggle · enter continue · esc cancel"),
            )
    }

    /// The confirm step: what will be sent, as what kind of review.
    fn render_confirm(&self, sub: &Submission, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme.clone();
        let (mut sending, mut rescued, mut refused) = (0, 0, None);
        let mut body_preview = String::new();
        if let (Some(number), Some(diff)) = (self.active_number(), self.diff()) {
            let drafts = self.drafts.for_review(number).to_vec();
            let view = diff.read(cx);
            let pre = preflight(&drafts, view.files());
            sending = pre.mappable.len();
            rescued = sub.moved(&pre).len();
            refused = sub.check(&pre).err();
            body_preview = sub.body(&pre);
        }

        let kind = match sub.event {
            Event::Comment => "comment",
            Event::Approve => "approve",
            Event::RequestChanges => "request changes",
            Event::Draft => "save as pending draft",
        };

        div()
            .flex()
            .flex_col()
            .gap_1()
            .w_full()
            .p_2()
            .border_t_1()
            .border_color(theme.border_subtle)
            .bg(theme.surface_raised)
            .child(
                div()
                    .text_color(theme.text_primary)
                    .child(SharedString::from(format!("submit as: {kind}"))),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(theme.text_secondary)
                    .child(SharedString::from(format!(
                        "{sending} line comment(s), {rescued} moved into the body"
                    ))),
            )
            .children((!body_preview.is_empty()).then(|| {
                div()
                    .text_sm()
                    .text_color(theme.text_secondary)
                    .child(SharedString::from(body_preview))
            }))
            .children(refused.map(|r| {
                div()
                    .text_color(theme.removed_fg)
                    .child(SharedString::from(r.reason().to_string()))
            }))
            .child(
                div()
                    .text_sm()
                    .text_color(theme.text_secondary)
                    .child("tab change kind · cmd-enter send · esc cancel"),
            )
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

        // The file panel. Built here rather than inside the diff view because
        // clicking an entry scrolls that view — the parent owns the wiring
        // between the two panes.
        let file_count = self
            .diff()
            .map(|diff| {
                let view = diff.read(cx);
                file_entries(view.files(), view.rows()).len()
            })
            .unwrap_or(0);

        div()
            .when_some(self.mode.key_context(), |this, ctx| this.key_context(ctx))
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
            .on_action(cx.listener(|this, _: &NextThread, _, cx| this.step_thread(1, cx)))
            .on_action(cx.listener(|this, _: &PrevThread, _, cx| this.step_thread(-1, cx)))
            .on_action(cx.listener(|this, _: &ToggleResolved, _, cx| this.toggle_resolved(cx)))
            .on_action(cx.listener(|this, _: &ReplyToThread, window, cx| this.reply(window, cx)))
            .on_action(cx.listener(|this, _: &Suggest, window, cx| this.suggest(window, cx)))
            .on_action(cx.listener(|this, _: &NextReview, _, cx| this.step_review(1, cx)))
            .on_action(cx.listener(|this, _: &PrevReview, _, cx| this.step_review(-1, cx)))
            .on_action(cx.listener(|this, _: &Submit, window, cx| this.begin_submit(window, cx)))
            .on_action(cx.listener(|this, _: &ToggleResolution, _, cx| this.toggle_resolution(cx)))
            .on_action(cx.listener(|this, _: &NextEvent, _, cx| this.next_event(cx)))
            .on_action(cx.listener(|this, _: &AdvanceSubmit, window, cx| {
                this.advance_submit(window, cx)
            }))
            .on_action(cx.listener(|this, _: &CancelSubmit, window, cx| this.leave_mode(window, cx)))
            .on_action(cx.listener(|this, _: &SendReview, window, cx| this.send_review(window, cx)))
            .flex()
            .size_full()
            .bg(theme.surface)
            .font_family(theme.font_code)
            .text_color(theme.text_primary)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .w(px(300.))
                    .h_full()
                    .p_2()
                    .border_r_1()
                    .border_color(theme.border_subtle)
                    .child(
                        div()
                            .px_3()
                            .py_2()
                            .text_sm()
                            .text_color(theme.text_tertiary)
                            .child(SharedString::from(format!(
                                "{} — {} reviews",
                                self.repo.slug(),
                                self.reviews.len()
                            ))),
                    )
                    .child(
                        uniform_list(
                            "reviews",
                            self.reviews.len(),
                            cx.processor(|this, range: std::ops::Range<usize>, _window, cx| {
                                let theme = this.theme.clone();
                                range
                                    .map(|ix| {
                                        let selected = this.active == Some(ix);
                                        let unreviewed = this.unreviewed_count(&this.reviews[ix]);
                                        div()
                                            .id(("review", ix))
                                            .child(rail_row(
                                                &this.reviews[ix],
                                                selected,
                                                unreviewed,
                                                &theme,
                                            ))
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.select(ix, cx)
                                            }))
                                    })
                                    .collect::<Vec<_>>()
                            }),
                        )
                        .flex_1(),
                    ),
            )
            .children((file_count > 0).then(|| {
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .w(px(320.))
                    .h_full()
                    .p_2()
                    .border_r_1()
                    .border_color(theme.border_subtle)
                    .child(
                        uniform_list(
                            "files",
                            file_count,
                            cx.processor(|this, range: std::ops::Range<usize>, _window, cx| {
                                let theme = this.theme.clone();
                                let Some(diff) = this.diff() else {
                                    return Vec::new();
                                };
                                let entries = {
                                    let view = diff.read(cx);
                                    file_entries(view.files(), view.rows())
                                };
                                range
                                    .map(|ix| {
                                        let entry = &entries[ix];
                                        let entry_file = diff.read(cx).files().get(ix);
                                        let is_read = this.active_number().is_some_and(|n| {
                                            entry_file
                                                .map(|f| {
                                                    this.reviewed.is_reviewed(
                                                        n,
                                                        &entry.path,
                                                        f.content_hash(),
                                                    )
                                                })
                                                .unwrap_or(false)
                                        });
                                        let (row_ix, diff) = (entry.row_ix, diff.clone());
                                        div()
                                            .id(("file", row_ix))
                                            .child(file_row(entry, is_read, &theme))
                                            .on_click(move |_, _, cx| {
                                                diff.update(cx, |view, cx| {
                                                    view.scroll_to(row_ix);
                                                    cx.notify();
                                                });
                                            })
                                    })
                                    .collect::<Vec<_>>()
                            }),
                        )
                        .flex_1(),
                    )
            }))
            .children({
                let drafts = self.render_drafts();
                let threads = self.render_threads(cx);
                (!drafts.is_empty() || !threads.is_empty()).then(|| {
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .w(px(320.))
                        .h_full()
                        .p_2()
                        .border_l_1()
                        .border_color(theme.border_subtle)
                        .children((!drafts.is_empty()).then(|| {
                            div()
                                .px_2()
                                .text_sm()
                                .text_color(theme.text_tertiary)
                                .child("drafts")
                        }))
                        .children(drafts)
                        .children(threads)
                })
            })
            .child(match self.diff() {
                Some(diff) => div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .h_full()
                    .child(div().flex_1().min_h(px(0.)).child(diff))
                    // Beneath the diff and above the composer, because that is
                    // where the reviewer is looking when a write fails. In the
                    // rail it sat beside the PR list, a pane away from the
                    // action that produced it.
                    .children(self.error.clone().map(|e| {
                        div()
                            .px_2()
                            .py_1()
                            .border_t_1()
                            .border_color(theme.border_subtle)
                            .text_sm()
                            .text_color(theme.removed_fg)
                            .child(SharedString::from(e))
                    }))
                    .children(match &self.mode {
                        Mode::Composing(c) => Some(self.render_composer(c, cx).into_any_element()),
                        Mode::Resolving(s) => Some(self.render_resolver(s).into_any_element()),
                        Mode::Confirming(s) => Some(self.render_confirm(s, cx).into_any_element()),
                        _ => None,
                    })
                    .into_any_element(),
                None => {
                    let active = self.active.and_then(|ix| self.reviews.get(ix));
                    let (text, colour) = match placeholder(active) {
                        Placeholder::NothingSelected => {
                            (SharedString::from("select a review"), theme.text_secondary)
                        }
                        Placeholder::Loading => {
                            (SharedString::from("loading…"), theme.text_secondary)
                        }
                        Placeholder::Failed(message) => (
                            SharedString::from(format!("failed: {message}")),
                            theme.removed_fg,
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
    use super::{DiffView, Entity, Mode, Placeholder, Residency, Theme, Workspace, placeholder};
    use gpui::AppContext as _;
    use diffident_forge::gh::FakeGh;
    use diffident_forge::github::GitHub;
    use diffident_forge::threads::{ReviewThread, ThreadComment};
    use diffident_model::{LoadState, Review, ReviewId};
    use std::sync::Arc;

    /// A diff with one file and enough rows to scroll: 1 context, 1 removed,
    /// 1 added, then 30 more context lines.
    fn many_rows() -> String {
        let mut d =
            String::from("diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,32 +1,32 @@\n ctx\n-old\n+new\n");
        for i in 0..30 {
            d.push_str(&format!(" line{i}\n"));
        }
        d
    }

    /// A `Workspace` with one review resident, drawn, and ready for keystrokes.
    ///
    /// Seeded directly rather than driven through a fetch: these tests are
    /// about what the *keymap* does once a review is on screen, and standing
    /// one up through `select` would make every one of them also a test of the
    /// loader.
    fn workspace_with_diff(
        cx: &mut gpui::TestAppContext,
        forge: GitHub<FakeGh>,
        threads: Vec<ReviewThread>,
    ) -> (Entity<Workspace>, &mut gpui::VisualTestContext) {
        let files = diffident_diff::parser::parse(&many_rows());
        let rows = diffident_diff::rows::build_rows(&files);
        let data = Arc::new(crate::loader::ReviewData {
            highlights: vec![Vec::new(); rows.len()],
            files,
            rows,
        });
        let repo = diffident_forge::Repo {
            owner: "o".into(),
            name: "r".into(),
        };
        // The keymap is installed by `main.rs`, not by `Workspace`, so a test
        // app has none until it does the same. Using the real `key_bindings()`
        // is the point: these tests exercise the bindings that ship.
        cx.update(|cx| cx.bind_keys(crate::navigate::key_bindings()));
        let (workspace, cx) = cx.add_window_view(|window, cx| {
            let mut this = Workspace::new(Arc::new(forge), repo, None, window, cx);
            this.reviews.push(Review {
                id: ReviewId {
                    repo: "o/r".into(),
                    number: 1,
                },
                title: "t".into(),
                branch: "b".into(),
                depth: 0,
                is_draft: false,
                head_sha: "abc".into(),
                rebased: false,
                state: LoadState::Idle,
            });
            this.active = Some(0);
            this.threads.insert(1, Ok(threads));
            let view = cx.new(|_| DiffView::new(data.clone(), Theme::dark()));
            this.residency.admit(1, (data, view), "abc");
            // The keymap only reaches the workspace when its own handle holds
            // focus — the same thing `enter_mode` does on every mode change.
            this.focus.focus(window, cx);
            this
        });
        // A frame must be drawn before any keystroke: key contexts live on the
        // element tree, so an undrawn window has no `Diff` scope to match.
        cx.run_until_parked();
        (workspace, cx)
    }

    /// Everything below rests on this: a simulated keystroke must travel the
    /// real path — keymap, key context, focus chain, action handler. If it
    /// does not, the rest of these tests would pass while asserting nothing.
    #[gpui::test]
    fn a_simulated_keystroke_reaches_the_real_action_handler(cx: &mut gpui::TestAppContext) {
        let (workspace, cx) =
            workspace_with_diff(cx, GitHub::new(FakeGh::new()), Vec::new());
        let start = workspace.read_with(cx, |this, cx| this.diff().unwrap().read(cx).cursor);
        cx.simulate_keystrokes("j");
        let moved = workspace.read_with(cx, |this, cx| this.diff().unwrap().read(cx).cursor);
        assert_eq!(moved, start + 1, "`j` must move the cursor one row");
    }

    /// An unresolved thread anchored to `line` on the new side.
    fn thread_at(line: u32) -> ReviewThread {
        ReviewThread {
            id: "PRRT_1".into(),
            path: "a.rs".into(),
            line: Some(line),
            original_line: Some(line),
            on_old_side: false,
            is_resolved: false,
            is_outdated: false,
            comments: vec![ThreadComment {
                id: "PRRC_1".into(),
                author: "octocat".into(),
                body: "nit".into(),
            }],
        }
    }

    /// §9's Phase 7 gate, half of it: "resolve … round-trips to GitHub".
    ///
    /// Driven through the real keymap rather than by calling the handler, so
    /// this covers the binding, the key context and the focus chain too — the
    /// three things that have broken before and that a direct call would miss.
    #[gpui::test]
    fn shift_space_resolves_the_selected_thread(cx: &mut gpui::TestAppContext) {
        let forge = GitHub::new(FakeGh::new().with(
            "api graphql --input -",
            r#"{"data":{"resolveReviewThread":{"thread":{"id":"PRRT_1","isResolved":true}}}}"#,
        ));
        let (workspace, cx) = workspace_with_diff(cx, forge, vec![thread_at(2)]);

        assert!(
            !workspace.read_with(cx, |this, _| this.threads_of(1)[0].is_resolved),
            "starts open"
        );
        cx.simulate_keystrokes("shift-space");
        cx.run_until_parked();

        workspace.read_with(cx, |this, _| {
            assert!(this.threads_of(1)[0].is_resolved, "GitHub took it, so it reads resolved");
            assert!(this.error.is_none(), "and nothing is reported as wrong");
        });
    }

    /// The other half: "reply … round-trips to GitHub".
    ///
    /// `a` opens the composer, which holds no key context, so the typing below
    /// travels a different path from the keystroke that opened it — that seam
    /// is exactly where Phase 5b once left focus orphaned and killed every key
    /// in the app.
    #[gpui::test]
    fn a_then_typing_then_cmd_enter_posts_a_reply(cx: &mut gpui::TestAppContext) {
        let forge = GitHub::new(FakeGh::new().with(
            "api graphql --input -",
            r#"{"data":{"addPullRequestReviewThreadReply":{"comment":
               {"id":"PRRC_9","author":{"login":"rranjan14"},"body":"done"}}}}"#,
        ));
        let (workspace, cx) = workspace_with_diff(cx, forge, vec![thread_at(2)]);

        cx.simulate_keystrokes("a");
        assert!(
            workspace.read_with(cx, |this, _| matches!(this.mode, Mode::Composing(_))),
            "`a` opens the composer"
        );
        cx.simulate_input("done");
        cx.simulate_keystrokes("cmd-enter");
        cx.run_until_parked();

        workspace.read_with(cx, |this, _| {
            let comments = &this.threads_of(1)[0].comments;
            assert_eq!(comments.len(), 2, "the reply joined the thread");
            assert_eq!(comments[1].body, "done");
            assert_eq!(
                comments[1].author, "rranjan14",
                "attributed from GitHub's own answer, not guessed locally"
            );
            assert!(
                matches!(this.mode, Mode::Browsing),
                "and the composer closed, handing focus back"
            );
        });
    }

    /// A tall row at the very end is where scroll clamping breaks: the content
    /// grows after the list has already decided where the end is. Nothing else
    /// in the suite puts a thread on the last line of a diff.
    #[gpui::test]
    fn the_last_line_stays_reachable_with_a_thread_on_it(cx: &mut gpui::TestAppContext) {
        // 32 is the final new-side line number in the fixture.
        let (workspace, cx) =
            workspace_with_diff(cx, GitHub::new(FakeGh::new()), vec![thread_at(32)]);

        let (last_row, thread_row) = workspace.read_with(cx, |this, cx| {
            let view = this.diff().unwrap();
            let rows = view.read(cx).rows().len();
            let placed = crate::threads::inline_groups(
                &this.threads_of(1),
                &this.data().unwrap().files,
                view.read(cx).rows(),
            );
            (rows - 1, placed.first().map(|(row, _)| *row))
        });
        assert!(
            thread_row.is_some_and(|r| r >= last_row.saturating_sub(3)),
            "the thread really is near the end of the diff, or this proves nothing"
        );

        cx.simulate_keystrokes("shift-g");
        assert_eq!(
            workspace.read_with(cx, |this, cx| this.diff().unwrap().read(cx).cursor),
            last_row,
            "G must reach the final row even though a tall thread sits just above it"
        );
    }

    /// One unresolved thread, the state the guarantee below is about.
    fn open_thread() -> ReviewThread {
        ReviewThread {
            id: "PRRT_1".into(),
            path: "a.rs".into(),
            line: Some(1),
            original_line: Some(1),
            on_old_side: false,
            is_resolved: false,
            is_outdated: false,
            comments: vec![ThreadComment {
                id: "PRRC_1".into(),
                author: "octocat".into(),
                body: "nit".into(),
            }],
        }
    }

    /// A key that resolves to nothing must say so.
    ///
    /// Pressing `c` on a file header or a spacer cannot produce a comment
    /// scope, and returning silently is indistinguishable from a broken app —
    /// the reviewer presses it again harder. `p` used to report one of its two
    /// failures and swallow the other four lines away, which is how this got
    /// noticed.
    #[gpui::test]
    fn a_comment_key_with_nowhere_to_land_says_why(cx: &mut gpui::TestAppContext) {
        let forge = Arc::new(GitHub::new(FakeGh::new()));
        let repo = diffident_forge::Repo {
            owner: "o".into(),
            name: "r".into(),
        };
        let window =
            cx.add_window(|window, cx| Workspace::new(forge, repo, None, window, cx));

        window
            .update(cx, |this, _, cx| {
                assert!(this.error.is_none(), "nothing has gone wrong yet");
                this.refuse("there is no line here to comment on", cx);
                assert_eq!(
                    this.error.as_deref(),
                    Some("there is no line here to comment on"),
                    "the reviewer is told, rather than left pressing the key again"
                );
            })
            .unwrap();
    }

    /// "Nobody has commented" and "we could not find out" are different
    /// statements, and the reviewer acts differently on each. They used to be
    /// two maps whose combination every reader had to reconstruct by hand;
    /// now one value carries both, and a review nobody has fetched yet is a
    /// third thing again — absent, not empty.
    #[gpui::test]
    fn an_unfetched_an_empty_and_a_failed_review_are_three_different_states(
        cx: &mut gpui::TestAppContext,
    ) {
        let forge = Arc::new(GitHub::new(FakeGh::new()));
        let repo = diffident_forge::Repo {
            owner: "o".into(),
            name: "r".into(),
        };
        let window =
            cx.add_window(|window, cx| Workspace::new(forge, repo, None, window, cx));

        window
            .update(cx, |this, _, _| {
                this.threads.insert(1, Ok(Vec::new()));
                this.threads.insert(2, Err("rate limited".into()));

                assert!(!this.threads.contains_key(&3), "never fetched");
                assert!(
                    this.threads.get(&1).is_some_and(|t| t.is_ok()),
                    "fetched, and there is genuinely nothing there"
                );
                assert!(
                    this.threads.get(&2).is_some_and(|t| t.is_err()),
                    "fetched and failed — not the same as having none"
                );

                // Everything that acts on a thread sees the failure as "no
                // threads to act on", so none of them has to handle it.
                assert!(this.threads_of(2).is_empty());
            })
            .unwrap();
    }

    /// The LRU is what bounds memory: a large diff is tens of MB (§10), and
    /// four stay resident. Sharing the parsed diff behind an `Arc` put a second
    /// handle in play, so this pins the thing that would otherwise rot in
    /// silence — evicting a review must actually release its diff, not just
    /// drop one of two references to it. If a clone ever gets parked in a
    /// longer-lived map, the app keeps every diff it has ever opened and
    /// nothing else fails.
    #[gpui::test]
    fn evicting_a_review_releases_its_diff(cx: &mut gpui::TestAppContext) {
        use crate::loader::ReviewData;
        let mut residency: Residency<(Arc<ReviewData>, Entity<DiffView>)> =
            Residency::new(super::RESIDENT);
        let first = Arc::new(ReviewData {
            files: Vec::new(),
            rows: Vec::new(),
            highlights: Vec::new(),
        });
        let watch = Arc::downgrade(&first);

        for n in 0..=super::RESIDENT as u32 {
            let data = if n == 0 {
                first.clone()
            } else {
                Arc::new(ReviewData {
                    files: Vec::new(),
                    rows: Vec::new(),
                    highlights: Vec::new(),
                })
            };
            let view = cx.new(|_| DiffView::new(data.clone(), Theme::dark()));
            residency.admit(n, (data, view), "head");
        }
        drop(first);
        // gpui reclaims a dropped entity during `flush_effects`, and dropping
        // the last handle does not by itself queue an effect — so force one.
        // In the running app every frame does this; here nothing else would.
        cx.update(|_| {});

        assert!(
            watch.upgrade().is_none(),
            "the evicted review's diff is still alive — the residency bound is decorative"
        );
    }

    /// §7: "a failed submit leaves everything at LocalDraft" — the same rule
    /// governs a failed resolve, and until the forge could be injected there
    /// was no way to test either. `FakeGh` with nothing registered fails every
    /// call, which is exactly the network error this guards against.
    #[gpui::test]
    fn a_failed_resolve_leaves_the_thread_as_it_was(cx: &mut gpui::TestAppContext) {
        let forge = Arc::new(GitHub::new(FakeGh::new()));
        let repo = diffident_forge::Repo {
            owner: "o".into(),
            name: "r".into(),
        };
        let window = cx.add_window(|window, cx| {
            let mut this = Workspace::new(forge, repo, None, window, cx);
            // Stand the review up directly rather than driving a whole fetch:
            // the guarantee under test is about what happens *after* the
            // mutation fails, not about how the thread got here.
            this.reviews.push(Review {
                id: ReviewId {
                    repo: "o/r".into(),
                    number: 1,
                },
                title: "t".into(),
                branch: "b".into(),
                depth: 0,
                is_draft: false,
                head_sha: "abc".into(),
                rebased: false,
                state: LoadState::Idle,
            });
            this.active = Some(0);
            this.threads.insert(1, Ok(vec![open_thread()]));
            this
        });

        window
            .update(cx, |this, _, cx| this.toggle_resolved(cx))
            .unwrap();
        cx.run_until_parked();

        window
            .update(cx, |this, _, _| {
                assert!(
                    !this.threads_of(1)[0].is_resolved,
                    "GitHub rejected the write, so the thread must still read open"
                );
                assert!(
                    this.error.is_some(),
                    "and the reviewer must be told, or they move on believing it resolved"
                );
            })
            .unwrap();
    }

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

    #[test]
    fn every_mutation_of_threads_syncs_the_view() {
        // `DiffView` holds its own copies of the threads. A site that moves
        // `self.thread_cursor` without pushing the result into the view leaves
        // the reviewer looking at a selection that is no longer there, and
        // nothing fails.
        //
        // Structural rather than a threshold: each assignment must be followed
        // by a sync somewhere in the same function. The count-based version of
        // this test could never notice a *new* mutation site — which is exactly
        // how `select` shipped with no sync on either of its early returns.
        //
        // What it still does not catch: a sync that pushes into the wrong
        // review, or a mutation of `self.threads` rather than the cursor. Those
        // are on the reviewer, which is why `sync_threads_for` says so in its
        // own doc.
        let src = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/workspace.rs"));
        // Split so this test's own source does not match the needles it is
        // searching for — the file it reads is the file it lives in.
        let assignment = concat!("thread_", "cursor = ");
        let sync = concat!("sync_", "threads");
        // A `}` at four-space indent closes the enclosing `fn`, so this is the
        // rest of the function the assignment sits in.
        let sites: Vec<&str> = src
            .match_indices(assignment)
            .map(|(at, _)| {
                let rest = &src[at..];
                &rest[..rest.find("\n    }").unwrap_or(rest.len())]
            })
            .collect();
        assert!(
            sites.len() >= 2,
            "expected at least `select` and `step_thread` to move the cursor, \
             found {} assignments — did the field get renamed?",
            sites.len()
        );
        for site in &sites {
            assert!(
                site.contains(sync),
                "a `thread_cursor` assignment with no sync of the view after it \
                 in the same function — the diff keeps the old selection:\n{site}"
            );
        }
    }

    /// The defect this whole plan exists to fix: the rail was a plain flex
    /// column, so a review past the window edge could not be reached at all.
    /// `uniform_list` both scrolls and virtualises — a 400-file PR builds ~40
    /// elements rather than 400.
    #[gpui::test]
    fn every_review_is_reachable_however_many_there_are(cx: &mut gpui::TestAppContext) {
        let (workspace, cx) = workspace_with_diff(cx, GitHub::new(FakeGh::new()), Vec::new());
        workspace.update(cx, |this, _| {
            for n in 2..80u32 {
                this.reviews.push(Review {
                    id: ReviewId { repo: "o/r".into(), number: n },
                    title: format!("review number {n}"),
                    branch: "b".into(),
                    depth: 0,
                    is_draft: false,
                    head_sha: "abc".into(),
                    rebased: false,
                    state: LoadState::Idle,
                });
            }
        });
        cx.run_until_parked();

        // Selecting the last one must work — under the old flex column it was
        // rendered off-screen with no way to scroll to it.
        workspace.update(cx, |this, cx| this.select(this.reviews.len() - 1, cx));
        cx.run_until_parked();
        assert_eq!(
            workspace.read_with(cx, |this, _| this.active_number()),
            Some(79),
            "the last review is selectable"
        );
    }

    /// `select` is index-based and does not need a rendered row, so the smoke
    /// test above cannot catch a flex-column rail. This one reads the source.
    #[test]
    fn the_rail_is_a_uniform_list_over_every_review() {
        let src = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/workspace.rs"));
        // Split so this test's own source does not match the needles.
        let list = concat!("uniform", "_list(");
        let count = concat!("self.reviews", ".len()");
        let old = concat!(".children(", "rail)");
        assert!(
            src.contains(list),
            "the rail must be a virtualised list, not a flex column"
        );
        assert!(
            src.contains(count),
            "that list's item count must be the review vec's length"
        );
        assert!(
            !src.contains(old),
            "the rail must not still push every review into a flex column"
        );
    }
}
