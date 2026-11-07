//! Summary: related normalized data is grouped into cards (`model`).
//! Cards appear only for reported data; content sets size and fixed weight sets placement (`layout`).
//! Secondary controls and technical groups share measured, balanced columns.

use super::components::scroll_area::ScrollAreaExt as _;
use crate::i18n::t;
use gpui_kit::base::{StyledExt as _, TestSupportExt as _};
use gpui_kit::component::{ActiveTheme as _, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, Styled as _,
    div, px,
};

use crate::state::{LanConnection, LocalPrinterId};

use super::{MakerCalcGpuiApp, PrinterEntry};

mod cards;
mod flow;
mod layout;
mod model;
pub(super) mod readings;

use model::{CardKind, Content};
pub(super) use model::{DeviceFacts, FanFact};

/// Minutes without seconds, so a running time does not change width every second.
fn format_elapsed(seconds: f64) -> String {
    let minutes = (seconds.max(0.) / 60.).round() as u64;
    match minutes {
        0 => "< 1 min".to_owned(),
        1..60 => format!("{minutes} min"),
        _ => format!("{} h {:02} min", minutes / 60, minutes % 60),
    }
}

fn format_filament_length(mm: f64) -> String {
    let meters = mm.max(0.) / 1000.;
    if meters.round() >= 1000. {
        format!("{:.1} km", meters / 1000.)
    } else if meters < 10. {
        format!("{meters:.1} m")
    } else {
        format!("{meters:.0} m")
    }
}

/// Render cards in order as full-width rows.
/// Related light summary cards share a surface.
fn rows(
    cards: &[model::Card<'_>],
    pair: bool,
    columns: u16,
    column: f32,
    printer: &PrinterEntry,
    muted: bool,
    cx: &mut Context<MakerCalcGpuiApp>,
) -> Vec<AnyElement> {
    // Resources in one visual family share a surface, independent of manufacturer.
    let mut units = Vec::with_capacity(cards.len());
    let thermal = |kind| matches!(kind, CardKind::Nozzle(_) | CardKind::Bed(_));
    let mut index = 0;
    while index < cards.len() {
        let start = index;
        if pair && thermal(cards[index].kind) {
            while index < cards.len() && thermal(cards[index].kind) {
                index += 1;
            }
            units.push((start, index, true));
            continue;
        }
        let paired = pair
            && cards
                .get(index + 1)
                .is_some_and(|next| cards[index].kind.pairs_with(next.kind));
        index += if paired { 2 } else { 1 };
        units.push((start, index, false));
    }
    let block = |card: &model::Card<'_>| match card.shape() {
        layout::Shape::Block(block) => block,
        _ => layout::Block {
            hero: false,
            rows: card.content.facts(),
        },
    };
    let shapes = units
        .iter()
        .map(|&(start, end, grouped)| match (grouped, end - start) {
            (true, _) => layout::Shape::Group(cards[start..end].iter().map(&block).collect()),
            (false, 2) => layout::Shape::Pair(block(&cards[start]), block(&cards[start + 1])),
            _ => cards[start].shape(),
        })
        .collect::<Vec<_>>();
    let mut units = units.into_iter();
    layout::arrange(&shapes, columns, column)
        .into_iter()
        .map(|row| {
            // Cell bounds are already known; explicit dimensions avoid repeated intrinsic sizing.
            let placed = row
                .cards
                .iter()
                .zip(units.by_ref())
                .map(|(placed, (first, end, grouped))| {
                    let width =
                        f32::from(placed.span) * column + f32::from(placed.span - 1) * layout::GAP;
                    let composition = placed.composition.clone();
                    if grouped {
                        let parts = cards[first..end]
                            .iter()
                            .zip(composition.group)
                            .map(|(card, layout)| {
                                let part = cards::content(
                                    card.kind,
                                    &card.content,
                                    layout::Composition {
                                        block: layout.block,
                                        ..Default::default()
                                    },
                                    printer,
                                    muted,
                                    cx,
                                );
                                (card.kind, part, layout)
                            })
                            .collect();
                        return cards::group_shell(placed.span, parts, cx)
                            .w(px(width))
                            .min_h(px(row.height))
                            .into_any_element();
                    }
                    let first_card = &cards[first];
                    let first_part = cards::content(
                        first_card.kind,
                        &first_card.content,
                        composition.clone(),
                        printer,
                        muted,
                        cx,
                    );
                    match (end - first == 2).then_some(first + 1) {
                        Some(second) => {
                            let second_card = &cards[second];
                            let second_part = cards::content(
                                second_card.kind,
                                &second_card.content,
                                layout::Composition {
                                    block: composition.second,
                                    ..composition
                                },
                                printer,
                                muted,
                                cx,
                            );
                            cards::pair_shell(
                                (first_card.kind, second_card.kind),
                                placed.span,
                                composition.side_by_side,
                                first_part,
                                second_part,
                                cx,
                            )
                            .w(px(width))
                            .min_h(px(row.height))
                            .into_any_element()
                        }
                        None => cards::card_shell(
                            first_card.kind,
                            Some(placed.span),
                            first_part.0,
                            first_part.1,
                            cx,
                        )
                        .w(px(width))
                        .min_h(px(row.height))
                        .into_any_element(),
                    }
                })
                .collect::<Vec<_>>();
            // Content may grow beyond the estimate when translated labels wrap.
            div()
                .flex_none()
                .grid()
                .grid_cols(columns)
                .w(px(
                    f32::from(columns) * column + f32::from(columns - 1) * layout::GAP
                ))
                .min_h(px(row.height))
                .gap(px(layout::GAP))
                .children(placed)
                .into_any_element()
        })
        .collect()
}

fn technical_rows(
    motion: Option<AnyElement>,
    cards: &[model::Card<'_>],
    columns: u16,
    column: f32,
    printer: &PrinterEntry,
    muted: bool,
    cx: &gpui_kit::App,
) -> Vec<AnyElement> {
    let mut width = f32::from(columns) * column + f32::from(columns - 1) * layout::GAP;
    if motion.is_none()
        && let [card] = cards
    {
        let span = f32::from(layout::technical_span(&card.shape(), 2));
        width = width.min(
            span * layout::LIST_COLUMN_MAX + (span - 1.) * layout::INNER_GAP + 2. * layout::PADDING,
        );
    }
    let count = cards.len() + usize::from(motion.is_some());
    let columns = ((width + layout::GAP) / (layout::TECHNICAL_CELL_MIN + layout::GAP))
        .floor()
        .clamp(1., f32::from((columns / 2).max(1))) as u16;
    let columns = columns.min(count.div_ceil(2).max(1) as u16);
    let column = (width - f32::from(columns - 1) * layout::GAP) / f32::from(columns);
    let mut items = Vec::with_capacity(count);
    if let Some(content) = motion {
        items.push(flow::Item {
            key: "manual-motion".into(),
            group: 0,
            heading: None,
            content,
        });
    }
    items.extend(cards.iter().map(|card| {
        let shape = card.shape();
        let composition = layout::technical_composition(column - 2. * layout::PADDING);
        let part =
            cards::technical_content(card.kind, &card.content, composition, printer, muted, cx);
        flow::Item {
            key: format!("{:?}:{shape:?}", card.kind),
            group: 0,
            heading: None,
            content: cards::card_shell(card.kind, None, part.0, part.1, cx)
                .w(px(column))
                .into_any_element(),
        }
    }));
    vec![
        flow::Flow::new(
            "technical-board",
            width,
            usize::from(columns),
            layout::GAP,
            flow::Mode::Board,
            items,
        )
        .into_any_element(),
    ]
}

/// A transparent grid of independent technical cards, using the regular card slots.
fn technical_grid(rows: Vec<AnyElement>) -> AnyElement {
    v_flex()
        .id("overview-technical")
        .test_support()
        .min_w_0()
        .flex_none()
        .gap(px(layout::GAP))
        .children(rows)
        .into_any_element()
}

impl MakerCalcGpuiApp {
    pub(super) fn render_overview(
        id: LocalPrinterId,
        printer: &PrinterEntry,
        viewport_width: f32,
        machine_controls: &super::machine_controls::MachineControls,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let columns = layout::columns_for(viewport_width);
        let column = layout::column_width(viewport_width, columns);
        let current =
            printer.state.connection == LanConnection::Connected && printer.state.telemetry_ready;
        // The job card heads Summary at full width; summary cards
        // and technical details each fill full-width rows below.
        let (job, cards): (Vec<_>, Vec<_>) = model::cards(printer)
            .into_iter()
            .partition(|card| card.kind == CardKind::Job);
        let (technical, summary): (Vec<_>, Vec<_>) =
            cards.into_iter().partition(|card| card.kind.technical());

        let job = job.iter().find_map(|card| match &card.content {
            Content::Job => {
                let content = Self::render_job(id, printer, cx);
                Some(
                    div()
                        .id("overview-card-Job")
                        .test_support()
                        .child(content)
                        .into_any_element(),
                )
            }
            _ => None,
        });
        let motion = Self::render_motion_controls(id, printer, machine_controls, cx);
        let summary: Vec<_> = summary
            .into_iter()
            .filter(|card| motion.is_none() || card.kind != CardKind::Motion)
            .collect();
        let summary_rows = rows(&summary, true, columns, column, printer, !current, cx);
        let technical_rows = (!technical.is_empty() || motion.is_some()).then(|| {
            technical_grid(technical_rows(
                motion, &technical, columns, column, printer, !current, cx,
            ))
        });

        v_flex()
            .size_full()
            .px_5()
            .pb_5()
            .scroll_area()
            .id("overview-scroll")
            .child(
                v_flex()
                    .id("overview-surface")
                    .test_support()
                    .flex_none()
                    .gap(px(layout::GAP))
                    .children(job)
                    .children(Self::render_machine_controls(
                        id,
                        printer,
                        machine_controls.is_pending(id),
                        cx,
                    ))
                    .children(summary_rows)
                    .when_some(technical_rows, |surface, technical| {
                        surface
                            .child(
                                div()
                                    .pt_2()
                                    .text_base()
                                    .font_semibold()
                                    .text_color(cx.theme().foreground)
                                    .child(t!("printers.overview.technical_details")),
                            )
                            .child(technical)
                    }),
            )
            .into_any_element()
    }

    fn render_job(
        id: LocalPrinterId,
        printer: &PrinterEntry,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let alerts = printer
            .state
            .telemetry
            .printer_alerts
            .as_deref()
            .filter(|alerts| {
                printer.state.connection == LanConnection::Connected && !alerts.is_empty()
            });
        let details = alerts.map(|alerts| {
            v_flex()
                .id("overview-alerts")
                .test_support()
                .min_w_0()
                .gap_2()
                .children(alerts.iter().enumerate().map(|(index, alert)| {
                    let description = t!(
                        "printers.overview.alert_message",
                        category = &alert.category,
                        message = &alert.message
                    )
                    .into_owned();
                    super::feedback::error_alert(("overview-alert", index), &description, cx)
                }))
                .into_any_element()
        });
        div()
            .id("overview-job")
            .test_support()
            .min_w_0()
            .child(Self::render_card(id, printer, true, details, cx))
            .into_any_element()
    }
}
