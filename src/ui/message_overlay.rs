//! Messages over the window's content. A message is a card of text placed at one of six places along the window's
//! edges, and is either temporary, going away by itself after a while, or persistent, staying until it is dismissed
//! by the code that showed it or by its close button. A persistent message can have its text changed while it is up.
//!
//! The messages are added to a `gtk::Overlay` as a column for each place. A column is only as big as its messages,
//! so the rest of the window under the overlay still takes the clicks.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;

/// How long a temporary message stays up unless it says otherwise.
pub const DEFAULT_DURATION: Duration = Duration::from_secs(4);
/// How long a message takes to fade in and out.
const FADE: Duration = Duration::from_millis(200);
/// The space between the edge of the window and the messages, and between one message and the next, in pixels.
const MARGIN: i32 = 12;
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

    fn alignment(self) -> (gtk::Align, gtk::Align) {
        let horizontal = match self {
            Self::TopLeft | Self::BottomLeft => gtk::Align::Start,
            Self::TopCenter | Self::BottomCenter => gtk::Align::Center,
            Self::TopRight | Self::BottomRight => gtk::Align::End,
        };
        let vertical = match self {
            Self::TopLeft | Self::TopCenter | Self::TopRight => gtk::Align::Start,
            _ => gtk::Align::End,
        };
        (horizontal, vertical)
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
    pub position: Position,
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
            position: Position::BottomCenter,
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
        self.position = position;
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
    revealer: gtk::Revealer,
    label: gtk::Label,
    /// The timer that dismisses a temporary message.
    timer: Option<glib::SourceId>,
}

struct Inner {
    /// The column of messages at each position.
    columns: HashMap<Position, gtk::Box>,
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
        let columns = Position::ALL
            .into_iter()
            .map(|position| {
                let column = gtk::Box::new(gtk::Orientation::Vertical, SPACING);
                let (horizontal, vertical) = position.alignment();
                column.set_halign(horizontal);
                column.set_valign(vertical);
                column.set_margin_start(MARGIN);
                column.set_margin_end(MARGIN);
                column.set_margin_top(MARGIN);
                column.set_margin_bottom(MARGIN);
                overlay.add_overlay(&column);
                (position, column)
            })
            .collect();
        Self { inner: Rc::new(Inner { columns, messages: RefCell::new(HashMap::new()), next: Cell::new(0) }) }
    }

    fn downgrade(&self) -> Weak<Inner> {
        Rc::downgrade(&self.inner)
    }

    /// Shows `message`, and returns the id to change or dismiss it with.
    pub fn show(&self, message: Message) -> MessageId {
        let id = self.inner.next.get() + 1;
        self.inner.next.set(id);

        let card = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        card.add_css_class("message");
        card.add_css_class(message.level.css_class());
        let label = gtk::Label::new(Some(&message.text));
        label.set_wrap(true);
        label.set_max_width_chars(WRAP_WIDTH);
        label.set_xalign(0.0);
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
        revealer.set_transition_type(gtk::RevealerTransitionType::Crossfade);
        revealer.set_transition_duration(FADE.as_millis() as u32);
        revealer.set_child(Some(&card));
        // A message that has faded out is taken off the window.
        revealer.connect_child_revealed_notify(|revealer| {
            if !revealer.is_child_revealed() && !revealer.reveals_child() {
                if let Some(column) = revealer.parent().and_downcast::<gtk::Box>() {
                    column.remove(revealer);
                }
            }
        });
        if let Some(column) = self.inner.columns.get(&message.position) {
            column.append(&revealer);
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
        self.inner.messages.borrow_mut().insert(id, Shown { revealer, label, timer });
        id
    }

    /// Changes the text of the message `id`, if it is still up. Returns whether it was.
    pub fn update(&self, id: MessageId, text: &str) -> bool {
        match self.inner.messages.borrow().get(&id) {
            Some(shown) => {
                shown.label.set_text(text);
                true
            }
            None => false,
        }
    }

    /// Takes the message `id` down, if it is still up. Returns whether it was.
    pub fn dismiss(&self, id: MessageId) -> bool {
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
        true
    }

    /// Takes down every message.
    pub fn dismiss_all(&self) {
        let ids: Vec<MessageId> = self.inner.messages.borrow().keys().copied().collect();
        for id in ids {
            self.dismiss(id);
        }
    }

    /// How many messages are up.
    pub fn message_count(&self) -> usize {
        self.inner.messages.borrow().len()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn cards_at(overlay: &MessageOverlay, position: Position) -> usize {
        let column = overlay.inner.columns.get(&position).expect("a column for each position");
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

        assert_eq!(Message::notification("a").lifetime, Lifetime::Temporary(DEFAULT_DURATION));
        assert!(Message::persistent("a").dismissible && !Message::notification("a").dismissible);
        assert_eq!(Level::from_name("error"), Some(Level::Error));
        assert_eq!(Position::from_name("top-left"), Some(Position::TopLeft));
        assert_eq!(Position::from_name("middle"), None);
    }
}
