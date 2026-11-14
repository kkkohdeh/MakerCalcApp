//! Small, bounded visual accent extraction, independent of printer protocols and GPUI.
use std::io::Cursor;

pub(crate) const FALLBACK: u32 = 0x2271ef;

#[derive(Clone, Copy, Default)]
struct Family {
    count: u32,
    sum: [u32; 3],
}

/// Called once on receipt, on the existing bounded thumbnail CPU executor.
pub(crate) fn from_png(bytes: &[u8], background: [u8; 3]) -> u32 {
    if bytes.is_empty() || bytes.len() > 1024 * 1024 {
        return FALLBACK;
    }
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), image::ImageFormat::Png);
    reader.limits(super::thumbnail_decode_limits());
    let Ok(image) = reader.decode() else {
        return FALLBACK;
    };
    dominant(&image.into_rgba8())
        .and_then(|color| readable_color(color, background))
        .unwrap_or(FALLBACK)
}

fn readable_color(color: u32, background: [u8; 3]) -> Option<u32> {
    if contrast(color, background) >= 4.0 {
        return Some(color);
    }
    let channels = [(color >> 16) as u8, (color >> 8) as u8, color as u8];
    let [r, g, b] = channels;
    if family(r, g, b) >= 24 {
        return None;
    }
    let peak = u32::from(r.max(g).max(b));
    // Raise brightness without replacing the detected hue.
    for value in peak + 1..=255 {
        let [r, g, b] = channels.map(|channel| u32::from(channel) * value / peak);
        let candidate = (r << 16) | (g << 8) | b;
        if contrast(candidate, background) >= 4.0 {
            return Some(candidate);
        }
    }
    None
}

fn dominant(image: &image::RgbaImage) -> Option<u32> {
    let (width, height) = image.dimensions();
    let (nx, ny) = (width.min(32), height.min(32));
    if nx == 0 || ny == 0 {
        return None;
    }
    // 24 hue families collect differently lit faces together; 3 neutral families retain grays.
    let mut families = [Family::default(); 27];
    for sy in 0..ny {
        let y = ((2 * sy + 1) * height) / (2 * ny);
        for sx in 0..nx {
            let x = ((2 * sx + 1) * width) / (2 * nx);
            let [r, g, b, a] = image.get_pixel(x, y).0;
            if a < 128 {
                continue;
            }
            let bucket = &mut families[family(r, g, b)];
            bucket.count += 1;
            for (sum, value) in bucket.sum.iter_mut().zip([r, g, b]) {
                *sum += u32::from(value);
            }
        }
    }
    let best = families
        .iter()
        .filter(|group| group.count > 0)
        .max_by_key(|group| group.count)?;
    Some(
        (best.sum[0] / best.count) << 16
            | (best.sum[1] / best.count) << 8
            | (best.sum[2] / best.count),
    )
}

fn family(r: u8, g: u8, b: u8) -> usize {
    let high = r.max(g).max(b);
    let low = r.min(g).min(b);
    let delta = high - low;
    if u16::from(delta) * 5 <= u16::from(high) {
        return 24 + usize::from(high) * 3 / 256;
    }
    let delta = f32::from(delta);
    let hue = if high == r {
        (f32::from(g) - f32::from(b)) / delta
    } else if high == g {
        (f32::from(b) - f32::from(r)) / delta + 2.
    } else {
        (f32::from(r) - f32::from(g)) / delta + 4.
    };
    ((hue.rem_euclid(6.) * 4. + 0.5).floor() as usize) % 24
}

fn luminance([r, g, b]: [u8; 3]) -> f32 {
    let linear = |value: u8| {
        let value = f32::from(value) / 255.;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
}

fn contrast(color: u32, background: [u8; 3]) -> f32 {
    let a = luminance([(color >> 16) as u8, (color >> 8) as u8, color as u8]);
    let b = luminance(background);
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}
