//! The live bindings: each catalogue action's chords, each with the value it passes if the action takes one,
//! which the editor changes and the window resolves key presses against. Pure: no GTK.

use serde_yaml_ng::{Mapping, Value};

use super::actions::{find, ActionInfo, Scope, ACTIONS};
use super::chord::canonical;

/// A chord bound to an action, and the value it passes, for an action that takes one; `None` uses the action's
/// default.
#[derive(Debug, Clone, PartialEq)]
pub struct Binding {
    pub chord: String,
    pub value: Option<f64>,
}

impl Binding {
    fn new(chord: String) -> Self {
        Self { chord, value: None }
    }
}

#[derive(Clone, PartialEq)]
pub struct BindingTable {
    /// The bindings of each action, in the catalogue's order.
    chords: Vec<Vec<Binding>>,
}

impl BindingTable {
    /// The table with every action on its default chords.
    pub fn defaults() -> Self {
        let chords = ACTIONS
            .iter()
            .map(|action| action.defaults.iter().map(|chord| Binding::new((*chord).to_owned())).collect())
            .collect();
        Self { chords }
    }

    fn index(name: &str) -> Option<usize> {
        ACTIONS.iter().position(|action| action.name == name)
    }

    /// The bindings of `name`, empty if there is no such action.
    pub fn bindings(&self, name: &str) -> &[Binding] {
        Self::index(name).map_or(&[], |index| self.chords[index].as_slice())
    }

    /// The chords bound to `name`, empty if there is no such action.
    #[cfg(test)]
    pub fn chords(&self, name: &str) -> Vec<String> {
        self.bindings(name).iter().map(|binding| binding.chord.clone()).collect()
    }

    /// Binds `chord` to `name`, if it is a valid chord not already bound to it. Returns whether it was added.
    #[cfg(test)]
    pub fn add(&mut self, name: &str, chord: &str) -> bool {
        self.add_with_value(name, chord, None)
    }

    /// Binds `chord` to `name` passing `value`, if it is a valid chord not already bound to it. Returns whether
    /// it was added.
    pub fn add_with_value(&mut self, name: &str, chord: &str, value: Option<f64>) -> bool {
        let (Some(index), Some(chord)) = (Self::index(name), canonical(chord)) else { return false };
        if self.chords[index].iter().any(|bound| bound.chord == chord) {
            return false;
        }
        self.chords[index].push(Binding { chord, value });
        true
    }

    /// Unbinds `chord` from `name`.
    pub fn remove(&mut self, name: &str, chord: &str) {
        if let (Some(index), Some(chord)) = (Self::index(name), canonical(chord)) {
            self.chords[index].retain(|bound| bound.chord != chord);
        }
    }

    /// Replaces the chords of `name`, keeping only the valid ones. Returns the ones that were refused.
    pub fn set(&mut self, name: &str, chords: &[&str]) -> Vec<String> {
        let Some(index) = Self::index(name) else { return Vec::new() };
        let mut refused = Vec::new();
        let mut bound: Vec<Binding> = Vec::new();
        for chord in chords {
            match canonical(chord) {
                Some(chord) if !bound.iter().any(|binding| binding.chord == chord) => bound.push(Binding::new(chord)),
                Some(_) => {}
                None => refused.push((*chord).to_owned()),
            }
        }
        self.chords[index] = bound;
        refused
    }

    pub fn reset_all(&mut self) {
        *self = Self::defaults();
    }

    /// The action `chord` runs in the active scopes, and the value it passes. Global applies everywhere; of the
    /// others, the deepest scope wins, and among equals the catalogue's first.
    pub fn resolve(&self, chord: &str, active: &[Scope]) -> Option<(&'static ActionInfo, Option<f64>)> {
        let mut best: Option<(&'static ActionInfo, Option<f64>)> = None;
        for (action, bindings) in ACTIONS.iter().zip(&self.chords) {
            let in_scope = action.scope == Scope::Global || active.contains(&action.scope);
            let Some(binding) = bindings.iter().find(|bound| bound.chord == chord) else { continue };
            if !in_scope {
                continue;
            }
            if best.is_none_or(|(current, _)| action.scope.depth() > current.scope.depth()) {
                best = Some((action, binding.value));
            }
        }
        best
    }

    /// The other actions whose scopes overlap `name`'s and that are bound to `chord`: what binding it to
    /// `name` would clash with.
    pub fn conflicts(&self, name: &str, chord: &str) -> Vec<&'static ActionInfo> {
        let Some(scope) = find(name).map(|action| action.scope) else { return Vec::new() };
        ACTIONS
            .iter()
            .zip(&self.chords)
            .filter(|(action, bindings)| {
                action.name != name && action.scope.overlaps(scope) && bindings.iter().any(|bound| bound.chord == chord)
            })
            .map(|(action, _)| action)
            .collect()
    }

    /// The YAML file text for the bindings that differ from the defaults. A binding with a value is a mapping of
    /// `key` and `value`; one without is just its chord.
    pub fn to_yaml(&self) -> String {
        let mut actions = Mapping::new();
        for (action, bindings) in ACTIONS.iter().zip(&self.chords) {
            let defaults = action.defaults.iter().map(|chord| Binding::new((*chord).to_owned()));
            if bindings.iter().cloned().eq(defaults) {
                continue;
            }
            let sequence = bindings
                .iter()
                .map(|binding| match binding.value {
                    None => Value::String(binding.chord.clone()),
                    Some(value) => {
                        let mut entry = Mapping::new();
                        entry.insert(Value::String("key".to_owned()), Value::String(binding.chord.clone()));
                        entry.insert(Value::String("value".to_owned()), Value::Number(value.into()));
                        Value::Mapping(entry)
                    }
                })
                .collect();
            actions.insert(Value::String(action.name.to_owned()), Value::Sequence(sequence));
        }
        let mut root = Mapping::new();
        root.insert(Value::String("schema".to_owned()), Value::Number(1.into()));
        root.insert(Value::String("actions".to_owned()), Value::Mapping(actions));
        serde_yaml_ng::to_string(&Value::Mapping(root)).unwrap_or_default()
    }

    /// Applies the actions in a YAML file over the table. Returns what could not be applied: unknown
    /// actions, invalid chords, or a file that cannot be read.
    pub fn apply_yaml(&mut self, text: &str) -> Vec<String> {
        let root: Value = match serde_yaml_ng::from_str(text) {
            Ok(root) => root,
            Err(err) => return vec![format!("the file could not be read: {err}")],
        };
        let Some(actions) = root.get("actions").and_then(Value::as_mapping) else { return Vec::new() };
        let mut problems = Vec::new();
        for (key, value) in actions {
            let Some(name) = key.as_str() else { continue };
            if find(name).is_none() {
                problems.push(format!("unknown action {name}"));
                continue;
            }
            // Each entry is a chord, or a mapping of a chord (`key`) and the value it passes (`value`).
            let entries: Vec<(&str, Option<f64>)> = value
                .as_sequence()
                .into_iter()
                .flatten()
                .filter_map(|entry| match entry {
                    Value::String(chord) => Some((chord.as_str(), None)),
                    Value::Mapping(_) => Some((entry.get("key")?.as_str()?, entry.get("value").and_then(Value::as_f64))),
                    _ => None,
                })
                .collect();
            let chords: Vec<&str> = entries.iter().map(|(chord, _)| *chord).collect();
            problems.extend(self.set(name, &chords).into_iter().map(|chord| format!("{name}: {chord} is not a valid key")));
            if let Some(index) = Self::index(name) {
                for binding in &mut self.chords[index] {
                    binding.value = entries
                        .iter()
                        .find(|(chord, _)| canonical(chord).as_deref() == Some(binding.chord.as_str()))
                        .and_then(|(_, value)| *value);
                }
            }
        }
        problems
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_deepest_scope_wins_a_chord() {
        let table = BindingTable::defaults();
        // Space is only bound in the video scope.
        assert_eq!(table.resolve("space", &[Scope::Global, Scope::Browse]).map(|(a, _)| a.name), None);
        assert_eq!(table.resolve("space", &[Scope::Global, Scope::View, Scope::Video]).map(|(a, _)| a.name), Some("video.play-pause"));
        // Escape is the viewer's, and only applies while the viewer is open.
        assert_eq!(table.resolve("escape", &[Scope::Global, Scope::Browse]).map(|(a, _)| a.name), None);
        assert_eq!(table.resolve("escape", &[Scope::Global, Scope::View, Scope::Image]).map(|(a, _)| a.name), Some("view.close"));
        // Settings is global, so it applies in any scope.
        assert_eq!(table.resolve("ctrl+comma", &[]).map(|(a, _)| a.name), Some("win.settings"));
    }

    #[test]
    fn a_rebound_chord_clashes_only_with_overlapping_scopes() {
        let mut table = BindingTable::defaults();
        // Binding space to a viewer action clashes with the video action, which is inside the viewer.
        assert!(table.add("view.next", "space"));
        assert_eq!(table.conflicts("view.next", "space").iter().map(|(a, _)| a.name).collect::<Vec<_>>(), vec!["video.play-pause"]);
        // Browsing and the viewer never overlap, so the viewer's left is not a conflict with browsing's.
        assert!(table.conflicts("view.previous", "left").iter().all(|action| action.scope != Scope::Browse));
    }

    #[test]
    fn arrow_and_page_keys_move_the_browse_cursor() {
        let table = BindingTable::defaults();
        let browsing = [Scope::Browse];
        assert_eq!(table.resolve("up", &browsing).map(|(a, _)| a.name), Some("browse.cursor-up"));
        assert_eq!(table.resolve("page_down", &browsing).map(|(a, _)| a.name), Some("browse.cursor-page-down"));
        // In the viewer the same keys seek or change volume, and the comma and period keys step between items.
        assert_eq!(table.resolve("period", &[Scope::View, Scope::Image]).map(|(a, _)| a.name), Some("view.next"));
        assert_eq!(table.resolve("right", &[Scope::View, Scope::Video]).map(|(a, _)| a.name), Some("video.seek-forward"));
    }

    #[test]
    fn overrides_round_trip_through_the_yaml_file() {
        let mut table = BindingTable::defaults();
        table.set("view.next", &["d", "Ctrl+Right"]);
        let text = table.to_yaml();
        assert!(text.contains("view.next"));
        assert!(!text.contains("view.previous"), "defaults are not written");

        let mut loaded = BindingTable::defaults();
        assert!(loaded.apply_yaml(&text).is_empty());
        assert_eq!(loaded.chords("view.next"), ["d", "ctrl+right"]);
    }

    #[test]
    fn values_round_trip_through_the_yaml_file() {
        let mut table = BindingTable::defaults();
        assert!(table.add_with_value("video.seek", "ctrl+right", Some(30.0)));
        let text = table.to_yaml();
        let mut loaded = BindingTable::defaults();
        assert!(loaded.apply_yaml(&text).is_empty());
        assert_eq!(loaded.bindings("video.seek"), [Binding { chord: "ctrl+right".to_owned(), value: Some(30.0) }]);
        assert_eq!(loaded.resolve("ctrl+right", &[Scope::View, Scope::Video]).map(|(a, value)| (a.name, value)), Some(("video.seek", Some(30.0))));
    }

    #[test]
    fn bad_entries_are_reported_and_skipped() {
        let mut table = BindingTable::defaults();
        let problems = table.apply_yaml("schema: 1\nactions:\n  no.such-action:\n    - q\n  view.next:\n    - hyper+q\n    - d\n");
        assert_eq!(problems.len(), 2);
        assert_eq!(table.chords("view.next"), ["d"]);
    }
}
