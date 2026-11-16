//! The time left before the slideshow moves on, drawn over the picture as a bar along its bottom edge, a circle that
//! empties, or the seconds left as text.

use std::cell::Cell;
use std::f64::consts::{FRAC_PI_2, PI, TAU};
use std::rc::Rc;

use gtk::prelude::*;

/// How the time left is shown.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Style {
    #[default]
    None,
    Bar,
    Circle,
    Text,
}

impl Style {
    /// The style a setting value names, or none for any other.
    pub fn from_name(name: &str) -> Self {
        match name {
            "bar" => Self::Bar,
            "circle" => Self::Circle,
            "text" => Self::Text,
            _ => Self::None,
        }
    }
}

/// What is drawn: the share of the wait that is left, and the whole seconds left, rounded up.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Progress {
    left: f64,
    seconds: u32,
}

/// The progress of a wait of `total` seconds of which `elapsed` have passed.
fn progress(elapsed: f64, total: f64) -> Progress {
    let total = total.max(f64::EPSILON);
    let remaining = (total - elapsed).clamp(0.0, total);
    Progress { left: remaining / total, seconds: remaining.ceil() as u32 }
}

const BAR_HEIGHT: i32 = 4;
const CIRCLE_SIZE: i32 = 40;
const CIRCLE_LINE: f64 = 4.0;
const MARGIN: i32 = 16;

/// The widget to put over the picture. It lets the pointer through, and is hidden unless it has something to show.
pub struct Countdown {
    area: gtk::DrawingArea,
    style: Cell<Style>,
    progress: Rc<Cell<Progress>>,
}

impl Default for Countdown {
    fn default() -> Self {
        let area = gtk::DrawingArea::new();
        area.set_can_target(false);
        area.set_visible(false);
        let progress = Rc::new(Cell::new(Progress::default()));
        let style = Cell::new(Style::None);
        let this = Self { area, style, progress };
        this.place();
        this
    }
}

impl Countdown {
    pub fn widget(&self) -> &gtk::DrawingArea {
        &self.area
    }

    /// Sets how the time left is shown. It is drawn again when the next progress is set.
    pub fn set_style(&self, style: Style) {
        self.style.set(style);
        self.place();
        if style == Style::None {
            self.area.set_visible(false);
        }
    }

    pub fn style(&self) -> Style {
        self.style.get()
    }

    /// Shows a wait of `total` seconds of which `elapsed` have passed, or nothing when `wait` is `None`.
    pub fn set_wait(&self, wait: Option<(f64, f64)>) {
        let Some((elapsed, total)) = wait.filter(|_| self.style.get() != Style::None) else {
            self.area.set_visible(false);
            return;
        };
        let now = progress(elapsed, total);
        if self.progress.replace(now) != now || !self.area.is_visible() {
            self.area.set_visible(true);
            self.area.queue_draw();
        }
    }

    /// Sizes and aligns the widget for the style, and sets what draws it.
    fn place(&self) {
        let area = &self.area;
        let style = self.style.get();
        area.set_margin_start(0);
        area.set_margin_top(0);
        area.set_valign(gtk::Align::End);
        match style {
            Style::Bar => {
                area.set_halign(gtk::Align::Fill);
                area.set_margin_end(0);
                area.set_margin_bottom(0);
                area.set_content_width(1);
                area.set_content_height(BAR_HEIGHT);
            }
            Style::Circle => {
                area.set_halign(gtk::Align::End);
                area.set_margin_end(MARGIN);
                area.set_margin_bottom(MARGIN);
                area.set_content_width(CIRCLE_SIZE);
                area.set_content_height(CIRCLE_SIZE);
            }
            Style::Text | Style::None => {
                area.set_halign(gtk::Align::End);
                area.set_margin_end(MARGIN);
                area.set_margin_bottom(MARGIN);
                area.set_content_width(56);
                area.set_content_height(32);
            }
        }
        let progress = self.progress.clone();
        area.set_draw_func(move |_, cr, width, height| draw(style, progress.get(), cr, f64::from(width), f64::from(height)));
    }
}

fn draw(style: Style, progress: Progress, cr: &gtk::cairo::Context, width: f64, height: f64) {
    match style {
        Style::None => {}
        Style::Bar => {
            cr.set_source_rgba(0.0, 0.0, 0.0, 0.4);
            let _ = cr.paint();
            cr.set_source_rgba(1.0, 1.0, 1.0, 0.85);
            cr.rectangle(0.0, 0.0, width * progress.left, height);
            let _ = cr.fill();
        }
        Style::Circle => {
            let (x, y, radius) = (width / 2.0, height / 2.0, (width.min(height) - CIRCLE_LINE) / 2.0);
            cr.set_source_rgba(0.0, 0.0, 0.0, 0.45);
            cr.arc(x, y, radius + CIRCLE_LINE / 2.0, 0.0, TAU);
            let _ = cr.fill();
            cr.set_line_width(CIRCLE_LINE);
            cr.set_line_cap(gtk::cairo::LineCap::Round);
            cr.set_source_rgba(1.0, 1.0, 1.0, 0.85);
            // Starts at the top and empties clockwise.
            cr.arc(x, y, radius, -FRAC_PI_2, -FRAC_PI_2 + TAU * progress.left);
            let _ = cr.stroke();
        }
        Style::Text => {
            let corner = height / 2.0;
            cr.set_source_rgba(0.0, 0.0, 0.0, 0.5);
            cr.new_sub_path();
            cr.arc(width - corner, corner, corner, -FRAC_PI_2, FRAC_PI_2);
            cr.arc(corner, corner, corner, FRAC_PI_2, FRAC_PI_2 + PI);
            cr.close_path();
            let _ = cr.fill();
            cr.select_font_face("sans", gtk::cairo::FontSlant::Normal, gtk::cairo::FontWeight::Bold);
            cr.set_font_size(15.0);
            let text = format!("{}s", progress.seconds);
            if let Ok(extents) = cr.text_extents(&text) {
                cr.set_source_rgba(1.0, 1.0, 1.0, 0.95);
                cr.move_to((width - extents.width()) / 2.0 - extents.x_bearing(), (height - extents.height()) / 2.0 - extents.y_bearing());
                let _ = cr.show_text(&text);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn style_names() {
        assert_eq!(Style::from_name("bar"), Style::Bar);
        assert_eq!(Style::from_name("circle"), Style::Circle);
        assert_eq!(Style::from_name("text"), Style::Text);
        assert_eq!(Style::from_name("none"), Style::None);
        assert_eq!(Style::from_name("unknown"), Style::None);
    }

    #[test]
    fn progress_counts_down_and_stays_in_range() {
        assert_eq!(progress(0.0, 5.0), Progress { left: 1.0, seconds: 5 });
        assert_eq!(progress(0.5, 5.0).seconds, 5, "a started second still counts");
        assert_eq!(progress(2.5, 5.0), Progress { left: 0.5, seconds: 3 });
        assert_eq!(progress(9.0, 5.0), Progress { left: 0.0, seconds: 0 });
        assert_eq!(progress(-1.0, 5.0).left, 1.0);
    }
}
