use super::*;
use crate::scene::UniformScale;

impl Window {
    /// Scale a subtree about a layout-space origin without changing its layout.
    ///
    /// Call the same scope during prepaint and paint. Drawing, hit testing and
    /// positional mouse events use the same positive uniform transform. The
    /// inherited clip stays fixed in its parent's coordinates. Identity scopes
    /// take the unmodified fast path; this does not change the window's DPI.
    pub fn with_element_scale<R>(
        &mut self,
        origin: Point<Pixels>,
        factor: f32,
        f: impl FnOnce(&mut Self) -> R,
    ) -> R {
        self.with_scale_transform(UniformScale::around(origin, factor), f)
    }

    pub(super) fn with_absolute_element_scale<R>(
        &mut self,
        scale: UniformScale,
        f: impl FnOnce(&mut Self) -> R,
    ) -> R {
        self.with_scale_transform(self.element_scale.inverse().compose(scale), f)
    }

    fn with_scale_transform<R>(
        &mut self,
        local: UniformScale,
        f: impl FnOnce(&mut Self) -> R,
    ) -> R {
        self.invalidator.debug_assert_paint_or_prepaint();
        if local == UniformScale::default() {
            return f(self);
        }
        let previous = self.element_scale;
        let mask = self.content_mask();
        // Put the inherited screen clip in child layout coordinates so normal
        // child masks can intersect it before the final scene transform.
        self.content_mask_stack.push(ContentMask {
            bounds: local.inverse().bounds(mask.bounds),
        });
        self.element_scale = previous.compose(local);
        let painting = matches!(self.invalidator.inner.borrow().draw_phase, DrawPhase::Paint);
        if painting {
            self.next_frame
                .scene
                .push_scale(local.device(self.scale_factor()));
        }
        let result = f(self);
        if painting {
            self.next_frame.scene.pop_scale();
        }
        self.element_scale = previous;
        self.content_mask_stack.pop();
        result
    }
}
