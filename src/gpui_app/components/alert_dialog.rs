use std::time::{Duration, Instant};

use super::button::Button;

use gpui_kit::base::motion::Easing;
use gpui_kit::base::{StyledExt as _, TestSupportExt as _};
use gpui_kit::component::{ActiveTheme as _, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, BoxShadow, Div, FocusHandle, Hsla, InteractiveElement as _, IntoElement,
    ParentElement, RenderOnce, Role, ScrollHandle, Stateful, StatefulInteractiveElement as _,
    StyleRefinement, Styled, Window, canvas, div, fill, point, px,
};

use super::zoom::Zoom;

const MOTION: Duration = Duration::from_millis(100);

#[derive(Clone)]
struct PresenceState<T> {
    value: Option<T>,
    open: bool,
    from: f32,
    target: f32,
    changed: Instant,
}

impl<T> PresenceState<T> {
    fn alpha(&self, now: Instant, reduced: bool) -> f32 {
        if reduced {
            return self.target;
        }
        let elapsed = now.saturating_duration_since(self.changed).as_secs_f32();
        self.from + (self.target - self.from) * Easing::Ease.sample(elapsed / MOTION.as_secs_f32())
    }
}

pub(in crate::gpui_app) struct ViewPresence<T> {
    pub(in crate::gpui_app) value: T,
    pub(in crate::gpui_app) alpha: f32,
    pub(in crate::gpui_app) closing: bool,
}

/// Retains only a small display value for the 100 ms exit, not an executable
/// intention. Callers clear real pending actions immediately and disable the
/// closing view. No printer state, credentials or file contents are retained.
pub(in crate::gpui_app) fn view_presence<T: Clone + PartialEq + 'static>(
    id: impl Into<gpui_kit::ElementId>,
    incoming: Option<T>,
    window: &mut Window,
    cx: &mut App,
) -> Option<ViewPresence<T>> {
    let now = cx.background_executor().now();
    let state = window.use_keyed_state((id.into(), "presence"), cx, |_, _| PresenceState {
        value: None::<T>,
        open: false,
        from: 0.,
        target: 0.,
        changed: now,
    });
    let mut presence = state.read(cx).clone();
    let mut changed = false;
    if incoming.is_some() != presence.open {
        presence.from = presence.alpha(now, cx.reduce_motion());
        presence.open = incoming.is_some();
        presence.target = if presence.open { 1. } else { 0. };
        presence.changed = now;
        changed = true;
    }
    if let Some(value) = incoming
        && presence.value.as_ref() != Some(&value)
    {
        presence.value = Some(value);
        changed = true;
    }
    let alpha = presence.alpha(now, cx.reduce_motion());
    if !presence.open && alpha <= 0. && presence.value.is_some() {
        presence.value = None;
        changed = true;
    }
    if changed {
        state.update(cx, |state, _| *state = presence.clone());
    }
    let value = presence.value?;
    if (alpha - presence.target).abs() > f32::EPSILON {
        window.request_animation_frame();
    }
    Some(ViewPresence {
        value,
        alpha,
        closing: !presence.open,
    })
}

struct ReturnFocus {
    open: bool,
    previous: Option<FocusHandle>,
}

pub(in crate::gpui_app) fn sync_focus(open: bool, window: &mut Window, cx: &mut App) {
    let state = window.use_keyed_state("confirmation-return-focus", cx, |_, _| ReturnFocus {
        open: false,
        previous: None,
    });
    if state.read(cx).open == open {
        return;
    }
    if open {
        let previous = window.focused(cx);
        state.update(cx, |state, _| {
            state.open = true;
            state.previous = previous;
        });
    } else {
        let previous = state.update(cx, |state, _| {
            state.open = false;
            state.previous.take()
        });
        if let Some(previous) = previous {
            previous.focus(window, cx);
        }
    }
}

pub(in crate::gpui_app) fn initial_cancel_focus(
    id: &'static str,
    scope: &FocusHandle,
    closing: bool,
    window: &mut Window,
    cx: &mut App,
) -> FocusHandle {
    let cancel = window
        .use_keyed_state((id, 1usize), cx, |_, cx| cx.focus_handle())
        .read(cx)
        .clone();
    if !scope.contains_focused(window, cx) && !cancel.is_focused(window) {
        if closing {
            scope.focus(window, cx);
        } else {
            cancel.focus(window, cx);
        }
    }
    cancel
}

pub(in crate::gpui_app) fn backdrop(id: &'static str, overlay: Hsla, alpha: f32) -> Stateful<Div> {
    div()
        .id(id)
        .absolute()
        .inset_0()
        .occlude()
        .flex()
        .items_center()
        .justify_center()
        .p(px(24.))
        .child(
            div()
                .id(format!("modal-blur-{id}"))
                .test_support()
                .absolute()
                .inset_0()
                .child(
                    canvas(
                        |_, _, _| (),
                        move |bounds, _, window, _| {
                            #[cfg(target_os = "windows")]
                            window.paint_backdrop_blur(bounds, px(4. * alpha));
                            window.paint_quad(fill(bounds, overlay.opacity(alpha)));
                        },
                    )
                    .size_full(),
                ),
        )
}

pub(in crate::gpui_app) fn cancel_button(
    id: &'static str,
    focus: &FocusHandle,
    disabled: bool,
    label: impl Into<gpui_kit::SharedString>,
    _cx: &App,
) -> Button {
    Button::new(id)
        .track_focus(focus)
        .disabled(disabled)
        .outline()
        .label(label)
}

#[derive(IntoElement)]
pub(in crate::gpui_app) struct AlertDialog {
    id: &'static str,
    style: StyleRefinement,
    title: String,
    description: Option<String>,
    children: Vec<AnyElement>,
    footer: Option<AnyElement>,
    alpha: f32,
}

impl AlertDialog {
    pub(in crate::gpui_app) fn new(id: &'static str, alpha: f32) -> Self {
        Self {
            id,
            style: Default::default(),
            title: String::new(),
            description: None,
            children: Vec::new(),
            footer: None,
            alpha,
        }
    }
    pub(in crate::gpui_app) fn title(mut self, title: impl AsRef<str>) -> Self {
        self.title = title.as_ref().to_owned();
        self
    }
    pub(in crate::gpui_app) fn description(mut self, description: impl AsRef<str>) -> Self {
        self.description = Some(description.as_ref().to_owned());
        self
    }
    pub(in crate::gpui_app) fn footer(mut self, footer: impl IntoElement) -> Self {
        self.footer = Some(footer.into_any_element());
        self
    }
}

impl Styled for AlertDialog {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}
impl ParentElement for AlertDialog {
    fn extend(&mut self, children: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(children);
    }
}

impl RenderOnce for AlertDialog {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let compact = window.viewport_size().width < px(640.);
        let surface = v_flex()
            .id(self.id)
            .test_support()
            .role(Role::AlertDialog)
            .aria_label(self.title.clone())
            .when_some(self.description.clone(), |view, description| {
                view.aria_description(description)
            })
            .w(px(if compact { 320. } else { 384. }))
            .max_w((window.viewport_size().width - px(48.)).max(px(1.)))
            .max_h((window.viewport_size().height - px(48.)).max(px(1.)))
            .min_w_0()
            .min_h_0()
            .flex_shrink_0()
            .p(px(16.))
            .gap(px(16.))
            .rounded(px(14.))
            .bg(cx.theme().popover)
            .opacity(self.alpha)
            .text_color(cx.theme().popover_foreground)
            .shadow(vec![BoxShadow {
                inset: false,
                color: cx.theme().border,
                offset: point(px(0.), px(0.)),
                blur_radius: px(0.),
                spread_radius: px(1.),
            }])
            .child(
                v_flex()
                    .w_full()
                    .min_w_0()
                    .flex_shrink_0()
                    .gap(px(6.))
                    .when(compact, |header| header.text_center().items_center())
                    .child(
                        div()
                            .id((self.id, 0usize))
                            .test_support()
                            .role(Role::Heading)
                            .aria_level(2)
                            .w_full()
                            .text_size(px(16.))
                            .line_height(px(24.))
                            .font_medium()
                            .whitespace_normal()
                            .child(self.title),
                    )
                    .when_some(self.description, |header, description| {
                        header.child(
                            div()
                                .w_full()
                                .text_size(px(14.))
                                .line_height(px(20.))
                                .text_color(cx.theme().muted_foreground)
                                .whitespace_normal()
                                .child(description),
                        )
                    }),
            )
            .when(!self.children.is_empty(), |surface| {
                let scroll = window
                    .use_keyed_state(
                        (gpui_kit::ElementId::from(self.id), "body-scroll"),
                        cx,
                        |_, _| ScrollHandle::default(),
                    )
                    .read(cx)
                    .clone();
                surface.child(
                    v_flex()
                        .id((self.id, 1usize))
                        .min_w_0()
                        .min_h_0()
                        .gap(px(16.))
                        .overflow_y_scroll()
                        .track_scroll(&scroll)
                        .map(|area| super::scroll_area::vertical(area, &scroll, window, cx))
                        .children(self.children),
                )
            })
            .children(self.footer)
            .refine_style(&self.style);
        Zoom::new((self.id, 2usize), surface).factor(0.95 + 0.05 * self.alpha)
    }
}

#[derive(IntoElement, Default)]
pub(in crate::gpui_app) struct DialogFooter {
    children: Vec<AnyElement>,
}
impl DialogFooter {
    pub(in crate::gpui_app) fn new() -> Self {
        Self::default()
    }
}
impl ParentElement for DialogFooter {
    fn extend(&mut self, children: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(children);
    }
}
impl RenderOnce for DialogFooter {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        div()
            .id("alert-dialog-footer")
            .test_support()
            .flex()
            .flex_shrink_0()
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
            .children(self.children)
    }
}
