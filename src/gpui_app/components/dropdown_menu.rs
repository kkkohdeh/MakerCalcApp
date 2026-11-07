use std::rc::Rc;

use gpui_kit::base::{
    Align, ElementExt as _, POPUP_PRIORITY, Placement, PopoverState, Positioner, Selectable as _,
    StyledExt as _, TestSupportExt as _,
};
use gpui_kit::component::menu::{PopupMenu, PopupMenuItemDisplay, PopupMenuPresentation};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Anchor, App, Bounds, BoxShadow, Context, DismissEvent, ElementId, Entity, Focusable as _,
    InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce, Role,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, deferred, div, point, px,
};

use super::{
    alert_dialog::{ViewPresence, view_presence},
    button::Button,
    tooltip::TooltipExt as _,
    zoom::Zoom,
};

type Builder = Rc<dyn Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu>;

impl Button {
    pub(in crate::gpui_app) fn dropdown_menu(
        self,
        builder: impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static,
    ) -> DropdownMenu {
        DropdownMenu {
            trigger: self,
            builder: Rc::new(builder),
            value: None,
        }
    }

    pub(in crate::gpui_app) fn select_menu(
        self,
        value: usize,
        builder: impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static,
    ) -> DropdownMenu {
        let mut menu = self
            .role(Role::ComboBox)
            .font_normal()
            .text_size(px(14.))
            .line_height(px(20.))
            .pl(px(10.))
            .pr(px(8.))
            .gap(px(6.))
            .dropdown_menu(builder);
        menu.value = Some(value);
        menu
    }
}

#[derive(IntoElement)]
pub(in crate::gpui_app) struct DropdownMenu {
    trigger: Button,
    builder: Builder,
    value: Option<usize>,
}

#[derive(Default)]
struct MenuState {
    menu: Option<Entity<PopupMenu>>,
    subscription: Option<Subscription>,
    initial_last: Option<bool>,
    aligned: bool,
    typeahead: Option<Entity<PopupMenu>>,
}

fn presentation(
    alpha: f32,
    interactive: bool,
    width: gpui_kit::Pixels,
    listbox: bool,
    available_width: gpui_kit::Pixels,
    available_height: gpui_kit::Pixels,
    cx: &App,
) -> PopupMenuPresentation {
    let mut surface = div().rounded(px(10.)).opacity(alpha).shadow(vec![
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
    ]);
    let max_width = px(500.).max(width).min(available_width).max(px(0.));
    let width = width
        .max(px(if listbox { 144. } else { 128. }))
        .min(max_width);
    // Trigger sets minimum width; nowrap text supplies intrinsic width, bounded before ellipsis.
    // Keep decorative padding outside the tracked viewport to avoid extra scrolling.
    let mut items = div()
        .p_0()
        .gap_0()
        .min_w((width - px(8.)).max(px(0.)))
        .max_w((max_width - px(8.)).max(px(0.)))
        .max_h((available_height - px(8.)).max(px(0.)));
    surface = surface.p(px(4.)).max_w(max_width);
    let mut row = div()
        .h(px(28.))
        .flex_shrink_0()
        .min_w_0()
        .px(px(6.))
        .py(px(4.))
        .gap(px(6.))
        .rounded(px(8.))
        .text_size(px(14.))
        .line_height(px(20.));
    PopupMenuPresentation {
        surface: surface.style().clone(),
        items: items.style().clone(),
        row: row.style().clone(),
        interactive,
        radio_items: !listbox,
        listbox,
        render_item: Rc::new(|item: PopupMenuItemDisplay, _, _| {
            let full_label = item.label.clone();
            div()
                .id(("dropdown-item-content", item.index))
                .test_support()
                .relative()
                .w_full()
                .min_w_0()
                .pr(px(26.))
                .h(px(20.))
                .truncate()
                .child(item.label)
                .app_tooltip(move |window, cx| Tooltip::new(full_label.clone()).build(window, cx))
                .when(item.checked, |this| {
                    this.child(
                        div()
                            .id(("dropdown-check", item.index))
                            .test_support()
                            .absolute()
                            .right(px(2.))
                            .top(px(2.))
                            .size(px(16.))
                            .child(Icon::new(IconName::Check).with_size(px(16.))),
                    )
                })
                .into_any_element()
        }),
    }
}

impl RenderOnce for DropdownMenu {
    fn render(mut self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        if self.value.is_some() {
            self.trigger = self.trigger.child(
                Icon::new(IconName::ChevronDown)
                    .size(px(16.))
                    .text_color(cx.theme().muted_foreground),
            );
        }
        let trigger_style = self.trigger.style().clone();
        // Copy only sizing; paint or padding would be duplicated, notably on month/year triggers.
        let layout = gpui_kit::StyleRefinement {
            size: trigger_style.size,
            min_size: trigger_style.min_size,
            max_size: trigger_style.max_size,
            flex_basis: trigger_style.flex_basis,
            flex_grow: trigger_style.flex_grow,
            flex_shrink: trigger_style.flex_shrink,
            ..Default::default()
        };
        let id: ElementId = format!("dropdown-menu:{}", self.trigger.id).into();
        let popup = window.use_keyed_state((id.clone(), "open"), cx, |_, cx| {
            PopoverState::new(false, cx)
        });
        let state = window.use_keyed_state((id.clone(), "menu"), cx, |_, _| MenuState::default());
        let focus = window
            .use_keyed_state((id.clone(), "trigger-focus"), cx, |_, cx| cx.focus_handle())
            .read(cx)
            .clone();
        let bounds = window.use_keyed_state((id.clone(), "anchor"), cx, |_, _| Bounds::default());
        let anchor = *bounds.read(cx);
        if self.trigger.disabled {
            popup.update(cx, |state, cx| state.dismiss(window, cx));
            state.update(cx, |state, _| state.typeahead = None);
        }
        let open = popup.read(cx).is_open();
        if open && state.read(cx).menu.is_none() {
            state.update(cx, |state, _| state.typeahead = None);
            let menu = PopupMenu::build(window, cx, |menu, window, cx| {
                (self.builder)(menu, window, cx)
            });
            let last = state.update(cx, |state, _| state.initial_last.take());
            if let Some(value) = self.value {
                menu.update(cx, |menu, cx| menu.highlight(value, cx));
            } else if let Some(last) = last {
                menu.update(cx, |menu, cx| menu.select_boundary(last, cx));
            }
            menu.focus_handle(cx).focus(window, cx);
            let close = popup.clone();
            let subscription =
                window.subscribe(&menu, cx, move |_, _: &DismissEvent, window, cx| {
                    close.update(cx, |state, cx| state.dismiss(window, cx));
                    window.refresh();
                });
            state.update(cx, |state, _| {
                state.menu = Some(menu);
                state.subscription = Some(subscription);
            });
        }
        if open && let (Some(value), Some(menu)) = (self.value, state.read(cx).menu.clone()) {
            let top = anchor.center().y - px(18. + 28. * value as f32);
            let bottom = top + px(8. + 28. * menu.read(cx).item_count() as f32);
            let aligned = top >= px(8.)
                && bottom <= window.viewport_size().height - px(8.)
                && anchor.top() >= px(20.)
                && anchor.bottom() <= window.viewport_size().height - px(20.);
            state.update(cx, |state, _| state.aligned = aligned);
        }
        let aligned = self.value.is_some() && state.read(cx).aligned;
        let incoming = open.then(|| state.read(cx).menu.clone()).flatten();
        let presence = if aligned {
            incoming.map(|value| ViewPresence {
                value,
                alpha: 1.,
                closing: false,
            })
        } else {
            view_presence(id.clone(), incoming, window, cx)
        };
        if !open {
            state.update(cx, |state, _| {
                state.menu = None;
                state.subscription = None;
            });
        }
        let toggle = popup.clone();
        let key_open = popup.clone();
        let key_state = state.clone();
        let key_focus = focus.clone();
        let key_builder = self.builder.clone();
        let value = self.value;
        let disabled = self.trigger.disabled;
        let selected = self.trigger.is_selected();
        let scope = popup.read(cx).focus_handle(cx);
        let mut root = div()
            .id(id.clone())
            .relative()
            .track_focus(&scope)
            .refine_style(&layout)
            .on_prepaint(move |current, window, cx| {
                if *bounds.read(cx) != current {
                    bounds.update(cx, |bounds, _| *bounds = current);
                    window.request_animation_frame();
                }
            })
            .child(
                self.trigger
                    .popup_trigger()
                    .track_focus(&focus)
                    .selected(selected || open)
                    .aria_expanded(open)
                    .on_key_down(move |event, window, cx| {
                        if disabled || !key_focus.is_focused(window) {
                            return;
                        }
                        let key = event.keystroke.key.as_str();
                        if !open
                            && let Some(value) = value
                            && key.chars().count() == 1
                            && !event.keystroke.modifiers.control
                            && !event.keystroke.modifiers.platform
                            && !event.keystroke.modifiers.alt
                        {
                            let menu = key_state.read(cx).typeahead.clone().unwrap_or_else(|| {
                                let menu = PopupMenu::build(window, cx, |menu, window, cx| {
                                    key_builder(menu, window, cx)
                                });
                                key_state
                                    .update(cx, |state, _| state.typeahead = Some(menu.clone()));
                                menu
                            });
                            menu.update(cx, |menu, cx| {
                                menu.highlight(value, cx);
                                menu.typeahead_confirm(key, window, cx);
                            });
                            window.refresh();
                            cx.stop_propagation();
                            return;
                        }
                        if matches!(key, "up" | "down") {
                            key_state.update(cx, |state, _| {
                                state.initial_last = Some(event.keystroke.key == "up")
                            });
                            key_open.update(cx, |state, cx| state.show(window, cx));
                            window.refresh();
                            cx.stop_propagation();
                        }
                    })
                    .on_click(move |_, window, cx| {
                        toggle.update(cx, |state, cx| state.toggle_open(window, cx));
                        window.refresh();
                    }),
            );
        if let Some(presence) = presence {
            let alpha = presence.alpha;
            let menu = presence.value;
            let available_height = if aligned {
                window.viewport_size().height - px(16.)
            } else {
                (anchor.top() - px(12.))
                    .max(window.viewport_size().height - anchor.bottom() - px(12.))
            };
            let appearance = presentation(
                alpha,
                !presence.closing,
                anchor.size.width,
                self.value.is_some(),
                window.viewport_size().width - px(16.),
                available_height,
                cx,
            );
            menu.update(cx, |menu, cx| menu.set_presentation(appearance, cx));
            let closing = presence.closing;
            let surface = div()
                .id((id.clone(), "surface"))
                .test_support()
                .relative()
                .occlude()
                .child(menu)
                .when(closing, |this| {
                    this.capture_key_down(|_, _, cx| cx.stop_propagation())
                        .capture_key_up(|_, _, cx| cx.stop_propagation())
                        .child(
                            div()
                                .id((id.clone(), "exit-shield"))
                                .absolute()
                                .inset_0()
                                .occlude()
                                .on_mouse_down(gpui_kit::MouseButton::Left, |_, _, cx| {
                                    cx.stop_propagation()
                                })
                                .on_mouse_up(gpui_kit::MouseButton::Left, |_, _, cx| {
                                    cx.stop_propagation()
                                }),
                        )
                });
            let positioner = if aligned {
                Positioner::corner(
                    Anchor::TopLeft,
                    point(
                        anchor.left() + px(1.),
                        anchor.center().y
                            - px(18.
                                + 28. * self.value.expect("an aligned Select has a value") as f32),
                    ),
                )
            } else {
                Positioner::side(anchor)
                    .placement(Placement::Bottom)
                    .align(Align::Start)
                    .offset(px(4.))
            };
            root = root.child(
                deferred(
                    positioner.margin(px(8.)).occlude().child(
                        Zoom::new((id, "zoom"), surface)
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
