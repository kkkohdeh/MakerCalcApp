use crate::i18n::t;
use gpui_kit::base::{Align, Placement, Positioner, StyledExt as _, TestSupportExt as _};
use gpui_kit::component::plot::{
    Grid, IntoPlot, Plot, StrokeStyle,
    scale::{Scale as _, ScaleLinear},
    shape::{Area, Line},
    tooltip::{Dot, TooltipState},
};
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, Bounds, ElementId, Entity, FocusHandle, InteractiveElement as _, IntoElement,
    ParentElement as _, Pixels, Point, RenderOnce, Role, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, deferred, div, point, px, size,
};

use crate::state::{LanConnection, LocalPrinterId};

use super::PrinterEntry;

// Chart body, including its header; the shared card adds 32px of padding.
pub(super) const CHART_MIN: f32 = 108.;
pub(super) const CHART_MAX: f32 = 168.;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TemperaturePoint {
    pub(crate) time: SharedString,
    pub(crate) sampled_at: std::time::Instant,
    pub(crate) nozzle: f64,
    pub(crate) bed: f64,
    pub(crate) nozzle_target: Option<f64>,
    pub(crate) bed_target: Option<f64>,
}

fn reading_value(value: f64, target: Option<f64>) -> String {
    match target.filter(|target| *target > 0.) {
        Some(target) => format!("{value:.1} / {target:.0} °C"),
        None => format!("{value:.1} °C"),
    }
}

fn sample_label(point: &TemperaturePoint, locale: &str) -> String {
    t!(
        "printers.charts.sample",
        locale = locale,
        time = &point.time,
        nozzle = reading_value(point.nozzle, point.nozzle_target),
        bed = reading_value(point.bed, point.bed_target)
    )
    .into_owned()
}

fn tooltip_content(
    point: &TemperaturePoint,
    max_width: Pixels,
    cx: &App,
) -> impl IntoElement + use<> {
    v_flex()
        .id("temperature-tooltip")
        .test_support()
        .role(Role::Tooltip)
        .aria_label(sample_label(point, rust_i18n::locale().as_ref()))
        .min_w(px(128.))
        .max_w(max_width)
        .bg(cx.theme().popover)
        .text_color(cx.theme().popover_foreground)
        .border_1()
        .border_color(cx.theme().border.opacity(0.5))
        .rounded(px(10.))
        .shadow_xl()
        .px(px(10.))
        .py(px(6.))
        .gap(px(6.))
        .text_size(px(12.))
        .line_height(px(16.))
        .child(
            h_flex()
                .justify_between()
                .gap(px(8.))
                .font_medium()
                .child(
                    div()
                        .text_color(cx.theme().muted_foreground)
                        .child(t!("printers.charts.time")),
                )
                .child(div().child(point.time.clone())),
        )
        .children(
            [
                (
                    "nozzle",
                    cx.theme().blue,
                    t!("printers.charts.nozzle").into_owned(),
                    reading_value(point.nozzle, point.nozzle_target),
                ),
                (
                    "bed",
                    cx.theme().warning,
                    t!("printers.charts.bed").into_owned(),
                    reading_value(point.bed, point.bed_target),
                ),
            ]
            .into_iter()
            .map(|(id, color, label, value)| {
                h_flex()
                    .id(id)
                    .test_support()
                    .items_center()
                    .flex_wrap()
                    .gap(px(8.))
                    .w_full()
                    .child(
                        div()
                            .id((ElementId::from(id), "indicator"))
                            .test_support()
                            .flex_none()
                            .size(px(10.))
                            .rounded(px(2.))
                            .bg(color),
                    )
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .justify_between()
                            .gap(px(8.))
                            .line_height(px(12.))
                            .child(div().text_color(cx.theme().muted_foreground).child(label))
                            .child(
                                div()
                                    .font_family(cx.theme().mono_font_family.clone())
                                    .font_medium()
                                    .child(value),
                            ),
                    )
            }),
        )
}

struct ChartInteraction {
    focus: FocusHandle,
    index: Option<usize>,
}

#[derive(IntoElement)]
struct TemperaturePlot {
    printer: LocalPrinterId,
    data: Vec<TemperaturePoint>,
}
impl RenderOnce for TemperaturePlot {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let interaction =
            window.use_keyed_state(("temperature-chart-input", self.printer.0), cx, |_, cx| {
                ChartInteraction {
                    focus: cx.focus_handle(),
                    index: None,
                }
            });
        let input = interaction.read(cx);
        let focus = input.focus.clone().tab_stop(true);
        let label = input
            .index
            .and_then(|index| self.data.get(index))
            .map(|point| sample_label(point, rust_i18n::locale().as_ref()))
            .unwrap_or_else(|| t!("printers.charts.title").into_owned());
        let length = self.data.len();
        let keys = interaction.clone();
        let pointer = interaction.clone();
        let mut chart = TemperatureChart::new(self.printer, self.data);
        chart.keyboard = Some(interaction);
        // The plot fills its assigned viewport; its width must not set the row height.
        div()
            .id(("temperature-chart", self.printer.0))
            .test_support()
            .role(Role::GraphicsDocument)
            .aria_label(label)
            .track_focus(&focus)
            .relative()
            .flex()
            .justify_center()
            .size_full()
            .min_w_0()
            .text_size(px(12.))
            .line_height(px(16.))
            .child(chart)
            .on_key_down(move |event, window, cx| {
                if !focus.is_focused(window) || length == 0 {
                    return;
                }
                let input = keys.read(cx);
                let index = input.index.unwrap_or(0).min(length - 1);
                let next = match event.keystroke.key.as_str() {
                    "left" => Some(index.saturating_sub(1)),
                    "right" => Some(if input.index.is_some() {
                        (index + 1).min(length - 1)
                    } else {
                        0
                    }),
                    "home" => Some(0),
                    "end" => Some(length - 1),
                    "escape" => None,
                    _ => return,
                };
                keys.update(cx, |input, cx| {
                    input.index = next;
                    cx.notify();
                });
                cx.stop_propagation();
            })
            .on_mouse_move(move |_, _, cx| {
                pointer.update(cx, |input, cx| {
                    if input.index.take().is_some() {
                        cx.notify();
                    }
                });
            })
    }
}

#[derive(IntoPlot)]
pub(super) struct TemperatureChart {
    data: Vec<TemperaturePoint>,
    id: ElementId,
    keyboard: Option<Entity<ChartInteraction>>,
}

impl TemperatureChart {
    pub(super) fn new(printer_id: LocalPrinterId, data: Vec<TemperaturePoint>) -> Self {
        Self {
            data,
            id: ElementId::NamedInteger("temp-area-chart".into(), printer_id.0),
            keyboard: None,
        }
    }

    fn time_scale(&self, width: f32) -> Option<(std::time::Instant, ScaleLinear<f64>)> {
        let origin = self.data.first()?.sampled_at;
        let span = self
            .data
            .last()?
            .sampled_at
            .duration_since(origin)
            .as_secs_f64();
        Some((
            origin,
            ScaleLinear::new(vec![0.0, span.max(1.0)], vec![0.0, width]),
        ))
    }

    fn state_for_index(&self, index: usize, bounds: Bounds<Pixels>) -> Option<TooltipState> {
        let sample = self.data.get(index)?;
        let (origin, x_scale) = self.time_scale(bounds.size.width.as_f32())?;
        let y_scale = ScaleLinear::new(
            vec![0.0, axis_top(&self.data)],
            vec![bounds.size.height.as_f32(), 4.0],
        );
        let x = px(x_scale.tick(&sample.sampled_at.duration_since(origin).as_secs_f64())?);
        Some(TooltipState::new(
            index,
            point(x, px(0.)),
            vec![
                point(x, px(y_scale.tick(&sample.nozzle)?)),
                point(x, px(y_scale.tick(&sample.bed)?)),
            ],
        ))
    }
}

impl Plot for TemperatureChart {
    fn prepaint(
        &mut self,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut App,
    ) -> Vec<AnyElement> {
        let index = self.keyboard.as_ref().and_then(|input| {
            let input = input.read(cx);
            input
                .focus
                .is_focused(window)
                .then_some(input.index)
                .flatten()
        });
        let Some(state) = index.and_then(|index| self.state_for_index(index, bounds)) else {
            return Vec::new();
        };
        let Some(mut overlay) = self.tooltip(&state, state.cross_line, bounds, window, cx) else {
            return Vec::new();
        };
        overlay.prepaint_as_root(bounds.origin, bounds.size.into(), window, cx);
        vec![overlay]
    }
    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn paint(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        if self.data.is_empty() || bounds.size.height <= px(10.) || bounds.size.width <= px(10.) {
            return;
        }

        let height = bounds.size.height.as_f32();
        let width = bounds.size.width.as_f32();

        let Some((origin, x_scale)) = self.time_scale(width) else {
            return;
        };
        let y_scale = ScaleLinear::new(vec![0.0, axis_top(&self.data)], vec![height, 4.0]);

        Grid::new()
            .y((0..=3).map(|i| height * i as f32 / 3.0).collect())
            .stroke(cx.theme().border)
            .dash_array(&[px(4.), px(2.)])
            .paint(&bounds, window);

        let blue = cx.theme().blue;
        let bed_color = cx.theme().warning;

        let x_scale_nozzle = x_scale.clone();
        let y_scale_nozzle = y_scale.clone();
        Area::new()
            .data(&self.data)
            .x(move |d: &&TemperaturePoint| {
                x_scale_nozzle.tick(&d.sampled_at.duration_since(origin).as_secs_f64())
            })
            .y0(height)
            .y1(move |d: &&TemperaturePoint| y_scale_nozzle.tick(&d.nozzle))
            .stroke(blue)
            .stroke_style(StrokeStyle::Natural)
            .fill(blue.opacity(0.18))
            .paint(&bounds, window);

        let x_scale_bed = x_scale.clone();
        let y_scale_bed = y_scale.clone();
        Line::new()
            .data(&self.data)
            .x(move |d: &&TemperaturePoint| {
                x_scale_bed.tick(&d.sampled_at.duration_since(origin).as_secs_f64())
            })
            .y(move |d: &&TemperaturePoint| y_scale_bed.tick(&d.bed))
            .stroke(bed_color)
            .stroke_width(px(1.8))
            .stroke_style(StrokeStyle::Natural)
            .paint(&bounds, window);

        if let Some(last_bed_target) = self
            .data
            .last()
            .and_then(|p| p.bed_target)
            .filter(|target| *target > 0.0)
        {
            let x_scale_target = x_scale.clone();
            let y_scale_target = y_scale.clone();
            Line::new()
                .data(&self.data)
                .x(move |d: &&TemperaturePoint| {
                    x_scale_target.tick(&d.sampled_at.duration_since(origin).as_secs_f64())
                })
                .y(move |_| y_scale_target.tick(&last_bed_target))
                .stroke(bed_color.opacity(0.45))
                .stroke_width(px(1.0))
                .paint(&bounds, window);
        }

        if let Some(last_nozzle_target) = self
            .data
            .last()
            .and_then(|p| p.nozzle_target)
            .filter(|target| *target > 0.0)
        {
            let x_scale_target = x_scale.clone();
            let y_scale_target = y_scale.clone();
            Line::new()
                .data(&self.data)
                .x(move |d: &&TemperaturePoint| {
                    x_scale_target.tick(&d.sampled_at.duration_since(origin).as_secs_f64())
                })
                .y(move |_| y_scale_target.tick(&last_nozzle_target))
                .stroke(blue.opacity(0.45))
                .stroke_width(px(1.0))
                .paint(&bounds, window);
        }
    }

    fn tooltip_state(
        &self,
        position: Point<Pixels>,
        bounds: Bounds<Pixels>,
        cx: &App,
    ) -> Option<TooltipState> {
        if self.data.is_empty()
            || self
                .keyboard
                .as_ref()
                .is_some_and(|input| input.read(cx).index.is_some())
        {
            return None;
        }

        let width = bounds.size.width.as_f32();
        let (origin, x_scale) = self.time_scale(width)?;

        let times: Vec<_> = self
            .data
            .iter()
            .map(|d| d.sampled_at.duration_since(origin).as_secs_f64())
            .collect();
        let (index, _) = x_scale.least_index_with_domain(position.x.as_f32(), &times);
        let mut state = self.state_for_index(index, bounds)?;
        state.cross_line.y = position.y;
        Some(state)
    }

    fn tooltip(
        &self,
        state: &TooltipState,
        cursor: Point<Pixels>,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        let d = self.data.get(state.index)?;
        let nozzle_color = cx.theme().blue;
        let bed_color = cx.theme().warning;

        let dot_nozzle = state.dots.first().copied();
        let dot_bed = state.dots.get(1).copied();

        let tooltip_box = deferred(
            Positioner::side(Bounds::new(bounds.origin + cursor, size(px(0.), px(0.))))
                .placement(Placement::Right)
                .align(Align::Start)
                .offset(px(10.))
                .margin(px(4.))
                .child(tooltip_content(
                    d,
                    (window.viewport_size().width - px(8.)).max(px(128.)),
                    cx,
                )),
        );

        let cross_line = div()
            .absolute()
            .left(state.cross_line.x)
            .top_0()
            .w(px(1.))
            .h_full()
            .bg(cx.theme().border);

        let mut overlay = div()
            .size_full()
            .absolute()
            .top_0()
            .left_0()
            .child(cross_line);

        if let Some(p) = dot_nozzle {
            overlay = overlay.child(Dot::new(p).fill(nozzle_color));
        }
        if let Some(p) = dot_bed {
            overlay = overlay.child(Dot::new(p).fill(bed_color));
        }

        Some(overlay.child(tooltip_box).into_any_element())
    }
}

/// Top of the Y axis: the plotted area, its tooltip and its label share this scale.
fn axis_top(points: &[TemperaturePoint]) -> f64 {
    let max = points
        .iter()
        .map(|p| {
            p.nozzle
                .max(p.bed)
                .max(p.nozzle_target.unwrap_or(f64::NEG_INFINITY))
                .max(p.bed_target.unwrap_or(f64::NEG_INFINITY))
        })
        .fold(60.0_f64, f64::max);
    max * 1.15
}

pub(super) fn temperature_history_cell(
    printer_id: LocalPrinterId,
    printer: &PrinterEntry,
    cx: &App,
) -> AnyElement {
    let points: Vec<TemperaturePoint> = printer.temperature_history.iter().cloned().collect();
    let max_temp = axis_top(&points);
    let telemetry_status_label =
        if printer.state.connection == LanConnection::Connected && printer.state.telemetry_ready {
            t!("printers.charts.live").into_owned()
        } else if printer.state.connection == LanConnection::Connected {
            t!("printers.charts.waiting").into_owned()
        } else {
            t!("printers.charts.retained").into_owned()
        };

    v_flex()
        .id("temperature-chart-card")
        .size_full()
        .min_h(px(CHART_MIN))
        .max_h(px(CHART_MAX))
        .gap_3()
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .child(t!("printers.charts.title")),
                )
                .child(div().flex_none().child(telemetry_status_label)),
        )
        .child(
            div()
                .relative()
                .w_full()
                .flex_1()
                .min_h(px(48.))
                .child(TemperaturePlot {
                    printer: printer_id,
                    data: points,
                })
                .child(
                    div()
                        .absolute()
                        .top_1()
                        .right_2()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground.opacity(0.7))
                        .child(format!("{:.0}°C", max_temp)),
                )
                .child(
                    div()
                        .absolute()
                        .bottom_1()
                        .right_2()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground.opacity(0.7))
                        .child("0°C"),
                ),
        )
        .into_any_element()
}
