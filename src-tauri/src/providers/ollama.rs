//! Ollama Cloud: the plan's monthly credits (or, on older plans, session and
//! weekly limits) from ollama.com's documented `/api/balance`.
//!
//! There's no token to read: Ollama creates a signing key at
//! `~/.ollama/id_ed25519` and `ollama signin` links it to an account.
//! Requests are signed with it exactly as the Ollama CLI signs them; the key
//! itself never leaves the machine, only the signature does. Because the key
//! exists whether or not you use Ollama Cloud, Ollama never turns itself on.

use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use serde_json::Value;

use super::*;

const HOST: &str = "https://ollama.com";
const NO_KEY: &str = "Install Ollama and run ollama signin";
const NOT_SIGNED_IN: &str = "Run ollama signin to see your Ollama Cloud usage";

struct Key {
    /// Base64 of the SSH wire-format public key (the body of `id_ed25519.pub`).
    public_b64: String,
    signing: SigningKey,
}

fn key_path() -> Option<std::path::PathBuf> {
    Some(home()?.join(".ollama").join("id_ed25519"))
}

pub fn has_login() -> bool {
    key_path().is_some_and(|p| p.is_file())
}

fn load_key() -> Result<Key, FetchError> {
    let path = key_path().ok_or_else(|| FetchError::SignedOut(NO_KEY.into()))?;
    let pem = std::fs::read_to_string(path).map_err(|_| FetchError::SignedOut(NO_KEY.into()))?;
    parse_openssh_ed25519(&pem).ok_or_else(|| FetchError::failed("Ollama's key file isn't one Quota can use"))
}

/// The unencrypted OpenSSH container Ollama writes (`PROTOCOL.key`): one
/// ed25519 key, no passphrase. Every read is bounds-checked.
fn parse_openssh_ed25519(pem: &str) -> Option<Key> {
    let body: String = pem.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with("-----")).collect();
    let blob = base64::engine::general_purpose::STANDARD.decode(body).ok()?;
    let magic = b"openssh-key-v1\0";
    let mut r = Reader { b: blob.strip_prefix(magic.as_slice())? };
    if r.string()? != b"none" || r.string()? != b"none" {
        return None; // encrypted: needs a passphrase Ollama never sets
    }
    r.string()?; // kdf options
    if r.u32()? != 1 {
        return None;
    }
    let public_blob = r.string()?;
    let mut p = Reader { b: r.string()? };
    p.u32()?;
    p.u32()?;
    if p.string()? != b"ssh-ed25519" {
        return None;
    }
    p.string()?; // public key, repeated
    let private = p.string()?;
    if private.len() != 64 {
        return None;
    }
    let signing = SigningKey::from_bytes(private[..32].try_into().ok()?);
    // A file whose halves disagree would sign requests ollama.com rejects.
    let mut pb = Reader { b: public_blob };
    if pb.string()? != b"ssh-ed25519" || pb.string()? != signing.verifying_key().as_bytes() || &private[32..] != signing.verifying_key().as_bytes() {
        return None;
    }
    Some(Key { public_b64: base64::engine::general_purpose::STANDARD.encode(public_blob), signing })
}

struct Reader<'a> {
    b: &'a [u8],
}

impl<'a> Reader<'a> {
    fn u32(&mut self) -> Option<u32> {
        let (head, rest) = self.b.split_at_checked(4)?;
        self.b = rest;
        Some(u32::from_be_bytes(head.try_into().ok()?))
    }
    fn string(&mut self) -> Option<&'a [u8]> {
        let n = self.u32()? as usize;
        let (head, rest) = self.b.split_at_checked(n)?;
        self.b = rest;
        Some(head)
    }
}

/// `"<base64 public key>:<base64 signature of METHOD,URI>"`, the Ollama CLI's scheme.
fn authorization(key: &Key, method: &str, uri: &str) -> String {
    let sig = key.signing.sign(format!("{method},{uri}").as_bytes());
    format!("{}:{}", key.public_b64, base64::engine::general_purpose::STANDARD.encode(sig.to_bytes()))
}

async fn send(key: &Key, method: reqwest::Method, path: &str) -> Result<reqwest::Response, FetchError> {
    // The signed URI carries a timestamp, so a captured header can't be replayed later.
    let uri = format!("{path}?ts={}", now_ms() / 1000);
    client()
        .request(method.clone(), format!("{HOST}{uri}"))
        .header("Authorization", authorization(key, method.as_str(), &uri))
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|_| FetchError::failed("Couldn't reach Ollama"))
}

pub async fn fetch() -> FetchResult {
    let key = load_key()?;
    let resp = send(&key, reqwest::Method::GET, "/api/balance").await?;
    let status = resp.status().as_u16();
    if status == 429 {
        return Err(FetchError::RateLimited { retry_after_secs: retry_after(&resp) });
    }
    if matches!(status, 401 | 403) {
        return Err(FetchError::SignedOut(NOT_SIGNED_IN.into()));
    }
    if !resp.status().is_success() {
        return Err(FetchError::Failed(format!("Ollama answered {status}")));
    }
    let body: Value = resp.json().await.map_err(|_| FetchError::failed("Ollama sent an unreadable answer"))?;
    let metrics = map_balance(&body)?;
    // The plan name is a nicety.
    let plan = match send(&key, reqwest::Method::POST, "/api/me").await {
        Ok(r) if r.status().is_success() => r.json::<Value>().await.ok().and_then(|b| {
            b.get("Plan").or(b.get("plan")).and_then(Value::as_str).map(title_case).filter(|p| !p.is_empty())
        }),
        _ => None,
    };
    Ok(Usage { plan, metrics })
}

/// Current plans: `included.{balance_usd, allowance_usd, period}`. Older
/// plans: `included.{session, weekly}.{remaining_percent, resets_at}`.
/// Ollama reports what's left; meters show what's used.
pub fn map_balance(body: &Value) -> Result<Vec<Metric>, FetchError> {
    let included = &body["included"];
    let invalid = || FetchError::failed("Ollama's usage answer changed");
    if !included.is_object() {
        return Err(invalid());
    }
    let mut out = Vec::new();
    for (key, id, label, period) in [("session", "session", "Session", FIVE_HOURS_MS), ("weekly", "weekly", "Weekly", WEEK_MS)] {
        let w = &included[key];
        if let Some(remaining) = num(w.get("remaining_percent")) {
            let resets = epoch_ms(w.get("resets_at"));
            out.push(Metric::percent(id, label, 100.0 - remaining, resets, resets.map(|_| period)));
        }
    }
    if let (Some(allowance), Some(balance)) = (num(included.get("allowance_usd")).filter(|a| *a > 0.0), num(included.get("balance_usd"))) {
        let from = epoch_ms(included["period"].get("from"));
        let until = epoch_ms(included["period"].get("until"));
        let period = match (from, until) {
            (Some(f), Some(u)) if u > f => Some(u - f),
            _ => None,
        };
        let mut m = Metric::money("monthly", "Monthly", (allowance - balance).clamp(0.0, allowance), Some(allowance), "USD").with_reset(until, period);
        m.primary = true;
        out.push(m);
    }
    if out.is_empty() {
        return Err(invalid());
    }
    if let Some(left) = num(body["purchased"].get("balance_usd")) {
        out.push(Metric::money("purchased", "Purchased credits", left.max(0.0), None, "USD"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn monthly_credits() {
        let body = json!({ "included": { "balance_usd": 72.5, "allowance_usd": 100,
            "period": { "from": "2026-09-15T09:30:00Z", "until": "2026-10-15T09:30:00Z" } },
            "purchased": { "balance_usd": 25 } });
        let m = map_balance(&body).unwrap();
        assert_eq!((m[0].id.as_str(), m[0].used, m[0].limit), ("monthly", 27.5, Some(100.0)));
        assert!((m[0].fill().unwrap() - 27.5).abs() < 1e-9);
        assert_eq!(m[0].period_ms, Some(30 * 24 * 3600 * 1000));
        assert_eq!((m[1].id.as_str(), m[1].used), ("purchased", 25.0));
    }

    #[test]
    fn legacy_limits() {
        let body = json!({ "included": {
            "session": { "remaining_percent": 75, "resets_at": "2026-10-01T07:00:00Z" },
            "weekly": { "remaining_percent": 40 } } });
        let m = map_balance(&body).unwrap();
        assert_eq!((m[0].used, m[1].used), (25.0, 60.0));
        assert_eq!(m[0].period_ms, Some(FIVE_HOURS_MS));
        assert_eq!(m[1].period_ms, None, "no reset, no period");
        assert!(map_balance(&json!({ "included": {} })).is_err());
    }

    #[test]
    fn reads_and_signs_with_an_openssh_key() {
        // A throwaway key in Ollama's format (ssh-keygen -t ed25519 -N "").
        let signing = SigningKey::from_bytes(&[7u8; 32]);
        let public = signing.verifying_key().to_bytes();
        let wire = |parts: &[&[u8]]| -> Vec<u8> {
            parts.iter().flat_map(|p| (p.len() as u32).to_be_bytes().into_iter().chain(p.iter().copied())).collect()
        };
        let public_blob = wire(&[b"ssh-ed25519", &public]);
        let private: Vec<u8> = [7u8; 32].iter().chain(public.iter()).copied().collect();
        let mut section = vec![0, 0, 0, 1, 0, 0, 0, 1];
        section.extend(wire(&[b"ssh-ed25519", &public, &private, b"comment"]));
        let mut blob = b"openssh-key-v1\0".to_vec();
        blob.extend(wire(&[b"none", b"none", b""]));
        blob.extend(1u32.to_be_bytes());
        blob.extend(wire(&[&public_blob, &section]));
        let pem = format!(
            "-----BEGIN OPENSSH PRIVATE KEY-----\n{}\n-----END OPENSSH PRIVATE KEY-----\n",
            base64::engine::general_purpose::STANDARD.encode(&blob)
        );
        let key = parse_openssh_ed25519(&pem).expect("parses");
        let header = authorization(&key, "GET", "/api/balance?ts=1");
        let (pk, sig) = header.split_once(':').unwrap();
        assert_eq!(pk, base64::engine::general_purpose::STANDARD.encode(&public_blob));
        let sig = ed25519_dalek::Signature::from_slice(&base64::engine::general_purpose::STANDARD.decode(sig).unwrap()).unwrap();
        assert!(signing.verifying_key().verify_strict(b"GET,/api/balance?ts=1", &sig).is_ok());
        assert!(parse_openssh_ed25519("not a key").is_none());
    }
}
