use std::{rc::Rc, time::Duration};

use gpui_kit::base::motion::{Easing, Transition, transition};
use gpui_kit::base::{StyledExt as _, Tab, Tabs as BaseTabs, TestSupportExt as _};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, BoxShadow, ElementId, FocusHandle, InteractiveElement as _, IntoElement, KeyDownEvent,
    ParentElement as _, RenderOnce, SharedString, StatefulInteractiveElement as _, Styled as _,
    Window, div, point, px,
};

type Change = Rc<dyn Fn(&usize, &mut Window, &mut App)>;

#[derive(IntoElement)]
pub(in crate::gpui_app) struct Tabs {
    id: ElementId,
    labels: Vec<SharedString>,
    selected: usize,
    change: Option<Change>,
}
impl Tabs {
    pub(in crate::gpui_app) fn new(
        id: impl Into<ElementId>,
        selected: usize,
        labels: impl IntoIterator<Item = SharedString>,
    ) -> Self {
        Self {
            id: id.into(),
            labels: labels.into_iter().collect(),
            selected,
            change: None,
        }
    }
    pub(in crate::gpui_app) fn on_change(
        mut self,
        change: impl Fn(&usize, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.change = Some(Rc::new(change));
        self
    }
}
impl RenderOnce for Tabs {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let total = self.labels.len();
        let (handles, roving) = window.with_element_namespace(self.id.clone(), |window| {
            let handles: Vec<FocusHandle> = (0..total)
                .map(|index| {
                    window
                        .use_keyed_state(("focus", index), cx, |_, cx| cx.focus_handle())
                        .read(cx)
                        .clone()
                })
                .collect();
            let roving = window.use_keyed_state("roving", cx, |_, _| self.selected);
            (handles, roving)
        });
        let timing = Transition::new(Duration::from_millis(150)).easing(Easing::CubicBezier {
            x1: 0.4,
            y1: 0.,
            x2: 0.2,
            y2: 1.,
        });
        let mut root = BaseTabs::new(self.id.clone())
            .tab_group()
            .aria_orientation(gpui_kit::accesskit::Orientation::Horizontal)
            .flex()
            .items_center()
            .flex_none()
            .h(px(32.))
            .p(px(3.))
            .gap(px(4.));
        for (index, label) in self.labels.into_iter().enumerate() {
            let id = ElementId::from((self.id.clone(), format!("tab-{index}")));
            let hover = window.use_keyed_state((id.clone(), "hover"), cx, |_, _| false);
            let focused = handles[index].is_focused(window) && window.last_input_was_keyboard();
            let selected = self.selected == index;
            let color = transition(
                (id.clone(), "color"),
                if selected || *hover.read(cx) {
                    cx.theme().tab_active_foreground
                } else {
                    cx.theme().tab_foreground
                },
                timing.clone(),
                window,
                cx,
            );
            let line = transition(
                (id.clone(), "line"),
                if selected { 1_f32 } else { 0_f32 },
                timing.clone(),
                window,
                cx,
            );
            let ring = transition(
                (id.clone(), "ring"),
                cx.theme().ring.opacity(if focused { 1. } else { 0. }),
                timing.clone(),
                window,
                cx,
            );
            let focus = handles[index].clone();
            let current = roving.clone();
            let change = self.change.clone();
            root = root.child(
                Tab::new(id.clone())
                    .selected(selected)
                    .set_position(index + 1, total)
                    .accessibility_label(label.clone())
                    .track_focus(
                        &focus
                            .clone()
                            .tab_index(index as isize)
                            .tab_stop(*roving.read(cx) == index),
                    )
                    .relative()
                    .flex_none()
                    .h(px(25.))
                    .px(px(6.))
                    .py(px(2.))
                    .border_1()
                    .border_color(ring)
                    .rounded(px(8.))
                    .text_size(px(14.))
                    .line_height(px(20.))
                    .font_medium()
                    .text_color(color)
                    .when(ring.a > 0., |tab| {
                        tab.shadow(vec![BoxShadow {
                            inset: false,
                            color: ring.opacity(ring.a * 0.5),
                            offset: point(px(0.), px(0.)),
                            blur_radius: px(0.),
                            spread_radius: px(3.),
                        }])
                    })
                    .on_hover(move |value, _, cx| {
                        hover.update(cx, |hover, cx| {
                            if *hover != *value {
                                *hover = *value;
                                cx.notify();
                            }
                        })
                    })
                    .on_click(move |_, window, cx| {
                        window.focus(&focus, cx);
                        current.update(cx, |current, cx| {
                            *current = index;
                            cx.notify();
                        });
                        if let Some(change) = &change {
                            change(&index, window, cx);
                        }
                    })
                    .child(label)
                    .child(
                        div()
                            .id((id, "indicator"))
                            .test_support()
                            .absolute()
                            .left_0()
                            .right_0()
                            .bottom(px(-5.))
                            .h(px(2.))
                            .bg(cx.theme().tokens.primary.opacity(line)),
                    ),
            );
        }
        root.on_key_down(move |event: &KeyDownEvent, window, cx| {
            if event.keystroke.modifiers.modified() {
                return;
            }
            let Some(current) = handles.iter().position(|handle| handle.is_focused(window)) else {
                return;
            };
            let next = match event.keystroke.key.as_str() {
                "left" => (current + total - 1) % total,
                "right" => (current + 1) % total,
                "home" => 0,
                "end" => total - 1,
                _ => return,
            };
            window.prevent_default();
            cx.stop_propagation();
            roving.update(cx, |current, cx| {
                *current = next;
                cx.notify();
            });
            // Manual activation: focusing a tab must not query its section.
            window.focus(&handles[next], cx);
        })
    }
}
