use crate::i18n::t;
use std::sync::Arc;

use gpui_kit::base::{Button as BaseButton, StyledExt as _, TestSupportExt as _};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, input::InputState, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{AppContext as _, ParentElement as _, Styled as _, *};

use crate::discovery::{
    Candidate, DiscoveryQuery, DiscoveryView, NetworkInterface, Search, SearchEnd, Verification,
};

use super::MakerCalcGpuiApp;
use super::components::accordion::Accordion;
use super::components::button::Button;
use super::components::field::Field;
use super::components::input::Input;
use super::components::item::{self, Item};
use super::components::radio_group::{Radio, RadioGroup};

pub(crate) struct PrinterFinder {
    pub(crate) interfaces: Vec<NetworkInterface>,
    pub(crate) selected: Option<usize>,
    pub(crate) range_start: Entity<InputState>,
    pub(crate) range_end: Entity<InputState>,
    pub(crate) query: Option<DiscoveryQuery>,
    pub(crate) view: Arc<DiscoveryView>,
    pub(crate) loading: bool,
    pub(crate) confirming: bool,
    pub(crate) choosing_network: bool,
    pub(crate) ip_expanded: bool,
    pub(crate) range_scope: Option<String>,
}

impl PrinterFinder {
    /// A short-lived visual exit must not retain a discovery cancellation owner.
    pub(super) fn view_for_exit(&self) -> Self {
        Self {
            interfaces: self.interfaces.clone(),
            selected: self.selected,
            range_start: self.range_start.clone(),
            range_end: self.range_end.clone(),
            query: None,
            view: self.view.clone(),
            loading: self.loading,
            confirming: self.confirming,
            choosing_network: self.choosing_network,
            ip_expanded: self.ip_expanded,
            range_scope: self.range_scope.clone(),
        }
    }
    pub(super) fn update_results(&mut self, view: Arc<DiscoveryView>) {
        self.view = view;
        if !self.view.running {
            self.query = None;
            if self.range_scope.is_none()
                && self.view.end == Some(SearchEnd::Finished)
                && self.view.candidates.is_empty()
            {
                self.ip_expanded = true;
            }
        }
    }
}

pub(super) fn same_candidate_endpoint(a: &Candidate, b: &Candidate) -> bool {
    a.address == b.address && a.host == b.host && a.protocol == b.protocol && a.tls == b.tls
}

pub(super) fn finder_caption(view: &DiscoveryView) -> String {
    if view.running {
        t!("printers.finder.searching").into_owned()
    } else {
        match view.end {
            None => t!("printers.finder.ready").into_owned(),
            Some(SearchEnd::Cancelled) => t!("printers.finder.stopped").into_owned(),
            Some(SearchEnd::Finished) if !view.errors.is_empty() || view.checked < view.total => {
                t!("printers.finder.warnings").into_owned()
            }
            Some(SearchEnd::Finished) => t!("printers.finder.finished").into_owned(),
        }
    }
}

impl MakerCalcGpuiApp {
    pub(super) fn clear_finder(&mut self) {
        self.finder_generation += 1;
        self.finder = None;
        self.finder_task = None;
    }

    pub(super) fn open_finder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.mutation_pending {
            return;
        }
        self.clear_finder();
        self.finder = Some(PrinterFinder {
            interfaces: Vec::new(),
            selected: None,
            range_start: cx.new(|cx| InputState::new(window, cx).placeholder("192.168.1.1")),
            range_end: cx.new(|cx| InputState::new(window, cx).placeholder("192.168.1.254")),
            query: None,
            view: Arc::new(DiscoveryView::default()),
            loading: true,
            confirming: false,
            choosing_network: true,
            ip_expanded: false,
            range_scope: None,
        });
        let discovery = self.discovery.clone();
        let generation = self.finder_generation;
        self.finder_task = Some(cx.spawn(async move |app, cx| {
            let interfaces = discovery.interfaces().await;
            let _ = app.update(cx, |app, cx| {
                if app.finder_generation != generation {
                    return;
                }
                if let Some(finder) = app.finder.as_mut() {
                    finder.loading = false;
                    match interfaces {
                        Ok(interfaces) => finder.interfaces = interfaces,
                        Err(error) => Arc::make_mut(&mut finder.view).errors.push(error),
                    }
                    cx.notify();
                }
            });
        }));
        cx.notify();
    }

    pub(super) fn select_interface(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(finder) = self.finder.as_mut()
            && !finder.view.running
            && !self.mutation_pending
            && let Some(interface) = finder.interfaces.get(index)
        {
            finder.choosing_network = false;
            if finder.selected != Some(index) {
                finder.query = None;
                finder.selected = Some(index);
                let range = interface.default_range();
                let (initial, final_ip) = range.split_once('-').unwrap_or(("", ""));
                finder.range_start.update(cx, |input, cx| {
                    input.set_value(initial.trim().to_owned(), window, cx)
                });
                finder.range_end.update(cx, |input, cx| {
                    input.set_value(final_ip.trim().to_owned(), window, cx)
                });
                finder.view = Arc::new(DiscoveryView::default());
                finder.range_scope = None;
                self.finder_task = None;
                self.finder_generation += 1;
            }
            self.printer_form_focus.focus(window, cx);
            cx.notify();
        }
    }

    pub(super) fn start_finder(&mut self, search: Search, cx: &mut Context<Self>) {
        if self.mutation_pending || !self.searching_printer() {
            return;
        }
        let Some(finder) = self.finder.as_mut() else {
            return;
        };
        if finder.view.running {
            return;
        }
        let Some(interface) = finder
            .selected
            .and_then(|i| finder.interfaces.get(i))
            .cloned()
        else {
            return;
        };
        self.finder_generation += 1;
        let generation = self.finder_generation;
        match &search {
            Search::Range(range) => finder.range_scope = Some(range.clone()),
            Search::Announcements => finder.range_scope = None,
            Search::Retry => {}
        }
        let query = self
            .discovery
            .start(interface, search, (*finder.view).clone());
        finder.view = query.view.borrow().clone();
        let mut view = query.view.clone();
        finder.query = Some(query);
        self.finder_task = Some(cx.spawn(async move |app, cx| {
            loop {
                if view.changed().await.is_err() {
                    break;
                }
                let value = view.borrow_and_update().clone();
                let running = value.running;
                if app
                    .update(cx, |app, cx| {
                        if app.finder_generation == generation
                            && let Some(finder) = app.finder.as_mut()
                        {
                            finder.update_results(value);
                            cx.notify();
                        }
                    })
                    .is_err()
                    || !running
                {
                    break;
                }
            }
        }));
        cx.notify();
    }

    pub(super) fn pause_finder(&mut self) {
        if let Some(finder) = self.finder.as_mut() {
            finder.query = None;
            Arc::make_mut(&mut finder.view).stop();
            // Interface enumeration is read-only and may finish while the form is visible.
            if finder.loading {
                return;
            }
        }
        self.finder_generation += 1;
        self.finder_task = None;
    }

    pub(super) fn show_confirmation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.pause_finder();
        if let Some(finder) = self.finder.as_mut() {
            finder.confirming = true;
        }
        if let Some(form) = self.printer_form.as_ref() {
            form.name.read(cx).focus_handle(cx).focus(window, cx);
        }
        cx.notify();
    }

    pub(super) fn searching_printer(&self) -> bool {
        self.finder
            .as_ref()
            .is_some_and(|finder| !finder.confirming)
    }

    pub(super) fn open_manual_printer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.mutation_pending {
            return;
        }
        if self
            .printer_form
            .as_ref()
            .is_some_and(|form| form.editing.is_some())
        {
            self.printer_form = Some(Self::empty_printer_form(window, cx));
        }
        if let Some(form) = self.printer_form.as_mut() {
            form.discovered = None;
        }
        self.mutation_error = None;
        self.show_confirmation(window, cx);
    }

    pub(super) fn select_candidate(
        &mut self,
        candidate: Candidate,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.mutation_pending || candidate.verification == Verification::Unsupported {
            return;
        }
        let same = self
            .printer_form
            .as_ref()
            .and_then(|form| form.discovered.as_ref())
            .is_some_and(|old| same_candidate_endpoint(old, &candidate));
        if same {
            if let Some(form) = self.printer_form.as_mut() {
                form.protocol = candidate.protocol;
                form.tls = candidate.tls;
                form.host.update(cx, |input, cx| {
                    input.set_value(candidate.host.clone(), window, cx)
                });
                form.port.update(cx, |input, cx| {
                    input.set_value(candidate.address.port().to_string(), window, cx)
                });
                form.discovered = Some(candidate);
                form.duplicate_match = None;
            }
            self.mutation_error = None;
            self.show_confirmation(window, cx);
            return;
        }
        self.pause_finder();
        if let Some((&id, _)) = self
            .printers
            .iter()
            .find(|(_, p)| candidate.matches(&p.host, p.port, p.protocol, p.connection_view.tls))
        {
            let finder = self.finder.take();
            self.open_edit_printer(id, window, cx);
            self.finder = finder;
            if let Some(form) = self.printer_form.as_mut() {
                form.discovered = Some(candidate);
                form.duplicate_match = None;
            }
            self.show_confirmation(window, cx);
            return;
        }
        if let Some(form) = self.printer_form.as_mut() {
            form.editing = None;
            form.protocol = candidate.protocol;
            form.tls = candidate.tls;
            form.name.update(cx, |input, cx| {
                input.set_value(candidate.name.clone(), window, cx)
            });
            form.host.update(cx, |input, cx| {
                input.set_value(candidate.host.clone(), window, cx)
            });
            form.port.update(cx, |input, cx| {
                input.set_value(candidate.address.port().to_string(), window, cx)
            });
            form.serial_number.update(cx, |input, cx| {
                input.set_value(candidate.serial.clone().unwrap_or_default(), window, cx)
            });
            // Never carry a different device's credentials into the selection.
            form.api_key
                .update(cx, |input, cx| input.set_value("", window, cx));
            form.check_code
                .update(cx, |input, cx| input.set_value("", window, cx));
            form.build_area_light
                .update(cx, |input, cx| input.set_value("", window, cx));
            form.toolhead_light
                .update(cx, |input, cx| input.set_value("", window, cx));
            let requires_cred = candidate.verification == Verification::RequiresCredential;
            form.trusted_lan = !requires_cred;
            form.credential_status = crate::shared::printer::CredentialStatus::None;
            form.credential_rejected = requires_cred;
            form.duplicate_match = None;
            form.discovered = Some(candidate);
            self.mutation_error = None;
        }
        self.show_confirmation(window, cx);
    }

    pub(super) fn render_finder(
        &self,
        finder: Option<&PrinterFinder>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(finder) = finder else {
            return div().into_any_element();
        };
        let running = finder.view.running;
        let locked = running || self.mutation_pending;
        let selectable = finder
            .view
            .candidates
            .iter()
            .filter(|candidate| candidate.verification != Verification::Unsupported)
            .count();
        let selected = finder
            .selected
            .and_then(|index| finder.interfaces.get(index));
        let ip_body = v_flex()
            .gap_3()
            .pt_3()
            .child(
                super::components::field::description(cx).child(t!("printers.finder.range_help")),
            )
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .gap_2()
                    .items_start()
                    .child(
                        Field::new("field-finder-ip-start")
                            .focus(finder.range_start.focus_handle(cx))
                            .disabled(locked)
                            .label(t!("printers.finder.initial_ip").into_owned())
                            .flex_1()
                            .min_w_0()
                            .child(
                                Input::new(&finder.range_start)
                                    .id("finder-ip-initial")
                                    .aria_label(t!("printers.finder.initial_ip").into_owned())
                                    .disabled(locked),
                            ),
                    )
                    .child(
                        Field::new("field-finder-ip-end")
                            .focus(finder.range_end.focus_handle(cx))
                            .disabled(locked)
                            .label(t!("printers.finder.final_ip").into_owned())
                            .flex_1()
                            .min_w_0()
                            .child(
                                Input::new(&finder.range_end)
                                    .id("finder-ip-final")
                                    .aria_label(t!("printers.finder.final_ip").into_owned())
                                    .disabled(locked),
                            ),
                    ),
            )
            .child(
                Button::new("discovery-range")
                    .label(t!("printers.finder.search_range").into_owned())
                    .disabled(locked || selected.is_none())
                    .on_click(cx.listener(|app, _, _, cx| {
                        if let Some(finder) = app.finder.as_ref() {
                            let range = format!(
                                "{} - {}",
                                finder.range_start.read(cx).value().trim(),
                                finder.range_end.read(cx).value().trim()
                            );
                            app.start_finder(Search::Range(range), cx);
                        }
                    })),
            );
        v_flex()
            .w_full()
            .min_w_0()
            .gap_4()
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .whitespace_normal()
                    .id("printer-finder-help")
                    .test_support()
                    .child(t!("printers.finder.help")),
            )
            .when(finder.loading, |panel| {
                panel.child(
                    div()
                        .text_sm()
                        .child(t!("printers.finder.loading_networks")),
                )
            })
            .when(!finder.loading && finder.interfaces.is_empty(), |panel| {
                panel.child(super::components::empty::Empty::new(
                    "finder-no-networks",
                    t!("printers.finder.no_networks").into_owned(),
                ))
            })
            .when(
                finder.choosing_network && !finder.interfaces.is_empty(),
                |panel| {
                    panel.child(
                        Field::new("field-finder-network")
                            .disabled(locked)
                            .label(t!("printers.finder.network").into_owned())
                            .child(
                                RadioGroup::new("finder-network")
                                    .selected_index(finder.selected)
                                    .disabled(locked)
                                    .on_change(cx.listener(|app, index, window, cx| {
                                        app.select_interface(*index, window, cx);
                                    }))
                                    .children(finder.interfaces.iter().enumerate().map(
                                        |(index, interface)| {
                                            let addresses = interface
                                                .addresses
                                                .iter()
                                                .filter(|(ip, _)| ip.is_ipv4())
                                                .chain(
                                                    interface
                                                        .addresses
                                                        .iter()
                                                        .filter(|(ip, _)| ip.is_ipv6()),
                                                )
                                                .map(|(ip, prefix)| format!("{ip}/{prefix}"))
                                                .collect::<Vec<_>>()
                                                .join(", ");
                                            Radio::new(index)
                                                .label(interface.name.clone())
                                                .accessibility_label(format!(
                                                    "{}: {addresses}",
                                                    interface.name
                                                ))
                                                .tooltip(addresses.clone())
                                                .card(
                                                    div()
                                                        .w_full()
                                                        .text_xs()
                                                        .text_color(cx.theme().muted_foreground)
                                                        .id(("finder-addresses", index))
                                                        .test_support()
                                                        .truncate()
                                                        .child(addresses),
                                                )
                                        },
                                    )),
                            ),
                    )
                },
            )
            .when(!finder.choosing_network, |panel| {
                panel.when_some(selected, |panel, interface| {
                    let address = interface
                        .addresses
                        .iter()
                        .find(|(ip, _)| ip.is_ipv4())
                        .or_else(|| interface.addresses.first())
                        .map_or_else(
                            || t!("printers.finder.no_lan").into_owned(),
                            |(ip, prefix)| format!("{ip}/{prefix}"),
                        );
                    panel.child(
                        h_flex()
                            .w_full()
                            .min_w_0()
                            .gap_3()
                            .items_center()
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .gap_1()
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_semibold()
                                            .truncate()
                                            .child(interface.name.clone()),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .truncate()
                                            .child(address),
                                    ),
                            )
                            .child(
                                Button::new("finder-change-network")
                                    .ghost()
                                    .small()
                                    .flex_shrink_0()
                                    .label(t!("printers.finder.change_network").into_owned())
                                    .disabled(locked)
                                    .on_click(cx.listener(|app, _, window, cx| {
                                        if let Some(finder) = app.finder.as_mut() {
                                            finder.choosing_network = true;
                                        }
                                        app.printer_form_focus.focus(window, cx);
                                        cx.notify();
                                    })),
                            ),
                    )
                })
            })
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("discovery-native")
                            .when(selectable == 0, |button| button.primary())
                            .label(t!("printers.finder.search").into_owned())
                            .disabled(locked || selected.is_none())
                            .on_click(cx.listener(|app, _, _, cx| {
                                app.start_finder(Search::Announcements, cx)
                            })),
                    )
                    .when(running, |row| {
                        row.child(
                            Button::new("discovery-cancel")
                                .label(t!("printers.finder.stop").into_owned())
                                .on_click(cx.listener(|app, _, _, cx| {
                                    app.pause_finder();
                                    cx.notify();
                                })),
                        )
                    }),
            )
            .when(finder.view.running || finder.view.end.is_some(), |panel| {
                panel
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(finder_caption(&finder.view)),
                    )
                    .when_some(finder.range_scope.as_ref(), |panel, range| {
                        panel.child(
                            div()
                                .text_xs()
                                .whitespace_normal()
                                .text_color(cx.theme().muted_foreground)
                                .child(t!("printers.finder.range", range = range).into_owned()),
                        )
                    })
                    .when(running && finder.view.total > 0, |panel| {
                        panel.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(
                                    t!(
                                        "printers.finder.checks",
                                        completed = finder.view.checked,
                                        total = finder.view.total
                                    )
                                    .into_owned(),
                                ),
                        )
                    })
                    .when(!running && finder.view.candidates.is_empty(), |panel| {
                        panel.child(super::components::empty::Empty::new(
                            "finder-empty-results",
                            if finder.range_scope.is_none() {
                                t!("printers.finder.empty_announcements")
                            } else {
                                t!("printers.finder.empty")
                            }
                            .into_owned(),
                        ))
                    })
            })
            .children(finder.view.errors.iter().enumerate().map(|(index, error)| {
                super::feedback::error_alert(("discovery-error", index), error, cx)
            }))
            .when(!finder.view.candidates.is_empty(), |panel| {
                panel.child(
                    v_flex()
                        .w_full()
                        .min_w_0()
                        .gap_2()
                        .child(
                            div().text_sm().font_semibold().child(
                                t!(
                                    "printers.finder.results",
                                    count = finder.view.candidates.len()
                                )
                                .into_owned(),
                            ),
                        )
                        .child(
                            v_flex()
                                .id("discovery-results")
                                .role(Role::List)
                                .gap_4()
                                .children(finder.view.candidates.iter().map(|candidate| {
                                    let existing_printer = self.printers.values().find(|p| {
                                        candidate.matches(
                                            &p.host,
                                            p.port,
                                            p.protocol,
                                            p.connection_view.tls,
                                        )
                                    });
                                    let existing_label =
                                        existing_printer.map_or(String::new(), |p| {
                                            t!("printers.finder.already_added", name = &p.name)
                                                .into_owned()
                                        });
                                    let chosen = candidate.clone();
                                    let disabled = self.mutation_pending
                                        || candidate.verification == Verification::Unsupported;
                                    let protocol = candidate.protocol.to_string();
                                    let kind = if candidate.name == protocol {
                                        String::new()
                                    } else {
                                        format!("{protocol} · ")
                                    };
                                    let id = format!(
                                        "discovery-select-{}-{}-{}",
                                        candidate.address, candidate.protocol, candidate.tls
                                    );
                                    let trigger = BaseButton::new(id.clone())
                                        .accessibility_label(if let Some(p) = existing_printer {
                                            t!(
                                                "printers.finder.edit_existing",
                                                name = &p.name,
                                                candidate = &candidate.name,
                                                address = candidate.address
                                            )
                                            .into_owned()
                                        } else {
                                            t!(
                                                "printers.finder.select",
                                                name = &candidate.name,
                                                address = candidate.address
                                            )
                                            .into_owned()
                                        })
                                        .on_click(cx.listener(move |app, _, window, cx| {
                                            app.select_candidate(chosen.clone(), window, cx)
                                        }));
                                    Item::new(
                                        id,
                                        trigger,
                                        item::content()
                                            .child(
                                                item::title()
                                                    .w_full()
                                                    .min_w_0()
                                                    .child(candidate.name.clone()),
                                            )
                                            .child(
                                                item::description(cx)
                                                    .w_full()
                                                    .child(candidate.address.to_string()),
                                            )
                                            .child(item::description(cx).child(format!(
                                                "{}{}{}{}",
                                                existing_label,
                                                kind,
                                                candidate.caption(),
                                                if candidate.tls { " · HTTPS" } else { "" }
                                            )))
                                            .when_some(candidate.issue, |row, issue| {
                                                row.child(
                                                    item::description(cx)
                                                        .text_color(cx.theme().red)
                                                        .child(issue),
                                                )
                                            }),
                                    )
                                    .outline()
                                    .disabled(disabled)
                                    .tooltip(format!("{} · {}", candidate.name, candidate.address))
                                })),
                        ),
                )
            })
            .when(finder.view.can_retry(), |panel| {
                panel.child(
                    Button::new("discovery-retry")
                        .outline()
                        .small()
                        .label(t!("printers.finder.retry").into_owned())
                        .disabled(self.mutation_pending)
                        .on_click(cx.listener(|app, _, _, cx| app.start_finder(Search::Retry, cx))),
                )
            })
            .child(Accordion::new(
                "finder-ip-accordion",
                t!("printers.finder.by_ip").into_owned(),
                finder.ip_expanded,
                BaseButton::new("finder-ip-trigger").on_click(cx.listener(|app, _, _, cx| {
                    if let Some(finder) = app.finder.as_mut() {
                        finder.ip_expanded = !finder.ip_expanded;
                    }
                    cx.notify();
                })),
                ip_body,
            ))
            .into_any_element()
    }
}
