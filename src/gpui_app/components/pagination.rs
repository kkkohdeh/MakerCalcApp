use gpui_kit::base::{StyledExt as _, TestSupportExt as _};
use gpui_kit::component::{Icon, IconName, Sizable as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ElementId, InteractiveElement as _, IntoElement, ParentElement, RenderOnce,
    Role, StatefulInteractiveElement as _, StyleRefinement, Styled, Window, div, px,
};

use super::button::Button;
use crate::i18n::t;

#[derive(IntoElement)]
pub(in crate::gpui_app) struct Pagination {
    id: ElementId,
    children: Vec<AnyElement>,
    spread: bool,
    style: StyleRefinement,
}
impl Pagination {
    pub(in crate::gpui_app) fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            children: Vec::new(),
            spread: false,
            style: StyleRefinement::default(),
        }
    }
    /// Preserve the compact sidebar's edge-aligned navigation.
    pub(in crate::gpui_app) fn spread(mut self) -> Self {
        self.spread = true;
        self
    }
}
impl Styled for Pagination {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}
impl ParentElement for Pagination {
    fn extend(&mut self, children: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(children);
    }
}

pub(in crate::gpui_app) fn previous(button: Button, icon_only: bool) -> Button {
    button
        .ghost()
        .with_size(px(32.))
        .icon(IconName::ChevronLeft)
        .accessibility_label(t!("history.pagination.previous").into_owned())
        .pl(px(6.))
        .pr(px(10.))
        .when(icon_only, |button| button.w(px(34.)))
        .when(!icon_only, |button| {
            button.label(t!("history.previous").into_owned())
        })
}
pub(in crate::gpui_app) fn next(button: Button, icon_only: bool) -> Button {
    button
        .ghost()
        .with_size(px(32.))
        .accessibility_label(t!("history.pagination.next").into_owned())
        .pl(px(10.))
        .pr(px(6.))
        .when(!icon_only, |button| {
            button.label(t!("common.actions.next").into_owned())
        })
        .child(Icon::new(IconName::ChevronRight).size(px(16.)))
}

impl RenderOnce for Pagination {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let id = self.id;
        h_flex()
            .id(id.clone())
            .test_support()
            .role(Role::Navigation)
            .aria_label(t!("history.pagination.label").into_owned())
            .w_full()
            .min_w_0()
            .justify_center()
            .refine_style(&self.style)
            .child(
                h_flex()
                    .id((id.clone(), "content"))
                    .test_support()
                    .role(Role::List)
                    .items_center()
                    .gap(px(2.))
                    .when(self.spread, |content| content.w_full().justify_between())
                    .children(self.children.into_iter().enumerate().map(|(index, child)| {
                        div()
                            .id(index)
                            .test_support()
                            .role(Role::ListItem)
                            .child(child)
                    })),
            )
    }
}
