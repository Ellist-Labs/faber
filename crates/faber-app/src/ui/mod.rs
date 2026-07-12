pub mod button;
pub mod divider;
pub mod icon;
pub mod input;
pub mod key_hint;
pub mod label;
pub mod modal;
pub mod scrollbar;
// surface.rs absorbed into modal.rs (Wave 2 adoption)

pub use button::Button;
pub use divider::Divider;
pub use icon::{Icon, IconName};
pub use key_hint::KeyHint;
pub use label::Label;
pub use modal::{
    glass_surface, modal_backdrop, modal_backdrop_clear, modal_container, modal_footer,
    popover_container, render_matched_text,
};
pub use scrollbar::{ScrollbarDrag, render_scrollbar};

use gpui::{Div, Styled as _, div};

/// Horizontal flex container — shorthand matching Zed conventions.
pub fn h_flex() -> Div {
    div().flex().flex_row().items_center()
}

/// Vertical flex container.
pub fn v_flex() -> Div {
    div().flex().flex_col()
}
