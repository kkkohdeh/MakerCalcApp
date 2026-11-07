use super::super::components::button::Button;
use gpui_kit::base::StyledExt as _;
use gpui_kit::component::label::Label;
use gpui_kit::component::menu::PopupMenuItem;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::{AnyElement, Context, IntoElement as _, ParentElement as _, Styled as _};

use super::super::MakerCalcGpuiApp;
use crate::i18n::t;
use crate::shared::workspace::{DesktopPreferences, UiLanguage};

fn language_label(language: UiLanguage) -> String {
    match language {
        UiLanguage::System => t!("common.languages.system").into_owned(),
        UiLanguage::English => t!("common.languages.en").into_owned(),
        UiLanguage::Spanish => t!("common.languages.es").into_owned(),
    }
}

impl MakerCalcGpuiApp {
    fn select_language(&mut self, language: UiLanguage, cx: &mut Context<Self>) {
        let preferences = self.desktop_settings.preferences;
        if preferences.language != language {
            self.save_desktop_preferences(
                DesktopPreferences {
                    language,
                    ..preferences
                },
                cx,
            );
        }
    }

    pub(super) fn render_language_setting(&self, cx: &mut Context<Self>) -> AnyElement {
        let selected = self.desktop_settings.preferences.language;
        let app = cx.entity().downgrade();
        v_flex()
            .gap_2()
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .justify_between()
                    .child(Label::new(t!("settings.language.label").into_owned()).font_semibold())
                    .child(
                        Button::new("settings-language")
                            .popup_trigger()
                            .outline()
                            .small()
                            .label(language_label(selected))
                            .disabled(self.mutation_pending)
                            .select_menu(
                                match selected {
                                    UiLanguage::System => 0,
                                    UiLanguage::English => 1,
                                    UiLanguage::Spanish => 2,
                                },
                                move |mut menu, _, _| {
                                    for language in [
                                        UiLanguage::System,
                                        UiLanguage::English,
                                        UiLanguage::Spanish,
                                    ] {
                                        let app = app.clone();
                                        menu = menu.item(
                                            PopupMenuItem::new(language_label(language))
                                                .checked(language == selected)
                                                .on_click(move |_, _, cx| {
                                                    let _ = app.update(cx, |app, cx| {
                                                        app.select_language(language, cx)
                                                    });
                                                }),
                                        );
                                    }
                                    menu
                                },
                            ),
                    ),
            )
            .child(
                Label::new(t!("settings.language.restart_help").into_owned())
                    .text_xs()
                    .text_color(cx.theme().muted_foreground),
            )
            .into_any_element()
    }
}
