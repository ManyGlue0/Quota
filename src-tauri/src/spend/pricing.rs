//! Model pricing: turns token counts into estimated API-rate dollars.
//!
//! A port of OpenUsage's pricing resolution (MIT), with its bundled data:
//! the OpenUsage supplement (Cursor-native models, fast multipliers, alias
//! rules for log/CSV slugs), a LiteLLM snapshot (primary catalog) and a
//! models.dev snapshot (gap filler). Rates are USD per million tokens.
//!
//! The two catalogs are also downloaded fresh on a schedule (`crate::prices`);
//! `install` swaps them in, and the bundled copies are the offline fallback.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, RwLock};

use regex::Regex;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rates {
    pub input: f64,
    pub output: f64,
    pub cache_write: f64,
    pub cache_read: f64,
    pub input_long: Option<f64>,
    pub output_long: Option<f64>,
    pub cache_write_long: Option<f64>,
    pub cache_read_long: Option<f64>,
    /// The source published a cache-read rate (else it's estimated at 10% of input).
    pub cache_read_explicit: bool,
    pub fast: f64,
    /// Prompt tokens above which the long-context rates apply.
    pub long_above: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tokens {
    pub input: u64,
    pub cache_write_5m: u64,
    pub cache_write_1h: u64,
    pub cache_read: u64,
    pub output: u64,
    pub fast: bool,
}

impl Tokens {
    pub fn prompt(&self) -> u64 {
        self.input + self.cache_write_5m + self.cache_write_1h + self.cache_read
    }
    pub fn total(&self) -> u64 {
        self.prompt() + self.output
    }
}

/// Where long-context rates start unless an entry says otherwise: a catalog
/// entry with `la` (GPT-5.5's is 272k), a supplement entry with
/// `long_context_above_tokens` (Haiku 5.5's is 100k).
const LONG_CONTEXT_TOKENS: u64 = 200_000;

impl Rates {
    /// Dollars for one request. Above `long_above` prompt tokens (cache reads
    /// and writes included) the long-context rates apply to the whole
    /// request; 1-hour cache writes bill at 2x input.
    pub fn cost(&self, t: &Tokens, long_context: bool) -> f64 {
        let long = long_context && t.prompt() > self.long_above;
        let pick = |base: f64, l: Option<f64>| if long { l.unwrap_or(base) } else { base };
        let input = pick(self.input, self.input_long);
        let per = |n: u64, rate: f64| n as f64 * rate / 1_000_000.0;
        let cost = per(t.input, input)
            + per(t.output, pick(self.output, self.output_long))
            + per(t.cache_write_5m, pick(self.cache_write, self.cache_write_long))
            + per(t.cache_write_1h, input * 2.0)
            + per(t.cache_read, pick(self.cache_read, self.cache_read_long));
        cost * if t.fast { self.fast } else { 1.0 }
    }

    fn scaled(&self, f: f64) -> Rates {
        let s = |o: Option<f64>| o.map(|v| v * f);
        Rates {
            input: self.input * f,
            output: self.output * f,
            cache_write: self.cache_write * f,
            cache_read: self.cache_read * f,
            input_long: s(self.input_long),
            output_long: s(self.output_long),
            cache_write_long: s(self.cache_write_long),
            cache_read_long: s(self.cache_read_long),
            cache_read_explicit: self.cache_read_explicit,
            fast: 1.0,
            long_above: self.long_above,
        }
    }
}

pub struct Pricing {
    supplement: HashMap<String, Rates>,
    fast_multipliers: HashMap<String, f64>,
    aliases: Vec<(Regex, String)>,
    primary: HashMap<String, Rates>,
    secondary: HashMap<String, Rates>,
    memo: Mutex<HashMap<String, Option<Rates>>>,
}

pub const BUNDLED_SUPPLEMENT: &str = include_str!("../../resources/pricing/supplement.json");
pub const BUNDLED_LITELLM: &str = include_str!("../../resources/pricing/litellm.json");
pub const BUNDLED_MODELS_DEV: &str = include_str!("../../resources/pricing/models_dev.json");

static CURRENT: RwLock<Option<Arc<Pricing>>> = RwLock::new(None);

/// The prices in use: the downloaded catalogs once `install`ed, else the bundled ones.
pub fn pricing() -> Arc<Pricing> {
    if let Some(p) = CURRENT.read().unwrap().as_ref() {
        return p.clone();
    }
    CURRENT.write().unwrap().get_or_insert_with(|| Arc::new(Pricing::bundled())).clone()
}

/// Puts these prices in use for every cost computed from now on.
pub fn install(p: Pricing) {
    *CURRENT.write().unwrap() = Some(Arc::new(p));
}

/// A catalog's `retrieved_at` (RFC 3339, UTC), to tell which copy is newer.
pub fn retrieved_at(catalog: &str) -> Option<String> {
    let root: Value = serde_json::from_str(catalog).ok()?;
    root["retrieved_at"].as_str().map(str::to_string)
}

fn compact_catalog(text: &str) -> HashMap<String, Rates> {
    let root: Value = serde_json::from_str(text).unwrap_or(Value::Null);
    let mut out = HashMap::new();
    for (key, m) in root["models"].as_object().into_iter().flatten() {
        let f = |k: &str| m.get(k).and_then(Value::as_f64);
        let (Some(i), Some(o)) = (f("i"), f("o")) else { continue };
        out.insert(
            key.clone(),
            Rates {
                input: i,
                output: o,
                cache_write: f("cw").unwrap_or(i),
                cache_read: f("cr").unwrap_or(i * 0.1),
                input_long: f("ia"),
                output_long: f("oa"),
                cache_write_long: f("cwa"),
                cache_read_long: f("cra"),
                cache_read_explicit: m.get("cre").and_then(Value::as_bool).unwrap_or(true),
                fast: f("fast").unwrap_or(1.0),
                long_above: m.get("la").and_then(Value::as_u64).unwrap_or(LONG_CONTEXT_TOKENS),
            },
        );
    }
    out
}

impl Pricing {
    pub fn bundled() -> Self {
        Self::with_catalogs(BUNDLED_LITELLM, BUNDLED_MODELS_DEV)
    }

    /// The bundled supplement over these LiteLLM and models.dev catalogs.
    pub fn with_catalogs(primary: &str, secondary: &str) -> Self {
        Self::load(BUNDLED_SUPPLEMENT, primary, secondary)
    }

    /// How many model names have a price.
    pub fn models(&self) -> usize {
        let mut keys: HashSet<&str> = self.supplement.keys().map(String::as_str).collect();
        keys.extend(self.primary.keys().map(String::as_str));
        keys.extend(self.secondary.keys().map(String::as_str));
        keys.len()
    }

    pub fn load(supplement: &str, primary: &str, secondary: &str) -> Self {
        let sup: Value = serde_json::from_str(supplement).unwrap_or(Value::Null);
        let fast_multipliers: HashMap<String, f64> = sup["fast_multipliers"]
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(k, v)| v.as_f64().map(|f| (k.clone(), f)))
            .collect();
        let mut sup_rates = HashMap::new();
        for (model, e) in sup["pricing"].as_object().into_iter().flatten() {
            let f = |k: &str| e.get(k).and_then(Value::as_f64);
            let (Some(i), Some(o)) = (f("input_per_million"), f("output_per_million")) else { continue };
            sup_rates.insert(
                model.clone(),
                Rates {
                    input: i,
                    output: o,
                    cache_write: f("cache_write_per_million").unwrap_or(i),
                    cache_read: f("cache_read_per_million").unwrap_or(i * 0.1),
                    input_long: f("input_long_per_million"),
                    output_long: f("output_long_per_million"),
                    cache_write_long: f("cache_write_long_per_million"),
                    cache_read_long: f("cache_read_long_per_million"),
                    cache_read_explicit: f("cache_read_per_million").is_some(),
                    fast: fast_multipliers.get(model).copied().unwrap_or(1.0),
                    long_above: e.get("long_context_above_tokens").and_then(Value::as_u64).unwrap_or(LONG_CONTEXT_TOKENS),
                },
            );
        }
        let aliases = sup["alias_rules"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|r| Some((Regex::new(r["pattern"].as_str()?).ok()?, r["canonical"].as_str()?.to_string())))
            .collect();
        let mut primary = compact_catalog(primary);
        let mut secondary = compact_catalog(secondary);
        // Fast mode for catalog models the catalogs give no multiplier for.
        for catalog in [&mut primary, &mut secondary] {
            for (model, m) in &fast_multipliers {
                if let Some(r) = catalog.get_mut(model).filter(|r| r.fast == 1.0) {
                    r.fast = *m;
                }
            }
        }
        Self {
            supplement: sup_rates,
            fast_multipliers,
            aliases,
            primary,
            secondary,
            memo: Mutex::new(HashMap::new()),
        }
    }

    /// The canonical pricing key for a log/CSV slug (alias rules), if one applies.
    pub fn canonical(&self, model: &str) -> Option<&str> {
        self.aliases.iter().find(|(re, _)| re.is_match(model)).map(|(_, c)| c.as_str())
    }

    pub fn resolve(&self, model: &str) -> Option<Rates> {
        if let Some(hit) = self.memo.lock().unwrap().get(model) {
            return *hit;
        }
        let rates = match self.canonical(model) {
            Some(c) if c != model => self.lookup(c).or_else(|| self.lookup(model)),
            _ => self.lookup(model),
        };
        self.memo.lock().unwrap().insert(model.to_string(), rates);
        rates
    }

    pub fn fast_multiplier(&self, model: &str) -> Option<f64> {
        if let Some(m) = self.fast_multipliers.get(model) {
            return Some(*m);
        }
        let normalized = normalized(model);
        for part in normalized.split(['/', ':']) {
            for (base, m) in &self.fast_multipliers {
                let base = normalized_key(base);
                if let Some(pos) = part.rfind(base.as_str()) {
                    let rest = &part[pos + base.len()..];
                    if rest.is_empty() || rest.starts_with('-') {
                        return Some(*m);
                    }
                }
            }
        }
        None
    }

    fn lookup(&self, name: &str) -> Option<Rates> {
        if let Some(r) = self.supplement.get(name) {
            return Some(*r);
        }
        if let Some(r) = self.primary.get(name) {
            return Some(*r);
        }
        if let Some(r) = self.fast_variant(name) {
            return Some(r);
        }
        if name.ends_with("-fast") {
            return self.secondary.get(name).copied();
        }
        if let Some(r) = find_fuzzy(&self.primary, name) {
            return Some(r);
        }
        self.secondary.get(name).copied()
    }

    fn fast_variant(&self, name: &str) -> Option<Rates> {
        let base = name.strip_suffix("-fast").filter(|b| !b.is_empty())?;
        let (key, rates) = if let Some(r) = self.supplement.get(base) {
            (base.to_string(), *r)
        } else if let Some(r) = self.primary.get(base) {
            (base.to_string(), *r)
        } else if let Some((k, r)) = find_fuzzy_keyed(&self.primary, base) {
            (k, r)
        } else {
            (base.to_string(), *self.secondary.get(base)?)
        };
        let mult = if rates.fast != 1.0 {
            rates.fast
        } else {
            self.fast_multiplier(&key).or_else(|| self.fast_multiplier(base))?
        };
        Some(rates.scaled(mult))
    }
}

// ---- fuzzy matching (port of ccusage's pricing.rs, via OpenUsage) -----------------

fn normalized(value: &str) -> String {
    normalized_key(value)
}

fn normalized_key(value: &str) -> String {
    if value.contains('.') || value.contains('@') {
        value.replace(['.', '@'], "-")
    } else {
        value.to_string()
    }
}

fn find_fuzzy(entries: &HashMap<String, Rates>, model: &str) -> Option<Rates> {
    find_fuzzy_keyed(entries, model).map(|(_, r)| r)
}

/// The longest catalog key that matches the model on word boundaries.
fn find_fuzzy_keyed(entries: &HashMap<String, Rates>, model: &str) -> Option<(String, Rates)> {
    let nm = normalized_key(model);
    let mut best: Option<(&String, &Rates)> = None;
    for (key, rates) in entries {
        if !key_matches(key, model, &nm) {
            continue;
        }
        let better = match best {
            None => true,
            Some((k, _)) => key.len() > k.len() || (key.len() == k.len() && key < k),
        };
        if better {
            best = Some((key, rates));
        }
    }
    best.map(|(k, r)| (k.clone(), *r))
}

fn key_matches(candidate: &str, model: &str, normalized_model: &str) -> bool {
    if contains_key(model, candidate) || contains_key(candidate, model) {
        return true;
    }
    let nc = normalized_key(candidate);
    contains_key(normalized_model, &nc) || contains_key(&nc, normalized_model)
}

fn contains_key(value: &str, key: &str) -> bool {
    let (v, k) = (value.as_bytes(), key.as_bytes());
    if k.is_empty() || k.len() > v.len() {
        return false;
    }
    for start in 0..=v.len() - k.len() {
        if &v[start..start + k.len()] != k {
            continue;
        }
        if start > 0 && v[start - 1].is_ascii_alphanumeric() {
            continue;
        }
        let suffix = &v[start + k.len()..];
        if suffix_allows_match(k, suffix) {
            return true;
        }
    }
    false
}

fn suffix_allows_match(key: &[u8], suffix: &[u8]) -> bool {
    match suffix.first() {
        None => true,
        Some(c) if c.is_ascii_alphanumeric() => false,
        Some(_) => !suffix_starts_with_numeric_version(key, suffix),
    }
}

/// `claude-sonnet-4` must not match `claude-sonnet-4-5`, but may match a dated
/// `claude-sonnet-4-20250514`.
fn suffix_starts_with_numeric_version(key: &[u8], suffix: &[u8]) -> bool {
    if !key.last().is_some_and(u8::is_ascii_digit) {
        return false;
    }
    if !matches!(suffix.first(), Some(b'-') | Some(b'.')) {
        return false;
    }
    let rest = &suffix[1..];
    let digits = rest.iter().take_while(|c| c.is_ascii_digit()).count();
    if digits == 0 {
        return false;
    }
    let after = rest.get(digits);
    let is_date = digits == 8 && after.is_none_or(|c| !c.is_ascii_alphanumeric());
    !is_date
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_real_models() {
        let p = pricing();
        let opus = p.resolve("claude-opus-5-5").expect("opus 5.5 priced");
        assert_eq!((opus.input, opus.output), (4.0, 20.0));
        assert_eq!(opus.fast, 2.0);
        // Anthropic's price list, 2026-10-31.
        let sonnet55 = p.resolve("claude-sonnet-5-5").expect("sonnet 5.5 priced");
        assert_eq!((sonnet55.input, sonnet55.cache_write, sonnet55.cache_read, sonnet55.output), (2.0, 2.5, 0.1, 10.0));
        let sonnet5 = p.resolve("claude-sonnet-5").expect("sonnet 5 priced");
        assert_eq!((sonnet5.input, sonnet5.cache_write, sonnet5.cache_read, sonnet5.output), (2.0, 2.5, 0.2, 10.0));
        let haiku = p.resolve("claude-haiku-5-5").expect("haiku 5.5 priced");
        assert_eq!((haiku.input, haiku.cache_write, haiku.cache_read, haiku.output), (0.1, 0.125, 0.01, 0.5));
        assert_eq!(haiku.long_above, 100_000);
        assert!(p.resolve("claude-haiku-5.5-thinking-high").is_some());
        assert!(p.resolve("claude-sonnet-4-5-20250929").is_some());
        assert!(p.resolve("gpt-5.5").is_some());
        assert!(p.resolve("auto").is_some(), "cursor auto via supplement");
        assert!(p.resolve("definitely-not-a-model-xyz").is_none());
    }

    #[test]
    fn version_boundaries() {
        assert!(contains_key("claude-sonnet-4-20250514", "claude-sonnet-4"));
        assert!(!contains_key("claude-sonnet-4-5", "claude-sonnet-4"));
        assert!(!contains_key("xgpt-5", "gpt-5"));
    }

    #[test]
    fn cost_math() {
        let r = Rates {
            input: 3.0,
            output: 15.0,
            cache_write: 3.75,
            cache_read: 0.3,
            input_long: Some(6.0),
            output_long: Some(22.5),
            cache_write_long: None,
            cache_read_long: None,
            cache_read_explicit: true,
            fast: 2.0,
            long_above: LONG_CONTEXT_TOKENS,
        };
        let t = Tokens { input: 1_000_000, output: 1_000_000, ..Default::default() };
        assert!((r.cost(&t, true) - 28.5).abs() < 1e-9, "long context applies above 200k");
        let t = Tokens { input: 100_000, output: 100_000, fast: true, ..Default::default() };
        assert!((r.cost(&t, true) - 3.6).abs() < 1e-9);
    }

    #[test]
    fn fable_5_2_under_every_name() {
        let p = Pricing::bundled();
        for name in ["claude-fable-5-2", "claude-fable-5.2", "claude-fable-5-2[1m]", "claude-fable-5-2-high", "Fable 5.2 (Auto Balanced)"] {
            let r = p.resolve(name).unwrap_or_else(|| panic!("{name} unpriced"));
            assert_eq!((r.input, r.cache_write, r.cache_read, r.output), (8.0, 10.0, 0.2, 40.0), "{name}");
        }
        // Fast mode: 2x ($16 / $80), from a fast request or a "-fast" name.
        let t = Tokens { input: 1_000_000, output: 1_000_000, fast: true, ..Default::default() };
        assert_eq!(p.resolve("claude-fable-5-2").unwrap().cost(&t, true), 96.0);
        let fast = p.resolve("Fable 5.2 Fast (Auto Balanced)").unwrap();
        assert_eq!((fast.input, fast.output), (16.0, 80.0));
    }

    #[test]
    fn haiku_5_5_prices_by_prompt_length() {
        let haiku = pricing().resolve("claude-haiku-5-5").unwrap();
        // 100k prompt tokens exactly: the low rates. $0.01 + $0.05.
        let t = Tokens { input: 100_000, output: 100_000, ..Default::default() };
        assert!((haiku.cost(&t, true) - 0.06).abs() < 1e-9);
        // Over 100k, counting cache reads: every token at the high rates,
        // output included. 1k * $0.50 + 100k * $0.05 + 10k * $2.50, per million.
        let t = Tokens { input: 1_000, cache_read: 100_000, output: 10_000, ..Default::default() };
        assert!((haiku.cost(&t, true) - 0.0305).abs() < 1e-9);
        // 1-hour cache writes: 2x input, $0.20 / $1.
        let t = Tokens { cache_write_1h: 1_000_000, ..Default::default() };
        assert!((haiku.cost(&t, true) - 1.0).abs() < 1e-9);
        let t = Tokens { cache_write_1h: 50_000, ..Default::default() };
        assert!((haiku.cost(&t, true) - 0.01).abs() < 1e-9);
    }
}
