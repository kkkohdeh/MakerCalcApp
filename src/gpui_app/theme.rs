use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::{App, Hsla, px, rgb};

pub(super) const CARD_FONT: &str = "Geist";

pub(super) fn load_card_font(cx: &App) -> Result<(), String> {
    cx.text_system()
        .add_fonts(vec![std::borrow::Cow::Borrowed(include_bytes!(
            "../../assets/fonts/geist-latin.ttf"
        ))])
        .map_err(|error| error.to_string())
}

pub(super) fn apply_makercalc_theme(cx: &mut App) {
    Theme::change(ThemeMode::Dark, None, cx);

    let background = rgb(0x000000).into();
    let card = rgb(0x121212).into();
    let border = rgb(0x292929).into();
    let border_hover = rgb(0x404040).into();
    let foreground = rgb(0xeaeaea).into();
    let muted_foreground = rgb(0xa0a0a0).into();
    let input_background = rgb(0x171717).into();
    let secondary = rgb(0x202020).into();
    let secondary_hover = rgb(0x2e2e2e).into();
    let primary = rgb(0x3b82f6).into();
    let primary_hover = rgb(0x60a5fa).into();
    let primary_active = rgb(0x2563eb).into();
    let success = primary;
    let warning = rgb(0xeab308).into();
    let danger = rgb(0xdc2626).into();
    let danger_hover = rgb(0xef4444).into();
    let danger_active = rgb(0xb91c1c).into();
    let white = rgb(0xffffff).into();

    let theme = Theme::global_mut(cx);
    theme.mode = ThemeMode::Dark;
    let colors = &mut theme.colors;
    colors.background = background;
    colors.foreground = foreground;
    colors.border = border;
    colors.input = border;
    colors.group_box = card;
    colors.group_box_foreground = foreground;
    colors.popover = background;
    colors.popover_foreground = foreground;
    colors.list = card;
    colors.list_even = card;
    colors.list_head = card;
    colors.list_hover = secondary;
    colors.list_active = secondary_hover;
    colors.list_active_border = border_hover;
    colors.table = card;
    colors.table_even = card;
    colors.table_head = card;
    colors.table_hover = secondary;
    colors.table_active = secondary_hover;
    colors.table_active_border = border_hover;
    colors.table_row_border = border;
    colors.title_bar = background;
    colors.title_bar_border = background;
    colors.status_bar = background;
    colors.status_bar_border = border;
    colors.sidebar = card;
    colors.sidebar_border = border;
    colors.sidebar_foreground = foreground;
    colors.accent = secondary;
    colors.accent_foreground = foreground;
    colors.muted = secondary;
    colors.muted_foreground = muted_foreground;
    colors.button = secondary;
    colors.button_hover = secondary_hover;
    colors.button_active = input_background;
    colors.button_foreground = foreground;
    colors.secondary = secondary;
    colors.secondary_hover = secondary_hover;
    colors.secondary_active = input_background;
    colors.secondary_foreground = foreground;
    colors.button_secondary = secondary;
    colors.button_secondary_hover = secondary_hover;
    colors.button_secondary_active = input_background;
    let dark_text: Hsla = rgb(0x09090b).into();
    let white_btn: Hsla = rgb(0xfafafa).into();
    let white_btn_hover: Hsla = rgb(0xf5f5f5).into();
    let white_btn_active: Hsla = rgb(0xe5e5e5).into();
    colors.primary = primary;
    colors.primary_hover = primary_hover;
    colors.primary_active = primary_active;
    colors.primary_foreground = dark_text;
    colors.button_primary = white_btn;
    colors.button_primary_hover = white_btn_hover;
    colors.button_primary_active = white_btn_active;
    colors.button_primary_foreground = dark_text;
    theme.tokens.primary_foreground = dark_text.into();
    theme.tokens.button_primary = white_btn.into();
    theme.tokens.button_primary_hover = white_btn_hover.into();
    theme.tokens.button_primary_active = white_btn_active.into();
    theme.tokens.button_primary_foreground = dark_text.into();
    colors.success = success;
    colors.green = success;
    colors.warning = warning;
    colors.yellow = warning;
    colors.danger = danger;
    colors.danger_hover = danger_hover;
    colors.danger_active = danger_active;
    colors.danger_foreground = white;
    colors.button_danger = danger;
    colors.button_danger_hover = danger_hover;
    colors.button_danger_active = danger_active;
    colors.button_danger_foreground = white;
    theme.tokens.button_danger = danger.into();
    theme.tokens.button_danger_hover = danger_hover.into();
    theme.tokens.button_danger_active = danger_active.into();
    theme.tokens.button_danger_foreground = white.into();
    colors.red = danger_hover;
    colors.blue = primary;
    colors.tab_active_foreground = white;
    colors.info = primary_hover;
    colors.info_foreground = white;
    colors.link = primary_hover;
    colors.link_hover = primary_hover;
    colors.ring = primary_hover;
    colors.caret = primary_hover;
    colors.selection = primary;
    colors.progress_bar = secondary;
    colors.scrollbar = gpui_kit::transparent_black();
    colors.scrollbar_thumb = border_hover;
    colors.scrollbar_thumb_hover = muted_foreground;
    colors.window_border = border;
    colors.overlay = background.opacity(0.72);
    theme.radius = px(8.);
    theme.radius_lg = px(16.);
    theme.shadow = false;
    theme.focus_ring = true;

    Theme::sync_base(cx);
    // Base theme synchronization replaces scrollbar styles; apply the port afterwards.
    super::components::scroll_area::configure(cx);
}
