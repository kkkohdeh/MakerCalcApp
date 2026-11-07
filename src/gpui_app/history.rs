use crate::i18n::t;
use gpui_kit::base::{StyledExt as _, TestSupportExt as _};
use std::sync::Arc;

use super::components::button::Button;
use super::components::data_table::DataTable;
use super::components::date_picker::DatePicker;
use super::components::input::Input;
use super::components::pagination::{self, Pagination};
use super::components::tooltip::TooltipExt as _;
use gpui_kit::component::calendar::{CalendarState, Date};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::menu::PopupMenuItem;
use gpui_kit::component::table::{TableEvent, TableState};
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, Focusable as _, InteractiveElement as _,
    IntoElement, ParentElement as _, Role, ScrollHandle, StatefulInteractiveElement as _,
    Styled as _, Window, canvas, div, px, relative,
};

use crate::config::Protocol;
use crate::shared::history::{HISTORY_PAGE_SIZE, HistoryRecord, HistorySource};
use crate::shared::printer::PrintStatus;
use crate::shared::workspace::ReadStatus;
use crate::state::LocalPrinterId;

use super::format::{format_duration, print_status_label};
use super::icons::SVG_CHART;
use super::{MakerCalcGpuiApp, PrinterEntry};

pub(super) mod sidebar;
pub(super) mod table;

pub(super) struct HistoryView {
    header_scroll: ScrollHandle,
    detail_scroll: ScrollHandle,
    pub(in crate::gpui_app) sidebar: sidebar::HistorySidebar,
    pub(super) table: Entity<TableState<table::HistoryTable>>,
    pub(super) search: Entity<InputState>,
    pub(super) dates: Entity<CalendarState>,
    pub(super) state: Option<PrintStatus>,
}

impl HistoryView {
    pub(super) fn new(window: &mut Window, cx: &mut App) -> Self {
        Self {
            header_scroll: ScrollHandle::new(),
            detail_scroll: ScrollHandle::new(),
            sidebar: Default::default(),
            table: cx.new(|cx| table::new_table(window, cx)),
            search: cx
                .new(|cx| InputState::new(window, cx).placeholder(t!("history.filters.search"))),
            dates: cx.new(|cx| {
                let mut state = CalendarState::new(window, cx);
                state.set_date(Date::Range(None, None), window, cx);
                state
            }),
            state: None,
        }
    }
}

impl MakerCalcGpuiApp {
    pub(super) fn bind_history_view(&self, window: &mut Window, cx: &mut Context<Self>) {
        cx.observe(&self.history_view.table, |app, table, cx| {
            let delegate = table.read(cx).delegate();
            let needs_query = delegate.requested.as_ref() != Some(&delegate.query);
            if needs_query
                && app.detail_tab == super::DetailTab::History
                && let Some(id) = app.selected
            {
                app.load_history(id, 0, cx);
            }
            app.notify_if_visible(cx);
        })
        .detach();
        cx.subscribe_in(
            &self.history_view.table,
            window,
            |app, table, event: &TableEvent, window, cx| {
                match event {
                    TableEvent::SelectRow(index) => table.update(cx, |state, cx| {
                        if !state.delegate().loading {
                            state.delegate_mut().selected = state
                                .delegate()
                                .page
                                .records
                                .get(*index)
                                .map(|record| record.job_uid.clone());
                            state.focus_handle(cx).focus(window, cx);
                        }
                    }),
                    TableEvent::ClearSelection => {
                        table.update(cx, |state, _| state.delegate_mut().selected = None)
                    }
                    _ => {}
                }
                app.notify_if_visible(cx);
            },
        )
        .detach();
        cx.subscribe(
            &self.history_view.search,
            |app, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    app.apply_history_filters(cx);
                }
            },
        )
        .detach();
    }

    pub(super) fn apply_history_filters(&mut self, cx: &mut Context<Self>) {
        if self.detail_tab != super::DetailTab::History
            || self.history_view.table.read(cx).delegate().loading
        {
            return;
        }
        let name = self.history_view.search.read(cx).value().trim().to_owned();
        let (from, until) = match self.history_view.dates.read(cx).date() {
            Date::Range(from, until) => (from, until),
            Date::Single(date) => (date, date),
        };
        let state = self.history_view.state;
        self.history_view.table.update(cx, |table, _| {
            let query = &mut table.delegate_mut().query;
            query.name = name;
            query.state = state;
            query.from = from;
            query.until = until;
        });
        if let Some(id) = self.selected {
            self.load_history(id, 0, cx);
        }
    }

    pub(super) fn load_history(&mut self, id: LocalPrinterId, offset: u64, cx: &mut Context<Self>) {
        if !self.visual_active {
            return;
        }
        if !self.printers.contains_key(&id) {
            return;
        }
        let filter = self.history_view.table.update(cx, |state, cx| {
            let delegate = state.delegate_mut();
            if delegate.owner != Some(id)
                || delegate.requested.as_ref() != Some(&delegate.query)
                || delegate.page.offset != offset
            {
                delegate.page = Arc::default();
                delegate.selected = None;
                state.clear_selection(cx);
                state.scroll_to_row(0, cx);
            }
            let delegate = state.delegate_mut();
            delegate.owner = Some(id);
            delegate.requested = Some(delegate.query.clone());
            delegate.loading = true;
            let filter = delegate.query.clone();
            state.refresh(cx);
            filter
        });
        let Some(printer) = self.printers.get_mut(&id) else {
            return;
        };
        printer.history_query = printer.history_query.wrapping_add(1);
        printer.history_loading = true;
        let query = printer.history_query;
        let version = *printer.history.changed.borrow();
        let store = printer.history.clone();
        let history_id = printer.history_id.clone();
        cx.spawn(async move |app, cx| {
            let result = store
                .filtered_page(&history_id, offset, filter.clone())
                .await
                .map(Arc::new);
            let _ = app.update(cx, |app, cx| {
                if let Some(printer) = app.printers.get_mut(&id)
                    && printer.history_query == query
                {
                    printer.local_history = Some(result.clone());
                    printer.history_version = Some(version);
                    printer.history_loading = false;
                    app.history_view.table.update(cx, |state, cx| {
                        let delegate = state.delegate_mut();
                        if delegate.owner == Some(id)
                            && delegate.requested.as_ref() == Some(&filter)
                            && delegate.query == filter
                        {
                            match result {
                                Ok(page) => delegate.set_page(page),
                                Err(_) => {
                                    delegate.loading = false;
                                    delegate.page = Arc::default();
                                    delegate.selected = None;
                                }
                            }
                            let index = delegate.selected.as_ref().and_then(|uid| {
                                delegate
                                    .page
                                    .records
                                    .iter()
                                    .position(|record| &record.job_uid == uid)
                            });
                            if let Some(index) = index {
                                if state.selected_row() != Some(index) {
                                    state.set_selected_row(index, cx);
                                }
                            } else {
                                state.clear_selection(cx);
                            }
                            state.refresh(cx);
                        }
                    });
                }
                app.notify_if_visible(cx);
            });
        })
        .detach();
    }
}

pub(super) fn average_per_job(total: Option<f64>, jobs: Option<u64>) -> Option<f64> {
    match (total, jobs) {
        (Some(total), Some(jobs)) if jobs > 0 => Some(total / jobs as f64),
        _ => None,
    }
}

fn history_stat(
    primary_label: String,
    primary_value: String,
    secondary: Option<(String, String)>,
    framed: bool,
    cx: &App,
) -> AnyElement {
    let mut box_el = v_flex()
        .flex_1()
        .min_w_0()
        .gap_0p5()
        .when(framed, |stat| {
            stat.p_2p5()
                .items_center()
                .justify_center()
                .rounded(cx.theme().radius)
                .border_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().secondary)
        })
        .when(!framed, |stat| stat.py_2())
        .child(
            div()
                .text_xs()
                .truncate()
                .text_color(cx.theme().muted_foreground)
                .child(primary_label),
        )
        .child(
            div()
                .text_xl()
                .font_features(gpui_kit::FontFeatures(Arc::new(vec![("tnum".into(), 1)])))
                .truncate()
                .font_semibold()
                .text_color(cx.theme().foreground)
                .child(primary_value),
        );

    if let Some((sec_label, sec_val)) = secondary {
        box_el = box_el
            .child(
                div()
                    .pt_0p5()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(sec_label),
            )
            .child(
                div()
                    .text_xs()
                    .font_medium()
                    .text_color(cx.theme().foreground)
                    .child(sec_val),
            );
    }

    box_el.into_any_element()
}

pub(super) fn history_totals(
    printer: &PrinterEntry,
    compact: bool,
    framed: bool,
    cx: &App,
) -> AnyElement {
    let totals = printer.detail_state.history_totals.value.as_ref();
    let jobs = totals.and_then(|t| t.jobs);
    let total_duration = totals.and_then(|t| t.total_duration_s);
    let print_duration = totals.and_then(|t| t.print_duration_s);
    let filament = totals.and_then(|t| t.filament_used_mm);
    let longest = totals.and_then(|t| t.longest_total_duration_s.or(t.longest_print_duration_s));

    let has_job_stats = jobs.is_some() || total_duration.is_some();

    let grid = if has_job_stats {
        let card_jobs = history_stat(
            t!("history.totals.jobs").into_owned(),
            jobs.map_or_else(|| "—".to_owned(), |n| n.to_string()),
            longest.map(|dur| {
                (
                    t!("history.totals.longest").into_owned(),
                    format_duration(dur),
                )
            }),
            framed,
            cx,
        );

        let card_total_time = history_stat(
            t!("history.totals.elapsed").into_owned(),
            total_duration
                .map(format_duration)
                .unwrap_or_else(|| "—".to_owned()),
            average_per_job(total_duration, jobs).map(|avg| {
                (
                    t!("history.totals.average").into_owned(),
                    format_duration(avg),
                )
            }),
            framed,
            cx,
        );

        let card_print_time = history_stat(
            t!("history.totals.print_time").into_owned(),
            print_duration
                .map(format_duration)
                .unwrap_or_else(|| "—".to_owned()),
            average_per_job(print_duration, jobs).map(|avg| {
                (
                    t!("history.totals.average").into_owned(),
                    format_duration(avg),
                )
            }),
            framed,
            cx,
        );

        let card_filament = history_stat(
            t!("history.totals.filament").into_owned(),
            filament
                .map(|mm| format!("{:.2} m", mm / 1000.0))
                .unwrap_or_else(|| "—".to_owned()),
            average_per_job(filament, jobs).map(|avg| {
                (
                    t!("history.totals.average").into_owned(),
                    format!("{:.2} m", avg / 1000.0),
                )
            }),
            framed,
            cx,
        );

        div()
            .grid()
            .when(compact, |grid| grid.grid_cols(2))
            .when(!compact, |grid| grid.grid_cols(4))
            .gap_2()
            .child(card_jobs)
            .child(card_total_time)
            .child(card_print_time)
            .child(card_filament)
            .into_any_element()
    } else {
        // Lifetime totals have no job averages.
        let card_print_time = history_stat(
            t!("history.totals.lifetime_print_time").into_owned(),
            print_duration
                .map(format_duration)
                .unwrap_or_else(|| "—".to_owned()),
            None,
            framed,
            cx,
        );

        let card_filament = history_stat(
            t!("history.totals.lifetime_filament").into_owned(),
            filament
                .map(|mm| format!("{:.2} m", mm / 1000.0))
                .unwrap_or_else(|| "—".to_owned()),
            None,
            framed,
            cx,
        );

        div()
            .grid()
            .grid_cols(2)
            .gap_2()
            .child(card_print_time)
            .child(card_filament)
            .into_any_element()
    };

    v_flex()
        .id(if framed {
            "history-totals-card"
        } else {
            "history-totals-summary"
        })
        .test_support()
        .gap_2()
        .when(framed, |totals| {
            totals
                .p_3()
                .rounded(cx.theme().radius)
                .border_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().group_box)
        })
        .when(!framed, |totals| {
            totals.pb_2().border_b_1().border_color(cx.theme().border)
        })
        .child(
            h_flex()
                .items_center()
                .justify_between()
                .child(
                    h_flex()
                        .items_center()
                        .gap_2()
                        .child(
                            Icon::default()
                                .data(SVG_CHART)
                                .size_3p5()
                                .text_color(cx.theme().foreground),
                        )
                        .child(
                            div()
                                .text_xs()
                                .font_semibold()
                                .text_color(cx.theme().foreground)
                                .child(t!("history.totals.title")),
                        ),
                )
                .when(
                    printer.detail_state.history_totals.status == ReadStatus::Loading,
                    |row| {
                        row.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(t!("history.loading_totals")),
                        )
                    },
                ),
        )
        .child(grid)
        .into_any_element()
}

pub(super) fn history_panel(
    printer: &PrinterEntry,
    view: &HistoryView,
    window: &mut Window,
    cx: &mut Context<MakerCalcGpuiApp>,
) -> AnyElement {
    let loading = printer.history_loading;
    let page = &view.table.read(cx).delegate().page;
    let id = printer.state.id;
    let offset = page.offset;
    let total = page.total;
    let current = if total == 0 {
        0
    } else {
        offset / HISTORY_PAGE_SIZE + 1
    };
    let pages = total.div_ceil(HISTORY_PAGE_SIZE);
    let selected_state = view.state;
    let app = cx.entity().downgrade();
    let state_label = selected_state.map_or_else(
        || t!("history.filters.all_states").into_owned(),
        |state| print_status_label(state).to_string(),
    );
    let error = printer
        .local_history
        .as_ref()
        .and_then(|result| result.as_ref().err());
    let mut header = v_flex()
        .w_full()
        .min_w_0()
        .gap_3()
        .child(history_totals(
            printer,
            super::bay::bay_columns(window.viewport_size().width.as_f32()) == 1,
            false,
            cx,
        ))
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .whitespace_normal()
                .child(if printer.protocol == Protocol::Moonraker {
                    t!("history.source.imported_help")
                } else {
                    t!("history.source.observed_help")
                }),
        )
        .child(
            h_flex()
                .w_full()
                .min_w_0()
                .gap_2()
                .flex_wrap()
                .items_center()
                .child(
                    Input::new(&view.search)
                        .id("history-search")
                        .aria_label(t!("history.filters.search").into_owned())
                        .prefix(Icon::new(IconName::Search))
                        .cleanable(true)
                        .flex_1()
                        .min_w(px(190.))
                        .disabled(loading),
                )
                .child(
                    Button::new("history-state-filter")
                        .popup_trigger()
                        .outline()
                        .small()
                        .label(t!("history.filters.state", state = state_label).into_owned())
                        .disabled(loading)
                        .select_menu(
                            match selected_state {
                                None | Some(PrintStatus::Idle) => 0,
                                Some(PrintStatus::Completed) => 1,
                                Some(PrintStatus::Cancelled) => 2,
                                Some(PrintStatus::Error) => 3,
                                Some(PrintStatus::Printing) => 4,
                                Some(PrintStatus::Paused) => 5,
                                Some(PrintStatus::Unknown) => 6,
                            },
                            move |mut menu, _, _| {
                                for state in [
                                    None,
                                    Some(PrintStatus::Completed),
                                    Some(PrintStatus::Cancelled),
                                    Some(PrintStatus::Error),
                                    Some(PrintStatus::Printing),
                                    Some(PrintStatus::Paused),
                                    Some(PrintStatus::Unknown),
                                ] {
                                    let label = state.map_or_else(
                                        || t!("history.filters.all_states").into_owned(),
                                        |state| print_status_label(state).to_string(),
                                    );
                                    let app = app.clone();
                                    menu = menu.item(
                                        PopupMenuItem::new(label)
                                            .checked(state == selected_state)
                                            .on_click(move |_, _, cx| {
                                                let _ = app.update(cx, |app, cx| {
                                                    app.history_view.state = state;
                                                    cx.notify();
                                                });
                                            }),
                                    );
                                }
                                menu
                            },
                        ),
                )
                .child(
                    div()
                        .id("history-date-filter")
                        .test_support()
                        .w(px(225.))
                        .child(DatePicker::new(&view.dates, loading)),
                )
                .child(
                    Button::new("history-apply-filters")
                        .small()
                        .label(t!("history.filters.apply").into_owned())
                        .disabled(loading)
                        .on_click(cx.listener(|app, _, _, cx| app.apply_history_filters(cx))),
                ),
        )
        .child(
            h_flex()
                .gap_3()
                .flex_wrap()
                .child(
                    div()
                        .id("history-result-count")
                        .test_support()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(t!("history.count", count = total).into_owned()),
                )
                .when(loading, |row| {
                    row.child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(t!("history.loading")),
                    )
                }),
        );
    if let Some(error) = &*printer.history.error.borrow() {
        header = header.child(super::feedback::error_alert(
            "history-import-error",
            error,
            cx,
        ));
    }
    if let Some(error) = error {
        header = header.child(super::feedback::error_alert(
            "history-read-error",
            &t!("history.read_error", error = error),
            cx,
        ));
    }
    let mut body = v_flex().size_full().min_w_0().min_h_0().gap_3().child(
        header
            .id("local-history-header")
            .test_support()
            .relative()
            .min_h_0()
            .max_h(relative(0.5))
            .overflow_y_scroll()
            .track_scroll(&view.header_scroll)
            .map(|area| {
                super::components::scroll_area::vertical(area, &view.header_scroll, window, cx)
            }),
    );
    if error.is_none() {
        let table = view.table.clone();
        body = body.child(
            div()
                .id("history-table")
                .test_support()
                .w_full()
                .min_w_0()
                .flex_1()
                .min_h(px(64.))
                .overflow_hidden()
                .relative()
                .child(DataTable::new(&view.table))
                .child(
                    canvas(
                        move |bounds, _, cx| {
                            let width = (bounds.size.width.as_f32() - 2.).max(0.);
                            // Use the table width, including parent padding and native border.
                            if table.read(cx).delegate().width_matches(width) {
                                return;
                            }
                            let table = table.clone();
                            cx.defer(move |cx| {
                                table.update(cx, |state, cx| {
                                    if state.delegate_mut().resize(width) {
                                        state.refresh(cx);
                                        cx.notify();
                                    }
                                })
                            });
                        },
                        |_, _, _, _| (),
                    )
                    .absolute()
                    .inset_0(),
                ),
        );
    } else {
        body = body.child(div().flex_1().min_h_0());
    }
    let detail = view
        .table
        .read(cx)
        .delegate()
        .selected_record()
        .map(|record| {
            history_record_detail(
                record,
                window.viewport_size().width.as_f32() < 900.,
                Some(
                    Button::new("history-detail-close")
                        .small()
                        .ghost()
                        .icon(IconName::Close)
                        .accessibility_label(t!("history.detail.close").into_owned())
                        .tooltip(t!("history.detail.close").into_owned())
                        .on_click(cx.listener(|app, _, window, cx| {
                            app.history_view.table.update(cx, |state, cx| {
                                state.delegate_mut().selected = None;
                                state.clear_selection(cx);
                                state.focus_handle(cx).focus(window, cx);
                            });
                            cx.notify();
                        })),
                ),
                cx,
            )
        });
    if let Some(detail) = detail {
        body = body.child(
            div()
                .id("local-history-detail")
                .test_support()
                .relative()
                .min_h_0()
                .max_h(relative(0.3))
                .overflow_y_scroll()
                .track_scroll(&view.detail_scroll)
                .map(|area| {
                    super::components::scroll_area::vertical(area, &view.detail_scroll, window, cx)
                })
                .child(detail),
        );
    }
    let icon_only = window.viewport_size().width < px(640.);
    body.child(
        h_flex()
            .id("local-history-footer")
            .test_support()
            .w_full()
            .flex_none()
            .gap_2()
            .justify_end()
            .items_center()
            .flex_wrap()
            .child(
                Pagination::new("local-history-pagination")
                    .w_auto()
                    .child(pagination::previous(
                        Button::new("local-history-previous")
                            .disabled(loading || offset == 0)
                            .on_click(cx.listener(move |app, _, _, cx| {
                                app.load_history(id, offset.saturating_sub(HISTORY_PAGE_SIZE), cx);
                            })),
                        icon_only,
                    ))
                    .child(
                        div()
                            .id("history-page-count")
                            .test_support()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .font_features(gpui_kit::FontFeatures(Arc::new(vec![(
                                "tnum".into(),
                                1,
                            )])))
                            .child(
                                t!("history.page", current = current, total = pages).into_owned(),
                            ),
                    )
                    .child(pagination::next(
                        Button::new("local-history-next")
                            .disabled(loading || offset.saturating_add(HISTORY_PAGE_SIZE) >= total)
                            .on_click(cx.listener(move |app, _, _, cx| {
                                app.load_history(id, offset.saturating_add(HISTORY_PAGE_SIZE), cx);
                            })),
                        icon_only,
                    )),
            )
            .child(
                Button::new("refresh-local-history")
                    .small()
                    .outline()
                    .label(t!("history.refresh").into_owned())
                    .disabled(loading)
                    .on_click(cx.listener(move |app, _, _, cx| app.load_history(id, offset, cx))),
            ),
    )
    .id("local-history")
    .test_support()
    .into_any_element()
}

fn history_record_detail(
    record: &HistoryRecord,
    compact: bool,
    close: Option<Button>,
    cx: &App,
) -> AnyElement {
    let progress = record
        .job
        .print_progress_pct
        .map(|value| format!("{value:.1} %"))
        .unwrap_or_else(|| "—".into());
    let end = record
        .ended_at
        .map(|date| date.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_else(|| "—".into());
    let elapsed = record
        .ended_at
        .map(|end| format_duration((end - record.started_at).num_seconds() as f64))
        .unwrap_or_else(|| "—".into());
    let materials = record
        .filament_supplies
        .as_ref()
        .map(|supplies| {
            supplies
                .iter()
                .filter_map(|supply| supply.material.as_deref())
                .collect::<Vec<_>>()
                .join(" / ")
        })
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "—".into());
    let source = match record.source {
        HistorySource::Native => t!("history.source.native").into_owned(),
        HistorySource::Observed => t!("history.source.observed").into_owned(),
    };
    let capture = if record.incomplete {
        t!("history.incomplete")
    } else {
        t!("history.capture_unmarked")
    };
    let name = record
        .job
        .print_name
        .clone()
        .unwrap_or_else(|| t!("history.missing_name").into_owned());
    let name_tooltip = name.clone();
    v_flex()
        .id("history-row-detail")
        .test_support()
        .w_full()
        .min_w_0()
        .gap_3()
        .pt_3()
        .border_t_1()
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
                        .truncate()
                        .text_base()
                        .font_medium()
                        .id("history-detail-name")
                        .test_support()
                        .app_tooltip(move |_, cx| {
                            cx.new(|_| {
                                gpui_kit::component::tooltip::Tooltip::new(name_tooltip.clone())
                            })
                            .into()
                        })
                        .child(name),
                )
                .when_some(close, |row, close| row.child(close)),
        )
        .child(
            div()
                .grid()
                .when(compact, |grid| grid.grid_cols(1))
                .when(!compact, |grid| grid.grid_cols(2))
                .gap_3()
                .children(
                    [
                        (t!("history.detail.progress").into_owned(), progress),
                        (t!("history.detail.ended").into_owned(), end),
                        (t!("history.detail.total_time").into_owned(), elapsed),
                        (t!("history.detail.materials").into_owned(), materials),
                        (t!("history.detail.source").into_owned(), source),
                        (
                            t!("history.detail.capture").into_owned(),
                            capture.into_owned(),
                        ),
                    ]
                    .into_iter()
                    .enumerate()
                    .map(|(index, (label, value))| {
                        let tooltip = value.clone();
                        v_flex()
                            .id(("history-detail-field", index))
                            .test_support()
                            .role(Role::Group)
                            .aria_label(format!("{label}: {value}"))
                            .min_w_0()
                            .gap_1()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(label),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .font_medium()
                                    .truncate()
                                    .id(("history-detail-value", index))
                                    .test_support()
                                    .aria_label(value.clone())
                                    .app_tooltip(move |_, cx| {
                                        cx.new(|_| {
                                            gpui_kit::component::tooltip::Tooltip::new(
                                                tooltip.clone(),
                                            )
                                        })
                                        .into()
                                    })
                                    .child(value),
                            )
                    }),
                ),
        )
        .into_any_element()
}
