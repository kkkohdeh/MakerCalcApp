use gpui_kit::{
    AnyElement, App, Bounds, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement,
    LayoutId, Pixels, Point, RenderOnce, Window, point, px,
};

/// CSS-like centered scale: layout is retained while paint and input are scaled.
#[derive(IntoElement)]
pub(in crate::gpui_app) struct Zoom {
    id: ElementId,
    factor: f32,
    anchor: Option<Bounds<Pixels>>,
    travel: Pixels,
    fraction_y: f32,
    child: AnyElement,
}

impl Zoom {
    pub(in crate::gpui_app) fn new(id: impl Into<ElementId>, child: impl IntoElement) -> Self {
        Self {
            id: id.into(),
            factor: 1.,
            anchor: None,
            travel: px(0.),
            fraction_y: 0.,
            child: child.into_any_element(),
        }
    }

    pub(in crate::gpui_app) fn factor(mut self, factor: f32) -> Self {
        self.factor = factor;
        self
    }

    /// Translate relative to the child's own height, without moving its layout slot.
    pub(in crate::gpui_app) fn translated_fraction(mut self, fraction_y: f32) -> Self {
        self.fraction_y = fraction_y;
        self
    }

    /// Popup motion originates at the trigger edge, including when it flips above.
    pub(in crate::gpui_app) fn anchored(mut self, anchor: Bounds<Pixels>, travel: Pixels) -> Self {
        self.anchor = Some(anchor);
        self.travel = travel;
        self
    }
}

impl RenderOnce for Zoom {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        Scaled {
            id: self.id,
            factor: self.factor,
            anchor: self.anchor,
            travel: self.travel,
            fraction_y: self.fraction_y,
            child: self.child,
        }
    }
}

struct Scaled {
    id: ElementId,
    factor: f32,
    anchor: Option<Bounds<Pixels>>,
    travel: Pixels,
    fraction_y: f32,
    child: AnyElement,
}

impl IntoElement for Scaled {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for Scaled {
    type RequestLayoutState = ();
    type PrepaintState = Point<Pixels>;
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
    ) -> (LayoutId, ()) {
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Point<Pixels> {
        let (origin, mut offset) = match self.anchor {
            Some(anchor) if bounds.top() >= anchor.top() => {
                (anchor.bottom_center(), point(px(0.), -self.travel))
            }
            Some(anchor) => (anchor.top_center(), point(px(0.), self.travel)),
            None => (bounds.center(), Point::default()),
        };
        offset.y += bounds.size.height * self.fraction_y;
        window.with_element_offset(offset, |window| {
            window.with_element_scale(origin, self.factor, |window| {
                self.child.prepaint(window, cx)
            });
        });
        origin
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        origin: &mut Point<Pixels>,
        window: &mut Window,
        cx: &mut App,
    ) {
        window.with_element_scale(*origin, self.factor, |window| self.child.paint(window, cx));
    }
}
