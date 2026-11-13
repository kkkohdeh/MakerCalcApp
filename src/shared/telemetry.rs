//! Provider-independent 21-concept normalized telemetry model for MakerCalc.

use serde::{Deserialize, Serialize};

use crate::shared::printer::{PrintStatus, PrinterObservation};

#[cfg(test)]
pub(crate) const DICTIONARY_VERSION: &str = "21-v1";
pub(crate) const MAX_LIGHTS: usize = 8;

/// Element identifier scoped to a local session.
#[derive(
    Clone, Copy, Debug, Default, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize,
)]
#[serde(transparent)]
pub(crate) struct ElementId(pub(crate) u32);

impl std::fmt::Display for ElementId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

/// Structured printer or job alert with a common category and bounded message.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct PrinterAlert {
    #[serde(rename = "category")]
    pub(crate) category: String,
    #[serde(rename = "message")]
    pub(crate) message: String,
}

impl PrinterAlert {
    pub(crate) fn new(category: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            category: category.into(),
            message: message.into(),
        }
    }
}

/// Nozzle / toolhead element.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub(crate) struct NozzleTelemetry {
    #[serde(rename = "id")]
    pub(crate) id: ElementId,
    #[serde(rename = "is_active")]
    pub(crate) is_active: bool,
    #[serde(rename = "PrinterNozzleTempC")]
    pub(crate) nozzle_temp_c: Option<f64>,
    #[serde(rename = "PrinterNozzleTargetTempC")]
    pub(crate) nozzle_target_temp_c: Option<f64>,
}

/// Heated bed element.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub(crate) struct BedTelemetry {
    #[serde(rename = "id")]
    pub(crate) id: ElementId,
    #[serde(rename = "PrinterBedTempC")]
    pub(crate) bed_temp_c: Option<f64>,
    #[serde(rename = "PrinterBedTargetTempC")]
    pub(crate) bed_target_temp_c: Option<f64>,
}

/// Part cooling fan element.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub(crate) struct PartCoolingFanTelemetry {
    #[serde(rename = "id")]
    pub(crate) id: ElementId,
    #[serde(rename = "PartCoolingFanPowerPct")]
    pub(crate) power_pct: Option<f64>,
    #[serde(rename = "PartCoolingFanRpm")]
    pub(crate) rpm: Option<f64>,
}

/// Semantic role of a printer light. Providers map their private names here.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LightRole {
    BuildArea,
    Toolhead,
}

/// Locally controllable printer light.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct LightTelemetry {
    pub(crate) id: ElementId,
    pub(crate) role: LightRole,
    #[serde(rename = "LightEnabled")]
    pub(crate) enabled: Option<bool>,
}

/// Filament presence or motion sensor element.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(crate) struct FilamentSensorTelemetry {
    #[serde(rename = "id")]
    pub(crate) id: ElementId,
    #[serde(rename = "FilamentPresent")]
    pub(crate) filament_present: Option<bool>,
    #[serde(default, skip_serializing)]
    pub(crate) name: Option<String>,
}

impl PartialEq for FilamentSensorTelemetry {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id && self.filament_present == other.filament_present
    }
}
impl Eq for FilamentSensorTelemetry {}

impl FilamentSensorTelemetry {
    pub(crate) fn new(id: ElementId, filament_present: Option<bool>) -> Self {
        Self {
            id,
            filament_present,
            name: None,
        }
    }
}

/// Operational state of a loaded filament supply in a material station.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SupplyState {
    #[default]
    Unknown,
    Empty,
    Available,
    Active,
    Loading,
    Unloading,
}

/// Loaded filament supply element.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub(crate) struct FilamentSupplyTelemetry {
    #[serde(rename = "id")]
    pub(crate) id: ElementId,
    #[serde(rename = "FilamentPresent")]
    pub(crate) filament_present: Option<bool>,
    #[serde(rename = "FilamentMaterial")]
    pub(crate) material: Option<String>,
    #[serde(rename = "FilamentColor")]
    pub(crate) color: Option<String>,
    #[serde(rename = "is_active")]
    pub(crate) is_active: Option<bool>,
    /// Operational state in material station; excluded from the 21-v1 cloud envelope.
    #[serde(default, skip_serializing)]
    pub(crate) state: SupplyState,
}

/// Job telemetry representing active or last observed terminal print.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub(crate) struct JobTelemetry {
    #[serde(rename = "PrintName")]
    pub(crate) print_name: Option<String>,
    #[serde(rename = "PrintState")]
    pub(crate) print_state: PrintStatus,
    #[serde(rename = "PrintProgressPct")]
    pub(crate) print_progress_pct: Option<f64>,
    #[serde(rename = "PrintElapsedTimeS")]
    pub(crate) print_elapsed_time_s: Option<f64>,
    #[serde(rename = "PrintRemainingTimeS")]
    pub(crate) print_remaining_time_s: Option<f64>,
    #[serde(rename = "PrintCurrentLayer")]
    pub(crate) print_current_layer: Option<u32>,
    #[serde(rename = "PrintTotalLayers")]
    pub(crate) print_total_layers: Option<u32>,
    #[serde(rename = "PrintFilamentUsedMm")]
    pub(crate) print_filament_used_mm: Option<f64>,
    #[serde(rename = "PrintFilamentUsedG")]
    pub(crate) print_filament_used_g: Option<f64>,
    #[serde(rename = "PrintSpeedOverridePct")]
    pub(crate) print_speed_override_pct: Option<f64>,
    #[serde(rename = "PrintThumbnailSha256", default)]
    pub(crate) print_thumbnail_sha256: Option<String>,
}

/// Complete normalized dictionary of the 21 MakerCalc concepts for a printer.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct PrinterTelemetry {
    #[serde(rename = "PrintState")]
    pub(crate) print_state: PrintStatus,
    #[serde(rename = "PrinterAlerts")]
    pub(crate) printer_alerts: Option<Vec<PrinterAlert>>,
    #[serde(rename = "PrinterChamberTempC")]
    pub(crate) chamber_temp_c: Option<f64>,
    #[serde(rename = "nozzles")]
    pub(crate) nozzles: Vec<NozzleTelemetry>,
    #[serde(rename = "beds")]
    pub(crate) beds: Vec<BedTelemetry>,
    #[serde(rename = "part_cooling_fans")]
    pub(crate) part_cooling_fans: Vec<PartCoolingFanTelemetry>,
    #[serde(rename = "filament_sensors")]
    pub(crate) filament_sensors: Vec<FilamentSensorTelemetry>,
    #[serde(rename = "filament_supplies")]
    pub(crate) filament_supplies: Option<Vec<FilamentSupplyTelemetry>>,
    /// Controllable roles with known or unknown state; an empty list means unsupported.
    #[serde(default)]
    pub(crate) lights: Vec<LightTelemetry>,
    /// Instant toolhead speed, distinct from the job's percentage override.
    #[serde(rename = "PrinterHeadVelocityMmS")]
    pub(crate) velocity_mm_s: Option<f64>,
    #[serde(rename = "current_job")]
    pub(crate) current_job: Option<JobTelemetry>,
    #[serde(rename = "last_job")]
    pub(crate) last_job: Option<JobTelemetry>,
}

impl Default for PrinterTelemetry {
    fn default() -> Self {
        Self {
            print_state: PrintStatus::Unknown,
            printer_alerts: None,
            chamber_temp_c: None,
            nozzles: Vec::new(),
            beds: Vec::new(),
            part_cooling_fans: Vec::new(),
            filament_sensors: Vec::new(),
            filament_supplies: None,
            lights: Vec::new(),
            velocity_mm_s: None,
            current_job: None,
            last_job: None,
        }
    }
}

/// Top-level envelope for normalized export / fixtures.
#[cfg(test)]
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct TelemetryEnvelope {
    #[serde(rename = "dictionary_version")]
    pub(crate) dictionary_version: String,
    #[serde(rename = "session_generation")]
    pub(crate) session_generation: u64,
    #[serde(rename = "printer_id")]
    pub(crate) printer_id: String,
    #[serde(rename = "age_ms")]
    pub(crate) age_ms: Option<u64>,
    #[serde(rename = "telemetry")]
    pub(crate) telemetry: PrinterTelemetry,
}

#[allow(clippy::enum_variant_names)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum TelemetryValidationError {
    InvalidProgress,
    #[cfg(test)]
    InvalidTemperature,
    InvalidLayers,
    InvalidDuration,
    InvalidFilament,
    #[cfg(test)]
    InvalidFan,
    #[cfg(test)]
    InvalidVelocity,
    InvalidSpeedOverride,
    InvalidThumbnailHash,
    #[cfg(test)]
    InvalidColor,
    #[cfg(test)]
    InvalidAlert,
    #[cfg(test)]
    InvalidLights,
}

impl std::fmt::Display for TelemetryValidationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::InvalidProgress => "progreso fuera del rango 0–100",
            #[cfg(test)]
            Self::InvalidTemperature => "temperatura inválida",
            Self::InvalidLayers => "capas inválidas",
            Self::InvalidDuration => "duración inválida",
            Self::InvalidFilament => "filamento inválido",
            #[cfg(test)]
            Self::InvalidFan => "ventilador de refrigeración inválido",
            #[cfg(test)]
            Self::InvalidVelocity => "velocidad del cabezal inválida",
            Self::InvalidSpeedOverride => "ajuste de velocidad inválido",
            Self::InvalidThumbnailHash => "hash de miniatura inválido",
            #[cfg(test)]
            Self::InvalidColor => "color de filamento inválido (debe ser #RRGGBB)",
            #[cfg(test)]
            Self::InvalidAlert => "alerta inválida",
            #[cfg(test)]
            Self::InvalidLights => "demasiadas luces",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for TelemetryValidationError {}

impl PrinterTelemetry {
    /// Validates all 21 concepts against domain constraints.
    #[cfg(test)]
    pub(crate) fn validate(&self) -> Result<(), TelemetryValidationError> {
        if self.lights.len() > MAX_LIGHTS {
            return Err(TelemetryValidationError::InvalidLights);
        }
        validate_finite(self.chamber_temp_c)
            .map_err(|()| TelemetryValidationError::InvalidTemperature)?;
        validate_finite_range(self.velocity_mm_s, 0.0, f64::INFINITY)
            .map_err(|()| TelemetryValidationError::InvalidVelocity)?;

        if let Some(alerts) = &self.printer_alerts {
            if alerts.len() > 32 {
                return Err(TelemetryValidationError::InvalidAlert);
            }
            for alert in alerts {
                if alert.category.is_empty() || alert.message.len() > 512 {
                    return Err(TelemetryValidationError::InvalidAlert);
                }
            }
        }

        for nozzle in &self.nozzles {
            validate_finite(nozzle.nozzle_temp_c)
                .map_err(|()| TelemetryValidationError::InvalidTemperature)?;
            validate_finite(nozzle.nozzle_target_temp_c)
                .map_err(|()| TelemetryValidationError::InvalidTemperature)?;
        }

        for bed in &self.beds {
            validate_finite(bed.bed_temp_c)
                .map_err(|()| TelemetryValidationError::InvalidTemperature)?;
            validate_finite(bed.bed_target_temp_c)
                .map_err(|()| TelemetryValidationError::InvalidTemperature)?;
        }

        for fan in &self.part_cooling_fans {
            validate_finite_range(fan.power_pct, 0.0, 100.0)
                .map_err(|()| TelemetryValidationError::InvalidFan)?;
            validate_finite_range(fan.rpm, 0.0, f64::INFINITY)
                .map_err(|()| TelemetryValidationError::InvalidFan)?;
        }

        if let Some(supplies) = &self.filament_supplies {
            for supply in supplies {
                if supply
                    .color
                    .as_deref()
                    .is_some_and(|c| !validate_hex_color(c))
                {
                    return Err(TelemetryValidationError::InvalidColor);
                }
            }
        }

        if let Some(job) = &self.current_job {
            job.validate()?;
        }
        if let Some(job) = &self.last_job {
            job.validate()?;
        }

        Ok(())
    }

    pub(crate) fn active_nozzle(&self) -> Option<&NozzleTelemetry> {
        self.nozzles.iter().find(|nozzle| nozzle.is_active)
    }

    pub(crate) fn active_nozzle_temp_c(&self) -> Option<f64> {
        self.active_nozzle().and_then(|n| n.nozzle_temp_c)
    }

    pub(crate) fn active_nozzle_target_temp_c(&self) -> Option<f64> {
        self.active_nozzle().and_then(|n| n.nozzle_target_temp_c)
    }

    pub(crate) fn primary_bed(&self) -> Option<&BedTelemetry> {
        self.beds.first()
    }

    pub(crate) fn primary_bed_temp_c(&self) -> Option<f64> {
        self.primary_bed().and_then(|b| b.bed_temp_c)
    }

    pub(crate) fn primary_bed_target_temp_c(&self) -> Option<f64> {
        self.primary_bed().and_then(|b| b.bed_target_temp_c)
    }

    pub(crate) fn primary_fan(&self) -> Option<&PartCoolingFanTelemetry> {
        self.part_cooling_fans.first()
    }

    pub(crate) fn primary_fan_power_pct(&self) -> Option<f64> {
        self.primary_fan().and_then(|f| f.power_pct)
    }

    #[cfg(test)]
    pub(crate) fn primary_fan_rpm(&self) -> Option<f64> {
        self.primary_fan().and_then(|f| f.rpm)
    }

    #[cfg(test)]
    pub(crate) fn any_filament_present(&self) -> Option<bool> {
        let mut any_reported = false;
        for sensor in &self.filament_sensors {
            if let Some(present) = sensor.filament_present {
                if present {
                    return Some(true);
                }
                any_reported = true;
            }
        }
        if any_reported { Some(false) } else { None }
    }

    /// Applies a validated observation to the 21 concepts model.
    pub(crate) fn observe(&mut self, observation: &PrinterObservation) {
        let previous_status = self.print_state;
        self.print_state = observation.status.unwrap_or(PrintStatus::Unknown);
        self.chamber_temp_c = observation.chamber_c;

        self.nozzles = observation.nozzles.clone();
        self.beds = observation.beds.clone();
        self.part_cooling_fans = observation.part_cooling_fans.clone();
        self.filament_sensors = observation.filament_sensors.clone();

        self.filament_supplies = observation.filament_supplies.clone();
        self.lights = observation.lights.clone();
        self.velocity_mm_s = observation.velocity_mm_s;

        match self.print_state {
            PrintStatus::Printing | PrintStatus::Paused => {
                let job = JobTelemetry::from_observation(observation, self.print_state);
                self.current_job = Some(job);
                // Starting a new print clears prior terminal alerts
                if previous_status != PrintStatus::Printing
                    && previous_status != PrintStatus::Paused
                {
                    self.printer_alerts = observation.alerts.clone();
                } else if let Some(alerts) = &observation.alerts {
                    self.printer_alerts = Some(alerts.clone());
                }
            }
            PrintStatus::Completed | PrintStatus::Cancelled | PrintStatus::Error => {
                let previous = self.current_job.take();
                let had_current = previous.is_some();
                let has_job = observation
                    .filename
                    .as_ref()
                    .is_some_and(|name| !name.is_empty())
                    || observation.elapsed_s.is_some_and(|seconds| seconds > 0.0)
                    || had_current;

                if has_job && (had_current || previous_status != self.print_state) {
                    self.last_job = Some(merge_job_observation(
                        &previous.unwrap_or_default(),
                        observation,
                        self.print_state,
                    ));
                }
                if let Some(alerts) = &observation.alerts {
                    self.printer_alerts = Some(alerts.clone());
                }
            }
            PrintStatus::Idle => {
                self.current_job = None;
                if let Some(alerts) = &observation.alerts {
                    self.printer_alerts = Some(alerts.clone());
                }
            }
            PrintStatus::Unknown => {
                self.printer_alerts = observation.alerts.clone();
            }
        }
    }
}

/// A terminal reset must not erase the last known counters of that execution.
pub(crate) fn merge_job_observation(
    previous: &JobTelemetry,
    observation: &PrinterObservation,
    status: PrintStatus,
) -> JobTelemetry {
    let mut job = JobTelemetry::from_observation(observation, status);
    job.print_name = job.print_name.or_else(|| previous.print_name.clone());
    job.print_total_layers = job.print_total_layers.or(previous.print_total_layers);
    job.print_remaining_time_s = job
        .print_remaining_time_s
        .or(previous.print_remaining_time_s);
    job.print_speed_override_pct = job
        .print_speed_override_pct
        .or(previous.print_speed_override_pct);
    let terminal = matches!(
        status,
        PrintStatus::Completed | PrintStatus::Cancelled | PrintStatus::Error
    );
    fn counter(next: Option<f64>, previous: Option<f64>, terminal: bool) -> Option<f64> {
        if terminal && next == Some(0.0) && previous.is_some_and(|value| value > 0.0) {
            previous
        } else {
            next.or(previous)
        }
    }
    job.print_progress_pct = counter(
        job.print_progress_pct,
        previous.print_progress_pct,
        terminal,
    );
    job.print_elapsed_time_s = counter(
        job.print_elapsed_time_s,
        previous.print_elapsed_time_s,
        terminal,
    );
    job.print_filament_used_mm = counter(
        job.print_filament_used_mm,
        previous.print_filament_used_mm,
        terminal,
    );
    job.print_filament_used_g = counter(
        job.print_filament_used_g,
        previous.print_filament_used_g,
        terminal,
    );
    job.print_current_layer = if terminal
        && job.print_current_layer == Some(0)
        && previous.print_current_layer.is_some_and(|value| value > 0)
    {
        previous.print_current_layer
    } else {
        job.print_current_layer.or(previous.print_current_layer)
    };
    job
}

impl JobTelemetry {
    pub(crate) fn validate(&self) -> Result<(), TelemetryValidationError> {
        validate_finite_range(self.print_progress_pct, 0.0, 100.0)
            .map_err(|()| TelemetryValidationError::InvalidProgress)?;
        if self
            .print_current_layer
            .zip(self.print_total_layers)
            .is_some_and(|(current, total)| current > total)
        {
            return Err(TelemetryValidationError::InvalidLayers);
        }
        for duration in [self.print_elapsed_time_s, self.print_remaining_time_s] {
            validate_finite_range(duration, 0.0, f64::INFINITY)
                .map_err(|()| TelemetryValidationError::InvalidDuration)?;
        }
        for filament in [self.print_filament_used_mm, self.print_filament_used_g] {
            validate_finite_range(filament, 0.0, f64::INFINITY)
                .map_err(|()| TelemetryValidationError::InvalidFilament)?;
        }
        validate_finite_range(self.print_speed_override_pct, 0.0, f64::INFINITY)
            .map_err(|()| TelemetryValidationError::InvalidSpeedOverride)?;
        if self
            .print_thumbnail_sha256
            .as_deref()
            .is_some_and(|hash| !thumbnail_sha256_is_valid(hash))
        {
            return Err(TelemetryValidationError::InvalidThumbnailHash);
        }
        Ok(())
    }

    fn from_observation(observation: &PrinterObservation, status: PrintStatus) -> Self {
        Self {
            print_name: observation.filename.clone().filter(|name| !name.is_empty()),
            print_state: status,
            print_progress_pct: observation.progress_pct,
            print_elapsed_time_s: observation.elapsed_s,
            print_remaining_time_s: observation.remaining_s,
            print_current_layer: observation.layer_current,
            print_total_layers: observation.layer_total,
            print_filament_used_mm: observation.filament_used_mm,
            print_filament_used_g: observation.filament_used_g,
            print_speed_override_pct: observation.speed_override_pct,
            print_thumbnail_sha256: observation.print_thumbnail_sha256.clone(),
        }
    }
}

pub(crate) fn thumbnail_sha256_is_valid(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
fn validate_finite(value: Option<f64>) -> Result<(), ()> {
    if value.is_some_and(|number| !number.is_finite()) {
        return Err(());
    }
    Ok(())
}

fn validate_finite_range(value: Option<f64>, minimum: f64, maximum: f64) -> Result<(), ()> {
    if value.is_some_and(|number| !number.is_finite() || number < minimum || number > maximum) {
        return Err(());
    }
    Ok(())
}

pub(crate) fn validate_hex_color(color: &str) -> bool {
    let bytes = color.as_bytes();
    if bytes.len() != 7 || bytes[0] != b'#' {
        return false;
    }
    bytes[1..].iter().all(|byte| byte.is_ascii_hexdigit())
}
