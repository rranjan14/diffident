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

/// A 4px drag handle between the sidebar and the diff.
///
/// Hand-rolled: GPUI has no splitter primitive. `width` is written back through
/// the closure rather than returned, because the drag continues across frames.
pub fn divider<V: 'static>(
    theme: &Theme,
    entity: gpui::Entity<V>,
    set_width: fn(&mut V, f32),
) -> impl IntoElement {
    div()
        .id("sidebar-divider")
        .w(px(4.))
        .h_full()
        .cursor_col_resize()
        .bg(theme.border_subtle)
        .hover(|s| s.bg(theme.accent))
        .on_drag((), |_, _, _, cx| cx.new(|_| gpui::Empty))
        .on_drag_move(move |ev: &gpui::DragMoveEvent<()>, _, cx| {
            let x: f32 = ev.event.position.x.into();
            entity.update(cx, |v, cx| {
                set_width(v, x.clamp(200., 480.));
                cx.notify();
            });
        })
}

#[cfg(test)]
mod tests {
    /// `on_drag_move` only fires while `cx.active_drag` holds `T`. Split the
    /// needle so this test does not match itself.
    #[test]
    fn divider_starts_a_unit_drag_so_on_drag_move_can_fire() {
        let src = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/sidebar.rs"));
        let start = concat!(".on", "_drag(");
        assert!(
            src.contains(start),
            "divider must call on_drag so on_drag_move can fire"
        );
    }
}
