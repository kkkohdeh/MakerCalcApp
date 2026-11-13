//! The only translation boundary between the Moonraker detail protocol and MakerCalc.
//! Resource paths, RPC methods and JSON never escape this module toward the UI.
use super::{DriverConfig, camera, detail as raw, local};
use crate::config::{MoonrakerLightBinding, MoonrakerLightBindings};
use crate::shared::telemetry::LightRole;
use crate::shared::workspace as model;
use crate::state::{PrintStatus, PrinterState, sanitize_message};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;
use tokio::sync::{mpsc, watch};
pub(super) mod control;
mod features;
mod file_estimates;
mod machine;

static NEXT_RESOURCE: AtomicU64 = AtomicU64::new(1);
const FILES: &str = "model:files";
const TOTALS: &str = "model:totals";
const QUEUE: &str = "model:queue";
const CAMERAS: &str = "model:cameras";
const POWER: &str = "model:power";
const LIGHTS: &str = "model:lights";
const SERVER: &str = "server:server.info:base";
const FIRMWARE: &str = "model:firmware";
const COMMANDS: &str = "model:commands";
const STORAGE: &str = "model:storage";
const CONFIGURATION: &str = "model:configuration_pending";

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Resource {
    File(String),
    Camera(String),
    Device(String),
    Storage,
    QueueItem(String),
    PrintObject(String),
}

struct Projection {
    state: model::DetailState,
    resources: BTreeMap<Resource, model::ResourceId>,
    previous: Option<Arc<raw::DetailState>>,
    light_bindings: MoonrakerLightBindings,
    estimates: file_estimates::Estimates,
    inspections: BTreeMap<String, file_estimates::FileKey>,
    next_inspection: u64,
    observed_at: Option<std::time::Instant>,
    object_job: Option<(String, f64)>,
}

impl Projection {
    fn with_light_bindings(light_bindings: MoonrakerLightBindings) -> Self {
        Self {
            state: model::DetailState::empty(0),
            resources: BTreeMap::new(),
            previous: None,
            light_bindings,
            estimates: file_estimates::Estimates::new(),
            inspections: BTreeMap::new(),
            next_inspection: 0,
            observed_at: None,
            object_job: None,
        }
    }

    fn id(&mut self, resource: Resource) -> model::ResourceId {
        *self
            .resources
            .entry(resource)
            .or_insert_with(|| model::ResourceId(NEXT_RESOURCE.fetch_add(1, Ordering::Relaxed)))
    }

    fn resolve(&self, id: model::ResourceId) -> Option<&Resource> {
        self.resources
            .iter()
            .find_map(|(resource, candidate)| (*candidate == id).then_some(resource))
    }

    fn update(&mut self, source: Arc<raw::DetailState>) {
        if self.state.generation != source.generation {
            self.state = model::DetailState::empty(source.generation);
            self.resources.clear();
            self.previous = None;
            self.inspections.clear();
            self.object_job = None;
        }
        self.state.live = source.live;
        self.state.revision = self.state.revision.saturating_add(1);
        self.state.devices = source
            .objects
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(name, value)| self.device(name, value))
            .collect();
        let active_tool = source
            .objects
            .get("toolhead")
            .and_then(|v| v.get("extruder"))
            .and_then(Value::as_str);
        self.state.temperature_targets.clear();
        if active_tool == Some("extruder")
            && source.objects.get("extruder").is_some_and(Value::is_object)
        {
            self.state
                .temperature_targets
                .push(model::HeaterKind::Nozzle);
        }
        if source
            .objects
            .get("heater_bed")
            .is_some_and(Value::is_object)
        {
            self.state.temperature_targets.push(model::HeaterKind::Bed);
        }
        for device in &mut self.state.devices {
            if let Some(extrusion) = &mut device.readings.extrusion
                && device.kind == model::DeviceKind::Nozzle
            {
                extrusion.active = active_tool.and_then(|active| {
                    self.resources
                        .get(&Resource::Device(active.to_owned()))
                        .map(|id| *id == device.id)
                });
            }
        }
        self.state.system.unmapped_objects = source
            .objects
            .as_object()
            .map_or(0, |o| o.len())
            .saturating_sub(self.state.devices.len());
        for (key, entry) in &source.sections {
            let unchanged = self
                .previous
                .as_ref()
                .and_then(|p| p.sections.get(key))
                .is_some_and(|p| {
                    p.status == entry.status
                        && p.data == entry.data
                        && p.error == entry.error
                        && p.gcode_facts == entry.gcode_facts
                        && p.thumbnail == entry.thumbnail
                });
            if !unchanged {
                self.section(key, entry);
            }
        }
        self.refresh_features(&source);
        self.state.system.discovered_devices = self.state.devices.len();
        self.observed_at = source.live.then(std::time::Instant::now);
        self.previous = Some(source);
    }

    fn device(&mut self, name: &str, value: &Value) -> Option<model::Device> {
        use model::DeviceKind as Kind;
        let (kind, label) = if name == "extruder"
            || name
                .strip_prefix("extruder")
                .is_some_and(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()))
        {
            (Kind::Nozzle, "Boquilla")
        } else if name == "heater_bed" {
            (Kind::Bed, "Cama")
        } else if name == "fan"
            || [
                "fan_generic ",
                "heater_fan ",
                "controller_fan ",
                "temperature_fan ",
            ]
            .iter()
            .any(|prefix| name.starts_with(prefix))
        {
            (Kind::Fan, "Ventilador")
        } else if name.starts_with("temperature_sensor ") || name.starts_with("heater_generic ") {
            (Kind::TemperatureSensor, "Temperatura")
        } else if name.starts_with("filament_switch_sensor ")
            || name.starts_with("filament_motion_sensor ")
        {
            (Kind::FilamentSensor, "Filamento")
        } else if name == "motion_report" {
            (Kind::Motion, "Movimiento")
        } else {
            machine::classify(name)?
        };
        let suffix = name.split_once(' ').map(|(_, label)| label).unwrap_or("");
        let label = if let Some(index) = name
            .strip_prefix("extruder")
            .and_then(|n| n.parse::<u32>().ok())
        {
            format!("Boquilla {}", u64::from(index) + 1)
        } else if let Some(configured_name) = name.strip_prefix("fan_generic ") {
            clean(configured_name)
        } else if suffix.is_empty() {
            label.to_owned()
        } else {
            format!("{label} · {}", clean(suffix))
        };
        let id = self.id(Resource::Device(name.to_owned()));
        let mut device = model::Device::new(id, kind, label);
        device.telemetry_id = match kind {
            Kind::Nozzle if name == "extruder" => Some(crate::shared::telemetry::ElementId(1)),
            Kind::Nozzle => name
                .strip_prefix("extruder")
                .and_then(|index| index.parse::<u32>().ok())
                .and_then(|index| index.checked_add(1))
                .map(crate::shared::telemetry::ElementId),
            Kind::Bed => Some(crate::shared::telemetry::ElementId(1)),
            _ => None,
        };
        machine::populate(name, value, &mut device.readings);
        device.motion_source = match name {
            "toolhead" => Some(crate::shared::machine::MotionSource::MotionPlanner),
            "gcode_move" => Some(crate::shared::machine::MotionSource::CommandInterpreter),
            _ => None,
        };
        device.temperature_c = number(value, "temperature");
        device.target_temperature_c = number(value, "target");
        device.output_pct = match kind {
            Kind::Fan => fraction(value, "speed"),
            Kind::Nozzle | Kind::Bed | Kind::TemperatureSensor => fraction(value, "power"),
            _ => None,
        };
        device.rpm = positive(value, "rpm");
        device.enabled = value.get("enabled").and_then(Value::as_bool);
        device.filament_present = value.get("filament_detected").and_then(Value::as_bool);
        device.can_extrude = value.get("can_extrude").and_then(Value::as_bool);
        device.velocity_mm_s = positive(value, "live_velocity");
        device.position_mm = value
            .get("live_position")
            .and_then(Value::as_array)
            .and_then(|p| {
                let [x, y, z, ..] = p.as_slice() else {
                    return None;
                };
                let coordinates = [x.as_f64()?, y.as_f64()?, z.as_f64()?];
                coordinates
                    .iter()
                    .all(|v| v.is_finite())
                    .then_some(coordinates)
            });
        Some(device)
    }

    fn section(&mut self, key: &str, entry: &raw::SectionData) {
        let value = entry.data.as_ref();
        match key {
            SERVER => {
                project_result(&mut self.state.system_read, entry, value.map(|_| ()));
                if let Some(value) = value {
                    let components = value.get("components").and_then(Value::as_array);
                    let supports = |name| {
                        components.map_or(model::Support::Unknown, |list| {
                            if list.iter().any(|v| v.as_str() == Some(name)) {
                                model::Support::Available
                            } else {
                                model::Support::Unavailable
                            }
                        })
                    };
                    self.state.capabilities = model::Capabilities {
                        files: value
                            .get("registered_directories")
                            .and_then(Value::as_array)
                            .map_or(model::Support::Unknown, |roots| {
                                if roots.iter().any(|r| r.as_str() == Some("gcodes")) {
                                    model::Support::Available
                                } else {
                                    model::Support::Unavailable
                                }
                            }),
                        history: supports("history"),
                        queue: supports("job_queue"),
                        cameras: supports("webcams"),
                        power: supports("power"),
                        lighting: supports("wled"),
                        sensors: supports("sensor"),
                        material_station: model::Support::Unavailable,
                    };
                    self.state.system.firmware_ready = value
                        .get("klippy_state")
                        .and_then(Value::as_str)
                        .map(|s| s == "ready");
                    self.state.system.software_version = string(value, "moonraker_version");
                }
            }
            FILES => {
                let mut files = value.and_then(Value::as_array).map(|files| {
                    files
                        .iter()
                        .take(10_000)
                        .filter_map(|file| {
                            let path = file.get("path")?.as_str()?;
                            if !local::valid_gcode_filename(path) {
                                return None;
                            }
                            Some(model::PrinterFile {
                                id: self.id(Resource::File(path.to_owned())),
                                display_name: basename(path),
                                size_bytes: file.get("size").and_then(Value::as_u64),
                                modified_at_unix_s: positive(file, "modified"),
                                estimated_duration_s: positive(file, "estimated_time"),
                                duration_read_status: if positive(file, "estimated_time").is_some()
                                {
                                    model::ReadStatus::Ready
                                } else {
                                    model::ReadStatus::NotLoaded
                                },
                            })
                        })
                        .collect::<Vec<_>>()
                });
                if let Some(files) = files.as_mut() {
                    let keys: BTreeMap<_, _> = files
                        .iter()
                        .map(|file| {
                            (
                                file.id,
                                file_estimates::FileKey::new(self.state.generation, file),
                            )
                        })
                        .collect();
                    let old_keys: BTreeMap<_, _> = self
                        .state
                        .files
                        .value
                        .as_deref()
                        .unwrap_or(&[])
                        .iter()
                        .map(|file| {
                            (
                                file.id,
                                file_estimates::FileKey::new(self.state.generation, file),
                            )
                        })
                        .collect();
                    self.state.file_details.retain(|id, _| {
                        keys.get(id)
                            .is_some_and(|key| old_keys.get(id) == Some(key))
                    });
                    self.inspections
                        .retain(|_, key| keys.get(&key.id) == Some(key));
                    self.estimates.sync_catalog(self.state.generation, files);
                }
                self.state.file_estimate_error = None;
                project_result(&mut self.state.files, entry, files);
            }
            TOTALS => {
                let totals =
                    value
                        .and_then(|v| v.get("job_totals"))
                        .map(|v| model::HistoryTotals {
                            jobs: v.get("total_jobs").and_then(Value::as_u64),
                            print_duration_s: positive(v, "total_print_time"),
                            total_duration_s: positive(v, "total_time"),
                            filament_used_mm: positive(v, "total_filament_used"),
                            longest_print_duration_s: positive(v, "longest_print"),
                            longest_total_duration_s: positive(v, "longest_job"),
                        });
                project_result(&mut self.state.history_totals, entry, totals);
            }
            QUEUE => {
                let queue = value.and_then(|v| {
                    Some(model::PrintQueue {
                        status: match v.get("queue_state")?.as_str()? {
                            "ready" => model::QueueStatus::Ready,
                            "loading" | "starting" => model::QueueStatus::Running,
                            "paused" => model::QueueStatus::Paused,
                            _ => model::QueueStatus::Unknown,
                        },
                        file_names: v
                            .get("queued_jobs")?
                            .as_array()?
                            .iter()
                            .filter_map(|j| j.get("filename")?.as_str().map(basename))
                            .collect(),
                        entries: v
                            .get("queued_jobs")?
                            .as_array()?
                            .iter()
                            .take(1000)
                            .filter_map(|job| {
                                let uid = job.get("job_id")?.as_str()?;
                                let filename = job.get("filename")?.as_str()?;
                                if !local::valid_gcode_filename(filename)
                                    || uid.is_empty()
                                    || uid.len() > 128
                                    || uid.chars().any(char::is_control)
                                {
                                    return None;
                                }
                                Some(model::QueueEntry {
                                    id: self.id(Resource::QueueItem(uid.to_owned())),
                                    display_name: basename(filename),
                                    added_at_unix_s: positive(job, "time_added"),
                                    waiting_s: positive(job, "time_in_queue"),
                                })
                            })
                            .collect(),
                    })
                });
                if let Some(queue) = &queue {
                    self.resources.retain(|r, id| {
                        !matches!(r, Resource::QueueItem(_))
                            || queue.entries.iter().any(|e| e.id == *id)
                    });
                }
                project_result(&mut self.state.queue, entry, queue);
            }
            CAMERAS => {
                let cameras = value
                    .and_then(|v| v.get("webcams")?.as_array())
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|v| {
                                let uid = v.get("uid")?.as_str()?;
                                Some(model::Camera {
                                    id: self.id(Resource::Camera(uid.to_owned())),
                                    display_name: string(v, "name")
                                        .unwrap_or_else(|| "Cámara".to_owned()),
                                    enabled: v.get("enabled").and_then(Value::as_bool),
                                    rotation_degrees: v
                                        .get("rotation")
                                        .and_then(Value::as_u64)
                                        .filter(|v| [0, 90, 180, 270].contains(v))
                                        .map(|v| v as u16),
                                    flip_horizontal: v
                                        .get("flip_horizontal")
                                        .and_then(Value::as_bool),
                                    flip_vertical: v.get("flip_vertical").and_then(Value::as_bool),
                                    target_fps: positive(v, "target_fps").filter(|v| *v <= 240.0),
                                })
                            })
                            .collect()
                    });
                project_result(&mut self.state.cameras, entry, cameras);
            }
            POWER | LIGHTS => {
                let devices = value
                    .and_then(|v| {
                        if key == POWER {
                            v.get("devices")?
                                .as_array()
                                .map(|a| a.iter().collect::<Vec<_>>())
                        } else {
                            v.get("strips")?
                                .as_object()
                                .map(|m| m.values().collect::<Vec<_>>())
                        }
                    })
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|v| {
                                let name = v.get("device").or_else(|| v.get("strip"))?.as_str()?;
                                let id = self.id(Resource::Device(format!("{key}:{name}")));
                                let mut d = model::Device::new(
                                    id,
                                    if key == POWER {
                                        model::DeviceKind::Power
                                    } else {
                                        model::DeviceKind::Light
                                    },
                                    clean(name),
                                );
                                d.enabled = match v.get("status").and_then(Value::as_str) {
                                    Some("on") => Some(true),
                                    Some("off") => Some(false),
                                    _ => None,
                                };
                                d.locked_while_printing =
                                    v.get("locked_while_printing").and_then(Value::as_bool);
                                if key == LIGHTS {
                                    d.output_pct = number(v, "brightness")
                                        .filter(|v| (0.0..=255.0).contains(v))
                                        .map(|v| v * 100.0 / 255.0);
                                }
                                Some(d)
                            })
                            .collect()
                    });
                project_result(
                    if key == POWER {
                        &mut self.state.power_devices
                    } else {
                        &mut self.state.lights
                    },
                    entry,
                    devices,
                );
            }
            _ => {
                if let Some(id) = self.file_section_id(key, "model:metadata:") {
                    let mut details = self
                        .state
                        .file_details
                        .get(&id)
                        .and_then(|r| r.value.clone())
                        .unwrap_or_default();
                    if let Some(v) = value {
                        details.size_bytes = v.get("size").and_then(Value::as_u64);
                        // Fill gaps only: the portable G-code parser takes precedence.
                        let facts = &mut details.analysis;
                        facts.slicer = facts.slicer.take().or_else(|| string(v, "slicer"));
                        facts.filament.material = facts
                            .filament
                            .material
                            .take()
                            .or_else(|| string(v, "filament_type"));
                        facts.filament.profile_name = facts
                            .filament
                            .profile_name
                            .take()
                            .or_else(|| string(v, "filament_name"));
                        facts.filament.length_mm = facts
                            .filament
                            .length_mm
                            .or_else(|| positive(v, "filament_total"));
                        facts.filament.weight_g = facts
                            .filament
                            .weight_g
                            .or_else(|| positive(v, "filament_weight_total"));
                        facts.print.estimated_seconds = facts
                            .print
                            .estimated_seconds
                            .or_else(|| positive(v, "estimated_time"));
                        facts.print.layer_height_mm = facts
                            .print
                            .layer_height_mm
                            .or_else(|| positive(v, "layer_height"));
                        facts.print.total_layers = facts.print.total_layers.or_else(|| {
                            v.get("layer_count")
                                .and_then(Value::as_u64)
                                .and_then(|n| n.try_into().ok())
                        });
                        features::metadata(v, facts);
                    }
                    self.file_result(id, entry, details, false);
                }
                if let Some(id) = self.file_section_id(key, "model:preview:") {
                    let mut details = self
                        .state
                        .file_details
                        .get(&id)
                        .and_then(|r| r.value.clone())
                        .unwrap_or_default();
                    if let Some(facts) = &entry.gcode_facts {
                        let fallback = &details.analysis;
                        let mut facts = facts.as_ref().clone().without_diagnostics();
                        facts.slicer = facts.slicer.or_else(|| fallback.slicer.clone());
                        facts.filament.material = facts
                            .filament
                            .material
                            .or_else(|| fallback.filament.material.clone());
                        facts.filament.profile_name = facts
                            .filament
                            .profile_name
                            .or_else(|| fallback.filament.profile_name.clone());
                        facts.filament.length_mm =
                            facts.filament.length_mm.or(fallback.filament.length_mm);
                        facts.filament.weight_g =
                            facts.filament.weight_g.or(fallback.filament.weight_g);
                        facts.print.estimated_seconds = facts
                            .print
                            .estimated_seconds
                            .or(fallback.print.estimated_seconds);
                        facts.print.layer_height_mm = facts
                            .print
                            .layer_height_mm
                            .or(fallback.print.layer_height_mm);
                        facts.print.total_layers =
                            facts.print.total_layers.or(fallback.print.total_layers);
                        features::merge_facts(&mut facts, fallback);
                        details.analysis = facts;
                    }
                    if let Some(v) = value {
                        details.thumbnail = entry.thumbnail.clone();
                        details.size_bytes = details
                            .size_bytes
                            .or_else(|| v.get("size").and_then(Value::as_u64));
                    }
                    self.file_result(id, entry, details, true);
                }
            }
        }
    }

    fn file_section_id(&self, key: &str, prefix: &str) -> Option<model::ResourceId> {
        key.strip_prefix(prefix)?;
        let expected = self.inspections.get(key)?;
        (file_estimates::FileKey::current(&self.state, expected.id).as_ref() == Some(expected))
            .then_some(expected.id)
    }

    fn cancel_estimate(&mut self, key: &file_estimates::FileKey) -> bool {
        if file_estimates::FileKey::current(&self.state, key.id).as_ref() != Some(key) {
            return false;
        }
        let Some(file) = self
            .state
            .files
            .value
            .as_mut()
            .and_then(|files| files.iter_mut().find(|file| file.id == key.id))
            .filter(|file| file.duration_read_status == model::ReadStatus::Loading)
        else {
            return false;
        };
        file.duration_read_status = model::ReadStatus::NotLoaded;
        true
    }

    fn file_result(
        &mut self,
        id: model::ResourceId,
        entry: &raw::SectionData,
        mut details: model::FileDetails,
        analysis: bool,
    ) {
        if !matches!(self.resolve(id), Some(Resource::File(_))) {
            return;
        }
        details.analysis.print.estimated_seconds =
            details.analysis.print.estimated_seconds.or_else(|| {
                self.state
                    .files
                    .value
                    .as_ref()?
                    .iter()
                    .find(|file| file.id == id)?
                    .estimated_duration_s
            });
        let seconds = details.analysis.print.estimated_seconds;
        if (seconds.is_some() || (analysis && entry.status == raw::SectionStatus::Ready))
            && let Some(key) = file_estimates::FileKey::current(&self.state, id)
        {
            if let Some(file) = self
                .state
                .files
                .value
                .as_mut()
                .and_then(|files| files.iter_mut().find(|file| file.id == id))
            {
                file.estimated_duration_s = seconds;
            }
            self.estimates.finish(&mut self.state, key, seconds);
        }
        if !self.state.file_details.contains_key(&id)
            && self.state.file_details.len() >= 8
            && let Some(oldest) = self.state.file_details.keys().copied().next()
        {
            self.state.file_details.remove(&oldest);
        }
        let error = entry.error.as_deref().map(error_message);
        if analysis {
            details.analysis_status = read_status(entry.status);
            details.analysis_error = error;
        } else {
            details.metadata_status = read_status(entry.status);
            details.metadata_error = error;
        }
        // A successful operation must not mask the other operation's failure.
        let priority = |s: model::ReadStatus| match s {
            model::ReadStatus::Error => 7,
            model::ReadStatus::Denied => 6,
            model::ReadStatus::Limited => 5,
            model::ReadStatus::Unsupported => 4,
            model::ReadStatus::Loading => 3,
            model::ReadStatus::NotLoaded => 2,
            model::ReadStatus::Ready => 1,
        };
        let status = [details.metadata_status, details.analysis_status]
            .into_iter()
            .max_by_key(|s| priority(*s))
            .unwrap_or_default();
        let errors = [
            details.metadata_error.as_deref(),
            details.analysis_error.as_deref(),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
        self.state.file_details.insert(
            id,
            model::ReadResult {
                status,
                value: Some(details),
                error: (!errors.is_empty()).then_some(errors),
            },
        );
    }

    fn queries(
        &mut self,
        request: &model::DetailRequest,
    ) -> Result<Vec<raw::DetailRequest>, &'static str> {
        use model::DetailRequest as Q;
        let rpc = |key: &str, method: &str, params| raw::DetailRequest::Rpc {
            key: key.to_owned(),
            method: method.to_owned(),
            params,
        };
        let queries = match request {
            Q::RefreshMachine | Q::System => vec![
                rpc(SERVER, "server.info", None),
                rpc(FIRMWARE, "printer.info", None),
                raw::DetailRequest::CommandCatalog {
                    key: COMMANDS.to_owned(),
                },
                rpc(
                    CONFIGURATION,
                    "printer.objects.query",
                    Some(json!({"objects":{"configfile":["save_config_pending","settings"]}})),
                ),
            ],
            Q::ListFiles => vec![rpc(
                FILES,
                "server.files.list",
                Some(json!({"root":"gcodes"})),
            )],
            Q::InspectFile(id) => {
                if !self.state.file_details.contains_key(id)
                    && self.state.file_details.len() >= 8
                    && let Some(oldest) = self.state.file_details.keys().copied().next()
                {
                    self.state.file_details.remove(&oldest);
                    self.inspections.retain(|_, key| key.id != oldest);
                }
                let Some(Resource::File(path)) = self.resolve(*id) else {
                    return Err("El archivo ya no está disponible. Actualiza la lista.");
                };
                let path = path.clone();
                let file_key = file_estimates::FileKey::current(&self.state, *id)
                    .ok_or("El archivo ya no está disponible. Actualiza la lista.")?;
                self.next_inspection = self
                    .next_inspection
                    .checked_add(1)
                    .ok_or("El archivo ya no está disponible. Actualiza la lista.")?;
                let metadata_key = format!("model:metadata:{}:{}", id.0, self.next_inspection);
                let preview_key = format!("model:preview:{}:{}", id.0, self.next_inspection);
                self.inspections.retain(|_, key| key.id != *id);
                self.inspections
                    .insert(metadata_key.clone(), file_key.clone());
                self.inspections.insert(preview_key.clone(), file_key);
                vec![
                    rpc(
                        &metadata_key,
                        "server.files.metadata",
                        Some(json!({"filename":path})),
                    ),
                    raw::DetailRequest::PreviewFile {
                        key: preview_key,
                        root: "gcodes".to_owned(),
                        path: path.clone(),
                        offset: 0,
                    },
                ]
            }
            Q::HistoryTotals => vec![rpc(TOTALS, "server.history.totals", None)],
            Q::Queue => vec![rpc(QUEUE, "server.job_queue.status", None)],
            Q::Cameras => vec![rpc(CAMERAS, "server.webcams.list", None)],
            Q::Peripherals => vec![
                rpc(POWER, "machine.device_power.devices", None),
                rpc(LIGHTS, "machine.wled.strips", None),
                rpc(
                    STORAGE,
                    "server.files.get_directory",
                    Some(json!({"path":"gcodes","extended":false})),
                ),
            ],
        };
        Ok(queries)
    }

    fn enqueue_read(
        &mut self,
        request: &model::DetailRequest,
        sender: &mpsc::Sender<raw::DetailRequest>,
    ) -> Result<(), &'static str> {
        // Reserve the full batch so file inspection cannot partially enqueue.
        let permits = sender
            .try_reserve_many(if matches!(request, model::DetailRequest::InspectFile(_)) {
                2
            } else if matches!(
                request,
                model::DetailRequest::RefreshMachine | model::DetailRequest::System
            ) {
                4
            } else if matches!(request, model::DetailRequest::Peripherals) {
                3
            } else {
                1
            })
            .map_err(|_| "Hay consultas pendientes. Vuelve a intentarlo.")?;
        let queries = self.queries(request)?;
        for (permit, query) in permits.zip(queries) {
            permit.send(query);
        }
        Ok(())
    }

    fn action(
        &self,
        action: model::PrinterAction,
        telemetry: &PrinterState,
    ) -> Result<local::PrinterAction, String> {
        use model::PrinterAction as A;
        Ok(match action {
            A::StartPrint { file_id, options } => {
                if options.first_layer_inspection || options.flow_calibration || options.timelapse {
                    return Err(crate::i18n::t!("printers.print_options.unsupported").into_owned());
                }
                let Some(Resource::File(path)) = self.resolve(file_id) else {
                    return Err(
                        "El archivo ya no está disponible. Actualiza la lista y vuelve a seleccionarlo.".to_owned(),
                    );
                };
                local::PrinterAction::StartPrint {
                    filename: path.clone(),
                }
            }
            A::PausePrint(context) => local::PrinterAction::PausePrint(context),
            A::ResumePrint(context) => local::PrinterAction::ResumePrint(context),
            A::CancelPrint(context) => local::PrinterAction::CancelPrint(context),
            A::SetTemperatureTarget {
                heater,
                target_c,
                connection_epoch,
                submitted_at,
            } => {
                if !self.state.live
                    || matches!(
                        telemetry.telemetry.print_state,
                        PrintStatus::Printing | PrintStatus::Paused
                    )
                    || !self.state.temperature_targets.contains(&heater)
                    || target_c > heater.max_target_c()
                    || telemetry.connection != crate::state::LanConnection::Connected
                    || telemetry.connection_epoch != connection_epoch
                    || !telemetry.telemetry_ready
                    || !telemetry
                        .age_ms(std::time::Instant::now())
                        .is_some_and(|age| age <= 15_000)
                    || submitted_at.elapsed() > Duration::from_secs(5)
                {
                    return Err(crate::i18n::t!("printers.temperature.unavailable").into_owned());
                }
                local::PrinterAction::SetTemperatureTarget {
                    heater,
                    target_c,
                    connection_epoch,
                    submitted_at,
                }
            }
            A::ToggleLight { role, .. } => {
                let binding = match role {
                    LightRole::BuildArea => self.light_bindings.build_area.as_ref(),
                    LightRole::Toolhead => self.light_bindings.toolhead.as_ref(),
                }
                .ok_or("Esta luz no está configurada para Moonraker")?;
                match binding {
                    MoonrakerLightBinding::OutputPin { name } => {
                        let enabled = telemetry
                            .telemetry
                            .lights
                            .iter()
                            .find(|light| light.role == role)
                            .and_then(|light| light.enabled)
                            .ok_or("Moonraker no informó el estado actual de esta salida")?;
                        local::PrinterAction::SetLightOutput {
                            name: name.clone(),
                            enabled: !enabled,
                        }
                    }
                    MoonrakerLightBinding::ToggleMacro { name } => {
                        local::PrinterAction::RunLightMacro { name: name.clone() }
                    }
                }
            }
            A::UploadGcode {
                source,
                destination,
            } => local::PrinterAction::UploadGcode {
                source,
                destination,
            },
        })
    }

    fn request_error(&mut self, request: &model::DetailRequest, message: &str) {
        fn fail<T>(result: &mut model::ReadResult<T>, message: &str) {
            result.status = model::ReadStatus::Error;
            result.error = Some(message.to_owned());
        }
        use model::DetailRequest as Q;
        match request {
            Q::ListFiles => fail(&mut self.state.files, message),
            Q::InspectFile(id) => fail(self.state.file_details.entry(*id).or_default(), message),
            Q::HistoryTotals => fail(&mut self.state.history_totals, message),
            Q::Queue => fail(&mut self.state.queue, message),
            Q::Cameras => fail(&mut self.state.cameras, message),
            Q::Peripherals => {
                fail(&mut self.state.power_devices, message);
                fail(&mut self.state.lights, message);
                fail(&mut self.state.storage, message);
            }
            Q::RefreshMachine | Q::System => fail(&mut self.state.system_read, message),
        }
    }
}

fn clean(text: &str) -> String {
    sanitize_message(text, 512)
}
fn error_message(text: &str) -> String {
    serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|v| v.get("message")?.as_str().map(clean))
        .unwrap_or_else(|| clean(text))
}
fn basename(path: &str) -> String {
    clean(path.rsplit(['/', '\\']).next().unwrap_or(path))
}
fn string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)?
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .map(clean)
}
fn number(value: &Value, key: &str) -> Option<f64> {
    value.get(key)?.as_f64().filter(|v| v.is_finite())
}
fn positive(value: &Value, key: &str) -> Option<f64> {
    number(value, key).filter(|v| *v >= 0.0)
}
fn fraction(value: &Value, key: &str) -> Option<f64> {
    number(value, key)
        .filter(|v| (0.0..=1.0).contains(v))
        .map(|v| v * 100.0)
}
fn read_status(status: raw::SectionStatus) -> model::ReadStatus {
    use raw::SectionStatus as S;
    match status {
        S::NotLoaded => model::ReadStatus::NotLoaded,
        S::Loading => model::ReadStatus::Loading,
        S::Ready => model::ReadStatus::Ready,
        S::Unsupported => model::ReadStatus::Unsupported,
        S::Denied => model::ReadStatus::Denied,
        S::Limited => model::ReadStatus::Limited,
        S::Error => model::ReadStatus::Error,
    }
}
fn project_result<T>(
    target: &mut model::ReadResult<T>,
    entry: &raw::SectionData,
    value: Option<T>,
) {
    target.status = read_status(entry.status);
    target.error = entry.error.as_deref().map(error_message);
    if let Some(value) = value {
        if matches!(
            entry.status,
            raw::SectionStatus::Ready | raw::SectionStatus::Limited
        ) {
            target.value = Some(value);
        }
    } else if entry.status == raw::SectionStatus::Ready {
        target.status = model::ReadStatus::Error;
        target.error =
            Some("La respuesta no contiene los datos esperados por MakerCalc.".to_owned());
    }
}
fn action_kind(kind: local::PrinterActionKind) -> model::PrinterActionKind {
    use local::PrinterActionKind as K;
    match kind {
        K::StartPrint => model::PrinterActionKind::StartPrint,
        K::PausePrint => model::PrinterActionKind::PausePrint,
        K::ResumePrint => model::PrinterActionKind::ResumePrint,
        K::CancelPrint => model::PrinterActionKind::CancelPrint,
        K::SetTemperature => model::PrinterActionKind::SetTemperature,
        K::SetLight => model::PrinterActionKind::SetLight,
        K::UploadGcode => model::PrinterActionKind::UploadGcode,
        K::MachineControl => model::PrinterActionKind::MachineControl,
    }
}
fn operation(source: &local::OperationState) -> model::OperationState {
    use local::OperationState as S;
    match source {
        S::Idle => model::OperationState::Idle,
        S::Sending(k) => model::OperationState::Sending(action_kind(*k)),
        S::Accepted(k) => model::OperationState::Accepted(action_kind(*k)),
        S::Unknown(k) => model::OperationState::Unknown(action_kind(*k)),
        S::Rejected { action, message } => model::OperationState::Rejected {
            action: action_kind(*action),
            message: if *action == local::PrinterActionKind::MachineControl {
                crate::i18n::t!("printers.actions.machine_control_rejected").into_owned()
            } else {
                clean(message)
            },
        },
    }
}

pub(crate) async fn thumbnail_body(
    response: reqwest::Response,
    limit: usize,
) -> Option<bytes::Bytes> {
    if response
        .content_length()
        .is_some_and(|len| len > limit as u64)
    {
        return None;
    }
    let mut body = bytes::BytesMut::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk_res) = stream.next().await {
        let chunk = chunk_res.ok()?;
        if chunk.len() > limit.saturating_sub(body.len()) {
            return None;
        }
        body.extend_from_slice(&chunk);
    }
    Some(body.freeze())
}

pub(crate) fn percent_encode_path(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.bytes() {
        match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(b as char);
            }
            _ => {
                use std::fmt::Write;
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

pub(crate) fn verify_request(
    client: &reqwest::Client,
    candidate: &crate::discovery::Candidate,
    trusted_lan: bool,
    api_key: Option<&str>,
) -> Result<reqwest::RequestBuilder, String> {
    let url = crate::discovery::endpoint_url(candidate, "/server/info")?;
    let request = client.get(url);
    if !trusted_lan {
        let key = api_key
            .filter(|s| !s.trim().is_empty())
            .ok_or("Introduce la API key")?;
        Ok(request.header("X-Api-Key", key))
    } else {
        Ok(request)
    }
}

pub(crate) fn verify_response(body: &[u8]) -> bool {
    crate::discovery::valid_moonraker(body)
}

async fn moonraker_active_filename(
    client: &reqwest::Client,
    scheme: &str,
    host: &str,
    port: u16,
    auth: Option<&str>,
    fallback: String,
) -> String {
    let query_url = format!("{scheme}://{host}:{port}/printer/objects/query?print_stats=filename");
    let mut request = client.get(query_url);
    if let Some(api_key) = auth.map(str::trim).filter(|key| !key.is_empty()) {
        request = request.header("X-Api-Key", api_key);
    }
    let Ok(response) = request.send().await else {
        return fallback;
    };
    if !response.status().is_success() {
        return fallback;
    }
    let Some(body) = thumbnail_body(response, 64 * 1024).await else {
        return fallback;
    };
    let Some(raw) = serde_json::from_slice::<serde_json::Value>(&body)
        .ok()
        .and_then(|value| {
            value
                .pointer("/result/status/print_stats/filename")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
    else {
        return fallback;
    };
    let expected_name = fallback.rsplit(['/', '\\']).next().unwrap_or_default();
    let raw_name = raw.rsplit('/').next().unwrap_or_default();
    if raw_name == expected_name
        && crate::integrations::moonraker::local::valid_gcode_filename(&raw)
    {
        raw
    } else {
        fallback
    }
}

pub(crate) async fn fetch_thumbnail(
    client: reqwest::Client,
    scheme: &'static str,
    host: String,
    port: u16,
    auth: Option<String>,
    key: crate::shared::thumbnail::ThumbnailResourceKey,
) -> Result<crate::shared::thumbnail::NormalizedThumbnail, crate::shared::thumbnail::ThumbnailError>
{
    let filename = moonraker_active_filename(
        &client,
        scheme,
        &host,
        port,
        auth.as_deref(),
        key.print_name.clone(),
    )
    .await;
    let encoded_filename = percent_encode_path(&filename);
    let file_url = format!("{scheme}://{host}:{port}/server/files/gcodes/{encoded_filename}");
    let mut req = client.get(&file_url).header("Range", "bytes=0-524287");
    if let Some(ref api_key) = auth {
        let trimmed = api_key.trim();
        if !trimmed.is_empty() {
            req = req.header("X-Api-Key", trimmed);
        }
    }
    let mut transport_failure = false;
    let range_res = req.send().await;
    match range_res {
        Ok(resp) if resp.status().is_success() => {
            if let Some(bytes) = thumbnail_body(resp, 512 * 1024).await {
                let thumb = crate::shared::thumbnail::run_blocking_thumbnail_cpu(move || {
                    crate::shared::thumbnail::embedded_thumbnail_image(&String::from_utf8_lossy(
                        &bytes,
                    ))
                })
                .await
                .ok()
                .flatten();
                if let Some((png_bytes, width, height)) = thumb {
                    return Ok(crate::shared::thumbnail::NormalizedThumbnail {
                        key,
                        png_bytes,
                        width,
                        height,
                    });
                }
            }
        }
        Err(_) => {
            transport_failure = true;
        }
        Ok(_) => {}
    }

    // Fall back to the Moonraker metadata thumbnail in .thumbs when present.
    let meta_url =
        format!("{scheme}://{host}:{port}/server/files/metadata?filename={encoded_filename}");
    let mut meta_req = client.get(&meta_url);
    if let Some(ref api_key) = auth {
        let trimmed = api_key.trim();
        if !trimmed.is_empty() {
            meta_req = meta_req.header("X-Api-Key", trimmed);
        }
    }
    match meta_req.send().await {
        Ok(meta_resp) => {
            if meta_resp.status().is_success()
                && let Some(body) = thumbnail_body(meta_resp, 64 * 1024).await
                && let Ok(val) = serde_json::from_slice::<serde_json::Value>(&body)
                && let Some(thumbs) = val.pointer("/result/thumbnails").and_then(|t| t.as_array())
            {
                let best_thumb = thumbs
                    .iter()
                    .filter(|thumbnail| {
                        let width = thumbnail.get("width").and_then(|w| w.as_u64());
                        let height = thumbnail.get("height").and_then(|h| h.as_u64());
                        width.is_some_and(|w| w > 0 && w <= 1024)
                            && height.is_some_and(|h| h > 0 && h <= 1024)
                    })
                    .max_by_key(|thumbnail| {
                        thumbnail.get("width").and_then(|w| w.as_u64()).unwrap_or(0)
                    });
                if let Some(rel_path) = best_thumb
                    .and_then(|thumbnail| thumbnail.get("relative_path"))
                    .and_then(serde_json::Value::as_str)
                {
                    let full_rel = if let Some((dir, _)) = filename.rsplit_once('/') {
                        let dir_prefix = format!("{dir}/");
                        if rel_path.starts_with(&dir_prefix) {
                            rel_path.to_owned()
                        } else {
                            format!("{dir}/{rel_path}")
                        }
                    } else {
                        rel_path.to_owned()
                    };
                    let enc_rel = percent_encode_path(&full_rel);
                    let thumb_img_url =
                        format!("{scheme}://{host}:{port}/server/files/gcodes/{enc_rel}");
                    let mut thumb_req = client.get(&thumb_img_url);
                    if let Some(api_key) =
                        auth.as_deref().map(str::trim).filter(|key| !key.is_empty())
                    {
                        thumb_req = thumb_req.header("X-Api-Key", api_key);
                    }
                    match thumb_req.send().await {
                        Ok(thumb_resp) => {
                            if thumb_resp.status().is_success()
                                && let Some(img_bytes) =
                                    thumbnail_body(thumb_resp, 1024 * 1024).await
                            {
                                let normalized =
                                    crate::shared::thumbnail::run_blocking_thumbnail_cpu(
                                        move || {
                                            crate::shared::thumbnail::normalize_thumbnail_image(
                                                &img_bytes,
                                            )
                                        },
                                    )
                                    .await
                                    .ok()
                                    .flatten();
                                if let Some((png_bytes, width, height)) = normalized {
                                    return Ok(crate::shared::thumbnail::NormalizedThumbnail {
                                        key,
                                        png_bytes,
                                        width,
                                        height,
                                    });
                                }
                            }
                        }
                        Err(_) => {
                            transport_failure = true;
                        }
                    }
                }
            }
        }
        Err(_) => {
            transport_failure = true;
        }
    }

    if transport_failure {
        Err(crate::shared::thumbnail::ThumbnailError::Unavailable)
    } else {
        Err(crate::shared::thumbnail::ThumbnailError::NotFound)
    }
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) async fn run_adapter(
    config: DriverConfig,
    state: watch::Sender<PrinterState>,
    shutdown: watch::Receiver<bool>,
    detail_io: model::DetailIo,
    operation_io: model::OperationIo,
    camera_io: model::CameraIo,
) {
    let (_thumb_handle, thumb_io) = crate::shared::thumbnail::make_thumbnail_channels();
    run_adapter_with_thumbnail(
        config,
        state,
        shutdown,
        detail_io,
        operation_io,
        camera_io,
        thumb_io,
    )
    .await;
}

pub(crate) async fn run_adapter_with_thumbnail(
    config: DriverConfig,
    state: watch::Sender<PrinterState>,
    shutdown: watch::Receiver<bool>,
    detail_io: model::DetailIo,
    operation_io: model::OperationIo,
    camera_io: model::CameraIo,
    thumbnail_io: crate::shared::thumbnail::ThumbnailIo,
) {
    let thumb_client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(4))
        .build()
        .ok();
    if let Some(thumb_client) = thumb_client {
        let scheme = if config.connection.tls {
            "https"
        } else {
            "http"
        };
        let host = config.connection.host.clone();
        let port = config.connection.port;
        let api_key = config.api_key.clone();
        let thumb_state = state.subscribe();
        tokio::spawn(crate::shared::thumbnail::run_thumbnail_actor(
            thumbnail_io,
            thumb_state,
            move |key| {
                let client = thumb_client.clone();
                let host = host.clone();
                let api_key = api_key.clone();
                async move { fetch_thumbnail(client, scheme, host, port, api_key, key).await }
            },
        ));
    }
    let light_bindings = config.connection.light_bindings.clone();
    let (detail, raw_detail_io) = raw::make_detail_channels();
    let (operation, raw_operation_io) = local::make_operation_channels();
    let (camera, raw_camera_io) = camera::make_camera_channels();
    detail.active.send_replace(*detail_io.active.borrow());
    let context = BridgeContext {
        shutdown: shutdown.clone(),
        telemetry: state.subscribe(),
        light_bindings,
        file_reader: super::make_file_reader(&config).ok(),
        file_secrets: config.api_key.iter().cloned().collect(),
    };
    let output = detail_io.state.clone();
    let driver = super::run_driver(
        config,
        state,
        shutdown.clone(),
        raw_detail_io,
        raw_operation_io,
        raw_camera_io,
    );
    let bridge = bridge(
        detail,
        operation,
        camera,
        detail_io,
        operation_io,
        camera_io,
        context,
    );
    tokio::pin!(driver, bridge);
    tokio::select! { () = &mut driver => {}, () = &mut bridge => {} }
    let mut final_state = output.borrow().as_ref().clone();
    final_state.live = false;
    output.send_replace(Arc::new(final_state));
}

struct BridgeContext {
    shutdown: watch::Receiver<bool>,
    telemetry: watch::Receiver<PrinterState>,
    light_bindings: MoonrakerLightBindings,
    file_reader: Option<super::FileReader>,
    file_secrets: Vec<String>,
}

async fn bridge(
    mut detail: raw::DetailHandle,
    mut operations: local::OperationHandle,
    mut camera: camera::CameraHandle,
    mut detail_io: model::DetailIo,
    mut operation_io: model::OperationIo,
    mut camera_io: model::CameraIo,
    mut context: BridgeContext,
) {
    let mut projection = Projection::with_light_bindings(context.light_bindings.clone());
    let mut estimate_jobs = tokio::task::JoinSet::new();
    let mut active_estimate = None;
    let mut pending_control: Option<
        tokio::sync::oneshot::Sender<crate::shared::control::ControlResult>,
    > = None;
    let mut pending_refresh = None;
    let mut job_generation = context.telemetry.borrow().job_generation;
    detail
        .active
        .send_replace(*detail_io.active.borrow_and_update());
    loop {
        let deadline = if estimate_jobs.is_empty()
            && *detail_io.active.borrow()
            && context.telemetry.borrow().connection == crate::state::LanConnection::Connected
            && !operation_io.state.borrow().is_pending()
        {
            projection.estimates.deadline(&projection.state)
        } else {
            None
        };
        tokio::select! {
            biased;
            changed = context.shutdown.changed() => { if changed.is_err() || *context.shutdown.borrow() { break } }
            changed = context.telemetry.changed() => {
                if changed.is_err() { break }
                let connected = context.telemetry.borrow_and_update().connection == crate::state::LanConnection::Connected;
                let current_job = context.telemetry.borrow().job_generation;
                if current_job != job_generation {
                    job_generation = current_job;
                    projection.resources.retain(|r,_| !matches!(r,Resource::PrintObject(_)));
                    projection.state.print_objects = model::ReadResult::default();
                    projection.object_job = None;
                }
                if !connected { estimate_jobs.abort_all(); }
                if !connected && let Some(reply) = pending_control.take() {
                    pending_refresh = None;
                    let _ = reply.send(crate::shared::control::ControlResult::Unknown);
                }
                let live = connected && detail.state.borrow().live && *detail_io.active.borrow();
                if projection.state.live != live {
                    projection.state.live = live;
                    detail_io.state.send_replace(Arc::new(projection.state.clone()));
                }
            }
            changed = detail_io.active.changed() => {
                if changed.is_err() { break }
                let active = *detail_io.active.borrow_and_update();
                detail.active.send_replace(active);
                if !active {
                    estimate_jobs.abort_all();
                    let generation = projection.state.generation;
                    let history_totals = projection.state.history_totals.clone();
                    projection = Projection::with_light_bindings(context.light_bindings.clone());
                    projection.state = model::DetailState::empty(generation);
                    projection.state.history_totals = history_totals;
                    detail_io.state.send_replace(Arc::new(projection.state.clone()));
                    camera_io.state.send_replace(Arc::new(model::CameraState::empty()));
                } else {
                    let connected = context.telemetry.borrow().connection == crate::state::LanConnection::Connected;
                    let live = connected && detail.state.borrow().live;
                    if projection.state.live != live {
                        projection.state.live = live;
                        detail_io.state.send_replace(Arc::new(projection.state.clone()));
                    }
                }
            }
            changed = detail_io.file_interest.changed() => {
                if changed.is_err() { break; }
                projection.estimates.replace(detail_io.file_interest.borrow_and_update().clone());
            }
            changed = detail.state.changed() => {
                if changed.is_err() { break }
                let source = detail.state.borrow_and_update().clone();
                if !*detail_io.active.borrow() {
                    if let Some(entry) = source.sections.get(TOTALS) {
                        let old = projection.state.history_totals.value.clone();
                        projection.section(TOTALS, entry);
                        if projection.state.history_totals.value != old {
                            projection.state.revision = projection.state.revision.saturating_add(1);
                            detail_io.state.send_replace(Arc::new(projection.state.clone()));
                        }
                    }
                    continue
                }
                if projection.state.generation != source.generation {
                    estimate_jobs.abort_all();
                    camera_io.state.send_replace(Arc::new(model::CameraState::empty()));
                }
                projection.update(source);
                projection.state.live &= context.telemetry.borrow().connection == crate::state::LanConnection::Connected;
                detail_io.state.send_replace(Arc::new(projection.state.clone()));
            }
            changed = operations.state.changed() => {
                if changed.is_err() { break }
                let state = operation(&operations.state.borrow_and_update());
                use crate::shared::control::{ControlError, ControlResult};
                let result = match &state {
                    model::OperationState::Accepted(model::PrinterActionKind::MachineControl) => Some(ControlResult::Accepted),
                    model::OperationState::Rejected { action: model::PrinterActionKind::MachineControl, .. } => Some(ControlResult::Rejected(ControlError::PrinterRejected)),
                    model::OperationState::Unknown(model::PrinterActionKind::MachineControl) => Some(ControlResult::Unknown),
                    _ => None,
                };
                let mut refresh = None;
                if let Some(result) = result && let Some(reply) = pending_control.take() {
                    let request = pending_refresh.take();
                    if result == ControlResult::Accepted { refresh = request; }
                    let _ = reply.send(result);
                }
                if matches!(state, model::OperationState::Accepted(model::PrinterActionKind::UploadGcode)) {
                    refresh = Some(model::DetailRequest::ListFiles);
                }
                let machine_control = matches!(state, model::OperationState::Accepted(model::PrinterActionKind::MachineControl));
                operation_io.state.send_replace(state);
                // Read back only the affected area; never replay a physical action.
                if let Some(request) = refresh
                    && *detail_io.active.borrow()
                    && context.telemetry.borrow().connection == crate::state::LanConnection::Connected {
                    if let Err(message) = projection.enqueue_read(&request, &detail.requests) {
                        projection.request_error(&request, message);
                    } else if machine_control {
                        match request {
                            model::DetailRequest::ListFiles => projection.state.files.status = model::ReadStatus::Loading,
                            model::DetailRequest::Queue => projection.state.queue.status = model::ReadStatus::Loading,
                            model::DetailRequest::Peripherals => {
                                projection.state.power_devices.status = model::ReadStatus::Loading;
                                projection.state.lights.status = model::ReadStatus::Loading;
                                projection.state.storage.status = model::ReadStatus::Loading;
                            }
                            _ => {}
                        }
                    } else {
                        continue;
                    }
                    projection.state.revision = projection.state.revision.saturating_add(1);
                    detail_io.state.send_replace(Arc::new(projection.state.clone()));
                }
            }
            changed = camera.state.changed() => {
                if changed.is_err() { break }
                let source = camera.state.borrow_and_update();
                if source.uid.as_ref().is_some_and(|uid| !projection.resources.contains_key(&Resource::Camera(uid.clone()))) {
                    camera_io.state.send_replace(Arc::new(model::CameraState::empty()));
                    continue;
                }
                use camera::CameraStatus as S;
                let status = match source.status { S::Idle => model::ReadStatus::NotLoaded, S::Loading => model::ReadStatus::Loading, S::Ready => model::ReadStatus::Ready,
                    S::Unsupported => model::ReadStatus::Unsupported, S::Denied => model::ReadStatus::Denied, S::Error => model::ReadStatus::Error, S::Limited => model::ReadStatus::Limited };
                camera_io.state.send_replace(Arc::new(model::CameraState { status, frame: source.frame.clone(), received_at: source.received_at, message: source.message.as_deref().map(clean) }));
            }
            Some(request) = detail_io.requests.recv() => {
                let connected = context.telemetry.borrow().connection == crate::state::LanConnection::Connected;
                if !connected {
                    projection.request_error(&request, "No hay conexión de detalle activa. Espera la reconexión y actualiza.");
                    detail_io.state.send_replace(Arc::new(projection.state.clone()));
                    continue;
                }
                if request == model::DetailRequest::ListFiles {
                    projection.estimates.retry_failed();
                    projection.state.file_estimate_error = None;
                }
                let result = projection.enqueue_read(&request, &detail.requests);
                if let Err(message) = result { projection.request_error(&request, message); detail_io.state.send_replace(Arc::new(projection.state.clone())); }
                else if request == model::DetailRequest::ListFiles {
                    projection.state.files.status = model::ReadStatus::Loading;
                    detail_io.state.send_replace(Arc::new(projection.state.clone()));
                }
            }
            Some(command) = operation_io.machine_actions.recv() => {
                use crate::shared::control::{ControlError, ControlResult};
                if command.reply.is_closed() { continue; }
                let busy = pending_control.is_some() || operations.state.borrow().is_pending() || operations.actions.capacity() != operations.actions.max_capacity();
                let prepared = projection.prepare_control(&command, &context.telemetry.borrow(), busy);
                match prepared {
                    Err(error) => { let _ = command.reply.send(ControlResult::Rejected(error)); }
                    Ok(control) => {
                        let refresh = control.refresh_request();
                        match operations.actions.try_send(local::PrinterAction::MachineControl { control: Box::new(control) }) {
                        Ok(()) => {
                            operation_io.state.send_replace(model::OperationState::Sending(model::PrinterActionKind::MachineControl));
                            pending_control = Some(command.reply);
                            pending_refresh = refresh;
                        }
                        Err(error) => {
                            let error = match error { mpsc::error::TrySendError::Full(_) => ControlError::Busy, mpsc::error::TrySendError::Closed(_) => ControlError::Disconnected };
                            let _ = command.reply.send(ControlResult::Rejected(error));
                        }
                    }}
                }
            }
            Some(action) = operation_io.actions.recv() => {
                let kind = action.kind();
                if pending_control.is_some()
                    || operations.state.borrow().is_pending()
                    || operations.actions.capacity() != operations.actions.max_capacity()
                {
                    operation_io.state.send_replace(model::OperationState::Rejected {action:kind,message:crate::i18n::t!("printers.actions.busy_control").into_owned()});
                    continue;
                }
                if context.telemetry.borrow().connection != crate::state::LanConnection::Connected {
                    operation_io.state.send_replace(model::OperationState::Rejected { action: kind, message: "Sin conexión activa. La orden no se enviará después de reconectar.".to_owned() });
                    continue;
                }
                let result = projection.action(action, &context.telemetry.borrow()).and_then(|action| operations.actions.try_send(action).map_err(|_| "Hay una acción pendiente. No se envió otra.".to_owned()));
                if let Err(message) = result { operation_io.state.send_replace(model::OperationState::Rejected { action: kind, message: message.to_owned() }); }
            }
            result = estimate_jobs.join_next(), if !estimate_jobs.is_empty() => {
                let previous = active_estimate.take();
                let result = match result {
                    Some(Ok(result)) => Some(result),
                    Some(Err(error)) if !error.is_cancelled() => previous.map(|key| (key, Err(()))),
                    Some(Err(_)) => {
                        if previous.as_ref().is_some_and(|key| projection.cancel_estimate(key)) {
                            projection.state.revision = projection.state.revision.saturating_add(1);
                            detail_io.state.send_replace(Arc::new(projection.state.clone()));
                        }
                        None
                    }
                    _ => None,
                };
                if let Some((key, result)) = result {
                    let changed = match result {
                        Ok(seconds) => projection.estimates.finish(&mut projection.state, key, seconds),
                        Err(()) => projection.estimates.fail(&mut projection.state, key),
                    };
                    if changed {
                        projection.state.revision = projection.state.revision.saturating_add(1);
                        detail_io.state.send_replace(Arc::new(projection.state.clone()));
                    }
                }
            }
            _ = file_estimates::wait_until(deadline) => {
                let Some(key) = projection.estimates.next(&projection.state) else { continue; };
                let Some(Resource::File(path)) = projection.resolve(key.id) else { continue; };
                let path = path.clone();
                active_estimate = Some(key.clone());
                if let Some(file) = projection.state.files.value.as_mut().and_then(|files| files.iter_mut().find(|file| file.id == key.id)) {
                    file.duration_read_status = model::ReadStatus::Loading;
                }
                projection.state.revision = projection.state.revision.saturating_add(1);
                detail_io.state.send_replace(Arc::new(projection.state.clone()));
                let reader = context.file_reader.as_ref().map(|reader| (reader.client.clone(), reader.base.clone()));
                let secrets = context.file_secrets.clone();
                estimate_jobs.spawn(async move {
                    let result = match reader {
                        Some((client, base)) => file_estimates::read(client, base, path, secrets, &key).await,
                        None => Err(()),
                    };
                    (key, result)
                });
            }
            Some(model::CameraRequest::Load { id }) = camera_io.requests.recv() => {
                let result = match projection.resolve(id).filter(|_| projection.state.live) {
                    Some(Resource::Camera(uid)) => camera.requests.try_send(camera::CameraRequest::Load { uid: uid.clone() }).map_err(|_| "Hay una captura pendiente."),
                    _ => Err("La cámara ya no está disponible. Actualiza la lista."),
                };
                if let Err(message) = result {
                    let mut next = model::CameraState::empty(); next.status = model::ReadStatus::Error; next.message = Some(message.to_owned());
                    camera_io.state.send_replace(Arc::new(next));
                }
            }
        }
    }
    if let Some(reply) = pending_control {
        let _ = reply.send(crate::shared::control::ControlResult::Unknown);
    }
}
