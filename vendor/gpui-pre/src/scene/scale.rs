//! Positive, uniform subtree scale; no changes to GPU instance formats.
use super::*;
use crate::{px, size};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct UniformScale {
    pub(crate) factor: f32,
    translation: Point<f32>,
}

impl Default for UniformScale {
    fn default() -> Self {
        Self {
            factor: 1.,
            translation: point(0., 0.),
        }
    }
}

impl UniformScale {
    pub(crate) fn around(origin: Point<Pixels>, factor: f32) -> Self {
        assert!(
            factor.is_finite() && factor > 0.,
            "element scale must be finite and positive"
        );
        assert!(
            origin.x.0.is_finite() && origin.y.0.is_finite(),
            "element scale origin must be finite"
        );
        Self {
            factor,
            translation: point(origin.x.0 * (1. - factor), origin.y.0 * (1. - factor)),
        }
    }

    pub(crate) fn compose(self, local: Self) -> Self {
        Self {
            factor: self.factor * local.factor,
            translation: point(
                self.translation.x + self.factor * local.translation.x,
                self.translation.y + self.factor * local.translation.y,
            ),
        }
    }

    pub(crate) fn inverse(self) -> Self {
        Self {
            factor: 1. / self.factor,
            translation: point(
                -self.translation.x / self.factor,
                -self.translation.y / self.factor,
            ),
        }
    }

    pub(crate) fn device(self, dpi: f32) -> Self {
        Self {
            translation: point(self.translation.x * dpi, self.translation.y * dpi),
            ..self
        }
    }

    pub(crate) fn point(self, p: Point<Pixels>) -> Point<Pixels> {
        point(
            px(p.x.0 * self.factor + self.translation.x),
            px(p.y.0 * self.factor + self.translation.y),
        )
    }

    pub(crate) fn bounds(self, b: Bounds<Pixels>) -> Bounds<Pixels> {
        Bounds {
            origin: self.point(b.origin),
            size: size(b.size.width * self.factor, b.size.height * self.factor),
        }
    }

    fn scaled_point(self, p: Point<ScaledPixels>) -> Point<ScaledPixels> {
        point(
            ScaledPixels(p.x.0 * self.factor + self.translation.x),
            ScaledPixels(p.y.0 * self.factor + self.translation.y),
        )
    }

    pub(super) fn scaled_bounds(self, b: Bounds<ScaledPixels>) -> Bounds<ScaledPixels> {
        Bounds {
            origin: self.scaled_point(b.origin),
            size: size(b.size.width * self.factor, b.size.height * self.factor),
        }
    }

    fn mask(self, mask: &mut ContentMask<ScaledPixels>) {
        mask.bounds = self.scaled_bounds(mask.bounds);
    }

    // Bounds are scaled separately, so conjugate any existing SVG/glyph
    // transform: A * M * inverse(A). UVs and atlas tiles remain unchanged.
    fn sprite_matrix(self, matrix: &mut TransformationMatrix) {
        let [x, y] = matrix.translation;
        matrix.translation = [
            self.factor * x + self.translation.x
                - matrix.rotation_scale[0][0] * self.translation.x
                - matrix.rotation_scale[0][1] * self.translation.y,
            self.factor * y + self.translation.y
                - matrix.rotation_scale[1][0] * self.translation.x
                - matrix.rotation_scale[1][1] * self.translation.y,
        ];
    }
}

impl Scene {
    /// Paint a device-pixel subtree at a positive uniform scale.
    /// Cached operations remain in their original coordinates and can be
    /// replayed at another scale without compounding the previous frame.
    pub fn with_uniform_scale<R>(
        &mut self,
        origin: Point<ScaledPixels>,
        factor: f32,
        f: impl FnOnce(&mut Self) -> R,
    ) -> R {
        let scale = UniformScale::around(origin.map(|p| px(p.0)), factor);
        if scale == UniformScale::default() {
            return f(self);
        }
        self.push_scale(scale);
        let result = f(self);
        self.pop_scale();
        result
    }

    pub(crate) fn push_scale(&mut self, local: UniformScale) {
        let combined = self
            .scale_stack
            .last()
            .copied()
            .unwrap_or_default()
            .compose(local);
        self.scale_stack.push(combined);
        self.paint_operations
            .push(PaintOperation::StartScale(local));
    }

    pub(crate) fn pop_scale(&mut self) {
        self.scale_stack
            .pop()
            .expect("unbalanced scene scale scope");
        self.paint_operations.push(PaintOperation::EndScale);
    }
}

impl Primitive {
    pub(super) fn apply_scale(&mut self, scale: UniformScale) {
        let s = scale.factor;
        match self {
            #[cfg(target_os = "windows")]
            Self::BackdropBlur(v) => {
                v.bounds = scale.scaled_bounds(v.bounds);
                scale.mask(&mut v.content_mask);
                v.radius = v.radius * s;
            }
            Self::Quad(v) => {
                v.bounds = scale.scaled_bounds(v.bounds);
                scale.mask(&mut v.content_mask);
                v.corner_radii = v.corner_radii.map(|p| *p * s);
                v.border_widths = v.border_widths.map(|p| *p * s);
            }
            Self::Shadow(v) => {
                v.bounds = scale.scaled_bounds(v.bounds);
                v.element_bounds = scale.scaled_bounds(v.element_bounds);
                scale.mask(&mut v.content_mask);
                v.blur_radius = v.blur_radius * s;
                v.corner_radii = v.corner_radii.map(|p| *p * s);
                v.element_corner_radii = v.element_corner_radii.map(|p| *p * s);
            }
            Self::Underline(v) => {
                v.bounds = scale.scaled_bounds(v.bounds);
                scale.mask(&mut v.content_mask);
                v.thickness = v.thickness * s;
            }
            Self::MonochromeSprite(v) => {
                v.bounds = scale.scaled_bounds(v.bounds);
                scale.mask(&mut v.content_mask);
                scale.sprite_matrix(&mut v.transformation);
            }
            Self::SubpixelSprite(v) => {
                v.bounds = scale.scaled_bounds(v.bounds);
                scale.mask(&mut v.content_mask);
                scale.sprite_matrix(&mut v.transformation);
            }
            Self::PolychromeSprite(v) => {
                v.bounds = scale.scaled_bounds(v.bounds);
                scale.mask(&mut v.content_mask);
                v.corner_radii = v.corner_radii.map(|p| *p * s);
            }
            Self::Surface(v) => {
                v.bounds = scale.scaled_bounds(v.bounds);
                scale.mask(&mut v.content_mask);
            }
            Self::Path(v) => {
                v.bounds = scale.scaled_bounds(v.bounds);
                v.start = scale.scaled_point(v.start);
                v.current = scale.scaled_point(v.current);
                scale.mask(&mut v.content_mask);
                for vertex in &mut v.vertices {
                    vertex.xy_position = scale.scaled_point(vertex.xy_position);
                    scale.mask(&mut vertex.content_mask);
                }
            }
        }
    }
}
