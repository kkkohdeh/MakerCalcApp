//! Plain resource headings and typed values, on the existing technical card surface.
use super::{Group, Readings, Value, presentation, sections};
use crate::gpui_app::components::button::Button;
use crate::gpui_app::feedback::break_long_words;
use crate::gpui_app::overview::{
    flow,
    layout::{Composition, INNER_GAP, LIST_COLUMN_MAX},
};
use crate::i18n::t;
use gpui_kit::base::{StyledExt as _, TestSupportExt as _};
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ClipboardItem, ElementId, InteractiveElement as _, IntoElement,
    ParentElement as _, RenderOnce, Role, StatefulInteractiveElement as _, Styled as _, Window,
    div, px, relative,
};

const THERMOMETER: &[u8] = include_bytes!("../../../../assets/icons/useanimations/thermometer.svg");
const MOVE: &[u8] = include_bytes!("../../../../assets/icons/useanimations/move.svg");
const CPU: &[u8] = include_bytes!("../../../../assets/icons/useanimations/cpu.svg");
const GRID: &[u8] = include_bytes!("../../../../assets/icons/useanimations/grid.svg");

fn group_icon(group: Group) -> Option<&'static [u8]> {
    match group {
        Group::Heaters | Group::Sensors => Some(THERMOMETER),
        Group::Motion | Group::MotionSpeed | Group::MotionLimits => Some(MOVE),
        Group::Controller => Some(CPU),
        // No motor, probe, filament or printer metaphor from an unrelated icon.
        _ => None,
    }
}

pub(in crate::gpui_app::overview) fn heading(
    label: String,
    icon: Option<&'static [u8]>,
    id: ElementId,
    primary: bool,
    cx: &App,
) -> AnyElement {
    h_flex()
        .id(id.clone())
        .test_support()
        .min_w_0()
        .gap_2()
        .items_center()
        .when_some(icon, |row, data| {
            row.child(
                div()
                    .id((id, "icon"))
                    .test_support()
                    .size(px(16.))
                    .flex_none()
                    .child(Icon::default().data(data).size(px(16.))),
            )
        })
        .child(
            crate::gpui_app::components::card::title(!primary)
                .min_w_0()
                .whitespace_normal()
                .text_color(if primary {
                    cx.theme().foreground
                } else {
                    cx.theme().muted_foreground
                })
                .child(label),
        )
        .into_any_element()
}

#[derive(IntoElement)]
struct LongValue {
    id: ElementId,
    label: String,
    full: String,
    summary: String,
    muted: bool,
}

impl RenderOnce for LongValue {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let expanded = window.use_keyed_state((self.id.clone(), "expanded"), cx, |_, _| false);
        let open = *expanded.read(cx);
        let copy = self.full.clone();
        let disclosure = if open {
            t!("printers.overview.technical.hide_full")
        } else {
            t!("printers.overview.technical.show_full")
        };
        v_flex()
            .w_full()
            .min_w_0()
            .gap_1()
            .text_color(if self.muted {
                cx.theme().muted_foreground
            } else {
                cx.theme().foreground
            })
            .child(
                div()
                    .id((self.id.clone(), "text"))
                    .test_support()
                    .min_w_0()
                    .whitespace_normal()
                    .line_height(px(20.))
                    .font_normal()
                    .child(break_long_words(
                        if open { &self.full } else { &self.summary },
                        16,
                    )),
            )
            .child(
                h_flex()
                    .gap_3()
                    .child(
                        Button::new((self.id.clone(), "expand"))
                            .ghost()
                            .p_0()
                            .h(px(24.))
                            .font_normal()
                            .bg(cx.theme().transparent)
                            .border_0()
                            .label(disclosure.into_owned())
                            .accessibility_label(
                                t!(
                                    "printers.overview.technical.toggle_value",
                                    label = &self.label
                                )
                                .into_owned(),
                            )
                            .toggled(Some(open))
                            .on_click(move |_, _, cx| {
                                expanded.update(cx, |open, cx| {
                                    *open = !*open;
                                    cx.notify();
                                })
                            }),
                    )
                    .child(
                        Button::new((self.id, "copy"))
                            .ghost()
                            .p_0()
                            .h(px(24.))
                            .font_normal()
                            .bg(cx.theme().transparent)
                            .border_0()
                            .label(t!("common.actions.copy").into_owned())
                            .accessibility_label(
                                t!(
                                    "printers.overview.technical.copy_value",
                                    label = &self.label
                                )
                                .into_owned(),
                            )
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))
                            }),
                    ),
            )
    }
}

pub(in crate::gpui_app::overview) fn long_value(
    id: ElementId,
    label: String,
    full: String,
    summary: String,
    muted: bool,
) -> AnyElement {
    LongValue {
        id,
        label,
        full,
        summary,
        muted,
    }
    .into_any_element()
}

pub(in crate::gpui_app::overview) fn read_state(
    field: &super::Field<'_>,
) -> crate::shared::workspace::ReadStatus {
    match field.value {
        Value::Status(state) => state,
        _ => field.status,
    }
}

pub(in crate::gpui_app::overview) fn state_indicator(
    id: String,
    state: crate::shared::workspace::ReadStatus,
    cx: &App,
) -> Option<AnyElement> {
    use crate::shared::workspace::ReadStatus;
    matches!(
        state,
        ReadStatus::Error | ReadStatus::Denied | ReadStatus::Limited
    )
    .then(|| {
        div()
            .id(ElementId::Name(format!("{id}-status-icon").into()))
            .test_support()
            .size(px(14.))
            .flex_none()
            .text_color(cx.theme().warning)
            .child(Icon::new(IconName::TriangleAlert).size(px(14.)))
            .into_any_element()
    })
}

fn row(
    id: String,
    field: &super::Field<'_>,
    formatted: &crate::gpui_app::machine::Row,
    muted: bool,
    cx: &App,
) -> AnyElement {
    let locale = rust_i18n::locale();
    let locale = locale.as_ref();
    let label = sections::label(field, locale);
    let description = t!(
        "printers.bay.metric",
        label = &formatted.label,
        value = &formatted.value
    )
    .into_owned();
    let root = v_flex()
        .id(ElementId::Name(id.clone().into()))
        .role(Role::Group)
        .aria_label(description)
        .test_support()
        .min_w_0()
        .min_h(px(22.))
        .gap_1()
        .text_sm()
        .font_features(gpui_kit::FontFeatures(std::sync::Arc::new(vec![(
            "tnum".into(),
            1,
        )])))
        .line_height(px(20.));
    let label_node = h_flex()
        .min_w_0()
        .gap_2()
        .items_center()
        .text_color(cx.theme().muted_foreground)
        .when(matches!(field.value, Value::Grid(_)), |label| {
            label.child(Icon::default().data(GRID).size(px(14.)).flex_none())
        })
        .child(
            div()
                .id(ElementId::Name(format!("{id}-label").into()))
                .test_support()
                .flex_1()
                .min_w_0()
                .whitespace_normal()
                .child(label.clone()),
        )
        .when(
            matches!(field.value, Value::Xyz(_) | Value::Xy(_)),
            |label| label.child(div().text_xs().child("mm")),
        )
        .children(state_indicator(id.clone(), read_state(field), cx));
    let long = match field.value {
        Value::Text(text) if text.chars().count() > 32 => {
            Some(text.chars().take(28).collect::<String>() + "…")
        }
        Value::Grid(grid) if grid.iter().any(|row| !row.is_empty()) => Some(
            t!(
                "printers.overview.technical.mesh_summary",
                rows = grid.len(),
                points = grid.iter().map(Vec::len).sum::<usize>()
            )
            .into_owned(),
        ),
        _ => None,
    };
    if let Some(summary) = long {
        return root
            .child(label_node)
            .child(long_value(
                ElementId::Name(format!("{id}-value").into()),
                formatted.label.clone(),
                formatted.value.clone(),
                summary,
                muted,
            ))
            .into_any_element();
    }
    let axes: Vec<_> = match field.value {
        Value::Xyz(values) => ['X', 'Y', 'Z'].into_iter().zip(values).collect(),
        Value::Xy(values) => ['X', 'Y'].into_iter().zip(values).collect(),
        _ => Vec::new(),
    };
    if !axes.is_empty() {
        return root
            .child(label_node)
            .child(
                div()
                    .id(ElementId::Name(format!("{id}-value").into()))
                    .test_support()
                    .grid()
                    .grid_cols(axes.len() as u16)
                    .w_full()
                    .min_w_0()
                    .gap_x_3()
                    .children(axes.into_iter().map(|(axis, value)| {
                        h_flex()
                            .id(ElementId::Name(format!("{id}-axis-{axis}").into()))
                            .test_support()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .justify_between()
                            .child(
                                div()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(axis.to_string()),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .text_right()
                                    .when(muted, |value| {
                                        value.text_color(cx.theme().muted_foreground)
                                    })
                                    .child(format!("{value:.2}")),
                            )
                    })),
            )
            .when_some(presentation::status(field.status, locale), |row, status| {
                row.child(div().text_color(cx.theme().muted_foreground).child(status))
            })
            .into_any_element();
    }
    root.child(
        h_flex()
            .min_w_0()
            .gap_3()
            .items_start()
            .child(div().flex_1().min_w_0().child(label_node))
            .child(
                div()
                    .id(ElementId::Name(format!("{id}-value").into()))
                    .test_support()
                    .min_w_0()
                    .max_w(relative(0.6))
                    .text_right()
                    .whitespace_normal()
                    .when(muted, |value| value.text_color(cx.theme().muted_foreground))
                    .child(break_long_words(&formatted.value, 16)),
            ),
    )
    .into_any_element()
}

pub(in crate::gpui_app::overview) fn render(
    group: Group,
    readings: &Readings<'_>,
    composition: &Composition,
    muted: bool,
    cx: &App,
) -> (Option<String>, AnyElement) {
    let sections = sections::sections(readings);
    let many = sections.len() > 1;
    let locale = rust_i18n::locale();
    let formatted = presentation::reading_rows(readings, locale.as_ref());
    let title = presentation::title(readings, locale.as_ref());
    let only_name = (!many)
        .then(|| {
            sections
                .first()
                .and_then(|section| sections::caption(section, readings, locale.as_ref()))
        })
        .flatten();
    let title = only_name
        .filter(|name| name != &title)
        .map_or(title.clone(), |name| {
            t!(
                "printers.overview.technical.resource_group",
                group = title,
                name = name
            )
            .into_owned()
        });
    let id = format!("Readings({group:?})");
    let mut content = v_flex()
        .id(ElementId::Name(format!("overview-{id}").into()))
        .test_support()
        .w_full()
        .min_w_0()
        .gap_3()
        .child(heading(
            title,
            group_icon(group),
            ElementId::Name(format!("overview-{id}-heading").into()),
            true,
            cx,
        ));
    let offset = usize::from(readings.status != crate::shared::workspace::ReadStatus::Ready);
    if let Some(status) = presentation::status(readings.status, locale.as_ref()) {
        content = content.child(
            h_flex()
                .id(ElementId::Name(format!("overview-{id}-0").into()))
                .test_support()
                .gap_2()
                .items_start()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .children(state_indicator(
                    format!("overview-{id}-0"),
                    readings.status,
                    cx,
                ))
                .child(t!("printers.overview.read_status").into_owned() + ": " + &status),
        );
    }
    let mut items = Vec::with_capacity(readings.fields.len());
    for (index, section) in sections.iter().enumerate() {
        for &field in &section.indices {
            let caption = many.then(|| {
                heading(
                    sections::caption(section, readings, locale.as_ref())
                        .unwrap_or_else(|| t!("printers.overview.technical.general").into_owned()),
                    None,
                    ElementId::Name(format!("overview-{id}-resource-{index}-{field}").into()),
                    false,
                    cx,
                )
            });
            items.push(flow::Item {
                key: format!(
                    "{:?}:{}:{}",
                    section.resource, field, readings.fields[field].key
                ),
                group: index,
                heading: caption,
                content: row(
                    format!("overview-{id}-{}", field + offset),
                    &readings.fields[field],
                    &formatted[field + offset],
                    muted,
                    cx,
                ),
            });
        }
    }
    let columns = composition.block.columns.max(1).min(items.len().max(1));
    let width = composition
        .group
        .first()
        .map_or(LIST_COLUMN_MAX, |part| part.width);
    (
        None,
        content
            .child(flow::Flow::new(
                ElementId::Name(format!("overview-{id}-fields").into()),
                width,
                columns,
                INNER_GAP,
                flow::Mode::Readings,
                items,
            ))
            .into_any_element(),
    )
}
