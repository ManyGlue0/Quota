//! Codex spend from the Codex CLI's session rollouts
//! (`~/.codex/sessions/**/*.jsonl` and `archived_sessions`, or `$CODEX_HOME`).
//!
//! Each turn logs a `token_count` event; the model comes from the latest
//! `turn_context`. Subagent and forked sessions first replay their parent's
//! history, which must not be counted twice, so a child rollout's events only
//! count once its own first task starts (OpenUsage's replay gate).

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::Value;

use super::pricing::{pricing, Tokens};
use super::*;

#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    ts: i64,
    model: Arc<str>,
    pricing_model: Option<&'static str>,
    input: u64,
    cached: u64,
    output: u64,
    total: u64,
    fast: bool,
    ultrafast: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Raw {
    input: u64,
    cached: u64,
    output: u64,
    reasoning: u64,
    total: u64,
}

impl Raw {
    fn from(v: &Value) -> Raw {
        let n = |keys: &[&str]| keys.iter().find_map(|k| v[*k].as_u64()).unwrap_or(0);
        let input = n(&["input_tokens", "prompt_tokens", "input"]);
        let output = n(&["output_tokens", "completion_tokens", "output"]);
        let reasoning = n(&["reasoning_output_tokens", "reasoning_tokens"]);
        let reported = n(&["total_tokens"]);
        let recomputed = input + output + reasoning;
        Raw {
            input,
            cached: n(&["cached_input_tokens", "cache_read_input_tokens", "cached_tokens"]),
            output,
            reasoning,
            total: if reported > 0 || recomputed == 0 { reported } else { recomputed },
        }
    }
    fn minus(&self, prev: Option<Raw>) -> Raw {
        let p = prev.unwrap_or_default();
        Raw {
            input: self.input.saturating_sub(p.input),
            cached: self.cached.saturating_sub(p.cached),
            output: self.output.saturating_sub(p.output),
            reasoning: self.reasoning.saturating_sub(p.reasoning),
            total: self.total.saturating_sub(p.total),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Gate {
    UntilStartedAt(f64),
    UntilSelfTimedTask,
}

/// Parser state carried from line to line (and across incremental reads).
#[derive(Debug, Clone, Default)]
pub struct State {
    previous: Option<Raw>,
    model: Option<Arc<str>>,
    fast: bool,
    ultrafast: bool,
    saw_meta: bool,
    gate: Option<Gate>,
}

fn homes() -> Vec<PathBuf> {
    if let Some(raw) = std::env::var_os("CODEX_HOME").map(|v| v.to_string_lossy().to_string()) {
        if !raw.trim().is_empty() {
            return raw.split(',').map(|p| PathBuf::from(p.trim())).filter(|p| !p.as_os_str().is_empty()).collect();
        }
    }
    crate::providers::home().map(|h| vec![h.join(".codex")]).unwrap_or_default()
}

fn cache() -> &'static Mutex<ScanCache<Event, State>> {
    static C: OnceLock<Mutex<ScanCache<Event, State>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(ScanCache::new()))
}

/// Scans (incrementally) and totals the last 30 days. Blocking: run off the async runtime.
pub fn scan() -> Option<Spend> {
    let since = since_ms();
    let mut files = Vec::new();
    for home in homes() {
        for dir in ["sessions", "archived_sessions"] {
            jsonl_files(&home.join(dir), since, &mut files);
        }
    }
    files.sort();
    files.dedup();
    let mut cache = cache().lock().unwrap();
    cache.update(&files, parse_line);
    Some(aggregate(cache.items(), since))
}

fn model_name(v: &Value) -> Option<Arc<str>> {
    [&v["model"], &v["model_name"], &v["metadata"]["model"]]
        .into_iter()
        .find_map(|m| m.as_str().map(str::trim).filter(|s| !s.is_empty()))
        .map(Arc::from)
}

fn non_null(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::String(s) => !s.trim().is_empty(),
        _ => true,
    }
}

fn is_child(meta: &Value) -> bool {
    non_null(&meta["forked_from_id"])
        || non_null(&meta["parent_thread_id"])
        || meta["thread_source"].as_str() == Some("subagent")
        || non_null(&meta["source"]["subagent"])
}

/// Auto-review turns are logged under a placeholder; price them as the
/// model that served them on that date.
fn auto_review_model(ts: &str) -> &'static str {
    const BY_RELEASE: [(&str, &str); 8] = [
        ("2026-07-09", "gpt-5.6-luna"),
        ("2026-04-23", "gpt-5.5"),
        ("2026-03-05", "gpt-5.4"),
        ("2026-02-05", "gpt-5.3-codex"),
        ("2025-12-11", "gpt-5.2-codex"),
        ("2025-11-13", "gpt-5.1-codex"),
        ("2025-09-15", "gpt-5-codex"),
        ("2025-08-07", "gpt-5"),
    ];
    let date = ts.get(..10).unwrap_or("");
    BY_RELEASE.iter().find(|(d, _)| date >= *d).map(|(_, m)| *m).unwrap_or("gpt-5")
}

pub fn parse_line(line: &[u8], st: &mut State, out: &mut Vec<Event>) {
    let interesting = contains(line, br#""type":"token_count""#)
        || contains(line, br#""type":"turn_context""#)
        || (!st.saw_meta && contains(line, br#""type":"session_meta""#))
        || (st.gate.is_some() && contains(line, br#""type":"task_started""#))
        || contains(line, br#""type":"thread_settings_applied""#);
    if !interesting {
        return;
    }
    let Ok(o) = serde_json::from_slice::<Value>(line) else { return };
    let ty = o["type"].as_str();
    let payload = &o["payload"];
    let ts_raw = o["timestamp"].as_str().map(str::trim);

    if ty == Some("turn_context") {
        if let Some(m) = model_name(payload) {
            st.model = Some(m);
        }
        return;
    }
    if ty == Some("session_meta") && !st.saw_meta {
        st.saw_meta = true;
        if is_child(payload) {
            st.gate = Some(match ts_raw.and_then(parse_ts) {
                Some(ms) => Gate::UntilStartedAt((ms / 1000) as f64),
                None => Gate::UntilSelfTimedTask,
            });
        }
        return;
    }
    if ty != Some("event_msg") {
        return;
    }
    match payload["type"].as_str() {
        Some("thread_settings_applied") => {
            let tier = [&payload["thread_settings"]["service_tier"], &payload["service_tier"]]
                .into_iter()
                .find_map(|v| v.as_str().map(str::trim).filter(|s| !s.is_empty()));
            if let Some(t) = tier {
                st.ultrafast = t == "ultrafast";
                st.fast = t == "fast" || t == "priority";
            }
        }
        Some("task_started") => {
            if let (Some(gate), Some(started)) = (st.gate, payload["started_at"].as_f64()) {
                let cleared = match gate {
                    Gate::UntilStartedAt(g) => started >= g,
                    Gate::UntilSelfTimedTask => ts_raw.and_then(parse_ts).is_some_and(|ms| started >= (ms / 1000) as f64),
                };
                if cleared {
                    st.gate = None;
                }
            }
        }
        Some("token_count") => {
            let (Some(ts_raw), Some(ts)) = (ts_raw, ts_raw.and_then(parse_ts)) else { return };
            let info = &payload["info"];
            let totals = info.get("total_token_usage").filter(|v| v.is_object()).map(Raw::from);
            if st.gate.is_some() {
                // A child replaying its parent: seed the totals, charge nothing.
                if totals.is_some() {
                    st.previous = totals;
                }
                return;
            }
            if totals.is_some() && totals == st.previous {
                return; // a repeated snapshot, not a new turn
            }
            let usage = match info.get("last_token_usage").filter(|v| v.is_object()) {
                Some(last) => Raw::from(last),
                None => match totals {
                    Some(t) => t.minus(st.previous),
                    None => return,
                },
            };
            if totals.is_some() {
                st.previous = totals;
            }
            if usage.input == 0 && usage.cached == 0 && usage.output == 0 && usage.reasoning == 0 {
                return;
            }
            if let Some(m) = model_name(payload).or_else(|| model_name(info)) {
                st.model = Some(m);
            }
            let model = st.model.get_or_insert_with(|| Arc::from("gpt-5")).clone();
            let pricing_model = match &*model {
                "codex-auto-review" => Some(auto_review_model(ts_raw)),
                "gpt-reserve" => Some("gpt-5.6-luna"),
                _ => None,
            };
            out.push(Event {
                ts,
                model,
                pricing_model,
                input: usage.input,
                cached: usage.cached.min(usage.input),
                output: usage.output,
                total: usage.total,
                fast: st.fast,
                ultrafast: st.ultrafast,
            });
        }
        _ => {}
    }
}

/// Codex's priority-tier multiplier for a model.
fn priority_multiplier(model: &str) -> Option<f64> {
    match model {
        "gpt-5.5" | "gpt-5.5-pro" => Some(2.5),
        "gpt-5.4" | "gpt-5.4-pro" | "gpt-5.6-sol" | "gpt-5.6-terra" | "gpt-5.6-luna" | "gpt-6-astra" | "gpt-6.1-sol"
        | "gpt-6-sol" | "gpt-6-luna" => Some(2.0),
        _ => pricing().fast_multiplier(model),
    }
}

fn cost(e: &Event) -> Option<f64> {
    let p = pricing();
    let slug = e.pricing_model.unwrap_or(&e.model);
    let canonical = p.canonical(slug).unwrap_or(slug);
    let fast_alias = canonical.ends_with("-fast");
    let rate_model = canonical.strip_suffix("-fast").unwrap_or(canonical);
    let base = p.resolve(rate_model);
    let mut rates = base.or_else(|| p.resolve(slug))?;
    // No published cache discount: cached input at the full input rate.
    if !rates.cache_read_explicit {
        rates.cache_read = rates.input;
        rates.cache_read_long = rates.input_long;
    }
    let fast = if fast_alias { base.is_some() } else { e.fast };
    rates.fast = if e.ultrafast && rate_model == "gpt-6-astra" {
        6.0
    } else {
        priority_multiplier(rate_model).unwrap_or(if rates.fast > 1.0 { rates.fast } else { 1.0 })
    };
    let tokens = Tokens {
        input: e.input - e.cached,
        cache_read: e.cached,
        output: e.output,
        fast: fast || e.ultrafast,
        ..Default::default()
    };
    Some(rates.cost(&tokens, false))
}

fn aggregate(events: Vec<&Event>, since: i64) -> Spend {
    let mut seen = std::collections::HashSet::new();
    let mut acc = Accumulator::default();
    for e in events.into_iter().filter(|e| e.ts >= since) {
        if !seen.insert((e.ts, e.model.clone(), e.input, e.cached, e.output, e.total)) {
            continue; // the same turn copied into another rollout
        }
        match cost(e) {
            Some(c) => acc.add(e.ts, c, e.total),
            None => {
                if e.total > 0 {
                    acc.unpriced(&e.model);
                }
            }
        }
    }
    acc.build()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(lines: &[&str]) -> Vec<Event> {
        let mut st = State::default();
        let mut out = Vec::new();
        for l in lines {
            parse_line(l.as_bytes(), &mut st, &mut out);
        }
        out
    }

    #[test]
    fn counts_turns_with_their_model() {
        let ev = run(&[
            r#"{"timestamp":"2026-10-01T10:00:00Z","type":"session_meta","payload":{"id":"a"}}"#,
            r#"{"timestamp":"2026-10-01T10:00:01Z","type":"turn_context","payload":{"model":"gpt-5.5"}}"#,
            r#"{"timestamp":"2026-10-01T10:00:02Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":1000,"cached_input_tokens":400,"output_tokens":100,"total_tokens":1100},"last_token_usage":{"input_tokens":1000,"cached_input_tokens":400,"output_tokens":100,"total_tokens":1100}}}}"#,
            r#"{"timestamp":"2026-10-01T10:00:03Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":1000,"cached_input_tokens":400,"output_tokens":100,"total_tokens":1100}}}}"#,
        ]);
        assert_eq!(ev.len(), 1, "a repeated snapshot isn't a new turn");
        assert_eq!(&*ev[0].model, "gpt-5.5");
        assert_eq!((ev[0].input, ev[0].cached, ev[0].total), (1000, 400, 1100));
        assert!(cost(&ev[0]).unwrap() > 0.0);
    }

    #[test]
    fn child_sessions_skip_the_replayed_parent() {
        let ev = run(&[
            r#"{"timestamp":"2026-10-01T10:00:00Z","type":"session_meta","payload":{"forked_from_id":"p"}}"#,
            r#"{"timestamp":"2026-10-01T09:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":5000,"output_tokens":500,"total_tokens":5500}}}}"#,
            r#"{"timestamp":"2026-10-01T10:00:05Z","type":"event_msg","payload":{"type":"task_started","started_at":1790848805}}"#,
            r#"{"timestamp":"2026-10-01T10:00:06Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":6000,"output_tokens":600,"total_tokens":6600}}}}"#,
        ]);
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].total, 1100, "only the child's own turn, as a delta");
    }

    #[test]
    fn auto_review_dates() {
        assert_eq!(auto_review_model("2026-08-01T00:00:00Z"), "gpt-5.6-luna");
        assert_eq!(auto_review_model("2025-10-01T00:00:00Z"), "gpt-5-codex");
    }
}
