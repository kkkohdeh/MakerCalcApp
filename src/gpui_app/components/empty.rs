use gpui_kit::base::{StyledExt as _, TestSupportExt as _};
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, ElementId, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce,
    SharedString, StatefulInteractiveElement as _, StyleRefinement, Styled, Window, div, px,
};

#[derive(IntoElement)]
pub(in crate::gpui_app) struct Empty {
    id: ElementId,
    message: SharedString,
    icon: Option<IconName>,
    style: StyleRefinement,
}
impl Empty {
    pub(in crate::gpui_app) fn new(
        id: impl Into<ElementId>,
        message: impl Into<SharedString>,
    ) -> Self {
        Self {
            id: id.into(),
            message: message.into(),
            icon: None,
            style: StyleRefinement::default(),
        }
    }
    pub(in crate::gpui_app) fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }
}
impl Styled for Empty {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}
impl RenderOnce for Empty {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let id = self.id;
        div()
            .id(id.clone())
            .test_support()
            .flex()
            .flex_1()
            .flex_col()
            .w_full()
            .min_w_0()
            .items_center()
            .justify_center()
            .gap(px(16.))
            .rounded(px(14.))
            .p(px(24.))
            .text_center()
            .whitespace_normal()
            .text_color(cx.theme().muted_foreground)
            .aria_label(self.message.clone())
            .refine_style(&self.style)
            .child(
                div()
                    .id((id.clone(), "header"))
                    .test_support()
                    .flex()
                    .flex_col()
                    .min_w_0()
                    .max_w(px(384.))
                    .items_center()
                    .gap(px(8.))
                    .when_some(self.icon, |header, icon| {
                        header.child(
                            div()
                                .id((id.clone(), "media"))
                                .test_support()
                                .flex()
                                .flex_none()
                                .items_center()
                                .justify_center()
                                .size(px(32.))
                                .mb(px(8.))
                                .rounded(px(10.))
                                .bg(cx.theme().muted)
                                .child(
                                    div()
                                        .id((id.clone(), "glyph"))
                                        .test_support()
                                        .size(px(16.))
                                        .child(Icon::new(icon).with_size(px(16.))),
                                ),
                        )
                    })
                    .child(
                        div()
                            .id((id, "description"))
                            .test_support()
                            .text_size(px(14.))
                            .line_height(px(22.75))
                            .min_h(px(22.75))
                            .child(self.message),
                    ),
            )
    }
}
