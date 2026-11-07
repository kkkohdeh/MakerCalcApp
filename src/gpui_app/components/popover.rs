use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{App, BoxShadow, Div, Styled as _, div, point, px};

/// Callers may override default width and padding.
pub(in crate::gpui_app) fn content(alpha: f32, cx: &App) -> Div {
    div()
        .relative()
        .flex()
        .flex_col()
        .w(px(288.))
        .gap(px(10.))
        .p(px(10.))
        .rounded(px(10.))
        .bg(cx.theme().popover)
        .text_color(cx.theme().popover_foreground)
        .text_size(px(14.))
        .line_height(px(20.))
        .opacity(alpha)
        .shadow(vec![
            BoxShadow {
                inset: false,
                color: cx.theme().border.opacity(alpha * alpha),
                offset: point(px(0.), px(0.)),
                blur_radius: px(0.),
                spread_radius: px(1.),
            },
            BoxShadow {
                inset: false,
                color: gpui_kit::black().opacity(0.1 * alpha * alpha),
                offset: point(px(0.), px(4.)),
                blur_radius: px(6.),
                spread_radius: px(-1.),
            },
            BoxShadow {
                inset: false,
                color: gpui_kit::black().opacity(0.1 * alpha * alpha),
                offset: point(px(0.), px(2.)),
                blur_radius: px(4.),
                spread_radius: px(-2.),
            },
        ])
}
