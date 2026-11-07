//! Place Summary cards in fixed order across full-width rows.
//! Size rows to content and reflow cards without stretching content.
//! Minimize layout cost using a one-dimensional GRIDS variant (Dayama et al., CHI 2020).

use super::super::charts::{CHART_MAX, CHART_MIN};

/// Narrowest column that keeps a card legible.
const CELL_MIN: f32 = 264.;
pub(super) const GAP: f32 = 16.;
/// Card padding on each side.
pub(super) const PADDING: f32 = 16.;
pub(super) const TECHNICAL_CELL_MIN: f32 = LIST_COLUMN_MIN + 2. * PADDING;
/// A card title and the space below it.
const TITLE: f32 = 32.;
/// One label/value row.
const LINE: f32 = 25.;
/// A main value: label, number and the space around them.
const HERO_HEIGHT: f32 = 76.;
pub(super) const HERO_WIDTH: f32 = 180.;
/// Space between a main value and its rows, or between the parts of a pair.
pub(super) const INNER_GAP: f32 = 32.;
// Two reading columns fit a technical card spanning two base cells.
const TECHNICAL_READING_MIN: f32 = CELL_MIN - PADDING - (INNER_GAP - GAP) / 2.;
/// Readable column widths; larger widths separate values from labels.
const LIST_COLUMN_MIN: f32 = 264.;
pub(super) const LIST_COLUMN_MAX: f32 = 340.;
/// One supply swatch with its slot, material and state.
const SPOOL_WIDTH: f32 = 120.;
const SPOOL_HEIGHT: f32 = 88.;
const SPOOL_GAP: f32 = 24.;
/// Sparse readouts keep a comfortable surface; dense ones grow with their content.
const CARD_MIN: f32 = 120.;
const CAMERA_MIN: f32 = 228.;
/// Cost of one more row, so that equal layouts prefer fewer rows.
const ROW_COST: f32 = 4.;
/// Per-column cost per pixel above the tall tier, for readouts that cannot fit elsewhere.
const OVERGROWN_COST: f32 = 0.5;
const TALL_ROW: f32 = 264.;

pub(super) fn columns_for(viewport_width: f32) -> u16 {
    ((available(viewport_width) + GAP) / (CELL_MIN + GAP))
        .floor()
        .clamp(2., 6.) as u16
}

pub(super) fn column_width(viewport_width: f32, columns: u16) -> f32 {
    let columns = f32::from(columns.max(1));
    (available(viewport_width) - (columns - 1.) * GAP) / columns
}

fn available(viewport_width: f32) -> f32 {
    // The detail view pads the surface by 20 px on each side.
    viewport_width - 40.
}

/// A main value with label/value rows, or rows under a title.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Block {
    /// The main value names the card; without it the card has a title.
    pub(super) hero: bool,
    pub(super) rows: usize,
}

/// What a card holds, as far as its size is concerned.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Shape {
    Block(Block),
    /// Related readouts share a surface; each keeps its own useful width.
    Group(Vec<Block>),
    /// Resource sections retain their headings inside one technical card.
    Sections(Vec<Block>, bool),
    /// Two related blocks on one surface, side by side or stacked.
    Pair(Block, Block),
    /// Supply swatches under a title.
    Spools(usize),
    /// A plot that fills the card.
    Chart,
    /// A camera frame with a minimum height.
    Camera,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct BlockLayout {
    /// Columns of label/value rows.
    pub(super) columns: usize,
    /// The main value sits beside its rows instead of above them.
    pub(super) beside: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Composition {
    /// The block, or the first block of a pair.
    pub(super) block: BlockLayout,
    pub(super) second: BlockLayout,
    /// The two blocks of a pair sit side by side instead of stacked.
    pub(super) side_by_side: bool,
    pub(super) spool_columns: usize,
    pub(super) group: Vec<GroupPart>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct GroupPart {
    pub(super) width: f32,
    pub(super) block: BlockLayout,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Placed {
    pub(super) span: u16,
    pub(super) composition: Composition,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Row {
    pub(super) height: f32,
    /// Consecutive cards, in their fixed order.
    pub(super) cards: Vec<Placed>,
}

/// Reserve a second reading column only for enough structural content to use it.
/// Numeric values and expansion state do not change a card's column assignment.
pub(super) fn technical_span(shape: &Shape, columns: u16) -> u16 {
    let lines = match shape {
        Shape::Sections(blocks, status) => {
            blocks.iter().map(|block| block.rows).sum::<usize>()
                + if blocks.len() > 1 { blocks.len() } else { 0 }
                + usize::from(*status)
        }
        Shape::Block(block) => block.rows,
        _ => 0,
    };
    let capacity = ((TALL_ROW - TITLE - 2. * PADDING) / LINE).floor() as usize;
    lines
        .div_ceil(2 * capacity)
        .clamp(1, usize::from(columns.clamp(1, 2))) as u16
}

pub(super) fn technical_composition(width: f32) -> Composition {
    let block = BlockLayout {
        columns: (((width + INNER_GAP) / (TECHNICAL_READING_MIN + INNER_GAP)).floor() as usize)
            .clamp(1, 2),
        beside: false,
    };
    Composition {
        block,
        group: vec![GroupPart { width, block }],
        ..Composition::default()
    }
}

/// Height a block needs at `width`, the width it actually uses and its layout.
fn measure_block(block: Block, width: f32) -> (f32, f32, BlockLayout) {
    let rows = block.rows;
    let lines = |columns: usize| rows.div_ceil(columns.max(1)) as f32 * LINE;
    let list_columns = |width: f32| {
        (((width + INNER_GAP) / (LIST_COLUMN_MIN + INNER_GAP)).floor() as usize)
            .clamp(1, rows.max(1))
    };
    let list_width = |width: f32, columns: usize| width.min(columns as f32 * LIST_COLUMN_MAX);
    if !block.hero {
        let columns = list_columns(width);
        let layout = BlockLayout {
            columns,
            beside: false,
        };
        return (TITLE + lines(columns), list_width(width, columns), layout);
    }
    if rows == 0 {
        return (HERO_HEIGHT, width.min(HERO_WIDTH), BlockLayout::default());
    }
    let beside_width = width - HERO_WIDTH - INNER_GAP;
    if beside_width >= LIST_COLUMN_MIN {
        let columns = list_columns(beside_width);
        let layout = BlockLayout {
            columns,
            beside: true,
        };
        return (
            HERO_HEIGHT.max(lines(columns)),
            HERO_WIDTH + INNER_GAP + list_width(beside_width, columns),
            layout,
        );
    }
    let columns = list_columns(width);
    let layout = BlockLayout {
        columns,
        beside: false,
    };
    (
        HERO_HEIGHT + 12. + lines(columns),
        list_width(width, columns).max(HERO_WIDTH.min(width)),
        layout,
    )
}

/// Height a card needs at `width`, the area it uses at `height` and its composition.
fn measure(shape: &Shape, width: f32, height: f32) -> (f32, f32, Composition) {
    match shape {
        Shape::Block(block) => {
            let (height, used, layout) = measure_block(*block, width);
            let composition = Composition {
                block: layout,
                ..Composition::default()
            };
            (height, height * used, composition)
        }
        Shape::Group(blocks) => measure_group(blocks, width),
        Shape::Sections(blocks, status) => {
            // Coordinates and expanded values cannot be split into columns.
            // Pack each resource's rows together.
            let count = (((width + INNER_GAP) / (LIST_COLUMN_MIN + INNER_GAP)).floor() as usize)
                .clamp(1, blocks.len().max(1));
            let part_width = ((width - (count - 1) as f32 * INNER_GAP) / count as f32)
                .min(LIST_COLUMN_MAX)
                .floor();
            let resource_title = if blocks.len() > 1 { TITLE } else { 0. };
            let mut height = 0.;
            let mut area = 0.;
            for row in blocks.chunks(count) {
                let needed = |block: &Block| resource_title + block.rows as f32 * LINE;
                height += row.iter().map(needed).fold(0., f32::max);
                area += row
                    .iter()
                    .map(|block| needed(block) * part_width)
                    .sum::<f32>();
            }
            height += blocks.len().div_ceil(count).saturating_sub(1) as f32 * INNER_GAP;
            let header = TITLE * if *status { 2. } else { 1. };
            let composition = Composition {
                group: blocks
                    .iter()
                    .map(|_| GroupPart {
                        width: part_width,
                        block: BlockLayout {
                            columns: 1,
                            beside: false,
                        },
                    })
                    .collect(),
                ..Composition::default()
            };
            (
                height + header,
                area + header * (count as f32 * part_width),
                composition,
            )
        }
        Shape::Pair(first, second) => {
            let half = (width - INNER_GAP) / 2.;
            if half >= LIST_COLUMN_MIN {
                let (first_height, first_used, block) = measure_block(*first, half);
                let (second_height, second_used, second_block) = measure_block(*second, half);
                let composition = Composition {
                    block,
                    second: second_block,
                    side_by_side: true,
                    spool_columns: 0,
                    group: Vec::new(),
                };
                let area = first_height * first_used + second_height * second_used;
                (first_height.max(second_height), area, composition)
            } else {
                let (first_height, first_used, block) = measure_block(*first, width);
                let (second_height, second_used, second_block) = measure_block(*second, width);
                let composition = Composition {
                    block,
                    second: second_block,
                    side_by_side: false,
                    spool_columns: 0,
                    group: Vec::new(),
                };
                let area = first_height * first_used + second_height * second_used;
                (first_height + INNER_GAP + second_height, area, composition)
            }
        }
        Shape::Spools(count) => {
            let columns = (((width + SPOOL_GAP) / (SPOOL_WIDTH + SPOOL_GAP)).floor() as usize)
                .clamp(1, (*count).max(1));
            let lines = count.div_ceil(columns) as f32;
            let height = TITLE + lines * SPOOL_HEIGHT + (lines - 1.).max(0.) * 16.;
            let used = columns as f32 * (SPOOL_WIDTH + SPOOL_GAP) - SPOOL_GAP;
            let composition = Composition {
                spool_columns: columns,
                ..Composition::default()
            };
            (height, height * used.min(width), composition)
        }
        // Plots stop gaining beyond a few lines of resolution.
        // Frames stop at their 16:9 height.
        Shape::Chart => (
            CHART_MIN,
            width * height.min(CHART_MAX),
            Composition::default(),
        ),
        Shape::Camera => {
            // The frame keeps its proportions: the area beside or below it stays empty.
            let frame_height = height.min(width * 9. / 16.);
            let frame_width = (frame_height * 16. / 9.).min(width);
            (
                CAMERA_MIN,
                frame_width * frame_height,
                Composition::default(),
            )
        }
    }
}

/// Pack blocks at useful widths; do not stretch readings or progress tracks across the window.
fn measure_group(blocks: &[Block], width: f32) -> (f32, f32, Composition) {
    let minimum = |block: Block| {
        if block.rows > 0 {
            LIST_COLUMN_MIN
        } else {
            HERO_WIDTH
        }
    };
    let preferred = |block: Block| {
        if block.hero && block.rows > 0 {
            HERO_WIDTH + INNER_GAP + LIST_COLUMN_MIN
        } else {
            minimum(block)
        }
    };
    let mut parts = Vec::with_capacity(blocks.len());
    let mut height = 0.;
    let mut area = 0.;
    let mut start = 0;
    while start < blocks.len() {
        let mut end = start + 1;
        let mut compact = minimum(blocks[start]).min(width);
        while end < blocks.len() && compact + INNER_GAP + minimum(blocks[end]) <= width {
            compact += INNER_GAP + minimum(blocks[end]);
            end += 1;
        }
        let demand = blocks[start..end]
            .iter()
            .map(|&block| preferred(block) - minimum(block))
            .sum::<f32>();
        let extra = (width - compact).max(0.).min(demand);
        let mut row_height: f32 = 0.;
        for &block in &blocks[start..end] {
            let growth = if demand > 0. {
                extra * (preferred(block) - minimum(block)) / demand
            } else {
                0.
            };
            let part_width = (minimum(block) + growth).min(width).floor();
            let (needed, used, layout) = measure_block(block, part_width);
            row_height = row_height.max(needed);
            area += needed * used;
            parts.push(GroupPart {
                width: part_width,
                block: layout,
            });
        }
        if start > 0 {
            height += INNER_GAP;
        }
        height += row_height;
        start = end;
    }
    (
        height,
        area,
        Composition {
            group: parts,
            ..Composition::default()
        },
    )
}

/// Inner width of a card spanning `span` columns.
fn inner_width(span: u16, column: f32) -> f32 {
    f32::from(span) * column + f32::from(span.saturating_sub(1)) * GAP - 2. * PADDING
}

/// The shortest height that fits every card of a row, and that row's empty area.
fn row_for(shapes: &[Shape], spans: &[u16], column: f32) -> (f32, f32, Vec<Placed>) {
    let widths = spans
        .iter()
        .map(|&span| inner_width(span, column))
        .collect::<Vec<_>>();
    let needed = shapes
        .iter()
        .zip(&widths)
        .map(|(shape, &width)| measure(shape, width, 0.).0)
        .fold(0., f32::max)
        + 2. * PADDING;
    let height = needed.ceil().max(CARD_MIN);
    let inner_height = height - 2. * PADDING;
    let mut waste = 0.;
    let mut placed = Vec::with_capacity(shapes.len());
    for ((shape, &width), &span) in shapes.iter().zip(&widths).zip(spans) {
        let (_, used, composition) = measure(shape, width, inner_height);
        waste += (width * inner_height - used).max(0.);
        placed.push(Placed { span, composition });
    }
    (height, waste, placed)
}

/// Every way to give `count` cards at least one column each, `columns` in total.
fn compositions(count: usize, columns: u16) -> Vec<Vec<u16>> {
    if count == 0 || count > usize::from(columns) {
        return Vec::new();
    }
    if count == 1 {
        return vec![vec![columns]];
    }
    (1..=columns - (count as u16 - 1))
        .flat_map(|first| {
            compositions(count - 1, columns - first)
                .into_iter()
                .map(move |mut rest| {
                    rest.insert(0, first);
                    rest
                })
        })
        .collect()
}

/// Split cards in order into full-width rows, minimizing empty area and row penalties.
/// `column` is the column width in pixels.
pub(super) fn arrange(shapes: &[Shape], columns: u16, column: f32) -> Vec<Row> {
    let count = shapes.len();
    // best[i]: least cost to place the first `i` cards, and where its last row starts.
    let mut best: Vec<Option<(f32, usize, Row)>> = vec![None; count + 1];
    best[0] = Some((
        0.,
        0,
        Row {
            height: 0.,
            cards: Vec::new(),
        },
    ));
    for start in 0..count {
        let Some(base) = best[start].as_ref().map(|(cost, _, _)| *cost) else {
            continue;
        };
        for end in start + 1..=(start + usize::from(columns)).min(count) {
            for spans in compositions(end - start, columns) {
                let (height, waste, cards) = row_for(&shapes[start..end], &spans, column);
                // Empty area in thousands of square pixels.
                let overgrown = (height - TALL_ROW).max(0.) * f32::from(columns);
                let cost = base + waste / 1000. + ROW_COST + overgrown * OVERGROWN_COST;
                if best[end].as_ref().is_none_or(|(known, _, _)| cost < *known) {
                    best[end] = Some((cost, start, Row { height, cards }));
                }
            }
        }
    }
    let mut rows = Vec::new();
    let mut end = count;
    while end > 0 {
        let Some((_, start, row)) = best[end].take() else {
            break;
        };
        rows.push(row);
        end = start;
    }
    rows.reverse();
    rows
}
