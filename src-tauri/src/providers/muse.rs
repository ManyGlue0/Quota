//! Muse Code (Meta): the subscription's 5-hour and weekly windows, read by
//! asking Muse Code itself, like Claude.
//!
//! Quota never touches the Muse login. It starts the real Muse Code's local
//! host (`muse serve`, Muse Code 1.4+), speaks its documented protocol
//! (JSON-RPC 2.0, one message per line on stdin/stdout) and calls
//! `usage/read`, which "reads the host's last-observed subscription usage
//! window without a model call". Then it closes stdin and the host exits.
//! Muse Code signs in and renews its own key, as it always does.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use super::*;

const INSTALL: &str = "Install Muse Code 1.4 or later to see your Muse limits";
const SIGN_IN: &str = "Run muse in a terminal and sign in";
/// The launcher may update Muse Code before it starts.
const TIMEOUT: Duration = Duration::from_secs(40);
/// After the handshake, how long to wait for a first observation when the
/// host hasn't seen any usage yet.
const FIRST_OBSERVATION: Duration = Duration::from_secs(8);

fn muse_command() -> Option<tokio::process::Command> {
    let mut first = Vec::new();
    if let Some(d) = std::env::var_os("MUSE_INSTALL_DIR").filter(|d| !d.is_empty()) {
        first.push(PathBuf::from(d));
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        first.push(PathBuf::from(local).join("Programs").join("muse"));
    }
    if let Some(h) = home() {
        first.push(h.join(".local").join("bin"));
    }
    find_command("muse", &first)
}

fn auth_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("MUSE_AUTH_PATH").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    Some(home()?.join(".config").join("muse").join("auth.json"))
}

/// Muse Code is installed and its login file names a provider (a logout
/// leaves `{"providers":{}}` behind).
pub fn has_login() -> bool {
    let signed_in = auth_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .is_some_and(|d| d["providers"].as_object().is_some_and(|p| !p.is_empty()));
    signed_in && muse_command().is_some()
}

pub async fn fetch() -> FetchResult {
    let mut cmd = muse_command().ok_or_else(|| FetchError::SignedOut(INSTALL.into()))?;
    const NO_WINDOW: u32 = 0x0800_0000; // CREATE_NO_WINDOW
    cmd.arg("serve")
        .current_dir(home().unwrap_or_else(std::env::temp_dir))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(NO_WINDOW)
        .kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|_| FetchError::SignedOut(INSTALL.into()))?;
    let result = tokio::time::timeout(TIMEOUT, talk(&mut child)).await;
    // Closing stdin asks the host to shut down cleanly; kill_on_drop backs it up.
    drop(child.stdin.take());
    let _ = tokio::time::timeout(Duration::from_secs(3), child.wait()).await;
    match result {
        Ok(r) => r,
        Err(_) => Err(FetchError::failed("Muse Code took too long to answer")),
    }
}

async fn talk(child: &mut tokio::process::Child) -> FetchResult {
    let mut stdin = child.stdin.take().ok_or_else(|| FetchError::failed("Couldn't run Muse Code"))?;
    let stdout = child.stdout.take().ok_or_else(|| FetchError::failed("Couldn't run Muse Code"))?;
    let mut lines = BufReader::new(stdout).lines();
    let initialize = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "clientInfo": { "name": "quota", "title": "Quota", "version": env!("CARGO_PKG_VERSION") },
        "capabilities": { "experimentalApi": true, "userInputDialogs": false }
    }});
    write_frame(&mut stdin, &initialize).await?;
    let init = read_reply(&mut lines, 1, &mut None).await?;
    if init.get("error").is_some() {
        return Err(FetchError::failed("Muse Code is too old to report usage. Update it with muse update"));
    }
    let mut observed: Option<Value> = None;
    for f in [
        json!({ "jsonrpc": "2.0", "method": "initialized" }),
        json!({ "jsonrpc": "2.0", "id": 2, "method": "account/read" }),
        json!({ "jsonrpc": "2.0", "id": 3, "method": "usage/read" }),
    ] {
        write_frame(&mut stdin, &f).await?;
    }
    let account = read_reply(&mut lines, 2, &mut observed).await?;
    if account["result"]["state"].as_str() == Some("loggedOut") {
        return Err(FetchError::SignedOut(SIGN_IN.into()));
    }
    if matches!(account["result"]["state"].as_str(), Some("apiKey" | "envKey")) {
        return Err(FetchError::SignedOut("Muse Code is using an API key, which has no plan limits".into()));
    }
    let usage = read_reply(&mut lines, 3, &mut observed).await?;
    let mut found = usage["result"]["usage"].clone();
    if !found.is_object() {
        found = observed.take().unwrap_or(Value::Null);
    }
    if !found.is_object() {
        // Nothing observed yet: the first observation arrives as `usage/changed`.
        let wait = async {
            while let Ok(Some(line)) = lines.next_line().await {
                if let Ok(v) = serde_json::from_str::<Value>(&line) {
                    if v["method"] == "usage/changed" && v["params"].is_object() {
                        return Some(v["params"].clone());
                    }
                }
            }
            None
        };
        found = tokio::time::timeout(FIRST_OBSERVATION, wait).await.ok().flatten().unwrap_or(Value::Null);
    }
    if !found.is_object() {
        return Err(FetchError::failed("Muse Code hasn't seen your usage yet. Send a prompt in Muse Code, then check again"));
    }
    Ok(map_usage(&found))
}

/// One message per line, never pretty-printed.
async fn write_frame(stdin: &mut tokio::process::ChildStdin, msg: &Value) -> Result<(), FetchError> {
    let line = msg.to_string() + "\n";
    stdin.write_all(line.as_bytes()).await.map_err(|_| FetchError::failed("Couldn't talk to Muse Code"))?;
    stdin.flush().await.map_err(|_| FetchError::failed("Couldn't talk to Muse Code"))
}

/// Reads frames until the reply with `id`, keeping a `usage/changed` seen on the way.
async fn read_reply<R: tokio::io::AsyncBufRead + Unpin>(
    lines: &mut tokio::io::Lines<R>,
    id: i64,
    observed: &mut Option<Value>,
) -> Result<Value, FetchError> {
    loop {
        let line = lines
            .next_line()
            .await
            .map_err(|_| FetchError::failed("Couldn't talk to Muse Code"))?
            .ok_or_else(|| FetchError::failed("Muse Code stopped before answering. Run muse in a terminal to check it works"))?;
        let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
        if v["id"].as_i64() == Some(id) && v.get("method").is_none() {
            return Ok(v);
        }
        if v["method"] == "usage/changed" && v["params"].is_object() {
            *observed = Some(v["params"].clone());
        }
    }
}

/// `SubscriptionUsage`: `{ observedAtMs, tier, window: { usedPercent,
/// resetsAtMs, windowDurationMins }, weekly: { usedPercent, resetsAtMs } }`.
pub fn map_usage(u: &Value) -> Usage {
    let mut metrics = Vec::new();
    let w = &u["window"];
    if let Some(used) = num(w.get("usedPercent")) {
        let mins = num(w.get("windowDurationMins")).filter(|m| *m > 0.0);
        let period = mins.map(|m| (m * 60_000.0) as i64).unwrap_or(FIVE_HOURS_MS);
        let (id, label) = window_by_duration(Some(period)).filter(|(id, _)| *id != "weekly").unwrap_or(("session", "Session"));
        metrics.push(Metric::percent(id, label, used, epoch_ms(w.get("resetsAtMs")), Some(period)));
    }
    let wk = &u["weekly"];
    if let Some(used) = num(wk.get("usedPercent")) {
        metrics.push(Metric::percent("weekly", "Weekly", used, epoch_ms(wk.get("resetsAtMs")), Some(WEEK_MS)));
    }
    Usage { plan: u["tier"].as_str().map(plan_name).filter(|p| !p.is_empty()), metrics }
}

/// Meta's plan names: Everyday, High, Power and Maximum Usage.
fn plan_name(tier: &str) -> String {
    let t = tier.trim().to_lowercase();
    for (key, name) in [("everyday", "Everyday"), ("high", "High"), ("power", "Power"), ("max", "Maximum")] {
        if t.contains(key) {
            return name.into();
        }
    }
    title_case(tier.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscription_usage() {
        let u = json!({ "observedAtMs": 1791000000000u64, "tier": "power_usage",
            "window": { "usedPercent": 42, "resetsAtMs": 1791010000000u64, "windowDurationMins": 300 },
            "weekly": { "usedPercent": 117, "resetsAtMs": 1791400000000u64 } });
        let m = map_usage(&u);
        assert_eq!(m.plan.as_deref(), Some("Power"));
        let got: Vec<_> = m.metrics.iter().map(|m| (m.id.as_str(), m.used, m.resets_at)).collect();
        assert_eq!(got, [("session", 42.0, Some(1791010000000)), ("weekly", 100.0, Some(1791400000000))]);
        assert_eq!(m.metrics[0].period_ms, Some(FIVE_HOURS_MS));
    }

    #[test]
    fn unknown_tiers_read_plainly() {
        assert_eq!(plan_name("contributor_plus"), "Contributor Plus");
        assert!(map_usage(&json!({})).metrics.is_empty());
    }
}
