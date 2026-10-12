//! Claude spend from Claude Code's session logs
//! (`~/.claude/projects/**/*.jsonl`, or `$CLAUDE_CONFIG_DIR`).
//!
//! Each assistant message records its token usage. Streaming writes the same
//! message more than once, and subagent (sidechain) logs repeat their parent's
//! messages, so entries are de-duplicated by message and request id, keeping
//! the main-chain copy with the most tokens (OpenUsage's rules). A cost the
//! log records itself is used as-is; otherwise it's estimated from tokens.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::Value;

use super::pricing::{pricing, Tokens};
use super::*;

#[derive(Debug, Clone)]
pub struct Entry {
    ts: i64,
    tokens: Tokens,
    message_id: Option<Arc<str>>,
    request_id: Option<Arc<str>>,
    sidechain: bool,
    has_speed: bool,
    cost: Option<f64>,
    model: Option<Arc<str>>,
}

fn roots() -> Vec<PathBuf> {
    if let Some(raw) = std::env::var_os("CLAUDE_CONFIG_DIR").map(|v| v.to_string_lossy().to_string()) {
        if !raw.trim().is_empty() {
            return raw
                .split(',')
                .map(|p| {
                    let p = PathBuf::from(p.trim());
                    if p.file_name().is_some_and(|n| n == "projects") {
                        p.parent().map(PathBuf::from).unwrap_or(p)
                    } else {
                        p
                    }
                })
                .collect();
        }
    }
    let Some(home) = crate::providers::home() else { return Vec::new() };
    vec![home.join(".config").join("claude"), home.join(".claude")]
}

fn cache() -> &'static Mutex<ScanCache<Entry, ()>> {
    static C: OnceLock<Mutex<ScanCache<Entry, ()>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(ScanCache::new()))
}

/// Scans (incrementally) and totals the last 30 days. Blocking: run off the async runtime.
pub fn scan() -> Option<Spend> {
    let since = since_ms();
    let mut files = Vec::new();
    for root in roots() {
        jsonl_files(&root.join("projects"), since, &mut files);
    }
    files.sort();
    files.dedup();
    let mut cache = cache().lock().unwrap();
    cache.update(&files, |line, _, out| out.extend(parse_line(line)));
    let entries = dedup(cache.items());
    Some(aggregate(&entries, since))
}

pub fn parse_line(line: &[u8]) -> Vec<Entry> {
    if !contains(line, br#""usage":{"#) {
        return Vec::new();
    }
    let Ok(o) = serde_json::from_slice::<Value>(line) else { return Vec::new() };
    let Some(ts) = o["timestamp"].as_str().and_then(parse_ts) else { return Vec::new() };
    let message = &o["message"];
    let usage = &message["usage"];
    if !usage.is_object() || has_unsupported_null(&o) || !is_valid(&o) {
        return Vec::new();
    }
    let Some((tokens, has_speed)) = breakdown(usage) else { return Vec::new() };
    let arc = |v: &Value| v.as_str().map(Arc::<str>::from);
    let model = message["model"].as_str().filter(|m| *m != "<synthetic>").map(Arc::<str>::from);
    let parent = Entry {
        ts,
        tokens,
        message_id: arc(&message["id"]),
        request_id: arc(&o["requestId"]),
        sidechain: o["isSidechain"].as_bool().unwrap_or(false),
        has_speed,
        cost: o["costUSD"].as_f64(),
        model,
    };
    let mut out = vec![parent.clone()];
    // Advisor work recorded inside a message, priced under the advisor's own model.
    let mut advisor = 0;
    for it in usage["iterations"].as_array().into_iter().flatten() {
        if it["type"].as_str() != Some("advisor_message") {
            continue;
        }
        let Some(model) = it["model"].as_str().filter(|m| !m.is_empty()) else { continue };
        let Some((tokens, has_speed)) = breakdown(it) else { continue };
        out.push(Entry {
            tokens,
            has_speed,
            cost: None,
            model: Some(model.into()),
            message_id: parent.message_id.as_ref().map(|id| format!("{id}:advisor:{advisor}").into()),
            ..parent.clone()
        });
        advisor += 1;
    }
    out
}

fn breakdown(usage: &Value) -> Option<(Tokens, bool)> {
    let input = usage["input_tokens"].as_u64()?;
    let output = usage["output_tokens"].as_u64()?;
    let speed = usage["speed"].as_str();
    if speed.is_some_and(|s| s != "fast" && s != "standard") {
        return None;
    }
    let n = |v: &Value| v.as_u64().unwrap_or(0);
    let (cw5, cw1) = match usage.get("cache_creation").filter(|c| c.is_object()) {
        Some(cc) => (n(&cc["ephemeral_5m_input_tokens"]), n(&cc["ephemeral_1h_input_tokens"])),
        None => (n(&usage["cache_creation_input_tokens"]), 0),
    };
    Some((
        Tokens {
            input,
            cache_write_5m: cw5,
            cache_write_1h: cw1,
            cache_read: n(&usage["cache_read_input_tokens"]),
            output,
            fast: speed == Some("fast"),
        },
        speed.is_some(),
    ))
}

fn is_valid(o: &Value) -> bool {
    if let Some(v) = o["version"].as_str() {
        let mut parts = v.split('.');
        let ok = (0..3).all(|i| {
            parts.next().is_some_and(|p| {
                let digits = p.bytes().take_while(u8::is_ascii_digit).count();
                digits > 0 && (i == 2 || digits == p.len())
            })
        });
        if !ok {
            return false;
        }
    }
    let m = &o["message"];
    [&o["sessionId"], &o["requestId"], &m["id"], &m["model"]].iter().all(|v| v.as_str() != Some(""))
}

fn has_unsupported_null(o: &Value) -> bool {
    let m = &o["message"];
    let u = &m["usage"];
    ["cwd", "costUSD", "version", "sessionId", "requestId", "isApiErrorMessage"].iter().any(|k| o.get(*k).is_some_and(Value::is_null))
        || ["id", "model"].iter().any(|k| m.get(*k).is_some_and(Value::is_null))
        || ["speed", "cache_read_input_tokens", "cache_creation_input_tokens"].iter().any(|k| u.get(*k).is_some_and(Value::is_null))
}

fn should_replace(candidate: &Entry, existing: &Entry) -> bool {
    if candidate.sidechain != existing.sidechain {
        return existing.sidechain;
    }
    let (c, e) = (candidate.tokens.total(), existing.tokens.total());
    if c != e {
        return c > e;
    }
    candidate.has_speed && !existing.has_speed
}

pub fn dedup(entries: Vec<&Entry>) -> Vec<Entry> {
    use std::collections::HashMap;
    let mut out: Vec<Entry> = Vec::with_capacity(entries.len());
    let mut exact: HashMap<(Arc<str>, Option<Arc<str>>), usize> = HashMap::new();
    let mut by_message: HashMap<Arc<str>, Vec<usize>> = HashMap::new();
    for e in entries {
        let Some(mid) = e.message_id.clone() else {
            out.push(e.clone());
            continue;
        };
        let key = (mid.clone(), e.request_id.clone());
        let collision = exact.get(&key).copied().or_else(|| {
            by_message.get(&mid).and_then(|ix| ix.iter().copied().find(|&i| e.sidechain || out[i].sidechain))
        });
        if let Some(i) = collision {
            if should_replace(e, &out[i]) {
                if let Some(old) = out[i].message_id.clone() {
                    exact.remove(&(old, out[i].request_id.clone()));
                }
                out[i] = e.clone();
                exact.insert(key, i);
            }
            continue;
        }
        exact.insert(key, out.len());
        by_message.entry(mid).or_default().push(out.len());
        out.push(e.clone());
    }
    out
}

fn aggregate(entries: &[Entry], since: i64) -> Spend {
    let p = pricing();
    let mut acc = Accumulator::default();
    for e in entries.iter().filter(|e| e.ts >= since) {
        let model = e.model.as_deref().map(str::trim).filter(|m| !m.is_empty());
        let cost = match (e.cost, model) {
            (Some(c), _) => c,
            (None, Some(m)) => match p.resolve(m) {
                Some(r) => r.cost(&e.tokens, true),
                None => {
                    if e.tokens.total() > 0 {
                        acc.unpriced(m);
                    }
                    continue;
                }
            },
            (None, None) => continue,
        };
        acc.add(e.ts, cost, e.tokens.total());
    }
    acc.build()
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINE: &str = r#"{"type":"assistant","timestamp":"2026-10-09T12:00:00Z","sessionId":"s","requestId":"r1","version":"2.1.280","message":{"id":"m1","model":"claude-opus-5-5","usage":{"input_tokens":1000,"output_tokens":2000,"cache_read_input_tokens":10000,"cache_creation":{"ephemeral_5m_input_tokens":500,"ephemeral_1h_input_tokens":0},"speed":"standard"}}}"#;

    #[test]
    fn parses_and_prices_a_message() {
        let e = parse_line(LINE.as_bytes());
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].tokens.total(), 13_500);
        let s = aggregate(&e, 0);
        let rates = pricing().resolve("claude-opus-5-5").unwrap();
        let exact = (1000.0 * rates.input + 2000.0 * rates.output + 10000.0 * rates.cache_read + 500.0 * rates.cache_write) / 1e6;
        assert!((s.days[0].cost - (exact * 100.0).round() / 100.0).abs() < 1e-9, "{} vs {exact}", s.days[0].cost);
        assert_eq!(s.days[0].tokens, 13_500);
    }

    #[test]
    fn dedups_streamed_and_sidechain_copies() {
        let a = parse_line(LINE.as_bytes()).remove(0);
        let mut b = a.clone();
        b.tokens.output = 2500; // a later, fuller copy of the same message
        let mut side = a.clone();
        side.sidechain = true;
        side.request_id = Some("other".into());
        let out = dedup(vec![&a, &b, &side]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].tokens.output, 2500);
        assert!(!out[0].sidechain);
    }

    #[test]
    fn skips_unknown_shapes() {
        assert!(parse_line(br#"{"type":"user","message":{"content":"hi"}}"#).is_empty());
        let weird = LINE.replace(r#""speed":"standard""#, r#""speed":"warp""#);
        assert!(parse_line(weird.as_bytes()).is_empty());
    }
}
