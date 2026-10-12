//! OpenRouter: credits and spend from OpenRouter's public API, with an API
//! key you paste in Settings (kept in Windows Credential Manager) or set as
//! `OPENROUTER_API_KEY`.

use serde_json::Value;

use super::*;
use crate::secrets;

const CREDITS_URL: &str = "https://openrouter.ai/api/v1/credits";
const KEY_URL: &str = "https://openrouter.ai/api/v1/key";
pub const ENV: &[&str] = &["OPENROUTER_API_KEY"];
const NO_KEY: &str = "Add an OpenRouter API key in Settings";

pub fn has_login() -> bool {
    secrets::api_key("openrouter", ENV).0.is_some()
}

async fn get(url: &str, key: &str) -> Result<reqwest::Response, FetchError> {
    client()
        .get(url)
        .bearer_auth(key)
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|_| FetchError::failed("Couldn't reach OpenRouter"))
}

pub async fn fetch() -> FetchResult {
    let key = secrets::api_key("openrouter", ENV).0.ok_or_else(|| FetchError::SignedOut(NO_KEY.into()))?;
    let resp = get(CREDITS_URL, &key).await?;
    let status = resp.status().as_u16();
    if status == 429 {
        return Err(FetchError::RateLimited { retry_after_secs: retry_after(&resp) });
    }
    if matches!(status, 401 | 403) {
        return Err(FetchError::SignedOut("OpenRouter didn't accept the API key. Check it at openrouter.ai/keys".into()));
    }
    if !resp.status().is_success() {
        return Err(FetchError::Failed(format!("OpenRouter answered {status}")));
    }
    let credits: Value = resp.json().await.map_err(|_| FetchError::failed("OpenRouter sent an unreadable answer"))?;
    let mut metrics = map_credits(&credits["data"]).ok_or_else(|| FetchError::failed("OpenRouter's credits answer changed"))?;
    // Key details (tier, spend, a per-key cap) are a nicety.
    let mut plan = None;
    if let Ok(r) = get(KEY_URL, &key).await {
        if r.status().is_success() {
            if let Ok(body) = r.json::<Value>().await {
                let (p, extra) = map_key(&body["data"]);
                plan = p;
                metrics.extend(extra);
            }
        }
    }
    Ok(Usage { plan, metrics })
}

/// `/credits`: lifetime `total_credits` bought and `total_usage` spent.
pub fn map_credits(data: &Value) -> Option<Vec<Metric>> {
    let used = num(data.get("total_usage"))?.max(0.0);
    let bought = num(data.get("total_credits")).unwrap_or(0.0).max(0.0);
    let mut out = Vec::new();
    if bought > 0.0 {
        let mut m = Metric::money("credits", "Credits", used, Some(bought), "USD");
        m.primary = true;
        out.push(m);
    }
    out.push(Metric::money("balance", "Balance", (bought - used).max(0.0), None, "USD"));
    Some(out)
}

/// `/key`: spend today / this week / this month, and a cap if the key has one.
pub fn map_key(data: &Value) -> (Option<String>, Vec<Metric>) {
    let mut out = Vec::new();
    for (key, id, label) in [("usage_daily", "today", "Today"), ("usage_weekly", "week", "This week"), ("usage_monthly", "month", "This month")] {
        if let Some(v) = num(data.get(key)) {
            out.push(Metric::money(id, label, v.max(0.0), None, "USD"));
        }
    }
    if let Some(limit) = num(data.get("limit")).filter(|l| *l > 0.0) {
        let left = num(data.get("limit_remaining")).unwrap_or(0.0).max(0.0);
        out.push(Metric::money("key-limit", "Key limit", (limit - left).max(0.0), Some(limit), "USD"));
    }
    let plan = data["is_free_tier"].as_bool().map(|free| if free { "Free tier" } else { "Pay as you go" }.to_string());
    (plan, out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn credits_and_key() {
        let m = map_credits(&json!({ "total_credits": 50, "total_usage": 12.5 })).unwrap();
        assert_eq!((m[0].id.as_str(), m[0].fill()), ("credits", Some(25.0)));
        assert_eq!((m[1].id.as_str(), m[1].used), ("balance", 37.5));
        let free = map_credits(&json!({ "total_credits": 0, "total_usage": 0 })).unwrap();
        assert_eq!(free.len(), 1, "no meter without credits bought, the balance still shows");
        let (plan, k) = map_key(&json!({ "usage_daily": 0, "usage_weekly": 1.25, "usage_monthly": 4, "limit": 10, "limit_remaining": 7.5, "is_free_tier": false }));
        assert_eq!(plan.as_deref(), Some("Pay as you go"));
        let ids: Vec<_> = k.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["today", "week", "month", "key-limit"]);
        assert_eq!(k[3].fill(), Some(25.0));
    }
}
