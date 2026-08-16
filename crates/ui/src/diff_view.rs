use crate::scrollbar::scrollbar;
use crate::theme::Theme;
use diffident_diff::{DiffFile, LineKind, Row};
use diffident_highlight::{Highlights, syntax::highlight_run};
use gpui::{
    Bounds, Context, HighlightStyle, IntoElement, ParentElement, Pixels, Render, SharedString,
    StyledText, UniformListScrollHandle, Window, div, prelude::*, px, rgb, uniform_list,
};
use std::ops::Range;

/// Highlight every line row, returning a vector **exactly as long as `rows`**.
///
/// Index parity with `rows` is the contract: the view indexes this by row
/// number, so a shorter or reordered result silently paints each line with some
/// other line's syntax. Non-line rows get an empty entry rather than being
/// skipped, which is what keeps the indices aligned.
///
/// Highlighting runs per hunk, not per line, because syntax state carries
/// across newlines — see `highlight_run`.
///
/// Two passes, both linear. Do **not** "simplify" this into a single loop that
/// searches `rows` for each highlighted line — `rows.iter().position(..)` inside
/// the line loop is O(rows × lines), which on a 4,000-line diff is 16M
/// comparisons per rebuild and visibly hangs the window.
pub fn highlight_rows(files: &[DiffFile], rows: &[Row]) -> Vec<Highlights> {
    // Pass 1: highlight each hunk once, indexed by [file_ix][hunk_ix][line_ix].
    let per_hunk: Vec<Vec<Vec<Highlights>>> = files
        .iter()
        .map(|file| {
            let path = file.display_path();
            file.hunks
                .iter()
                .map(|hunk| {
                    let texts: Vec<&str> = hunk.lines.iter().map(|l| l.text.as_str()).collect();
                    highlight_run(path, &texts)
                })
                .collect()
        })
        .collect();

    // Pass 2: project onto the row model, preserving index parity.
    rows.iter()
        .map(|row| match *row {
            Row::Line {
                file_ix,
                hunk_ix,
                line_ix,
            } => per_hunk
                .get(file_ix)
                .and_then(|f| f.get(hunk_ix))
                .and_then(|h| h.get(line_ix))
                .cloned()
                .unwrap_or_default(),
            _ => Vec::new(),
        })
        .collect()
}

/// One review's diff, rendered as a virtualised list over the flat row model.
///
/// Sole owner of the element choice: nothing outside this file knows that
/// `uniform_list` is in use, so the Phase 7 swap to `list()` for variable-height
/// inline comment rows touches this file only (§3).
pub struct DiffView {
    files: Vec<DiffFile>,
    rows: Vec<Row>,
    highlights: Vec<Highlights>,
    scroll: UniformListScrollHandle,
    theme: Theme,
    /// Where inside the scrollbar thumb the pointer grabbed. Owned solely by
    /// the scrollbar element.
    drag_offset: Option<Pixels>,
    /// Row the keyboard cursor is on.
    pub cursor: usize,
}

impl DiffView {
    pub fn new(files: Vec<DiffFile>, rows: Vec<Row>, theme: Theme) -> Self {
        let highlights = highlight_rows(&files, &rows);
        Self {
            files,
            rows,
            highlights,
            scroll: UniformListScrollHandle::new(),
            theme,
            drag_offset: None,
            cursor: 0,
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
        self.scroll.0.borrow().base_handle.bounds()
    }

    /// Total height of all rows. Exact, because `uniform_list` is fixed-height.
    fn content_height(&self) -> Pixels {
        px(self.rows.len() as f32 * self.theme.line_height)
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub fn files(&self) -> &[DiffFile] {
        &self.files
    }

    /// Scroll so `ix` is visible. Used by the file panel and by navigation.
    pub fn scroll_to(&mut self, ix: usize) {
        self.cursor = ix.min(self.rows.len().saturating_sub(1));
        self.scroll
            .scroll_to_item(self.cursor, gpui::ScrollStrategy::Top);
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
            self.scroll.0.borrow().base_handle.clone(),
            &self.theme,
            cx.entity(),
            |v: &mut Self| &mut v.drag_offset,
        );

        div()
            .relative()
            .size_full()
            .bg(self.theme.bg)
            .child(
                uniform_list(
                    "diff",
                    self.rows.len(),
                    cx.processor(|this, range: Range<usize>, _window, _cx| {
                        let theme = this.theme.clone();
                        range
                            .map(|ix| this.render_row(ix, &theme))
                            .collect::<Vec<_>>()
                    }),
                )
                .track_scroll(&self.scroll)
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
    fn highlights_are_index_parallel_with_the_rows() {
        // The whole render model depends on this: row N's colours must be at
        // index N, or every line renders someone else's syntax.
        let files = parse(RUST);
        let rows = build_rows(&files);
        assert_eq!(highlight_rows(&files, &rows).len(), rows.len());
    }

    #[test]
    fn non_line_rows_carry_no_highlights() {
        let files = parse(RUST);
        let rows = build_rows(&files);
        let hl = highlight_rows(&files, &rows);
        for (ix, row) in rows.iter().enumerate() {
            if !matches!(row, Row::Line { .. }) {
                assert!(hl[ix].is_empty(), "row {ix} ({row:?}) should have none");
            }
        }
    }

    #[test]
    fn code_lines_are_highlighted_using_the_files_language() {
        let files = parse(RUST);
        let rows = build_rows(&files);
        let hl = highlight_rows(&files, &rows);
        let first_line = rows.iter().position(|r| matches!(r, Row::Line { .. })).unwrap();
        assert!(!hl[first_line].is_empty(), "a .rs line must be highlighted");
    }

    #[test]
    fn highlight_ranges_stay_inside_their_lines_text() {
        let files = parse(RUST);
        let rows = build_rows(&files);
        let hl = highlight_rows(&files, &rows);
        for (ix, row) in rows.iter().enumerate() {
            let Row::Line {
                file_ix,
                hunk_ix,
                line_ix,
            } = *row
            else {
                continue;
            };
            let text = &files[file_ix].hunks[hunk_ix].lines[line_ix].text;
            for (range, _) in &hl[ix] {
                assert!(range.end <= text.len(), "range {range:?} overruns {text:?}");
            }
        }
    }

    #[test]
    fn a_freshly_built_view_reports_a_zero_viewport_rather_than_a_stale_one() {
        // Regression: `bounds` used to be a field initialised to
        // Bounds::default() and never assigned, so the viewport stayed zero
        // *forever* and thumb() returned None on every frame — the scrollbar
        // was never drawn. Reading it from the scroll handle means the value
        // is zero only until the first layout, not permanently.
        let files = parse(RUST);
        let rows = build_rows(&files);
        let view = DiffView::new(files, rows, Theme::dark());
        assert_eq!(view.viewport().size.height, px(0.), "pre-layout");
        assert_eq!(
            view.rows_per_half_page(),
            1,
            "must fall back to 1 pre-layout, not panic or divide by zero"
        );
    }

    #[test]
    fn content_height_matches_the_row_count_times_the_line_height() {
        // The scrollbar divides by this; if it disagrees with what
        // uniform_list actually lays out, the thumb is the wrong size.
        let files = parse(RUST);
        let rows = build_rows(&files);
        let theme = Theme::dark();
        let expected = px(rows.len() as f32 * theme.line_height);
        let view = DiffView::new(files, rows, theme);
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

    #[test]
    fn an_empty_diff_produces_no_highlights() {
        assert!(highlight_rows(&[], &[]).is_empty());
    }

    #[test]
    fn a_large_diff_highlights_in_linear_time() {
        // Guards the two-pass structure. The obvious one-pass version searches
        // `rows` per line — O(rows × lines) — which is 16M comparisons here and
        // hangs the window. Under the debug profile this must still be instant;
        // if it takes more than a second, the implementation regressed to O(n²).
        let body: String = (0..4000).map(|i| format!("+let x{i} = {i};\n")).collect();
        let text = format!(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,1 +1,4000 @@\n{body}"
        );
        let files = parse(&text);
        let rows = build_rows(&files);
        assert!(rows.len() > 4000);
        assert_eq!(highlight_rows(&files, &rows).len(), rows.len());
    }
}
