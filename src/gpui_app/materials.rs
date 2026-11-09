use std::collections::BTreeMap;
use std::time::Instant;

use super::components::alert_dialog::backdrop;
use super::components::{button::Button, dialog, input::Input, scroll_area};
use super::feedback::ToastKind;
use super::{MakerCalcGpuiApp, PrinterEntry};
use crate::i18n::t;
use crate::shared::control::{ControlError, ControlResult, MachineAction, MachineActionKind as K};
use crate::shared::machine::{SupplyBlock, SupplyOperationPhase, SupplyResource};
use crate::shared::telemetry::{FilamentSupplyTelemetry, SupplyState};
use crate::shared::workspace::ResourceId;
use crate::state::{LanConnection, LocalPrinterId};
use gpui_kit::base::{FocusTrapElement as _, StyledExt as _, TestSupportExt as _};
use gpui_kit::component::input::InputState;
use gpui_kit::component::menu::PopupMenuItem;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, ElementId, Entity, FocusHandle, Hsla,
    InteractiveElement as _, IntoElement, ParentElement as _, ScrollHandle,
    StatefulInteractiveElement as _, Styled as _, Task, Window, div, px, rgb,
};

// Presentation choices, not a compatibility list.
// Keep observed material names verbatim, including names outside this catalog.
const MATERIAL_TYPES: [&str; 13] = [
    "PLA", "PETG", "ABS", "ASA", "TPU", "PA", "PC", "PVA", "HIPS", "PP", "PLA-CF", "PETG-CF",
    "SILK",
];

fn material_options(current: &str) -> Vec<String> {
    let mut options: Vec<_> = MATERIAL_TYPES
        .iter()
        .map(|value| (*value).to_owned())
        .collect();
    if !MATERIAL_TYPES.contains(&current) {
        options.insert(0, current.to_owned());
    }
    options
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct MaterialTarget {
    printer: LocalPrinterId,
    pub(super) session: u64,
    pub(super) epoch: u64,
    supply: ResourceId,
}

#[derive(Default)]
pub(super) struct MaterialControls {
    tasks: BTreeMap<LocalPrinterId, Task<()>>,
    editor: Option<MaterialEditor>,
    editor_rendered: bool,
}

#[derive(Clone)]
struct MaterialEditor {
    target: MaterialTarget,
    position: String,
    material: String,
    color: Entity<InputState>,
    focus: FocusHandle,
    scroll: ScrollHandle,
    opener: Option<FocusHandle>,
    error: Option<String>,
}

fn block_label(block: SupplyBlock) -> String {
    match block {
        SupplyBlock::Unknown => t!("printers.materials.controls.unknown"),
        SupplyBlock::Unsupported => t!("printers.materials.controls.unsupported"),
        SupplyBlock::Stale => t!("printers.materials.controls.stale"),
        SupplyBlock::Disconnected => t!("printers.materials.controls.disconnected"),
        SupplyBlock::Busy => t!("printers.materials.controls.busy"),
        SupplyBlock::NotIdle => t!("printers.materials.controls.not_idle"),
        SupplyBlock::NoFilament => t!("printers.materials.controls.no_filament"),
        SupplyBlock::NoOperation => t!("printers.materials.controls.no_operation"),
        SupplyBlock::NotCurrent => t!("printers.materials.controls.not_current"),
    }
    .into_owned()
}

fn action_label(action: K) -> String {
    match action {
        K::LoadFilament => t!("printers.materials.controls.load"),
        K::UnloadFilament => t!("printers.materials.controls.unload"),
        K::ConfigureFilamentSupply => t!("printers.materials.controls.edit"),
        _ => t!("printers.materials.controls.cancel"),
    }
    .into_owned()
}

fn availability(
    printer: &PrinterEntry,
    target: MaterialTarget,
    action: K,
) -> Result<(), SupplyBlock> {
    if printer.state.connection != LanConnection::Connected {
        return Err(SupplyBlock::Disconnected);
    }
    if printer.session_generation != target.session
        || printer.state.connection_epoch != target.epoch
    {
        return Err(SupplyBlock::Stale);
    }
    if (printer.material_pending.is_some() && action != K::CancelFilamentOperation)
        || printer.operation_state.is_pending()
    {
        return Err(SupplyBlock::Busy);
    }
    if !printer.state.telemetry_ready || !printer.detail_state.live {
        return Err(SupplyBlock::Unknown);
    }
    let resource = printer
        .detail_state
        .machine
        .supplies
        .iter()
        .find(|s| s.id == target.supply)
        .ok_or(SupplyBlock::Stale)?;
    resource.availability(action, Instant::now())
}

fn action_button(
    id: ElementId,
    target: MaterialTarget,
    action: K,
    label: String,
    block: Option<SupplyBlock>,
    cx: &mut Context<MakerCalcGpuiApp>,
) -> Button {
    let tooltip = block.map_or_else(
        || label.clone(),
        |block| {
            t!(
                "printers.materials.controls.blocked_action",
                action = &label,
                reason = block_label(block)
            )
            .into_owned()
        },
    );
    Button::new(id)
        .ghost()
        .small()
        .w(px(30.))
        .px_0()
        .child(
            Icon::default()
                .data(if action == K::LoadFilament {
                    super::icons::SVG_LOAD_FILAMENT
                } else {
                    super::icons::SVG_UNLOAD_FILAMENT
                })
                .w(px(22.))
                .h(px(20.)),
        )
        .accessibility_label(label)
        .tooltip(tooltip)
        .disabled(block.is_some())
        .on_click(cx.listener(move |app, _, _, cx| {
            app.send_material_action(target, action, None, cx);
        }))
}

pub(super) fn panel_header(
    printer: &PrinterEntry,
    cx: &mut Context<MakerCalcGpuiApp>,
) -> AnyElement {
    let header = h_flex().items_center().justify_between().gap_2().child(
        super::components::card::title(true)
            .text_color(cx.theme().muted_foreground)
            .child(t!("printers.overview.materials").into_owned()),
    );
    let detail = &printer.detail_state;
    let humidity: Vec<_> = detail
        .devices
        .iter()
        .filter_map(|device| {
            if !detail.machine.material_station_sensors.contains(&device.id) {
                return None;
            }
            let value = device
                .readings
                .environment
                .as_ref()?
                .relative_humidity_pct?;
            Some((&device.display_name, format!("{value:.0}")))
        })
        .collect();
    let mut right = h_flex().items_center().justify_end().flex_wrap().gap_2();
    if printer.detail_state.machine.supplies.iter().any(|s| {
        s.controls
            .iter()
            .flatten()
            .any(|c| c.action == K::UnloadFilament)
    }) {
        let target = MaterialTarget {
            printer: printer.state.id,
            session: printer.session_generation,
            epoch: printer.state.connection_epoch,
            supply: printer
                .detail_state
                .machine
                .current_supply
                .unwrap_or(ResourceId(0)),
        };
        let block = if printer.detail_state.machine.current_supply.is_none() {
            Some(SupplyBlock::Unknown)
        } else {
            availability(printer, target, K::UnloadFilament).err()
        };
        right = right.child(action_button(
            "material-unload-current".into(),
            target,
            K::UnloadFilament,
            t!("printers.materials.controls.unload_current").into_owned(),
            block,
            cx,
        ));
    }
    right = right.children(humidity.iter().map(|(unit, value)| {
        let label = if humidity.len() == 1 {
            t!("printers.materials.humidity", value = value)
        } else {
            t!(
                "printers.materials.humidity_unit",
                unit = *unit,
                value = value
            )
        };
        div()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(label.into_owned())
    }));
    header.child(right).into_any_element()
}

pub(super) fn parse_hex_color(hex: &str) -> Option<Hsla> {
    let clean = hex.trim().trim_start_matches('#');
    if clean.len() != 6 {
        return None;
    }
    Some(rgb(u32::from_str_radix(clean, 16).ok()?).into())
}

pub(super) fn supply_state_label(state: SupplyState) -> String {
    match state {
        SupplyState::Loading => t!("common.status.loading"),
        SupplyState::Unloading => t!("printers.materials.unloading"),
        SupplyState::Active => t!("printers.materials.active"),
        SupplyState::Available => t!("printers.materials.available"),
        SupplyState::Empty => t!("printers.materials.empty"),
        SupplyState::Unknown => t!("common.status.unknown"),
    }
    .into_owned()
}

/// View and intentions consume only the common supply contract, including read-only providers.
pub(super) fn supply_cell(
    id: ElementId,
    supply: &FilamentSupplyTelemetry,
    external: bool,
    resource: Option<&SupplyResource>,
    printer: &PrinterEntry,
    cx: &mut Context<MakerCalcGpuiApp>,
) -> AnyElement {
    let position = if external {
        t!("printers.overview.external_spool").into_owned()
    } else {
        t!("printers.overview.slot", number = supply.id.0).into_owned()
    };
    let status = match resource.and_then(|r| r.operation) {
        Some(operation) if operation.phase == SupplyOperationPhase::Cancelling => {
            t!("printers.materials.controls.cancelling").into_owned()
        }
        Some(operation) if operation.phase == SupplyOperationPhase::Running => {
            if operation.action == K::LoadFilament {
                supply_state_label(SupplyState::Loading)
            } else {
                supply_state_label(SupplyState::Unloading)
            }
        }
        _ => supply_state_label(supply.state),
    };
    let status_color = match resource.and_then(|r| r.operation) {
        Some(operation)
            if operation.phase == SupplyOperationPhase::Cancelling
                || (operation.phase == SupplyOperationPhase::Running
                    && operation.action == K::UnloadFilament) =>
        {
            cx.theme().warning
        }
        Some(operation) if operation.phase == SupplyOperationPhase::Running => cx.theme().primary,
        _ => match supply.state {
            SupplyState::Loading | SupplyState::Active => cx.theme().primary,
            SupplyState::Unloading => cx.theme().warning,
            SupplyState::Available => cx.theme().foreground,
            _ => cx.theme().muted_foreground,
        },
    };
    let empty = supply.filament_present == Some(false);
    let color = supply.color.as_deref().and_then(parse_hex_color);
    let material = supply
        .material
        .clone()
        .unwrap_or_else(|| t!("common.status.not_configured").into_owned());
    let description = t!(
        "printers.materials.controls.description",
        position = &position,
        material = &material,
        status = &status
    )
    .into_owned();
    let mut header = h_flex().items_center().justify_between().gap_1().child(
        h_flex()
            .id((id.clone(), "position"))
            .gap_1()
            .items_center()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(
                Icon::default()
                    .data(super::icons::SVG_FILAMENT)
                    .size(px(16.)),
            )
            .child(if external {
                position.clone()
            } else {
                supply.id.0.to_string()
            })
            .aria_label(position.clone()),
    );
    if let Some(resource) = resource.filter(|r| r.controls.as_ref().is_some_and(|c| !c.is_empty()))
    {
        let target = MaterialTarget {
            printer: printer.state.id,
            session: printer.session_generation,
            epoch: printer.state.connection_epoch,
            supply: resource.id,
        };
        let app = cx.entity().downgrade();
        let mut actions = h_flex().gap_1();
        if resource
            .controls
            .iter()
            .flatten()
            .any(|c| c.action == K::LoadFilament)
        {
            actions = actions.child(action_button(
                ("material-load", resource.id.0 as usize).into(),
                target,
                K::LoadFilament,
                t!(
                    "printers.materials.controls.load_position",
                    position = &position
                )
                .into_owned(),
                availability(printer, target, K::LoadFilament).err(),
                cx,
            ));
        }
        let completed = resource
            .operation
            .is_some_and(|operation| operation.phase == SupplyOperationPhase::Completed);
        let items = resource
            .controls
            .iter()
            .flatten()
            .filter(|c| {
                matches!(
                    c.action,
                    K::LoadFilament | K::ConfigureFilamentSupply | K::CancelFilamentOperation
                )
            })
            .filter(|c| {
                c.action != K::CancelFilamentOperation
                    || resource
                        .operation
                        .is_some_and(|op| op.phase != SupplyOperationPhase::Completed)
            })
            .map(|control| {
                (
                    control.action,
                    availability(printer, target, control.action).err(),
                )
            })
            .collect::<Vec<_>>();
        let common_block = items
            .first()
            .and_then(|(_, block)| *block)
            .filter(|block| items.iter().all(|(_, other)| *other == Some(*block)));
        let explanations = if let Some(block) = common_block {
            vec![block_label(block)]
        } else {
            items
                .iter()
                .filter_map(|(action, block)| {
                    block.map(|block| {
                        t!(
                            "printers.materials.controls.blocked_action",
                            action = action_label(*action),
                            reason = block_label(block)
                        )
                        .into_owned()
                    })
                })
                .collect()
        };
        actions = actions.child(
            Button::new(("material-menu", resource.id.0 as usize))
                .ghost()
                .small()
                .label("⋯")
                .accessibility_label(
                    t!("printers.materials.controls.menu", position = &position).into_owned(),
                )
                .tooltip(t!("printers.materials.controls.menu", position = &position).into_owned())
                .dropdown_menu(move |mut menu, _, _| {
                    if completed {
                        menu = menu.label(t!("printers.materials.controls.completed").into_owned());
                    }
                    for (action, block) in &items {
                        let (action, app) = (*action, app.clone());
                        menu = menu.item(
                            PopupMenuItem::new(action_label(action))
                                .disabled(block.is_some())
                                .on_click(move |_, window, cx| {
                                    let _ = app.update(cx, |app, cx| {
                                        if action == K::ConfigureFilamentSupply {
                                            app.open_material_editor(target, window, cx);
                                        } else {
                                            app.send_material_action(target, action, None, cx);
                                        }
                                    });
                                }),
                        );
                    }
                    if !explanations.is_empty() {
                        menu = menu.separator();
                        for explanation in &explanations {
                            menu = menu.label(explanation.clone());
                        }
                    }
                    menu.scrollable(true)
                }),
        );
        if resource
            .operation
            .is_some_and(|operation| operation.phase == SupplyOperationPhase::Running)
            && resource
                .controls
                .iter()
                .flatten()
                .any(|c| c.action == K::CancelFilamentOperation)
        {
            actions = actions.child(
                Button::new(("material-cancel", resource.id.0 as usize))
                    .ghost()
                    .small()
                    .icon(IconName::Close)
                    .disabled(availability(printer, target, K::CancelFilamentOperation).is_err())
                    .accessibility_label(action_label(K::CancelFilamentOperation))
                    .tooltip(action_label(K::CancelFilamentOperation))
                    .on_click(cx.listener(move |app, _, _, cx| {
                        app.send_material_action(target, K::CancelFilamentOperation, None, cx);
                    })),
            );
        }
        header = header.child(actions);
    }
    let swatch = div().size(px(22.)).rounded_full();
    let swatch = match (empty, color) {
        (true, Some(color)) => swatch.border_2().border_color(color),
        (true, None) => swatch
            .border_2()
            .border_dashed()
            .border_color(cx.theme().border),
        (false, Some(color)) => swatch.bg(color).border_1().border_color(cx.theme().border),
        (false, None) => swatch
            .bg(cx.theme().secondary)
            .border_1()
            .border_dashed()
            .border_color(cx.theme().muted_foreground),
    };
    let mut details = Vec::new();
    if let Some(value) = resource.and_then(|resource| resource.remaining_pct) {
        details.push(t!("printers.materials.remaining_pct", value = value).into_owned());
    }
    if supply.state == SupplyState::Unknown {
        if let Some(present) = supply.filament_present {
            details.push(
                if present {
                    t!("printers.overview.filament_present")
                } else {
                    t!("printers.overview.filament_absent")
                }
                .into_owned(),
            );
        }
        if let Some(active) = supply.is_active {
            details.push(
                if active {
                    t!("printers.materials.active")
                } else {
                    t!("printers.materials.controls.not_selected")
                }
                .into_owned(),
            );
        }
    }
    v_flex()
        .id(id)
        .aria_label(description)
        .test_support()
        .size_full()
        .gap_2()
        .child(header)
        .child(
            h_flex()
                .gap_3()
                .items_center()
                .child(
                    div()
                        .flex_none()
                        .p(px(2.))
                        .rounded_full()
                        .border_2()
                        .border_color(if supply.is_active == Some(true) {
                            cx.theme().primary
                        } else {
                            cx.theme().transparent
                        })
                        .child(swatch),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_0p5()
                        .child(
                            div()
                                .text_base()
                                .font_semibold()
                                .truncate()
                                .when(empty, |name| name.text_color(cx.theme().muted_foreground))
                                .child(material),
                        )
                        .child(div().text_xs().text_color(status_color).child(status))
                        .children(details.into_iter().map(|text| {
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(text)
                        })),
                ),
        )
        .into_any_element()
}

pub(super) fn result_message(error: ControlError) -> String {
    match error {
        ControlError::Unsupported => block_label(SupplyBlock::Unsupported),
        ControlError::Disconnected => block_label(SupplyBlock::Disconnected),
        ControlError::StaleResource => block_label(SupplyBlock::Stale),
        ControlError::Busy => block_label(SupplyBlock::Busy),
        ControlError::InvalidState => t!("printers.materials.controls.invalid_state").into_owned(),
        ControlError::InvalidData => t!("printers.materials.controls.invalid_data").into_owned(),
        ControlError::SendFailed => t!("printers.materials.controls.send_failed").into_owned(),
        ControlError::PrinterRejected => t!("printers.materials.controls.rejected").into_owned(),
    }
}

impl MakerCalcGpuiApp {
    pub(super) fn sync_material_controls(&mut self) {
        self.material_controls
            .tasks
            .retain(|id, _| self.printers.contains_key(id));
    }

    fn send_material_action(
        &mut self,
        target: MaterialTarget,
        kind: K,
        values: Option<(String, String)>,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(printer) = self.printers.get(&target.printer) else {
            return false;
        };
        if let Err(block) = availability(printer, target, kind) {
            self.push_toast(ToastKind::Warning, block_label(block), None, cx);
            return false;
        }
        if kind == K::UnloadFilament
            && printer.detail_state.machine.current_supply != Some(target.supply)
        {
            self.push_toast(
                ToastKind::Warning,
                block_label(SupplyBlock::NotCurrent),
                None,
                cx,
            );
            return false;
        }
        let action = match kind {
            K::LoadFilament => MachineAction::LoadFilament {
                supply_id: target.supply,
            },
            K::UnloadFilament => MachineAction::UnloadFilament {
                supply_id: target.supply,
            },
            K::CancelFilamentOperation => MachineAction::CancelFilamentOperation {
                supply_id: target.supply,
            },
            K::ConfigureFilamentSupply => {
                let Some((material, color)) = values else {
                    return false;
                };
                MachineAction::ConfigureFilamentSupply {
                    supply_id: target.supply,
                    material,
                    color,
                }
            }
            _ => return false,
        };
        let result = match printer.operation.machine_action(target.epoch, action) {
            Ok(result) => result,
            Err(error) => {
                self.push_toast(ToastKind::Error, result_message(error), None, cx);
                return false;
            }
        };
        self.printers
            .get_mut(&target.printer)
            .unwrap()
            .material_pending = Some(target);
        let task = cx.spawn(async move |app, cx| {
            let result = result.await.unwrap_or(ControlResult::Unknown);
            let _ = app.update(cx, |app, cx| {
                let Some(printer) = app.printers.get_mut(&target.printer) else {
                    return;
                };
                if printer.material_pending != Some(target) {
                    return;
                }
                printer.material_pending = None;
                if printer.session_generation != target.session
                    || printer.state.connection_epoch != target.epoch
                {
                    cx.notify();
                    return;
                }
                let (tone, message) = match result {
                    ControlResult::Accepted => (
                        ToastKind::Info,
                        t!("printers.materials.controls.accepted").into_owned(),
                    ),
                    ControlResult::Rejected(error) => (ToastKind::Error, result_message(error)),
                    ControlResult::Unknown => (
                        ToastKind::Warning,
                        t!("printers.materials.controls.uncertain").into_owned(),
                    ),
                };
                app.push_toast(tone, message, None, cx);
                cx.notify();
            });
        });
        self.material_controls.tasks.insert(target.printer, task);
        cx.notify();
        true
    }

    fn open_material_editor(
        &mut self,
        target: MaterialTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(printer) = self.printers.get(&target.printer) else {
            return;
        };
        if let Err(block) = availability(printer, target, K::ConfigureFilamentSupply) {
            self.push_toast(ToastKind::Warning, block_label(block), None, cx);
            return;
        }
        let resource = printer
            .detail_state
            .machine
            .supplies
            .iter()
            .find(|s| s.id == target.supply)
            .unwrap();
        let material = resource.reading.material.clone().unwrap_or_default();
        let color = resource.reading.color.clone().unwrap_or_default();
        let position = match resource.kind {
            crate::shared::machine::SupplyKind::ExternalSpool => {
                t!("printers.overview.external_spool").into_owned()
            }
            _ => t!("printers.overview.slot", number = resource.reading.id.0).into_owned(),
        };
        self.material_controls.editor = Some(MaterialEditor {
            target,
            position,
            material,
            color: cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value(color)
                    .placeholder("#RRGGBB")
            }),
            focus: cx.focus_handle(),
            scroll: ScrollHandle::default(),
            opener: window.focused(cx),
            error: None,
        });
        cx.notify();
    }

    fn close_material_editor(&mut self, cx: &mut Context<Self>) {
        self.material_controls.editor = None;
        cx.notify();
    }

    fn save_material_editor(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = &self.material_controls.editor else {
            return;
        };
        let (target, material, color) = (
            editor.target,
            editor.material.clone(),
            editor.color.read(cx).value().to_string(),
        );
        let action = MachineAction::ConfigureFilamentSupply {
            supply_id: target.supply,
            material: material.clone(),
            color: color.clone(),
        };
        if action.validate().is_err() {
            self.material_controls.editor.as_mut().unwrap().error =
                Some(result_message(ControlError::InvalidData));
            cx.notify();
            return;
        }
        if let Some(printer) = self.printers.get(&target.printer) {
            if let Err(block) = availability(printer, target, K::ConfigureFilamentSupply) {
                self.material_controls.editor.as_mut().unwrap().error = Some(block_label(block));
                cx.notify();
                return;
            }
        } else {
            self.close_material_editor(cx);
            return;
        }
        if self.send_material_action(
            target,
            K::ConfigureFilamentSupply,
            Some((material, color)),
            cx,
        ) {
            self.close_material_editor(cx);
        }
    }

    pub(super) fn render_material_editor(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        // Do not mount a Presence animation on every frame before this dialog is ever used.
        if self.material_controls.editor.is_none() && !self.material_controls.editor_rendered {
            return None;
        }
        let Some(display) = dialog::present(
            "material-editor-presence",
            self.material_controls.editor.clone(),
            self.material_controls
                .editor
                .as_ref()
                .and_then(|editor| editor.opener.clone()),
            window,
            cx,
        ) else {
            self.material_controls.editor_rendered = false;
            return None;
        };
        self.material_controls.editor_rendered = true;
        let editor = display.value;
        let materials = material_options(&editor.material);
        let selected = materials
            .iter()
            .position(|value| value == &editor.material)
            .expect("the current material is retained in the menu");
        let selected_material = editor.material.clone();
        let material_label = if selected_material.is_empty() {
            t!("printers.materials.controls.material_placeholder").into_owned()
        } else {
            selected_material.clone()
        };
        let material_target = editor.target;
        let app = cx.entity().downgrade();
        let title = t!(
            "printers.materials.controls.edit_title",
            position = &editor.position
        )
        .into_owned();
        if !display.closing && !editor.focus.contains_focused(window, cx) {
            editor.focus.focus(window, cx);
        }
        Some(
            backdrop(
                "material-editor-backdrop",
                cx.theme().overlay,
                display.alpha,
            )
            .on_key_down(
                cx.listener(|app, event: &gpui_kit::KeyDownEvent, window, cx| {
                    if event.keystroke.key == "escape" {
                        if let Some(opener) = app
                            .material_controls
                            .editor
                            .as_ref()
                            .and_then(|e| e.opener.clone())
                        {
                            opener.focus(window, cx);
                        }
                        app.close_material_editor(cx);
                        cx.stop_propagation();
                    }
                }),
            )
            .child(dialog::animated(
                "material-editor-zoom",
                dialog::surface("material-editor-dialog", title.clone(), cx)
                    .max_w(px(420.))
                    .max_h(window.viewport_size().height - px(48.))
                    .child(dialog::header().child(dialog::title().child(title)))
                    .child(
                        Button::new("material-editor-close")
                            .ghost()
                            .small()
                            .icon(IconName::Close)
                            .accessibility_label(t!("common.actions.close").into_owned())
                            .absolute()
                            .top(px(8.))
                            .right(px(8.))
                            .on_click(cx.listener(|app, _, window, cx| {
                                app.dismiss_material_editor(window, cx);
                            })),
                    )
                    .child(
                        v_flex()
                            .id("material-editor-scroll")
                            .test_support()
                            .flex_auto()
                            .min_h_0()
                            .overflow_y_scroll()
                            .track_scroll(&editor.scroll)
                            .map(|area| scroll_area::vertical(area, &editor.scroll, window, cx))
                            .child(
                                v_flex()
                                    .flex_none()
                                    .gap_3()
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(t!(
                                                "printers.materials.controls.configuration_hint"
                                            )),
                                    )
                                    .child(
                                        div()
                                            .text_sm()
                                            .child(t!("printers.materials.controls.material")),
                                    )
                                    .child(
                                        Button::new("material-editor-material")
                                            .outline()
                                            .w_full()
                                            .disabled(display.closing)
                                            .child(div().flex_1().min_w_0().truncate().text_left()
                                                .child(material_label.clone()))
                                            .accessibility_label(
                                                t!("printers.materials.controls.material_selection", material = &material_label)
                                                    .into_owned(),
                                            )
                                            .select_menu(selected, move |mut menu, _, _| {
                                                for value in &materials {
                                                    let app = app.clone();
                                                    let material = value.clone();
                                                    menu = menu.item(PopupMenuItem::new(if value.is_empty() {
                                                        t!("printers.materials.controls.material_placeholder").into_owned()
                                                    } else { value.clone() })
                                                        .checked(*value == selected_material)
                                                        .disabled(value.is_empty())
                                                        .on_click(move |_, _, cx| {
                                                            let _ = app.update(cx, |app, cx| {
                                                                if let Some(editor) = app.material_controls.editor.as_mut()
                                                                    && editor.target == material_target
                                                                {
                                                                    editor.material = material.clone();
                                                                    editor.error = None;
                                                                    cx.notify();
                                                                }
                                                            });
                                                        }));
                                                }
                                                menu.scrollable(true)
                                            }),
                                    )
                                    .child(
                                        div().text_sm().text_color(cx.theme().muted_foreground)
                                            .child(t!("printers.materials.controls.catalog_hint")),
                                    )
                                    .child(
                                        div()
                                            .text_sm()
                                            .child(t!("printers.materials.controls.color")),
                                    )
                                    .child(
                                        Input::new(&editor.color)
                                            .id("material-editor-color")
                                            .aria_label(
                                                t!("printers.materials.controls.color")
                                                    .into_owned(),
                                            ),
                                    )
                                    .children(editor.error.map(|error| {
                                        div().text_sm().text_color(cx.theme().danger).child(error)
                                    })),
                            ),
                    )
                    .child(
                        dialog::footer(window, cx)
                            .child(
                                Button::new("material-editor-cancel")
                                    .secondary()
                                    .label(t!("common.actions.cancel").into_owned())
                                    .on_click(cx.listener(|app, _, window, cx| {
                                        app.dismiss_material_editor(window, cx);
                                    })),
                            )
                            .child(
                                Button::new("material-editor-save")
                                    .primary()
                                    .label(t!("common.actions.save").into_owned())
                                    .on_click(cx.listener(|app, _, window, cx| {
                                        let opener = app
                                            .material_controls
                                            .editor
                                            .as_ref()
                                            .and_then(|e| e.opener.clone());
                                        app.save_material_editor(cx);
                                        if app.material_controls.editor.is_none()
                                            && let Some(opener) = opener
                                        {
                                            opener.focus(window, cx);
                                        }
                                    })),
                            ),
                    ),
                display.alpha,
                display.closing,
            ))
            .focus_trap("material-editor-trap", &editor.focus)
            .into_any_element(),
        )
    }

    fn dismiss_material_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(opener) = self
            .material_controls
            .editor
            .as_ref()
            .and_then(|e| e.opener.clone())
        {
            opener.focus(window, cx);
        }
        self.close_material_editor(cx);
    }
}

#[cfg(test)]
pub(super) use material_controls_tests::open_test_material_editor;
