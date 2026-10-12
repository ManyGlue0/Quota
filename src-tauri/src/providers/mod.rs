//! Usage providers. Each one reads the login its own tool already keeps on
//! this machine (Claude Code, the Codex CLI, the Cursor app), asks that
//! service's usage endpoint, and maps the answer onto plain metrics.
//!
//! Request shapes and field mappings follow OpenUsage (MIT,
//! github.com/robinebers/openusage), adapted to the Windows file locations.

pub mod claude;
pub mod codex;
pub mod cursor;

use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const FIVE_HOURS_MS: i64 = 5 * 3600 * 1000;
pub const WEEK_MS: i64 = 7 * 24 * 3600 * 1000;
pub const MONTH_MS: i64 = 30 * 24 * 3600 * 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MetricKind {
    /// `used` is a percentage of the limit (0–100).
    Percent,
    /// `used` and `limit` are amounts of `currency`.
    Money,
    /// `used` is a plain count (e.g. reset credits available).
    Count,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Metric {
    /// Stable within a provider: "session", "weekly", "monthly", "extra"…
    pub id: String,
    pub label: String,
    pub kind: MetricKind,
    pub used: f64,
    pub limit: Option<f64>,
    pub currency: Option<String>,
    /// Epoch milliseconds.
    pub resets_at: Option<i64>,
    pub period_ms: Option<i64>,
    /// A plan limit (shown as a meter, counted for the headline and alerts).
    pub primary: bool,
    /// Short qualifier for counts ("available").
    pub unit: Option<String>,
}

impl Metric {
    pub fn percent(id: &str, label: &str, used: f64, resets_at: Option<i64>, period_ms: Option<i64>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            kind: MetricKind::Percent,
            used: used.clamp(0.0, 100.0),
            limit: Some(100.0),
            currency: None,
            resets_at,
            period_ms,
            primary: true,
            unit: None,
        }
    }

    pub fn money(id: &str, label: &str, used: f64, limit: Option<f64>, currency: &str) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            kind: MetricKind::Money,
            used,
            limit: limit.filter(|l| *l > 0.0),
            currency: Some(currency.into()),
            resets_at: None,
            period_ms: None,
            primary: false,
            unit: None,
        }
    }

    pub fn count(id: &str, label: &str, n: f64, unit: &str) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            kind: MetricKind::Count,
            used: n,
            limit: None,
            currency: None,
            resets_at: None,
            period_ms: None,
            primary: false,
            unit: Some(unit.into()),
        }
    }

    pub fn with_reset(mut self, resets_at: Option<i64>, period_ms: Option<i64>) -> Self {
        self.resets_at = resets_at;
        self.period_ms = period_ms;
        self
    }

    /// How full this limit is, 0–100, when it has one.
    pub fn fill(&self) -> Option<f64> {
        match (self.kind, self.limit) {
            (MetricKind::Percent, _) => Some(self.used),
            (MetricKind::Money, Some(l)) if l > 0.0 => Some((self.used / l * 100.0).clamp(0.0, 100.0)),
            _ => None,
        }
    }
}

/// What a successful fetch returns.
#[derive(Debug, Clone, Default)]
pub struct Usage {
    pub plan: Option<String>,
    pub metrics: Vec<Metric>,
}

#[derive(Debug)]
pub enum FetchError {
    /// No usable login on this machine; the message says how to sign in.
    SignedOut(String),
    /// The service asked us to back off.
    RateLimited { retry_after_secs: Option<u64> },
    /// Anything else, in one calm sentence.
    Failed(String),
}

impl FetchError {
    pub fn failed(e: impl std::fmt::Display) -> Self {
        Self::Failed(e.to_string())
    }
}

pub type FetchResult = Result<Usage, FetchError>;

/// Static description of a provider, shared with the UI and the preview.
pub struct ProviderInfo {
    pub id: &'static str,
    pub name: &'static str,
    /// Palette name (see the Holster palette): tints its meters and icon well.
    pub color: &'static str,
    /// Lucide icon name.
    pub icon: &'static str,
}

pub const PROVIDERS: [ProviderInfo; 3] = [
    ProviderInfo { id: "claude", name: "Claude", color: "peach", icon: "sparkle" },
    ProviderInfo { id: "codex", name: "Codex", color: "sky", icon: "square-terminal" },
    ProviderInfo { id: "cursor", name: "Cursor", color: "lavender", icon: "mouse-pointer-2" },
];

pub async fn fetch(id: &str) -> FetchResult {
    match id {
        "claude" => claude::fetch().await,
        "codex" => codex::fetch().await,
        "cursor" => cursor::fetch().await,
        _ => Err(FetchError::failed("Unknown provider")),
    }
}

// ---- shared helpers ------------------------------------------------------------

pub fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .connect_timeout(Duration::from_secs(8))
            .build()
            .unwrap_or_default()
    })
}

pub fn home() -> Option<std::path::PathBuf> {
    std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(Into::into)
}

/// A JSON number, or a string holding one (Cursor sends int64s as strings).
pub fn num(v: Option<&Value>) -> Option<f64> {
    match v? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
    .filter(|n: &f64| n.is_finite())
}

/// An RFC 3339 date, or epoch seconds / milliseconds, as epoch milliseconds.
pub fn epoch_ms(v: Option<&Value>) -> Option<i64> {
    if let Some(Value::String(s)) = v {
        if let Ok(d) = chrono::DateTime::parse_from_rfc3339(s.trim()) {
            return Some(d.timestamp_millis());
        }
    }
    let n = num(v)?;
    Some(if n.abs() < 1e11 { (n * 1000.0) as i64 } else { n as i64 })
}

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// The payload of a JWT, without verifying it (we only read `exp` and `sub`).
pub fn jwt_payload(token: &str) -> Option<Value> {
    let part = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(part.trim_end_matches('=')).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// A JWT's `exp` as epoch milliseconds.
pub fn jwt_exp_ms(token: &str) -> Option<i64> {
    num(jwt_payload(token)?.get("exp")).map(|s| (s * 1000.0) as i64)
}

/// "self_serve_business" → "Self Serve Business".
pub fn title_case(raw: &str) -> String {
    raw.split(|c: char| c == '_' || c == ' ' || c == '-')
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            c.next().map(|f| f.to_uppercase().collect::<String>() + &c.as_str().to_lowercase()).unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Reads `Retry-After` (seconds) from a response.
pub fn retry_after(resp: &reqwest::Response) -> Option<u64> {
    resp.headers().get(reqwest::header::RETRY_AFTER)?.to_str().ok()?.trim().parse().ok()
}

/// Replaces a credential file another tool owns, but only if it still holds
/// what we read before refreshing (`expect`), so a login that tool rotated
/// meanwhile is never overwritten. Temp file, then rename.
pub fn write_back_if_unchanged(path: &Path, expect: &str, text: &str) -> std::io::Result<bool> {
    if std::fs::read_to_string(path)? != expect {
        return Ok(false);
    }
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("credentials");
    let tmp = path.with_file_name(format!(".{name}.quota.tmp"));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)?;
    Ok(true)
}

/// Classifies a rate-limit window by its length.
pub fn window_by_duration(period_ms: Option<i64>) -> Option<(&'static str, &'static str)> {
    let p = period_ms?;
    let near = |target: i64| (p - target).abs() <= target / 20;
    if near(FIVE_HOURS_MS) {
        Some(("session", "Session"))
    } else if near(WEEK_MS) {
        Some(("weekly", "Weekly"))
    } else if p >= 28 * 24 * 3600 * 1000 && p <= 31 * 24 * 3600 * 1000 {
        Some(("monthly", "Monthly"))
    } else if p == 24 * 3600 * 1000 {
        Some(("daily", "Daily"))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn numbers_and_dates() {
        assert_eq!(num(Some(&json!("1794233100922"))), Some(1794233100922.0));
        assert_eq!(num(Some(&json!(17.5))), Some(17.5));
        assert_eq!(num(Some(&json!(null))), None);
        assert_eq!(epoch_ms(Some(&json!(1793726480))), Some(1793726480000));
        assert_eq!(epoch_ms(Some(&json!("1794233100922"))), Some(1794233100922));
        assert_eq!(epoch_ms(Some(&json!("2026-10-09T23:00:00.106398+00:00"))), Some(1791586800106));
    }

    #[test]
    fn windows() {
        assert_eq!(window_by_duration(Some(18_000_000)).unwrap().0, "session");
        assert_eq!(window_by_duration(Some(604_800_000)).unwrap().0, "weekly");
        assert_eq!(window_by_duration(Some(2_592_000_000)).unwrap().0, "monthly");
        assert_eq!(window_by_duration(Some(1_000)), None);
    }

    #[test]
    fn titles() {
        assert_eq!(title_case("self_serve_business"), "Self Serve Business");
        assert_eq!(title_case("pro"), "Pro");
    }

    #[test]
    fn fill() {
        assert_eq!(Metric::money("x", "X", 25.0, Some(100.0), "USD").fill(), Some(25.0));
        assert_eq!(Metric::money("x", "X", 25.0, None, "USD").fill(), None);
        assert_eq!(Metric::count("x", "X", 2.0, "available").fill(), None);
    }
}
