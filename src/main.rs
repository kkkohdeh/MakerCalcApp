#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod cloud;
mod config;
mod desktop;
mod diagnostics;
mod discovery;
mod edge;
mod gpui_app;
mod history;
mod i18n;
mod integrations;
mod shared;
mod state;
mod updater;

#[allow(unused_imports)]
pub(crate) use i18n::{
    _rust_i18n_available_locales, _rust_i18n_backend, _rust_i18n_extend,
    _rust_i18n_lookup_fallback, _rust_i18n_t, _rust_i18n_tkv, _rust_i18n_translate,
    _rust_i18n_try_translate,
};

use crate::i18n::t;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use tokio::runtime::{Builder, Handle};

/// Shared only by main and GPUI's quit callback. Take-before-wait makes cleanup idempotent.
struct AppShutdown {
    edge: Option<crate::edge::EdgeController>,
    runtime: Handle,
    logger: Rc<RefCell<Option<crate::diagnostics::LoggerGuard>>>,
}

impl AppShutdown {
    fn finish(&mut self) {
        if let Some(edge) = self.edge.take() {
            edge.shutdown(&self.runtime);
            crate::diagnostics::log(
                crate::diagnostics::LogEvent::new(
                    crate::diagnostics::LogLevel::Info,
                    crate::diagnostics::LogArea::App,
                    "app.stopped",
                )
                .result("ok"),
            );
            // The Windows session-end path can exit before main returns. Drain our logger here too.
            if let Some(logger) = self.logger.borrow_mut().take() {
                logger.shutdown();
            }
        }
    }
}

fn report_startup_error(error: &str) {
    eprintln!("{}", t!("common.startup.failed", error = error));
    let _ = rfd::MessageDialog::new()
        .set_title("MakerCalc")
        .set_description(error)
        .set_level(rfd::MessageLevel::Error)
        .show();
}

#[cfg(target_os = "windows")]
fn init_windows_taskbar_identity() {
    if crate::desktop::package::is_packaged() {
        return; // Windows assigns the package's AUMID; do not impersonate the Inno app.
    }
    #[link(name = "shell32")]
    unsafe extern "system" {
        fn SetCurrentProcessExplicitAppUserModelID(app_id: *const u16) -> i32;
    }

    let id: Vec<u16> = crate::desktop::notifications::APP_ID
        .encode_utf16()
        .chain(Some(0))
        .collect();
    unsafe {
        let _ = SetCurrentProcessExplicitAppUserModelID(id.as_ptr());
    }
}

fn main() {
    if let Some(background) = crate::desktop::setup_startup_preference(std::env::args_os().skip(1))
    {
        if let Err(error) = crate::desktop::configure_startup_from_setup(background) {
            eprintln!("{error}");
            std::process::exit(1);
        }
        return;
    }
    let activation =
        match crate::desktop::notifications::activation_from_args(std::env::args_os().skip(1)) {
            Ok(activation) => activation,
            Err(()) => {
                report_startup_error(&t!("settings.notifications.invalid_activation"));
                return;
            }
        };
    let notification_printer = activation.and_then(|activation| activation.printer());
    let startup = crate::desktop::is_startup(std::env::args_os().skip(1));
    // Do not start logging, update cleanup, a runtime or SQLite in a duplicate process.
    let store = match crate::config::ConfigStore::open_default() {
        Ok(store) => Arc::new(store),
        Err(crate::config::ConfigError::LockBusy) => {
            let restored = if activation.is_some() {
                crate::desktop::restore_existing_printer(notification_printer)
            } else if !startup {
                crate::desktop::restore_existing()
            } else {
                Ok(())
            };
            if let Err(error) = restored {
                report_startup_error(&error);
            }
            return;
        }
        Err(error) => {
            report_startup_error(&error.to_string());
            return;
        }
    };
    let initial_language = match store.load_or_empty() {
        Ok(config) => config.desktop.language,
        Err(error) => {
            report_startup_error(&error.to_string());
            return;
        }
    };
    crate::i18n::init(initial_language);
    #[cfg(target_os = "windows")]
    init_windows_taskbar_identity();

    std::panic::set_hook(Box::new(|info| {
        crate::diagnostics::write_crash(&info.to_string());
    }));
    crate::updater::cleanup_previous_update();
    let logger = match crate::diagnostics::init_default() {
        Ok(logger) => Some(logger),
        Err(error) => {
            eprintln!("{}", t!("common.startup.diagnostics_failed", error = error));
            None
        }
    };
    crate::diagnostics::log(crate::diagnostics::LogEvent::new(
        crate::diagnostics::LogLevel::Info,
        crate::diagnostics::LogArea::App,
        "app.started",
    ));
    let logger = Rc::new(RefCell::new(logger));
    let res = run(store, startup, notification_printer, logger.clone());
    if let Err(ref error) = res {
        crate::diagnostics::log(
            crate::diagnostics::LogEvent::new(
                crate::diagnostics::LogLevel::Error,
                crate::diagnostics::LogArea::App,
                "app.failed",
            )
            .result("failed")
            .detail(error),
        );
        eprintln!("{}", t!("common.startup.failed", error = error));
        let _ = rfd::MessageDialog::new()
            .set_title("MakerCalc")
            .set_description(t!("common.startup.failed", error = error).into_owned())
            .set_level(rfd::MessageLevel::Error)
            .show();
    }
    if let Some(logger) = logger.borrow_mut().take() {
        logger.shutdown();
    }
    if let Err(error) = crate::updater::launch_pending_restart() {
        report_startup_error(&error);
    }
}

fn run(
    store: Arc<crate::config::ConfigStore>,
    startup: bool,
    notification_printer: Option<crate::state::LocalPrinterId>,
    logger: Rc<RefCell<Option<crate::diagnostics::LoggerGuard>>>,
) -> Result<(), String> {
    let runtime = Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| t!("common.startup.runtime_failed", error = error).into_owned())?;
    let guard = runtime.enter();
    let (edge, ui) =
        match crate::edge::EdgeController::start_with_store(runtime.handle(), store.clone()) {
            Ok(res) => res,
            Err(error) => {
                drop(guard);
                return Err(error.to_string());
            }
        };
    let shutdown = Rc::new(RefCell::new(AppShutdown {
        edge: Some(edge),
        runtime: runtime.handle().clone(),
        logger,
    }));
    let on_quit = shutdown.clone();
    crate::gpui_app::run(ui, startup, notification_printer, move || {
        on_quit.borrow_mut().finish()
    });
    drop(guard);
    shutdown.borrow_mut().finish();
    Ok(())
}
