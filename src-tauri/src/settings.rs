//! User settings, persisted as JSON in the app's data folder.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const DEFAULT_HOTKEY: &str = "Ctrl+Alt+U";
/// Which limits a notification watches: every limit, or one window.
pub const ALERT_LIMITS: [&str; 4] = ["any", "session", "weekly", "monthly"];
/// At most this many notifications.
pub const MAX_ALERTS: usize = 8;

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
    /// Provider id → on. Absent means off.
    pub providers: BTreeMap<String, bool>,
    /// Every provider this install has seen. A provider missing from both
    /// lists is new: it turns on once, only if its login is on this machine,
    /// and is the user's to manage after that.
    pub known_providers: Vec<String>,
    /// Holster notifies when a limit passes one of these.
    pub alerts: Vec<AlertRule>,
    /// Holster notifies when a limit you were close to resets.
    #[serde(alias = "glowOnReset")]
    pub notify_on_reset: bool,
    /// 0.2's single threshold (0 = off), read once to carry it into `alerts`.
    #[serde(rename = "alertAt", skip_serializing)]
    pub(crate) legacy_alert_at: Option<u32>,
}

/// "Notify me when {limit} passes {at}%".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlertRule {
    /// Percent used, 1-100 (100: the limit ran out).
    pub at: u32,
    /// One of `ALERT_LIMITS`: "any", or a window ("session" is the 5-hour one).
    pub limit: String,
}

impl AlertRule {
    pub fn new(at: u32, limit: &str) -> Self {
        Self { at, limit: limit.into() }
    }

    /// Whether this rule watches a limit in `window` (see `Metric::window`).
    pub fn watches(&self, window: Option<&str>) -> bool {
        self.limit == "any" || window == Some(self.limit.as_str())
    }
}

fn default_alerts() -> Vec<AlertRule> {
    vec![AlertRule::new(80, "any"), AlertRule::new(100, "any")]
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
            providers: BTreeMap::new(),
            known_providers: Vec::new(),
            alerts: default_alerts(),
            notify_on_reset: true,
            legacy_alert_at: None,
        }
    }
}

impl Settings {
    pub fn provider_on(&self, id: &str) -> bool {
        self.providers.get(id).copied().unwrap_or(false)
    }

    /// Turns on providers this install has never seen, if their login is on
    /// this machine (local files and saved keys only, never the network).
    /// Returns whether anything changed.
    fn discover_providers(&mut self, has_login: impl Fn(&str) -> bool) -> bool {
        // Settings from before this list existed: what they have toggles for was seen.
        if self.known_providers.is_empty() {
            self.known_providers = self.providers.keys().cloned().collect();
        }
        let mut changed = false;
        for p in crate::providers::PROVIDERS.iter() {
            if self.known_providers.iter().any(|k| k == p.id) {
                continue;
            }
            self.known_providers.push(p.id.to_string());
            let on = p.auto_enable && has_login(p.id);
            self.providers.insert(p.id.to_string(), on);
            changed = true;
        }
        changed
    }

    fn normalized(mut self) -> Self {
        if self.spend_metric != "tokens" {
            self.spend_metric = "cost".into();
        }
        if !REFRESH_CHOICES.contains(&self.refresh_minutes) {
            self.refresh_minutes = if self.refresh_minutes < 5 { 5 } else { 15 };
        }
        // 0.2 had one threshold for every limit, plus running out.
        if let Some(old) = self.legacy_alert_at.take() {
            if self.alerts == default_alerts() {
                self.alerts = match old {
                    0 => Vec::new(),
                    90 => vec![AlertRule::new(90, "any"), AlertRule::new(100, "any")],
                    _ => default_alerts(),
                };
            }
        }
        let mut alerts: Vec<AlertRule> = Vec::new();
        for mut r in std::mem::take(&mut self.alerts) {
            r.at = r.at.clamp(1, 100);
            if !ALERT_LIMITS.contains(&r.limit.as_str()) {
                r.limit = "any".into();
            }
            if alerts.len() < MAX_ALERTS {
                alerts.push(r);
            }
        }
        self.alerts = alerts;
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
        let mut s: Settings = std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        let discovered = s.discover_providers(crate::providers::has_login);
        let s = s.normalized();
        if discovered || !self.path.exists() {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn carries_the_old_threshold_into_rules() {
        let s = |json: &str| serde_json::from_str::<Settings>(json).unwrap().normalized().alerts;
        assert_eq!(s(r#"{"alertAt":90}"#), vec![AlertRule::new(90, "any"), AlertRule::new(100, "any")]);
        assert_eq!(s(r#"{"alertAt":0}"#), vec![]);
        assert_eq!(s(r#"{}"#), default_alerts());
        assert_eq!(
            s(r#"{"alerts":[{"at":150,"limit":"weekly"},{"at":50,"limit":"daily"}]}"#),
            vec![AlertRule::new(100, "weekly"), AlertRule::new(50, "any")]
        );
        // Saved settings don't keep the old field.
        assert!(!serde_json::to_string(&Settings::default()).unwrap().contains("alertAt"));
    }

    #[test]
    fn new_providers_turn_on_once_and_only_with_a_login() {
        // Settings from 0.1.x: three toggles, no known list.
        let mut s: Settings = serde_json::from_str(r#"{"providers":{"claude":true,"codex":false,"cursor":true}}"#).unwrap();
        assert!(s.discover_providers(|id| id == "grok" || id == "codex" || id == "ollama"));
        assert!(s.provider_on("claude") && !s.provider_on("codex"), "earlier choices stay");
        assert!(s.provider_on("grok"), "a new provider with a login turns on");
        assert!(!s.provider_on("ollama"), "Ollama never turns itself on");
        assert!(!s.provider_on("openrouter"));
        // Turned off by the user: never back on.
        s.providers.insert("grok".into(), false);
        assert!(!s.discover_providers(|_| true));
        assert!(!s.provider_on("grok") && !s.provider_on("openrouter"));
    }
}
