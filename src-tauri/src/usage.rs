//! The poller: refreshes every enabled provider on the user's interval (and
//! on demand), keeps the last good numbers when a fetch fails, backs off when
//! a service rate-limits us, and decides when a change is worth a glow on
//! Holster's tab (a limit crossing the alert threshold, or a limit you were
//! close to resetting).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};
use tokio::sync::Notify;

use crate::providers::{self, FetchError, Metric, PROVIDERS};
use crate::settings::{Prefs, Settings};
use crate::spend::{self, Spend};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Status {
    /// Not fetched yet.
    Loading,
    Ok,
    /// Showing the last good numbers; the latest fetch failed (see message).
    Stale,
    /// No usable login on this machine.
    SignedOut,
    /// Failed, with nothing earlier to fall back on.
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderState {
    pub id: String,
    pub name: String,
    pub color: String,
    pub icon: String,
    pub enabled: bool,
    pub status: Status,
    pub message: Option<String>,
    pub plan: Option<String>,
    pub metrics: Vec<Metric>,
    /// Last successful fetch (epoch ms).
    pub updated_at: Option<i64>,
    /// Last attempt (epoch ms).
    pub checked_at: Option<i64>,
    /// What the last 30 days would cost at API rates, per day.
    pub spend: Option<Spend>,
    /// While rate-limited: no requests before this (epoch ms).
    pub retry_at: Option<i64>,
}

impl ProviderState {
    /// The fullest plan limit: what the preview leads with.
    pub fn headline(&self) -> Option<&Metric> {
        self.metrics
            .iter()
            .filter(|m| m.primary && m.fill().is_some())
            .max_by(|a, b| a.fill().partial_cmp(&b.fill()).unwrap_or(std::cmp::Ordering::Equal))
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub providers: Vec<ProviderState>,
    pub refreshing: bool,
    /// Last completed refresh round (epoch ms).
    pub checked_at: Option<i64>,
}

pub struct Poller {
    app: AppHandle,
    prefs: Arc<Prefs>,
    file: PathBuf,
    snap: Mutex<Snapshot>,
    busy: AtomicBool,
    kick: Notify,
    /// Counts finished refresh rounds, for callers that wait for fresh numbers.
    rounds: tokio::sync::watch::Sender<u64>,
}

/// Looking (a hover, the panel opening) checks only numbers older than this,
/// so sweeping across the dock doesn't run a check per pass.
const FRESH_MS: i64 = 30_000;
/// A provider is asked at most this often, however often refreshes are kicked.
const MIN_GAP_MS: i64 = 15_000;
/// When a service rate-limits us without saying for how long.
const DEFAULT_BACKOFF_MS: i64 = 10 * 60_000;

impl Poller {
    pub fn new(app: AppHandle, prefs: Arc<Prefs>, data_dir: &std::path::Path) -> Arc<Self> {
        let file = data_dir.join("usage.json");
        // Last known numbers, so the panel and the preview have something to
        // show before the first refresh finishes.
        let cached: Snapshot = std::fs::read_to_string(&file).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        let settings = prefs.get();
        let providers = PROVIDERS
            .iter()
            .map(|p| {
                let mut st = cached.providers.iter().find(|c| c.id == p.id).cloned().unwrap_or_else(|| ProviderState {
                    id: p.id.into(),
                    name: p.name.into(),
                    color: p.color.into(),
                    icon: p.icon.into(),
                    enabled: true,
                    status: Status::Loading,
                    message: None,
                    plan: None,
                    metrics: Vec::new(),
                    updated_at: None,
                    checked_at: None,
                    spend: None,
                    retry_at: None,
                });
                st.name = p.name.into();
                st.color = p.color.into();
                st.icon = p.icon.into();
                st.enabled = settings.provider_on(p.id);
                st
            })
            .collect();
        Arc::new(Self {
            app,
            prefs,
            file,
            snap: Mutex::new(Snapshot { providers, refreshing: false, checked_at: cached.checked_at }),
            busy: AtomicBool::new(false),
            kick: Notify::new(),
            rounds: tokio::sync::watch::channel(0).0,
        })
    }

    pub fn snapshot(&self) -> Snapshot {
        self.snap.lock().unwrap().clone()
    }

    /// Runs forever. On a schedule: a refresh now, then one per interval or
    /// whenever kicked. "When I look" (interval 0): nothing goes out until a
    /// hover or the panel asks; at launch only the local logs are read, so
    /// spend is ready the first time you look.
    pub fn start(self: &Arc<Self>) {
        let me = self.clone();
        tauri::async_runtime::spawn(async move {
            let mut first = true;
            loop {
                if first && me.on_demand() {
                    me.warm_spend().await;
                } else {
                    me.refresh_all(first).await;
                }
                first = false;
                let minutes = me.prefs.get().refresh_minutes;
                if minutes == 0 {
                    me.kick.notified().await;
                } else {
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_secs(minutes as u64 * 60)) => {}
                        _ = me.kick.notified() => {}
                    }
                }
            }
        });
    }

    fn on_demand(&self) -> bool {
        self.prefs.get().refresh_minutes == 0
    }

    fn is_fresh(&self) -> bool {
        let checked = self.snap.lock().unwrap().checked_at;
        checked.is_some_and(|t| providers::now_ms() - t < FRESH_MS)
    }

    /// Someone is looking (the preview on hover): refresh unless the numbers
    /// are fresh, and return once they are (or after a while, regardless).
    pub async fn look(&self) {
        if self.is_fresh() {
            return;
        }
        let mut rounds = self.rounds.subscribe();
        rounds.borrow_and_update();
        self.refresh();
        let _ = tokio::time::timeout(Duration::from_secs(20), rounds.changed()).await;
    }

    /// Reads the local logs only (no network), to warm the scan caches.
    async fn warm_spend(&self) {
        if self.busy.swap(true, Ordering::SeqCst) {
            return;
        }
        self.refresh_spend(true).await;
        self.busy.store(false, Ordering::SeqCst);
        self.emit();
        self.persist();
        self.publish();
    }

    /// Refresh now (the refresh button, a settings change).
    pub fn refresh(&self) {
        self.kick.notify_one();
    }

    /// The panel opened: refresh unless the numbers are fresh.
    pub fn refresh_if_old(&self) {
        if !self.is_fresh() {
            self.refresh();
        }
    }

    /// Settings changed: providers on or off take effect right away.
    pub fn apply_settings(&self, s: &Settings) {
        {
            let mut snap = self.snap.lock().unwrap();
            for p in &mut snap.providers {
                p.enabled = s.provider_on(&p.id);
            }
        }
        self.publish();
        self.refresh();
    }

    async fn refresh_all(&self, first: bool) {
        if self.busy.swap(true, Ordering::SeqCst) {
            return;
        }
        let now = providers::now_ms();
        let due: Vec<String> = {
            let mut snap = self.snap.lock().unwrap();
            snap.refreshing = true;
            snap.providers
                .iter()
                .filter(|p| p.enabled && p.retry_at.is_none_or(|t| t <= now))
                .filter(|p| p.checked_at.is_none_or(|t| now - t >= MIN_GAP_MS))
                .map(|p| p.id.clone())
                .collect()
        };
        self.emit();

        let tasks: Vec<_> = due
            .into_iter()
            .map(|id| tauri::async_runtime::spawn(async move { (id.clone(), providers::fetch(&id).await) }))
            .collect();
        let mut results = Vec::new();
        for t in tasks {
            if let Ok(r) = t.await {
                results.push(r);
            }
        }

        let settings = self.prefs.get();
        let mut glow = false;
        {
            let mut snap = self.snap.lock().unwrap();
            let now = providers::now_ms();
            for (id, result) in results {
                let Some(p) = snap.providers.iter_mut().find(|p| p.id == id) else { continue };
                let before = p.clone();
                apply(p, result, now);
                // No glow for the first round after launch (nothing changed while
                // we watched), nor when checks only happen because you're looking.
                if !first && settings.refresh_minutes > 0 && worth_a_glow(&before, p, &settings) {
                    glow = true;
                }
            }
            snap.checked_at = Some(now);
        }
        self.emit();
        self.publish();
        if glow {
            crate::holster::signal(&self.app);
        }

        // Spend second: the logs can take a while on the first scan, and the
        // limits shouldn't wait for them.
        self.refresh_spend(false).await;
        self.snap.lock().unwrap().refreshing = false;
        self.busy.store(false, Ordering::SeqCst);
        self.rounds.send_modify(|n| *n += 1);
        self.emit();
        self.persist();
        self.publish();
    }

    /// `local_only`: read the logs, skip Cursor's export download.
    async fn refresh_spend(&self, local_only: bool) {
        // Logs for every provider that's on; downloads only for signed-in ones.
        let ids: Vec<String> = {
            let snap = self.snap.lock().unwrap();
            snap.providers
                .iter()
                .filter(|p| p.enabled && (!spend::is_remote(&p.id) || (!local_only && p.status != Status::SignedOut)))
                .map(|p| p.id.clone())
                .collect()
        };
        let tasks: Vec<_> = ids
            .into_iter()
            .map(|id| {
                tauri::async_runtime::spawn(async move {
                    let spend = if spend::is_remote(&id) {
                        spend::fetch_remote(&id).await
                    } else {
                        let scan_id = id.clone();
                        tauri::async_runtime::spawn_blocking(move || spend::scan_local(&scan_id)).await.ok().flatten()
                    };
                    (id, spend)
                })
            })
            .collect();
        let mut results = Vec::new();
        for t in tasks {
            if let Ok(r) = t.await {
                results.push(r);
            }
        }
        let mut snap = self.snap.lock().unwrap();
        for p in snap.providers.iter_mut().filter(|p| !p.enabled) {
            p.spend = None;
        }
        for (id, spend) in results {
            if let Some(p) = snap.providers.iter_mut().find(|p| p.id == id) {
                // A failed export keeps the last good history rather than blanking it.
                if spend.is_some() || !p.enabled {
                    p.spend = spend;
                }
            }
        }
    }

    fn emit(&self) {
        let _ = self.app.emit("usage", self.snapshot());
    }

    fn persist(&self) {
        let snap = self.snapshot();
        let Ok(text) = serde_json::to_string(&snap) else { return };
        let tmp = self.file.with_extension("json.tmp");
        if std::fs::write(&tmp, text).and_then(|_| std::fs::rename(&tmp, &self.file)).is_err() {
            eprintln!("usage: couldn't save the last known numbers");
        }
    }

    /// Rewrites the Holster preview snapshot.
    fn publish(&self) {
        crate::holster::publish(&self.snapshot());
    }
}

fn apply(p: &mut ProviderState, result: providers::FetchResult, now: i64) {
    p.retry_at = None;
    p.checked_at = Some(now);
    match result {
        Ok(usage) => {
            p.status = Status::Ok;
            p.message = None;
            p.plan = usage.plan.or(p.plan.take());
            p.metrics = usage.metrics;
            p.updated_at = Some(now);
        }
        Err(FetchError::SignedOut(msg)) => {
            p.status = Status::SignedOut;
            p.message = Some(msg);
            p.metrics.clear();
            p.plan = None;
        }
        Err(FetchError::RateLimited { retry_after_secs }) => {
            let wait = retry_after_secs.map(|s| s as i64 * 1000).unwrap_or(DEFAULT_BACKOFF_MS).max(60_000);
            p.retry_at = Some(now + wait);
            let mins = (wait + 59_999) / 60_000;
            p.status = if p.metrics.is_empty() { Status::Error } else { Status::Stale };
            p.message = Some(format!("{} is limiting updates. Trying again in about {mins} min", p.name));
        }
        Err(FetchError::Failed(msg)) => {
            p.status = if p.metrics.is_empty() { Status::Error } else { Status::Stale };
            p.message = Some(msg);
        }
    }
}

/// A plan limit crossed the alert threshold (or ran out), or a limit you
/// were close to reset. Compared against the previous good numbers.
fn worth_a_glow(before: &ProviderState, after: &ProviderState, s: &Settings) -> bool {
    if after.status != Status::Ok || before.metrics.is_empty() {
        return false;
    }
    let threshold = s.alert_at as f64;
    let close = if threshold > 0.0 { threshold } else { 80.0 };
    after.metrics.iter().filter(|m| m.primary).any(|m| {
        let (Some(now), Some(prev)) = (m.fill(), before.metrics.iter().find(|b| b.id == m.id).and_then(Metric::fill)) else {
            return false;
        };
        let crossed = threshold > 0.0 && ((prev < threshold && now >= threshold) || (prev < 100.0 && now >= 100.0));
        let window_rolled = before.metrics.iter().find(|b| b.id == m.id).is_some_and(|b| b.resets_at != m.resets_at);
        let reset = s.glow_on_reset && prev >= close && now < close && now <= prev - 20.0 && window_rolled;
        crossed || reset
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(fill: f64, resets: i64) -> ProviderState {
        ProviderState {
            id: "claude".into(),
            name: "Claude".into(),
            color: "peach".into(),
            icon: "sparkle".into(),
            enabled: true,
            status: Status::Ok,
            message: None,
            plan: None,
            metrics: vec![Metric::percent("session", "Session", fill, Some(resets), None)],
            updated_at: None,
            checked_at: None,
            spend: None,
            retry_at: None,
        }
    }

    #[test]
    fn glows_on_crossing_and_reset_only() {
        let s = Settings::default();
        assert!(worth_a_glow(&state(70.0, 1), &state(85.0, 1), &s));
        assert!(!worth_a_glow(&state(85.0, 1), &state(88.0, 1), &s));
        assert!(worth_a_glow(&state(92.0, 1), &state(100.0, 1), &s));
        assert!(worth_a_glow(&state(95.0, 1), &state(0.0, 2), &s));
        assert!(!worth_a_glow(&state(40.0, 1), &state(0.0, 2), &s));
        let off = Settings { alert_at: 0, glow_on_reset: false, ..Settings::default() };
        assert!(!worth_a_glow(&state(70.0, 1), &state(100.0, 1), &off));
        assert!(!worth_a_glow(&state(95.0, 1), &state(0.0, 2), &off));
    }

    #[test]
    fn keeps_last_numbers_when_a_fetch_fails() {
        let mut p = state(40.0, 1);
        apply(&mut p, Err(FetchError::Failed("Couldn't reach Claude".into())), 10);
        assert_eq!(p.status, Status::Stale);
        assert_eq!(p.metrics.len(), 1);
        apply(&mut p, Err(FetchError::RateLimited { retry_after_secs: Some(300) }), 10);
        assert_eq!(p.retry_at, Some(300_010));
        apply(&mut p, Err(FetchError::SignedOut("Run claude".into())), 10);
        assert_eq!(p.status, Status::SignedOut);
        assert!(p.metrics.is_empty());
    }

    #[test]
    fn headline_is_the_fullest_limit() {
        let mut p = state(17.0, 1);
        p.metrics.push(Metric::percent("weekly", "Weekly", 60.0, None, None));
        p.metrics.push(Metric::count("resets", "Limit resets", 3.0, "available"));
        assert_eq!(p.headline().unwrap().id, "weekly");
    }
}
