use crate::theme::Theme;
use gpui::{
    Bounds, Entity, ListState, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    Stateful, canvas, div, point, prelude::*, px,
};

/// Below this the thumb is too small to grab with a mouse.
pub const MIN_THUMB: Pixels = px(24.);

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Thumb {
    /// Offset from the top of the track.
    pub top: Pixels,
    pub height: Pixels,
}

/// Where the scrollbar thumb sits, or `None` when no scrollbar is warranted.
///
/// `scroll_top` is a positive distance already scrolled. GPUI's own scroll
/// offset is *negative* — convert before calling, or the thumb runs backwards.
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
        // puts the thumb's *bottom* at the track's bottom.
        top: (viewport - height) * progress,
        height,
    })
}

/// A dragging scrollbar overlaid on the right edge of `track`.
///
/// `drag_offset` must be a field on `V` that this element owns exclusively: it
/// holds where inside the thumb the pointer grabbed, so dragging does not snap
/// the thumb's top to the cursor.
pub fn scrollbar<V: 'static>(
    track: Bounds<Pixels>,
    content_height: Pixels,
    scroll: ListState,
    theme: &Theme,
    entity: Entity<V>,
    drag_offset: fn(&mut V) -> &mut Option<Pixels>,
) -> Stateful<gpui::Div> {
    let viewport = track.size.height;
    let Some(t) = thumb(viewport, content_height, -scroll.scroll_px_offset_for_scrollbar().y)
    else {
        return div().id("scrollbar");
    };

    div()
        .id("scrollbar")
        .absolute()
        .top(t.top)
        .right_0()
        .w(px(10.))
        .h(t.height)
        .bg(theme.border_strong)
        .hover(|this| this.bg(theme.text_tertiary))
        .rounded_full()
        .child(
            canvas(|_, _, _| (), move |thumb_bounds, _, window, _| {
                let (scroll_down, scroll_up) = (scroll.clone(), scroll.clone());
                window.on_mouse_event({
                    let entity = entity.clone();
                    move |ev: &MouseDownEvent, _, _, cx| {
                        if !thumb_bounds.contains(&ev.position) {
                            return;
                        }
                        scroll_down.scrollbar_drag_started();
                        entity.update(cx, |v, _| {
                            *drag_offset(v) = Some(ev.position.y - thumb_bounds.origin.y);
                        });
                    }
                });
                window.on_mouse_event({
                    let entity = entity.clone();
                    move |_: &MouseUpEvent, _, _, cx| {
                        scroll_up.scrollbar_drag_ended();
                        entity.update(cx, |v, _| *drag_offset(v) = None);
                    }
                });
                window.on_mouse_event(move |ev: &MouseMoveEvent, _, _, cx| {
                    if !ev.dragging() {
                        return;
                    }
                    let Some(grab) = entity.update(cx, |v, _| *drag_offset(v)) else {
                        return;
                    };
                    let progress = ((ev.position.y - track.origin.y - grab)
                        / (viewport - t.height))
                        .clamp(0., 1.);
                    scroll.set_offset_from_scrollbar(point(
                        px(0.),
                        -(scroll.max_offset_for_scrollbar().y * progress),
                    ));
                    cx.notify(entity.entity_id());
                });
            })
            .size_full(),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

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
