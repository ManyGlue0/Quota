//! Cursor spend from its usage export (a CSV of usage events, downloaded with
//! the Cursor login). Rows are priced from their token counts, like OpenUsage
//! does: the export's own Cost column says what was billed, which is $0 for
//! usage included in a plan.

use std::collections::HashMap;

use super::pricing::{pricing, Tokens};
use super::*;

const REQUIRED: [&str; 6] = ["Date", "Model", "Input (w/ Cache Write)", "Input (w/o Cache Write)", "Cache Read", "Output Tokens"];

pub async fn fetch() -> Option<Spend> {
    let end = crate::providers::now_ms();
    let csv = crate::providers::cursor::export_csv(since_ms(), end).await.ok()?;
    parse(&csv)
}

/// Splits CSV text into records, honoring quotes (and quoted newlines).
fn records(text: &str) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    let (mut row, mut field, mut quoted) = (Vec::new(), String::new(), false);
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                field.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            ',' if !quoted => row.push(std::mem::take(&mut field)),
            '\n' if !quoted => {
                row.push(std::mem::take(&mut field));
                out.push(std::mem::take(&mut row));
            }
            '\r' if !quoted => {}
            _ => field.push(c),
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        out.push(row);
    }
    out
}

fn int(raw: &str) -> Option<u64> {
    let s = raw.trim().replace(',', "");
    if s.is_empty() {
        return Some(0);
    }
    s.parse().ok()
}

fn date(raw: &str) -> Option<i64> {
    let raw = raw.trim();
    parse_ts(raw).or_else(|| {
        let naive = chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S").ok()?;
        chrono::Local.from_local_datetime(&naive).earliest().map(|d| d.timestamp_millis())
    })
}

pub fn parse(text: &str) -> Option<Spend> {
    let mut rows = records(text).into_iter();
    let header = rows.next()?;
    let col: HashMap<&str, usize> = header.iter().enumerate().map(|(i, h)| (h.trim(), i)).collect();
    if REQUIRED.iter().any(|c| !col.contains_key(c)) {
        return None;
    }
    fn get<'a>(col: &HashMap<&str, usize>, r: &'a [String], c: &str) -> &'a str {
        r.get(col[c]).map(String::as_str).unwrap_or("")
    }
    let get = |r: &[String], c: &str| -> String { get(&col, r, c).to_string() };
    let p = pricing();
    let mut acc = Accumulator::default();
    for r in rows {
        let Some(ts) = date(&get(&r, "Date")) else { continue };
        let model = get(&r, "Model");
        let model = model.trim();
        if model.is_empty() {
            continue;
        }
        let (Some(cw), Some(input), Some(cr), Some(output)) = (
            int(&get(&r, "Input (w/ Cache Write)")),
            int(&get(&r, "Input (w/o Cache Write)")),
            int(&get(&r, "Cache Read")),
            int(&get(&r, "Output Tokens")),
        ) else {
            continue;
        };
        let tokens = Tokens { input, cache_write_5m: cw, cache_read: cr, output, ..Default::default() };
        match p.resolve(model) {
            // One row folds many requests together: no long-context guess.
            Some(rates) => acc.add(ts, rates.cost(&tokens, false), tokens.total()),
            None if tokens.total() > 0 => acc.unpriced(model),
            None => {}
        }
    }
    Some(acc.build())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_an_export() {
        let csv = "Date,Cloud Agent ID,Automation ID,Kind,Model,Max Mode,Input (w/ Cache Write),Input (w/o Cache Write),Cache Read,Output Tokens,Total Tokens,Cost\n\
            \"2026-10-09T10:00:00.000Z\",,,Included,auto,No,\"1,000\",2000,30000,500,33500,Included\n\
            2026-10-09T11:00:00Z,,,Included,mystery-model-9,No,0,10,0,10,20,$0.01\n";
        let s = parse(csv).unwrap();
        assert_eq!(s.days.len(), 1);
        assert_eq!(s.days[0].tokens, 33_500);
        assert!(s.days[0].cost > 0.0);
        assert_eq!(s.unpriced, ["mystery-model-9"]);
    }

    #[test]
    fn rejects_unknown_layouts() {
        assert!(parse("Date,Model\n2026-10-09,auto\n").is_none());
        assert_eq!(parse(&format!("{}\n", REQUIRED.join(","))).unwrap().days.len(), 0);
    }
}
