//! The scrollbar track as a compressed picture of the whole diff (§8A).

use crate::loader::ReviewData;
use crate::theme::Theme;
use diffident_diff::{DiffFile, LineKind, Row};
use gpui::{Bounds, Entity, IntoElement, Pixels, canvas, div, prelude::*, px};
use std::sync::Arc;

/// Below this the viewport marker is too small to see.
pub const MIN_THUMB: Pixels = px(24.);

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Thumb {
    /// Offset from the top of the track.
    pub top: Pixels,
    pub height: Pixels,
}

/// Where the viewport marker sits, or `None` when no marker is warranted.
///
/// `scroll_top` is a positive distance already scrolled. GPUI's own scroll
/// offset is *negative* — convert before calling, or the marker runs backwards.
///
/// Cannot fail: a viewport taller than its content, or a zero-height viewport
/// mid-layout, both yield `None` rather than a division by zero.
pub fn thumb(viewport: Pixels, content: Pixels, scroll_top: Pixels) -> Option<Thumb> {
    if content <= viewport || viewport <= px(0.) {
        return None;
    }
    let height = (viewport * (viewport / content)).max(MIN_THUMB);
    let max_scroll = content - viewport;
    let progress = (scroll_top / max_scroll).clamp(0., 1.);
    Some(Thumb {
        // Against `viewport - height`, not `viewport`, so a fully scrolled list
        // puts the marker's *bottom* at the track's bottom.
        top: (viewport - height) * progress,
        height,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkKind {
    Added,
    Removed,
    Thread,
}

/// One band of the track. `y` is a row index into the track, not a pixel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mark {
    pub y: usize,
    pub kind: MarkKind,
}

/// Compress `rows` into at most `track_px` bands.
///
/// Bucketed by track pixel rather than by row, so a 10,000-row diff costs the
/// same as a 100-row one. A band takes the strongest thing in it: a thread
/// outranks a change, because "there is a conversation here" is the rarer and
/// more actionable signal.
///
/// Takes `files` because `Row::Line` carries indices, not a kind — `row.line`
/// (added by Phase 8's refactor) is what resolves one to the other.
pub fn marks(
    files: &[DiffFile],
    rows: &[Row],
    thread_rows: &[usize],
    track_px: usize,
) -> Vec<Mark> {
    if rows.is_empty() || track_px == 0 {
        return Vec::new();
    }
    let band = |ix: usize| ix * track_px / rows.len();
    let mut seen: Vec<Option<MarkKind>> = vec![None; track_px];

    for (ix, row) in rows.iter().enumerate() {
        let Some(line) = row.line(files) else { continue };
        let kind = match line.kind {
            LineKind::Added => MarkKind::Added,
            LineKind::Removed => MarkKind::Removed,
            // Context lines are most of a diff and mark nothing — a track that
            // is uniformly lit says as little as one that is dark.
            LineKind::Context => continue,
        };
        seen[band(ix)].get_or_insert(kind);
    }
    // Threads last, so they overwrite a change in the same band.
    for &ix in thread_rows {
        if ix < rows.len() {
            seen[band(ix)] = Some(MarkKind::Thread);
        }
    }

    seen.into_iter()
        .enumerate()
        .filter_map(|(y, kind)| kind.map(|kind| Mark { y, kind }))
        .collect()
}

/// The track: density behind, viewport window in front.
///
/// Marks are computed from `bounds` during paint, not from the last layout's
/// viewport — that is why this paints on the first frame, which the old
/// scrollbar did not.
pub fn track<V: 'static>(
    data: Arc<ReviewData>,
    thread_rows: Vec<usize>,
    viewport_frac: (f32, f32),
    theme: &Theme,
    entity: Entity<V>,
    jump: fn(&mut V, f32),
) -> impl IntoElement {
    let (added, removed, accent, thumb_color) =
        (theme.added_fg, theme.removed_fg, theme.accent, theme.border_strong);
    div()
        .id("density")
        .absolute()
        .top_0()
        .right_0()
        .w(px(12.))
        .h_full()
        .child(
            canvas(|_, _, _| (), move |bounds, _, window, _| {
                let h: f32 = bounds.size.height.into();
                let track_px = h.max(0.) as usize;
                let marks = marks(&data.files, &data.rows, &thread_rows, track_px);
                // `y` is a band index 0..track_px. Scale by the painted height
                // so empty trailing bands still take space — dividing by
                // marks.len() would squash a sparse track.
                let n = track_px.max(1) as f32;
                for m in &marks {
                    let colour = match m.kind {
                        MarkKind::Added => added,
                        MarkKind::Removed => removed,
                        MarkKind::Thread => accent,
                    };
                    let y = bounds.origin.y + px(m.y as f32 / n * h);
                    window.paint_quad(gpui::fill(
                        Bounds::new(
                            gpui::point(bounds.origin.x + px(2.), y),
                            gpui::size(px(8.), px(2.)),
                        ),
                        colour,
                    ));
                }
                let (top, frac) = viewport_frac;
                window.paint_quad(gpui::fill(
                    Bounds::new(
                        gpui::point(bounds.origin.x, bounds.origin.y + px(top * h)),
                        gpui::size(px(12.), px((frac * h).max(24.))),
                    ),
                    thumb_color,
                ));
            })
            .size_full(),
        )
        .on_click(move |ev, _, cx| {
            entity.update(cx, |v, cx| {
                jump(v, ev.position().y.into());
                cx.notify();
            });
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use diffident_diff::{parser::parse, rows::build_rows};

    const DIFF: &str = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,3 +1,3 @@\n ctx\n-old\n+new\n";

    fn fixture() -> (Vec<diffident_diff::DiffFile>, Vec<Row>) {
        let files = parse(DIFF);
        let rows = build_rows(&files);
        (files, rows)
    }

    #[test]
    fn marks_are_bounded_by_the_track_not_the_diff() {
        // One mark per row is 10,000 rects on a large diff. Cost must scale
        // with the window, not the file.
        let (files, rows) = fixture();
        // `repeat_n`, not `repeat().take()` — clippy rejects the latter at
        // `-D warnings`.
        let big: Vec<Row> = std::iter::repeat_n(rows.clone(), 2_000).flatten().collect();
        assert!(marks(&files, &big, &[], 400).len() <= 400);
    }

    #[test]
    fn added_and_removed_lines_are_distinguishable() {
        let (files, rows) = fixture();
        let m = marks(&files, &rows, &[], 100);
        assert!(m.iter().any(|m| m.kind == MarkKind::Added));
        assert!(m.iter().any(|m| m.kind == MarkKind::Removed));
    }

    #[test]
    fn a_thread_outranks_a_change_in_the_same_band() {
        // "There is a conversation here" is rarer and more actionable than
        // "something changed here", and on a compressed track only one of them
        // fits.
        let (files, rows) = fixture();
        let added = rows
            .iter()
            .position(|r| r.line(&files).is_some_and(|l| l.kind == LineKind::Added))
            .expect("the fixture has an added line");
        let m = marks(&files, &rows, &[added], rows.len());
        assert!(m.iter().any(|m| m.kind == MarkKind::Thread));
        assert!(
            !m.iter().any(|m| m.y == added && m.kind == MarkKind::Added),
            "the thread took the band"
        );
    }

    #[test]
    fn context_lines_mark_nothing() {
        // They are most of a diff; a uniformly lit track says as little as a
        // dark one.
        let (files, rows) = fixture();
        assert_eq!(marks(&files, &rows, &[], 100).len(), 2, "one added, one removed");
    }

    #[test]
    fn an_empty_diff_produces_no_marks_rather_than_dividing_by_zero() {
        let (files, rows) = fixture();
        assert!(marks(&files, &[], &[], 400).is_empty());
        assert!(marks(&files, &rows, &[], 0).is_empty());
    }

    #[test]
    fn no_thumb_when_the_content_fits() {
        assert_eq!(thumb(px(500.), px(400.), px(0.)), None);
    }

    #[test]
    fn thumb_is_proportional_to_the_visible_fraction() {
        let t = thumb(px(500.), px(1000.), px(0.)).unwrap();
        assert_eq!(t.height, px(250.));
        assert_eq!(t.top, px(0.));
    }

    #[test]
    fn thumb_sits_at_the_bottom_when_scrolled_to_the_end() {
        let t = thumb(px(500.), px(1000.), px(500.)).unwrap();
        assert_eq!(t.top + t.height, px(500.));
    }

    #[test]
    fn thumb_never_shrinks_below_a_grabbable_height() {
        // A 100k-line diff would otherwise compute a sub-pixel thumb.
        let t = thumb(px(500.), px(100_000.), px(0.)).unwrap();
        assert_eq!(t.height, MIN_THUMB);
    }

    #[test]
    fn a_zero_height_viewport_has_no_thumb_rather_than_dividing_by_zero() {
        assert_eq!(thumb(px(0.), px(1000.), px(0.)), None);
    }
}
