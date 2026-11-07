//! Toast presentation only; feedback retains episodes, limits and expiry ownership.
use std::{rc::Rc, time::Duration};

use gpui_kit::base::motion::{Easing, Presence, Transition};
use gpui_kit::base::{StyledExt as _, TestSupportExt as _};
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, h_flex, label::Label, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Context, InteractiveElement as _, IntoElement, ParentElement as _, Role,
    ScrollHandle, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};

use super::{button::Button, zoom::Zoom};
use crate::gpui_app::MakerCalcGpuiApp;
use crate::gpui_app::feedback::{MAX_TOASTS, Toast, ToastKind, break_long_words};

pub(in crate::gpui_app) struct Display {
    toast: Rc<Toast>,
    progress: f32,
    closing: bool,
}

/// At most five current and five inert ending display values. No timeout,
/// episode, queue, action or printer state is owned by this projection.
pub(in crate::gpui_app) fn present(
    incoming: &[Toast],
    window: &mut Window,
    cx: &mut App,
) -> Vec<Display> {
    let retained = window.use_keyed_state("toast-display", cx, |_, _| Vec::<Rc<Toast>>::new());
    let values = retained.update(cx, |values, _| {
        let mut ending = 0;
        values.retain(|toast| {
            if incoming.iter().any(|item| item.id == toast.id) {
                true
            } else {
                ending += 1;
                ending <= MAX_TOASTS
            }
        });
        for toast in incoming {
            if !values.iter().any(|item| item.id == toast.id) {
                values.push(Rc::new(toast.clone()));
            }
        }
        values.clone()
    });
    let displays: Vec<_> = values
        .into_iter()
        .filter_map(|toast| {
            let open = incoming.iter().any(|item| item.id == toast.id);
            let sample = Presence::new((("toast-presence", toast.id), "presence"), open)
                .transition(Transition::new(Duration::from_millis(500)).easing(
                    Easing::CubicBezier {
                        x1: 0.22,
                        y1: 1.,
                        x2: 0.36,
                        y2: 1.,
                    },
                ))
                .sample(window, cx);
            sample.should_render().then_some(Display {
                toast,
                progress: sample.progress,
                closing: !open,
            })
        })
        .collect();
    retained.update(cx, |values, _| {
        values.retain(|value| displays.iter().any(|display| display.toast.id == value.id))
    });
    displays
}

pub(in crate::gpui_app) fn render(
    display: Display,
    max_toast_height: gpui_kit::Pixels,
    window: &mut Window,
    cx: &mut Context<MakerCalcGpuiApp>,
) -> AnyElement {
    let toast = display.toast;
    let id = toast.id;
    let body_scroll = window
        .use_keyed_state(("toast-text-scroll", id as usize), cx, |_, _| {
            ScrollHandle::default()
        })
        .read(cx)
        .clone();
    let (icon, color) = match toast.kind {
        ToastKind::Info => (gpui_kit::assets::IconName::Info, cx.theme().blue),
        ToastKind::Success => (gpui_kit::assets::IconName::CircleCheck, cx.theme().green),
        ToastKind::Warning => (gpui_kit::assets::IconName::TriangleAlert, cx.theme().yellow),
        ToastKind::Error => (gpui_kit::assets::IconName::OctagonX, cx.theme().red),
    };
    let text = v_flex()
        .id(("toast-text-content", id as usize))
        .test_support()
        .flex_shrink_0()
        .gap(px(4.))
        .child(
            Label::new(break_long_words(&toast.title, 28))
                .flex_shrink_0()
                .text_size(px(14.))
                .line_height(px(20.))
                .font_medium()
                .whitespace_normal(),
        )
        .when_some(toast.description.as_ref(), |item, description| {
            item.child(
                Label::new(break_long_words(description, 28))
                    .flex_shrink_0()
                    .text_size(px(14.))
                    .line_height(px(20.))
                    .text_color(cx.theme().muted_foreground)
                    .whitespace_normal(),
            )
        });
    let surface = h_flex()
        .id(("toast-item", id as usize))
        .test_support()
        .role(if toast.kind == ToastKind::Error {
            Role::Alert
        } else {
            Role::Status
        })
        .aria_label(toast.title.clone())
        .w_full()
        .flex_shrink_0()
        .items_center()
        .gap(px(12.))
        .p(px(16.))
        .rounded(px(16.))
        .bg(cx.theme().group_box)
        .border_1()
        .border_color(cx.theme().border)
        .shadow_lg()
        .overflow_hidden()
        .child(
            div()
                .id(("toast-icon", id as usize))
                .test_support()
                .flex_none()
                .size(px(16.))
                .text_color(color)
                .child(Icon::new(icon).size(px(16.))),
        )
        .child(
            v_flex()
                .id(("toast-body", id as usize))
                .test_support()
                .flex_1()
                .min_w_0()
                // Leave room for the card's 16px padding and 1px border.
                .max_h((max_toast_height - px(34.)).max(px(0.)))
                .occlude()
                .track_scroll(&body_scroll)
                .overflow_y_scroll()
                .map(|area| super::scroll_area::vertical(area, &body_scroll, window, cx))
                .child(text),
        )
        .child(
            Button::new(("toast-close", id as usize))
                .ghost()
                .small()
                .w(px(28.))
                .px(px(0.))
                .child(Icon::new(IconName::Close).size(px(16.)))
                .accessibility_label(crate::i18n::t!("common.actions.dismiss").into_owned())
                .tooltip(crate::i18n::t!("common.actions.dismiss").into_owned())
                .flex_shrink_0()
                .disabled(display.closing)
                .on_click(cx.listener(move |app, _, _, cx| app.dismiss_toast(id, cx))),
        )
        .when(display.closing, |surface| {
            surface
                .capture_any_mouse_down(|_, _, cx| cx.stop_propagation())
                .capture_any_mouse_up(|_, _, cx| cx.stop_propagation())
                .capture_key_down(|_, _, cx| cx.stop_propagation())
                .capture_key_up(|_, _, cx| cx.stop_propagation())
                .child(div().absolute().inset_0().occlude())
        });
    Zoom::new(("toast-motion", id as usize), surface)
        .translated_fraction(1.5 * (1. - display.progress))
        .into_any_element()
}
