use super::components::button::Button;
use super::components::card;
use super::components::data_table::DataTable;
use super::components::input::Input;
use super::components::scroll_area::ScrollAreaExt as _;
use super::components::separator;
use super::components::tooltip::TooltipExt as _;
use gpui_kit::TestSupportExt as _;
use gpui_kit::base::StyledExt as _;
use gpui_kit::component::description_list::DescriptionList;
use gpui_kit::component::input::InputState;
use gpui_kit::component::table::{TableEvent, TableState};
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, Focusable as _, InteractiveElement as _,
    IntoElement, ObjectFit, ParentElement as _, Styled as _, StyledImage as _, Window, div, img,
    rems,
};

use crate::shared::printer::PrintStatus;
use crate::shared::workspace::{DetailRequest, ReadStatus, ResourceId};
use crate::state::{LanConnection, LocalPrinterId};

use super::format::{format_bytes, format_duration};
use super::media::ImageSlot;
use super::{MakerCalcGpuiApp, PrinterEntry};

pub(super) mod list;
mod upload;

impl MakerCalcGpuiApp {
    pub(super) fn bind_files_list(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.observe(&self.files_list, |_, _, cx| cx.notify())
            .detach();
        cx.subscribe_in(
            &self.files_list,
            window,
            |app, list, event: &TableEvent, window, cx| match event {
                TableEvent::SelectRow(ix) => {
                    let id = list.update(cx, |state, cx| {
                        if state.delegate_mut().consume_selection_sync(*ix) {
                            return None;
                        }
                        state.delegate_mut().select(Some(*ix));
                        state.focus_handle(cx).focus(window, cx);
                        state.delegate().file_at(*ix).map(|file| file.id)
                    });
                    if let (Some(printer), Some(id)) = (app.selected, id) {
                        app.select_file(printer, id, cx);
                    }
                }
                TableEvent::ClearSelection => {
                    list.update(cx, |state, _| state.delegate_mut().select(None));
                    app.selected_file = None;
                    cx.notify();
                }
                _ => {}
            },
        )
        .detach();
    }
}

impl MakerCalcGpuiApp {
    pub(super) fn select_file(
        &mut self,
        printer_id: LocalPrinterId,
        file_id: ResourceId,
        cx: &mut Context<Self>,
    ) {
        self.selected_file = Some(file_id);
        if let Some(printer) = self.printers.get(&printer_id) {
            let result = printer
                .detail
                .requests
                .try_send(DetailRequest::InspectFile(file_id));
            if result.is_err() {
                self.push_toast(
                    super::feedback::ToastKind::Error,
                    rust_i18n::t!("files.toasts.inspect_failed.title"),
                    Some(rust_i18n::t!("files.toasts.inspect_failed.description").into_owned()),
                    cx,
                );
            }
        }
        cx.notify();
    }
}

pub(super) fn gcode_preview_element(
    bytes: Option<&bytes::Bytes>,
    image_slot: &ImageSlot,
    is_loading: bool,
    cx: &App,
) -> AnyElement {
    let image = image_slot.get(bytes);
    let has_thumbnail = image.is_some();

    (if has_thumbnail {
        super::components::aspect_ratio::aspect_ratio(4. / 3.).max_h(rems(16.))
    } else {
        div().h_40()
    })
    .w_full()
    .flex_shrink_0()
    .rounded(cx.theme().radius)
    .border_1()
    .border_color(cx.theme().border)
    .bg(cx.theme().background)
    .overflow_hidden()
    .flex()
    .items_center()
    .justify_center()
    .when_some(image, |frame, img_arc| {
        frame.child(img(img_arc).size_full().object_fit(ObjectFit::Contain))
    })
    .when(!has_thumbnail && is_loading, |frame| {
        frame.child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(rust_i18n::t!("files.preview.loading")),
        )
    })
    .when(!has_thumbnail && !is_loading, |frame| {
        frame.child(
            super::components::empty::Empty::new(
                "file-preview-empty",
                rust_i18n::t!("files.preview.empty").into_owned(),
            )
            .icon(IconName::File)
            .size_full(),
        )
    })
    .into_any_element()
}

pub(super) fn print_disabled_reason_with_locale(
    printer: &PrinterEntry,
    has_file: bool,
    locale: &str,
) -> Option<std::borrow::Cow<'static, str>> {
    if !has_file {
        Some(rust_i18n::t!(
            "files.print.disabled.no_file",
            locale = locale
        ))
    } else if printer.state.connection != LanConnection::Connected {
        Some(rust_i18n::t!(
            "files.print.disabled.disconnected",
            locale = locale
        ))
    } else if !printer.state.telemetry_ready {
        Some(rust_i18n::t!(
            "files.print.disabled.telemetry_waiting",
            locale = locale
        ))
    } else if printer.operation_state.is_pending() {
        Some(rust_i18n::t!(
            "files.print.disabled.operation_pending",
            locale = locale
        ))
    } else if printer.state.telemetry.print_state == PrintStatus::Completed {
        Some(rust_i18n::t!(
            "files.print.disabled.bed_not_cleared",
            locale = locale
        ))
    } else if printer.state.telemetry.print_state == PrintStatus::Printing {
        Some(rust_i18n::t!(
            "files.print.disabled.already_printing",
            locale = locale
        ))
    } else if printer.state.telemetry.print_state == PrintStatus::Paused {
        Some(rust_i18n::t!(
            "files.print.disabled.print_paused",
            locale = locale
        ))
    } else if printer.state.telemetry.print_state != PrintStatus::Idle {
        Some(rust_i18n::t!(
            "files.print.disabled.not_ready",
            locale = locale
        ))
    } else {
        None
    }
}

pub(super) fn print_disabled_reason(
    printer: &PrinterEntry,
    has_file: bool,
) -> Option<std::borrow::Cow<'static, str>> {
    print_disabled_reason_with_locale(printer, has_file, rust_i18n::locale().as_ref())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn files_panel(
    printer_id: LocalPrinterId,
    printer: &PrinterEntry,
    selected_file_id: Option<ResourceId>,
    compact: bool,
    visual_active: bool,
    files_search: &Entity<InputState>,
    files_list: &Entity<TableState<list::FileList>>,
    window: &mut Window,
    cx: &mut Context<MakerCalcGpuiApp>,
) -> AnyElement {
    let controls_disabled = printer.state.connection != LanConnection::Connected
        || !printer.state.telemetry_ready
        || printer.operation_state.is_pending();
    let files_status = printer.detail_state.files.status;
    let files = printer.detail_state.files.value.as_deref().unwrap_or(&[]);
    let query = files_search.read(cx).value();
    files_list.update(cx, |state, cx| {
        state
            .delegate_mut()
            .bind_interest(&printer.detail.file_interest, visual_active);
        let connection_changed = state
            .delegate_mut()
            .sync_connection(printer.state.connection);
        if state.delegate_mut().sync(
            printer_id,
            printer.detail_state.clone(),
            selected_file_id,
            &query,
        ) || connection_changed
        {
            list::sync_selection(state, cx);
        }
    });
    let delegate = files_list.read(cx).delegate();
    let active_file = delegate.selected_file();
    let summary = rust_i18n::t!(
        "files.list.summary",
        count = delegate.visible.len(),
        total = files.len()
    );
    let status_message = match files_status {
        ReadStatus::Loading => Some(rust_i18n::t!("files.status.loading")),
        ReadStatus::NotLoaded => Some(rust_i18n::t!("files.status.not_loaded")),
        ReadStatus::Error => Some(rust_i18n::t!("files.status.error")),
        ReadStatus::Unsupported => Some(rust_i18n::t!("files.status.unsupported")),
        ReadStatus::Denied => Some(rust_i18n::t!("files.status.denied")),
        _ => None,
    };
    let refresh_sender = printer.detail.requests.clone();

    let list_panel = v_flex()
        .id("files-list-panel")
        .test_support()
        .flex_1()
        .h_full()
        .min_h_0()
        .min_w_0()
        .child(
            h_flex()
                .py_2()
                .justify_between()
                .items_center()
                .child(
                    v_flex()
                        .gap_0p5()
                        .child(
                            div()
                                .font_semibold()
                                .text_sm()
                                .child(rust_i18n::t!("files.list.title")),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(summary),
                        ),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(
                            Button::new("refresh-files")
                                .small()
                                .ghost()
                                .label(rust_i18n::t!("common.actions.refresh"))
                                .on_click(move |_, _, _| {
                                    let _ = refresh_sender.try_send(DetailRequest::ListFiles);
                                }),
                        )
                        .child(
                            Button::new("upload-gcode")
                                .small()
                                .label(rust_i18n::t!("files.actions.upload_gcode"))
                                .disabled(controls_disabled)
                                .on_click(cx.listener(move |app, _, _, cx| {
                                    app.pick_gcode(printer_id, cx);
                                })),
                        ),
                ),
        )
        .child(
            h_flex()
                .flex_wrap()
                .gap_2()
                .py_2()
                .child(list::list_controls(files_list, cx))
                .child(
                    div().flex_1().min_w(gpui_kit::px(200.)).child(
                        Input::new(files_search)
                            .id("files-search")
                            .aria_label(rust_i18n::t!("files.search.placeholder").into_owned())
                            .prefix(Icon::new(IconName::Search))
                            .cleanable(true)
                            .w_full(),
                    ),
                ),
        )
        .when(files_status == ReadStatus::Limited, |panel| {
            panel.child(super::feedback::warning_alert(
                "files-partial-reading",
                &rust_i18n::t!("files.status.limited"),
                cx,
            ))
        })
        .when_some(
            printer.detail_state.file_estimate_error.as_ref(),
            |panel, message| {
                panel.child(super::feedback::warning_alert(
                    "files-estimate-error",
                    message,
                    cx,
                ))
            },
        )
        .when(
            delegate.duration != list::FileRange::All
                || matches!(
                    delegate.order,
                    list::FileOrder::DurationAsc | list::FileOrder::DurationDesc
                ),
            |panel| {
                let unknown = files
                    .iter()
                    .filter(|file| file.estimated_duration_s.is_none())
                    .count();
                panel.when(unknown > 0, |panel| {
                    panel.child(
                        div()
                            .id("files-duration-partial")
                            .test_support()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(
                                rust_i18n::t!("files.table.duration_partial", count = unknown)
                                    .into_owned(),
                            ),
                    )
                })
            },
        )
        .child(
            div()
                .id(if files_status == ReadStatus::Denied {
                    "files-access-denied"
                } else {
                    "files-virtual-list"
                })
                .test_support()
                .flex_1()
                .min_h_0()
                .min_w_0()
                .pt_1()
                .when_some(status_message.as_deref(), |panel, message| {
                    if matches!(files_status, ReadStatus::Error | ReadStatus::Denied) {
                        panel.child(super::feedback::error_alert(
                            "files-catalog-error",
                            message,
                            cx,
                        ))
                    } else {
                        panel.child(div().p_4().text_sm().child(message.to_owned()))
                    }
                })
                .when(status_message.is_none(), |panel| {
                    panel.child(DataTable::new(files_list))
                }),
        );

    let details_panel = {
        let (active_name, active_size, active_format) = if let Some(file) = active_file {
            let fmt = if file.display_name.ends_with(".gcode") {
                "G-code (.gcode)".to_owned()
            } else if file.display_name.ends_with(".g") {
                "G-code (.g)".to_owned()
            } else if file.display_name.ends_with(".3mf") {
                "3MF (.3mf)".to_owned()
            } else {
                "G-code".to_owned()
            };
            (file.display_name.clone(), file.size_bytes, fmt)
        } else {
            (
                rust_i18n::t!("files.details.no_selection").into_owned(),
                None,
                "—".to_owned(),
            )
        };

        let active_details_result =
            active_file.and_then(|f| printer.detail_state.file_details.get(&f.id));
        let is_loading = active_details_result
            .map(|r| r.status == ReadStatus::Loading)
            .unwrap_or(false);
        let active_details = active_details_result.and_then(|r| r.value.as_ref());
        let thumbnail_bytes = active_details.and_then(|d| d.thumbnail.as_ref());

        let has_facts = active_details
            .map(|d| {
                d.analysis.print.estimated_seconds.is_some()
                    || d.analysis.filament.length_mm.is_some()
                    || d.analysis.print.total_layers.is_some()
            })
            .unwrap_or(false);

        let slicing_section: AnyElement = if active_file.is_some() {
            let details_status = active_details_result
                .map(|r| r.status)
                .unwrap_or(ReadStatus::NotLoaded);
            if is_loading {
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(rust_i18n::t!("files.details.slicing.loading"))
                    .into_any_element()
            } else if details_status == ReadStatus::Error {
                super::feedback::error_alert(
                    "file-details-error",
                    &rust_i18n::t!("files.details.slicing.error"),
                    cx,
                )
            } else if details_status == ReadStatus::NotLoaded {
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(rust_i18n::t!("files.details.slicing.not_loaded"))
                    .into_any_element()
            } else if details_status == ReadStatus::Unsupported {
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(rust_i18n::t!("files.details.slicing.unsupported"))
                    .into_any_element()
            } else if details_status == ReadStatus::Denied {
                super::feedback::error_alert(
                    "file-details-access-denied",
                    &rust_i18n::t!("files.details.slicing.denied"),
                    cx,
                )
            } else if let Some(facts) = active_details.filter(|_| has_facts).map(|d| &d.analysis) {
                let mut values = DescriptionList::horizontal()
                    .columns(1)
                    .bordered(false)
                    .small();
                if let Some(seconds) = facts.print.estimated_seconds {
                    values = values.item(
                        rust_i18n::t!("files.details.slicing.duration").as_ref(),
                        format_duration(seconds),
                        1,
                    );
                }
                if let Some(mm) = facts.filament.length_mm {
                    values = values.item(
                        rust_i18n::t!("files.details.slicing.filament").as_ref(),
                        format!("{:.2} m", mm / 1000.0),
                        1,
                    );
                }
                if let Some(layers) = facts.print.total_layers {
                    values = values.item(
                        rust_i18n::t!("files.details.slicing.layers").as_ref(),
                        layers.to_string(),
                        1,
                    );
                }
                v_flex()
                    .gap_2()
                    .when(details_status == ReadStatus::Limited, |column| {
                        column.child(super::feedback::warning_alert(
                            "file-details-partial-reading",
                            &rust_i18n::t!("files.details.slicing.partial"),
                            cx,
                        ))
                    })
                    .child(
                        div()
                            .font_semibold()
                            .text_xs()
                            .child(rust_i18n::t!("files.details.slicing.title")),
                    )
                    .child(values)
                    .into_any_element()
            } else {
                div()
                    .id("file-details-missing-facts")
                    .test_support()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(if details_status == ReadStatus::Limited {
                        rust_i18n::t!("files.details.slicing.limited_no_facts")
                    } else {
                        rust_i18n::t!("files.details.slicing.no_facts")
                    })
                    .into_any_element()
            }
        } else {
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(rust_i18n::t!("files.details.empty"))
                .into_any_element()
        };

        let mut metadata = DescriptionList::horizontal()
            .columns(1)
            .bordered(false)
            .small()
            .item(
                rust_i18n::t!("files.details.metadata.format").as_ref(),
                active_format,
                1,
            )
            .item(
                rust_i18n::t!("files.details.metadata.storage").as_ref(),
                rust_i18n::t!("files.details.metadata.storage_printer").as_ref(),
                1,
            );
        if let Some(size) = active_size {
            metadata = metadata.item(
                rust_i18n::t!("files.details.metadata.size").as_ref(),
                format_bytes(Some(size)),
                1,
            );
        }

        let print_target = active_file.map(|f| (f.id, f.display_name.clone()));
        let disabled_reason = print_disabled_reason(printer, print_target.is_some());
        let print_button = Button::new("print-selected-file")
            .primary()
            .w_full()
            .label(rust_i18n::t!("files.actions.print_file"))
            .disabled(disabled_reason.is_some())
            .on_click(cx.listener(move |app, _, _, cx| {
                if let Some((fid, fname)) = print_target.clone() {
                    app.request_start_print(printer_id, fid, fname, cx);
                }
            }));
        let print_action_block = v_flex().w_full().gap_1p5().child(print_button).when_some(
            disabled_reason,
            |col, reason| {
                col.child(
                    div()
                        .id("file-print-disabled-reason")
                        .test_support()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(rust_i18n::t!(
                            "files.print.disabled.prefix",
                            reason = reason
                        )),
                )
            },
        );

        card::surface(false, cx)
            .id("files-preview-panel")
            .test_support()
            .when(!compact, |p| p.w(rems(22.)).flex_none().h_full())
            .when(compact, |p| p.w_full())
            .min_h_0()
            .pb_0()
            // Keep the existing selected-row/preview 1 px frame contract.
            .shadow(Vec::new())
            .border_1()
            .border_color(if active_file.is_some() {
                cx.theme().table_active_border
            } else {
                cx.theme().border
            })
            .child(
                card::content(false)
                    .w_full()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .gap_4()
                    .scroll_area()
                    .child(
                        card::header(false)
                            .px_0()
                            .child(
                                card::title(false)
                                    .id("selected-file-name")
                                    .test_support()
                                    .truncate()
                                    .app_tooltip({
                                        let name = active_name.clone();
                                        move |_, cx| {
                                            cx.new(|_| {
                                                gpui_kit::component::tooltip::Tooltip::new(
                                                    name.clone(),
                                                )
                                            })
                                            .into()
                                        }
                                    })
                                    .child(active_name),
                            )
                            .child(
                                card::description(cx).child(rust_i18n::t!("files.preview.title")),
                            ),
                    )
                    .child(gcode_preview_element(
                        thumbnail_bytes,
                        &printer.file_image,
                        is_loading,
                        cx,
                    ))
                    .child(metadata)
                    .child(separator::horizontal("file-preview-metadata-separator", cx))
                    .child(slicing_section),
            )
            .child(card::footer(false, cx).child(print_action_block.flex_1()))
    };

    let content = if compact {
        div()
            .grid()
            .grid_cols(1)
            .grid_rows(2)
            .size_full()
            .gap_3()
            .child(details_panel)
            .child(list_panel)
            .into_any_element()
    } else {
        h_flex()
            .size_full()
            .items_stretch()
            .gap_4()
            .child(list_panel)
            .child(details_panel)
            .into_any_element()
    };
    upload::drop_area(
        printer_id,
        &printer.name,
        !controls_disabled,
        content,
        window,
        cx,
    )
}
