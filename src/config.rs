use std::env;
use std::ffi::{OsString, c_void};
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{Read, Write};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use url::Url;
use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::Cryptography::{
    BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom, CRYPT_INTEGER_BLOB,
    CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
};
use windows_sys::Win32::Storage::FileSystem::ReplaceFileW;
use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::Win32::UI::Shell::{FOLDERID_LocalAppData, SHGetKnownFolderPath};

use crate::state::LocalPrinterId;

const DATA_DIRECTORY_NAME: &str = "MakerCalc";
const CONFIG_FILENAME: &str = "config.json";
const HISTORY_FILENAME: &str = "history.sqlite3";
const BACKUP_FILENAME: &str = "config.json.bak";
const INSTANCE_LOCK_FILENAME: &str = "instance.lock";
const MAX_CONFIG_BYTES: usize = 1024 * 1024;
const SCHEMA_VERSION: u32 = 1;
static TEMPORARY_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct PersistedConfig {
    pub(crate) schema_version: u32,
    pub(crate) next_local_id: u64,
    #[serde(default)]
    pub(crate) installation_id: String,
    pub(crate) edge_token_dpapi: Option<Vec<u8>>,
    pub(crate) printers: Vec<PrinterConfig>,
    #[serde(default)]
    pub(crate) desktop: crate::shared::workspace::DesktopPreferences,
}

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct PrinterConfig {
    pub(crate) local_id: LocalPrinterId,
    pub(crate) name: String,
    pub(crate) connection: MoonrakerConnection,
}

#[derive(Clone, Default, Deserialize, Serialize)]
pub(crate) struct MoonrakerConnection {
    pub(crate) protocol: Protocol,
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) tls: bool,
    pub(crate) auth: MoonrakerAuth,
    #[serde(default)]
    pub(crate) light_bindings: MoonrakerLightBindings,
    /// Explicit sensor object association; never guessed from its display name.
    #[serde(default)]
    pub(crate) chamber_temperature_sensor: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct MoonrakerLightBindings {
    pub(crate) build_area: Option<MoonrakerLightBinding>,
    pub(crate) toolhead: Option<MoonrakerLightBinding>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum MoonrakerLightBinding {
    OutputPin { name: String },
    ToggleMacro { name: String },
}

impl MoonrakerLightBinding {
    pub(crate) fn parse_optional(value: &str) -> Result<Option<Self>, ConfigError> {
        let value = value.trim();
        if value.is_empty() {
            return Ok(None);
        }
        let binding = if let Some(name) = value.strip_prefix("output_pin ") {
            Self::OutputPin {
                name: name.trim().to_owned(),
            }
        } else {
            Self::ToggleMacro {
                name: value.to_owned(),
            }
        };
        validate_light_binding(&binding)?;
        Ok(Some(binding))
    }

    pub(crate) fn display_value(&self) -> String {
        match self {
            Self::OutputPin { name } => format!("output_pin {name}"),
            Self::ToggleMacro { name } => name.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Protocol {
    #[default]
    Moonraker,
    Flashforge,
    Bambulab,
}

pub(crate) fn bambu_port_suggestion(
    previous: Protocol,
    next: Protocol,
    port: &str,
) -> Option<&'static str> {
    match (previous, next, port.trim()) {
        (Protocol::Bambulab, Protocol::Bambulab, _) => None,
        (_, Protocol::Bambulab, "" | "7125" | "8898" | "8899") => Some("8883"),
        (Protocol::Bambulab, _, "8883") => Some("7125"),
        _ => None,
    }
}

pub(crate) fn bambu_credentials(value: &str) -> Option<(&str, &str)> {
    let (serial, code) = value.split_once(':')?;
    let (serial, code) = (serial.trim(), code.trim());
    (!serial.is_empty()
        && serial.len() <= 63
        && serial.bytes().all(|byte| byte.is_ascii_alphanumeric())
        && !code.is_empty()
        && code.len() <= 64
        && code
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && byte != b':'))
    .then_some((serial, code))
}

impl std::fmt::Display for Protocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Moonraker => write!(f, "Moonraker / Klipper"),
            Self::Flashforge => write!(f, "FlashForge (Adventurer 5M / AD5X)"),
            Self::Bambulab => write!(f, "Bambu Lab"),
        }
    }
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub(crate) enum MoonrakerAuth {
    ApiKey {
        secret_dpapi: Vec<u8>,
    },
    #[default]
    TrustedLan,
}

#[derive(Clone)]
pub(crate) struct PrinterDraft {
    pub(crate) name: String,
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) tls: bool,
    pub(crate) api_key: Option<String>,
    pub(crate) trusted_lan: bool,
    pub(crate) protocol: Option<Protocol>,
    pub(crate) light_bindings: MoonrakerLightBindings,
}

pub(crate) struct ConfigStore {
    directory: PathBuf,
    config_path: PathBuf,
    backup_path: PathBuf,
    _profile_lock: File,
}

#[derive(Debug)]
pub(crate) enum ConfigError {
    ConfigDirectoryUnavailable(std::io::Error),
    DataDirectoryConflict(PathBuf),
    LockBusy,
    Io(std::io::Error),
    InvalidJson(String),
    InvalidSchema(u32),
    Validation(String),
    Dpapi(std::io::Error),
    InvalidSecretEncoding,
    SizeLimitExceeded,
    RandomnessUnavailable,
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ConfigDirectoryUnavailable(error) => {
                write!(
                    formatter,
                    "Windows no pudo localizar la carpeta de datos locales: {error}"
                )
            }
            Self::DataDirectoryConflict(path) => write!(
                formatter,
                "Se detectó un perfil anterior en {}. No se creará otro perfil. Restaura la ubicación anterior o migra ese perfil antes de abrir la app.",
                path.display()
            ),
            Self::LockBusy => formatter.write_str("MakerCalc ya está abierto"),
            Self::Io(error) => write!(formatter, "error de archivo: {error}"),
            Self::InvalidJson(error) => write!(formatter, "configuración JSON inválida: {error}"),
            Self::InvalidSchema(version) => {
                write!(
                    formatter,
                    "versión de configuración no soportada: {version}"
                )
            }
            Self::Validation(error) => formatter.write_str(error),
            Self::Dpapi(error) => {
                write!(formatter, "DPAPI no pudo proteger la credencial: {error}")
            }
            Self::InvalidSecretEncoding => {
                formatter.write_str("la credencial protegida no contiene texto UTF-8 válido")
            }
            Self::SizeLimitExceeded => formatter.write_str("la configuración supera 1 MiB"),
            Self::RandomnessUnavailable => {
                formatter.write_str("no se pudo generar el id de instalación")
            }
        }
    }
}

impl std::error::Error for ConfigError {}

impl From<std::io::Error> for ConfigError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl PersistedConfig {
    pub(crate) fn empty() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            next_local_id: 1,
            installation_id: String::new(),
            edge_token_dpapi: None,
            printers: Vec::new(),
            desktop: Default::default(),
        }
    }

    pub(crate) fn add_printer(
        &mut self,
        draft: PrinterDraft,
    ) -> Result<LocalPrinterId, ConfigError> {
        validate_printer_draft(&draft)?;
        let local_id = LocalPrinterId(self.next_local_id);
        self.next_local_id = self
            .next_local_id
            .checked_add(1)
            .ok_or_else(|| ConfigError::Validation("el contador local está agotado".to_owned()))?;
        let protocol = draft
            .protocol
            .unwrap_or(if draft.port == 8898 || draft.port == 8899 {
                Protocol::Flashforge
            } else {
                Protocol::Moonraker
            });
        let auth = match (protocol, draft.trusted_lan, draft.api_key) {
            (Protocol::Bambulab, _, Some(key)) if bambu_credentials(&key).is_none() => {
                return Err(bambu_credential_error());
            }
            (Protocol::Flashforge | Protocol::Bambulab, _, Some(key)) => MoonrakerAuth::ApiKey {
                secret_dpapi: protect_secret(key.as_bytes())?,
            },
            (Protocol::Flashforge, _, None) => MoonrakerAuth::TrustedLan,
            (Protocol::Bambulab, _, None) => return Err(bambu_credential_error()),
            (Protocol::Moonraker, true, None) => MoonrakerAuth::TrustedLan,
            (Protocol::Moonraker, false, Some(api_key)) => MoonrakerAuth::ApiKey {
                secret_dpapi: protect_secret(api_key.as_bytes())?,
            },
            (Protocol::Moonraker, true, Some(_)) => {
                return Err(ConfigError::Validation(
                    "acceso de confianza no acepta API key".to_owned(),
                ));
            }
            (Protocol::Moonraker, false, None) => {
                return Err(ConfigError::Validation(
                    "Moonraker requiere API key o acceso de confianza explícito".to_owned(),
                ));
            }
        };
        self.printers.push(PrinterConfig {
            local_id,
            name: draft.name.trim().to_owned(),
            connection: MoonrakerConnection {
                protocol,
                host: draft.host.trim().to_owned(),
                port: draft.port,
                tls: draft.tls,
                auth,
                chamber_temperature_sensor: None,
                light_bindings: if protocol == Protocol::Moonraker {
                    draft.light_bindings
                } else {
                    MoonrakerLightBindings::default()
                },
            },
        });
        self.validate()?;
        Ok(local_id)
    }

    pub(crate) fn remove_printer(&mut self, id: LocalPrinterId) -> bool {
        let previous_length = self.printers.len();
        self.printers.retain(|printer| printer.local_id != id);
        self.printers.len() != previous_length
    }

    pub(crate) fn update_printer_metadata(
        &mut self,
        id: LocalPrinterId,
        name: String,
    ) -> Result<(), ConfigError> {
        validate_name(&name)?;
        let printer = self
            .printers
            .iter_mut()
            .find(|printer| printer.local_id == id)
            .ok_or_else(|| ConfigError::Validation("la impresora ya no existe".to_owned()))?;
        printer.name = name.trim().to_owned();
        self.validate()
    }

    pub(crate) fn update_printer(
        &mut self,
        id: LocalPrinterId,
        draft: PrinterDraft,
    ) -> Result<(), ConfigError> {
        validate_name(&draft.name)?;
        validate_host(&draft.host)?;
        if draft.port == 0 {
            return Err(ConfigError::Validation(
                "el puerto debe estar entre 1 y 65535".to_owned(),
            ));
        }

        let (existing_protocol, existing_auth) = self
            .printers
            .iter()
            .find(|printer| printer.local_id == id)
            .map(|printer| (printer.connection.protocol, printer.connection.auth.clone()))
            .ok_or_else(|| ConfigError::Validation("la impresora ya no existe".to_owned()))?;

        let protocol = draft
            .protocol
            .unwrap_or(if draft.port == 8898 || draft.port == 8899 {
                Protocol::Flashforge
            } else {
                Protocol::Moonraker
            });

        let auth = match (protocol, draft.trusted_lan, draft.api_key) {
            (Protocol::Bambulab, _, Some(key)) if bambu_credentials(&key).is_none() => {
                return Err(bambu_credential_error());
            }
            (Protocol::Flashforge | Protocol::Bambulab, _, Some(key)) if !key.trim().is_empty() => {
                validate_secret_header_value(&key)?;
                MoonrakerAuth::ApiKey {
                    secret_dpapi: protect_secret(key.as_bytes())?,
                }
            }
            (Protocol::Flashforge, _, Some(_)) => MoonrakerAuth::TrustedLan,
            (Protocol::Flashforge, _, None) => existing_auth,
            (Protocol::Bambulab, _, None)
                if existing_protocol == Protocol::Bambulab
                    && matches!(existing_auth, MoonrakerAuth::ApiKey { .. }) =>
            {
                existing_auth
            }
            (Protocol::Bambulab, _, _) => return Err(bambu_credential_error()),
            (Protocol::Moonraker, true, None) => MoonrakerAuth::TrustedLan,
            (Protocol::Moonraker, false, Some(api_key)) => {
                validate_secret_header_value(&api_key)?;
                MoonrakerAuth::ApiKey {
                    secret_dpapi: protect_secret(api_key.as_bytes())?,
                }
            }
            (Protocol::Moonraker, false, None) => match existing_auth {
                existing @ MoonrakerAuth::ApiKey { .. } => existing,
                _ => {
                    return Err(ConfigError::Validation(
                        "Moonraker requiere API key o acceso de confianza explícito".to_owned(),
                    ));
                }
            },
            (Protocol::Moonraker, true, Some(_)) => {
                return Err(ConfigError::Validation(
                    "acceso de confianza no acepta API key".to_owned(),
                ));
            }
        };

        let printer = self
            .printers
            .iter_mut()
            .find(|printer| printer.local_id == id)
            .ok_or_else(|| ConfigError::Validation("la impresora ya no existe".to_owned()))?;

        printer.name = draft.name.trim().to_owned();
        printer.connection.protocol = protocol;
        if protocol != Protocol::Moonraker {
            printer.connection.chamber_temperature_sensor = None;
        }
        printer.connection.host = draft.host.trim().to_owned();
        printer.connection.port = draft.port;
        printer.connection.tls = draft.tls;
        printer.connection.auth = auth;
        printer.connection.light_bindings = if protocol == Protocol::Moonraker {
            draft.light_bindings
        } else {
            MoonrakerLightBindings::default()
        };

        self.validate()
    }

    pub(crate) fn set_edge_token(&mut self, token: Option<&str>) -> Result<(), ConfigError> {
        self.edge_token_dpapi = token
            .map(|value| {
                let value = value.trim();
                validate_edge_token(value)?;
                protect_secret(value.as_bytes())
            })
            .transpose()?;
        Ok(())
    }

    pub(crate) fn edge_token(&self) -> Result<Option<String>, ConfigError> {
        self.edge_token_dpapi
            .as_deref()
            .map(unprotect_secret)
            .transpose()
    }

    pub(crate) fn moonraker_api_key(
        &self,
        id: LocalPrinterId,
    ) -> Result<Option<String>, ConfigError> {
        let Some(printer) = self.printers.iter().find(|printer| printer.local_id == id) else {
            return Ok(None);
        };
        match &printer.connection.auth {
            MoonrakerAuth::ApiKey { secret_dpapi } => unprotect_secret(secret_dpapi).map(Some),
            MoonrakerAuth::TrustedLan => Ok(None),
        }
    }

    pub(crate) fn validate(&self) -> Result<(), ConfigError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(ConfigError::InvalidSchema(self.schema_version));
        }
        let highest_id = self.printers.iter().map(|printer| printer.local_id.0).max();
        if highest_id.is_some_and(|id| self.next_local_id <= id) {
            return Err(ConfigError::Validation(
                "next_local_id debe superar todos los identificadores guardados".to_owned(),
            ));
        }
        for (index, printer) in self.printers.iter().enumerate() {
            validate_printer(printer)?;
            if self.printers[..index]
                .iter()
                .any(|previous| previous.local_id == printer.local_id)
            {
                return Err(ConfigError::Validation(
                    "identificador local de impresora duplicado".to_owned(),
                ));
            }
        }
        Ok(())
    }
}

pub(crate) fn known_folder_path(folder: &windows_sys::core::GUID) -> std::io::Result<PathBuf> {
    let mut pointer = std::ptr::null_mut();
    // Windows allocates a UTF-16 path for the current user; no impersonation token is used.
    let status = unsafe { SHGetKnownFolderPath(folder, 0, std::ptr::null_mut(), &mut pointer) };
    let result = if status < 0 || pointer.is_null() {
        Err(std::io::Error::other(format!(
            "SHGetKnownFolderPath falló ({status:#010x})"
        )))
    } else {
        // A successful call returns a valid, NUL-terminated buffer until CoTaskMemFree.
        let path = unsafe {
            let mut length = 0;
            while *pointer.add(length) != 0 {
                length += 1;
            }
            PathBuf::from(OsString::from_wide(std::slice::from_raw_parts(
                pointer, length,
            )))
        };
        if path.is_absolute() {
            Ok(path)
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Windows devolvió una ruta no absoluta",
            ))
        }
    };
    // The API requires freeing its buffer even on failure; freeing NULL is allowed.
    unsafe { CoTaskMemFree(pointer.cast()) };
    result
}

pub(crate) fn default_data_directory() -> Result<PathBuf, ConfigError> {
    if crate::desktop::package::is_packaged() {
        // Do not merge a Store profile with an existing EXE installation's data.
        return crate::desktop::package::data_directory()
            .map(|root| root.join(DATA_DIRECTORY_NAME))
            .map_err(ConfigError::ConfigDirectoryUnavailable);
    }
    let root = known_folder_path(&FOLDERID_LocalAppData)
        .map_err(ConfigError::ConfigDirectoryUnavailable)?;
    let legacy_root = env::var_os("LOCALAPPDATA").map(PathBuf::from);
    resolve_data_directory(&root, legacy_root.as_deref())
}

fn resolve_data_directory(root: &Path, legacy_root: Option<&Path>) -> Result<PathBuf, ConfigError> {
    let directory = root.join(DATA_DIRECTORY_NAME);
    if let Some(legacy_root) = legacy_root {
        let legacy = legacy_root.join(DATA_DIRECTORY_NAME);
        if legacy != directory {
            let mut has_profile = false;
            for name in [CONFIG_FILENAME, BACKUP_FILENAME, HISTORY_FILENAME] {
                has_profile |= legacy.join(name).try_exists()?;
            }
            if has_profile
                && !(directory.try_exists()?
                    && fs::canonicalize(&legacy)? == fs::canonicalize(&directory)?)
            {
                return Err(ConfigError::DataDirectoryConflict(legacy));
            }
        }
    }
    Ok(directory)
}

impl ConfigStore {
    pub(crate) fn history_path(&self) -> PathBuf {
        self.directory.join(HISTORY_FILENAME)
    }

    pub(crate) fn open_default() -> Result<Self, ConfigError> {
        Self::open(default_data_directory()?)
    }

    pub(crate) fn open(directory: PathBuf) -> Result<Self, ConfigError> {
        fs::create_dir_all(&directory)?;
        let lock_path = directory.join(INSTANCE_LOCK_FILENAME);
        let profile_lock = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(lock_path)?;
        profile_lock.try_lock().map_err(|error| match error {
            TryLockError::WouldBlock => ConfigError::LockBusy,
            TryLockError::Error(error) => ConfigError::Io(error),
        })?;
        Ok(Self {
            config_path: directory.join(CONFIG_FILENAME),
            backup_path: directory.join(BACKUP_FILENAME),
            directory,
            _profile_lock: profile_lock,
        })
    }

    pub(crate) fn load_or_empty(&self) -> Result<PersistedConfig, ConfigError> {
        let mut config = if self.config_path.try_exists()? {
            let bytes = read_limited(&self.config_path)?;
            serde_json::from_slice::<PersistedConfig>(&bytes)
                .map_err(|error| ConfigError::InvalidJson(error.to_string()))?
        } else {
            if self.backup_path.try_exists()? {
                return Err(ConfigError::Validation(
                    crate::i18n::t!("common.startup.incomplete_profile").into_owned(),
                ));
            }
            PersistedConfig::empty()
        };
        config.validate()?;
        if !valid_installation_id(&config.installation_id) {
            config.installation_id = generate_installation_id()?;
            self.save(&config)?;
        }
        Ok(config)
    }

    pub(crate) fn save(&self, config: &PersistedConfig) -> Result<(), ConfigError> {
        config.validate()?;
        let bytes = serde_json::to_vec_pretty(config)
            .map_err(|error| ConfigError::InvalidJson(error.to_string()))?;
        if bytes.len() > MAX_CONFIG_BYTES {
            return Err(ConfigError::SizeLimitExceeded);
        }
        let temporary_path = self.temporary_path();
        write_durable_new_file(&temporary_path, &bytes)?;
        let result = if self.config_path.exists() {
            replace_file(&self.config_path, &temporary_path, &self.backup_path)
        } else {
            fs::rename(&temporary_path, &self.config_path).map_err(ConfigError::Io)
        };
        if let Err(error) = &result {
            cleanup_failed_replacement(&temporary_path, error);
        }
        result
    }

    fn temporary_path(&self) -> PathBuf {
        let sequence = TEMPORARY_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        self.directory.join(format!(
            "{CONFIG_FILENAME}.{}.{}.tmp",
            std::process::id(),
            sequence
        ))
    }
}

fn validate_printer_draft(draft: &PrinterDraft) -> Result<(), ConfigError> {
    validate_name(&draft.name)?;
    validate_host(&draft.host)?;
    if draft.port == 0 {
        return Err(ConfigError::Validation(
            "el puerto debe estar entre 1 y 65535".to_owned(),
        ));
    }
    let protocol = draft
        .protocol
        .unwrap_or(if draft.port == 8898 || draft.port == 8899 {
            Protocol::Flashforge
        } else {
            Protocol::Moonraker
        });
    if protocol == Protocol::Moonraker {
        validate_light_bindings(&draft.light_bindings)?;
    }
    if protocol == Protocol::Moonraker {
        match (draft.trusted_lan, draft.api_key.as_deref()) {
            (true, None) => Ok(()),
            (false, Some(api_key)) => validate_secret_header_value(api_key),
            (true, Some(_)) => Err(ConfigError::Validation(
                "acceso de confianza no acepta API key".to_owned(),
            )),
            (false, None) => Err(ConfigError::Validation(
                "Moonraker requiere API key o acceso de confianza explícito".to_owned(),
            )),
        }
    } else {
        if let Some(key) = draft.api_key.as_deref() {
            validate_secret_header_value(key)?;
        }
        Ok(())
    }
}

fn validate_printer(printer: &PrinterConfig) -> Result<(), ConfigError> {
    validate_name(&printer.name)?;
    validate_host(&printer.connection.host)?;
    if printer.connection.port == 0 {
        return Err(ConfigError::Validation(
            "el puerto debe estar entre 1 y 65535".to_owned(),
        ));
    }
    if printer.connection.protocol == Protocol::Moonraker {
        validate_light_bindings(&printer.connection.light_bindings)?;
        if let Some(sensor) = &printer.connection.chamber_temperature_sensor
            && !(sensor.len() <= 128
                && !sensor.contains(['\\', '/', '?', '#'])
                && !sensor.chars().any(char::is_control)
                && ["temperature_sensor ", "heater_generic "]
                    .iter()
                    .any(|p| sensor.strip_prefix(p).is_some_and(|n| !n.trim().is_empty())))
        {
            return Err(ConfigError::Validation(
                crate::i18n::t!("printers.actions.invalid_chamber_sensor").into_owned(),
            ));
        }
    }
    match &printer.connection.auth {
        MoonrakerAuth::ApiKey { secret_dpapi } if secret_dpapi.is_empty() => Err(
            ConfigError::Validation("API key protegida vacía".to_owned()),
        ),
        MoonrakerAuth::ApiKey { .. } | MoonrakerAuth::TrustedLan => Ok(()),
    }
}

fn validate_light_bindings(bindings: &MoonrakerLightBindings) -> Result<(), ConfigError> {
    for binding in [bindings.build_area.as_ref(), bindings.toolhead.as_ref()]
        .into_iter()
        .flatten()
    {
        validate_light_binding(binding)?;
    }
    Ok(())
}

fn validate_light_binding(binding: &MoonrakerLightBinding) -> Result<(), ConfigError> {
    let name = match binding {
        MoonrakerLightBinding::OutputPin { name } | MoonrakerLightBinding::ToggleMacro { name } => {
            name
        }
    };
    if name.is_empty()
        || name.len() > 128
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return Err(ConfigError::Validation(
            "la luz Moonraker debe ser una macro o output_pin con nombre ASCII seguro".to_owned(),
        ));
    }
    Ok(())
}

fn validate_name(name: &str) -> Result<(), ConfigError> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed.len() > 128 || trimmed.chars().any(char::is_control) {
        return Err(ConfigError::Validation(
            "el nombre debe tener entre 1 y 128 caracteres visibles".to_owned(),
        ));
    }
    Ok(())
}

fn validate_host(host: &str) -> Result<(), ConfigError> {
    let host = host.trim();
    if host.is_empty()
        || host.len() > 255
        || host.contains("://")
        || host.chars().any(|character| {
            character.is_whitespace()
                || character.is_control()
                || matches!(character, '@' | '/' | '?' | '#')
        })
    {
        return Err(ConfigError::Validation(
            "host Moonraker inválido".to_owned(),
        ));
    }
    let url = Url::parse(&format!("http://{host}"))
        .map_err(|_| ConfigError::Validation("host Moonraker inválido".to_owned()))?;
    if url.host_str().is_none() || url.port().is_some() || url.path() != "/" {
        return Err(ConfigError::Validation(
            "host Moonraker inválido".to_owned(),
        ));
    }
    Ok(())
}

fn bambu_credential_error() -> ConfigError {
    ConfigError::Validation(
        crate::i18n::t!("printers.form.errors.access_code_required").into_owned(),
    )
}

fn validate_secret_header_value(value: &str) -> Result<(), ConfigError> {
    if value.is_empty()
        || value.len() > 8 * 1024
        || !value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
    {
        return Err(ConfigError::Validation(
            "la credencial debe ser ASCII visible, sin espacios ni controles".to_owned(),
        ));
    }
    Ok(())
}

fn validate_edge_token(value: &str) -> Result<(), ConfigError> {
    if value.len() == 47
        && value.starts_with("mkc_")
        && value[4..]
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        Ok(())
    } else {
        Err(ConfigError::Validation(
            "La llave debe empezar con mkc_ y tener 47 caracteres".to_owned(),
        ))
    }
}

fn valid_installation_id(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(crate) fn generate_installation_id() -> Result<String, ConfigError> {
    let mut bytes = [0_u8; 16];
    let status = unsafe {
        BCryptGenRandom(
            std::ptr::null_mut(),
            bytes.as_mut_ptr(),
            bytes.len() as u32,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    if status < 0 {
        return Err(ConfigError::RandomnessUnavailable);
    }
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn read_limited(path: &Path) -> Result<Vec<u8>, ConfigError> {
    let metadata = fs::metadata(path)?;
    if metadata.len() > MAX_CONFIG_BYTES as u64 {
        return Err(ConfigError::SizeLimitExceeded);
    }
    let mut file = File::open(path)?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.read_to_end(&mut bytes)?;
    if bytes.len() > MAX_CONFIG_BYTES {
        return Err(ConfigError::SizeLimitExceeded);
    }
    Ok(bytes)
}

fn write_durable_new_file(path: &Path, bytes: &[u8]) -> Result<(), ConfigError> {
    let mut file = OpenOptions::new().create_new(true).write(true).open(path)?;
    let result = file.write_all(bytes).and_then(|()| file.sync_all());
    drop(file);
    if result.is_err() {
        remove_owned_temporary(path);
    }
    result.map_err(ConfigError::Io)
}

fn cleanup_failed_replacement(path: &Path, error: &ConfigError) {
    // ReplaceFileW 1177 moved the old config to backup; this replacement is recovery data.
    if matches!(error, ConfigError::Io(error) if error.raw_os_error() == Some(windows_sys::Win32::Foundation::ERROR_UNABLE_TO_MOVE_REPLACEMENT_2 as i32))
    {
        return;
    }
    remove_owned_temporary(path);
}

fn remove_owned_temporary(path: &Path) {
    if let Err(error) = fs::remove_file(path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        crate::diagnostics::log(crate::diagnostics::LogEvent::new(
            crate::diagnostics::LogLevel::Warn,
            crate::diagnostics::LogArea::Config,
            "config.temporary_cleanup_failed",
        ));
    }
}

fn replace_file(destination: &Path, replacement: &Path, backup: &Path) -> Result<(), ConfigError> {
    let destination = path_to_wide(destination)?;
    let replacement = path_to_wide(replacement)?;
    let backup = path_to_wide(backup)?;
    // SAFETY: the paths are nul-terminated UTF-16 buffers that live for this call.
    let succeeded = unsafe {
        ReplaceFileW(
            destination.as_ptr(),
            replacement.as_ptr(),
            backup.as_ptr(),
            0,
            std::ptr::null::<c_void>(),
            std::ptr::null::<c_void>(),
        )
    };
    if succeeded == 0 {
        return Err(ConfigError::Io(std::io::Error::last_os_error()));
    }
    Ok(())
}

fn path_to_wide(path: &Path) -> Result<Vec<u16>, ConfigError> {
    let mut value = path.as_os_str().encode_wide().collect::<Vec<_>>();
    if value.contains(&0) {
        return Err(ConfigError::Validation(
            "ruta de configuración inválida".to_owned(),
        ));
    }
    value.push(0);
    Ok(value)
}

fn protect_secret(secret: &[u8]) -> Result<Vec<u8>, ConfigError> {
    let mut input = secret.to_vec();
    let input_blob = blob_from_mut_bytes(&mut input)?;
    let mut output_blob = CRYPT_INTEGER_BLOB::default();
    // SAFETY: input_blob points to input for the duration of the call; Windows allocates output_blob.
    let succeeded = unsafe {
        CryptProtectData(
            &input_blob,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output_blob,
        )
    };
    if succeeded == 0 {
        return Err(ConfigError::Dpapi(std::io::Error::last_os_error()));
    }
    copy_and_free_blob(output_blob)
}

fn unprotect_secret(secret_dpapi: &[u8]) -> Result<String, ConfigError> {
    let mut input = secret_dpapi.to_vec();
    let input_blob = blob_from_mut_bytes(&mut input)?;
    let mut output_blob = CRYPT_INTEGER_BLOB::default();
    // SAFETY: input_blob points to input for the duration of the call; Windows allocates output_blob.
    let succeeded = unsafe {
        CryptUnprotectData(
            &input_blob,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output_blob,
        )
    };
    if succeeded == 0 {
        return Err(ConfigError::Dpapi(std::io::Error::last_os_error()));
    }
    String::from_utf8(copy_and_free_blob(output_blob)?)
        .map_err(|_| ConfigError::InvalidSecretEncoding)
}

fn blob_from_mut_bytes(bytes: &mut [u8]) -> Result<CRYPT_INTEGER_BLOB, ConfigError> {
    let cb_data = u32::try_from(bytes.len()).map_err(|_| ConfigError::SizeLimitExceeded)?;
    Ok(CRYPT_INTEGER_BLOB {
        cbData: cb_data,
        pbData: bytes.as_mut_ptr(),
    })
}

fn copy_and_free_blob(blob: CRYPT_INTEGER_BLOB) -> Result<Vec<u8>, ConfigError> {
    if blob.pbData.is_null() {
        return Err(ConfigError::Dpapi(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "DPAPI devolvió un buffer nulo",
        )));
    }
    let bytes = unsafe { std::slice::from_raw_parts(blob.pbData, blob.cbData as usize).to_vec() };
    // SAFETY: DPAPI allocates pbData with LocalAlloc and requires LocalFree.
    unsafe {
        LocalFree(blob.pbData.cast());
    }
    Ok(bytes)
}
