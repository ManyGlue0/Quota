//! Claude: plan limits, read by asking Claude Code itself.
//!
//! Quota never uses the Claude login directly. It runs the real Claude Code,
//! headless, with its built-in `/usage` command, and reads what it prints:
//!
//! ```text
//! claude -p /usage --no-session-persistence --strict-mcp-config --settings <no hooks>
//! Current session: 70% used · resets Oct 10, 1am (Europe/Rome)
//! Current week (all models): 13% used · resets Oct 16, 6pm (Europe/Rome)
//! ```
//!
//! `/usage` is a local command (no model request, nothing counted against the
//! limits), the run is kept out of the session history, and hooks and MCP
//! servers are skipped so a check is quick and quiet. Claude Code does its own
//! sign-in and token renewal, as it always does. The plan name comes from
//! Claude Code's saved login, read from disk and never sent anywhere.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use chrono::{Datelike, Local, NaiveDate, NaiveTime, TimeZone};
use serde_json::Value;

use super::*;

const INSTALL: &str = "Install Claude Code to see your Claude limits";
const SIGN_IN: &str = "Run claude in a terminal and sign in";
const NO_WINDOW: u32 = 0x0800_0000; // CREATE_NO_WINDOW

/// Where Claude Code lives: the native installer's `~/.local/bin`, then PATH,
/// then npm's shim (which needs cmd to run).
fn claude_command() -> Option<tokio::process::Command> {
    let home = home();
    let native = home.as_ref().map(|h| h.join(".local").join("bin").join("claude.exe"));
    if let Some(exe) = native.filter(|p| p.is_file()) {
        return Some(tokio::process::Command::new(exe));
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path) {
        let exe = dir.join("claude.exe");
        if exe.is_file() {
            return Some(tokio::process::Command::new(exe));
        }
        let shim = dir.join("claude.cmd");
        if shim.is_file() {
            let mut cmd = tokio::process::Command::new("cmd");
            cmd.arg("/C").arg(shim);
            return Some(cmd);
        }
    }
    None
}

/// A settings file that turns hooks off for the check (a user's SessionStart
/// hooks shouldn't fire every few minutes).
fn quiet_settings() -> PathBuf {
    let file = std::env::temp_dir().join("quota-claude-usage-settings.json");
    if std::fs::read_to_string(&file).ok().as_deref() != Some(r#"{"disableAllHooks":true}"#) {
        let _ = std::fs::write(&file, r#"{"disableAllHooks":true}"#);
    }
    file
}

pub async fn fetch() -> FetchResult {
    let mut cmd = claude_command().ok_or_else(|| FetchError::SignedOut(INSTALL.into()))?;
    cmd.args(["-p", "/usage", "--no-session-persistence", "--strict-mcp-config", "--settings"])
        .arg(quiet_settings())
        .current_dir(home().unwrap_or_else(std::env::temp_dir))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(NO_WINDOW)
        .kill_on_drop(true);
    let child = cmd.spawn().map_err(|_| FetchError::SignedOut(INSTALL.into()))?;
    let out = tokio::time::timeout(Duration::from_secs(45), child.wait_with_output())
        .await
        .map_err(|_| FetchError::failed("Claude Code took too long to answer"))?
        .map_err(|_| FetchError::failed("Couldn't run Claude Code"))?;
    let text = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    let metrics = parse_usage(&text, Local::now().naive_local());
    if metrics.is_empty() {
        return Err(explain(&text));
    }
    Ok(Usage { plan: plan(), metrics })
}

/// Why `/usage` printed no limits, in one calm sentence.
fn explain(text: &str) -> FetchError {
    let lower = text.to_lowercase();
    if lower.contains("not logged in") || lower.contains("/login") || lower.contains("please log in") {
        return FetchError::SignedOut(SIGN_IN.into());
    }
    if lower.contains("api key") || lower.contains("api usage billing") {
        return FetchError::SignedOut("Claude Code is using an API key, which has no plan limits".into());
    }
    if lower.contains("rate limit") || lower.contains("too many requests") {
        return FetchError::RateLimited { retry_after_secs: None };
    }
    let first = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("Claude Code didn't report your limits");
    FetchError::Failed(first.chars().take(140).collect())
}

/// The plan, from Claude Code's saved login (read locally, never sent):
/// "pro" + "default_claude_max_5x" → "Pro" / "Max 5x".
fn plan() -> Option<String> {
    let dir = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|d| !d.is_empty()).map(PathBuf::from).or_else(|| home().map(|h| h.join(".claude")))?;
    let doc: Value = serde_json::from_str(&std::fs::read_to_string(dir.join(".credentials.json")).ok()?).ok()?;
    plan_name(&doc["claudeAiOauth"])
}

fn plan_name(oauth: &Value) -> Option<String> {
    let base = title_case(oauth["subscriptionType"].as_str()?.trim());
    if base.is_empty() {
        return None;
    }
    let tier = oauth["rateLimitTier"].as_str().unwrap_or("");
    let mult = tier
        .split('_')
        .find(|p| p.len() >= 2 && p.ends_with('x') && p[..p.len() - 1].bytes().all(|b| b.is_ascii_digit()));
    Some(match mult {
        Some(m) => format!("{base} {m}"),
        None => base,
    })
}

/// Turns `/usage` lines ("Current week (all models): 13% used · resets Oct 16,
/// 6pm (Europe/Rome)") into meters.
pub fn parse_usage(text: &str, now: chrono::NaiveDateTime) -> Vec<Metric> {
    let mut out: Vec<Metric> = Vec::new();
    for line in text.lines().map(str::trim) {
        let Some((name, rest)) = line.split_once(':') else { continue };
        let rest = rest.trim();
        let Some(pct_end) = rest.find('%') else { continue };
        let Ok(used) = rest[..pct_end].trim().parse::<f64>() else { continue };
        if !rest[pct_end + 1..].trim_start().starts_with("used") {
            continue;
        }
        let resets = rest
            .split_once("resets")
            .and_then(|(_, r)| parse_reset(r.trim(), now))
            .and_then(|dt| Local.from_local_datetime(&dt).earliest())
            .map(|d| d.timestamp_millis());
        let (id, label, period) = classify(name.trim());
        if out.iter().any(|m| m.id == id) {
            continue;
        }
        let mut m = Metric::percent(&id, &label, used, resets, period);
        m.primary = id == "session" || id == "weekly" || !out.iter().any(|m| m.primary);
        out.push(m);
    }
    out
}

fn classify(name: &str) -> (String, String, Option<i64>) {
    let lower = name.to_lowercase();
    let scope = name.split_once('(').map(|(_, s)| s.trim_end_matches(')').trim().to_string());
    if lower.contains("session") {
        return ("session".into(), "Session".into(), Some(FIVE_HOURS_MS));
    }
    if lower.contains("week") {
        return match scope {
            None => ("weekly".into(), "Weekly".into(), Some(WEEK_MS)),
            Some(s) if s.to_lowercase().starts_with("all") => ("weekly".into(), "Weekly".into(), Some(WEEK_MS)),
            Some(s) => {
                let model = s.trim_end_matches(" only").trim().to_string();
                (model.to_lowercase().replace(' ', "-"), model, Some(WEEK_MS))
            }
        };
    }
    let id = lower.replace(|c: char| !c.is_ascii_alphanumeric(), "-");
    (id, name.to_string(), None)
}

/// "Oct 10, 1am (Europe/Rome)", "Oct 16, 6:30pm", "1am": the next such moment.
fn parse_reset(text: &str, now: chrono::NaiveDateTime) -> Option<chrono::NaiveDateTime> {
    let text = text.split('(').next().unwrap_or(text).trim().trim_end_matches(',');
    let (date_part, time_part) = match text.rsplit_once(',') {
        Some((d, t)) => (Some(d.trim()), t.trim()),
        None if text.chars().next().is_some_and(|c| c.is_ascii_digit()) => (None, text),
        None => (Some(text), ""),
    };
    let time = parse_clock(time_part).unwrap_or(NaiveTime::MIN);
    let date = match date_part {
        Some(d) => {
            let mut parts = d.split_whitespace();
            let month = month_number(parts.next()?)?;
            let day: u32 = parts.next()?.trim_end_matches(|c: char| !c.is_ascii_digit()).parse().ok()?;
            let this_year = NaiveDate::from_ymd_opt(now.year(), month, day)?;
            // A reset is always ahead: a date already well past means next year.
            if this_year.and_time(time) < now - chrono::Duration::hours(12) {
                NaiveDate::from_ymd_opt(now.year() + 1, month, day)?
            } else {
                this_year
            }
        }
        None => {
            let today = now.date();
            if today.and_time(time) < now { today.succ_opt()? } else { today }
        }
    };
    Some(date.and_time(time))
}

fn parse_clock(s: &str) -> Option<NaiveTime> {
    let s = s.trim().to_lowercase().replace(' ', "");
    let (digits, pm) = if let Some(d) = s.strip_suffix("pm") {
        (d, true)
    } else if let Some(d) = s.strip_suffix("am") {
        (d, false)
    } else {
        // 24-hour "18:00"
        let (h, m) = s.split_once(':')?;
        return NaiveTime::from_hms_opt(h.parse().ok()?, m.parse().ok()?, 0);
    };
    let (h, m) = match digits.split_once(':') {
        Some((h, m)) => (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?),
        None => (digits.parse::<u32>().ok()?, 0),
    };
    let h = match (h % 12, pm) {
        (h, true) => h + 12,
        (h, false) => h,
    };
    NaiveTime::from_hms_opt(h, m, 0)
}

fn month_number(m: &str) -> Option<u32> {
    let m = m.get(..3)?.to_lowercase();
    ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"]
        .iter()
        .position(|x| *x == m)
        .map(|i| i as u32 + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn at(s: &str) -> chrono::NaiveDateTime {
        chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap()
    }

    const OUTPUT: &str = "You are currently using your subscription to power your Claude Code usage\n\n\
        Current session: 70% used · resets Oct 10, 1am (Europe/Rome)\n\
        Current week (all models): 13% used · resets Oct 16, 6pm (Europe/Rome)\n\
        Current week (Sonnet only): 4% used · resets Oct 16, 6:30pm (Europe/Rome)\n\n\
        Last 24h · 390 requests · 4 sessions\n  87% of your usage was at >150k context\n";

    #[test]
    fn parses_usage_output() {
        let m = parse_usage(OUTPUT, at("2026-10-09 22:05"));
        let ids: Vec<_> = m.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["session", "weekly", "sonnet"], "the activity lines aren't limits");
        assert_eq!(m[0].used, 70.0);
        assert!(m[0].primary && m[1].primary && !m[2].primary);
        assert_eq!(m[2].label, "Sonnet");
        let local = |s: &str| Local.from_local_datetime(&at(s)).earliest().unwrap().timestamp_millis();
        assert_eq!(m[0].resets_at, Some(local("2026-10-10 01:00")));
        assert_eq!(m[1].resets_at, Some(local("2026-10-16 18:00")));
        assert_eq!(m[2].resets_at, Some(local("2026-10-16 18:30")));
    }

    #[test]
    fn resets_roll_into_next_year_and_day() {
        assert_eq!(parse_reset("Jan 2, 9am (UTC)", at("2026-12-30 10:00")), Some(at("2027-01-02 09:00")));
        assert_eq!(parse_reset("1am", at("2026-10-09 22:00")), Some(at("2026-10-10 01:00")));
        assert_eq!(parse_reset("12pm", at("2026-10-09 08:00")), Some(at("2026-10-09 12:00")));
        assert_eq!(parse_reset("Oct 16, 18:00", at("2026-10-09 08:00")), Some(at("2026-10-16 18:00")));
    }

    #[test]
    fn a_session_not_started_has_no_reset() {
        let m = parse_usage("Current session: 0% used\nCurrent week (all models): 13% used · resets Oct 16, 6pm", at("2026-10-09 22:05"));
        assert_eq!(m[0].resets_at, None);
        assert!(m[1].resets_at.is_some());
    }

    #[test]
    fn explains_missing_limits() {
        assert!(matches!(explain("Not logged in · Please run /login"), FetchError::SignedOut(_)));
        assert!(matches!(explain("You are using an API key"), FetchError::SignedOut(_)));
        assert!(matches!(explain("something odd"), FetchError::Failed(_)));
    }

    #[test]
    fn plans() {
        assert_eq!(plan_name(&json!({ "subscriptionType": "pro", "rateLimitTier": "default_claude_ai" })).as_deref(), Some("Pro"));
        assert_eq!(plan_name(&json!({ "subscriptionType": "max", "rateLimitTier": "default_claude_max_20x" })).as_deref(), Some("Max 20x"));
    }
}

#[cfg(test)]
mod real {
    /// `cargo test real_claude_usage -- --ignored --nocapture`: asks this machine's Claude Code.
    #[tokio::test]
    #[ignore]
    async fn real_claude_usage() {
        let t = std::time::Instant::now();
        let r = super::fetch().await;
        println!("took {:?}", t.elapsed());
        match r {
            Ok(u) => println!("plan {:?}: {:?}", u.plan, u.metrics.iter().map(|m| (&m.id, m.used, m.resets_at)).collect::<Vec<_>>()),
            Err(e) => println!("error: {e:?}"),
        }
    }
}
