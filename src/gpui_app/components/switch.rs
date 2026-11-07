use std::{rc::Rc, time::Duration};

use gpui_kit::base::motion::{Easing, Transition, transition};
use gpui_kit::base::{StyledExt as _, Switch as BaseSwitch, TestSupportExt as _};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, BoxShadow, CursorStyle, ElementId, InteractiveElement as _, IntoElement,
    ParentElement as _, RenderOnce, SharedString, Styled as _, Window, div, point, px,
};

type Change = Rc<dyn Fn(&bool, &mut Window, &mut App)>;

#[derive(IntoElement)]
pub(in crate::gpui_app) struct Switch {
    id: ElementId,
    label: SharedString,
    checked: bool,
    disabled: bool,
    change: Option<Change>,
}
impl Switch {
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

impl RenderOnce for Switch {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let focus = window
            .use_keyed_state((self.id.clone(), "focus-handle"), cx, |_, cx| {
                cx.focus_handle()
            })
            .read(cx)
            .clone();
        let focused =
            !self.disabled && focus.is_focused(window) && window.last_input_was_keyboard();
        let timing = Transition::new(Duration::from_millis(150)).easing(Easing::CubicBezier {
            x1: 0.4,
            y1: 0.,
            x2: 0.2,
            y2: 1.,
        });
        let background = transition(
            (self.id.clone(), "background"),
            (if self.checked {
                cx.theme().tokens.primary
            } else {
                cx.theme().tokens.switch
            })
            .opacity(if self.disabled { 0.5 } else { 1. }),
            timing.clone(),
            window,
            cx,
        );
        let thumb_x = transition(
            (self.id.clone(), "thumb"),
            if self.checked { 14_f32 } else { 0_f32 },
            timing.clone(),
            window,
            cx,
        );
        let border = transition(
            (self.id.clone(), "focus"),
            cx.theme().ring.opacity(if focused { 1. } else { 0. }),
            timing,
            window,
            cx,
        );
        let track = div()
            .id((self.id.clone(), "track"))
            .test_support()
            .flex_none()
            .flex()
            .items_center()
            .relative()
            .w(px(32.))
            .h(px(18.4))
            .rounded_full()
            .border_1()
            .border_color(border)
            .bg(background)
            .when(border.a > 0., |track| {
                track.shadow(vec![BoxShadow {
                    inset: false,
                    color: cx.theme().ring.opacity(border.a * 0.5),
                    offset: point(px(0.), px(0.)),
                    blur_radius: px(0.),
                    spread_radius: px(3.),
                }])
            })
            .child(
                div()
                    .id((self.id.clone(), "thumb"))
                    .test_support()
                    .flex_none()
                    .relative()
                    .size(px(16.))
                    .left(px(thumb_x))
                    .rounded_full()
                    .bg(cx.theme().tokens.switch_thumb),
            );
        BaseSwitch::new(self.id.clone())
            .checked(self.checked)
            .disabled(self.disabled)
            .track_focus(&focus)
            .accessibility_label(self.label.clone())
            .flex()
            .items_center()
            .min_w_0()
            .gap(px(8.))
            .px(px(12.))
            .py(px(8.))
            .mx(px(-12.))
            .my(px(-8.))
            .cursor(if self.disabled {
                CursorStyle::OperationNotAllowed
            } else {
                CursorStyle::PointingHand
            })
            .child(track)
            .child(
                div()
                    .id((self.id, "label"))
                    .test_support()
                    .min_w_0()
                    .refine_style(&super::label::style(self.disabled, cx))
                    .child(self.label),
            )
            .when_some(self.change, |root, change| {
                root.on_change(move |checked, _, window, cx| {
                    change(&checked, window, cx);
                })
            })
    }
}
