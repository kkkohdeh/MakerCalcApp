use super::tooltip::TooltipExt as _;
use std::time::Duration;

use gpui_kit::base::motion::{Easing, Interpolate, Transition, transition};
use gpui_kit::base::{Button as BaseButton, StyledExt as _};
use gpui_kit::component::{ActiveTheme as _, tooltip::Tooltip, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, BoxShadow, Div, ElementId, Hsla, IntoElement, ParentElement as _, RenderOnce,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, point, px,
};

#[derive(Clone, Copy, PartialEq)]
struct Paint {
    background: Hsla,
    border: Hsla,
}
impl Interpolate for Paint {
    fn interpolate(&self, target: &Self, progress: f32) -> Self {
        Self {
            background: self.background.interpolate(&target.background, progress),
            border: self.border.interpolate(&target.border, progress),
        }
    }
}

#[derive(IntoElement)]
pub(in crate::gpui_app) struct Item {
    id: ElementId,
    trigger: BaseButton,
    content: AnyElement,
    outline: bool,
    selected: bool,
    disabled: bool,
    tooltip: Option<SharedString>,
}
impl Item {
    pub(in crate::gpui_app) fn new(
        id: impl Into<ElementId>,
        trigger: BaseButton,
        content: impl IntoElement,
    ) -> Self {
        Self {
            id: id.into(),
            trigger,
            content: content.into_any_element(),
            outline: false,
            selected: false,
            disabled: false,
            tooltip: None,
        }
    }
    pub(in crate::gpui_app) fn outline(mut self) -> Self {
        self.outline = true;
        self
    }
    pub(in crate::gpui_app) fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }
    pub(in crate::gpui_app) fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
    pub(in crate::gpui_app) fn tooltip(mut self, tooltip: impl Into<SharedString>) -> Self {
        self.tooltip = Some(tooltip.into());
        self
    }
}

pub(in crate::gpui_app) fn content() -> Div {
    v_flex().flex_1().min_w_0().gap(px(4.))
}
pub(in crate::gpui_app) fn title() -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .text_size(px(14.))
        .line_height(px(19.25))
        .font_medium()
        .truncate()
}
pub(in crate::gpui_app) fn description(cx: &App) -> Div {
    div()
        .min_w_0()
        .text_size(px(14.))
        .line_height(px(21.))
        .font_normal()
        .text_color(cx.theme().muted_foreground)
        .whitespace_normal()
        .line_clamp(2)
}

impl RenderOnce for Item {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let hover = window.use_keyed_state((self.id.clone(), "hover"), cx, |_, _| false);
        let focus = window
            .use_keyed_state((self.id.clone(), "focus"), cx, |_, cx| cx.focus_handle())
            .read(cx)
            .clone();
        let focused =
            !self.disabled && focus.is_focused(window) && window.last_input_was_keyboard();
        let hovered = !self.disabled && *hover.read(cx) && !window.last_input_was_keyboard();
        let theme = cx.theme();
        let ring = theme.ring;
        let foreground = theme.foreground;
        let target = Paint {
            background: if self.selected {
                theme.list_active
            } else if hovered {
                if self.outline {
                    theme.muted
                } else {
                    theme.list_hover
                }
            } else if self.outline {
                theme.input_background()
            } else {
                theme.transparent
            },
            border: if focused {
                theme.ring
            } else if self.outline {
                theme.border
            } else {
                theme.transparent
            },
        };
        let paint = transition(
            (self.id, "paint"),
            target,
            Transition::new(Duration::from_millis(100)).easing(Easing::CubicBezier {
                x1: 0.4,
                y1: 0.,
                x2: 0.2,
                y2: 1.,
            }),
            window,
            cx,
        );
        self.trigger
            .track_focus(&focus)
            .disabled(self.disabled)
            .w_full()
            .min_w_0()
            .flex_wrap()
            .items_center()
            .gap(px(10.))
            .px(px(12.))
            .py(px(10.))
            .rounded(px(10.))
            .border_1()
            .border_color(paint.border)
            .bg(paint.background)
            .text_color(foreground)
            .text_size(px(14.))
            .line_height(px(20.))
            .when(self.disabled, |item| item.opacity(0.5))
            .when(!self.disabled, |item| {
                item.cursor_pointer().on_hover(move |hovered, _, cx| {
                    hover.update(cx, |value, cx| {
                        if *value != *hovered {
                            *value = *hovered;
                            cx.notify();
                        }
                    });
                })
            })
            .when(focused, |item| {
                item.shadow(vec![BoxShadow {
                    inset: false,
                    color: ring.opacity(0.5),
                    offset: point(px(0.), px(0.)),
                    blur_radius: px(0.),
                    spread_radius: px(3.),
                }])
            })
            .when_some(self.tooltip, |item, tooltip| {
                item.app_tooltip_with_focus(Some(focus.clone()), move |window, cx| {
                    Tooltip::new(tooltip.clone()).build(window, cx)
                })
            })
            .child(self.content)
    }
}
