use gpui_kit::base::StyledExt as _;
use gpui_kit::{Div, Styled as _, div, px};

pub(in crate::gpui_app) fn badge() -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .gap(px(4.))
        .h(px(20.))
        .px(px(8.))
        .py(px(2.))
        .rounded(px(26.))
        .border_1()
        .overflow_hidden()
        .text_size(px(12.))
        .line_height(px(16.))
        .font_medium()
        .whitespace_nowrap()
}
