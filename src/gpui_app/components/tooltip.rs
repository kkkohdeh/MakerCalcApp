//! One tooltip provider for hover and keyboard help; no action or data ownership.
use std::{rc::Rc, time::Duration};

use gpui_kit::base::motion::{Easing, Presence, Transition};
use gpui_kit::base::{Placement, TestSupportExt as _, TooltipRequest, TooltipTransition};
use gpui_kit::component::{ActiveTheme as _, Root};
use gpui_kit::{
    AnyElement, AnyView, App, Bounds, DispatchPhase, EntityId, FocusHandle,
    InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent,
    ParentElement, Pixels, StatefulInteractiveElement, Styled, Subscription, Window, canvas, div,
    point, px,
};

use super::zoom::Zoom;
type Builder = Rc<dyn Fn(&mut Window, &mut App) -> AnyView>;

#[derive(Default)]
struct Trigger {
    active: bool,
    hovered: bool,
    focused: bool,
    bounds: Bounds<Pixels>,
    suppressed: bool,
    escape: Option<Subscription>,
}

fn show(
    owner: EntityId,
    bounds: Bounds<Pixels>,
    build: Builder,
    window: &mut Window,
    cx: &mut App,
) {
    if let Some(overlay) = Root::tooltip_overlay(window, cx) {
        overlay.update(cx, |overlay, cx| {
            overlay.presentation(Duration::ZERO, Duration::from_millis(150), render);
            overlay.request_show(
                TooltipRequest::new(bounds, move |window, cx| build(window, cx))
                    .placement(Placement::Top)
                    .offset(px(4.))
                    .owner(owner),
                window,
                cx,
            );
        });
    }
}

fn hide(owner: EntityId, window: &mut Window, cx: &mut App) {
    if let Some(overlay) = Root::tooltip_overlay(window, cx) {
        overlay.update(cx, |overlay, cx| {
            if overlay.owner() == Some(owner) {
                overlay.hide(cx);
            }
        });
    }
}

fn request_hide(owner: EntityId, window: &mut Window, cx: &mut App) {
    if let Some(overlay) = Root::tooltip_overlay(window, cx) {
        overlay.update(cx, |overlay, cx| {
            if overlay.owner() == Some(owner) {
                overlay.request_hide(window, cx);
            }
        });
    }
}

pub(in crate::gpui_app) trait TooltipExt:
    ParentElement + Styled + StatefulInteractiveElement + Sized
{
    fn app_tooltip(self, build: impl Fn(&mut Window, &mut App) -> AnyView + 'static) -> Self {
        self.app_tooltip_with_focus(None, build)
    }
    fn app_tooltip_with_focus(
        self,
        focus: Option<FocusHandle>,
        build: impl Fn(&mut Window, &mut App) -> AnyView + 'static,
    ) -> Self {
        let build = Rc::new(build);
        let paint_build = build.clone();
        self.child(
            canvas(
                move |bounds, window, cx| {
                    let state =
                        window.use_keyed_state("tooltip-trigger", cx, |_, _| Trigger::default());
                    let focused = focus
                        .as_ref()
                        .is_some_and(|focus| focus.contains_focused(window, cx))
                        && window.last_input_was_keyboard();
                    // Scrolling can move a hovered row without another mouse event.
                    let active = focused
                        || (state.read(cx).hovered
                            && bounds.contains(&window.mouse_position())
                            && !window.last_input_was_keyboard());
                    let moved = state.read(cx).bounds != bounds;
                    state.update(cx, |state, _| {
                        state.bounds = bounds;
                        state.focused = focused;
                    });
                    if active != state.read(cx).active {
                        state.update(cx, |state, _| {
                            state.active = active;
                            state.suppressed = false;
                        });
                        if active {
                            show(state.entity_id(), bounds, build.clone(), window, cx);
                        } else {
                            request_hide(state.entity_id(), window, cx);
                        }
                    }
                    if active && moved && !state.read(cx).suppressed {
                        show(state.entity_id(), bounds, build.clone(), window, cx);
                    }
                    if state.read(cx).escape.is_none() {
                        let overlay =
                            Root::tooltip_overlay(window, cx).map(|overlay| overlay.downgrade());
                        let owner = state.entity_id();
                        cx.observe_release(&state, move |_, cx| {
                            if let Some(overlay) =
                                overlay.as_ref().and_then(|overlay| overlay.upgrade())
                            {
                                overlay.update(cx, |overlay, cx| {
                                    if overlay.owner() == Some(owner) {
                                        overlay.hide(cx);
                                    }
                                });
                            }
                        })
                        .detach();
                        let weak = state.downgrade();
                        let window_id = window.window_handle().window_id();
                        let subscription = cx.intercept_keystrokes(move |event, window, cx| {
                            if window.window_handle().window_id() != window_id
                                || event.keystroke.key != "escape"
                            {
                                return;
                            }
                            let Some(state) = weak.upgrade() else {
                                return;
                            };
                            if !state.read(cx).suppressed
                                && Root::tooltip_overlay(window, cx).is_some_and(|overlay| {
                                    overlay.read(cx).owner() == Some(state.entity_id())
                                })
                            {
                                hide(state.entity_id(), window, cx);
                                state.update(cx, |state, _| state.suppressed = true);
                                cx.stop_propagation();
                            }
                        });
                        state.update(cx, |state, _| state.escape = Some(subscription));
                    }
                    let hitbox = window.insert_hitbox(bounds, gpui_kit::HitboxBehavior::Normal);
                    (state, bounds, hitbox)
                },
                move |_, (state, bounds, hitbox), window, _| {
                    let state = state.clone();
                    let build = paint_build.clone();
                    let hover_state = state.clone();
                    let exit_state = state.clone();
                    let hitbox = hitbox.clone();
                    window.on_mouse_event(move |_: &MouseMoveEvent, phase, window, cx| {
                        if phase != DispatchPhase::Bubble {
                            return;
                        }
                        let hovered = hitbox.is_hovered(window);
                        let active = hovered || hover_state.read(cx).focused;
                        hover_state.update(cx, |state, _| state.hovered = hovered);
                        if active != hover_state.read(cx).active {
                            hover_state.update(cx, |state, _| {
                                state.active = active;
                                state.suppressed = false;
                            });
                            if active {
                                show(hover_state.entity_id(), bounds, build.clone(), window, cx);
                            } else {
                                request_hide(hover_state.entity_id(), window, cx);
                            }
                        }
                    });
                    window.on_mouse_event(move |event: &MouseDownEvent, _, window, cx| {
                        if event.button == MouseButton::Left && state.read(cx).active {
                            hide(state.entity_id(), window, cx);
                            state.update(cx, |state, _| state.suppressed = true);
                        }
                    });
                    window.on_mouse_event(
                        move |_: &gpui_kit::MouseExitEvent, phase, window, cx| {
                            if phase == DispatchPhase::Bubble {
                                exit_state.update(cx, |state, _| {
                                    state.hovered = false;
                                    state.active = state.focused;
                                });
                                if !exit_state.read(cx).active {
                                    request_hide(exit_state.entity_id(), window, cx);
                                }
                            }
                        },
                    );
                },
            )
            .absolute()
            .inset_0(),
        )
    }
}
impl<E: ParentElement + Styled + StatefulInteractiveElement> TooltipExt for E {}

fn render(
    view: AnyView,
    phase: TooltipTransition,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let (epoch, trigger, open) = match phase {
        TooltipTransition::Enter { epoch, trigger } => (epoch, trigger, true),
        TooltipTransition::Switch { epoch, current, .. } => (epoch, current, true),
        TooltipTransition::Exit { epoch, trigger } => (epoch, trigger, false),
    };
    let sample = Presence::new((("app-tooltip", epoch), "presence"), open)
        .transition(Transition::new(Duration::from_millis(150)).easing(Easing::Ease))
        .sample(window, cx);
    if !sample.should_render() {
        return div().into_any_element();
    }
    let surface = div()
        .id("app-tooltip")
        .test_support()
        .relative()
        .opacity(sample.progress)
        .on_hover(move |hovered, window, cx| {
            if let Some(overlay) = Root::tooltip_overlay(window, cx) {
                overlay.update(cx, |overlay, cx| {
                    if *hovered {
                        overlay.keep_open(cx);
                    } else {
                        overlay.hide(cx);
                    }
                });
            }
        })
        .child(view)
        .child(
            canvas(
                |_, _, _| {},
                move |bounds, _, window, cx| {
                    let above = bounds.center().y < trigger.center().y;
                    let x = trigger
                        .center()
                        .x
                        .clamp(bounds.left() + px(10.), bounds.right() - px(10.));
                    let y = if above { bounds.bottom() } else { bounds.top() };
                    let direction = if above { 1. } else { -1. };
                    let mut path = gpui_kit::PathBuilder::fill();
                    path.move_to(point(x - px(5.), y));
                    path.line_to(point(x, y + px(5. * direction)));
                    path.line_to(point(x + px(5.), y));
                    path.close();
                    if let Ok(path) = path.build() {
                        window.paint_path(path, cx.theme().popover);
                    }
                },
            )
            .absolute()
            .inset_0(),
        );
    Zoom::new("tooltip-motion", surface)
        .factor(0.95 + 0.05 * sample.progress)
        .anchored(trigger, px(8. * (1. - sample.progress)))
        .into_any_element()
}
