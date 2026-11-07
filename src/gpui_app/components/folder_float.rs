//! Folder Float, adapted from React Bits (Copyright 2026 David Haz).
//! See assets/licenses/react-bits-LICENSE.md. Native presentation for file drops;
//! the caller owns drag lifecycle and uploading. No physics engine or background task.
use std::time::Duration;

use gpui_kit::base::{StyledExt as _, TestSupportExt as _};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{
    App, BoxShadow, InteractiveElement as _, IntoElement, ParentElement as _, PathBuilder,
    RenderOnce, Styled as _, Window, canvas, div, point, px, rgb,
};

const EXAMPLES: [(&str, f32, f32); 4] = [
    ("3DBenchy.gcode", 2., 48.),
    ("CalibrationCube.gcode", 174., 2.),
    ("TempTower.gcode", 182., 53.),
    ("Gridfinity.gcode", 12., 3.),
];

#[derive(IntoElement)]
pub(in crate::gpui_app) struct FolderFloat {
    elapsed: Duration,
    active: bool,
    valid: bool,
}

impl FolderFloat {
    pub(in crate::gpui_app) fn new(elapsed: Duration, active: bool, valid: bool) -> Self {
        Self {
            elapsed,
            active,
            valid,
        }
    }
}

fn spring(t: f32) -> f32 {
    let t = t.clamp(0., 1.) - 1.;
    1. + 2.7 * t * t * t + 1.7 * t * t
}

impl RenderOnce for FolderFloat {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let reduced = cx.reduce_motion();
        let elapsed = self.elapsed.as_secs_f32();
        let open = if reduced {
            1.
        } else {
            (elapsed / 0.52).clamp(0., 1.)
        };
        if self.active && !reduced {
            window.request_animation_frame();
        }
        let front = rgb(0x303030);
        let back = cx.theme().border;
        let paper = cx.theme().foreground;
        div()
            .id("folder-float")
            .test_support()
            .relative()
            .flex_shrink_0()
            .w(px(360.))
            .h(px(220.))
            .child(
                div()
                    .absolute()
                    .left(px(80.))
                    .top(px(68.))
                    .w(px(200.))
                    .h(px(148.))
                    .rounded(px(14.))
                    .bg(back),
            )
            .child(
                div()
                    .absolute()
                    .left(px(80.))
                    .top(px(54.))
                    .w(px(84.))
                    .h(px(28.))
                    .rounded_t(px(12.))
                    .bg(back),
            )
            .child(
                div()
                    .id("folder-float-paper")
                    .test_support()
                    .absolute()
                    .left(px(96.))
                    .top(px(90. - open * 12.))
                    .w(px(168.))
                    .h(px(74.))
                    .rounded(px(6.))
                    .bg(paper)
                    .opacity(open),
            )
            .children(EXAMPLES.iter().enumerate().map(|(index, &(label, x, y))| {
                let t = if reduced {
                    1.
                } else {
                    ((elapsed - index as f32 * 0.045) / 0.52).clamp(0., 1.)
                };
                let travel = spring(t);
                let drift = if reduced {
                    0.
                } else {
                    (elapsed * 1.96 + index as f32 * 1.4).sin() * 1.5 * t
                };
                div()
                    .id(("folder-float-example", index))
                    .test_support()
                    .absolute()
                    .left(px(100. + (x - 100.) * travel))
                    .top(px(130. + (y - 130.) * travel + drift))
                    .h(px(30.))
                    .px(px(12.))
                    .rounded(px(17.))
                    .flex()
                    .items_center()
                    .text_size(px(12.))
                    .line_height(px(16.))
                    .bg(paper)
                    .text_color(cx.theme().background)
                    .opacity(t)
                    .shadow(vec![BoxShadow {
                        color: rgb(0x000000).opacity(0.2).into(),
                        offset: point(px(0.), px(4.)),
                        blur_radius: px(12.),
                        spread_radius: px(0.),
                        inset: false,
                    }])
                    .child(label)
            }))
            .child(
                canvas(
                    |_, _, _| (),
                    move |bounds, _, window, _| {
                        // Project the front flap into 2D instead of extending the renderer with CSS 3D.
                        let origin = bounds.origin;
                        let top = 35. + open * 27.;
                        let wide = open * 8.;
                        let p = |x, y| origin + point(px(x), px(y));
                        let mut path = PathBuilder::fill();
                        path.move_to(p(14. - wide, top));
                        path.line_to(p(186. + wide, top));
                        path.curve_to(p(200. + wide, top + 14.), p(200. + wide, top));
                        path.line_to(p(200., 134.));
                        path.curve_to(p(186., 148.), p(200., 148.));
                        path.line_to(p(14., 148.));
                        path.curve_to(p(0., 134.), p(0., 148.));
                        path.line_to(p(-wide, top + 14.));
                        path.curve_to(p(14. - wide, top), p(-wide, top));
                        path.close();
                        if let Ok(path) = path.build() {
                            window.paint_path(path, front);
                        }
                    },
                )
                .absolute()
                .left(px(80.))
                .top(px(68.))
                .w(px(200.))
                .h(px(148.)),
            )
            .child(
                div()
                    .absolute()
                    .left(px(96.))
                    .bottom(px(25.))
                    .text_sm()
                    .font_medium()
                    .text_color(paper)
                    .child("G-code"),
            )
            .child(
                div()
                    .absolute()
                    .left(px(96.))
                    .bottom(px(9.))
                    .text_xs()
                    .text_color(if self.valid {
                        cx.theme().muted_foreground
                    } else {
                        cx.theme().warning
                    })
                    .child(".gcode · .g"),
            )
    }
}
