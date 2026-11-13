use std::rc::Rc;
use std::sync::Arc;

use super::super::components::button::Button;
use super::super::components::item::{self, Item};
use super::super::components::pagination::{self, Pagination};
use super::super::components::tooltip::TooltipExt as _;
use gpui_kit::base::{Button as BaseButton, StyledExt as _, TestSupportExt as _};
use gpui_kit::component::sidebar::{Sidebar, SidebarCollapsible, SidebarItem};
use gpui_kit::component::{
    ActiveTheme as _, Collapsible, Icon, IconName, Sizable as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, ClickEvent, Context, ElementId, InteractiveElement as _,
    IntoElement, ParentElement as _, Role, StatefulInteractiveElement as _, Styled as _, Window,
    div, px,
};

use crate::history::HistoryHandle;
use crate::i18n::t;
use crate::shared::history::{HISTORY_PAGE_SIZE, LocalHistoryPage};

use super::super::MakerCalcGpuiApp;
use super::super::bay::bay_columns;
use super::super::format::print_status_label;
use super::super::icons::SVG_TIME;

const WIDTH: f32 = 340.;
const BAY_MINIMUM: f32 = 600.;

#[derive(Default)]
pub(in crate::gpui_app) struct HistorySidebar {
    pub(in crate::gpui_app) store: Option<HistoryHandle>,
    pub(in crate::gpui_app) open: bool,
    pub(in crate::gpui_app) page: Arc<LocalHistoryPage>,
    pub(in crate::gpui_app) selected: Option<(String, String)>,
    pub(in crate::gpui_app) loading: bool,
    pub(in crate::gpui_app) error: Option<String>,
    version: Option<u64>,
}

impl MakerCalcGpuiApp {
    pub(in crate::gpui_app) fn toggle_global_history(&mut self, cx: &mut Context<Self>) {
        self.history_view.sidebar.open = !self.history_view.sidebar.open;
        if self.history_view.sidebar.open {
            self.load_global_history(0, cx);
        }
        cx.notify();
    }

    pub(in crate::gpui_app) fn refresh_global_history(&mut self, cx: &mut Context<Self>) {
        let sidebar = &self.history_view.sidebar;
        if sidebar.open
            && self.selected.is_none()
            && !sidebar.loading
            && sidebar
                .store
                .as_ref()
                .is_some_and(|store| sidebar.version != Some(*store.changed.borrow()))
        {
            self.load_global_history(sidebar.page.offset, cx);
        }
    }

    pub(in crate::gpui_app) fn load_global_history(&mut self, offset: u64, cx: &mut Context<Self>) {
        let sidebar = &mut self.history_view.sidebar;
        if !self.visual_active || self.selected.is_some() || !sidebar.open || sidebar.loading {
            return;
        }
        let Some(store) = sidebar.store.clone() else {
            sidebar.error = Some(t!("history.query_error").into_owned());
            return;
        };
        let version = *store.changed.borrow();
        sidebar.loading = true;
        sidebar.error = None;
        cx.notify();
        cx.spawn(async move |app, cx| {
            let result = store.global_page(offset).await;
            let _ = app.update(cx, |app, cx| {
                let sidebar = &mut app.history_view.sidebar;
                sidebar.loading = false;
                sidebar.version = Some(version);
                match result {
                    Ok(page) => {
                        if sidebar.selected.as_ref().is_some_and(|(printer, uid)| {
                            !page
                                .records
                                .iter()
                                .any(|row| &row.printer_id == printer && &row.job_uid == uid)
                        }) {
                            sidebar.selected = None;
                        }
                        sidebar.page = Arc::new(page);
                    }
                    Err(error) => sidebar.error = Some(error),
                }
                if sidebar.open && app.selected.is_none() {
                    app.notify_if_visible(cx);
                }
            });
        })
        .detach();
    }

    pub(in crate::gpui_app) fn render_history_toggle(&self, cx: &Context<Self>) -> Button {
        Button::new("global-history-toggle")
            .small()
            .ghost()
            .toggled(self.history_view.sidebar.open)
            .icon(Icon::default().data(SVG_TIME))
            .label(t!("history.sidebar.toggle"))
            .tooltip(t!("history.sidebar.title"))
            .on_click(cx.listener(|app, _, _, cx| app.toggle_global_history(cx)))
    }

    pub(in crate::gpui_app) fn render_bay_with_history(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let width = window.viewport_size().width.as_f32();
        let open = self.history_view.sidebar.open;
        let floating = width < WIDTH + BAY_MINIMUM;
        let bay_width = if open && !floating {
            width - WIDTH
        } else {
            width
        };
        let columns = bay_columns(bay_width).min(self.printers.len().max(1));
        let bay = self.render_bay(columns, cx);
        let body = div()
            .id("bay-history-layout")
            .test_support()
            .flex()
            .size_full()
            .relative()
            .min_w_0();
        if floating {
            return body
                .child(bay)
                .when(open, |body| {
                    body.child(
                        div()
                            .id("global-history-overlay")
                            .test_support()
                            .absolute()
                            .left_0()
                            .top_0()
                            .bottom_0()
                            .w(px(WIDTH))
                            .occlude()
                            .child(self.render_global_history(cx)),
                    )
                })
                .into_any_element();
        }
        body.child(self.render_global_history(cx))
            .child(div().flex_1().min_w_0().h_full().child(bay))
            .into_any_element()
    }

    fn render_global_history(&self, cx: &mut Context<Self>) -> AnyElement {
        let sidebar = &self.history_view.sidebar;
        let notice = if sidebar.loading {
            Some(t!("history.loading").into_owned())
        } else {
            sidebar
                .error
                .as_ref()
                .map(|error| t!("history.read_error", error = error).into_owned())
        };
        let header = v_flex()
            .id("global-history-header")
            .test_support()
            .w_full()
            .min_w_0()
            .gap_2()
            .pb_3()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_base()
                            .font_semibold()
                            .child(t!("history.sidebar.title")),
                    )
                    .child(
                        Button::new("global-history-close")
                            .small()
                            .ghost()
                            .icon(IconName::ChevronLeft)
                            .accessibility_label(t!("common.actions.close").into_owned())
                            .tooltip(t!("common.actions.close"))
                            .on_click(cx.listener(|app, _, _, cx| app.toggle_global_history(cx))),
                    ),
            )
            .when_some(notice, |header, notice| {
                if sidebar.error.is_some() {
                    return header.child(super::super::feedback::error_alert(
                        "global-history-notice",
                        &notice,
                        cx,
                    ));
                }
                header.child(
                    div()
                        .id("global-history-notice")
                        .test_support()
                        .text_xs()
                        .child(notice),
                )
            });
        let page = sidebar.page.offset / HISTORY_PAGE_SIZE + 1;
        let pages = sidebar.page.total.div_ceil(HISTORY_PAGE_SIZE).max(1);
        let offset = sidebar.page.offset;
        let footer = v_flex()
            .id("global-history-footer")
            .test_support()
            .w_full()
            .gap_2()
            .mb_1()
            .pt_3()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(
                Button::new("global-history-refresh")
                    .small()
                    .w_full()
                    .label(t!("common.actions.refresh"))
                    .disabled(sidebar.loading)
                    .on_click(
                        cx.listener(move |app, _, _, cx| app.load_global_history(offset, cx)),
                    ),
            )
            .child(
                Pagination::new("global-history-pagination")
                    .spread()
                    .child(pagination::previous(
                        Button::new("global-history-previous")
                            .disabled(sidebar.loading || offset == 0)
                            .on_click(cx.listener(move |app, _, _, cx| {
                                app.load_global_history(
                                    offset.saturating_sub(HISTORY_PAGE_SIZE),
                                    cx,
                                )
                            })),
                        true,
                    ))
                    .child(
                        div()
                            .id("global-history-page")
                            .test_support()
                            .text_xs()
                            .child(t!("history.page", current = page, total = pages)),
                    )
                    .child(pagination::next(
                        Button::new("global-history-next")
                            .disabled(
                                sidebar.loading || offset + HISTORY_PAGE_SIZE >= sidebar.page.total,
                            )
                            .on_click(cx.listener(move |app, _, _, cx| {
                                app.load_global_history(
                                    offset.saturating_add(HISTORY_PAGE_SIZE),
                                    cx,
                                )
                            })),
                        true,
                    )),
            );
        let records = if sidebar.open {
            sidebar.page.records.as_slice()
        } else {
            &[]
        };
        let mut rows = records
            .iter()
            .enumerate()
            .map(|(index, record)| {
                let key = (record.printer_id.clone(), record.job_uid.clone());
                let printer = self
                    .printers
                    .values()
                    .find(|printer| printer.history_id == record.printer_id)
                    .map(|printer| printer.name.clone())
                    .unwrap_or_else(|| t!("history.sidebar.unavailable_printer").into_owned());
                SidebarRow::Record(SidebarRecord {
                    page: sidebar.page.clone(),
                    index,
                    printer,
                    selected: sidebar.selected.as_ref() == Some(&key),
                    on_click: Rc::new(cx.listener(move |app, _: &ClickEvent, _, cx| {
                        let selected = &mut app.history_view.sidebar.selected;
                        *selected = if selected.as_ref() == Some(&key) {
                            None
                        } else {
                            Some(key.clone())
                        };
                        cx.notify();
                    })),
                })
            })
            .collect::<Vec<_>>();
        if sidebar.open
            && !sidebar.loading
            && sidebar.error.is_none()
            && sidebar.page.records.is_empty()
        {
            rows.push(SidebarRow::Empty(t!("history.empty").into_owned()));
        }
        let panel = Sidebar::new("global-history-sidebar")
            .w(px(WIDTH))
            .bg(cx.theme().group_box)
            .text_color(cx.theme().foreground)
            .border_t_1()
            .border_b_1()
            .border_color(cx.theme().list_active_border)
            .rounded_r(cx.theme().radius_lg)
            .collapsible(SidebarCollapsible::Offcanvas)
            .collapsed(!sidebar.open)
            .header(header)
            .footer(footer)
            .children(rows)
            .into_any_element();
        div()
            .id("global-history-panel")
            .test_support()
            .flex_none()
            .h_full()
            .pb_3()
            .child(panel)
            .into_any_element()
    }
}

#[derive(Clone)]
enum SidebarRow {
    Record(SidebarRecord),
    Empty(String),
}
impl Collapsible for SidebarRow {
    fn is_collapsed(&self) -> bool {
        false
    }
    fn collapsed(self, _: bool) -> Self {
        self
    }
}
impl SidebarItem for SidebarRow {
    fn render(
        self,
        id: impl Into<ElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> impl IntoElement {
        match self {
            Self::Record(row) => SidebarItem::render(row, id, window, cx).into_any_element(),
            Self::Empty(message) => {
                super::super::components::empty::Empty::new("global-history-notice", message)
                    .into_any_element()
            }
        }
    }
}

#[derive(Clone)]
struct SidebarRecord {
    page: Arc<LocalHistoryPage>,
    index: usize,
    printer: String,
    selected: bool,
    on_click: RowClickHandler,
}

type RowClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

impl Collapsible for SidebarRecord {
    fn is_collapsed(&self) -> bool {
        false
    }
    fn collapsed(self, _: bool) -> Self {
        self
    }
}

impl SidebarItem for SidebarRecord {
    fn render(self, _: impl Into<ElementId>, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let record = &self.page.records[self.index];
        let name = record
            .job
            .print_name
            .clone()
            .unwrap_or_else(|| t!("history.missing_name").into_owned());
        let tooltip = name.clone();
        let label = format!("{name} · {}", self.printer);
        let on_click = self.on_click;
        let content = item::content()
            .child(
                item::title()
                    .id(("global-history-name", self.index))
                    .aria_label(name.clone())
                    .test_support()
                    .w_full()
                    .min_w_0()
                    .child(name)
                    .app_tooltip(move |_, cx| {
                        cx.new(|_| gpui_kit::component::tooltip::Tooltip::new(tooltip.clone()))
                            .into()
                    }),
            )
            .child(
                item::description(cx)
                    .id(("global-history-printer", self.index))
                    .aria_label(self.printer.clone())
                    .test_support()
                    .w_full()
                    .truncate()
                    .child(self.printer),
            )
            .child(
                h_flex().child(
                    super::super::feedback::status_badge(
                        print_status_label(record.job.print_state),
                        cx,
                    )
                    .text_color(cx.theme().muted_foreground),
                ),
            )
            .child(
                item::description(cx).child(
                    record
                        .ended_at
                        .map(|at| at.format("%Y-%m-%d %H:%M UTC").to_string())
                        .unwrap_or_else(|| "—".into()),
                ),
            )
            .when(record.incomplete, |row| {
                row.child(item::description(cx).child(t!("history.sidebar.partial")))
            });
        let trigger = Item::new(
            ("global-history-row", self.index),
            BaseButton::new(("global-history-row", self.index))
                .accessibility_label(label.clone())
                .aria_expanded(self.selected)
                .on_click(move |event, window, cx| on_click(event, window, cx)),
            content,
        )
        .selected(self.selected);
        // Reading/selecting the panel must not activate the trigger again.
        v_flex()
            .id(("global-history-item", self.index))
            .test_support()
            .role(Role::Group)
            .aria_label(label.clone())
            .w_full()
            .min_w_0()
            .border_b_1()
            .border_color(cx.theme().border)
            .when(self.selected, |root| root.bg(cx.theme().list_active))
            .child(trigger)
            .when(self.selected, |root| {
                root.child(
                    div()
                        .id(("global-history-content", self.index))
                        .test_support()
                        .role(Role::Region)
                        .aria_label(label)
                        .w_full()
                        .min_w_0()
                        .px_3()
                        .pb(px(10.))
                        .child(super::history_record_detail(record, true, None, cx)),
                )
            })
    }
}
