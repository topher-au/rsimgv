//! Resampling for the scaling qualities GTK has no filter for: bicubic, and bilinear or bicubic followed by
//! sharpening. OpenCV does the work, on pictures held as premultiplied RGBA, 8 bits to a channel.

use opencv::core::{self, Mat, Rect, Size, Vector};
use opencv::imgproc;
use opencv::prelude::*;

/// How the picture is resampled: the filter, and whether the result is sharpened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quality {
    pub cubic: bool,
    pub sharpen: bool,
}

/// How much of the blur an unsharp mask takes away, and how wide that blur is, in pixels.
const SHARPEN_AMOUNT: f64 = 0.6;
const SHARPEN_SIGMA: f64 = 1.0;

/// A picture: premultiplied RGBA, with no padding between rows.
pub struct Picture {
    pub width: i32,
    pub height: i32,
    pub pixels: Vec<u8>,
}

/// The part of `picture` inside `area`, resized to `width` by `height` pixels and sharpened if `quality` says so.
/// Shrinking uses area averaging whatever the filter, as it is the one that does not alias.
pub fn resample(picture: &Picture, area: Rect, width: i32, height: i32, quality: Quality) -> opencv::Result<Vec<u8>> {
    if area.x < 0 || area.y < 0 || area.width < 1 || area.height < 1 || area.x + area.width > picture.width || area.y + area.height > picture.height {
        return Err(opencv::Error::new(core::StsOutOfRange, "the area is outside the picture"));
    }
    let flat = Mat::from_slice(&picture.pixels)?;
    let whole = flat.reshape(4, picture.height)?;
    let part = Mat::roi(&whole, area)?;
    let shrinking = width < area.width || height < area.height;
    let method = match (shrinking, quality.cubic) {
        (true, _) => imgproc::INTER_AREA,
        (false, true) => imgproc::INTER_CUBIC,
        (false, false) => imgproc::INTER_LINEAR,
    };
    let mut resized = Mat::default();
    imgproc::resize(&part, &mut resized, Size::new(width, height), 0.0, 0.0, method)?;
    if quality.sharpen {
        resized = sharpened(&resized)?;
    }
    Ok(resized.data_bytes()?.to_vec())
}

/// `picture` with an unsharp mask applied. The colour channels are held to the alpha channel afterwards, as
/// premultiplied pixels are never brighter than their alpha.
fn sharpened(picture: &Mat) -> opencv::Result<Mat> {
    let mut blurred = Mat::default();
    imgproc::gaussian_blur(
        picture,
        &mut blurred,
        Size::new(0, 0),
        SHARPEN_SIGMA,
        SHARPEN_SIGMA,
        core::BORDER_DEFAULT,
        core::AlgorithmHint::ALGO_HINT_DEFAULT,
    )?;
    let mut sharp = Mat::default();
    core::add_weighted(picture, 1.0 + SHARPEN_AMOUNT, &blurred, -SHARPEN_AMOUNT, 0.0, &mut sharp, -1)?;

    let mut channels = Vector::<Mat>::new();
    core::split(&sharp, &mut channels)?;
    let alpha = channels.get(3)?;
    let mut held = Vector::<Mat>::new();
    for index in 0..3 {
        let mut channel = Mat::default();
        core::min(&channels.get(index)?, &alpha, &mut channel)?;
        held.push(channel);
    }
    held.push(alpha);
    let mut merged = Mat::default();
    core::merge(&held, &mut merged)?;
    Ok(merged)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A picture `width` wide and `height` tall that is dark on its left half and light on its right, opaque.
    fn step(width: i32, height: i32) -> Picture {
        let mut pixels = Vec::new();
        for _ in 0..height {
            for x in 0..width {
                let level = if x < width / 2 { 60 } else { 190 };
                pixels.extend_from_slice(&[level, level, level, 255]);
            }
        }
        Picture { width, height, pixels }
    }

    fn row(pixels: &[u8], width: i32) -> Vec<u8> {
        pixels[..width as usize * 4].chunks(4).map(|pixel| pixel[0]).collect()
    }

    fn whole(picture: &Picture) -> Rect {
        Rect::new(0, 0, picture.width, picture.height)
    }

    #[test]
    fn the_result_has_the_size_asked_for() {
        let picture = step(16, 8);
        for (cubic, sharpen) in [(false, false), (false, true), (true, false), (true, true)] {
            let out = resample(&picture, whole(&picture), 48, 24, Quality { cubic, sharpen }).unwrap();
            assert_eq!(out.len(), 48 * 24 * 4, "cubic {cubic}, sharpen {sharpen}");
        }
        let shrunk = resample(&picture, whole(&picture), 4, 2, Quality { cubic: true, sharpen: false }).unwrap();
        assert_eq!(shrunk.len(), 4 * 2 * 4, "shrinking too");
    }

    #[test]
    fn an_area_outside_the_picture_is_an_error() {
        let picture = step(16, 8);
        let quality = Quality { cubic: true, sharpen: false };
        assert!(resample(&picture, Rect::new(10, 0, 8, 8), 8, 8, quality).is_err());
        assert!(resample(&picture, Rect::new(0, 0, 0, 8), 8, 8, quality).is_err());
    }

    #[test]
    fn only_the_area_asked_for_is_resampled() {
        let picture = step(16, 8);
        let right = Rect::new(8, 0, 8, 8);
        let out = resample(&picture, right, 16, 16, Quality { cubic: true, sharpen: false }).unwrap();
        assert!(row(&out, 16).iter().all(|&level| level == 190), "the light half alone");
    }

    #[test]
    fn sharpening_deepens_an_edge() {
        let picture = step(16, 8);
        let plain = resample(&picture, whole(&picture), 64, 32, Quality { cubic: true, sharpen: false }).unwrap();
        let sharp = resample(&picture, whole(&picture), 64, 32, Quality { cubic: true, sharpen: true }).unwrap();
        let (plain, sharp) = (row(&plain, 64), row(&sharp, 64));
        assert!(sharp.iter().min() < plain.iter().min(), "the dark side overshoots darker");
        assert!(sharp.iter().max() > plain.iter().max(), "the light side overshoots lighter");
    }

    #[test]
    fn colour_never_exceeds_alpha() {
        // Half-transparent white beside a clear pixel: sharpening would push the colour past the alpha.
        let mut pixels = Vec::new();
        for x in 0..16 {
            pixels.extend_from_slice(if x < 8 { &[128, 128, 128, 128] } else { &[0, 0, 0, 0] });
        }
        let picture = Picture { width: 16, height: 1, pixels };
        let out = resample(&picture, whole(&picture), 64, 4, Quality { cubic: true, sharpen: true }).unwrap();
        assert!(out.chunks(4).all(|pixel| pixel[0] <= pixel[3] && pixel[1] <= pixel[3] && pixel[2] <= pixel[3]));
    }
}
