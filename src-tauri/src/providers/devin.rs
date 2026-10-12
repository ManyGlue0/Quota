//! Devin: daily and weekly quota and the extra-usage balance, from the
//! account service the Devin CLI and app use (`GetUserStatus`), with the key
//! the Devin CLI keeps in `credentials.toml` or the Devin app's state database
//! (opened read-only).

use std::path::PathBuf;

use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};

use super::*;

const DEFAULT_SERVER: &str = "https://server.codeium.com";
const SERVICE: &str = "exa.seat_management_pb.SeatManagementService";
const COMPAT_VERSION: &str = "1.108.2";
const SIGN_IN: &str = "Run devin auth login, or sign in to the Devin app";
const DAY_MS: i64 = 24 * 3600 * 1000;

#[derive(Debug, Clone, PartialEq)]
struct Auth {
    key: String,
    server: Option<String>,
}

fn credentials_files() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(h) = home() {
        out.push(h.join(".local").join("share").join("devin").join("credentials.toml"));
    }
    for var in ["APPDATA", "LOCALAPPDATA"] {
        if let Some(d) = std::env::var_os(var) {
            out.push(PathBuf::from(d).join("devin").join("credentials.toml"));
        }
    }
    out
}

fn app_db() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("APPDATA")?).join("Devin").join("User").join("globalStorage").join("state.vscdb"))
}

fn cli_auth() -> Option<Auth> {
    credentials_files().into_iter().find_map(|p| {
        let text = std::fs::read_to_string(p).ok()?;
        let key = toml_string(&text, "windsurf_api_key")?;
        let server = toml_string(&text, "api_server_url")
            .map(|s| s.trim_end_matches('/').to_string())
            .filter(|s| s.starts_with("https://") && s.len() > "https://".len());
        Some(Auth { key, server })
    })
}

fn app_auth() -> Option<Auth> {
    let path = app_db().filter(|p| p.exists())?;
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX).ok()?;
    let _ = conn.busy_timeout(std::time::Duration::from_millis(1500));
    let value: String = conn.query_row("SELECT value FROM ItemTable WHERE key = 'windsurfAuthStatus' LIMIT 1", [], |r| r.get(0)).ok()?;
    let doc: Value = serde_json::from_str(&value).ok()?;
    let key = doc["apiKey"].as_str().map(str::trim).filter(|k| !k.is_empty())?.to_string();
    Some(Auth { key, server: None })
}

/// `key = "value"` (or bare, with a trailing comment) on one line.
fn toml_string(text: &str, key: &str) -> Option<String> {
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else { continue };
        if k.trim() != key {
            continue;
        }
        let v = v.trim();
        let value = match v.chars().next()? {
            q @ ('"' | '\'') => v[1..].split(q).next().unwrap_or("").trim().to_string(),
            _ => v.split('#').next().unwrap_or("").trim().to_string(),
        };
        return Some(value).filter(|s| !s.is_empty());
    }
    None
}

pub fn has_login() -> bool {
    cli_auth().is_some() || app_auth().is_some()
}

enum Attempt {
    Ok(Usage),
    Rejected,
    Unavailable(FetchError),
}

async fn attempt(auth: &Auth) -> Attempt {
    let server = auth.server.as_deref().unwrap_or(DEFAULT_SERVER);
    let body = json!({ "metadata": {
        "apiKey": auth.key, "ideName": "devin", "ideVersion": COMPAT_VERSION,
        "extensionName": "devin", "extensionVersion": COMPAT_VERSION, "locale": "en"
    }});
    let resp = match client()
        .post(format!("{server}/{SERVICE}/GetUserStatus"))
        .header("Content-Type", "application/json")
        .header("Connect-Protocol-Version", "1")
        .json(&body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(_) => return Attempt::Unavailable(FetchError::failed("Couldn't reach Devin")),
    };
    let status = resp.status().as_u16();
    if status == 429 {
        return Attempt::Unavailable(FetchError::RateLimited { retry_after_secs: retry_after(&resp) });
    }
    if matches!(status, 401 | 403) {
        return Attempt::Rejected;
    }
    if !(200..300).contains(&status) {
        return Attempt::Unavailable(FetchError::Failed(format!("Devin answered {status}")));
    }
    match resp.json::<Value>().await.map_err(|_| FetchError::failed("Devin sent an unreadable answer")).and_then(|b| map_status(&b["userStatus"])) {
        Ok(u) => Attempt::Ok(u),
        Err(e) => Attempt::Unavailable(e),
    }
}

/// The CLI's login first; the app's if the CLI's fails and it's a different one.
pub async fn fetch() -> FetchResult {
    let cli = cli_auth();
    let mut last = None;
    if let Some(a) = &cli {
        match attempt(a).await {
            Attempt::Ok(u) => return Ok(u),
            Attempt::Rejected => last = Some(FetchError::SignedOut(SIGN_IN.into())),
            Attempt::Unavailable(e) => last = Some(e),
        }
    }
    if let Some(a) = app_auth().filter(|a| cli.as_ref() != Some(a)) {
        match attempt(&a).await {
            Attempt::Ok(u) => return Ok(u),
            Attempt::Rejected => last = Some(FetchError::SignedOut(SIGN_IN.into())),
            Attempt::Unavailable(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| FetchError::SignedOut(SIGN_IN.into())))
}

/// Quotas arrive as percent *remaining*. Proto-JSON drops zeros: a weekly
/// reset with no weekly percent means 0% left.
pub fn map_status(status: &Value) -> FetchResult {
    let invalid = || FetchError::failed("Devin's quota answer changed");
    if !status.is_object() {
        return Err(invalid());
    }
    let ps = &status["planStatus"];
    let info = &ps["planInfo"];
    let plan = info["planName"].as_str().map(str::trim).filter(|p| !p.is_empty()).map(str::to_string);
    let hide_daily = info["hideDailyQuota"].as_bool() == Some(true);
    let daily_left = num(ps.get("dailyQuotaRemainingPercent"));
    let weekly_left = num(ps.get("weeklyQuotaRemainingPercent"));
    if ps.get("weeklyQuotaRemainingPercent").is_some() && weekly_left.is_none() {
        return Err(invalid());
    }
    let secs = |k: &str| num(ps.get(k)).map(|s| (s * 1000.0) as i64);
    let weekly_reset = secs("weeklyQuotaResetAtUnix");
    let mut out = Vec::new();
    if !hide_daily {
        if let Some(left) = daily_left {
            out.push(Metric::percent("daily", "Daily", 100.0 - left, secs("dailyQuotaResetAtUnix"), Some(DAY_MS)));
        }
    }
    if let Some(left) = weekly_left.or(weekly_reset.map(|_| 0.0)).or(if hide_daily { daily_left } else { None }) {
        out.push(Metric::percent("weekly", "Weekly", 100.0 - left, weekly_reset, Some(WEEK_MS)));
    }
    if let Some(micros) = num(ps.get("overageBalanceMicros")) {
        out.push(Metric::money("extra", "Extra usage balance", micros.max(0.0) / 1e6, None, "USD"));
    }
    if out.is_empty() {
        return Err(FetchError::failed("Devin didn't report a quota"));
    }
    Ok(Usage { plan, metrics: out })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daily_weekly_and_balance() {
        let s = json!({ "planStatus": {
            "planInfo": { "planName": "Pro" },
            "dailyQuotaRemainingPercent": 80, "dailyQuotaResetAtUnix": 1791000000,
            "weeklyQuotaRemainingPercent": 55, "weeklyQuotaResetAtUnix": 1791500000,
            "overageBalanceMicros": "12500000"
        }});
        let u = map_status(&s).unwrap();
        assert_eq!(u.plan.as_deref(), Some("Pro"));
        let got: Vec<_> = u.metrics.iter().map(|m| (m.id.as_str(), m.used)).collect();
        assert_eq!(got, [("daily", 20.0), ("weekly", 45.0), ("extra", 12.5)]);
        assert_eq!(u.metrics[1].resets_at, Some(1791500000000));
    }

    #[test]
    fn omitted_zeros_and_hidden_daily() {
        let exhausted = json!({ "planStatus": { "weeklyQuotaResetAtUnix": 1791500000 } });
        assert_eq!(map_status(&exhausted).unwrap().metrics[0].used, 100.0);
        let hidden = json!({ "planStatus": { "planInfo": { "hideDailyQuota": true }, "dailyQuotaRemainingPercent": 30 } });
        let m = map_status(&hidden).unwrap().metrics;
        assert_eq!((m.len(), m[0].id.as_str(), m[0].used), (1, "weekly", 70.0));
        assert!(map_status(&json!({ "planStatus": {} })).is_err());
    }

    #[test]
    fn toml_values() {
        let t = "windsurf_api_key = \"sk-1\"\napi_server_url = https://x.example # comment\n";
        assert_eq!(toml_string(t, "windsurf_api_key").as_deref(), Some("sk-1"));
        assert_eq!(toml_string(t, "api_server_url").as_deref(), Some("https://x.example"));
    }
}
