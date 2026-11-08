//! A draggable rectangle over the picture for setting a video's crop. The video is shown uncropped behind it; the area
//! outside the rectangle is dimmed, and its edges and corners can be dragged. Apply or Cancel is shown over it.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, glib};

use crate::mpv::framing::{self, CropRect};

/// A crop as shares of the video's own width and height.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CropFraction {
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
}

impl CropFraction {
    pub fn full() -> Self {
        Self { left: 0.0, top: 0.0, width: 1.0, height: 1.0 }
    }

    /// `rect` of a video of `source` size, as shares.
    pub fn from_rect(rect: CropRect, source: (i32, i32)) -> Self {
        let (width, height) = (f64::from(source.0.max(1)), f64::from(source.1.max(1)));
        Self {
            left: f64::from(rect.left) / width,
            top: f64::from(rect.top) / height,
            width: f64::from(rect.width) / width,
            height: f64::from(rect.height) / height,
        }
    }

    /// The rectangle of a video of `source` size this describes, with even offsets and sizes.
    pub fn to_rect(self, source: (i32, i32)) -> CropRect {
        let (source_width, source_height) = (source.0.max(2), source.1.max(2));
        let left = (self.left.clamp(0.0, 1.0) * f64::from(source_width)).round() as i32;
        let left = (left & !1).min(source_width - 2);
        let top = (self.top.clamp(0.0, 1.0) * f64::from(source_height)).round() as i32;
        let top = (top & !1).min(source_height - 2);
        let width = ((self.width.clamp(0.0, 1.0) * f64::from(source_width)).round() as i32 & !1).max(2);
        let height = ((self.height.clamp(0.0, 1.0) * f64::from(source_height)).round() as i32 & !1).max(2);
        CropRect { width: width.min((source_width - left) & !1), height: height.min((source_height - top) & !1), left, top }
    }
}

/// The smallest share of the picture a crop edge may be dragged to.
const MIN_SHARE: f64 = 0.02;
/// How near an edge, in pixels, a drag grabs it.
const GRAB: f64 = 14.0;
const HANDLE: f32 = 8.0;

#[derive(Clone, Copy, PartialEq)]
enum Grab {
    Move,
    Left,
    Right,
    Top,
    Bottom,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

struct State {
    source: (i32, i32),
    rect: CropFraction,
    grab: Option<Grab>,
    start: (f64, f64),
    start_rect: CropFraction,
}

/// The draggable crop overlay. Keep `root` in the viewer while it is being edited.
pub struct CropEditor {
    pub root: gtk::Overlay,
}

/// The four edge values shown round the crop, as pixels of the video.
#[derive(Clone)]
struct Entries {
    left: gtk::SpinButton,
    top: gtk::SpinButton,
    right: gtk::SpinButton,
    bottom: gtk::SpinButton,
    syncing: Rc<Cell<bool>>,
}

#[derive(Clone, Copy)]
enum Edge {
    Left,
    Top,
    Right,
    Bottom,
}

impl Entries {
    fn new(source: (i32, i32), rect: CropFraction) -> Self {
        let (width, height) = (source.0.max(2), source.1.max(2));
        let left = edge_value("Left edge, in pixels", 0, width - 2);
        let top = edge_value("Top edge, in pixels", 0, height - 2);
        let right = edge_value("Right edge, in pixels", 2, width);
        let bottom = edge_value("Bottom edge, in pixels", 2, height);
        let entries = Self { left, top, right, bottom, syncing: Rc::new(Cell::new(false)) };
        sync_entries(&entries, rect, source);
        entries
    }

    fn all(&self) -> [&gtk::SpinButton; 4] {
        [&self.left, &self.top, &self.right, &self.bottom]
    }
}

fn edge_value(tooltip: &str, lower: i32, upper: i32) -> gtk::SpinButton {
    let spin = gtk::SpinButton::with_range(f64::from(lower), f64::from(upper), 2.0);
    spin.set_numeric(true);
    spin.set_digits(0);
    spin.set_width_chars(4);
    spin.set_tooltip_text(Some(tooltip));
    spin
}

fn sync_entries(entries: &Entries, rect: CropFraction, source: (i32, i32)) {
    let pixels = rect.to_rect(source);
    entries.syncing.set(true);
    entries.left.set_value(f64::from(pixels.left));
    entries.top.set_value(f64::from(pixels.top));
    entries.right.set_value(f64::from(pixels.left + pixels.width));
    entries.bottom.set_value(f64::from(pixels.top + pixels.height));
    entries.syncing.set(false);
}

/// Puts each edge's value box at the middle of that edge of the crop.
fn position_entries(fixed: &gtk::Fixed, entries: &Entries, state: &State, width: i32, height: i32) {
    let picture = framing::fitted(f64::from(width), f64::from(height), f64::from(state.source.0.max(1)), f64::from(state.source.1.max(1)));
    let (left, top, crop_width, crop_height) = crop_pixels(state.rect, picture);
    let (right, bottom) = (left + crop_width, top + crop_height);
    let (box_width, box_height) = (
        f64::from(entries.left.measure(gtk::Orientation::Horizontal, -1).1),
        f64::from(entries.left.measure(gtk::Orientation::Vertical, -1).1),
    );
    let most_x = (f64::from(width) - box_width).max(0.0);
    let most_y = (f64::from(height) - box_height).max(0.0);
    let place = |spin: &gtk::SpinButton, x: f64, y: f64| {
        fixed.move_(spin, x.clamp(0.0, most_x).round(), y.clamp(0.0, most_y).round());
    };
    place(&entries.left, left - box_width / 2.0, top + crop_height / 2.0 - box_height / 2.0);
    place(&entries.right, right - box_width / 2.0, top + crop_height / 2.0 - box_height / 2.0);
    place(&entries.top, left + crop_width / 2.0 - box_width / 2.0, top - box_height / 2.0);
    place(&entries.bottom, left + crop_width / 2.0 - box_width / 2.0, bottom - box_height / 2.0);
}

fn adjust_edge(pixels: &mut CropRect, edge: Edge, value: i32, source: (i32, i32)) {
    match edge {
        Edge::Left => {
            let right = pixels.left + pixels.width;
            pixels.left = value.clamp(0, right - 2);
            pixels.width = right - pixels.left;
        }
        Edge::Top => {
            let bottom = pixels.top + pixels.height;
            pixels.top = value.clamp(0, bottom - 2);
            pixels.height = bottom - pixels.top;
        }
        Edge::Right => {
            let right = value.clamp(pixels.left + 2, source.0);
            pixels.width = right - pixels.left;
        }
        Edge::Bottom => {
            let bottom = value.clamp(pixels.top + 2, source.1);
            pixels.height = bottom - pixels.top;
        }
    }
}

fn connect_edge(entry: &gtk::SpinButton, edge: Edge, state: &Rc<RefCell<State>>, entries: &Entries, area: &gtk::DrawingArea) {
    entry.connect_value_changed({
        let state = state.clone();
        let entries = entries.clone();
        let area = area.clone();
        move |spin| {
            if entries.syncing.get() {
                return;
            }
            let (source, mut pixels) = {
                let state = state.borrow();
                (state.source, state.rect.to_rect(state.source))
            };
            adjust_edge(&mut pixels, edge, (spin.value().round() as i32) & !1, source);
            {
                let mut state = state.borrow_mut();
                state.rect = CropFraction::from_rect(pixels, source);
            }
            sync_entries(&entries, state.borrow().rect, source);
            area.queue_draw();
        }
    });
}

impl CropEditor {
    pub fn new(
        source: (i32, i32),
        rect: CropFraction,
        on_apply: impl Fn(CropFraction) + 'static,
        on_cancel: impl Fn() + 'static,
    ) -> Self {
        let state = Rc::new(RefCell::new(State { source, rect, grab: None, start: (0.0, 0.0), start_rect: rect }));
        let area = gtk::DrawingArea::new();
        area.set_hexpand(true);
        area.set_vexpand(true);

        let entries = Entries::new(source, rect);
        let fixed = gtk::Fixed::new();
        fixed.set_hexpand(true);
        fixed.set_vexpand(true);
        fixed.set_can_target(false);
        for entry in entries.all() {
            fixed.put(entry, 0.0, 0.0);
        }
        connect_edge(&entries.left, Edge::Left, &state, &entries, &area);
        connect_edge(&entries.top, Edge::Top, &state, &entries, &area);
        connect_edge(&entries.right, Edge::Right, &state, &entries, &area);
        connect_edge(&entries.bottom, Edge::Bottom, &state, &entries, &area);

        area.set_draw_func({
            let state = state.clone();
            let entries = entries.clone();
            let fixed = fixed.clone();
            move |area, cr, width, height| {
                let state = state.borrow();
                position_entries(&fixed, &entries, &state, width, height);
                draw(&state, area, cr, width, height);
            }
        });

        let drag = gtk::GestureDrag::new();
        drag.connect_drag_begin({
            let state = state.clone();
            let area = area.clone();
            move |_, x, y| {
                let mut state = state.borrow_mut();
                let picture = picture_rect(&area, state.source);
                state.grab = hit(picture, state.rect, x, y);
                state.start = (x, y);
                state.start_rect = state.rect;
            }
        });
        drag.connect_drag_update({
            let state = state.clone();
            let entries = entries.clone();
            let area = area.clone();
            move |_, dx, dy| {
                let mut state = state.borrow_mut();
                let Some(grab) = state.grab else { return };
                let picture = picture_rect(&area, state.source);
                if picture.2 <= 0.0 || picture.3 <= 0.0 {
                    return;
                }
                state.rect = drag_rect(grab, state.start_rect, dx / picture.2, dy / picture.3);
                sync_entries(&entries, state.rect, state.source);
                area.queue_draw();
            }
        });
        drag.connect_drag_end({
            let state = state.clone();
            move |_, _, _| state.borrow_mut().grab = None
        });
        area.add_controller(drag);

        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion({
            let state = state.clone();
            let area = area.clone();
            move |_, x, y| {
                let state = state.borrow();
                let picture = picture_rect(&area, state.source);
                area.set_cursor_from_name(cursor(hit(picture, state.rect, x, y)));
            }
        });
        area.add_controller(motion);

        let apply = gtk::Button::with_label("Apply");
        apply.add_css_class("suggested-action");
        let cancel = gtk::Button::with_label("Cancel");
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        bar.add_css_class("osd");
        bar.set_halign(gtk::Align::Center);
        bar.set_valign(gtk::Align::End);
        bar.set_margin_bottom(24);
        bar.append(&gtk::Label::new(Some("Drag the crop or type its edges")));
        bar.append(&cancel);
        bar.append(&apply);

        let on_apply = Rc::new(on_apply);
        let on_cancel = Rc::new(on_cancel);
        apply.connect_clicked({
            let state = state.clone();
            let on_apply = on_apply.clone();
            move |_| on_apply(state.borrow().rect)
        });
        cancel.connect_clicked({
            let on_cancel = on_cancel.clone();
            move |_| on_cancel()
        });

        let root = gtk::Overlay::new();
        root.set_focusable(true);
        root.set_child(Some(&area));
        root.add_overlay(&fixed);
        root.add_overlay(&bar);
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        keys.connect_key_pressed({
            let on_cancel = on_cancel.clone();
            move |_, key, _, _| {
                if key == gdk::Key::Escape {
                    on_cancel();
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            }
        });
        root.add_controller(keys);
        root.grab_focus();

        Self { root }
    }
}

/// The picture's rectangle in `area`, from the video's own size.
fn picture_rect(area: &gtk::DrawingArea, source: (i32, i32)) -> (f64, f64, f64, f64) {
    framing::fitted(f64::from(area.width()), f64::from(area.height()), f64::from(source.0.max(1)), f64::from(source.1.max(1)))
}

/// The crop rectangle in `area` pixels, within `picture`.
fn crop_pixels(rect: CropFraction, picture: (f64, f64, f64, f64)) -> (f64, f64, f64, f64) {
    (
        picture.0 + rect.left.clamp(0.0, 1.0) * picture.2,
        picture.1 + rect.top.clamp(0.0, 1.0) * picture.3,
        rect.width.clamp(0.0, 1.0) * picture.2,
        rect.height.clamp(0.0, 1.0) * picture.3,
    )
}

fn hit(picture: (f64, f64, f64, f64), rect: CropFraction, x: f64, y: f64) -> Option<Grab> {
    let (left, top, width, height) = crop_pixels(rect, picture);
    let (right, bottom) = (left + width, top + height);
    let near_left = (x - left).abs() <= GRAB;
    let near_right = (x - right).abs() <= GRAB;
    let near_top = (y - top).abs() <= GRAB;
    let near_bottom = (y - bottom).abs() <= GRAB;
    let within_x = x >= left - GRAB && x <= right + GRAB;
    let within_y = y >= top - GRAB && y <= bottom + GRAB;
    match (near_left, near_right, near_top, near_bottom) {
        (true, _, true, _) if within_x && within_y => Some(Grab::TopLeft),
        (_, true, true, _) if within_x && within_y => Some(Grab::TopRight),
        (true, _, _, true) if within_x && within_y => Some(Grab::BottomLeft),
        (_, true, _, true) if within_x && within_y => Some(Grab::BottomRight),
        (true, _, _, _) if within_y => Some(Grab::Left),
        (_, true, _, _) if within_y => Some(Grab::Right),
        (_, _, true, _) if within_x => Some(Grab::Top),
        (_, _, _, true) if within_x => Some(Grab::Bottom),
        _ if x >= left && x <= right && y >= top && y <= bottom => Some(Grab::Move),
        _ => None,
    }
}

fn cursor(grab: Option<Grab>) -> Option<&'static str> {
    match grab {
        Some(Grab::Move) => Some("move"),
        Some(Grab::Left | Grab::Right) => Some("ew-resize"),
        Some(Grab::Top | Grab::Bottom) => Some("ns-resize"),
        Some(Grab::TopLeft | Grab::BottomRight) => Some("nwse-resize"),
        Some(Grab::TopRight | Grab::BottomLeft) => Some("nesw-resize"),
        None => None,
    }
}

fn drag_rect(grab: Grab, start: CropFraction, dx: f64, dy: f64) -> CropFraction {
    let right = start.left + start.width;
    let bottom = start.top + start.height;
    let mut rect = start;
    match grab {
        Grab::Move => {
            rect.left = (start.left + dx).clamp(0.0, 1.0 - start.width);
            rect.top = (start.top + dy).clamp(0.0, 1.0 - start.height);
        }
        Grab::Left | Grab::TopLeft | Grab::BottomLeft => {
            rect.left = (start.left + dx).clamp(0.0, right - MIN_SHARE);
            rect.width = right - rect.left;
        }
        Grab::Right | Grab::TopRight | Grab::BottomRight => {
            let right = (right + dx).clamp(start.left + MIN_SHARE, 1.0);
            rect.width = right - start.left;
        }
        _ => {}
    }
    match grab {
        Grab::Top | Grab::TopLeft | Grab::TopRight => {
            rect.top = (start.top + dy).clamp(0.0, bottom - MIN_SHARE);
            rect.height = bottom - rect.top;
        }
        Grab::Bottom | Grab::BottomLeft | Grab::BottomRight => {
            let bottom = (bottom + dy).clamp(start.top + MIN_SHARE, 1.0);
            rect.height = bottom - start.top;
        }
        _ => {}
    }
    rect
}

fn draw(state: &State, area: &gtk::DrawingArea, cr: &gtk::cairo::Context, width: i32, height: i32) {
    let picture = picture_rect(area, state.source);
    let (left, top, crop_width, crop_height) = crop_pixels(state.rect, picture);
    let (right, bottom) = (left + crop_width, top + crop_height);
    let (width, height) = (f64::from(width), f64::from(height));
    cr.set_source_rgba(0.0, 0.0, 0.0, 0.55);
    for (x, y, w, h) in [
        (0.0, 0.0, width, top),
        (0.0, bottom, width, height - bottom),
        (0.0, top, left, crop_height),
        (right, top, width - right, crop_height),
    ] {
        if w > 0.0 && h > 0.0 {
            cr.rectangle(x, y, w, h);
        }
    }
    let _ = cr.fill();
    cr.set_source_rgba(1.0, 1.0, 1.0, 0.92);
    for (x, y, w, h) in [
        (left, top, crop_width, 2.0),
        (left, bottom - 2.0, crop_width, 2.0),
        (left, top, 2.0, crop_height),
        (right - 2.0, top, 2.0, crop_height),
    ] {
        cr.rectangle(x, y, w, h);
    }
    for (x, y) in [(left, top), (right - f64::from(HANDLE), top), (left, bottom - f64::from(HANDLE)), (right - f64::from(HANDLE), bottom - f64::from(HANDLE))] {
        cr.rectangle(x, y, f64::from(HANDLE), f64::from(HANDLE));
    }
    let _ = cr.fill();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fraction_converts_both_ways_with_even_sizes() {
        let source = (1280, 720);
        let rect = CropRect { width: 960, height: 720, left: 160, top: 0 };
        let fraction = CropFraction::from_rect(rect, source);
        assert_eq!(fraction, CropFraction { left: 0.125, top: 0.0, width: 0.75, height: 1.0 });
        assert_eq!(fraction.to_rect(source), rect);
    }

    #[test]
    fn moving_and_resizing_stay_inside_the_picture() {
        let start = CropFraction { left: 0.25, top: 0.25, width: 0.5, height: 0.5 };
        let moved = drag_rect(Grab::Move, start, 10.0, -10.0);
        assert_eq!(moved, CropFraction { left: 0.5, top: 0.0, width: 0.5, height: 0.5 });
        let resized = drag_rect(Grab::Left, start, -0.1, 0.0);
        assert_eq!(resized, CropFraction { left: 0.15, top: 0.25, width: 0.6, height: 0.5 });
    }

    #[test]
    fn edge_values_adjust_one_side_and_stay_inside_the_video() {
        let mut pixels = CropRect { width: 400, height: 300, left: 100, top: 50 };
        adjust_edge(&mut pixels, Edge::Left, 160, (800, 600));
        assert_eq!(pixels, CropRect { width: 340, height: 300, left: 160, top: 50 });
        adjust_edge(&mut pixels, Edge::Right, 900, (800, 600));
        assert_eq!(pixels.left + pixels.width, 800, "the right edge stops at the video");
        adjust_edge(&mut pixels, Edge::Top, 0, (800, 600));
        assert_eq!(pixels.top, 0);
        adjust_edge(&mut pixels, Edge::Bottom, 100, (800, 600));
        assert_eq!(pixels.height, 100, "the bottom cannot pass the top");
    }
}
