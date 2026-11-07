use super::components::button::Button;
use super::components::tooltip::TooltipExt as _;
use super::{MakerCalcGpuiApp, PrinterEntry};
use crate::i18n::t;
use crate::shared::printer::PrintStatus;
use crate::shared::workspace::{HeaterKind, PrinterAction};
use crate::state::{LanConnection, LocalPrinterId};
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, FocusHandle, InteractiveElement as _,
    IntoElement, ParentElement as _, RenderOnce, StatefulInteractiveElement as _, Styled as _,
    Subscription, WeakEntity, Window, div, px,
};

const CONTROL_WIDTH: f32 = 44.;
const CONTROL_HEIGHT: f32 = 22.;

fn availability(printer: &PrinterEntry, heater: HeaterKind) -> Result<(), &'static str> {
    if !printer.detail_state.temperature_targets.contains(&heater) {
        return Err("printers.temperature.unavailable");
    }
    if printer.state.connection != LanConnection::Connected
        || !printer.state.telemetry_ready
        || !printer.detail_state.live
        || !printer
            .state
            .age_ms(std::time::Instant::now())
            .is_some_and(|age| age <= 15_000)
    {
        return Err("printers.temperature.stale");
    }
    if matches!(
        printer.state.telemetry.print_state,
        PrintStatus::Printing | PrintStatus::Paused
    ) {
        return Err("printers.temperature.printing");
    }
    if printer.operation_state.is_pending()
        || printer.material_pending.is_some()
        || printer.actions.capacity() < printer.actions.max_capacity()
    {
        return Err("printers.temperature.pending");
    }
    Ok(())
}

fn parse_target(value: &str, heater: HeaterKind) -> Option<u16> {
    let value = value.trim();
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value
        .parse::<u16>()
        .ok()
        .filter(|target| *target <= heater.max_target_c())
}

fn heater_label(heater: HeaterKind) -> String {
    match heater {
        HeaterKind::Nozzle => t!("printers.metrics.nozzle"),
        HeaterKind::Bed => t!("printers.metrics.bed"),
    }
    .into_owned()
}

#[derive(Clone, IntoElement)]
struct Target {
    app: WeakEntity<MakerCalcGpuiApp>,
    printer: LocalPrinterId,
    session: u64,
    epoch: u64,
    revision: u64,
    heater: HeaterKind,
    current: String,
    observed: Option<f64>,
    available: Result<(), &'static str>,
}

pub(super) fn target(
    printer: &PrinterEntry,
    heater: HeaterKind,
    current: String,
    cx: &mut Context<MakerCalcGpuiApp>,
) -> Option<AnyElement> {
    printer
        .detail_state
        .temperature_targets
        .contains(&heater)
        .then(|| {
            Target {
                app: cx.entity().downgrade(),
                printer: printer.state.id,
                session: printer.session_generation,
                epoch: printer.state.connection_epoch,
                revision: printer.state.revision,
                heater,
                current,
                observed: match heater {
                    HeaterKind::Nozzle => printer.state.telemetry.active_nozzle_target_temp_c(),
                    HeaterKind::Bed => printer.state.telemetry.primary_bed_target_temp_c(),
                },
                available: availability(printer, heater),
            }
            .into_any_element()
        })
}

struct Editor {
    input: Entity<InputState>,
    opener: FocusHandle,
    editing: bool,
    error: Option<String>,
    pending_revision: Option<u64>,
    props: Target,
    _events: Subscription,
    _blur: Subscription,
}

impl Editor {
    fn new(props: Target, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let heater = props.heater;
        let max_digits = heater.max_target_c().to_string().len();
        let input = cx.new(|cx| {
            InputState::new(window, cx).validate(move |value, _| {
                value.len() <= max_digits
                    && value.bytes().all(|byte| byte.is_ascii_digit())
                    && (value.is_empty() || parse_target(value, heater).is_some())
            })
        });
        let events = cx.subscribe(&input, |editor, _, event, cx| {
            if matches!(event, InputEvent::Change) {
                editor.error = None;
                cx.notify();
            }
        });
        let opener = cx.focus_handle();
        let blur = cx.on_focus_out(&opener, window, |editor, _, _, cx| {
            editor.editing = false;
            editor.error = None;
            cx.notify();
        });
        Self {
            input,
            opener,
            editing: false,
            error: None,
            pending_revision: None,
            props,
            _events: events,
            _blur: blur,
        }
    }

    fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let allowed = self.props.app.upgrade().is_some_and(|app| {
            app.read(cx)
                .printers
                .get(&self.props.printer)
                .is_some_and(|printer| {
                    printer.session_generation == self.props.session
                        && printer.state.connection_epoch == self.props.epoch
                        && availability(printer, self.props.heater).is_ok()
                })
        });
        if !allowed {
            return;
        }
        self.editing = true;
        self.error = None;
        let value = self
            .props
            .observed
            .map(|value| format!("{value:.0}"))
            .unwrap_or_default();
        self.input.update(cx, |input, cx| {
            input.set_value(value, window, cx);
            input.focus(window, cx);
            input.select_all(window, cx);
        });
        cx.notify();
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target_c) = parse_target(self.input.read(cx).value().as_ref(), self.props.heater)
        else {
            self.error = Some(
                t!(
                    "printers.temperature.range",
                    max = self.props.heater.max_target_c()
                )
                .into_owned(),
            );
            cx.notify();
            return;
        };
        let result = self
            .props
            .app
            .upgrade()
            .ok_or("printers.temperature.stale")
            .and_then(|app| {
                let app = app.read(cx);
                let printer = app
                    .printers
                    .get(&self.props.printer)
                    .ok_or("printers.temperature.stale")?;
                if printer.session_generation != self.props.session
                    || printer.state.connection_epoch != self.props.epoch
                {
                    return Err("printers.temperature.stale");
                }
                availability(printer, self.props.heater)?;
                printer
                    .actions
                    .try_send(PrinterAction::SetTemperatureTarget {
                        heater: self.props.heater,
                        target_c,
                        connection_epoch: self.props.epoch,
                        submitted_at: std::time::Instant::now(),
                    })
                    .map_err(|error| match error {
                        tokio::sync::mpsc::error::TrySendError::Full(_) => {
                            "printers.temperature.pending"
                        }
                        tokio::sync::mpsc::error::TrySendError::Closed(_) => {
                            "printers.temperature.stale"
                        }
                    })
            });
        match result {
            Ok(()) => {
                self.pending_revision = Some(self.props.revision);
                self.editing = false;
                self.opener.focus(window, cx);
            }
            Err(key) => self.error = Some(t!(key).into_owned()),
        }
        cx.notify();
    }

    fn render_target(&mut self, cx: &mut Context<Self>) -> AnyElement {
        if self
            .pending_revision
            .is_some_and(|revision| revision != self.props.revision)
        {
            self.pending_revision = None;
        }
        let available = self.props.available.and_then(|()| {
            if self.pending_revision.is_some() {
                Err("printers.temperature.pending")
            } else {
                Ok(())
            }
        });
        if available.is_err() {
            self.editing = false;
            self.error = None;
        }
        let label = t!(
            "printers.temperature.edit",
            heater = heater_label(self.props.heater),
            max = self.props.heater.max_target_c()
        )
        .into_owned();
        let help = self.error.clone().or_else(|| match available {
            Ok(()) | Err("printers.temperature.printing") => None,
            Err(key) => Some(t!(key).into_owned()),
        });
        let id = match self.props.heater {
            HeaterKind::Nozzle => "nozzle",
            HeaterKind::Bed => "bed",
        };
        if !self.editing {
            return Button::new(format!("temperature-target-{id}"))
                .ghost()
                .focus_handle(&self.opener)
                .w(px(CONTROL_WIDTH))
                .h(px(CONTROL_HEIGHT))
                .p_0()
                .border_0()
                .rounded(px(2.))
                .accessibility_label(label)
                .when_some(help, |button, help| button.tooltip(help))
                .disabled(available.is_err())
                .tab_stop(available.is_ok())
                .opacity(1.)
                .child(
                    div()
                        .id(format!("temperature-current-{id}"))
                        .test_support()
                        .when(available.is_ok(), |reading| reading.border_b_1())
                        .border_color(cx.theme().muted_foreground)
                        .text_size(px(13.5))
                        .line_height(px(17.55))
                        .text_color(cx.theme().foreground)
                        .child(self.props.current.clone()),
                )
                .on_click(cx.listener(|editor, _, window, cx| {
                    cx.stop_propagation();
                    editor.open(window, cx);
                }))
                .into_any_element();
        }
        let input = Input::new(&self.input)
            .id(format!("temperature-input-{id}"))
            .aria_label(label)
            .appearance(false)
            .focus_bordered(false)
            .bordered(false)
            .w(px(26.))
            .h(px(CONTROL_HEIGHT))
            .p_0()
            .rounded_none()
            .bg(cx.theme().transparent)
            .border_b_1()
            .border_color(if self.error.is_some() {
                cx.theme().danger
            } else {
                cx.theme().ring
            })
            .text_size(px(13.5))
            .text_right()
            .line_height(px(17.55));
        let mut row = h_flex()
            .id(format!("temperature-editor-{id}"))
            .test_support()
            .track_focus(&self.opener)
            .relative()
            .w(px(CONTROL_WIDTH))
            .h(px(CONTROL_HEIGHT))
            .items_center()
            .justify_center()
            .text_size(px(13.5))
            .line_height(px(17.55))
            .on_action(cx.listener(
                |editor, _: &gpui_kit::component::input::Enter, window, cx| {
                    editor.submit(window, cx);
                    cx.stop_propagation();
                },
            ))
            .on_action(cx.listener(
                |editor, _: &gpui_kit::component::input::Escape, window, cx| {
                    editor.editing = false;
                    editor.error = None;
                    editor.opener.focus(window, cx);
                    cx.stop_propagation();
                    cx.notify();
                },
            ))
            .on_click(|_, _, cx| cx.stop_propagation())
            .on_mouse_down_out(
                cx.listener(|editor, event: &gpui_kit::MouseDownEvent, _, cx| {
                    if event.button != gpui_kit::MouseButton::Left {
                        return;
                    }
                    editor.editing = false;
                    editor.error = None;
                    cx.notify();
                }),
            )
            .child(input)
            .child("°");
        if let Some(error) = &self.error {
            row = row.child(
                div()
                    .id(format!("temperature-error-{id}"))
                    .test_support()
                    .role(gpui_kit::Role::Alert)
                    .aria_label(error.clone())
                    .absolute()
                    .right_0()
                    .top_0()
                    .text_color(cx.theme().danger)
                    .child("!"),
            );
        }
        row.when_some(self.error.clone(), |row, error| {
            row.app_tooltip(move |window, cx| {
                gpui_kit::component::tooltip::Tooltip::new(error.clone()).build(window, cx)
            })
        })
        .into_any_element()
    }
}

impl RenderOnce for Target {
    fn render(self, window: &mut Window, cx: &mut gpui_kit::App) -> impl IntoElement {
        let key = format!(
            "temperature-{}-{}-{}-{:?}",
            self.printer.0, self.session, self.epoch, self.heater
        );
        // Keyed state dies with this card or connection; drafts never survive reconnects.
        let editor =
            window.use_keyed_state(key, cx, |window, cx| Editor::new(self.clone(), window, cx));
        editor.update(cx, |editor, cx| {
            editor.props = self;
            div()
                .w(px(CONTROL_WIDTH))
                .h(px(CONTROL_HEIGHT))
                .flex_none()
                .child(editor.render_target(cx))
                .into_any_element()
        })
    }
}
