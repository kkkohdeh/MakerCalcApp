use super::localized_decimal;
use super::{PrinterEntry, ResourceId, control_device};
use crate::i18n::t;
use gpui_kit::base::StyledExt as _;
use gpui_kit::component::slider::{Slider, SliderState};
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::{AnyElement, Entity, IntoElement, ParentElement as _, Styled as _, div};

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
