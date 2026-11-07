use super::components::button::Button;
use super::components::progress::Progress;
use crate::i18n::t;
use gpui_kit::base::{StyledExt as _, TestSupportExt as _};
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ElementId, Hsla, InteractiveElement as _, IntoElement, ParentElement as _,
    Role, StatefulInteractiveElement as _, Styled as _, div, px,
};

use crate::shared::workspace::{CameraRequest, DeviceKind, ResourceId};
use crate::state::LanConnection;

use super::PrinterEntry;
use super::format::format_percent;
use super::overview::{DeviceFacts, FanFact};

pub(super) fn motion_position(
    printer: &PrinterEntry,
) -> Option<([f64; 3], ResourceId, &'static str)> {
    let devices =
        || {
            printer.detail_state.devices.iter().filter(|device| {
                matches!(device.kind, DeviceKind::Motion | DeviceKind::MotionSettings)
            })
        };
    devices()
        .find_map(|device| Some((device.position_mm?, device.id, "position")))
        .or_else(|| {
            devices().find_map(|device| {
                Some((
                    device.readings.motion.as_ref()?.commanded_position_mm?,
                    device.id,
                    "commanded_position",
                ))
            })
        })
        .or_else(|| {
            devices().find_map(|device| {
                Some((
                    device.readings.motion.as_ref()?.work_position_mm?,
                    device.id,
                    "work_position",
                ))
            })
        })
}

pub(super) fn printer_serial_number(printer: &PrinterEntry) -> String {
    if let Some(suffix) = printer.metadata.serial_suffix.as_deref()
        && !suffix.trim().is_empty()
    {
        return t!("printers.device.serial_suffix", suffix = suffix).into_owned();
    }
    t!("printers.device.not_reported").into_owned()
}

/// Visual weight of a reading. It follows the datum's fixed importance, never its value.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum Level {
    Primary,
    #[default]
    Secondary,
}

/// One normalized reading as presented in Summary.
#[derive(Clone)]
pub(super) struct Reading {
    pub(super) label: String,
    pub(super) icon: Option<&'static [u8]>,
    pub(super) value: String,
    /// Secondary value next to the main one, such as a heater target.
    pub(super) detail: Option<String>,
    /// Complete text for assistive technologies.
    pub(super) spoken: String,
    pub(super) progress: Option<f32>,
    /// Series color when the reading is also plotted in the temperature history.
    pub(super) legend: Option<Hsla>,
    pub(super) level: Level,
    /// Retained instead of live: same place, dimmed value.
    pub(super) muted: bool,
}

impl Reading {
    pub(super) fn new(label: String, value: String) -> Self {
        Self {
            label,
            icon: None,
            spoken: value.clone(),
            value,
            detail: None,
            progress: None,
            legend: None,
            level: Level::default(),
            muted: false,
        }
    }

    pub(super) fn temperature(label: String, current: Option<f64>, target: Option<f64>) -> Self {
        // A zero target means the heater is off: there is nothing to approach.
        let active_target = target.filter(|target| *target > 0.);
        let value = current.map_or_else(|| "—".to_owned(), |value| format!("{value:.0}°"));
        Self {
            spoken: super::format::format_temperature_with_target(current, target),
            detail: target.map(|target| {
                if target > 0. {
                    format!("/ {target:.0}°")
                } else {
                    t!("printers.overview.off").into_owned()
                }
            }),
            progress: current
                .zip(active_target)
                .map(|(current, target)| (current / target * 100.).clamp(0., 100.) as f32),
            ..Self::new(label, value)
        }
    }
}

pub(super) fn reading_cell(id: ElementId, reading: Reading, cx: &App) -> AnyElement {
    let description = t!(
        "printers.bay.metric",
        label = &reading.label,
        value = &reading.spoken
    )
    .into_owned();
    let progress_id = (id.clone(), "progress");
    v_flex()
        .id(id)
        .role(Role::Group)
        .aria_label(description.clone())
        .test_support()
        .gap_2()
        .child(
            h_flex()
                .gap_1p5()
                .items_center()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .when_some(reading.icon, |row, icon| {
                    row.child(
                        Icon::default()
                            .data(icon)
                            .size_3p5()
                            .flex_none()
                            .text_color(cx.theme().muted_foreground),
                    )
                })
                .child(div().min_w_0().truncate().child(reading.label))
                .when_some(reading.legend, |row, color| {
                    row.child(div().flex_none().size_2().rounded_full().bg(color))
                }),
        )
        .child(
            h_flex()
                .items_baseline()
                .gap_1()
                .child(
                    div()
                        .font_semibold()
                        .truncate()
                        .map(|value| match reading.level {
                            Level::Primary => value.text_3xl().line_height(gpui_kit::relative(1.2)),
                            Level::Secondary => value.text_lg(),
                        })
                        .when(reading.muted, |value| {
                            value.text_color(cx.theme().muted_foreground)
                        })
                        .child(reading.value),
                )
                .when_some(reading.detail, |row, detail| {
                    row.child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(detail),
                    )
                }),
        )
        // The track keeps its height without a value so readings in one row stay aligned.
        .child(div().h(px(4.)).when_some(reading.progress, |track, value| {
            track.child(
                Progress::new(progress_id)
                    .value(value)
                    .accessibility_label(description),
            )
        }))
        .into_any_element()
}

pub(super) struct Row {
    pub(super) label: String,
    pub(super) value: String,
    /// A control after the value, such as the camera refresh.
    pub(super) action: Option<AnyElement>,
}

impl Row {
    fn text(label: String, value: String) -> Self {
        Self {
            label,
            value,
            action: None,
        }
    }
}

/// Label each fan by role and number repeated labels.
/// An on switch is not 100%: show on/off without power and include reported RPM.
pub(super) fn fan_rows(fans: &[FanFact]) -> Vec<Row> {
    let labels = fans.iter().map(|fan| fan.name.clone()).collect::<Vec<_>>();
    fans.iter()
        .enumerate()
        .map(|(index, fan)| {
            let same = |label: &&String| **label == labels[index];
            let label = if labels.iter().filter(same).count() > 1 {
                t!(
                    "printers.overview.numbered",
                    label = &labels[index],
                    number = labels[..=index].iter().filter(same).count()
                )
                .into_owned()
            } else {
                labels[index].clone()
            };
            let state = match (fan.power_pct, fan.enabled) {
                (Some(power), _) => Some(format_percent(Some(power))),
                (None, Some(true)) => Some(t!("printers.overview.on").into_owned()),
                (None, Some(false)) => Some(t!("printers.overview.off").into_owned()),
                (None, None) => None,
            };
            let speed = fan.rpm.map(|rpm| format!("{rpm:.0} RPM"));
            let value = match (state, speed) {
                (Some(state), Some(speed)) => format!("{state} · {speed}"),
                (state, speed) => state.or(speed).unwrap_or_default(),
            };
            Row::text(label, value)
        })
        .collect()
}

/// Device-card rows include unknown camera support.
/// Show refresh only for a reported camera.
pub(super) fn detail_rows(
    printer: &PrinterEntry,
    device: &DeviceFacts<'_>,
    camera: Option<ResourceId>,
) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut text = |label: String, value: Option<String>| {
        if let Some(value) = value {
            rows.push(Row::text(label, value));
        }
    };
    let model = match (device.manufacturer, device.model) {
        (Some(manufacturer), Some(model)) => Some(format!("{manufacturer} {model}")),
        (manufacturer, model) => model.or(manufacturer).map(str::to_owned),
    };
    text(t!("printers.device.model").into_owned(), model);
    text(
        t!("printers.overview.firmware").into_owned(),
        device.firmware.map(str::to_owned),
    );
    text(
        t!("printers.overview.server").into_owned(),
        device.server.map(str::to_owned),
    );
    text(
        t!("printers.overview.firmware_state").into_owned(),
        device.firmware_ready.map(|ready| {
            if ready {
                t!("common.status.ready").into_owned()
            } else {
                t!("printers.details.not_ready").into_owned()
            }
        }),
    );
    text(
        t!("printers.overview.nozzle_count").into_owned(),
        device.nozzle_count.map(|count| count.to_string()),
    );
    text(
        t!("printers.overview.build_volume").into_owned(),
        device
            .build_volume_mm
            .map(|[x, y, z]| format!("{x:.0} × {y:.0} × {z:.0} mm")),
    );
    text(
        t!("printers.overview.location").into_owned(),
        device.location.map(str::to_owned),
    );
    text(
        t!("printers.device.serial").into_owned(),
        device.serial_suffix.map(|_| printer_serial_number(printer)),
    );
    rows.extend(super::overview::readings::presentation::rows(
        &device.extra,
        rust_i18n::locale().as_ref(),
    ));
    if device.camera_retry {
        let sender = printer.camera.requests.clone();
        rows.push(Row {
            label: t!("printers.overview.camera").into_owned(),
            value: t!("printers.camera.not_detected").into_owned(),
            action: camera.map(|id| {
                Button::new("overview-camera-refresh")
                    .ghost()
                    .xsmall()
                    .label(t!("common.actions.refresh").into_owned())
                    .disabled(printer.state.connection != LanConnection::Connected)
                    .on_click(move |_, _, _| {
                        let _ = sender.try_send(CameraRequest::Load { id });
                    })
                    .into_any_element()
            }),
        });
    }
    rows
}
