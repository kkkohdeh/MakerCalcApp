use super::super::{
    MakerCalcGpuiApp, PrinterEntry,
    components::{alert_dialog::backdrop, button::Button, dialog, input::Input},
    detail::DetailTab,
};
use crate::i18n::t;
use crate::shared::control::MachineAction;
use crate::shared::machine::{DryerResource, DryingCycle, DryingState};
use crate::shared::workspace::ResourceId;
use crate::state::{LanConnection, LocalPrinterId};
use gpui_kit::base::{FocusTrapElement as _, StyledExt as _};
use gpui_kit::component::input::InputState;
use gpui_kit::component::menu::PopupMenuItem;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, FocusHandle, InteractiveElement as _,
    IntoElement, KeyDownEvent, ParentElement as _, Styled as _, Window, div, px,
};

#[derive(Default)]
pub(super) struct State {
    editor: Option<Editor>,
    rendered: bool,
}

#[derive(Clone)]
struct Editor {
    printer: LocalPrinterId,
    session: u64,
    epoch: u64,
    dryer: ResourceId,
    temperature: Entity<InputState>,
    hours: Entity<InputState>,
    focus: FocusHandle,
    opener: FocusHandle,
    error: Option<String>,
}

fn available_dryers(printer: &PrinterEntry) -> &[DryerResource] {
    if printer.state.connection == LanConnection::Connected
        && printer.state.telemetry_ready
        && printer.detail_state.live
        && printer
            .state
            .age_ms(std::time::Instant::now())
            .is_some_and(|age| age <= 15_000)
    {
        &printer.detail_state.machine.dryers
    } else {
        &[]
    }
}

fn state_label(state: DryingState) -> String {
    let key = match state {
        DryingState::Idle => "printers.drying.idle",
        DryingState::Starting => "printers.drying.starting",
        DryingState::Drying => "printers.drying.running",
        DryingState::Cooling => "printers.drying.cooling",
        DryingState::Stopping => "printers.drying.stopping",
        DryingState::Fault => "printers.drying.fault",
        DryingState::Unknown => "common.status.unknown",
    };
    t!(key).into_owned()
}

pub(super) fn toolbar_button(
    id: LocalPrinterId,
    printer: &PrinterEntry,
    window: &mut Window,
    cx: &mut Context<MakerCalcGpuiApp>,
) -> Option<AnyElement> {
    let dryer = available_dryers(printer).first()?.id;
    let opener = window
        .use_keyed_state(("printer-drying", id.0), cx, |_, cx| cx.focus_handle())
        .read(cx)
        .clone();
    let title = t!("printers.drying.title").into_owned();
    Some(
        Button::new("printer-drying")
            .focus_handle(&opener)
            .ghost()
            .small()
            .w(px(30.))
            .px_0()
            .icon(
                Icon::default()
                    .data(super::super::icons::SVG_DRYER)
                    .size(px(16.)),
            )
            .accessibility_label(title.clone())
            .tooltip(title)
            .popup_trigger()
            .on_click(cx.listener(move |app, _, window, cx| {
                app.open_drying(id, dryer, opener.clone(), window, cx);
            }))
            .into_any_element(),
    )
}

impl Editor {
    fn printer<'a>(&self, app: &'a MakerCalcGpuiApp) -> Option<&'a PrinterEntry> {
        app.printers.get(&self.printer).filter(|printer| {
            app.selected == Some(self.printer)
                && app.detail_tab == DetailTab::Overview
                && printer.session_generation == self.session
                && printer.state.connection_epoch == self.epoch
        })
    }

    fn cycle(&self, cx: &gpui_kit::App) -> Option<DryingCycle> {
        Some(DryingCycle {
            temperature_c: self.temperature.read(cx).value().parse().ok()?,
            duration_hours: self.hours.read(cx).value().parse().ok()?,
        })
    }
}

fn input(
    value: Option<u16>,
    window: &mut Window,
    cx: &mut Context<MakerCalcGpuiApp>,
) -> Entity<InputState> {
    let field = cx.new(|cx| {
        InputState::new(window, cx)
            .validate(|value, _| {
                value.len() <= 3 && value.bytes().all(|byte| byte.is_ascii_digit())
            })
            .default_value(value.map_or_else(String::new, |value| value.to_string()))
    });
    cx.observe(&field, |_, _, cx| cx.notify()).detach();
    field
}

fn field(
    input: &Entity<InputState>,
    label: String,
    range: Option<String>,
    disabled: bool,
) -> impl IntoElement {
    v_flex()
        .flex_1()
        .min_w_0()
        .gap_1()
        .child(div().text_xs().child(label.clone()))
        .child(Input::new(input).aria_label(label).disabled(disabled))
        .when_some(range, |this, range| {
            this.child(div().text_xs().child(range))
        })
}

impl MakerCalcGpuiApp {
    fn open_drying(
        &mut self,
        id: LocalPrinterId,
        dryer: ResourceId,
        opener: FocusHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(printer) = self
            .printers
            .get(&id)
            .filter(|_| self.selected == Some(id) && self.detail_tab == DetailTab::Overview)
        else {
            return;
        };
        let Some(unit) = available_dryers(printer)
            .iter()
            .find(|unit| unit.id == dryer)
        else {
            return;
        };
        let target = unit.target.filter(|cycle| {
            !unit.can_start || unit.limits.is_some_and(|limits| limits.contains(*cycle))
        });
        let focus = self
            .machine_controls
            .drying
            .editor
            .as_ref()
            .map_or_else(|| cx.focus_handle(), |editor| editor.focus.clone());
        self.machine_controls.drying.editor = Some(Editor {
            printer: id,
            session: printer.session_generation,
            epoch: printer.state.connection_epoch,
            dryer,
            temperature: input(target.map(|cycle| cycle.temperature_c), window, cx),
            hours: input(target.map(|cycle| cycle.duration_hours), window, cx),
            focus,
            opener,
            error: None,
        });
        cx.notify();
    }

    fn submit_drying(&mut self, stop: bool, cx: &mut Context<Self>) {
        let Some(editor) = self.machine_controls.drying.editor.clone() else {
            return;
        };
        let Some(printer) = editor.printer(self) else {
            return;
        };
        let Some(unit) = available_dryers(printer)
            .iter()
            .find(|unit| unit.id == editor.dryer)
        else {
            return;
        };
        if self.machine_controls.is_pending(editor.printer) || printer.operation_state.is_pending()
        {
            return;
        }
        let action = if stop && unit.can_stop {
            MachineAction::StopDrying {
                dryer_id: editor.dryer,
            }
        } else if !stop && unit.can_start {
            let Some(cycle) = editor
                .cycle(cx)
                .filter(|cycle| unit.limits.is_some_and(|limits| limits.contains(*cycle)))
            else {
                return;
            };
            MachineAction::StartDrying {
                dryer_id: editor.dryer,
                cycle,
            }
        } else {
            return;
        };
        match printer
            .operation
            .machine_action(editor.epoch, action.clone())
        {
            Ok(reply) => {
                self.machine_controls.drying.editor.as_mut().unwrap().error = None;
                self.track_machine_request(
                    editor.printer,
                    editor.session,
                    editor.epoch,
                    action,
                    reply,
                    cx,
                );
            }
            Err(error) => {
                self.machine_controls.drying.editor.as_mut().unwrap().error =
                    Some(super::machine_result_message(error));
                cx.notify();
            }
        }
    }

    pub(in crate::gpui_app) fn render_drying_editor(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let state = &mut self.machine_controls.drying;
        if state.editor.is_none() && !state.rendered {
            return None;
        }
        let Some(display) = dialog::present(
            "drying-editor-presence",
            state.editor.clone(),
            state.editor.as_ref().map(|editor| editor.opener.clone()),
            window,
            cx,
        ) else {
            state.rendered = false;
            return None;
        };
        state.rendered = true;
        let editor = display.value;
        let printer = editor.printer(self);
        let units = printer.map_or(&[][..], available_dryers);
        let unit = units.iter().find(|unit| unit.id == editor.dryer);
        let pending = self.machine_controls.is_pending(editor.printer)
            || printer.is_some_and(|printer| printer.operation_state.is_pending());
        let can_start = unit.is_some_and(|unit| unit.can_start) && !pending && !display.closing;
        let stop = unit.is_some_and(|unit| unit.can_stop);
        let limits = unit.and_then(|unit| unit.limits);
        let valid = limits
            .is_some_and(|limits| editor.cycle(cx).is_some_and(|cycle| limits.contains(cycle)));
        let disabled = display.closing || pending || (!stop && !(can_start && valid));
        let title = t!("printers.drying.title").into_owned();
        let notice = match unit {
            None => Some(t!("printers.drying.unavailable").into_owned()),
            Some(unit) if unit.state == DryingState::Idle && unit.limits.is_none() => {
                Some(t!("printers.drying.material_unknown").into_owned())
            }
            Some(unit) if unit.state == DryingState::Idle && !unit.can_start => {
                Some(t!("printers.drying.start_blocked").into_owned())
            }
            _ => editor
                .error
                .or_else(|| can_start.then(|| t!("printers.drying.check_spools").into_owned())),
        };
        if !display.closing && !editor.focus.contains_focused(window, cx) {
            editor.focus.focus(window, cx);
        }
        let label = unit.map_or_else(
            || t!("printers.drying.unit").into_owned(),
            |unit| unit.label.clone(),
        );
        let selected = if units.len() > 1 {
            let items = units
                .iter()
                .map(|unit| (unit.id, unit.label.clone()))
                .collect::<Vec<_>>();
            let app = cx.entity().downgrade();
            let opener = editor.opener.clone();
            let id = editor.printer;
            let selected = editor.dryer;
            Button::new("drying-unit")
                .label(label)
                .icon(IconName::ChevronDown)
                .accessibility_label(t!("printers.drying.unit").into_owned())
                .disabled(display.closing || pending)
                .dropdown_menu(move |mut menu, _, _| {
                    for (dryer, name) in &items {
                        let (dryer, app, opener) = (*dryer, app.clone(), opener.clone());
                        menu = menu.item(
                            PopupMenuItem::new(name.clone())
                                .checked(dryer == selected)
                                .on_click(move |_, window, cx| {
                                    if dryer != selected {
                                        let _ = app.update(cx, |app, cx| {
                                            app.open_drying(id, dryer, opener.clone(), window, cx)
                                        });
                                    }
                                }),
                        );
                    }
                    menu.scrollable(true)
                })
                .into_any_element()
        } else {
            div()
                .text_sm()
                .font_medium()
                .child(label)
                .into_any_element()
        };
        let temperature_range = limits.map(|limits| {
            let [min, max] = limits.temperature_c;
            t!("printers.drying.temperature_range", min = min, max = max).into_owned()
        });
        let hours_range = limits.map(|limits| {
            let [min, max] = limits.duration_hours;
            t!("printers.drying.duration_range", min = min, max = max).into_owned()
        });
        let status = unit.map(|unit| {
            let mut status = div().text_sm().child(state_label(unit.state));
            if let Some(minutes) = unit.remaining_minutes.filter(|minutes| *minutes > 0) {
                status = status.child(div().child(t!(
                    "printers.drying.remaining",
                    hours = minutes / 60,
                    minutes = minutes % 60
                )));
            }
            if unit.state != DryingState::Idle
                && let Some(cycle) = unit.target
            {
                status = status.child(div().child(t!(
                    "printers.drying.target",
                    temperature = cycle.temperature_c,
                    hours = cycle.duration_hours
                )));
            }
            status
        });
        Some(
            backdrop("drying-editor-backdrop", cx.theme().overlay, display.alpha)
                .on_key_down(cx.listener(|app, event: &KeyDownEvent, _, cx| {
                    if event.keystroke.key == "escape" {
                        app.machine_controls.drying.editor = None;
                        cx.notify();
                        cx.stop_propagation();
                    }
                }))
                .child(dialog::animated(
                    "drying-editor-zoom",
                    dialog::surface("drying-editor-dialog", title.clone(), cx)
                        .track_focus(&editor.focus)
                        .child(dialog::header().child(dialog::title().child(title)))
                        .child(selected)
                        .children(status)
                        .when(
                            unit.is_some_and(|unit| unit.state == DryingState::Idle),
                            |dialog| {
                                dialog.child(
                                    h_flex()
                                        .gap_3()
                                        .child(field(
                                            &editor.temperature,
                                            t!("printers.drying.temperature").into_owned(),
                                            temperature_range,
                                            !can_start,
                                        ))
                                        .child(field(
                                            &editor.hours,
                                            t!("printers.drying.duration").into_owned(),
                                            hours_range,
                                            !can_start,
                                        )),
                                )
                            },
                        )
                        .when_some(notice, |dialog, notice| {
                            dialog.child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(notice),
                            )
                        })
                        .child(
                            dialog::footer(window, cx)
                                .child(
                                    Button::new("drying-close")
                                        .secondary()
                                        .label(t!("common.actions.close").into_owned())
                                        .on_click(cx.listener(|app, _, _, cx| {
                                            app.machine_controls.drying.editor = None;
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    Button::new("drying-submit")
                                        .primary()
                                        .label(
                                            if stop {
                                                t!("printers.drying.stop")
                                            } else {
                                                t!("printers.drying.start")
                                            }
                                            .into_owned(),
                                        )
                                        .disabled(disabled)
                                        .on_click(cx.listener(move |app, _, _, cx| {
                                            app.submit_drying(stop, cx)
                                        })),
                                ),
                        ),
                    display.alpha,
                    display.closing,
                ))
                .focus_trap("drying-editor-trap", &editor.focus)
                .into_any_element(),
        )
    }
}
