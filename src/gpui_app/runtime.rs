use crate::i18n::t;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, Root, TitleBar};
use gpui_kit::{AppContext as _, Context, Task, Window, WindowBounds, px, size};

use crate::diagnostics::{LogArea, LogEvent, LogLevel};
use crate::edge::{EdgeUi, UiPrinterReceiver};
use crate::shared::workspace::{DetailRequest, OperationState, ReadStatus};
use crate::state::{LanConnection, LocalPrinterId};

use super::detail::DetailTab;
use super::feedback::{ToastKind, log_connection_change, log_operation_state, operation_state_eq};
use super::theme::apply_makercalc_theme;
use super::window::initial_window_geometry;
use super::{MakerCalcGpuiApp, PrinterEntry};

impl MakerCalcGpuiApp {
    pub(super) fn new(edge: EdgeUi, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let EdgeUi {
            history,
            desktop,
            discovery,
            actions,
            mut registry,
            mut cloud,
            mut diagnostics,
        } = edge;
        let mut this = Self {
            desktop_settings: super::settings::desktop::DesktopSettings::new(
                desktop,
                cx.focus_handle(),
            ),
            desktop_window: None,
            visual_active: window.is_visible(),
            printers: BTreeMap::new(),
            selected: None,
            selected_file: None,
            detail_tab: DetailTab::Overview,
            settings_open: false,
            printer_form: None,
            edge_actions: actions,
            discovery,
            finder: None,
            finder_task: None,
            finder_generation: 0,
            printer_form_focus: cx.focus_handle(),
            printer_form_opener: None,
            settings_opener: None,
            printer_delete_focus: cx.focus_handle(),
            mutation_pending: false,
            mutation_error: None,
            pending_print: None,
            pending_print_control: None,
            material_controls: Default::default(),
            machine_controls: Default::default(),
            print_bed_leveling: true,
            print_material_station: false,
            print_first_layer_inspection: false,
            print_flow_calibration: false,
            print_timelapse: false,
            print_safety_confirmed: false,
            print_countdown: None,
            print_countdown_task: None,
            pending_delete: None,
            pending_upload: None,
            file_picker_open: false,
            diagnostics: crate::diagnostics::drain_display_lines(200).into(),
            logs_search: cx
                .new(|cx| InputState::new(window, cx).placeholder(t!("settings.logs.search"))),
            cloud_view: cloud.borrow().clone(),
            cloud_token: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(t!("settings.api.empty"))
                    .masked(true)
            }),
            files_search: cx
                .new(|cx| InputState::new(window, cx).placeholder(t!("files.search.placeholder"))),
            files_list: cx.new(|cx| super::files::list::new_table(window, cx)),
            history_view: super::history::HistoryView::new(window, cx),
            registry_task: Task::ready(()),
            diagnostics_task: Task::ready(()),
            cloud_task: Task::ready(()),
            printer_tasks: Vec::new(),
            detail_tasks: Vec::new(),
            operation_tasks: Vec::new(),
            camera_tasks: Vec::new(),
            timer_task: Task::ready(()),
            toasts: Vec::new(),
            toast_overflowed: false,
            dismissed_alerts: BTreeSet::new(),
            updater: Arc::new(crate::updater::AutoUpdater::new()),
            update_notice: Default::default(),
        };
        cx.subscribe(&this.files_search, |_, _, _: &InputEvent, cx| {
            cx.notify();
        })
        .detach();
        this.bind_files_list(window, cx);
        this.bind_history_view(window, cx);
        this.history_view.sidebar.store = Some(history);
        cx.subscribe(&this.logs_search, |_, _, _: &InputEvent, cx| {
            cx.notify();
        })
        .detach();
        this.replace_registry(registry.borrow().printers.clone(), cx);

        #[cfg(feature = "ui-diagnostics")]
        let mut performance = super::performance::Probe::new(window, cx);
        this.timer_task = cx.spawn(async move |app, cx| {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
            loop {
                interval.tick().await;
                let res = app.update(cx, |app, cx| {
                    #[cfg(feature = "ui-diagnostics")]
                    performance.poll(app, cx);
                    let mut updated = false;
                    for line in crate::diagnostics::drain_display_lines(128) {
                        if app.diagnostics.len() >= 200 {
                            app.diagnostics.pop_front();
                        }
                        app.diagnostics.push_back(line);
                        updated = true;
                    }
                    if !app.visual_active {
                        return;
                    }
                    for printer in app.printers.values_mut() {
                        if (!printer.state.telemetry_ready
                            || printer.state.connection != LanConnection::Connected)
                            && printer.state.last_valid_at.is_some()
                            && app.selected.is_none_or(|id| {
                                id == printer.state.id && app.detail_tab == DetailTab::Overview
                            })
                        {
                            updated = true;
                        }
                        if printer.state.telemetry_ready
                            || printer.state.connection == LanConnection::Connected
                        {
                            updated |= printer.record_temperature_sample();
                        }
                    }
                    if app.detail_tab == DetailTab::History
                        && let Some(id) = app.selected
                        && let Some(printer) = app.printers.get(&id)
                        && !printer.history_loading
                        && printer.history_version != Some(*printer.history.changed.borrow())
                    {
                        let offset = printer
                            .local_history
                            .as_ref()
                            .and_then(|page| page.as_ref().ok())
                            .map_or(0, |page| page.offset);
                        app.load_history(id, offset, cx);
                    }
                    app.refresh_global_history(cx);
                    app.sync_printer_alerts(cx);
                    if updated {
                        app.notify_if_visible(cx);
                    }
                });
                if res.is_err() {
                    break;
                }
            }
        });

        this.registry_task = cx.spawn(async move |app, cx| {
            loop {
                if registry.changed().await.is_err() {
                    break;
                }
                let printers = registry.borrow_and_update().printers.clone();
                if app
                    .update(cx, |app, cx| app.replace_registry(printers, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        this.diagnostics_task = cx.spawn(async move |app, cx| {
            while let Some(line) = diagnostics.recv().await {
                if line.starts_with("No se pudo") {
                    crate::diagnostics::log(
                        LogEvent::new(LogLevel::Error, LogArea::Integration, "edge.message")
                            .result("failed")
                            .detail("Integration error; details available in UI"),
                    );
                }
                if app
                    .update(cx, |app, cx| {
                        if app.diagnostics.len() == 200 {
                            app.diagnostics.pop_front();
                        }
                        app.diagnostics.push_back(line);
                        app.notify_if_visible(cx);
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        this.cloud_task = cx.spawn(async move |app, cx| {
            loop {
                if cloud.changed().await.is_err() {
                    break;
                }
                let view = cloud.borrow_and_update().clone();
                if app
                    .update(cx, |app, cx| {
                        app.cloud_view = view;
                        app.notify_if_visible(cx);
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        this.start_startup_update(cx);
        this
    }

    pub(super) fn replace_registry(
        &mut self,
        printers: Vec<UiPrinterReceiver>,
        cx: &mut Context<Self>,
    ) {
        for printer in self.printers.values() {
            let _ = printer.detail.active.send(false);
        }
        self.printer_tasks.clear();
        self.detail_tasks.clear();
        self.operation_tasks.clear();
        self.camera_tasks.clear();
        self.printers = printers
            .iter()
            .map(|printer| {
                let old = self.printers.get(&printer.id);
                let old_history = old.map(|p| p.temperature_history.clone());
                let old_sample = old.map(|p| p.last_temp_sample);
                let old_thumb_key = old.and_then(|p| p.active_thumbnail_key.clone());
                let old_thumb = old.and_then(|p| p.thumbnail.clone());
                let old_thumb_color = old.and_then(|p| p.thumbnail_color);
                let state = printer.receiver.borrow().clone();
                let material_pending = old.and_then(|p| p.material_pending).filter(|target| {
                    target.session == printer.session_generation
                        && target.epoch == state.connection_epoch
                });
                let temperature_history = old_history.unwrap_or_default();
                let last_temp_sample = old_sample.unwrap_or_else(std::time::Instant::now);
                (
                    printer.id,
                    PrinterEntry {
                        history: printer.history.clone(),
                        history_id: printer.history_id.clone(),
                        local_history: None,
                        history_version: None,
                        history_query: 0,
                        history_loading: false,
                        name: printer.name.clone(),
                        protocol: printer.connection_view.protocol,
                        host: printer.connection_view.host.clone(),
                        port: printer.connection_view.port,
                        session_generation: printer.session_generation,
                        connection_view: printer.connection_view.clone(),
                        metadata: printer.metadata.clone(),
                        thumbnail_handle: printer.thumbnail.clone(),
                        active_thumbnail_key: old_thumb_key,
                        state,
                        detail: printer.detail.clone(),
                        detail_state: printer.detail.state.borrow().clone(),
                        actions: printer.operation.actions.clone(),
                        operation_state: printer.operation.state.borrow().clone(),
                        operation: printer.operation.clone(),
                        material_pending,
                        camera: printer.camera.clone(),
                        camera_state: printer.camera.state.borrow().clone(),
                        thumbnail: old_thumb,
                        thumbnail_color: old_thumb_color,
                        thumbnail_image: Default::default(),
                        camera_image: Default::default(),
                        file_image: Default::default(),
                        thumbnail_fetching: false,
                        thumbnail_retry_at: None,
                        temperature_history,
                        last_temp_sample,
                    },
                )
            })
            .collect();

        self.sync_material_controls();
        self.sync_machine_controls();
        for printer in printers {
            let id = printer.id;
            let mut receiver = printer.receiver;
            self.printer_tasks.push(cx.spawn(async move |app, cx| {
                loop {
                    if receiver.changed().await.is_err() {
                        break;
                    }
                    let state = receiver.borrow_and_update().clone();
                    if app
                        .update(cx, |app, cx| {
                            let mut epoch_changed = false;
                            if let Some(printer) = app.printers.get_mut(&id) {
                                let previous = printer.state.connection;
                                epoch_changed =
                                    printer.state.connection_epoch != state.connection_epoch;
                                printer.state = state;
                                if previous != printer.state.connection {
                                    log_connection_change(printer, previous);
                                }
                                if app.visual_active {
                                    printer.update_latest_temperature_sample();
                                }
                            }
                            if epoch_changed {
                                app.sync_machine_controls();
                            }
                            app.request_thumbnail(id, cx);
                            app.notify_if_visible(cx);
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            }));

            let mut detail_receiver = printer.detail.state;
            self.detail_tasks.push(cx.spawn(async move |app, cx| {
                loop {
                    if detail_receiver.changed().await.is_err() {
                        break;
                    }
                    let detail = detail_receiver.borrow_and_update().clone();
                    if app
                        .update(cx, |app, cx| {
                            if let Some(printer) = app.printers.get_mut(&id) {
                                let should_fetch_history = app.visual_active
                                    && detail.live
                                    && app.selected == Some(id)
                                    && app.detail_tab == DetailTab::Overview
                                    && detail.history_totals.status == ReadStatus::NotLoaded
                                    && (!printer.detail_state.live
                                        || printer.detail_state.generation != detail.generation);
                                let next_read = (app.selected == Some(id))
                                    .then(|| {
                                        super::overview::readings::next_read(
                                            &printer.detail_state,
                                            &detail,
                                        )
                                    })
                                    .flatten();
                                printer.detail_state = detail;
                                if should_fetch_history {
                                    let _ = printer
                                        .detail
                                        .requests
                                        .try_send(DetailRequest::HistoryTotals);
                                }
                                let read_error = next_read.and_then(|request| {
                                    printer.detail.requests.try_send(request).err()
                                });
                                if let Some(error) = read_error {
                                    let message = match error {
                                        tokio::sync::mpsc::error::TrySendError::Full(_) => {
                                            t!("printers.errors.queue_full")
                                        }
                                        tokio::sync::mpsc::error::TrySendError::Closed(_) => {
                                            t!("printers.errors.channel_closed")
                                        }
                                    }
                                    .into_owned();
                                    app.push_toast(
                                        ToastKind::Error,
                                        t!("common.read_value.error").into_owned(),
                                        Some(message),
                                        cx,
                                    );
                                }
                                app.notify_if_visible(cx);
                            }
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            }));

            let mut operation_receiver = printer.operation.state;
            self.operation_tasks.push(cx.spawn(async move |app, cx| {
                loop {
                    if operation_receiver.changed().await.is_err() {
                        break;
                    }
                    let state = operation_receiver.borrow_and_update().clone();
                    if app
                        .update(cx, |app, cx| {
                            let mut toast_to_push = None;
                            if let Some(printer) = app.printers.get_mut(&id) {
                                let changed = !operation_state_eq(&printer.operation_state, &state);
                                printer.operation_state = state;
                                if changed {
                                    log_operation_state(printer);
                                    match &printer.operation_state {
                                        OperationState::Rejected { action, message } => {
                                            toast_to_push = Some((
                                                ToastKind::Error,
                                                t!(
                                                    "printers.actions.failed",
                                                    action = super::format::action_label(*action)
                                                )
                                                .into_owned(),
                                                Some(message.clone()),
                                            ));
                                        }
                                        OperationState::Unknown(action) => {
                                            toast_to_push = Some((
                                                ToastKind::Warning,
                                                t!(
                                                    "printers.actions.uncertain",
                                                    action = super::format::action_label(*action)
                                                )
                                                .into_owned(),
                                                Some(
                                                    t!("printers.actions.verify_before_retry")
                                                        .into_owned(),
                                                ),
                                            ));
                                        }
                                        _ => {}
                                    }
                                }
                                app.notify_if_visible(cx);
                            }
                            if let Some((kind, title, desc)) = toast_to_push {
                                app.push_toast(kind, title, desc, cx);
                            }
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            }));

            let mut camera_receiver = printer.camera.state;
            self.camera_tasks.push(cx.spawn(async move |app, cx| {
                loop {
                    if camera_receiver.changed().await.is_err() {
                        break;
                    }
                    let state = camera_receiver.borrow_and_update().clone();
                    if app
                        .update(cx, |app, cx| {
                            if let Some(printer) = app.printers.get_mut(&id) {
                                printer.camera_state = state;
                                app.notify_if_visible(cx);
                            }
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            }));
        }
        let printer_ids = self.printers.keys().copied().collect::<Vec<_>>();
        for id in printer_ids {
            self.request_thumbnail(id, cx);
        }
        self.selected = self.selected.filter(|id| self.printers.contains_key(id));
        if let Some(id) = self.selected
            && let Some(printer) = self.printers.get(&id)
        {
            let _ = printer.detail.active.send(true);
        }
        self.notify_if_visible(cx);
    }

    pub(super) fn request_thumbnail(&mut self, id: LocalPrinterId, cx: &mut Context<Self>) {
        if !self.visual_active {
            return;
        }
        let Some(printer) = self.printers.get_mut(&id) else {
            return;
        };
        let Some(filename) = printer
            .state
            .telemetry
            .current_job
            .as_ref()
            .and_then(|job| job.print_name.clone())
            .filter(|name| !name.trim().is_empty())
        else {
            printer.active_thumbnail_key = None;
            printer.thumbnail = None;
            printer.thumbnail_color = None;
            printer.thumbnail_fetching = false;
            printer.thumbnail_retry_at = None;
            return;
        };
        let key = crate::shared::thumbnail::ThumbnailResourceKey {
            printer_id: id,
            session_generation: printer.session_generation,
            connection_epoch: printer.state.connection_epoch,
            job_generation: printer.state.job_generation,
            print_name: filename,
        };
        if printer.active_thumbnail_key.as_ref() == Some(&key) {
            if printer.thumbnail.is_some() || printer.thumbnail_fetching {
                return;
            }
        } else {
            printer.active_thumbnail_key = Some(key.clone());
            printer.thumbnail = None;
            printer.thumbnail_color = None;
            printer.thumbnail_fetching = false;
            printer.thumbnail_retry_at = None;
        }
        if let Some(retry_at) = printer.thumbnail_retry_at
            && std::time::Instant::now() < retry_at
        {
            return;
        }
        printer.thumbnail_fetching = true;
        let handle = printer.thumbnail_handle.clone();
        let target_key = key;
        let background = cx.theme().group_box.to_rgb();
        let background =
            [background.r, background.g, background.b].map(|value| (value * 255.).round() as u8);
        cx.spawn(async move |app, cx| {
            let result = handle.request_active(target_key.clone()).await;
            let accent = if let Ok(normalized) = &result {
                let png = normalized.png_bytes.clone();
                crate::shared::thumbnail::run_blocking_thumbnail_cpu(move || {
                    crate::shared::thumbnail::color::from_png(&png, background)
                })
                .await
                .unwrap_or(crate::shared::thumbnail::color::FALLBACK)
            } else {
                crate::shared::thumbnail::color::FALLBACK
            };
            let _ = app.update(cx, |app, cx| {
                if let Some(printer) = app.printers.get_mut(&id)
                    && printer.active_thumbnail_key.as_ref() == Some(&target_key)
                {
                    printer.thumbnail_fetching = false;
                    match result {
                        Ok(normalized) => {
                            printer.thumbnail = Some(normalized.png_bytes.clone());
                            printer.thumbnail_color = Some(accent);
                            printer.thumbnail_retry_at = None;
                        }
                        Err(_) => {
                            printer.thumbnail = None;
                            printer.thumbnail_color = None;
                            printer.thumbnail_retry_at =
                                Some(std::time::Instant::now() + std::time::Duration::from_secs(3));
                        }
                    }
                    app.notify_if_visible(cx);
                }
            });
        })
        .detach();
    }
}

pub(crate) fn run(
    edge: EdgeUi,
    startup: bool,
    notification_printer: Option<LocalPrinterId>,
    mut finish: impl FnMut() + 'static,
) {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            // GPUI starts its 200 ms future timeout AFTER invoking this synchronous part.
            cx.on_app_quit(move |_| {
                finish();
                std::future::ready(())
            })
            .detach();
            gpui_kit::init(cx);
            if let Err(error) = super::theme::load_card_font(cx) {
                crate::report_startup_error(&format!("Geist: {error}"));
                cx.quit();
                return;
            }
            apply_makercalc_theme(cx);
            let mut options = TitleBar::window_options();
            let initially_hidden = startup && edge.desktop.start_in_background;
            options.show = !initially_hidden;
            options.focus = !initially_hidden;
            if let Some(display) = cx.primary_display() {
                let (bounds, minimum) = initial_window_geometry(display.visible_bounds());
                options.window_bounds = Some(WindowBounds::Windowed(bounds));
                options.window_min_size = Some(minimum);
            } else {
                options.window_bounds = Some(WindowBounds::centered(size(px(1200.), px(720.)), cx));
                options.window_min_size = Some(size(px(640.), px(480.)));
            }
            cx.spawn(async move |cx| {
                cx.open_window(options, |window, cx| {
                    let view = cx.new(|cx| {
                        let mut app = MakerCalcGpuiApp::new(edge, window, cx);
                        app.attach_desktop(startup, window, cx);
                        if let Some(id) =
                            notification_printer.filter(|id| app.printers.contains_key(id))
                        {
                            app.open_notification_printer(id, cx);
                        }
                        app
                    });
                    cx.new(|cx| Root::new(view, window, cx))
                })
                .expect("failed to open GPUI window");
            })
            .detach();
        });
}
