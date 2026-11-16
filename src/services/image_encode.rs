//! Crops, shrinks and re-encodes the pictures the IPC methods return (viewer screenshots and thumbnails).
//!
//! The parameters, all optional:
//!   format     "png" (the default), "jpeg" or "webp"
//!   quality    1 to 100, 85 by default; ignored for png
//!   max_size   the longest side in pixels, from 1 to 8192; a picture already smaller is not enlarged
//!   crop       {"x", "y", "width", "height"}, each a fraction from 0 to 1 of the picture's full size, measured from
//!              its top left; the crop is made before the picture is shrunk

use gtk::gdk_pixbuf::{Colorspace, InterpType, Pixbuf};
use gtk::{gio, glib};
use serde_json::Value;

use crate::services::ipc::Error;

const DEFAULT_QUALITY: u8 = 85;
const MAX_DIMENSION: i64 = 8192;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Png,
    Jpeg,
    Webp,
}

impl Format {
    pub fn name(self) -> &'static str {
        match self {
            Format::Png => "png",
            Format::Jpeg => "jpeg",
            Format::Webp => "webp",
        }
    }

    pub fn mime_type(self) -> &'static str {
        match self {
            Format::Png => "image/png",
            Format::Jpeg => "image/jpeg",
            Format::Webp => "image/webp",
        }
    }
}

/// The part of a picture to keep, as fractions of its width and height.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Crop {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct Options {
    pub format: Format,
    pub quality: u8,
    pub max_size: Option<i32>,
    pub crop: Option<Crop>,
}

/// A finished picture.
pub struct Encoded {
    pub format: Format,
    pub width: i32,
    pub height: i32,
    pub bytes: Vec<u8>,
}

fn invalid(message: impl Into<String>) -> Error {
    Error::new("invalid_params", message)
}

impl Options {
    pub fn from_params(params: &Value) -> Result<Self, Error> {
        let format = match params.get("format").filter(|value| !value.is_null()) {
            None => Format::Png,
            Some(value) => match value.as_str() {
                Some("png") => Format::Png,
                Some("jpeg" | "jpg") => Format::Jpeg,
                Some("webp") => Format::Webp,
                _ => return Err(invalid("the parameter format must be png, jpeg or webp")),
            },
        };
        let quality = match params.get("quality").filter(|value| !value.is_null()) {
            None => DEFAULT_QUALITY,
            Some(value) => value
                .as_i64()
                .filter(|quality| (1..=100).contains(quality))
                .ok_or_else(|| invalid("the parameter quality must be a whole number from 1 to 100"))? as u8,
        };
        let max_size = match params.get("max_size").filter(|value| !value.is_null()) {
            None => None,
            Some(value) => Some(
                value
                    .as_i64()
                    .filter(|size| (1..=MAX_DIMENSION).contains(size))
                    .ok_or_else(|| invalid(format!("the parameter max_size must be a whole number from 1 to {MAX_DIMENSION}")))?
                    as i32,
            ),
        };
        let crop = match params.get("crop").filter(|value| !value.is_null()) {
            None => None,
            Some(value) => Some(Crop::from_value(value)?),
        };
        Ok(Self { format, quality, max_size, crop })
    }

    /// Whether `format` can be written here. WebP needs gdk-pixbuf's WebP loader, which not every system has.
    fn check_writable(&self) -> Result<(), Error> {
        let name = self.format.name();
        let writable = Pixbuf::formats()
            .iter()
            .any(|format| format.name().is_some_and(|candidate| candidate == name) && format.is_writable());
        if writable {
            Ok(())
        } else {
            Err(Error::new("unsupported_format", format!("this system cannot write {name} pictures")))
        }
    }
}

impl Crop {
    fn from_value(value: &Value) -> Result<Self, Error> {
        let field = |key: &str| {
            value
                .get(key)
                .and_then(Value::as_f64)
                .filter(|number| number.is_finite())
                .ok_or_else(|| invalid(format!("the crop needs a number for {key}")))
        };
        let (x, y, width, height) = (field("x")?, field("y")?, field("width")?, field("height")?);
        if !(0.0..1.0).contains(&x) || !(0.0..1.0).contains(&y) {
            return Err(invalid("the crop's x and y must be fractions from 0 up to 1"));
        }
        if width <= 0.0 || height <= 0.0 || x + width > 1.0 + 1e-9 || y + height > 1.0 + 1e-9 {
            return Err(invalid("the crop must have a positive width and height and stay inside the picture"));
        }
        Ok(Self { x, y, width, height })
    }

    /// The crop in pixels of a `width` by `height` picture, at least one pixel across and inside it.
    fn pixels(&self, width: i32, height: i32) -> (i32, i32, i32, i32) {
        let left = ((self.x * f64::from(width)).floor() as i32).clamp(0, width - 1);
        let top = ((self.y * f64::from(height)).floor() as i32).clamp(0, height - 1);
        let right = (((self.x + self.width) * f64::from(width)).ceil() as i32).clamp(left + 1, width);
        let bottom = (((self.y + self.height) * f64::from(height)).ceil() as i32).clamp(top + 1, height);
        (left, top, right - left, bottom - top)
    }
}

/// The picture in `png`, decoded.
pub fn decode(png: &[u8]) -> Result<Pixbuf, Error> {
    let stream = gio::MemoryInputStream::from_bytes(&glib::Bytes::from(png));
    Pixbuf::from_stream(&stream, gio::Cancellable::NONE)
        .map_err(|err| Error::new("encode_failed", format!("cannot read the picture: {err}")))
}

/// Crops, shrinks and encodes the picture in `png` as `options` ask. A png that needs no change is returned as it is.
pub fn transcode(png: &[u8], options: &Options) -> Result<Encoded, Error> {
    options.check_writable()?;
    let mut pixbuf = decode(png)?;
    let mut changed = false;

    if let Some(crop) = options.crop {
        let (x, y, width, height) = crop.pixels(pixbuf.width(), pixbuf.height());
        if (width, height) != (pixbuf.width(), pixbuf.height()) {
            pixbuf = pixbuf.new_subpixbuf(x, y, width, height);
            changed = true;
        }
    }
    if let Some(max_size) = options.max_size {
        let longest = pixbuf.width().max(pixbuf.height());
        if longest > max_size {
            let scale = f64::from(max_size) / f64::from(longest);
            let width = ((f64::from(pixbuf.width()) * scale).round() as i32).max(1);
            let height = ((f64::from(pixbuf.height()) * scale).round() as i32).max(1);
            pixbuf = pixbuf
                .scale_simple(width, height, InterpType::Hyper)
                .ok_or_else(|| Error::new("encode_failed", "cannot shrink the picture"))?;
            changed = true;
        }
    }

    let (width, height) = (pixbuf.width(), pixbuf.height());
    if options.format == Format::Png && !changed {
        return Ok(Encoded { format: Format::Png, width, height, bytes: png.to_vec() });
    }
    let bytes = encode(&pixbuf, options)?;
    Ok(Encoded { format: options.format, width, height, bytes })
}

fn encode(pixbuf: &Pixbuf, options: &Options) -> Result<Vec<u8>, Error> {
    let quality = options.quality.to_string();
    let failed = |err: glib::Error| Error::new("encode_failed", format!("cannot write the {} picture: {err}", options.format.name()));
    match options.format {
        Format::Png => pixbuf.save_to_bufferv("png", &[]).map_err(failed),
        Format::Webp => pixbuf.save_to_bufferv("webp", &[("quality", quality.as_str())]).map_err(failed),
        Format::Jpeg => flatten(pixbuf)?.save_to_bufferv("jpeg", &[("quality", quality.as_str())]).map_err(failed),
    }
}

/// The picture on a white background, because JPEG has no transparency.
fn flatten(pixbuf: &Pixbuf) -> Result<Pixbuf, Error> {
    if !pixbuf.has_alpha() {
        return Ok(pixbuf.clone());
    }
    let (width, height) = (pixbuf.width(), pixbuf.height());
    let flat = Pixbuf::new(Colorspace::Rgb, false, 8, width, height)
        .ok_or_else(|| Error::new("encode_failed", "cannot make room for the picture"))?;
    flat.fill(0xffff_ffff);
    pixbuf.composite(&flat, 0, 0, width, height, 0.0, 0.0, 1.0, 1.0, InterpType::Nearest, 255);
    Ok(flat)
}
