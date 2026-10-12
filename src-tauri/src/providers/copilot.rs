//! GitHub Copilot: the monthly AI-credit pool (or, on free plans, chat and
//! completions), from GitHub's Copilot usage endpoint, the one Copilot's own
//! editor plugins call.
//!
//! The GitHub token comes from where Copilot tooling already keeps one: the
//! editor plugins' `apps.json` (JetBrains, Neovim…), else the GitHub CLI,
//! asked with `gh auth token` (gh keeps its token in Windows Credential
//! Manager; Quota asks gh rather than reading it). Only github.com tokens are
//! used, and only sent to api.github.com.

use std::path::PathBuf;
use std::time::Duration;

use serde_json::Value;

use super::*;

const USAGE_URL: &str = "https://api.github.com/copilot_internal/user";
const SIGN_IN: &str = "Sign in to Copilot in your editor, or run gh auth login";
const DAY_MS: i64 = 24 * 3600 * 1000;

fn editor_configs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        dirs.push(PathBuf::from(local).join("github-copilot"));
    }
    if let Some(h) = home() {
        dirs.push(h.join(".config").join("github-copilot"));
    }
    dirs.iter().flat_map(|d| [d.join("apps.json"), d.join("hosts.json")]).collect()
}

/// A github.com `oauth_token` from the editor plugins' config, keyed by host
/// (`github.com`, or `github.com:<app id>`). Enterprise hosts are skipped.
fn editor_token() -> Option<String> {
    editor_configs().into_iter().find_map(|path| {
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
        doc.as_object()?.iter().find_map(|(host, v)| {
            (host == "github.com" || host.starts_with("github.com:"))
                .then(|| v["oauth_token"].as_str().map(str::trim).filter(|t| !t.is_empty()).map(str::to_string))
                .flatten()
        })
    })
}

fn gh_command() -> Option<tokio::process::Command> {
    let mut first = Vec::new();
    for var in ["ProgramFiles", "ProgramFiles(x86)"] {
        if let Some(p) = std::env::var_os(var) {
            first.push(PathBuf::from(p).join("GitHub CLI"));
        }
    }
    find_command("gh", &first)
}

async fn gh_token() -> Option<String> {
    let mut cmd = gh_command()?;
    cmd.args(["auth", "token", "--hostname", "github.com"]);
    let out = run_quiet(cmd, Duration::from_secs(15)).await.ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok().map(|t| t.trim().to_string()).filter(|t| !t.is_empty())
}

/// Signed in to the GitHub CLI (its hosts file names github.com), or an
/// editor plugin's token is on disk.
pub fn has_login() -> bool {
    if editor_token().is_some() {
        return true;
    }
    let hosts = std::env::var_os("GH_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("GitHub CLI")))
        .map(|d| d.join("hosts.yml"));
    gh_command().is_some()
        && hosts.and_then(|h| std::fs::read_to_string(h).ok()).is_some_and(|t| t.lines().any(|l| l.trim_end() == "github.com:"))
}

pub async fn fetch() -> FetchResult {
    let token = match editor_token() {
        Some(t) => t,
        None => gh_token().await.ok_or_else(|| FetchError::SignedOut(SIGN_IN.into()))?,
    };
    let resp = client()
        .get(USAGE_URL)
        .header("Authorization", format!("token {token}"))
        .header("Accept", "application/json")
        .header("Editor-Version", "vscode/1.96.2")
        .header("Editor-Plugin-Version", "copilot-chat/0.26.7")
        .header("User-Agent", "GitHubCopilotChat/0.26.7")
        .header("X-Github-Api-Version", "2025-04-01")
        .send()
        .await
        .map_err(|_| FetchError::failed("Couldn't reach GitHub"))?;
    let status = resp.status().as_u16();
    if status == 429 {
        return Err(FetchError::RateLimited { retry_after_secs: retry_after(&resp) });
    }
    if matches!(status, 401 | 403) {
        return Err(FetchError::SignedOut("GitHub didn't accept your token. Run gh auth login again".into()));
    }
    if status == 404 {
        return Err(FetchError::SignedOut("No Copilot plan on this GitHub account".into()));
    }
    if !resp.status().is_success() {
        return Err(FetchError::Failed(format!("GitHub answered {status}")));
    }
    let body: Value = resp.json().await.map_err(|_| FetchError::failed("GitHub sent an unreadable answer"))?;
    map_usage(&body)
}

/// Since June 2026 every plan bills by AI credits: `premium_interactions` is
/// the credit pool (percent *remaining*, flipped to used). Paid plans report
/// chat and completions as unlimited (`-1`), free plans as real counts.
/// Org-managed seats have no personal pool: their plan shows, with a plain
/// credit count when GitHub reports one.
pub fn map_usage(body: &Value) -> FetchResult {
    let plan = body["copilot_plan"].as_str().map(title_case).filter(|p| !p.is_empty());
    let resets = reset(&body["quota_reset_date"]).or_else(|| reset(&body["limited_user_reset_date"]));
    let snaps = &body["quota_snapshots"];
    let mut out = Vec::new();
    let premium = &snaps["premium_interactions"];
    if let Some(m) = snapshot(premium, "credits", "Credits", resets) {
        out.push(m);
        if premium["overage_permitted"].as_bool() == Some(true) {
            out.push(Metric::count("extra", "Extra usage", num(premium.get("overage_count")).unwrap_or(0.0).max(0.0), "premium requests"));
        }
    }
    out.extend(snapshot(&snaps["chat"], "chat", "Chat", resets));
    out.extend(snapshot(&snaps["completions"], "completions", "Completions", resets));
    if out.is_empty() {
        // Older free accounts: remaining counts against monthly limits.
        for (key, label) in [("chat", "Chat"), ("completions", "Completions")] {
            let total = num(body["monthly_quotas"].get(key)).filter(|t| *t > 0.0);
            let left = num(body["limited_user_quotas"].get(key));
            if let (Some(total), Some(left)) = (total, left) {
                out.push(Metric::percent(key, label, (total - left).max(0.0) / total * 100.0, resets, Some(30 * DAY_MS)));
            }
        }
    }
    if out.is_empty() {
        if body["token_based_billing"].as_bool() != Some(true) {
            return Err(FetchError::failed("Copilot doesn't report usage for this account"));
        }
        if let Some(n) = num(premium.get("credits_used")).filter(|n| *n > 0.0) {
            out.push(Metric::count("credits", "Credits", n, "used"));
        }
    }
    Ok(Usage { plan, metrics: out })
}

fn snapshot(s: &Value, id: &str, label: &str, resets: Option<i64>) -> Option<Metric> {
    if !s.is_object() {
        return None;
    }
    let entitlement = num(s.get("entitlement"));
    let remaining = num(s.get("remaining"));
    if s["unlimited"].as_bool() == Some(true) || entitlement == Some(-1.0) || remaining == Some(-1.0) || entitlement == Some(0.0) {
        return None;
    }
    let used = match (num(s.get("percent_remaining")), entitlement, remaining) {
        (Some(left), _, _) => 100.0 - left,
        (None, Some(e), Some(r)) if e > 0.0 => 100.0 - r / e * 100.0,
        _ => return None,
    };
    Some(Metric::percent(id, label, used, resets, Some(30 * DAY_MS)))
}

/// An ISO date-time (paid) or a bare `yyyy-mm-dd` (free, UTC).
fn reset(v: &Value) -> Option<i64> {
    let s = v.as_str()?.trim();
    epoch_ms(Some(v)).or_else(|| {
        chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok().map(|d| d.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp_millis())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn paid_plan_credits() {
        let body = json!({
            "copilot_plan": "individual_pro", "quota_reset_date": "2026-11-01T00:00:00Z",
            "quota_snapshots": {
                "premium_interactions": { "entitlement": 300, "remaining": 210, "percent_remaining": 70, "overage_permitted": true, "overage_count": 4 },
                "chat": { "entitlement": -1, "remaining": -1, "unlimited": true },
                "completions": { "entitlement": -1, "remaining": -1, "unlimited": true }
            }
        });
        let u = map_usage(&body).unwrap();
        assert_eq!(u.plan.as_deref(), Some("Individual Pro"));
        let ids: Vec<_> = u.metrics.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["credits", "extra"]);
        assert_eq!(u.metrics[0].used, 30.0);
        assert_eq!(u.metrics[0].resets_at, Some(1793491200000));
    }

    #[test]
    fn free_and_org_seats() {
        let free = json!({ "copilot_plan": "free", "limited_user_reset_date": "2026-11-01",
            "quota_snapshots": { "premium_interactions": { "entitlement": 0, "remaining": 0 },
                "chat": { "entitlement": 50, "remaining": 40 }, "completions": { "entitlement": 2000, "remaining": 1000 } } });
        let u = map_usage(&free).unwrap();
        let ids: Vec<_> = u.metrics.iter().map(|m| (m.id.as_str(), m.used)).collect();
        assert_eq!(ids, [("chat", 20.0), ("completions", 50.0)]);
        assert_eq!(u.metrics[0].resets_at, Some(1793491200000));

        let org = json!({ "copilot_plan": "business", "token_based_billing": true,
            "quota_snapshots": { "premium_interactions": { "entitlement": 0, "remaining": 0, "credits_used": 42, "overage_permitted": true } } });
        let u = map_usage(&org).unwrap();
        assert_eq!(u.metrics.len(), 1);
        assert_eq!((u.metrics[0].id.as_str(), u.metrics[0].used), ("credits", 42.0));
        assert!(map_usage(&json!({})).is_err());
    }
}
