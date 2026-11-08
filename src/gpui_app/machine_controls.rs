//! Local controls driven exclusively by normalized capabilities and resource handles.
use std::collections::BTreeMap;

use super::components::{alert_dialog::backdrop, button::Button, dialog, input::Input};
use super::feedback::ToastKind;
use super::{MakerCalcGpuiApp, PrinterEntry};
use crate::i18n::t;
use crate::shared::control::{
    Axis, CalibrationKind, ControlError, ControlResult, MachineAction, MachineActionKind as K,
};
use crate::shared::machine::MachineInfo;
use crate::shared::workspace::ResourceId;
use crate::state::{LanConnection, LocalPrinterId, MachineJobTarget};
use gpui_kit::base::{FocusTrapElement as _, StyledExt as _, TestSupportExt as _};
use gpui_kit::component::input::InputState;
use gpui_kit::component::slider::{Slider, SliderState};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, FocusHandle, InteractiveElement as _,
    IntoElement, ParentElement as _, Styled as _, Task, Window, div, px,
};

mod intent;
mod motion;
mod toolbar;

use intent::action;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Control {
    Speed,
    ZOffset,
    Fan(ResourceId),
    FanEnabled(ResourceId, bool),
    Camera(ResourceId, bool),
    Calibration(CalibrationKind),
    Home,
    Move(Axis),
    Extrude(ResourceId),
    ClearPlate,
    AutoShutdown(bool),
    Name,
    Acknowledge(ResourceId),
}

impl Control {
    fn requires_idle(self) -> bool {
        matches!(
            self,
            Self::Home | Self::Move(_) | Self::Extrude(_) | Self::Calibration(_) | Self::ClearPlate
        )
    }
}

#[derive(Clone)]
struct Choice {
    label: String,
    control: Control,
    value: Option<String>,
}

fn choices(printer: &PrinterEntry) -> Vec<Choice> {
    let machine = &printer.detail_state.machine;
    let mut choices = Vec::new();
    let mut add = |key, kind, control, value| {
        if machine.actions.contains(&kind) {
            choices.push(Choice {
                label: label(key),
                control,
                value,
            });
        }
    };
    if speed_range(printer).is_some() {
        add("speed", K::SetPrintSpeedOverride, Control::Speed, None);
    }
    if z_offset_range(machine).is_some() {
        add(
            "z_offset",
            K::SetZOffset,
            Control::ZOffset,
            Some(z_offset_draft(machine)),
        );
    }
    add("home", K::HomeAxes, Control::Home, None);
    add(
        "clear_plate",
        K::ConfirmBuildPlateCleared,
        Control::ClearPlate,
        None,
    );
    let auto_shutdown_enabled = machine.auto_shutdown_enabled == Some(true);
    add(
        if auto_shutdown_enabled {
            "auto_shutdown_off"
        } else {
            "auto_shutdown_on"
        },
        K::SetAutoShutdown,
        Control::AutoShutdown(!auto_shutdown_enabled),
        (!auto_shutdown_enabled).then(|| "1800".into()),
    );
    add(
        "name",
        K::SetDeviceName,
        Control::Name,
        machine.device.reported_name.clone().or(Some(String::new())),
    );
    for kind in &machine.calibrations {
        add(
            match kind {
                CalibrationKind::BedLeveling => "level_bed",
                CalibrationKind::VibrationCompensation => "calibrate_vibration",
            },
            K::RunCalibration,
            Control::Calibration(*kind),
            None,
        );
    }
    if machine.actions.contains(&K::MoveAxisRelative) {
        for (axis, name) in [(Axis::X, "X"), (Axis::Y, "Y"), (Axis::Z, "Z")] {
            choices.push(Choice {
                label: t!("printers.controls.move_axis", axis = name).into_owned(),
                control: Control::Move(axis),
                value: Some("10".into()),
            });
        }
    }
    for device in &printer.detail_state.devices {
        for (kind, key, control, value) in [
            (
                K::SetFanPower,
                "fan_power",
                Control::Fan(device.id),
                Some(device.output_pct.unwrap_or(0.0).to_string()),
            ),
            (
                K::SetFanEnabled,
                "fan_switch",
                Control::FanEnabled(device.id, !device.enabled.unwrap_or(false)),
                None,
            ),
            (
                K::ExtrudeFilament,
                "extrude",
                Control::Extrude(device.id),
                Some("10".into()),
            ),
        ] {
            if device.actions.contains(&kind)
                && (kind != K::SetFanEnabled || device.enabled.is_some())
            {
                choices.push(Choice {
                    label: t!(
                        "printers.controls.resource",
                        action = label(key),
                        resource = super::overview::readings::presentation::device_name(
                            device,
                            &printer.detail_state.devices,
                            rust_i18n::locale().as_ref()
                        )
                    )
                    .into_owned(),
                    control,
                    value,
                });
            }
        }
    }
    if machine.actions.contains(&K::SetCameraStreamEnabled) {
        for camera in printer.detail_state.cameras.value.iter().flatten() {
            if let Some(enabled) = camera.enabled {
                choices.push(Choice {
                    label: label(if enabled { "camera_off" } else { "camera_on" }),
                    control: Control::Camera(camera.id, !enabled),
                    value: None,
                });
            }
        }
    }
    if machine.actions.contains(&K::AcknowledgePrinterAlert) {
        for id in &machine.alerts {
            choices.push(Choice {
                label: label("acknowledge"),
                control: Control::Acknowledge(*id),
                value: None,
            });
        }
    }
    choices
}

fn speed_range(printer: &PrinterEntry) -> Option<[u16; 2]> {
    let machine = &printer.detail_state.machine;
    if !machine.actions.contains(&K::SetPrintSpeedOverride) {
        return None;
    }
    machine.speed_override_range_pct.filter(|[min, max]| {
        min < max
            && MachineAction::SetPrintSpeedOverride { percent: *min }
                .validate()
                .is_ok()
            && MachineAction::SetPrintSpeedOverride { percent: *max }
                .validate()
                .is_ok()
    })
}

fn observed_speed(printer: &PrinterEntry) -> Option<f64> {
    printer
        .detail_state
        .devices
        .iter()
        .filter(|device| {
            device.motion_source == Some(crate::shared::machine::MotionSource::CommandInterpreter)
        })
        .find_map(|device| device.readings.motion.as_ref()?.speed_override_pct)
        .or_else(|| {
            printer
                .state
                .telemetry
                .current_job
                .as_ref()?
                .print_speed_override_pct
        })
        .filter(|value| value.is_finite())
}

fn z_offset_range(machine: &MachineInfo) -> Option<[f64; 2]> {
    machine.z_offset_range_mm.filter(|[min, max]| {
        machine.actions.contains(&K::SetZOffset) && min.is_finite() && max.is_finite() && min < max
    })
}

fn z_offset_draft(machine: &MachineInfo) -> String {
    machine
        .z_offset_mm
        .map(|value| localized_decimal(&value.to_string()))
        .unwrap_or_default()
}

fn availability(
    printer: &PrinterEntry,
    control: Control,
    pending: bool,
) -> Result<(), ControlError> {
    if printer.state.connection != LanConnection::Connected {
        return Err(ControlError::Disconnected);
    }
    if !printer.state.telemetry_ready
        || !printer.detail_state.live
        || !printer
            .state
            .age_ms(std::time::Instant::now())
            .is_some_and(|age| age <= 15_000)
    {
        return Err(ControlError::InvalidState);
    }
    if pending || printer.operation_state.is_pending() {
        return Err(ControlError::Busy);
    }
    if control == Control::ClearPlate
        && !printer
            .state
            .telemetry
            .print_state
            .is_completed_or_cancelled()
    {
        return Err(ControlError::InvalidState);
    }
    if control == Control::ClearPlate {
        match printer.detail_state.machine.material_motion_idle {
            Some(true) => {}
            Some(false) => return Err(ControlError::Busy),
            None => return Err(ControlError::InvalidState),
        }
    }
    if matches!(control, Control::Name | Control::AutoShutdown(_))
        && printer.material_pending.is_some()
    {
        return Err(ControlError::Busy);
    }
    if let Control::AutoShutdown(enabled) = control {
        printer
            .detail_state
            .machine
            .auto_shutdown_availability(printer.state.telemetry.print_state, enabled)?;
    }
    if control == Control::Name {
        printer
            .detail_state
            .machine
            .idle_setting_availability(printer.state.telemetry.print_state)?;
    }
    if matches!(control, Control::Calibration(_)) {
        printer
            .detail_state
            .machine
            .calibration_readiness
            .ok_or(ControlError::InvalidState)?
            .check()?;
    }
    if control.requires_idle() {
        motion::availability(printer, false).map_err(|_| ControlError::InvalidState)?;
    }
    if matches!(control, Control::Speed | Control::ZOffset)
        && !printer
            .detail_state
            .machine
            .allows_print_adjustment(printer.state.telemetry.print_state)
    {
        return Err(ControlError::InvalidState);
    }
    Ok(())
}

fn label(key: &str) -> String {
    match key {
        "speed" => t!("printers.controls.speed"),
        "z_offset" => t!("printers.controls.z_offset"),
        "home" => t!("printers.controls.home"),
        "clear_plate" => t!("printers.controls.clear_plate"),
        "auto_shutdown_on" => t!("printers.controls.auto_shutdown_on"),
        "auto_shutdown_off" => t!("printers.controls.auto_shutdown_off"),
        "name" => t!("printers.controls.name"),
        "level_bed" => t!("printers.controls.level_bed"),
        "calibrate_vibration" => t!("printers.controls.calibrate_vibration"),
        "fan_power" => t!("printers.controls.fan_power"),
        "fan_switch" => t!("printers.controls.fan_switch"),
        "extrude" => t!("printers.controls.extrude"),
        "camera_off" => t!("printers.controls.camera_off"),
        "camera_on" => t!("printers.controls.camera_on"),
        "acknowledge" => t!("printers.controls.acknowledge"),
        _ => unreachable!("closed control label set"),
    }
    .into_owned()
}

#[derive(Default)]
pub(super) struct MachineControls {
    editor: Option<Editor>,
    rendered: bool,
    tasks: BTreeMap<LocalPrinterId, Task<()>>,
    motion: motion::State,
}

impl MachineControls {
    pub(super) fn is_pending(&self, id: LocalPrinterId) -> bool {
        self.tasks.contains_key(&id)
    }
}

#[derive(Clone)]
struct Editor {
    printer: LocalPrinterId,
    session: u64,
    epoch: u64,
    choice: Choice,
    value: EditorValue,
    job: Option<MachineJobTarget>,
    z_range: Option<[f64; 2]>,
    shutdown_step: Option<u32>,
    focus: FocusHandle,
    opener: Option<FocusHandle>,
    error: Option<String>,
}

#[derive(Clone)]
enum EditorValue {
    Confirm,
    Input(Entity<InputState>),
    Speed {
        slider: Entity<SliderState>,
        range: [u16; 2],
    },
}

fn machine_result_message(error: ControlError) -> String {
    match error {
        ControlError::InvalidData => t!("printers.controls.invalid_value").into_owned(),
        _ => super::materials::result_message(error),
    }
}

fn editor_message(error: ControlError) -> String {
    if error == ControlError::StaleResource {
        return t!("printers.controls.reopen").into_owned();
    }
    machine_result_message(error)
}

fn calibration_block_message(printer: &PrinterEntry, error: ControlError) -> Option<String> {
    use crate::shared::machine::CalibrationReadiness;

    if error != ControlError::InvalidState {
        return None;
    }
    if let Err(key) = motion::availability(printer, false) {
        return Some(t!(key).into_owned());
    }
    let key = match printer.detail_state.machine.calibration_readiness {
        Some(CalibrationReadiness::NeedsHoming) => "printers.controls.calibration_home_required",
        None | Some(CalibrationReadiness::NotReady) => "printers.controls.calibration_wait",
        _ => return None,
    };
    Some(t!(key).into_owned())
}

fn localized_decimal(value: &str) -> String {
    if rust_i18n::locale().starts_with("es") {
        value.replace('.', ",")
    } else {
        value.to_owned()
    }
}

fn speed_field(
    slider: &Entity<SliderState>,
    [min, max]: [u16; 2],
    observed: Option<f64>,
    label: &str,
    disabled: bool,
    cx: &gpui_kit::App,
) -> AnyElement {
    let reported = observed.map_or_else(
        || t!("printers.controls.speed_unreported").into_owned(),
        |value| {
            let decimal = format!("{value:.2}");
            let value = decimal.trim_end_matches('0').trim_end_matches('.');
            let value = localized_decimal(value);
            t!("printers.controls.speed_observed", value = value).into_owned()
        },
    );
    let selected = slider.read(cx).value().end();
    let muted = cx.theme().muted_foreground;
    v_flex()
        .gap_2()
        .child(
            h_flex()
                .justify_between()
                .gap_2()
                .text_sm()
                .child(div().text_color(muted).child(reported))
                .child(
                    div()
                        .font_semibold()
                        .child(t!("printers.controls.speed_selected", value = selected)),
                ),
        )
        .child(
            Slider::new(slider)
                .accessibility_label(label.to_owned())
                .disabled(disabled),
        )
        .child(
            h_flex()
                .justify_between()
                .text_xs()
                .text_color(muted)
                .child(format!("{min}%"))
                .child(format!("{max}%")),
        )
        .into_any_element()
}

impl MakerCalcGpuiApp {
    pub(super) fn render_machine_controls(
        id: LocalPrinterId,
        printer: &PrinterEntry,
        pending: bool,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let choices = choices(printer)
            .into_iter()
            .filter(|choice| {
                choice.control.header_id().is_none()
                    && !matches!(choice.control, Control::Home | Control::Move(_))
            })
            .collect::<Vec<_>>();
        if choices.is_empty() {
            return None;
        }
        Some(
            h_flex()
                .id("machine-controls")
                .test_support()
                .flex_wrap()
                .items_start()
                .gap_2()
                .children(choices.into_iter().enumerate().map(|(index, choice)| {
                    Button::new(("machine-control", index))
                        .outline()
                        .small()
                        .label(choice.label.clone())
                        .disabled(availability(printer, choice.control, pending).is_err())
                        .on_click(cx.listener(move |app, _, window, cx| {
                            app.open_machine_editor(id, choice.clone(), window, cx);
                        }))
                }))
                .into_any_element(),
        )
    }

    fn open_machine_editor(
        &mut self,
        id: LocalPrinterId,
        mut choice: Choice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(printer) = self.printers.get(&id) else {
            return;
        };
        if matches!(
            choice.control,
            Control::ClearPlate
                | Control::AutoShutdown(_)
                | Control::Name
                | Control::Calibration(_)
        ) && !choices(printer)
            .iter()
            .any(|current| current.control == choice.control)
        {
            return;
        }
        if self.selected != Some(id)
            || self.detail_tab != super::detail::DetailTab::Overview
            || availability(
                printer,
                choice.control,
                self.machine_controls.is_pending(id),
            )
            .is_err()
        {
            return;
        }
        if choice.control == Control::Name {
            choice.value = Some(
                printer
                    .detail_state
                    .machine
                    .device
                    .reported_name
                    .clone()
                    .unwrap_or_default(),
            );
        }
        let z_range = if choice.control == Control::ZOffset {
            let machine = &printer.detail_state.machine;
            let Some(range) = z_offset_range(machine) else {
                return;
            };
            choice.value = Some(z_offset_draft(machine));
            Some(range)
        } else {
            None
        };
        let job = match choice.control {
            Control::Speed | Control::ZOffset | Control::AutoShutdown(_) | Control::Name => {
                Some(printer.state.machine_job_target())
            }
            Control::ClearPlate => Some(printer.state.plate_job_target()),
            Control::Calibration(_) => Some(printer.state.machine_job_target()),
            _ => None,
        };
        let value = if choice.control == Control::Speed {
            let Some(range @ [min, max]) = speed_range(printer) else {
                return;
            };
            let initial = observed_speed(printer)
                .unwrap_or(100.)
                .round()
                .clamp(f64::from(min), f64::from(max)) as f32;
            let slider = cx.new(|_| {
                SliderState::new()
                    .min(f32::from(min))
                    .max(f32::from(max))
                    .default_value(initial)
            });
            cx.observe(&slider, |_, _, cx| cx.notify()).detach();
            EditorValue::Speed { slider, range }
        } else if let Some(value) = &choice.value {
            EditorValue::Input(
                cx.new(|cx| InputState::new(window, cx).default_value(value.clone())),
            )
        } else {
            EditorValue::Confirm
        };
        self.machine_controls.editor = Some(Editor {
            printer: id,
            session: printer.session_generation,
            epoch: printer.state.connection_epoch,
            value,
            job,
            z_range,
            shutdown_step: matches!(choice.control, Control::AutoShutdown(true))
                .then_some(printer.detail_state.machine.auto_shutdown_delay_step_s)
                .flatten(),
            choice,
            focus: cx.focus_handle(),
            opener: window.focused(cx),
            error: None,
        });
        cx.notify();
    }

    fn machine_editor_printer(&self, editor: &Editor) -> Result<&PrinterEntry, ControlError> {
        let printer = self
            .printers
            .get(&editor.printer)
            .ok_or(ControlError::Disconnected)?;
        if self.selected != Some(editor.printer)
            || self.detail_tab != super::detail::DetailTab::Overview
            || printer.session_generation != editor.session
            || printer.state.connection_epoch != editor.epoch
        {
            return Err(ControlError::StaleResource);
        }
        if !choices(printer)
            .iter()
            .any(|choice| choice.control == editor.choice.control)
        {
            return Err(ControlError::Unsupported);
        }
        if editor
            .job
            .as_ref()
            .is_some_and(|job| !printer.state.matches_machine_job_target(job))
            || editor.z_range.is_some()
                && editor.z_range != z_offset_range(&printer.detail_state.machine)
            || editor.shutdown_step.is_some()
                && editor.shutdown_step != printer.detail_state.machine.auto_shutdown_delay_step_s
            || editor.choice.control == Control::Name
                && editor.choice.value.as_deref().unwrap_or_default()
                    != printer
                        .detail_state
                        .machine
                        .device
                        .reported_name
                        .as_deref()
                        .unwrap_or_default()
            || matches!(&editor.value, EditorValue::Speed { range, .. } if Some(*range) != speed_range(printer))
        {
            return Err(ControlError::StaleResource);
        }
        availability(
            printer,
            editor.choice.control,
            self.machine_controls.is_pending(editor.printer),
        )?;
        Ok(printer)
    }

    fn submit_machine_editor(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.machine_controls.editor.clone() else {
            return;
        };
        let result = (|| {
            let printer = self.machine_editor_printer(&editor)?;
            let value = match &editor.value {
                EditorValue::Confirm => String::new(),
                EditorValue::Input(input) => input.read(cx).value().to_string(),
                EditorValue::Speed { slider, .. } => slider.read(cx).value().end().to_string(),
            };
            let action = action(printer, editor.choice.control, &value)?;
            printer
                .operation
                .machine_action_for_job(editor.epoch, editor.job.clone(), action.clone())
                .map(|receiver| (action, receiver))
        })();
        let (requested, receiver) = match result {
            Ok(request) => request,
            Err(error) => {
                if let Some(editor) = &mut self.machine_controls.editor {
                    editor.error = Some(editor_message(error));
                }
                cx.notify();
                return;
            }
        };
        self.machine_controls.editor = None;
        self.track_machine_request(
            editor.printer,
            editor.session,
            editor.epoch,
            requested,
            receiver,
            cx,
        );
    }

    fn track_machine_request(
        &mut self,
        id: LocalPrinterId,
        session: u64,
        epoch: u64,
        action: MachineAction,
        receiver: tokio::sync::oneshot::Receiver<ControlResult>,
        cx: &mut Context<Self>,
    ) {
        let is_motion = matches!(
            action,
            MachineAction::DisableMotors
                | MachineAction::HomeAxes { .. }
                | MachineAction::MoveAxisRelative { .. }
        );
        if is_motion {
            self.machine_controls.motion.feedback = Some(motion::Feedback {
                printer: id,
                session,
                epoch,
                action,
                result: None,
            });
        }
        let task = cx.spawn(async move |app, cx| {
            let result = receiver.await.unwrap_or(ControlResult::Unknown);
            let _ = app.update(cx, |app, cx| {
                app.machine_controls.tasks.remove(&id);
                if !app.printers.get(&id).is_some_and(|p| {
                    p.session_generation == session && p.state.connection_epoch == epoch
                }) {
                    return;
                }
                if is_motion
                    && let Some(feedback) = &mut app.machine_controls.motion.feedback
                    && feedback.printer == id
                    && feedback.session == session
                    && feedback.epoch == epoch
                {
                    feedback.result = Some(result);
                    cx.notify();
                    return;
                }
                let (tone, message) = match result {
                    ControlResult::Accepted => (
                        ToastKind::Info,
                        t!("printers.controls.accepted").into_owned(),
                    ),
                    ControlResult::Unknown => (
                        ToastKind::Warning,
                        t!("printers.controls.uncertain").into_owned(),
                    ),
                    ControlResult::Rejected(error) => {
                        (ToastKind::Error, machine_result_message(error))
                    }
                };
                app.push_toast(tone, message, None, cx);
                cx.notify();
            });
        });
        self.machine_controls.tasks.insert(id, task);
        cx.notify();
    }

    pub(super) fn render_machine_editor(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.machine_controls.editor.is_none() && !self.machine_controls.rendered {
            return None;
        }
        let Some(display) = dialog::present(
            "machine-editor-presence",
            self.machine_controls.editor.clone(),
            self.machine_controls
                .editor
                .as_ref()
                .and_then(|e| e.opener.clone()),
            window,
            cx,
        ) else {
            self.machine_controls.rendered = false;
            return None;
        };
        self.machine_controls.rendered = true;
        let editor = display.value;
        let auto_shutdown = matches!(editor.choice.control, Control::AutoShutdown(_));
        let printer_name = editor.choice.control == Control::Name;
        let live_check = editor
            .job
            .is_some()
            .then(|| self.machine_editor_printer(&editor));
        let disabled = display.closing || live_check.as_ref().is_some_and(Result::is_err);
        let error = live_check
            .as_ref()
            .and_then(|result| result.as_ref().err())
            .map(|&error| {
                let hint = matches!(editor.choice.control, Control::Calibration(_))
                    .then(|| self.printers.get(&editor.printer))
                    .flatten()
                    .and_then(|printer| calibration_block_message(printer, error));
                hint.unwrap_or_else(|| editor_message(error))
            })
            .or(editor.error);
        let speed = editor.choice.control == Control::Speed;
        let clear_plate = editor.choice.control == Control::ClearPlate;
        let calibration_hint = match editor.choice.control {
            Control::Calibration(CalibrationKind::BedLeveling) => {
                Some(t!("printers.controls.level_bed_confirm").into_owned())
            }
            Control::Calibration(CalibrationKind::VibrationCompensation) => {
                Some(t!("printers.controls.vibration_confirm").into_owned())
            }
            _ => None,
        };
        let shutdown_hint = match editor.choice.control {
            Control::AutoShutdown(true) => editor
                .shutdown_step
                .map(|step| t!("printers.controls.auto_shutdown_delay", step = step).into_owned()),
            Control::AutoShutdown(false) => {
                Some(t!("printers.controls.auto_shutdown_confirm_off").into_owned())
            }
            _ => None,
        };
        let z_unreported = editor.z_range.is_some()
            && live_check.as_ref().is_some_and(|result| {
                result
                    .as_ref()
                    .is_ok_and(|printer| printer.detail_state.machine.z_offset_mm.is_none())
            });
        let z_range = editor.z_range.map(|[min, max]| {
            t!(
                "printers.controls.z_offset_range",
                min = localized_decimal(&min.to_string()),
                max = localized_decimal(&max.to_string())
            )
            .into_owned()
        });
        let field = match &editor.value {
            EditorValue::Confirm => None,
            EditorValue::Input(input) => Some(
                Input::new(input)
                    .when(z_range.is_some(), |input| {
                        input.id("machine-z-offset-input")
                    })
                    .when(auto_shutdown, |input| {
                        input.id("machine-auto-shutdown-delay")
                    })
                    .when(printer_name, |input| input.id("machine-printer-name"))
                    .aria_label(if auto_shutdown {
                        t!("printers.controls.auto_shutdown_seconds").into_owned()
                    } else if z_range.is_some() || printer_name {
                        editor.choice.label.clone()
                    } else {
                        t!("printers.controls.value").into_owned()
                    })
                    .disabled(disabled)
                    .into_any_element(),
            ),
            EditorValue::Speed { slider, range, .. } => Some(speed_field(
                slider,
                *range,
                live_check.and_then(Result::ok).and_then(observed_speed),
                &editor.choice.label,
                disabled,
                cx,
            )),
        };
        if !display.closing && !editor.focus.contains_focused(window, cx) {
            editor.focus.focus(window, cx);
        }
        Some(
            backdrop("machine-editor-backdrop", cx.theme().overlay, display.alpha)
                .on_key_down(cx.listener(|app, event: &gpui_kit::KeyDownEvent, _, cx| {
                    if event.keystroke.key == "escape" {
                        app.machine_controls.editor = None;
                        cx.notify();
                        cx.stop_propagation();
                    }
                }))
                .child(dialog::animated(
                    "machine-editor-zoom",
                    dialog::surface("machine-editor-dialog", editor.choice.label.clone(), cx)
                        .max_h(window.viewport_size().height - px(48.))
                        .track_focus(&editor.focus)
                        .child(
                            dialog::header()
                                .child(dialog::title().child(editor.choice.label.clone())),
                        )
                        .when(!speed, |dialog| {
                            dialog.child(
                                div().text_sm().whitespace_normal().child(
                                    z_range
                                        .or(shutdown_hint)
                                        .or(calibration_hint)
                                        .unwrap_or_else(|| {
                                            if clear_plate {
                                                t!("printers.controls.clear_plate_confirm")
                                                    .into_owned()
                                            } else if printer_name {
                                                t!("printers.controls.name_limit").into_owned()
                                            } else {
                                                t!("printers.controls.confirm").into_owned()
                                            }
                                        }),
                                ),
                            )
                        })
                        .when(z_unreported, |dialog| {
                            dialog.child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(t!("printers.controls.z_offset_unreported")),
                            )
                        })
                        .children(field)
                        .when_some(error, |dialog, error| {
                            dialog.child(
                                div()
                                    .id("machine-editor-error")
                                    .test_support()
                                    .text_color(cx.theme().danger)
                                    .child(error),
                            )
                        })
                        .child(
                            dialog::footer(window, cx)
                                .child(
                                    Button::new("machine-editor-cancel")
                                        .outline()
                                        .label(t!("common.actions.cancel").into_owned())
                                        .disabled(display.closing)
                                        .on_click(cx.listener(|app, _, _, cx| {
                                            app.machine_controls.editor = None;
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    Button::new("machine-editor-submit")
                                        .label(if speed {
                                            t!("printers.controls.speed_apply").into_owned()
                                        } else if editor.choice.control == Control::ZOffset {
                                            t!("printers.controls.z_offset_apply").into_owned()
                                        } else if clear_plate {
                                            t!("printers.controls.clear_plate").into_owned()
                                        } else if auto_shutdown
                                            || matches!(
                                                editor.choice.control,
                                                Control::Calibration(_)
                                            )
                                        {
                                            editor.choice.label.clone()
                                        } else if printer_name {
                                            t!("printers.controls.name_apply").into_owned()
                                        } else {
                                            t!("printers.controls.send").into_owned()
                                        })
                                        .disabled(disabled)
                                        .on_click(cx.listener(|app, _, _, cx| {
                                            app.submit_machine_editor(cx)
                                        })),
                                ),
                        ),
                    display.alpha,
                    display.closing,
                ))
                .focus_trap("machine-editor-trap", &editor.focus)
                .into_any_element(),
        )
    }
}
