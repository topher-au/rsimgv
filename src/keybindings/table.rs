//! The live bindings: each catalogue action's chords, each with the context it is active in and the value it
//! passes if the action takes one, which the editor changes and the window resolves key presses against. Pure:
//! no GTK.

use serde_yaml_ng::{Mapping, Value};

use super::actions::{ActionInfo, Scope, ACTIONS, DEFAULT_NOTIFICATION_SECONDS, MAX_NOTIFICATION_SECONDS, NOTIFICATION_LEVELS};
use super::chord::canonical;

/// A notification to show over the window: its message, its level (one of `NOTIFICATION_LEVELS`) and how many seconds it
/// stays up.
#[derive(Debug, Clone, PartialEq)]
pub struct Notification {
    pub message: String,
    pub level: String,
    pub seconds: f64,
}

/// A chord bound to an action, the context it is active in, the value it passes, for an action that takes one
/// (`None` uses the action's default), the script it runs, for the action that runs one, and the notification it shows,
/// for the action that shows one.
#[derive(Debug, Clone, PartialEq)]
pub struct Binding {
    pub chord: String,
    pub context: Scope,
    pub value: Option<f64>,
    pub script: Option<String>,
    pub notification: Option<Notification>,
}

impl Binding {
    fn new(chord: String, context: Scope) -> Self {
        Self { chord, context, value: None, script: None, notification: None }
    }
}

/// The notification a bindings file entry describes: a `message`, a `level` that is `info` unless it says another, and
/// the `seconds` it stays, a default number unless it says another. `Err` is what is wrong with the entry.
fn notification_of(entry: &Value) -> Result<Notification, String> {
    let message = entry.get("message").and_then(Value::as_str).filter(|message| !message.trim().is_empty());
    let Some(message) = message else { return Err("has no message".to_owned()) };
    let level = match entry.get("level") {
        None => "info",
        Some(level) => level.as_str().filter(|level| NOTIFICATION_LEVELS.contains(level)).ok_or_else(|| "has an unknown level".to_owned())?,
    };
    let seconds = match entry.get("seconds") {
        None => DEFAULT_NOTIFICATION_SECONDS,
        Some(seconds) => seconds
            .as_f64()
            .filter(|seconds| *seconds > 0.0 && *seconds <= MAX_NOTIFICATION_SECONDS)
            .ok_or_else(|| format!("must stay between 0 and {MAX_NOTIFICATION_SECONDS} seconds"))?,
    };
    Ok(Notification { message: message.to_owned(), level: level.to_owned(), seconds })
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
    #[cfg(test)]
    pub fn add(&mut self, name: &str, chord: &str, context: Scope, value: Option<f64>) -> bool {
        self.insert(name, Binding { value, ..Binding::new(chord.to_owned(), context) })
    }

    /// Binds `chord` to the action that runs scripts, in `context`, to run the script called `script`. Returns whether
    /// it was added, as `add` does.
    #[cfg(test)]
    pub fn add_script(&mut self, chord: &str, context: Scope, script: &str) -> bool {
        self.insert(super::actions::SCRIPT_ACTION, Binding { script: Some(script.to_owned()), ..Binding::new(chord.to_owned(), context) })
    }

    /// Binds the chord of `binding` to `name`, with the rest of the binding, if the action can be bound in its context,
    /// the chord is valid, and the chord is not already bound to the action. Returns whether it was added.
    pub fn insert(&mut self, name: &str, binding: Binding) -> bool {
        let (Some(index), Some(chord)) = (Self::index(name), canonical(&binding.chord)) else { return false };
        if !ACTIONS[index].contexts().contains(&binding.context) || self.chords[index].iter().any(|bound| bound.chord == chord) {
            return false;
        }
        self.chords[index].push(Binding { chord, ..binding });
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
    #[cfg(test)]
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

    /// Every binding in force in the active contexts, with its action, in the catalogue's order: those in Global and in
    /// the active contexts, less any a deeper context takes the chord from.
    pub fn active_bindings(&self, active: &[Scope]) -> Vec<(&'static ActionInfo, &Binding)> {
        let mut found = Vec::new();
        for (action, bindings) in ACTIONS.iter().zip(&self.chords) {
            for binding in bindings {
                let in_force = self.resolve_binding(&binding.chord, active).is_some_and(|(_, winner)| std::ptr::eq(winner, binding));
                if in_force {
                    found.push((action, binding));
                }
            }
        }
        found
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
    /// them (a notification is `message`, `level` and `seconds`); any other is just its chord.
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
                    if binding.value.is_none() && binding.script.is_none() && binding.notification.is_none() && own_context {
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
                    if let Some(notification) = &binding.notification {
                        entry.insert(Value::String("message".to_owned()), Value::String(notification.message.clone()));
                        entry.insert(Value::String("level".to_owned()), Value::String(notification.level.clone()));
                        entry.insert(Value::String("seconds".to_owned()), Value::Number(notification.seconds.into()));
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
    /// cannot be bound in, a script binding that names no script, a notification with no message or with a level or a
    /// duration that does not fit, or a file that cannot be read.
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
                if !action.contexts().contains(&context) {
                    problems.push(format!("{name}: {chord} cannot be bound in {}", context.title()));
                    continue;
                }
                if action.takes_script() && script.is_none() {
                    problems.push(format!("{name}: {chord} names no script"));
                    continue;
                }
                let notification = if action.takes_notification() {
                    match notification_of(entry) {
                        Ok(notification) => Some(notification),
                        Err(problem) => {
                            problems.push(format!("{name}: {chord} {problem}"));
                            continue;
                        }
                    }
                } else {
                    None
                };
                if bindings.iter().any(|bound| bound.chord == chord) {
                    continue;
                }
                bindings.push(Binding { chord, context, value: passed, script: script.filter(|_| action.takes_script()), notification });
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
        // The viewer toggle is global too, so the same key works in browsing and in the viewer.
        assert_eq!(table.resolve("b", &[Scope::Global, Scope::Browse]).map(|(a, _)| a.name), Some("win.toggle-viewer"));
        assert_eq!(table.resolve("b", &[Scope::Global, Scope::View, Scope::Image]).map(|(a, _)| a.name), Some("win.toggle-viewer"));
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
            [Binding { chord: "ctrl+right".to_owned(), context: Scope::Video, value: Some(30.0), script: None, notification: None }]
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
        assert_eq!(loaded.bindings("view.next").last(), Some(&Binding { chord: "ctrl+n".to_owned(), context: Scope::Global, value: None, script: None, notification: None }));
    }

    #[test]
    fn the_active_bindings_are_those_in_force_in_the_contexts() {
        let table = BindingTable::defaults();
        let names = |active: &[Scope]| table.active_bindings(active).iter().map(|(action, _)| action.name).collect::<Vec<_>>();
        let browsing = names(&[Scope::Browse]);
        assert!(browsing.contains(&"browse.cursor-up") && browsing.contains(&"win.settings"), "browsing and global ones");
        assert!(!browsing.contains(&"video.seek-forward") && !browsing.contains(&"view.next"), "not the viewer's");
        let video = names(&[Scope::View, Scope::Video]);
        assert!(video.contains(&"video.seek-forward") && video.contains(&"view.next") && video.contains(&"win.settings"));
        assert!(!video.contains(&"browse.cursor-up") && !video.contains(&"image.zoom-in"));
    }

    #[test]
    fn a_chord_a_deeper_context_takes_is_not_active_in_the_shallower() {
        let mut table = BindingTable::defaults();
        assert!(table.add("view.next", "ctrl+x", Scope::Global, None));
        assert!(table.add("video.mute", "ctrl+x", Scope::Video, None));
        let active = table.active_bindings(&[Scope::View, Scope::Video]);
        let on_x: Vec<&str> = active.iter().filter(|(_, binding)| binding.chord == "ctrl+x").map(|(action, _)| action.name).collect();
        assert_eq!(on_x, ["video.mute"]);
        let browsing = table.active_bindings(&[Scope::Browse]);
        let on_x: Vec<&str> = browsing.iter().filter(|(_, binding)| binding.chord == "ctrl+x").map(|(action, _)| action.name).collect();
        assert_eq!(on_x, ["view.next"], "the video's binding does not apply while browsing");
    }

    #[test]
    fn a_binding_names_the_script_it_runs() {
        let mut table = BindingTable::defaults();
        assert!(table.add_script("ctrl+g", Scope::Global, "Open in GIMP"));
        assert!(table.add_script("ctrl+shift+g", Scope::Browse, "Resize"));
        assert!(!table.add_script("ctrl+g", Scope::Browse, "Other"), "a chord is bound to an action once");
        assert!(table.add_script("ctrl+t", Scope::Video, "Transcode"), "a script can be bound in any context");

        let (action, binding) = table.resolve_binding("ctrl+g", &[Scope::Browse]).unwrap();
        assert_eq!((action.name, binding.script.as_deref()), ("script", Some("Open in GIMP")));
        let (_, binding) = table.resolve_binding("ctrl+shift+g", &[Scope::Browse]).unwrap();
        assert_eq!(binding.script.as_deref(), Some("Resize"));
        assert!(table.resolve_binding("ctrl+shift+g", &[Scope::View]).is_none(), "a browsing script is not run in the viewer");
        let (_, binding) = table.resolve_binding("ctrl+t", &[Scope::View, Scope::Video]).unwrap();
        assert_eq!(binding.script.as_deref(), Some("Transcode"));
        assert!(table.resolve_binding("ctrl+t", &[Scope::Browse]).is_none(), "a video script is not run while browsing");

        let text = table.to_yaml();
        assert!(text.contains("script: Open in GIMP"));
        let mut loaded = BindingTable::defaults();
        assert!(loaded.apply_yaml(&text).is_empty());
        assert_eq!(loaded.bindings("script"), table.bindings("script"));
    }

    #[test]
    fn a_binding_carries_the_notification_it_shows() {
        let mut table = BindingTable::defaults();
        let saved = Notification { message: "Saved".to_owned(), level: "success".to_owned(), seconds: 2.5 };
        assert!(table.insert("win.notify", Binding { notification: Some(saved.clone()), ..Binding::new("ctrl+shift+s".to_owned(), Scope::Global) }));
        assert!(table.insert("win.notify", Binding { notification: Some(saved.clone()), ..Binding::new("ctrl+shift+v".to_owned(), Scope::Video) }));
        let text = table.to_yaml();
        assert!(text.contains("message: Saved") && text.contains("level: success"));
        let mut loaded = BindingTable::defaults();
        assert!(loaded.apply_yaml(&text).is_empty());
        assert_eq!(loaded.bindings("win.notify"), table.bindings("win.notify"));
        let (action, binding) = loaded.resolve_binding("ctrl+shift+s", &[Scope::Browse]).unwrap();
        assert_eq!((action.name, binding.notification.as_ref()), ("win.notify", Some(&saved)));
    }

    #[test]
    fn a_notification_binding_that_does_not_fit_is_reported() {
        let mut table = BindingTable::defaults();
        let text = "schema: 1\nactions:\n  win.notify:\n    - ctrl+a\n    - key: ctrl+b\n      message: ''\n    - key: ctrl+c\n      message: Hi\n      level: loud\n    - key: ctrl+d\n      message: Hi\n      seconds: 0\n    - key: ctrl+e\n      message: Hi\n    - key: ctrl+f\n      message: Hi\n      level: error\n      seconds: 9\n";
        let problems = table.apply_yaml(text);
        assert_eq!(problems.len(), 4, "{problems:?}");
        let bound = table.bindings("win.notify");
        assert_eq!(bound.iter().map(|binding| binding.chord.as_str()).collect::<Vec<_>>(), ["ctrl+e", "ctrl+f"]);
        assert_eq!(bound[0].notification, Some(Notification { message: "Hi".to_owned(), level: "info".to_owned(), seconds: DEFAULT_NOTIFICATION_SECONDS }));
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
