//! Z.ai: the GLM Coding Plan's session and weekly quotas and monthly web
//! searches, with an API key you paste in Settings (kept in Windows Credential
//! Manager) or set as `ZAI_API_KEY` / `GLM_API_KEY`. The endpoints are the
//! ones Z.ai's own subscription page uses (undocumented, stable in practice).

use serde_json::Value;

use super::*;
use crate::secrets;

const QUOTA_URL: &str = "https://api.z.ai/api/monitor/usage/quota/limit";
const SUBSCRIPTION_URL: &str = "https://api.z.ai/api/biz/subscription/list";
pub const ENV: &[&str] = &["ZAI_API_KEY", "GLM_API_KEY"];
const NO_KEY: &str = "Add a Z.ai API key in Settings";
const DAY_MS: i64 = 24 * 3600 * 1000;

pub fn has_login() -> bool {
    secrets::api_key("zai", ENV).0.is_some()
}

async fn get(url: &str, key: &str) -> Result<reqwest::Response, FetchError> {
    client()
        .get(url)
        .bearer_auth(key)
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|_| FetchError::failed("Couldn't reach Z.ai"))
}

pub async fn fetch() -> FetchResult {
    let key = secrets::api_key("zai", ENV).0.ok_or_else(|| FetchError::SignedOut(NO_KEY.into()))?;
    let resp = get(QUOTA_URL, &key).await?;
    let status = resp.status().as_u16();
    if status == 429 {
        return Err(FetchError::RateLimited { retry_after_secs: retry_after(&resp) });
    }
    if matches!(status, 401 | 403) {
        return Err(FetchError::SignedOut("Z.ai didn't accept the API key. Make a new one in the Z.ai console".into()));
    }
    if !resp.status().is_success() {
        return Err(FetchError::Failed(format!("Z.ai answered {status}")));
    }
    let body: Value = resp.json().await.map_err(|_| FetchError::failed("Z.ai sent an unreadable answer"))?;
    if body["success"].as_bool() == Some(false) && body["msg"].as_str().unwrap_or("").to_lowercase().contains("coding plan") {
        return Err(FetchError::SignedOut("No active GLM Coding Plan on this key".into()));
    }
    let metrics = map_quota(&body)?;
    let plan = match get(SUBSCRIPTION_URL, &key).await {
        Ok(r) if r.status().is_success() => r.json::<Value>().await.ok().and_then(|b| {
            b["data"][0]["productName"].as_str().map(str::trim).filter(|p| !p.is_empty()).map(str::to_string)
        }),
        _ => None,
    };
    Ok(Usage { plan, metrics })
}

/// `data.limits`: `CREDIT_LIMIT` (older: `TOKENS_LIMIT`) entries are percent
/// windows, sorted by length into Session (under a day) and Weekly; a
/// `TIME_LIMIT` is the monthly web-search count. Windows come as
/// `(unit, number)`: 3 hours, 4 days, 6 weeks, 5 months.
pub fn map_quota(body: &Value) -> Result<Vec<Metric>, FetchError> {
    let invalid = || FetchError::failed("Z.ai's usage answer changed");
    let container = if body.get("data").is_some() { &body["data"] } else { body };
    let limits = container["limits"].as_array().ok_or_else(invalid)?;
    let kind = |e: &Value| e["type"].as_str().or(e["name"].as_str()).unwrap_or("").to_string();
    let mut out = Vec::new();
    for e in limits.iter().filter(|e| matches!(kind(e).as_str(), "CREDIT_LIMIT" | "TOKENS_LIMIT")) {
        let unit_ms = match num(e.get("unit")).ok_or_else(invalid)? as i64 {
            3 => 3600_000,
            4 => DAY_MS,
            6 => 7 * DAY_MS,
            5 => 30 * DAY_MS,
            _ => continue,
        };
        let n = num(e.get("number")).filter(|n| *n > 0.0).ok_or_else(invalid)?;
        let period = (unit_ms as f64 * n) as i64;
        let used = num(e.get("percentage")).ok_or_else(invalid)?;
        let resets = epoch_ms(e.get("nextResetTime"));
        let (id, label) = if period < DAY_MS { ("session", "Session") } else { ("weekly", "Weekly") };
        out.push(Metric::percent(id, label, used, resets, Some(period)));
    }
    if let Some(e) = limits.iter().find(|e| kind(e) == "TIME_LIMIT") {
        let used = num(e.get("currentValue")).filter(|v| *v >= 0.0).ok_or_else(invalid)?;
        let limit = num(e.get("usage")).filter(|v| *v >= 0.0).ok_or_else(invalid)?;
        out.push(Metric::count_of("searches", "Web searches", used, limit, "searches").with_reset(epoch_ms(e.get("nextResetTime")), Some(30 * DAY_MS)));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn session_weekly_and_searches() {
        let body = json!({ "success": true, "data": { "limits": [
            { "type": "CREDIT_LIMIT", "unit": 3, "number": 5, "percentage": 34, "nextResetTime": 1770648402389u64 },
            { "type": "TOKENS_LIMIT", "unit": 6, "number": 1, "percentage": 12 },
            { "type": "TIME_LIMIT", "unit": 5, "number": 1, "currentValue": 120, "usage": 1000, "nextResetTime": 1770648402389u64 }
        ]}});
        let m = map_quota(&body).unwrap();
        let ids: Vec<_> = m.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["session", "weekly", "searches"]);
        assert_eq!((m[0].used, m[0].period_ms, m[0].resets_at), (34.0, Some(FIVE_HOURS_MS), Some(1770648402389)));
        assert_eq!(m[1].period_ms, Some(WEEK_MS));
        assert_eq!(m[2].fill(), Some(12.0));
        assert!(map_quota(&json!({ "data": { "limits": [] } })).unwrap().is_empty());
        assert!(map_quota(&json!({ "data": {} })).is_err());
    }
}
