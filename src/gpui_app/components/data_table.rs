use super::button::Button;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::table::{ColumnSort, DataTable as NativeTable, TableDelegate, TableState};
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, Size};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Div, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    RenderOnce, SharedString, Styled as _, Window, div, px,
};

#[derive(IntoElement)]
pub(in crate::gpui_app) struct DataTable<D: TableDelegate> {
    state: Entity<TableState<D>>,
}

impl<D: TableDelegate> DataTable<D> {
    pub(in crate::gpui_app) fn new(state: &Entity<TableState<D>>) -> Self {
        Self {
            state: state.clone(),
        }
    }
}

impl<D: TableDelegate> RenderOnce for DataTable<D> {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        div()
            .id(("data-table-frame", self.state.entity_id()))
            .test_support()
            .size_full()
            .min_w_0()
            .min_h_0()
            .overflow_hidden()
            .rounded(px(8.))
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().table)
            .child(
                NativeTable::new(&self.state)
                    .bordered(false)
                    .stripe(false)
                    .with_size(Size::Size(px(37.)))
                    .header_row_height(px(40.))
                    .flat_rows(true)
                    .sort_icon(false),
            )
    }
}

pub(in crate::gpui_app) fn header(
    button: Button,
    label: SharedString,
    sort: Option<ColumnSort>,
    right: bool,
    cx: &App,
) -> AnyElement {
    let icon = match sort {
        Some(ColumnSort::Ascending) => IconName::ArrowUp,
        Some(ColumnSort::Descending) => IconName::ArrowDown,
        _ => IconName::ChevronsUpDown,
    };
    div()
        .size_full()
        .min_w_0()
        .flex()
        .items_center()
        .px(px(8.))
        .child(
            button
                .ghost()
                .w_full()
                .min_w_0()
                .h(px(32.))
                .text_size(px(14.))
                .line_height(px(20.))
                .text_color(cx.theme().muted_foreground)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .map(|title| {
                            if right {
                                title.text_right()
                            } else {
                                title.text_left()
                            }
                        })
                        .child(label),
                )
                .child(Icon::new(icon).size(px(16.))),
        )
        .into_any_element()
}

pub(in crate::gpui_app) fn cell() -> Div {
    div()
        .size_full()
        .min_w_0()
        .px(px(8.))
        .py(px(8.))
        .text_size(px(14.))
        .line_height(px(20.))
}
