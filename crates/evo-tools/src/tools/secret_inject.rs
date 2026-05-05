//! Secret and TOTP injection for browser tools.
//!
//! Resolves `${SECRET:name}` and `${TOTP:name}` placeholders from the local
//! Vault. Real credential values never appear in tool observations or model
//! context — the model only ever sees the placeholder tokens.
//!
//! Syntax
//! ------
//!   ${SECRET:name}   vault entry with that name → raw value
//!   ${TOTP:name}     vault entry holds base32 TOTP seed → current 6-digit code
//!
//! Multi-account naming convention
//! --------------------------------
//!   {site}_{account}_{field}
//!   e.g.  google_work_user  /  google_work_pass  /  google_work_totp
//!         google_home_user  /  google_home_pass
//!
//! The agent constructs the full key name from the account_id parameter,
//! so the model never sees the raw value at any point.

use evo_policy::Vault;
use hmac::{Hmac, Mac};
use sha1::Sha1;

type HmacSha1 = Hmac<Sha1>;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Outcome of resolving a text value against the vault.
pub enum Resolved {
    /// No placeholder detected — use the original text as-is.
    Plain,
    /// Placeholder resolved to this value. Never echo in observations.
    Value(String),
    /// Placeholder found but the named key is absent from the vault.
    Missing { key: String },
}

/// Resolve `${SECRET:name}` or `${TOTP:name}` in `text` against `vault`.
///
/// Returns `Plain` immediately when no placeholder is detected so callers
/// skip vault I/O for ordinary (non-credential) inputs.
pub fn resolve(text: &str, vault: &Vault) -> Resolved {
    if let Some(key) = extract_ref(text, "SECRET") {
        match vault.get(key) {
            Some(e) => Resolved::Value(e.value.clone()),
            None => Resolved::Missing { key: key.to_owned() },
        }
    } else if let Some(key) = extract_ref(text, "TOTP") {
        match vault.get(key) {
            Some(e) => match totp_now(&e.value) {
                Ok(code) => Resolved::Value(code),
                Err(err) => Resolved::Missing {
                    key: format!("{key} (TOTP error: {err})"),
                },
            },
            None => Resolved::Missing { key: key.to_owned() },
        }
    } else {
        Resolved::Plain
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Extract the key name from `${PREFIX:key}`.
/// Returns `None` if the text does not match exactly (trailing chars rejected).
fn extract_ref<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let tag = format!("${{{prefix}:");
    text.strip_prefix(tag.as_str())?.strip_suffix('}')
}

/// Compute the current TOTP code from a base32-encoded seed.
/// RFC 6238 defaults: HMAC-SHA1, 6 digits, 30-second step.
fn totp_now(seed_b32: &str) -> Result<String, String> {
    let seed = decode_base32(seed_b32)?;
    let step = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs()
        / 30;
    let mut mac =
        HmacSha1::new_from_slice(&seed).map_err(|e| format!("HMAC key: {e}"))?;
    mac.update(&step.to_be_bytes());
    let digest = mac.finalize().into_bytes();
    // Dynamic truncation per RFC 4226 §5.3
    let offset = (digest[19] & 0x0f) as usize;
    let code = u32::from_be_bytes([
        digest[offset] & 0x7f,
        digest[offset + 1],
        digest[offset + 2],
        digest[offset + 3],
    ]) % 1_000_000;
    Ok(format!("{code:06}"))
}

/// Minimal RFC 4648 base32 decoder. Trailing `=` padding is ignored.
fn decode_base32(s: &str) -> Result<Vec<u8>, String> {
    const ALPHA: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let upper = s.trim_end_matches('=').to_ascii_uppercase();
    let mut bits: u32 = 0;
    let mut bit_count: u32 = 0;
    let mut out = Vec::with_capacity(upper.len() * 5 / 8);
    for ch in upper.chars() {
        let val = ALPHA
            .iter()
            .position(|&b| b == ch as u8)
            .ok_or_else(|| format!("invalid base32 char '{ch}'"))? as u32;
        bits = (bits << 5) | val;
        bit_count += 5;
        if bit_count >= 8 {
            bit_count -= 8;
            out.push((bits >> bit_count) as u8);
            bits &= (1 << bit_count) - 1;
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_secret_ref() {
        assert_eq!(extract_ref("${SECRET:my_key}", "SECRET"), Some("my_key"));
        assert_eq!(extract_ref("${TOTP:seed}", "TOTP"), Some("seed"));
        assert_eq!(extract_ref("plain text", "SECRET"), None);
        assert_eq!(extract_ref("${SECRET:key}extra", "SECRET"), None);
    }

    #[test]
    fn base32_rfc_vector() {
        // RFC 4648 §10: "foobar" encodes as "MZXW6YTBOI"
        assert_eq!(decode_base32("MZXW6YTBOI").unwrap(), b"foobar");
    }

    #[test]
    fn totp_produces_6_digits() {
        let code = totp_now("JBSWY3DPEHPK3PXP").unwrap();
        assert_eq!(code.len(), 6);
        assert!(code.chars().all(|c| c.is_ascii_digit()));
    }

    #[test]
    fn resolve_plain_text() {
        let vault = Vault::default();
        assert!(matches!(resolve("hello world", &vault), Resolved::Plain));
    }

    #[test]
    fn resolve_missing_key() {
        let vault = Vault::default();
        assert!(matches!(
            resolve("${SECRET:no_such_key}", &vault),
            Resolved::Missing { .. }
        ));
    }

    #[test]
    fn resolve_present_secret() {
        let mut vault = Vault::default();
        vault.upsert("site_pass", "hunter2");
        assert!(matches!(
            resolve("${SECRET:site_pass}", &vault),
            Resolved::Value(_)
        ));
    }
}
