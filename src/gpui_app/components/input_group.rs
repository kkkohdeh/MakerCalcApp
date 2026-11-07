//! Inline Input Groups share the Input frame and editor.
use gpui_kit::base::{StyledExt as _, TestSupportExt as _};
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, CursorStyle, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, RenderOnce, Role, StatefulInteractiveElement as _, StyleRefinement, Styled,
    Window, div, px,
};

use super::{button::Button, input::Input};
use crate::i18n::t;

#[derive(IntoElement)]
pub(in crate::gpui_app) struct InputGroup {
    input: Input,
    prefix: Option<Icon>,
    suffix: Option<AnyElement>,
    cleanable: bool,
}
impl InputGroup {
    pub(super) fn new(input: Input) -> Self {
        Self {
            input,
            prefix: None,
            suffix: None,
            cleanable: false,
        }
    }
    pub(super) fn prefix(mut self, prefix: Icon) -> Self {
        self.prefix = Some(prefix);
        self
    }
    pub(super) fn suffix(mut self, suffix: impl IntoElement) -> Self {
        self.suffix = Some(suffix.into_any_element());
        self
    }
    pub(in crate::gpui_app) fn cleanable(mut self, cleanable: bool) -> Self {
        self.cleanable = cleanable;
        self
    }
    pub(in crate::gpui_app) fn disabled(mut self, disabled: bool) -> Self {
        self.input.disabled = disabled;
        self
    }
}
impl Styled for InputGroup {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.input.style
    }
}

/// Help and clear buttons use the reference's icon-xs variant.
pub(in crate::gpui_app) fn icon_button(button: Button) -> Button {
    button.size(px(24.)).rounded(px(7.))
}

impl RenderOnce for InputGroup {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = self.input.state.clone();
        let id = self.input.id.clone();
        let disabled = self.input.disabled;
        let presentation = state.read(cx).presentation();
        let clear = self.cleanable
            && !disabled
            && !presentation.is_loading()
            && !presentation.is_readonly()
            && !state.read(cx).value().is_empty();
        let has_suffix = self.suffix.is_some() || clear;
        let compact = window.viewport_size().width < px(768.);
        let mut control_style = div()
            .flex_1()
            .min_w_0()
            .h(px(32.))
            .px(px(10.))
            .py(px(4.))
            .rounded_none()
            .border_0()
            .bg(gpui_kit::transparent_black())
            .text_size(px(if compact { 16. } else { 14. }))
            .line_height(px(if compact { 24. } else { 20. }))
            .when(self.prefix.is_some(), |control| control.pl(px(6.)))
            .when(has_suffix, |control| control.pr(px(6.)));
        let addon_focus = presentation.focus_handle().clone();
        let mut root = self
            .input
            .frame(window, cx)
            .id(id.clone())
            .test_support()
            .role(Role::Group)
            .when_some(self.input.label, |root, label| root.aria_label(label))
            .relative()
            .flex()
            .items_center()
            .px_0()
            .py_0()
            .refine_style(&self.input.style)
            .when(disabled, |root| {
                root.capture_any_mouse_down(|_, window, cx| {
                    window.prevent_default();
                    cx.stop_propagation();
                })
            })
            .when(!disabled, |root| {
                root.on_mouse_down(MouseButton::Left, move |_, window, cx| {
                    if !window.default_prevented() {
                        addon_focus.focus(window, cx);
                        window.prevent_default();
                    }
                })
            });
        // The prefix is visual and not a tab stop; interactive suffixes follow the editor in focus order.
        if let Some(prefix) = self.prefix {
            root = root.child(
                div()
                    .id((id.clone(), "prefix"))
                    .test_support()
                    .role(Role::Group)
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .gap(px(8.))
                    .py(px(6.))
                    .pl(px(8.))
                    .text_color(cx.theme().muted_foreground)
                    .opacity(if disabled { 0.5 } else { 1. })
                    .cursor(CursorStyle::IBeam)
                    .child(prefix.with_size(px(16.))),
            );
        }
        root = root.child(
            self.input
                .editor
                .id((id.clone(), "control"))
                .appearance(false)
                .focus_bordered(false)
                .disabled(disabled)
                .refine_style(control_style.style()),
        );
        if has_suffix {
            let mut addon = div()
                .id((id.clone(), "suffix"))
                .test_support()
                .role(Role::Group)
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .gap(px(8.))
                .py(px(6.))
                .pr(px(8.))
                .text_color(cx.theme().muted_foreground)
                .opacity(if disabled { 0.5 } else { 1. })
                .cursor(CursorStyle::IBeam)
                .mr(px(-4.8));
            if clear {
                addon = addon.child(icon_button(
                    Button::new((id, "clear"))
                        .ghost()
                        .icon(IconName::Close)
                        .tab_stop(false)
                        .accessibility_label(t!("common.actions.clear").into_owned())
                        .on_click(move |_, window, cx| {
                            state.update(cx, |state, cx| {
                                state.clean(window, cx);
                                state.focus(window, cx);
                            });
                            cx.stop_propagation();
                        }),
                ));
            }
            root = root.child(addon.children(self.suffix));
        }
        root
    }
}
