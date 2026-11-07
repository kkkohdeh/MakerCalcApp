//! SVG paths from printer cards, MaterialIcon, and lucide-react.
use crate::shared::telemetry::LightRole;
use crate::state::LocalPrinterId;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{ActiveTheme as _, Icon, v_flex};
use gpui_kit::{
    Animation, AnimationExt as _, AnyElement, App, ElementId, InteractiveElement as _, IntoElement,
    ParentElement as _, Styled as _, div, px, radians,
};
use std::time::Duration;

pub(super) fn layers(id: LocalPrinterId, progress: f32, cx: &App) -> AnyElement {
    let lit = ((progress.clamp(0., 1.) * 4.).ceil() as usize).clamp(1, 4);
    div()
        .relative()
        .size(px(14.))
        .children((0..4).map(|index| {
            let width = 16. - 2.4 * index as f32;
            let bar = div()
                .absolute()
                .left(px((12. - width / 2.) * 14. / 24.))
                .top(px((19.75 - (index + 1) as f32 * 2.9 - index as f32 * 1.3)
                    * 14.
                    / 24.))
                .w(px(width * 14. / 24.))
                .h(px(2.9 * 14. / 24.))
                .rounded_full()
                .bg(cx.theme().muted_foreground)
                .opacity(if index < lit { 1. } else { 0.28 });
            if index + 1 == lit {
                bar.with_animation(
                    ("bay-layer-pulse", id.0 as usize),
                    Animation::new(Duration::from_millis(1600))
                        .repeat_synced()
                        .with_max_fps(24.),
                    |bar, progress| {
                        bar.opacity(0.775 + 0.225 * (progress * std::f32::consts::TAU).cos())
                    },
                )
                .into_any_element()
            } else {
                bar.into_any_element()
            }
        }))
        .into_any_element()
}

pub(super) fn fan(id: LocalPrinterId, speed: f64, current: bool, cx: &App) -> AnyElement {
    let icon = Icon::default()
        .data(FAN)
        .size(px(14.))
        .text_color(cx.theme().muted_foreground);
    if current && speed > 0. {
        icon.with_animation(
            ("bay-fan-spin", id.0 as usize),
            Animation::new(Duration::from_secs_f64(
                2.4 - 1.9 * speed.clamp(0., 100.) / 100.,
            ))
            .repeat_synced()
            .with_max_fps(24.),
            |icon, progress| icon.rotate(radians(progress * std::f32::consts::TAU)),
        )
        .into_any_element()
    } else {
        icon.into_any_element()
    }
}

pub(super) fn light(
    id: LocalPrinterId,
    role: LightRole,
    enabled: Option<bool>,
    cx: &App,
) -> AnyElement {
    let key = match role {
        LightRole::BuildArea => "area-light-icon",
        LightRole::Toolhead => "hotend-light-icon",
    };
    let key: ElementId = (key, id.0 as usize).into();
    let data = match (role, enabled) {
        (LightRole::Toolhead, _) => HOTEND,
        (LightRole::BuildArea, Some(true)) => LIGHT_ON,
        _ => LIGHT_OFF,
    };
    v_flex()
        .id(key.clone())
        .test_support()
        .relative()
        .size_full()
        .items_center()
        .justify_center()
        .child(
            div()
                .id((key.clone(), "body"))
                .test_support()
                .size(px(14.))
                .child(
                    Icon::default()
                        .data(data)
                        .size(px(14.))
                        .text_color(cx.theme().muted_foreground),
                ),
        )
        .children(enabled.is_none().then(|| {
            div()
                .id((key, "unknown"))
                .test_support()
                .absolute()
                .right(px(1.))
                .bottom(px(1.))
                .size(px(9.))
                .child(
                    Icon::default()
                        .data(UNKNOWN)
                        .size(px(9.))
                        .text_color(cx.theme().yellow),
                )
        }))
        .into_any_element()
}
pub(super) const NOZZLE: &[u8] = br##"<svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" xmlns="http://www.w3.org/2000/svg"><path d="M7.4 3.4h9.2v4.4H7.4z"></path><path d="M8.6 7.8h6.8l-1.8 6.4a1.6 1.6 0 0 1-1.55 1.2h-.1a1.6 1.6 0 0 1-1.55-1.2z"></path></svg>"##;
pub(super) const BED: &[u8] = br##"<svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" xmlns="http://www.w3.org/2000/svg"><rect x="2.6" y="15.4" width="18.8" height="4.6" rx="1.3"></rect><path d="M8.6 12.2c0-1.7 1.7-1.7 1.7-3.4S8.6 7.1 8.6 5.4"></path><path d="M14.4 12.2c0-1.7 1.7-1.7 1.7-3.4s-1.7-1.7-1.7-3.4"></path></svg>"##;
pub(super) const SPEED: &[u8] = br##"<svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" xmlns="http://www.w3.org/2000/svg"><path d="M3.4 17.6a9.4 9.4 0 1 1 17.2 0"></path><path d="M12 17.6 16.4 10"></path><circle cx="12" cy="17.6" r="1.5" fill="currentColor" stroke="none"></circle></svg>"##;
pub(super) const FAN: &[u8] = br##"<svg viewBox="0 0 24 24" width="16" height="16" fill="currentColor" aria-hidden="true" xmlns="http://www.w3.org/2000/svg"><g><path d="M12 12C10.6 7.5 11.6 2.6 15.2 2.2C18.8 1.8 20.4 5.4 18 8.2C16 10.5 13.8 11.5 12 12Z" transform="rotate(0 12 12)"></path><path d="M12 12C10.6 7.5 11.6 2.6 15.2 2.2C18.8 1.8 20.4 5.4 18 8.2C16 10.5 13.8 11.5 12 12Z" transform="rotate(90 12 12)"></path><path d="M12 12C10.6 7.5 11.6 2.6 15.2 2.2C18.8 1.8 20.4 5.4 18 8.2C16 10.5 13.8 11.5 12 12Z" transform="rotate(180 12 12)"></path><path d="M12 12C10.6 7.5 11.6 2.6 15.2 2.2C18.8 1.8 20.4 5.4 18 8.2C16 10.5 13.8 11.5 12 12Z" transform="rotate(270 12 12)"></path><circle cx="12" cy="12" r="2.1"></circle></g></svg>"##;
pub(super) const FILAMENT: &[u8] = br##"<svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" xmlns="http://www.w3.org/2000/svg"><path pathLength="1" d="M13 4C17.5 3.2 20.5 4.5 21.5 7.5"></path><path d="M14.5 4H9.5a5 8 0 0 0 0 16h5"></path><ellipse cx="14.5" cy="12" rx="5" ry="8"></ellipse><ellipse cx="14.5" cy="12" rx="1.8" ry="2.5"></ellipse></svg>"##;
pub(super) const TIME: &[u8] = br##"<svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" xmlns="http://www.w3.org/2000/svg"><circle cx="12" cy="12" r="8.6"></circle><path d="M12 6.8V12l3.4 2"></path></svg>"##;
pub(super) const LIGHT_OFF: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M15 14c.2-1 .7-1.7 1.5-2.5 1-.9 1.5-2.2 1.5-3.5A6 6 0 0 0 6 8c0 1 .2 2.2 1.5 3.5.7.7 1.3 1.5 1.5 2.5"></path><path d="M9 18h6"></path><path d="M10 22h4"></path></svg>"##;
pub(super) const LIGHT_ON: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" class="lucide lucide-lightbulb-off" aria-hidden="true"><path d="M16.8 11.2c.8-.9 1.2-2 1.2-3.2a6 6 0 0 0-9.3-5"></path><path d="m2 2 20 20"></path><path d="M6.3 6.3a4.67 4.67 0 0 0 1.2 5.2c.7.7 1.3 1.5 1.5 2.5"></path><path d="M9 18h6"></path><path d="M10 22h4"></path></svg>"##;
pub(super) const HOTEND: &[u8] = br##"<svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" xmlns="http://www.w3.org/2000/svg"><path d="M7.4 3.4h9.2v4.4H7.4z"></path><path d="M8.6 7.8h6.8l-1.8 6.4a1.6 1.6 0 0 1-1.55 1.2h-.1a1.6 1.6 0 0 1-1.55-1.2z"></path></svg>"##;
pub(super) const UNKNOWN: &[u8] = br##"<svg viewBox="13.4 14.1 9 9" fill="none" stroke="currentColor" stroke-width="1.8" xmlns="http://www.w3.org/2000/svg"><path d="M16.5 16.5a2 2 0 1 1 2.8 1.8c-.9.4-1.1.7-1.1 1.3M18.2 22h.01" stroke-linecap="round"></path></svg>"##;
pub(super) const PAUSE: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" class="lucide lucide-pause" aria-hidden="true"><rect x="14" y="3" width="5" height="18" rx="1"></rect><rect x="5" y="3" width="5" height="18" rx="1"></rect></svg>"##;
pub(super) const RESUME: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" class="lucide lucide-play" aria-hidden="true"><path d="M5 5a2 2 0 0 1 3.008-1.728l11.997 6.998a2 2 0 0 1 .003 3.458l-12 7A2 2 0 0 1 5 19z"></path></svg>"##;
pub(super) const STOP: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" class="lucide lucide-square" aria-hidden="true"><rect width="18" height="18" x="3" y="3" rx="2"></rect></svg>"##;
pub(super) const COMPLETED: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" class="lucide lucide-circle-check" aria-hidden="true"><circle cx="12" cy="12" r="10"></circle><path d="m9 12 2 2 4-4"></path></svg>"##;
pub(super) const CANCELLED: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" class="lucide lucide-circle-x" aria-hidden="true"><circle cx="12" cy="12" r="10"></circle><path d="m15 9-6 6"></path><path d="m9 9 6 6"></path></svg>"##;
