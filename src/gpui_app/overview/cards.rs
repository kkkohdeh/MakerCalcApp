//! Summary cards except the job card; the app owns job actions.
//! Cards fit an optional main value and rows into the available columns.
//! Related cards share a surface separated by a hairline, without nested boxes.

use crate::i18n::t;
use gpui_kit::base::{StyledExt as _, TestSupportExt as _};
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ElementId, InteractiveElement as _, IntoElement, ParentElement as _, Role,
    StatefulInteractiveElement as _, Styled as _, div, px, relative,
};

use super::super::PrinterEntry;
use super::super::charts::temperature_history_cell;
use super::super::components::{card, separator};
use super::super::format::format_percent;
use super::super::icons::{SVG_BED, SVG_NOZZLE};
use super::super::machine::{Level, Reading, Row, detail_rows, fan_rows, reading_cell};
use super::super::materials::supply_cell;
use super::super::media::camera_cell;
use super::layout::{BlockLayout, Composition, GroupPart, HERO_WIDTH, INNER_GAP, LIST_COLUMN_MAX};
use super::model::{
    CardKind, Content, EnvironmentFact, HeaterFacts, MotionFacts, Supply, reported_camera,
};
use super::readings::{Value, presentation};

fn titled(title: Option<String>, content: AnyElement, cx: &App) -> gpui_kit::Div {
    v_flex()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .gap_3()
        .when_some(title, |part, title| {
            part.child(
                card::title(true)
                    .text_color(cx.theme().muted_foreground)
                    .truncate()
                    .child(title),
            )
        })
        .child(v_flex().flex_1().min_h_0().child(content))
}

fn surface(
    id: String,
    span: Option<u16>,
    cx: &App,
) -> impl gpui_kit::ParentElement + IntoElement + gpui_kit::Styled {
    card::surface(false, cx)
        .id(ElementId::Name(id.into()))
        .test_support()
        .when_some(span, |card, span| card.col_span(span).overflow_hidden())
        .min_w_0()
}

/// Grid cards span layout columns; the job card heads Summary without a grid span.
pub(super) fn card_shell(
    kind: CardKind,
    span: Option<u16>,
    title: Option<String>,
    content: AnyElement,
    cx: &App,
) -> impl IntoElement + gpui_kit::Styled {
    surface(format!("overview-card-{kind:?}"), span, cx).child(
        card::content(false)
            .flex_1()
            .min_h_0()
            .child(titled(title, content, cx)),
    )
}

/// Pair related cards on one surface, side by side or stacked, separated by a hairline.
pub(super) fn pair_shell(
    kinds: (CardKind, CardKind),
    span: u16,
    side_by_side: bool,
    first: (Option<String>, AnyElement),
    second: (Option<String>, AnyElement),
    cx: &App,
) -> impl IntoElement + gpui_kit::Styled {
    let parts = if side_by_side {
        h_flex()
            .size_full()
            .gap_4()
            .items_stretch()
            .child(titled(first.0, first.1, cx))
            .child(separator::vertical(
                format!("overview-pair-separator-{:?}", kinds.0),
                cx,
            ))
            .child(titled(second.0, second.1, cx))
    } else {
        v_flex()
            .size_full()
            .gap_4()
            .child(titled(first.0, first.1, cx).flex_none())
            .child(separator::horizontal(
                format!("overview-pair-separator-{:?}", kinds.0),
                cx,
            ))
            .child(titled(second.0, second.1, cx).flex_none())
    };
    surface(
        format!("overview-card-{:?}+{:?}", kinds.0, kinds.1),
        Some(span),
        cx,
    )
    .child(card::content(false).flex_1().min_h_0().child(parts))
}

/// A shared surface containing related reading blocks, without nested card surfaces.
pub(super) fn group_shell(
    span: u16,
    parts: Vec<(CardKind, (Option<String>, AnyElement), GroupPart)>,
    cx: &App,
) -> impl IntoElement + gpui_kit::Styled {
    let readings = h_flex()
        .w_full()
        .flex_wrap()
        .items_start()
        .gap(px(INNER_GAP))
        .children(parts.into_iter().map(|(kind, part, layout)| {
            div()
                .id(ElementId::Name(format!("overview-card-{kind:?}").into()))
                .test_support()
                .w(px(layout.width))
                .flex_none()
                .min_w_0()
                .child(titled(part.0, part.1, cx).flex_none())
        }));
    surface("overview-card-Temperatures".into(), Some(span), cx)
        .child(card::content(false).child(readings))
}

/// Label/value rows split into `columns` columns of similar length, read top to bottom.
fn list(id: &str, rows: Vec<Row>, columns: usize, muted: bool, cx: &App) -> AnyElement {
    list_with_status(id, rows, columns, muted, &[], None, cx)
}

fn list_with_status(
    id: &str,
    rows: Vec<Row>,
    columns: usize,
    muted: bool,
    states: &[crate::shared::workspace::ReadStatus],
    width: Option<f32>,
    cx: &App,
) -> AnyElement {
    let columns = columns.clamp(1, rows.len().max(1));
    let per_column = rows.len().div_ceil(columns).max(1);
    let mut split = (0..columns).map(|_| Vec::new()).collect::<Vec<_>>();
    let mut items = Vec::new();
    for (index, row) in rows.into_iter().enumerate() {
        let long = id == "details" && row.value.chars().count() > 32;
        let description = t!(
            "printers.bay.metric",
            label = &row.label,
            value = &row.value
        )
        .into_owned();
        let element = h_flex()
            .id(ElementId::Name(format!("overview-{id}-{index}").into()))
            .role(Role::Group)
            .aria_label(description.clone())
            .test_support()
            .min_h(px(22.))
            .gap_3()
            .items_center()
            .when(long, |row| row.flex_col().items_start().gap_1())
            .text_sm()
            .font_features(gpui_kit::FontFeatures(std::sync::Arc::new(vec![(
                "tnum".into(),
                1,
            )])))
            .child(
                div()
                    .id(ElementId::Name(
                        format!("overview-{id}-{index}-label").into(),
                    ))
                    .test_support()
                    .flex_1()
                    .min_w_0()
                    .whitespace_normal()
                    .text_color(cx.theme().muted_foreground)
                    .child(row.label.clone()),
            )
            .when_some(states.get(index), |row, &state| {
                row.children(super::readings::view::state_indicator(
                    format!("overview-{id}-{index}"),
                    state,
                    cx,
                ))
            })
            .child(
                div()
                    .id(ElementId::Name(
                        format!("overview-{id}-{index}-value").into(),
                    ))
                    .test_support()
                    .min_w_0()
                    .when(long, |value| value.w_full())
                    .when(!long, |value| {
                        value
                            .flex_shrink_0()
                            .max_w(relative(0.65))
                            .whitespace_normal()
                    })
                    .when(id == "details", |value| value.font_normal())
                    .when(id != "details", |value| value.font_medium())
                    .when(muted, |value| value.text_color(cx.theme().muted_foreground))
                    .child(if long {
                        super::readings::view::long_value(
                            ElementId::Name(format!("overview-{id}-{index}-full").into()),
                            row.label.clone(),
                            row.value.clone(),
                            row.value.chars().take(28).collect::<String>() + "…",
                            muted,
                        )
                    } else {
                        div()
                            .child(crate::gpui_app::feedback::break_long_words(&row.value, 16))
                            .into_any_element()
                    }),
            )
            .children(row.action)
            .into_any_element();
        if width.is_some() {
            items.push(super::flow::Item {
                key: format!("{id}-{index}"),
                group: 0,
                heading: None,
                content: element,
            });
        } else {
            split[index / per_column].push(element);
        }
    }
    if let Some(width) = width {
        let fields = super::flow::Flow::new(
            ElementId::Name(format!("overview-{id}-fields").into()),
            width,
            columns,
            INNER_GAP,
            super::flow::Mode::Readings,
            items,
        )
        .into_any_element();
        return div()
            .id(ElementId::Name(format!("overview-{id}").into()))
            .test_support()
            .min_w_0()
            .child(fields)
            .into_any_element();
    }
    div()
        .id(ElementId::Name(format!("overview-{id}").into()))
        .test_support()
        // Limit width to keep values near labels; layout avoids remaining empty space.
        .max_w(px(columns as f32 * LIST_COLUMN_MAX))
        // Equal-width columns need one-dimensional layout, avoiding nested grid measurement.
        .flex()
        .flex_row()
        .items_start()
        .gap_8()
        .children(
            split
                .into_iter()
                .map(|column| v_flex().flex_1().min_w_0().gap_0p5().children(column)),
        )
        .into_any_element()
}

/// Place rows beside the main value when wide enough, otherwise below.
fn led_by(
    hero: AnyElement,
    id: &str,
    rows: Vec<Row>,
    layout: BlockLayout,
    muted: bool,
    cx: &App,
) -> AnyElement {
    let hero = div()
        .max_w(px(HERO_WIDTH))
        .min_w_0()
        .child(hero)
        .into_any_element();
    if rows.is_empty() {
        return hero;
    }
    let rows = list(id, rows, layout.columns, muted, cx);
    if layout.beside {
        h_flex()
            .gap_8()
            .items_start()
            .child(div().w(px(HERO_WIDTH)).flex_none().child(hero))
            .child(div().flex_1().min_w_0().child(rows))
            .into_any_element()
    } else {
        v_flex().gap_3().child(hero).child(rows).into_any_element()
    }
}

fn heater(
    kind: CardKind,
    heater: &HeaterFacts,
    printer: &PrinterEntry,
    layout: BlockLayout,
    muted: bool,
    cx: &App,
) -> AnyElement {
    let telemetry = &printer.state.telemetry;
    let charted = !printer.temperature_history.is_empty();
    let (name, icon, index, many, legend) = match kind {
        CardKind::Nozzle(index) => (
            t!("printers.overview.nozzle"),
            SVG_NOZZLE,
            index,
            telemetry.nozzles.len().max(
                printer
                    .detail_state
                    .devices
                    .iter()
                    .filter(|device| device.kind == crate::shared::workspace::DeviceKind::Nozzle)
                    .count(),
            ) > 1,
            // The plot follows the active nozzle and the first bed.
            (charted && heater.active == Some(true)).then_some(cx.theme().blue),
        ),
        _ => {
            let index = match kind {
                CardKind::Bed(index) => index,
                _ => 0,
            };
            (
                t!("printers.overview.bed"),
                SVG_BED,
                index,
                telemetry.beds.len().max(
                    printer
                        .detail_state
                        .devices
                        .iter()
                        .filter(|device| device.kind == crate::shared::workspace::DeviceKind::Bed)
                        .count(),
                ) > 1,
                (charted && index == 0).then_some(cx.theme().warning),
            )
        }
    };
    let label = if many {
        t!(
            "printers.overview.numbered",
            label = name,
            number = index + 1
        )
        .into_owned()
    } else {
        name.into_owned()
    };
    let label = match (kind, heater.active) {
        (CardKind::Nozzle(_), Some(active)) => t!(
            "printers.overview.nozzle_state_label",
            label = label,
            state = if active {
                t!("printers.overview.active_toolhead")
            } else {
                t!("printers.overview.inactive_toolhead")
            }
        )
        .into_owned(),
        _ => label,
    };
    let rows = [
        heater.diameter_mm.map(|diameter| {
            text_row(
                t!("printers.overview.nozzle_diameter").into_owned(),
                format!("{diameter:.1} mm"),
            )
        }),
        heater.power_pct.map(|power| {
            text_row(
                t!("printers.overview.output_power").into_owned(),
                format_percent(Some(power)),
            )
        }),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    let id = format!("{kind:?}");
    // Without a temperature or a target there is no main value, only the rows.
    if heater.current_c.is_none() && heater.target_c.is_none() {
        return titled(Some(label), list(&id, rows, layout.columns, muted, cx), cx)
            .flex_none()
            .into_any_element();
    }
    let hero = reading_cell(
        ElementId::Name(format!("overview-{id}").into()),
        Reading {
            icon: Some(icon),
            legend,
            level: Level::Primary,
            muted,
            ..Reading::temperature(label, heater.current_c, heater.target_c)
        },
        cx,
    );
    led_by(hero, &id, rows, layout, muted, cx)
}

fn materials(
    supplies: &[Supply<'_>],
    columns: usize,
    printer: &PrinterEntry,
    cx: &mut gpui_kit::Context<super::super::MakerCalcGpuiApp>,
) -> AnyElement {
    let grid = div()
        .grid()
        .grid_cols(columns.max(1) as u16)
        .gap_x_6()
        .gap_y_4()
        .children(supplies.iter().enumerate().map(|(index, supply)| {
            supply_cell(
                ElementId::Name(format!("overview-Supply({index})").into()),
                supply.reading,
                supply.external,
                supply.resource,
                printer,
                cx,
            )
        }))
        .into_any_element();
    v_flex()
        .gap_3()
        .child(super::super::materials::panel_header(printer, cx))
        .child(grid)
        .into_any_element()
}

fn environment_rows(sensors: &[EnvironmentFact]) -> Vec<Row> {
    let many = sensors.len() > 1;
    let label = |key: String, sensor: &EnvironmentFact| {
        if many {
            t!(
                "printers.overview.sensor_reading",
                label = key,
                name = &sensor.name
            )
            .into_owned()
        } else {
            key
        }
    };
    sensors
        .iter()
        .flat_map(|sensor| {
            [
                sensor.temperature_c.map(|value| {
                    text_row(
                        label(t!("printers.overview.temperature").into_owned(), sensor),
                        format!("{value:.0}°"),
                    )
                }),
                sensor.relative_humidity_pct.map(|value| {
                    text_row(
                        label(t!("printers.overview.humidity").into_owned(), sensor),
                        format_percent(Some(value)),
                    )
                }),
                sensor.pressure_pa.map(|value| {
                    text_row(
                        label(t!("printers.overview.pressure").into_owned(), sensor),
                        format!("{:.0} hPa", value / 100.),
                    )
                }),
            ]
        })
        .flatten()
        .collect()
}

/// Position as one row per axis, so each value stays short enough to read whole.
fn motion_rows(motion: &MotionFacts) -> Vec<Row> {
    let mut rows = motion
        .position_mm
        .into_iter()
        .flat_map(|position| {
            ["X", "Y", "Z"]
                .into_iter()
                .zip(position)
                .map(|(axis, value)| text_row(axis.to_owned(), format!("{value:.2} mm")))
        })
        .collect::<Vec<_>>();
    rows.extend(motion.homed_axes.map(|axes| {
        text_row(
            t!("printers.overview.homed_axes").into_owned(),
            presentation::value(Value::Axes(axes), rust_i18n::locale().as_ref()),
        )
    }));
    rows
}

fn text_row(label: String, value: String) -> Row {
    Row {
        label,
        value,
        action: None,
    }
}

pub(super) fn content(
    kind: CardKind,
    content: &Content<'_>,
    composition: Composition,
    printer: &PrinterEntry,
    muted: bool,
    cx: &mut gpui_kit::Context<super::super::MakerCalcGpuiApp>,
) -> (Option<String>, AnyElement) {
    let layout = composition.block;
    let list_columns = layout.columns.max(1);
    match content {
        // The job card is rendered by the app, which owns its actions.
        Content::Job => (None, div().into_any_element()),
        Content::Heater(facts) => (None, heater(kind, facts, printer, layout, muted, cx)),
        Content::Camera(camera) => (None, camera_cell(printer, *camera, cx)),
        Content::Materials(supplies) => (
            None,
            materials(supplies, composition.spool_columns, printer, cx),
        ),
        Content::TemperatureHistory => (
            None,
            temperature_history_cell(printer.state.id, printer, cx),
        ),
        Content::Chamber {
            temperature_c,
            fans,
        } => {
            let title = t!("printers.overview.chamber").into_owned();
            let rows = fan_rows(fans);
            match temperature_c {
                // The temperature leads, and its label names the card.
                Some(value) => {
                    let hero = reading_cell(
                        ElementId::Name("overview-Chamber".into()),
                        Reading {
                            level: Level::Primary,
                            muted,
                            ..Reading::temperature(title, Some(*value), None)
                        },
                        cx,
                    );
                    (None, led_by(hero, "Chamber", rows, layout, muted, cx))
                }
                None => (Some(title), list("Chamber", rows, list_columns, muted, cx)),
            }
        }
        Content::Motion(motion) => (
            Some(t!("printers.overview.motion").into_owned()),
            list("Motion", motion_rows(motion), list_columns, muted, cx),
        ),
        Content::Environment(sensors) => (
            Some(t!("printers.overview.environment").into_owned()),
            list(
                "Environment",
                environment_rows(sensors),
                list_columns,
                muted,
                cx,
            ),
        ),
        Content::Machine {
            z_offset_mm,
            auto_shutdown,
            filament_sensors,
            fans,
        } => {
            let mut rows = Vec::new();
            rows.extend(z_offset_mm.map(|offset| {
                text_row(
                    t!("printers.overview.z_offset").into_owned(),
                    format!("{offset:+.2} mm"),
                )
            }));
            rows.extend(auto_shutdown.map(|enabled| {
                text_row(
                    t!("printers.overview.auto_shutdown").into_owned(),
                    if enabled {
                        t!("printers.overview.enabled").into_owned()
                    } else {
                        t!("printers.overview.disabled").into_owned()
                    },
                )
            }));
            // The sensor's configured name is not a label: several are numbered instead.
            let many = filament_sensors.len() > 1;
            rows.extend(filament_sensors.iter().enumerate().map(|(index, present)| {
                text_row(
                    if many {
                        t!(
                            "printers.overview.numbered",
                            label = t!("printers.overview.filament_sensor"),
                            number = index + 1
                        )
                        .into_owned()
                    } else {
                        t!("printers.overview.filament_sensor").into_owned()
                    },
                    if *present {
                        t!("printers.overview.filament_present").into_owned()
                    } else {
                        t!("printers.overview.filament_absent").into_owned()
                    },
                )
            }));
            rows.extend(fan_rows(fans));
            (
                Some(t!("printers.overview.machine").into_owned()),
                list("Machine", rows, list_columns, muted, cx),
            )
        }
        Content::Lifetime(totals) => {
            let rows = [
                totals.print_duration_s.map(|seconds| {
                    text_row(
                        t!("printers.lifetime.time").into_owned(),
                        super::format_elapsed(seconds),
                    )
                }),
                totals.filament_used_mm.map(|mm| {
                    text_row(
                        t!("printers.lifetime.filament").into_owned(),
                        super::format_filament_length(mm),
                    )
                }),
                totals.jobs.map(|jobs| {
                    text_row(t!("printers.lifetime.jobs").into_owned(), jobs.to_string())
                }),
                totals.total_duration_s.map(|seconds| {
                    text_row(
                        t!("printers.overview.total_duration").into_owned(),
                        super::format_elapsed(seconds),
                    )
                }),
                totals.longest_print_duration_s.map(|seconds| {
                    text_row(
                        t!("printers.overview.longest_print").into_owned(),
                        super::format_elapsed(seconds),
                    )
                }),
                totals.longest_total_duration_s.map(|seconds| {
                    text_row(
                        t!("printers.overview.longest_total").into_owned(),
                        super::format_elapsed(seconds),
                    )
                }),
            ]
            .into_iter()
            .flatten()
            .collect();
            (
                Some(t!("printers.overview.lifetime").into_owned()),
                // Totals are history, never live: they are not dimmed when the link drops.
                list("Lifetime", rows, list_columns, false, cx),
            )
        }
        Content::Readings(_) | Content::Device(_) => {
            technical_content(kind, content, composition, printer, muted, cx)
        }
    }
}

pub(super) fn technical_content(
    kind: CardKind,
    content: &Content<'_>,
    composition: Composition,
    printer: &PrinterEntry,
    muted: bool,
    cx: &App,
) -> (Option<String>, AnyElement) {
    match content {
        Content::Readings(readings) => super::readings::view::render(
            match kind {
                CardKind::Readings(group) => group,
                _ => unreachable!(),
            },
            readings,
            &composition,
            muted || readings.status != crate::shared::workspace::ReadStatus::Ready,
            cx,
        ),
        Content::Device(device) => {
            let rows = detail_rows(printer, device, reported_camera(printer));
            let start = rows.len() - device.extra.len() - usize::from(device.camera_retry);
            let mut states = vec![crate::shared::workspace::ReadStatus::Ready; rows.len()];
            for (index, field) in device.extra.iter().enumerate() {
                states[start + index] = super::readings::view::read_state(field);
            }
            (
                None,
                v_flex()
                    .gap_3()
                    .child(super::readings::view::heading(
                        t!("printers.device.title").into_owned(),
                        None,
                        ElementId::from("overview-Device-heading"),
                        true,
                        cx,
                    ))
                    .child(list_with_status(
                        "details",
                        rows,
                        composition.block.columns.max(1),
                        false,
                        &states,
                        composition.group.first().map(|part| part.width),
                        cx,
                    ))
                    .into_any_element(),
            )
        }
        _ => unreachable!("Only technical content belongs in the technical surface"),
    }
}
