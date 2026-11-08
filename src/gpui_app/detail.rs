use crate::i18n::t;

use super::components::button::Button;
use super::components::scroll_area::ScrollAreaExt as _;
use super::components::tabs::Tabs;
use gpui_kit::base::{StyledExt as _, TestSupportExt as _};
use gpui_kit::component::input::InputState;
use gpui_kit::component::table::TableState;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Context, Entity, InteractiveElement as _, IntoElement, ObjectFit,
    ParentElement as _, StatefulInteractiveElement as _, Styled as _, StyledImage as _, Window,
    div, img, px,
};

use crate::shared::workspace::{
    CameraRequest, CameraState, DetailRequest, QueueStatus, ReadStatus, ResourceId,
};
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
                            .gap_2()
                            .items_center()
                            .when(tab == DetailTab::Overview, |row| {
                                row.children(Self::render_machine_header_controls(
                                    id,
                                    printer,
                                    machine_controls.is_pending(id),
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

#[allow(dead_code)]
pub(super) fn queue_panel(printer: &PrinterEntry, cx: &App) -> AnyElement {
    let result = &printer.detail_state.queue;
    let (status, files) = result.value.as_ref().map_or_else(
        || {
            (
                read_status_label(result.status, result.error.as_deref()),
                Vec::new(),
            )
        },
        |queue| {
            let status = match queue.status {
                QueueStatus::Unknown => t!("common.status.unknown").into_owned(),
                QueueStatus::Ready => t!("printers.queue.ready").into_owned(),
                QueueStatus::Running => t!("printers.queue.running").into_owned(),
                QueueStatus::Paused => t!("common.status.paused").into_owned(),
            }
            .to_owned();
            (status, queue.file_names.clone())
        },
    );
    let rows = files
        .into_iter()
        .enumerate()
        .map(|(index, name)| {
            div()
                .id(("queue-file", index))
                .py_2()
                .border_b_1()
                .border_color(cx.theme().border)
                .text_sm()
                .child(name)
                .into_any_element()
        })
        .collect::<Vec<_>>();
    v_flex()
        .id("queue-panel")
        .min_h(px(280.))
        .max_h(px(470.))
        .p_4()
        .gap_3()
        .scroll_area()
        .rounded(cx.theme().radius)
        .border_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().group_box)
        .child(
            h_flex()
                .justify_between()
                .child(div().font_semibold().child(t!("printers.queue.title")))
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(status),
                ),
        )
        .when(rows.is_empty(), |panel| {
            panel.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(t!("printers.queue.empty")),
            )
        })
        .children(rows)
        .into_any_element()
}

#[allow(dead_code)]
pub(super) fn cameras_panel(printer: &PrinterEntry, cx: &App) -> AnyElement {
    let result = &printer.detail_state.cameras;
    let camera_buttons = result
        .value
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|camera| {
            let camera_id = camera.id;
            let sender = printer.camera.requests.clone();
            Button::new(("camera", camera.id.0 as usize))
                .small()
                .label(camera.display_name.clone())
                .disabled(camera.enabled == Some(false))
                .on_click(move |_, _, _| {
                    let _ = sender.try_send(CameraRequest::Load { id: camera_id });
                })
                .into_any_element()
        })
        .collect::<Vec<_>>();

    let frame = printer
        .camera_image
        .get(printer.camera_state.frame.as_ref())
        .map(|image| {
            img(image)
                .w_full()
                .h(px(300.))
                .object_fit(ObjectFit::Contain)
                .into_any_element()
        })
        .unwrap_or_else(|| {
            div()
                .w_full()
                .h(px(300.))
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(camera_state_label(&printer.camera_state))
                .into_any_element()
        });

    v_flex()
        .id("cameras-panel")
        .min_h(px(380.))
        .p_4()
        .gap_3()
        .rounded(cx.theme().radius)
        .border_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().group_box)
        .child(
            h_flex()
                .items_center()
                .justify_between()
                .child(div().font_semibold().child(t!("printers.camera.title")))
                .child(h_flex().gap_2().children(camera_buttons)),
        )
        .when(result.value.as_ref().is_some_and(Vec::is_empty), |panel| {
            panel.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(t!("printers.camera.none_reported")),
            )
        })
        .when(result.value.is_none(), |panel| {
            panel.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(read_status_label(result.status, result.error.as_deref())),
            )
        })
        .child(frame)
        .into_any_element()
}

#[allow(dead_code)]
pub(super) fn camera_state_label(state: &CameraState) -> String {
    let label = state.message.clone().unwrap_or_else(|| {
        match state.status {
            ReadStatus::NotLoaded => t!("printers.camera.select").into_owned(),
            ReadStatus::Loading => t!("printers.camera.loading").into_owned(),
            ReadStatus::Ready => t!("printers.camera.unsupported_format").into_owned(),
            ReadStatus::Unsupported => t!("printers.camera.unsupported").into_owned(),
            ReadStatus::Denied => t!("printers.camera.denied").into_owned(),
            ReadStatus::Limited => t!("printers.camera.limited").into_owned(),
            ReadStatus::Error => t!("printers.camera.error").into_owned(),
        }
        .to_owned()
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

#[allow(dead_code)]
pub(super) fn read_status_label(status: ReadStatus, error: Option<&str>) -> String {
    match status {
        ReadStatus::NotLoaded => t!("printers.read.not_loaded").into_owned(),
        ReadStatus::Loading => t!("common.status.loading").into_owned(),
        ReadStatus::Ready => t!("common.status.ready").into_owned(),
        ReadStatus::Unsupported => t!("printers.read.unsupported").into_owned(),
        ReadStatus::Denied => t!("printers.read.denied").into_owned(),
        ReadStatus::Limited => t!("printers.read.limited").into_owned(),
        ReadStatus::Error => {
            error.map_or_else(|| t!("printers.read.error").into_owned(), str::to_owned)
        }
    }
}
