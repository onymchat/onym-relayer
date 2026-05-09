use base64::Engine;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

use crate::config::{Config, ContractType, Network};

/// Functions that are read-only (use `--send no`).
pub const READ_ONLY_FUNCTIONS: &[&str] = &[
    "verify_membership",
    "get_commitment",
    "get_admin_commitment",
    "get_history",
];

/// State-changing admin functions that must not be exposed by an unauthenticated relayer.
const RELAYER_AUTH_REQUIRED_FUNCTIONS: &[&str] = &["set_restricted_mode"];

/// Functions that include a proof field in the payload.
const PROOF_FUNCTIONS: &[&str] = &[
    "create_group",
    "create_oligarchy_group",
    "update_commitment",
    "verify_membership",
];

/// Expected decoded PLONK proof size.
const EXPECTED_PROOF_SIZE: usize = 1601;

/// Validate a request against the relayer's security rules.
pub fn validate_request(
    config: &Config,
    network: Network,
    contract_type: ContractType,
    contract_id: &str,
    function: &str,
    payload: &serde_json::Value,
) -> Result<(), String> {
    // 1. Contract ID whitelist
    match config.contract_type_for_id(network, contract_id) {
        Some(configured_type) if configured_type == contract_type => {}
        Some(configured_type) => {
            return Err(format!(
                "contract ID {contract_id} belongs to {configured_type} on {network}, not {contract_type}"
            ));
        }
        None => {
            return Err(format!(
                "unknown or unconfigured contract ID for {network}: {contract_id}"
            ))
        }
    }

    // 2. Function whitelist
    if !function_allowed(contract_type, function) {
        return Err(format!(
            "function not allowed for {contract_type}: {function}"
        ));
    }
    if RELAYER_AUTH_REQUIRED_FUNCTIONS.contains(&function) && !config.auth_required() {
        return Err(format!(
            "{function} requires RELAYER_AUTH_TOKENS because it signs an admin operation"
        ));
    }

    // 3. Proof size validation (for functions that include a proof)
    if PROOF_FUNCTIONS.contains(&function) {
        if let Some(proof) = payload.get("proof").and_then(|v| v.as_str()) {
            let decoded =
                decode_hex_or_base64(proof).map_err(|e| format!("invalid proof encoding: {e}"))?;
            if decoded.len() != EXPECTED_PROOF_SIZE {
                return Err(format!(
                    "proof must be {EXPECTED_PROOF_SIZE} bytes, got {}",
                    decoded.len()
                ));
            }
        }
    }

    Ok(())
}

fn function_allowed(contract_type: ContractType, function: &str) -> bool {
    match contract_type {
        ContractType::Anarchy | ContractType::Democracy => matches!(
            function,
            "create_group"
                | "update_commitment"
                | "verify_membership"
                | "get_commitment"
                | "bump_group_ttl"
                | "set_restricted_mode"
                | "get_history"
        ),
        ContractType::Tyranny => matches!(
            function,
            "create_group"
                | "update_commitment"
                | "verify_membership"
                | "get_commitment"
                | "get_admin_commitment"
                | "bump_group_ttl"
                | "set_restricted_mode"
                | "get_history"
        ),
        ContractType::OneOnOne => {
            matches!(
                function,
                "create_group"
                    | "verify_membership"
                    | "get_commitment"
                    | "bump_group_ttl"
                    | "set_restricted_mode"
            )
        }
        ContractType::Oligarchy => matches!(
            function,
            "create_oligarchy_group"
                | "update_commitment"
                | "verify_membership"
                | "get_commitment"
                | "bump_group_ttl"
                | "set_restricted_mode"
                | "get_history"
        ),
    }
}

fn decode_hex_or_base64(raw: &str) -> Result<Vec<u8>, String> {
    let trimmed = raw.trim();
    let hex = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
        .unwrap_or(trimmed);
    if hex.len() == EXPECTED_PROOF_SIZE * 2 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return hex::decode(hex).map_err(|e| format!("invalid proof hex: {e}"));
    }
    base64::engine::general_purpose::STANDARD
        .decode(trimmed)
        .map_err(|e| format!("invalid proof base64: {e}"))
}

/// Validate bearer token if auth is required.
pub fn validate_auth(config: &Config, auth_header: Option<&str>) -> Result<(), String> {
    if !config.auth_required() {
        return Ok(());
    }
    let header = auth_header.ok_or("Authorization header required")?;
    let token = header
        .strip_prefix("Bearer ")
        .ok_or("Authorization must be: Bearer <token>")?;
    if config.auth_tokens.contains(token) {
        Ok(())
    } else {
        Err("invalid bearer token".to_string())
    }
}

/// Simple in-memory rate limiter by IP address.
pub struct RateLimiter {
    /// IP → (window_start, count)
    buckets: Mutex<HashMap<String, (Instant, u32)>>,
    max_per_minute: u32,
}

impl RateLimiter {
    pub fn new(max_per_minute: u32) -> Self {
        Self {
            buckets: Mutex::new(HashMap::new()),
            max_per_minute,
        }
    }

    /// Returns Ok if the request is allowed, Err if rate-limited.
    pub fn check(&self, ip: &str) -> Result<(), String> {
        let mut buckets = self.buckets.lock().unwrap();
        let now = Instant::now();
        let entry = buckets.entry(ip.to_string()).or_insert((now, 0));

        // Reset window if more than 60 seconds have passed
        if now.duration_since(entry.0).as_secs() >= 60 {
            *entry = (now, 0);
        }

        entry.1 += 1;
        if entry.1 > self.max_per_minute {
            Err(format!(
                "rate limited: {} requests/minute exceeded",
                self.max_per_minute
            ))
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};

    fn make_config(
        contract_type: ContractType,
        contract_id: &str,
        auth_tokens: HashSet<String>,
    ) -> Config {
        let mut contracts = HashMap::new();
        contracts.insert(contract_type, HashSet::from([contract_id.to_string()]));
        let mut contract_allowlist = HashMap::new();
        contract_allowlist.insert(Network::Testnet, contracts);
        let mut networks = HashMap::new();
        networks.insert(
            Network::Testnet,
            crate::config::NetworkConfig {
                rpc_url: String::new(),
                network_passphrase: String::new(),
                cli_network: "testnet".to_string(),
            },
        );
        networks.insert(
            Network::Public,
            crate::config::NetworkConfig {
                rpc_url: String::new(),
                network_passphrase: String::new(),
                cli_network: "mainnet".to_string(),
            },
        );
        Config {
            secret_key: String::new(),
            public_address: String::new(),
            contract_allowlist: std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(
                contract_allowlist,
            )),
            networks,
            bind_address: String::new(),
            auth_tokens,
            rate_limit_per_minute: 30,
            max_payload_size: 8192,
            identity_name: String::new(),
            allowlist_source: crate::config::AllowlistSource::Static,
        }
    }

    fn config_no_auth(contract_id: &str) -> Config {
        make_config(ContractType::Anarchy, contract_id, HashSet::new())
    }

    fn config_with_auth(contract_id: &str, token: &str) -> Config {
        let mut tokens = HashSet::new();
        tokens.insert(token.to_string());
        make_config(ContractType::Anarchy, contract_id, tokens)
    }

    /// Helper: produce a base64-encoded proof of exactly `n` bytes.
    fn make_proof_base64(n: usize) -> String {
        let bytes = vec![0xABu8; n];
        base64::engine::general_purpose::STANDARD.encode(&bytes)
    }

    // ---- Auth tests ----

    #[test]
    fn test_auth_not_required_when_no_tokens() {
        let config = config_no_auth("C123");
        assert!(validate_auth(&config, None).is_ok());
    }

    #[test]
    fn test_auth_required_when_tokens_configured() {
        let config = config_with_auth("C123", "validtoken");
        assert!(validate_auth(&config, None).is_err());
    }

    #[test]
    fn test_valid_bearer_token_accepted() {
        let config = config_with_auth("C123", "validtoken");
        assert!(validate_auth(&config, Some("Bearer validtoken")).is_ok());
    }

    #[test]
    fn test_invalid_bearer_token_rejected() {
        let config = config_with_auth("C123", "validtoken");
        let result = validate_auth(&config, Some("Bearer wrongtoken"));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("invalid bearer token"));
    }

    #[test]
    fn test_missing_auth_header_rejected() {
        let config = config_with_auth("C123", "validtoken");
        let result = validate_auth(&config, None);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .contains("Authorization header required"));
    }

    #[test]
    fn test_wrong_prefix_rejected() {
        let config = config_with_auth("C123", "validtoken");
        let result = validate_auth(&config, Some("Basic xyz"));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Bearer"));
    }

    // ---- Request validation tests ----

    #[test]
    fn test_correct_contract_id_accepted() {
        let config = config_no_auth("CABC123");
        let payload = serde_json::json!({});
        assert!(validate_request(
            &config,
            Network::Testnet,
            ContractType::Anarchy,
            "CABC123",
            "get_commitment",
            &payload
        )
        .is_ok());
    }

    #[test]
    fn test_wrong_contract_id_rejected() {
        let config = config_no_auth("CABC123");
        let payload = serde_json::json!({});
        let result = validate_request(
            &config,
            Network::Testnet,
            ContractType::Anarchy,
            "CWRONG",
            "get_commitment",
            &payload,
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown or unconfigured"));
    }

    #[test]
    fn test_anarchy_allowed_functions_accepted() {
        let config = config_no_auth("C1");
        let payload = serde_json::json!({});
        for func in [
            "create_group",
            "update_commitment",
            "verify_membership",
            "get_commitment",
            "get_history",
            "bump_group_ttl",
        ] {
            assert!(
                validate_request(
                    &config,
                    Network::Testnet,
                    ContractType::Anarchy,
                    "C1",
                    func,
                    &payload
                )
                .is_ok(),
                "expected function '{}' to be allowed",
                func
            );
        }
    }

    #[test]
    fn test_disallowed_function_rejected() {
        let config = config_no_auth("C1");
        let payload = serde_json::json!({});
        let result = validate_request(
            &config,
            Network::Testnet,
            ContractType::Anarchy,
            "C1",
            "initialize",
            &payload,
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("function not allowed"));
    }

    #[test]
    fn test_oneonone_rejects_update_commitment() {
        let config = make_config(ContractType::OneOnOne, "C1", HashSet::new());
        let payload = serde_json::json!({});
        let result = validate_request(
            &config,
            Network::Testnet,
            ContractType::OneOnOne,
            "C1",
            "update_commitment",
            &payload,
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("function not allowed"));
    }

    #[test]
    fn test_oligarchy_create_oligarchy_group_allowed() {
        let config = make_config(ContractType::Oligarchy, "C1", HashSet::new());
        let payload = serde_json::json!({});
        assert!(validate_request(
            &config,
            Network::Testnet,
            ContractType::Oligarchy,
            "C1",
            "create_oligarchy_group",
            &payload
        )
        .is_ok());
    }

    #[test]
    fn test_set_restricted_mode_requires_relayer_auth_tokens() {
        let config = config_no_auth("C1");
        let payload = serde_json::json!({ "restricted": true });
        let result = validate_request(
            &config,
            Network::Testnet,
            ContractType::Anarchy,
            "C1",
            "set_restricted_mode",
            &payload,
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("RELAYER_AUTH_TOKENS"));
    }

    #[test]
    fn test_set_restricted_mode_allowed_when_relayer_auth_configured() {
        let config = config_with_auth("C1", "validtoken");
        let payload = serde_json::json!({ "restricted": true });
        assert!(validate_request(
            &config,
            Network::Testnet,
            ContractType::Anarchy,
            "C1",
            "set_restricted_mode",
            &payload
        )
        .is_ok());
    }

    #[test]
    fn test_get_admin_commitment_only_allowed_for_tyranny() {
        let anarchy = config_no_auth("C1");
        let payload = serde_json::json!({});
        assert!(validate_request(
            &anarchy,
            Network::Testnet,
            ContractType::Anarchy,
            "C1",
            "get_admin_commitment",
            &payload
        )
        .is_err());

        let tyranny = make_config(ContractType::Tyranny, "C2", HashSet::new());
        assert!(validate_request(
            &tyranny,
            Network::Testnet,
            ContractType::Tyranny,
            "C2",
            "get_admin_commitment",
            &payload
        )
        .is_ok());
    }

    #[test]
    fn test_proof_size_1601_bytes_accepted() {
        let config = config_no_auth("C1");
        let proof = make_proof_base64(1601);
        let payload = serde_json::json!({ "proof": proof });
        assert!(validate_request(
            &config,
            Network::Testnet,
            ContractType::Anarchy,
            "C1",
            "create_group",
            &payload
        )
        .is_ok());
    }

    #[test]
    fn test_proof_size_wrong_rejected() {
        let config = config_no_auth("C1");
        let proof = make_proof_base64(100);
        let payload = serde_json::json!({ "proof": proof });
        let result = validate_request(
            &config,
            Network::Testnet,
            ContractType::Anarchy,
            "C1",
            "create_group",
            &payload,
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("1601 bytes"));
    }

    #[test]
    fn test_proof_not_required_for_get_commitment() {
        let config = config_no_auth("C1");
        let payload = serde_json::json!({});
        assert!(validate_request(
            &config,
            Network::Testnet,
            ContractType::Anarchy,
            "C1",
            "get_commitment",
            &payload
        )
        .is_ok());
    }

    #[test]
    fn test_invalid_base64_proof_rejected() {
        let config = config_no_auth("C1");
        let payload = serde_json::json!({ "proof": "not-base64!!!" });
        let result = validate_request(
            &config,
            Network::Testnet,
            ContractType::Anarchy,
            "C1",
            "create_group",
            &payload,
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("invalid proof base64"));
    }

    // ---- Rate limiter tests ----

    #[test]
    fn test_rate_limit_allows_within_window() {
        let limiter = RateLimiter::new(5);
        for i in 0..5 {
            assert!(
                limiter.check("192.168.1.1").is_ok(),
                "request {} should be allowed",
                i + 1
            );
        }
    }

    #[test]
    fn test_rate_limit_rejects_over_limit() {
        let limiter = RateLimiter::new(3);
        for _ in 0..3 {
            assert!(limiter.check("10.0.0.1").is_ok());
        }
        let result = limiter.check("10.0.0.1");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("rate limited"));
    }

    #[test]
    fn test_rate_limit_independent_per_ip() {
        let limiter = RateLimiter::new(2);
        // Use up IP A's quota
        assert!(limiter.check("ip_a").is_ok());
        assert!(limiter.check("ip_a").is_ok());
        assert!(limiter.check("ip_a").is_err());

        // IP B should still have its full quota
        assert!(limiter.check("ip_b").is_ok());
        assert!(limiter.check("ip_b").is_ok());
        assert!(limiter.check("ip_b").is_err());
    }
}
