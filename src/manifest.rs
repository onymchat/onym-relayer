//! Cumulative `contracts-manifest.json` fetcher.
//!
//! The relayer's allowlist is "every contract ever deployed via an
//! `onymchat/onym-contracts` GitHub Release," not just the latest tag.
//! That historical union is exactly what `contracts-manifest.json`
//! already carries — `scripts/generate-contracts-manifest.py` walks every
//! release via the GitHub API and emits:
//!
//! ```json
//! {
//!   "version": 1,
//!   "releases": [
//!     {"release": "v0.2.0", "publishedAt": "...", "contracts": [
//!         {"network": "testnet", "type": "anarchy", "id": "C..."},
//!         ...
//!     ]},
//!     {"release": "v0.1.0", ...}
//!   ]
//! }
//! ```
//!
//! Flattening `releases[].contracts[]` and deduping is therefore exactly
//! the same allowlist the build-time `generate-contract-allowlist.py`
//! script used to produce — minus the env-var bake step.
//!
//! On every refresh:
//! 1. HTTPS GET the manifest URL (default: latest release asset).
//! 2. Parse + flatten + validate (must contain at least one contract ID).
//! 3. Atomically swap the live allowlist via `Config::replace_allowlist`.
//! 4. Persist the raw bytes to a disk cache so a GitHub-outage restart
//!    can still bring up a working relayer.

use std::path::Path;
use std::time::Duration;

use serde::Deserialize;
use tokio::fs;

use crate::config::{insert_contract_id, ContractAllowlist, ContractType, Network};

/// Soft cap on a single fetch. The manifest is small (KBs); a multi-MB
/// response means something is wrong (wrong URL, HTML 404 page, etc.) and
/// we'd rather bail fast than spend memory parsing it.
const MAX_MANIFEST_BYTES: u64 = 1 * 1024 * 1024;

/// Per-fetch timeout. Any sane CDN serves this in well under a second; if
/// it's slower than this, we'd rather fail and try again next tick.
const FETCH_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Deserialize)]
struct Manifest {
    #[allow(dead_code)]
    version: u32,
    releases: Vec<ManifestRelease>,
}

#[derive(Debug, Deserialize)]
struct ManifestRelease {
    #[allow(dead_code)]
    release: String,
    #[serde(rename = "publishedAt", default)]
    #[allow(dead_code)]
    published_at: String,
    #[serde(default)]
    contracts: Vec<ManifestContract>,
}

#[derive(Debug, Deserialize)]
struct ManifestContract {
    network: Network,
    #[serde(rename = "type")]
    contract_type: ContractType,
    id: String,
}

/// Fetch the manifest URL, parse it, and return raw bytes alongside the
/// decoded allowlist. Raw bytes are returned so the caller can persist
/// them verbatim to the disk cache — round-tripping the parsed value
/// would lose the `publishedAt` ordering and any future fields.
pub async fn fetch_and_parse(url: &str) -> Result<(Vec<u8>, ContractAllowlist), String> {
    let client = reqwest::Client::builder()
        .timeout(FETCH_TIMEOUT)
        .build()
        .map_err(|e| format!("failed to build HTTP client: {e}"))?;

    let response = client
        .get(url)
        .header("User-Agent", "onym-relayer-manifest")
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| format!("manifest fetch failed: {e}"))?;

    if !response.status().is_success() {
        return Err(format!(
            "manifest fetch returned HTTP {}: {url}",
            response.status()
        ));
    }

    if let Some(len) = response.content_length() {
        if len > MAX_MANIFEST_BYTES {
            return Err(format!(
                "manifest too large: {len} bytes (cap {MAX_MANIFEST_BYTES})"
            ));
        }
    }

    let bytes = response
        .bytes()
        .await
        .map_err(|e| format!("manifest read failed: {e}"))?;
    if bytes.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(format!(
            "manifest too large after read: {} bytes (cap {MAX_MANIFEST_BYTES})",
            bytes.len()
        ));
    }

    let allowlist = parse_manifest_bytes(&bytes)?;
    Ok((bytes.to_vec(), allowlist))
}

/// Parse a manifest blob and flatten every release's contracts into the
/// allowlist shape. Validates non-empty (at least one contract ID across
/// all networks/types) — same invariant the build-time
/// `generate-contract-allowlist.py` enforces.
pub fn parse_manifest_bytes(bytes: &[u8]) -> Result<ContractAllowlist, String> {
    let manifest: Manifest =
        serde_json::from_slice(bytes).map_err(|e| format!("manifest JSON parse failed: {e}"))?;

    let mut allowlist = ContractAllowlist::new();
    for release in &manifest.releases {
        for contract in &release.contracts {
            insert_contract_id(
                &mut allowlist,
                contract.network,
                contract.contract_type,
                &contract.id,
            )?;
        }
    }

    let total: usize = allowlist
        .values()
        .flat_map(|by_type| by_type.values())
        .map(|ids| ids.len())
        .sum();
    if total == 0 {
        return Err("manifest contained no contract IDs across any release".to_string());
    }

    Ok(allowlist)
}

/// Best-effort write of the manifest blob to a last-known-good cache. We
/// log and swallow any error: the relayer still has the new allowlist
/// loaded — only the next-boot fallback is degraded.
pub async fn save_cache(path: &Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        if let Err(e) = fs::create_dir_all(parent).await {
            eprintln!(
                "[manifest] WARN: failed to create cache dir {}: {e}",
                parent.display()
            );
            return;
        }
    }
    if let Err(e) = fs::write(path, bytes).await {
        eprintln!(
            "[manifest] WARN: failed to write cache {}: {e}",
            path.display()
        );
    }
}

/// Read the disk cache and parse it. Returns `Ok(None)` when the cache
/// doesn't exist yet (first boot before the first successful fetch);
/// `Err` only on real corruption that should be surfaced.
pub async fn load_cache(path: &Path) -> Result<Option<ContractAllowlist>, String> {
    let bytes = match fs::read(path).await {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cache read failed: {e}")),
    };
    parse_manifest_bytes(&bytes).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flattens_contracts_across_every_release() {
        let bytes = serde_json::to_vec(&serde_json::json!({
            "version": 1,
            "releases": [
                {
                    "release": "v0.2.0",
                    "publishedAt": "2026-05-01T00:00:00Z",
                    "contracts": [
                        {"network": "testnet", "type": "anarchy",
                         "id": "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAD2KM"},
                        {"network": "testnet", "type": "tyranny",
                         "id": "CBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBQ6L2"}
                    ]
                },
                {
                    "release": "v0.1.0",
                    "publishedAt": "2026-04-01T00:00:00Z",
                    "contracts": [
                        {"network": "testnet", "type": "anarchy",
                         "id": "CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCQI2M"}
                    ]
                }
            ]
        }))
        .unwrap();

        let allowlist = parse_manifest_bytes(&bytes).unwrap();
        let testnet_anarchy = &allowlist[&Network::Testnet][&ContractType::Anarchy];
        // Both releases' anarchy contracts present — historical union, not just latest.
        assert_eq!(testnet_anarchy.len(), 2);
        assert!(allowlist[&Network::Testnet][&ContractType::Tyranny]
            .contains("CBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBQ6L2"));
    }

    #[test]
    fn dedupes_when_same_id_appears_in_multiple_releases() {
        let bytes = serde_json::to_vec(&serde_json::json!({
            "version": 1,
            "releases": [
                {
                    "release": "v0.2.0",
                    "publishedAt": "2026-05-01T00:00:00Z",
                    "contracts": [
                        {"network": "testnet", "type": "anarchy",
                         "id": "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAD2KM"}
                    ]
                },
                {
                    "release": "v0.1.0",
                    "publishedAt": "2026-04-01T00:00:00Z",
                    "contracts": [
                        {"network": "testnet", "type": "anarchy",
                         "id": "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAD2KM"}
                    ]
                }
            ]
        }))
        .unwrap();

        let allowlist = parse_manifest_bytes(&bytes).unwrap();
        assert_eq!(
            allowlist[&Network::Testnet][&ContractType::Anarchy].len(),
            1
        );
    }

    #[test]
    fn rejects_empty_manifest() {
        let bytes = serde_json::to_vec(&serde_json::json!({
            "version": 1,
            "releases": []
        }))
        .unwrap();
        let err = parse_manifest_bytes(&bytes).unwrap_err();
        assert!(err.contains("no contract IDs"));
    }

    #[test]
    fn rejects_id_collision_across_types() {
        // Same contract ID listed under two different governance types
        // on the same network — would silently mis-route requests.
        let bytes = serde_json::to_vec(&serde_json::json!({
            "version": 1,
            "releases": [
                {
                    "release": "v0.1.0",
                    "publishedAt": "2026-04-01T00:00:00Z",
                    "contracts": [
                        {"network": "testnet", "type": "anarchy",
                         "id": "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAD2KM"},
                        {"network": "testnet", "type": "tyranny",
                         "id": "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAD2KM"}
                    ]
                }
            ]
        }))
        .unwrap();
        let err = parse_manifest_bytes(&bytes).unwrap_err();
        assert!(err.contains("mapped to both"));
    }

    #[test]
    fn rejects_non_c_prefixed_id() {
        let bytes = serde_json::to_vec(&serde_json::json!({
            "version": 1,
            "releases": [
                {
                    "release": "v0.1.0",
                    "publishedAt": "2026-04-01T00:00:00Z",
                    "contracts": [
                        {"network": "testnet", "type": "anarchy", "id": "GBADIDEA"}
                    ]
                }
            ]
        }))
        .unwrap();
        let err = parse_manifest_bytes(&bytes).unwrap_err();
        assert!(err.contains("must start with C"));
    }
}
