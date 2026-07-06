pub mod button;
pub mod divider;
pub mod input;
pub mod key_hint;
pub mod label;
pub mod surface;

pub use button::Button;
pub use divider::Divider;
pub use input::Input;
pub use key_hint::KeyHint;
pub use label::Label;
pub use surface::Surface;

use gpui::{Div, Styled as _, div};

/// Horizontal flex container — shorthand matching Zed conventions.
pub fn h_flex() -> Div {
    div().flex().flex_row().items_center()
}

/// Vertical flex container.
pub fn v_flex() -> Div {
    div().flex().flex_col()
}
