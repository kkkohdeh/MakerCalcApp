use std::time::Duration;

use gpui_kit::base::motion::{Easing, Transition, transition};
use gpui_kit::base::{Button, StyledExt as _, TestSupportExt as _};
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, BoxShadow, ElementId, FocusHandle, InteractiveElement as _, IntoElement,
    ParentElement as _, RenderOnce, Role, StatefulInteractiveElement as _, Styled as _, Window,
    div, point, px,
};

/// Controlled single-item Accordion with a 200 ms ease-out reveal.
#[derive(IntoElement)]
pub(in crate::gpui_app) struct Accordion {
    id: ElementId,
    trigger: Button,
    label: gpui_kit::SharedString,
    content: AnyElement,
    open: bool,
}

impl Accordion {
    pub(in crate::gpui_app) fn new(
        id: impl Into<ElementId>,
        label: impl Into<gpui_kit::SharedString>,
        open: bool,
        trigger: Button,
        content: impl IntoElement,
    ) -> Self {
        Self {
            id: id.into(),
            trigger,
            label: label.into(),
            content: content.into_any_element(),
            open,
        }
    }

    pub(in crate::gpui_app) fn track_focus(mut self, focus: &FocusHandle) -> Self {
        self.trigger = self.trigger.track_focus(focus);
        self
    }
}

impl RenderOnce for Accordion {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let progress = transition(
            (self.id.clone(), "height"),
            if self.open { 1_f32 } else { 0_f32 },
            Transition::new(Duration::from_millis(200)).easing(Easing::EaseOut),
            window,
            cx,
        );
        let ring = cx.theme().ring;
        let label = self.label.clone();
        let trigger = self
            .trigger
            .accessibility_label(self.label.clone())
            .aria_expanded(self.open)
            .w_full()
            .min_w_0()
            .flex()
            .items_start()
            .justify_between()
            .py(px(10.))
            .border_1()
            .border_color(cx.theme().transparent)
            .rounded(px(10.))
            .text_size(px(14.))
            .line_height(px(20.))
            .font_medium()
            .text_color(cx.theme().foreground)
            .cursor_pointer()
            .styles(|styles| styles.disabled(|style| style.opacity(0.5)))
            .hover(|style| style.underline())
            .focus_visible(move |style| {
                style.border_color(ring).shadow(vec![BoxShadow {
                    inset: false,
                    color: ring.opacity(0.5),
                    offset: point(px(0.), px(0.)),
                    blur_radius: px(0.),
                    spread_radius: px(3.),
                }])
            })
            .child(div().flex_1().min_w_0().child(self.label))
            .child(
                div()
                    .id((self.id.clone(), "icon"))
                    .test_support()
                    .flex_none()
                    .child(
                        Icon::new(if self.open {
                            IconName::ChevronUp
                        } else {
                            IconName::ChevronDown
                        })
                        .size(px(16.))
                        .text_color(cx.theme().muted_foreground),
                    ),
            );
        let content = div()
            .id((self.id.clone(), "content"))
            .test_support()
            .role(Role::Region)
            .aria_label(label)
            .w_full()
            .min_w_0()
            .pb(px(10.))
            .text_size(px(14.))
            .line_height(px(20.))
            .child(self.content)
            .into_any_element();
        v_flex()
            .id(self.id.clone())
            .test_support()
            .w_full()
            .min_w_0()
            .child(
                div()
                    .id((self.id.clone(), "header"))
                    .test_support()
                    .role(Role::Heading)
                    .aria_level(3)
                    .flex()
                    .w_full()
                    .child(trigger),
            )
            .when(self.open || progress > 0., |root| {
                if cx.reduce_motion() {
                    root.child(content)
                } else {
                    root.child(gpui_kit::base::MotionReveal::new(
                        (self.id, "reveal"),
                        progress,
                        content,
                    ))
                }
            })
    }
}
