//! How far a video is zoomed in, and where: a window of the picture, centred or moved to one side, cropped out and
//! shown as big as the picture was.

/// The most a video can be zoomed in, as a multiple.
pub const MAX_ZOOM: f64 = 8.0;

/// What a zoom in or out multiplies the zoom by.
pub const ZOOM_STEP: f64 = 1.25;

/// How far a pan moves the window, as a fraction of the way from one edge to the other.
pub const PAN_STEP: f64 = 0.2;

/// The zoom of a video: how many times larger the picture is, and where the window sits in it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Zoom {
    /// The multiple, from 1 (all of the picture) to `MAX_ZOOM`.
    pub level: f64,
    /// Where the window is horizontally and vertically, from -1 for against the left or top edge to 1 for against the right or
    /// bottom one. It means nothing at a level of 1.
    pub x: f64,
    pub y: f64,
}

impl Default for Zoom {
    fn default() -> Self {
        Self { level: 1.0, x: 0.0, y: 0.0 }
    }
}

impl Zoom {
    /// This zoom with its level and window kept within what they can be. Nearly 1 is 1, so zooming in and out ends where it began.
    pub fn clamped(self) -> Self {
        let level = if self.level < 1.001 { 1.0 } else { self.level.min(MAX_ZOOM) };
        if level == 1.0 {
            return Self::default();
        }
        Self { level, x: self.x.clamp(-1.0, 1.0), y: self.y.clamp(-1.0, 1.0) }
    }

    /// Whether this shows all of the picture.
    pub fn is_none(self) -> bool {
        self.level <= 1.0
    }

    /// This zoom with its level multiplied by `factor`, over the same window.
    pub fn zoomed(self, factor: f64) -> Self {
        Self { level: self.level * factor, ..self }.clamped()
    }

    /// This zoom with its window moved by `dx` and `dy`, as fractions of the way across.
    pub fn panned(self, dx: f64, dy: f64) -> Self {
        Self { x: self.x + dx, y: self.y + dy, ..self }.clamped()
    }

    /// The libavfilter that crops the window out, or `None` when all of the picture shows.
    pub fn filter_graph(self) -> Option<String> {
        let zoom = self.clamped();
        if zoom.is_none() {
            return None;
        }
        // Even sizes, as the filters and encoders after it need them.
        let (fraction_x, fraction_y) = ((1.0 + zoom.x) / 2.0, (1.0 + zoom.y) / 2.0);
        Some(format!(
            "crop=w=trunc(iw/{level:.4}/2)*2:h=trunc(ih/{level:.4}/2)*2:x=trunc((iw-ow)*{fraction_x:.4}/2)*2:y=trunc((ih-oh)*{fraction_y:.4}/2)*2",
            level = zoom.level
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zooming_in_and_back_out_ends_at_no_zoom() {
        let zoom = Zoom::default().zoomed(ZOOM_STEP).zoomed(ZOOM_STEP).panned(0.4, -0.2).zoomed(1.0 / ZOOM_STEP).zoomed(1.0 / ZOOM_STEP);
        assert!(zoom.is_none());
        assert_eq!(zoom, Zoom::default(), "the window is let go with the zoom");
    }

    #[test]
    fn the_zoom_and_window_stay_in_range() {
        let zoom = Zoom { level: 100.0, x: 3.0, y: -3.0 }.clamped();
        assert_eq!((zoom.level, zoom.x, zoom.y), (MAX_ZOOM, 1.0, -1.0));
        assert_eq!(Zoom::default().zoomed(0.5), Zoom::default(), "it does not zoom out past the picture");
    }

    #[test]
    fn the_filter_crops_the_window() {
        assert_eq!(Zoom::default().filter_graph(), None);
        let graph = Zoom { level: 2.0, x: -1.0, y: 1.0 }.filter_graph().unwrap();
        assert!(graph.starts_with("crop=w=trunc(iw/2.0000/2)*2:h=trunc(ih/2.0000/2)*2"), "{graph}");
        assert!(graph.contains("x=trunc((iw-ow)*0.0000/2)*2") && graph.contains("y=trunc((ih-oh)*1.0000/2)*2"), "{graph}");
    }
}
