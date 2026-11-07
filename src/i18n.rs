//! Compile-time en/es translations, Windows language resolution and GPUI Kit catalog extension.

use std::sync::Once;

use crate::shared::workspace::UiLanguage;

// Embed all locale JSON files at compile time with English fallback.
rust_i18n::i18n!("locales", fallback = "en");

pub use rust_i18n::t;

static EXTENSION_ONCE: Once = Once::new();

/// Extends GPUI Kit's component catalog (`gpui_component`) with our application translations.
/// Safe to call multiple times; the underlying extension registration runs exactly once.
fn extend_component_catalog() {
    EXTENSION_ONCE.call_once(|| {
        gpui_kit::component::_rust_i18n_extend(crate::_rust_i18n_backend(), "gpui_component");
    });
}

/// Queries the user's preferred UI languages from Windows in priority order.
/// Returns an empty list if resolution fails or on non-Windows platforms.
#[cfg(target_os = "windows")]
fn windows_preferred_languages() -> Vec<String> {
    use windows_sys::Win32::Globalization::{GetUserPreferredUILanguages, MUI_LANGUAGE_NAME};

    let mut num_languages: u32 = 0;
    let mut buffer_len: u32 = 0;

    let success = unsafe {
        GetUserPreferredUILanguages(
            MUI_LANGUAGE_NAME,
            &mut num_languages,
            std::ptr::null_mut(),
            &mut buffer_len,
        )
    };

    if success == 0 || buffer_len == 0 {
        return Vec::new();
    }

    let mut buffer: Vec<u16> = vec![0; buffer_len as usize];
    let success = unsafe {
        GetUserPreferredUILanguages(
            MUI_LANGUAGE_NAME,
            &mut num_languages,
            buffer.as_mut_ptr(),
            &mut buffer_len,
        )
    };

    if success == 0 {
        return Vec::new();
    }

    let mut languages = Vec::with_capacity(num_languages as usize);
    let mut start = 0;
    for (i, &ch) in buffer.iter().enumerate() {
        if ch == 0 {
            if i > start {
                let tag = String::from_utf16_lossy(&buffer[start..i]);
                let trimmed = tag.trim();
                if !trimmed.is_empty() {
                    languages.push(trimmed.to_string());
                }
            }
            start = i + 1;
            if languages.len() == num_languages as usize {
                break;
            }
        }
    }

    languages
}

/// Fallback for non-Windows platforms.
#[cfg(not(target_os = "windows"))]
fn windows_preferred_languages() -> Vec<String> {
    Vec::new()
}

/// Resolve the first supported language tag (`es`, `en`, or a `-`/`_` variant).
/// Default to `en` when none match.
fn resolve_system_language(preferred: &[impl AsRef<str>]) -> &'static str {
    for lang in preferred {
        let tag = lang.as_ref().trim().to_ascii_lowercase();
        if tag == "es" || tag.starts_with("es-") || tag.starts_with("es_") {
            return "es";
        }
        if tag == "en" || tag.starts_with("en-") || tag.starts_with("en_") {
            return "en";
        }
    }
    "en"
}

/// Resolves the effective locale code (`"en"` or `"es"`) for a given `UiLanguage` setting.
fn resolve_locale(language: UiLanguage) -> &'static str {
    match language {
        UiLanguage::English => "en",
        UiLanguage::Spanish => "es",
        UiLanguage::System => {
            let preferred = windows_preferred_languages();
            resolve_system_language(&preferred)
        }
    }
}

/// Initializes application localization: extends the component catalog and sets the active locale.
/// Must be called before `gpui_kit::init` and before constructing texts stored in UI entities.
pub(crate) fn init(language: UiLanguage) {
    extend_component_catalog();
    let locale = resolve_locale(language);
    rust_i18n::set_locale(locale);
}
