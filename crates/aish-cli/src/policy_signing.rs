//! Admin keypair generation and policy envelope signing for #587.
//!
//! Trust model: the administrator holds an ed25519 keypair. Target hosts
//! configure the admin's public key (hex) in
//! `~/.config/aish/policy-signers.json`; an envelope whose signature does
//! not verify against ANY configured signer is rejected, replacing the
//! "non-empty signature = reject" placeholder.
//!
//! Key storage: `~/.config/aish/admin-keypair.json` (0600) — created by
//! `aish remote admin keygen`.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use std::path::PathBuf;

const KEYPAIR_FILE: &str = "admin-keypair.json";
const SIGNERS_FILE: &str = "policy-signers.json";

fn config_dir() -> Result<PathBuf, String> {
    let base = dirs::config_dir().ok_or("cannot resolve config dir")?;
    let dir = base.join("aish");
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {dir:?}: {e}"))?;
    Ok(dir)
}

fn keypair_path() -> Result<PathBuf, String> {
    Ok(config_dir()?.join(KEYPAIR_FILE))
}

fn signers_path() -> Result<PathBuf, String> {
    Ok(config_dir()?.join(SIGNERS_FILE))
}

/// Generate a new admin keypair and store it (0600). Returns the public key
/// hex for distribution to target hosts.
pub fn keygen() -> Result<String, String> {
    let mut csprng = rand_core::OsRng;
    let signing = SigningKey::generate(&mut csprng);
    let path = keypair_path()?;
    if path.exists() {
        return Err(format!(
            "admin keypair already exists at {}.\\nTip: move it aside before generating a new one (re-signing with a new key requires updating known signers on every host)",
            path.display()
        ));
    }
    let json = serde_json::json!({
        "public_key_hex": hex_encode(signing.verifying_key().as_bytes()),
        "secret_key_hex": hex_encode(&signing.to_bytes()),
    });
    // Atomic 0600 publish: the private key is never readable under the
    // ambient umask (fs::write alone would create it 0644 first).
    crate::remote_hosts::write_secret_file(
        &path,
        &serde_json::to_string_pretty(&json).unwrap_or_default(),
    )
    .map_err(|e| format!("cannot write keypair: {e}"))?;
    Ok(hex_encode(signing.verifying_key().as_bytes()))
}

fn load_signing_key() -> Result<SigningKey, String> {
    let path = keypair_path()?;
    let text = std::fs::read_to_string(&path).map_err(|_| {
        format!(
            "no admin keypair at {}.\\nTip: generate one with 'aish remote admin keygen'",
            path.display()
        )
    })?;
    let json: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("corrupt keypair file: {e}"))?;
    let hex_key = json
        .get("secret_key_hex")
        .and_then(|v| v.as_str())
        .ok_or("keypair file missing secret_key_hex")?;
    let bytes = hex_decode(hex_key).ok_or("keypair file has invalid hex")?;
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| "secret key must be 32 bytes")?;
    SigningKey::from_bytes(&arr).pipe(Ok)
}

trait Pipe: Sized {
    fn pipe<T>(self, f: impl FnOnce(Self) -> T) -> T {
        f(self)
    }
}
impl<T> Pipe for T {}

/// Sign the canonical form of a policy envelope. The signed payload is the
/// concatenation `version|targets|policy-body` — stable and order-independent.
pub fn sign_envelope(version: u64, targets: &[String], policy: &str) -> Result<String, String> {
    let key = load_signing_key()?;
    let payload = canonical_payload(version, targets, policy);
    let sig = key.sign(payload.as_bytes());
    Ok(hex_encode(&sig.to_bytes()))
}

/// Verify an envelope signature against the locally configured signers
/// (used on target hosts).
pub fn verify_envelope(
    version: u64,
    targets: &[String],
    policy: &str,
    signature_hex: &str,
) -> Result<(), String> {
    let path = signers_path()?;
    let text = std::fs::read_to_string(&path)
        .map_err(|_| format!("no policy signers configured at {}.\\nTip: write the admin public key hex into {SIGNERS_FILE} as a JSON array of hex strings", path.display()))?;
    let signers: Vec<String> =
        serde_json::from_str(&text).map_err(|e| format!("corrupt signers file: {e}"))?;
    if signers.is_empty() {
        return Err("no policy signers configured".to_string());
    }
    let payload = canonical_payload(version, targets, policy);
    let sig_bytes = hex_decode(signature_hex).ok_or("signature is not valid hex")?;
    let sig_bytes: [u8; 64] = sig_bytes
        .try_into()
        .map_err(|_| "signature must be 64 bytes")?;
    let signature = Signature::from_bytes(&sig_bytes);
    for hex_key in &signers {
        if let Some(bytes) = hex_decode(hex_key) {
            if let Ok(arr) = <[u8; 32]>::try_from(bytes) {
                if let Ok(vk) = VerifyingKey::from_bytes(&arr) {
                    if vk.verify(&payload.as_bytes(), &signature).is_ok() {
                        return Ok(());
                    }
                }
            }
        }
    }
    Err("signature does not match any configured policy signer".to_string())
}

fn canonical_payload(version: u64, targets: &[String], policy: &str) -> String {
    format!("{version}|{}|{}", targets.join(","), policy)
}

/// Register the admin public key on THIS host (target side).
pub fn add_signer(public_key_hex: &str) -> Result<(), String> {
    let bytes = hex_decode(public_key_hex).ok_or("public key is not valid hex")?;
    if bytes.len() != 32 {
        return Err("public key must be 32 bytes (64 hex chars)".to_string());
    }
    let path = signers_path()?;
    let mut signers: Vec<String> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    if signers.contains(&public_key_hex.to_string()) {
        return Ok(()); // idempotent
    }
    signers.push(public_key_hex.to_string());
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&signers).unwrap_or_default(),
    )
    .map_err(|e| format!("cannot write signers: {e}"))
}

/// Read the admin public key without regenerating.
pub fn keygen_public_key() -> Result<String, String> {
    let path = keypair_path()?;
    let text = std::fs::read_to_string(&path).map_err(|_| {
        format!(
            "no admin keypair at {}.\nTip: generate one with 'aish remote admin keygen'",
            path.display()
        )
    })?;
    let json: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("corrupt keypair file: {e}"))?;
    json.get("public_key_hex")
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| "keypair file missing public_key_hex".to_string())
}

pub fn list_signers() -> Result<Vec<String>, String> {
    let path = signers_path()?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read signers: {e}"))
        .and_then(|s| serde_json::from_str(&s).map_err(|e| format!("corrupt signers file: {e}")))
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}
