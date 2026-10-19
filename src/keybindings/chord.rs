//! Key chords: a key and its modifiers, written as text such as `ctrl+shift+p`. Bindings are stored and
//! shown in this form. Pure: no GTK.

use std::fmt;

/// A key with its modifiers. `key` is the lowercase GDK key name, such as `p`, `space` or `page_down`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Chord {
    pub key: String,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub super_key: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ChordError(pub String);

impl fmt::Display for ChordError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} is not a valid key", self.0)
    }
}

impl Chord {
    /// Reads a chord such as `Ctrl+Q` or `shift+page_down`. Modifiers may come in any order and any case.
    pub fn parse(text: &str) -> Result<Self, ChordError> {
        let parts: Vec<&str> = text.split('+').map(str::trim).collect();
        let Some((key, modifiers)) = parts.split_last() else {
            return Err(ChordError(text.to_owned()));
        };
        if key.is_empty() {
            return Err(ChordError(text.to_owned()));
        }
        let mut chord = Chord { key: key.to_ascii_lowercase(), ctrl: false, alt: false, shift: false, super_key: false };
        for modifier in modifiers {
            match modifier.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => chord.ctrl = true,
                "alt" => chord.alt = true,
                "shift" => chord.shift = true,
                "super" | "meta" => chord.super_key = true,
                _ => return Err(ChordError(text.to_owned())),
            }
        }
        Ok(chord)
    }

    /// The canonical text: modifiers in the order `ctrl`, `alt`, `shift`, `super`, then the key.
    pub fn format(&self) -> String {
        let mut parts = Vec::new();
        if self.ctrl {
            parts.push("ctrl");
        }
        if self.alt {
            parts.push("alt");
        }
        if self.shift {
            parts.push("shift");
        }
        if self.super_key {
            parts.push("super");
        }
        parts.push(&self.key);
        parts.join("+")
    }
}

/// The canonical text of `text`, or `None` if it is not a chord.
pub fn canonical(text: &str) -> Option<String> {
    Chord::parse(text).ok().map(|chord| chord.format())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chords_round_trip_in_canonical_form() {
        assert_eq!(Chord::parse("Shift+Ctrl+P").unwrap().format(), "ctrl+shift+p");
        assert_eq!(Chord::parse("page_down").unwrap().format(), "page_down");
        assert_eq!(canonical("Alt+Left"), Some("alt+left".to_owned()));
    }

    #[test]
    fn bad_chords_are_refused() {
        assert!(Chord::parse("").is_err());
        assert!(Chord::parse("ctrl+").is_err());
        assert!(Chord::parse("hyper+q").is_err());
    }
}
