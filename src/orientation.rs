//! How a picture or video is turned and mirrored for viewing: mirrored left to right, then turned clockwise by a
//! number of quarter turns. Every way of turning and mirroring a picture is one of these eight.

/// The turn and mirror of what is shown, relative to the file.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Orientation {
    /// Whether the picture is mirrored left to right before it is turned.
    mirrored: bool,
    /// The clockwise quarter turns, 0 to 3.
    turns: u8,
}

impl Orientation {
    /// Whether this leaves the picture as the file has it.
    pub fn is_identity(self) -> bool {
        self == Self::default()
    }

    /// Whether the shown picture's width and height are the file's, swapped.
    pub fn swaps_axes(self) -> bool {
        self.turns % 2 == 1
    }

    /// Whether the picture is mirrored left to right before it is turned.
    pub fn is_mirrored(self) -> bool {
        self.mirrored
    }

    /// The orientation that mirrors the picture if `mirrored`, then turns it `degrees` clockwise, to the nearest quarter turn.
    pub fn from_parts(mirrored: bool, degrees: f64) -> Self {
        Self { mirrored, turns: 0 }.with_rotation(degrees)
    }

    /// The clockwise rotation in degrees: 0, 90, 180 or 270.
    pub fn degrees(self) -> u16 {
        u16::from(self.turns) * 90
    }

    /// Turns what is shown a quarter turn clockwise.
    pub fn rotated_clockwise(self) -> Self {
        Self { turns: (self.turns + 1) % 4, ..self }
    }

    pub fn rotated_counter_clockwise(self) -> Self {
        Self { turns: (self.turns + 3) % 4, ..self }
    }

    /// Turns to `degrees` clockwise from the file's orientation, to the nearest quarter turn. The mirror stays.
    pub fn with_rotation(self, degrees: f64) -> Self {
        let turns = (degrees / 90.0).round().rem_euclid(4.0) as u8;
        Self { turns, ..self }
    }

    /// Mirrors what is shown, left to right.
    pub fn flipped_horizontally(self) -> Self {
        Self { mirrored: !self.mirrored, turns: (4 - self.turns) % 4 }
    }

    /// Mirrors what is shown, top to bottom.
    pub fn flipped_vertically(self) -> Self {
        Self { mirrored: !self.mirrored, turns: (6 - self.turns) % 4 }
    }

    /// The libavfilter graph that gives a video this orientation, or `None` if it needs none.
    pub fn filter_graph(self) -> Option<String> {
        let mut filters = Vec::new();
        if self.mirrored {
            filters.push("hflip");
        }
        match self.turns {
            1 => filters.push("transpose=clock"),
            2 => filters.extend(["hflip", "vflip"]),
            3 => filters.push("transpose=cclock"),
            _ => {}
        }
        (!filters.is_empty()).then(|| filters.join(","))
    }

    /// The size of a picture of `width` by `height` once oriented.
    pub fn oriented_size(self, width: i32, height: i32) -> (i32, i32) {
        if self.swaps_axes() {
            (height, width)
        } else {
            (width, height)
        }
    }

    /// `pixels`, 4 bytes each with no padding between rows, `width` by `height`, oriented. The result has the size
    /// `oriented_size` gives.
    pub fn apply(self, pixels: &[u8], width: i32, height: i32) -> Vec<u8> {
        let (out_width, out_height) = self.oriented_size(width, height);
        let (width, height) = (width as usize, height as usize);
        let mut out = vec![0u8; pixels.len()];
        for y in 0..out_height as usize {
            for x in 0..out_width as usize {
                let (mut sx, sy) = match self.turns {
                    1 => (y, height - 1 - x),
                    2 => (width - 1 - x, height - 1 - y),
                    3 => (width - 1 - y, x),
                    _ => (x, y),
                };
                if self.mirrored {
                    sx = width - 1 - sx;
                }
                let from = (sy * width + sx) * 4;
                let to = (y * out_width as usize + x) * 4;
                out[to..to + 4].copy_from_slice(&pixels[from..from + 4]);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 2 by 3 picture whose pixels are numbered row by row.
    fn picture() -> Vec<u8> {
        (0..6u8).flat_map(|n| [n, 0, 0, 255]).collect()
    }

    fn numbers(pixels: &[u8]) -> Vec<u8> {
        pixels.chunks(4).map(|pixel| pixel[0]).collect()
    }

    #[test]
    fn an_orientation_is_made_from_its_parts() {
        let turned = Orientation::from_parts(true, 270.0);
        assert!(turned.is_mirrored());
        assert_eq!(turned.degrees(), 270);
        assert_eq!(Orientation::from_parts(false, 0.0), Orientation::default());
        assert_eq!(Orientation::from_parts(false, 450.0).degrees(), 90, "a turn past the full circle wraps");
    }

    #[test]
    fn a_quarter_turn_clockwise_puts_the_left_column_on_top() {
        // 0 1 / 2 3 / 4 5 turned clockwise is 4 2 0 / 5 3 1.
        let turned = Orientation::default().rotated_clockwise();
        assert_eq!(numbers(&turned.apply(&picture(), 2, 3)), [4, 2, 0, 5, 3, 1]);
        assert_eq!(turned.oriented_size(2, 3), (3, 2));
    }

    #[test]
    fn a_quarter_turn_counter_clockwise_puts_the_right_column_on_top() {
        let turned = Orientation::default().rotated_counter_clockwise();
        assert_eq!(numbers(&turned.apply(&picture(), 2, 3)), [1, 3, 5, 0, 2, 4]);
    }

    #[test]
    fn flips_mirror_what_is_shown_even_after_a_turn() {
        assert_eq!(numbers(&Orientation::default().flipped_horizontally().apply(&picture(), 2, 3)), [1, 0, 3, 2, 5, 4]);
        assert_eq!(numbers(&Orientation::default().flipped_vertically().apply(&picture(), 2, 3)), [4, 5, 2, 3, 0, 1]);
        // Turned clockwise the picture is 4 2 0 / 5 3 1, and mirrored left to right 0 2 4 / 1 3 5.
        let turned = Orientation::default().rotated_clockwise().flipped_horizontally();
        assert_eq!(numbers(&turned.apply(&picture(), 2, 3)), [0, 2, 4, 1, 3, 5]);
    }

    #[test]
    fn four_turns_and_two_flips_are_no_change() {
        let mut orientation = Orientation::default();
        for _ in 0..4 {
            orientation = orientation.rotated_clockwise();
        }
        assert!(orientation.is_identity());
        assert!(orientation.flipped_horizontally().flipped_horizontally().is_identity());
        assert!(orientation.flipped_vertically().flipped_vertically().is_identity());
        assert!(orientation.flipped_horizontally().flipped_vertically().rotated_clockwise().rotated_clockwise().is_identity());
    }

    #[test]
    fn set_rotation_keeps_the_mirror_and_wraps() {
        let orientation = Orientation::default().flipped_horizontally().with_rotation(270.0);
        assert_eq!(orientation.degrees(), 270);
        assert!(!orientation.is_identity());
        assert_eq!(Orientation::default().with_rotation(-90.0).degrees(), 270);
        assert_eq!(Orientation::default().with_rotation(360.0).degrees(), 0);
    }

    #[test]
    fn the_video_filter_matches_the_pixels() {
        assert_eq!(Orientation::default().filter_graph(), None);
        assert_eq!(Orientation::default().rotated_clockwise().filter_graph().as_deref(), Some("transpose=clock"));
        assert_eq!(Orientation::default().flipped_horizontally().filter_graph().as_deref(), Some("hflip"));
        assert_eq!(Orientation::default().flipped_vertically().filter_graph().as_deref(), Some("hflip,hflip,vflip"));
    }
}
