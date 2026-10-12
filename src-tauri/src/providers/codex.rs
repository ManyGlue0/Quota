//! Codex: ChatGPT/Codex rate-limit windows, read with the login the Codex CLI
//! keeps in `~/.codex/auth.json` (or `$CODEX_HOME`).
//!
//! Windows are classified by their length, not their slot: Codex moves a
//! sole limit into the primary slot, and free plans get one 30-day window.
//! The token is renewed only near its JWT expiry (or on a 401), and written
//! back only if `auth.json` still holds the login we started from.

use std::path::PathBuf;

use serde_json::{json, Value};

use super::*;

const USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
const REFRESH_URL: &str = "https://auth.openai.com/oauth/token";
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const SIGN_IN: &str = "Run codex in a terminal and sign in with ChatGPT";
const EXPIRED: &str = "Your Codex login expired. Run codex to sign in again";

struct Login {
    path: PathBuf,
    raw: String,
    doc: Value,
}

impl Login {
    fn access_token(&self) -> Option<&str> {
        self.doc["tokens"]["access_token"].as_str().filter(|t| !t.is_empty())
    }
    fn account_id(&self) -> Option<&str> {
        self.doc["tokens"]["account_id"].as_str().filter(|t| !t.is_empty())
    }
}

fn auth_path() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CODEX_HOME").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(dir).join("auth.json"));
    }
    Some(home()?.join(".codex").join("auth.json"))
}

fn load() -> Result<Login, FetchError> {
    let path = auth_path().ok_or_else(|| FetchError::SignedOut(SIGN_IN.into()))?;
    let raw = std::fs::read_to_string(&path).map_err(|_| FetchError::SignedOut(SIGN_IN.into()))?;
    let doc: Value = serde_json::from_str(&raw).map_err(|_| FetchError::SignedOut(SIGN_IN.into()))?;
    let login = Login { path, raw, doc };
    if login.access_token().is_none() {
        // An API-key-only setup has no subscription limits to read.
        return Err(FetchError::SignedOut(SIGN_IN.into()));
    }
    Ok(login)
}

pub fn has_login() -> bool {
    load().is_ok()
}

pub async fn fetch() -> FetchResult {
    let mut login = load()?;
    let near_expiry = login.access_token().and_then(jwt_exp_ms).is_some_and(|exp| exp <= now_ms() + 60_000);
    if near_expiry {
        refresh(&mut login).await?;
    }
    let mut resp = request_usage(&login).await?;
    if matches!(resp.status().as_u16(), 401 | 403) && !near_expiry {
        refresh(&mut login).await?;
        resp = request_usage(&login).await?;
    }
    let status = resp.status();
    if status.as_u16() == 429 {
        return Err(FetchError::RateLimited { retry_after_secs: retry_after(&resp) });
    }
    if matches!(status.as_u16(), 401 | 403) {
        return Err(FetchError::SignedOut(EXPIRED.into()));
    }
    if !status.is_success() {
        return Err(FetchError::Failed(format!("Codex answered {}", status.as_u16())));
    }
    let body: Value = resp.json().await.map_err(|_| FetchError::failed("Codex sent an unreadable answer"))?;
    Ok(map_usage(&body, now_ms()))
}

async fn request_usage(login: &Login) -> Result<reqwest::Response, FetchError> {
    let mut req = client()
        .get(USAGE_URL)
        .bearer_auth(login.access_token().unwrap_or_default())
        .header("Accept", "application/json")
        .header("User-Agent", "Quota");
    if let Some(id) = login.account_id() {
        req = req.header("ChatGPT-Account-Id", id);
    }
    req.send().await.map_err(|_| FetchError::failed("Couldn't reach Codex"))
}

async fn refresh(login: &mut Login) -> Result<(), FetchError> {
    let Some(refresh_token) = login.doc["tokens"]["refresh_token"].as_str().map(str::to_string) else {
        return Err(FetchError::SignedOut(EXPIRED.into()));
    };
    let resp = client()
        .post(REFRESH_URL)
        .form(&[("grant_type", "refresh_token"), ("client_id", CLIENT_ID), ("refresh_token", refresh_token.as_str())])
        .send()
        .await
        .map_err(|_| FetchError::failed("Couldn't reach Codex"))?;
    let status = resp.status().as_u16();
    if matches!(status, 400 | 401) {
        return Err(FetchError::SignedOut(EXPIRED.into()));
    }
    if !resp.status().is_success() {
        return Err(FetchError::Failed(format!("Codex sign-in answered {status}")));
    }
    let body: Value = resp.json().await.map_err(|_| FetchError::failed("Codex sent an unreadable answer"))?;
    let Some(access) = body["access_token"].as_str().filter(|t| !t.is_empty()) else {
        return Err(FetchError::SignedOut(EXPIRED.into()));
    };

    let tokens = &mut login.doc["tokens"];
    tokens["access_token"] = json!(access);
    for key in ["refresh_token", "id_token"] {
        if let Some(v) = body[key].as_str() {
            tokens[key] = json!(v);
        }
    }
    login.doc["last_refresh"] = json!(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true));
    let text = serde_json::to_string_pretty(&login.doc).unwrap_or_default();
    match write_back_if_unchanged(&login.path, &login.raw, &text) {
        Ok(true) => login.raw = text,
        Ok(false) => eprintln!("codex: auth.json changed while refreshing; left it as it is"),
        Err(e) => eprintln!("codex: couldn't save the renewed login: {e}"),
    }
    Ok(())
}

fn window_metric(w: &Value, fallback: (&str, &str), now: i64) -> Option<Metric> {
    if !w.is_object() {
        return None;
    }
    let period = num(w.get("limit_window_seconds")).map(|s| (s * 1000.0) as i64);
    let (id, label) = window_by_duration(period).unwrap_or(fallback);
    let used = num(w.get("used_percent")).unwrap_or(0.0);
    let resets = epoch_ms(w.get("reset_at"))
        .or_else(|| num(w.get("reset_after_seconds")).map(|s| now + (s * 1000.0) as i64));
    Some(Metric::percent(id, label, used, resets, period))
}

pub fn map_usage(body: &Value, now: i64) -> Usage {
    let mut metrics = Vec::new();
    let rl = &body["rate_limit"];
    for (key, fallback) in [("primary_window", ("session", "Session")), ("secondary_window", ("weekly", "Weekly"))] {
        if let Some(m) = window_metric(&rl[key], fallback, now) {
            if !metrics.iter().any(|x: &Metric| x.id == m.id) {
                metrics.push(m);
            }
        }
    }

    // Model-specific limits (e.g. Codex Spark), each with its own windows.
    for extra in body["additional_rate_limits"].as_array().into_iter().flatten() {
        let name = extra["limit_name"].as_str().or(extra["metered_feature"].as_str()).unwrap_or("");
        if name.is_empty() {
            continue;
        }
        let pretty = title_case(name.trim_start_matches("codex_").trim_start_matches("gpt_"));
        for key in ["primary_window", "secondary_window"] {
            if let Some(mut m) = window_metric(&extra["rate_limit"][key], ("session", "Session"), now) {
                let slug = name.to_lowercase().replace([' ', '_'], "-");
                m.id = format!("{slug}-{}", m.id);
                m.label = if m.label == "Session" { pretty.clone() } else { format!("{pretty} {}", m.label.to_lowercase()) };
                m.primary = false;
                metrics.push(m);
            }
        }
    }

    // On-demand rate-limit reset credits.
    if let Some(n) = num(body["rate_limit_reset_credits"].get("available_count")) {
        metrics.push(Metric::count("resets", "Limit resets", n.floor().max(0.0), "available"));
    }

    // Flex credits, worth 4¢ each.
    let credits = &body["credits"];
    if let Some(balance) = num(credits.get("balance")) {
        if balance > 0.0 {
            metrics.push(Metric::money("credits", "Credits", balance.floor() * 0.04, None, "USD"));
        }
    }

    Usage { plan: body["plan_type"].as_str().map(plan_name).filter(|p| !p.is_empty()), metrics }
}

fn plan_name(raw: &str) -> String {
    match raw.trim().to_lowercase().as_str() {
        "prolite" => "Pro 100".into(),
        "pro" => "Pro 200".into(),
        "promax" => "Pro 500".into(),
        "self_serve_business_prolite" => "Business Premium".into(),
        _ => title_case(raw.trim()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_plan_monthly_window() {
        let body = json!({
            "plan_type": "free",
            "rate_limit": {
                "primary_window": { "used_percent": 100, "limit_window_seconds": 2592000, "reset_after_seconds": 2155664, "reset_at": 1793726480 },
                "secondary_window": null
            },
            "additional_rate_limits": null,
            "credits": { "has_credits": false, "balance": null },
            "rate_limit_reset_credits": { "available_count": 1 }
        });
        let u = map_usage(&body, 0);
        assert_eq!(u.plan.as_deref(), Some("Free"));
        assert_eq!(u.metrics.len(), 2);
        assert_eq!(u.metrics[0].id, "monthly");
        assert_eq!(u.metrics[0].used, 100.0);
        assert_eq!(u.metrics[0].resets_at, Some(1793726480000));
        assert_eq!(u.metrics[1].id, "resets");
        assert_eq!(u.metrics[1].used, 1.0);
    }

    #[test]
    fn paid_plan_session_and_weekly() {
        let body = json!({
            "plan_type": "pro",
            "rate_limit": {
                "primary_window": { "used_percent": 12, "limit_window_seconds": 18000, "reset_after_seconds": 600 },
                "secondary_window": { "used_percent": 40, "limit_window_seconds": 604800, "reset_at": 1793726480 }
            },
            "additional_rate_limits": [
                { "limit_name": "codex_spark", "rate_limit": { "primary_window": { "used_percent": 5, "limit_window_seconds": 18000 } } }
            ],
            "credits": { "balance": 796.4 }
        });
        let u = map_usage(&body, 1_000);
        let ids: Vec<_> = u.metrics.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["session", "weekly", "codex-spark-session", "credits"]);
        assert_eq!(u.metrics[0].resets_at, Some(601_000));
        assert_eq!(u.metrics[2].label, "Spark");
        assert!((u.metrics[3].used - 31.84).abs() < 1e-9);
        assert_eq!(u.plan.as_deref(), Some("Pro 200"));
    }
}
