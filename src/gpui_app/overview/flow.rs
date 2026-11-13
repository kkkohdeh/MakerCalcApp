//! Measured cards and readings in balanced, consecutive columns.
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AvailableSpace, Bounds, Element, ElementId, GlobalElementId,
    InspectorElementId, IntoElement, LayoutId, ParentElement as _, Pixels, Point, Style,
    Styled as _, Window, div, point, px, size,
};

const MAX_TERMINAL_STRETCH: f32 = 96.;

pub(super) struct Item {
    pub key: String,
    pub group: usize,
    pub heading: Option<AnyElement>,
    pub content: AnyElement,
}

#[derive(Clone, Copy)]
pub(super) enum Mode {
    Board,
    Readings,
}

#[derive(Default)]
struct Plan {
    signature: (u32, u32, usize, Vec<(String, usize)>),
    columns: Vec<usize>,
}

pub(super) struct Flow {
    id: ElementId,
    width: f32,
    columns: usize,
    gap: f32,
    mode: Mode,
    items: Vec<Item>,
}

impl Flow {
    pub(super) fn new(
        id: impl Into<ElementId>,
        width: f32,
        columns: usize,
        gap: f32,
        mode: Mode,
        items: Vec<Item>,
    ) -> Self {
        Self {
            id: id.into(),
            width,
            columns: columns.max(1),
            gap,
            mode,
            items,
        }
    }
}

fn board_columns(heights: &[f32], columns: usize, gap: f32) -> Vec<usize> {
    if heights.is_empty() {
        return Vec::new();
    }
    let columns = columns.min(heights.len());
    let mut sums = vec![0.];
    for height in heights {
        sums.push(sums.last().copied().unwrap_or(0.) + height + gap);
    }
    fn visit(
        sums: &[f32],
        gap: f32,
        remaining: usize,
        cuts: &mut Vec<usize>,
        best: &mut (f32, f32, Vec<usize>),
    ) {
        let start = cuts.last().copied().unwrap_or(0);
        if remaining == 1 {
            cuts.push(sums.len() - 1);
            let mut previous = 0;
            let mut shortest = f32::INFINITY;
            let mut tallest: f32 = 0.;
            for &end in cuts.iter() {
                let height = sums[end] - sums[previous] - gap;
                shortest = shortest.min(height);
                tallest = tallest.max(height);
                previous = end;
            }
            let spread = tallest - shortest;
            if spread < best.0 || (spread == best.0 && tallest < best.1) {
                *best = (spread, tallest, cuts.clone());
            }
            cuts.pop();
            return;
        }
        for end in start + 1..=sums.len() - remaining {
            cuts.push(end);
            visit(sums, gap, remaining - 1, cuts, best);
            cuts.pop();
        }
    }
    // At most three board columns; evaluate consecutive cuts without reordering cards.
    let mut best = (f32::INFINITY, f32::INFINITY, Vec::new());
    visit(&sums, gap, columns, &mut Vec::new(), &mut best);
    let mut previous = 0;
    best.2
        .into_iter()
        .enumerate()
        .flat_map(|(column, end)| {
            let length = end - previous;
            previous = end;
            std::iter::repeat_n(column, length)
        })
        .collect()
}

fn reading_columns(
    heights: &[f32],
    groups: &[usize],
    headings: &[f32],
    columns: usize,
) -> Vec<usize> {
    if heights.is_empty() {
        return Vec::new();
    }
    let columns = columns.min(heights.len());
    let pack = |limit: f32, force: bool| {
        let mut column = 0;
        let mut y = 0.;
        let mut previous = None;
        heights
            .iter()
            .enumerate()
            .map(|(index, &height)| {
                let cost = |previous: Option<usize>| {
                    let changed = previous != Some(groups[index]);
                    height
                        + if changed && headings[index] > 0. {
                            headings[index] + 8.
                        } else {
                            0.
                        }
                        + if previous.is_none() {
                            0.
                        } else if changed {
                            16.
                        } else {
                            4.
                        }
                };
                if previous.is_some()
                    && (y + cost(previous) > limit
                        || (force && heights.len() - index == columns - column - 1))
                    && (!force || column + 1 < columns)
                {
                    column += 1;
                    previous = None;
                    y = 0.;
                }
                y += cost(previous);
                previous = Some(groups[index]);
                column
            })
            .collect::<Vec<_>>()
    };
    let mut low = heights
        .iter()
        .zip(headings)
        .map(|(h, title)| h + title + 8.)
        .fold(0., f32::max)
        .ceil() as u32;
    let mut high =
        (heights.iter().sum::<f32>() + headings.iter().sum::<f32>() + 24. * heights.len() as f32)
            .ceil() as u32;
    while low < high {
        let middle = low + (high - low) / 2;
        if pack(middle as f32, false).last().copied().unwrap_or(0) < columns {
            high = middle;
        } else {
            low = middle + 1;
        }
    }
    pack(low as f32, true)
}

impl IntoElement for Flow {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for Flow {
    type RequestLayoutState = Vec<(Point<Pixels>, Option<Point<Pixels>>)>;
    type PrepaintState = ();
    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let column_width =
            (self.width - (self.columns - 1) as f32 * self.gap) / self.columns as f32;
        let mut heights = Vec::with_capacity(self.items.len());
        let mut heading_heights = Vec::with_capacity(self.items.len());
        let mut groups = std::collections::BTreeMap::new();
        let board = matches!(self.mode, Mode::Board);
        let stretch = board && self.columns > 1;
        for item in &mut self.items {
            let width = column_width;
            let available = size(
                AvailableSpace::Definite(px(width)),
                AvailableSpace::MaxContent,
            );
            let content = std::mem::replace(&mut item.content, div().into_any_element());
            item.content = div()
                .w(px(width))
                .min_w_0()
                .when(stretch, |wrapper| wrapper.h_full())
                .child(content)
                .into_any_element();
            heights.push(
                item.content
                    .layout_as_root(available, window, cx)
                    .height
                    .as_f32(),
            );
            let heading_height = *groups.entry(item.group).or_insert_with(|| {
                item.heading.as_mut().map_or(0., |heading| {
                    heading
                        .layout_as_root(available, window, cx)
                        .height
                        .as_f32()
                })
            });
            heading_heights.push(heading_height);
        }
        let signature = (
            self.width.to_bits(),
            window.scale_factor().to_bits(),
            self.columns,
            self.items
                .iter()
                .map(|item| (item.key.clone(), item.group))
                .collect(),
        );
        let cache = window.use_keyed_state("measured-flow-plan", cx, |_, _| Plan::default());
        if cache.read(cx).signature != signature {
            let groups = self.items.iter().map(|item| item.group).collect::<Vec<_>>();
            let columns = match self.mode {
                Mode::Board => board_columns(&heights, self.columns, self.gap),
                Mode::Readings => {
                    reading_columns(&heights, &groups, &heading_heights, self.columns)
                }
            };
            // Geometry is retained without notifying or changing any printer state.
            cache.update(cx, |cache, _| {
                cache.signature = signature;
                cache.columns = columns;
            });
        }
        let columns = cache.read(cx).columns.clone();
        let mut ends = vec![0_f32; self.columns];
        let mut previous = vec![None; self.columns];
        let mut height = 0_f32;
        let positions = self
            .items
            .iter_mut()
            .enumerate()
            .map(|(index, item)| {
                let column = columns[index];
                let x = column as f32 * (column_width + self.gap);
                let mut y = ends[column];
                let mut heading_at = None;
                if matches!(self.mode, Mode::Readings) {
                    let changed = previous[column] != Some(item.group);
                    y += if previous[column].is_none() {
                        0.
                    } else if changed {
                        16.
                    } else {
                        4.
                    };
                    if changed && let Some(heading) = &mut item.heading {
                        let available = size(
                            AvailableSpace::Definite(px(column_width)),
                            AvailableSpace::MaxContent,
                        );
                        let title_height = heading
                            .layout_as_root(available, window, cx)
                            .height
                            .as_f32();
                        heading_at = Some(point(px(x), px(y)));
                        y += title_height + 8.;
                    }
                    previous[column] = Some(item.group);
                }
                let bottom = y + heights[index];
                height = height.max(bottom);
                let trailing = if board { self.gap } else { 0. };
                ends[column] = bottom + trailing;
                (point(px(x), px(y)), heading_at)
            })
            .collect::<Vec<_>>();
        if stretch {
            for (index, (position, _)) in positions.iter().enumerate() {
                if columns.get(index + 1) == columns.get(index) {
                    continue;
                }
                let bottom = position.y.as_f32() + heights[index];
                let extra = (height - bottom).clamp(0., MAX_TERMINAL_STRETCH);
                if extra <= 0. {
                    continue;
                }
                self.items[index].content.layout_as_root(
                    size(
                        AvailableSpace::Definite(px(column_width)),
                        AvailableSpace::Definite(px(heights[index] + extra)),
                    ),
                    window,
                    cx,
                );
            }
        }
        let style = Style {
            size: size(px(self.width).into(), px(height).into()),
            flex_shrink: 0.,
            ..Style::default()
        };
        (window.request_layout(style, [], cx), positions)
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        positions: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) {
        for (item, (content_at, heading_at)) in self.items.iter_mut().zip(positions) {
            if let Some(at) = heading_at
                && let Some(heading) = &mut item.heading
            {
                heading.prepaint_at(bounds.origin + *at, window, cx);
            }
            item.content
                .prepaint_at(bounds.origin + *content_at, window, cx);
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        positions: &mut Self::RequestLayoutState,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        for (item, (_, heading_at)) in self.items.iter_mut().zip(positions) {
            if heading_at.is_some()
                && let Some(heading) = &mut item.heading
            {
                heading.paint(window, cx);
            }
            item.content.paint(window, cx);
        }
    }
}
