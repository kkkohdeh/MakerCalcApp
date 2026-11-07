use std::time::Duration;

use gpui_kit::base::motion::{Easing, Transition, transition};
use gpui_kit::base::{Progress as BaseProgress, StyledExt as _, TestSupportExt as _};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, Bounds, ContentMask, ElementId, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, RenderOnce, SharedString, StyleRefinement, Styled, Window, canvas, div,
    fill, px, size,
};

#[derive(IntoElement)]
pub(in crate::gpui_app) struct Progress {
    id: ElementId,
    value: f32,
    label: Option<SharedString>,
    color: Option<Hsla>,
    track_color: Option<Hsla>,
    track_height: f32,
    style: StyleRefinement,
}
impl Progress {
    pub(in crate::gpui_app) fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            value: 0.,
            label: None,
            color: None,
            track_color: None,
            track_height: 4.,
            style: StyleRefinement::default(),
        }
    }
    pub(in crate::gpui_app) fn value(mut self, value: f32) -> Self {
        self.value = value.clamp(0., 100.);
        self
    }
    pub(in crate::gpui_app) fn color(mut self, color: Hsla) -> Self {
        self.color = Some(color);
        self
    }
    pub(in crate::gpui_app) fn track_color(mut self, color: Hsla) -> Self {
        self.track_color = Some(color);
        self
    }
    pub(in crate::gpui_app) fn track_height(mut self, height: f32) -> Self {
        self.track_height = height;
        self
    }
    pub(in crate::gpui_app) fn accessibility_label(
        mut self,
        label: impl Into<SharedString>,
    ) -> Self {
        self.label = Some(label.into());
        self
    }
}
impl Styled for Progress {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}
impl RenderOnce for Progress {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let value = transition(
            (self.id.clone(), "value"),
            self.value,
            Transition::new(Duration::from_millis(150)).easing(Easing::CubicBezier {
                x1: 0.4,
                y1: 0.,
                x2: 0.2,
                y2: 1.,
            }),
            window,
            cx,
        );
        let color = self.color.unwrap_or(cx.theme().progress_bar);
        let track_color = self.track_color.unwrap_or(color.opacity(0.2));
        let radius = px(self.track_height / 2.);
        BaseProgress::new(self.id.clone())
            .value(self.value)
            .when_some(self.label, |root, label| root.accessibility_label(label))
            .w_full()
            .min_w_0()
            .flex()
            .flex_wrap()
            .gap(px(12.))
            .refine_style(&self.style)
            .child(
                div()
                    .id((self.id, "track"))
                    .test_support()
                    .relative()
                    .flex()
                    .items_center()
                    .h(px(self.track_height))
                    .w_full()
                    .overflow_hidden()
                    .rounded(radius)
                    .bg(track_color)
                    .child(
                        canvas(
                            |bounds, _, _| bounds,
                            move |bounds, _, window, _| {
                                if value <= 0. {
                                    return;
                                }
                                // Clip the pill to the value for a square indicator, even below the rounded track's radius.
                                let visible = Bounds::new(
                                    bounds.origin,
                                    size(bounds.size.width * (value / 100.), bounds.size.height),
                                );
                                window.with_content_mask(
                                    Some(ContentMask { bounds: visible }),
                                    |window| {
                                        window.paint_quad(fill(bounds, color).corner_radii(radius));
                                    },
                                );
                            },
                        )
                        .size_full(),
                    ),
            )
    }
}
