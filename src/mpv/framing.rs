//! How a video is framed by mpv filters: the picture cropped out of the bars or backdrop round it, and the picture padded
//! to a canvas of a set shape over a blurred, dimmed copy of itself.
//!
//! The crop is an mpv `crop` filter. Where it should crop is found by mpv's own `cropdetect` filter, which reports the
//! area that is not near black, or by looking at a frame decoded from the file: at the steps in brightness that mark the
//! picture's border (`Edges`), which also strips a blurred backdrop, or at the area that changes between two frames
//! (`Static`), which strips a backdrop that does not move. The backdrop is an `lavfi` filter graph.

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

/// The backdrop: the shape of the canvas, as width over height, how far the copy behind is zoomed, how blurred, and how
/// bright it is, from 0 for black to 1 for unchanged.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Backdrop {
    pub aspect: f64,
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

/// The label of the filter that finds the picture, the one that crops to it, and the backdrop.
pub const DETECTOR_LABEL: &str = "rsimgv-cropdetect";
pub const CROP_LABEL: &str = "rsimgv-crop";
pub const BACKDROP_LABEL: &str = "rsimgv-backdrop";

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

/// The `lavfi` graph that pads the picture to `backdrop.aspect` over a blurred copy of itself. The copy is the picture
/// scaled up (zoomed), cropped to the canvas, shrunk to a quarter, blurred, dimmed and scaled back up; working at a quarter
/// size keeps the blur cheap. The picture is then scaled to fit and centred on it. The canvas is the smallest box of that
/// shape that holds the picture, so content of that shape is left as it is.
pub fn backdrop_graph(backdrop: Backdrop) -> String {
    let aspect = backdrop.aspect.max(0.1);
    let box_w = format!("max(iw,ih*{aspect})");
    let box_h = format!("max(ih,iw/{aspect})");
    let zoom = backdrop.zoom.max(1.0);
    let (blur, dim) = (backdrop.blur.max(1), backdrop.dim.clamp(0.0, 1.0));
    format!(
        "[in]split[p][a];\
         [a]scale=w='2*trunc({box_w}*{zoom}/8)':h='2*trunc({box_h}*{zoom}/8)':force_original_aspect_ratio=increase,\
         crop=w='min(iw,ih*{aspect})':h='min(ih,iw/{aspect})',\
         boxblur={blur}:1,lutyuv=y='val*{dim}',scale=w='iw*4':h='ih*4'[bg];\
         [p]scale=w='{box_w}':h='{box_h}':force_original_aspect_ratio=decrease[fg];\
         [bg][fg]overlay=x='(W-w)/2':y='(H-h)/2',format=yuv420p"
    )
}

/// The backdrop as an `vf` argument.
pub fn backdrop_filter(backdrop: Backdrop) -> String {
    format!("@{BACKDROP_LABEL}:lavfi=[{}]", backdrop_graph(backdrop))
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
    fn the_filters_are_labelled_and_the_backdrop_pads_to_the_canvas() {
        assert_eq!(crop_filter(rect(404, 720, 438, 0)), "@rsimgv-crop:crop=w=404:h=720:x=438:y=0");
        assert!(detector_filter().starts_with("@rsimgv-cropdetect:cropdetect="));
        let filter = backdrop_filter(Backdrop { aspect: 16.0 / 9.0, zoom: 0.5, blur: 8, dim: 0.5 });
        assert!(filter.starts_with("@rsimgv-backdrop:lavfi=[[in]split[p][a];"));
        assert!(filter.contains("boxblur=8:1") && filter.contains("val*0.5") && filter.ends_with("format=yuv420p]"));
        assert!(filter.contains("/8)':h="), "a zoom below one counts as one");
        assert_eq!(CropMethod::from_name("edges"), CropMethod::Edges);
        assert_eq!(CropMethod::from_name("nonsense"), CropMethod::CropDetect);
    }
}
