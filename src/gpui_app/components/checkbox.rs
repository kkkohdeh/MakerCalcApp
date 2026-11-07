use gpui_kit::base::motion::{Easing, Interpolate, Transition, transition};
use gpui_kit::base::{Checkbox as BaseCheckbox, CheckboxState, TestSupportExt as _};
use gpui_kit::component::{ActiveTheme as _, Icon, IconName};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, BoxShadow, CursorStyle, ElementId, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, RenderOnce, SharedString, Styled as _, Window, div, point, px,
};
use std::{rc::Rc, time::Duration};

type Change = Rc<dyn Fn(&bool, &mut Window, &mut App)>;

#[derive(Clone, Copy, PartialEq)]
struct Colors {
    background: Hsla,
    border: Hsla,
    foreground: Hsla,
}
impl Interpolate for Colors {
    fn interpolate(&self, to: &Self, delta: f32) -> Self {
        Self {
            background: self.background.interpolate(&to.background, delta),
            border: self.border.interpolate(&to.border, delta),
            foreground: self.foreground.interpolate(&to.foreground, delta),
        }
    }
}

#[derive(IntoElement)]
pub(in crate::gpui_app) struct Checkbox {
    id: ElementId,
    label: SharedString,
    checked: bool,
    disabled: bool,
    change: Option<Change>,
}
impl Checkbox {
    pub(in crate::gpui_app) fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            label: "".into(),
            checked: false,
            disabled: false,
            change: None,
        }
    }
    pub(in crate::gpui_app) fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = label.into();
        self
    }
    pub(in crate::gpui_app) fn checked(mut self, checked: bool) -> Self {
        self.checked = checked;
        self
    }
    pub(in crate::gpui_app) fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
    pub(in crate::gpui_app) fn on_change(
        mut self,
        change: impl Fn(&bool, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.change = Some(Rc::new(change));
        self
    }
}
impl RenderOnce for Checkbox {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let focus = window
            .use_keyed_state((self.id.clone(), "focus"), cx, |_, cx| cx.focus_handle())
            .read(cx)
            .clone();
        let focused =
            !self.disabled && focus.is_focused(window) && window.last_input_was_keyboard();
        let colors = transition(
            (self.id.clone(), "colors"),
            Colors {
                background: if self.checked {
                    cx.theme().primary
                } else {
                    cx.theme().input_background()
                },
                border: if focused {
                    cx.theme().ring
                } else if self.checked {
                    cx.theme().primary
                } else {
                    cx.theme().input
                },
                foreground: if self.checked {
                    cx.theme().primary_foreground
                } else {
                    cx.theme().foreground
                },
            },
            Transition::new(Duration::from_millis(150)).easing(Easing::CubicBezier {
                x1: 0.4,
                y1: 0.,
                x2: 0.2,
                y2: 1.,
            }),
            window,
            cx,
        );
        let indicator = div()
            .id((self.id.clone(), "indicator"))
            .test_support()
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .size(px(16.))
            .rounded(px(4.))
            .border_1()
            .border_color(colors.border)
            .bg(colors.background)
            .when(focused, |indicator| {
                indicator.shadow(vec![BoxShadow {
                    inset: false,
                    color: cx.theme().ring.opacity(0.5),
                    offset: point(px(0.), px(0.)),
                    blur_radius: px(0.),
                    spread_radius: px(3.),
                }])
            })
            .when(self.checked, |indicator| {
                indicator.child(
                    div()
                        .id((self.id.clone(), "check"))
                        .test_support()
                        .size(px(14.))
                        .child(
                            Icon::new(IconName::Check)
                                .size(px(14.))
                                .text_color(colors.foreground),
                        ),
                )
            });
        // Negative margins expand the pointer target without changing flow geometry.
        // The full label remains one controlled toggle.
        BaseCheckbox::new(self.id.clone())
            .checked(self.checked)
            .disabled(self.disabled)
            .track_focus(&focus)
            .accessibility_label(self.label.clone())
            .flex()
            .items_start()
            .gap(px(8.))
            .px(px(12.))
            .py(px(8.))
            .mx(px(-12.))
            .my(px(-8.))
            .text_color(cx.theme().foreground)
            .opacity(if self.disabled { 0.5 } else { 1. })
            .cursor(if self.disabled {
                CursorStyle::OperationNotAllowed
            } else {
                CursorStyle::PointingHand
            })
            .child(indicator)
            .child(
                super::label::inline()
                    .id((self.id, "label"))
                    .test_support()
                    .flex_1()
                    .min_w_0()
                    .mt(px(1.))
                    .child(self.label),
            )
            .when_some(self.change, |checkbox, change| {
                checkbox.on_change(move |state, _, window, cx| {
                    window.prevent_default();
                    change(&(state == CheckboxState::Checked), window, cx);
                })
            })
    }
}
