use super::components::button::Button;
use crate::i18n::t;
use gpui_kit::base::{StyledExt as _, TestSupportExt as _};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, TitleBar, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Bounds, Context, Div, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, Pixels, Size, Stateful, Styled as _, Window, WindowControlArea, canvas,
    div, fill, px, size, transparent_black,
};

use crate::state::LocalPrinterId;

use super::media::render_brand_logo;
use super::{MakerCalcGpuiApp, PrinterEntry};

pub(super) fn initial_window_geometry(work_area: Bounds<Pixels>) -> (Bounds<Pixels>, Size<Pixels>) {
    // Leave room for the native frame, without covering the taskbar or docked panels.
    let available = size(
        (work_area.size.width - px(32.)).max(px(1.)),
        (work_area.size.height - px(32.)).max(px(1.)),
    );
    let preferred = size(
        px(1200.).min(available.width),
        px(720.).min(available.height),
    );
    let minimum = size(
        px(640.).min(available.width),
        px(480.).min(available.height),
    );
    (Bounds::centered_at(work_area.center(), preferred), minimum)
}

pub(super) fn modal_backdrop(id: &'static str, overlay: Hsla) -> Stateful<Div> {
    div().id(id).absolute().inset_0().occlude().child(
        div()
            .id(format!("modal-blur-{id}"))
            .test_support()
            .absolute()
            .inset_0()
            .child(
                canvas(
                    |_, _, _| (),
                    move |bounds, _, window, _| {
                        #[cfg(target_os = "windows")]
                        window.paint_backdrop_blur(bounds, px(6.));
                        window.paint_quad(fill(bounds, overlay));
                    },
                )
                .size_full(),
            ),
    )
}

impl MakerCalcGpuiApp {
    pub(super) fn render_window_controls() -> impl IntoElement {
        #[cfg(not(target_os = "macos"))]
        {
            div()
                .id("window-controls")
                .test_support()
                .flex_none()
                .w(px(105.))
                .h(px(44.))
                .child(
                    TitleBar::new()
                        .h(px(44.))
                        .pl(px(0.))
                        .border_b_0()
                        .bg(transparent_black()),
                )
        }
        #[cfg(target_os = "macos")]
        {
            div()
        }
    }

    pub(super) fn render_title_bar(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let update_status = self.updater.status.lock().unwrap().clone();
        let compact = f32::from(window.viewport_size().width) < 900.;
        if let Some((id, printer)) = self
            .selected
            .and_then(|id| self.printers.get(&id).map(|printer| (id, printer)))
        {
            Self::render_detail_title_bar(id, printer, &update_status, compact, cx)
        } else {
            self.render_bay_title_bar(&update_status, compact, cx)
        }
    }

    pub(super) fn render_bay_title_bar(
        &self,
        update_status: &crate::updater::UpdateStatus,
        compact: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let left_padding = if cfg!(target_os = "macos") {
            px(80.)
        } else {
            px(20.)
        };
        h_flex()
            .items_center()
            .w_full()
            .h(px(44.))
            .pl(left_padding)
            .pr_0()
            .bg(cx.theme().background)
            .child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_start()
                    .child(
                        h_flex()
                            .items_center()
                            .gap_3()
                            .child(render_brand_logo(cx))
                            .when(!compact, |row| {
                                row.child(div().h(px(14.)).w(px(1.)).bg(cx.theme().border))
                                    .child(
                                        div()
                                            .text_xs()
                                            .font_medium()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(t!("common.labels.printers")),
                                    )
                            }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .h_full()
                            .window_control_area(WindowControlArea::Drag),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_end()
                    .child(
                        div()
                            .flex_1()
                            .h_full()
                            .window_control_area(WindowControlArea::Drag),
                    )
                    .child(
                        h_flex()
                            .gap_4()
                            .items_center()
                            .pr_3()
                            .when(
                                matches!(update_status, crate::updater::UpdateStatus::Downloading),
                                |bar| {
                                    bar.child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(if compact {
                                                t!("common.updates.downloading_short").into_owned()
                                            } else {
                                                t!("common.updates.downloading").into_owned()
                                            }),
                                    )
                                },
                            )
                            .when_some(
                                if let crate::updater::UpdateStatus::ReadyToRestart(v) =
                                    update_status
                                {
                                    Some(v.clone())
                                } else {
                                    None
                                },
                                |bar, v| {
                                    bar.child(
                                        Button::new("restart-update-bay")
                                            .primary()
                                            .small()
                                            .label(if compact {
                                                t!("common.updates.restart").into_owned()
                                            } else {
                                                t!("common.updates.restart_version", version = v)
                                                    .into_owned()
                                            })
                                            .tooltip(
                                                t!("common.updates.restart_tooltip", version = v)
                                                    .into_owned(),
                                            )
                                            .on_click(|_, _, cx| {
                                                crate::updater::restart_app();
                                                cx.quit();
                                            }),
                                    )
                                },
                            )
                            .when(!compact, |row| {
                                row.child(
                                    div()
                                        .text_sm()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(
                                            t!(
                                                "common.labels.printer_count",
                                                count = self.printers.len()
                                            )
                                            .into_owned(),
                                        ),
                                )
                            })
                            .child(self.render_history_toggle(cx))
                            .child(Self::render_settings_button("settings", cx)),
                    )
                    .child(Self::render_window_controls()),
            )
            .into_any_element()
    }

    pub(super) fn render_detail_title_bar(
        _id: LocalPrinterId,
        _printer: &PrinterEntry,
        update_status: &crate::updater::UpdateStatus,
        compact: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let left_padding = if cfg!(target_os = "macos") {
            px(80.)
        } else {
            px(20.)
        };
        h_flex()
            .items_center()
            .justify_between()
            .w_full()
            .h(px(44.))
            .pl(left_padding)
            .pr_0()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .child(
                h_flex()
                    .items_center()
                    .gap_3()
                    .child(
                        Button::new("brand-logo-detail")
                            .ghost()
                            .h_8()
                            .px_0()
                            .accessibility_label(t!("common.actions.bay").into_owned())
                            .child(render_brand_logo(cx))
                            .on_click(cx.listener(|app, _, _, cx| app.close_detail(cx))),
                    )
                    .child(div().h(px(16.)).w(px(1.)).bg(cx.theme().border))
                    .child(
                        Button::new("back-to-bay")
                            .outline()
                            .small()
                            .label(if compact {
                                t!("common.actions.bay_short").into_owned()
                            } else {
                                t!("common.actions.bay").into_owned()
                            })
                            .on_click(cx.listener(|app, _, _, cx| app.close_detail(cx))),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .h_full()
                    .window_control_area(WindowControlArea::Drag),
            )
            .child(
                h_flex()
                    .items_center()
                    .pr_3()
                    .gap_3()
                    .when_some(
                        if let crate::updater::UpdateStatus::ReadyToRestart(v) = update_status {
                            Some(v.clone())
                        } else {
                            None
                        },
                        |bar, v| {
                            bar.child(
                                Button::new("restart-update-detail")
                                    .primary()
                                    .small()
                                    .label(if compact {
                                        t!("common.updates.restart").into_owned()
                                    } else {
                                        t!("common.updates.restart_version", version = v)
                                            .into_owned()
                                    })
                                    .tooltip(
                                        t!("common.updates.restart_tooltip", version = v)
                                            .into_owned(),
                                    )
                                    .on_click(|_, _, cx| {
                                        crate::updater::restart_app();
                                        cx.quit();
                                    }),
                            )
                        },
                    )
                    .child(Self::render_settings_button("settings-detail", cx)),
            )
            .child(Self::render_window_controls())
            .into_any_element()
    }
}
