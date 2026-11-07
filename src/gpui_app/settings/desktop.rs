//! Preferences and native desktop intentions; no configuration file or credential access.
use super::super::components::button::Button;
use super::super::components::card;
use super::super::components::switch::Switch;
use crate::i18n::t;
use gpui_kit::base::{Button as BaseButton, StyledExt as _};
use gpui_kit::component::label::Label;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, v_flex};
use gpui_kit::{
    AnyElement, Context, FocusHandle, IntoElement as _, ParentElement as _, Styled as _, Window,
};

use super::super::MakerCalcGpuiApp;
use super::super::components::accordion::Accordion;
use super::super::detail::DetailTab;
use super::super::feedback::ToastKind;
use crate::desktop::{DesktopEvent, DesktopWindow, StartupView};
use crate::edge::{EdgeAction, EdgeCommand};
use crate::shared::workspace::DesktopPreferences;

pub(crate) struct DesktopSettings {
    pub(crate) preferences: DesktopPreferences,
    pub(crate) startup: Option<Result<StartupView, String>>,
    pub(crate) busy: bool,
    notification_busy: bool,
    pub(super) expanded: bool,
    section_focus: FocusHandle,
}

impl DesktopSettings {
    pub(crate) fn new(preferences: DesktopPreferences, section_focus: FocusHandle) -> Self {
        Self {
            preferences,
            startup: None,
            busy: false,
            notification_busy: false,
            expanded: false,
            section_focus,
        }
    }
}

impl MakerCalcGpuiApp {
    pub(in crate::gpui_app) fn attach_desktop(
        &mut self,
        startup: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let weak = cx.entity().downgrade();
        let executor = cx.foreground_executor().clone();
        let async_cx = cx.to_async();
        let handle = window.window_handle();
        let native = DesktopWindow::attach(window, move |event| {
            let weak = weak.clone();
            let mut async_cx = async_cx.clone();
            // Queue onto the existing executor. Native callbacks must not borrow GPUI.
            executor
                .spawn(async move {
                    let _ = handle.update(&mut async_cx, |_, window, cx| {
                        let _ = weak.update(cx, |app, cx| match event {
                            DesktopEvent::Quit => cx.quit(),
                            DesktopEvent::Settings => {
                                if !app.visual_active {
                                    app.resume_visuals(cx);
                                }
                                app.open_settings(cx);
                                app.sync_cloud_token_placeholder(window, cx);
                            }
                            DesktopEvent::Open => {
                                if !app.visual_active {
                                    app.resume_visuals(cx);
                                }
                            }
                            DesktopEvent::OpenPrinter(id) => {
                                if !app.visual_active {
                                    app.resume_visuals(cx);
                                }
                                app.open_notification_printer(id, cx);
                            }
                            DesktopEvent::TrayUnavailable => app.push_toast(
                                ToastKind::Error,
                                t!("settings.desktop.tray_error").into_owned(),
                                Some(t!("settings.desktop.tray_error_help").into_owned()),
                                cx,
                            ),
                        });
                    });
                })
                .detach();
        });
        match native {
            Ok(native) => {
                if let Err(error) = native.set_preferences(self.desktop_settings.preferences) {
                    self.desktop_error(error, cx);
                }
                if !crate::desktop::should_start_hidden(
                    startup,
                    self.desktop_settings.preferences,
                    native.tray_ready(),
                ) && !window.is_visible()
                    && let Err(error) = native.show()
                {
                    self.desktop_error(error, cx);
                }
                let close_policy = native.clone();
                window.on_window_should_close(cx, move |_, _| close_policy.should_close());
                self.desktop_window = Some(native);
            }
            Err(error) => {
                if let Err(show_error) = crate::desktop::show_without_tray(window) {
                    self.desktop_error(show_error, cx);
                }
                self.desktop_error(error, cx);
            }
        }
        let weak = cx.entity().downgrade();
        window
            .observe_window_visibility(move |visibility, _, cx| {
                let _ = weak.update(cx, |app, cx| {
                    let was_visible =
                        std::mem::replace(&mut app.visual_active, visibility.is_visible());
                    if app.visual_active && !was_visible {
                        app.resume_visuals(cx);
                    } else if !app.visual_active && was_visible {
                        app.files_list
                            .update(cx, |table, _| table.delegate_mut().suspend_reads());
                        cx.notify();
                    } // One redraw removes animation nodes; no periodic repaint.
                });
            })
            .detach();
    }

    pub(in crate::gpui_app) fn notify_if_visible(&self, cx: &mut Context<Self>) {
        if self.visual_active {
            cx.notify();
        }
    }

    fn resume_visuals(&mut self, cx: &mut Context<Self>) {
        self.visual_active = true;
        if self.detail_tab == DetailTab::Files && self.selected.is_some() {
            self.files_list
                .update(cx, |table, _| table.delegate_mut().resume_reads());
        }
        for printer in self.printers.values_mut() {
            printer.update_latest_temperature_sample();
        }
        for id in self.printers.keys().copied().collect::<Vec<_>>() {
            self.request_thumbnail(id, cx);
        }
        if self.detail_tab == DetailTab::History
            && let Some(id) = self.selected
            && self.printers.get(&id).is_some_and(|p| !p.history_loading)
        {
            let offset = self
                .printers
                .get(&id)
                .and_then(|p| p.local_history.as_ref())
                .and_then(|p| p.as_ref().ok())
                .map_or(0, |p| p.offset);
            self.load_history(id, offset, cx);
        }
        self.sync_printer_alerts(cx);
        cx.notify();
    }

    fn desktop_error(&mut self, error: String, cx: &mut Context<Self>) {
        self.push_toast(
            ToastKind::Error,
            t!("settings.desktop.title").into_owned(),
            Some(error),
            cx,
        );
    }

    pub(super) fn refresh_startup_setting(&mut self, cx: &mut Context<Self>) {
        // Mock windows and failed attachments have no native integration.
        // Keep Windows registration untouched in headless UI tests.
        if self.desktop_window.is_none() {
            return;
        }
        if self.desktop_settings.busy {
            return;
        }
        self.desktop_settings.startup = None;
        self.desktop_settings.busy = true;
        let operation = tokio::task::spawn_blocking(crate::desktop::read_startup);
        cx.spawn(async move |app, cx| {
            let result = operation
                .await
                .unwrap_or_else(|_| Err(t!("settings.desktop.read_error").into_owned()));
            let _ = app.update(cx, |app, cx| {
                app.desktop_settings.startup = Some(result);
                app.desktop_settings.busy = false;
                app.notify_if_visible(cx);
            });
        })
        .detach();
    }

    fn set_startup_setting(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if self.desktop_settings.busy {
            return;
        }
        self.desktop_settings.busy = true;
        let operation = tokio::task::spawn_blocking(move || crate::desktop::set_startup(enabled));
        cx.spawn(async move |app, cx| {
            let result = operation
                .await
                .unwrap_or_else(|_| Err(t!("settings.desktop.write_error").into_owned()));
            let _ = app.update(cx, |app, cx| {
                app.desktop_settings.busy = false;
                match result {
                    Ok(view) => app.desktop_settings.startup = Some(Ok(view)),
                    Err(error) => {
                        app.desktop_error(error.clone(), cx);
                        app.desktop_settings.startup = Some(Err(error));
                    }
                }
                app.notify_if_visible(cx);
            });
        })
        .detach();
    }

    pub(super) fn save_desktop_preferences(
        &mut self,
        preferences: DesktopPreferences,
        cx: &mut Context<Self>,
    ) {
        if self.mutation_pending {
            return;
        }
        let (reply, response) = tokio::sync::oneshot::channel();
        if self
            .edge_actions
            .try_send(EdgeCommand {
                action: EdgeAction::SetDesktopPreferences(preferences),
                reply,
            })
            .is_err()
        {
            self.desktop_error(t!("common.errors.send").into_owned(), cx);
            return;
        }
        self.mutation_pending = true;
        cx.spawn(async move |app, cx| {
            let result = response
                .await
                .unwrap_or_else(|_| Err(t!("common.errors.controller_closed").into_owned()));
            let _ = app.update(cx, |app, cx| {
                app.mutation_pending = false;
                match result {
                    Ok(()) => {
                        app.desktop_settings.preferences = preferences;
                        if let Some(native) = &app.desktop_window
                            && let Err(error) = native.set_preferences(preferences)
                        {
                            app.desktop_error(error, cx);
                        }
                    }
                    Err(error) => app.desktop_error(error, cx),
                }
                app.notify_if_visible(cx);
            });
        })
        .detach();
    }

    pub(super) fn render_desktop_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        let preferences = self.desktop_settings.preferences;
        let startup = self
            .desktop_settings
            .startup
            .as_ref()
            .and_then(|v| v.as_ref().ok());
        let requested = startup.is_some_and(|v| v.requested);
        let startup_disabled = self.desktop_settings.busy
            || startup.is_none()
            || startup.is_some_and(|v| {
                !v.can_enable && (!v.requested || crate::desktop::package::is_packaged())
            });
        let background_disabled = !requested || self.mutation_pending || self.desktop_settings.busy;
        let content = v_flex()
            .gap_3()
            .child(
                Switch::new("desktop-startup")
                    .label(t!("settings.desktop.start_with_windows").into_owned())
                    .checked(requested)
                    .disabled(startup_disabled)
                    .on_change(
                        cx.listener(|app, enabled, _, cx| app.set_startup_setting(*enabled, cx)),
                    ),
            )
            .child(
                Label::new(if self.desktop_settings.busy {
                    t!("settings.desktop.busy").into_owned()
                } else if startup.is_none() {
                    t!("settings.desktop.unknown").into_owned()
                } else if requested {
                    t!("settings.desktop.requested").into_owned()
                } else {
                    t!("settings.desktop.not_requested").into_owned()
                })
                .text_xs()
                .text_color(cx.theme().muted_foreground),
            )
            .child(
                Switch::new("desktop-background")
                    .label(t!("settings.desktop.start_background").into_owned())
                    .checked(preferences.start_in_background)
                    .disabled(background_disabled)
                    .on_change(cx.listener(move |app, enabled, _, cx| {
                        app.save_desktop_preferences(
                            DesktopPreferences {
                                start_in_background: *enabled,
                                ..preferences
                            },
                            cx,
                        )
                    })),
            )
            .child(
                Label::new(t!("settings.desktop.background_help").into_owned())
                    .text_xs()
                    .text_color(cx.theme().muted_foreground),
            )
            .child(
                Switch::new("desktop-close")
                    .label(t!("settings.desktop.minimize_on_close").into_owned())
                    .checked(preferences.minimize_on_close)
                    .disabled(self.mutation_pending)
                    .on_change(cx.listener(move |app, enabled, _, cx| {
                        app.save_desktop_preferences(
                            DesktopPreferences {
                                minimize_on_close: *enabled,
                                ..preferences
                            },
                            cx,
                        )
                    })),
            )
            .child(
                Label::new(t!("settings.desktop.close_help").into_owned())
                    .text_xs()
                    .text_color(cx.theme().muted_foreground),
            )
            .child(
                Button::new("desktop-windows-settings")
                    .outline()
                    .small()
                    .label(t!("settings.desktop.open_windows_settings").into_owned())
                    .on_click(cx.listener(|app, _, _, cx| {
                        if let Err(error) = crate::desktop::open_windows_startup_settings() {
                            app.desktop_error(error, cx);
                        }
                    })),
            )
            .children(startup.filter(|v| !v.can_enable).map(|_| {
                Label::new(if crate::desktop::package::is_packaged() {
                    t!("settings.desktop.store_managed").into_owned()
                } else {
                    t!("settings.desktop.installed_only").into_owned()
                })
                .text_xs()
                .text_color(cx.theme().muted_foreground)
            }))
            .children(
                self.desktop_settings
                    .startup
                    .as_ref()
                    .and_then(|v| v.as_ref().err())
                    .map(|error| {
                        super::super::feedback::error_alert("desktop-startup-error", error, cx)
                    }),
            );
        let expanded = self.desktop_settings.expanded;
        card::surface(false, cx)
            .px_4()
            .child(
                Accordion::new(
                    "desktop-settings-accordion",
                    t!("settings.desktop.title").into_owned(),
                    expanded,
                    BaseButton::new("desktop-settings-trigger").on_click(cx.listener(
                        |app, _, _, cx| {
                            app.desktop_settings.expanded = !app.desktop_settings.expanded;
                            cx.notify();
                        },
                    )),
                    content,
                )
                .track_focus(&self.desktop_settings.section_focus),
            )
            .into_any_element()
    }

    pub(in crate::gpui_app) fn open_notification_printer(
        &mut self,
        id: crate::state::LocalPrinterId,
        cx: &mut Context<Self>,
    ) {
        // A click never discards unsaved forms or accepts/cancels a hardware confirmation.
        if self.settings_open
            || self.printer_form.is_some()
            || self.mutation_pending
            || self.pending_print.is_some()
            || self.pending_print_control.is_some()
            || self.pending_delete.is_some()
            || self.pending_upload.is_some()
            || self.file_picker_open
        {
            return;
        }
        if self.printers.contains_key(&id) {
            self.select_printer(id, cx);
        }
    }

    fn test_windows_notification(&mut self, cx: &mut Context<Self>) {
        if self.desktop_settings.notification_busy || self.mutation_pending {
            return;
        }
        let (reply, response) = tokio::sync::oneshot::channel();
        if self
            .edge_actions
            .try_send(EdgeCommand {
                action: EdgeAction::TestNotification,
                reply,
            })
            .is_err()
        {
            self.desktop_error(t!("common.errors.send").into_owned(), cx);
            return;
        }
        self.desktop_settings.notification_busy = true;
        cx.spawn(async move |app, cx| {
            let result = response
                .await
                .unwrap_or_else(|_| Err(t!("common.errors.controller_closed").into_owned()));
            let _ = app.update(cx, |app, cx| {
                app.desktop_settings.notification_busy = false;
                match result {
                    Ok(()) => app.push_toast(
                        ToastKind::Info,
                        t!("settings.notifications.title").into_owned(),
                        Some(t!("settings.notifications.requested").into_owned()),
                        cx,
                    ),
                    Err(error) => app.desktop_error(error, cx),
                }
                app.notify_if_visible(cx);
            });
        })
        .detach();
    }

    pub(super) fn render_notification_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        let preferences = self.desktop_settings.preferences;
        let notification_disabled =
            self.mutation_pending || self.desktop_settings.notification_busy;
        card::surface(false, cx)
            .px_4()
            .child(
                v_flex()
                    .gap_2()
                    .child(
                        Label::new(t!("settings.notifications.title").into_owned()).font_semibold(),
                    )
                    .child(
                        Switch::new("windows-notifications-enabled")
                            .label(t!("settings.notifications.enable").into_owned())
                            .checked(preferences.windows_notifications)
                            .disabled(notification_disabled)
                            .on_change(cx.listener(move |app, enabled, _, cx| {
                                app.save_desktop_preferences(
                                    DesktopPreferences {
                                        windows_notifications: *enabled,
                                        ..preferences
                                    },
                                    cx,
                                )
                            })),
                    )
                    .child(
                        Label::new(t!("settings.notifications.help").into_owned())
                            .text_xs()
                            .text_color(cx.theme().muted_foreground),
                    )
                    .child(
                        Button::new("windows-notifications-test")
                            .outline()
                            .small()
                            .label(t!("settings.notifications.test").into_owned())
                            .disabled(
                                !preferences.windows_notifications
                                    || self.mutation_pending
                                    || self.desktop_settings.notification_busy,
                            )
                            .on_click(
                                cx.listener(|app, _, _, cx| app.test_windows_notification(cx)),
                            ),
                    ),
            )
            .into_any_element()
    }
}
