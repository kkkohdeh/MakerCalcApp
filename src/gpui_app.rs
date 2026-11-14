use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::input::InputState;
use gpui_kit::component::table::TableState;
use gpui_kit::component::{ActiveTheme as _, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Context, Entity, FocusHandle, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    Styled as _, Task, Window, WindowControlArea, deferred, div, px,
};

use crate::cloud::CloudView;
use crate::config::Protocol;
use crate::discovery::DiscoveryHandle;
use crate::edge::EdgeCommand;
use crate::shared::printer::{EditableConnectionView, PrinterLocalMetadata};
use crate::shared::thumbnail::{ThumbnailHandle, ThumbnailResourceKey};
use crate::shared::workspace::{
    CameraHandle, CameraState, DetailHandle, DetailState, OperationState, PrinterAction, ResourceId,
};
use crate::state::{LocalPrinterId, PrinterState};

mod bay;
mod charts;
mod components;
mod confirmations;
mod detail;
mod feedback;
mod files;
mod finder;
mod format;
mod history;
mod icons;
mod machine;
mod machine_controls;
mod materials;
mod media;
mod overview;
#[cfg(feature = "ui-diagnostics")]
mod performance;
mod printer_form;
mod runtime;
mod settings;
mod temperatures;
mod theme;
mod update_notice;
mod window;

pub(crate) use runtime::run;

use self::charts::TemperaturePoint;
use self::detail::DetailTab;
use self::feedback::{PrinterAlertKey, Toast};
use self::finder::PrinterFinder;
use self::printer_form::PrinterForm;
use self::window::modal_backdrop;

pub(super) struct PrinterEntry {
    pub(super) history: crate::history::HistoryHandle,
    pub(super) history_id: String,
    pub(super) local_history: Option<Result<Arc<crate::shared::history::LocalHistoryPage>, String>>,
    pub(super) history_version: Option<u64>,
    pub(super) history_query: u64,
    pub(super) history_loading: bool,
    pub(super) name: String,
    pub(super) protocol: Protocol,
    pub(super) host: String,
    pub(super) port: u16,
    pub(super) session_generation: u64,
    pub(super) connection_view: EditableConnectionView,
    pub(super) metadata: PrinterLocalMetadata,
    pub(super) thumbnail_handle: ThumbnailHandle,
    pub(super) active_thumbnail_key: Option<ThumbnailResourceKey>,
    pub(super) state: PrinterState,
    pub(super) detail: DetailHandle,
    pub(super) detail_state: Arc<DetailState>,
    pub(super) actions: tokio::sync::mpsc::Sender<PrinterAction>,
    pub(super) operation_state: OperationState,
    pub(super) operation: crate::shared::workspace::OperationHandle,
    material_pending: Option<materials::MaterialTarget>,
    pub(super) camera: CameraHandle,
    pub(super) camera_state: Arc<CameraState>,
    pub(super) thumbnail: Option<bytes::Bytes>,
    pub(super) thumbnail_color: Option<u32>,
    thumbnail_image: media::ImageSlot,
    camera_image: media::ImageSlot,
    file_image: media::ImageSlot,
    pub(super) thumbnail_fetching: bool,
    pub(super) thumbnail_retry_at: Option<std::time::Instant>,
    pub(super) temperature_history: VecDeque<TemperaturePoint>,
    pub(super) last_temp_sample: std::time::Instant,
}

impl PrinterEntry {
    pub(super) fn record_temperature_sample(&mut self) -> bool {
        self.record_temperature_sample_at(std::time::Instant::now())
    }

    fn record_temperature_sample_at(&mut self, now: std::time::Instant) -> bool {
        if !self.temperature_history.is_empty()
            && now.duration_since(self.last_temp_sample) < std::time::Duration::from_secs(1)
        {
            return false;
        }
        let (Some(nozzle), Some(bed)) = (
            self.state.telemetry.active_nozzle_temp_c(),
            self.state.telemetry.primary_bed_temp_c(),
        ) else {
            return false;
        };
        let nozzle_target = self.state.telemetry.active_nozzle_target_temp_c();
        let bed_target = self.state.telemetry.primary_bed_target_temp_c();

        if self.temperature_history.len() >= 30 {
            self.temperature_history.pop_front();
        }
        for p in self.temperature_history.iter_mut() {
            let offset = now.duration_since(p.sampled_at).as_secs();
            p.time = format!("-{}s", offset).into();
        }
        self.temperature_history.push_back(TemperaturePoint {
            time: crate::i18n::t!("common.charts.now").into_owned().into(),
            sampled_at: now,
            nozzle,
            bed,
            nozzle_target,
            bed_target,
        });
        self.last_temp_sample = now;
        true
    }

    pub(super) fn update_latest_temperature_sample(&mut self) {
        let (Some(nozzle), Some(bed)) = (
            self.state.telemetry.active_nozzle_temp_c(),
            self.state.telemetry.primary_bed_temp_c(),
        ) else {
            return;
        };
        let nozzle_target = self.state.telemetry.active_nozzle_target_temp_c();
        let bed_target = self.state.telemetry.primary_bed_target_temp_c();

        if self.temperature_history.is_empty()
            || self.last_temp_sample.elapsed() >= std::time::Duration::from_secs(1)
        {
            self.record_temperature_sample();
        } else if let Some(last) = self.temperature_history.back_mut() {
            last.nozzle = nozzle;
            last.bed = bed;
            last.nozzle_target = nozzle_target;
            last.bed_target = bed_target;
        }
    }
}

pub(crate) struct MakerCalcGpuiApp {
    pub(super) desktop_settings: settings::desktop::DesktopSettings,
    pub(super) desktop_window: Option<crate::desktop::DesktopWindow>,
    pub(super) visual_active: bool,
    pub(super) printers: BTreeMap<LocalPrinterId, PrinterEntry>,
    pub(super) bay_order: Vec<LocalPrinterId>,
    pub(super) bay_dragging: Option<LocalPrinterId>,
    pub(super) bay_hovered: Option<LocalPrinterId>,
    pub(super) selected: Option<LocalPrinterId>,
    pub(super) selected_file: Option<ResourceId>,
    pub(super) detail_tab: DetailTab,
    pub(super) settings_open: bool,
    pub(super) printer_form: Option<PrinterForm>,
    pub(super) edge_actions: tokio::sync::mpsc::Sender<EdgeCommand>,
    pub(super) discovery: DiscoveryHandle,
    pub(super) finder: Option<PrinterFinder>,
    pub(super) finder_task: Option<Task<()>>,
    pub(super) finder_generation: u64,
    pub(super) printer_form_focus: FocusHandle,
    pub(super) printer_form_opener: Option<FocusHandle>,
    pub(super) settings_opener: Option<FocusHandle>,
    pub(super) printer_delete_focus: FocusHandle,
    pub(super) mutation_pending: bool,
    pub(super) mutation_error: Option<String>,
    pub(in crate::gpui_app) pending_print: Option<confirmations::PendingPrint>,
    pending_print_control: Option<confirmations::PendingPrintControl>,
    pub(super) print_bed_leveling: bool,
    pub(super) print_material_station: bool,
    pub(super) print_first_layer_inspection: bool,
    pub(super) print_flow_calibration: bool,
    pub(super) print_timelapse: bool,
    pub(super) print_safety_confirmed: bool,
    pub(super) print_countdown: Option<u8>,
    pub(super) print_countdown_task: Option<Task<()>>,
    pub(super) pending_delete: Option<LocalPrinterId>,
    pub(in crate::gpui_app) pending_upload: Option<confirmations::PendingUpload>,
    pub(super) file_picker_open: bool,
    pub(super) diagnostics: VecDeque<String>,
    pub(super) logs_search: Entity<InputState>,
    pub(super) cloud_view: CloudView,
    pub(super) cloud_token: Entity<InputState>,
    pub(super) files_search: Entity<InputState>,
    pub(super) files_list: Entity<TableState<files::list::FileList>>,
    history_view: history::HistoryView,
    pub(super) registry_task: Task<()>,
    pub(super) diagnostics_task: Task<()>,
    pub(super) cloud_task: Task<()>,
    pub(super) printer_tasks: Vec<Task<()>>,
    pub(super) detail_tasks: Vec<Task<()>>,
    pub(super) operation_tasks: Vec<Task<()>>,
    pub(super) camera_tasks: Vec<Task<()>>,
    pub(super) timer_task: Task<()>,
    pub(super) toasts: Vec<Toast>,
    pub(super) toast_overflowed: bool,
    dismissed_alerts: BTreeSet<PrinterAlertKey>,
    pub(super) updater: Arc<crate::updater::AutoUpdater>,
    update_notice: update_notice::StartupNotice,
    material_controls: materials::MaterialControls,
    machine_controls: machine_controls::MachineControls,
}

impl Render for MakerCalcGpuiApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Remove visual animation nodes while hidden, but retain all entities/data/selection.
        if !self.visual_active {
            return div().size_full().into_any_element();
        }
        let content = if let Some((id, printer)) = self
            .selected
            .and_then(|id| self.printers.get(&id).map(|printer| (id, printer)))
        {
            Self::render_detail(
                id,
                printer,
                self.detail_tab,
                self.selected_file,
                window.viewport_size().width.as_f32(),
                self.visual_active,
                &self.machine_controls,
                &self.files_search,
                &self.files_list,
                &self.history_view,
                window,
                cx,
            )
        } else {
            self.render_bay_with_history(window, cx)
        };

        // Actions clear immediately; the exit animation retains no executable controls.
        if self.pending_upload.is_some()
            || self.pending_delete.is_some()
            || self.pending_print.is_some()
            || self.pending_print_control.is_some()
        {
            components::alert_dialog::sync_focus(true, window, cx);
        }
        let confirmations = [
            self.render_upload_confirmation(window, cx),
            self.render_delete_confirmation(window, cx),
            self.render_start_print_confirmation(window, cx),
            self.render_print_control_confirmation(window, cx),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
        let confirmation_visible = !confirmations.is_empty();
        components::alert_dialog::sync_focus(confirmation_visible, window, cx);
        let printer_form = self.render_printer_form(window, cx);
        let settings = self.render_settings(window, cx);
        let material_editor = self.render_material_editor(window, cx);
        let machine_editor = self
            .render_machine_editor(window, cx)
            .or_else(|| self.render_drying_editor(window, cx));
        let mut dialog_visible = printer_form.is_some()
            || settings.is_some()
            || material_editor.is_some()
            || machine_editor.is_some();
        let update_notice = self.render_update_notice(
            dialog_visible || confirmation_visible || self.file_picker_open,
            window,
            cx,
        );
        dialog_visible |= update_notice.is_some();

        v_flex()
            .size_full()
            .relative()
            .on_key_down(
                cx.listener(|app, event: &gpui_kit::KeyDownEvent, window, cx| {
                    if event.keystroke.key == "escape" && app.bay_dragging.is_some() {
                        cx.stop_active_drag(window);
                        cx.stop_propagation();
                    }
                }),
            )
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(self.render_title_bar(window, cx))
            .child(
                div()
                    .flex_1()
                    .w_full()
                    .min_h(px(0.))
                    .overflow_hidden()
                    .child(content),
            )
            .children(printer_form)
            .children(settings)
            .children(material_editor)
            .children(machine_editor)
            .children(update_notice)
            .children(confirmations)
            .when(self.file_picker_open, |app| {
                app.child(modal_backdrop(
                    "file-picker-modal-backdrop",
                    cx.theme().overlay,
                ))
            })
            .child(self.render_toasts(window, cx))
            .when(
                dialog_visible || confirmation_visible || self.file_picker_open,
                |app| {
                    app.child(
                        deferred(
                            div()
                                .id("modal-window-drag")
                                .test_support()
                                .absolute()
                                .top_0()
                                .left_0()
                                .right(px(105.))
                                .h(px(44.))
                                .occlude()
                                .window_control_area(WindowControlArea::Drag),
                        )
                        .with_priority(11),
                    )
                },
            )
            .into_any_element()
    }
}
