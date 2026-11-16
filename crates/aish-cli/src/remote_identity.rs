//! Remote identity authentication (#586/#587 hardening): a challenge-response
//! protocol that proves the *operator* behind an ssh connection without
//! relying on passwordless ssh alone.
//!
//! Trust model:
//! * Every aish instance owns an identity keypair at
//!   `~/.config/aish/identity-key.json` (0600). This is a *host-operator*
//!   identity, distinct from the admin signing key (which authorizes policy
//!   content).
//! * A target host maintains an allow-list of trusted operator public keys in
//!   `~/.config/aish/trusted-operators.json` (a JSON array of hex public
//!   keys). An empty or absent file means "trust any operator" — the single
//!   -machine default, so single-host usage stays exactly as it was.
//! * The remote agent entries (`--remote-*`) verify a challenge signed with
//!   the caller's identity key before performing the action. The challenge is
//!   bound to the requested action, so a captured response cannot be replayed
//!   for a different operation.
//!
//! Flow (per ssh invocation):
//!   client:                          target agent:
//!     IDENTITY <pubkey-hex>  ------>
//!                              <--   CHALLENGE <nonce-hex>
//!     SIGN <sig-hex>          ------>
//!                              <--   OK   (action proceeds)
//!                                    DENY  (identity not trusted)

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use std::path::PathBuf;

const IDENTITY_FILE: &str = "identity-key.json";
const TRUSTED_FILE: &str = "trusted-operators.json";

fn config_dir() -> Result<PathBuf, String> {
    let base = dirs::config_dir().ok_or("cannot resolve config dir")?;
    let dir = base.join("aish");
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {dir:?}: {e}"))?;
    Ok(dir)
}

fn identity_path() -> Result<PathBuf, String> {
    Ok(config_dir()?.join(IDENTITY_FILE))
}

fn trusted_path() -> Result<PathBuf, String> {
    Ok(config_dir()?.join(TRUSTED_FILE))
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok())
        .collect()
}

/// Load (or lazily create) this host's operator identity keypair. Creation is
/// silent and automatic: every aish instance has an identity from first run.
pub fn ensure_identity() -> Result<(String, SigningKey), String> {
    let path = identity_path()?;
    if path.exists() {
        let text =
            std::fs::read_to_string(&path).map_err(|e| format!("cannot read identity key: {e}"))?;
        let json: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| format!("corrupt identity key: {e}"))?;
        let hex_key = json
            .get("secret_key_hex")
            .and_then(|v| v.as_str())
            .ok_or("identity file missing secret_key_hex")?;
        let bytes = hex_decode(hex_key).ok_or("identity key has invalid hex")?;
        let arr: [u8; 32] = bytes
            .try_into()
            .map_err(|_| "identity secret key must be 32 bytes")?;
        let key = SigningKey::from_bytes(&arr);
        return Ok((hex_encode(key.verifying_key().as_bytes()), key));
    }
    let mut csprng = rand_core::OsRng;
    let key = SigningKey::generate(&mut csprng);
    let json = serde_json::json!({
        "public_key_hex": hex_encode(key.verifying_key().as_bytes()),
        "secret_key_hex": hex_encode(&key.to_bytes()),
    });
    crate::remote_hosts::write_secret_file(
        &path,
        &serde_json::to_string_pretty(&json).unwrap_or_default(),
    )
    .map_err(|e| format!("cannot write identity key: {e}"))?;
    Ok((hex_encode(key.verifying_key().as_bytes()), key))
}

/// Is operator identity enforcement active on THIS host? True when at least
/// one trusted operator public key is configured. Absent file = open mode
/// (single-machine behavior is unchanged). A file that exists but cannot be
/// read or parsed FAILS CLOSED: enforcement is treated as active (and every
/// action denied downstream) rather than silently degrading to open mode.
pub fn enforcement_active() -> bool {
    match trusted_path() {
        Err(_) => false,
        Ok(p) => match std::fs::read_to_string(&p) {
            // Absent file: open mode.
            Err(_) => false,
            Ok(s) => match serde_json::from_str::<Vec<String>>(&s) {
                // Corrupt file: fail closed.
                Err(_) => true,
                Ok(v) => !v.is_empty(),
            },
        },
    }
}

/// Register an operator public key on THIS host (target side). Idempotent.
pub fn trust_operator(public_key_hex: &str) -> Result<(), String> {
    let bytes = hex_decode(public_key_hex).ok_or("public key is not valid hex")?;
    if bytes.len() != 32 {
        return Err("public key must be 32 bytes (64 hex chars)".to_string());
    }
    let path = trusted_path()?;
    let mut trusted: Vec<String> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    if trusted.contains(&public_key_hex.to_string()) {
        return Ok(());
    }
    trusted.push(public_key_hex.to_string());
    write_trusted_file(&path, &trusted)
}

/// Write the trusted-operators file with 0600: it gates who may act on this
/// host, so a world-readable copy would leak the operator identity list.
fn write_trusted_file(path: &std::path::Path, trusted: &[String]) -> Result<(), String> {
    // Atomic 0600 publish via tmp+rename; the file gates who may act on
    // this host, so it must never be world-readable mid-write.
    crate::remote_hosts::write_secret_file(
        path,
        &serde_json::to_string_pretty(trusted).unwrap_or_default(),
    )
}

pub fn list_trusted() -> Result<Vec<String>, String> {
    let path = trusted_path()?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read trusted operators: {e}"))
        .and_then(|s| serde_json::from_str(&s).map_err(|e| format!("corrupt file: {e}")))
}

/// Remove a trusted operator. No-op if absent.
pub fn untrust_operator(public_key_hex: &str) -> Result<bool, String> {
    let path = trusted_path()?;
    if !path.exists() {
        return Ok(false);
    }
    let trusted: Vec<String> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let before = trusted.len();
    let kept: Vec<String> = trusted
        .into_iter()
        .filter(|k| k != public_key_hex)
        .collect();
    let removed = kept.len() < before;
    write_trusted_file(&path, &kept)?;
    Ok(removed)
}

/// Sign an action-bound challenge: the payload is `action|nonce`. Both sides
/// construct it identically, so a signature is only valid for exactly one
/// (action, nonce) pair.
pub fn sign_challenge(key: &SigningKey, action: &str, nonce_hex: &str) -> String {
    let payload = format!("{action}|{nonce_hex}");
    hex_encode(&key.sign(payload.as_bytes()).to_bytes())
}

/// Verify a signed challenge against an operator public key.
pub fn verify_challenge(
    pubkey_hex: &str,
    action: &str,
    nonce_hex: &str,
    sig_hex: &str,
) -> Result<(), String> {
    let bytes = hex_decode(pubkey_hex).ok_or("operator key is not valid hex")?;
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| "operator key must be 32 bytes")?;
    let vk = VerifyingKey::from_bytes(&arr).map_err(|e| format!("bad operator key: {e}"))?;
    let sig_bytes = hex_decode(sig_hex).ok_or("signature is not valid hex")?;
    let sig_bytes: [u8; 64] = sig_bytes
        .try_into()
        .map_err(|_| "signature must be 64 bytes")?;
    let sig = Signature::from_bytes(&sig_bytes);
    let payload = format!("{action}|{nonce_hex}");
    match vk.verify(payload.as_bytes(), &sig) {
        Ok(()) => Ok(()),
        Err(_) => Err("challenge signature does not verify".to_string()),
    }
}

/// Generate a fresh random nonce (32 bytes, hex).
pub fn fresh_nonce() -> String {
    use rand_core::RngCore;
    let mut rng = rand_core::OsRng;
    let mut bytes = [0u8; 32];
    rng.fill_bytes(&mut bytes);
    hex_encode(&bytes)
}

/// Target-side authentication check, shared by all `--remote-*` agent
/// entries. The agent reads one JSON line from the caller containing
/// `{action, pubkey, challenge, signature}` and verifies:
///   1. enforcement is active (else accept: open mode),
///   2. the operator key is trusted,
///   3. the signature verifies over `action|challenge`.
///
/// `read_line` supplies the auth request line; `fail` reports a rejection
/// through whatever channel the entry uses (JSON line on stdout).
pub fn authenticate_agent_action(
    action: &str,
    mut read_line: impl FnMut() -> Option<String>,
    mut fail: impl FnMut(String),
) -> bool {
    if !enforcement_active() {
        return true;
    }
    let Some(line) = read_line() else {
        fail("no identity presented".to_string());
        return false;
    };
    let req: serde_json::Value = match serde_json::from_str(line.trim()) {
        Ok(v) => v,
        Err(e) => {
            fail(format!("malformed identity request: {e}"));
            return false;
        }
    };
    let pubkey = req.get("pubkey").and_then(|v| v.as_str()).unwrap_or("");
    let sig = req.get("signature").and_then(|v| v.as_str()).unwrap_or("");
    let nonce = req.get("challenge").and_then(|v| v.as_str()).unwrap_or("");
    let presented_action = req.get("action").and_then(|v| v.as_str()).unwrap_or("");
    if presented_action != action {
        fail(format!(
            "identity bound to action '{presented_action}', this entry requires '{action}'"
        ));
        return false;
    }
    let trusted = match list_trusted() {
        Ok(t) => t,
        Err(e) => {
            // Fail closed: a corrupt/unreadable trust list must not widen
            // access to open mode.
            fail(format!(
                "trusted-operators file cannot be read: {e}\nTip: fix or remove the file, then re-register operators with 'aish remote admin trust-operator'"
            ));
            return false;
        }
    };
    if !trusted.iter().any(|k| k == pubkey) {
        fail(format!(
            "operator {pubkey} is not trusted on this host.\nTip: run 'aish remote admin trust-operator {pubkey}' on the target host"
        ));
        return false;
    }
    if verify_challenge(pubkey, action, nonce, sig).is_err() {
        fail("challenge signature verification failed".to_string());
        return false;
    }
    true
}

/// Open-mode counterpart of the auth gate: when enforcement is NOT active,
/// the client still sends the identity line (so the wire format is
/// uniform); the agent must consume and discard it so downstream stdin
/// consumers (frame relay, envelope reader) see clean input. In
/// enforcement mode the gate has already consumed the line, so this is a
/// no-op there.
pub fn consume_identity_line_if_present() {
    if enforcement_active() {
        return;
    }
    let mut line = String::new();
    // Best-effort: EOF or parse failure are both fine — the line exists
    // only to keep the two modes wire-compatible.
    let _ = std::io::stdin().read_line(&mut line);
}

/// Client-side helper: produce the identity JSON line for `action`.
pub fn identity_request_json(action: &str) -> Result<String, String> {
    let (pubkey, key) = ensure_identity()?;
    let nonce = fresh_nonce();
    let sig = sign_challenge(&key, action, &nonce);
    Ok(serde_json::json!({
        "action": action,
        "pubkey": pubkey,
        "challenge": nonce,
        "signature": sig,
    })
    .to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_roundtrip_sign_verify() {
        let (_pub, key) = ensure_identity().expect("identity create failed");
        let nonce = fresh_nonce();
        let sig = sign_challenge(&key, "sessions.list", &nonce);
        assert!(verify_challenge(&_pub, "sessions.list", &nonce, &sig).is_ok());
        // Wrong action must fail.
        assert!(verify_challenge(&_pub, "policy.push", &nonce, &sig).is_err());
        // Wrong nonce must fail.
        assert!(verify_challenge(&_pub, "sessions.list", &fresh_nonce(), &sig).is_err());
    }

    #[test]
    fn sign_is_deterministic_per_payload_and_verifies() {
        let (_pub, key) = ensure_identity().expect("identity create failed");
        let nonce = fresh_nonce();
        let s1 = sign_challenge(&key, "policy.push", &nonce);
        let s2 = sign_challenge(&key, "policy.push", &nonce);
        // ed25519 is deterministic: identical payload → identical signature.
        assert_eq!(s1, s2);
        assert!(verify_challenge(&_pub, "policy.push", &nonce, &s1).is_ok());
    }

    #[test]
    fn hex_roundtrip() {
        let bytes: Vec<u8> = (0..=255).collect();
        let enc = hex_encode(&bytes);
        assert_eq!(hex_decode(&enc).unwrap(), bytes);
        assert!(hex_decode("abc").is_none());
        assert!(hex_decode("zz").is_none());
    }
}
