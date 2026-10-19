//! The live bindings: each catalogue action's chords, which the editor changes and the window resolves
//! key presses against. Pure: no GTK.

use serde_yaml_ng::{Mapping, Value};

use super::actions::{find, ActionInfo, Scope, ACTIONS};
use super::chord::canonical;

pub struct BindingTable {
    /// The chords of each action, in the catalogue's order.
    chords: Vec<Vec<String>>,
}

impl BindingTable {
    /// The table with every action on its default chords.
    pub fn defaults() -> Self {
        let chords = ACTIONS.iter().map(|action| action.defaults.iter().map(|chord| (*chord).to_owned()).collect()).collect();
        Self { chords }
    }

    fn index(name: &str) -> Option<usize> {
        ACTIONS.iter().position(|action| action.name == name)
    }

    /// The chords bound to `name`, empty if there is no such action.
    pub fn chords(&self, name: &str) -> &[String] {
        Self::index(name).map_or(&[], |index| self.chords[index].as_slice())
    }

    /// Binds `chord` to `name`, if it is a valid chord not already bound to it. Returns whether it was added.
    pub fn add(&mut self, name: &str, chord: &str) -> bool {
        let (Some(index), Some(chord)) = (Self::index(name), canonical(chord)) else { return false };
        if self.chords[index].contains(&chord) {
            return false;
        }
        self.chords[index].push(chord);
        true
    }

    /// Unbinds `chord` from `name`.
    pub fn remove(&mut self, name: &str, chord: &str) {
        if let (Some(index), Some(chord)) = (Self::index(name), canonical(chord)) {
            self.chords[index].retain(|bound| *bound != chord);
        }
    }

    /// Replaces the chords of `name`, keeping only the valid ones. Returns the ones that were refused.
    pub fn set(&mut self, name: &str, chords: &[&str]) -> Vec<String> {
        let Some(index) = Self::index(name) else { return Vec::new() };
        let mut refused = Vec::new();
        let mut bound: Vec<String> = Vec::new();
        for chord in chords {
            match canonical(chord) {
                Some(chord) if !bound.contains(&chord) => bound.push(chord),
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

    /// The action `chord` runs in the active scopes. Global applies everywhere; of the others, the deepest
    /// scope wins, and among equals the catalogue's first.
    pub fn resolve(&self, chord: &str, active: &[Scope]) -> Option<&'static ActionInfo> {
        let mut best: Option<&'static ActionInfo> = None;
        for (action, chords) in ACTIONS.iter().zip(&self.chords) {
            let in_scope = action.scope == Scope::Global || active.contains(&action.scope);
            if !in_scope || !chords.iter().any(|bound| bound == chord) {
                continue;
            }
            if best.is_none_or(|current| action.scope.depth() > current.scope.depth()) {
                best = Some(action);
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
            .filter(|(action, chords)| action.name != name && action.scope.overlaps(scope) && chords.iter().any(|bound| bound == chord))
            .map(|(action, _)| action)
            .collect()
    }

    /// The YAML file text for the chords that differ from the defaults.
    pub fn to_yaml(&self) -> String {
        let mut actions = Mapping::new();
        for (action, chords) in ACTIONS.iter().zip(&self.chords) {
            let defaults: Vec<&str> = action.defaults.to_vec();
            if chords.iter().map(String::as_str).eq(defaults.iter().copied()) {
                continue;
            }
            let sequence = chords.iter().map(|chord| Value::String(chord.clone())).collect();
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
            let chords: Vec<&str> = value.as_sequence().into_iter().flatten().filter_map(Value::as_str).collect();
            problems.extend(self.set(name, &chords).into_iter().map(|chord| format!("{name}: {chord} is not a valid key")));
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
        assert_eq!(table.resolve("space", &[Scope::Global, Scope::Browse]).map(|a| a.name), None);
        assert_eq!(table.resolve("space", &[Scope::Global, Scope::View, Scope::Video]).map(|a| a.name), Some("video.play-pause"));
        // Escape is the viewer's, and only applies while the viewer is open.
        assert_eq!(table.resolve("escape", &[Scope::Global, Scope::Browse]).map(|a| a.name), None);
        assert_eq!(table.resolve("escape", &[Scope::Global, Scope::View, Scope::Image]).map(|a| a.name), Some("view.close"));
        // Settings is global, so it applies in any scope.
        assert_eq!(table.resolve("ctrl+comma", &[]).map(|a| a.name), Some("win.settings"));
    }

    #[test]
    fn a_rebound_chord_clashes_only_with_overlapping_scopes() {
        let mut table = BindingTable::defaults();
        // Binding space to a viewer action clashes with the video action, which is inside the viewer.
        assert!(table.add("view.next", "space"));
        assert_eq!(table.conflicts("view.next", "space").iter().map(|a| a.name).collect::<Vec<_>>(), vec!["video.play-pause"]);
        // Browsing and the viewer never overlap, so the viewer's left is not a conflict with browsing's.
        assert!(table.conflicts("view.previous", "left").iter().all(|action| action.scope != Scope::Browse));
    }

    #[test]
    fn arrow_and_page_keys_move_the_browse_cursor() {
        let table = BindingTable::defaults();
        let browsing = [Scope::Browse];
        assert_eq!(table.resolve("up", &browsing).map(|a| a.name), Some("browse.cursor-up"));
        assert_eq!(table.resolve("page_down", &browsing).map(|a| a.name), Some("browse.cursor-page-down"));
        // In the viewer the same keys seek or change volume, and the comma and period keys step between items.
        assert_eq!(table.resolve("period", &[Scope::View, Scope::Image]).map(|a| a.name), Some("view.next"));
        assert_eq!(table.resolve("right", &[Scope::View, Scope::Video]).map(|a| a.name), Some("video.seek-forward"));
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
    fn bad_entries_are_reported_and_skipped() {
        let mut table = BindingTable::defaults();
        let problems = table.apply_yaml("schema: 1\nactions:\n  no.such-action:\n    - q\n  view.next:\n    - hyper+q\n    - d\n");
        assert_eq!(problems.len(), 2);
        assert_eq!(table.chords("view.next"), ["d"]);
    }
}
