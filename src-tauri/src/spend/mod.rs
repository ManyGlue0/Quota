//! Local spend: what your usage would cost at API rates, per day, for the last
//! 30 days. Claude and Codex are read from their own session logs on this
//! machine; Cursor from its usage export. Token counts are measured; dollars
//! are estimates (see `pricing`). Nothing here leaves the machine except the
//! Cursor export request, which goes to Cursor.
//!
//! The logs are large (gigabytes), so scanning is incremental: each file is
//! read once, then only the bytes appended since (logs are append-only).

pub mod claude;
pub mod codex;
pub mod cursor;
pub mod grok;
pub mod opencode;
pub mod pricing;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{Local, TimeZone};
use serde::{Deserialize, Serialize};

/// Spend from logs on this machine, for providers that keep them. Blocking:
/// run off the async runtime. `None`: no local logs for this provider.
pub fn scan_local(id: &str) -> Option<Spend> {
    match id {
        "claude" => claude::scan(),
        "codex" => codex::scan(),
        "grok" => grok::scan(),
        "opencode" => opencode::scan(),
        _ => None,
    }
}

/// Spend that has to be downloaded (Cursor's usage export).
pub async fn fetch_remote(id: &str) -> Option<Spend> {
    match id {
        "cursor" => cursor::fetch().await,
        _ => None,
    }
}

/// Providers whose spend comes from the network, skipped at launch.
pub fn is_remote(id: &str) -> bool {
    id == "cursor"
}

/// Days of history kept (today included).
pub const DAYS: i64 = 30;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DaySpend {
    /// Local calendar day, "YYYY-MM-DD".
    pub date: String,
    pub cost: f64,
    pub tokens: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Spend {
    /// Days with usage, oldest first.
    pub days: Vec<DaySpend>,
    /// Models no price list knows; their usage is left out of the totals.
    pub unpriced: Vec<String>,
}

/// Local midnight at the start of the 30-day window, as epoch ms.
pub fn since_ms() -> i64 {
    let start = Local::now().date_naive() - chrono::Duration::days(DAYS - 1);
    Local
        .from_local_datetime(&start.and_hms_opt(0, 0, 0).unwrap())
        .earliest()
        .map(|d| d.timestamp_millis())
        .unwrap_or(0)
}

pub fn day_key(ms: i64) -> String {
    Local.timestamp_millis_opt(ms).single().map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_default()
}

/// RFC 3339 timestamp → epoch ms.
pub fn parse_ts(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s.trim()).ok().map(|d| d.timestamp_millis())
}

#[derive(Default)]
pub struct Accumulator {
    days: BTreeMap<String, (f64, u64)>,
    unpriced: BTreeSet<String>,
}

impl Accumulator {
    pub fn add(&mut self, ts: i64, cost: f64, tokens: u64) {
        let e = self.days.entry(day_key(ts)).or_default();
        e.0 += cost;
        e.1 += tokens;
    }
    pub fn unpriced(&mut self, model: &str) {
        self.unpriced.insert(model.to_string());
    }
    pub fn build(self) -> Spend {
        Spend {
            days: self
                .days
                .into_iter()
                .map(|(date, (cost, tokens))| DaySpend { date, cost: (cost * 100.0).round() / 100.0, tokens })
                .collect(),
            unpriced: self.unpriced.into_iter().collect(),
        }
    }
}

// ---- incremental JSONL reading -------------------------------------------------------

/// Every `*.jsonl` under `root` modified since `since_ms`.
pub fn jsonl_files(root: &Path, since_ms: i64, out: &mut Vec<PathBuf>) {
    let Ok(dir) = std::fs::read_dir(root) else { return };
    for entry in dir.flatten() {
        let path = entry.path();
        let Ok(meta) = std::fs::metadata(&path) else { continue };
        if meta.is_dir() {
            jsonl_files(&path, since_ms, out);
        } else if path.extension().is_some_and(|e| e == "jsonl") {
            let modified = meta.modified().ok().and_then(|m| m.duration_since(SystemTime::UNIX_EPOCH).ok());
            if modified.is_some_and(|m| m.as_millis() as i64 >= since_ms) {
                out.push(path);
            }
        }
    }
}

/// What a file contributed so far, and how far it was read.
pub struct FileScan<E, S> {
    size: u64,
    mtime: Option<SystemTime>,
    offset: u64,
    pub items: Vec<E>,
    state: S,
}

/// Per-file results, kept across refreshes.
pub struct ScanCache<E, S> {
    files: HashMap<PathBuf, FileScan<E, S>>,
}

impl<E, S: Default + Clone> ScanCache<E, S> {
    pub fn new() -> Self {
        Self { files: HashMap::new() }
    }

    /// Brings every file up to date, parsing only what's new, and forgets
    /// files that are gone (or fell out of the window).
    pub fn update(&mut self, paths: &[PathBuf], mut parse_line: impl FnMut(&[u8], &mut S, &mut Vec<E>)) {
        self.files.retain(|p, _| paths.contains(p));
        for path in paths {
            let Ok(meta) = std::fs::metadata(path) else { continue };
            let (size, mtime) = (meta.len(), meta.modified().ok());
            let entry = self.files.entry(path.clone()).or_insert_with(|| FileScan {
                size: 0,
                mtime: None,
                offset: 0,
                items: Vec::new(),
                state: S::default(),
            });
            if entry.size == size && entry.mtime == mtime {
                continue;
            }
            if size < entry.offset {
                // Rewritten, not appended: start over.
                *entry = FileScan { size: 0, mtime: None, offset: 0, items: Vec::new(), state: S::default() };
            }
            let Ok(mut file) = std::fs::File::open(path) else { continue };
            if file.seek(SeekFrom::Start(entry.offset)).is_err() {
                continue;
            }
            let mut buf = Vec::with_capacity((size - entry.offset) as usize);
            if file.read_to_end(&mut buf).is_err() {
                continue;
            }
            // Only whole lines; a line still being written is read next time.
            let Some(last_nl) = buf.iter().rposition(|&b| b == b'\n') else {
                entry.size = size;
                entry.mtime = mtime;
                continue;
            };
            let mut state = entry.state.clone();
            for line in buf[..last_nl].split(|&b| b == b'\n') {
                let line = line.strip_suffix(b"\r").unwrap_or(line);
                if !line.is_empty() {
                    parse_line(line, &mut state, &mut entry.items);
                }
            }
            entry.state = state;
            entry.offset += last_nl as u64 + 1;
            entry.size = size;
            entry.mtime = mtime;
        }
    }

    /// All items, files in path order (so de-duplication is deterministic).
    pub fn items(&self) -> Vec<&E> {
        let mut paths: Vec<_> = self.files.keys().collect();
        paths.sort();
        paths.iter().flat_map(|p| self.files[*p].items.iter()).collect()
    }
}

pub fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incremental_reads_only_appended_lines() {
        let dir = std::env::temp_dir().join(format!("quota-scan-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.jsonl");
        std::fs::write(&file, "one\ntwo\npart").unwrap();
        let mut cache: ScanCache<String, usize> = ScanCache::new();
        let parse = |l: &[u8], seen: &mut usize, out: &mut Vec<String>| {
            *seen += 1;
            out.push(format!("{}:{}", seen, String::from_utf8_lossy(l)));
        };
        cache.update(&[file.clone()], parse);
        assert_eq!(cache.items(), [&"1:one".to_string(), &"2:two".to_string()]);
        std::fs::write(&file, "one\ntwo\npartial\nthree\n").unwrap();
        cache.update(&[file.clone()], parse);
        let got: Vec<_> = cache.items().into_iter().cloned().collect();
        assert_eq!(got, ["1:one", "2:two", "3:partial", "4:three"]);
        std::fs::remove_dir_all(&dir).ok();
    }
}

#[cfg(test)]
mod real {
    /// `cargo test real_logs -- --ignored --nocapture`: scans this machine's logs.
    #[test]
    #[ignore]
    fn real_logs() {
        for name in ["claude", "codex", "grok", "opencode"] {
            let scan = || super::scan_local(name);
            let t = std::time::Instant::now();
            let s = scan().unwrap();
            let cold = t.elapsed();
            let t = std::time::Instant::now();
            let again = scan().unwrap();
            let warm = t.elapsed();
            assert_eq!(s, again);
            let total: f64 = s.days.iter().map(|d| d.cost).sum();
            let tokens: u64 = s.days.iter().map(|d| d.tokens).sum();
            println!("{name}: {} days, ${total:.2}, {tokens} tokens; cold {cold:?}, warm {warm:?}; unpriced {:?}", s.days.len(), s.unpriced);
            for d in s.days.iter().rev().take(3) {
                println!("   {} ${:.2} {} tokens", d.date, d.cost, d.tokens);
            }
        }
    }
}
