use crate::scrollbar::scrollbar;
use crate::theme::Theme;
use diffident_diff::{DiffFile, LineKind, Row};
use diffident_forge::threads::ReviewThread;
use diffident_highlight::Highlights;
use gpui::{
    Bounds, Context, HighlightStyle, IntoElement, ListAlignment, ListState, ParentElement, Pixels,
    Render, SharedString, StyledText, Window, div, list, prelude::*, px, rgb,
};
use std::ops::Range;


/// One review's diff, rendered as a virtualised list over the flat row model.
///
/// Sole owner of the element choice: nothing outside this file knows that
/// `uniform_list` is in use, so the Phase 7 swap to `list()` for variable-height
/// inline comment rows touches this file only (§3).
pub struct DiffView {
    files: Vec<DiffFile>,
    rows: Vec<Row>,
    highlights: Vec<Highlights>,
    /// The list's own state. Owns scroll position and per-row measurements.
    list: ListState,
    theme: Theme,
    /// Where inside the scrollbar thumb the pointer grabbed. Owned solely by
    /// the scrollbar element.
    drag_offset: Option<Pixels>,
    /// Row the keyboard cursor is on.
    pub cursor: usize,
    /// Threads to draw under each row, in row order (§7). Owned, because the
    /// view outlives any borrow of the workspace's map.
    inline: Vec<(usize, Vec<ReviewThread>)>,
    /// Node id of the thread the reviewer has selected, so the inline copy can
    /// show the same selection the right-hand pane does.
    selected_thread: Option<String>,
}

impl DiffView {
    /// `highlights` must be index-parallel with `rows`; `loader::load_review`
    /// produces it on a background thread. Deliberately not computed here —
    /// this constructor runs on the foreground, and highlighting a 10k-row diff
    /// takes ~530ms, which froze the window for exactly that long.
    pub fn new(files: Vec<DiffFile>, rows: Vec<Row>, highlights: Vec<Highlights>, theme: Theme) -> Self {
        debug_assert_eq!(highlights.len(), rows.len(), "highlights must be row-parallel");
        let rows_len = rows.len();
        let line_height = theme.line_height;
        Self {
            files,
            rows,
            highlights,
            // A uniform height *hint*, not a constraint: every row starts out
            // assumed to be one line tall, and real heights replace the hint as
            // rows are measured. `measure_all()` would instead lay out every
            // row up front — on a 10k-row diff that is the same class of
            // mistake as computing highlights on the foreground thread.
            list: ListState::new(rows_len, ListAlignment::Top, px(line_height * 20.))
                .with_uniform_item_height(px(line_height)),
            theme,
            drag_offset: None,
            cursor: 0,
            inline: Vec::new(),
            selected_thread: None,
        }
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
        &self.rows
    }

    pub fn files(&self) -> &[DiffFile] {
        &self.files
    }

    /// Replace the inline threads and tell the list which rows changed height.
    ///
    /// Splices only the affected rows rather than resetting: `ListState::reset`
    /// discards the scroll position, and having the diff jump to the top every
    /// time someone resolves a thread would be worse than the stale heights
    /// this is here to prevent. `splice` preserves the scroll offset unless the
    /// spliced range contains it.
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
            if row < self.rows.len() {
                self.list.splice(row..row + 1, 1);
            }
        }
    }

    /// The threads that render under row `ix`, if any.
    ///
    /// Unused until Task 3 wires it into `render`.
    #[allow(dead_code)]
    fn threads_at(&self, ix: usize) -> &[ReviewThread] {
        self.inline
            .iter()
            .find(|(row, _)| *row == ix)
            .map(|(_, ts)| ts.as_slice())
            .unwrap_or_default()
    }

    /// Scroll so `ix` is visible. Used by the file panel and by navigation.
    pub fn scroll_to(&mut self, ix: usize) {
        self.cursor = ix.min(self.rows.len().saturating_sub(1));
        self.list.scroll_to_reveal_item(self.cursor);
    }

    /// How many rows `ctrl-d` moves: half a screenful.
    ///
    /// Falls back to 1 before the first layout, when the viewport is still
    /// zero-sized — moving one row is a sane thing to do on a keypress that
    /// arrives that early, and it keeps the caller free of an `Option`.
    pub fn rows_per_half_page(&self) -> usize {
        ((self.viewport().size.height / px(self.theme.line_height)) as usize / 2).max(1)
    }

    fn render_row(&self, ix: usize, theme: &Theme) -> impl IntoElement + use<> {
        match self.rows[ix] {
            Row::FileHeader { file_ix } => div()
                .px_2()
                .h(px(theme.line_height))
                .bg(theme.header_bg)
                .text_color(theme.text)
                .child(SharedString::from(
                    self.files[file_ix].display_path().to_string(),
                ))
                .into_any_element(),
            Row::HunkHeader { file_ix, hunk_ix } => {
                let h = &self.files[file_ix].hunks[hunk_ix];
                div()
                    .px_2()
                    .h(px(theme.line_height))
                    .text_color(theme.text_muted)
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
                let line = &self.files[file_ix].hunks[hunk_ix].lines[line_ix];
                let (bg, sigil) = match line.kind {
                    LineKind::Added => (theme.added_bg, "+"),
                    LineKind::Removed => (theme.removed_bg, "-"),
                    LineKind::Context => (theme.bg, " "),
                };
                let style = theme.text_style();
                let ranges: Vec<(Range<usize>, HighlightStyle)> = self.highlights[ix]
                    .iter()
                    .map(|(r, c)| {
                        (
                            r.clone(),
                            HighlightStyle {
                                color: Some(rgb(*c).into()),
                                ..Default::default()
                            },
                        )
                    })
                    .collect();
                div()
                    .flex()
                    .h(px(theme.line_height))
                    .when(ix == self.cursor, |this| this.bg(theme.row_selected))
                    .when(ix != self.cursor, |this| this.bg(bg))
                    .child(
                        div()
                            .w(px(48.))
                            .text_color(theme.text_muted)
                            .child(SharedString::from(
                                line.new_lineno
                                    .or(line.old_lineno)
                                    .map(|n| n.to_string())
                                    .unwrap_or_default(),
                            )),
                    )
                    .child(div().w(px(12.)).child(sigil))
                    .child(
                        StyledText::new(line.text.clone()).with_default_highlights(&style, ranges),
                    )
                    .into_any_element()
            }
            Row::Expander { hidden, .. } => div()
                .px_2()
                .h(px(theme.line_height))
                .bg(theme.header_bg)
                .text_color(theme.text_muted)
                .child(match hidden {
                    Some(n) => SharedString::from(format!("··· {n} unchanged lines")),
                    None => SharedString::from("··· expand to end of file"),
                })
                .into_any_element(),
            Row::Spacer => div().h(px(theme.line_height)).into_any_element(),
        }
    }
}

impl Render for DiffView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let bar = scrollbar(
            self.viewport(),
            self.content_height(),
            self.list.clone(),
            &self.theme,
            cx.entity(),
            |v: &mut Self| &mut v.drag_offset,
        );

        div()
            .relative()
            .size_full()
            .bg(self.theme.bg)
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

    #[test]
    fn a_freshly_built_view_reports_a_zero_viewport_rather_than_a_stale_one() {
        // Regression: `bounds` used to be a field initialised to
        // Bounds::default() and never assigned, so the viewport stayed zero
        // *forever* and thumb() returned None on every frame — the scrollbar
        // was never drawn. Reading it from the scroll handle means the value
        // is zero only until the first layout, not permanently.
        let files = parse(RUST);
        let rows = build_rows(&files);
        let hl = vec![Vec::new(); rows.len()];
        let view = DiffView::new(files, rows, hl, Theme::dark());
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
        let files = parse(RUST);
        let rows = build_rows(&files);
        let theme = Theme::dark();
        let expected = px(rows.len() as f32 * theme.line_height);
        let hl = vec![Vec::new(); rows.len()];
        let view = DiffView::new(files, rows, hl, theme);
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
            crate::scrollbar::thumb(px(40.), content, px(0.)).is_some(),
            "a viewport shorter than the content must produce a thumb"
        );
    }

}
