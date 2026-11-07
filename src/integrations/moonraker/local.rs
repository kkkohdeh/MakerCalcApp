use std::fmt;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use tokio::sync::{mpsc, watch};

pub(crate) const PRINTER_ACTION_CAPACITY: usize = 4;

#[derive(Clone)]
pub(crate) struct OperationHandle {
    pub(crate) actions: mpsc::Sender<PrinterAction>,
    pub(crate) state: watch::Receiver<OperationState>,
}

pub(crate) struct OperationIo {
    pub(crate) actions: mpsc::Receiver<PrinterAction>,
    pub(crate) state: watch::Sender<OperationState>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PrinterActionKind {
    StartPrint,
    PausePrint,
    ResumePrint,
    CancelPrint,
    SetTemperature,
    SetLight,
    UploadGcode,
    MachineControl,
}

pub(crate) use crate::shared::workspace::{HeaterKind, PrintControlContext};

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PrinterAction {
    MachineControl {
        control: Box<super::adapter::control::PreparedControl>,
    },
    StartPrint {
        filename: String,
    },
    PausePrint(PrintControlContext),
    ResumePrint(PrintControlContext),
    CancelPrint(PrintControlContext),
    SetTemperatureTarget {
        heater: HeaterKind,
        target_c: u16,
    },
    SetLightOutput {
        name: String,
        enabled: bool,
    },
    RunLightMacro {
        name: String,
    },
    /// The source path is local only; never serialize or log it.
    UploadGcode {
        source: PathBuf,
        destination: String,
    },
}

impl PrinterAction {
    pub(crate) fn kind(&self) -> PrinterActionKind {
        match self {
            Self::StartPrint { .. } => PrinterActionKind::StartPrint,
            Self::PausePrint(..) => PrinterActionKind::PausePrint,
            Self::ResumePrint(..) => PrinterActionKind::ResumePrint,
            Self::CancelPrint(..) => PrinterActionKind::CancelPrint,
            Self::SetTemperatureTarget { .. } => PrinterActionKind::SetTemperature,
            Self::SetLightOutput { .. } | Self::RunLightMacro { .. } => PrinterActionKind::SetLight,
            Self::UploadGcode { .. } => PrinterActionKind::UploadGcode,
            Self::MachineControl { .. } => PrinterActionKind::MachineControl,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum OperationState {
    Idle,
    Sending(PrinterActionKind),
    Accepted(PrinterActionKind),
    Rejected {
        action: PrinterActionKind,
        message: String,
    },
    Unknown(PrinterActionKind),
}

impl OperationState {
    pub(crate) fn is_pending(&self) -> bool {
        matches!(self, Self::Sending(_))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ControlError {
    Filename,
    Temperature,
    Light,
    Upload,
    Machine,
}

impl fmt::Display for ControlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Filename => formatter.write_str("archivo G-code inválido"),
            Self::Temperature => formatter.write_str("temperatura fuera del rango permitido"),
            Self::Light => formatter.write_str("esa luz no está disponible en esta impresora"),
            Self::Upload => formatter.write_str("archivo local para subir inválido"),
            Self::Machine => {
                formatter.write_str(&crate::i18n::t!("printers.actions.invalid_machine_control"))
            }
        }
    }
}

impl std::error::Error for ControlError {}

pub(crate) fn make_operation_channels() -> (OperationHandle, OperationIo) {
    let (action_sender, action_receiver) = mpsc::channel(PRINTER_ACTION_CAPACITY);
    let (state_sender, state_receiver) = watch::channel(OperationState::Idle);
    (
        OperationHandle {
            actions: action_sender,
            state: state_receiver,
        },
        OperationIo {
            actions: action_receiver,
            state: state_sender,
        },
    )
}

pub(crate) fn build_control_request(
    id: u64,
    action: &PrinterAction,
) -> Result<Value, ControlError> {
    let (method, params) = match action {
        PrinterAction::StartPrint { filename } => {
            if !valid_gcode_filename(filename) {
                return Err(ControlError::Filename);
            }
            ("printer.print.start", Some(json!({"filename": filename})))
        }
        PrinterAction::PausePrint(..) => ("printer.print.pause", None),
        PrinterAction::ResumePrint(..) => ("printer.print.resume", None),
        PrinterAction::CancelPrint(..) => ("printer.print.cancel", None),
        PrinterAction::SetTemperatureTarget { heater, target_c } => {
            let heater_name = match heater {
                HeaterKind::Nozzle => "extruder",
                HeaterKind::Bed => "heater_bed",
            };
            if *target_c > heater.max_target_c() {
                return Err(ControlError::Temperature);
            }
            (
                "printer.gcode.script",
                Some(json!({
                    "script": format!(
                        "SET_HEATER_TEMPERATURE HEATER={heater_name} TARGET={target_c}"
                    )
                })),
            )
        }
        PrinterAction::SetLightOutput { name, enabled } => {
            if !valid_gcode_identifier(name) {
                return Err(ControlError::Light);
            }
            let script = format!("SET_PIN PIN={name} VALUE={}", if *enabled { 1 } else { 0 });
            ("printer.gcode.script", Some(json!({ "script": script })))
        }
        PrinterAction::RunLightMacro { name } => {
            if !valid_gcode_identifier(name) {
                return Err(ControlError::Light);
            }
            ("printer.gcode.script", Some(json!({ "script": name })))
        }
        PrinterAction::UploadGcode { .. } => return Err(ControlError::Upload),
        PrinterAction::MachineControl { .. } => return Err(ControlError::Machine),
    };
    let mut request = json!({"jsonrpc":"2.0", "id":id, "method":method});
    if let Some(params) = params {
        request["params"] = params;
    }
    Ok(request)
}

pub(crate) fn validate_upload(source: &Path, destination: &str) -> Result<u64, ControlError> {
    if !valid_gcode_filename(destination) {
        return Err(ControlError::Upload);
    }
    let metadata = std::fs::metadata(source).map_err(|_| ControlError::Upload)?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > 256 * 1024 * 1024 {
        return Err(ControlError::Upload);
    }
    Ok(metadata.len())
}

pub(crate) fn valid_gcode_filename(filename: &str) -> bool {
    let lower = filename.to_ascii_lowercase();
    if !(lower.ends_with(".gcode") || lower.ends_with(".g"))
        || filename.is_empty()
        || filename.len() > 512
        || filename.starts_with('/')
        || filename.contains('\\')
        || filename.chars().any(char::is_control)
    {
        return false;
    }
    filename
        .split('/')
        .all(|segment| !segment.is_empty() && !matches!(segment, "." | ".."))
}

fn valid_gcode_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}
