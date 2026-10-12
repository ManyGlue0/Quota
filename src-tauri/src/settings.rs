//! User settings, persisted as JSON in the app's data folder.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const DEFAULT_HOTKEY: &str = "Ctrl+Alt+U";
/// Refresh intervals (minutes). 0: "When I look" — check only on a hover of
/// the tile or when the panel opens, nothing in the background.
pub const REFRESH_CHOICES: [u32; 3] = [0, 5, 15];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    /// "system" | "light" | "dark"
    pub theme: String,
    pub hotkey: String,
    pub click_outside_closes: bool,
    pub launch_at_startup: bool,
    /// Show remaining instead of used on meters.
    pub show_left: bool,
    /// Reset labels as exact times instead of countdowns.
    pub exact_resets: bool,
    /// Provider cards whose caret is open.
    pub expanded: Vec<String>,
    /// Total spend card: "cost" | "tokens".
    pub spend_metric: String,
    pub refresh_minutes: u32,
    pub providers: ProviderToggles,
    /// Holster's tab glows when a limit crosses this percentage (0 = off).
    pub alert_at: u32,
    /// Holster's tab glows when a limit you were close to resets.
    pub glow_on_reset: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderToggles {
    pub claude: bool,
    pub codex: bool,
    pub cursor: bool,
}

impl ProviderToggles {
    pub fn enabled(&self, id: &str) -> bool {
        match id {
            "claude" => self.claude,
            "codex" => self.codex,
            "cursor" => self.cursor,
            _ => false,
        }
    }
}

impl Default for ProviderToggles {
    fn default() -> Self {
        Self { claude: true, codex: true, cursor: true }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: "system".into(),
            hotkey: DEFAULT_HOTKEY.into(),
            click_outside_closes: true,
            launch_at_startup: true,
            show_left: false,
            exact_resets: false,
            expanded: Vec::new(),
            spend_metric: "cost".into(),
            refresh_minutes: 0,
            providers: ProviderToggles::default(),
            alert_at: 80,
            glow_on_reset: true,
        }
    }
}

impl Settings {
    fn normalized(mut self) -> Self {
        if self.spend_metric != "tokens" {
            self.spend_metric = "cost".into();
        }
        if !REFRESH_CHOICES.contains(&self.refresh_minutes) {
            self.refresh_minutes = if self.refresh_minutes < 5 { 5 } else { 15 };
        }
        if ![0, 80, 90].contains(&self.alert_at) {
            self.alert_at = 80;
        }
        if !["system", "light", "dark"].contains(&self.theme.as_str()) {
            self.theme = "system".into();
        }
        self
    }
}

pub struct SettingsStore {
    path: PathBuf,
}

impl SettingsStore {
    pub fn new(dir: &Path) -> Self {
        Self { path: dir.join("settings.json") }
    }

    pub fn load(&self) -> Settings {
        let s: Settings = std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        let s = s.normalized();
        if !self.path.exists() {
            let _ = self.save(&s);
        }
        s
    }

    /// Atomic write: temp file then rename, so a crash never leaves half a file.
    pub fn save(&self, s: &Settings) -> anyhow::Result<()> {
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(s)?)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

/// Live settings shared across the app, saved on every change.
pub struct Prefs {
    current: std::sync::RwLock<Settings>,
    store: SettingsStore,
}

impl Prefs {
    pub fn new(store: SettingsStore) -> Self {
        Self { current: std::sync::RwLock::new(store.load()), store }
    }

    pub fn get(&self) -> Settings {
        self.current.read().unwrap().clone()
    }

    pub fn update(&self, f: impl FnOnce(&mut Settings)) -> Settings {
        let mut s = self.current.write().unwrap();
        f(&mut s);
        *s = s.clone().normalized();
        let _ = self.store.save(&s);
        s.clone()
    }
}
