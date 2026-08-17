use crate::density;
use crate::loader::ReviewData;
use crate::theme::Theme;
use std::sync::Arc;
use diffident_diff::{DiffFile, LineKind, Row};
use diffident_forge::threads::ReviewThread;
use gpui::{
    Bounds, Context, HighlightStyle, IntoElement, ListAlignment, ListState, ParentElement, Pixels,
    Render, SharedString, StyledText, Window, div, list, point, prelude::*, px, rgb,
};
use std::ops::Range;


/// One review's diff, rendered as a virtualised list over the flat row model.
///
/// Sole owner of the element choice: nothing outside this file knows that
/// `list()` is in use rather than `uniform_list`. That choice is what lets a
/// row's height vary, which is what lets a thread render inline under the
/// line it replies to (§3, §7).
pub struct DiffView {
    /// The parsed diff, shared with `Workspace` rather than owned here.
    /// Immutable for the view's whole life; see `ReviewData`'s doc for why the
    /// `Arc` must not outlive its residency entry.
    data: Arc<ReviewData>,
    /// The list's own state. Owns scroll position and per-row measurements.
    list: ListState,
    theme: Theme,
    /// Row the keyboard cursor is on.
    pub cursor: usize,
    /// Threads to draw under each row, in row order (§7). Owned, because the
    /// view outlives any borrow of the workspace's map.
    inline: Vec<(usize, Vec<ReviewThread>)>,
    /// Node id of the thread the reviewer has selected, so the inline copy can
    /// show the same selection the right-hand pane does.
    selected_thread: Option<String>,
    /// Search hits, in row order. Pushed in rather than computed here: matching
    /// is pure and belongs in `search`, and the view only needs to paint.
    matches: Vec<crate::search::Match>,
    /// Soft wrap. On by default: a clipped line hides code the reviewer is
    /// being asked to approve. `w` turns it off for when column alignment
    /// matters more, e.g. an aligned struct literal or a table in a comment.
    wrap: bool,
    /// Viewport width last seen while wrap was on. Compared in `render` so a
    /// resize re-measures every row.
    last_width: f32,
}

impl DiffView {
    /// `highlights` must be index-parallel with `rows`; `loader::load_review`
    /// produces it on a background thread. Deliberately not computed here —
    /// this constructor runs on the foreground, and highlighting a 10k-row diff
    /// takes ~530ms, which froze the window for exactly that long.
    pub fn new(data: Arc<ReviewData>, theme: Theme) -> Self {
        debug_assert_eq!(
            data.highlights.len(),
            data.rows.len(),
            "highlights must be row-parallel"
        );
        let rows_len = data.rows.len();
        let line_height = theme.line_height;
        Self {
            data,
            // A uniform height *hint*, not a constraint: every row starts out
            // assumed to be one line tall, and real heights replace the hint as
            // rows are measured. `measure_all()` would instead lay out every
            // row up front — on a 10k-row diff that is the same class of
            // mistake as computing highlights on the foreground thread.
            list: ListState::new(rows_len, ListAlignment::Top, px(line_height * 20.))
                .with_uniform_item_height(px(line_height)),
            theme,
            cursor: 0,
            inline: Vec::new(),
            selected_thread: None,
            matches: Vec::new(),
            wrap: true,
            last_width: 0.,
        }
    }

    pub fn wrap(&self) -> bool {
        self.wrap
    }

    /// Toggling wrap changes every row's height, so every row must be
    /// re-measured — otherwise the list draws them at their old heights.
    pub fn set_wrap(&mut self, wrap: bool) {
        self.wrap = wrap;
        self.list.remeasure_items(0..self.data.rows.len());
    }

    pub fn toggle_wrap(&mut self) {
        self.set_wrap(!self.wrap);
    }

    /// The visible area of the list, as laid out on the last frame.
    ///
    /// Read from the scroll handle rather than stored: the handle already
    /// tracks it, and a separate field has to be assigned from inside the
    /// render pass — which is exactly what was missed before, leaving the
    /// viewport permanently zero and the scrollbar permanently invisible.
    ///
    /// Zero-sized before the first layout. Every caller must tolerate that.
    fn viewport(&self) -> Bounds<Pixels> {
        self.list.viewport_bounds()
    }

    /// Total height of all rows, as far as the list has measured them.
    ///
    /// No longer `rows * line_height`: once threads render inline some rows are
    /// much taller. The list tracks the real total, using the uniform hint for
    /// rows it has not measured yet, so this is exact from the first frame for
    /// a diff with no threads and converges as thread rows are measured.
    fn content_height(&self) -> Pixels {
        self.viewport().size.height + self.list.max_offset_for_scrollbar().y
    }

    pub fn rows(&self) -> &[Row] {
        &self.data.rows
    }

    pub fn files(&self) -> &[DiffFile] {
        &self.data.files
    }

    /// Replace the inline threads and tell the list which rows changed height.
    ///
    /// `remeasure_items` rather than `reset` or `splice`. `reset` discards the
    /// scroll position, so the diff would jump to the top every time someone
    /// resolved a thread. `splice` replaces the rows with unmeasured ones,
    /// which throws away the uniform height hint — the affected rows would
    /// count as *zero* tall until next drawn, and these are precisely the tall
    /// ones. `remeasure_items` is documented for exactly this case: same index,
    /// same count, changed content, possibly changed height, and it anchors the
    /// scroll position when the changed row is the topmost visible one.
    pub fn set_threads(
        &mut self,
        groups: Vec<(usize, Vec<ReviewThread>)>,
        selected: Option<String>,
    ) {
        let touched: std::collections::BTreeSet<usize> = self
            .inline
            .iter()
            .chain(groups.iter())
            .map(|(row, _)| *row)
            .collect();
        self.inline = groups;
        self.selected_thread = selected;
        for row in touched {
            if row < self.data.rows.len() {
                self.list.remeasure_items(row..row + 1);
            }
        }
    }

    /// Replace the search hits and re-measure nothing.
    ///
    /// Unlike `set_threads`, a match changes a row's colours but never its
    /// height, so the measured layout stays valid and the list needs no
    /// remeasure at all.
    pub fn set_matches(&mut self, matches: Vec<crate::search::Match>) {
        self.matches = matches;
    }

    /// The threads that render under row `ix`, if any.
    fn threads_at(&self, ix: usize) -> &[ReviewThread] {
        self.inline
            .iter()
            .find(|(row, _)| *row == ix)
            .map(|(_, ts)| ts.as_slice())
            .unwrap_or_default()
    }

    /// Scroll so `ix` is visible. Used by the file panel and by navigation.
    pub fn scroll_to(&mut self, ix: usize) {
        self.cursor = ix.min(self.data.rows.len().saturating_sub(1));
        self.list.scroll_to_reveal_item(self.cursor);
    }

    /// How many rows `ctrl-d` moves: as many as fill half the viewport.
    ///
    /// Measured, not computed — rows have not been a uniform height since
    /// threads went inline, and wrapping made the gap worse. `bounds_for_item`
    /// returns `None` for anything before the current scroll top or not yet
    /// rendered, so this walks forward only and naturally stops at the last
    /// drawn row.
    ///
    /// Half-page *up* reuses the same count rather than walking backwards,
    /// which `bounds_for_item` cannot do. That makes `ctrl-u` approximate on a
    /// diff with wildly varying row heights — acceptable, and the same
    /// approximation the fixed-height version always made.
    pub fn rows_per_half_page(&self) -> usize {
        let half = self.viewport().size.height / 2.;
        if half <= px(0.) {
            return 1;
        }
        let top = self.list.logical_scroll_top().item_ix;
        let mut used = px(0.);
        let mut rows = 0usize;
        for ix in top..self.data.rows.len() {
            let Some(b) = self.list.bounds_for_item(ix) else {
                break;
            };
            used += b.size.height;
            rows += 1;
            if used >= half {
                break;
            }
        }
        rows.max(1)
    }

    /// Jump the list so `y` (window pixels) is the new scroll fraction.
    fn jump_to_y(&mut self, y: f32) {
        let bounds = self.viewport();
        let top: f32 = bounds.origin.y.into();
        let h: f32 = bounds.size.height.into();
        if h <= 0. {
            return;
        }
        let frac = ((y - top) / h).clamp(0., 1.);
        let max = self.list.max_offset_for_scrollbar().y;
        self.list
            .set_offset_from_scrollbar(point(px(0.), -(max * frac)));
    }

    fn viewport_frac(&self) -> (f32, f32) {
        let content: f32 = self.content_height().into();
        let view_h: f32 = self.viewport().size.height.into();
        if content <= 0. {
            return (0., 1.);
        }
        let scroll: f32 = (-self.list.scroll_px_offset_for_scrollbar().y).into();
        (
            (scroll / content).clamp(0., 1.),
            (view_h / content).clamp(0., 1.),
        )
    }

    fn render_row(&self, ix: usize, theme: &Theme) -> impl IntoElement + use<> {
        match self.data.rows[ix] {
            Row::FileHeader { file_ix } => div()
                .px_2()
                .h(px(theme.line_height))
                .bg(theme.surface_raised)
                .text_color(theme.text_primary)
                .child(SharedString::from(
                    self.data.files[file_ix].display_path().to_string(),
                ))
                .into_any_element(),
            Row::HunkHeader { file_ix, hunk_ix } => {
                let h = &self.data.files[file_ix].hunks[hunk_ix];
                div()
                    .px_2()
                    .h(px(theme.line_height))
                    .text_color(theme.text_secondary)
                    .child(SharedString::from(format!(
                        "@@ -{},{} +{},{} @@ {}",
                        h.old_start, h.old_count, h.new_start, h.new_count, h.section
                    )))
                    .into_any_element()
            }
            Row::Line {
                file_ix,
                hunk_ix,
                line_ix,
            } => {
                let line = &self.data.files[file_ix].hunks[hunk_ix].lines[line_ix];
                let (bg, sigil) = match line.kind {
                    LineKind::Added => (theme.added_bg, "+"),
                    LineKind::Removed => (theme.removed_bg, "-"),
                    LineKind::Context => (theme.surface, " "),
                };
                let style = theme.code_style();
                // Matches are laid *over* syntax rather than appended to it:
                // `with_default_highlights` requires sorted, non-overlapping
                // ranges (§3), and a hit inside a keyword overlaps it.
                let hits: Vec<crate::search::Match> = self
                    .matches
                    .iter()
                    .filter(|m| m.row == ix)
                    .copied()
                    .collect();
                let ranges: Vec<(Range<usize>, HighlightStyle)> =
                    crate::search::overlay(&self.data.highlights[ix], &hits)
                        .into_iter()
                        .map(|s| {
                            (
                                s.range,
                                HighlightStyle {
                                    color: s.color.map(|c| rgb(c).into()),
                                    background_color: s
                                        .is_match
                                        .then(|| theme.accent_soft.into()),
                                    ..Default::default()
                                },
                            )
                        })
                        .collect();
                let num = |n: Option<u32>| {
                    div()
                        .w(px(38.))
                        .text_color(theme.text_tertiary)
                        .child(SharedString::from(
                            n.map(|n| n.to_string()).unwrap_or_default(),
                        ))
                };
                let gutter = div()
                    .flex()
                    .flex_none()
                    .child(num(line.old_lineno))
                    .child(num(line.new_lineno))
                    .child(div().w(px(14.)).child(sigil));
                let code = div()
                    .flex()
                    .items_start()
                    .when(ix == self.cursor, |this| this.bg(theme.accent_soft))
                    .when(ix != self.cursor, |this| this.bg(bg))
                    .child(gutter)
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .when(self.wrap, |d| d.whitespace_normal())
                            .when(!self.wrap, |d| d.truncate())
                            .child(
                                StyledText::new(line.text.clone())
                                    .with_default_highlights(&style, ranges),
                            ),
                    );

                let threads = self.threads_at(ix);
                if threads.is_empty() {
                    // The overwhelmingly common case. Returning the bare line
                    // keeps its measured height exactly one line, which is what
                    // the uniform hint assumes.
                    return code.into_any_element();
                }
                div()
                    .flex()
                    .flex_col()
                    .child(code)
                    .children(
                        threads
                            .iter()
                            .map(|t| self.render_inline_thread(t, theme).into_any_element()),
                    )
                    .into_any_element()
            }
            Row::Expander { hidden, .. } => div()
                .px_2()
                .h(px(theme.line_height))
                .bg(theme.surface_raised)
                .text_color(theme.text_secondary)
                .child(match hidden {
                    Some(n) => SharedString::from(format!("··· {n} unchanged lines")),
                    None => SharedString::from("··· expand to end of file"),
                })
                .into_any_element(),
            Row::Spacer => div().h(px(theme.line_height)).into_any_element(),
        }
    }

    /// One conversation, drawn under the line it refers to.
    ///
    /// Indented past the gutter so it reads as attached to the code rather
    /// than as another line of it, and boxed so a long comment cannot be
    /// mistaken for the file's contents.
    fn render_inline_thread(&self, thread: &ReviewThread, theme: &Theme) -> impl IntoElement {
        let status = crate::comment_view::status_label(thread);
        let is_selected = self.selected_thread.as_deref() == Some(thread.id.as_str());
        div()
            .flex()
            .flex_col()
            .gap(px(theme.s1))
            // Aligned to the code column, not the window edge: the conversation
            // belongs to the line, so it starts where the line's text does.
            .ml(px(90.))
            .my(px(theme.s1))
            .pl(px(theme.s3))
            .py(px(theme.s2))
            .border_l_2()
            .border_color(if is_selected { theme.accent } else { theme.border_strong })
            .bg(theme.surface_raised)
            .rounded_r(px(theme.r_sm))
            .child(
                div()
                    .text_sm()
                    .text_color(theme.text_tertiary)
                    .child(SharedString::from(status.to_string())),
            )
            .children(crate::comment_view::thread_comments(
                thread,
                theme,
                thread.is_resolved,
            ))
    }
}

impl Render for DiffView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Any width change re-wraps every line, so every measured height is
        // stale. Nothing needed this before wrap existed.
        let w: f32 = self.viewport().size.width.into();
        if self.wrap && (w - self.last_width).abs() > 0.5 {
            self.last_width = w;
            self.list.remeasure_items(0..self.data.rows.len());
        }

        let thread_rows: Vec<usize> = self.inline.iter().map(|(ix, _)| *ix).collect();
        let bar = density::track(
            self.data.clone(),
            thread_rows,
            self.viewport_frac(),
            &self.theme,
            cx.entity(),
            Self::jump_to_y,
        );

        div()
            .relative()
            .size_full()
            .bg(self.theme.surface)
            .child(
                list(
                    self.list.clone(),
                    cx.processor(|this, ix: usize, _window, _cx| {
                        let theme = this.theme.clone();
                        this.render_row(ix, &theme).into_any_element()
                    }),
                )
                .size_full(),
            )
            .child(bar)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use diffident_diff::{parser::parse, rows::build_rows};

    const RUST: &str = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,3 +1,3 @@\n fn main() {}\n-let x = 1;\n+let y = 2;\n";

    /// The parsed fixture, in the shared shape the view now takes.
    fn data() -> Arc<ReviewData> {
        let files = parse(RUST);
        let rows = build_rows(&files);
        let highlights = vec![Vec::new(); rows.len()];
        Arc::new(ReviewData {
            files,
            rows,
            highlights,
        })
    }

    #[test]
    fn a_freshly_built_view_reports_a_zero_viewport_rather_than_a_stale_one() {
        // Regression: `bounds` used to be a field initialised to
        // Bounds::default() and never assigned, so the viewport stayed zero
        // *forever* and thumb() returned None on every frame — the scrollbar
        // was never drawn. Reading it from the scroll handle means the value
        // is zero only until the first layout, not permanently.
        let view = DiffView::new(data(), Theme::dark());
        assert_eq!(view.viewport().size.height, px(0.), "pre-layout");
        assert_eq!(
            view.rows_per_half_page(),
            1,
            "must fall back to 1 pre-layout, not panic or divide by zero"
        );
    }

    #[test]
    fn an_unmeasured_view_reports_one_line_per_row() {
        // Before layout every row still carries the uniform height hint, so the
        // total is exact. Once threads render inline this stops being the rule
        // and becomes just the starting point — the list replaces each hint
        // with a real measurement as the row is drawn.
        let theme = Theme::dark();
        let d = data();
        let expected = px(d.rows.len() as f32 * theme.line_height);
        let view = DiffView::new(d, theme);
        assert_eq!(view.content_height(), expected);
    }

    #[test]
    fn a_scrollbar_appears_once_the_viewport_is_smaller_than_the_content() {
        // Proves the geometry the render pass feeds scrollbar(): with a real
        // viewport there IS a thumb. Pairs with the pre-layout test above,
        // which proves the old permanently-zero viewport yielded none.
        let files = parse(RUST);
        let rows = build_rows(&files);
        let theme = Theme::dark();
        let content = px(rows.len() as f32 * theme.line_height);
        assert!(
            crate::density::thumb(px(40.), content, px(0.)).is_some(),
            "a viewport shorter than the content must produce a thumb"
        );
    }

    /// The first test in this crate to run inside a real gpui app.
    ///
    /// Everything above is a pure function; this proves the harness itself —
    /// that `TestAppContext` builds an app with no window server and no fonts,
    /// and that our own view types survive being created as entities inside
    /// it. Until now nothing in `crates/ui` could construct an entity in a
    /// test, which is why two guard tests elsewhere resort to reading their
    /// own source file. `Workspace` follows once it has somewhere to inject a
    /// forge — until then it would shell out to real `gh` on the first
    /// `run_until_parked`.
    #[test]
    fn wrap_is_on_by_default_and_set_wrap_flips_it() {
        let mut view = DiffView::new(data(), Theme::dark());
        assert!(view.wrap(), "clipped lines hide the code under review");
        assert_eq!(view.last_width, 0.0);
        view.set_wrap(false);
        assert!(!view.wrap());
        view.set_wrap(true);
        assert!(view.wrap());
    }

    #[test]
    fn a_line_row_uses_the_wrap_incantation_and_a_two_column_gutter() {
        let src = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/diff_view.rs"));
        let flex = concat!("flex", "_1()");
        let min = concat!("min_w(px(", "0.))");
        let wrap_on = concat!("whitespace_", "normal()");
        let wrap_off = concat!(".trun", "cate()");
        let old_col = concat!("px(38", ".)");
        let sigil = concat!("px(14", ".)");
        let single = concat!(".or(line.", "old_lineno)");
        assert!(src.contains(flex), "the code child must take leftover width");
        assert!(src.contains(min), "flex will not wrap unless it can shrink");
        assert!(src.contains(wrap_on), "wrap on → whitespace_normal");
        assert!(src.contains(wrap_off), "wrap off → truncate");
        assert!(src.contains(old_col), "each lineno column is 38px");
        assert!(src.contains(sigil), "the sigil column is 14px");
        assert!(
            !src.contains(single),
            "the gutter must not collapse to one number"
        );
    }

    #[test]
    fn half_page_walks_measured_row_bounds_and_remeasures_on_width() {
        let src = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/diff_view.rs"));
        let walk = concat!("bounds_for_", "item");
        let computed = concat!("line_height)) as usize / ", "2)");
        let last = concat!("last_", "width");
        assert!(src.contains(walk), "ctrl-d must ask the list, not divide");
        assert!(
            !src.contains(computed),
            "the old viewport÷line_height half-page must be gone"
        );
        assert!(src.contains(last), "wrap + resize invalidates row heights");
    }

    #[gpui::test]
    fn a_view_can_be_created_as_an_entity_in_a_test_app(cx: &mut gpui::TestAppContext) {
        let d = data();
        let row_count = d.rows.len();
        let view = cx.new(|_| DiffView::new(d, Theme::dark()));
        view.read_with(cx, |view, _| {
            assert_eq!(view.rows().len(), row_count);
            assert_eq!(view.cursor, 0);
        });
    }
}
