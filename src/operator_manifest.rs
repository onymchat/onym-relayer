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

    // Expiry: clients REJECT expired manifests, so serving one only
    // poisons pinning clients. Refusing to boot puts the check where
    // the bytes are actually served — CI tripwires cover the repo, but
    // the onym-infra droplet path deploys with no CI in front of it.
    let valid_until = object
        .get("validUntil")
        .and_then(Value::as_str)
        .ok_or("manifest must declare validUntil")?;
    let until =
        parse_rfc3339_utc(valid_until).map_err(|e| format!("validUntil {valid_until:?}: {e}"))?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| format!("system clock before epoch: {e}"))?
        .as_secs() as i64;
    if until <= now {
        return Err(format!(
            "manifest EXPIRED: validUntil {valid_until} is in the past — clients \
             reject expired manifests; bump validUntil in operator-manifest.src.json \
             and re-run the sign-manifest workflow"
        ));
    }

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

/// Parse an RFC 3339 timestamp (`2027-08-14T00:00:00Z`, fractional
/// seconds and numeric offsets accepted) to Unix seconds. Hand-rolled
/// on purpose: the expiry comparison is the only date arithmetic in
/// the crate, so a whole datetime dependency is not warranted. Uses
/// the days-from-civil algorithm (Howard Hinnant's formulation).
fn parse_rfc3339_utc(s: &str) -> Result<i64, String> {
    let bytes = s.as_bytes();
    let digits = |range: std::ops::Range<usize>| -> Result<i64, String> {
        let part = bytes
            .get(range.clone())
            .ok_or_else(|| format!("truncated at byte {}", range.start))?;
        if !part.iter().all(u8::is_ascii_digit) {
            return Err(format!("non-digit in {:?}", &s[range]));
        }
        Ok(std::str::from_utf8(part).unwrap().parse().unwrap())
    };
    let sep = |index: usize, expected: &[u8]| -> Result<(), String> {
        match bytes.get(index) {
            Some(b) if expected.contains(&b.to_ascii_uppercase()) => Ok(()),
            other => Err(format!(
                "expected one of {:?} at byte {index}, got {:?}",
                std::str::from_utf8(expected).unwrap(),
                other.map(|&b| b as char),
            )),
        }
    };
    let (year, month, day) = (digits(0..4)?, digits(5..7)?, digits(8..10)?);
    sep(4, b"-")?;
    sep(7, b"-")?;
    sep(10, b"T")?; // to_ascii_uppercase above also admits lowercase t
    let (hour, minute, second) = (digits(11..13)?, digits(14..16)?, digits(17..19)?);
    sep(13, b":")?;
    sep(16, b":")?;
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return Err("date/time component out of range".into());
    }

    // Fractional seconds are ignored (sub-second expiry precision is
    // meaningless here); then Z or a numeric offset, nothing after.
    let mut index = 19;
    if bytes.get(index) == Some(&b'.') {
        index += 1;
        let start = index;
        while bytes.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
        }
        if index == start {
            return Err("empty fractional seconds".into());
        }
    }
    let offset_seconds = match bytes.get(index) {
        Some(b'Z') | Some(b'z') if index + 1 == bytes.len() => 0,
        Some(sign @ (b'+' | b'-')) if index + 6 == bytes.len() => {
            let (oh, om) = (digits(index + 1..index + 3)?, digits(index + 4..index + 6)?);
            sep(index + 3, b":")?;
            if oh > 23 || om > 59 {
                return Err("UTC offset out of range".into());
            }
            (oh * 3600 + om * 60) * if *sign == b'+' { 1 } else { -1 }
        }
        _ => return Err("must end in Z or a +HH:MM/-HH:MM offset".into()),
    };

    // Days since 1970-01-01 for a proleptic-Gregorian civil date.
    let year_shifted = year - i64::from(month <= 2);
    let era = year_shifted.div_euclid(400);
    let year_of_era = year_shifted - era * 400;
    let month_shifted = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * month_shifted + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146097 + day_of_era - 719468;
    Ok(days * 86400 + hour * 3600 + minute * 60 + second - offset_seconds)
}

/// [`load_and_verify`] plus, when a detached signature file is given,
/// the agreement check the discovery spec calls for (§3 of the
/// signing rule): the detached bytes must be the SAME signature the
/// manifest embeds, whitespace aside. A disagreeing pair is refused
/// even if each would verify on its own — tooling that verifies the
/// detached form before parsing must see exactly what parsers see.
pub fn verify_file(
    path: &std::path::Path,
    sig_path: Option<&std::path::Path>,
) -> Result<OperatorManifest, String> {
    let manifest = load_and_verify(path)?;
    if let Some(sig_path) = sig_path {
        let detached = std::fs::read_to_string(sig_path)
            .map_err(|e| format!("read {}: {e}", sig_path.display()))?;
        if detached.trim() != manifest.detached_signature {
            return Err(format!(
                "detached signature at {} does not match the embedded signature",
                sig_path.display()
            ));
        }
    }
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn signed_manifest(seat: &str, tamper: bool) -> Vec<u8> {
        signed_manifest_at(seat, tamper, "2030-01-01T00:00:00Z")
    }

    fn signed_manifest_at(seat: &str, tamper: bool, valid_until: &str) -> Vec<u8> {
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let operator = format!("onym:key:{}", hex::encode(key.verifying_key().to_bytes()));
        let mut doc = serde_json::json!({
            "version": 1,
            "componentId": "onym:component:onym-relayer",
            "seat": seat,
            "operator": operator,
            "powers": { "gateCreation": true, "canAuthorTransitions": false },
            "validUntil": valid_until,
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
    fn agreeing_detached_signature_accepted() {
        let bytes = signed_manifest("notary", false);
        let path = write_temp("detached-ok", &bytes);
        let embedded: Value = serde_json::from_slice(&bytes).unwrap();
        let sig = embedded["signature"].as_str().unwrap();
        // The signer CLI writes the base64 line with a trailing newline.
        let sig_path = write_temp("detached-ok-sig", format!("{sig}\n").as_bytes());
        verify_file(&path, Some(&sig_path)).unwrap();
        std::fs::remove_file(path).ok();
        std::fs::remove_file(sig_path).ok();
    }

    #[test]
    fn disagreeing_detached_signature_refused() {
        let bytes = signed_manifest("notary", false);
        let path = write_temp("detached-bad", &bytes);
        let sig_path = write_temp("detached-bad-sig", b"AAAA not the embedded signature\n");
        let err = verify_file(&path, Some(&sig_path)).unwrap_err();
        assert!(err.contains("does not match the embedded"), "{err}");
        std::fs::remove_file(path).ok();
        std::fs::remove_file(sig_path).ok();
    }

    #[test]
    fn expired_manifest_refused_at_boot() {
        let bytes = signed_manifest_at("notary", false, "2020-01-01T00:00:00Z");
        let path = write_temp("expired", &bytes);
        let err = load_and_verify(&path).unwrap_err();
        assert!(err.contains("EXPIRED"), "{err}");
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn future_valid_until_accepted() {
        // The default helper date (2030) IS the future case, but make
        // the intent explicit and independent of the helper default.
        let bytes = signed_manifest_at("notary", false, "2099-12-31T23:59:59Z");
        let path = write_temp("future", &bytes);
        load_and_verify(&path).unwrap();
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn malformed_valid_until_refused() {
        for bad in [
            "soon",
            "2030-13-01T00:00:00Z",
            "2030-01-01",
            "2030-01-01T00:00:00",
        ] {
            let bytes = signed_manifest_at("notary", false, bad);
            let path = write_temp("malformed", &bytes);
            let err = load_and_verify(&path).unwrap_err();
            assert!(err.contains("validUntil"), "{bad}: {err}");
            std::fs::remove_file(path).ok();
        }
    }

    #[test]
    fn missing_valid_until_refused() {
        // Build a signed manifest with NO validUntil at all: the field
        // is part of the terms clients pin, so its absence is refused
        // rather than treated as "never expires".
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let operator = format!("onym:key:{}", hex::encode(key.verifying_key().to_bytes()));
        let mut doc = serde_json::json!({
            "version": 1,
            "componentId": "onym:component:onym-relayer",
            "seat": "notary",
            "operator": operator,
        });
        let canonical = serde_json::to_vec(&doc).unwrap();
        let sig_b64 =
            base64::engine::general_purpose::STANDARD.encode(key.sign(&canonical).to_bytes());
        doc.as_object_mut()
            .unwrap()
            .insert("signature".into(), Value::String(sig_b64));
        let path = write_temp("no-validuntil", &serde_json::to_vec(&doc).unwrap());
        let err = load_and_verify(&path).unwrap_err();
        assert!(err.contains("validUntil"), "{err}");
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn parse_rfc3339_utc_handles_offsets_and_fractions() {
        assert_eq!(parse_rfc3339_utc("1970-01-01T00:00:00Z").unwrap(), 0);
        assert_eq!(
            parse_rfc3339_utc("2027-08-14T00:00:00Z").unwrap(),
            1818201600
        );
        // Offset and fractional forms of the same instant.
        assert_eq!(
            parse_rfc3339_utc("2027-08-14T02:30:00+02:30").unwrap(),
            1818201600
        );
        assert_eq!(
            parse_rfc3339_utc("2027-08-13T22:00:00-02:00").unwrap(),
            1818201600
        );
        assert_eq!(
            parse_rfc3339_utc("2027-08-14T00:00:00.500Z").unwrap(),
            1818201600
        );
        for bad in [
            "",
            "2027-08-14",
            "2027-08-14T00:00:00",
            "2027-08-14 00:00:00Z",
            "2027-08-14T00:00:00+0200",
            "2027-08-14T00:00:00.Z",
            "2027-08-14T25:00:00Z",
            "not-a-date",
        ] {
            assert!(parse_rfc3339_utc(bad).is_err(), "{bad:?} should not parse");
        }
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
