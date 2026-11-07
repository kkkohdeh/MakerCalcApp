use std::{panic::Location, time::Duration};

use gpui_kit::base::motion::{Easing, Transition, transition};

use gpui_kit::base::{ScrollbarMode, ScrollbarMotion, ScrollbarStyles, TestSupportExt as _};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::scroll::{Scrollable, ScrollableElement};
use gpui_kit::{
    App, BoxShadow, Div, ElementId, KeyDownEvent, ParentElement as _, Role, ScrollHandle, Stateful,
    StatefulInteractiveElement, Styled as _, Window, point, px,
};

/// Native lists/tables and ordinary areas share these styles, not a second scroll engine.
pub(in crate::gpui_app) fn configure(cx: &mut App) {
    let clear = gpui_kit::transparent_black();
    let thumb = cx.theme().scrollbar_thumb;
    let styles = ScrollbarStyles::default()
        .track(|style| style.width(px(10.)).bg(clear).border_color(clear))
        .thumb(|style| style.width(px(7.)).inset(px(1.)).radius(px(999.)).bg(thumb));
    let scrollbar = &mut gpui_kit::base::Theme::global_mut(cx).scrollbar;
    *scrollbar = scrollbar
        .clone()
        .with_styles(styles)
        .with_mode(ScrollbarMode::Always)
        .with_motion(ScrollbarMotion::default());
}

pub(in crate::gpui_app) trait ScrollAreaExt: ScrollableElement + Sized {
    #[track_caller]
    fn scroll_area(self) -> Scrollable<Self> {
        self.overflow_y_scrollbar().presentation(present)
    }
}
impl<E: ScrollableElement> ScrollAreaExt for E {}

fn present(
    root: Stateful<Div>,
    area: Stateful<Div>,
    scroll: &ScrollHandle,
    window: &mut Window,
    cx: &mut App,
) -> Stateful<Div> {
    let (area, ring) = viewport(area, scroll, window, cx);
    // Paint the ring on the root, outside the viewport's clip.
    root.shadow(ring).child(area.test_support())
}

/// Tracked viewports keep their layout, identity and handle.
#[track_caller]
pub(in crate::gpui_app) fn vertical<E>(
    area: E,
    scroll: &ScrollHandle,
    window: &mut Window,
    cx: &mut App,
) -> E
where
    E: ScrollableElement + StatefulInteractiveElement,
{
    let (area, ring) = viewport(area, scroll, window, cx);
    area.shadow(ring).vertical_scrollbar(scroll)
}

#[track_caller]
fn viewport<E: StatefulInteractiveElement + gpui_kit::Styled>(
    mut area: E,
    scroll: &ScrollHandle,
    window: &mut Window,
    cx: &mut App,
) -> (E, Vec<BoxShadow>) {
    // Preserve caller insets; otherwise leave room for outer rings and the 10px bar.
    let padding = &mut area.style().padding;
    padding.left.get_or_insert(px(4.).into());
    padding.right.get_or_insert(px(14.).into());
    padding.top.get_or_insert(px(4.).into());
    padding.bottom.get_or_insert(px(4.).into());
    let id = area
        .interactivity()
        .element_id
        .clone()
        .unwrap_or_else(|| ElementId::CodeLocation(*Location::caller()));
    let focus = window
        .use_keyed_state((id.clone(), "focus"), cx, |_, cx| cx.focus_handle())
        .read(cx)
        .clone();
    let focused = focus.is_focused(window) && window.last_input_was_keyboard();
    let alpha = transition(
        (id, "ring"),
        if focused { 0.5_f32 } else { 0. },
        Transition::new(Duration::from_millis(150)).easing(Easing::CubicBezier {
            x1: 0.4,
            y1: 0.,
            x2: 0.2,
            y2: 1.,
        }),
        window,
        cx,
    );
    let ring = vec![BoxShadow {
        inset: false,
        color: cx.theme().ring.opacity(alpha),
        offset: point(px(0.), px(0.)),
        blur_radius: px(0.),
        spread_radius: px(3.),
    }];
    let keys = scroll.clone();
    let view_id = window.current_view();
    let area = area.role(Role::ScrollView).track_focus(&focus).on_key_down(
        move |event: &KeyDownEvent, window, cx| {
            if !focus.is_focused(window)
                || event.keystroke.modifiers.control
                || event.keystroke.modifiers.platform
                || event.keystroke.modifiers.alt
            {
                return;
            }
            let offset = keys.offset();
            let page = keys.bounds().size.height * 0.9;
            let bottom = -keys.max_offset().y;
            let y = match event.keystroke.key.as_str() {
                "up" => offset.y + px(40.),
                "down" => offset.y - px(40.),
                "pageup" => offset.y + page,
                "pagedown" => offset.y - page,
                "space" if event.keystroke.modifiers.shift => offset.y + page,
                "space" => offset.y - page,
                "home" => px(0.),
                "end" => bottom,
                _ => return,
            };
            keys.set_offset(point(offset.x, y.clamp(bottom, px(0.))));
            window.prevent_default();
            cx.stop_propagation();
            cx.notify(view_id);
        },
    );
    (area, ring)
}
