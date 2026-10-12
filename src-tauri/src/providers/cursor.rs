//! Cursor: plan usage for the billing cycle, read with the session the Cursor
//! app keeps in its state database
//! (`%APPDATA%\Cursor\User\globalStorage\state.vscdb`).
//!
//! The database is opened read-only and never written: Cursor owns it while
//! it runs. A renewed access token is kept in memory instead.

use std::path::PathBuf;
use std::sync::Mutex;

use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};

use super::*;

const API: &str = "https://api2.cursor.sh";
const CLIENT_ID: &str = "KbZUR41cY7W6zRSdpSUJ7I7mLYBKOCmB";
const SIGN_IN: &str = "Open Cursor and sign in";
const EXPIRED: &str = "Your Cursor session expired. Open Cursor to sign in again";

/// (refresh token it came from, renewed access token).
static RENEWED: Mutex<Option<(String, String)>> = Mutex::new(None);

fn db_path() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(appdata).join("Cursor").join("User").join("globalStorage").join("state.vscdb"))
}

pub fn has_login() -> bool {
    db_path().is_some_and(|p| p.exists())
}

fn read_tokens() -> Result<(Option<String>, Option<String>), FetchError> {
    let path = db_path().filter(|p| p.exists()).ok_or_else(|| FetchError::SignedOut(SIGN_IN.into()))?;
    let conn = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
        .map_err(|_| FetchError::failed("Couldn't read Cursor's login"))?;
    let _ = conn.busy_timeout(std::time::Duration::from_millis(1500));
    let get = |key: &str| -> Option<String> {
        conn.query_row("SELECT value FROM ItemTable WHERE key = ?1 LIMIT 1", [key], |r| r.get::<_, String>(0))
            .ok()
            .map(|v| v.trim().trim_matches('"').to_string())
            .filter(|v| !v.is_empty())
    };
    Ok((get("cursorAuth/accessToken"), get("cursorAuth/refreshToken")))
}

fn fresh(token: &str) -> bool {
    jwt_exp_ms(token).is_some_and(|exp| exp > now_ms() + 5 * 60_000)
}

/// A usable access token: Cursor's own while it's fresh, else one we renewed.
async fn access_token() -> Result<String, FetchError> {
    let (access, refresh) = read_tokens()?;
    if let Some(a) = access.as_deref().filter(|a| fresh(a)) {
        return Ok(a.to_string());
    }
    let Some(refresh) = refresh else {
        return Err(FetchError::SignedOut(SIGN_IN.into()));
    };
    if let Some((from, renewed)) = RENEWED.lock().unwrap().clone() {
        if from == refresh && fresh(&renewed) {
            return Ok(renewed);
        }
    }
    let resp = client()
        .post(format!("{API}/oauth/token"))
        .json(&json!({ "grant_type": "refresh_token", "client_id": CLIENT_ID, "refresh_token": refresh }))
        .send()
        .await
        .map_err(|_| FetchError::failed("Couldn't reach Cursor"))?;
    if !resp.status().is_success() {
        return Err(FetchError::SignedOut(EXPIRED.into()));
    }
    let body: Value = resp.json().await.map_err(|_| FetchError::failed("Cursor sent an unreadable answer"))?;
    if body["shouldLogout"].as_bool() == Some(true) {
        return Err(FetchError::SignedOut(EXPIRED.into()));
    }
    let token = body["access_token"].as_str().filter(|t| !t.is_empty()).ok_or_else(|| FetchError::SignedOut(EXPIRED.into()))?;
    *RENEWED.lock().unwrap() = Some((refresh, token.to_string()));
    Ok(token.to_string())
}

async fn connect(method: &str, token: &str) -> Result<reqwest::Response, FetchError> {
    client()
        .post(format!("{API}/aiserver.v1.DashboardService/{method}"))
        .bearer_auth(token)
        .header("Content-Type", "application/json")
        .header("Connect-Protocol-Version", "1")
        .body("{}")
        .send()
        .await
        .map_err(|_| FetchError::failed("Couldn't reach Cursor"))
}

pub async fn fetch() -> FetchResult {
    let token = access_token().await?;
    let resp = connect("GetCurrentPeriodUsage", &token).await?;
    let status = resp.status().as_u16();
    if status == 429 {
        return Err(FetchError::RateLimited { retry_after_secs: retry_after(&resp) });
    }
    if matches!(status, 401 | 403) {
        *RENEWED.lock().unwrap() = None;
        return Err(FetchError::SignedOut(EXPIRED.into()));
    }
    if !resp.status().is_success() {
        return Err(FetchError::Failed(format!("Cursor answered {status}")));
    }
    let usage: Value = resp.json().await.map_err(|_| FetchError::failed("Cursor sent an unreadable answer"))?;

    // The plan name is a nicety: a failure here doesn't fail the fetch.
    let plan = match connect("GetPlanInfo", &token).await {
        Ok(r) if r.status().is_success() => r.json::<Value>().await.ok().and_then(|b| {
            b["planInfo"]["planName"].as_str().map(|p| title_case(p.trim())).filter(|p| !p.is_empty())
        }),
        _ => None,
    };
    let metrics = map_usage(&usage)?;
    Ok(Usage { plan, metrics })
}

/// The usage-events export (CSV) for `[start, end]` (epoch ms), with the
/// same login as the dashboard calls, presented as Cursor's session cookie.
pub async fn export_csv(start: i64, end: i64) -> Result<String, FetchError> {
    let token = access_token().await?;
    let sub = jwt_payload(&token).and_then(|p| p["sub"].as_str().map(str::to_string)).unwrap_or_default();
    let user = sub.rsplit('|').next().unwrap_or("").trim().to_string();
    if user.is_empty() {
        return Err(FetchError::SignedOut(SIGN_IN.into()));
    }
    let resp = client()
        .get("https://cursor.com/api/dashboard/export-usage-events-csv")
        .query(&[("startDate", start.to_string()), ("endDate", end.to_string()), ("strategy", "tokens".into())])
        .header("Cookie", format!("WorkosCursorSessionToken={user}%3A%3A{token}"))
        .header("Accept", "text/csv")
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .map_err(|_| FetchError::failed("Couldn't reach Cursor"))?;
    if !resp.status().is_success() {
        return Err(FetchError::Failed(format!("Cursor's export answered {}", resp.status().as_u16())));
    }
    resp.text().await.map_err(|_| FetchError::failed("Cursor sent an unreadable export"))
}

pub fn map_usage(usage: &Value) -> Result<Vec<Metric>, FetchError> {
    let plan = &usage["planUsage"];
    if usage["enabled"].as_bool() == Some(false) || !plan.is_object() {
        return Err(FetchError::failed("No active Cursor subscription"));
    }
    let start = epoch_ms(usage.get("billingCycleStart"));
    let end = epoch_ms(usage.get("billingCycleEnd"));
    let period = match (start, end) {
        (Some(s), Some(e)) if e > s => Some(e - s),
        _ => Some(MONTH_MS),
    };

    let mut out = Vec::new();
    let limit_cents = num(plan.get("limit"));
    if let Some(total) = num(plan.get("totalPercentUsed")) {
        out.push(Metric::percent("total", "Total usage", total, end, period));
    } else if let Some(limit) = limit_cents.filter(|l| *l > 0.0) {
        let spent = num(plan.get("totalSpend")).unwrap_or_else(|| limit - num(plan.get("remaining")).unwrap_or(limit));
        let mut m = Metric::money("total", "Total usage", spent / 100.0, Some(limit / 100.0), "USD").with_reset(end, period);
        m.primary = true;
        out.push(m);
    }
    for (key, id, label) in [("autoPercentUsed", "auto", "Cursor models"), ("apiPercentUsed", "api", "Other models")] {
        if let Some(p) = num(plan.get(key)) {
            let mut m = Metric::percent(id, label, p, end, period);
            m.primary = out.is_empty();
            out.push(m);
        }
    }
    if out.is_empty() {
        return Err(FetchError::failed("Cursor didn't report a usage limit"));
    }

    // On-demand spend past the plan (cents).
    let sl = &usage["spendLimitUsage"];
    if sl.is_object() {
        let pick = |a: &str, b: &str| num(sl.get(a)).filter(|v| *v > 0.0).or_else(|| num(sl.get(b)));
        let limit = pick("individualLimit", "pooledLimit").unwrap_or(0.0);
        let remaining = pick("individualRemaining", "pooledRemaining").unwrap_or(0.0);
        let reported = ["individualUsed", "pooledUsed", "totalSpend"].iter().find_map(|k| num(sl.get(*k)).filter(|v| *v > 0.0));
        let spent = reported.unwrap_or((limit - remaining).max(0.0));
        if limit > 0.0 || spent > 0.0 {
            out.push(Metric::money("ondemand", "On-demand", spent / 100.0, Some(limit / 100.0), "USD"));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_plan() {
        let body = json!({
            "billingCycleStart": "1791554700922",
            "billingCycleEnd": "1794233100922",
            "planUsage": { "autoPercentUsed": 0, "apiPercentUsed": 0, "totalPercentUsed": 0 },
            "spendLimitUsage": { "pooledLimit": "0", "pooledRemaining": "0", "individualLimit": 0, "limitType": "user" }
        });
        let m = map_usage(&body).unwrap();
        let ids: Vec<_> = m.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["total", "auto", "api"]);
        assert!(m[0].primary && !m[1].primary);
        assert_eq!(m[0].resets_at, Some(1794233100922));
        assert_eq!(m[0].period_ms, Some(1794233100922 - 1791554700922));
    }

    #[test]
    fn dollar_plan_with_on_demand() {
        let body = json!({
            "billingCycleEnd": "1794233100922",
            "planUsage": { "limit": 2000, "totalSpend": 500 },
            "spendLimitUsage": { "individualLimit": 5000, "individualUsed": 1250 }
        });
        let m = map_usage(&body).unwrap();
        assert_eq!(m[0].kind, MetricKind::Money);
        assert_eq!(m[0].fill(), Some(25.0));
        assert_eq!(m[1].id, "ondemand");
        assert_eq!(m[1].used, 12.5);
        assert_eq!(m[1].limit, Some(50.0));
    }

    #[test]
    fn no_subscription() {
        assert!(map_usage(&json!({ "enabled": false })).is_err());
    }
}
