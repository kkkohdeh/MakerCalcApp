use gpui_kit::base::StyledExt as _;
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::{App, BoxShadow, Div, Hsla, Styled as _, point, px};

pub(in crate::gpui_app) const RADIUS: f32 = 14.;

fn spacing(small: bool) -> f32 {
    if small { 12. } else { 16. }
}

/// An outer ring, not a layout border: slots keep the reference's exact insets.
pub(in crate::gpui_app) fn ring(color: Hsla) -> Vec<BoxShadow> {
    vec![BoxShadow {
        inset: false,
        color,
        offset: point(px(0.), px(0.)),
        blur_radius: px(0.),
        spread_radius: px(1.),
    }]
}

pub(in crate::gpui_app) fn surface(small: bool, cx: &App) -> Div {
    v_flex()
        .min_w_0()
        .gap(px(spacing(small)))
        .py(px(spacing(small)))
        .rounded(px(RADIUS))
        .overflow_hidden()
        .bg(cx.theme().group_box)
        .text_color(cx.theme().group_box_foreground)
        .text_size(px(14.))
        .line_height(px(20.))
        .shadow(ring(cx.theme().border))
}

pub(in crate::gpui_app) fn header(small: bool) -> Div {
    v_flex()
        .flex_none()
        .min_w_0()
        .gap(px(4.))
        .px(px(spacing(small)))
}

pub(in crate::gpui_app) fn title(small: bool) -> Div {
    let font = if small { 14. } else { 16. };
    gpui_kit::div()
        .text_size(px(font))
        .line_height(px(font * 1.375))
        .font_medium()
}

pub(in crate::gpui_app) fn description(cx: &App) -> Div {
    gpui_kit::div()
        .text_size(px(14.))
        .line_height(px(20.))
        .text_color(cx.theme().muted_foreground)
}

pub(in crate::gpui_app) fn content(small: bool) -> Div {
    v_flex().min_w_0().px(px(spacing(small)))
}

pub(in crate::gpui_app) fn footer(small: bool, cx: &App) -> Div {
    h_flex()
        .flex_none()
        .items_center()
        .p(px(spacing(small)))
        .rounded_b(px(RADIUS))
        .border_t_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().group_box)
}
