use gpui::{MouseButton, MouseDownEvent, MouseMoveEvent, Point, ScrollHandle, div, prelude::*, px};

use crate::theme::RuntimeTheme;

/// Drag state stored in the host view while a scrollbar thumb is being dragged.
#[derive(Clone, Copy)]
pub struct ScrollbarDrag {
    /// Window-space y of the mouse when drag started.
    pub start_mouse_y: f32,
    /// scroll handle offset.y when drag started (always <= 0).
    pub start_offset_y: f32,
}

/// Render a thin vertical scrollbar track+thumb to the right of a scrollable area.
///
/// Pass the `ScrollHandle` (or the `base_handle` of a `UniformListScrollHandle`).
/// Returns an empty zero-width div if the content fits (no scroll needed) or
/// `show` is false.
///
/// The caller is responsible for:
/// - Adding `scrollbar_drag: Option<ScrollbarDrag>` to their view.
/// - Registering a window-level `on_mouse_move` + `on_mouse_up` when dragging
///   (use `on_scrollbar_drag` / `on_scrollbar_release` helpers below).
pub fn render_scrollbar(
    track_id: impl Into<gpui::ElementId> + Clone,
    thumb_id: impl Into<gpui::ElementId>,
    handle: &ScrollHandle,
    show: bool,
    drag: bool,
    on_thumb_down: impl Fn(&MouseDownEvent, &mut gpui::Window, &mut gpui::App) + 'static,
    t: &RuntimeTheme,
    // Fraction [0,1] for where to draw a thin caret-position indicator on the track.
    position_frac: Option<f32>,
) -> gpui::AnyElement {
    if !show {
        return div().w(px(0.)).into_any_element();
    }

    let max_offset = handle.max_offset();
    let max_scroll = f32::from(max_offset.height);

    if max_scroll <= 0.5 {
        return div().w(px(10.)).flex_shrink_0().into_any_element();
    }

    let offset = handle.offset();
    let bounds = handle.bounds();
    let viewport_h = f32::from(bounds.size.height).max(1.0);
    let content_h = viewport_h + max_scroll;

    let thumb_h = (viewport_h * viewport_h / content_h).max(20.0).min(viewport_h);
    let scroll_frac = f32::from(-offset.y) / max_scroll;
    let available = (viewport_h - thumb_h).max(0.0);
    let thumb_top = (scroll_frac * available).clamp(0.0, available);

    let track_bg = t.bg_sunken;
    let thumb_color = if drag { t.text_muted } else { t.text_subtle };
    let thumb_hover = t.text_muted;
    let marker_color = t.gutter_active;

    div()
        .id(track_id)
        .w(px(10.))
        .flex_shrink_0()
        .h_full()
        .bg(track_bg)
        .relative()
        .flex()
        .flex_col()
        .child(div().h(px(thumb_top)).flex_shrink_0())
        .child(
            div()
                .id(thumb_id)
                .h(px(thumb_h))
                .flex_shrink_0()
                .mx(px(1.))
                .rounded(px(3.))
                .bg(thumb_color)
                .hover(move |s| s.bg(thumb_hover))
                .cursor_pointer()
                .on_mouse_down(MouseButton::Left, move |ev, window, cx| {
                    on_thumb_down(ev, window, cx);
                }),
        )
        .child(div().flex_1())
        .when(position_frac.is_some(), |el| {
            let frac = position_frac.unwrap_or(0.0);
            let pos_y = (frac * viewport_h).clamp(0.0, (viewport_h - 2.0).max(0.0));
            el.child(
                div()
                    .absolute()
                    .top(px(pos_y))
                    .left(px(0.))
                    .w_full()
                    .h(px(2.))
                    .bg(marker_color),
            )
        })
        .into_any_element()
}

/// Compute the new scroll y offset while dragging.
///
/// `drag` — the drag state recorded on mouse-down.
/// `current_mouse_y` — current window-space y of the mouse.
/// `handle` — the scroll handle to update.
pub fn apply_scrollbar_drag(drag: &ScrollbarDrag, current_mouse_y: f32, handle: &ScrollHandle) {
    let max_offset = handle.max_offset();
    let max_scroll = f32::from(max_offset.height);
    if max_scroll <= 0.0 {
        return;
    }

    let bounds = handle.bounds();
    let viewport_h = f32::from(bounds.size.height).max(1.0);
    let content_h = viewport_h + max_scroll;
    let thumb_h = (viewport_h * viewport_h / content_h).max(20.0).min(viewport_h);
    let available = (viewport_h - thumb_h).max(1.0);

    let delta_track = current_mouse_y - drag.start_mouse_y;
    let delta_content = delta_track * max_scroll / available;
    let new_y = (drag.start_offset_y - delta_content).clamp(-max_scroll, 0.0);

    let current = handle.offset();
    handle.set_offset(Point { x: current.x, y: px(new_y) });
}

/// Convenience: build a `ScrollbarDrag` from a `MouseDownEvent` + handle.
pub fn start_drag(ev: &MouseDownEvent, handle: &ScrollHandle) -> ScrollbarDrag {
    ScrollbarDrag {
        start_mouse_y: f32::from(ev.position.y),
        start_offset_y: f32::from(handle.offset().y),
    }
}

/// Apply a mouse-move event to an active drag.
pub fn update_drag(drag: &ScrollbarDrag, ev: &MouseMoveEvent, handle: &ScrollHandle) {
    apply_scrollbar_drag(drag, f32::from(ev.position.y), handle);
}
