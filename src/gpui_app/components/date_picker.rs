use std::rc::Rc;

use gpui_kit::base::{
    Align, ElementExt as _, POPUP_PRIORITY, Placement, PopoverState, Positioner, Selectable as _,
    StyledExt as _, TestSupportExt as _, actions::Cancel,
};
use gpui_kit::component::calendar::{CalendarState, Date};
use gpui_kit::component::{ActiveTheme as _, IconName};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, Bounds, Entity, Focusable as _, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, RenderOnce, Role, StatefulInteractiveElement as _, Styled as _, Window,
    deferred, div, px,
};

use super::{alert_dialog::view_presence, button::Button, calendar::Calendar, zoom::Zoom};
use crate::i18n::t;

#[derive(IntoElement)]
pub(in crate::gpui_app) struct DatePicker {
    state: Entity<CalendarState>,
    disabled: bool,
}
impl DatePicker {
    pub(in crate::gpui_app) fn new(state: &Entity<CalendarState>, disabled: bool) -> Self {
        Self {
            state: state.clone(),
            disabled,
        }
    }
}
impl RenderOnce for DatePicker {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let date = self.state.read(cx).date();
        let label = date
            .format("%Y-%m-%d")
            .unwrap_or_else(|| t!("history.filters.dates").into_owned().into());
        let focus = window
            .use_keyed_state("history-date-focus", cx, |_, cx| cx.focus_handle())
            .read(cx)
            .clone();
        let popup = window.use_keyed_state("history-date-open", cx, |_, cx| {
            PopoverState::new(false, cx)
        });
        if self.disabled {
            popup.update(cx, |popup, cx| popup.dismiss(window, cx));
        }
        let scope = popup.read(cx).focus_handle(cx);
        let open = popup.read(cx).is_open();
        let presence = view_presence("history-date-presence", open.then_some(()), window, cx);
        let bounds = window.use_keyed_state("history-date-anchor", cx, |_, _| Bounds::default());
        let trigger_bounds = *bounds.read(cx);
        let toggle = popup.clone();
        let delete = self.state.clone();
        let trigger_focus = focus.clone();
        let disabled = self.disabled;
        let mut root = div()
            .id("history-date-root")
            .relative()
            .w_full()
            .on_prepaint(move |current, window, cx| {
                if *bounds.read(cx) != current {
                    bounds.update(cx, |bounds, _| *bounds = current);
                    window.request_animation_frame();
                }
            })
            .child(
                Button::new("history-date-picker")
                    .outline()
                    .popup_trigger()
                    .w_full()
                    .h(px(32.))
                    .justify_start()
                    .font_normal()
                    .text_left()
                    .icon(IconName::Calendar)
                    .track_focus(&focus)
                    .disabled(disabled)
                    .selected(open)
                    .aria_expanded(open)
                    .accessibility_label(label.clone())
                    .when(!date.is_some(), |button| {
                        button.text_color(cx.theme().muted_foreground)
                    })
                    .when(date.is_some(), |button| button.pr(px(32.)))
                    .child(div().min_w_0().truncate().child(label))
                    .on_click(move |_, window, cx| {
                        toggle.update(cx, |state, cx| state.toggle_open(window, cx));
                        window.refresh();
                    })
                    .on_key_down(move |event, window, cx| {
                        if !disabled
                            && trigger_focus.is_focused(window)
                            && matches!(event.keystroke.key.as_str(), "delete" | "backspace")
                        {
                            delete.update(cx, |state, cx| {
                                state.set_date(Date::Range(None, None), window, cx)
                            });
                            cx.stop_propagation();
                        }
                    }),
            );
        if date.is_some() {
            let reset = self.state.clone();
            root = root.child(
                Button::new("history-date-clear")
                    .ghost()
                    .icon(IconName::Close)
                    .size(px(24.))
                    .absolute()
                    .right(px(4.))
                    .top(px(4.))
                    .disabled(disabled)
                    .accessibility_label(t!("common.actions.clear").into_owned())
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(move |_, window, cx| {
                        reset.update(cx, |state, cx| {
                            state.set_date(Date::Range(None, None), window, cx)
                        });
                        focus.focus(window, cx);
                        cx.stop_propagation();
                    }),
            );
        }
        if let Some(presence) = presence {
            let dismiss = Rc::new(move |window: &mut Window, cx: &mut App| {
                popup.update(cx, |popup, cx| popup.dismiss(window, cx));
                window.refresh();
            });
            let outside = dismiss.clone();
            let escape = dismiss.clone();
            let alpha = presence.alpha;
            let surface = super::popover::content(alpha, cx)
                .id("history-date-surface")
                .test_support()
                .w_auto()
                .p_0()
                .role(Role::Dialog)
                .aria_label(t!("history.filters.dates").into_owned())
                .track_focus(&scope)
                .key_context("Popover")
                .tab_group()
                .occlude()
                .on_action(move |_: &Cancel, window, cx| escape(window, cx))
                .on_mouse_down_out(move |_, window, cx| outside(window, cx))
                .child(
                    Calendar::new(&self.state)
                        .autofocus()
                        .disabled(disabled || presence.closing)
                        .on_selected(move |_, window, cx| dismiss(window, cx)),
                );
            root = root.child(
                deferred(
                    Positioner::side(trigger_bounds)
                        .placement(Placement::Bottom)
                        .align(Align::Start)
                        .offset(px(4.))
                        .margin(px(8.))
                        .occlude()
                        .child(
                            Zoom::new("history-date-zoom", surface)
                                .factor(0.95 + 0.05 * alpha)
                                .anchored(trigger_bounds, px(8. * (1. - alpha))),
                        ),
                )
                .with_priority(POPUP_PRIORITY),
            );
        }
        root
    }
}
