use crate::i18n::t;
use std::cell::RefCell;
use std::sync::Arc;

use super::components::button::Button;
use gpui_kit::base::{StyledExt as _, TestSupportExt as _};
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Image, ImageFormat, InteractiveElement as _, IntoElement, ObjectFit,
    ParentElement as _, StatefulInteractiveElement as _, Styled as _, StyledImage as _, div, img,
    px, rems, rgb,
};

use crate::shared::workspace::{CameraRequest, ReadStatus, ResourceId};
use crate::state::LanConnection;

use super::icons::{SVG_BRAND_WORDMARK, SVG_CAMERA};
use super::{PrinterEntry, detail::camera_state_label};

#[derive(Default)]
pub(super) struct ImageSlot {
    cached: RefCell<Option<(bytes::Bytes, Arc<Image>)>>,
}

impl ImageSlot {
    pub(super) fn get(&self, bytes: Option<&bytes::Bytes>) -> Option<Arc<Image>> {
        let mut cached = self.cached.borrow_mut();
        let Some(bytes) = bytes else {
            *cached = None;
            return None;
        };
        if let Some((previous, image)) = cached.as_ref()
            && previous.as_ptr() == bytes.as_ptr()
            && previous.len() == bytes.len()
        {
            return Some(image.clone());
        }
        let Some(format) = image_format(bytes) else {
            *cached = None;
            return None;
        };
        let image = Arc::new(Image::from_bytes(format, bytes.to_vec()));
        // Keep the source alive: its allocation cannot be reused for a different image.
        *cached = Some((bytes.clone(), image.clone()));
        Some(image)
    }
}

pub(super) fn placeholder_image() -> Arc<Image> {
    static PLACEHOLDER: std::sync::OnceLock<Arc<Image>> = std::sync::OnceLock::new();
    PLACEHOLDER
        .get_or_init(|| {
            Arc::new(Image::from_bytes(
                ImageFormat::Png,
                include_bytes!("../../assets/images/printer-placeholder.png").to_vec(),
            ))
        })
        .clone()
}

pub(super) fn image_format(bytes: &[u8]) -> Option<ImageFormat> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(ImageFormat::Png)
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some(ImageFormat::Jpeg)
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some(ImageFormat::Gif)
    } else if bytes.starts_with(b"BM") {
        Some(ImageFormat::Bmp)
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some(ImageFormat::Webp)
    } else {
        None
    }
}

pub(super) fn camera_capture_label(printer: &PrinterEntry) -> String {
    let camera = &printer.camera_state;
    let connected = printer.state.connection == LanConnection::Connected;
    if camera
        .frame
        .as_ref()
        .is_some_and(|bytes| image_format(bytes).is_some())
    {
        if connected
            && printer.state.telemetry_ready
            && camera.status == ReadStatus::Ready
            && camera.received_at.is_some()
        {
            t!("printers.camera.current_badge").into_owned()
        } else {
            t!("printers.camera.retained_badge").into_owned()
        }
    } else if !connected {
        t!("printers.status.disconnected").into_owned()
    } else {
        match camera.status {
            ReadStatus::Loading => t!("printers.camera.loading_badge").into_owned(),
            ReadStatus::Denied => t!("printers.camera.denied_badge").into_owned(),
            ReadStatus::Error => t!("printers.camera.error_badge").into_owned(),
            ReadStatus::Limited => t!("printers.camera.limited_badge").into_owned(),
            ReadStatus::Unsupported => t!("printers.camera.unsupported_badge").into_owned(),
            _ => t!("printers.camera.none_badge").into_owned(),
        }
    }
}

/// Refresh the reported camera; without one, only show the frame.
pub(super) fn camera_cell(
    printer: &PrinterEntry,
    camera: Option<ResourceId>,
    cx: &App,
) -> AnyElement {
    let has_frame = printer
        .camera_image
        .get(printer.camera_state.frame.as_ref());
    let is_connected = printer.state.connection == LanConnection::Connected;
    let empty_message = if !is_connected {
        t!("printers.camera.connect_required").into_owned()
    } else if printer.camera_state.status == ReadStatus::NotLoaded {
        t!("printers.camera.not_requested").into_owned()
    } else if printer.camera_state.status == ReadStatus::Ready
        && printer.camera_state.frame.is_none()
    {
        t!("printers.camera.no_capture").into_owned()
    } else {
        camera_state_label(&printer.camera_state)
    };
    let camera_sender = printer.camera.requests.clone();

    let capture_label = camera_capture_label(printer);
    let is_current = is_connected
        && printer.state.telemetry_ready
        && printer.camera_state.status == ReadStatus::Ready
        && printer.camera_state.received_at.is_some();

    let color = if has_frame.is_some() && is_current {
        cx.theme().primary
    } else {
        cx.theme().muted_foreground
    };
    let status_badge = super::components::badge::badge()
        .bg(if has_frame.is_some() && is_current {
            cx.theme().primary.opacity(0.12)
        } else {
            cx.theme().secondary
        })
        .border_color(if has_frame.is_some() && is_current {
            cx.theme().primary.opacity(0.3)
        } else {
            cx.theme().border
        })
        .text_color(color)
        .when(has_frame.is_some(), |badge| {
            badge.child(div().size(px(6.)).flex_none().rounded_full().bg(color))
        })
        .child(capture_label);

    let header = h_flex()
        .justify_between()
        .items_center()
        .gap_2()
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(t!("printers.camera.heading")),
        )
        .child(status_badge);

    let viewport = div()
        .flex_1()
        .w_full()
        .rounded(cx.theme().radius)
        .border_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().secondary)
        .overflow_hidden()
        .flex()
        .items_center()
        .justify_center();

    let viewport = match has_frame {
        Some(image) => viewport.child(img(image).size_full().object_fit(ObjectFit::Contain)),
        None => viewport.child(
            v_flex()
                .items_center()
                .gap_1p5()
                .child(
                    Icon::default()
                        .data(SVG_CAMERA)
                        .size_6()
                        .text_color(cx.theme().muted_foreground),
                )
                .child(
                    div()
                        .text_xs()
                        .font_semibold()
                        .text_color(cx.theme().muted_foreground)
                        .id("camera-empty-message")
                        .aria_label(empty_message.clone())
                        .test_support()
                        .child(empty_message),
                ),
        ),
    };

    let footer = h_flex()
        .justify_between()
        .items_center()
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(printer.camera_state.received_at.map_or_else(
                    || t!("printers.camera.local").into_owned(),
                    |received| {
                        t!(
                            "printers.camera.last_capture",
                            seconds = received.elapsed().as_secs()
                        )
                        .into_owned()
                    },
                )),
        )
        .child(
            Button::new("cam-refresh")
                .small()
                .outline()
                .label(t!("common.actions.refresh").into_owned())
                .disabled(!is_connected || camera.is_none())
                .on_click(move |_, _, _| {
                    if let Some(id) = camera {
                        let _ = camera_sender.try_send(CameraRequest::Load { id });
                    }
                }),
        );

    // Same size with or without a frame: the first capture must not reflow the surface.
    v_flex()
        .id("camera-preview-card")
        .test_support()
        .size_full()
        .min_h(rems(14.25))
        .gap_3()
        .child(header)
        .child(viewport)
        .child(footer)
        .into_any_element()
}

pub(super) fn render_brand_logo(_cx: &App) -> AnyElement {
    h_flex()
        .id("brand-logo")
        .test_support()
        .flex_none()
        .items_center()
        .child(
            Icon::default()
                .data(SVG_BRAND_WORDMARK)
                .h(px(20.))
                .w(px(149.))
                .text_color(rgb(0xffffff)),
        )
        .into_any_element()
}
