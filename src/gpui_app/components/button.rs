use super::tooltip::TooltipExt as _;
use std::time::Duration;

use gpui_kit::base::motion::{Easing, Interpolate, Transition, transition};
use gpui_kit::base::{Button as BaseButton, Selectable, StyledExt as _};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable, Size};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, BoxShadow, ClickEvent, ElementId, Entity, FocusHandle, Hsla,
    InteractiveElement, Interactivity, IntoElement, KeyDownEvent, KeyUpEvent, MouseButton,
    ParentElement, RenderOnce, Role, SharedString, StatefulInteractiveElement, StyleRefinement,
    Styled, Window, div, point, px,
};

#[derive(Clone, Copy, Default)]
enum Variant {
    #[default]
    Default,
    Primary,
    Secondary,
    Danger,
    Ghost,
}

#[derive(Clone, Copy, Default, PartialEq)]
struct InputState {
    hovered: bool,
    pressed: bool,
}

#[derive(Clone, Copy, PartialEq)]
struct Paint {
    background: Hsla,
    foreground: Hsla,
    border: Hsla,
    offset: f32,
    opacity: f32,
}

impl Interpolate for Paint {
    fn interpolate(&self, to: &Self, progress: f32) -> Self {
        Self {
            background: self.background.interpolate(&to.background, progress),
            foreground: self.foreground.interpolate(&to.foreground, progress),
            border: self.border.interpolate(&to.border, progress),
            offset: self.offset.interpolate(&to.offset, progress),
            opacity: self.opacity.interpolate(&to.opacity, progress),
        }
    }
}

#[derive(IntoElement)]
pub(in crate::gpui_app) struct Button {
    pub(super) id: ElementId,
    base: BaseButton,
    style: StyleRefinement,
    variant: Variant,
    outline: bool,
    size: Size,
    label: Option<SharedString>,
    accessibility_label: Option<SharedString>,
    icon: Option<Icon>,
    children: Vec<AnyElement>,
    pub(super) disabled: bool,
    tooltip: Option<SharedString>,
    pub(super) focus: Option<FocusHandle>,
    popup: bool,
    toggle: bool,
    selected: bool,
    role: Option<Role>,
}

impl Button {
    pub(in crate::gpui_app) fn new(id: impl Into<ElementId>) -> Self {
        let id = id.into();
        Self {
            base: BaseButton::new(id.clone()),
            id,
            style: Default::default(),
            variant: Variant::Default,
            outline: false,
            size: Size::Medium,
            label: None,
            accessibility_label: None,
            icon: None,
            children: Vec::new(),
            disabled: false,
            tooltip: None,
            focus: None,
            popup: false,
            toggle: false,
            selected: false,
            role: None,
        }
    }
    pub(in crate::gpui_app) fn tab_stop(mut self, enabled: bool) -> Self {
        self.base = self.base.tab_stop(enabled);
        self
    }
    pub(in crate::gpui_app) fn focus_handle(mut self, focus: &FocusHandle) -> Self {
        self.focus = Some(focus.clone());
        self
    }
    pub(in crate::gpui_app) fn primary(mut self) -> Self {
        self.variant = Variant::Primary;
        self
    }
    pub(in crate::gpui_app) fn secondary(mut self) -> Self {
        self.variant = Variant::Secondary;
        self
    }
    pub(in crate::gpui_app) fn danger(mut self) -> Self {
        self.variant = Variant::Danger;
        self
    }
    pub(in crate::gpui_app) fn ghost(mut self) -> Self {
        self.variant = Variant::Ghost;
        self
    }
    pub(in crate::gpui_app) fn outline(mut self) -> Self {
        self.outline = true;
        self
    }
    pub(in crate::gpui_app) fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }
    pub(in crate::gpui_app) fn icon(mut self, icon: impl Into<Icon>) -> Self {
        self.icon = Some(icon.into());
        self
    }
    pub(in crate::gpui_app) fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
    pub(in crate::gpui_app) fn tooltip(mut self, tooltip: impl Into<SharedString>) -> Self {
        self.tooltip = Some(tooltip.into());
        self
    }
    pub(in crate::gpui_app) fn accessibility_label(
        mut self,
        label: impl Into<SharedString>,
    ) -> Self {
        self.accessibility_label = Some(label.into());
        self
    }
    pub(in crate::gpui_app) fn toggled(mut self, toggled: impl Into<Option<bool>>) -> Self {
        let toggled = toggled.into();
        self.toggle = true;
        self.selected = toggled == Some(true);
        self.base = self.base.when_some(toggled, |base, toggled| {
            base.aria_toggled(if toggled {
                gpui_kit::accesskit::Toggled::True
            } else {
                gpui_kit::accesskit::Toggled::False
            })
        });
        self
    }
    pub(in crate::gpui_app) fn popup_trigger(mut self) -> Self {
        self.popup = true;
        self
    }
    pub(super) fn role(mut self, role: Role) -> Self {
        self.role = Some(role);
        self
    }
    pub(in crate::gpui_app) fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.base = self.base.on_click(handler);
        self
    }

    fn paint(&self, input: InputState, cx: &App) -> Paint {
        let theme = cx.theme();
        let hover = (input.hovered || self.selected) && !self.disabled;
        let pressed = input.pressed && !self.disabled;
        let (background, foreground) = match self.variant {
            Variant::Primary => (
                if pressed {
                    theme.button_primary_active
                } else if hover {
                    theme.button_primary_hover
                } else {
                    theme.button_primary
                },
                theme.button_primary_foreground,
            ),
            Variant::Danger => (
                if pressed {
                    theme.button_danger_active
                } else if hover {
                    theme.button_danger_hover
                } else {
                    theme.button_danger
                },
                theme.button_danger_foreground,
            ),
            Variant::Ghost => (
                if hover {
                    theme.muted
                } else {
                    theme.transparent
                },
                theme.foreground,
            ),
            Variant::Default | Variant::Secondary => (
                if pressed {
                    theme.button_active
                } else if hover {
                    theme.button_hover
                } else {
                    theme.button
                },
                theme.button_foreground,
            ),
        };
        let (background, foreground, border) = if self.outline {
            if matches!(self.variant, Variant::Danger) {
                (
                    theme.danger.opacity(if hover { 0.2 } else { 0.1 }),
                    theme.danger,
                    theme.danger,
                )
            } else {
                (
                    if hover {
                        theme.muted
                    } else {
                        theme.input_background()
                    },
                    theme.foreground,
                    theme.border,
                )
            }
        } else {
            (
                background,
                foreground,
                if matches!(self.variant, Variant::Default) {
                    theme.border
                } else {
                    theme.transparent
                },
            )
        };
        Paint {
            background,
            foreground,
            border,
            offset: if pressed && !self.popup && !self.toggle {
                1.
            } else {
                0.
            },
            opacity: if self.disabled { 0.5 } else { 1. },
        }
    }
}

impl Sizable for Button {
    fn with_size(mut self, size: impl Into<Size>) -> Self {
        self.size = size.into();
        self
    }
}
impl Selectable for Button {
    fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }
    fn is_selected(&self) -> bool {
        self.selected
    }
}
impl Styled for Button {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}
impl ParentElement for Button {
    fn extend(&mut self, children: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(children);
    }
}
impl InteractiveElement for Button {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.base.interactivity()
    }
    fn track_focus(mut self, focus: &FocusHandle) -> Self {
        self.focus = Some(focus.clone());
        self
    }
}
impl StatefulInteractiveElement for Button {}

fn update_input(state: &Entity<InputState>, cx: &mut App, update: impl FnOnce(&mut InputState)) {
    state.update(cx, |state, cx| {
        let before = *state;
        update(state);
        if *state != before {
            cx.notify();
        }
    });
}

impl RenderOnce for Button {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let input =
            window.use_keyed_state((self.id.clone(), "input"), cx, |_, _| InputState::default());
        let paint = transition(
            (self.id.clone(), "paint"),
            self.paint(*input.read(cx), cx),
            Transition::new(Duration::from_millis(150)).easing(Easing::CubicBezier {
                x1: 0.4,
                y1: 0.,
                x2: 0.2,
                y2: 1.,
            }),
            window,
            cx,
        );
        let focus = self.focus.unwrap_or_else(|| {
            window
                .use_keyed_state((self.id.clone(), "focus"), cx, |_, cx| cx.focus_handle())
                .read(cx)
                .clone()
        });
        let (height, font, line, icon_size, gap, padding, radius) = match self.size {
            Size::XSmall => (24., 12., 16., 12., 4., 8., 8.),
            Size::Small => (28., 12.8, 19.2, 14., 4., 10., 8.),
            Size::Medium => (32., 14., 20., 16., 6., 10., 10.),
            Size::Large => (36., 14., 20., 16., 6., 10., 10.),
            Size::Size(height) => (f32::from(height), 14., 20., 16., 6., 10., 10.),
        };
        let icon_only = self.icon.is_some() && self.label.is_none() && self.children.is_empty();
        let select = self.role == Some(Role::ComboBox);
        let label = self.label;
        let mut base = self
            .base
            .role(self.role.unwrap_or(Role::Button))
            .track_focus(&focus)
            .disabled(self.disabled)
            .relative()
            .flex_shrink_0()
            .h(px(height))
            .px(px(if icon_only { 0. } else { padding }))
            .when(icon_only, |base| base.w(px(height)))
            .when(self.icon.is_some() && !icon_only, |base| {
                base.pl(px(
                    if self.size == Size::Medium || self.size == Size::Large {
                        8.
                    } else {
                        6.
                    },
                ))
            })
            .gap(px(gap))
            .rounded(px(radius))
            .border_1()
            .border_color(paint.border)
            .bg(paint.background)
            .text_color(paint.foreground)
            .opacity(paint.opacity)
            .text_size(px(font))
            .line_height(px(line))
            .font_medium()
            .whitespace_nowrap()
            .top(px(paint.offset))
            .refine_style(&self.style)
            .when_some(
                self.accessibility_label.or_else(|| label.clone()),
                |base, label| base.accessibility_label(label),
            )
            .when_some(self.icon, |base, icon| {
                base.child(div().flex_none().child(icon.size(px(icon_size))))
            })
            .children(label)
            .children(self.children);
        if !self.disabled {
            let hover_input = input.clone();
            let down_input = input.clone();
            let up_input = input.clone();
            let out_input = input.clone();
            let key_input = input.clone();
            base = base
                .on_hover(move |hovered, _, cx| {
                    update_input(&hover_input, cx, |state| state.hovered = *hovered)
                })
                .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                    update_input(&down_input, cx, |state| state.pressed = true)
                })
                .on_mouse_up(MouseButton::Left, move |_, _, cx| {
                    update_input(&up_input, cx, |state| state.pressed = false)
                })
                .on_mouse_up_out(MouseButton::Left, move |_, _, cx| {
                    update_input(&out_input, cx, |state| state.pressed = false)
                })
                .on_key_down(move |event: &KeyDownEvent, _, cx| {
                    if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                        update_input(&key_input, cx, |state| state.pressed = true);
                    }
                })
                .on_key_up(move |event: &KeyUpEvent, _, cx| {
                    if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                        update_input(&input, cx, |state| state.pressed = false);
                    }
                });
        }
        let ring = cx.theme().ring;
        base.focus_visible(move |style| {
            style.border_color(ring).shadow(if select {
                vec![]
            } else {
                vec![BoxShadow {
                    inset: false,
                    color: ring.opacity(0.5),
                    offset: point(px(0.), px(0.)),
                    blur_radius: px(0.),
                    spread_radius: px(3.),
                }]
            })
        })
        .when_some(self.tooltip, |base, tooltip| {
            base.app_tooltip_with_focus(Some(focus.clone()), move |window, cx| {
                Tooltip::new(tooltip.clone()).build(window, cx)
            })
        })
    }
}
