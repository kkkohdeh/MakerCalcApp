use std::time::Duration;

use gpui_kit::base::StyledExt as _;
use gpui_kit::base::motion::{Easing, Interpolate, Transition, transition};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::Icon;
use gpui_kit::component::input::{Input as EditorInput, InputState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, CursorStyle, Div, ElementId, Entity, Focusable as _, Hsla, IntoElement, RenderOnce,
    SharedString, StyleRefinement, Styled, Window, div, px,
};

#[derive(Clone, Copy, PartialEq)]
struct Paint {
    background: Hsla,
    border: Hsla,
}
impl Interpolate for Paint {
    fn interpolate(&self, target: &Self, progress: f32) -> Self {
        Self {
            background: self.background.interpolate(&target.background, progress),
            border: self.border.interpolate(&target.border, progress),
        }
    }
}

#[derive(IntoElement)]
pub(in crate::gpui_app) struct Input {
    pub(super) id: ElementId,
    pub(super) state: Entity<InputState>,
    pub(super) editor: EditorInput,
    pub(super) disabled: bool,
    pub(super) label: Option<SharedString>,
    pub(super) style: StyleRefinement,
}
impl Input {
    pub(in crate::gpui_app) fn new(state: &Entity<InputState>) -> Self {
        Self {
            id: ("input", state.entity_id()).into(),
            state: state.clone(),
            editor: EditorInput::new(state),
            disabled: false,
            label: None,
            style: StyleRefinement::default(),
        }
    }
    pub(in crate::gpui_app) fn id(mut self, id: impl Into<ElementId>) -> Self {
        self.id = id.into();
        self
    }
    pub(in crate::gpui_app) fn aria_label(mut self, label: impl Into<SharedString>) -> Self {
        let label = label.into();
        self.label = Some(label.clone());
        self.editor = self.editor.aria_label(label);
        self
    }
    pub(in crate::gpui_app) fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
    pub(in crate::gpui_app) fn prefix(self, prefix: Icon) -> super::input_group::InputGroup {
        super::input_group::InputGroup::new(self).prefix(prefix)
    }
    pub(in crate::gpui_app) fn suffix(
        self,
        suffix: impl IntoElement,
    ) -> super::input_group::InputGroup {
        super::input_group::InputGroup::new(self).suffix(suffix)
    }
}
impl Styled for Input {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}
impl Input {
    pub(super) fn frame(&self, window: &mut Window, cx: &mut App) -> Div {
        // Text inputs match :focus-visible for pointer as well as keyboard focus.
        let focused = self.state.focus_handle(cx).is_focused(window) && !self.disabled;
        let ring = cx.theme().ring;
        let paint = transition(
            (self.id.clone(), "paint"),
            Paint {
                background: if self.disabled {
                    cx.theme().input.opacity(0.8)
                } else {
                    cx.theme().input_background()
                },
                border: if focused { ring } else { cx.theme().input },
            },
            Transition::new(Duration::from_millis(150)).easing(Easing::CubicBezier {
                x1: 0.4,
                y1: 0.,
                x2: 0.2,
                y2: 1.,
            }),
            window,
            cx,
        );
        let compact = window.viewport_size().width < px(768.);
        div()
            .w_full()
            .min_w_0()
            .h(px(32.))
            .px(px(10.))
            .py(px(4.))
            .rounded(px(10.))
            .border_1()
            .border_color(paint.border.opacity(if self.disabled { 0.5 } else { 1. }))
            .bg(paint
                .background
                .opacity(if self.disabled { 0.5 } else { 1. }))
            .text_size(px(if compact { 16. } else { 14. }))
            .line_height(px(if compact { 24. } else { 20. }))
            // The native editor and adornments already dim their own content.
            .when(self.disabled, |frame| {
                frame.cursor(CursorStyle::OperationNotAllowed)
            })
    }
}
impl RenderOnce for Input {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let mut frame = self.frame(window, cx);
        self.editor
            .id(self.id)
            .appearance(false)
            .focus_bordered(false)
            .disabled(self.disabled)
            .refine_style(frame.style())
            .refine_style(&self.style)
    }
}
