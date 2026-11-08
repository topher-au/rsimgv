//! A draggable rectangle over the picture for setting a video's crop. The video is shown uncropped behind it; the area
//! outside the rectangle is dimmed, and its edges and corners can be dragged. Apply or Cancel is shown over it.

use std::cell::RefCell;
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
        area.set_draw_func({
            let state = state.clone();
            move |area, snapshot, width, height| draw(&state.borrow(), area, snapshot, width, height)
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
            let area = area.clone();
            move |_, dx, dy| {
                let mut state = state.borrow_mut();
                let Some(grab) = state.grab else { return };
                let picture = picture_rect(&area, state.source);
                if picture.2 <= 0.0 || picture.3 <= 0.0 {
                    return;
                }
                state.rect = drag_rect(grab, state.start_rect, dx / picture.2, dy / picture.3);
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
        bar.append(&gtk::Label::new(Some("Drag the crop")));
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
        root.add_overlay(&bar);
        let keys = gtk::EventControllerKey::new();
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
}
