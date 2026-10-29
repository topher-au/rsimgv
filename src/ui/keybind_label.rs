//! A keybinding's chord, drawn as its keys: each key in a label of its own, and modifiers marked as such. It is a
//! box with the CSS name `keybind`; its keys are `label.key`, and the modifiers among them also `.modifier`. The
//! look is in the stylesheet.

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::glib;

use crate::keybindings::chord::Chord;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct KeybindLabel;

    #[glib::object_subclass]
    impl ObjectSubclass for KeybindLabel {
        const NAME: &'static str = "RsimgvKeybindLabel";
        type Type = super::KeybindLabel;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("keybind");
        }
    }

    impl ObjectImpl for KeybindLabel {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.set_orientation(gtk::Orientation::Horizontal);
            obj.set_valign(gtk::Align::Center);
        }
    }

    impl WidgetImpl for KeybindLabel {}
    impl BoxImpl for KeybindLabel {}
}

glib::wrapper! {
    pub struct KeybindLabel(ObjectSubclass<imp::KeybindLabel>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl KeybindLabel {
    /// The keys of `chord`, such as `ctrl+shift+v`, each in its own label. Text that is not a chord is one key.
    pub fn new(chord: &str) -> Self {
        let label: Self = glib::Object::new();
        match Chord::parse(chord) {
            Ok(parsed) => {
                for (name, modifier) in key_names(&parsed) {
                    label.append(&key(&name, modifier));
                }
            }
            Err(_) => label.append(&key(&chord.to_uppercase(), false)),
        }
        label
    }
}

/// One key's label: its name in capitals, with the `modifier` class if it is one.
fn key(name: &str, modifier: bool) -> gtk::Label {
    let key = gtk::Label::new(Some(name));
    key.set_justify(gtk::Justification::Center);
    key.add_css_class("key");
    if modifier {
        key.add_css_class("modifier");
    }
    key
}

/// The names of a chord's modifiers and key, in the order they are written, each with whether it is a modifier:
/// `CTRL`, `ALT`, `SHIFT`, `META`, then the key.
fn key_names(chord: &Chord) -> Vec<(String, bool)> {
    let modifiers = [(chord.ctrl, "CTRL"), (chord.alt, "ALT"), (chord.shift, "SHIFT"), (chord.super_key, "META")];
    let mut names: Vec<(String, bool)> =
        modifiers.into_iter().filter(|(on, _)| *on).map(|(_, name)| (name.to_owned(), true)).collect();
    names.push((key_name(&chord.key), false));
    names
}

/// The name a key is drawn with: its symbol where it has one, otherwise its name in capitals.
fn key_name(key: &str) -> String {
    match key {
        "comma" => ",".to_owned(),
        "period" => ".".to_owned(),
        "equal" => "=".to_owned(),
        "minus" => "-".to_owned(),
        "return" => "ENTER".to_owned(),
        "escape" => "ESC".to_owned(),
        _ => key.replace('_', " ").to_uppercase(),
    }
}
