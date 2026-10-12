//! Grok: the weekly credit pool, read with the login the Grok CLI keeps in
//! `~/.grok/auth.json` (or `$GROK_HOME`).
//!
//! The billing call is the one the Grok CLI itself makes
//! (`/v1/billing?format=credits`). Grok's access tokens last six hours, so an
//! expired one is renewed with the CLI's refresh token and written back into
//! `auth.json`, touching only the token fields of that login, and only if the
//! file still holds what Quota read (so a login the CLI rotated meanwhile is
//! never overwritten).

use std::path::PathBuf;

use serde_json::{json, Value};

use super::*;

const BILLING_URL: &str = "https://cli-chat-proxy.grok.com/v1/billing?format=credits";
const SETTINGS_URL: &str = "https://cli-chat-proxy.grok.com/v1/settings";
const REFRESH_URL: &str = "https://auth.x.ai/oauth2/token";
const DEFAULT_CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
const SIGN_IN: &str = "Run grok login in a terminal";
const EXPIRED: &str = "Your Grok login expired. Run grok login again";
/// Renew a little before the token runs out.
const REFRESH_EARLY_MS: i64 = 5 * 60_000;

fn auth_path() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("GROK_HOME").filter(|d| !d.to_string_lossy().trim().is_empty()) {
        return Some(PathBuf::from(dir).join("auth.json"));
    }
    Some(home()?.join(".grok").join("auth.json"))
}

struct Login {
    path: PathBuf,
    raw: String,
    doc: Value,
    /// The entry in `auth.json` ("https://auth.x.ai::<client id>").
    key: String,
}

impl Login {
    fn entry(&self) -> &Value {
        &self.doc[&self.key]
    }
    fn token(&self) -> &str {
        self.entry()["key"].as_str().map(str::trim).unwrap_or("")
    }
    fn expires_ms(&self) -> Option<i64> {
        jwt_exp_ms(self.token()).or_else(|| {
            let e = self.entry();
            e["expires_at"].as_str().or(e["expires"].as_str()).and_then(|s| epoch_ms(Some(&json!(s))))
        })
    }
    fn client_id(&self) -> String {
        let e = self.entry();
        e["oidc_client_id"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(|| self.key.rsplit("::").next().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string))
            .unwrap_or_else(|| DEFAULT_CLIENT_ID.into())
    }
}

/// Logins with a token, in file order.
fn load() -> Result<Vec<Login>, FetchError> {
    let path = auth_path().ok_or_else(|| FetchError::SignedOut(SIGN_IN.into()))?;
    let raw = std::fs::read_to_string(&path).map_err(|_| FetchError::SignedOut(SIGN_IN.into()))?;
    let doc: Value = serde_json::from_str(&raw).map_err(|_| FetchError::SignedOut(SIGN_IN.into()))?;
    let keys: Vec<String> = doc
        .as_object()
        .map(|o| o.iter().filter(|(_, e)| e["key"].as_str().is_some_and(|k| !k.trim().is_empty())).map(|(k, _)| k.clone()).collect())
        .unwrap_or_default();
    if keys.is_empty() {
        return Err(FetchError::SignedOut(SIGN_IN.into()));
    }
    Ok(keys.into_iter().map(|key| Login { path: path.clone(), raw: raw.clone(), doc: doc.clone(), key }).collect())
}

pub fn has_login() -> bool {
    load().is_ok()
}

pub async fn fetch() -> FetchResult {
    let mut expired = false;
    for mut login in load()? {
        let stale = login.expires_ms().is_some_and(|exp| exp <= now_ms() + REFRESH_EARLY_MS);
        if stale {
            // A token with minutes left still works if renewing fails.
            if let Err(e) = refresh(&mut login).await {
                if login.expires_ms().is_some_and(|exp| exp <= now_ms()) {
                    if matches!(e, FetchError::Failed(_)) {
                        return Err(e);
                    }
                    expired = true;
                    continue;
                }
            }
        }
        let mut resp = request(BILLING_URL, login.token()).await?;
        if matches!(resp.status().as_u16(), 401 | 403) && !stale {
            refresh(&mut login).await?;
            resp = request(BILLING_URL, login.token()).await?;
        }
        let status = resp.status().as_u16();
        if status == 429 {
            return Err(FetchError::RateLimited { retry_after_secs: retry_after(&resp) });
        }
        if matches!(status, 401 | 403) {
            return Err(FetchError::SignedOut(EXPIRED.into()));
        }
        let plan = plan(login.token()).await;
        let body: Value = resp.json().await.unwrap_or(Value::Null);
        // A team login has no personal pool: show the plan, spend still comes from the logs.
        if status == 412 && body["error"].as_str().is_some_and(|e| e.to_lowercase().contains("no personal team")) {
            return Ok(Usage { plan, metrics: Vec::new() });
        }
        if !(200..300).contains(&status) {
            return Err(FetchError::Failed(format!("Grok answered {status}")));
        }
        return Ok(Usage { plan, metrics: map_billing(&body)? });
    }
    Err(FetchError::SignedOut(if expired { EXPIRED } else { SIGN_IN }.into()))
}

async fn request(url: &str, token: &str) -> Result<reqwest::Response, FetchError> {
    client()
        .get(url)
        .bearer_auth(token)
        .header("X-XAI-Token-Auth", "xai-grok-cli")
        .header("Accept", "application/json")
        .header("User-Agent", "Quota")
        .send()
        .await
        .map_err(|_| FetchError::failed("Couldn't reach Grok"))
}

async fn plan(token: &str) -> Option<String> {
    let resp = request(SETTINGS_URL, token).await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body: Value = resp.json().await.ok()?;
    body["subscription_tier_display"].as_str().map(str::trim).filter(|p| !p.is_empty()).map(str::to_string)
}

async fn refresh(login: &mut Login) -> Result<(), FetchError> {
    let entry = login.entry();
    let Some(refresh_token) = entry["refresh_token"].as_str().or(entry["refresh"].as_str()).map(str::trim).filter(|t| !t.is_empty()).map(str::to_string) else {
        return Err(FetchError::SignedOut(EXPIRED.into()));
    };
    let client_id = login.client_id();
    let resp = client()
        .post(REFRESH_URL)
        .form(&[("grant_type", "refresh_token"), ("client_id", client_id.as_str()), ("refresh_token", refresh_token.as_str())])
        .send()
        .await
        .map_err(|_| FetchError::failed("Couldn't reach Grok"))?;
    if !resp.status().is_success() {
        return Err(FetchError::SignedOut(EXPIRED.into()));
    }
    let body: Value = resp.json().await.map_err(|_| FetchError::failed("Grok sent an unreadable answer"))?;
    let Some(access) = body["access_token"].as_str().map(str::trim).filter(|t| !t.is_empty()) else {
        return Err(FetchError::SignedOut(EXPIRED.into()));
    };
    let expires = num(body.get("expires_in"))
        .filter(|s| *s > 0.0)
        .map(|s| now_ms() + (s * 1000.0) as i64)
        .or_else(|| jwt_exp_ms(access))
        .unwrap_or(now_ms() + 3600_000);

    let e = &mut login.doc[&login.key];
    e["key"] = json!(access);
    for (field, name) in [("refresh_token", "refresh_token"), ("id_token", "id_token")] {
        if let Some(v) = body[field].as_str().map(str::trim).filter(|v| !v.is_empty()) {
            e[name] = json!(v);
        }
    }
    if let Some(at) = chrono::DateTime::from_timestamp_millis(expires) {
        e["expires_at"] = json!(at.to_rfc3339_opts(chrono::SecondsFormat::Micros, true));
    }
    let text = serde_json::to_string_pretty(&login.doc).unwrap_or_default();
    match write_back_if_unchanged(&login.path, &login.raw, &text) {
        Ok(true) => login.raw = text,
        Ok(false) => eprintln!("grok: auth.json changed while refreshing; left it as it is"),
        Err(e) => eprintln!("grok: couldn't save the renewed login: {e}"),
    }
    Ok(())
}

/// `{"config":{"creditUsagePercent":99,"currentPeriod":{"type":"USAGE_PERIOD_TYPE_WEEKLY","start":…,"end":…},"onDemandCap":{"val":2500}}}`.
/// Proto-JSON drops zero values: a missing percent is 0%, a missing cap is off.
pub fn map_billing(body: &Value) -> Result<Vec<Metric>, FetchError> {
    let invalid = || FetchError::failed("Grok's billing answer changed");
    let config = &body["config"];
    let period = &config["currentPeriod"];
    let kind = period["type"].as_str().map(str::trim).filter(|t| !t.is_empty()).ok_or_else(invalid)?;
    let start = epoch_ms(period.get("start")).ok_or_else(invalid)?;
    let end = epoch_ms(period.get("end")).filter(|e| *e > start).ok_or_else(invalid)?;
    let used = match config.get("creditUsagePercent") {
        None => 0.0,
        Some(v) => num(Some(v)).ok_or_else(invalid)?,
    };
    let mut out = Vec::new();
    // An account still on monthly billing has no weekly pool; better blank than mislabeled.
    if kind == "USAGE_PERIOD_TYPE_WEEKLY" {
        out.push(Metric::percent("weekly", "Weekly", used, Some(end), Some(end - start)));
    }
    let cap = num(config["onDemandCap"].get("val")).unwrap_or(0.0);
    if cap > 0.0 {
        out.push(Metric::count("payg", "Pay as you go", cap, "cap"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weekly_pool_and_cap() {
        let body = json!({ "config": {
            "creditUsagePercent": 99.0,
            "currentPeriod": { "type": "USAGE_PERIOD_TYPE_WEEKLY", "start": "2026-07-03T04:01:09.238389+00:00", "end": "2026-07-10T04:01:09.238389+00:00" },
            "onDemandCap": { "val": 2500 }
        }});
        let m = map_billing(&body).unwrap();
        assert_eq!(m[0].id, "weekly");
        assert_eq!(m[0].used, 99.0);
        assert_eq!(m[0].period_ms, Some(WEEK_MS));
        assert_eq!((m[1].id.as_str(), m[1].used), ("payg", 2500.0));
    }

    #[test]
    fn zero_usage_and_monthly_accounts() {
        let weekly = json!({ "config": { "currentPeriod": { "type": "USAGE_PERIOD_TYPE_WEEKLY", "start": "2026-07-03T00:00:00Z", "end": "2026-07-10T00:00:00Z" } } });
        let m = map_billing(&weekly).unwrap();
        assert_eq!((m.len(), m[0].used), (1, 0.0), "an absent percent is 0%, an absent cap is off");
        let monthly = json!({ "config": { "creditUsagePercent": 40, "currentPeriod": { "type": "USAGE_PERIOD_TYPE_MONTHLY", "start": "2026-07-01T00:00:00Z", "end": "2026-08-01T00:00:00Z" } } });
        assert!(map_billing(&monthly).unwrap().is_empty());
        assert!(map_billing(&json!({ "config": {} })).is_err());
    }

    #[test]
    fn client_id_from_the_entry_key() {
        let doc = json!({ "https://auth.x.ai::abc": { "key": "t" } });
        let l = Login { path: PathBuf::new(), raw: String::new(), doc, key: "https://auth.x.ai::abc".into() };
        assert_eq!(l.client_id(), "abc");
    }
}

#[cfg(test)]
mod real {
    /// `cargo test real_grok -- --ignored --nocapture`: asks Grok with this machine's login.
    #[tokio::test]
    #[ignore]
    async fn real_grok() {
        match super::fetch().await {
            Ok(u) => println!("plan {:?}: {:?}", u.plan, u.metrics.iter().map(|m| (&m.id, m.used, m.resets_at)).collect::<Vec<_>>()),
            Err(e) => println!("error: {e:?}"),
        }
    }
}
