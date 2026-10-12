//! Usage providers. Each one reads the login its own tool already keeps on
//! this machine (Claude Code, the Codex CLI, the Cursor app), asks that
//! service's usage endpoint, and maps the answer onto plain metrics.
//!
//! Request shapes and field mappings follow OpenUsage (MIT,
//! github.com/robinebers/openusage), adapted to the Windows file locations.

pub mod claude;
pub mod codex;
pub mod copilot;
pub mod cursor;
pub mod devin;
pub mod grok;
pub mod muse;
pub mod ollama;
pub mod opencode;
pub mod openrouter;
pub mod zai;

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

    /// A count against a limit ("120 of 1,000 searches").
    pub fn count_of(id: &str, label: &str, n: f64, limit: f64, unit: &str) -> Self {
        Self { limit: Some(limit).filter(|l| *l > 0.0), ..Self::count(id, label, n, unit) }
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
            (MetricKind::Money | MetricKind::Count, Some(l)) if l > 0.0 => Some((self.used / l * 100.0).clamp(0.0, 100.0)),
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
    /// Palette name (see the Holster palette).
    pub color: &'static str,
    /// Lucide icon name.
    pub icon: &'static str,
    /// Turns itself on the first time Quota finds its login. Off for tools
    /// whose login says nothing about a cloud plan (Ollama runs local models).
    pub auto_enable: bool,
}

const fn info(id: &'static str, name: &'static str, color: &'static str, icon: &'static str) -> ProviderInfo {
    ProviderInfo { id, name, color, icon, auto_enable: true }
}

/// OpenUsage's order: the three established providers, then the rest by name.
pub const PROVIDERS: &[ProviderInfo] = &[
    info("claude", "Claude", "peach", "sparkle"),
    info("codex", "Codex", "sky", "square-terminal"),
    info("cursor", "Cursor", "lavender", "mouse-pointer-2"),
    info("copilot", "Copilot", "lilac", "github"),
    info("devin", "Devin", "rose", "bot"),
    info("grok", "Grok", "mist", "zap"),
    info("muse", "Muse Code", "sky", "sparkles"),
    ProviderInfo { auto_enable: false, ..info("ollama", "Ollama", "mist", "cloud") },
    info("opencode", "OpenCode", "mist", "code"),
    info("openrouter", "OpenRouter", "lavender", "route"),
    info("zai", "Z.ai", "mist", "zap"),
];

pub async fn fetch(id: &str) -> FetchResult {
    match id {
        "claude" => claude::fetch().await,
        "codex" => codex::fetch().await,
        "cursor" => cursor::fetch().await,
        "copilot" => copilot::fetch().await,
        "devin" => devin::fetch().await,
        "grok" => grok::fetch().await,
        "muse" => muse::fetch().await,
        "ollama" => ollama::fetch().await,
        "opencode" => opencode::fetch().await,
        "openrouter" => openrouter::fetch().await,
        "zai" => zai::fetch().await,
        _ => Err(FetchError::failed("Unknown provider")),
    }
}

/// Providers that need an API key the user pastes (no app or CLI leaves one),
/// and the environment variables that can hold it instead.
pub fn key_env(id: &str) -> Option<&'static [&'static str]> {
    match id {
        "openrouter" => Some(openrouter::ENV),
        "zai" => Some(zai::ENV),
        _ => None,
    }
}

/// Whether the provider's login (or saved key) is on this machine. Local
/// files and settings only, never the network.
pub fn has_login(id: &str) -> bool {
    match id {
        "claude" => claude::has_login(),
        "codex" => codex::has_login(),
        "cursor" => cursor::has_login(),
        "copilot" => copilot::has_login(),
        "devin" => devin::has_login(),
        "grok" => grok::has_login(),
        "muse" => muse::has_login(),
        "ollama" => ollama::has_login(),
        "opencode" => opencode::has_login(),
        "openrouter" => openrouter::has_login(),
        "zai" => zai::has_login(),
        _ => false,
    }
}

// ---- shared helpers ------------------------------------------------------------

/// A command-line tool by name: the given folders first (a native
/// installer's), then PATH, where an npm `.cmd` shim runs through cmd.
pub fn find_command(name: &str, first: &[std::path::PathBuf]) -> Option<tokio::process::Command> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let dirs: Vec<std::path::PathBuf> = first.iter().cloned().chain(std::env::split_paths(&path)).collect();
    for dir in &dirs {
        let exe = dir.join(format!("{name}.exe"));
        if exe.is_file() {
            return Some(tokio::process::Command::new(exe));
        }
    }
    for dir in &dirs {
        let shim = dir.join(format!("{name}.cmd"));
        if shim.is_file() {
            let mut cmd = tokio::process::Command::new("cmd");
            cmd.arg("/C").arg(shim);
            return Some(cmd);
        }
    }
    None
}

#[derive(Debug)]
pub enum RunError {
    Spawn,
    Timeout,
    Io,
}

/// Runs a tool windowless, from the home folder, with nothing on stdin, and
/// collects its output. Killed if it takes longer than `timeout`.
pub async fn run_quiet(mut cmd: tokio::process::Command, timeout: Duration) -> Result<std::process::Output, RunError> {
    use std::process::Stdio;
    const NO_WINDOW: u32 = 0x0800_0000; // CREATE_NO_WINDOW
    cmd.current_dir(home().unwrap_or_else(std::env::temp_dir))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(NO_WINDOW)
        .kill_on_drop(true);
    let child = cmd.spawn().map_err(|_| RunError::Spawn)?;
    tokio::time::timeout(timeout, child.wait_with_output()).await.map_err(|_| RunError::Timeout)?.map_err(|_| RunError::Io)
}

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
        assert_eq!(Metric::count_of("x", "X", 120.0, 1000.0, "searches").fill(), Some(12.0));
    }
}

#[cfg(test)]
mod real {
    /// `cargo test real_new_providers -- --ignored --nocapture`: the providers
    /// added in 0.2 whose login is on this machine, against their services.
    #[tokio::test]
    #[ignore]
    async fn real_new_providers() {
        for id in ["copilot", "devin", "muse", "ollama", "opencode", "openrouter", "zai"] {
            if !super::has_login(id) {
                println!("{id}: no login here");
                continue;
            }
            match super::fetch(id).await {
                Ok(u) => println!("{id}: plan {:?}: {:?}", u.plan, u.metrics.iter().map(|m| (&m.id, m.used, m.limit, m.resets_at)).collect::<Vec<_>>()),
                Err(e) => println!("{id}: error {e:?}"),
            }
        }
    }
}
