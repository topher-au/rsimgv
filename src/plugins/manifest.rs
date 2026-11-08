//! Plugin manifests: the YAML file that describes a plugin and what it contributes.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The API version this build understands.
pub const API_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Runtime {
    #[default]
    Native,
    Python,
}

impl Runtime {
    pub fn name(self) -> &'static str {
        match self {
            Runtime::Native => "native",
            Runtime::Python => "python",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Startup {
    #[default]
    OnDemand,
    Always,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Manifest {
    pub api: u32,
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub runtime: Runtime,
    #[serde(default)]
    pub entry: Option<String>,
    #[serde(default)]
    pub library: Option<String>,
    #[serde(default)]
    pub startup: Startup,
    #[serde(default)]
    pub events: Vec<String>,
    #[serde(default)]
    pub contributes: Contributions,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Contributions {
    #[serde(default)]
    pub actions: Vec<ActionContribution>,
    #[serde(default)]
    pub menus: Vec<MenuContribution>,
    #[serde(default)]
    pub panels: Vec<PanelContribution>,
    #[serde(default)]
    pub settings: Vec<SettingContribution>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ActionContribution {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_scope")]
    pub scope: String,
    #[serde(default)]
    pub default_keys: Vec<String>,
}

fn default_scope() -> String {
    "global".to_owned()
}

#[derive(Clone, Debug, Deserialize)]
pub struct MenuContribution {
    #[serde(default = "default_menu")]
    pub menu: String,
    pub label: String,
    pub action: String,
}

fn default_menu() -> String {
    "main".to_owned()
}

#[derive(Clone, Debug, Deserialize)]
pub struct PanelContribution {
    pub id: String,
    pub slot: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub position: String,
}

/// A setting a plugin exposes in the Plugins preferences page. The value is kept in the plugin's own
/// `config.json`, so a plugin can read it with the same format it would use on its own.
#[derive(Clone, Debug, Deserialize)]
pub struct SettingContribution {
    pub key: String,
    pub label: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_setting_kind")]
    pub kind: String,
    #[serde(default)]
    pub default: serde_json::Value,
    #[serde(default)]
    pub choices: Vec<String>,
    #[serde(default)]
    pub min: Option<i64>,
    #[serde(default)]
    pub max: Option<i64>,
}

fn default_setting_kind() -> String {
    "string".to_owned()
}

impl Manifest {
    /// Reads and validates the manifest at `path`. `dir` is the plugin directory.
    pub fn load(path: &Path, dir: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|err| format!("cannot read {}: {err}", path.display()))?;
        let manifest: Self = serde_yaml_ng::from_str(&text)
            .map_err(|err| format!("{} is not valid: {err}", path.display()))?;
        manifest.validate(dir)?;
        Ok(manifest)
    }

    pub fn validate(&self, dir: &Path) -> Result<(), String> {
        if self.api != API_VERSION {
            return Err(format!("plugin API {} is not supported (this build expects {API_VERSION})", self.api));
        }
        if !valid_id(&self.id) {
            return Err(format!(
                "the plugin id {:?} must contain only lowercase letters, digits and hyphens, and not start or end with a hyphen",
                self.id
            ));
        }
        if self.name.trim().is_empty() {
            return Err("the plugin has no name".to_owned());
        }
        for action in &self.contributes.actions {
            if !valid_id(&action.id) {
                return Err(format!("the action id {:?} is not valid", action.id));
            }
            if action.label.trim().is_empty() {
                return Err(format!("the action {:?} has no label", action.id));
            }
            if crate::keybindings::actions::Scope::from_id(&action.scope).is_none() {
                return Err(format!("the action {:?} names an unknown scope {:?}", action.id, action.scope));
            }
        }
        for menu in &self.contributes.menus {
            if !matches!(menu.menu.as_str(), "main" | "item-context") {
                return Err(format!("the menu {:?} is not known", menu.menu));
            }
            if !self.contributes.actions.iter().any(|action| action.id == menu.action) {
                return Err(format!("the menu item {:?} names no action of this plugin", menu.label));
            }
        }
        for panel in &self.contributes.panels {
            if !matches!(panel.slot.as_str(), "sidebar.bottom" | "browse.header" | "viewer.overlay" | "preferences.plugins") {
                return Err(format!("the panel slot {:?} is not known", panel.slot));
            }
            if !panel.position.is_empty()
                && !matches!(panel.position.as_str(), "top-left" | "top-right" | "bottom-left" | "bottom-right")
            {
                return Err(format!("the panel position {:?} is not known", panel.position));
            }
        }
        let mut keys = std::collections::HashSet::new();
        for setting in &self.contributes.settings {
            if setting.key.trim().is_empty() || !keys.insert(setting.key.clone()) {
                return Err(format!("the setting key {:?} is empty or repeated", setting.key));
            }
            if !matches!(setting.kind.as_str(), "string" | "text" | "int" | "bool" | "choice") {
                return Err(format!("the setting {:?} has an unknown kind {:?}", setting.key, setting.kind));
            }
            if setting.kind == "choice" {
                if setting.choices.is_empty() {
                    return Err(format!("the choice setting {:?} has no choices", setting.key));
                }
                let default = setting.default.as_str().unwrap_or_default();
                if !setting.choices.iter().any(|choice| choice == default) {
                    return Err(format!("the setting {:?} has a default outside its choices", setting.key));
                }
            }
            if setting.kind == "int" && !setting.default.is_null() && !setting.default.is_i64() {
                return Err(format!("the setting {:?} needs an integer default", setting.key));
            }
            if setting.kind == "bool" && !setting.default.is_null() && !setting.default.is_boolean() {
                return Err(format!("the setting {:?} needs a true or false default", setting.key));
            }
        }
        match self.runtime {
            Runtime::Python => {
                let entry = self.entry.as_deref().ok_or_else(|| "a Python plugin needs an entry file".to_owned())?;
                let path = dir.join(entry);
                if !path.is_file() {
                    return Err(format!("the entry file {} does not exist", path.display()));
                }
            }
            Runtime::Native => {
                if self.library.as_deref().is_none_or(str::is_empty) {
                    return Err("a native plugin needs a library".to_owned());
                }
            }
        }
        Ok(())
    }

    /// The full action name the keybindings and IPC use.
    pub fn action_name(&self, action: &str) -> String {
        format!("plugin.{}.{}", self.id, action)
    }

    pub fn entry_path(&self, dir: &Path) -> PathBuf {
        dir.join(self.entry.as_deref().unwrap_or_default())
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && !id.starts_with('-')
        && !id.ends_with('-')
        && !id.starts_with('.')
        && !id.ends_with('.')
        && !id.contains("..")
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '.')
}
