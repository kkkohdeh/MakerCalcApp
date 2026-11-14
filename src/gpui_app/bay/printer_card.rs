//! Web bay composition adapted to GPUI, with local connection metadata in the plate.
use super::*;
use crate::shared::telemetry::JobTelemetry;
use gpui_kit::{
    Bounds, ContentMask, Div, FontFeatures, FontWeight, ObjectFit, StyledImage as _, canvas, fill,
    img, size,
};

pub(super) fn display_job(state: &PrinterState) -> Option<&JobTelemetry> {
    let telemetry = &state.telemetry;
    if matches!(
        telemetry.print_state,
        PrintStatus::Printing | PrintStatus::Paused
    ) && telemetry.current_job.is_some()
    {
        return telemetry.current_job.as_ref();
    }
    telemetry
        .current_job
        .as_ref()
        .filter(|job| {
            matches!(
                job.print_state,
                PrintStatus::Completed | PrintStatus::Cancelled | PrintStatus::Error
            )
        })
        .or(telemetry.last_job.as_ref())
        .filter(|job| {
            job.print_name.is_some()
                || job.print_elapsed_time_s.is_some()
                || job.print_filament_used_g.is_some()
                || job.print_filament_used_mm.is_some()
        })
}

fn short_duration(seconds: f64) -> String {
    let seconds = seconds.max(0.).floor() as u64;
    if seconds >= 3600 {
        format!("{}h {:02}m", seconds / 3600, seconds % 3600 / 60)
    } else if seconds >= 60 {
        format!("{}m", seconds / 60)
    } else {
        format!("{seconds}s")
    }
}

fn thumbnail(printer: &PrinterEntry, cx: &App) -> impl IntoElement {
    div()
        .id(("bay-thumbnail", printer.state.id.0 as usize))
        .test_support()
        .w(px(116.))
        .h(px(74.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .p_1()
        .rounded(px(12.))
        .bg(cx.theme().background)
        .overflow_hidden()
        .when_some(
            printer.thumbnail_image.get(printer.thumbnail.as_ref()),
            |frame, image| {
                frame.child(
                    img(image)
                        .id(("bay-thumbnail-image", printer.state.id.0 as usize))
                        .size_full()
                        // Keep the viewport's ratio when GPUI learns the image's intrinsic size.
                        .aspect_ratio(108. / 66.)
                        .object_fit(ObjectFit::Contain)
                        .test_support(),
                )
            },
        )
        .when(printer.thumbnail.is_none(), |frame| {
            frame.child(
                img(super::super::media::placeholder_image())
                    .w(px(80.))
                    .h(px(40.))
                    .object_fit(ObjectFit::Contain),
            )
        })
}

fn controls(
    id: LocalPrinterId,
    printer: &PrinterEntry,
    in_detail: bool,
    cx: &mut Context<MakerCalcGpuiApp>,
) -> Div {
    use crate::shared::workspace::PrinterActionKind;

    let state = &printer.state;
    let active = printer.operation_state.allows_actions()
        && state.connection == LanConnection::Connected
        && matches!(
            state.telemetry.print_state,
            PrintStatus::Printing | PrintStatus::Paused
        );
    h_flex()
        .on_mouse_down(gpui_kit::MouseButton::Left, |_, _, cx| {
            cx.stop_propagation()
        })
        .flex_none()
        .gap_1()
        .items_center()
        .when(active, |row| {
            let paused = state.telemetry.print_state == PrintStatus::Paused;
            let pause_or_resume = if paused {
                PrinterActionKind::ResumePrint
            } else {
                PrinterActionKind::PausePrint
            };
            let sender = printer.actions.clone();
            let disabled = !state.telemetry_ready || printer.operation_state.is_pending();
            let pause_label = if paused {
                t!("printers.actions.resume")
            } else {
                t!("printers.actions.pause")
            }
            .into_owned();
            let cancel_label = t!("common.actions.cancel").into_owned();
            let generation = state.job_generation;
            let epoch = state.connection_epoch;
            let name = state
                .telemetry
                .current_job
                .as_ref()
                .and_then(|job| job.print_name.clone())
                .unwrap_or_else(|| t!("printers.bay.current_job").into_owned());
            let pause_name = name.clone();
            row.when(state.controls.allows_print(pause_or_resume), |row| {
                row.child(
                    Button::new(if in_detail {
                        gpui_kit::ElementId::from("overview-pause")
                    } else {
                        ("bay-pause", id.0 as usize).into()
                    })
                    .secondary()
                    .child(
                        Icon::default()
                            .data(if paused {
                                web_icons::RESUME
                            } else {
                                web_icons::PAUSE
                            })
                            .size(px(14.)),
                    )
                    .size(px(26.))
                    .p_0()
                    .rounded(px(7.))
                    .tooltip(pause_label.clone())
                    .accessibility_label(pause_label)
                    .disabled(disabled)
                    .on_click(cx.listener(move |app, _, _, cx| {
                        cx.stop_propagation();
                        if paused {
                            let _ = sender.try_send(PrinterAction::ResumePrint(
                                crate::shared::workspace::PrintControlContext::new(
                                    epoch, generation,
                                ),
                            ));
                        } else {
                            app.request_pause_print(id, generation, pause_name.clone(), cx);
                        }
                    })),
                )
            })
            .when(state.controls.cancel, |row| {
                row.child(
                    Button::new(if in_detail {
                        gpui_kit::ElementId::from("overview-cancel")
                    } else {
                        ("bay-cancel", id.0 as usize).into()
                    })
                    .child(Icon::default().data(web_icons::STOP).size(px(14.)))
                    .danger()
                    .size(px(26.))
                    .p_0()
                    .rounded(px(7.))
                    .tooltip(cancel_label.clone())
                    .accessibility_label(cancel_label)
                    .disabled(disabled)
                    .on_click(cx.listener(move |app, _, _, cx| {
                        cx.stop_propagation();
                        app.request_cancel_print(id, generation, name.clone(), cx);
                    })),
                )
            })
        })
        .children(super::light_buttons(id, printer, cx))
}

fn elapsed(id: LocalPrinterId, job: &JobTelemetry, cx: &App) -> impl IntoElement {
    let value = job
        .print_elapsed_time_s
        .map_or_else(|| "—".to_owned(), short_duration);
    let label = t!(
        "printers.bay.metric",
        label = t!("printers.metrics.elapsed"),
        value = &value
    )
    .into_owned();
    h_flex()
        .id(("bay-elapsed", id.0 as usize))
        .test_support()
        .aria_label(label.clone())
        .flex_none()
        .max_w(px(124.))
        .min_w_0()
        .gap_1()
        .items_center()
        .text_size(px(12.))
        .font_medium()
        .line_height(px(12.))
        .h(px(13.))
        .text_color(cx.theme().muted_foreground)
        .child(
            Icon::default()
                .data(web_icons::TIME)
                .size(px(13.))
                .flex_none(),
        )
        .child(div().min_w_0().truncate().child(value))
        .app_tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx))
}

fn job_preview(
    id: LocalPrinterId,
    printer: &PrinterEntry,
    in_detail: bool,
    cx: &App,
) -> impl IntoElement {
    let state = &printer.state;
    let current = state.connection == LanConnection::Connected && state.telemetry_ready;
    let running = current
        && matches!(
            state.telemetry.print_state,
            PrintStatus::Printing | PrintStatus::Paused
        );
    let mut preview = h_flex()
        .id(("bay-preview", id.0 as usize))
        .test_support()
        .min_h(px(74.))
        .w_full()
        .flex_none()
        .min_w_0()
        .gap(px(20.));
    let Some(job) = display_job(state) else {
        return preview.child(thumbnail(printer, cx)).child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_1()
                .child(
                    div()
                        .id("overview-job-title")
                        .test_support()
                        .text_sm()
                        .font_semibold()
                        .truncate()
                        .child(bay_idle_title(state)),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .whitespace_normal()
                        .child(job_description(state)),
                ),
        );
    };
    let name = job
        .print_name
        .clone()
        .unwrap_or_else(|| bay_idle_title(state));
    let full_name = name.clone();
    let remaining = running.then_some(job.print_remaining_time_s).flatten();
    let description = if !current {
        job_description(state).to_owned()
    } else if let Some(seconds) = remaining {
        short_duration(seconds)
    } else if running {
        t!("printers.bay.no_remaining").into_owned()
    } else {
        super::super::format::print_status_label(job.print_state)
    };
    let timeline = h_flex()
        .w_full()
        .min_w_0()
        .min_h(px(20.6))
        .flex_wrap()
        .gap_2()
        .items_center()
        .child(
            h_flex()
                .flex_1()
                .min_w(px(100.))
                .gap(px(6.))
                .items_baseline()
                .when(!running && current, |row| {
                    row.child(
                        Icon::default()
                            .data(if job.print_state == PrintStatus::Completed {
                                web_icons::COMPLETED
                            } else {
                                web_icons::CANCELLED
                            })
                            .size(px(18.)),
                    )
                })
                .child(
                    div()
                        .id(("bay-description", id.0 as usize))
                        .test_support()
                        .min_w_0()
                        .truncate()
                        .line_height(px(20.6))
                        .when(remaining.is_some(), |text| {
                            text.text_size(px(18.)).font_semibold()
                        })
                        .when(remaining.is_none(), |text| text.text_size(px(12.)))
                        .child(description),
                )
                .when(remaining.is_some(), |row| {
                    row.child(
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(t!("printers.bay.left")),
                    )
                }),
        )
        .child(elapsed(id, job, cx));
    let alert = state
        .telemetry
        .printer_alerts
        .as_ref()
        .and_then(|alerts| alerts.first());
    let mut side = v_flex()
        .flex_1()
        .min_w_0()
        .min_h(px(74.))
        .gap_0()
        .child(
            div()
                .id(("bay-file-name", id.0 as usize))
                .test_support()
                .w_full()
                .h(px(16.1))
                .flex_none()
                .min_w_0()
                .truncate()
                .text_size(px(11.5))
                .line_height(px(16.1))
                .text_color(cx.theme().muted_foreground)
                .child(name)
                .app_tooltip(move |window, cx| Tooltip::new(full_name.clone()).build(window, cx)),
        )
        .when_some(alert, |column, alert| {
            let message = alert.message.clone();
            column.child(
                div()
                    .id(("bay-alert", id.0 as usize))
                    .h(px(16.))
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(cx.theme().yellow)
                    .child(message.clone())
                    .app_tooltip(move |window, cx| Tooltip::new(message.clone()).build(window, cx)),
            )
        })
        .child(div().flex_1().min_h_0())
        .child(timeline);
    if let Some(progress) = running.then_some(job.print_progress_pct).flatten() {
        side = side.child(
            h_flex()
                .w_full()
                .min_w_0()
                .h(px(12.))
                .mt(px(6.))
                .gap_2()
                .items_center()
                .child(
                    Progress::new(if in_detail {
                        gpui_kit::ElementId::from("overview-progress")
                    } else {
                        ("printer-progress", id.0 as usize).into()
                    })
                    .flex_1()
                    .color(thumbnail_accent(printer))
                    .track_color(cx.theme().secondary)
                    .track_height(6.)
                    .value(progress as f32)
                    .accessibility_label(
                        t!("printers.bay.progress", name = &printer.name).into_owned(),
                    ),
                )
                .child(
                    div()
                        .flex_none()
                        .text_xs()
                        .font_medium()
                        .line_height(px(12.))
                        .child(format!("{progress:.0}%")),
                ),
        );
    }
    preview = preview.child(thumbnail(printer, cx)).child(side);
    preview
}

fn footer(printer: &PrinterEntry, cx: &App) -> Div {
    let id = printer.state.id.0 as usize;
    let endpoint = t!(
        "printers.bay.endpoint",
        host = &printer.host,
        port = printer.port
    )
    .into_owned();
    let protocol = match printer.protocol {
        Protocol::Moonraker => "Klipper",
        Protocol::Flashforge => "FlashForge",
        Protocol::Bambulab => "Bambu Lab",
    };
    let protocol_port = t!(
        "printers.bay.protocol_port",
        protocol = protocol,
        port = printer.port
    )
    .into_owned();
    h_flex()
        .h(px(38.7))
        .rounded_b(px(15.))
        .w_full()
        .flex_none()
        .items_center()
        .px(px(18.))
        .bg(cx.theme().secondary)
        .border_t_1()
        .border_color(cx.theme().border)
        .child(
            h_flex()
                .id(("bay-footer", id))
                .test_support()
                .w_full()
                .h(px(21.7))
                .min_w_0()
                .gap_2()
                .items_center()
                .text_size(px(12.))
                .line_height(px(21.7))
                .text_color(cx.theme().muted_foreground)
                .child(
                    h_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_1()
                        .child(div().flex_none().child(t!("printers.bay.ip")))
                        .child(
                            div()
                                .id(("bay-endpoint", id))
                                .aria_label(endpoint.clone())
                                .test_support()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(cx.theme().foreground)
                                .child(printer.host.clone())
                                .app_tooltip(move |window, cx| {
                                    Tooltip::new(endpoint.clone()).build(window, cx)
                                }),
                        ),
                )
                .child(
                    div()
                        .id(("bay-protocol", id))
                        .aria_label(protocol_port.clone())
                        .test_support()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_center()
                        .child(protocol_port),
                )
                .child(
                    div()
                        .id(("bay-footer-status", id))
                        .aria_label(connection_footer(&printer.state))
                        .test_support()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_right()
                        .child(connection_footer(&printer.state)),
                ),
        )
}

impl MakerCalcGpuiApp {
    pub(in crate::gpui_app) fn render_card(
        id: LocalPrinterId,
        printer: &PrinterEntry,
        in_detail: bool,
        drag_preview: bool,
        details: Option<AnyElement>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let printer_name = div()
            .flex_1()
            .min_w_0()
            .truncate()
            .text_size(px(16.))
            .font_medium()
            .line_height(px(24.8))
            .child(printer.name.clone());
        let printer_name = if in_detail || drag_preview {
            printer_name.into_any_element()
        } else {
            Button::new(("bay-open-printer", id.0 as usize))
                .on_mouse_down(gpui_kit::MouseButton::Left, |_, _, cx| {
                    cx.stop_propagation()
                })
                .ghost()
                .bg(cx.theme().transparent)
                .flex_1()
                .min_w_0()
                .p_0()
                .h(px(24.8))
                .accessibility_label(t!("printers.bay.open", name = &printer.name).into_owned())
                .child(printer_name)
                .on_click(cx.listener(move |app, _, _, cx| {
                    cx.stop_propagation();
                    app.select_printer(id, cx);
                }))
                .into_any_element()
        };
        let age = printer.state.age_ms(std::time::Instant::now()).map(|ms| {
            let age = if ms < 60_000 {
                format!("{} s", ms / 1000)
            } else if ms < 3_600_000 {
                format!("{} min", ms / 60_000)
            } else {
                format!("{} h", ms / 3_600_000)
            };
            t!("printers.bay.updated", age = age).into_owned()
        });
        let printing = printer.state.connection == LanConnection::Connected
            && printer.state.telemetry.print_state == PrintStatus::Printing;
        let strip_color = thumbnail_accent(printer);
        let strip_border = cx.theme().border;
        let drag = BayDrag {
            id,
            owner: cx.weak_entity(),
        };
        let surface = card::surface(false, cx)
            .id(("printer-card", id.0 as usize))
            .test_support()
            .when(in_detail, |surface| surface.min_h(px(220.)))
            .when(!in_detail, |surface| surface.h(px(220.)))
            .font_family(super::super::theme::CARD_FONT)
            .font_weight(FontWeight::NORMAL)
            .font_features(FontFeatures(std::sync::Arc::new(vec![
                ("ss01".into(), 1),
                ("cv11".into(), 1),
            ])))
            .line_height(px(21.7))
            .border_1()
            .border_color(cx.theme().border)
            .shadow(Vec::new())
            .p_0()
            .gap_0()
            .rounded(px(16.))
            .flex_shrink_0()
            .when(!in_detail && !drag_preview, |surface| {
                // GPUI takes the drag cursor from the base style.
                surface
                    .cursor(CursorStyle::ClosedHand)
                    .hover(|style| {
                        style
                            .cursor(CursorStyle::OpenHand)
                            .border_color(cx.theme().muted_foreground)
                    })
                    .on_drag(drag, |drag, _, _, cx| drag.start(cx))
                    .on_drop(cx.listener(move |app, drag: &BayDrag, _, cx| {
                        if drag.owner.entity_id() == cx.entity_id() {
                            app.move_bay_printer(drag.id, id, cx);
                        }
                    }))
                    .on_click(cx.listener(move |app, _, _, cx| app.select_printer(id, cx)))
            });
        surface
            .child(
                div()
                    .id(("bay-accent-strip", id.0 as usize))
                    .test_support()
                    .h(px(3.))
                    .w_full()
                    .flex_none()
                    .child(
                        canvas(
                            |bounds, _, _| bounds,
                            move |bounds, _, window, _| {
                                let color = if printing { strip_color } else { strip_border };
                                window.with_content_mask(Some(ContentMask { bounds }), |window| {
                                    window.paint_quad(
                                        fill(
                                            Bounds::new(
                                                bounds.origin,
                                                size(bounds.size.width, px(218.)),
                                            ),
                                            color,
                                        )
                                        .corner_radii(px(15.)),
                                    );
                                });
                            },
                        )
                        .size_full(),
                    ),
            )
            .child(
                h_flex()
                    .id(("bay-header", id.0 as usize))
                    .test_support()
                    .h(px(26.))
                    .mt(px(12.))
                    .mx(px(18.))
                    .flex_none()
                    .min_w_0()
                    .gap(px(14.))
                    .items_center()
                    .child(printer_status(printer, cx))
                    .child(printer_name)
                    .when_some(age, |row, age| {
                        row.child(
                            div()
                                .flex_none()
                                .text_size(px(11.))
                                .font_medium()
                                .text_color(cx.theme().muted_foreground)
                                .child(age),
                        )
                    })
                    .child(controls(id, printer, in_detail, cx)),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .px(px(18.))
                    .py_2()
                    .gap_2()
                    .justify_center()
                    .child(job_preview(id, printer, in_detail, cx))
                    .child(bay_metrics(printer, in_detail, cx))
                    .children(details),
            )
            .child(footer(printer, cx))
    }
}
