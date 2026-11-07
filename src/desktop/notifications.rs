//! Windows owns presentation; our activation protocol can only open a local view.
use std::ffi::OsString;
use std::ptr::null_mut;

use windows::Data::Xml::Dom::XmlDocument;
use windows::Foundation::{DateTime, IReference, PropertyValue};
use windows::UI::Notifications::{
    NotificationSetting, ToastNotification, ToastNotificationManager,
};
use windows::core::{HSTRING, Interface};
use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_SZ, RegGetValueW};

use super::package::Apartment;
use crate::i18n::t;
use crate::state::LocalPrinterId;

pub(crate) const APP_ID: &str = "MakerCalc.Desktop";
const PROTOCOL: &str = "makercalc-notify";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Activation {
    Open,
    Printer(LocalPrinterId),
}

impl Activation {
    pub(crate) fn printer(self) -> Option<LocalPrinterId> {
        match self {
            Self::Open => None,
            Self::Printer(id) => Some(id),
        }
    }

    fn uri(self) -> String {
        let protocol = if super::package::is_packaged() {
            super::package::NOTIFICATION_PROTOCOL
        } else {
            PROTOCOL
        };
        match self {
            Self::Open => format!("{protocol}://open"),
            Self::Printer(id) => format!("{protocol}://printer/{}", id.0),
        }
    }
}

pub(crate) fn activation_from_args(
    args: impl IntoIterator<Item = OsString>,
) -> Result<Option<Activation>, ()> {
    let args: Vec<_> = args.into_iter().take(4).collect();
    if !args.iter().any(|arg| arg == "--notification") {
        return Ok(None);
    }
    if args.len() != 2 || args[0] != "--notification" {
        return Err(());
    }
    parse_activation(args[1].to_str().ok_or(())?).map(Some)
}

fn parse_activation(uri: &str) -> Result<Activation, ()> {
    let target = uri
        .strip_prefix("makercalc-notify://")
        .or_else(|| uri.strip_prefix("makercalc-store-notify://"))
        .ok_or(())?;
    if target == "open" {
        return Ok(Activation::Open);
    }
    let number = target.strip_prefix("printer/").ok_or(())?;
    if number.is_empty() || number.len() > 20 || !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(());
    }
    let id = number.parse::<u64>().map_err(|_| ())?;
    if id == 0 || id.to_string() != number {
        return Err(());
    }
    Ok(Activation::Printer(LocalPrinterId(id)))
}

fn registered() -> Result<(), String> {
    if super::package::is_packaged() {
        return Ok(()); // Protocol and app identity are registered by the MSIX manifest.
    }
    // Portable executables must not impersonate the installed app or hijack its click target.
    super::installed_command()
        .map_err(|_| t!("settings.notifications.installed_only").into_owned())?;
    let exe = std::env::current_exe()
        .map_err(|_| t!("settings.notifications.installed_only").into_owned())?;
    let expected = format!("\"{}\" --notification \"%1\"", exe.display());
    let key = super::wide("Software\\Classes\\makercalc-notify\\shell\\open\\command");
    let mut buffer = [0_u16; 1024];
    let mut bytes = std::mem::size_of_val(&buffer) as u32;
    // SAFETY: the buffer and length match; only a bounded REG_SZ is read from our own HKCU key.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            std::ptr::null(),
            RRF_RT_REG_SZ,
            null_mut(),
            buffer.as_mut_ptr().cast(),
            &mut bytes,
        )
    };
    let end = buffer
        .iter()
        .position(|&unit| unit == 0)
        .ok_or_else(|| t!("settings.notifications.installed_only").into_owned())?;
    let actual = String::from_utf16(&buffer[..end])
        .map_err(|_| t!("settings.notifications.installed_only").into_owned())?;
    if status != ERROR_SUCCESS || !actual.eq_ignore_ascii_case(&expected) {
        return Err(t!("settings.notifications.installed_only").into_owned());
    }
    Ok(())
}

fn document(activation: Activation, title: &str, body: &str) -> windows::core::Result<XmlDocument> {
    let document = XmlDocument::new()?;
    // The only interpolated attribute comes from our closed, numeric activation type.
    document.LoadXml(&HSTRING::from(format!(
        "<toast activationType=\"protocol\" launch=\"{}\"><visual><binding template=\"ToastGeneric\"><text/><text/></binding></visual></toast>",
        activation.uri()
    )))?;
    let texts = document.GetElementsByTagName(&HSTRING::from("text"))?;
    for (index, value) in [title, body].iter().enumerate() {
        let bounded: String = value
            .chars()
            .filter(|c| !c.is_control())
            .take(240)
            .collect();
        texts
            .Item(index as u32)?
            .AppendChild(&document.CreateTextNode(&HSTRING::from(bounded))?)?;
    }
    Ok(document)
}

/// Called serially on Tokio's blocking pool. Failure never interrupts printer monitoring.
pub(crate) fn show(printer: Option<LocalPrinterId>, body: String) -> Result<(), String> {
    if printer.is_some() {
        let mut foreground_pid = 0;
        // A contextual in-app view already exists when MakerCalc is the foreground app.
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(
                windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow(),
                &mut foreground_pid,
            );
        }
        if foreground_pid == std::process::id() {
            return Ok(());
        }
    }
    registered()?;
    let result = (|| -> windows::core::Result<()> {
        let _apartment = Apartment::enter()?;
        let notifier = if super::package::is_packaged() {
            ToastNotificationManager::CreateToastNotifier()?
        } else {
            ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(APP_ID))?
        };
        if notifier.Setting()? != NotificationSetting::Enabled {
            return Err(windows::core::Error::from_hresult(windows::core::HRESULT(
                0x800704EC_u32 as i32,
            )));
        }
        let activation = printer.map_or(Activation::Open, Activation::Printer);
        let toast =
            ToastNotification::CreateToastNotification(&document(activation, "MakerCalc", &body)?)?;
        toast.SetGroup(&HSTRING::from("printers"))?;
        toast.SetTag(&HSTRING::from(
            printer.map_or_else(|| "test".into(), |id| format!("{:016x}", id.0)),
        ))?;
        let expiration = DateTime {
            UniversalTime: 116_444_736_000_000_000
                + (chrono::Utc::now().timestamp_millis() + 3_600_000) * 10_000,
        };
        let expiration: IReference<DateTime> = PropertyValue::CreateDateTime(expiration)?.cast()?;
        toast.SetExpirationTime(&expiration)?;
        notifier.Show(&toast)
    })();
    result.map_err(|error| t!("settings.notifications.failed", error = error.code()).into_owned())
}
