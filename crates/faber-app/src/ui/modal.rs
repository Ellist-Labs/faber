//! Composable helpers for elevated modal overlays and anchored popovers.
//! Every floating surface (centered dialog, contextual menu) should be built
//! with these helpers so elevation tokens, scrim depth, and dismiss behavior
//! stay consistent across the whole app.

use gpui::{
    AnyElement, BoxShadow, Div, ElementId, Hsla, MouseButton, SharedString, Stateful, div, hsla,
    point, prelude::*, px, rgba,
};

use crate::theme::RuntimeTheme;
use crate::ui::{KeyHint, h_flex};

/// Scrim color sourced from the theme — eliminates inline `hsla(0,0,0,…)` literals.
pub fn scrim_color(t: &RuntimeTheme) -> Hsla {
    hsla(0., 0., 0., t.scrim)
}

/// Glass material (spec §3.2): translucent black surface, `border_focus` hairline,
/// layered drop shadows. Border radius is applied by the caller (per-surface, §3.3).
pub fn glass_surface(t: &RuntimeTheme) -> Div {
    div()
        .bg(t.bg_overlay)
        .border_1()
        .border_color(t.border_focus)
        .shadow(vec![
            BoxShadow {
                color: rgba(0x000000A6).into(),
                blur_radius: px(40.),
                spread_radius: px(0.),
                offset: point(px(0.), px(10.)),
            },
            BoxShadow {
                color: rgba(0x00000080).into(),
                blur_radius: px(16.),
                spread_radius: px(0.),
                offset: point(px(0.), px(4.)),
            },
        ])
}

/// Backdrop with NO scrim tint (spec §5.5 — the palette sits directly over the
/// editor; glass provides the separation). Anchors its child near the top with
/// `pad_top` pixels of offset. Same dismiss wiring contract as `modal_backdrop`.
pub fn modal_backdrop_clear(id: impl Into<ElementId>, pad_top: f32) -> Stateful<Div> {
    div()
        .id(id)
        .absolute()
        .inset_0()
        .occlude()
        .flex()
        .flex_col()
        .items_center()
        .pt(px(pad_top))
}

/// Full-window backdrop that **centers its child both horizontally and vertically**
/// (VSCode style). The caller must:
///   - `.on_mouse_down(MouseButton::Left, dismiss_listener)` for click-outside dismiss
///   - `.child(modal_container(…))` for the modal content
///   - wrap the result in `deferred(…).with_priority(N)`
pub fn modal_backdrop(id: impl Into<ElementId>, t: &RuntimeTheme) -> Stateful<Div> {
    div()
        .id(id)
        .absolute()
        .inset_0()
        .occlude()
        .flex()
        .items_center()
        .justify_center()
        .bg(scrim_color(t))
}

/// Elevated modal container: `bg_elevated` surface + `border` + `shadow_lg` + `rounded_lg` +
/// `overflow_hidden` (clips content to rounded corners). A `stop_propagation` mouse-down
/// handler is pre-wired so clicks inside don't bubble to the backdrop's dismiss handler.
///
/// The caller must add `.key_context`, `.track_focus`, `.on_action`, `.on_key_down`,
/// size constraints (`w`, `max_h`, …), and `.child(…)` for content sections.
pub fn modal_container(id: impl Into<ElementId>, t: &RuntimeTheme) -> Stateful<Div> {
    glass_surface(t)
        .id(id)
        .occlude()
        .relative()
        .rounded(px(t.radius_xl))
        .overflow_hidden()
        .flex()
        .flex_col()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
}

/// Anchored popover container (no backdrop) for context menus and inline dropdowns.
/// Intended for use inside `deferred(anchored().position(pos).snap_to_window().child(…))`.
/// Uses `bg_elevated` (not `bg_overlay`) for visual consistency with centered modals.
pub fn popover_container(id: impl Into<ElementId>, t: &RuntimeTheme) -> Stateful<Div> {
    glass_surface(t)
        .id(id)
        .occlude()
        .rounded(px(t.radius_lg))
        .overflow_hidden()
}

/// Render `text` as accent-highlighted spans where `positions` lists matched char indices.
/// `char_off` is the offset of `text`'s first character in the larger string `positions` indexes into.
/// Unmatched characters render in `base_color`; matched characters render in the theme accent.
pub fn render_matched_text(
    text: &str,
    positions: &[u32],
    char_off: u32,
    base_color: Hsla,
    t: &RuntimeTheme,
) -> AnyElement {
    let plain = || {
        div()
            .text_color(base_color)
            .overflow_hidden()
            .text_ellipsis()
            .child(SharedString::from(text.to_string()))
            .into_any()
    };
    let chars: Vec<char> = text.chars().collect();
    if positions.is_empty() || chars.is_empty() {
        return plain();
    }
    let end = char_off + chars.len() as u32;
    let local: Vec<usize> = positions
        .iter()
        .filter(|&&p| p >= char_off && p < end)
        .map(|&p| (p - char_off) as usize)
        .collect();
    if local.is_empty() {
        return plain();
    }
    let mut spans: Vec<AnyElement> = Vec::new();
    let mut i = 0usize;
    while i < chars.len() {
        let matched = local.binary_search(&i).is_ok();
        let start = i;
        while i < chars.len() && local.binary_search(&i).is_ok() == matched {
            i += 1;
        }
        let seg: String = chars[start..i].iter().collect();
        spans.push(
            div()
                .text_color(if matched { t.accent } else { base_color })
                .child(SharedString::from(seg))
                .into_any(),
        );
    }
    h_flex()
        .min_w(px(0.))
        .overflow_hidden()
        .children(spans)
        .into_any()
}

/// Standard hint row for modal footers.
/// Pass an ordered slice of `(key_badge, label)` pairs — e.g. `&[("↑↓", "Navigate".into())]`.
/// Renders a horizontal bar separated from the content above with a `separator` line.
pub fn modal_footer(t: &RuntimeTheme, hints: &[(&str, String)]) -> AnyElement {
    let hint_els: Vec<AnyElement> = hints
        .iter()
        .map(|(keys, label)| {
            h_flex()
                .gap_1()
                .child(KeyHint::new((*keys).to_owned()))
                .child(
                    div()
                        .font_family(t.ui_family.clone())
                        .text_size(px(t.font_size_caption - 1.))
                        .text_color(t.text_muted)
                        .child(label.clone()),
                )
                .into_any()
        })
        .collect();

    h_flex()
        .px_4()
        .py(px(6.))
        .gap_3()
        .border_t_1()
        .border_color(t.separator)
        .children(hint_els)
        .into_any()
}
