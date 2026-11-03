//! Messages over the window's content. A message is a card of text placed at one of six places along the window's
//! edges, and is either temporary, going away by itself after a while, or persistent, staying until it is dismissed
//! by the code that showed it or by its close button. A persistent message can have its text changed while it is up.
//!
//! The messages are added to a `gtk::Overlay` as a column for each place, which is the window's, or the viewer's frame
//! while the viewer is open. A message that names no place goes where the
//! overlay's notification element is, and takes its look. A column is only as big as its messages,
//! so the rest of the window under the overlay still takes the clicks.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::rc::{Rc, Weak};
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;

use super::media_overlay::{set_fade, effects_css, set_edge_margins, align_label, horizontal_align, style_css, unique_class, Sheet};
use crate::overlay::{Layout, DEFAULT_MARGIN, Element, Kind, Overflow, Slot, Style, DEFAULT_MAX_NOTIFICATIONS};

/// How long a temporary message stays up unless it says otherwise.
pub const DEFAULT_DURATION: Duration = Duration::from_secs(4);
/// How long a message takes to fade in and out.
const FADE: Duration = Duration::from_millis(200);
/// The space between one message and the next, in pixels.
const SPACING: i32 = 6;
/// The widest a message's text goes before it wraps, in characters.
const WRAP_WIDTH: i32 = 60;

/// Names a message that is up, for changing or dismissing it.
pub type MessageId = u64;

/// What a message is about, which sets how it looks. `Plain` is bare text over the window, with no card behind it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Level {
    #[default]
    Info,
    Success,
    Warning,
    Error,
    Plain,
}

impl Level {
    /// The level a name in the IPC stands for.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "info" => Some(Self::Info),
            "success" => Some(Self::Success),
            "warning" => Some(Self::Warning),
            "error" => Some(Self::Error),
            "plain" => Some(Self::Plain),
            _ => None,
        }
    }

    fn css_class(self) -> &'static str {
        match self {
            Self::Info => "message-info",
            Self::Success => "message-success",
            Self::Warning => "message-warning",
            Self::Error => "message-error",
            Self::Plain => "message-plain",
        }
    }
}

/// Where along the window's edges a message sits. Messages in one place stack, the newest last.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Position {
    TopLeft,
    TopCenter,
    TopRight,
    BottomLeft,
    #[default]
    BottomCenter,
    BottomRight,
}

impl Position {
    pub const ALL: [Position; 6] = [
        Position::TopLeft,
        Position::TopCenter,
        Position::TopRight,
        Position::BottomLeft,
        Position::BottomCenter,
        Position::BottomRight,
    ];

    /// The position a name in the IPC stands for, such as `top-left`.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "top-left" => Some(Self::TopLeft),
            "top-center" => Some(Self::TopCenter),
            "top-right" => Some(Self::TopRight),
            "bottom-left" => Some(Self::BottomLeft),
            "bottom-center" => Some(Self::BottomCenter),
            "bottom-right" => Some(Self::BottomRight),
            _ => None,
        }
    }

    /// The place over the window the position stands for.
    pub fn slot(self) -> Slot {
        let (row, column) = match self {
            Self::TopLeft => (0, 0),
            Self::TopCenter => (0, 1),
            Self::TopRight => (0, 2),
            Self::BottomLeft => (2, 0),
            Self::BottomCenter => (2, 1),
            Self::BottomRight => (2, 2),
        };
        Slot { row, column }
    }
}

/// How long a message stays up.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Lifetime {
    /// It goes away by itself after this long.
    Temporary(Duration),
    /// It stays until it is dismissed.
    Persistent,
}

/// A message to show.
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub text: String,
    pub level: Level,
    /// Where it sits, or `None` for where the notification element says.
    pub position: Option<Slot>,
    pub lifetime: Lifetime,
    /// Whether the message has a close button. A temporary message goes by itself, so it has none unless asked to.
    pub dismissible: bool,
}

impl Message {
    /// A notification: a temporary message at the bottom of the window.
    pub fn notification(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            level: Level::Info,
            position: None,
            lifetime: Lifetime::Temporary(DEFAULT_DURATION),
            dismissible: false,
        }
    }

    /// A message that stays until it is dismissed, which its close button does.
    pub fn persistent(text: impl Into<String>) -> Self {
        Self { lifetime: Lifetime::Persistent, dismissible: true, ..Self::notification(text) }
    }

    pub fn level(mut self, level: Level) -> Self {
        self.level = level;
        self
    }

    pub fn at(mut self, position: Position) -> Self {
        self.position = Some(position.slot());
        self
    }

    pub fn lasting(mut self, duration: Duration) -> Self {
        self.lifetime = Lifetime::Temporary(duration);
        self
    }

    pub fn dismissible(mut self, dismissible: bool) -> Self {
        self.dismissible = dismissible;
        self
    }
}

/// A message that is up.
struct Shown {
    slot: Slot,
    revealer: gtk::Revealer,
    label: gtk::Label,
    /// The timer that dismisses a temporary message.
    timer: Option<glib::SourceId>,
}

/// How messages look, from the notification element.
struct Look {
    /// The element the look is from, so the same look is not made again, which would drop the stylesheet of the messages
    /// already up.
    element: Element,
    class: String,
    _sheet: Sheet,
}

struct Inner {
    /// The column of messages at each place.
    columns: HashMap<Slot, gtk::Box>,
    /// The overlay the columns are on.
    host: RefCell<gtk::Overlay>,
    /// Where a message goes that names no place.
    default_slot: Cell<Slot>,
    look: RefCell<Option<Look>>,
    /// The most messages a stack shows at once, and what happens to one that comes to a full stack.
    limit: Cell<usize>,
    overflow: Cell<Overflow>,
    /// Messages waiting for room in the stack they are for, oldest first.
    queue: RefCell<VecDeque<(MessageId, Slot, Message)>>,
    messages: RefCell<HashMap<MessageId, Shown>>,
    next: Cell<MessageId>,
}

/// Shows messages over a window. Clones show the same messages.
#[derive(Clone)]
pub struct MessageOverlay {
    inner: Rc<Inner>,
}

impl MessageOverlay {
    /// Messages over what `overlay` holds.
    pub fn new(overlay: &gtk::Overlay) -> Self {
        let columns = Slot::all()
            .map(|slot| {
                let column = gtk::Box::new(gtk::Orientation::Vertical, SPACING);
                column.set_halign(horizontal_align(slot.text_align()));
                column.set_valign([gtk::Align::Start, gtk::Align::Center, gtk::Align::End][usize::from(slot.row)]);
                set_edge_margins(&column, slot, DEFAULT_MARGIN);
                overlay.add_overlay(&column);
                (slot, column)
            })
            .collect();
        Self {
            inner: Rc::new(Inner {
                columns,
                host: RefCell::new(overlay.clone()),
                default_slot: Cell::new(Slot::BOTTOM_MIDDLE),
                look: RefCell::new(None),
                limit: Cell::new(DEFAULT_MAX_NOTIFICATIONS as usize),
                overflow: Cell::new(Overflow::PushOut),
                queue: RefCell::new(VecDeque::new()),
                messages: RefCell::new(HashMap::new()),
                next: Cell::new(0),
            }),
        }
    }

    /// Sets where messages that name no place go, and how all later messages look, from the layout's notification
    /// element. With none, they go to the bottom middle and look as the stylesheet says.
    pub fn configure(&self, element: Option<&Element>) {
        let element = element.filter(|element| element.kind == Kind::Messages && element.enabled);
        self.inner.default_slot.set(element.map_or(Slot::BOTTOM_MIDDLE, |element| element.slot));
        self.set_limit(element.map_or(DEFAULT_MAX_NOTIFICATIONS as usize, |element| element.max_notifications.max(1) as usize), element.map_or_else(Overflow::default, |element| element.overflow));
        // The messages up keep the look they were made with, so the same look is left as it is.
        if self.inner.look.borrow().as_ref().map(|look| &look.element) == element {
            return;
        }
        let look = element.map(|element| {
            let class = unique_class("message-el");
            let Style { font_size, .. } = element.style;
            // A plain message has no background, so only the rest of the look applies to it.
            let css = format!(
                "{} .message-plain.{class} {{ font-size: {font_size}pt; }} {}",
                style_css(&format!(".message.{class}:not(.message-plain)"), &element.style),
                effects_css(&format!(".message.{class}:not(.message-plain)"), &format!(".message.{class} label"), &element.style),
            );
            Look { element: element.clone(), class, _sheet: Sheet::new(&css) }
        });
        self.inner.look.replace(look);
    }

    /// Moves the messages to `overlay`, such as the viewer's frame, so they stay within it and clear of what is beside it.
    pub fn move_to(&self, overlay: &gtk::Overlay) {
        if *self.inner.host.borrow() == *overlay {
            return;
        }
        let old = self.inner.host.replace(overlay.clone());
        for column in self.inner.columns.values() {
            old.remove_overlay(column);
            overlay.add_overlay(column);
        }
    }

    /// Keeps each place's messages as far from the edges as the layout says.
    pub fn set_margins(&self, layout: &Layout) {
        for (slot, column) in &self.inner.columns {
            set_edge_margins(column, *slot, layout.margin(*slot));
        }
    }

    /// Sets the most messages a stack shows at once, and whether one that comes to a full stack pushes the oldest out or
    /// waits for room. Messages already up stay.
    pub fn set_limit(&self, limit: usize, overflow: Overflow) {
        self.inner.limit.set(limit.max(1));
        self.inner.overflow.set(overflow);
        // A larger limit makes room for those that were waiting.
        let slots: Vec<Slot> = self.inner.queue.borrow().iter().map(|(_, slot, _)| *slot).collect();
        for slot in slots {
            self.fill_from_queue(slot);
        }
    }

    /// How many messages are up in the stack at `slot`.
    fn count_in(&self, slot: Slot) -> usize {
        self.inner.messages.borrow().values().filter(|shown| shown.slot == slot).count()
    }

    /// Brings up the messages waiting for the stack at `slot`, while it has room.
    fn fill_from_queue(&self, slot: Slot) {
        while self.count_in(slot) < self.inner.limit.get() {
            let next = {
                let mut queue = self.inner.queue.borrow_mut();
                queue.iter().position(|(_, queued, _)| *queued == slot).and_then(|index| queue.remove(index))
            };
            let Some((id, slot, message)) = next else { return };
            self.present(id, slot, message);
        }
    }

    fn downgrade(&self) -> Weak<Inner> {
        Rc::downgrade(&self.inner)
    }

    /// Shows `message`, and returns the id to change or dismiss it with.
    pub fn show(&self, message: Message) -> MessageId {
        let id = self.inner.next.get() + 1;
        self.inner.next.set(id);

        let slot = message.position.unwrap_or_else(|| self.inner.default_slot.get());
        // A full stack either gives up its oldest message, or the new one waits its turn.
        if self.count_in(slot) >= self.inner.limit.get() {
            match self.inner.overflow.get() {
                Overflow::Wait => {
                    self.inner.queue.borrow_mut().push_back((id, slot, message));
                    return id;
                }
                Overflow::PushOut => {
                    while self.count_in(slot) >= self.inner.limit.get() {
                        let oldest = self.inner.messages.borrow().iter().filter(|(_, shown)| shown.slot == slot).map(|(id, _)| *id).min();
                        match oldest {
                            Some(oldest) => self.dismiss(oldest),
                            None => break,
                        };
                    }
                }
            }
        }
        self.present(id, slot, message);
        id
    }

    /// Puts the message `id` up in the stack at `slot`.
    fn present(&self, id: MessageId, slot: Slot, message: Message) {
        let align = slot.text_align();
        let card = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        card.add_css_class("message");
        card.add_css_class(message.level.css_class());
        if let Some(look) = self.inner.look.borrow().as_ref() {
            card.add_css_class(&look.class);
        }
        let label = gtk::Label::new(Some(&message.text));
        label.set_wrap(true);
        label.set_max_width_chars(WRAP_WIDTH);
        align_label(&label, align);
        label.set_hexpand(true);
        card.append(&label);
        if message.dismissible {
            let close = gtk::Button::from_icon_name("window-close-symbolic");
            close.add_css_class("flat");
            close.add_css_class("circular");
            close.set_valign(gtk::Align::Center);
            close.set_tooltip_text(Some("Dismiss"));
            let weak = self.downgrade();
            close.connect_clicked(move |_| {
                if let Some(inner) = weak.upgrade() {
                    Self { inner }.dismiss(id);
                }
            });
            card.append(&close);
        }

        let revealer = gtk::Revealer::new();
        match self.inner.look.borrow().as_ref() {
            Some(look) => set_fade(&revealer, &look.element),
            None => {
                revealer.set_transition_type(gtk::RevealerTransitionType::Crossfade);
                revealer.set_transition_duration(FADE.as_millis() as u32);
            }
        }
        revealer.set_child(Some(&card));
        revealer.set_halign(horizontal_align(align));
        // A message that has faded out is taken off the window.
        revealer.connect_child_revealed_notify(|revealer| {
            if !revealer.is_child_revealed() && !revealer.reveals_child() {
                if let Some(column) = revealer.parent().and_downcast::<gtk::Box>() {
                    column.remove(revealer);
                }
            }
        });
        // The newest message is the nearest the edge of the window: at the top of a stack at the top, and at the
        // bottom of one below.
        if let Some(column) = self.inner.columns.get(&slot) {
            if slot.row == 0 {
                column.prepend(&revealer);
            } else {
                column.append(&revealer);
            }
        }
        let appearing = revealer.clone();
        glib::idle_add_local_once(move || appearing.set_reveal_child(true));

        let timer = match message.lifetime {
            Lifetime::Temporary(duration) => {
                let weak = self.downgrade();
                Some(glib::timeout_add_local_once(duration, move || {
                    let Some(inner) = weak.upgrade() else { return };
                    // The timer has fired, so there is nothing left to cancel.
                    if let Some(shown) = inner.messages.borrow_mut().get_mut(&id) {
                        shown.timer = None;
                    }
                    Self { inner }.dismiss(id);
                }))
            }
            Lifetime::Persistent => None,
        };
        self.inner.messages.borrow_mut().insert(id, Shown { slot, revealer, label, timer });
    }

    /// Changes the text of the message `id`, if it is still up or waiting. Returns whether it was.
    pub fn update(&self, id: MessageId, text: &str) -> bool {
        if let Some((_, _, message)) = self.inner.queue.borrow_mut().iter_mut().find(|(queued, _, _)| *queued == id) {
            message.text = text.to_owned();
            return true;
        }
        match self.inner.messages.borrow().get(&id) {
            Some(shown) => {
                shown.label.set_text(text);
                true
            }
            None => false,
        }
    }

    /// Takes the message `id` down, if it is still up or waiting. Returns whether it was.
    pub fn dismiss(&self, id: MessageId) -> bool {
        // A message still waiting is only taken out of the line.
        let waiting = self.inner.queue.borrow().iter().position(|(queued, _, _)| *queued == id);
        if let Some(index) = waiting {
            self.inner.queue.borrow_mut().remove(index);
            return true;
        }
        let Some(shown) = self.inner.messages.borrow_mut().remove(&id) else { return false };
        if let Some(timer) = shown.timer {
            timer.remove();
        }
        if shown.revealer.is_child_revealed() {
            shown.revealer.set_reveal_child(false);
        } else if let Some(column) = shown.revealer.parent().and_downcast::<gtk::Box>() {
            // It never finished appearing, so there is no fade to wait for.
            column.remove(&shown.revealer);
        }
        // The room it leaves goes to the message that has waited longest for it.
        self.fill_from_queue(shown.slot);
        true
    }

    /// Takes down every message.
    pub fn dismiss_all(&self) {
        self.inner.queue.borrow_mut().clear();
        let ids: Vec<MessageId> = self.inner.messages.borrow().keys().copied().collect();
        for id in ids {
            self.dismiss(id);
        }
    }

    /// How many messages are waiting for room in a full stack.
    pub fn queued_count(&self) -> usize {
        self.inner.queue.borrow().len()
    }

    /// How many messages are up.
    pub fn message_count(&self) -> usize {
        self.inner.messages.borrow().len()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::overlay::Overflow;

    fn cards_at(overlay: &MessageOverlay, position: Position) -> usize {
        let column = overlay.inner.columns.get(&position.slot()).expect("a column for each position");
        std::iter::successors(column.first_child(), |child| child.next_sibling()).count()
    }

    pub(crate) fn run_scenarios() {
        let layer = gtk::Overlay::new();
        let overlay = MessageOverlay::new(&layer);

        let note = overlay.show(Message::notification("Saved"));
        let pinned = overlay.show(Message::persistent("Indexing").at(Position::TopRight).level(Level::Warning));
        assert_ne!(note, pinned, "each message has its own id");
        assert_eq!(overlay.message_count(), 2);
        assert_eq!((cards_at(&overlay, Position::BottomCenter), cards_at(&overlay, Position::TopRight)), (1, 1), "each in its place");

        // A persistent message's text can change while it is up; one that is gone cannot.
        assert!(overlay.update(pinned, "Indexing 40%"));
        assert!(!overlay.update(pinned + 100, "nothing"));

        // Messages in one place stack.
        let second = overlay.show(Message::notification("Copied"));
        assert_eq!(cards_at(&overlay, Position::BottomCenter), 2);

        assert!(overlay.dismiss(pinned));
        assert!(!overlay.dismiss(pinned), "a message is dismissed once");
        assert_eq!(cards_at(&overlay, Position::TopRight), 0, "one that never finished appearing goes at once");
        overlay.dismiss_all();
        assert_eq!(overlay.message_count(), 0);
        assert!(!overlay.update(second, "late"));
        assert_eq!(cards_at(&overlay, Position::BottomCenter), 0);

        // A stack holds as many as the limit says. A new message pushes the oldest out, or waits for room.
        let layer = gtk::Overlay::new();
        let overlay = MessageOverlay::new(&layer);
        overlay.set_limit(2, Overflow::PushOut);
        let (first, second, third) = (overlay.show(Message::persistent("1")), overlay.show(Message::persistent("2")), overlay.show(Message::persistent("3")));
        assert_eq!(overlay.message_count(), 2, "the oldest was pushed out");
        assert!(!overlay.dismiss(first) && overlay.update(second, "2b") && overlay.update(third, "3b"));
        overlay.dismiss_all();

        overlay.set_limit(1, Overflow::Wait);
        let (up, waiting) = (overlay.show(Message::persistent("up")), overlay.show(Message::persistent("waiting")));
        assert_eq!((overlay.message_count(), overlay.queued_count()), (1, 1));
        assert!(overlay.update(waiting, "still waiting"), "a waiting message can be changed");
        assert!(overlay.dismiss(up));
        assert_eq!((overlay.message_count(), overlay.queued_count()), (1, 0), "room went to the one that waited");
        assert!(overlay.update(waiting, "now up"));
        // Another place has its own stack.
        overlay.show(Message::persistent("elsewhere").at(Position::TopRight));
        assert_eq!(overlay.message_count(), 2);
        overlay.dismiss_all();

        assert_eq!(Message::notification("a").lifetime, Lifetime::Temporary(DEFAULT_DURATION));
        assert!(Message::persistent("a").dismissible && !Message::notification("a").dismissible);
        assert_eq!(Level::from_name("error"), Some(Level::Error));
        assert_eq!(Position::from_name("top-left"), Some(Position::TopLeft));
        assert_eq!(Position::from_name("middle"), None);
    }
}
