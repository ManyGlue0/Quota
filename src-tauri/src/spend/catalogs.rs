//! The public price lists Quota reads, LiteLLM's and models.dev's, turned into
//! the compact catalog `pricing` loads:
//! `{"retrieved_at": "...", "models": {name: {"i", "o", "cw", "cr", ...}}}`,
//! USD per million tokens. `crate::prices` downloads them on a schedule; the
//! same conversion makes the snapshots bundled with Quota.
//!
//! Compact fields: `i`/`o` input/output, `cw`/`cr` 5-minute cache write/read
//! (`cre: false` when the list has no cache-read rate, so it is estimated),
//! `ia`/`oa`/`cwa`/`cra` the same above `la` prompt tokens (default 200k), and
//! `fast` a fast-mode multiplier.

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};

pub const LITELLM_URL: &str = "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";
pub const MODELS_DEV_URL: &str = "https://models.dev/api.json";

/// A real list prices thousands of models; fewer means a broken download.
pub const MIN_MODELS: usize = 500;
/// No real rate comes close; anything above is a typo in the list.
const MAX_RATE: f64 = 10_000.0;
/// The long-prompt threshold `pricing` assumes when a model names none.
const DEFAULT_LONG: u64 = 200_000;

/// Which provider's price wins when models.dev lists a model under several:
/// the model's maker, then the first in alphabetical order.
const MAKERS: [&str; 13] = [
    "anthropic", "openai", "google", "xai", "deepseek", "mistral", "moonshotai", "zai", "alibaba", "minimax", "cohere",
    "meta", "perplexity",
];

/// A rate in USD per million tokens, if it's a sane one.
fn sane(x: f64) -> Option<f64> {
    (x.is_finite() && (0.0..MAX_RATE).contains(&x)).then(|| (x * 1e6).round() / 1e6)
}

/// LiteLLM's `model_prices_and_context_window.json` (USD per token).
pub fn litellm(text: &str, retrieved_at: &str) -> Option<String> {
    let root: Value = serde_json::from_str(text).ok()?;
    let mut models = BTreeMap::new();
    for (name, m) in root.as_object()? {
        let Some(m) = m.as_object() else { continue };
        if name == "sample_spec" {
            continue;
        }
        // Text models only: image, audio and embedding prices aren't per token in/out.
        if !matches!(m.get("mode").and_then(Value::as_str), None | Some("chat" | "responses" | "completion")) {
            continue;
        }
        let per_token = |k: &str| m.get(k).and_then(Value::as_f64).and_then(|x| sane(x * 1e6));
        let (Some(i), Some(o)) = (per_token("input_cost_per_token"), per_token("output_cost_per_token")) else { continue };
        let mut e = Map::new();
        e.insert("i".into(), json!(i));
        e.insert("o".into(), json!(o));
        if let Some(cw) = per_token("cache_creation_input_token_cost") {
            e.insert("cw".into(), json!(cw));
        }
        match per_token("cache_read_input_token_cost") {
            Some(cr) => e.insert("cr".into(), json!(cr)),
            None => e.insert("cre".into(), json!(false)),
        };
        // The long-prompt tier ("..._above_200k_tokens"); the lowest if several.
        let tier = m
            .keys()
            .filter_map(|k| k.strip_prefix("input_cost_per_token_above_")?.strip_suffix("k_tokens")?.parse::<u64>().ok())
            .min();
        if let Some(k) = tier {
            let at = |base: &str| per_token(&format!("{base}_above_{k}k_tokens"));
            for (field, base) in [
                ("ia", "input_cost_per_token"),
                ("oa", "output_cost_per_token"),
                ("cwa", "cache_creation_input_token_cost"),
                ("cra", "cache_read_input_token_cost"),
            ] {
                if let Some(v) = at(base) {
                    e.insert(field.into(), json!(v));
                }
            }
            if k * 1000 != DEFAULT_LONG {
                e.insert("la".into(), json!(k * 1000));
            }
        }
        let fast = m.get("provider_specific_entry").and_then(|p| p.get("fast")).and_then(Value::as_f64);
        if let Some(f) = fast.filter(|f| (1.0..=20.0).contains(f) && *f != 1.0) {
            e.insert("fast".into(), json!(f));
        }
        models.insert(name.clone(), Value::Object(e));
    }
    finish(models, retrieved_at)
}

/// models.dev's `api.json` (USD per million tokens, models grouped by provider).
pub fn models_dev(text: &str, retrieved_at: &str) -> Option<String> {
    let root: Value = serde_json::from_str(text).ok()?;
    let mut providers: Vec<(&String, &Value)> = root.as_object()?.iter().collect();
    providers.sort_by_key(|(id, _)| (MAKERS.iter().position(|m| m == id).unwrap_or(MAKERS.len()), id.as_str()));
    let mut models: BTreeMap<String, Value> = BTreeMap::new();
    for (_, p) in providers {
        for (name, m) in p["models"].as_object().into_iter().flatten() {
            let cost = &m["cost"];
            let rate = |v: &Value, k: &str| v.get(k).and_then(Value::as_f64).and_then(sane);
            let (Some(i), Some(o)) = (rate(cost, "input"), rate(cost, "output")) else { continue };
            // An earlier provider wins, unless it lists the model as free (a
            // subscription plan, not a price) and this one doesn't.
            if let Some(prev) = models.get(name) {
                if prev["i"].as_f64() != Some(0.0) || prev["o"].as_f64() != Some(0.0) || (i == 0.0 && o == 0.0) {
                    continue;
                }
            }
            let mut e = Map::new();
            e.insert("i".into(), json!(i));
            e.insert("o".into(), json!(o));
            if let Some(cw) = rate(cost, "cache_write") {
                e.insert("cw".into(), json!(cw));
            }
            match rate(cost, "cache_read") {
                Some(cr) => e.insert("cr".into(), json!(cr)),
                None => e.insert("cre".into(), json!(false)),
            };
            // The long-prompt tier: the first context tier, or the older field.
            let tier = cost["tiers"]
                .as_array()
                .into_iter()
                .flatten()
                .find_map(|t| (t["tier"]["type"] == "context").then(|| Some((t, t["tier"]["size"].as_u64()?))).flatten())
                .or_else(|| cost.get("context_over_200k").map(|t| (t, DEFAULT_LONG)));
            if let Some((t, size)) = tier {
                for (field, k) in [("ia", "input"), ("oa", "output"), ("cwa", "cache_write"), ("cra", "cache_read")] {
                    if let Some(v) = rate(t, k) {
                        e.insert(field.into(), json!(v));
                    }
                }
                if size != DEFAULT_LONG {
                    e.insert("la".into(), json!(size));
                }
            }
            models.insert(name.clone(), Value::Object(e));
        }
    }
    finish(models, retrieved_at)
}

fn finish(models: BTreeMap<String, Value>, retrieved_at: &str) -> Option<String> {
    if models.len() < MIN_MODELS {
        return None;
    }
    let models: Map<String, Value> = models.into_iter().collect();
    serde_json::to_string(&json!({ "retrieved_at": retrieved_at, "models": models })).ok()
}

/// A compact catalog is usable: it parses and prices enough models.
pub fn usable(catalog: &str) -> bool {
    serde_json::from_str::<Value>(catalog)
        .ok()
        .and_then(|v| v["models"].as_object().map(|m| m.len() >= MIN_MODELS))
        .unwrap_or(false)
}

pub enum Fetched {
    /// Same list as last time (the server said so from the ETag).
    Unchanged,
    New { text: String, etag: Option<String> },
}

/// Downloads a list, asking only for a changed one when `etag` is known.
pub async fn fetch(client: &reqwest::Client, url: &str, etag: Option<&str>) -> Result<Fetched, String> {
    let mut req = client.get(url);
    if let Some(tag) = etag {
        req = req.header(reqwest::header::IF_NONE_MATCH, tag);
    }
    let res = req.send().await.map_err(|e| if e.is_timeout() { "timed out".to_string() } else { "no connection".to_string() })?;
    if res.status() == reqwest::StatusCode::NOT_MODIFIED {
        return Ok(Fetched::Unchanged);
    }
    if !res.status().is_success() {
        return Err(format!("HTTP {}", res.status().as_u16()));
    }
    let etag = res.headers().get(reqwest::header::ETAG).and_then(|v| v.to_str().ok()).map(str::to_string);
    let text = res.text().await.map_err(|_| "download cut off".to_string())?;
    Ok(Fetched::New { text, etag })
}

pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(concat!("Quota/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(90))
        .build()
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spend::pricing::{Pricing, Tokens};

    fn padded(extra: Value, pad: impl Fn(usize) -> (String, Value)) -> String {
        let mut root = extra.as_object().unwrap().clone();
        for n in 0..MIN_MODELS {
            let (k, v) = pad(n);
            root.insert(k, v);
        }
        Value::Object(root).to_string()
    }

    #[test]
    fn converts_litellm() {
        let text = padded(
            json!({
                "sample_spec": { "input_cost_per_token": 1.0, "output_cost_per_token": 1.0 },
                "claude-fable-5-2": {
                    "mode": "chat", "input_cost_per_token": 8e-6, "output_cost_per_token": 4e-5,
                    "cache_creation_input_token_cost": 1e-5, "cache_read_input_token_cost": 2e-7,
                    "cache_creation_input_token_cost_above_1hr": 1.6e-5,
                    "provider_specific_entry": { "us": 1.1, "fast": 2 }
                },
                "gpt-5.5": {
                    "mode": "chat", "input_cost_per_token": 5e-6, "output_cost_per_token": 3e-5,
                    "cache_read_input_token_cost": 5e-7,
                    "input_cost_per_token_above_272k_tokens": 1e-5, "output_cost_per_token_above_272k_tokens": 4.5e-5,
                    "input_cost_per_token_above_272k_tokens_batches": 5e-6
                },
                "dall-e-3": { "mode": "image_generation", "input_cost_per_token": 1e-6, "output_cost_per_token": 1e-6 },
                "broken": { "mode": "chat", "input_cost_per_token": "free" }
            }),
            |n| (format!("pad-{n}"), json!({ "input_cost_per_token": 1e-6, "output_cost_per_token": 2e-6 })),
        );
        let out: Value = serde_json::from_str(&litellm(&text, "2026-11-10T00:00:00Z").unwrap()).unwrap();
        let m = &out["models"];
        assert_eq!(out["retrieved_at"], "2026-11-10T00:00:00Z");
        assert_eq!(m["claude-fable-5-2"], json!({ "i": 8.0, "o": 40.0, "cw": 10.0, "cr": 0.2, "fast": 2.0 }));
        assert_eq!(m["gpt-5.5"], json!({ "i": 5.0, "o": 30.0, "cr": 0.5, "ia": 10.0, "oa": 45.0, "la": 272000 }));
        assert_eq!(m["pad-0"], json!({ "i": 1.0, "o": 2.0, "cre": false }));
        for gone in ["sample_spec", "dall-e-3", "broken"] {
            assert!(m.get(gone).is_none(), "{gone}");
        }
    }

    #[test]
    fn converts_models_dev() {
        let text = json!({
            "aaa-reseller": { "models": {
                "claude-fable-5-2": { "cost": { "input": 9, "output": 45 } },
                "glm-9": { "cost": { "input": 0, "output": 0 } }
            }},
            "anthropic": { "models": {
                "claude-fable-5-2": { "cost": { "input": 8, "output": 40, "cache_read": 0.2, "cache_write": 10 } },
                "claude-haiku-5-5": { "cost": { "input": 0.1, "output": 0.5, "cache_read": 0.01, "cache_write": 0.125,
                    "tiers": [{ "input": 0.5, "output": 2.5, "cache_read": 0.05, "cache_write": 0.625, "tier": { "type": "context", "size": 100000 } }] } }
            }},
            "zzz": { "models": {
                "glm-9": { "cost": { "input": 1, "output": 3 } },
                "pad": { "cost": {} }
            }},
            "pads": { "models": (0..MIN_MODELS).map(|n| (format!("pad-{n}"), json!({ "cost": { "input": 1, "output": 2 } }))).collect::<Map<_, _>>() }
        })
        .to_string();
        let out: Value = serde_json::from_str(&models_dev(&text, "t").unwrap()).unwrap();
        let m = &out["models"];
        // The maker's price beats a reseller's, a real price beats "free".
        assert_eq!(m["claude-fable-5-2"], json!({ "i": 8.0, "o": 40.0, "cw": 10.0, "cr": 0.2 }));
        assert_eq!(m["glm-9"], json!({ "i": 1.0, "o": 3.0, "cre": false }));
        assert_eq!(
            m["claude-haiku-5-5"],
            json!({ "i": 0.1, "o": 0.5, "cw": 0.125, "cr": 0.01, "ia": 0.5, "oa": 2.5, "cwa": 0.625, "cra": 0.05, "la": 100000 })
        );
        assert!(m.get("pad").is_none());
    }

    #[test]
    fn too_few_models_is_a_broken_download() {
        assert!(litellm(r#"{"a": {"input_cost_per_token": 1e-6, "output_cost_per_token": 1e-6}}"#, "t").is_none());
        assert!(litellm("<html>rate limited</html>", "t").is_none());
        assert!(models_dev("{}", "t").is_none());
        assert!(!usable(r#"{"models": {}}"#));
    }

    /// A converted catalog prices like the bundled one does, long tiers included.
    #[test]
    fn converted_catalog_prices_models() {
        let text = padded(
            json!({ "gpt-5.5": {
                "mode": "chat", "input_cost_per_token": 5e-6, "output_cost_per_token": 3e-5, "cache_read_input_token_cost": 5e-7,
                "input_cost_per_token_above_272k_tokens": 1e-5, "output_cost_per_token_above_272k_tokens": 4.5e-5
            }}),
            |n| (format!("pad-{n}"), json!({ "input_cost_per_token": 1e-6, "output_cost_per_token": 2e-6 })),
        );
        let catalog = litellm(&text, "t").unwrap();
        let p = Pricing::with_catalogs(&catalog, r#"{"models":{}}"#);
        let r = p.resolve("gpt-5.5").unwrap();
        let short = Tokens { input: 272_000, output: 1_000_000, ..Default::default() };
        assert!((r.cost(&short, true) - (272_000.0 * 5.0 / 1e6 + 30.0)).abs() < 1e-9);
        let long = Tokens { input: 272_001, output: 1_000_000, ..Default::default() };
        assert!((r.cost(&long, true) - (272_001.0 * 10.0 / 1e6 + 45.0)).abs() < 1e-9);
    }

    /// Rewrites the snapshots bundled with Quota from the live lists. Run before
    /// a release: `cargo test --lib refresh_bundled_prices -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn refresh_bundled_prices() {
        let client = client();
        let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/pricing");
        for (url, file, convert) in [
            (LITELLM_URL, "litellm.json", litellm as fn(&str, &str) -> Option<String>),
            (MODELS_DEV_URL, "models_dev.json", models_dev),
        ] {
            let Fetched::New { text, .. } = fetch(&client, url, None).await.unwrap() else { unreachable!() };
            let catalog = convert(&text, &now).expect("list converts");
            std::fs::write(dir.join(file), catalog).unwrap();
            println!("{file} rewritten");
        }
    }
}
