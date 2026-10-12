//! OpenCode: the OpenCode Go plan's windows (5-hour, weekly, monthly), from
//! OpenCode's official usage API with the `opencode-go` key OpenCode keeps
//! locally. Zen (pay as you go) has no limits: its users see spend only.
//!
//! OpenCode 2 keeps the key in each channel database's `credential` table
//! (`opencode.db`, `opencode-next.db`…); OpenCode 1 in `auth.json`. Once
//! OpenCode 2 has imported `auth.json`, the file no longer counts (a logout
//! there must not be revived by the old file). Everything is read-only.

use std::path::PathBuf;

use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::Value;

use super::*;

const USAGE_URL: &str = "https://opencode.ai/zen/go/v1/usage";

/// OpenCode's data folder: `$OPENCODE_DATA_DIR`, `$XDG_DATA_HOME/opencode`,
/// or `~/.local/share/opencode` (on Windows too).
pub fn data_dir() -> Option<PathBuf> {
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.to_string_lossy().trim().is_empty()).map(PathBuf::from);
    env("OPENCODE_DATA_DIR")
        .or_else(|| env("XDG_DATA_HOME").map(|d| d.join("opencode")))
        .or_else(|| home().map(|h| h.join(".local").join("share").join("opencode")))
}

/// Every channel database (`opencode*.db`), sorted.
pub fn databases() -> Vec<PathBuf> {
    let Some(dir) = data_dir() else { return Vec::new() };
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("opencode") && n.ends_with(".db"))
        })
        .collect();
    out.sort();
    out
}

pub fn open_read_only(path: &std::path::Path) -> Option<Connection> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX).ok()?;
    let _ = conn.busy_timeout(std::time::Duration::from_millis(1500));
    Some(conn)
}

fn has_table(conn: &Connection, name: &str) -> bool {
    conn.query_row("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1", [name], |_| Ok(()))
        .optional()
        .ok()
        .flatten()
        .is_some()
}

/// The OpenCode Go key, if logged in.
fn go_key() -> Option<String> {
    let mut imported = false;
    for path in databases() {
        let Some(conn) = open_read_only(&path) else { continue };
        if !has_table(&conn, "credential") || !has_table(&conn, "migration") {
            continue;
        }
        let done = conn
            .query_row("SELECT 1 FROM migration WHERE id = '20260805200742_import_legacy_credentials'", [], |_| Ok(()))
            .optional()
            .ok()
            .flatten()
            .is_some();
        if !done {
            continue;
        }
        imported = true;
        let key: Option<String> = conn
            .query_row(
                "SELECT json_extract(value, '$.key') FROM credential \
                 WHERE integration_id = 'opencode-go' AND (active IS NULL OR active = 1) \
                 ORDER BY active DESC, time_updated DESC, id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()
            .ok()
            .flatten();
        if let Some(k) = key.map(|k| k.trim().to_string()).filter(|k| !k.is_empty()) {
            return Some(k);
        }
    }
    if imported {
        return None;
    }
    let text = std::fs::read_to_string(data_dir()?.join("auth.json")).ok()?;
    let doc: Value = serde_json::from_str(&text).ok()?;
    doc["opencode-go"]["key"].as_str().map(str::trim).filter(|k| !k.is_empty()).map(str::to_string)
}

/// A Go key, or usage through OpenCode's own gateways in the local logs.
pub fn has_login() -> bool {
    go_key().is_some() || crate::spend::opencode::has_hosted_usage()
}

pub async fn fetch() -> FetchResult {
    // Zen only: nothing to meter, spend comes from the logs.
    let Some(key) = go_key() else { return Ok(Usage::default()) };
    let resp = client()
        .get(USAGE_URL)
        .bearer_auth(key)
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|_| FetchError::failed("Couldn't reach OpenCode"))?;
    let status = resp.status().as_u16();
    if status == 429 {
        return Err(FetchError::RateLimited { retry_after_secs: retry_after(&resp) });
    }
    let server_now = resp
        .headers()
        .get(reqwest::header::DATE)
        .and_then(|d| d.to_str().ok())
        .and_then(|d| chrono::DateTime::parse_from_rfc2822(d).ok())
        .map(|d| d.timestamp_millis())
        .unwrap_or_else(now_ms);
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    if status == 401 {
        return Err(FetchError::SignedOut("OpenCode didn't accept your Go key. Log in to OpenCode Go again".into()));
    }
    if status == 403 && body["error"]["type"].as_str() == Some("EntitlementError") {
        // A valid key without a Go plan: spend still shows.
        return Ok(Usage::default());
    }
    if !(200..300).contains(&status) {
        return Err(FetchError::Failed(format!("OpenCode answered {status}")));
    }
    Ok(Usage { plan: Some("Go".into()), metrics: map_usage(&body, server_now)? })
}

/// `{ usage: { rolling, weekly, monthly } }`, each `{ percent, resetsAt }`.
pub fn map_usage(body: &Value, now: i64) -> Result<Vec<Metric>, FetchError> {
    let usage = &body["usage"];
    let window = |key: &str, id: &str, label: &str, period: i64| -> Result<Metric, FetchError> {
        let w = &usage[key];
        let used = num(w.get("percent")).ok_or_else(|| FetchError::failed("OpenCode's usage answer changed"))?;
        let mut resets = epoch_ms(w.get("resetsAt"));
        // An untouched 5-hour window reports "now + 5h", recomputed on every
        // request: that's not a reset, the window just hasn't started.
        if id == "session" && used == 0.0 && resets.is_some_and(|r| (r - now - period).abs() <= 2_000) {
            resets = None;
        }
        Ok(Metric::percent(id, label, used, resets, Some(period)))
    };
    Ok(vec![
        window("rolling", "session", "Session", FIVE_HOURS_MS)?,
        window("weekly", "weekly", "Weekly", WEEK_MS)?,
        window("monthly", "monthly", "Monthly", MONTH_MS)?,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn go_windows() {
        let now = 1_791_000_000_000;
        let body = json!({ "usage": {
            "rolling": { "percent": 0, "resetsAt": "2026-10-03T08:00:00Z" },
            "weekly": { "percent": 41, "resetsAt": "2026-10-05T00:00:00Z" },
            "monthly": { "percent": 12, "resetsAt": "2026-11-01T00:00:00Z" }
        }});
        let m = map_usage(&body, now).unwrap();
        let ids: Vec<_> = m.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["session", "weekly", "monthly"]);
        assert_eq!(m[1].used, 41.0);
        assert!(m[0].resets_at.is_some(), "a reset that isn't now + 5h is real");
    }

    #[test]
    fn an_untouched_session_has_no_reset() {
        let now = 1_791_000_000_000;
        let at = chrono::DateTime::from_timestamp_millis(now + FIVE_HOURS_MS + 1_000).unwrap().to_rfc3339();
        let body = json!({ "usage": {
            "rolling": { "percent": 0, "resetsAt": at },
            "weekly": { "percent": 0, "resetsAt": at },
            "monthly": { "percent": 0, "resetsAt": at }
        }});
        let m = map_usage(&body, now).unwrap();
        assert_eq!(m[0].resets_at, None);
        assert!(m[1].resets_at.is_some(), "only the 5-hour window does this");
        assert!(map_usage(&json!({ "usage": {} }), now).is_err());
    }
}
