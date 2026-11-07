use super::tooltip::TooltipExt as _;
use std::rc::Rc;

use gpui_kit::base::{Radio as BaseRadio, TestSupportExt as _};
use gpui_kit::component::{ActiveTheme as _, tooltip::Tooltip, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, BoxShadow, ElementId, FocusHandle, InteractiveElement as _, IntoElement,
    KeyDownEvent, ParentElement as _, RenderOnce, Role, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, point, px,
};

type Change = Rc<dyn Fn(&usize, &mut Window, &mut App)>;

pub(in crate::gpui_app) struct Radio {
    id: ElementId,
    label: SharedString,
    name: Option<SharedString>,
    description: Option<AnyElement>,
    tooltip: Option<SharedString>,
}
impl Radio {
    pub(in crate::gpui_app) fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            label: "".into(),
            name: None,
            description: None,
            tooltip: None,
        }
    }
    pub(in crate::gpui_app) fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = label.into();
        self
    }
    pub(in crate::gpui_app) fn accessibility_label(
        mut self,
        name: impl Into<SharedString>,
    ) -> Self {
        self.name = Some(name.into());
        self
    }
    pub(in crate::gpui_app) fn card(mut self, description: impl IntoElement) -> Self {
        self.description = Some(description.into_any_element());
        self
    }
    pub(in crate::gpui_app) fn tooltip(mut self, tooltip: impl Into<SharedString>) -> Self {
        self.tooltip = Some(tooltip.into());
        self
    }
    fn render(
        self,
        position: (usize, usize),
        checked: bool,
        disabled: bool,
        focus: &FocusHandle,
        window: &Window,
        cx: &App,
    ) -> BaseRadio {
        let focused = !disabled && focus.is_focused(window) && window.last_input_was_keyboard();
        let ring = cx.theme().ring;
        let primary = cx.theme().primary;
        let card = self.description.is_some();
        let mark = div()
            .id((self.id.clone(), "indicator"))
            .test_support()
            .flex_none()
            .size(px(16.))
            .rounded_full()
            .border_1()
            .border_color(if focused {
                ring
            } else if checked {
                primary
            } else {
                cx.theme().input
            })
            .bg(if checked {
                primary
            } else {
                cx.theme().input_background()
            })
            .flex()
            .items_center()
            .justify_center()
            .when(focused, |mark| {
                mark.shadow(vec![BoxShadow {
                    inset: false,
                    color: ring.opacity(0.5),
                    offset: point(px(0.), px(0.)),
                    blur_radius: px(0.),
                    spread_radius: px(3.),
                }])
            })
            .when(checked, |mark| {
                mark.child(
                    div()
                        .id((self.id.clone(), "dot"))
                        .test_support()
                        .size(px(8.))
                        .rounded_full()
                        .bg(cx.theme().primary_foreground),
                )
            });
        BaseRadio::new(self.id.clone())
            .checked(checked)
            .disabled(disabled)
            .track_focus(focus)
            .set_position(position.0, position.1)
            .accessibility_label(self.name.unwrap_or_else(|| self.label.clone()))
            .flex()
            .items_start()
            .w_full()
            .min_w_0()
            .gap(px(12.))
            .text_color(cx.theme().foreground)
            .opacity(if disabled { 0.5 } else { 1. })
            .when(disabled, |radio| radio.cursor_not_allowed())
            .when(!disabled, |radio| radio.cursor_pointer())
            .when(card, |radio| {
                radio
                    .p(px(8.))
                    .border_1()
                    .border_color(if focused { ring } else { cx.theme().border })
                    .rounded(px(8.))
                    .when(!disabled, |radio| {
                        radio.hover(move |style| style.border_color(primary))
                    })
            })
            // Expand the pointer area without changing row spacing.
            .when(!card, |radio| {
                radio.px(px(12.)).py(px(8.)).mx(px(-12.)).my(px(-8.))
            })
            .when_some(self.tooltip, |radio, tooltip| {
                radio.app_tooltip_with_focus(Some(focus.clone()), move |window, cx| {
                    Tooltip::new(tooltip.clone()).build(window, cx)
                })
            })
            .child(mark)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(4.))
                    .child(
                        super::label::inline()
                            .id((self.id, "label"))
                            .test_support()
                            .w_full()
                            .mt(px(1.))
                            .truncate()
                            .child(self.label),
                    )
                    .children(self.description),
            )
    }
}

#[derive(IntoElement)]
pub(in crate::gpui_app) struct RadioGroup {
    id: ElementId,
    radios: Vec<Radio>,
    selected: Option<usize>,
    disabled: bool,
    change: Option<Change>,
}
impl RadioGroup {
    pub(in crate::gpui_app) fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            radios: Vec::new(),
            selected: None,
            disabled: false,
            change: None,
        }
    }
    pub(in crate::gpui_app) fn children(mut self, radios: impl IntoIterator<Item = Radio>) -> Self {
        self.radios.extend(radios);
        self
    }
    pub(in crate::gpui_app) fn selected_index(mut self, selected: Option<usize>) -> Self {
        self.selected = selected;
        self
    }
    pub(in crate::gpui_app) fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
    pub(in crate::gpui_app) fn on_change(
        mut self,
        change: impl Fn(&usize, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.change = Some(Rc::new(change));
        self
    }
}
impl RenderOnce for RadioGroup {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let total = self.radios.len();
        let handles: Vec<FocusHandle> = window.with_element_namespace(self.id.clone(), |window| {
            self.radios
                .iter()
                .map(|radio| {
                    window
                        .use_keyed_state((radio.id.clone(), "focus"), cx, |_, cx| cx.focus_handle())
                        .read(cx)
                        .clone()
                })
                .collect()
        });
        let selected = self.selected.filter(|index| *index < total);
        let mut root = v_flex()
            .id(self.id)
            .test_support()
            .role(Role::RadioGroup)
            .aria_orientation(gpui_kit::accesskit::Orientation::Vertical)
            .w_full()
            .min_w_0()
            .gap(px(8.));
        for (index, radio) in self.radios.into_iter().enumerate() {
            let change = self.change.clone();
            let focus = handles[index].clone();
            let item = radio
                .render(
                    (index + 1, total),
                    selected == Some(index),
                    self.disabled,
                    &focus,
                    window,
                    cx,
                )
                .tab_stop(index == selected.unwrap_or(0))
                .when_some(change, |radio, change| {
                    radio.on_change(move |_, _, window, cx| {
                        window.focus(&focus, cx);
                        change(&index, window, cx);
                    })
                });
            // BaseRadio owns Space/Enter, checked semantics and disabled activation.
            root = root.child(item);
        }
        root.on_key_down(move |event: &KeyDownEvent, window, cx| {
            if self.disabled || event.keystroke.modifiers.modified() {
                return;
            }
            let Some(current) = handles.iter().position(|handle| handle.is_focused(window)) else {
                return;
            };
            let next = match event.keystroke.key.as_str() {
                "up" | "left" => (current + total - 1) % total,
                "down" | "right" => (current + 1) % total,
                _ => return,
            };
            window.prevent_default();
            cx.stop_propagation();
            window.focus(&handles[next], cx);
            if selected != Some(next)
                && let Some(change) = &self.change
            {
                change(&next, window, cx);
            }
        })
    }
}
