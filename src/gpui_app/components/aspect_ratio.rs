use gpui_kit::{Div, Styled as _, div};

pub(in crate::gpui_app) fn aspect_ratio(ratio: f32) -> Div {
    assert!(
        ratio.is_finite() && ratio > 0.,
        "aspect ratio must be finite and positive"
    );
    div().relative().aspect_ratio(ratio)
}
