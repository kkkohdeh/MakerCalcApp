//! One startup check and a deferred, dismissible notice. No persisted acknowledgement.
use gpui_kit::base::{FocusTrapElement as _, TestSupportExt as _};
use gpui_kit::component::{ActiveTheme as _, IconName, Sizable as _, label::Label, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, FocusHandle, InteractiveElement as _, IntoElement as _,
    ParentElement as _, ScrollHandle, StatefulInteractiveElement as _, Styled as _, Task, Window,
    px,
};

use crate::i18n::t;
use crate::updater::{RemoteRelease, UpdateStatus};

use super::MakerCalcGpuiApp;
use super::components::{alert_dialog::backdrop, button::Button, dialog, scroll_area};

#[derive(Default)]
pub(super) struct StartupNotice {
    started: bool,
    pub(super) pending: Option<RemoteRelease>,
    open: bool,
    opener: Option<FocusHandle>,
    task: Option<Task<()>>,
    check: Option<tokio::task::AbortHandle>,
}

impl Drop for StartupNotice {
    fn drop(&mut self) {
        if let Some(check) = &self.check {
            check.abort();
        }
    }
}

impl MakerCalcGpuiApp {
    pub(super) fn start_startup_update(&mut self, cx: &mut Context<Self>) {
        if std::mem::replace(&mut self.update_notice.started, true) {
            return;
        }
        // The updater also skips the packaged edition: Microsoft Store owns its updates.
        let Some(check) = self.updater.start_background_check() else {
            return;
        };
        self.watch_startup_update(check, cx);
    }

    fn watch_startup_update(
        &mut self,
        check: tokio::task::JoinHandle<UpdateStatus>,
        cx: &mut Context<Self>,
    ) {
        self.update_notice.check = Some(check.abort_handle());
        self.update_notice.task = Some(cx.spawn(async move |app, cx| {
            let result = check.await;
            let _ = app.update(cx, |app, cx| {
                match result {
                    Ok(UpdateStatus::Available(release)) => {
                        app.update_notice.pending = Some(release);
                    }
                    Ok(_) => {}
                    Err(_) => {
                        let error = t!("settings.updates.check_failed").into_owned();
                        crate::updater::log_check_failure(&error);
                        *app.updater.status.lock().unwrap() = UpdateStatus::Failed(error);
                    }
                }
                app.notify_if_visible(cx);
            });
        }));
    }

    fn dismiss_update_notice(&mut self, cx: &mut Context<Self>) {
        self.update_notice.pending = None;
        cx.notify();
    }

    pub(super) fn render_update_notice(
        &mut self,
        blocked: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if blocked
            || !self.visual_active
            || (!self.update_notice.open && self.update_notice.pending.is_none())
        {
            return None;
        }
        if !self.update_notice.open && self.update_notice.pending.is_some() {
            self.update_notice.opener = window.focused(cx);
            self.update_notice.open = true;
        }
        let Some(presence) = dialog::present(
            "startup-update-dialog",
            self.update_notice.pending.clone(),
            self.update_notice.opener.clone(),
            window,
            cx,
        ) else {
            self.update_notice.open = false;
            self.update_notice.opener = None;
            return None;
        };
        let release = presence.value;
        let title = t!("settings.updates.notice_title", version = release.version).into_owned();
        let focus = window
            .use_keyed_state("update-notice-focus", cx, |_, cx| cx.focus_handle())
            .read(cx)
            .clone();
        let scroll = window
            .use_keyed_state("update-notice-scroll", cx, |_, _| ScrollHandle::default())
            .read(cx)
            .clone();
        if !presence.closing && !focus.contains_focused(window, cx) {
            focus.focus(window, cx);
        }
        // Public release text is inert: no HTML, scripts or links from the body are executed.
        let notes: gpui_kit::SharedString = if release.notes.trim().is_empty() {
            t!("settings.updates.notice_empty").into_owned().into()
        } else {
            release.notes.clone().into()
        };
        Some(
            backdrop("update-notice-backdrop", cx.theme().overlay, presence.alpha)
                .on_key_down(cx.listener(|app, event: &gpui_kit::KeyDownEvent, _, cx| {
                    if event.keystroke.key == "escape" {
                        app.dismiss_update_notice(cx);
                        cx.stop_propagation();
                    }
                }))
                .on_click(cx.listener(|app, _, _, cx| {
                    app.dismiss_update_notice(cx);
                    cx.stop_propagation();
                }))
                .child(dialog::animated(
                    "update-notice-zoom",
                    dialog::surface("update-notice-dialog", title.clone(), cx)
                        .max_w(px(600.))
                        .h(px(560.))
                        .max_h(window.viewport_size().height - px(48.))
                        .child(dialog::header().child(dialog::title().child(title)).child(
                            Label::new(t!("settings.updates.notice_heading").into_owned()),
                        ))
                        .child(
                            Button::new("update-notice-close")
                                .ghost()
                                .small()
                                .size(px(28.))
                                .absolute()
                                .top(px(8.))
                                .right(px(8.))
                                .icon(IconName::Close)
                                .accessibility_label(t!("common.actions.close").into_owned())
                                .on_click(
                                    cx.listener(|app, _, _, cx| app.dismiss_update_notice(cx)),
                                ),
                        )
                        .child(
                            v_flex()
                                .id("update-notice-body")
                                .test_support()
                                .flex_1()
                                .min_h_0()
                                .mx(px(-16.))
                                .mb(px(-16.))
                                .px(px(16.))
                                .pr(px(26.))
                                .py(px(4.))
                                .overflow_y_scroll()
                                .track_scroll(&scroll)
                                .map(|area| scroll_area::vertical(area, &scroll, window, cx))
                                .child(
                                    v_flex()
                                        .id("update-notice-notes")
                                        .test_support()
                                        .flex_none()
                                        .child(Label::new(notes).w_full().whitespace_normal()),
                                ),
                        )
                        .child(
                            dialog::footer(window, cx)
                                .child(
                                    Button::new("update-notice-later")
                                        .secondary()
                                        .label(t!("settings.updates.notice_later").into_owned())
                                        .on_click(cx.listener(|app, _, _, cx| {
                                            app.dismiss_update_notice(cx);
                                        })),
                                )
                                .child(
                                    Button::new("update-notice-open")
                                        .primary()
                                        .label(t!("settings.updates.notice_open").into_owned())
                                        .on_click(cx.listener(move |app, _, _, cx| {
                                            cx.open_url(&release.release_url);
                                            app.dismiss_update_notice(cx);
                                        })),
                                ),
                        ),
                    presence.alpha,
                    presence.closing,
                ))
                .focus_trap("update-notice-trap", &focus)
                .into_any_element(),
        )
    }
}
