use gpui_kit::base::{StyledExt as _, TestSupportExt as _};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ElementId, FocusHandle, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement, RenderOnce, Role, SharedString, StatefulInteractiveElement as _,
    StyleRefinement, Styled, Window, div, px,
};

#[derive(IntoElement)]
pub(in crate::gpui_app) struct Field {
    id: ElementId,
    label: Option<SharedString>,
    description: Option<SharedString>,
    focus: Option<FocusHandle>,
    disabled: bool,
    children: Vec<AnyElement>,
    style: StyleRefinement,
}
impl Field {
    pub(in crate::gpui_app) fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            label: None,
            description: None,
            focus: None,
            disabled: false,
            children: Vec::new(),
            style: StyleRefinement::default(),
        }
    }
    pub(in crate::gpui_app) fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }
    pub(in crate::gpui_app) fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.description = Some(description.into());
        self
    }
    pub(in crate::gpui_app) fn focus(mut self, focus: FocusHandle) -> Self {
        self.focus = Some(focus);
        self
    }
    pub(in crate::gpui_app) fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}
impl Styled for Field {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}
impl ParentElement for Field {
    fn extend(&mut self, children: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(children);
    }
}

pub(in crate::gpui_app) fn description(cx: &App) -> gpui_kit::Div {
    div()
        .w_full()
        .text_size(px(14.))
        .line_height(px(21.))
        .whitespace_normal()
        .font_normal()
        .text_color(cx.theme().muted_foreground)
}
impl RenderOnce for Field {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let id = self.id;
        let disabled = self.disabled;
        div()
            .id(id.clone())
            .test_support()
            .role(Role::Group)
            .when_some(self.label.clone(), |field, label| field.aria_label(label))
            .flex()
            .flex_col()
            .flex_none()
            .w_full()
            .min_w_0()
            .gap(px(8.))
            .text_color(cx.theme().foreground)
            .refine_style(&self.style)
            .when_some(self.label, |field, label| {
                field.child(
                    div()
                        .id((id.clone(), "label"))
                        .test_support()
                        .w_full()
                        .text_size(px(14.))
                        .line_height(px(19.25))
                        .font_medium()
                        .whitespace_normal()
                        .when(disabled, |label| label.opacity(0.5))
                        .when_some(self.focus, |label, focus| {
                            label.when(!disabled, |label| {
                                label.on_mouse_down(MouseButton::Left, move |_, window, cx| {
                                    focus.focus(window, cx);
                                    cx.stop_propagation();
                                })
                            })
                        })
                        .child(label),
                )
            })
            .child(
                div()
                    .id((id.clone(), "content"))
                    .test_support()
                    .w_full()
                    .min_w_0()
                    .children(self.children),
            )
            .when_some(self.description, |field, text| {
                field.child(
                    description(cx)
                        .id((id, "description"))
                        .test_support()
                        .child(text),
                )
            })
    }
}
