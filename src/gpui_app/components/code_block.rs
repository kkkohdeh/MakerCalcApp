use gpui_kit::base::{SelectableText, TestSupportExt as _};

use gpui_kit::component::{
    ActiveTheme as _, InteractiveElementExt as _, scroll::ScrollableElement as _,
};

use gpui_kit::{
    App, Bounds, BoxShadow, ElementId, FocusHandle, InteractiveElement as _, IntoElement,
    KeyDownEvent, MouseButton, PaintQuad, ParentElement as _, RenderOnce, Role, ScrollHandle,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, canvas, div,
    linear_color_stop, linear_gradient, point, px, size,
};

#[derive(Clone)]
struct State {
    focus: FocusHandle,
    scroll: ScrollHandle,
}

#[derive(IntoElement)]
pub(in crate::gpui_app) struct CodeBlock {
    id: ElementId,
    text: SharedString,
    label: SharedString,
}

impl CodeBlock {
    pub(in crate::gpui_app) fn new(
        id: impl Into<ElementId>,
        text: impl Into<SharedString>,
        label: impl Into<SharedString>,
    ) -> Self {
        Self {
            id: id.into(),
            text: text.into(),
            label: label.into(),
        }
    }
}

impl RenderOnce for CodeBlock {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = window
            .use_keyed_state((self.id.clone(), "state"), cx, |_, cx| State {
                focus: cx.focus_handle().tab_stop(true),
                scroll: ScrollHandle::default(),
            })
            .read(cx)
            .clone();
        let ring = if state.focus.is_focused(window) && window.last_input_was_keyboard() {
            (cx.theme().ring.opacity(0.3), 3.)
        } else {
            (cx.theme().border, 1.)
        };
        let key_state = state.clone();
        let mouse_focus = state.focus.clone();
        let fade_scroll = state.scroll.clone();
        let surface = cx.theme().group_box;

        div()
            .id(self.id.clone())
            .test_support()
            .relative()
            .w_full()
            .h(px(140.))
            .min_w_0()
            .rounded(px(14.))
            .overflow_hidden()
            .shadow(vec![BoxShadow {
                inset: false,
                color: ring.0,
                offset: point(px(0.), px(0.)),
                blur_radius: px(0.),
                spread_radius: px(ring.1),
            }])
            .child(
                div()
                    .id((self.id.clone(), "viewport"))
                    .size_full()
                    .overflow_y_scroll()
                    .lock_scroll_axis()
                    .test_support()
                    .track_scroll(&state.scroll)
                    .track_focus(&state.focus)
                    .role(Role::Document)
                    .aria_label(self.label)
                    .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                        mouse_focus.focus(window, cx)
                    })
                    .on_key_down(move |event: &KeyDownEvent, window, cx| {
                        if !key_state.focus.is_focused(window)
                            || event.keystroke.modifiers.control
                            || event.keystroke.modifiers.platform
                            || event.keystroke.modifiers.alt
                        {
                            return;
                        }
                        let offset = key_state.scroll.offset().y;
                        let page = key_state.scroll.bounds().size.height;
                        let bottom = -key_state.scroll.max_offset().y;
                        let next = match event.keystroke.key.as_str() {
                            "up" => offset + px(19.5),
                            "down" => offset - px(19.5),
                            "pageup" => offset + page,
                            "pagedown" => offset - page,
                            "space" if event.keystroke.modifiers.shift => offset + page,
                            "space" => offset - page,
                            "home" => px(0.),
                            "end" => bottom,
                            _ => return,
                        };
                        key_state
                            .scroll
                            .set_offset(point(px(0.), next.clamp(bottom, px(0.))));
                        window.refresh();
                        window.prevent_default();
                        cx.stop_propagation();
                    })
                    .child(
                        div()
                            .id((self.id, "content"))
                            .test_support()
                            .min_w_0()
                            .w_full()
                            .p(px(12.))
                            .text_size(px(12.))
                            .line_height(px(19.5))
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_color(cx.theme().muted_foreground)
                            .child(SelectableText::new("diagnostics-code", self.text)),
                    ),
            )
            .child(
                canvas(
                    |_, _, _| (),
                    move |bounds, _, window, _| {
                        // Native gradients preserve the opaque surface and pointer input during scroll fades.
                        let extent = (bounds.size.height * 0.12).min(px(40.));
                        let top = -fade_scroll.offset().y;
                        let remaining = fade_scroll.max_offset().y - top;
                        for (visible, y, angle) in [
                            (top > px(0.), bounds.top(), 180.),
                            (remaining > px(0.), bounds.bottom() - extent, 0.),
                        ] {
                            if visible {
                                window.paint_quad(PaintQuad {
                                    bounds: Bounds::new(
                                        point(bounds.left(), y),
                                        size(bounds.size.width, extent),
                                    ),
                                    background: linear_gradient(
                                        angle,
                                        linear_color_stop(surface, 0.),
                                        linear_color_stop(surface.opacity(0.), 1.),
                                    ),
                                    corner_radii: Default::default(),
                                    border_widths: Default::default(),
                                    border_color: gpui_kit::transparent_black(),
                                    border_style: Default::default(),
                                });
                            }
                        }
                    },
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            )
            .vertical_scrollbar(&state.scroll)
    }
}
