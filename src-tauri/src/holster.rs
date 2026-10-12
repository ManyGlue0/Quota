//! Holster integration: Quota is one of the dock's tiles.
//!
//! - A loopback-only listener serves `/holster/health` and `/holster/open`
//!   (Holster POSTs there when the Quota tile is clicked).
//! - The manifest goes to `%APPDATA%\Holster\apps\quota.json` on every start,
//!   only when Holster is installed.
//! - The preview is a page on disk in `<data dir>\holster-preview\`, next to a
//!   `state.json` snapshot rewritten after every refresh, so Holster shows the
//!   last known limits even while Quota isn't running. That folder holds
//!   nothing else: Holster serves it.
//! - When a limit crosses the alert threshold or resets while the panel is
//!   closed, Quota sends Holster an activity signal and the dock's tab glows.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::{
    body::Bytes,
    extract::{Request, State},
    http::{header, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Serialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Manager};

use crate::shell::Mode;
use crate::usage::Snapshot;
use crate::Core;

pub const ID: &str = "quota";
const NAME: &str = "Quota";
const PREFERRED_PORT: u16 = 47333;
/// OpenUsage's blue: the tile Quota's mark sits on.
const COLOR: &str = "#2B7EFF";
/// Quota's mark (white, transparent), drawn untinted on the tile.
const ICON_SVG: &str = include_str!("../icons/quota-glyph.svg");
const PREVIEW_HTML: &str = include_str!("../holster-preview/index.html");
/// Provider marks, the same files the panel draws, with their tints (None:
/// the text color; src/lib/providers.tsx has the same tints).
const LOGOS: &[(&str, &str, Option<&str>)] = &[
    ("antigravity", include_str!("../../src/assets/providers/antigravity.svg"), Some("#4285F4")),
    ("claude", include_str!("../../src/assets/providers/claude.svg"), Some("#DE7356")),
    ("codex", include_str!("../../src/assets/providers/codex.svg"), None),
    ("copilot", include_str!("../../src/assets/providers/copilot.svg"), None),
    ("cursor", include_str!("../../src/assets/providers/cursor.svg"), None),
    ("devin", include_str!("../../src/assets/providers/devin.svg"), None),
    ("grok", include_str!("../../src/assets/providers/grok.svg"), None),
    ("muse", include_str!("../../src/assets/providers/muse.svg"), Some("#0082FB")),
    ("ollama", include_str!("../../src/assets/providers/ollama.svg"), None),
    ("opencode", include_str!("../../src/assets/providers/opencode.svg"), None),
    ("openrouter", include_str!("../../src/assets/providers/openrouter.svg"), None),
    ("zai", include_str!("../../src/assets/providers/zai.svg"), None),
];

/// The preview page with the logo table filled in.
fn preview_html() -> String {
    let logos: serde_json::Map<String, Value> =
        LOGOS.iter().map(|(id, svg, tint)| (id.to_string(), json!({ "svg": svg.trim(), "tint": tint }))).collect();
    PREVIEW_HTML.replacen("/*QUOTA_LOGOS*/ {}", &Value::Object(logos).to_string(), 1)
}
/// Preview card layout (CSS px), mirrored by the page's CSS. The page resizes
/// itself when the row count changes; this only sizes the first showing.
const PREVIEW_WIDTH: u32 = 300;
const SIGNAL_EVERY: Duration = Duration::from_secs(10);

fn preview_height(rows: usize) -> u32 {
    let n = rows.clamp(1, 3) as u32;
    // padding 14 + head 16 + gap 10 + rows (54 each, 6 apart) + padding 14
    14 + 16 + 10 + n * 54 + (n - 1) * 6 + 14
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HolsterStatus {
    /// `%APPDATA%\Holster` exists.
    pub installed: bool,
    /// The manifest is in Holster's apps folder.
    pub registered: bool,
    /// Holster's verdict on the manifest, when it was running to give one.
    pub error: Option<String>,
    pub warnings: Vec<String>,
    /// Port of the loopback listener Holster talks to (0 if it failed).
    pub port: u16,
}

struct Holster {
    dir: PathBuf,
    status: Mutex<HolsterStatus>,
    last_signal: Mutex<Option<Instant>>,
}

static HOLSTER: OnceLock<Arc<Holster>> = OnceLock::new();

fn holster_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("Holster")).filter(|d| d.is_dir())
}

/// Holster's API base from `holster.json`, read fresh every time: the port
/// changes on every Holster start.
fn holster_api() -> Option<String> {
    let text = std::fs::read_to_string(holster_dir()?.join("holster.json")).ok()?;
    let api = serde_json::from_str::<Value>(&text).ok()?.get("api")?.as_str()?.trim_end_matches('/').to_string();
    api.starts_with("http://127.0.0.1:").then_some(api)
}

fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| reqwest::Client::builder().timeout(Duration::from_millis(1500)).no_proxy().build().unwrap_or_default())
}

/// Temp file, then rename, so Holster never reads half a file. Skips the
/// write when nothing changed.
fn write_if_changed(file: &Path, text: &str) -> std::io::Result<()> {
    if std::fs::read_to_string(file).ok().as_deref() == Some(text) {
        return Ok(());
    }
    let name = file.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let tmp = file.with_file_name(format!(".{name}.tmp"));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, file)
}

/// Starts the listener, writes the preview and registers with Holster.
/// Call once the shell and the poller exist: `/holster/open` drives them.
pub fn start(app: AppHandle, data_dir: &Path, snap: &Snapshot) {
    let me = Arc::new(Holster {
        dir: data_dir.join("holster-preview"),
        status: Mutex::new(HolsterStatus::default()),
        last_signal: Mutex::new(None),
    });
    let _ = HOLSTER.set(me.clone());
    let _ = std::fs::create_dir_all(&me.dir);
    for (name, text) in [("index.html", preview_html().as_str()), ("icon.svg", ICON_SVG)] {
        if let Err(e) = write_if_changed(&me.dir.join(name), text) {
            eprintln!("holster: couldn't write {name}: {e}");
        }
    }
    publish(snap);
    let rows = snap.providers.iter().filter(|p| p.enabled).count();

    let app_for_state = app.clone();
    tauri::async_runtime::spawn(async move {
        let port = match serve(app).await {
            Ok(port) => port,
            Err(e) => {
                eprintln!("holster: couldn't listen on loopback: {e}");
                return;
            }
        };
        me.status.lock().unwrap().port = port;
        if let Some(core) = app_for_state.try_state::<Arc<Core>>() {
            publish(&core.poller.snapshot());
        }
        me.register(port, rows).await;
    });
}

pub fn status() -> HolsterStatus {
    let mut st = HOLSTER.get().map(|h| h.status.lock().unwrap().clone()).unwrap_or_default();
    st.installed = holster_dir().is_some();
    st
}

/// Rewrites `state.json` for the preview: per enabled provider, its fullest
/// limit (the page turns `resetsAt` into "resets in 4h" itself, so it stays
/// right while Quota is closed).
pub fn publish(snap: &Snapshot) {
    let Some(me) = HOLSTER.get() else { return };
    let providers: Vec<Value> = snap
        .providers
        .iter()
        .filter(|p| p.enabled)
        .map(|p| {
            let headline = p.headline().map(|m| {
                json!({ "label": m.label, "fill": m.fill(), "resetsAt": m.resets_at, "periodMs": m.period_ms })
            });
            // The first two plan limits (session over weekly), stacked on the right.
            let limits: Vec<Value> = p
                .metrics
                .iter()
                .filter(|m| m.primary && m.fill().is_some())
                .take(2)
                .map(|m| json!({ "label": short_label(&m.id, &m.label), "fill": m.fill(), "resetsAt": m.resets_at, "periodMs": m.period_ms }))
                .collect();
            // Days stay dated: the page picks today's by its own clock.
            let days: Vec<Value> = p
                .spend
                .as_ref()
                .map(|s| s.days.iter().rev().take(2).map(|d| json!({ "date": d.date, "cost": d.cost })).collect())
                .unwrap_or_default();
            json!({
                "id": p.id,
                "name": p.name,
                "color": p.color,
                "icon": p.icon,
                "plan": p.plan,
                "status": p.status,
                "message": p.message,
                "headline": headline,
                "limits": limits,
                "days": days,
                "updatedAt": p.updated_at,
            })
        })
        .collect();
    // Where the page asks for fresh numbers when it's shown (absent until the
    // listener is up, or while Quota isn't running: the page then shows these).
    let port = me.status.lock().unwrap().port;
    let peek = (port != 0).then(|| format!("http://127.0.0.1:{port}/holster/peek"));
    let state = json!({ "version": 1, "checkedAt": snap.checked_at, "peek": peek, "providers": providers });
    let text = serde_json::to_string(&state).unwrap_or_default();
    if let Err(e) = write_if_changed(&me.dir.join("state.json"), &text) {
        eprintln!("holster: couldn't write the preview state: {e}");
    }
}

/// A limit's name, short enough to sit beside its percentage.
fn short_label(id: &str, label: &str) -> String {
    match id {
        "session" => "5h".into(),
        "weekly" => "Week".into(),
        "monthly" => "Month".into(),
        "daily" => "Day".into(),
        "total" => "Total".into(),
        _ => label.split_whitespace().next().unwrap_or(label).to_string(),
    }
}

/// Makes Holster's tab glow in Quota's color, unless the panel is out (the
/// user is already looking) or a glow just happened.
pub fn signal(app: &AppHandle) {
    let Some(me) = HOLSTER.get() else { return };
    let closed = app
        .try_state::<Arc<Core>>()
        .and_then(|c| c.try_shell().map(|s| s.mode() == Mode::Closed))
        .unwrap_or(true);
    if !closed {
        return;
    }
    {
        let mut last = me.last_signal.lock().unwrap();
        if last.is_some_and(|t| t.elapsed() < SIGNAL_EVERY) {
            return;
        }
        *last = Some(Instant::now());
    }
    tauri::async_runtime::spawn(async {
        let Some(api) = holster_api() else { return };
        let _ = client().post(format!("{api}/apps/{ID}/signal")).json(&json!({ "kind": "activity" })).send().await;
    });
}

impl Holster {
    /// Writes the manifest (only if Holster is installed), then asks a running
    /// Holster to validate it so Settings can show its verdict.
    async fn register(&self, port: u16, rows: usize) {
        let Some(dir) = holster_dir() else { return };
        let apps = dir.join("apps");
        let exe = std::env::current_exe().ok();
        let base = format!("http://127.0.0.1:{port}/holster");
        let manifest = json!({
            "schema": 1,
            "id": ID,
            "name": NAME,
            "icon": { "path": self.dir.join("icon.svg"), "size": "100%" },
            "color": COLOR,
            "health": format!("{base}/health"),
            "preview": {
                "path": self.dir.join("index.html"),
                "width": PREVIEW_WIDTH,
                "height": preview_height(rows),
            },
            "open": { "mode": "panel", "url": format!("{base}/open") },
            "launch": { "exe": exe, "args": [] },
        });
        let text = serde_json::to_string_pretty(&manifest).unwrap_or_default() + "\n";
        let written = std::fs::create_dir_all(&apps).and_then(|_| write_if_changed(&apps.join(format!("{ID}.json")), &text));
        if let Err(e) = written {
            eprintln!("holster: couldn't write the manifest: {e}");
            self.status.lock().unwrap().error = Some(format!("Couldn't register with Holster: {e}"));
            return;
        }
        self.status.lock().unwrap().registered = true;

        let Some(api) = holster_api() else { return };
        let Ok(resp) = client().post(format!("{api}/apps/{ID}/reload")).send().await else { return };
        let ok = resp.status().is_success();
        let body: Value = resp.json().await.unwrap_or(Value::Null);
        let mut st = self.status.lock().unwrap();
        st.warnings = body
            .get("warnings")
            .and_then(Value::as_array)
            .map(|w| w.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        st.error = if ok {
            None
        } else {
            Some(body.get("error").and_then(Value::as_str).unwrap_or("Holster rejected the manifest").to_string())
        };
        for w in &st.warnings {
            eprintln!("holster: {w}");
        }
        if let Some(e) = &st.error {
            eprintln!("holster: {e}");
        }
    }
}

// ---- loopback listener -------------------------------------------------------------

async fn serve(app: AppHandle) -> std::io::Result<u16> {
    // Loopback only. The preferred port keeps the manifest stable, so a
    // Holster that launches Quota finds health where it expects it.
    let listener = match tokio::net::TcpListener::bind(("127.0.0.1", PREFERRED_PORT)).await {
        Ok(l) => l,
        Err(_) => tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?,
    };
    let port = listener.local_addr()?.port();
    let router = Router::new()
        .route("/holster/health", get(|| async { "ok" }))
        .route("/holster/peek", post({
            // The preview is being looked at: check (unless the numbers are
            // fresh) and answer once state.json holds the result.
            let app = app.clone();
            move || async move {
                if let Some(core) = app.try_state::<Arc<Core>>() {
                    let poller = core.poller.clone();
                    poller.look().await;
                }
                StatusCode::NO_CONTENT
            }
        }))
        .route(
            "/holster/open",
            post(move |body: Bytes| async move {
                let anchor_y = serde_json::from_slice::<Value>(&body)
                    .ok()
                    .and_then(|v| v.get("anchorY").and_then(Value::as_f64));
                // Answer first; the panel opens on its own threads.
                if let Some(core) = app.try_state::<Arc<Core>>() {
                    if let Some(shell) = core.try_shell().cloned() {
                        std::thread::spawn(move || shell.open_from_dock(anchor_y));
                    }
                    core.poller.refresh_if_old();
                }
                Json(json!({ "ok": true }))
            }),
        )
        .fallback(|| async { (StatusCode::NOT_FOUND, "Not found") })
        .layer(middleware::from_fn_with_state(port, guard));
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    Ok(port)
}

fn is_loopback_origin(origin: &str) -> bool {
    ["http://127.0.0.1", "http://localhost"].iter().any(|base| {
        origin
            .strip_prefix(base)
            .is_some_and(|rest| rest.is_empty() || (rest.starts_with(':') && rest[1..].bytes().all(|b| b.is_ascii_digit())))
    })
}

/// DNS-rebinding and cross-site guard: the Host must be this loopback port,
/// and a browser request must come from a loopback page.
async fn guard(State(port): State<u16>, req: Request, next: Next) -> Response {
    let host = req.headers().get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or("");
    let host_ok = host == format!("127.0.0.1:{port}") || host == format!("localhost:{port}");
    let origin_ok = req
        .headers()
        .get(header::ORIGIN)
        .map(|o| o.to_str().is_ok_and(is_loopback_origin))
        .unwrap_or(true);
    if !host_ok || !origin_ok {
        return (StatusCode::FORBIDDEN, "Forbidden").into_response();
    }
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_origins() {
        assert!(is_loopback_origin("http://127.0.0.1:52710"));
        assert!(is_loopback_origin("http://localhost"));
        assert!(!is_loopback_origin("http://127.0.0.1.evil.com"));
        assert!(!is_loopback_origin("https://example.com"));
        assert!(!is_loopback_origin("null"));
    }

    #[test]
    fn preview_gets_every_logo() {
        let html = preview_html();
        assert!(!html.contains("/*QUOTA_LOGOS*/"));
        for p in crate::providers::PROVIDERS {
            assert!(LOGOS.iter().any(|(id, _, _)| *id == p.id), "{} has no logo", p.id);
        }
        // A provider's name can't break out of the script: markup is data in a JSON string.
        assert!(!html.contains("</script><svg"));
    }

    #[test]
    fn preview_sizes() {
        assert_eq!(preview_height(0), 108);
        assert_eq!(preview_height(3), 228);
    }
}
