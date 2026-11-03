//! The scripts listed on the Scripts page: a name, a command, and whether to wait for it to finish. A command is run
//! by the shell, with `%file%` standing for the path of the item it is run on.

use crate::settings::Settings;

use std::path::Path;
use std::process::{Child, Command};


/// What `%file%` in a command becomes: the shell's first argument, which is the item's path.
const FILE_ARGUMENT: &str = "\"$1\"";

/// One script: what it is called, the command it runs, and whether to wait for it to finish.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Script {
    pub name: String,
    pub command: String,
    pub wait: bool,
}

impl Script {
    /// The entry this script takes in the `scripts` setting, a JSON object.
    pub fn to_setting(&self) -> String {
        serde_json::json!({ "name": self.name, "command": self.command, "wait": self.wait }).to_string()
    }

    /// The script an entry of the `scripts` setting holds, or `None` if the entry is not one.
    pub fn from_setting(text: &str) -> Option<Self> {
        let value: serde_json::Value = serde_json::from_str(text).ok()?;
        Some(Self {
            name: value.get("name")?.as_str()?.to_owned(),
            command: value.get("command")?.as_str()?.to_owned(),
            wait: value.get("wait")?.as_bool()?,
        })
    }

    /// Whether the command is run on an item, rather than on nothing.
    pub fn uses_file(&self) -> bool {
        self.command.contains("%file%")
    }

    /// The command as the shell runs it. The path is not pasted into it but passed as an argument, so a name with
    /// spaces, quotes or other characters the shell treats specially is still one path.
    fn program(&self) -> String {
        self.command.replace("%file%", FILE_ARGUMENT)
    }

    /// Starts the script on `file`, if it uses one, without waiting for it.
    pub fn spawn(&self, file: Option<&Path>) -> std::io::Result<Child> {
        let mut command = Command::new("sh");
        command.arg("-c").arg(self.program()).arg("sh");
        if let Some(file) = file {
            command.arg(file);
        }
        command.spawn()
    }
}

/// The scripts in the `scripts` setting, in order. Entries that are not scripts are skipped.
pub fn read_scripts(settings: &Settings) -> Vec<Script> {
    settings.strv("scripts.list").iter().filter_map(|text| Script::from_setting(text)).collect()
}

/// Saves `scripts` as the `scripts` setting.
pub fn write_scripts(settings: &Settings, scripts: &[Script]) {
    let entries: Vec<String> = scripts.iter().map(Script::to_setting).collect();
    let entries: Vec<&str> = entries.iter().map(String::as_str).collect();
    let _ = settings.set_strv("scripts.list", entries.as_slice());
}

/// The script called `name`, the first if several share it.
pub fn find_script(settings: &Settings, name: &str) -> Option<Script> {
    read_scripts(settings).into_iter().find(|script| script.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_script_survives_a_round_trip_through_the_setting() {
        let script = Script { name: "Open in GIMP".into(), command: "gimp %file%".into(), wait: true };
        assert_eq!(Script::from_setting(&script.to_setting()), Some(script));
    }

    #[test]
    fn entries_that_are_not_scripts_are_skipped() {
        assert_eq!(Script::from_setting("not json"), None);
        assert_eq!(Script::from_setting(r#"{"name": "x"}"#), None);
    }

    #[test]
    fn the_path_is_an_argument_and_not_part_of_the_command() {
        let script = Script { name: "Edit".into(), command: "gimp %file% && echo %file%".into(), wait: false };
        assert!(script.uses_file());
        assert_eq!(script.program(), "gimp \"$1\" && echo \"$1\"");
        assert!(!Script { command: "notify-send done".into(), ..Script::default() }.uses_file());
    }

    #[test]
    fn a_path_the_shell_would_split_reaches_the_script_whole() {
        let directory = std::env::temp_dir().join(format!("rsimgv-scripts-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let file = directory.join("a b; $(echo x).png");
        let output = directory.join("out");
        let script = Script { name: "Copy".into(), command: format!("printf '%s' %file% > '{}'", output.display()), wait: true };
        script.spawn(Some(&file)).unwrap().wait().unwrap();
        assert_eq!(std::fs::read_to_string(&output).unwrap(), file.to_string_lossy());
        let _ = std::fs::remove_dir_all(&directory);
    }
}
