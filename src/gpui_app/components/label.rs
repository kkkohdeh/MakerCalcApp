use gpui_kit::base::StyledExt as _;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{App, Div, StyleRefinement, Styled as _, div, px};

pub(in crate::gpui_app) fn inline() -> Div {
    // Plain GPUI text is not selectable; no rich-text selection engine is needed.
    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .text_size(px(14.))
        .line_height(px(14.))
        .font_medium()
}

pub(in crate::gpui_app) fn style(disabled: bool, cx: &App) -> StyleRefinement {
    inline()
        .self_center()
        .text_color(cx.theme().foreground)
        .opacity(if disabled { 0.5 } else { 1. })
        .when(disabled, |label| label.cursor_not_allowed())
        .style()
        .clone()
}
