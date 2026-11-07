use std::sync::Arc;

use super::super::components::button::Button;
use super::super::components::data_table;
use super::super::components::tooltip::TooltipExt as _;
use gpui_kit::base::{StyledExt as _, TestSupportExt as _};
use gpui_kit::component::table::{Column, ColumnSort, TableDelegate, TableState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Div, InteractiveElement as _, IntoElement, ParentElement as _,
    Stateful, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};

use crate::i18n::t;
use crate::shared::history::{HistoryColumn, HistoryQuery, HistoryRecord, LocalHistoryPage};
use crate::state::LocalPrinterId;

use super::super::format::{format_duration, print_status_label};

const COLUMNS: [HistoryColumn; 6] = [
    HistoryColumn::Name,
    HistoryColumn::State,
    HistoryColumn::Started,
    HistoryColumn::PrintTime,
    HistoryColumn::Grams,
    HistoryColumn::Length,
];
const MIN_WIDTHS: [f32; 6] = [245., 145., 160., 165., 105., 105.];
const GROWTH: [f32; 6] = [0.40, 0.12, 0.16, 0.14, 0.09, 0.09];

#[derive(Default)]
pub(in crate::gpui_app) struct HistoryTable {
    pub(in crate::gpui_app) owner: Option<LocalPrinterId>,
    pub(in crate::gpui_app) query: HistoryQuery,
    pub(in crate::gpui_app) requested: Option<HistoryQuery>,
    pub(in crate::gpui_app) page: Arc<LocalHistoryPage>,
    pub(in crate::gpui_app) selected: Option<String>,
    pub(in crate::gpui_app) loading: bool,
    width: f32,
}

impl HistoryTable {
    pub(in crate::gpui_app) fn width_matches(&self, width: f32) -> bool {
        self.width == width.floor().max(MIN_WIDTHS.iter().sum())
    }

    pub(in crate::gpui_app) fn resize(&mut self, width: f32) -> bool {
        let width = width.floor().max(MIN_WIDTHS.iter().sum());
        if self.width == width {
            return false;
        }
        self.width = width;
        true
    }

    pub(in crate::gpui_app) fn selected_record(&self) -> Option<&HistoryRecord> {
        self.selected
            .as_ref()
            .and_then(|uid| self.page.records.iter().find(|row| &row.job_uid == uid))
    }

    pub(in crate::gpui_app) fn set_page(&mut self, page: Arc<LocalHistoryPage>) {
        self.page = page;
        if self.selected_record().is_none() {
            self.selected = None;
        }
        self.loading = false;
    }
}

pub(in crate::gpui_app) fn new_table(
    window: &mut Window,
    cx: &mut Context<TableState<HistoryTable>>,
) -> TableState<HistoryTable> {
    TableState::new(HistoryTable::default(), window, cx)
        .col_selectable(false)
        .cell_selectable(false)
        .col_movable(false)
        .col_resizable(false)
        .loop_selection(false)
}

pub(in crate::gpui_app) fn cell_text(record: &HistoryRecord, column: HistoryColumn) -> String {
    match column {
        HistoryColumn::Name => record
            .job
            .print_name
            .clone()
            .unwrap_or_else(|| t!("history.missing_name").into_owned()),
        HistoryColumn::State => print_status_label(record.job.print_state).to_string(),
        HistoryColumn::Started => record.started_at.format("%Y-%m-%d %H:%M").to_string(),
        HistoryColumn::PrintTime => record
            .job
            .print_elapsed_time_s
            .map(format_duration)
            .unwrap_or_else(|| "—".into()),
        HistoryColumn::Grams => record
            .job
            .print_filament_used_g
            .map(|grams| format!("{grams:.2} g"))
            .unwrap_or_else(|| "—".into()),
        HistoryColumn::Length => record
            .job
            .print_filament_used_mm
            .map(|mm| format!("{:.2} m", mm / 1000.))
            .unwrap_or_else(|| "—".into()),
    }
}

impl TableDelegate for HistoryTable {
    fn columns_count(&self, _: &App) -> usize {
        COLUMNS.len()
    }
    fn rows_count(&self, _: &App) -> usize {
        self.page.records.len()
    }
    fn loading(&self, _: &App) -> bool {
        self.loading
    }

    fn column(&self, ix: usize, _: &App) -> Column {
        let (key, name) = match COLUMNS[ix] {
            HistoryColumn::Name => ("name", t!("history.table.name")),
            HistoryColumn::State => ("state", t!("history.table.state")),
            HistoryColumn::Started => ("started", t!("history.table.started")),
            HistoryColumn::PrintTime => ("print_time", t!("history.table.print_time")),
            HistoryColumn::Grams => ("grams", t!("history.table.grams")),
            HistoryColumn::Length => ("length", t!("history.table.length")),
        };
        let extra = (self.width - MIN_WIDTHS.iter().sum::<f32>()).max(0.);
        let width = MIN_WIDTHS[ix] + extra * GROWTH[ix];
        let sort = if self.query.column == COLUMNS[ix] {
            if self.query.descending {
                ColumnSort::Descending
            } else {
                ColumnSort::Ascending
            }
        } else {
            ColumnSort::Default
        };
        Column::new(key, name)
            .p_0()
            .width(px(width))
            .min_width(px(MIN_WIDTHS[ix]))
            .sort(sort)
            .resizable(false)
            .movable(false)
            .when(ix == 0, |column| column.fixed_left())
            .when(ix >= 3, |column| column.text_right())
    }

    fn perform_sort(
        &mut self,
        ix: usize,
        sort: ColumnSort,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
        if self.loading {
            return;
        }
        self.query.column = COLUMNS[ix];
        self.query.descending = sort != ColumnSort::Ascending;
        cx.notify();
    }

    fn render_th(
        &mut self,
        ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let column = self.column(ix, cx);
        let button = Button::new(("history-column", ix))
            .disabled(self.loading)
            .accessibility_label(column.name.clone())
            .tooltip(column.name.clone())
            .on_click(cx.listener(move |state, _, window, cx| {
                let column = state.delegate().column(ix, cx);
                let sort = if column.sort == Some(ColumnSort::Descending) {
                    ColumnSort::Ascending
                } else {
                    ColumnSort::Descending
                };
                state.delegate_mut().perform_sort(ix, sort, window, cx);
            }));
        data_table::header(button, column.name, column.sort, ix >= 3, cx)
    }

    fn render_tr(
        &mut self,
        ix: usize,
        _: &mut Window,
        _: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        let identity = self
            .page
            .records
            .get(ix)
            .map(|record| record.job_uid.clone())
            .unwrap_or_else(|| ix.to_string());
        div().id(format!("history-row-{identity}"))
    }

    fn render_td(
        &mut self,
        row: usize,
        ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let record = self.page.records.get(row);
        let value = record
            .map(|record| cell_text(record, COLUMNS[ix]))
            .unwrap_or_else(|| "—".into());
        let uid = record.map(|record| record.job_uid.as_str()).unwrap_or("");
        let tooltip = value.clone();
        data_table::cell()
            .id(format!("history-cell-{uid}-{ix}"))
            .test_support()
            .aria_label(value.clone())
            .app_tooltip(move |_, cx| {
                cx.new(|_| gpui_kit::component::tooltip::Tooltip::new(tooltip.clone()))
                    .into()
            })
            .truncate()
            .when(ix == 0, |cell| cell.font_medium())
            .when(ix >= 3, |cell| cell.text_right())
            .when(ix >= 2, |cell| {
                cell.font_features(gpui_kit::FontFeatures(Arc::new(vec![("tnum".into(), 1)])))
            })
            .child(if ix == 1 {
                gpui_kit::component::h_flex()
                    .child(super::super::feedback::status_badge(value, cx))
                    .into_any_element()
            } else {
                div().child(value).into_any_element()
            })
    }

    fn render_empty(
        &mut self,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        super::super::feedback::empty_state(
            "history-empty-state",
            gpui_kit::component::IconName::Calendar,
            &t!("history.no_matches"),
            cx,
        )
    }

    fn render_last_empty_col(
        &mut self,
        _: &mut Window,
        _: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        div().w_0().h_full()
    }
}
