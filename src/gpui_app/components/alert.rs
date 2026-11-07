use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{ActiveTheme as _, Icon, h_flex};
use gpui_kit::{
    App, ElementId, Hsla, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce,
    Role, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};

#[derive(IntoElement)]
pub(in crate::gpui_app) struct Alert {
    id: ElementId,
    message: String,
    icon: Icon,
    color: Hsla,
}

impl Alert {
    pub(in crate::gpui_app) fn new(
        id: impl Into<ElementId>,
        icon: Icon,
        message: &str,
        color: Hsla,
    ) -> Self {
        Self {
            id: id.into(),
            message: message.to_owned(),
            icon,
            color,
        }
    }
}

impl RenderOnce for Alert {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        h_flex()
            .id(self.id.clone())
            .test_support()
            .role(Role::Alert)
            .aria_label(self.message.clone())
            .w_full()
            .min_w_0()
            .items_start()
            .gap(px(8.))
            .px(px(10.))
            .py(px(8.))
            .border_1()
            .border_color(cx.theme().border)
            .rounded(px(10.))
            .bg(cx.theme().transparent)
            .text_size(px(14.))
            .line_height(px(20.))
            .text_color(self.color)
            .child(
                div()
                    .id((self.id.clone(), "icon"))
                    .test_support()
                    .flex_none()
                    .mt(px(2.))
                    .size(px(16.))
                    .child(self.icon.size(px(16.))),
            )
            .child(
                div()
                    .id((self.id, "description"))
                    .test_support()
                    .flex_1()
                    .min_w_0()
                    .whitespace_normal()
                    .child(self.message),
            )
    }
}
