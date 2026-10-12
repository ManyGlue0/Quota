//! OpenCode spend from its local SQLite logs (every `opencode*.db` in its
//! data folder, read-only): messages through OpenCode's own gateways, Go
//! (`opencode-go`) and Zen (`opencode`). OpenCode records what each message
//! cost, so that's summed as-is; other providers (your own keys) log no cost
//! and belong to those providers.
//!
//! OpenCode 1 writes `message` rows; OpenCode 2 `session_message` rows (and
//! copies old messages over under the same id, so ids count once).

use std::collections::HashSet;

use rusqlite::Connection;

use super::*;
use crate::providers::opencode::{databases, open_read_only};

const PROVIDER_ID: &str = "COALESCE(json_extract(data,'$.model.providerID'), json_extract(data,'$.providerID'))";
const TOTAL_TOKENS: &str = "COALESCE(json_extract(data,'$.tokens.total'), \
     COALESCE(json_extract(data,'$.tokens.input'),0) + COALESCE(json_extract(data,'$.tokens.output'),0) \
     + COALESCE(json_extract(data,'$.tokens.reasoning'),0) + COALESCE(json_extract(data,'$.tokens.cache.read'),0) \
     + COALESCE(json_extract(data,'$.tokens.cache.write'),0))";
const V1_ROLE: &str = "json_extract(data,'$.role') = 'assistant'";
const V2_ROLE: &str = "(type = 'assistant' OR (type = 'compaction' AND json_extract(data,'$.status') = 'completed'))";

fn tables(conn: &Connection) -> Vec<(&'static str, &'static str)> {
    let has = |t: &str| conn.query_row("SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1", [t], |_| Ok(())).is_ok();
    let mut out = Vec::new();
    if has("message") {
        out.push(("message", V1_ROLE));
    }
    if has("session_message") {
        out.push(("session_message", V2_ROLE));
    }
    out
}

fn rows_sql(table: &str, role: &str, cutoff: Option<i64>) -> String {
    let since = cutoff.map(|c| format!("time_created >= {c} AND ")).unwrap_or_default();
    format!(
        "SELECT id, time_created, json_extract(data,'$.cost'), {TOTAL_TOKENS} FROM {table} \
         WHERE {since}json_valid(data) AND {role} AND {PROVIDER_ID} IN ('opencode-go','opencode') \
         AND json_type(data,'$.cost') IN ('integer','real')"
    )
}

/// Totals the last 30 days. Blocking: run off the async runtime.
pub fn scan() -> Option<Spend> {
    let dbs = databases();
    if dbs.is_empty() {
        return None;
    }
    let since = since_ms();
    let mut acc = Accumulator::default();
    let mut seen = HashSet::new();
    for path in dbs {
        let Some(conn) = open_read_only(&path) else { continue };
        for (table, role) in tables(&conn) {
            let Ok(mut stmt) = conn.prepare(&rows_sql(table, role, Some(since))) else { continue };
            let rows = stmt.query_map([], |r| {
                Ok((r.get::<_, Option<String>>(0)?, r.get::<_, f64>(1)?, r.get::<_, f64>(2)?, r.get::<_, Option<f64>>(3)?))
            });
            for (id, ms, cost, tokens) in rows.into_iter().flatten().flatten() {
                if id.as_ref().is_some_and(|id| !seen.insert(id.clone())) || cost < 0.0 {
                    continue;
                }
                acc.add(ms as i64, cost, tokens.unwrap_or(0.0).clamp(0.0, 1e15) as u64);
            }
        }
    }
    Some(acc.build())
}

/// Any message through OpenCode's own gateways, ever (the login probe).
pub fn has_hosted_usage() -> bool {
    databases().iter().any(|path| {
        let Some(conn) = open_read_only(path) else { return false };
        tables(&conn)
            .into_iter()
            .any(|(table, role)| conn.query_row(&format!("{} LIMIT 1", rows_sql(table, role, None)), [], |_| Ok(())).is_ok())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sums_hosted_messages_once() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE message (id TEXT, time_created INTEGER, data TEXT);
             CREATE TABLE session_message (id TEXT, time_created INTEGER, type TEXT, data TEXT);
             INSERT INTO message VALUES ('a', 1790000000000, '{\"role\":\"assistant\",\"providerID\":\"opencode\",\"cost\":0.5,\"tokens\":{\"total\":1000}}');
             INSERT INTO message VALUES ('b', 1790000000000, '{\"role\":\"assistant\",\"providerID\":\"lmstudio\",\"cost\":0,\"tokens\":{\"total\":50}}');
             INSERT INTO session_message VALUES ('a', 1790000000000, 'assistant', '{\"model\":{\"providerID\":\"opencode\"},\"cost\":0.5,\"tokens\":{\"input\":600,\"output\":400}}');
             INSERT INTO session_message VALUES ('c', 1790000000000, 'assistant', '{\"model\":{\"providerID\":\"opencode-go\"},\"cost\":0.25,\"tokens\":{\"input\":100,\"output\":50,\"cache\":{\"read\":50}}}');",
        )
        .unwrap();
        let mut seen = HashSet::new();
        let (mut cost, mut tokens) = (0.0, 0.0);
        for (table, role) in tables(&conn) {
            let mut stmt = conn.prepare(&rows_sql(table, role, Some(0))).unwrap();
            for r in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, f64>(2)?, r.get::<_, f64>(3)?))).unwrap() {
                let (id, c, t) = r.unwrap();
                if seen.insert(id) {
                    cost += c;
                    tokens += t;
                }
            }
        }
        assert_eq!(cost, 0.75, "own-key providers are out; the copied message counts once");
        assert_eq!(tokens, 1200.0);
    }
}
