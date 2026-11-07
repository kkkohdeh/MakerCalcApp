use super::super::components::{card, separator};
use super::super::icons::SVG_CROSSHAIR;
use super::*;
use crate::shared::machine::{MotionSettings, SupplyOperationPhase};
use crate::shared::printer::PrintStatus;
use crate::shared::workspace::{DeviceKind, OperationState, PrinterActionKind};
use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Input as GpuiInput, InputEvent};
use gpui_kit::component::{Icon, v_flex};
use gpui_kit::{App, RenderOnce, Role, StatefulInteractiveElement as _, Subscription, WeakEntity};
use std::{cell::RefCell, rc::Rc};

const STEPS: [f64; 3] = [0.1, 1., 10.];
const SVG_HOME: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m3 10 9-7 9 7M5 9v11h14V9M9 20v-7h6v7"/></svg>"#;
const SVG_UNLOCK: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect width="18" height="11" x="3" y="11" rx="2" ry="2"/><path d="M7 11V7a5 5 0 0 1 9.9-1"/></svg>"#;

pub(super) struct State {
    step_mm: f64,
    speed_draft: Rc<RefCell<Option<SpeedDraft>>>,
    pub(super) feedback: Option<Feedback>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            step_mm: 1.,
            speed_draft: Rc::new(RefCell::new(None)),
            feedback: None,
        }
    }
}

struct SpeedDraft {
    printer: LocalPrinterId,
    session: u64,
    epoch: u64,
    input: Entity<InputState>,
}

fn parse_speed(value: &str) -> Option<u8> {
    if value.is_empty() || value.len() > 2 || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse().ok().filter(|value| (5..=75).contains(value))
}

impl State {
    fn speed_text(&self, printer: &PrinterEntry, cx: &App) -> String {
        if let Some(draft) = &*self.speed_draft.borrow()
            && draft.printer == printer.state.id
            && draft.session == printer.session_generation
            && draft.epoch == printer.state.connection_epoch
        {
            return draft.input.read(cx).value().to_string();
        }
        let max = printer
            .detail_state
            .machine
            .manual_move_max_speed_mm_s
            .map(|limits| limits.into_iter().fold(10., f64::min))
            .unwrap_or(10.);
        format!("{:.0}", max.floor())
    }

    fn speed(&self, printer: &PrinterEntry, cx: &App) -> Result<Option<u8>, &'static str> {
        if printer
            .detail_state
            .machine
            .manual_move_max_speed_mm_s
            .is_none()
        {
            return Ok(None);
        }
        parse_speed(&self.speed_text(printer, cx))
            .map(Some)
            .ok_or("printers.motion.speed_range")
    }
}

#[derive(Clone, IntoElement)]
struct SpeedField {
    app: WeakEntity<MakerCalcGpuiApp>,
    printer: LocalPrinterId,
    session: u64,
    epoch: u64,
    value: String,
    enabled: bool,
    draft: Rc<RefCell<Option<SpeedDraft>>>,
}

struct SpeedEditor {
    input: Entity<InputState>,
    props: SpeedField,
    _events: Subscription,
}

impl SpeedEditor {
    fn new(props: SpeedField, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx).validate(|value, _| {
                value.len() <= 2
                    && value.bytes().all(|byte| byte.is_ascii_digit())
                    && (value.is_empty() || value.parse::<u8>().is_ok_and(|speed| speed <= 75))
            })
        });
        input.update(cx, |input, cx| {
            input.set_value(props.value.clone(), window, cx)
        });
        let events = cx.subscribe(&input, |editor, _, event, cx| {
            if !matches!(event, InputEvent::Change) {
                return;
            }
            cx.notify();
            let props = &editor.props;
            let _ = props.app.update(cx, |app, cx| {
                let valid = app.selected == Some(props.printer)
                    && app.printers.get(&props.printer).is_some_and(|printer| {
                        printer.session_generation == props.session
                            && printer.state.connection_epoch == props.epoch
                            && printer
                                .detail_state
                                .machine
                                .manual_move_max_speed_mm_s
                                .is_some()
                            && availability(printer, app.machine_controls.is_pending(props.printer))
                                .is_ok()
                    });
                if valid {
                    app.machine_controls.motion.feedback = None;
                    cx.notify();
                }
            });
        });
        Self {
            input,
            props,
            _events: events,
        }
    }
}

impl RenderOnce for SpeedField {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let key = format!(
            "motion-speed-{}-{}-{}-{}",
            self.printer.0, self.session, self.epoch, self.enabled
        );
        let editor = window.use_keyed_state(key, cx, |window, cx| {
            SpeedEditor::new(self.clone(), window, cx)
        });
        editor.update(cx, |editor, cx| {
            editor.props = self;
            // Read the input itself at dispatch, before deferred Change events can lag behind.
            editor.props.draft.replace(Some(SpeedDraft {
                printer: editor.props.printer,
                session: editor.props.session,
                epoch: editor.props.epoch,
                input: editor.input.clone(),
            }));
            h_flex()
                .id("motion-speed-field")
                .test_support()
                .flex_none()
                .gap_2()
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(t!("printers.motion.speed")),
                )
                .child(
                    GpuiInput::new(&editor.input)
                        .id("motion-speed-input")
                        .aria_label(t!("printers.motion.speed_accessibility").into_owned())
                        .small()
                        .w(px(42.))
                        .h(px(28.))
                        .disabled(!editor.props.enabled),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("mm/s"),
                )
        })
    }
}

pub(super) struct Feedback {
    pub(super) printer: LocalPrinterId,
    pub(super) session: u64,
    pub(super) epoch: u64,
    pub(super) action: MachineAction,
    pub(super) result: Option<ControlResult>,
}

pub(super) fn availability(printer: &PrinterEntry, pending: bool) -> Result<(), &'static str> {
    if printer.state.connection != LanConnection::Connected {
        return Err("printers.motion.disconnected");
    }
    if !printer.state.telemetry_ready
        || !printer.detail_state.live
        || !printer
            .state
            .age_ms(std::time::Instant::now())
            .is_some_and(|age| age <= 15_000)
    {
        return Err("printers.motion.stale");
    }
    if !matches!(
        printer.state.telemetry.print_state,
        PrintStatus::Idle | PrintStatus::Completed | PrintStatus::Cancelled
    ) {
        return Err("printers.motion.not_idle");
    }
    if pending
        || printer.operation_state.is_pending()
        || printer.material_pending.is_some()
        || printer.detail_state.machine.activity.is_some()
        || printer.actions.capacity() < printer.actions.max_capacity()
        || printer.operation.actions.capacity() < printer.operation.actions.max_capacity()
        || printer.detail_state.machine.supplies.iter().any(|supply| {
            supply
                .operation
                .is_some_and(|operation| operation.phase != SupplyOperationPhase::Completed)
        })
    {
        return Err("printers.motion.busy");
    }
    Ok(())
}

fn settings(printer: &PrinterEntry) -> impl Iterator<Item = &MotionSettings> {
    printer
        .detail_state
        .devices
        .iter()
        .filter(|device| matches!(device.kind, DeviceKind::Motion | DeviceKind::MotionSettings))
        .filter_map(|device| device.readings.motion.as_ref())
}

fn move_block(printer: &PrinterEntry, axis: Axis, delta: f64) -> Option<&'static str> {
    let index = match axis {
        Axis::X => 0,
        Axis::Y => 1,
        Axis::Z => 2,
    };
    if settings(printer)
        .find_map(|motion| motion.homed_axes)
        .is_some_and(|axes| !axes[index])
    {
        return Some("printers.motion.home_required");
    }
    // Compare positions and limits from the same reported coordinate frame.
    for motion in settings(printer) {
        if let (Some(position), Some(min), Some(max)) = (
            motion.commanded_position_mm,
            motion.travel_min_mm,
            motion.travel_max_mm,
        ) {
            let target = position[index] + delta;
            if !target.is_finite() || !(min[index]..=max[index]).contains(&target) {
                return Some("printers.motion.travel_limit");
            }
        }
    }
    None
}

fn axis_name(axis: Axis) -> &'static str {
    match axis {
        Axis::X => "X",
        Axis::Y => "Y",
        Axis::Z => "Z",
    }
}

fn step_label(step: f64) -> String {
    if step == 0.1 {
        t!("printers.motion.step_tenth").into_owned()
    } else {
        t!("printers.motion.step_mm", distance = format!("{step:.0}")).into_owned()
    }
}

fn request_label(action: &MachineAction) -> String {
    match action {
        MachineAction::MoveAxisRelative {
            axis, distance_mm, ..
        } => t!(
            "printers.motion.move_request",
            axis = axis_name(*axis),
            direction = if *distance_mm > 0. { "+" } else { "−" },
            distance = step_label(distance_mm.abs())
        )
        .into_owned(),
        MachineAction::HomeAxes { .. } => t!("printers.controls.home").into_owned(),
        MachineAction::DisableMotors => t!("printers.motion.disable_motors").into_owned(),
        _ => unreachable!("motion feedback only"),
    }
}

fn jog(
    id: LocalPrinterId,
    printer: &PrinterEntry,
    axis: Axis,
    positive: bool,
    controls: &MachineControls,
    cx: &mut Context<MakerCalcGpuiApp>,
) -> AnyElement {
    let distance_mm = controls.motion.step_mm * if positive { 1. } else { -1. };
    let (speed_mm_s, speed_error) = match controls.motion.speed(printer, cx) {
        Ok(speed) => (speed, None),
        Err(key) => (None, Some(t!(key).into_owned())),
    };
    let action = MachineAction::MoveAxisRelative {
        axis,
        distance_mm,
        speed_mm_s,
    };
    let supported = printer
        .detail_state
        .machine
        .actions
        .contains(&K::MoveAxisRelative);
    let block = move_block(printer, axis, distance_mm)
        .map(|key| t!(key).into_owned())
        .or(speed_error)
        .or_else(|| speed_block(printer, axis, speed_mm_s));
    let enabled =
        supported && availability(printer, controls.is_pending(id)).is_ok() && block.is_none();
    let name = axis_name(axis);
    let sign = if positive { "+" } else { "−" };
    let icon = match (axis, positive) {
        (Axis::X, true) => IconName::ChevronRight,
        (Axis::X, false) => IconName::ChevronLeft,
        (_, true) => IconName::ChevronUp,
        (_, false) => IconName::ChevronDown,
    };
    let session = printer.session_generation;
    let epoch = printer.state.connection_epoch;
    Button::new(format!(
        "motion-{name}-{}",
        if positive { "plus" } else { "minus" }
    ))
    .outline()
    .w(px(52.))
    .h(px(46.))
    .p_0()
    .accessibility_label(request_label(&action))
    .disabled(!enabled)
    .when_some(block, |button, message| button.tooltip(message))
    .child(
        v_flex()
            .items_center()
            .gap_1()
            .child(Icon::new(icon).size(px(16.)))
            .child(
                div()
                    .text_size(px(11.))
                    .line_height(px(12.))
                    .child(format!("{name}{sign}")),
            ),
    )
    .on_click(cx.listener(move |app, _, _, cx| {
        let speed_mm_s = app
            .printers
            .get(&id)
            .and_then(|printer| app.machine_controls.motion.speed(printer, cx).ok())
            .flatten();
        app.send_motion(
            id,
            session,
            epoch,
            MachineAction::MoveAxisRelative {
                axis,
                distance_mm,
                speed_mm_s,
            },
            cx,
        );
    }))
    .into_any_element()
}

fn position(printer: &PrinterEntry, cx: &App) -> AnyElement {
    let position = super::super::machine::motion_position(printer);
    let coordinates = position.map(|(value, _, _)| value);
    let homed = settings(printer).find_map(|motion| motion.homed_axes);
    v_flex()
        .gap_2()
        .child(
            h_flex()
                .flex_wrap()
                .gap_4()
                .child(
                    div()
                        .flex_1()
                        .text_color(cx.theme().muted_foreground)
                        .child(match position.map(|(_, _, key)| key) {
                            Some("commanded_position") => t!("printers.motion.commanded_position"),
                            Some("work_position") => t!("printers.motion.work_position"),
                            _ => t!("printers.motion.position"),
                        }),
                )
                .children(
                    ["X", "Y", "Z"]
                        .into_iter()
                        .enumerate()
                        .map(|(index, axis)| {
                            let value = coordinates
                                .map(|position| format!("{:.1}", position[index]))
                                .unwrap_or_else(|| "—".into());
                            h_flex()
                                .id(format!("motion-position-{axis}"))
                                .test_support()
                                .role(Role::Group)
                                .aria_label(
                                    t!(
                                        "printers.bay.metric",
                                        label = axis,
                                        value = format!("{value} mm")
                                    )
                                    .into_owned(),
                                )
                                .gap_2()
                                .child(div().text_color(cx.theme().muted_foreground).child(axis))
                                .child(value)
                        }),
                ),
        )
        .when_some(homed, |row, axes| {
            let axes = ["X", "Y", "Z"]
                .into_iter()
                .zip(axes)
                .filter_map(|(axis, homed)| homed.then_some(axis))
                .collect::<Vec<_>>()
                .join(" · ");
            row.child(div().text_color(cx.theme().muted_foreground).child(t!(
                "printers.motion.homed",
                axes = if axes.is_empty() {
                    "—"
                } else {
                    axes.as_str()
                }
            )))
        })
        .into_any_element()
}

impl MakerCalcGpuiApp {
    pub(in crate::gpui_app) fn render_motion_controls(
        id: LocalPrinterId,
        printer: &PrinterEntry,
        controls: &MachineControls,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let actions = &printer.detail_state.machine.actions;
        let home = actions.contains(&K::HomeAxes);
        let movement = actions.contains(&K::MoveAxisRelative);
        let release = actions.contains(&K::DisableMotors);
        if !home && !movement && !release {
            return None;
        }
        let available = availability(printer, controls.is_pending(id));
        let enabled = available.is_ok();
        let release_hint = match available {
            Err(key) => t!(key),
            Ok(()) => match printer.detail_state.machine.motion_idle {
                Some(true) => t!("printers.motion.disable_motors_hint"),
                Some(false) => t!("printers.motion.busy"),
                None => t!("printers.motion.disable_motors_unconfirmed"),
            },
        }
        .into_owned();
        let session = printer.session_generation;
        let epoch = printer.state.connection_epoch;
        let step = controls.motion.step_mm;
        let header = h_flex()
            .justify_between()
            .gap_3()
            .child(card::title(false).child(t!("printers.motion.title")))
            .child(
                h_flex()
                    .gap_2()
                    .when(release, |buttons| {
                        let ready =
                            enabled && printer.detail_state.machine.motion_idle == Some(true);
                        buttons.child(
                            Button::new("motion-disable-motors")
                                .outline()
                                .small()
                                .w(px(28.))
                                .h(px(28.))
                                .p_0()
                                .icon(Icon::default().data(SVG_UNLOCK).size(px(16.)))
                                .accessibility_label(
                                    t!("printers.motion.disable_motors").into_owned(),
                                )
                                .tooltip(release_hint.clone())
                                .disabled(!ready)
                                .on_click(cx.listener(move |app, _, _, cx| {
                                    app.send_motion(
                                        id,
                                        session,
                                        epoch,
                                        MachineAction::DisableMotors,
                                        cx,
                                    )
                                })),
                        )
                    })
                    .when(home, |buttons| {
                        buttons.child(
                            Button::new("motion-home")
                                .outline()
                                .small()
                                .icon(Icon::default().data(SVG_HOME))
                                .label(t!("printers.controls.home").into_owned())
                                .disabled(!enabled)
                                .on_click(cx.listener(move |app, _, window, cx| {
                                    if app.selected != Some(id)
                                        || app.detail_tab
                                            != super::super::detail::DetailTab::Overview
                                    {
                                        return;
                                    }
                                    if !app.printers.get(&id).is_some_and(|printer| {
                                        printer.session_generation == session
                                            && printer.state.connection_epoch == epoch
                                            && availability(
                                                printer,
                                                app.machine_controls.is_pending(id),
                                            )
                                            .is_ok()
                                    }) {
                                        return;
                                    }
                                    app.open_machine_editor(
                                        id,
                                        Choice {
                                            label: label("home"),
                                            control: Control::Home,
                                            value: None,
                                        },
                                        window,
                                        cx,
                                    );
                                })),
                        )
                    }),
            );
        let steps = h_flex()
            .justify_between()
            .flex_wrap()
            .gap_3()
            .child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child(t!("printers.motion.step")),
            )
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_2()
                    .child(h_flex().gap_1().children(STEPS.into_iter().enumerate().map(
                        |(index, value)| {
                            Button::new(("motion-step", index))
                                .outline()
                                .small()
                                .label(step_label(value))
                                .toggled(step == value)
                                .when(step == value, |button| {
                                    button.border_color(cx.theme().muted_foreground)
                                })
                                .disabled(!enabled || !movement)
                                .on_click(cx.listener(move |app, _, _, cx| {
                                    if app.printers.get(&id).is_some_and(|printer| {
                                        printer.session_generation == session
                                            && printer.state.connection_epoch == epoch
                                            && availability(
                                                printer,
                                                app.machine_controls.is_pending(id),
                                            )
                                            .is_ok()
                                    }) {
                                        app.machine_controls.motion.step_mm = value;
                                        app.machine_controls.motion.feedback = None;
                                        cx.notify();
                                    }
                                }))
                        },
                    )))
                    .when(
                        printer
                            .detail_state
                            .machine
                            .manual_move_max_speed_mm_s
                            .is_some()
                            && movement,
                        |row| {
                            row.child(SpeedField {
                                app: cx.entity().downgrade(),
                                printer: id,
                                session,
                                epoch,
                                value: controls.motion.speed_text(printer, cx),
                                enabled: enabled && movement,
                                draft: controls.motion.speed_draft.clone(),
                            })
                        },
                    ),
            );
        let spacer = || div().w(px(52.)).h(px(46.));
        let xy = v_flex()
            .items_center()
            .gap_3()
            .child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child(t!("printers.motion.xy")),
            )
            .child(
                v_flex()
                    .gap_2()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(spacer())
                            .child(jog(id, printer, Axis::Y, true, controls, cx))
                            .child(spacer()),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(jog(id, printer, Axis::X, false, controls, cx))
                            .child(
                                div()
                                    .w(px(52.))
                                    .h(px(46.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(Icon::default().data(SVG_CROSSHAIR).size(px(22.))),
                            )
                            .child(jog(id, printer, Axis::X, true, controls, cx)),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(spacer())
                            .child(jog(id, printer, Axis::Y, false, controls, cx))
                            .child(spacer()),
                    ),
            );
        let z = v_flex()
            .items_center()
            .gap_3()
            .child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child(t!("printers.motion.z")),
            )
            .child(
                v_flex()
                    .gap_2()
                    .child(jog(id, printer, Axis::Z, true, controls, cx))
                    .child(
                        div()
                            .w(px(52.))
                            .h(px(46.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_color(cx.theme().muted_foreground)
                            .child(
                                if let Some(limits) =
                                    printer.detail_state.machine.manual_move_max_speed_mm_s
                                {
                                    let label = t!(
                                        "printers.motion.axis_speed_limit",
                                        max = format!("{:.0}", limits[2].floor())
                                    )
                                    .into_owned();
                                    div()
                                        .id("motion-z-speed-limit")
                                        .test_support()
                                        .w_full()
                                        .min_w_0()
                                        .aria_label(label.clone())
                                        .text_xs()
                                        .text_center()
                                        .whitespace_normal()
                                        .when(
                                            controls
                                                .motion
                                                .speed(printer, cx)
                                                .ok()
                                                .flatten()
                                                .is_some_and(|speed| f64::from(speed) > limits[2]),
                                            |text| text.text_color(cx.theme().warning),
                                        )
                                        .child(label)
                                        .into_any_element()
                                } else {
                                    div().child("Z").into_any_element()
                                },
                            ),
                    )
                    .child(jog(id, printer, Axis::Z, false, controls, cx)),
            );
        let feedback = controls.motion.feedback.as_ref().filter(|feedback| {
            feedback.printer == id && feedback.session == session && feedback.epoch == epoch
        });
        let waiting = feedback.is_some()
            && (controls.is_pending(id)
                || matches!(
                    printer.operation_state,
                    OperationState::Sending(PrinterActionKind::MachineControl)
                ));
        let blocked = available
            .err()
            .filter(|key| *key != "printers.motion.busy" || !waiting);
        let (message, detail) = if let Some(key) = blocked {
            (
                t!(key).into_owned(),
                t!("printers.motion.wait").into_owned(),
            )
        } else if let Err(key) = controls.motion.speed(printer, cx) {
            (
                t!(key).into_owned(),
                t!("printers.motion.choose_direction").into_owned(),
            )
        } else if let Some(feedback) = feedback {
            let description = request_label(&feedback.action);
            match feedback.result {
                None => (
                    t!("printers.motion.sending", action = description).into_owned(),
                    t!("printers.motion.wait").into_owned(),
                ),
                Some(ControlResult::Accepted) => (
                    t!("printers.motion.requested", action = description).into_owned(),
                    t!("printers.motion.observation").into_owned(),
                ),
                Some(ControlResult::Unknown) => (
                    t!("printers.controls.uncertain").into_owned(),
                    t!("printers.motion.check").into_owned(),
                ),
                Some(ControlResult::Rejected(error)) => (
                    super::super::materials::result_message(error),
                    t!("printers.motion.check").into_owned(),
                ),
            }
        } else if !movement && release {
            (
                t!("printers.motion.disable_motors").into_owned(),
                release_hint,
            )
        } else {
            (
                t!("printers.motion.per_click", distance = step_label(step)).into_owned(),
                if settings(printer)
                    .find_map(|motion| motion.homed_axes)
                    .is_some_and(|axes| axes.contains(&false))
                {
                    t!("printers.motion.home_required").into_owned()
                } else if let Some(limits) = printer.detail_state.machine.manual_move_max_speed_mm_s
                {
                    t!(
                        "printers.motion.speed_limits",
                        x = format!("{:.0}", limits[0].floor()),
                        y = format!("{:.0}", limits[1].floor()),
                        z = format!("{:.0}", limits[2].floor())
                    )
                    .into_owned()
                } else {
                    t!("printers.motion.choose_direction").into_owned()
                },
            )
        };
        Some(
            card::surface(false, cx)
                .id("motion-controls")
                .test_support()
                .role(Role::Group)
                .aria_label(t!("printers.motion.title").into_owned())
                .child(card::header(false).child(header))
                .child(
                    card::content(false)
                        .text_sm()
                        .gap_3()
                        .child(position(printer, cx))
                        .child(separator::horizontal("motion-position-separator", cx))
                        .when(movement, |body| {
                            body.child(steps).child(
                                h_flex().justify_center().gap_6().py_1().child(xy).child(
                                    div()
                                        .border_l_1()
                                        .border_color(cx.theme().border)
                                        .pl_6()
                                        .child(z),
                                ),
                            )
                        }),
                )
                .child(
                    card::content(false)
                        .border_t_1()
                        .border_color(cx.theme().border)
                        .pt_3()
                        .min_h(px(52.))
                        .gap_1()
                        .child(
                            div()
                                .id("motion-feedback")
                                .test_support()
                                .aria_label(message.clone())
                                .text_sm()
                                .child(message),
                        )
                        .child(
                            div()
                                .text_size(px(11.))
                                .line_height(px(16.))
                                .text_color(cx.theme().muted_foreground)
                                .child(detail),
                        ),
                )
                .into_any_element(),
        )
    }

    fn send_motion(
        &mut self,
        id: LocalPrinterId,
        session: u64,
        epoch: u64,
        action: MachineAction,
        cx: &mut Context<Self>,
    ) {
        let result = (|| {
            let printer = self.printers.get(&id).ok_or(ControlError::Disconnected)?;
            if self.selected != Some(id)
                || self.detail_tab != super::super::detail::DetailTab::Overview
                || printer.session_generation != session
                || printer.state.connection_epoch != epoch
            {
                return Err(ControlError::StaleResource);
            }
            availability(printer, self.machine_controls.is_pending(id))
                .map_err(|_| ControlError::InvalidState)?;
            if action == MachineAction::DisableMotors
                && printer.detail_state.machine.motion_idle != Some(true)
            {
                return Err(ControlError::InvalidState);
            }
            if !printer
                .detail_state
                .machine
                .actions
                .contains(&action.kind())
            {
                return Err(ControlError::Unsupported);
            }
            if let MachineAction::MoveAxisRelative {
                axis,
                distance_mm,
                speed_mm_s,
            } = action
            {
                if speed_mm_s
                    != self
                        .machine_controls
                        .motion
                        .speed(printer, cx)
                        .map_err(|_| ControlError::InvalidData)?
                {
                    return Err(ControlError::InvalidData);
                }
                if speed_block(printer, axis, speed_mm_s).is_some() {
                    return Err(ControlError::InvalidData);
                }
                if !STEPS.contains(&distance_mm.abs()) {
                    return Err(ControlError::InvalidData);
                }
                if move_block(printer, axis, distance_mm).is_some() {
                    return Err(ControlError::InvalidState);
                }
            }
            printer.operation.machine_action(epoch, action.clone())
        })();
        match result {
            Ok(receiver) => self.track_machine_request(id, session, epoch, action, receiver, cx),
            Err(error) => {
                self.machine_controls.motion.feedback = Some(Feedback {
                    printer: id,
                    session,
                    epoch,
                    action,
                    result: Some(ControlResult::Rejected(error)),
                });
                cx.notify();
            }
        }
    }
}

fn speed_block(printer: &PrinterEntry, axis: Axis, speed: Option<u8>) -> Option<String> {
    let limits = printer.detail_state.machine.manual_move_max_speed_mm_s?;
    let speed = speed?;
    let index = match axis {
        Axis::X => 0,
        Axis::Y => 1,
        Axis::Z => 2,
    };
    (f64::from(speed) > limits[index]).then(|| {
        t!(
            "printers.motion.speed_limit",
            axis = axis_name(axis),
            max = format!("{:.0}", limits[index].floor())
        )
        .into_owned()
    })
}
