use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{
    App, Div, ElementId, InteractiveElement as _, Role, Stateful, StatefulInteractiveElement as _,
    Styled as _, div, px,
};

pub(in crate::gpui_app) fn horizontal(id: impl Into<ElementId>, cx: &App) -> Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .w_full()
        .h(px(1.))
        .bg(cx.theme().border)
        .role(Role::Splitter)
        .aria_orientation(gpui_kit::accesskit::Orientation::Horizontal)
}

pub(in crate::gpui_app) fn vertical(id: impl Into<ElementId>, cx: &App) -> Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .w(px(1.))
        .self_stretch()
        .bg(cx.theme().border)
        .role(Role::Splitter)
        .aria_orientation(gpui_kit::accesskit::Orientation::Vertical)
}
