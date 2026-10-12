//! Grok spend from the Grok CLI's session transcripts
//! (`~/.grok/sessions/**/updates.jsonl`, or `$GROK_HOME/sessions`).
//!
//! Each completed turn records its tokens per model and, usually, what Grok
//! billed for it (`costUsdTicks`, 1e-10 dollars). That cost is used as-is;
//! older turns without one are estimated from tokens. Child and resumed
//! sessions can replay a parent's events, so events are counted once per
//! event id and model (OpenUsage's rules).

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::Value;

use super::pricing::{pricing, Tokens};
use super::*;

/// Implausible counts (corrupt logs) are capped, not trusted.
const MAX_TOKENS: f64 = 1e12;

#[derive(Debug, Clone)]
pub struct Entry {
    ts: i64,
    event_id: Option<Arc<str>>,
    model: Arc<str>,
    tokens: Tokens,
    cost: Option<f64>,
}

fn sessions_dir() -> Option<PathBuf> {
    let home = std::env::var_os("GROK_HOME")
        .filter(|v| !v.to_string_lossy().trim().is_empty())
        .map(PathBuf::from)
        .or_else(|| crate::providers::home().map(|h| h.join(".grok")))?;
    Some(home.join("sessions"))
}

fn cache() -> &'static Mutex<ScanCache<Entry, ()>> {
    static C: OnceLock<Mutex<ScanCache<Entry, ()>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(ScanCache::new()))
}

/// Scans (incrementally) and totals the last 30 days. Blocking: run off the async runtime.
pub fn scan() -> Option<Spend> {
    let since = since_ms();
    let mut files = Vec::new();
    jsonl_files(&sessions_dir()?, since, &mut files);
    files.retain(|f| f.file_name().is_some_and(|n| n == "updates.jsonl"));
    files.sort();
    let mut cache = cache().lock().unwrap();
    cache.update(&files, |line, _, out| out.extend(parse_line(line)));
    let entries = dedup(cache.items());
    Some(aggregate(&entries, since))
}

fn count(v: &Value) -> u64 {
    match crate::providers::num(Some(v)) {
        Some(n) if n > 0.0 => n.min(MAX_TOKENS) as u64,
        _ => 0,
    }
}

pub fn parse_line(line: &[u8]) -> Vec<Entry> {
    if !contains(line, b"turn_completed") {
        return Vec::new();
    }
    let Ok(o) = serde_json::from_slice::<Value>(line) else { return Vec::new() };
    let params = &o["params"];
    let update = if params["update"].is_object() { &params["update"] } else { &o["update"] };
    if update["sessionUpdate"].as_str() != Some("turn_completed") {
        return Vec::new();
    }
    let usage = &update["usage"];
    let Some(models) = usage["modelUsage"].as_object() else { return Vec::new() };
    let meta = if params["_meta"].is_object() { &params["_meta"] } else { &o["_meta"] };
    let Some(ts) = timestamp(&o, meta) else { return Vec::new() };
    let event_id = meta["eventId"].as_str().map(str::trim).filter(|s| !s.is_empty()).map(Arc::<str>::from);
    let top_ticks = crate::providers::num(usage.get("costUsdTicks"));

    let mut out = Vec::new();
    let mut names: Vec<_> = models.keys().collect();
    names.sort();
    for name in names {
        let model = name.trim();
        let v = &models[name];
        if model.is_empty() || !crate::providers::num(v.get("inputTokens")).is_some_and(|n| n >= 0.0) {
            continue;
        }
        let input = count(&v["inputTokens"]);
        let cache_read = count(&v["cachedReadTokens"]).min(input);
        let cache_write = count(&v["cacheCreationTokens"]).min(input - cache_read);
        // Reasoning is already part of outputTokens.
        let output = count(&v["outputTokens"]);
        let ticks = crate::providers::num(v.get("costUsdTicks")).or(if models.len() == 1 { top_ticks } else { None });
        out.push(Entry {
            ts,
            event_id: event_id.clone(),
            model: model.into(),
            tokens: Tokens {
                input: input - cache_read - cache_write,
                cache_write_5m: cache_write,
                cache_write_1h: 0,
                cache_read,
                output,
                fast: false,
            },
            cost: ticks.filter(|t| *t >= 0.0).map(|t| t / 1e10),
        });
    }
    out
}

fn timestamp(o: &Value, meta: &Value) -> Option<i64> {
    if let Some(ms) = crate::providers::num(meta.get("agentTimestampMs")).filter(|n| *n > 0.0) {
        return Some(ms as i64);
    }
    match &o["timestamp"] {
        Value::String(s) => parse_ts(s),
        v => crate::providers::num(Some(v)).filter(|n| *n > 0.0).map(|s| (s * 1000.0) as i64),
    }
}

fn dedup(entries: Vec<&Entry>) -> Vec<Entry> {
    let mut seen = std::collections::HashSet::new();
    entries
        .into_iter()
        .filter(|e| match &e.event_id {
            Some(id) => seen.insert((id.clone(), e.model.clone())),
            None => true,
        })
        .cloned()
        .collect()
}

fn aggregate(entries: &[Entry], since: i64) -> Spend {
    let p = pricing();
    let mut acc = Accumulator::default();
    for e in entries.iter().filter(|e| e.ts >= since) {
        let cost = match e.cost {
            Some(c) => c,
            None => match p.resolve(&e.model) {
                Some(r) => r.cost(&e.tokens, true),
                None => {
                    if e.tokens.total() > 0 {
                        acc.unpriced(&e.model);
                    }
                    continue;
                }
            },
        };
        acc.add(e.ts, cost, e.tokens.total());
    }
    acc.build()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real turn's shape (Grok CLI 1.0.46).
    const LINE: &str = r#"{"timestamp":1789761456,"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"turn_completed","usage":{"inputTokens":160054,"outputTokens":2923,"cachedReadTokens":115072,"cacheCreationTokens":0,"reasoningTokens":1741,"costUsdTicks":611129200,"modelUsage":{"grok-4.6-build":{"inputTokens":160054,"outputTokens":2923,"cachedReadTokens":115072,"cacheCreationTokens":0,"reasoningTokens":1741,"costUsdTicks":611129200}}}},"_meta":{"eventId":"e1","agentTimestampMs":1789761456508}}}"#;

    #[test]
    fn reads_a_completed_turn_with_its_cost() {
        let e = parse_line(LINE.as_bytes());
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].ts, 1789761456508);
        assert_eq!(e[0].tokens.input, 160054 - 115072);
        assert_eq!(e[0].tokens.cache_read, 115072);
        assert_eq!(e[0].tokens.total(), 160054 + 2923, "reasoning isn't counted twice");
        assert!((e[0].cost.unwrap() - 0.06111292).abs() < 1e-9);
    }

    #[test]
    fn replayed_events_count_once() {
        let a = parse_line(LINE.as_bytes()).remove(0);
        let b = a.clone();
        let s = aggregate(&dedup(vec![&a, &b]), 0);
        assert_eq!(s.days.len(), 1);
        assert_eq!(s.days[0].tokens, 162977);
        assert!((s.days[0].cost - 0.06).abs() < 1e-9);
    }

    #[test]
    fn ignores_other_updates() {
        assert!(parse_line(br#"{"params":{"update":{"sessionUpdate":"agent_message_chunk"}}}"#).is_empty());
    }
}
