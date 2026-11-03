//! How a video is framed: the picture cropped out of the bars or backdrop round it by an mpv filter, and the space round the
//! picture filled with a blurred, dimmed copy of it, which the stream draws itself.
//!
//! The crop is an mpv `crop` filter. Where it should crop is found by mpv's own `cropdetect` filter, which reports the
//! area that is not near black, or by looking at a frame decoded from the file: at the steps in brightness that mark the
//! picture's border (`Edges`), which also strips a blurred backdrop, or at the area that changes between two frames
//! (`Static`), which strips a backdrop that does not move.

/// The area of a frame to keep, in pixels, with even offsets and sizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CropRect {
    pub width: i32,
    pub height: i32,
    pub left: i32,
    pub top: i32,
}

/// How the picture to crop to is found.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CropMethod {
    /// mpv's `cropdetect` filter, which finds near-black borders.
    #[default]
    CropDetect,
    /// Steps in brightness at the picture's border, which also strips a blurred backdrop.
    Edges,
    /// The area that changes between frames, which strips a backdrop that does not move.
    Static,
}

impl CropMethod {
    pub const ALL: [CropMethod; 3] = [CropMethod::CropDetect, CropMethod::Edges, CropMethod::Static];

    pub fn name(self) -> &'static str {
        match self {
            CropMethod::CropDetect => "cropdetect",
            CropMethod::Edges => "edges",
            CropMethod::Static => "static",
        }
    }

    /// The method a setting value names, or `CropDetect` for any other.
    pub fn from_name(name: &str) -> Self {
        Self::ALL.into_iter().find(|method| method.name() == name).unwrap_or_default()
    }
}

/// The backdrop drawn behind the picture: how far the blurred copy is zoomed in (1 for just covering the space), how blurred
/// it is, and how bright it is, from 0 for black to 1 for unchanged.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Backdrop {
    pub zoom: f64,
    pub blur: i32,
    pub dim: f64,
}

/// What the player does to frame a video.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Framing {
    /// How the picture is found and cropped to, or no crop.
    pub crop: Option<CropMethod>,
    /// Pixels trimmed from each side that was cut out, so a soft edge is not left in.
    pub trim_pixels: i32,
    pub backdrop: Option<Backdrop>,
}

/// The label of the filter that finds the picture, and the one that crops to it.
pub const DETECTOR_LABEL: &str = "rsimgv-cropdetect";
pub const CROP_LABEL: &str = "rsimgv-crop";

/// The brightness step, in levels of mean luma, that marks a picture's border, and the mean change per column or row that
/// counts as motion.
const EDGE_THRESHOLD: f64 = 12.0;
const MOTION_THRESHOLD: f64 = 3.0;
/// The most soft, blurred lines trimmed from each side of the picture the steps found. A line is soft when it is darker
/// than this share of the picture's brightness.
const SOFT_PIXELS: usize = 4;
const SOFT_RATIO: f64 = 0.6;
/// How much a crop must change in size, as a share, to be taken in place of the one in use.
const CHANGE_RATIO: f64 = 0.10;
/// How far apart, in pixels, two measurements may be and still agree.
const AGREE_TOLERANCE: i32 = 4;

/// mpv's `cropdetect` filter as an `vf` argument, labelled so it can be found and replaced.
pub fn detector_filter() -> String {
    format!("@{DETECTOR_LABEL}:cropdetect=limit=24/255:round=2:reset=200")
}

/// The `crop` filter for `rect` as an `vf` argument.
pub fn crop_filter(rect: CropRect) -> String {
    format!("@{CROP_LABEL}:crop=w={}:h={}:x={}:y={}", rect.width, rect.height, rect.left, rect.top)
}

/// The rectangle, as (x, y, width, height), of a `width` by `height` picture shown as big as fits an `area_width` by `area_height`
/// area without cropping it, centred.
pub fn fitted(area_width: f64, area_height: f64, width: f64, height: f64) -> (f64, f64, f64, f64) {
    if width <= 0.0 || height <= 0.0 {
        return (0.0, 0.0, area_width, area_height);
    }
    let scale = (area_width / width).min(area_height / height);
    let (fitted_width, fitted_height) = (width * scale, height * scale);
    ((area_width - fitted_width) / 2.0, (area_height - fitted_height) / 2.0, fitted_width, fitted_height)
}

/// The rectangle, as (x, y, width, height), of a `width` by `height` picture scaled to cover an `area_width` by `area_height`
/// area and `zoom` times further in, centred. The parts outside the area are cropped by drawing it clipped to the area.
pub fn covering(area_width: f64, area_height: f64, width: f64, height: f64, zoom: f64) -> (f64, f64, f64, f64) {
    let scale = (area_width / width).max(area_height / height) * zoom.max(1.0);
    let (covered_width, covered_height) = (width * scale, height * scale);
    ((area_width - covered_width) / 2.0, (area_height - covered_height) / 2.0, covered_width, covered_height)
}

/// The strips of an `area_width` by `area_height` area that a picture at `fitted` (as `fitted` returns it) leaves bare, as
/// (x, y, width, height): at the sides, or above and below.
pub fn bars(area_width: f64, area_height: f64, fitted: (f64, f64, f64, f64)) -> Vec<(f64, f64, f64, f64)> {
    let (x, y, width, height) = fitted;
    if x > 0.5 {
        vec![(0.0, 0.0, x, area_height), (x + width, 0.0, area_width - x - width, area_height)]
    } else if y > 0.5 {
        vec![(0.0, 0.0, area_width, y), (0.0, y + height, area_width, area_height - y - height)]
    } else {
        Vec::new()
    }
}

/// The part `region` (x0, y0, x1, y1, in pixels) of a `width` by `height` picture of 4-byte pixels, averaged down to
/// `out_width` by `out_height`. A few samples are taken for each pixel out, so the cost follows the size out.
pub fn shrink_region(pixels: &[u8], width: usize, height: usize, region: (f64, f64, f64, f64), out_width: usize, out_height: usize) -> Vec<u8> {
    const SAMPLES: usize = 4;
    let mut out = vec![0u8; out_width * out_height * 4];
    if width == 0 || height == 0 || pixels.len() < width * height * 4 {
        return out;
    }
    let (x0, y0, x1, y1) = region;
    let (step_x, step_y) = ((x1 - x0) / (out_width * SAMPLES) as f64, (y1 - y0) / (out_height * SAMPLES) as f64);
    for oy in 0..out_height {
        for ox in 0..out_width {
            let mut sum = [0u32; 4];
            for sy in 0..SAMPLES {
                let y = ((y0 + ((oy * SAMPLES + sy) as f64 + 0.5) * step_y) as usize).min(height - 1);
                for sx in 0..SAMPLES {
                    let x = ((x0 + ((ox * SAMPLES + sx) as f64 + 0.5) * step_x) as usize).min(width - 1);
                    let at = (y * width + x) * 4;
                    for (total, byte) in sum.iter_mut().zip(&pixels[at..at + 4]) {
                        *total += u32::from(*byte);
                    }
                }
            }
            let at = (oy * out_width + ox) * 4;
            for (byte, total) in out[at..at + 4].iter_mut().zip(sum) {
                *byte = (total / (SAMPLES * SAMPLES) as u32) as u8;
            }
        }
    }
    out
}

/// Blurs 4-byte pixels `width` by `height` with two passes of a box `radius` pixels either side (clamped at the edges), and
/// scales them to `dim` of their brightness. The fourth byte of each pixel is not used. `scratch` holds the pass between the
/// two directions, and is kept by the caller so it is not made for each picture.
pub fn blur_and_dim(pixels: &mut [u8], scratch: &mut Vec<u8>, width: usize, height: usize, radius: usize, dim: f64) {
    if width == 0 || height == 0 || pixels.len() < width * height * 4 {
        return;
    }
    scratch.resize(pixels.len(), 0);
    for _ in 0..2 {
        box_pass(pixels, scratch, width, height, radius, true);
        box_pass(scratch, pixels, width, height, radius, false);
    }
    let dim = dim.clamp(0.0, 1.0);
    let levels: [u8; 256] = std::array::from_fn(|level| (level as f64 * dim) as u8);
    for pixel in pixels.chunks_exact_mut(4) {
        for byte in &mut pixel[..3] {
            *byte = levels[usize::from(*byte)];
        }
    }
}

/// One box blur pass along rows (`horizontal`) or columns, of the colour bytes of each pixel, with a window that slides along
/// each line: the sum gains the pixel entering it and loses the one leaving, so the cost does not follow the radius. Pixels
/// past either end of a line count as its end pixel.
fn box_pass(from: &[u8], to: &mut [u8], width: usize, height: usize, radius: usize, horizontal: bool) {
    let (length, lines, along, across) = if horizontal { (width, height, 1, width) } else { (height, width, width, 1) };
    let window = (2 * radius + 1) as u32;
    let last = length as isize - 1;
    let radius = radius as isize;
    for line in 0..lines {
        let at = |i: isize| (line * across + i.clamp(0, last) as usize * along) * 4;
        let mut total = [0u32; 3];
        for k in -radius..=radius {
            let from_at = at(k);
            for (sum, byte) in total.iter_mut().zip(&from[from_at..from_at + 3]) {
                *sum += u32::from(*byte);
            }
        }
        for i in 0..length as isize {
            let to_at = at(i);
            for (byte, sum) in to[to_at..to_at + 3].iter_mut().zip(total) {
                *byte = (sum / window) as u8;
            }
            let (leaving, entering) = (at(i - radius), at(i + radius + 1));
            for (channel, sum) in total.iter_mut().enumerate() {
                *sum = *sum + u32::from(from[entering + channel]) - u32::from(from[leaving + channel]);
            }
        }
    }
}

// -- finding the picture -----------------------------------------------------

fn column_means(gray: &[u8], width: usize, height: usize) -> Vec<f64> {
    (0..width).map(|x| (0..height).map(|y| f64::from(gray[y * width + x])).sum::<f64>() / height as f64).collect()
}

fn row_means(gray: &[u8], width: usize, height: usize) -> Vec<f64> {
    (0..height).map(|y| gray[y * width..(y + 1) * width].iter().map(|value| f64::from(*value)).sum::<f64>() / width as f64).collect()
}

/// The picture's extent along one axis, as `(start, end)` with the end exclusive: from the first step above `threshold`
/// to the last, so steps inside the picture are never reached.
fn picture_span(profile: &[f64], threshold: f64) -> (usize, usize) {
    let strong: Vec<usize> = profile.windows(2).enumerate().filter(|(_, pair)| (pair[1] - pair[0]).abs() > threshold).map(|(index, _)| index).collect();
    let (Some(first), Some(last)) = (strong.first(), strong.last()) else { return (0, profile.len()) };
    let (start, end) = (first + 1, last + 1);
    if start >= end {
        return (0, profile.len());
    }
    (start, end)
}

/// How many lines from `start`, moving by `step`, are soft, at most `limit`. `line(n)` gives the pixels of line `n`. The
/// picture's brightness is the median of lines well inside it, past any soft band the cap could cover.
fn soft_lines(line: &dyn Fn(i64) -> Vec<u8>, start: i64, step: i64, limit: usize) -> usize {
    let mut values: Vec<u8> = (4 * limit..6 * limit).flat_map(|k| line(start + step * k as i64).into_iter().step_by(4)).collect();
    if values.is_empty() {
        return 0;
    }
    values.sort_unstable();
    let reference = f64::from(values[values.len() / 2]) * SOFT_RATIO;
    let mut count = 0;
    while count < limit {
        let pixels = line(start + step * count as i64);
        if pixels.is_empty() {
            break;
        }
        let darker = pixels.iter().filter(|value| f64::from(**value) < reference).count();
        if darker * 2 <= pixels.len() {
            break;
        }
        count += 1;
    }
    count
}

/// The picture rectangle of one greyscale frame `width` by `height`, with even offsets and sizes. Up to `soft_max` soft lines
/// are trimmed from each side of what the steps found.
pub fn find_picture(gray: &[u8], width: usize, height: usize, threshold: f64, soft_max: usize) -> CropRect {
    let (mut left, mut right) = picture_span(&column_means(gray, width, height), threshold);
    let (mut top, mut bottom) = picture_span(&row_means(gray, width, height), threshold);
    if soft_max > 0 && right - left > 6 * soft_max && bottom - top > 6 * soft_max {
        let (l, r, t, b) = (left, right, top, bottom);
        let column = |x: i64| -> Vec<u8> {
            if x < 0 || x as usize >= width {
                return Vec::new();
            }
            (t..b).map(|y| gray[y * width + x as usize]).collect()
        };
        let row = |y: i64| -> Vec<u8> {
            if y < 0 || y as usize >= height {
                return Vec::new();
            }
            gray[y as usize * width + l..y as usize * width + r].to_vec()
        };
        let left_trim = soft_lines(&column, l as i64, 1, soft_max);
        let right_trim = soft_lines(&column, r as i64 - 1, -1, soft_max);
        let top_trim = soft_lines(&row, t as i64, 1, soft_max);
        let bottom_trim = soft_lines(&row, b as i64 - 1, -1, soft_max);
        left += left_trim;
        right -= right_trim;
        top += top_trim;
        bottom -= bottom_trim;
    }
    left += left % 2;
    top += top % 2;
    CropRect { width: ((right - left) & !1) as i32, height: ((bottom - top) & !1) as i32, left: left as i32, top: top as i32 }
}

/// The extent of the entries of `profile` above `threshold`, end exclusive, or `None` if there are none.
fn moving_span(profile: &[f64], threshold: f64) -> Option<(usize, usize)> {
    let first = profile.iter().position(|value| *value > threshold)?;
    let last = profile.iter().rposition(|value| *value > threshold)?;
    Some((first, last + 1))
}

/// The area that changes between two frames: the picture, when the backdrop is static. `None` when nothing changes, such as
/// a paused frame.
pub fn find_moving_picture(first: &[u8], second: &[u8], width: usize, height: usize, threshold: f64) -> Option<CropRect> {
    let changed: Vec<u8> = first.iter().zip(second).map(|(a, b)| a.abs_diff(*b)).collect();
    let (x_start, x_end) = moving_span(&column_means(&changed, width, height), threshold)?;
    let (y_start, y_end) = moving_span(&row_means(&changed, width, height), threshold)?;
    let left = x_start + x_start % 2;
    let top = y_start + y_start % 2;
    Some(CropRect { width: ((x_end.saturating_sub(left)) & !1) as i32, height: ((y_end.saturating_sub(top)) & !1) as i32, left: left as i32, top: top as i32 })
}

/// The rectangle to crop `gray` (a frame `width` by `height`) to, by `method`. `later` is the frame a moment after, for `Static`.
pub fn measure(method: CropMethod, gray: &[u8], later: Option<&[u8]>, width: usize, height: usize) -> Option<CropRect> {
    if gray.len() != width * height || width == 0 || height == 0 {
        return None;
    }
    match method {
        CropMethod::Edges => Some(find_picture(gray, width, height, EDGE_THRESHOLD, SOFT_PIXELS)),
        CropMethod::Static => later.filter(|later| later.len() == gray.len()).and_then(|later| find_moving_picture(gray, later, width, height, MOTION_THRESHOLD)),
        CropMethod::CropDetect => None,
    }
}

/// `rect` with `pixels` removed from each side that was cut out of the `source`, so only a backdrop or a pillarbox is trimmed.
/// Sizes stay even. A trim that would leave nothing is ignored.
pub fn trimmed(rect: CropRect, pixels: i32, source: Option<(i32, i32)>) -> CropRect {
    let Some((source_width, source_height)) = source.filter(|_| pixels > 0) else { return rect };
    let inset = pixels + pixels % 2;
    let left = if rect.left > 0 { inset } else { 0 };
    let right = if rect.left + rect.width < source_width { inset } else { 0 };
    let top = if rect.top > 0 { inset } else { 0 };
    let bottom = if rect.top + rect.height < source_height { inset } else { 0 };
    let (width, height) = (rect.width - left - right, rect.height - top - bottom);
    if width <= 2 || height <= 2 {
        return rect;
    }
    CropRect { width: width & !1, height: height & !1, left: rect.left + left, top: rect.top + top }
}

/// Whether two measurements agree to within a few pixels.
pub fn agree(a: CropRect, b: CropRect) -> bool {
    [(a.width, b.width), (a.height, b.height), (a.left, b.left), (a.top, b.top)].iter().all(|(x, y)| (x - y).abs() <= AGREE_TOLERANCE)
}

/// Whether `new` differs enough from `old` in size to replace it.
pub fn changed(old: CropRect, new: CropRect) -> bool {
    f64::from((old.width - new.width).abs()) > f64::from(old.width) * CHANGE_RATIO || f64::from((old.height - new.height).abs()) > f64::from(old.height) * CHANGE_RATIO
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: usize = 1280;
    const H: usize = 720;

    fn frame(value: impl Fn(usize, usize) -> u8) -> Vec<u8> {
        (0..H).flat_map(|y| (0..W).map(move |x| (x, y))).map(|(x, y)| value(x, y)).collect()
    }

    fn rect(width: i32, height: i32, left: i32, top: i32) -> CropRect {
        CropRect { width, height, left, top }
    }

    #[test]
    fn the_span_ignores_steps_inside_the_picture() {
        let profile: Vec<f64> = [vec![40.0; 10], vec![200.0; 10], vec![40.0; 10]].concat();
        assert_eq!(picture_span(&profile, 12.0), (10, 20));
        assert_eq!(picture_span(&[50.0; 30], 12.0), (0, 30), "no steps keeps the whole axis");
    }

    #[test]
    fn a_pillarbox_is_cropped_to_the_picture_column() {
        let gray = frame(|x, _| if (438..842).contains(&x) { 200 } else { 40 });
        assert_eq!(find_picture(&gray, W, H, 12.0, 0), rect(404, 720, 438, 0));
    }

    #[test]
    fn letterbox_bars_are_removed() {
        let gray = frame(|x, y| if (110..610).contains(&y) { (60 + (x * 7 + y * 3) % 60) as u8 } else { 0 });
        assert_eq!(find_picture(&gray, W, H, 12.0, 0), rect(1280, 500, 0, 110));
    }

    #[test]
    fn small_steps_do_not_crop_and_offsets_are_even() {
        assert_eq!(find_picture(&frame(|x, _| if x < 600 { 50 } else { 52 }), W, H, 12.0, 0), rect(1280, 720, 0, 0));
        let odd = find_picture(&frame(|x, _| if (437..841).contains(&x) { 200 } else { 40 }), W, H, 12.0, 0);
        assert!(odd.left % 2 == 0 && odd.width % 2 == 0);
    }

    #[test]
    fn a_static_backdrop_is_cropped_to_the_moving_picture() {
        let moving = |t: usize| frame(move |x, _| if (438..842).contains(&x) { ((x + t * 5) % 200) as u8 } else { 60 });
        assert_eq!(find_moving_picture(&moving(0), &moving(1), W, H, 3.0), Some(rect(404, 720, 438, 0)));
        let still = vec![60u8; W * H];
        assert_eq!(find_moving_picture(&still, &still, W, H, 3.0), None, "nothing moving gives no crop");
    }

    fn soft_border(soft_value: u8, soft_width: usize) -> Vec<u8> {
        frame(move |x, _| match x {
            x if x < 438 => 0,
            x if x < 438 + soft_width => soft_value,
            x if x < 842 => 200,
            _ => 0,
        })
    }

    #[test]
    fn a_near_black_soft_band_is_trimmed_up_to_the_cap() {
        let gray = soft_border(60, 3);
        assert_eq!(find_picture(&gray, W, H, 12.0, 0).left, 438);
        assert_eq!(find_picture(&gray, W, H, 12.0, 8), rect(400, 720, 442, 0));
        assert_eq!(find_picture(&soft_border(0, 0), W, H, 12.0, 8), rect(404, 720, 438, 0), "a sharp border is not trimmed");
        assert_eq!(find_picture(&soft_border(60, 6), W, H, 12.0, 2).left, 440, "the trim is capped");
    }

    #[test]
    fn a_trim_only_removes_sides_that_were_cut_out() {
        let source = Some((1280, 720));
        let pillarbox = rect(404, 720, 438, 0);
        assert_eq!(trimmed(pillarbox, 4, source), rect(396, 720, 442, 0));
        assert_eq!(trimmed(rect(1280, 500, 0, 110), 4, source), rect(1280, 492, 0, 114));
        assert_eq!(trimmed(pillarbox, 0, source), pillarbox);
        assert_eq!(trimmed(pillarbox, 4, None), pillarbox);
    }

    #[test]
    fn measurements_agree_within_a_few_pixels_and_a_crop_changes_by_a_tenth() {
        assert!(agree(rect(404, 720, 438, 0), rect(406, 720, 440, 2)));
        assert!(!agree(rect(404, 720, 438, 0), rect(404, 720, 450, 0)));
        assert!(!changed(rect(1000, 500, 0, 0), rect(1040, 500, 0, 0)));
        assert!(changed(rect(1000, 500, 0, 0), rect(1200, 500, 0, 0)));
    }

    #[test]
    fn a_picture_is_fitted_inside_the_area_and_covers_it() {
        // A wide picture in a tall area is as wide as the area, and has room above and below.
        let (x, y, width, height) = fitted(400.0, 800.0, 200.0, 100.0);
        assert_eq!((x, width, height), (0.0, 400.0, 200.0));
        assert!((y - 300.0).abs() < 1e-9);
        // It covers a tall area by being as tall as the area, cropped at the sides.
        let (x, y, width, height) = covering(400.0, 800.0, 200.0, 100.0, 1.0);
        assert_eq!((y, height), (0.0, 800.0));
        assert!((width - 1600.0).abs() < 1e-9 && (x + 600.0).abs() < 1e-9);
        // Zoomed in, it is bigger, and still centred.
        let (x, _, width, _) = covering(400.0, 800.0, 200.0, 100.0, 2.0);
        assert!((width - 3200.0).abs() < 1e-9 && (x + 1400.0).abs() < 1e-9);
    }

    #[test]
    fn the_bare_strips_are_shrunk_blurred_and_dimmed() {
        assert_eq!(bars(400.0, 800.0, (0.0, 300.0, 400.0, 200.0)), vec![(0.0, 0.0, 400.0, 300.0), (0.0, 500.0, 400.0, 300.0)]);
        assert!(bars(400.0, 200.0, (0.0, 0.0, 400.0, 200.0)).is_empty());
        // A 4 by 4 picture, white on the left half and black on the right, averaged to 2 by 1.
        let pixels: Vec<u8> = (0..16).flat_map(|i| if i % 4 < 2 { [255; 4] } else { [0; 4] }).collect();
        let small = shrink_region(&pixels, 4, 4, (0.0, 0.0, 4.0, 4.0), 2, 1);
        assert_eq!(small, vec![255, 255, 255, 255, 0, 0, 0, 0]);
        let mut blurred = small;
        blur_and_dim(&mut blurred, &mut Vec::new(), 2, 1, 1, 0.5);
        assert!(blurred[0] < 128 && blurred[0] > blurred[4]);
    }

    /// The blur as first written, summing the whole window for each pixel, to check the sliding window against.
    fn box_pass_by_sum(from: &[u8], width: usize, height: usize, radius: usize, horizontal: bool) -> Vec<u8> {
        let (length, lines, along, across) = if horizontal { (width, height, 1, width) } else { (height, width, width, 1) };
        let mut to = from.to_vec();
        for line in 0..lines {
            for i in 0..length {
                for channel in 0..3 {
                    let total: u32 = (i as isize - radius as isize..=(i + radius) as isize)
                        .map(|k| u32::from(from[(line * across + k.clamp(0, length as isize - 1) as usize * along) * 4 + channel]))
                        .sum();
                    to[(line * across + i * along) * 4 + channel] = (total / (2 * radius as u32 + 1)) as u8;
                }
            }
        }
        to
    }

    #[test]
    fn the_sliding_window_blur_matches_summing_the_window() {
        let (width, height) = (13, 7);
        let pixels: Vec<u8> = (0..width * height * 4).map(|i| ((i * 37 + i / 5 * 11) % 256) as u8).collect();
        // Radii smaller than, equal to and larger than the line, so the window reaches past both ends.
        for radius in [1, 3, 8, 20] {
            for horizontal in [true, false] {
                let mut slid = pixels.clone();
                box_pass(&pixels, &mut slid, width, height, radius, horizontal);
                let summed = box_pass_by_sum(&pixels, width, height, radius, horizontal);
                for (at, (a, b)) in slid.iter().zip(&summed).enumerate() {
                    if at % 4 != 3 {
                        assert_eq!(a, b, "radius {radius}, horizontal {horizontal}, byte {at}");
                    }
                }
            }
        }
    }

    #[test]
    fn the_filters_are_labelled() {
        assert_eq!(crop_filter(rect(404, 720, 438, 0)), "@rsimgv-crop:crop=w=404:h=720:x=438:y=0");
        assert!(detector_filter().starts_with("@rsimgv-cropdetect:cropdetect="));
        assert_eq!(CropMethod::from_name("edges"), CropMethod::Edges);
        assert_eq!(CropMethod::from_name("nonsense"), CropMethod::CropDetect);
    }
}
