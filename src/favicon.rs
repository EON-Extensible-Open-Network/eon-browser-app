// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 EON contributors

//! Site icons for the tab strip: Servo hands over the page's favicon as raw pixels; the
//! interface gets a small PNG (32 px at most, sharp at 16 CSS px on a 2× screen).

use image::{ImageBuffer, ImageFormat, Rgba, RgbaImage};
use servo::{Image, PixelFormat};

/// The largest side kept, in pixels.
const SIDE: u32 = 32;

/// Convert Servo's favicon to a PNG, or `None` if its pixels cannot be read.
pub fn to_png(img: &Image) -> Option<Vec<u8>> {
    let rgba = to_rgba(img.width, img.height, img.format, img.data())?;
    encode(&rgba)
}

fn to_rgba(width: u32, height: u32, format: PixelFormat, data: &[u8]) -> Option<RgbaImage> {
    let pixels = (width as usize).checked_mul(height as usize)?;
    let mut out = Vec::with_capacity(pixels.checked_mul(4)?);
    let per = match format {
        PixelFormat::K8 => 1,
        PixelFormat::KA8 => 2,
        PixelFormat::RGB8 => 3,
        PixelFormat::RGBA8 | PixelFormat::BGRA8 => 4,
    };
    let data = data.get(..pixels.checked_mul(per)?)?;
    for p in data.chunks_exact(per) {
        let px: [u8; 4] = match format {
            PixelFormat::K8 => [p[0], p[0], p[0], 255],
            PixelFormat::KA8 => [p[0], p[0], p[0], p[1]],
            PixelFormat::RGB8 => [p[0], p[1], p[2], 255],
            PixelFormat::RGBA8 => [p[0], p[1], p[2], p[3]],
            PixelFormat::BGRA8 => [p[2], p[1], p[0], p[3]],
        };
        out.extend_from_slice(&px);
    }
    ImageBuffer::<Rgba<u8>, _>::from_raw(width, height, out)
}

fn encode(rgba: &RgbaImage) -> Option<Vec<u8>> {
    if rgba.width() == 0 || rgba.height() == 0 {
        return None;
    }
    let small = if rgba.width() > SIDE || rgba.height() > SIDE {
        image::imageops::thumbnail(rgba, SIDE, SIDE)
    } else {
        rgba.clone()
    };
    let mut png = std::io::Cursor::new(Vec::new());
    small.write_to(&mut png, ImageFormat::Png).ok()?;
    Some(png.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bgra_becomes_a_small_png() {
        // 64×64 pure blue in BGRA: the PNG is 32×32 and still blue.
        let data: Vec<u8> = std::iter::repeat_n([255u8, 0, 0, 255], 64 * 64)
            .flatten()
            .collect();
        let rgba = to_rgba(64, 64, PixelFormat::BGRA8, &data).unwrap_or_default();
        let png = encode(&rgba).unwrap_or_default();
        let back = image::load_from_memory(&png).map(|i| i.to_rgba8()).ok();
        let back = back.unwrap_or_default();
        assert_eq!((back.width(), back.height()), (32, 32));
        assert_eq!(back.get_pixel(5, 5).0, [0, 0, 255, 255]);
    }

    #[test]
    fn short_data_is_refused() {
        assert!(to_rgba(16, 16, PixelFormat::RGBA8, &[0; 10]).is_none());
    }
}
