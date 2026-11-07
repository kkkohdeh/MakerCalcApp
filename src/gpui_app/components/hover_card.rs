use gpui_kit::base::{
    Align, ElementExt as _, HoverCardState, POPUP_PRIORITY, Placement, Positioner,
    TestSupportExt as _,
};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Bounds, BoxShadow, Context, ElementId, Entity, FocusHandle,
    InteractiveElement as _, IntoElement, ParentElement as _, Pixels, RenderOnce,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, deferred, div, point, px,
};
use std::time::Duration;

use super::{alert_dialog::view_presence, button::Button, zoom::Zoom};

#[derive(IntoElement)]
pub(in crate::gpui_app) struct HoverCard {
    id: ElementId,
    trigger: Button,
    content: AnyElement,
    width: Pixels,
    placement: Placement,
    align: Align,
}
impl HoverCard {
    pub(in crate::gpui_app) fn new(
        id: impl Into<ElementId>,
        trigger: Button,
        content: impl IntoElement,
    ) -> Self {
        Self {
            id: id.into(),
            trigger,
            content: content.into_any_element(),
            width: px(256.),
            placement: Placement::Bottom,
            align: Align::Center,
        }
    }
    pub(in crate::gpui_app) fn width(mut self, width: Pixels) -> Self {
        self.width = width;
        self
    }
    pub(in crate::gpui_app) fn placement(mut self, placement: Placement, align: Align) -> Self {
        self.placement = placement;
        self.align = align;
        self
    }
}

struct Session {
    state: Entity<HoverCardState>,
    focus: FocusHandle,
    bounds: Bounds<Pixels>,
    _subscriptions: Vec<Subscription>,
}
impl Session {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state =
            cx.new(|_| HoverCardState::new(Duration::from_millis(600), Duration::from_millis(300)));
        let focus = cx.focus_handle();
        let watch = cx.observe(&state, |_, _, cx| cx.notify());
        let weak = state.downgrade();
        let window_id = window.window_handle();
        // Escape dismisses hovered previews even when focus is elsewhere.
        // Subscription lasts only while this help is mounted.
        let escape = cx.intercept_keystrokes(move |event, window, cx| {
            if window.window_handle() == window_id
                && event.keystroke.key == "escape"
                && let Some(state) = weak.upgrade()
                && state.read(cx).is_open()
            {
                state.update(cx, |state, cx| state.dismiss(window, cx));
                window.refresh();
                cx.stop_propagation();
            }
        });
        Self {
            state,
            focus,
            bounds: Bounds::default(),
            _subscriptions: vec![watch, escape],
        }
    }
}

impl RenderOnce for HoverCard {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let session = window.use_keyed_state((self.id.clone(), "session"), cx, Session::new);
        let state = session.read(cx).state.clone();
        let focus = session.read(cx).focus.clone();
        state.update(cx, |state, cx| {
            state.on_trigger_focus(focus.is_focused(window), window, cx)
        });
        let anchor = session.read(cx).bounds;
        let mut positioned_anchor = anchor;
        positioned_anchor.origin.x += if self.align == Align::End {
            px(-4.)
        } else {
            px(4.)
        };
        let open = state.read(cx).is_open();
        let presence = view_presence(self.id.clone(), open.then_some(()), window, cx);
        let mut root = div()
            .id(self.id.clone())
            .relative()
            .flex()
            .flex_shrink_0()
            .on_prepaint(move |bounds, window, cx| {
                if session.read(cx).bounds != bounds {
                    session.update(cx, |session, _| session.bounds = bounds);
                    window.request_animation_frame();
                }
            })
            .on_hover(window.listener_for(&state, |state, hovered, window, cx| {
                state.on_trigger_hover(*hovered, window, cx);
            }))
            .child(self.trigger.track_focus(&focus));
        if let Some(presence) = presence {
            let alpha = presence.alpha;
            let surface = div()
                .id((self.id.clone(), "surface"))
                .test_support()
                .relative()
                .occlude()
                .w(self.width)
                .p(px(10.))
                .rounded(px(10.))
                .bg(cx.theme().popover)
                .text_color(cx.theme().popover_foreground)
                .text_size(px(14.))
                .line_height(px(20.))
                .whitespace_normal()
                .opacity(alpha)
                .shadow(vec![
                    BoxShadow {
                        inset: false,
                        color: cx.theme().border.opacity(alpha * alpha),
                        offset: point(px(0.), px(0.)),
                        blur_radius: px(0.),
                        spread_radius: px(1.),
                    },
                    BoxShadow {
                        inset: false,
                        color: gpui_kit::black().opacity(0.1 * alpha * alpha),
                        offset: point(px(0.), px(4.)),
                        blur_radius: px(6.),
                        spread_radius: px(-1.),
                    },
                ])
                .when(!presence.closing, |surface| {
                    surface.on_hover(window.listener_for(&state, |state, hovered, window, cx| {
                        state.on_content_hover(*hovered, window, cx);
                    }))
                })
                .child(self.content);
            root = root.child(
                deferred(
                    Positioner::side(positioned_anchor)
                        .placement(self.placement)
                        .align(self.align)
                        .offset(px(4.))
                        .margin(px(8.))
                        .occlude()
                        .child(
                            Zoom::new((self.id, "zoom"), surface)
                                .factor(0.95 + 0.05 * alpha)
                                .anchored(anchor, px(8. * (1. - alpha))),
                        ),
                )
                .with_priority(POPUP_PRIORITY),
            );
        }
        root
    }
}
