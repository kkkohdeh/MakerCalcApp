use std::fmt;
use std::time::Instant;

use serde::{Deserialize, Serialize};

pub(crate) use crate::shared::printer::{PrintStatus, PrinterObservation as Snapshot};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub(crate) struct LocalPrinterId(pub(crate) u64);

impl fmt::Display for LocalPrinterId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LanConnection {
    Connecting,
    Connected,
    Disconnected,
    NeedsCredentials,
    Incompatible,
}

#[derive(Clone, Debug)]
pub(crate) struct PrinterState {
    pub(crate) notifications: Option<crate::shared::notifications::NoticeCapture>,
    pub(crate) history_capture: Option<crate::history::Capture>,
    pub(crate) native_history: Option<crate::history::NativeHistory>,
    pub(crate) id: LocalPrinterId,
    pub(crate) connection: LanConnection,
    pub(crate) last_error: Option<String>,
    pub(crate) last_valid_at: Option<Instant>,
    pub(crate) revision: u64,
    pub(crate) job_generation: u64,
    pub(crate) connection_epoch: u64,
    pub(crate) controls: crate::shared::workspace::PrinterControls,
    pub(crate) snapshot: Option<Snapshot>,
    /// True only after a validated snapshot in the current LAN session.
    pub(crate) telemetry_ready: bool,
    pub(crate) telemetry: crate::shared::telemetry::PrinterTelemetry,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MachineJobTarget {
    Active(u64),
    Inactive(u64),
    Plate {
        generation: u64,
        status: PrintStatus,
        filename: Option<String>,
    },
}

impl PrinterState {
    pub(crate) fn machine_job_target(&self) -> MachineJobTarget {
        match self.telemetry.print_state {
            PrintStatus::Printing | PrintStatus::Paused => {
                MachineJobTarget::Active(self.job_generation)
            }
            _ => MachineJobTarget::Inactive(self.job_generation),
        }
    }

    fn plate_job_filename(&self) -> Option<&str> {
        self.snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.filename.as_deref())
            .or_else(|| self.telemetry.last_job.as_ref()?.print_name.as_deref())
    }

    pub(crate) fn plate_job_target(&self) -> MachineJobTarget {
        MachineJobTarget::Plate {
            generation: self.job_generation,
            status: self.telemetry.print_state,
            filename: self.plate_job_filename().map(str::to_owned),
        }
    }

    pub(crate) fn matches_machine_job_target(&self, target: &MachineJobTarget) -> bool {
        match target {
            MachineJobTarget::Plate {
                generation,
                status,
                filename,
            } => {
                self.job_generation == *generation
                    && self.telemetry.print_state == *status
                    && self.plate_job_filename() == filename.as_deref()
            }
            _ => &self.machine_job_target() == target,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ValidationError {
    InvalidProgress,
    InvalidTemperature,
    InvalidLayers,
    InvalidDuration,
    InvalidFilament,
    InvalidVelocity,
    InvalidFan,
    InvalidSpeedOverride,
    InvalidThumbnailHash,
    InvalidAlert,
    InvalidLights,
    RevisionExhausted,
}

impl fmt::Display for ValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidProgress => "progreso fuera del rango 0–100",
            Self::InvalidTemperature => "temperatura inválida",
            Self::InvalidLayers => "capas inválidas",
            Self::InvalidDuration => "duración inválida",
            Self::InvalidFilament => "filamento inválido",
            Self::InvalidVelocity => "velocidad inválida",
            Self::InvalidFan => "ventilador de refrigeración inválido",
            Self::InvalidSpeedOverride => "ajuste de velocidad inválido",
            Self::InvalidThumbnailHash => "hash de miniatura inválido",
            Self::InvalidAlert => "alerta inválida",
            Self::InvalidLights => "demasiadas luces",
            Self::RevisionExhausted => "contador de revisiones agotado",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for ValidationError {}

impl Snapshot {
    pub(crate) fn validate(&self) -> Result<(), ValidationError> {
        if self.lights.len() > crate::shared::telemetry::MAX_LIGHTS {
            return Err(ValidationError::InvalidLights);
        }
        validate_finite_range(self.progress_pct, 0.0, 100.0)
            .map_err(|()| ValidationError::InvalidProgress)?;
        validate_finite(self.chamber_c).map_err(|()| ValidationError::InvalidTemperature)?;
        if self
            .layer_current
            .zip(self.layer_total)
            .is_some_and(|(current, total)| current > total)
        {
            return Err(ValidationError::InvalidLayers);
        }
        for duration in [self.elapsed_s, self.remaining_s] {
            validate_finite_range(duration, 0.0, f64::INFINITY)
                .map_err(|()| ValidationError::InvalidDuration)?;
        }
        validate_finite_range(self.filament_used_mm, 0.0, f64::INFINITY)
            .map_err(|()| ValidationError::InvalidFilament)?;
        validate_finite_range(self.filament_used_g, 0.0, f64::INFINITY)
            .map_err(|()| ValidationError::InvalidFilament)?;
        validate_finite_range(self.velocity_mm_s, 0.0, f64::INFINITY)
            .map_err(|()| ValidationError::InvalidVelocity)?;
        validate_finite_range(self.speed_override_pct, 0.0, f64::INFINITY)
            .map_err(|()| ValidationError::InvalidSpeedOverride)?;
        if self
            .print_thumbnail_sha256
            .as_deref()
            .is_some_and(|hash| !crate::shared::telemetry::thumbnail_sha256_is_valid(hash))
        {
            return Err(ValidationError::InvalidThumbnailHash);
        }
        for nozzle in &self.nozzles {
            validate_finite(nozzle.nozzle_temp_c)
                .map_err(|()| ValidationError::InvalidTemperature)?;
            validate_finite(nozzle.nozzle_target_temp_c)
                .map_err(|()| ValidationError::InvalidTemperature)?;
        }
        for bed in &self.beds {
            validate_finite(bed.bed_temp_c).map_err(|()| ValidationError::InvalidTemperature)?;
            validate_finite(bed.bed_target_temp_c)
                .map_err(|()| ValidationError::InvalidTemperature)?;
        }
        for fan in &self.part_cooling_fans {
            validate_finite_range(fan.power_pct, 0.0, 100.0)
                .map_err(|()| ValidationError::InvalidFan)?;
            validate_finite_range(fan.rpm, 0.0, f64::INFINITY)
                .map_err(|()| ValidationError::InvalidFan)?;
        }
        if let Some(alerts) = &self.alerts {
            if alerts.len() > 32 {
                return Err(ValidationError::InvalidAlert);
            }
            for alert in alerts {
                if alert.category.is_empty() {
                    return Err(ValidationError::InvalidAlert);
                }
            }
        }
        Ok(())
    }

    pub(crate) fn sanitized(mut self) -> Result<Self, ValidationError> {
        self.validate()?;
        self.filename = self
            .filename
            .as_deref()
            .map(sanitize_filename)
            .filter(|name| !name.is_empty());
        if let Some(alerts) = self.alerts.as_mut() {
            for alert in alerts {
                alert.message = sanitize_message(&alert.message, 512);
            }
        }
        Ok(self)
    }
}

impl PrinterState {
    pub(crate) fn permits_print_control(
        &self,
        kind: crate::shared::workspace::PrinterActionKind,
        context: &crate::shared::workspace::PrintControlContext,
    ) -> bool {
        use crate::shared::workspace::PrinterActionKind as K;
        let now = Instant::now();
        self.connection == LanConnection::Connected
            && self.controls.allows_print(kind)
            && self.telemetry_ready
            && self.connection_epoch == context.connection_epoch
            && self.job_generation == context.job_generation
            && self.age_ms(now).is_some_and(|age| age <= 15_000)
            && now
                .checked_duration_since(context.submitted_at)
                .is_some_and(|age| age <= std::time::Duration::from_secs(5))
            && match kind {
                K::PausePrint => self.telemetry.print_state == PrintStatus::Printing,
                K::ResumePrint => self.telemetry.print_state == PrintStatus::Paused,
                K::CancelPrint => matches!(
                    self.telemetry.print_state,
                    PrintStatus::Printing | PrintStatus::Paused
                ),
                _ => false,
            }
    }

    pub(crate) fn new(id: LocalPrinterId) -> Self {
        Self {
            notifications: None,
            history_capture: None,
            native_history: None,
            id,
            connection: LanConnection::Connecting,
            last_error: None,
            last_valid_at: None,
            revision: 0,
            job_generation: 0,
            connection_epoch: 0,
            controls: crate::shared::workspace::PrinterControls::STANDARD,
            snapshot: None,
            telemetry_ready: false,
            telemetry: crate::shared::telemetry::PrinterTelemetry::default(),
        }
    }

    pub(crate) fn publish_snapshot(
        &mut self,
        snapshot: Snapshot,
        received_at: Instant,
    ) -> Result<(), ValidationError> {
        let snapshot = snapshot.sanitized().inspect_err(|error| {
            crate::diagnostics::log(
                crate::diagnostics::LogEvent::new(
                    crate::diagnostics::LogLevel::Warn,
                    crate::diagnostics::LogArea::Normalization,
                    "normalized_data.rejected",
                )
                .printer(self.id.0)
                .result("rejected")
                .detail(error.to_string()),
            );
        })?;
        let job_generation = if matches!(
            snapshot.status,
            Some(PrintStatus::Printing | PrintStatus::Paused)
        ) && self
            .telemetry
            .current_job
            .as_ref()
            .is_none_or(|job| job.print_name != snapshot.filename)
        {
            self.job_generation
                .checked_add(1)
                .ok_or(ValidationError::RevisionExhausted)?
        } else {
            self.job_generation
        };
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or(ValidationError::RevisionExhausted)?;
        self.job_generation = job_generation;
        self.telemetry.observe(&snapshot);
        if let Some(notifications) = &self.notifications {
            notifications.observe(&snapshot, received_at);
        }
        if let Some(capture) = &self.history_capture {
            capture.observe(&snapshot, received_at);
        }
        self.snapshot = Some(snapshot);
        self.telemetry_ready = true;
        self.last_valid_at = Some(received_at);
        self.connection = LanConnection::Connected;
        self.last_error = None;
        Ok(())
    }

    pub(crate) fn set_connection(
        &mut self,
        connection: LanConnection,
    ) -> Result<(), ValidationError> {
        if self.connection != connection {
            self.revision = self
                .revision
                .checked_add(1)
                .ok_or(ValidationError::RevisionExhausted)?;
            let was_connected = self.connection == LanConnection::Connected;
            self.connection = connection;
            if connection != LanConnection::Connected {
                if let Some(notifications) = &self.notifications {
                    notifications.disconnected(Instant::now());
                }
                if was_connected {
                    self.connection_epoch = self
                        .connection_epoch
                        .checked_add(1)
                        .ok_or(ValidationError::RevisionExhausted)?;
                }
                if let Some(capture) = &self.history_capture {
                    capture.disconnected();
                }
                self.telemetry_ready = false;
            }
        }
        Ok(())
    }

    pub(crate) fn record_error(
        &mut self,
        message: impl Into<String>,
    ) -> Result<(), ValidationError> {
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or(ValidationError::RevisionExhausted)?;
        self.last_error = Some(sanitize_message(&message.into(), 512));
        Ok(())
    }

    pub(crate) fn age_ms(&self, now: Instant) -> Option<u64> {
        self.last_valid_at
            .and_then(|instant| now.checked_duration_since(instant))
            .and_then(|duration| u64::try_from(duration.as_millis()).ok())
    }

    #[cfg(test)]
    pub(crate) fn telemetry_envelope(
        &self,
        session_generation: u64,
        now: Instant,
    ) -> crate::shared::telemetry::TelemetryEnvelope {
        crate::shared::telemetry::TelemetryEnvelope {
            dictionary_version: crate::shared::telemetry::DICTIONARY_VERSION.to_owned(),
            session_generation,
            printer_id: self.id.to_string(),
            age_ms: self.age_ms(now),
            telemetry: self.telemetry.clone(),
        }
    }
}

pub(crate) fn sanitize_filename(value: &str) -> String {
    let component = value.rsplit(['/', '\\']).next().unwrap_or_default();
    sanitize_message(component, 1024)
}

pub(crate) fn sanitize_message(value: &str, maximum_bytes: usize) -> String {
    let mut sanitized = String::with_capacity(value.len().min(maximum_bytes));
    for character in value.chars().filter(|character| !character.is_control()) {
        if sanitized.len() + character.len_utf8() > maximum_bytes {
            break;
        }
        sanitized.push(character);
    }
    sanitized
}

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
