use super::components::button::Button;
use super::components::checkbox::Checkbox;
use super::components::field::Field;
use super::components::hover_card::HoverCard;
use super::components::input::Input;
use super::components::radio_group::{Radio, RadioGroup};
use super::components::scroll_area::ScrollAreaExt as _;
use crate::i18n::t;
use gpui_kit::base::{
    Align, FocusTrapElement as _, Placement, StyledExt as _, TestSupportExt as _,
};
use gpui_kit::component::input::InputState;
use gpui_kit::component::{ActiveTheme as _, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, Focusable as _, InteractiveElement as _,
    IntoElement, KeyDownEvent, ParentElement as _, Role, StatefulInteractiveElement as _,
    Styled as _, Window, div, px,
};

use crate::config::{MoonrakerLightBinding, MoonrakerLightBindings, PrinterDraft, Protocol};
use crate::discovery::Candidate;
use crate::edge::{EdgeAction, EdgeCommand};
use crate::shared::printer::{CredentialStatus, EditableConnectionView};
use crate::state::LocalPrinterId;

use super::MakerCalcGpuiApp;
use super::components::{alert_dialog::backdrop, dialog};
use super::feedback::{ToastKind, log_config_operation};

#[derive(Clone)]
pub(crate) struct PrinterForm {
    pub(crate) editing: Option<LocalPrinterId>,
    pub(crate) protocol: Protocol,
    pub(crate) build_area_light: Entity<InputState>,
    pub(crate) toolhead_light: Entity<InputState>,
    pub(crate) name: Entity<InputState>,
    pub(crate) host: Entity<InputState>,
    pub(crate) port: Entity<InputState>,
    pub(crate) api_key: Entity<InputState>,
    pub(crate) serial_number: Entity<InputState>,
    pub(crate) check_code: Entity<InputState>,
    pub(crate) trusted_lan: bool,
    pub(crate) tls: bool,
    pub(crate) discovered: Option<Candidate>,
    pub(crate) credential_status: CredentialStatus,
    pub(crate) credential_rejected: bool,
    pub(crate) duplicate_match: Option<(LocalPrinterId, String)>,
}

pub(super) fn same_printer_endpoint(
    draft: &PrinterDraft,
    connection: &EditableConnectionView,
) -> bool {
    let normalize = |host: &str| {
        let host = host.trim().trim_matches(['[', ']']).trim_end_matches('.');
        host.parse::<std::net::IpAddr>()
            .map_or_else(|_| host.to_ascii_lowercase(), |ip| ip.to_string())
    };
    normalize(&draft.host) == normalize(&connection.host)
        && draft.port == connection.port
        && draft.tls == connection.tls
        && draft.protocol.unwrap_or_default() == connection.protocol
}

pub(super) fn printer_save_action(
    editing: Option<LocalPrinterId>,
    draft: PrinterDraft,
) -> EdgeAction {
    match editing {
        Some(id) => EdgeAction::UpdatePrinter { id, draft },
        None => EdgeAction::AddPrinter(draft),
    }
}

fn light_binding_help(id: &'static str, title: String, example: &'static str) -> HoverCard {
    let help = t!("printers.form.light_help", example = example).into_owned();
    HoverCard::new(
        format!("{id}-hover"),
        super::components::input_group::icon_button(
            Button::new(id)
                .ghost()
                .icon(IconName::Info)
                .accessibility_label(
                    t!(
                        "printers.form.light_accessibility",
                        title = title,
                        help = help
                    )
                    .into_owned(),
                ),
        ),
        v_flex()
            .id(format!("{id}-content"))
            .test_support()
            .role(Role::Tooltip)
            .aria_label(help.clone())
            .w_full()
            .min_w_0()
            .gap_2()
            .whitespace_normal()
            .child(div().font_semibold().child(title.clone()))
            .child(help.clone()),
    )
    .width(px(340.))
    .placement(Placement::Top, Align::End)
}

impl MakerCalcGpuiApp {
    pub(super) fn empty_printer_form(window: &mut Window, cx: &mut Context<Self>) -> PrinterForm {
        PrinterForm {
            editing: None,
            protocol: Protocol::Moonraker,
            build_area_light: cx.new(|cx| {
                InputState::new(window, cx).placeholder(t!("printers.form.light_area.placeholder"))
            }),
            toolhead_light: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(t!("printers.form.light_toolhead.placeholder"))
            }),
            name: cx.new(|cx| {
                InputState::new(window, cx).placeholder(t!("printers.form.name.placeholder"))
            }),
            host: cx.new(|cx| InputState::new(window, cx).placeholder("192.168.1.100")),
            port: cx.new(|cx| InputState::new(window, cx).default_value("7125")),
            api_key: cx.new(|cx| {
                InputState::new(window, cx).placeholder(t!("printers.form.api.optional"))
            }),
            serial_number: cx.new(|cx| {
                InputState::new(window, cx).placeholder(t!("printers.form.serial.placeholder"))
            }),
            check_code: cx.new(|cx| {
                InputState::new(window, cx).placeholder(t!("printers.form.check_code.placeholder"))
            }),
            trusted_lan: true,
            tls: false,
            discovered: None,
            credential_status: CredentialStatus::None,
            credential_rejected: false,
            duplicate_match: None,
        }
    }

    pub(super) fn open_new_printer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.printer_form.is_none() {
            self.printer_form_opener = window.focused(cx);
        }
        self.clear_finder();
        self.mutation_error = None;
        self.printer_form = Some(Self::empty_printer_form(window, cx));
        self.printer_form_focus.focus(window, cx);
        cx.notify();
    }

    pub(super) fn open_edit_printer(
        &mut self,
        id: LocalPrinterId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.printer_form.is_none() {
            self.printer_form_opener = window.focused(cx);
        }
        self.clear_finder();
        let Some(printer) = self.printers.get(&id) else {
            return;
        };
        let view = &printer.connection_view;
        let is_configured = view.credential_status == CredentialStatus::Configured;
        let is_rejected = printer.state.connection == crate::state::LanConnection::NeedsCredentials;
        let api_key_placeholder = if is_rejected {
            if is_configured {
                t!("printers.form.credentials.rejected").into_owned()
            } else {
                t!("printers.form.api.rejected").into_owned()
            }
        } else if is_configured {
            t!("printers.form.api.saved").into_owned()
        } else {
            t!("printers.form.api.connect_optional").into_owned()
        };
        let ff_placeholder = if is_rejected {
            if is_configured {
                t!("printers.form.credentials.rejected").into_owned()
            } else {
                t!("printers.form.credentials.required").into_owned()
            }
        } else if is_configured {
            t!("printers.form.credentials.saved").into_owned()
        } else {
            String::new()
        };
        self.mutation_error = None;
        self.printer_form = Some(PrinterForm {
            editing: Some(id),
            protocol: view.protocol,
            build_area_light: cx.new(|cx| {
                InputState::new(window, cx).default_value(
                    view.light_bindings
                        .build_area
                        .as_ref()
                        .map(MoonrakerLightBinding::display_value)
                        .unwrap_or_default(),
                )
            }),
            toolhead_light: cx.new(|cx| {
                InputState::new(window, cx).default_value(
                    view.light_bindings
                        .toolhead
                        .as_ref()
                        .map(MoonrakerLightBinding::display_value)
                        .unwrap_or_default(),
                )
            }),
            name: cx.new(|cx| InputState::new(window, cx).default_value(printer.name.clone())),
            host: cx.new(|cx| InputState::new(window, cx).default_value(view.host.clone())),
            port: cx.new(|cx| InputState::new(window, cx).default_value(view.port.to_string())),
            api_key: cx.new(|cx| InputState::new(window, cx).placeholder(api_key_placeholder)),
            serial_number: cx
                .new(|cx| InputState::new(window, cx).placeholder(ff_placeholder.clone())),
            check_code: cx.new(|cx| InputState::new(window, cx).placeholder(ff_placeholder)),
            trusted_lan: view.trusted_lan,
            tls: view.tls,
            discovered: None,
            credential_status: view.credential_status,
            credential_rejected: is_rejected,
            duplicate_match: None,
        });
        self.printer_form_focus.focus(window, cx);
        cx.notify();
    }

    pub(super) fn close_printer_form(&mut self, cx: &mut Context<Self>) {
        if !self.mutation_pending {
            self.clear_finder();
            self.printer_form = None;
            self.mutation_error = None;
            cx.notify();
        }
    }

    pub(super) fn set_form_protocol(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(form) = self.printer_form.as_mut() {
            form.protocol = if index == 1 {
                Protocol::Flashforge
            } else {
                Protocol::Moonraker
            };
            if form.protocol == Protocol::Flashforge {
                form.tls = false;
            }
            cx.notify();
        }
    }

    pub(super) fn submit_new_printer(&mut self, cx: &mut Context<Self>) {
        if self.mutation_pending || self.searching_printer() {
            return;
        }
        let Some(form) = self.printer_form.as_mut() else {
            return;
        };
        let value = |input: &Entity<InputState>| input.read(cx).value().to_string();
        let name = value(&form.name);
        let host = value(&form.host);
        let port = match value(&form.port).trim().parse::<u16>() {
            Ok(port) if port > 0 => port,
            _ => {
                self.mutation_error = Some(t!("printers.form.errors.port").into_owned());
                cx.notify();
                return;
            }
        };
        let (trusted_lan, api_key) = match form.protocol {
            Protocol::Moonraker => {
                let key = value(&form.api_key);
                let trimmed = key.trim();
                if trimmed.contains('•') {
                    self.mutation_error = Some(t!("printers.form.errors.api_mask").into_owned());
                    cx.notify();
                    return;
                }
                let api_key = if form.editing.is_some() {
                    if form.trusted_lan || trimmed.is_empty() {
                        None
                    } else {
                        Some(trimmed.to_owned())
                    }
                } else {
                    (!form.trusted_lan)
                        .then(|| trimmed.to_owned())
                        .filter(|k| !k.is_empty())
                };
                (form.trusted_lan, api_key)
            }
            Protocol::Flashforge => {
                let serial = value(&form.serial_number);
                let code = value(&form.check_code);
                let s_trim = serial.trim();
                let c_trim = code.trim();
                if s_trim.contains('•') || c_trim.contains('•') {
                    self.mutation_error =
                        Some(t!("printers.form.errors.credential_mask").into_owned());
                    cx.notify();
                    return;
                }
                if s_trim.contains(':') || c_trim.contains(':') {
                    self.mutation_error = Some(t!("printers.form.errors.colon").into_owned());
                    cx.notify();
                    return;
                }
                let (trusted_lan, credential) = if form.editing.is_some() {
                    match (s_trim.is_empty(), c_trim.is_empty()) {
                        (true, true) => (false, None),
                        (false, false) => (false, Some(format!("{s_trim}:{c_trim}"))),
                        _ => {
                            self.mutation_error =
                                Some(t!("printers.form.errors.credentials_pair").into_owned());
                            cx.notify();
                            return;
                        }
                    }
                } else {
                    match (s_trim.is_empty(), c_trim.is_empty()) {
                        (true, true) => (true, None),
                        (false, true) => (false, Some(s_trim.to_owned())),
                        (false, false) => (false, Some(format!("{s_trim}:{c_trim}"))),
                        (true, false) => {
                            self.mutation_error = Some(
                                t!("printers.form.errors.serial_required")
                                    .into_owned()
                                    .to_owned(),
                            );
                            cx.notify();
                            return;
                        }
                    }
                };
                (trusted_lan, credential)
            }
        };
        let light_bindings = if form.protocol == Protocol::Moonraker {
            let build_area = MoonrakerLightBinding::parse_optional(&value(&form.build_area_light));
            let toolhead = MoonrakerLightBinding::parse_optional(&value(&form.toolhead_light));
            match (build_area, toolhead) {
                (Ok(build_area), Ok(toolhead)) => MoonrakerLightBindings {
                    build_area,
                    toolhead,
                },
                (Err(error), _) | (_, Err(error)) => {
                    self.mutation_error = Some(error.to_string());
                    cx.notify();
                    return;
                }
            }
        } else {
            MoonrakerLightBindings::default()
        };
        let toast_name = name.clone();
        let draft = PrinterDraft {
            name,
            host,
            port,
            tls: form.tls,
            api_key,
            trusted_lan,
            protocol: Some(form.protocol),
            light_bindings,
        };
        if form.editing.is_none()
            && let Some((&existing_id, existing_printer)) = self
                .printers
                .iter()
                .find(|(_, printer)| same_printer_endpoint(&draft, &printer.connection_view))
        {
            let existing_name = existing_printer.name.clone();
            form.duplicate_match = Some((existing_id, existing_name.clone()));
            self.mutation_error =
                Some(t!("printers.form.duplicate", name = &existing_name).into_owned());
            cx.notify();
            return;
        }
        form.duplicate_match = None;
        let discovered = form.discovered.clone();
        let log_id = form.editing;
        let log_action = if log_id.is_some() {
            "update_printer"
        } else {
            "add_printer"
        };
        let actions = self.edge_actions.clone();
        self.mutation_pending = true;
        self.mutation_error = None;
        cx.spawn(async move |app, cx| {
            let result = async {
                if let Some(candidate) = discovered {
                    crate::edge::verify_discovered_printer(&candidate, &draft).await?;
                }
                let action = printer_save_action(log_id, draft);
                let (reply, response) = tokio::sync::oneshot::channel();
                actions
                    .try_send(EdgeCommand { action, reply })
                    .map_err(|_| t!("common.errors.send").into_owned())?;
                response
                    .await
                    .unwrap_or_else(|_| Err(t!("common.errors.controller_closed").into_owned()))
            }
            .await;
            let _ = app.update(cx, |app, cx| {
                app.mutation_pending = false;
                log_config_operation(log_action, log_id, &result, app);
                match result {
                    Ok(()) => {
                        app.clear_finder();
                        app.printer_form = None;
                        app.push_toast(
                            ToastKind::Success,
                            if log_id.is_some() {
                                t!("printers.form.saved").into_owned()
                            } else {
                                t!("printers.form.added").into_owned()
                            },
                            Some(toast_name),
                            cx,
                        );
                    }
                    Err(error) => {
                        app.push_toast(
                            ToastKind::Error,
                            t!("printers.form.save_error").into_owned(),
                            Some(error.clone()),
                            cx,
                        );
                        app.mutation_error = Some(error);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn render_printer_form(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let presence = dialog::present(
            "printer-form-dialog",
            self.printer_form.clone().map(|form| {
                (
                    form,
                    self.finder
                        .as_ref()
                        .map(|finder| std::rc::Rc::new(finder.view_for_exit())),
                )
            }),
            self.printer_form_opener.clone(),
            window,
            cx,
        )?;
        let (form, finder) = &presence.value;
        let editing = form.editing.is_some();
        let searching = finder.as_ref().is_some_and(|finder| !finder.confirming);
        let search_panel = searching.then(|| self.render_finder(finder.as_deref(), cx));
        let title = if searching {
            t!("printers.form.search_title").into_owned()
        } else if editing {
            t!("printers.form.edit_title").into_owned()
        } else {
            if form.discovered.is_some() {
                t!("printers.form.confirm_title").into_owned()
            } else {
                t!("printers.form.add_title").into_owned()
            }
        };
        let protocol_index = usize::from(form.protocol == Protocol::Flashforge);
        let is_discovered = form.discovered.is_some();
        let destination_locked = self.mutation_pending || is_discovered;
        let credentials = if form.protocol == Protocol::Flashforge {
            v_flex()
                .gap_3()
                .when(form.credential_rejected, |col| {
                    col.child(super::feedback::error_alert(
                        "printer-credentials-rejected",
                        &t!("printers.form.credentials.rejected_help"),
                        cx,
                    ))
                })
                .when(
                    !form.credential_rejected
                        && form.credential_status == CredentialStatus::Configured,
                    |col| {
                        col.child(
                            super::components::field::description(cx)
                                .child(t!("printers.form.credentials.saved_help")),
                        )
                    },
                )
                .child(
                    Field::new("field-printer-serial")
                        .focus(form.serial_number.focus_handle(cx))
                        .disabled(self.mutation_pending)
                        .label(t!("printers.form.serial.label").into_owned())
                        .child(
                            Input::new(&form.serial_number)
                                .id("printer-form-serial")
                                .aria_label(t!("printers.form.serial.label").into_owned())
                                .disabled(self.mutation_pending),
                        ),
                )
                .child(
                    Field::new("field-printer-check-code")
                        .focus(form.check_code.focus_handle(cx))
                        .disabled(self.mutation_pending)
                        .label(t!("printers.form.check_code.label").into_owned())
                        .child(
                            Input::new(&form.check_code)
                                .id("printer-form-check-code")
                                .aria_label(t!("printers.form.check_code.label").into_owned())
                                .disabled(self.mutation_pending),
                        ),
                )
                .into_any_element()
        } else {
            v_flex()
                .gap_3()
                .child(
                    Checkbox::new("printer-tls")
                        .label(t!("printers.form.tls").into_owned())
                        .checked(form.tls)
                        .disabled(destination_locked)
                        .on_change(cx.listener(|app, value, _, cx| {
                            if let Some(form) = app.printer_form.as_mut() {
                                form.tls = *value;
                            }
                            cx.notify();
                        })),
                )
                .child(
                    Field::new("field-trusted-lan").child(
                        Checkbox::new("trusted-lan")
                            .label(t!("printers.form.trusted_lan").into_owned())
                            .checked(form.trusted_lan)
                            .disabled(self.mutation_pending)
                            .on_change(cx.listener(|app, value, _, cx| {
                                if let Some(form) = app.printer_form.as_mut() {
                                    form.trusted_lan = *value;
                                    cx.notify();
                                }
                            })),
                    ),
                )
                .when(form.trusted_lan, |fields| {
                    fields.child(
                        super::components::field::description(cx)
                            .text_color(
                                if editing && form.credential_status == CredentialStatus::Configured
                                {
                                    cx.theme().yellow
                                } else {
                                    cx.theme().muted_foreground
                                },
                            )
                            .child(
                                if editing && form.credential_status == CredentialStatus::Configured
                                {
                                    t!("printers.form.trusted_lan_remove").into_owned()
                                } else {
                                    t!("printers.form.trusted_lan_help").into_owned()
                                },
                            ),
                    )
                })
                .when(!form.trusted_lan, |fields| {
                    fields
                        .when(form.credential_rejected, |col| {
                            col.child(super::feedback::error_alert(
                                "printer-api-rejected",
                                &t!("printers.form.api.rejected_help"),
                                cx,
                            ))
                        })
                        .when(
                            !form.credential_rejected
                                && form.credential_status == CredentialStatus::Configured,
                            |col| {
                                col.child(
                                    super::components::field::description(cx)
                                        .child(t!("printers.form.api.saved_help")),
                                )
                            },
                        )
                        .child(
                            Field::new("field-printer-api-key")
                                .focus(form.api_key.focus_handle(cx))
                                .disabled(self.mutation_pending)
                                .label(t!("printers.form.api.label").into_owned())
                                .child(
                                    Input::new(&form.api_key)
                                        .id("printer-form-api-key")
                                        .aria_label(t!("printers.form.api.label").into_owned())
                                        .disabled(self.mutation_pending),
                                ),
                        )
                })
                .child(
                    Field::new("field-printer-build-area-light")
                        .focus(form.build_area_light.focus_handle(cx))
                        .disabled(self.mutation_pending)
                        .label(t!("printers.form.light_area.label").into_owned())
                        .child(
                            Input::new(&form.build_area_light)
                                .aria_label(t!("printers.form.light_area.label").into_owned())
                                .id("printer-build-area-light")
                                .disabled(self.mutation_pending)
                                .suffix(light_binding_help(
                                    "build-area-light-help",
                                    t!("printers.form.light_area.label").into_owned(),
                                    "output_pin caselight",
                                )),
                        ),
                )
                .child(
                    Field::new("field-printer-toolhead-light")
                        .focus(form.toolhead_light.focus_handle(cx))
                        .disabled(self.mutation_pending)
                        .label(t!("printers.form.light_toolhead.label").into_owned())
                        .child(
                            Input::new(&form.toolhead_light)
                                .aria_label(t!("printers.form.light_toolhead.label").into_owned())
                                .id("printer-toolhead-light")
                                .disabled(self.mutation_pending)
                                .suffix(light_binding_help(
                                    "toolhead-light-help",
                                    t!("printers.form.light_toolhead.label").into_owned(),
                                    "output_pin toolhead",
                                )),
                        ),
                )
                .into_any_element()
        };

        Some(
            backdrop(
                "printer-form-modal-backdrop",
                cx.theme().overlay,
                presence.alpha,
            )
            .on_click(cx.listener(|app, _, _, cx| app.close_printer_form(cx)))
            .child(dialog::animated(
                "printer-form-zoom",
                dialog::surface("printer-form-modal-dialog", title.clone(), cx)
                    .max_w(px(600.))
                    .w_full()
                    .h((window.viewport_size().height - px(48.))
                        .min(px(780.))
                        .max(px(240.)))
                    .max_h(
                        (window.viewport_size().height - px(48.))
                            .min(px(780.))
                            .max(px(240.)),
                    )
                    .on_key_down(cx.listener(|app, event: &KeyDownEvent, _, cx| {
                        if event.keystroke.key == "escape" && app.pending_delete.is_none() {
                            app.close_printer_form(cx);
                            cx.stop_propagation();
                        }
                    }))
                    .child(
                        dialog::header()
                            .id("printer-form-title-row")
                            .test_support()
                            .child(dialog::title().child(title)),
                    )
                    .child(
                        Button::new("close-form-x")
                            .ghost()
                            .small()
                            .size(px(28.))
                            .absolute()
                            .top(px(8.))
                            .right(px(8.))
                            .icon(IconName::Close)
                            .accessibility_label(t!("common.actions.close").into_owned())
                            .disabled(self.mutation_pending)
                            .on_click(cx.listener(|app, _, _, cx| app.close_printer_form(cx))),
                    )
                    .child(
                        v_flex()
                            .flex_auto()
                            .min_h_0()
                            .gap_4()
                            .scroll_area()
                            .when_some(search_panel, |panel, search| panel.child(search))
                            .when(!searching, |panel| {
                                panel
                                    .when(finder.is_some(), |panel| {
                                        panel.child(
                                            Button::new("finder-back")
                                                .ghost()
                                                .small()
                                                .label(t!("printers.form.back").into_owned())
                                                .disabled(self.mutation_pending)
                                                .on_click(cx.listener(|app, _, window, cx| {
                                                    if let Some(finder) = app.finder.as_mut() {
                                                        finder.confirming = false;
                                                    }
                                                    app.printer_form_focus.focus(window, cx);
                                                    cx.notify();
                                                })),
                                        )
                                    })
                                    .when(is_discovered, |panel| {
                                        panel.child(
                                            div()
                                                .id("printer-form-discovered-notice")
                                                .test_support()
                                                .text_xs()
                                                .text_color(cx.theme().muted_foreground)
                                                .whitespace_normal()
                                                .child(if editing {
                                                    t!("printers.form.discovered_existing")
                                                        .into_owned()
                                                } else {
                                                    t!("printers.form.discovered_new").into_owned()
                                                }),
                                        )
                                    })
                                    .child(
                                        Field::new("field-printer-protocol")
                                            .disabled(destination_locked)
                                            .label(t!("printers.form.protocol").into_owned())
                                            .child(
                                                RadioGroup::new("printer-protocol")
                                                    .children(
                                                        ["Moonraker / Klipper", "FlashForge"]
                                                            .into_iter()
                                                            .enumerate()
                                                            .map(|(index, text)| {
                                                                Radio::new(index).label(text)
                                                            }),
                                                    )
                                                    .selected_index(Some(protocol_index))
                                                    .disabled(destination_locked)
                                                    .on_change(cx.listener(|app, index, _, cx| {
                                                        app.set_form_protocol(*index, cx);
                                                    })),
                                            ),
                                    )
                                    .child(
                                        Field::new("field-printer-name")
                                            .focus(form.name.focus_handle(cx))
                                            .disabled(self.mutation_pending)
                                            .label(t!("printers.form.name.label").into_owned())
                                            .child(
                                                Input::new(&form.name)
                                                    .aria_label(
                                                        t!("printers.form.name.label").into_owned(),
                                                    )
                                                    .id("printer-form-name")
                                                    .disabled(self.mutation_pending),
                                            ),
                                    )
                                    .child(
                                        h_flex()
                                            .gap_3()
                                            .child(
                                                Field::new("field-printer-host")
                                                    .flex_1()
                                                    .focus(form.host.focus_handle(cx))
                                                    .disabled(destination_locked)
                                                    .label(
                                                        t!("printers.form.host.label").into_owned(),
                                                    )
                                                    .child(
                                                        Input::new(&form.host)
                                                            .aria_label(
                                                                t!("printers.form.host.label")
                                                                    .into_owned(),
                                                            )
                                                            .id("printer-form-host")
                                                            .disabled(destination_locked),
                                                    ),
                                            )
                                            .child(
                                                Field::new("field-printer-port")
                                                    .flex_1()
                                                    .focus(form.port.focus_handle(cx))
                                                    .disabled(destination_locked)
                                                    .label(
                                                        t!("printers.form.port.label").into_owned(),
                                                    )
                                                    .child(
                                                        Input::new(&form.port)
                                                            .aria_label(
                                                                t!("printers.form.port.label")
                                                                    .into_owned(),
                                                            )
                                                            .id("printer-form-port")
                                                            .disabled(destination_locked),
                                                    ),
                                            ),
                                    )
                                    .when(is_discovered, |panel| {
                                        panel.child(
                                            super::components::field::description(cx).child(
                                                t!("printers.form.destination_locked").into_owned(),
                                            ),
                                        )
                                    })
                                    .child(credentials)
                            })
                            .when_some(self.mutation_error.as_ref(), |panel, error| {
                                panel.child(super::feedback::error_alert(
                                    "printer-form-error",
                                    error,
                                    cx,
                                ))
                            })
                            .when_some(
                                form.duplicate_match.clone(),
                                |panel, (existing_id, existing_name)| {
                                    panel.child(
                                        h_flex()
                                            .gap_2()
                                            .items_center()
                                            .child(
                                                Button::new("open-existing-duplicate-printer")
                                                    .outline()
                                                    .small()
                                                    .label(
                                                        t!(
                                                            "printers.form.edit_existing",
                                                            name = existing_name
                                                        )
                                                        .into_owned(),
                                                    )
                                                    .disabled(self.mutation_pending)
                                                    .on_click(cx.listener(
                                                        move |app, _, window, cx| {
                                                            app.open_edit_printer(
                                                                existing_id,
                                                                window,
                                                                cx,
                                                            );
                                                        },
                                                    )),
                                            )
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(cx.theme().muted_foreground)
                                                    .child(
                                                        t!("printers.form.keep_identity")
                                                            .into_owned(),
                                                    ),
                                            ),
                                    )
                                },
                            ),
                    )
                    .child(
                        dialog::footer(window, cx)
                            .id("printer-form-footer")
                            .test_support()
                            .justify_between()
                            .items_center()
                            .child(h_flex().when_else(
                                searching,
                                |row| {
                                    row.child(
                                        Button::new("discovery-manual")
                                            .ghost()
                                            .label(t!("printers.form.manual").into_owned())
                                            .disabled(self.mutation_pending)
                                            .on_click(cx.listener(|app, _, window, cx| {
                                                app.open_manual_printer(window, cx)
                                            })),
                                    )
                                },
                                |row| {
                                    row.when_some(form.editing, |row, edit_id| {
                                        row.child(
                                            Button::new("delete-printer-form")
                                                .danger()
                                                .outline()
                                                .label(t!("printers.form.delete").into_owned())
                                                .disabled(self.mutation_pending)
                                                .on_click(cx.listener(
                                                    move |app, _, window, cx| {
                                                        app.request_delete_printer(
                                                            edit_id, window, cx,
                                                        );
                                                    },
                                                )),
                                        )
                                    })
                                    .when(
                                        !editing && self.finder.is_none(),
                                        |row| {
                                            row.child(
                                                Button::new("printer-search-network")
                                                    .ghost()
                                                    .label(
                                                        t!("printers.form.search_network")
                                                            .into_owned(),
                                                    )
                                                    .disabled(self.mutation_pending)
                                                    .on_click(cx.listener(|app, _, window, cx| {
                                                        app.open_finder(window, cx)
                                                    })),
                                            )
                                        },
                                    )
                                },
                            ))
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(
                                        Button::new("cancel-printer-form")
                                            .label(t!("common.actions.cancel").into_owned())
                                            .disabled(self.mutation_pending)
                                            .on_click(cx.listener(|app, _, _, cx| {
                                                app.close_printer_form(cx)
                                            })),
                                    )
                                    .when(!searching, |row| {
                                        row.child(
                                            Button::new("save-printer-form")
                                                .primary()
                                                .label(
                                                    if self.mutation_pending
                                                        && form.discovered.is_some()
                                                    {
                                                        t!("printers.form.verifying").into_owned()
                                                    } else if self.mutation_pending {
                                                        t!("common.actions.saving").into_owned()
                                                    } else if editing {
                                                        t!("printers.form.save_changes")
                                                            .into_owned()
                                                    } else {
                                                        t!("printers.form.save_new").into_owned()
                                                    },
                                                )
                                                .disabled(self.mutation_pending)
                                                .on_click(cx.listener(|app, _, _, cx| {
                                                    app.submit_new_printer(cx)
                                                })),
                                        )
                                    }),
                            ),
                    ),
                presence.alpha,
                presence.closing,
            ))
            .focus_trap("printer-form-focus-trap", &self.printer_form_focus)
            .into_any_element(),
        )
    }
}
