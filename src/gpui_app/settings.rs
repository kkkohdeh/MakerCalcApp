use crate::i18n::t;
use std::sync::Arc;

use super::components::button::Button;
use super::components::card;
use super::components::code_block::CodeBlock;
use super::components::field::Field;
use super::components::input::Input;
use super::components::{alert_dialog::backdrop, dialog, scroll_area, separator};
use gpui_kit::base::{FocusTrapElement as _, StyledExt as _};
use gpui_kit::component::label::Label;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, ElementId, Focusable as _, InteractiveElement as _, IntoElement,
    ParentElement as _, ScrollHandle, StatefulInteractiveElement as _, Styled as _, Window, px,
};

use crate::cloud::{CloudStatus, CloudView};
use crate::edge::{EdgeAction, EdgeCommand};
use crate::updater::UpdateStatus;

use super::MakerCalcGpuiApp;
use super::feedback::{ToastKind, log_config_operation};
use super::format::cloud_status_label;
use super::icons::SVG_SETTINGS;

pub(super) mod desktop;
mod language;

pub(crate) fn is_cloud_token_configured(view: &CloudView) -> bool {
    matches!(
        view.status,
        Some(
            CloudStatus::Connecting
                | CloudStatus::WaitingForReady
                | CloudStatus::Connected
                | CloudStatus::NeedsAttention(_)
                | CloudStatus::Unavailable(_)
                | CloudStatus::Incompatible(_)
        )
    )
}

pub(super) fn update_status_caption(status: &UpdateStatus) -> Option<String> {
    match status {
        UpdateStatus::Failed(_) => Some(t!("settings.updates.failed_caption").into_owned()),
        UpdateStatus::UpToDate => Some(t!("settings.updates.current").into_owned()),
        UpdateStatus::Available(_) => Some(t!("settings.updates.install_help").into_owned()),
        _ => None,
    }
}

pub(super) fn log_matches_search(line: &str, lowercase_query: &str) -> bool {
    line.to_lowercase().contains(lowercase_query)
}

impl MakerCalcGpuiApp {
    pub(super) fn render_settings_button(
        id: impl Into<ElementId>,
        cx: &mut Context<Self>,
    ) -> Button {
        Button::new(id)
            .ghost()
            .small()
            .icon(Icon::default().data(SVG_SETTINGS))
            .label(t!("settings.title").into_owned())
            .on_click(cx.listener(|app, _, window, cx| {
                app.settings_opener = window.focused(cx);
                app.open_settings(cx);
                app.sync_cloud_token_placeholder(window, cx);
            }))
    }

    pub(super) fn sync_cloud_token_placeholder(&self, window: &mut Window, cx: &mut Context<Self>) {
        let is_configured = is_cloud_token_configured(&self.cloud_view);
        let placeholder = if is_configured {
            t!("settings.api.configured").into_owned()
        } else {
            t!("settings.api.empty").into_owned()
        };
        self.cloud_token.update(cx, |input, cx| {
            input.clean(window, cx);
            input.set_placeholder(placeholder, window, cx);
        });
        self.cloud_token.read(cx).focus_handle(cx).focus(window, cx);
    }

    pub(super) fn open_settings(&mut self, cx: &mut Context<Self>) {
        for line in crate::diagnostics::drain_display_lines(128) {
            if self.diagnostics.len() >= 200 {
                self.diagnostics.pop_front();
            }
            self.diagnostics.push_back(line);
        }
        self.settings_open = true;
        self.desktop_settings.expanded = false;
        self.refresh_startup_setting(cx);
        cx.notify();
    }

    pub(super) fn close_settings(&mut self, cx: &mut Context<Self>) {
        self.settings_open = false;
        cx.notify();
    }

    pub(super) fn save_cloud_token(&mut self, cx: &mut Context<Self>) {
        if self.mutation_pending {
            return;
        }
        let token = self.cloud_token.read(cx).value().trim().to_owned();

        if token.is_empty() {
            self.mutation_error = Some(t!("settings.api.empty_error").into_owned());
            self.push_toast(
                ToastKind::Warning,
                "MakerCalc",
                Some(t!("settings.api.empty_error").into_owned()),
                cx,
            );
            cx.notify();
            return;
        }

        if token.contains('•') || token.contains('*') {
            self.mutation_error = Some(t!("settings.api.mask_error").into_owned());
            self.push_toast(
                ToastKind::Error,
                t!("settings.api.save_error").into_owned(),
                Some(t!("settings.api.mask_invalid").into_owned()),
                cx,
            );
            cx.notify();
            return;
        }

        let (reply, response) = tokio::sync::oneshot::channel();
        if self
            .edge_actions
            .try_send(EdgeCommand {
                action: EdgeAction::SetEdgeToken(Some(token)),
                reply,
            })
            .is_err()
        {
            self.mutation_error = Some(t!("common.errors.send").into_owned());
            self.push_toast(
                ToastKind::Error,
                t!("settings.api.save_error").into_owned(),
                Some(t!("common.errors.send").into_owned()),
                cx,
            );
            cx.notify();
            return;
        }
        self.mutation_pending = true;
        self.mutation_error = None;
        cx.spawn(async move |app, cx| {
            let result = response
                .await
                .unwrap_or_else(|_| Err(t!("common.errors.controller_closed").into_owned()));
            let _ = app.update(cx, |app, cx| {
                app.mutation_pending = false;
                log_config_operation("set_cloud_token", None, &result, app);
                match result {
                    Ok(()) => {
                        app.push_toast(
                            ToastKind::Success,
                            "MakerCalc",
                            Some(t!("settings.api.saved").into_owned()),
                            cx,
                        );
                    }
                    Err(error) => {
                        app.push_toast(
                            ToastKind::Error,
                            t!("settings.api.save_error").into_owned(),
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

    pub(super) fn disconnect_cloud(&mut self, cx: &mut Context<Self>) {
        if self.mutation_pending {
            return;
        }
        let (reply, response) = tokio::sync::oneshot::channel();
        if self
            .edge_actions
            .try_send(EdgeCommand {
                action: EdgeAction::SetEdgeToken(None),
                reply,
            })
            .is_err()
        {
            self.mutation_error = Some(t!("common.errors.send").into_owned());
            self.push_toast(
                ToastKind::Error,
                t!("settings.api.disconnect_error").into_owned(),
                Some(t!("common.errors.send").into_owned()),
                cx,
            );
            cx.notify();
            return;
        }
        self.mutation_pending = true;
        self.mutation_error = None;
        cx.spawn(async move |app, cx| {
            let result = response
                .await
                .unwrap_or_else(|_| Err(t!("common.errors.controller_closed").into_owned()));
            let _ = app.update(cx, |app, cx| {
                app.mutation_pending = false;
                log_config_operation("disconnect_cloud", None, &result, app);
                match result {
                    Ok(()) => {
                        app.push_toast(
                            ToastKind::Success,
                            "MakerCalc",
                            Some(t!("settings.api.disconnected").into_owned()),
                            cx,
                        );
                    }
                    Err(error) => {
                        app.push_toast(
                            ToastKind::Error,
                            t!("settings.api.disconnect_error").into_owned(),
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

    pub(super) fn render_settings(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let presence = dialog::present(
            "settings-dialog",
            self.settings_open.then_some(()),
            self.settings_opener.clone(),
            window,
            cx,
        )?;
        let focus = window
            .use_keyed_state("settings-dialog-focus", cx, |_, cx| cx.focus_handle())
            .read(cx)
            .clone();
        let scroll = window
            .use_keyed_state("settings-body-scroll", cx, |_, _| ScrollHandle::default())
            .read(cx)
            .clone();
        let token_configured = is_cloud_token_configured(&self.cloud_view);
        let query = self.logs_search.read(cx).value().trim().to_lowercase();
        let mut lines = self
            .diagnostics
            .iter()
            .filter(|line| log_matches_search(line, &query))
            .cloned()
            .collect::<Vec<_>>();
        let matching_lines = lines.len();
        if lines.is_empty() {
            lines.push(if query.is_empty() {
                t!("settings.logs.empty").into_owned()
            } else {
                t!("settings.logs.no_results").into_owned()
            });
        }

        Some(backdrop("settings-modal-backdrop", cx.theme().overlay, presence.alpha)
            .on_key_down(cx.listener(|app, event: &gpui_kit::KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    app.close_settings(cx);
                    app.cloud_token.update(cx, |input, cx| input.clean(window, cx));
                    cx.stop_propagation();
                }
            }))
            .on_click(cx.listener(|app, _, window, cx| {
                app.close_settings(cx);
                app.cloud_token.update(cx, |input, cx| input.clean(window, cx));
                cx.stop_propagation();
            }))
            .child(
                dialog::animated("settings-dialog-zoom", dialog::surface("settings-modal-dialog", t!("settings.title").into_owned(), cx)
                    .max_w(px(720.))
                    .w_full()
                    .h(px(640.))
                    .max_h(window.viewport_size().height - px(48.))
                    .child(
                        dialog::header()
                                    .child(dialog::title().child(t!("settings.title").into_owned()))
                                    .child(
                                        Label::new(t!("settings.subtitle").into_owned())
                                            .text_size(px(14.)).line_height(px(20.))
                                            .text_color(cx.theme().muted_foreground),
                                    ),
                    )
                    .child(
                                Button::new("close-settings-x")
                                    .ghost()
                                    .small()
                                    .size(px(28.)).absolute().top(px(8.)).right(px(8.))
                                    .icon(IconName::Close)
                                    .accessibility_label(t!("common.actions.close").into_owned())
                                    .on_click(cx.listener(|app, _, window, cx| {
                                        app.close_settings(cx);
                                        app.cloud_token.update(cx, |input, cx| {
                                            input.clean(window, cx);
                                        });
                                    })),
                    )
                    .child(
                        v_flex()
                            .id("settings-scroll-area")
                            .flex_1()
                            .min_h(px(0.))
                            // Clip at the panel edge, not through child rings; reserve the bar's gutter.
                            .mx(px(-16.))
                            .mb(px(-16.))
                            .px(px(16.))
                            .pr(px(26.))
                            .py(px(4.))
                            .overflow_y_scroll()
                            .track_scroll(&scroll)
                            .map(|area| scroll_area::vertical(area, &scroll, window, cx))
                            .child(v_flex().flex_none().gap_4()
                            .child(
                                v_flex()
                                    .gap_3()
                                    .child(
                                        h_flex()
                                            .justify_between()
                                            .child(Label::new("MakerCalc").font_semibold())
                                            .child(
                                                Label::new(cloud_status_label(
                                                    self.cloud_view.status.as_ref(),
                                                ))
                                                .text_xs()
                                                .text_color(cx.theme().muted_foreground),
                                            ),
                                    )
                                    .when(matches!(self.cloud_view.status, Some(CloudStatus::NotConfigured)), |section| {
                                        section.child(
                                            v_flex()
                                                .gap_2()
                                                .child(Label::new(t!("settings.api.web_title").into_owned()).font_semibold())
                                                .child(Label::new(t!("settings.api.web_help").into_owned())
                                                    .text_sm()
                                                    .w_full()
                                                    .whitespace_normal()
                                                    .text_color(cx.theme().muted_foreground))
                                                .child(h_flex().child(
                                                    Button::new("open-web-portal")
                                                        .outline()
                                                        .small()
                                                        .label(t!("settings.api.web_open").into_owned())
                                                        .on_click(|_, _, cx| cx.open_url("https://makercalc.app/")),
                                                )),
                                        )
                                    })
                                    .child(
                                        Field::new("field-cloud-token").focus(self.cloud_token.focus_handle(cx))
                                            .label(t!("settings.api.label").into_owned())
                                            .when(!token_configured, |field| {
                                                field.description(t!("settings.api.save_help").into_owned())
                                            })
                                            .child(
                                                h_flex()
                                                    .gap_2()
                                                    .child(
                                                        Input::new(&self.cloud_token)
                                                            .id("cloud-token-input")
                                                            .aria_label(t!("settings.api.label").into_owned())
                                                            .flex_1(),
                                                    )
                                                    .when(!token_configured, |row| row.child(
                                                        Button::new("save-cloud-token")
                                                            .primary()
                                                            .label(if self.mutation_pending {
                                                                t!("common.actions.saving").into_owned()
                                                            } else {
                                                                t!("settings.api.save").into_owned()
                                                            })
                                                            .disabled(self.mutation_pending)
                                                            .on_click(cx.listener(
                                                                |app, _, _, cx| {
                                                                    app.save_cloud_token(cx);
                                                                },
                                                            )),
                                                    ))
                                                    .when(token_configured, |row| {
                                                        row.child(
                                                            Button::new("disconnect-cloud-token")
                                                                .outline()
                                                                .danger()
                                                                .label(t!("settings.api.disconnect").into_owned())
                                                                .disabled(self.mutation_pending)
                                                                .on_click(cx.listener(
                                                                    |app, _, _, cx| {
                                                                        app.disconnect_cloud(cx);
                                                                    },
                                                                )),
                                                        )
                                                    }),
                                            ),
                                    )
                                    .child(
                                        h_flex()
                                            .gap_4()
                                            .child(
                                                Label::new(t!("settings.cloud.pending", count = self.cloud_view.pending_printers).into_owned())
                                                .text_xs()
                                                .text_color(cx.theme().muted_foreground),
                                            )
                                            .child(
                                                Label::new(t!(
                                                    "settings.cloud.last_sequence", value =
                                                    self.cloud_view
                                                        .last_accepted_seq
                                                        .map(|seq| seq.to_string())
                                                        .unwrap_or_else(|| "—".to_owned())
                                                ))
                                                .text_xs()
                                                .text_color(cx.theme().muted_foreground),
                                            ),
                                    )
                                    .when_some(
                                        self.cloud_view.last_rejected.as_ref(),
                                        |panel, reason| {
                                            panel.child(
                                                super::feedback::error_alert("cloud-rejection-alert", &t!("settings.cloud.last_reject", reason = reason), cx),
                                            )
                                        },
                                    )
                                    .when_some(self.mutation_error.as_ref(), |panel, error| {
                                        panel.child(
                                            super::feedback::error_alert("settings-save-error", error, cx),
                                        )
                                    }),
                            )
                            .child(separator::horizontal("settings-cloud-updates-separator", cx))
                            .child({
                                let update_status = self
                                    .updater
                                    .status
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                                    .clone();
                                v_flex()
                                    .gap_3()
                                    .child(
                                        h_flex()
                                            .justify_between()
                                            .items_center()
                                            .child(
                                                v_flex()
                                                    .gap_0p5()
                                                    .child(
                                                        Label::new(t!("settings.updates.title").into_owned())
                                                            .font_semibold(),
                                                    )
                                                    .child(
                                                        Label::new(t!("settings.updates.installed", version = env!("CARGO_PKG_VERSION")).into_owned())
                                                        .text_xs()
                                                        .text_color(cx.theme().muted_foreground),
                                                    ),
                                            )
                                            .child(match (&update_status, crate::desktop::package::is_packaged()) {
                                                (_, true) => {
                                                    Button::new("store-updates-btn")
                                                        .outline()
                                                        .small()
                                                        .label(t!("settings.updates.store_open").into_owned())
                                                        .on_click(|_, _, cx| cx.open_url(crate::desktop::package::STORE_URL))
                                                        .into_any_element()
                                                }
                                                (UpdateStatus::Available(release), false) => {
                                                    let release_url = release.release_url.clone();
                                                    Button::new("download-release-btn")
                                                        .outline()
                                                        .small()
                                                        .label(t!("settings.updates.download", version = &release.version).into_owned())
                                                        .tooltip(t!("settings.updates.release_tooltip").into_owned())
                                                        .on_click(move |_, _, cx| {
                                                            cx.open_url(&release_url);
                                                        })
                                                        .into_any_element()
                                                }
                                                (UpdateStatus::ReadyToRestart(v), false) => {
                                                    Button::new("settings-restart-btn")
                                                        .primary()
                                                        .small()
                                                        .label(t!("settings.updates.restart", version = v).into_owned())
                                                        .on_click(|_, _, cx| {
                                                            crate::updater::restart_app();
                                                            cx.quit();
                                                        })
                                                        .into_any_element()
                                                }
                                                (_, false) => {
                                                    let busy = matches!(
                                                        &update_status,
                                                        UpdateStatus::Checking
                                                            | UpdateStatus::Available(_)
                                                            | UpdateStatus::Downloading
                                                    );
                                                    let updater = Arc::clone(&self.updater);
                                                    Button::new("check-updates-btn")
                                                        .outline()
                                                        .small()
                                                        .label(if busy {
                                                            t!("settings.updates.checking").into_owned()
                                                        } else {
                                                            t!("settings.updates.check").into_owned()
                                                        })
                                                        .disabled(busy)
                                                        .on_click(cx.listener(move |_, _, _, cx| {
                                                            if let Some(check) = updater.start_background_check() {
                                                                let updater = Arc::clone(&updater);
                                                                cx.spawn(async move |app, cx| {
                                                                    if check.await.is_err() {
                                                                        *updater
                                                                            .status
                                                                            .lock()
                                                                            .unwrap_or_else(std::sync::PoisonError::into_inner) =
                                                                            UpdateStatus::Failed(
                                                                                t!("settings.updates.check_failed").into_owned(),
                                                                            );
                                                                    }
                                                                    let _ = app.update(cx, |_, cx| cx.notify());
                                                                }).detach();
                                                            }
                                                            cx.notify();
                                                        }))
                                                        .into_any_element()
                                                }
                                            }),
                                    )
                                    .when_some(
                                        if crate::desktop::package::is_packaged() {
                                            Some(t!("settings.updates.store_help").into_owned())
                                        } else {
                                            update_status_caption(&update_status)
                                        },
                                        |panel, msg| {
                                            panel.child(
                                                Label::new(msg)
                                                    .text_xs()
                                                    .text_color(cx.theme().muted_foreground),
                                            )
                                        },
                                    )
                            })
                            .child(separator::horizontal("settings-updates-language-separator", cx))
                            .child(self.render_language_setting(cx))
                            .child(self.render_desktop_settings(cx))
                            .child(self.render_notification_settings(cx))
                            .child(separator::horizontal("settings-language-desktop-separator", cx))
                            .child(
                                card::surface(false, cx)
                                    .px_4()
                                    .child(
                                        h_flex()
                                            .justify_between()
                                            .child(
                                                v_flex()
                                                    .gap_0p5()
                                                    .child(
                                                        Label::new(t!("settings.logs.title").into_owned())
                                                            .font_semibold(),
                                                    )
                                                    .child(
                                                        Label::new(if query.is_empty() {
                                                            t!("settings.logs.latest", count = self.diagnostics.len()).into_owned()
                                                        } else {
                                                            t!("settings.logs.results", count = matching_lines).into_owned()
                                                        })
                                                        .text_xs()
                                                        .text_color(cx.theme().muted_foreground),
                                                    ),
                                            )
                                            .child(
                                                Button::new("open-log-folder")
                                                    .outline()
                                                    .small()
                                                    .label(t!("settings.logs.open_folder").into_owned())
                                                    .on_click(|_, _, _| {
                                                        let _ =
                                                            crate::diagnostics::open_log_directory(
                                                            );
                                                    }),
                                            ),
                                    )
                                    .child(
                                        Input::new(&self.logs_search)
                                            .id("diagnostics-search")
                                            .aria_label(t!("settings.logs.search").into_owned())
                                            .prefix(Icon::new(IconName::Search))
                                            .cleanable(true)
                                            .w_full(),
                                    )
                                    .child(
                                        CodeBlock::new("diagnostics-list", lines.join("\n"),
                                            t!("settings.logs.title").into_owned()),
                                    ),
                            )),
                    )
                    .child(
                        dialog::footer(window, cx).child(
                            Button::new("close-settings-bottom")
                                .secondary()
                                .label(t!("common.actions.close").into_owned())
                                .on_click(cx.listener(|app, _, window, cx| {
                                    app.close_settings(cx);
                                    app.cloud_token.update(cx, |input, cx| {
                                        input.clean(window, cx);
                                    });
                                })),
                        ),
                    ), presence.alpha, presence.closing),
            )
            .focus_trap("settings-dialog-trap", &focus)
            .into_any_element())
    }
}
