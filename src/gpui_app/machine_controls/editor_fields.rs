use super::intent::parse_decimal;
use super::localized_decimal;
use super::{Control, Editor, PrinterEntry, ResourceId, control_device};
use crate::gpui_app::components::{button::Button, card};
use crate::gpui_app::{MakerCalcGpuiApp, icons};
use crate::i18n::t;
use gpui_kit::assets::IconName;
use gpui_kit::base::StyledExt as _;
use gpui_kit::component::input::InputState;
use gpui_kit::component::slider::{Slider, SliderState};
use gpui_kit::component::{ActiveTheme as _, Icon, h_flex, v_flex};
use gpui_kit::{
    Animation, AnimationExt as _, AnyElement, App, Context, Div, Entity, IntoElement,
    ParentElement as _, Styled as _, Window, div, px,
};
use std::time::Duration;

const Z_STEP_MM: f64 = 0.01;

pub(super) fn fan_reading(printer: &PrinterEntry, id: ResourceId) -> Option<f64> {
    control_device(printer, id)?
        .output_pct
        .filter(|value| (0.0..=100.0).contains(value))
}

pub(super) fn fan_hint([min, max]: [u8; 2], reading: Option<f64>) -> String {
    let Some(reading) = reading else {
        return t!("printers.controls.fan_unreported", min = min, max = max).into_owned();
    };
    let decimal = format!("{reading:.2}");
    let value = localized_decimal(decimal.trim_end_matches('0').trim_end_matches('.'));
    t!(
        "printers.controls.fan_reported",
        value = value,
        min = min,
        max = max
    )
    .into_owned()
}

pub(super) fn speed_field(
    slider: &Entity<SliderState>,
    [min, max]: [u16; 2],
    observed: Option<f64>,
    label: &str,
    disabled: bool,
    cx: &gpui_kit::App,
) -> AnyElement {
    let reported = observed.map_or_else(
        || t!("printers.controls.speed_unreported").into_owned(),
        |value| {
            let decimal = format!("{value:.2}");
            let value = decimal.trim_end_matches('0').trim_end_matches('.');
            let value = localized_decimal(value);
            t!("printers.controls.speed_observed", value = value).into_owned()
        },
    );
    let selected = slider.read(cx).value().end();
    let muted = cx.theme().muted_foreground;
    v_flex()
        .gap_2()
        .child(
            h_flex()
                .justify_between()
                .gap_2()
                .text_sm()
                .child(div().text_color(muted).child(reported))
                .child(
                    div()
                        .font_semibold()
                        .child(t!("printers.controls.speed_selected", value = selected)),
                ),
        )
        .child(
            Slider::new(slider)
                .accessibility_label(label.to_owned())
                .disabled(disabled),
        )
        .child(
            h_flex()
                .justify_between()
                .text_xs()
                .text_color(muted)
                .child(format!("{min}%"))
                .child(format!("{max}%")),
        )
        .into_any_element()
}

fn z_offset_value(input: &Entity<InputState>, cx: &App) -> f64 {
    // An unreported offset leaves the draft empty; steps then start from zero.
    parse_decimal(input.read(cx).value().as_ref())
        .ok()
        .filter(|value| value.is_finite())
        .unwrap_or(0.)
}

fn step_z_offset(
    input: &Entity<InputState>,
    [min, max]: [f64; 2],
    delta_mm: f64,
    window: &mut Window,
    cx: &mut App,
) {
    // Avoid accumulated float error and negative zero.
    let stepped = ((z_offset_value(input, cx) + delta_mm) * 1000.).round() / 1000. + 0.;
    let text = format!("{:.3}", stepped.clamp(min, max));
    let text = localized_decimal(text.strip_suffix('0').unwrap_or(&text));
    input.update(cx, |input, cx| input.set_value(text, window, cx));
}

fn z_step_button(
    input: &Entity<InputState>,
    range: [f64; 2],
    positive: bool,
    disabled: bool,
    cx: &mut Context<MakerCalcGpuiApp>,
) -> Button {
    let step = localized_decimal(&Z_STEP_MM.to_string());
    let (id, icon, sign, delta_mm, label) = if positive {
        (
            "machine-z-offset-up",
            IconName::ChevronUp,
            "+",
            Z_STEP_MM,
            t!("printers.controls.z_offset_raise", step = step.as_str()),
        )
    } else {
        (
            "machine-z-offset-down",
            IconName::ChevronDown,
            "−",
            -Z_STEP_MM,
            t!("printers.controls.z_offset_lower", step = step.as_str()),
        )
    };
    let input = input.clone();
    Button::new(id)
        .outline()
        .w(px(52.))
        .h(px(46.))
        .p_0()
        .accessibility_label(label.into_owned())
        .disabled(disabled)
        .child(
            v_flex()
                .items_center()
                .gap_1()
                .child(Icon::new(icon).size(px(16.)))
                .child(
                    div()
                        .text_size(px(11.))
                        .line_height(px(12.))
                        .child(format!("{sign}{step}")),
                ),
        )
        .on_click(cx.listener(move |_, _, window, cx| {
            step_z_offset(&input, range, delta_mm, window, cx);
            cx.notify();
        }))
}

/// Illustrates nozzle clearance.
fn z_offset_stage(cx: &App) -> Div {
    let theme = cx.theme();
    let (accent, nozzle) = (theme.primary, theme.foreground);
    div()
        .flex()
        .flex_1()
        .min_w_0()
        .h(px(154.))
        .justify_center()
        .rounded(px(10.))
        .bg(theme.group_box)
        .shadow(card::ring(theme.border))
        .overflow_hidden()
        .child(
            div()
                .relative()
                .flex_none()
                .w(px(160.))
                .h_full()
                .child(
                    div()
                        .absolute()
                        .left(px(0.))
                        .top(px(124.))
                        .w_full()
                        .h(px(10.))
                        .rounded(px(3.))
                        .border_1()
                        .border_color(theme.muted_foreground)
                        .bg(theme.muted),
                )
                .child(
                    div()
                        .absolute()
                        .left(px(131.))
                        .top(px(102.))
                        .text_size(px(11.))
                        .line_height(px(12.))
                        .font_medium()
                        .text_color(accent)
                        .child("Z"),
                )
                .with_animation(
                    "machine-z-offset-nozzle",
                    Animation::new(Duration::from_millis(3200))
                        .repeat_synced()
                        .with_max_fps(24.),
                    move |scene, progress| {
                        // Progress 0 is the mid height, which reduced motion shows as a still.
                        let lift = 14. + 14. * (progress * std::f32::consts::TAU).sin();
                        scene
                            .child(
                                div().absolute().left(px(0.)).top(px(-42. - lift)).child(
                                    Icon::default()
                                        .data(icons::SVG_Z_NOZZLE)
                                        .size(px(160.))
                                        .text_color(nozzle),
                                ),
                            )
                            .child(
                                div()
                                    .absolute()
                                    .left(px(123.))
                                    .top(px(118. - lift))
                                    .w(px(2.))
                                    .h(px(6. + lift))
                                    .bg(accent),
                            )
                    },
                ),
        )
}

pub(super) fn z_offset_field(
    input: &Entity<InputState>,
    range @ [min, max]: [f64; 2],
    disabled: bool,
    cx: &mut Context<MakerCalcGpuiApp>,
) -> AnyElement {
    let current = z_offset_value(input, cx);
    let draft = input.read(cx).value();
    let readout = if draft.is_empty() {
        "—".into()
    } else {
        draft
    };
    h_flex()
        .gap(px(16.))
        .child(z_offset_stage(cx))
        .child(
            v_flex()
                .flex_none()
                .items_center()
                .gap_2()
                .child(z_step_button(
                    input,
                    range,
                    true,
                    disabled || current >= max,
                    cx,
                ))
                .child(
                    v_flex()
                        .items_center()
                        .justify_center()
                        .min_w(px(88.))
                        .h(px(46.))
                        .child(
                            div()
                                .text_size(px(24.))
                                .line_height(px(28.))
                                .font_semibold()
                                .child(readout),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("mm"),
                        ),
                )
                .child(z_step_button(
                    input,
                    range,
                    false,
                    disabled || current <= min,
                    cx,
                )),
        )
        .into_any_element()
}

pub(super) fn submit_label(editor: &Editor) -> String {
    match editor.choice.control {
        Control::Speed => t!("printers.controls.speed_apply").into_owned(),
        Control::ZOffset => t!("printers.controls.z_offset_apply").into_owned(),
        Control::ClearPlate => t!("printers.controls.clear_plate").into_owned(),
        Control::AutoShutdown(_) | Control::Calibration(_) => editor.choice.label.clone(),
        Control::Extrude(_) => t!("printers.controls.extrude_apply").into_owned(),
        Control::Fan(_) if editor.fan_range.is_some() => {
            t!("printers.controls.fan_apply").into_owned()
        }
        Control::Name => t!("printers.controls.name_apply").into_owned(),
        _ => t!("printers.controls.send").into_owned(),
    }
}
