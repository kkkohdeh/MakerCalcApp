use crate::i18n::t;

use super::components::button::Button;
use super::components::tabs::Tabs;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::input::InputState;
use gpui_kit::component::table::TableState;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, Window, div, px,
};

use crate::shared::workspace::{CameraState, DetailRequest, ReadStatus, ResourceId};
use crate::state::LocalPrinterId;

use super::files::{files_panel, list};
use super::history::history_panel;
use super::{MakerCalcGpuiApp, PrinterEntry};

#[derive(Clone, Copy, Default, Eq, PartialEq)]
pub(crate) enum DetailTab {
    #[default]
    Overview,
    Files,
    History,
}

impl MakerCalcGpuiApp {
    pub(super) fn set_detail_tab(&mut self, tab: DetailTab, cx: &mut Context<Self>) {
        if tab != DetailTab::Files {
            self.files_list
                .update(cx, |table, _| table.delegate_mut().suspend_reads());
        }
        if tab == DetailTab::Files && self.detail_tab != DetailTab::Files {
            self.files_list.update(cx, |state, cx| {
                state.delegate_mut().order = list::FileOrder::Newest;
                state.delegate_mut().rebuild();
                list::sync_selection(state, cx);
                state.scroll_to_row(0, cx);
            });
        }
        self.detail_tab = tab;
        if tab == DetailTab::History
            && let Some(id) = self.selected
        {
            self.load_history(id, 0, cx);
        }
        if let Some(printer) = self.selected.and_then(|id| self.printers.get(&id)) {
            match tab {
                DetailTab::Files => {
                    let _ = printer.detail.requests.try_send(DetailRequest::ListFiles);
                }
                DetailTab::History => {
                    let _ = printer
                        .detail
                        .requests
                        .try_send(DetailRequest::HistoryTotals);
                }
                DetailTab::Overview => {
                    if printer.detail_state.history_totals.value.is_none()
                        && printer.detail_state.history_totals.status != ReadStatus::Loading
                    {
                        let _ = printer
                            .detail
                            .requests
                            .try_send(DetailRequest::HistoryTotals);
                    }
                }
            }
        }
        cx.notify();
    }

    pub(super) fn close_detail(&mut self, cx: &mut Context<Self>) {
        self.files_list
            .update(cx, |table, _| table.delegate_mut().suspend_reads());
        if let Some(id) = self.selected.take()
            && let Some(printer) = self.printers.get(&id)
        {
            let _ = printer.detail.active.send(false);
        }
        self.selected_file = None;
        cx.notify();
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_detail(
        id: LocalPrinterId,
        printer: &PrinterEntry,
        tab: DetailTab,
        selected_file: Option<ResourceId>,
        viewport_width: f32,
        visual_active: bool,
        machine_controls: &super::machine_controls::MachineControls,
        files_search: &Entity<InputState>,
        files_list: &Entity<TableState<super::files::list::FileList>>,
        history_view: &super::history::HistoryView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        v_flex()
            .id(("printer-detail", id.0 as usize))
            .size_full()
            .pt_2()
            .gap_3()
            .child(
                h_flex()
                    .px_5()
                    .items_center()
                    .justify_between()
                    .w_full()
                    .child(
                        Tabs::new(
                            "detail-tabs",
                            match tab {
                                DetailTab::Overview => 0,
                                DetailTab::Files => 1,
                                DetailTab::History => 2,
                            },
                            [
                                t!("printers.tabs.overview").into_owned().into(),
                                t!("printers.tabs.files").into_owned().into(),
                                t!("printers.tabs.history").into_owned().into(),
                            ],
                        )
                        .on_change(cx.listener(
                            |app, ix, _, cx| match *ix {
                                0 => app.set_detail_tab(DetailTab::Overview, cx),
                                1 => app.set_detail_tab(DetailTab::Files, cx),
                                2 => app.set_detail_tab(DetailTab::History, cx),
                                _ => {}
                            },
                        )),
                    )
                    .child(div().flex_1())
                    .child(
                        h_flex()
                            .gap(px(4.))
                            .items_center()
                            .when(tab == DetailTab::Overview, |row| {
                                row.children(Self::render_machine_header_controls(
                                    id,
                                    printer,
                                    machine_controls.is_pending(id),
                                    window,
                                    cx,
                                ))
                            })
                            .child(
                                Button::new("edit-printer")
                                    .ghost()
                                    .small()
                                    .label(t!("common.actions.edit").into_owned())
                                    .on_click(cx.listener(move |app, _, window, cx| {
                                        app.open_edit_printer(id, window, cx);
                                    }))
                                    .child(
                                        div()
                                            .absolute()
                                            .left(px(8.))
                                            .right(px(8.))
                                            .bottom_0()
                                            .h(px(2.))
                                            .bg(cx.theme().tokens.primary),
                                    ),
                            ),
                    ),
            )
            .child(
                div()
                    .id("detail-tab-panel")
                    .test_support()
                    .role(gpui_kit::Role::TabPanel)
                    .aria_label(match tab {
                        DetailTab::Overview => t!("printers.tabs.overview").into_owned(),
                        DetailTab::Files => t!("printers.tabs.files").into_owned(),
                        DetailTab::History => t!("printers.tabs.history").into_owned(),
                    })
                    .flex_1()
                    .w_full()
                    .min_h(px(0.))
                    .when(tab != DetailTab::Overview, |panel| panel.px_5().pb_5())
                    .child(match tab {
                        DetailTab::Files => {
                            // Files use their own breakpoint instead of the printer-card grid.
                            files_panel(
                                id,
                                printer,
                                selected_file,
                                viewport_width < 900.,
                                visual_active,
                                files_search,
                                files_list,
                                window,
                                cx,
                            )
                        }
                        DetailTab::History => v_flex()
                            .size_full()
                            .min_h_0()
                            .overflow_hidden()
                            .child(history_panel(printer, history_view, window, cx))
                            .into_any_element(),
                        DetailTab::Overview => {
                            Self::render_overview(id, printer, viewport_width, machine_controls, cx)
                        }
                    }),
            )
            .into_any_element()
    }
}

pub(super) fn camera_state_label(state: &CameraState) -> String {
    let label = state.message.clone().unwrap_or_else(|| match state.status {
        ReadStatus::NotLoaded => t!("printers.camera.select").into_owned(),
        ReadStatus::Loading => t!("printers.camera.loading").into_owned(),
        ReadStatus::Ready => t!("printers.camera.unsupported_format").into_owned(),
        ReadStatus::Unsupported => t!("printers.camera.unsupported").into_owned(),
        ReadStatus::Denied => t!("printers.camera.denied").into_owned(),
        ReadStatus::Limited => t!("printers.camera.limited").into_owned(),
        ReadStatus::Error => t!("printers.camera.error").into_owned(),
    });
    state.received_at.map_or(label.clone(), |received_at| {
        t!(
            "printers.camera.age",
            label = label,
            seconds = received_at.elapsed().as_secs()
        )
        .into_owned()
    })
}
