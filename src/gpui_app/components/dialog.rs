use std::time::Duration;

use gpui_kit::base::motion::{Easing, Presence, Transition};
use gpui_kit::base::{ObservedElement, StyledExt as _, TestSupportExt as _};
use gpui_kit::component::{ActiveTheme as _, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, Div, FocusHandle, InteractiveElement as _, IntoElement, ParentElement, Role, Stateful,
    StatefulInteractiveElement, Styled, Window, div, px,
};

use super::{card, zoom::Zoom};

struct DisplayState<T> {
    value: Option<T>,
    previous: Option<FocusHandle>,
}

pub(in crate::gpui_app) struct Display<T> {
    pub value: T,
    pub alpha: f32,
    pub closing: bool,
}

/// Only UI handles are retained during exit, not a second executable intention.
/// Input handles are shared, not copied into strings, persisted, logged or sent.
/// Callers block the closing subtree and drop these handles after 100 ms.
pub(in crate::gpui_app) fn present<T: Clone + 'static>(
    id: &'static str,
    incoming: Option<T>,
    previous: Option<FocusHandle>,
    window: &mut Window,
    cx: &mut App,
) -> Option<Display<T>> {
    let open = incoming.is_some();
    let sample = Presence::new(id, open)
        .transition(Transition::new(Duration::from_millis(100)).easing(Easing::Ease))
        .sample(window, cx);
    let state = window.use_keyed_state((id, 1usize), cx, |_, _| DisplayState::<T> {
        value: None,
        previous: None,
    });
    if let Some(incoming) = incoming {
        state.update(cx, |state, _| {
            if state.value.is_none() {
                state.previous = previous;
            }
            state.value = Some(incoming);
        });
    }
    if !sample.should_render() {
        let previous = state.update(cx, |state, _| {
            state.value = None;
            state.previous.take()
        });
        if let Some(previous) = previous {
            previous.focus(window, cx);
        }
        return None;
    }
    Some(Display {
        value: state.read(cx).value.clone()?,
        alpha: sample.progress,
        closing: !open,
    })
}

pub(in crate::gpui_app) fn surface(
    id: &'static str,
    title: String,
    cx: &App,
) -> ObservedElement<Stateful<Div>> {
    v_flex()
        .id(id)
        .test_support()
        .relative()
        .role(Role::Dialog)
        .aria_label(title)
        .w_full()
        .max_w(px(384.))
        .min_h_0()
        .min_w_0()
        .p(px(16.))
        .gap(px(16.))
        .rounded(px(14.))
        .bg(cx.theme().background)
        .text_color(cx.theme().foreground)
        .text_size(px(14.))
        .line_height(px(20.))
        .shadow(card::ring(cx.theme().border))
        .overflow_hidden()
        .on_click(|_, _, cx| cx.stop_propagation())
}

pub(in crate::gpui_app) fn header() -> Div {
    v_flex().flex_none().min_w_0().pr(px(24.)).gap(px(8.))
}
pub(in crate::gpui_app) fn title() -> Stateful<Div> {
    div()
        .id("dialog-title")
        .role(Role::Heading)
        .aria_level(2)
        .text_size(px(16.))
        .line_height(px(16.))
        .font_medium()
}
pub(in crate::gpui_app) fn footer(window: &Window, cx: &App) -> Div {
    div()
        .flex()
        .flex_none()
        .gap(px(8.))
        .p(px(16.))
        .mx(px(-16.))
        .mb(px(-16.))
        .border_t_1()
        .border_color(cx.theme().border)
        .rounded_b(px(14.))
        .bg(cx.theme().muted.opacity(0.5))
        .when(window.viewport_size().width < px(640.), |footer| {
            footer.flex_col_reverse()
        })
        .when(window.viewport_size().width >= px(640.), |footer| {
            footer.justify_end()
        })
}

pub(in crate::gpui_app) fn animated(
    id: &'static str,
    surface: impl IntoElement + Styled + ParentElement + StatefulInteractiveElement + 'static,
    alpha: f32,
    closing: bool,
) -> impl IntoElement {
    let shield = div().id((id, 2usize)).absolute().inset_0().occlude();
    let content = surface.opacity(alpha).when(closing, |root| {
        root.capture_any_mouse_down(|_, _, cx| cx.stop_propagation())
            .capture_any_mouse_up(|_, _, cx| cx.stop_propagation())
            .capture_key_down(|_, _, cx| cx.stop_propagation())
            .capture_key_up(|_, _, cx| cx.stop_propagation())
            .child(shield)
    });
    Zoom::new(id, content).factor(0.95 + 0.05 * alpha)
}
