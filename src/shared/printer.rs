//! MakerCalc's provider-independent printer language. No RPC names or source paths.

use std::net::IpAddr;

use serde::{Deserialize, Serialize};

use crate::config::{MoonrakerLightBindings, Protocol};
use crate::shared::telemetry::{
    BedTelemetry, FilamentSensorTelemetry, FilamentSupplyTelemetry, LightTelemetry,
    NozzleTelemetry, PartCoolingFanTelemetry, PrinterAlert,
};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PrintStatus {
    #[default]
    Unknown,
    Idle,
    Printing,
    Paused,
    Completed,
    Cancelled,
    Error,
}

/// Full observation after the adapter has merged source deltas and converted units.
/// The state boundary validates and sanitizes it before publishing.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub(crate) struct PrinterObservation {
    #[serde(default)]
    pub(crate) status: Option<PrintStatus>,
    pub(crate) progress_pct: Option<f64>,
    pub(crate) filename: Option<String>,
    pub(crate) chamber_c: Option<f64>,
    pub(crate) layer_current: Option<u32>,
    pub(crate) layer_total: Option<u32>,
    pub(crate) elapsed_s: Option<f64>,
    pub(crate) remaining_s: Option<f64>,
    pub(crate) filament_used_mm: Option<f64>,
    pub(crate) filament_used_g: Option<f64>,
    pub(crate) velocity_mm_s: Option<f64>,
    pub(crate) speed_override_pct: Option<f64>,
    #[serde(default)]
    pub(crate) print_thumbnail_sha256: Option<String>,
    #[serde(default)]
    pub(crate) alerts: Option<Vec<PrinterAlert>>,
    #[serde(default)]
    pub(crate) nozzles: Vec<NozzleTelemetry>,
    #[serde(default)]
    pub(crate) beds: Vec<BedTelemetry>,
    #[serde(default)]
    pub(crate) part_cooling_fans: Vec<PartCoolingFanTelemetry>,
    #[serde(default)]
    pub(crate) filament_sensors: Vec<FilamentSensorTelemetry>,
    #[serde(default)]
    pub(crate) filament_supplies: Option<Vec<FilamentSupplyTelemetry>>,
    /// Local normalized light roles exposed by this printer integration.
    #[serde(default)]
    pub(crate) lights: Vec<LightTelemetry>,
    /// Local material station evidence; excluded from the Cloud contract.
    #[serde(default)]
    pub(crate) material_station_available: Option<bool>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CredentialStatus {
    #[default]
    None,
    Configured,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EditableConnectionView {
    pub(crate) protocol: Protocol,
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) tls: bool,
    pub(crate) trusted_lan: bool,
    pub(crate) credential_status: CredentialStatus,
    pub(crate) light_bindings: MoonrakerLightBindings,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct CloudDeviceMetadata {
    pub(crate) serial_suffix: Option<String>,
    pub(crate) local_ip: Option<IpAddr>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct PrinterLocalMetadata {
    pub(crate) serial_suffix: Option<String>,
    pub(crate) local_ip: Option<IpAddr>,
}
