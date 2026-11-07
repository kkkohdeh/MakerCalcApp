//! Durable normalized executions. Vendor JSON and transient resource handles are not history.
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use super::printer::PrintStatus;
use super::telemetry::{FilamentSupplyTelemetry, JobTelemetry};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum HistorySource {
    Observed,
    Native,
}

#[derive(Clone, Deserialize, PartialEq, Serialize)]
pub(crate) struct HistoryRecord {
    pub(crate) printer_id: String,
    pub(crate) job_uid: String,
    pub(crate) source: HistorySource,
    /// Observed starts are estimates: first observation minus reported printing time.
    pub(crate) started_at: DateTime<Utc>,
    pub(crate) first_observed_at: DateTime<Utc>,
    pub(crate) last_observed_at: DateTime<Utc>,
    pub(crate) ended_at: Option<DateTime<Utc>>,
    pub(crate) incomplete: bool,
    pub(crate) job: JobTelemetry,
    pub(crate) filament_supplies: Option<Vec<FilamentSupplyTelemetry>>,
}

#[derive(Clone, Default)]
pub(crate) struct LocalHistoryPage {
    pub(crate) total: u64,
    pub(crate) offset: u64,
    pub(crate) records: Vec<HistoryRecord>,
}

pub(crate) const HISTORY_PAGE_SIZE: u64 = 50;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum HistoryColumn {
    Name,
    State,
    #[default]
    Started,
    PrintTime,
    Grams,
    Length,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HistoryQuery {
    pub(crate) name: String,
    pub(crate) state: Option<PrintStatus>,
    pub(crate) from: Option<NaiveDate>,
    pub(crate) until: Option<NaiveDate>,
    pub(crate) column: HistoryColumn,
    pub(crate) descending: bool,
}

impl Default for HistoryQuery {
    fn default() -> Self {
        Self {
            name: String::new(),
            state: None,
            from: None,
            until: None,
            column: HistoryColumn::Started,
            descending: true,
        }
    }
}

/// Wire projection of a persisted normalized record.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct PrintJob {
    pub(crate) printer_id: String,
    pub(crate) job_uid: String,
    pub(crate) print_name: Option<String>,
    pub(crate) print_state: String,
    pub(crate) started_at: DateTime<Utc>,
    pub(crate) ended_at: DateTime<Utc>,
    pub(crate) print_duration_s: Option<f64>,
    pub(crate) filament_used_mm: Option<f64>,
    pub(crate) filament_used_g: Option<f64>,
}

impl HistoryRecord {
    pub(crate) fn wire_job(&self) -> Option<PrintJob> {
        let state = match self.job.print_state {
            PrintStatus::Completed => "completed",
            PrintStatus::Cancelled => "cancelled",
            PrintStatus::Error => "error",
            _ => return None,
        };
        Some(PrintJob {
            printer_id: self.printer_id.clone(),
            job_uid: self.job_uid.clone(),
            print_name: self.job.print_name.clone(),
            print_state: state.to_owned(),
            started_at: self.started_at,
            ended_at: self.ended_at?,
            print_duration_s: self.job.print_elapsed_time_s,
            filament_used_mm: self.job.print_filament_used_mm,
            filament_used_g: self.job.print_filament_used_g,
        })
    }
}
