use std::collections::BTreeMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::runtime::Handle;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::timeout;

use crate::cloud::{
    CloudConfig, CloudPrinterReceiver, CloudRegistry, CloudView, Device, run_cloud,
};
use crate::config::{
    ConfigError, ConfigStore, PersistedConfig, PrinterConfig, PrinterDraft, Protocol,
};
use crate::integrations::moonraker::DriverConfig;
use crate::shared::workspace::{
    CameraHandle, DetailHandle, OperationHandle, make_camera_channels, make_detail_channels,
    make_operation_channels,
};
use crate::state::{LanConnection, LocalPrinterId, PrinterState, sanitize_message};

const ACTION_CAPACITY: usize = 32;
const DIAGNOSTIC_CAPACITY: usize = 128;
const SHUTDOWN_BUDGET: Duration = Duration::from_secs(4);
const HISTORY_FLUSH_BUDGET: Duration = Duration::from_secs(4);
const DESKTOP_SHUTDOWN_BUDGET: Duration = Duration::from_secs(12);
mod notifications;
#[derive(Clone)]
pub(crate) struct UiPrinterReceiver {
    pub(crate) history: crate::history::HistoryHandle,
    pub(crate) history_id: String,
    pub(crate) id: LocalPrinterId,
    pub(crate) name: String,
    pub(crate) session_generation: u64,
    pub(crate) connection_view: crate::shared::printer::EditableConnectionView,
    pub(crate) metadata: crate::shared::printer::PrinterLocalMetadata,
    pub(crate) thumbnail: crate::shared::thumbnail::ThumbnailHandle,
    pub(crate) receiver: watch::Receiver<PrinterState>,
    pub(crate) detail: DetailHandle,
    pub(crate) operation: OperationHandle,
    pub(crate) camera: CameraHandle,
}

#[derive(Clone, Default)]
pub(crate) struct UiRegistry {
    pub(crate) revision: u64,
    pub(crate) printers: Vec<UiPrinterReceiver>,
}

pub(crate) enum EdgeAction {
    AddPrinter(PrinterDraft),
    UpdatePrinter {
        id: LocalPrinterId,
        draft: PrinterDraft,
    },
    #[cfg_attr(not(test), allow(dead_code))]
    UpdatePrinterMetadata {
        id: LocalPrinterId,
        name: String,
    },
    RemovePrinter(LocalPrinterId),
    SetEdgeToken(Option<String>),
    SetDesktopPreferences(crate::shared::workspace::DesktopPreferences),
    TestNotification,
}

impl std::fmt::Debug for EdgeAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AddPrinter(draft) => f
                .debug_struct("AddPrinter")
                .field("name", &draft.name)
                .field("host", &draft.host)
                .field("port", &draft.port)
                .field("tls", &draft.tls)
                .field("trusted_lan", &draft.trusted_lan)
                .field("api_key", &"[REDACTED]")
                .finish(),
            Self::UpdatePrinter { id, draft } => f
                .debug_struct("UpdatePrinter")
                .field("id", id)
                .field("name", &draft.name)
                .field("host", &draft.host)
                .field("port", &draft.port)
                .field("tls", &draft.tls)
                .field("trusted_lan", &draft.trusted_lan)
                .field("api_key", &"[REDACTED]")
                .finish(),
            Self::UpdatePrinterMetadata { id, name } => f
                .debug_struct("UpdatePrinterMetadata")
                .field("id", id)
                .field("name", name)
                .finish(),
            Self::RemovePrinter(id) => f.debug_tuple("RemovePrinter").field(id).finish(),
            Self::SetEdgeToken(_) => f.debug_tuple("SetEdgeToken").field(&"[REDACTED]").finish(),
            Self::SetDesktopPreferences(prefs) => {
                f.debug_tuple("SetDesktopPreferences").field(prefs).finish()
            }
            Self::TestNotification => f.write_str("TestNotification"),
        }
    }
}

pub(crate) struct EdgeCommand {
    pub(crate) action: EdgeAction,
    pub(crate) reply: tokio::sync::oneshot::Sender<Result<(), String>>,
}

impl std::fmt::Debug for EdgeCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EdgeCommand")
            .field("action", &self.action)
            .finish()
    }
}

pub(crate) struct EdgeUi {
    pub(crate) history: crate::history::HistoryHandle,
    pub(crate) desktop: crate::shared::workspace::DesktopPreferences,
    pub(crate) discovery: crate::discovery::DiscoveryHandle,
    pub(crate) actions: mpsc::Sender<EdgeCommand>,
    pub(crate) registry: watch::Receiver<UiRegistry>,
    pub(crate) cloud: watch::Receiver<CloudView>,
    pub(crate) diagnostics: mpsc::Receiver<String>,
}

pub(crate) struct EdgeController {
    shutdown: watch::Sender<bool>,
    actor: JoinHandle<()>,
}

struct DriverTask {
    generation: u64,
    shutdown: watch::Sender<bool>,
    join: JoinHandle<()>,
    receiver: watch::Receiver<PrinterState>,
    detail: DetailHandle,
    operation: OperationHandle,
    camera: CameraHandle,
    thumbnail: crate::shared::thumbnail::ThumbnailHandle,
}

struct EdgeActor {
    notice_sender: mpsc::Sender<crate::shared::notifications::PrinterNotice>,
    notices_enabled: watch::Sender<bool>,
    notice_captures: BTreeMap<LocalPrinterId, crate::shared::notifications::NoticeCapture>,
    notifications_join: JoinHandle<()>,
    history: crate::history::HistoryWorker,
    runtime: Handle,
    store: Arc<ConfigStore>,
    config: PersistedConfig,
    actions: mpsc::Receiver<EdgeCommand>,
    diagnostics: mpsc::Sender<String>,
    ui_registry_sender: watch::Sender<UiRegistry>,
    cloud_registry_sender: watch::Sender<CloudRegistry>,
    cloud_config_sender: watch::Sender<CloudConfig>,
    drivers: BTreeMap<LocalPrinterId, DriverTask>,
    next_generation: u64,
    shutdown: watch::Receiver<bool>,
    cloud_join: JoinHandle<()>,
}

impl EdgeController {
    pub(crate) fn start_with_store(
        runtime: &Handle,
        store: Arc<ConfigStore>,
    ) -> Result<(Self, EdgeUi), ConfigError> {
        let config = store.load_or_empty()?;
        let desktop = config.desktop;
        let history = crate::history::HistoryWorker::open(store.history_path())
            .unwrap_or_else(crate::history::HistoryWorker::unavailable);
        let ui_history = history.handle();
        let (actions_sender, actions) = mpsc::channel(ACTION_CAPACITY);
        let (diagnostic_sender, diagnostics) = mpsc::channel(DIAGNOSTIC_CAPACITY);
        let (ui_registry_sender, ui_registry) = watch::channel(UiRegistry::default());
        let (cloud_registry_sender, cloud_registry) = watch::channel(CloudRegistry::default());
        let edge_token = match config.edge_token() {
            Ok(token) => token,
            Err(error) => {
                let _ = diagnostic_sender.try_send(sanitize_message(
                    &format!("No se pudo descifrar la credencial cloud: {error}"),
                    512,
                ));
                None
            }
        };
        let (cloud_config_sender, cloud_config) = watch::channel(CloudConfig {
            edge_token,
            installation_id: config.installation_id.clone(),
        });
        let (cloud_view_sender, cloud) = watch::channel(CloudView::default());
        let (shutdown_sender, shutdown) = watch::channel(false);
        let (notice_sender, notices) = mpsc::channel(crate::shared::notifications::QUEUE_CAPACITY);
        let (notices_enabled, notice_preferences) = watch::channel(desktop.windows_notifications);
        let notifications_join = runtime.spawn(notifications::run(
            notices,
            ui_registry.clone(),
            notice_preferences,
            shutdown.clone(),
        ));
        let discovery = crate::discovery::DiscoveryHandle::new(runtime, shutdown.clone());
        let cloud_join = runtime.spawn(run_cloud(
            cloud_registry,
            cloud_config,
            cloud_view_sender,
            shutdown.clone(),
        ));
        let mut actor = EdgeActor {
            notice_sender,
            notices_enabled,
            notice_captures: BTreeMap::new(),
            notifications_join,
            history,
            runtime: runtime.clone(),
            store,
            config,
            actions,
            diagnostics: diagnostic_sender,
            ui_registry_sender,
            cloud_registry_sender,
            cloud_config_sender,
            drivers: BTreeMap::new(),
            next_generation: 1,
            shutdown,
            cloud_join,
        };
        actor.start_saved_printers();
        actor.refresh_registries();
        let controller = Self {
            shutdown: shutdown_sender,
            actor: runtime.spawn(actor.run()),
        };
        let ui = EdgeUi {
            history: ui_history,
            desktop,
            discovery,
            actions: actions_sender,
            registry: ui_registry,
            cloud,
            diagnostics,
        };
        Ok((controller, ui))
    }

    pub(crate) async fn shutdown_async(self) {
        let _ = self.shutdown.send(true);
        let mut actor = self.actor;
        match timeout(DESKTOP_SHUTDOWN_BUDGET, &mut actor).await {
            Ok(Ok(())) => {}
            result => {
                crate::diagnostics::log(
                    crate::diagnostics::LogEvent::new(
                        crate::diagnostics::LogLevel::Error,
                        crate::diagnostics::LogArea::App,
                        "app.shutdown_failed",
                    )
                    .result(if result.is_err() { "timeout" } else { "failed" }),
                );
                actor.abort();
                if result.is_err() {
                    let _ = actor.await;
                }
            }
        }
    }

    pub(crate) fn shutdown(self, runtime: &Handle) {
        runtime.block_on(self.shutdown_async());
    }
}

/// Verification is an explicit, credential-bearing action, not part of automatic discovery.
pub(crate) async fn verify_discovered_printer(
    candidate: &crate::discovery::Candidate,
    draft: &PrinterDraft,
) -> Result<(), String> {
    if candidate.verification == crate::discovery::Verification::Unsupported
        || !candidate.matches(
            &draft.host,
            draft.port,
            draft.protocol.unwrap_or_default(),
            draft.tls,
        )
    {
        return Err("La dirección cambió o no es compatible; vuelve a seleccionarla o usa el formulario manual".into());
    }
    let client = crate::discovery::verification_client(candidate)?;
    let request = match candidate.protocol {
        Protocol::Moonraker => crate::integrations::moonraker::adapter::verify_request(
            &client,
            candidate,
            draft.trusted_lan,
            draft.api_key.as_deref(),
        )?,
        Protocol::Flashforge => crate::integrations::flashforge::verify_request(
            &client,
            candidate,
            draft.api_key.as_deref(),
        )?,
    };
    let response = request
        .send()
        .await
        .map_err(|_| "No se pudo verificar: revisa dirección, certificado y credenciales")?;
    if !response.status().is_success() {
        return Err("El servicio rechazó la verificación; no se guardó la impresora".into());
    }
    let body = crate::discovery::bounded_body(response).await?;
    let valid = match candidate.protocol {
        Protocol::Moonraker => crate::integrations::moonraker::adapter::verify_response(&body),
        Protocol::Flashforge => crate::integrations::flashforge::verify_response(&body),
    };
    if valid {
        Ok(())
    } else {
        Err("La respuesta no identifica una impresora compatible; no se guardó".into())
    }
}

impl EdgeActor {
    async fn run(mut self) {
        loop {
            tokio::select! {
                changed = self.shutdown.changed() => {
                    if changed.is_err() || *self.shutdown.borrow() {
                        self.stop_all().await;
                        return;
                    }
                }
                command = self.actions.recv() => {
                    let Some(command) = command else {
                        self.stop_all().await;
                        return;
                    };
                    self.handle_command(command).await;
                }
            }
        }
    }

    fn start_saved_printers(&mut self) {
        let printers = self.config.printers.clone();
        for printer in printers {
            self.start_driver(printer);
        }
    }

    async fn handle_command(&mut self, command: EdgeCommand) {
        let EdgeCommand { action, reply } = command;
        let result = match action {
            EdgeAction::SetDesktopPreferences(desktop) => {
                match self
                    .persist_candidate(move |mut candidate| {
                        candidate.desktop = desktop;
                        Ok(candidate)
                    })
                    .await
                {
                    Ok(config) => {
                        self.notices_enabled
                            .send_replace(desktop.windows_notifications);
                        self.config = config;
                        Ok(())
                    }
                    Err(_) => {
                        Err("No se pudieron guardar las opciones de inicio y segundo plano".into())
                    }
                }
            }
            EdgeAction::TestNotification => {
                if !self.config.desktop.windows_notifications {
                    Err(crate::i18n::t!("settings.notifications.enable_first").into_owned())
                } else {
                    tokio::task::spawn_blocking(|| {
                        crate::desktop::notifications::show(
                            None,
                            crate::i18n::t!("settings.notifications.test_body").into_owned(),
                        )
                    })
                    .await
                    .unwrap_or_else(|_| {
                        Err(crate::i18n::t!("settings.notifications.worker_failed").into_owned())
                    })
                }
            }
            EdgeAction::AddPrinter(draft) => {
                let result = self
                    .persist_candidate(move |mut candidate| {
                        candidate.add_printer(draft)?;
                        Ok(candidate)
                    })
                    .await;
                match result {
                    Ok(config) => {
                        let printer = config.printers.last().cloned();
                        self.config = config;
                        if let Some(printer) = printer {
                            self.start_driver(printer);
                            self.refresh_registries();
                        }
                        self.emit("Impresora guardada");
                        Ok(())
                    }
                    Err(error) => {
                        let msg = sanitize_message(
                            &format!("No se pudo guardar la impresora: {error}"),
                            512,
                        );
                        self.emit(&msg);
                        Err(msg)
                    }
                }
            }
            EdgeAction::UpdatePrinter { id, draft } => {
                let result = self
                    .persist_candidate(move |mut candidate| {
                        candidate.update_printer(id, draft)?;
                        Ok(candidate)
                    })
                    .await;
                match result {
                    Ok(config) => {
                        self.config = config;
                        self.stop_driver(id).await;
                        if let Some(capture) = self.notice_captures.get(&id) {
                            capture.cancel_disconnect();
                        }
                        if let Some(printer) = self
                            .config
                            .printers
                            .iter()
                            .find(|p| p.local_id == id)
                            .cloned()
                        {
                            self.start_driver(printer);
                        }
                        self.refresh_registries();
                        self.emit("Impresora actualizada");
                        Ok(())
                    }
                    Err(error) => {
                        let msg = sanitize_message(
                            &format!("No se pudo actualizar la impresora: {error}"),
                            512,
                        );
                        self.emit(&msg);
                        Err(msg)
                    }
                }
            }
            EdgeAction::UpdatePrinterMetadata { id, name } => {
                let result = self
                    .persist_candidate(move |mut candidate| {
                        candidate.update_printer_metadata(id, name)?;
                        Ok(candidate)
                    })
                    .await;
                match result {
                    Ok(config) => {
                        self.config = config;
                        self.refresh_registries();
                        self.emit("Impresora actualizada");
                        Ok(())
                    }
                    Err(error) => {
                        let msg = sanitize_message(
                            &format!("No se pudo actualizar la impresora: {error}"),
                            512,
                        );
                        self.emit(&msg);
                        Err(msg)
                    }
                }
            }
            EdgeAction::RemovePrinter(id) => {
                let result = self
                    .persist_candidate(move |mut candidate| {
                        if !candidate.remove_printer(id) {
                            return Err(ConfigError::Validation(
                                "la impresora ya no existe".to_owned(),
                            ));
                        }
                        Ok(candidate)
                    })
                    .await;
                match result {
                    Ok(config) => {
                        self.config = config;
                        self.stop_driver(id).await;
                        self.refresh_registries();
                        self.emit("Impresora eliminada");
                        self.notice_captures.remove(&id);
                        Ok(())
                    }
                    Err(error) => {
                        let msg = sanitize_message(
                            &format!("No se pudo eliminar la impresora: {error}"),
                            512,
                        );
                        self.emit(&msg);
                        Err(msg)
                    }
                }
            }
            EdgeAction::SetEdgeToken(token) => {
                let result = self
                    .persist_candidate(move |mut candidate| {
                        candidate.set_edge_token(token.as_deref())?;
                        Ok(candidate)
                    })
                    .await;
                match result {
                    Ok(config) => {
                        self.config = config;
                        match self.config.edge_token() {
                            Ok(edge_token) => {
                                let _ = self.cloud_config_sender.send(CloudConfig {
                                    edge_token,
                                    installation_id: self.config.installation_id.clone(),
                                });
                                self.emit("Credencial cloud guardada");
                                Ok(())
                            }
                            Err(error) => {
                                let _ = self.cloud_config_sender.send(CloudConfig {
                                    edge_token: None,
                                    installation_id: self.config.installation_id.clone(),
                                });
                                let msg = sanitize_message(
                                    &format!(
                                        "La credencial cloud se guardó, pero no se pudo descifrar: {error}"
                                    ),
                                    512,
                                );
                                self.emit(&msg);
                                Ok(())
                            }
                        }
                    }
                    Err(error) => {
                        let msg = sanitize_message(
                            &format!("No se pudo guardar la credencial cloud: {error}"),
                            512,
                        );
                        self.emit(&msg);
                        Err(msg)
                    }
                }
            }
        };
        let _ = reply.send(result);
    }

    async fn persist_candidate<F>(&self, operation: F) -> Result<PersistedConfig, ConfigError>
    where
        F: FnOnce(PersistedConfig) -> Result<PersistedConfig, ConfigError> + Send + 'static,
    {
        let store = Arc::clone(&self.store);
        let current = self.config.clone();
        tokio::task::spawn_blocking(move || {
            let candidate = operation(current)?;
            store.save(&candidate)?;
            Ok(candidate)
        })
        .await
        .map_err(|error| ConfigError::Io(std::io::Error::other(error.to_string())))?
    }

    fn start_driver(&mut self, printer: PrinterConfig) {
        let generation = self.next_generation;
        let Some(next_generation) = self.next_generation.checked_add(1) else {
            self.emit("No se pudo iniciar impresora: contador de generación agotado");
            return;
        };
        self.next_generation = next_generation;
        let mut initial_state = PrinterState::new(printer.local_id);
        let capture = self
            .notice_captures
            .entry(printer.local_id)
            .or_insert_with(|| {
                crate::shared::notifications::NoticeCapture::new(
                    printer.local_id,
                    self.notice_sender.clone(),
                    self.notices_enabled.subscribe(),
                )
            });
        initial_state.notifications = Some(capture.clone());
        let cloud_id = derived_cloud_printer_id(&self.config.installation_id, printer.local_id);
        let history = self.history.handle();
        if printer.connection.protocol == Protocol::Flashforge {
            initial_state.history_capture = Some(history.capture(cloud_id));
        } else {
            initial_state.native_history = Some(crate::history::NativeHistory {
                handle: history,
                printer_id: cloud_id,
            });
        }
        let api_key = match self.config.moonraker_api_key(printer.local_id) {
            Ok(api_key) => api_key,
            Err(error) => {
                let _ = initial_state.set_connection(LanConnection::NeedsCredentials);
                let _ = initial_state.record_error("No se pudo descifrar la credencial Moonraker");
                self.emit(format!("{}: {error}", printer.name));
                None
            }
        };
        let (state_sender, state_receiver) = watch::channel(initial_state);
        let (driver_shutdown_sender, driver_shutdown) = watch::channel(false);
        let (detail, detail_io) = make_detail_channels();
        let (operation, operation_io) = make_operation_channels();
        let (camera, camera_io) = make_camera_channels();
        let (thumbnail, thumbnail_io) = crate::shared::thumbnail::make_thumbnail_channels();
        let join = if printer.connection.protocol == crate::config::Protocol::Flashforge {
            self.runtime
                .spawn(crate::integrations::flashforge::run_driver_with_thumbnail(
                    printer.local_id,
                    printer.connection.host,
                    printer.connection.port,
                    api_key,
                    state_sender,
                    driver_shutdown,
                    detail_io,
                    operation_io,
                    thumbnail_io,
                    camera_io,
                ))
        } else {
            let config = DriverConfig {
                id: printer.local_id,
                connection: printer.connection,
                api_key,
            };
            self.runtime.spawn(
                crate::integrations::moonraker::adapter::run_adapter_with_thumbnail(
                    config,
                    state_sender,
                    driver_shutdown,
                    detail_io,
                    operation_io,
                    camera_io,
                    thumbnail_io,
                ),
            )
        };
        self.drivers.insert(
            printer.local_id,
            DriverTask {
                generation,
                shutdown: driver_shutdown_sender,
                join,
                receiver: state_receiver,
                detail,
                operation,
                camera,
                thumbnail,
            },
        );
    }

    async fn stop_driver(&mut self, id: LocalPrinterId) {
        let Some(mut task) = self.drivers.remove(&id) else {
            return;
        };
        let _ = task.shutdown.send(true);
        if timeout(Duration::from_secs(2), &mut task.join)
            .await
            .is_err()
        {
            task.join.abort();
            let _ = (&mut task.join).await;
        }
        if let Some(capture) = &task.receiver.borrow().history_capture {
            capture.close_unknown();
        }
    }

    async fn stop_all(&mut self) {
        let captures = self
            .drivers
            .values()
            .filter_map(|task| task.receiver.borrow().history_capture.clone())
            .collect::<Vec<_>>();
        let mut tasks = std::mem::take(&mut self.drivers)
            .into_values()
            .collect::<Vec<_>>();
        stop_driver_tasks(&mut tasks, SHUTDOWN_BUDGET).await;
        self.refresh_registries();
        self.cloud_join.abort();
        let _ = (&mut self.cloud_join).await;
        self.notifications_join.abort();
        let _ = (&mut self.notifications_join).await;
        let flushed = timeout(HISTORY_FLUSH_BUDGET, async {
            for capture in captures {
                capture.checkpoint_checked().await?;
            }
            self.history.flush_checked().await
        })
        .await;
        if let Some(event) = history_flush_failure(flushed) {
            crate::diagnostics::log(event);
            self.emit("No se terminó de guardar el historial al cerrar");
        }
    }

    fn refresh_registries(&self) {
        let mut ui_printers = Vec::new();
        let mut cloud_printers = Vec::new();
        for printer in &self.config.printers {
            let Some(task) = self.drivers.get(&printer.local_id) else {
                continue;
            };
            if task.receiver.borrow().id != printer.local_id {
                self.emit("El estado local de una impresora no coincide con su configuración");
                continue;
            }
            let decrypted_auth = self
                .config
                .moonraker_api_key(printer.local_id)
                .ok()
                .flatten();
            let device = device_for_cloud(printer, decrypted_auth.as_deref());
            let credential_status = if decrypted_auth.is_some() {
                crate::shared::printer::CredentialStatus::Configured
            } else {
                crate::shared::printer::CredentialStatus::None
            };
            let connection_view = crate::shared::printer::EditableConnectionView {
                protocol: printer.connection.protocol,
                host: printer.connection.host.clone(),
                port: printer.connection.port,
                tls: printer.connection.tls,
                trusted_lan: matches!(
                    printer.connection.auth,
                    crate::config::MoonrakerAuth::TrustedLan
                ),
                credential_status,
                light_bindings: printer.connection.light_bindings.clone(),
            };
            let metadata = crate::shared::printer::PrinterLocalMetadata {
                serial_suffix: device.as_ref().and_then(|d| d.serial_suffix.clone()),
                local_ip: device.as_ref().and_then(|d| d.local_ip),
            };
            ui_printers.push(UiPrinterReceiver {
                history: self.history.handle(),
                history_id: derived_cloud_printer_id(
                    &self.config.installation_id,
                    printer.local_id,
                ),
                id: printer.local_id,
                name: printer.name.clone(),
                session_generation: task.generation,
                connection_view,
                metadata,
                thumbnail: task.thumbnail.clone(),
                receiver: task.receiver.clone(),
                detail: task.detail.clone(),
                operation: task.operation.clone(),
                camera: task.camera.clone(),
            });
            cloud_printers.push(CloudPrinterReceiver {
                id: printer.local_id,
                generation: task.generation,
                cloud_printer_id: derived_cloud_printer_id(
                    &self.config.installation_id,
                    printer.local_id,
                ),
                name: printer.name.clone(),
                receiver: task.receiver.clone(),
                detail: task.detail.clone(),
                device,
                operation: task.operation.clone(),
                thumbnail: Some(task.thumbnail.clone()),
            });
        }
        let revision = self.ui_registry_sender.borrow().revision.saturating_add(1);
        let _ = self.ui_registry_sender.send(UiRegistry {
            revision,
            printers: ui_printers,
        });
        let _ = self.cloud_registry_sender.send(CloudRegistry {
            history: Some(self.history.handle()),
            revision,
            printers: cloud_printers,
        });
    }

    fn emit(&self, message: impl AsRef<str>) {
        let _ = self
            .diagnostics
            .try_send(sanitize_message(message.as_ref(), 512));
    }
}

pub(crate) fn cloud_device_metadata(
    printer: &PrinterConfig,
    secret: Option<&str>,
) -> Option<crate::shared::printer::CloudDeviceMetadata> {
    let serial_suffix = (printer.connection.protocol == Protocol::Flashforge)
        .then(|| flashforge_serial_suffix(secret))
        .flatten();
    let local_ip = private_local_ip(&printer.connection.host);
    if serial_suffix.is_none() && local_ip.is_none() {
        None
    } else {
        Some(crate::shared::printer::CloudDeviceMetadata {
            serial_suffix,
            local_ip,
        })
    }
}

fn device_for_cloud(printer: &PrinterConfig, secret: Option<&str>) -> Option<Device> {
    cloud_device_metadata(printer, secret).map(|m| Device {
        serial_suffix: m.serial_suffix,
        local_ip: m.local_ip,
    })
}

fn flashforge_serial_suffix(secret: Option<&str>) -> Option<String> {
    let serial = secret?.split_once(':')?.0.trim();
    let start = serial.char_indices().rev().nth(3)?.0;
    let suffix = &serial[start..];
    (start > 0 && suffix.chars().all(|ch| ch.is_ascii_alphanumeric())).then(|| suffix.to_owned())
}

fn private_local_ip(host: &str) -> Option<IpAddr> {
    let host = host
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(host);
    let ip = host.parse::<IpAddr>().ok()?;
    match ip {
        IpAddr::V4(value) if value.is_private() || value.is_link_local() => Some(ip),
        IpAddr::V6(value) if value.is_unique_local() || value.is_unicast_link_local() => Some(ip),
        _ => None,
    }
}

fn derived_cloud_printer_id(installation_id: &str, local_id: LocalPrinterId) -> String {
    format!("{installation_id}-{}", local_id.0)
}

pub(crate) fn history_flush_failure(
    result: Result<Result<(), crate::history::HistoryFlushError>, tokio::time::error::Elapsed>,
) -> Option<crate::diagnostics::LogEvent> {
    let reason = match result {
        Ok(Ok(())) => return None,
        Err(_) => "timeout",
        Ok(Err(crate::history::HistoryFlushError::Unavailable)) => "unavailable",
        Ok(Err(crate::history::HistoryFlushError::WriteFailed)) => "write_failed",
    };
    Some(
        crate::diagnostics::LogEvent::new(
            crate::diagnostics::LogLevel::Error,
            crate::diagnostics::LogArea::App,
            "history.flush_failed",
        )
        .result(reason),
    )
}

async fn stop_driver_tasks(tasks: &mut [DriverTask], budget: Duration) {
    for task in tasks.iter() {
        let _ = task.shutdown.send(true);
    }
    let deadline = tokio::time::Instant::now() + budget;
    for task in tasks {
        if tokio::time::timeout_at(deadline, &mut task.join)
            .await
            .is_err()
        {
            task.join.abort();
            let _ = (&mut task.join).await;
        }
    }
}
