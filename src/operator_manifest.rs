//! The relayer's signed notary-operator manifest.
//!
//! The relayer is a declared seat operator (see
//! `onym-system/notary/UI-Notary-BNB.md` §8.1): it publishes a signed
//! manifest describing its component id, operator key, powers, and
//! offers, and serves it **byte-for-byte** — group bindings and
//! entitlements pin the SHA-256 of exactly these bytes, so
//! re-serializing on the way out would break consent, exactly as with
//! the moderation authority's manifest.
//!
//! The manifest is signed offline (with the `onym-discovery` CLI or
//! any Ed25519 tool that follows the canonical-bytes rule below); the
//! relayer only verifies at boot and refuses to start on a manifest
//! whose signature does not verify against its own `operator` key.
//! Signing bytes are the moderation seat's mechanism: decode, remove
//! the top-level `signature` field structurally, re-serialize with
//! sorted keys (serde_json's default `BTreeMap` map ordering).

use base64::Engine as _;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde_json::Value;

/// Verified manifest state held for the lifetime of the process.
#[derive(Debug)]
pub struct OperatorManifest {
    /// The exact bytes served at `GET /manifest.json`.
    pub raw: Vec<u8>,
    /// The embedded signature, re-served at `GET /manifest.json.sig`
    /// as a detached form for tooling that verifies before parsing.
    pub detached_signature: String,
    /// `onym:key:<hex>` — logged at boot so the fingerprint can be
    /// checked against what Discovery catalogs pin.
    pub operator: String,
}

pub fn load_and_verify(path: &std::path::Path) -> Result<OperatorManifest, String> {
    let raw = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let value: Value =
        serde_json::from_slice(&raw).map_err(|e| format!("manifest is not valid JSON: {e}"))?;
    let object = value.as_object().ok_or("manifest must be a JSON object")?;

    let seat = object
        .get("seat")
        .and_then(Value::as_str)
        .ok_or("manifest must declare a seat")?;
    if seat != "notary" {
        return Err(format!("manifest seat must be \"notary\", got {seat:?}"));
    }
    let component_id = object
        .get("componentId")
        .and_then(Value::as_str)
        .ok_or("manifest must declare componentId")?;
    if !component_id.starts_with("onym:component:") {
        return Err(format!(
            "componentId must start with onym:component: — got {component_id}"
        ));
    }
    let operator = object
        .get("operator")
        .and_then(Value::as_str)
        .ok_or("manifest must declare operator")?
        .to_string();
    let key_hex = operator
        .strip_prefix("onym:key:")
        .ok_or("operator must start with onym:key:")?;
    let key_bytes: [u8; 32] = hex::decode(key_hex)
        .map_err(|e| format!("operator key hex: {e}"))?
        .try_into()
        .map_err(|_| "operator key must be 32 bytes".to_string())?;
    let verifying_key =
        VerifyingKey::from_bytes(&key_bytes).map_err(|e| format!("operator key: {e}"))?;

    let signature_b64 = object
        .get("signature")
        .and_then(Value::as_str)
        .ok_or("manifest is unsigned")?
        .to_string();
    let signature_raw = base64::engine::general_purpose::STANDARD
        .decode(&signature_b64)
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(&signature_b64))
        .map_err(|e| format!("signature base64: {e}"))?;
    let signature_bytes: [u8; 64] = signature_raw
        .try_into()
        .map_err(|_| "signature must be 64 bytes".to_string())?;

    // Canonical signing bytes: structural removal, sorted keys.
    let mut unsigned = value.clone();
    unsigned
        .as_object_mut()
        .expect("checked object above")
        .remove("signature");
    let canonical = serde_json::to_vec(&unsigned).map_err(|e| format!("canonicalize: {e}"))?;
    verifying_key
        .verify(&canonical, &Signature::from_bytes(&signature_bytes))
        .map_err(|_| "manifest signature does not verify against its operator key".to_string())?;

    Ok(OperatorManifest {
        raw,
        detached_signature: signature_b64,
        operator,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn signed_manifest(seat: &str, tamper: bool) -> Vec<u8> {
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let operator = format!("onym:key:{}", hex::encode(key.verifying_key().to_bytes()));
        let mut doc = serde_json::json!({
            "version": 1,
            "componentId": "onym:component:onym-relayer",
            "seat": seat,
            "operator": operator,
            "powers": { "gateCreation": true, "canAuthorTransitions": false },
            "validUntil": "2030-01-01T00:00:00Z",
        });
        let canonical = serde_json::to_vec(&doc).unwrap();
        let signature = key.sign(&canonical);
        let mut sig_b64 = base64::engine::general_purpose::STANDARD.encode(signature.to_bytes());
        if tamper {
            // Flip the first character to break the signature.
            let replacement = if sig_b64.starts_with('A') { "B" } else { "A" };
            sig_b64.replace_range(0..1, replacement);
        }
        doc.as_object_mut()
            .unwrap()
            .insert("signature".into(), Value::String(sig_b64));
        serde_json::to_vec(&doc).unwrap()
    }

    fn write_temp(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "operator-manifest-test-{}-{name}.json",
            std::process::id()
        ));
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn valid_manifest_loads_and_serves_exact_bytes() {
        let bytes = signed_manifest("notary", false);
        let path = write_temp("valid", &bytes);
        let manifest = load_and_verify(&path).unwrap();
        assert_eq!(
            manifest.raw, bytes,
            "served bytes must be exactly the file bytes"
        );
        assert!(manifest.operator.starts_with("onym:key:"));
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn tampered_signature_refused() {
        let bytes = signed_manifest("notary", true);
        let path = write_temp("tampered", &bytes);
        let err = load_and_verify(&path).unwrap_err();
        assert!(err.contains("does not verify"), "{err}");
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn wrong_seat_refused() {
        let bytes = signed_manifest("moderation", false);
        let path = write_temp("wrongseat", &bytes);
        let err = load_and_verify(&path).unwrap_err();
        assert!(err.contains("seat"), "{err}");
        std::fs::remove_file(path).ok();
    }
}
