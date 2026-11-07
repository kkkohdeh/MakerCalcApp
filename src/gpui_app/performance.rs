//! Opt-in UI measurements from GPUI's profiler; no printer payloads or extra polling task.
use std::time::{Duration, Instant};

use gpui_kit::profiler::{FrameDurationSnapshot, FramePhase, InputLatencySnapshot};
use gpui_kit::{AnyWindowHandle, App, Context, Window};

use crate::diagnostics::{LogArea, LogEvent, LogLevel};

use super::{DetailTab, MakerCalcGpuiApp};

const PERIOD: Duration = Duration::from_secs(5);

#[derive(Clone)]
struct Snapshot {
    frames: FrameDurationSnapshot,
    input: InputLatencySnapshot,
    viewport: [u32; 2],
}

impl Snapshot {
    fn capture(window: &Window) -> Self {
        Self {
            frames: window.frame_duration_snapshot(),
            input: window.input_latency_snapshot(),
            viewport: [window.viewport_size().width, window.viewport_size().height]
                .map(|size| f32::from(size).round() as u32),
        }
    }

    fn interval(previous: &Self, next: &Self) -> Result<Self, ()> {
        let mut delta = next.clone();
        for (next, previous) in delta
            .frames
            .phase_histograms
            .iter_mut()
            .zip(&previous.frames.phase_histograms)
        {
            next.subtract(previous).map_err(|_| ())?;
        }
        delta
            .frames
            .draw_duration_histogram
            .subtract(&previous.frames.draw_duration_histogram)
            .map_err(|_| ())?;
        delta
            .frames
            .dirty_to_present_histogram
            .subtract(&previous.frames.dirty_to_present_histogram)
            .map_err(|_| ())?;
        delta
            .frames
            .present_interval_histogram
            .subtract(&previous.frames.present_interval_histogram)
            .map_err(|_| ())?;
        delta
            .input
            .latency_histogram
            .subtract(&previous.input.latency_histogram)
            .map_err(|_| ())?;
        delta
            .input
            .events_per_frame_histogram
            .subtract(&previous.input.events_per_frame_histogram)
            .map_err(|_| ())?;
        delta.input.mid_draw_events_dropped = next
            .input
            .mid_draw_events_dropped
            .checked_sub(previous.input.mid_draw_events_dropped)
            .ok_or(())?;
        Ok(delta)
    }

    fn phase_report(
        &self,
        app: &MakerCalcGpuiApp,
        elapsed: Duration,
        changed_view: bool,
    ) -> String {
        let phases = [
            (FramePhase::Build, "build"),
            (FramePhase::Prepare, "prepare"),
            (FramePhase::Paint, "paint"),
            (FramePhase::Accessibility, "a11y"),
            (FramePhase::Present, "submit"),
        ]
        .map(|(phase, name)| {
            let histogram = &self.frames.phase_histograms[phase as usize];
            format!(
                "{name}_samples={} {name}_p50_ms={} {name}_p95_ms={}",
                histogram.len(),
                milliseconds(histogram.len(), histogram.value_at_quantile(0.5)),
                milliseconds(histogram.len(), histogram.value_at_quantile(0.95))
            )
        })
        .join(" ");
        format!(
            "view_at_sample={} view_changed={} period_ms={} width={} height={} {phases}",
            view(app),
            changed_view,
            elapsed.as_millis(),
            self.viewport[0],
            self.viewport[1]
        )
    }

    fn report(&self, app: &MakerCalcGpuiApp, elapsed: Duration, changed_view: bool) -> String {
        let printer = app.selected.and_then(|id| app.printers.get(&id));
        let frames = &self.frames.draw_duration_histogram;
        let input = &self.input.latency_histogram;
        let present = &self.frames.dirty_to_present_histogram;
        let gaps = &self.frames.present_interval_histogram;
        format!(
            "view_at_sample={} view_changed={} visible={} period_ms={} frames={} draw_p50_ms={} draw_p95_ms={} draw_max_ms={} input_samples={} input_p95_ms={} input_max_ms={} mid_draw_input={} ready_p95_ms={} anim_gap_p95_ms={} printers={} devices={} files={} temperatures={} camera_bytes={}",
            view(app),
            changed_view,
            app.visual_active,
            elapsed.as_millis(),
            frames.len(),
            milliseconds(frames.len(), frames.value_at_quantile(0.5)),
            milliseconds(frames.len(), frames.value_at_quantile(0.95)),
            milliseconds(frames.len(), frames.max()),
            input.len(),
            milliseconds(input.len(), input.value_at_quantile(0.95)),
            milliseconds(input.len(), input.max()),
            self.input.mid_draw_events_dropped,
            milliseconds(present.len(), present.value_at_quantile(0.95)),
            milliseconds(gaps.len(), gaps.value_at_quantile(0.95)),
            app.printers.len(),
            printer.map_or(0, |p| p.detail_state.devices.len()),
            printer
                .and_then(|p| p.detail_state.files.value.as_ref())
                .map_or(0, Vec::len),
            printer.map_or(0, |p| p.temperature_history.len()),
            printer
                .and_then(|p| p.camera_state.frame.as_ref())
                .map_or(0, bytes::Bytes::len),
        )
    }
}

fn milliseconds(count: u64, nanoseconds: u64) -> String {
    if count == 0 {
        "-".to_owned()
    } else {
        format!("{:.2}", nanoseconds as f64 / 1_000_000.)
    }
}

fn view(app: &MakerCalcGpuiApp) -> &'static str {
    if app.settings_open {
        "settings"
    } else if app.printer_form.is_some() {
        "printer_form"
    } else if app.selected.is_none() {
        "bay"
    } else {
        match app.detail_tab {
            DetailTab::Overview => "overview",
            DetailTab::Files => "files",
            DetailTab::History => "history",
        }
    }
}

pub(super) struct Probe {
    window: AnyWindowHandle,
    last: Instant,
    previous: Snapshot,
    previous_view: &'static str,
}

impl Probe {
    pub(super) fn new(window: &Window, cx: &App) -> Self {
        crate::diagnostics::log(LogEvent::new(
            LogLevel::Info,
            LogArea::App,
            "app.ui_diagnostics.started",
        ));
        Self {
            window: window.window_handle(),
            last: cx.background_executor().now(),
            previous: Snapshot::capture(window),
            previous_view: "bay",
        }
    }

    pub(super) fn poll(&mut self, app: &MakerCalcGpuiApp, cx: &mut Context<MakerCalcGpuiApp>) {
        let now = cx.background_executor().now();
        let elapsed = now.saturating_duration_since(self.last);
        if elapsed < PERIOD {
            return;
        }
        // Window closure during shutdown is expected; the owning timer is dropped with the app.
        let Ok(next) = self
            .window
            .update(cx, |_, window, _| Snapshot::capture(window))
        else {
            return;
        };
        let delta = Snapshot::interval(&self.previous, &next);
        self.previous = next;
        self.last = now;
        let changed_view = self.previous_view != view(app);
        self.previous_view = view(app);
        match delta {
            Ok(delta) => {
                crate::diagnostics::log(
                    LogEvent::new(LogLevel::Info, LogArea::App, "app.ui_performance")
                        .detail(delta.report(app, elapsed, changed_view)),
                );
                crate::diagnostics::log(
                    LogEvent::new(LogLevel::Info, LogArea::App, "app.ui_frame_phases")
                        .detail(delta.phase_report(app, elapsed, changed_view)),
                );
            }
            Err(()) => crate::diagnostics::log(LogEvent::new(
                LogLevel::Warn,
                LogArea::App,
                "app.ui_diagnostics.counters_reset",
            )),
        }
    }
}
