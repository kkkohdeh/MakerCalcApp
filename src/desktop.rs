//! Windows desktop lifecycle only. No credentials, printer actions or network traffic.
use std::cell::Cell;
use std::ffi::{OsStr, OsString};
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::ptr::{null, null_mut};
use std::rc::Rc;
use std::time::{Duration, Instant};

use crate::i18n::t;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use sha2::{Digest as _, Sha256};
use windows_sys::Win32::Foundation::{
    ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, HWND, LPARAM, LRESULT, POINT, WPARAM,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_SZ,
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegGetValueW, RegSetValueExW,
};
use windows_sys::Win32::UI::Controls::LoadIconWithScaleDown;
use windows_sys::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
use windows_sys::Win32::UI::Shell::{
    DefSubclassProc, NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NIM_SETVERSION, NIN_SELECT, NOTIFYICON_VERSION_4, NOTIFYICONDATAW, NOTIFYICONDATAW_0,
    RemoveWindowSubclass, SetWindowSubclass, Shell_NotifyIconW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::shared::workspace::DesktopPreferences;

// Shared with installer/makercalc.iss. These messages carry no paths or commands.
const WINDOW_MARKER: &str = "MakerCalc.Desktop.Instance.v1";
const RESTORE_MESSAGE: &str = "MakerCalc.Desktop.Restore.v1";
const NOTIFICATION_MESSAGE: &str = "MakerCalc.Desktop.Notification.v1";
const QUIT_MESSAGE: &str = "MakerCalc.Desktop.Quit.v1";
const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const RUN_VALUE: &str = "MakerCalc";
const TRAY_MESSAGE: u32 = WM_APP + 83;
const HIDE_MESSAGE: u32 = WM_APP + 84;
const NIN_KEYSELECT: u32 = NIN_SELECT | 1;
const SUBCLASS_ID: usize = 0x4d43;
const OPEN: u32 = 1;
const SETTINGS: u32 = 2;
const EXIT: u32 = 3;

pub(crate) mod notifications;
pub(crate) mod package;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DesktopEvent {
    Open,
    OpenPrinter(crate::state::LocalPrinterId),
    Settings,
    Quit,
    TrayUnavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct StartupView {
    /// Requested registration, not proof that Windows has enabled execution.
    pub(crate) requested: bool,
    pub(crate) can_enable: bool,
}

pub(crate) fn is_startup(args: impl IntoIterator<Item = OsString>) -> bool {
    args.into_iter().any(|arg| arg == OsStr::new("--startup"))
}

pub(crate) fn setup_startup_preference(args: impl IntoIterator<Item = OsString>) -> Option<bool> {
    let mut args = args.into_iter();
    let first = args.next()?;
    match (first.to_str()?, args.next()) {
        ("--setup-startup=window", None) => Some(false),
        ("--setup-startup=background", None) => Some(true),
        _ => None,
    }
}

pub(crate) fn should_start_hidden(
    startup: bool,
    prefs: DesktopPreferences,
    tray_ready: bool,
) -> bool {
    startup && prefs.start_in_background && tray_ready
}

pub(crate) fn show_without_tray(window: &impl HasWindowHandle) -> Result<(), String> {
    let RawWindowHandle::Win32(raw) = window.window_handle().map_err(|e| e.to_string())?.as_raw()
    else {
        return Err(t!("settings.desktop.errors.window_handle").into_owned());
    };
    // Async avoids invoking GPUI visibility callbacks inside construction's mutable borrow.
    if unsafe { ShowWindowAsync(raw.hwnd.get() as HWND, SW_SHOW) } == 0 {
        return Err(win_error(t!("settings.desktop.errors.show")));
    }
    Ok(())
}

fn wide(value: &str) -> Vec<u16> {
    OsStr::new(value).encode_wide().chain(Some(0)).collect()
}

fn profile_marker(path: &Path) -> Vec<u16> {
    let bytes: Vec<u8> = path
        .as_os_str()
        .encode_wide()
        .flat_map(u16::to_le_bytes)
        .collect();
    wide(&format!("MakerCalc.Profile.{:x}", Sha256::digest(bytes)))
}

fn win_error(operation: impl AsRef<str>) -> String {
    format!(
        "{}: {}",
        operation.as_ref(),
        std::io::Error::last_os_error()
    )
}

/// Called only when the profile lock is busy, before logging or starting Tokio/Edge.
pub(crate) fn restore_existing() -> Result<(), String> {
    restore_existing_printer(None)
}

pub(crate) fn restore_existing_printer(
    printer: Option<crate::state::LocalPrinterId>,
) -> Result<(), String> {
    let marker =
        profile_marker(&crate::config::default_data_directory().map_err(|e| e.to_string())?);
    let signal = if printer.is_some() {
        NOTIFICATION_MESSAGE
    } else {
        RESTORE_MESSAGE
    };
    let message = unsafe { RegisterWindowMessageW(wide(signal).as_ptr()) };
    if message == 0 {
        return Err(win_error(t!("settings.desktop.errors.request_open")));
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if let Some(hwnd) = find_profile_window(&marker) {
            let mut pid = 0;
            unsafe {
                GetWindowThreadProcessId(hwnd, &mut pid);
            }
            // Windows may deny focus; the directed message still makes the window visible.
            if pid != 0 {
                unsafe {
                    AllowSetForegroundWindow(pid);
                }
            }
            let mut acknowledged = 0;
            let delivered = unsafe {
                SendMessageTimeoutW(
                    hwnd,
                    message,
                    printer.map_or(0, |id| id.0 as usize),
                    0,
                    SMTO_ABORTIFHUNG | SMTO_BLOCK,
                    250,
                    &mut acknowledged,
                )
            };
            if delivered != 0
                && acknowledged == 1
                && unsafe { IsWindowVisible(hwnd) != 0 && IsIconic(hwnd) == 0 }
            {
                return Ok(());
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(t!("settings.desktop.errors.already_open").into_owned())
}

fn find_profile_window(marker: &[u16]) -> Option<HWND> {
    struct Search<'a> {
        marker: &'a [u16],
        found: HWND,
    }
    unsafe extern "system" fn visit(hwnd: HWND, param: LPARAM) -> i32 {
        // SAFETY: EnumWindows is synchronous; Search and its marker live through the call.
        let search = unsafe { &mut *(param as *mut Search<'_>) };
        if unsafe { GetPropW(hwnd, search.marker.as_ptr()) } as usize == SUBCLASS_ID {
            search.found = hwnd;
            0
        } else {
            1
        }
    }
    let mut search = Search {
        marker,
        found: null_mut(),
    };
    unsafe {
        EnumWindows(Some(visit), &mut search as *mut _ as LPARAM);
    }
    (!search.found.is_null()).then_some(search.found)
}

/// One Rc allocation remains stable while the native subclass is installed. UI thread only.
#[derive(Clone)]
pub(crate) struct DesktopWindow(Rc<NativeWindow>);

struct NativeWindow {
    hwnd: Cell<HWND>,
    icon: Cell<HICON>,
    tray: Cell<bool>,
    prefs: Cell<DesktopPreferences>,
    taskbar_created: u32,
    restore: u32,
    notification: u32,
    quit: u32,
    marker: Vec<u16>,
    on_event: Box<dyn Fn(DesktopEvent)>,
}

impl DesktopWindow {
    pub(crate) fn attach(
        window: &impl HasWindowHandle,
        on_event: impl Fn(DesktopEvent) + 'static,
    ) -> Result<Self, String> {
        let marker =
            profile_marker(&crate::config::default_data_directory().map_err(|e| e.to_string())?);
        Self::attach_with_marker(window, on_event, marker)
    }

    fn attach_with_marker(
        window: &impl HasWindowHandle,
        on_event: impl Fn(DesktopEvent) + 'static,
        marker: Vec<u16>,
    ) -> Result<Self, String> {
        let RawWindowHandle::Win32(raw) =
            window.window_handle().map_err(|e| e.to_string())?.as_raw()
        else {
            return Err(t!("settings.desktop.errors.window_handle").into_owned());
        };
        let hwnd = raw.hwnd.get() as HWND;
        let state = Rc::new(NativeWindow {
            hwnd: Cell::new(hwnd),
            icon: Cell::new(null_mut()),
            tray: Cell::new(false),
            prefs: Cell::new(Default::default()),
            taskbar_created: unsafe { RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()) },
            restore: unsafe { RegisterWindowMessageW(wide(RESTORE_MESSAGE).as_ptr()) },
            notification: unsafe { RegisterWindowMessageW(wide(NOTIFICATION_MESSAGE).as_ptr()) },
            quit: unsafe { RegisterWindowMessageW(wide(QUIT_MESSAGE).as_ptr()) },
            marker,
            on_event: Box::new(on_event),
        });
        if state.restore == 0
            || state.notification == 0
            || state.quit == 0
            || state.taskbar_created == 0
        {
            return Err(win_error(t!("settings.desktop.errors.register_signals")));
        }
        // SAFETY: installed on the owning thread; Rc keeps the callback pointer stable.
        if unsafe {
            SetWindowSubclass(
                hwnd,
                Some(subclass),
                SUBCLASS_ID,
                Rc::as_ptr(&state) as usize,
            )
        } == 0
        {
            return Err(win_error(t!("settings.desktop.errors.attach_tray")));
        }
        if unsafe { SetPropW(hwnd, wide(WINDOW_MARKER).as_ptr(), SUBCLASS_ID as _) } == 0
            || unsafe { SetPropW(hwnd, state.marker.as_ptr(), SUBCLASS_ID as _) } == 0
        {
            return Err(win_error(t!("settings.desktop.errors.identify_window")));
        }
        Ok(Self(state))
    }

    pub(crate) fn set_preferences(&self, prefs: DesktopPreferences) -> Result<(), String> {
        self.0.prefs.set(prefs);
        if prefs.minimize_on_close || prefs.start_in_background {
            self.0.ensure_tray()
        } else {
            // Keep the existing window reachable when the last background option is removed.
            if unsafe { IsWindowVisible(self.0.hwnd.get()) } == 0 {
                self.show()?;
            }
            self.0.remove_tray();
            Ok(())
        }
    }

    pub(crate) fn tray_ready(&self) -> bool {
        self.0.tray.get()
    }
    pub(crate) fn hide(&self) -> bool {
        if !self.tray_ready() {
            return false;
        }
        // The close callback holds GPUI's mutable borrow; hide after it returns.
        unsafe { PostMessageW(self.0.hwnd.get(), HIDE_MESSAGE, 0, 0) != 0 }
    }
    pub(crate) fn show(&self) -> Result<(), String> {
        let hwnd = self.0.hwnd.get();
        if hwnd.is_null() {
            return Err(t!("settings.desktop.errors.window_closed").into_owned());
        }
        if unsafe { PostMessageW(hwnd, self.0.restore, 0, 0) } == 0 {
            return Err(win_error(t!("settings.desktop.errors.request_show")));
        }
        Ok(())
    }
    pub(crate) fn should_close(&self) -> bool {
        !(self.0.prefs.get().minimize_on_close && self.hide())
    }
}

impl NativeWindow {
    fn show(&self) {
        let hwnd = self.hwnd.get();
        if hwnd.is_null() {
            return;
        }
        unsafe {
            ShowWindow(
                hwnd,
                if IsIconic(hwnd) != 0 {
                    SW_RESTORE
                } else {
                    SW_SHOW
                },
            );
            SetForegroundWindow(hwnd);
        }
    }

    fn icon_data(&self) -> NOTIFYICONDATAW {
        let mut data = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd.get(),
            uID: 1,
            // HWND + uID work for installed and portable binaries without binding an EXE path.
            uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP | NIF_SHOWTIP,
            uCallbackMessage: TRAY_MESSAGE,
            hIcon: self.icon.get(),
            ..Default::default()
        };
        let tip = wide("MakerCalc");
        data.szTip[..tip.len()].copy_from_slice(&tip);
        data
    }

    fn load_icon(&self) -> Result<(), String> {
        let mut icon = null_mut();
        let dpi = unsafe { GetDpiForWindow(self.hwnd.get()) }.max(96);
        let size = unsafe { GetSystemMetricsForDpi(SM_CXSMICON, dpi) };
        let result = unsafe {
            LoadIconWithScaleDown(GetModuleHandleW(null()), 1usize as _, size, size, &mut icon)
        };
        if result < 0 || icon.is_null() {
            return Err(t!("settings.desktop.errors.load_icon").into_owned());
        }
        let old = self.icon.replace(icon);
        if !old.is_null() {
            unsafe {
                DestroyIcon(old);
            }
        }
        Ok(())
    }

    fn ensure_tray(&self) -> Result<(), String> {
        if self.tray.get() {
            return Ok(());
        }
        if self.icon.get().is_null() {
            self.load_icon()?;
        }
        let mut data = self.icon_data();
        if unsafe { Shell_NotifyIconW(NIM_ADD, &data) } == 0 {
            return Err(t!("settings.desktop.errors.tray_visible").into_owned());
        }
        data.Anonymous = NOTIFYICONDATAW_0 {
            uVersion: NOTIFYICON_VERSION_4,
        };
        if unsafe { Shell_NotifyIconW(NIM_SETVERSION, &data) } == 0 {
            unsafe {
                Shell_NotifyIconW(NIM_DELETE, &data);
            }
            return Err(t!("settings.desktop.errors.tray_controls").into_owned());
        }
        self.tray.set(true);
        Ok(())
    }

    fn remove_tray(&self) {
        if self.tray.replace(false) {
            unsafe {
                Shell_NotifyIconW(NIM_DELETE, &self.icon_data());
            }
        }
    }

    fn tray_failed(&self) {
        self.remove_tray();
        self.show();
        (self.on_event)(DesktopEvent::TrayUnavailable);
    }

    fn menu(&self, coordinates: WPARAM) {
        let menu = unsafe { CreatePopupMenu() };
        if menu.is_null() {
            self.tray_failed();
            return;
        }
        let mut ok = true;
        for (id, label) in [
            (OPEN, t!("settings.tray.open").into_owned()),
            (SETTINGS, t!("settings.title").into_owned()),
            (EXIT, t!("settings.tray.quit").into_owned()),
        ] {
            ok &= unsafe { AppendMenuW(menu, MF_STRING, id as usize, wide(&label).as_ptr()) } != 0;
        }
        if !ok {
            unsafe {
                DestroyMenu(menu);
            }
            self.tray_failed();
            return;
        }
        let mut point = POINT {
            x: coordinates as u16 as i16 as i32,
            y: (coordinates >> 16) as u16 as i16 as i32,
        };
        if point.x == -1 && point.y == -1 {
            unsafe {
                GetCursorPos(&mut point);
            }
        }
        let hwnd = self.hwnd.get();
        let command = unsafe {
            SetForegroundWindow(hwnd);
            TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_RIGHTBUTTON,
                point.x,
                point.y,
                0,
                hwnd,
                null(),
            )
        } as u32;
        unsafe {
            PostMessageW(hwnd, WM_NULL, 0, 0);
            DestroyMenu(menu);
        }
        match command {
            OPEN => {
                self.show();
                (self.on_event)(DesktopEvent::Open);
            }
            SETTINGS => {
                self.show();
                (self.on_event)(DesktopEvent::Settings);
            }
            EXIT => (self.on_event)(DesktopEvent::Quit),
            _ => {}
        }
    }
}

impl Drop for NativeWindow {
    fn drop(&mut self) {
        self.remove_tray();
        let hwnd = self.hwnd.get();
        if !hwnd.is_null() {
            unsafe {
                RemoveWindowSubclass(hwnd, Some(subclass), SUBCLASS_ID);
                RemovePropW(hwnd, wide(WINDOW_MARKER).as_ptr());
                RemovePropW(hwnd, self.marker.as_ptr());
            }
        }
        if !self.icon.get().is_null() {
            unsafe {
                DestroyIcon(self.icon.get());
            }
        }
    }
}

unsafe extern "system" fn subclass(
    hwnd: HWND,
    message: u32,
    wp: WPARAM,
    lp: LPARAM,
    _: usize,
    data: usize,
) -> LRESULT {
    // SAFETY: DesktopWindow and the window-close callback retain the Rc until WM_NCDESTROY.
    // Keep it alive even if DefSubclassProc synchronously destroys the owning GPUI view.
    let state = unsafe {
        Rc::increment_strong_count(data as *const NativeWindow);
        Rc::from_raw(data as *const NativeWindow)
    };
    // Never unwind across the Win32 ABI, even on dispatch failure.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if message == HIDE_MESSAGE && wp == 0 && lp == 0 {
            if state.tray.get() && state.prefs.get().minimize_on_close {
                unsafe {
                    ShowWindow(hwnd, SW_HIDE);
                }
            }
            return Some(0);
        }
        if message == state.restore && wp == 0 && lp == 0 {
            state.show();
            (state.on_event)(DesktopEvent::Open);
            return Some(1);
        }
        if message == state.notification && wp != 0 && lp == 0 {
            state.show();
            (state.on_event)(DesktopEvent::OpenPrinter(crate::state::LocalPrinterId(
                wp as u64,
            )));
            return Some(1);
        }
        if message == state.quit && wp == 0 && lp == 0 {
            (state.on_event)(DesktopEvent::Quit);
            return Some(1);
        }
        if message == TRAY_MESSAGE {
            match lp as u32 & 0xffff {
                NIN_SELECT | NIN_KEYSELECT => {
                    state.show();
                    (state.on_event)(DesktopEvent::Open);
                }
                WM_CONTEXTMENU => state.menu(wp),
                _ => {}
            }
            return Some(0);
        }
        if message == state.taskbar_created {
            let prefs = state.prefs.get();
            let needed = prefs.minimize_on_close || prefs.start_in_background;
            state.tray.set(false);
            if needed && state.ensure_tray().is_err() {
                state.tray_failed();
            }
            return Some(0);
        }
        if message == WM_DPICHANGED
            && state.tray.get()
            && (state.load_icon().is_err()
                || unsafe { Shell_NotifyIconW(NIM_MODIFY, &state.icon_data()) } == 0)
        {
            state.tray_failed();
        }
        if message == WM_NCDESTROY {
            state.remove_tray();
            unsafe {
                RemovePropW(hwnd, wide(WINDOW_MARKER).as_ptr());
                RemovePropW(hwnd, state.marker.as_ptr());
                RemoveWindowSubclass(hwnd, Some(subclass), SUBCLASS_ID);
            }
            state.hwnd.set(null_mut());
        }
        // QUERYENDSESSION is only a query; ENDSESSION remains GPUI's shutdown path.
        None
    }));
    match result {
        Ok(Some(value)) => value,
        Ok(None) => unsafe { DefSubclassProc(hwnd, message, wp, lp) },
        Err(_) => {
            crate::diagnostics::log(
                crate::diagnostics::LogEvent::new(
                    crate::diagnostics::LogLevel::Error,
                    crate::diagnostics::LogArea::App,
                    "app.desktop_callback_failed",
                )
                .result("failed"),
            );
            state.show();
            unsafe { DefSubclassProc(hwnd, message, wp, lp) }
        }
    }
}

fn startup_command(exe: &Path) -> Result<Vec<u16>, String> {
    let path: Vec<u16> = exe.as_os_str().encode_wide().collect();
    if !exe.is_absolute() || path.iter().any(|c| *c == 0 || *c == b'"' as u16) {
        return Err(t!("settings.desktop.errors.installation_path").into_owned());
    }
    let command: Vec<u16> = Some(b'"' as u16)
        .into_iter()
        .chain(path)
        .chain("\" --startup".encode_utf16())
        .collect();
    if command.len() > 260 {
        return Err(t!("settings.desktop.errors.path_length").into_owned());
    }
    Ok(command.into_iter().chain(Some(0)).collect())
}

fn installed_command() -> Result<Vec<u16>, String> {
    let data = crate::config::default_data_directory().map_err(|e| e.to_string())?;
    let programs = data
        .parent()
        .ok_or(t!("settings.desktop.errors.local_app_data").into_owned())?
        .join("Programs");
    let current = std::env::current_exe()
        .map_err(|_| t!("settings.desktop.errors.executable").into_owned())?;
    startup_command_for_installation(&programs, &current)
}

fn startup_command_for_installation(programs: &Path, current: &Path) -> Result<Vec<u16>, String> {
    let actual = current
        .canonicalize()
        .map_err(|_| t!("settings.desktop.errors.verify_executable").into_owned())?;
    let mut found_installation = false;
    // Inno Setup can keep the previous install directory when upgrading.
    for directory in ["MakerCalc", "MakerCalcCloud"] {
        let installed = programs.join(directory).join("makercalc.exe");
        if let Ok(expected) = installed.canonicalize() {
            if actual == expected {
                return startup_command(&installed);
            }
            found_installation = true;
        }
    }
    if found_installation {
        Err(t!("settings.desktop.errors.installed_only").into_owned())
    } else {
        Err(t!("settings.desktop.available_installed").into_owned())
    }
}

fn registry_error(code: u32) -> String {
    t!(
        "settings.desktop.errors.registration",
        error = std::io::Error::from_raw_os_error(code as i32)
    )
    .into_owned()
}

/// Read on opening Settings, not from render or a timer. Never inspect StartupApproved.
pub(crate) fn read_startup() -> Result<StartupView, String> {
    if package::is_packaged() {
        return package::startup_setting(None);
    }
    let mut size = 0;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            wide(RUN_KEY).as_ptr(),
            wide(RUN_VALUE).as_ptr(),
            RRF_RT_REG_SZ,
            null_mut(),
            null_mut(),
            &mut size,
        )
    };
    if status != ERROR_SUCCESS && status != ERROR_FILE_NOT_FOUND {
        return Err(registry_error(status));
    }
    Ok(StartupView {
        requested: status == ERROR_SUCCESS,
        can_enable: installed_command().is_ok(),
    })
}

pub(crate) fn set_startup(enable: bool) -> Result<StartupView, String> {
    if package::is_packaged() {
        return package::startup_setting(Some(enable));
    }
    let command = if enable {
        Some(installed_command()?)
    } else {
        None
    };
    write_startup(command.as_deref())?;
    read_startup()
}

pub(crate) fn configure_startup_from_setup(background: bool) -> Result<(), String> {
    let command = installed_command()?;
    let store = crate::config::ConfigStore::open_default().map_err(|error| error.to_string())?;
    let mut config = store.load_or_empty().map_err(|error| error.to_string())?;
    if config.desktop.start_in_background != background {
        config.desktop.start_in_background = background;
        store.save(&config).map_err(|error| error.to_string())?;
    }
    write_startup(Some(&command))
}

fn write_startup(command: Option<&[u16]>) -> Result<(), String> {
    let mut key: HKEY = null_mut();
    let status = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            wide(RUN_KEY).as_ptr(),
            0,
            null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            null(),
            &mut key,
            null_mut(),
        )
    };
    if status != ERROR_SUCCESS {
        return Err(registry_error(status));
    }
    let status = unsafe {
        if let Some(command) = command {
            RegSetValueExW(
                key,
                wide(RUN_VALUE).as_ptr(),
                0,
                REG_SZ,
                command.as_ptr().cast(),
                (command.len() * 2) as u32,
            )
        } else {
            RegDeleteValueW(key, wide(RUN_VALUE).as_ptr())
        }
    };
    unsafe {
        RegCloseKey(key);
    }
    if status != ERROR_SUCCESS && !(status == ERROR_FILE_NOT_FOUND && command.is_none()) {
        return Err(registry_error(status));
    }
    Ok(())
}

pub(crate) fn open_windows_startup_settings() -> Result<(), String> {
    let result = unsafe {
        windows_sys::Win32::UI::Shell::ShellExecuteW(
            null_mut(),
            wide("open").as_ptr(),
            wide("ms-settings:startupapps").as_ptr(),
            null(),
            null(),
            SW_SHOWNORMAL,
        )
    };
    if result as usize <= 32 {
        Err(t!("settings.desktop.errors.open_settings").into_owned())
    } else {
        Ok(())
    }
}
