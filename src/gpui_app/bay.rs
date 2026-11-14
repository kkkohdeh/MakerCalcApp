use super::components::button::Button;
use super::components::card;
use super::components::progress::Progress;
use super::components::scroll_area::ScrollAreaExt as _;
use super::components::tooltip::TooltipExt as _;
use crate::i18n::t;
use gpui_kit::base::motion::{Easing, Transition, transition};
use gpui_kit::base::{StyledExt as _, TestSupportExt as _};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, Icon, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, BoxShadow, Context, CursorStyle, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, StatefulInteractiveElement as _, Styled as _, WeakEntity, Window,
    div, point, px, rems, rgb,
};

use crate::config::Protocol;
use crate::shared::printer::PrintStatus;
use crate::shared::telemetry::LightRole;
use crate::shared::workspace::{DetailRequest, PrinterAction};
use crate::state::{LanConnection, LocalPrinterId, PrinterState};

use super::detail::DetailTab;
use super::format::{
    format_duration, format_percent, format_speed, format_temperature, job_description, layer_label,
};
use super::{MakerCalcGpuiApp, PrinterEntry};

mod printer_card;
mod web_icons;
pub(super) use web_icons::SPEED as SPEED_ICON;
pub(super) const FAN_ICON: &[u8] = web_icons::FAN;

const BAY_CARD_WIDTH: f32 = 560.;

pub(super) fn reconcile_order(
    order: &mut Vec<LocalPrinterId>,
    ids: impl Iterator<Item = LocalPrinterId>,
) {
    let ids: Vec<_> = ids.collect();
    order.retain(|id| ids.contains(id));
    for id in ids {
        if !order.contains(&id) {
            order.push(id);
        }
    }
}

#[derive(Clone)]
struct BayDrag {
    id: LocalPrinterId,
    owner: WeakEntity<MakerCalcGpuiApp>,
}

impl BayDrag {
    fn start(&self, cx: &mut App) -> gpui_kit::Entity<Self> {
        use gpui_kit::AppContext as _;

        let _ = self.owner.update(cx, |app, cx| {
            app.bay_dragging = Some(self.id);
            cx.notify();
        });
        cx.new(|cx| {
            let owner = self.owner.clone();
            let id = self.id;
            cx.on_release(move |_, cx| {
                let _ = owner.update(cx, |app, cx| {
                    if app.bay_dragging == Some(id) {
                        app.bay_dragging = None;
                        cx.notify();
                    }
                });
            })
            .detach();
            self.clone()
        })
    }
}

impl Render for BayDrag {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let card = self
            .owner
            .update(cx, |app, cx| {
                app.printers.get(&self.id).map(|printer| {
                    MakerCalcGpuiApp::render_card(self.id, printer, false, true, None, cx)
                        .into_any_element()
                })
            })
            .ok()
            .flatten();
        div().w(px(BAY_CARD_WIDTH)).children(card)
    }
}

pub(super) fn bay_columns(width: f32) -> usize {
    // Fixed-size cards, 16px gaps and 20px outer padding on each side.
    ((width - 2. * 20. + 16.) / (BAY_CARD_WIDTH + 16.))
        .floor()
        .max(1.) as usize
}

pub(super) fn bay_idle_title(state: &PrinterState) -> String {
    match state.connection {
        LanConnection::Disconnected => t!("printers.status.disconnected").into_owned(),
        LanConnection::Connecting => t!("printers.status.connecting").into_owned(),
        LanConnection::NeedsCredentials => t!("printers.status.credentials").into_owned(),
        LanConnection::Incompatible => t!("printers.status.incompatible").into_owned(),
        LanConnection::Connected => match state.telemetry.print_state {
            PrintStatus::Idle => t!("printers.status.ready").into_owned(),
            PrintStatus::Printing => t!("common.status.printing").into_owned(),
            PrintStatus::Paused => t!("printers.status.paused").into_owned(),
            PrintStatus::Completed => t!("printers.status.completed").into_owned(),
            PrintStatus::Cancelled => t!("printers.status.cancelled").into_owned(),
            PrintStatus::Error => t!("printers.status.error").into_owned(),
            PrintStatus::Unknown => t!("printers.status.unknown").into_owned(),
        },
    }
}

pub(super) fn bay_metrics(
    printer: &PrinterEntry,
    in_detail: bool,
    cx: &mut Context<MakerCalcGpuiApp>,
) -> AnyElement {
    let state = &printer.state;
    let telemetry = &state.telemetry;
    let is_current = state.connection == LanConnection::Connected && state.telemetry_ready;
    let job = printer_card::display_job(state);
    let consumed = job
        .and_then(|job| {
            let grams = job
                .print_filament_used_g
                .map(|grams| format!("{grams:.0} g"));
            let meters = job
                .print_filament_used_mm
                .map(|mm| format!("{:.1} m", mm / 1000.));
            match (grams, meters) {
                (Some(g), Some(m)) => Some(format!("{g} · {m}")),
                (g, m) => g.or(m),
            }
        })
        .unwrap_or_else(|| "—".into());
    let layers = if is_current {
        job.and_then(
            |job| match (job.print_current_layer, job.print_total_layers) {
                (Some(layer), Some(total)) => Some(format!("{layer}/{total}")),
                (Some(layer), None) => Some(layer.to_string()),
                (None, Some(total)) => Some(format!("—/{total}")),
                (None, None) => None,
            },
        )
    } else {
        None
    }
    .unwrap_or_else(|| "—".into());

    let values = [
        (
            t!("printers.metrics.nozzle"),
            format_temperature(telemetry.active_nozzle_temp_c()).replace(" °C", "°"),
        ),
        (
            t!("printers.metrics.bed"),
            format_temperature(telemetry.primary_bed_temp_c()).replace(" °C", "°"),
        ),
        (t!("printers.overview.layer"), layers),
        (
            t!("printers.metrics.speed"),
            format_speed(telemetry.velocity_mm_s),
        ),
        (
            t!("printers.metrics.fan"),
            format_percent(telemetry.primary_fan_power_pct()),
        ),
        (t!("printers.metrics.consumption"), consumed),
    ];
    let progress = job
        .and_then(|job| job.print_current_layer.zip(job.print_total_layers))
        .filter(|(_, total)| *total > 0)
        .map_or(0., |(current, total)| current as f32 / total as f32);
    let filament_color = telemetry.filament_supplies.as_ref().and_then(|supplies| {
        supplies
            .iter()
            .find(|supply| supply.is_active == Some(true))
            .or_else(|| supplies.first())
            .and_then(|supply| supply.color.as_deref())
            .and_then(super::materials::parse_hex_color)
    });
    let metrics = h_flex()
        .id(("bay-metrics", state.id.0 as usize))
        .test_support()
        .flex_none()
        .w_full()
        .min_w_0()
        .flex_wrap()
        .justify_between()
        .gap_x(px(4.))
        .gap_y(px(6.))
        .text_size(px(13.5))
        .font_medium()
        .line_height(px(17.55))
        .font_features(gpui_kit::FontFeatures(std::sync::Arc::new(vec![(
            "tnum".into(),
            1,
        )])))
        .text_color(cx.theme().foreground)
        .children(
            values
                .into_iter()
                .enumerate()
                .map(|(index, (label, value))| {
                    let editor = if in_detail {
                        match index {
                            0 => super::temperatures::target(
                                printer,
                                crate::shared::workspace::HeaterKind::Nozzle,
                                value.clone(),
                                cx,
                            ),
                            1 => super::temperatures::target(
                                printer,
                                crate::shared::workspace::HeaterKind::Bed,
                                value.clone(),
                                cx,
                            ),
                            _ => None,
                        }
                    } else {
                        None
                    };
                    let editable = editor.is_some();
                    let target = match index {
                        0 => telemetry.active_nozzle_target_temp_c(),
                        1 => telemetry.primary_bed_target_temp_c(),
                        _ => None,
                    }
                    .filter(|target| editable || *target > 0.)
                    .map(|target| format!("→{target:.0}°"));
                    let description = if index == 2 && job.is_some() {
                        layer_label(state)
                    } else {
                        t!(
                            "printers.bay.metric",
                            label = label,
                            value = format!("{value}{}", target.as_deref().unwrap_or(""))
                                .replace('°', " °C")
                        )
                        .into_owned()
                    };
                    let icon = match index {
                        2 => web_icons::layers(state.id, progress, cx),
                        4 => web_icons::fan(
                            state.id,
                            telemetry.primary_fan_power_pct().unwrap_or(0.),
                            is_current,
                            cx,
                        ),
                        _ => Icon::default()
                            .data(match index {
                                0 => web_icons::NOZZLE,
                                1 => web_icons::BED,
                                3 => web_icons::SPEED,
                                _ => web_icons::FILAMENT,
                            })
                            .size(px(14.))
                            .text_color(if index == 5 {
                                filament_color.unwrap_or(cx.theme().muted_foreground)
                            } else {
                                cx.theme().muted_foreground
                            })
                            .into_any_element(),
                    };
                    h_flex()
                        .min_w_0()
                        .max_w(px(142.))
                        .flex_none()
                        .items_center()
                        .gap(px(4.))
                        .child(
                            div()
                                .id(("bay-metric-icon", index))
                                .test_support()
                                .size(px(14.))
                                .flex_none()
                                .child(icon),
                        )
                        .child(
                            h_flex()
                                .id(("bay-metric", index))
                                .aria_label(description.clone())
                                .test_support()
                                .min_w_0()
                                .max_w(px(124.))
                                .when(in_detail, |metric| metric.items_center().h(px(22.)))
                                .when(!in_detail, |metric| metric.items_baseline())
                                .truncate()
                                .when(!editable, |metric| metric.child(div().child(value)))
                                .children(editor)
                                .when_some(target, |value, target| {
                                    value.child(
                                        div()
                                            .id(("bay-metric-target", index))
                                            .test_support()
                                            .ml(px(3.))
                                            .text_size(px(11.5))
                                            .text_color(cx.theme().muted_foreground)
                                            .child(target),
                                    )
                                })
                                .when(index >= 2, |metric| {
                                    metric.app_tooltip(move |window, cx| {
                                        Tooltip::new(description.clone()).build(window, cx)
                                    })
                                }),
                        )
                        .into_any_element()
                }),
        );

    if !is_current {
        let status_text = retained_reading_label(
            state,
            std::time::Instant::now(),
            rust_i18n::locale().as_ref(),
        );
        v_flex()
            .flex_none()
            .w_full()
            .min_w_0()
            .items_center()
            .gap_1()
            .child(
                div()
                    .id(("bay-reading-label", state.id.0 as usize))
                    .aria_label(status_text.clone())
                    .test_support()
                    .text_xs()
                    .line_height(px(16.))
                    .text_color(cx.theme().muted_foreground)
                    .child(status_text),
            )
            .child(metrics)
            .into_any_element()
    } else {
        metrics.into_any_element()
    }
}

pub(super) fn retained_reading_label(
    state: &PrinterState,
    now: std::time::Instant,
    locale: &str,
) -> String {
    let telemetry = &state.telemetry;
    let has_readings = telemetry.active_nozzle_temp_c().is_some()
        || telemetry.primary_bed_temp_c().is_some()
        || telemetry.velocity_mm_s.is_some()
        || telemetry.primary_fan_power_pct().is_some();
    if !has_readings {
        t!("printers.bay.no_current", locale = locale).into_owned()
    } else if let Some(age) = state.age_ms(now) {
        t!(
            "printers.bay.retained_age",
            locale = locale,
            age = format_duration(age as f64 / 1000.)
        )
        .into_owned()
    } else {
        t!("printers.bay.retained", locale = locale).into_owned()
    }
}

fn light_buttons(id: LocalPrinterId, printer: &PrinterEntry, cx: &App) -> Vec<AnyElement> {
    if !printer.operation_state.allows_actions() {
        return Vec::new();
    }
    printer
        .state
        .telemetry
        .lights
        .iter()
        .filter(|light| printer.state.controls.allows_light(light.role))
        .map(|light| {
            let role = light.role;
            let expected_enabled = light.enabled;
            let connection_epoch = printer.state.connection_epoch;
            let sender = printer.actions.clone();
            let (key, label) = match (role, light.enabled) {
                (LightRole::BuildArea, Some(true)) => {
                    ("bay-area", t!("printers.lights.build_area.on").into_owned())
                }
                (LightRole::BuildArea, Some(false)) => (
                    "bay-area",
                    t!("printers.lights.build_area.off").into_owned(),
                ),
                (LightRole::BuildArea, None) => (
                    "bay-area",
                    t!("printers.lights.build_area.unknown").into_owned(),
                ),
                (LightRole::Toolhead, Some(true)) => (
                    "bay-toolhead",
                    t!("printers.lights.toolhead.on").into_owned(),
                ),
                (LightRole::Toolhead, Some(false)) => (
                    "bay-toolhead",
                    t!("printers.lights.toolhead.off").into_owned(),
                ),
                (LightRole::Toolhead, None) => (
                    "bay-toolhead",
                    t!("printers.lights.toolhead.unknown").into_owned(),
                ),
            };
            let enabled = light.enabled == Some(true);
            let icon_color = if enabled {
                rgb(0xffffff).into()
            } else {
                cx.theme().muted_foreground
            };
            let disabled = !printer.state.telemetry_ready || printer.operation_state.is_pending();
            let button_id: gpui_kit::ElementId = (key, id.0 as usize).into();
            Button::new(button_id.clone())
                .ghost()
                .text_color(icon_color)
                .hover(|style| {
                    style.bg(if !disabled {
                        cx.theme().secondary
                    } else {
                        cx.theme().transparent
                    })
                })
                .bg(cx.theme().transparent)
                .p_0()
                .border_1()
                .border_color(cx.theme().border)
                .child(
                    div()
                        .id((button_id, "hover-outline"))
                        .test_support()
                        .size_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(6.))
                        .child(web_icons::light(id, role, light.enabled, cx)),
                )
                .tooltip(label.clone())
                .accessibility_label(label)
                .toggled(light.enabled)
                .size(px(26.))
                .rounded(px(7.))
                .disabled(disabled)
                .on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    let _ = sender.try_send(PrinterAction::ToggleLight {
                        role,
                        expected_enabled,
                        connection_epoch,
                        submitted_at: std::time::Instant::now(),
                    });
                })
                .into_any_element()
        })
        .collect()
}

pub(super) fn connection_label(state: &PrinterState, cx: &App) -> (String, Hsla) {
    match state.connection {
        LanConnection::Connected => match state.telemetry.print_state {
            PrintStatus::Idle => (
                t!("common.status.idle").into_owned(),
                cx.theme().muted_foreground,
            ),
            PrintStatus::Printing => (t!("common.status.printing").into_owned(), cx.theme().blue),
            PrintStatus::Paused => (t!("common.status.paused").into_owned(), cx.theme().yellow),
            PrintStatus::Completed => (
                t!("common.status.completed").into_owned(),
                cx.theme().yellow,
            ),
            PrintStatus::Cancelled => (
                t!("common.status.cancelled").into_owned(),
                cx.theme().muted_foreground,
            ),
            PrintStatus::Error => (t!("common.status.error").into_owned(), cx.theme().red),
            PrintStatus::Unknown => (
                t!("common.status.unknown").into_owned(),
                cx.theme().muted_foreground,
            ),
        },
        LanConnection::Connecting => (
            t!("printers.status.connecting_short").into_owned(),
            cx.theme().yellow,
        ),
        LanConnection::Disconnected => (
            t!("printers.status.disconnected").into_owned(),
            cx.theme().red,
        ),
        LanConnection::NeedsCredentials => (
            t!("printers.status.credentials_short").into_owned(),
            cx.theme().yellow,
        ),
        LanConnection::Incompatible => (
            t!("common.status.incompatible").into_owned(),
            cx.theme().red,
        ),
    }
}

pub(super) fn connection_footer(state: &PrinterState) -> String {
    if state.connection == LanConnection::Connected {
        t!("printers.bay.online").into_owned()
    } else {
        t!("printers.bay.offline").into_owned()
    }
}

pub(super) fn thumbnail_accent(printer: &PrinterEntry) -> Hsla {
    rgb(printer
        .thumbnail
        .as_ref()
        .filter(|bytes| super::media::image_format(bytes).is_some())
        .and(printer.thumbnail_color)
        .unwrap_or(crate::shared::thumbnail::color::FALLBACK))
    .into()
}

pub(super) fn printer_status(printer: &PrinterEntry, cx: &App) -> AnyElement {
    let id = printer.state.id.0 as usize;
    let (label, color) = connection_label(&printer.state, cx);
    if printer.state.connection == LanConnection::Connected
        && printer.state.telemetry.print_state == PrintStatus::Printing
    {
        div()
            .id(("printing-state", id))
            .test_support()
            .text_xs()
            .font_medium()
            .line_height(px(18.6))
            .text_color(thumbnail_accent(printer))
            .child(label)
            .into_any_element()
    } else {
        let color = if printer.state.connection == LanConnection::Connected
            && printer.state.telemetry.print_state == PrintStatus::Paused
        {
            thumbnail_accent(printer)
        } else {
            color
        };
        h_flex()
            .gap_2()
            .items_center()
            .child(
                div()
                    .id(("printer-status-dot", id))
                    .test_support()
                    .size(rems(0.375))
                    .rounded_full()
                    .bg(color),
            )
            .child(
                div()
                    .id(("printer-status-badge", id))
                    .test_support()
                    .text_xs()
                    .line_height(px(18.6))
                    .font_medium()
                    .whitespace_nowrap()
                    .text_color(color)
                    .child(label),
            )
            .into_any_element()
    }
}

impl MakerCalcGpuiApp {
    fn move_bay_printer(
        &mut self,
        id: LocalPrinterId,
        target: LocalPrinterId,
        cx: &mut Context<Self>,
    ) {
        reconcile_order(&mut self.bay_order, self.printers.keys().copied());
        if self.mutation_pending || id == target {
            return;
        }
        let Some(from) = self.bay_order.iter().position(|value| *value == id) else {
            return;
        };
        let Some(to) = self.bay_order.iter().position(|value| *value == target) else {
            return;
        };
        let (reply, response) = tokio::sync::oneshot::channel();
        if self
            .edge_actions
            .try_send(crate::edge::EdgeCommand {
                action: crate::edge::EdgeAction::MovePrinter { id, target },
                reply,
            })
            .is_err()
        {
            self.push_toast(
                super::feedback::ToastKind::Error,
                t!("printers.bay.order_save_failed").into_owned(),
                None,
                cx,
            );
            return;
        }
        let previous_order = self.bay_order.clone();
        self.bay_order.remove(from);
        self.bay_order.insert(to, id);
        self.mutation_pending = true;
        cx.notify();
        cx.spawn(async move |app, cx| {
            let saved = matches!(response.await, Ok(Ok(())));
            let _ = app.update(cx, |app, cx| {
                app.mutation_pending = false;
                if !saved {
                    app.bay_order = previous_order;
                    reconcile_order(&mut app.bay_order, app.printers.keys().copied());
                    app.push_toast(
                        super::feedback::ToastKind::Error,
                        t!("printers.bay.order_save_failed").into_owned(),
                        None,
                        cx,
                    );
                }
                app.notify_if_visible(cx);
            });
        })
        .detach();
    }
    pub(super) fn select_printer(&mut self, id: LocalPrinterId, cx: &mut Context<Self>) {
        self.bay_hovered = None;
        self.files_list
            .update(cx, |table, _| table.delegate_mut().suspend_reads());
        if let Some(previous) = self.selected
            && let Some(printer) = self.printers.get(&previous)
        {
            let _ = printer.detail.active.send(false);
        }
        self.selected = Some(id);
        self.selected_file = None;
        self.detail_tab = DetailTab::Overview;
        if let Some(printer) = self.printers.get(&id) {
            let _ = printer.detail.active.send(true);
            let _ = printer
                .detail
                .requests
                .try_send(DetailRequest::RefreshMachine);
            let _ = printer
                .detail
                .requests
                .try_send(DetailRequest::HistoryTotals);
        }
        cx.notify();
    }

    pub(super) fn render_bay(
        &self,
        columns: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut cards = self
            .bay_order
            .iter()
            .chain(
                self.printers
                    .keys()
                    .filter(|id| !self.bay_order.contains(id)),
            )
            .filter_map(|id| self.printers.get(id).map(|printer| (*id, printer)))
            .map(|(id, printer)| {
                let hover = transition(
                    (
                        gpui_kit::ElementId::from(("bay-slot", id.0 as usize)),
                        "shadow",
                    ),
                    if self.bay_hovered == Some(id) {
                        1_f32
                    } else {
                        0_f32
                    },
                    Transition::new(std::time::Duration::from_millis(180)).easing(Easing::Ease),
                    window,
                    cx,
                );
                let dragging = self.bay_dragging == Some(id);
                div()
                    .id(("bay-slot", id.0 as usize))
                    .test_support()
                    .relative()
                    .rounded(px(16.))
                    .when(dragging, |slot| slot.opacity(0.45))
                    .shadow(vec![
                        BoxShadow {
                            inset: false,
                            color: rgb(0).opacity(0.65 * hover).into(),
                            offset: point(px(0.), px(40.)),
                            blur_radius: px(90.),
                            spread_radius: px(-18.),
                        },
                        BoxShadow {
                            inset: false,
                            color: rgb(0).opacity(0.45 * hover).into(),
                            offset: point(px(0.), px(8.)),
                            blur_radius: px(24.),
                            spread_radius: px(-8.),
                        },
                    ])
                    .on_hover(cx.listener(move |app, hovered, _, cx| {
                        if *hovered {
                            app.bay_hovered = Some(id);
                        } else if app.bay_hovered == Some(id) {
                            app.bay_hovered = None;
                        }
                        cx.notify();
                    }))
                    .child(Self::render_card(id, printer, false, false, None, cx))
                    .child(
                        div()
                            .id(("bay-drop-outline", id.0 as usize))
                            .test_support()
                            .absolute()
                            .inset(px(-5.))
                            .rounded(px(21.))
                            .border_2()
                            .border_dashed()
                            .border_color(cx.theme().transparent)
                            .drag_over::<BayDrag>(move |style, drag, _, cx| {
                                if drag.id != id {
                                    style.border_color(cx.theme().ring)
                                } else {
                                    style
                                }
                            }),
                    )
                    .into_any_element()
            })
            .collect::<Vec<_>>();
        cards.push(
            Button::new("new-printer")
                .ghost()
                .bg(cx.theme().background)
                .hover(|style| style.bg(cx.theme().group_box))
                .text_color(cx.theme().muted_foreground)
                .h(px(220.))
                .w_full()
                .rounded(px(card::RADIUS))
                .border_1()
                .border_dashed()
                .border_color(cx.theme().border)
                .child(
                    v_flex()
                        .items_center()
                        .gap_4()
                        .child(div().text_2xl().child("+"))
                        .child(div().text_sm().child(t!("printers.bay.add"))),
                )
                .on_click(cx.listener(|app, _, window, cx| app.open_new_printer(window, cx)))
                .into_any_element(),
        );

        v_flex()
            .id("printer-bay")
            .size_full()
            .px_5()
            .pt_3()
            .pb_5()
            .when(self.printers.len() > 1, |bay| bay.items_center())
            .scroll_area()
            .child(
                div()
                    .id("printer-bay-grid")
                    .test_support()
                    .grid()
                    .w(px(columns as f32 * (BAY_CARD_WIDTH + 16.) - 16.))
                    .flex_shrink_0()
                    .grid_cols(columns as u16)
                    .gap_4()
                    .children(cards),
            )
            .into_any_element()
    }
}
