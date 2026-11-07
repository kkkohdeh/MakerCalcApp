//! Package identity selects Windows integration, never printer or cloud behavior.
use std::path::PathBuf;
use std::sync::OnceLock;

use windows::ApplicationModel::{StartupTask, StartupTaskState};
use windows::Storage::ApplicationData;
use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize};
use windows::core::HSTRING;
use windows_sys::Win32::Foundation::{APPMODEL_ERROR_NO_PACKAGE, RPC_E_CHANGED_MODE};
use windows_sys::Win32::Storage::Packaging::Appx::GetCurrentPackageFullName;

use super::StartupView;
use crate::i18n::t;

pub(crate) const STORE_URL: &str = "ms-windows-store://pdp/?ProductId=9N7264XCS137";
pub(crate) const STARTUP_TASK: &str = "MakerCalcStartup";
pub(crate) const NOTIFICATION_PROTOCOL: &str = "makercalc-store-notify";

pub(crate) fn is_packaged() -> bool {
    static PACKAGED: OnceLock<bool> = OnceLock::new();
    *PACKAGED.get_or_init(|| {
        let mut length = 0;
        // SAFETY: a NULL buffer with length zero only queries this process's identity.
        // Unexpected errors must not enable legacy registry/profile operations.
        let result = unsafe { GetCurrentPackageFullName(&mut length, std::ptr::null_mut()) };
        result != APPMODEL_ERROR_NO_PACKAGE
    })
}

/// Balance only our own initialization, including when called on GPUI's existing STA.
pub(super) struct Apartment(bool);

impl Apartment {
    pub(super) fn enter() -> windows::core::Result<Self> {
        // SAFETY: no pointers; RoUninitialize is paired only after a successful call.
        match unsafe { RoInitialize(RO_INIT_MULTITHREADED) } {
            Ok(()) => Ok(Self(true)),
            Err(error) if error.code().0 == RPC_E_CHANGED_MODE => Ok(Self(false)),
            Err(error) => Err(error),
        }
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        if self.0 {
            unsafe { RoUninitialize() };
        }
    }
}

pub(crate) fn data_directory() -> std::io::Result<PathBuf> {
    let _apartment = Apartment::enter().map_err(std::io::Error::other)?;
    let path = ApplicationData::Current()
        .and_then(|data| data.LocalFolder())
        .and_then(|folder| folder.Path())
        .map_err(std::io::Error::other)?;
    let path = PathBuf::from(path.to_os_string());
    if !path.is_absolute() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            t!("settings.desktop.errors.package_directory").into_owned(),
        ));
    }
    Ok(path)
}

fn startup_view(state: StartupTaskState) -> StartupView {
    StartupView {
        requested: matches!(
            state,
            StartupTaskState::Enabled | StartupTaskState::EnabledByPolicy
        ),
        can_enable: matches!(
            state,
            StartupTaskState::Disabled | StartupTaskState::Enabled
        ),
    }
}

/// Called on the existing blocking pool, not during rendering or on GPUI's thread.
pub(crate) fn startup_setting(enable: Option<bool>) -> Result<StartupView, String> {
    let result = (|| -> windows::core::Result<StartupView> {
        let _apartment = Apartment::enter()?;
        let task = StartupTask::GetAsync(&HSTRING::from(STARTUP_TASK))?.join()?;
        if let Some(enable) = enable {
            if enable {
                task.RequestEnableAsync()?.join()?;
            } else {
                task.Disable()?;
            }
        }
        Ok(startup_view(task.State()?))
    })();
    result.map_err(|error| {
        t!("settings.desktop.errors.registration", error = error.code()).into_owned()
    })
}
