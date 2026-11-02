//! The live bindings: each catalogue action's chords, each with the context it is active in and the value it
//! passes if the action takes one, which the editor changes and the window resolves key presses against. Pure:
//! no GTK.

use serde_yaml_ng::{Mapping, Value};

use super::actions::{ActionInfo, Scope, ACTIONS, SCRIPT_ACTION};
use super::chord::canonical;

/// A chord bound to an action, the context it is active in, the value it passes, for an action that takes one
/// (`None` uses the action's default), and the script it runs, for the action that runs one.
#[derive(Debug, Clone, PartialEq)]
pub struct Binding {
    pub chord: String,
    pub context: Scope,
    pub value: Option<f64>,
    pub script: Option<String>,
}

impl Binding {
    fn new(chord: String, context: Scope) -> Self {
        Self { chord, context, value: None, script: None }
    }
}

#[derive(Clone, PartialEq)]
pub struct BindingTable {
    /// The bindings of each action, in the catalogue's order.
    chords: Vec<Vec<Binding>>,
}

impl BindingTable {
    /// The table with every action on its default chords, each in the action's own context.
    pub fn defaults() -> Self {
        let chords = ACTIONS
            .iter()
            .map(|action| action.defaults.iter().map(|chord| Binding::new((*chord).to_owned(), action.default_context())).collect())
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

    /// Binds `chord` to `name` in `context`, passing `value`, if the action can be bound in that context, the chord
    /// is valid, and the chord is not already bound to the action. Returns whether it was added.
    pub fn add(&mut self, name: &str, chord: &str, context: Scope, value: Option<f64>) -> bool {
        self.add_binding(name, chord, context, value, None)
    }

    /// Binds `chord` to the action that runs scripts, in `context`, to run the script called `script`. Returns whether
    /// it was added, as `add` does.
    pub fn add_script(&mut self, chord: &str, context: Scope, script: &str) -> bool {
        self.add_binding(SCRIPT_ACTION, chord, context, None, Some(script.to_owned()))
    }

    fn add_binding(&mut self, name: &str, chord: &str, context: Scope, value: Option<f64>, script: Option<String>) -> bool {
        let (Some(index), Some(chord)) = (Self::index(name), canonical(chord)) else { return false };
        if !ACTIONS[index].contexts.contains(&context) || self.chords[index].iter().any(|bound| bound.chord == chord) {
            return false;
        }
        self.chords[index].push(Binding { chord, context, value, script });
        true
    }

    /// Unbinds `chord` from `name`.
    pub fn remove(&mut self, name: &str, chord: &str) {
        if let (Some(index), Some(chord)) = (Self::index(name), canonical(chord)) {
            self.chords[index].retain(|bound| bound.chord != chord);
        }
    }

    pub fn reset_all(&mut self) {
        *self = Self::defaults();
    }

    /// The action `chord` runs in the active contexts, and the value it passes. Global applies everywhere; of the
    /// others, the deepest context wins, and among equals the catalogue's first.
    pub fn resolve(&self, chord: &str, active: &[Scope]) -> Option<(&'static ActionInfo, Option<f64>)> {
        self.resolve_binding(chord, active).map(|(action, binding)| (action, binding.value))
    }

    /// The action `chord` runs, as `resolve` finds it, with the binding that runs it.
    pub fn resolve_binding(&self, chord: &str, active: &[Scope]) -> Option<(&'static ActionInfo, &Binding)> {
        let mut best: Option<(&'static ActionInfo, &Binding)> = None;
        for (action, bindings) in ACTIONS.iter().zip(&self.chords) {
            for binding in bindings.iter().filter(|bound| bound.chord == chord) {
                let context = binding.context;
                if context != Scope::Global && !active.contains(&context) {
                    continue;
                }
                if best.is_none_or(|(_, current)| context.depth() > current.context.depth()) {
                    best = Some((action, binding));
                }
            }
        }
        best
    }

    /// The other actions with a binding to `chord` whose context overlaps `context`, each with the context of that
    /// binding: what binding it to `name` in `context` would clash with.
    pub fn conflicts(&self, name: &str, chord: &str, context: Scope) -> Vec<(&'static ActionInfo, Scope)> {
        ACTIONS
            .iter()
            .zip(&self.chords)
            .filter_map(|(action, bindings)| {
                if action.name == name {
                    return None;
                }
                bindings
                    .iter()
                    .find(|bound| bound.chord == chord && bound.context.overlaps(context))
                    .map(|bound| (action, bound.context))
            })
            .collect()
    }

    /// The YAML file text for the bindings that differ from the defaults. A binding with a value, a script or a
    /// context other than the action's own is a mapping of `key`, and `value`, `script` and `context` when it has
    /// them; any other is just its chord.
    pub fn to_yaml(&self) -> String {
        let mut actions = Mapping::new();
        for (action, bindings) in ACTIONS.iter().zip(&self.chords) {
            let defaults = action.defaults.iter().map(|chord| Binding::new((*chord).to_owned(), action.default_context()));
            if bindings.iter().cloned().eq(defaults) {
                continue;
            }
            let sequence = bindings
                .iter()
                .map(|binding| {
                    let own_context = binding.context == action.default_context();
                    if binding.value.is_none() && binding.script.is_none() && own_context {
                        return Value::String(binding.chord.clone());
                    }
                    let mut entry = Mapping::new();
                    entry.insert(Value::String("key".to_owned()), Value::String(binding.chord.clone()));
                    if let Some(value) = binding.value {
                        entry.insert(Value::String("value".to_owned()), Value::Number(value.into()));
                    }
                    if let Some(script) = &binding.script {
                        entry.insert(Value::String("script".to_owned()), Value::String(script.clone()));
                    }
                    if !own_context {
                        entry.insert(Value::String("context".to_owned()), Value::String(binding.context.id().to_owned()));
                    }
                    Value::Mapping(entry)
                })
                .collect();
            actions.insert(Value::String(action.name.to_owned()), Value::Sequence(sequence));
        }
        let mut root = Mapping::new();
        root.insert(Value::String("schema".to_owned()), Value::Number(1.into()));
        root.insert(Value::String("actions".to_owned()), Value::Mapping(actions));
        serde_yaml_ng::to_string(&Value::Mapping(root)).unwrap_or_default()
    }

    /// Applies the actions in a YAML file over the table: each listed action gets the bindings the file gives it.
    /// Returns what could not be applied: unknown actions, invalid chords, unknown contexts, contexts the action
    /// cannot be bound in, a script binding that names no script, or a file that cannot be read.
    pub fn apply_yaml(&mut self, text: &str) -> Vec<String> {
        let root: Value = match serde_yaml_ng::from_str(text) {
            Ok(root) => root,
            Err(err) => return vec![format!("the file could not be read: {err}")],
        };
        let Some(actions) = root.get("actions").and_then(Value::as_mapping) else { return Vec::new() };
        let mut problems = Vec::new();
        for (key, value) in actions {
            let Some(name) = key.as_str() else { continue };
            let Some(index) = Self::index(name) else {
                problems.push(format!("unknown action {name}"));
                continue;
            };
            let action = &ACTIONS[index];
            let mut bindings: Vec<Binding> = Vec::new();
            // Each entry is a chord, or a mapping of a chord (`key`), the value it passes (`value`) and the context
            // it is active in (`context`), which is the action's own when it is left out.
            for entry in value.as_sequence().into_iter().flatten() {
                let (chord, context, passed, script) = match entry {
                    Value::String(chord) => (chord.as_str(), action.default_context(), None, None),
                    Value::Mapping(_) => {
                        let Some(chord) = entry.get("key").and_then(Value::as_str) else { continue };
                        let context = match entry.get("context") {
                            None => action.default_context(),
                            Some(context) => match context.as_str().and_then(Scope::from_id) {
                                Some(context) => context,
                                None => {
                                    problems.push(format!("{name}: {chord} has an unknown context"));
                                    continue;
                                }
                            },
                        };
                        let script = entry.get("script").and_then(Value::as_str).map(str::to_owned);
                        (chord, context, entry.get("value").and_then(Value::as_f64), script)
                    }
                    _ => continue,
                };
                let Some(chord) = canonical(chord) else {
                    problems.push(format!("{name}: {chord} is not a valid key"));
                    continue;
                };
                if !action.contexts.contains(&context) {
                    problems.push(format!("{name}: {chord} cannot be bound in {}", context.title()));
                    continue;
                }
                if action.takes_script() && script.is_none() {
                    problems.push(format!("{name}: {chord} names no script"));
                    continue;
                }
                if bindings.iter().any(|bound| bound.chord == chord) {
                    continue;
                }
                bindings.push(Binding { chord, context, value: passed, script: script.filter(|_| action.takes_script()) });
            }
            self.chords[index] = bindings;
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
    fn a_rebound_chord_clashes_only_with_overlapping_contexts() {
        let mut table = BindingTable::defaults();
        // Binding space to a viewer action clashes with the video action, which is inside the viewer.
        assert!(table.add("view.next", "space", Scope::View, None));
        assert_eq!(table.conflicts("view.next", "space", Scope::View).iter().map(|(a, _)| a.name).collect::<Vec<_>>(), vec!["video.play-pause"]);
        // Browsing and the viewer never overlap, so the viewer's left is not a conflict with browsing's.
        assert!(table.conflicts("view.previous", "left", Scope::View).iter().all(|(action, _)| !action.name.starts_with("browse.")));
    }

    #[test]
    fn a_global_binding_is_active_in_every_context() {
        let mut table = BindingTable::defaults();
        // Binding the viewer's next action globally makes it run while browsing too.
        assert!(table.add("view.next", "ctrl+n", Scope::Global, None));
        assert_eq!(table.resolve("ctrl+n", &[Scope::Global, Scope::Browse]).map(|(a, _)| a.name), Some("view.next"));
        // A binding can only be made in one of its action's contexts.
        assert!(!table.add("view.next", "ctrl+m", Scope::Browse, None));
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
        table.remove("view.next", "period");
        assert!(table.add("view.next", "d", Scope::View, None));
        assert!(table.add("view.next", "ctrl+right", Scope::View, None));
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
        assert!(table.add("video.seek", "ctrl+right", Scope::Video, Some(30.0)));
        let text = table.to_yaml();
        let mut loaded = BindingTable::defaults();
        assert!(loaded.apply_yaml(&text).is_empty());
        assert_eq!(
            loaded.bindings("video.seek"),
            [Binding { chord: "ctrl+right".to_owned(), context: Scope::Video, value: Some(30.0), script: None }]
        );
        assert_eq!(loaded.resolve("ctrl+right", &[Scope::View, Scope::Video]).map(|(a, value)| (a.name, value)), Some(("video.seek", Some(30.0))));
    }

    #[test]
    fn contexts_round_trip_through_the_yaml_file() {
        let mut table = BindingTable::defaults();
        assert!(table.add("view.next", "ctrl+n", Scope::Global, None));
        let text = table.to_yaml();
        assert!(text.contains("context: global"));
        let mut loaded = BindingTable::defaults();
        assert!(loaded.apply_yaml(&text).is_empty());
        assert_eq!(loaded.bindings("view.next").last(), Some(&Binding { chord: "ctrl+n".to_owned(), context: Scope::Global, value: None, script: None }));
    }

    #[test]
    fn a_binding_names_the_script_it_runs() {
        let mut table = BindingTable::defaults();
        assert!(table.add_script("ctrl+g", Scope::Global, "Open in GIMP"));
        assert!(table.add_script("ctrl+shift+g", Scope::Browse, "Resize"));
        assert!(!table.add_script("ctrl+g", Scope::Browse, "Other"), "a chord is bound to an action once");
        assert!(!table.add_script("ctrl+t", Scope::Video, "Too deep"), "the action is not bound in the video scope");

        let (action, binding) = table.resolve_binding("ctrl+g", &[Scope::Browse]).unwrap();
        assert_eq!((action.name, binding.script.as_deref()), ("script", Some("Open in GIMP")));
        let (_, binding) = table.resolve_binding("ctrl+shift+g", &[Scope::Browse]).unwrap();
        assert_eq!(binding.script.as_deref(), Some("Resize"));
        assert!(table.resolve_binding("ctrl+shift+g", &[Scope::View]).is_none(), "a browsing script is not run in the viewer");

        let text = table.to_yaml();
        assert!(text.contains("script: Open in GIMP"));
        let mut loaded = BindingTable::defaults();
        assert!(loaded.apply_yaml(&text).is_empty());
        assert_eq!(loaded.bindings("script"), table.bindings("script"));
    }

    #[test]
    fn a_script_binding_with_no_script_is_reported() {
        let mut table = BindingTable::defaults();
        let problems = table.apply_yaml("schema: 1\nactions:\n  script:\n    - ctrl+g\n    - key: ctrl+h\n      script: Resize\n");
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert_eq!(table.chords("script"), ["ctrl+h"]);
    }

    #[test]
    fn bad_entries_are_reported_and_skipped() {
        let mut table = BindingTable::defaults();
        let problems = table.apply_yaml("schema: 1\nactions:\n  no.such-action:\n    - q\n  view.next:\n    - hyper+q\n    - d\n");
        assert_eq!(problems.len(), 2);
        assert_eq!(table.chords("view.next"), ["d"]);
        // A context the action cannot be bound in is reported, and its entry skipped.
        let problems = table.apply_yaml("schema: 1\nactions:\n  view.next:\n    - key: x\n      context: browse\n    - key: y\n      context: nowhere\n");
        assert_eq!(problems.len(), 2);
        assert!(table.chords("view.next").is_empty());
    }
}
