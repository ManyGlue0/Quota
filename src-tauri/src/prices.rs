//! Live prices: LiteLLM's and models.dev's current lists, downloaded at launch
//! and then every few hours (the user picks how often), so a new model gets a
//! price without a new Quota. The converted lists are kept in the app folder
//! for offline starts. Off, Quota uses the lists it was built with.
//!
//! Only the two public lists are requested; nothing about the user is sent.
//! Each download asks "changed since this ETag?", so an unchanged list costs a
//! few hundred bytes.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};
use tokio::sync::Notify;

use crate::providers::now_ms;
use crate::settings::{Prefs, Settings};
use crate::spend::catalogs::{self, Fetched};
use crate::spend::pricing::{self, Pricing};
use crate::usage::Poller;

struct Source {
    id: &'static str,
    name: &'static str,
    url: &'static str,
    convert: fn(&str, &str) -> Option<String>,
    bundled: &'static str,
}

/// LiteLLM first: it's the primary catalog, models.dev fills its gaps.
const SOURCES: [Source; 2] = [
    Source { id: "litellm", name: "LiteLLM", url: catalogs::LITELLM_URL, convert: catalogs::litellm, bundled: pricing::BUNDLED_LITELLM },
    Source {
        id: "models_dev",
        name: "models.dev",
        url: catalogs::MODELS_DEV_URL,
        convert: catalogs::models_dev,
        bundled: pricing::BUNDLED_MODELS_DEV,
    },
];

/// After a failed download, try again this soon (or at the usual time, if sooner).
const RETRY_MS: i64 = 30 * 60_000;
/// Waits are cut into steps this long and re-measured on the wall clock, so a
/// PC that slept past the due time checks soon after it wakes.
const STEP_MS: i64 = 10 * 60_000;

#[derive(Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Saved {
    /// Last time every list downloaded (or was confirmed unchanged).
    checked_at: Option<i64>,
    etags: BTreeMap<String, String>,
}

#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PricesStatus {
    pub online: bool,
    /// The prices in use come from a download (else from the built-in lists).
    pub live: bool,
    pub checked_at: Option<i64>,
    pub checking: bool,
    /// How many model names have a price.
    pub models: usize,
    /// When the built-in lists were made (YYYY-MM-DD).
    pub built_in: String,
    pub error: Option<String>,
}

pub struct Prices {
    app: AppHandle,
    prefs: Arc<Prefs>,
    poller: Arc<Poller>,
    dir: PathBuf,
    saved: Mutex<Saved>,
    status: Mutex<PricesStatus>,
    failed_at: Mutex<Option<i64>>,
    kick: Notify,
}

impl Prices {
    /// Loads the last downloaded lists (when on) and puts them in use, so the
    /// first spend scan already has them.
    pub fn new(app: AppHandle, prefs: Arc<Prefs>, poller: Arc<Poller>, data_dir: &std::path::Path) -> Arc<Self> {
        let dir = data_dir.join("prices");
        let saved: Saved = std::fs::read_to_string(dir.join("state.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        let built_in = pricing::retrieved_at(pricing::BUNDLED_LITELLM).unwrap_or_default().chars().take(10).collect();
        let me = Arc::new(Self {
            app,
            prefs: prefs.clone(),
            poller,
            dir,
            saved: Mutex::new(saved),
            status: Mutex::new(PricesStatus { built_in, ..Default::default() }),
            failed_at: Mutex::new(None),
            kick: Notify::new(),
        });
        me.install(prefs.get().prices_online);
        me
    }

    pub fn status(&self) -> PricesStatus {
        let mut st = self.status.lock().unwrap().clone();
        st.checked_at = self.saved.lock().unwrap().checked_at;
        st
    }

    fn emit(&self) {
        let _ = self.app.emit("prices", self.status());
    }

    fn file(&self, s: &Source) -> PathBuf {
        self.dir.join(format!("{}.json", s.id))
    }

    /// Puts each list's newest usable copy in use: the download, when on and
    /// newer than the one built in (a Quota update can bring a newer one).
    fn install(&self, online: bool) {
        let mut live = false;
        let lists: Vec<String> = SOURCES
            .iter()
            .map(|s| {
                if online {
                    if let Ok(text) = std::fs::read_to_string(self.file(s)) {
                        if catalogs::usable(&text) && pricing::retrieved_at(&text) > pricing::retrieved_at(s.bundled) {
                            live = true;
                            return text;
                        }
                    }
                }
                s.bundled.to_string()
            })
            .collect();
        let p = Pricing::with_catalogs(&lists[0], &lists[1]);
        let models = p.models();
        pricing::install(p);
        let mut st = self.status.lock().unwrap();
        st.online = online;
        st.live = live;
        st.models = models;
    }

    /// Runs forever: a check at launch, then whenever one is due.
    pub fn start(self: &Arc<Self>) {
        let me = self.clone();
        tauri::async_runtime::spawn(async move {
            let mut first = true;
            loop {
                let s = me.prefs.get();
                if s.prices_online && (first || me.due(&s) <= now_ms()) {
                    me.check().await;
                }
                first = false;
                let s = me.prefs.get();
                if s.prices_online {
                    let wait = (me.due(&s) - now_ms()).clamp(60_000, STEP_MS) as u64;
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_millis(wait)) => {}
                        _ = me.kick.notified() => {}
                    }
                } else {
                    me.kick.notified().await;
                }
            }
        });
    }

    fn due(&self, s: &Settings) -> i64 {
        let every = s.prices_hours as i64 * 3_600_000;
        if let Some(failed) = *self.failed_at.lock().unwrap() {
            return failed + every.min(RETRY_MS);
        }
        self.saved.lock().unwrap().checked_at.unwrap_or(0) + every
    }

    /// Settings changed: on/off takes effect now; a new interval reschedules.
    pub fn apply_settings(self: &Arc<Self>, before: &Settings, after: &Settings) {
        if before.prices_online != after.prices_online {
            self.install(after.prices_online);
            self.emit();
            let poller = self.poller.clone();
            tauri::async_runtime::spawn(async move { poller.reprice().await });
        }
        if before.prices_online != after.prices_online || before.prices_hours != after.prices_hours {
            self.kick.notify_one();
        }
    }

    async fn check(&self) {
        self.status.lock().unwrap().checking = true;
        self.emit();

        let client = catalogs::client();
        let tag = |s: &Source| {
            // Without the file, "unchanged" would leave nothing to use.
            let have = self.file(s).exists();
            self.saved.lock().unwrap().etags.get(s.id).filter(|_| have).cloned()
        };
        let (t0, t1) = (tag(&SOURCES[0]), tag(&SOURCES[1]));
        let results = tokio::join!(
            catalogs::fetch(&client, SOURCES[0].url, t0.as_deref()),
            catalogs::fetch(&client, SOURCES[1].url, t1.as_deref()),
        );

        let mut changed = false;
        let mut problems = Vec::new();
        for (s, result) in SOURCES.iter().zip([results.0, results.1]) {
            match result {
                Ok(Fetched::Unchanged) => {}
                Ok(Fetched::New { text, etag }) => {
                    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
                    let convert = s.convert;
                    let catalog = tauri::async_runtime::spawn_blocking(move || convert(&text, &now)).await.ok().flatten();
                    let Some(catalog) = catalog else {
                        problems.push(format!("{}'s list looked wrong", s.name));
                        continue;
                    };
                    if self.save(s, &catalog).is_err() {
                        problems.push(format!("{}'s list couldn't be saved", s.name));
                        continue;
                    }
                    let mut saved = self.saved.lock().unwrap();
                    match etag {
                        Some(tag) => saved.etags.insert(s.id.into(), tag),
                        None => saved.etags.remove(s.id),
                    };
                    changed = true;
                }
                Err(e) => problems.push(format!("{}: {e}", s.name)),
            }
        }

        if problems.is_empty() {
            self.saved.lock().unwrap().checked_at = Some(now_ms());
            *self.failed_at.lock().unwrap() = None;
        } else {
            *self.failed_at.lock().unwrap() = Some(now_ms());
        }
        self.save_state();
        if changed && self.prefs.get().prices_online {
            self.install(true);
        }
        {
            let mut st = self.status.lock().unwrap();
            st.checking = false;
            st.error = (!problems.is_empty()).then(|| format!("Couldn't get new prices ({})", problems.join(", ")));
        }
        self.emit();
        if changed {
            self.poller.reprice().await;
        }
    }

    fn save(&self, s: &Source, catalog: &str) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let path = self.file(s);
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, catalog)?;
        std::fs::rename(&tmp, &path)
    }

    fn save_state(&self) {
        let Ok(text) = serde_json::to_string(&*self.saved.lock().unwrap()) else { return };
        let _ = std::fs::create_dir_all(&self.dir);
        let path = self.dir.join("state.json");
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, text).and_then(|_| std::fs::rename(&tmp, &path)).is_err() {
            eprintln!("prices: couldn't save the download state");
        }
    }
}
