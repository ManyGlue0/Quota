//! Tauri commands called by the UI. All are async so they never run on (and
//! never block) the window thread.

use std::sync::Arc;

use serde_json::{json, Value};
use tauri::{AppHandle, State};

use crate::settings::Settings;
use crate::shell::ShellState;
use crate::usage::Snapshot;
use crate::Core;

type Res<T> = Result<T, String>;
type C<'a> = State<'a, Arc<Core>>;

// ---- shell -------------------------------------------------------------------

#[tauri::command]
pub async fn shell_state(core: C<'_>) -> Res<ShellState> {
    Ok(core.shell().snapshot())
}

#[tauri::command]
pub async fn shell_close(core: C<'_>) -> Res<()> {
    core.shell().close();
    Ok(())
}

#[tauri::command]
pub async fn shell_set_height(core: C<'_>, height: f64) -> Res<()> {
    core.shell().set_content_height(height);
    Ok(())
}

// ---- usage -------------------------------------------------------------------

#[tauri::command]
pub async fn usage_snapshot(core: C<'_>) -> Res<Snapshot> {
    Ok(core.poller.snapshot())
}

#[tauri::command]
pub async fn usage_refresh(core: C<'_>) -> Res<()> {
    core.poller.refresh();
    Ok(())
}

// ---- settings ------------------------------------------------------------------

#[tauri::command]
pub async fn get_settings(core: C<'_>) -> Res<Settings> {
    Ok(core.prefs.get())
}

/// Applies a partial settings object (same shape as `Settings`, camelCase).
#[tauri::command]
pub async fn update_settings(app: AppHandle, core: C<'_>, patch: Value) -> Res<Settings> {
    let before = core.prefs.get();
    let mut merged = serde_json::to_value(&before).map_err(|e| e.to_string())?;
    merge(&mut merged, patch);
    let next: Settings = serde_json::from_value(merged).map_err(|e| e.to_string())?;
    if next.hotkey != before.hotkey {
        // A taken shortcut is reported in the UI rather than rejected, so the
        // user sees which combination failed.
        let _ = crate::register_hotkey(&app, &next.hotkey);
    }
    let after = core.prefs.update(|s| *s = next);
    core.apply_settings_change(&app, &before, &after);
    Ok(after)
}

fn merge(a: &mut Value, b: Value) {
    match (a, b) {
        (Value::Object(a), Value::Object(b)) => {
            for (k, v) in b {
                merge(a.entry(k).or_insert(Value::Null), v);
            }
        }
        (a, b) => *a = b,
    }
}

// ---- API keys ------------------------------------------------------------------

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyStatus {
    id: String,
    source: crate::secrets::KeySource,
    env: Vec<&'static str>,
}

/// Where each key-based provider's key comes from (never the key itself).
#[tauri::command]
pub async fn provider_keys() -> Res<Vec<KeyStatus>> {
    Ok(crate::providers::PROVIDERS
        .iter()
        .filter_map(|p| {
            let env = crate::providers::key_env(p.id)?;
            Some(KeyStatus { id: p.id.into(), source: crate::secrets::api_key(p.id, env).1, env: env.to_vec() })
        })
        .collect())
}

/// Saves a pasted key in Windows Credential Manager and turns the provider on.
#[tauri::command]
pub async fn set_provider_key(app: AppHandle, core: C<'_>, id: String, key: String) -> Res<Settings> {
    if crate::providers::key_env(&id).is_none() {
        return Err("This provider doesn't take a key".into());
    }
    let key = key.trim();
    if key.is_empty() || key.len() > 4096 || key.chars().any(char::is_whitespace) {
        return Err("That doesn't look like an API key".into());
    }
    crate::secrets::set(&id, key)?;
    let before = core.prefs.get();
    let after = core.prefs.update(|s| {
        s.providers.insert(id.clone(), true);
    });
    core.apply_settings_change(&app, &before, &after);
    core.poller.refresh();
    Ok(after)
}

#[tauri::command]
pub async fn clear_provider_key(core: C<'_>, id: String) -> Res<()> {
    if crate::providers::key_env(&id).is_none() {
        return Err("This provider doesn't take a key".into());
    }
    crate::secrets::delete(&id)?;
    core.poller.refresh();
    Ok(())
}

#[tauri::command]
pub async fn holster_status() -> Res<crate::holster::HolsterStatus> {
    Ok(crate::holster::status())
}

#[tauri::command]
pub async fn app_info(app: AppHandle) -> Res<Value> {
    Ok(json!({
        "version": app.package_info().version.to_string(),
        "hotkeyError": crate::hotkey_error(),
    }))
}
