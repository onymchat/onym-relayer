use std::process::Command;
use std::sync::Arc;

use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Json;
use axum::response::{IntoResponse, Response};
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::config::{Config, ContractType};
use crate::validation::{self, RateLimiter, READ_ONLY_FUNCTIONS};

/// Shared application state.
pub struct AppState {
    pub config: Config,
    pub rate_limiter: RateLimiter,
}

/// Incoming request from mobile apps.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayerRequest {
    #[serde(alias = "contractID")]
    #[serde(default, alias = "contract_id")]
    pub contract_id: Option<String>,
    #[serde(default, alias = "contract_type")]
    pub contract_type: Option<ContractType>,
    pub function: String,
    pub payload: Value,
}

/// Response returned to mobile apps.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayerResponse {
    pub accepted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transaction_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ResolvedInvocation {
    pub contract_type: ContractType,
    pub contract_id: String,
    pub contract_function: String,
}

/// POST / — main relayer endpoint.
pub async fn handle_invoke(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<RelayerRequest>,
) -> Response {
    let ip = addr.ip().to_string();

    // Auth check
    let auth_header = headers.get("authorization").and_then(|v| v.to_str().ok());
    if let Err(e) = validation::validate_auth(&state.config, auth_header) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(RelayerResponse {
                accepted: false,
                transaction_hash: None,
                message: Some(e),
            }),
        )
            .into_response();
    }

    // Rate limit check
    if let Err(e) = state.rate_limiter.check(&ip) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(RelayerResponse {
                accepted: false,
                transaction_hash: None,
                message: Some(e),
            }),
        )
            .into_response();
    }

    let invocation = match resolve_invocation(&state.config, &request) {
        Ok(invocation) => invocation,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(RelayerResponse {
                    accepted: false,
                    transaction_hash: None,
                    message: Some(e),
                }),
            )
                .into_response();
        }
    };

    // Validate request
    if let Err(e) = validation::validate_request(
        &state.config,
        invocation.contract_type,
        &invocation.contract_id,
        &request.function,
        &request.payload,
    ) {
        return (
            StatusCode::BAD_REQUEST,
            Json(RelayerResponse {
                accepted: false,
                transaction_hash: None,
                message: Some(e),
            }),
        )
            .into_response();
    }

    // Build and execute the stellar CLI invocation
    match invoke_contract(&state.config, &invocation, &request.payload).await {
        Ok(output) => match success_response(&invocation, output) {
            Ok(response) => response,
            Err(e) => (
                StatusCode::BAD_GATEWAY,
                Json(RelayerResponse {
                    accepted: false,
                    transaction_hash: None,
                    message: Some(e),
                }),
            )
                .into_response(),
        },
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(RelayerResponse {
                accepted: false,
                transaction_hash: None,
                message: Some(e),
            }),
        )
            .into_response(),
    }
}

fn resolve_invocation(
    config: &Config,
    request: &RelayerRequest,
) -> Result<ResolvedInvocation, String> {
    let id_contract_type = match request.contract_id.as_deref() {
        Some(contract_id) => Some(
            config
                .contract_type_for_id(contract_id)
                .ok_or_else(|| format!("unknown or unconfigured contract ID: {contract_id}"))?,
        ),
        None => None,
    };
    let payload_contract_type = payload_contract_type(&request.payload)?;
    let implied_contract_type = implied_contract_type(&request.function)?;

    let mut resolved_type = id_contract_type
        .or(request.contract_type)
        .or(payload_contract_type)
        .or(implied_contract_type)
        .ok_or_else(|| {
            "contractID, contractType, or payload.group_type is required to select a contract"
                .to_string()
        })?;

    for (label, maybe_contract_type) in [
        ("contractType", request.contract_type),
        ("payload.group_type", payload_contract_type),
        ("function", implied_contract_type),
    ] {
        if let Some(contract_type) = maybe_contract_type {
            if contract_type != resolved_type {
                return Err(format!(
                    "{label} selects {contract_type}, but request resolves to {resolved_type}"
                ));
            }
            resolved_type = contract_type;
        }
    }

    let contract_id = config
        .contract_id_for(resolved_type)
        .ok_or_else(|| format!("contract ID for {resolved_type} is not configured"))?;
    if let Some(requested_id) = request.contract_id.as_deref() {
        if requested_id != contract_id {
            return Err(format!(
                "contractID {requested_id} does not match configured {resolved_type} contract {contract_id}"
            ));
        }
    }

    let contract_function = contract_function_for(resolved_type, &request.function)?;
    Ok(ResolvedInvocation {
        contract_type: resolved_type,
        contract_id: contract_id.to_string(),
        contract_function: contract_function.to_string(),
    })
}

fn implied_contract_type(function: &str) -> Result<Option<ContractType>, String> {
    if function == "create_oligarchy_group" {
        return Ok(Some(ContractType::Oligarchy));
    }
    Ok(None)
}

fn payload_contract_type(payload: &Value) -> Result<Option<ContractType>, String> {
    let Some(value) = field_value(payload, GROUP_TYPE_KEYS) else {
        return Ok(None);
    };

    if let Some(id) = value.as_u64() {
        let id = u32::try_from(id).map_err(|_| format!("group_type out of range: {id}"))?;
        return ContractType::from_group_type_id(id).map(Some);
    }
    if let Some(id) = value.as_i64() {
        let id = u32::try_from(id).map_err(|_| format!("group_type out of range: {id}"))?;
        return ContractType::from_group_type_id(id).map(Some);
    }
    if let Some(name) = value.as_str() {
        return name.parse::<ContractType>().map(Some);
    }

    Err("group_type must be a number or contract type string".to_string())
}

fn contract_function_for(
    contract_type: ContractType,
    requested_function: &str,
) -> Result<&'static str, String> {
    match requested_function {
        "create_group" => match contract_type {
            ContractType::Oligarchy => {
                Err("oligarchy contract uses create_oligarchy_group".to_string())
            }
            _ => Ok("create_group"),
        },
        "create_oligarchy_group" => {
            if contract_type == ContractType::Oligarchy {
                Ok("create_oligarchy_group")
            } else {
                Err(format!(
                    "create_oligarchy_group is only valid for the oligarchy contract, got {contract_type}"
                ))
            }
        }
        "update_commitment" => {
            if contract_type == ContractType::OneOnOne {
                Err("oneonone contract does not support update_commitment".to_string())
            } else {
                Ok("update_commitment")
            }
        }
        "verify_membership" => Ok("verify_membership"),
        "get_commitment" => Ok("get_commitment"),
        "get_history" => {
            if contract_type == ContractType::OneOnOne {
                Err("oneonone contract does not support get_history".to_string())
            } else {
                Ok("get_history")
            }
        }
        "get_admin_commitment" => {
            if contract_type == ContractType::Tyranny {
                Ok("get_admin_commitment")
            } else {
                Err(format!(
                    "get_admin_commitment is only valid for the tyranny contract, got {contract_type}"
                ))
            }
        }
        "bump_group_ttl" => Ok("bump_group_ttl"),
        "set_restricted_mode" => Ok("set_restricted_mode"),
        other => Err(format!("function not allowed for {contract_type}: {other}")),
    }
}

fn success_response(invocation: &ResolvedInvocation, output: String) -> Result<Response, String> {
    match invocation.contract_function.as_str() {
        "create_group"
        | "update_commitment"
        | "create_oligarchy_group"
        | "bump_group_ttl"
        | "set_restricted_mode" => Ok((
            StatusCode::OK,
            Json(RelayerResponse {
                accepted: true,
                transaction_hash: None,
                message: if output.is_empty() {
                    None
                } else {
                    Some(output)
                },
            }),
        )
            .into_response()),
        "verify_membership" => {
            let valid = parse_bool_output(&output)?;
            Ok((StatusCode::OK, Json(json!({ "valid": valid }))).into_response())
        }
        "get_commitment" | "get_history" => {
            let mut value: Value = serde_json::from_str(&output).map_err(|e| {
                format!("failed to parse stellar CLI JSON output: {e}; output={output}")
            })?;
            normalize_bytes_fields(&mut value)?;
            Ok((StatusCode::OK, Json(value)).into_response())
        }
        "get_admin_commitment" => {
            let commitment = parse_bytes_output(&output)?;
            Ok((
                StatusCode::OK,
                Json(json!({ "adminPubkeyCommitment": commitment })),
            )
                .into_response())
        }
        other => Err(format!("unsupported function: {other}")),
    }
}

fn parse_bool_output(output: &str) -> Result<bool, String> {
    let trimmed = output.trim();
    if trimmed.eq_ignore_ascii_case("true") {
        return Ok(true);
    }
    if trimmed.eq_ignore_ascii_case("false") {
        return Ok(false);
    }
    serde_json::from_str::<bool>(trimmed)
        .map_err(|e| format!("failed to parse boolean output: {e}; output={trimmed}"))
}

fn normalize_bytes_fields(value: &mut Value) -> Result<(), String> {
    match value {
        Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                if is_bytes_field(key) {
                    if let Some(s) = child.as_str() {
                        *child = Value::String(hex_to_base64_if_needed(s)?);
                    }
                } else {
                    normalize_bytes_fields(child)?;
                }
            }
            Ok(())
        }
        Value::Array(items) => {
            for item in items.iter_mut() {
                normalize_bytes_fields(item)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn parse_bytes_output(output: &str) -> Result<String, String> {
    let trimmed = output.trim();
    let value = serde_json::from_str::<Value>(trimmed)
        .unwrap_or_else(|_| Value::String(trimmed.trim_matches('"').to_string()));
    let Some(s) = value.as_str() else {
        return Err(format!("expected bytes output string, got: {output}"));
    };
    hex_to_base64_if_needed(s)
}

fn is_bytes_field(key: &str) -> bool {
    matches!(
        key,
        "commitment"
            | "occupancy_commitment"
            | "occupancy_commitment_initial"
            | "occupancy_commitment_old"
            | "occupancy_commitment_new"
            | "admin_pubkey_commitment"
            | "member_root"
            | "admin_root"
            | "salt_initial"
            | "c_old"
            | "c_new"
    )
}

fn hex_to_base64_if_needed(s: &str) -> Result<String, String> {
    let hex = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s);
    if hex.len() % 2 == 0 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        let bytes = hex::decode(hex).map_err(|e| format!("invalid hex commitment: {e}"))?;
        Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
    } else {
        Ok(s.to_string())
    }
}

/// Invoke the Soroban contract via the `stellar` CLI.
async fn invoke_contract(
    config: &Config,
    invocation: &ResolvedInvocation,
    payload: &Value,
) -> Result<String, String> {
    let function = invocation.contract_function.as_str();
    let is_read_only = READ_ONLY_FUNCTIONS.contains(&function);

    let mut cmd = Command::new("stellar");
    cmd.arg("contract")
        .arg("invoke")
        .arg("--rpc-url")
        .arg(&config.rpc_url)
        .arg("--network-passphrase")
        .arg(&config.network_passphrase)
        .arg("--network")
        .arg(&config.network)
        .arg("--id")
        .arg(&invocation.contract_id)
        .arg("--source-account")
        .arg(&config.identity_name);

    if is_read_only {
        cmd.arg("--send").arg("no");
    }

    cmd.arg("--");
    cmd.arg(function);

    // Build function-specific arguments.
    //
    // Every `field_value` lookup uses the canonical ordering declared in
    // `GROUP_ID_KEYS`, `GROUP_TYPE_KEYS`, etc. — snake_case first (Soroban
    // ABI convention, what the contract ships today), camelCase as a
    // alternate spelling for callers that encode request JSON that way. Keep the constants as the
    // single source of truth: a malicious client that sends *both* keys
    // with divergent values will always resolve to the snake_case value,
    // regardless of which endpoint is hit.
    match (invocation.contract_type, function) {
        (_, "create_group") => {
            add_create_group_args(invocation.contract_type, config, &mut cmd, payload)?;
        }
        (ContractType::Oligarchy, "create_oligarchy_group") => {
            add_create_oligarchy_group_args(config, &mut cmd, payload)?;
        }
        (_, "update_commitment") => {
            // #59: UpdateCircuit binds c_new cryptographically. The relayer no
            // longer forwards client-supplied `new_commitment` or `new_epoch`;
            // c_new comes from the UpdatePublicInputs payload and the contract
            // derives new_epoch on-chain as stored_epoch + 1.
            add_hex_arg(&mut cmd, "--group-id", payload, GROUP_ID_KEYS)?;
            add_proof_arg(&mut cmd, payload)?;
            add_public_inputs_arg(invocation.contract_type, function, &mut cmd, payload)?;
        }
        (_, "verify_membership") => {
            add_hex_arg(&mut cmd, "--group-id", payload, GROUP_ID_KEYS)?;
            add_proof_arg(&mut cmd, payload)?;
            add_public_inputs_arg(invocation.contract_type, function, &mut cmd, payload)?;
        }
        (_, "get_commitment") => {
            add_hex_arg(&mut cmd, "--group-id", payload, GROUP_ID_KEYS)?;
        }
        (_, "get_admin_commitment") => {
            add_hex_arg(&mut cmd, "--group-id", payload, GROUP_ID_KEYS)?;
        }
        (_, "get_history") => {
            add_hex_arg(&mut cmd, "--group-id", payload, GROUP_ID_KEYS)?;
            let max_entries = field_value(payload, MAX_ENTRIES_KEYS)
                .and_then(|v| v.as_u64())
                .unwrap_or(64);
            cmd.arg("--max-entries").arg(max_entries.to_string());
        }
        (_, "bump_group_ttl") => {
            add_hex_arg(&mut cmd, "--group-id", payload, GROUP_ID_KEYS)?;
        }
        (_, "set_restricted_mode") => {
            add_bool_arg(&mut cmd, "--restricted", payload, RESTRICTED_KEYS)?;
        }
        _ => return Err(format!("unsupported function: {function}")),
    }

    // Execute
    let output = tokio::task::spawn_blocking(move || cmd.output())
        .await
        .map_err(|e| format!("task join error: {e}"))?
        .map_err(|e| format!("failed to execute stellar CLI: {e}"))?;

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Ok(stdout)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Err(format!("{stderr} {stdout}").trim().to_string())
    }
}

/// Canonical key-lookup orders for payload fields.
///
/// Snake_case comes first (Soroban ABI convention), camelCase second for
/// callers that encode request JSON that way.
/// Keep these as the single source of truth so every `field_value` call
/// site resolves conflicts the same way: if a payload contains *both*
/// variants with different values, the snake_case value always wins.
pub(crate) const GROUP_ID_KEYS: &[&str] = &["group_id", "groupID"];
pub(crate) const GROUP_TYPE_KEYS: &[&str] = &["group_type", "groupType"];
pub(crate) const COMMITMENT_KEYS: &[&str] = &["commitment"];
pub(crate) const EPOCH_KEYS: &[&str] = &["epoch"];
pub(crate) const MEMBER_COUNT_KEYS: &[&str] = &["member_count", "memberCount"];
pub(crate) const TIER_KEYS: &[&str] = &["tier"];
pub(crate) const MEMBER_TIER_KEYS: &[&str] = &["member_tier", "memberTier", "tier"];
pub(crate) const THRESHOLD_NUMERATOR_KEYS: &[&str] =
    &["threshold_numerator", "thresholdNumerator", "threshold"];
pub(crate) const ADMIN_THRESHOLD_NUMERATOR_KEYS: &[&str] = &[
    "admin_threshold_numerator",
    "adminThresholdNumerator",
    "threshold_numerator",
    "thresholdNumerator",
    "threshold",
];
pub(crate) const OCCUPANCY_COMMITMENT_INITIAL_KEYS: &[&str] = &[
    "occupancy_commitment_initial",
    "occupancyCommitmentInitial",
    "occupancy_commitment",
    "occupancyCommitment",
];
pub(crate) const OCCUPANCY_COMMITMENT_OLD_KEYS: &[&str] =
    &["occupancy_commitment_old", "occupancyCommitmentOld"];
pub(crate) const OCCUPANCY_COMMITMENT_NEW_KEYS: &[&str] =
    &["occupancy_commitment_new", "occupancyCommitmentNew"];
pub(crate) const ADMIN_PUBKEY_COMMITMENT_KEYS: &[&str] = &[
    "admin_pubkey_commitment",
    "adminPubkeyCommitment",
    "admin_public_key_commitment",
    "adminPublicKeyCommitment",
    "admin_commitment",
    "adminCommitment",
];
pub(crate) const MEMBER_ROOT_KEYS: &[&str] = &["member_root", "memberRoot"];
pub(crate) const ADMIN_ROOT_KEYS: &[&str] = &["admin_root", "adminRoot"];
pub(crate) const SALT_INITIAL_KEYS: &[&str] = &["salt_initial", "saltInitial"];
pub(crate) const C_OLD_KEYS: &[&str] = &["c_old", "cOld"];
pub(crate) const EPOCH_OLD_KEYS: &[&str] = &["epoch_old", "epochOld"];
pub(crate) const C_NEW_KEYS: &[&str] = &["c_new", "cNew"];
pub(crate) const GROUP_ID_FR_KEYS: &[&str] = &["group_id_fr", "groupIdFr"];
pub(crate) const PUBLIC_INPUTS_KEYS: &[&str] = &["public_inputs", "publicInputs"];
pub(crate) const MAX_ENTRIES_KEYS: &[&str] = &["max_entries", "maxEntries"];
pub(crate) const RESTRICTED_KEYS: &[&str] = &["restricted"];

/// Look up a value under the first matching key (supports camelCase/snake_case
/// payload variants emitted by different client SDKs).
fn field_value<'a>(payload: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().find_map(|k| payload.get(*k))
}

/// Decode a base64 payload field (by any of its accepted names) and add as a
/// hex argument.
fn add_hex_arg(
    cmd: &mut Command,
    flag: &str,
    payload: &Value,
    keys: &[&str],
) -> Result<(), String> {
    let raw = field_value(payload, keys)
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("missing field: {}", keys.join("/")))?;
    let bytes = decode_wire_bytes(raw, &keys.join("/"), Some(32))?;
    cmd.arg(flag).arg(hex_encode(&bytes));
    Ok(())
}

/// Add an integer argument from the payload (by any of its accepted names).
fn add_int_arg(
    cmd: &mut Command,
    flag: &str,
    payload: &Value,
    keys: &[&str],
) -> Result<(), String> {
    let val = field_value(payload, keys)
        .and_then(|v| v.as_u64())
        .ok_or_else(|| format!("missing or invalid field: {}", keys.join("/")))?;
    cmd.arg(flag).arg(val.to_string());
    Ok(())
}

fn add_int_arg_or_default(
    cmd: &mut Command,
    flag: &str,
    payload: &Value,
    keys: &[&str],
    default: u64,
) -> Result<(), String> {
    let val = match field_value(payload, keys) {
        Some(value) => value
            .as_u64()
            .ok_or_else(|| format!("invalid field: {}", keys.join("/")))?,
        None => default,
    };
    cmd.arg(flag).arg(val.to_string());
    Ok(())
}

fn add_bool_arg(
    cmd: &mut Command,
    flag: &str,
    payload: &Value,
    keys: &[&str],
) -> Result<(), String> {
    let val = field_value(payload, keys)
        .and_then(|v| v.as_bool())
        .ok_or_else(|| format!("missing or invalid field: {}", keys.join("/")))?;
    cmd.arg(flag).arg(if val { "true" } else { "false" });
    Ok(())
}

fn add_create_group_args(
    contract_type: ContractType,
    config: &Config,
    cmd: &mut Command,
    payload: &Value,
) -> Result<(), String> {
    cmd.arg("--caller").arg(&config.public_address);
    add_hex_arg(cmd, "--group-id", payload, GROUP_ID_KEYS)?;
    add_hex_arg(cmd, "--commitment", payload, &["commitment"])?;

    match contract_type {
        ContractType::OneOnOne => {
            add_proof_arg(cmd, payload)?;
            add_public_inputs_arg(contract_type, "create_group", cmd, payload)?;
        }
        ContractType::Anarchy => {
            add_int_arg(cmd, "--tier", payload, TIER_KEYS)?;
            add_int_arg_or_default(cmd, "--member-count", payload, MEMBER_COUNT_KEYS, 0)?;
            add_proof_arg(cmd, payload)?;
            add_public_inputs_arg(contract_type, "create_group", cmd, payload)?;
        }
        ContractType::Democracy => {
            add_int_arg(cmd, "--tier", payload, TIER_KEYS)?;
            add_int_arg(
                cmd,
                "--threshold-numerator",
                payload,
                THRESHOLD_NUMERATOR_KEYS,
            )?;
            add_hex_arg(
                cmd,
                "--occupancy-commitment-initial",
                payload,
                OCCUPANCY_COMMITMENT_INITIAL_KEYS,
            )?;
            add_proof_arg(cmd, payload)?;
            add_public_inputs_arg(contract_type, "create_group", cmd, payload)?;
        }
        ContractType::Tyranny => {
            add_int_arg(cmd, "--tier", payload, TIER_KEYS)?;
            add_hex_arg(
                cmd,
                "--admin-pubkey-commitment",
                payload,
                ADMIN_PUBKEY_COMMITMENT_KEYS,
            )?;
            add_proof_arg(cmd, payload)?;
            add_public_inputs_arg(contract_type, "create_group", cmd, payload)?;
        }
        ContractType::Oligarchy => {
            return Err("oligarchy uses create_oligarchy_group".to_string());
        }
    }

    Ok(())
}

fn add_create_oligarchy_group_args(
    config: &Config,
    cmd: &mut Command,
    payload: &Value,
) -> Result<(), String> {
    cmd.arg("--caller").arg(&config.public_address);
    add_hex_arg(cmd, "--group-id", payload, GROUP_ID_KEYS)?;
    add_hex_arg(cmd, "--commitment", payload, &["commitment"])?;
    add_int_arg(cmd, "--member-tier", payload, MEMBER_TIER_KEYS)?;
    add_int_arg(
        cmd,
        "--admin-threshold-numerator",
        payload,
        ADMIN_THRESHOLD_NUMERATOR_KEYS,
    )?;
    add_hex_arg(
        cmd,
        "--occupancy-commitment-initial",
        payload,
        OCCUPANCY_COMMITMENT_INITIAL_KEYS,
    )?;
    add_proof_arg(cmd, payload)?;
    add_public_inputs_arg(
        ContractType::Oligarchy,
        "create_oligarchy_group",
        cmd,
        payload,
    )?;
    Ok(())
}

/// Decode the PLONK proof and add it as the `BytesN<1601>` hex argument
/// expected by the Stellar implicit CLI.
fn add_proof_arg(cmd: &mut Command, payload: &Value) -> Result<(), String> {
    let proof = payload
        .get("proof")
        .and_then(|v| v.as_str())
        .ok_or("missing proof field")?;
    let proof_bytes = decode_wire_bytes(proof, "proof", Some(1601))?;
    cmd.arg("--proof").arg(hex_encode(&proof_bytes));
    Ok(())
}

fn add_public_inputs_arg(
    contract_type: ContractType,
    function: &str,
    cmd: &mut Command,
    payload: &Value,
) -> Result<(), String> {
    let public_inputs = public_inputs_bytes(contract_type, function, payload)?;
    let expected = expected_public_input_count(contract_type, function)?;
    if public_inputs.len() != expected {
        return Err(format!(
            "{function} for {contract_type} expects {expected} public inputs, got {}",
            public_inputs.len()
        ));
    }

    let hex_values: Vec<String> = public_inputs
        .iter()
        .map(|bytes| hex_encode(bytes))
        .collect();
    let pi_json = serde_json::to_string(&hex_values)
        .map_err(|e| format!("failed to encode public inputs JSON: {e}"))?;
    cmd.arg("--public-inputs").arg(pi_json);
    Ok(())
}

fn public_inputs_bytes(
    contract_type: ContractType,
    function: &str,
    payload: &Value,
) -> Result<Vec<Vec<u8>>, String> {
    let pi = field_value(payload, PUBLIC_INPUTS_KEYS).ok_or("missing publicInputs field")?;
    public_inputs_from_value(contract_type, function, pi, payload)
}

fn public_inputs_from_value(
    contract_type: ContractType,
    function: &str,
    value: &Value,
    payload: &Value,
) -> Result<Vec<Vec<u8>>, String> {
    match value {
        Value::Array(items) => items
            .iter()
            .enumerate()
            .map(|(i, item)| {
                let raw = item
                    .as_str()
                    .ok_or_else(|| format!("publicInputs[{i}] must be a hex or base64 string"))?;
                decode_wire_bytes(raw, &format!("publicInputs[{i}]"), Some(32))
            })
            .collect(),
        Value::String(raw) => {
            let trimmed = raw.trim();
            if trimmed.starts_with('[') {
                let parsed: Value = serde_json::from_str(trimmed)
                    .map_err(|e| format!("invalid publicInputs JSON array string: {e}"))?;
                return public_inputs_from_value(contract_type, function, &parsed, payload);
            }
            let bytes = decode_wire_bytes(trimmed, "publicInputs", None)?;
            if bytes.len() % 32 != 0 {
                return Err(format!(
                    "publicInputs byte string length must be a multiple of 32, got {}",
                    bytes.len()
                ));
            }
            Ok(bytes.chunks(32).map(|chunk| chunk.to_vec()).collect())
        }
        Value::Object(_) => {
            build_public_inputs_from_object(contract_type, function, value, payload)
        }
        _ => Err("publicInputs must be an array, object, or encoded byte string".to_string()),
    }
}

fn build_public_inputs_from_object(
    contract_type: ContractType,
    function: &str,
    pi: &Value,
    payload: &Value,
) -> Result<Vec<Vec<u8>>, String> {
    match function {
        "verify_membership" => membership_public_inputs(pi, payload),
        "create_group" => match contract_type {
            ContractType::OneOnOne | ContractType::Anarchy => {
                create_membership_public_inputs(pi, payload)
            }
            ContractType::Democracy => Ok(vec![
                bytes32_field_any(pi, payload, COMMITMENT_KEYS)?,
                scalar_field_any_or_default(pi, payload, EPOCH_KEYS, 0)?,
                bytes32_field_any(pi, payload, OCCUPANCY_COMMITMENT_INITIAL_KEYS)?,
            ]),
            ContractType::Tyranny => Ok(vec![
                bytes32_field_any(pi, payload, COMMITMENT_KEYS)?,
                scalar_field_any_or_default(pi, payload, EPOCH_KEYS, 0)?,
                bytes32_field_any(pi, payload, ADMIN_PUBKEY_COMMITMENT_KEYS)?,
                bytes32_field_any(pi, payload, GROUP_ID_FR_KEYS)
                    .or_else(|_| bytes32_field_any(payload, payload, GROUP_ID_KEYS))?,
            ]),
            ContractType::Oligarchy => Err("oligarchy uses create_oligarchy_group".to_string()),
        },
        "create_oligarchy_group" => Ok(vec![
            bytes32_field_any(pi, payload, COMMITMENT_KEYS)?,
            scalar_field_any_or_default(pi, payload, EPOCH_KEYS, 0)?,
            bytes32_field_any(pi, payload, OCCUPANCY_COMMITMENT_INITIAL_KEYS)?,
            bytes32_field_any(pi, payload, MEMBER_ROOT_KEYS)?,
            bytes32_field_any(pi, payload, ADMIN_ROOT_KEYS)?,
            bytes32_field_any(pi, payload, SALT_INITIAL_KEYS)?,
        ]),
        "update_commitment" => update_public_inputs(contract_type, pi, payload),
        _ => Err(format!(
            "unsupported public inputs for function: {function}"
        )),
    }
}

fn create_membership_public_inputs(pi: &Value, payload: &Value) -> Result<Vec<Vec<u8>>, String> {
    Ok(vec![
        bytes32_field_any(pi, payload, COMMITMENT_KEYS)?,
        scalar_field_any_or_default(pi, payload, EPOCH_KEYS, 0)?,
    ])
}

fn membership_public_inputs(pi: &Value, payload: &Value) -> Result<Vec<Vec<u8>>, String> {
    Ok(vec![
        bytes32_field_any(pi, payload, COMMITMENT_KEYS)?,
        scalar_field_any(pi, payload, EPOCH_KEYS)?,
    ])
}

fn update_public_inputs(
    contract_type: ContractType,
    pi: &Value,
    payload: &Value,
) -> Result<Vec<Vec<u8>>, String> {
    let mut public_inputs = vec![
        bytes32_field_any(pi, payload, C_OLD_KEYS)?,
        scalar_field_any(pi, payload, EPOCH_OLD_KEYS)?,
        bytes32_field_any(pi, payload, C_NEW_KEYS)?,
    ];

    match contract_type {
        ContractType::Anarchy => {}
        ContractType::Democracy => {
            public_inputs.push(bytes32_field_any(
                pi,
                payload,
                OCCUPANCY_COMMITMENT_OLD_KEYS,
            )?);
            public_inputs.push(bytes32_field_any(
                pi,
                payload,
                OCCUPANCY_COMMITMENT_NEW_KEYS,
            )?);
            public_inputs.push(scalar_field_any(pi, payload, THRESHOLD_NUMERATOR_KEYS)?);
        }
        ContractType::Oligarchy => {
            public_inputs.push(bytes32_field_any(
                pi,
                payload,
                OCCUPANCY_COMMITMENT_OLD_KEYS,
            )?);
            public_inputs.push(bytes32_field_any(
                pi,
                payload,
                OCCUPANCY_COMMITMENT_NEW_KEYS,
            )?);
            public_inputs.push(scalar_field_any(
                pi,
                payload,
                ADMIN_THRESHOLD_NUMERATOR_KEYS,
            )?);
        }
        ContractType::Tyranny => {
            public_inputs.push(bytes32_field_any(
                pi,
                payload,
                ADMIN_PUBKEY_COMMITMENT_KEYS,
            )?);
            public_inputs.push(
                bytes32_field_any(pi, payload, GROUP_ID_FR_KEYS)
                    .or_else(|_| bytes32_field_any(payload, payload, GROUP_ID_KEYS))?,
            );
        }
        ContractType::OneOnOne => {
            return Err("oneonone contract does not support update_commitment".to_string());
        }
    }

    Ok(public_inputs)
}

fn expected_public_input_count(
    contract_type: ContractType,
    function: &str,
) -> Result<usize, String> {
    match function {
        "verify_membership" => Ok(2),
        "create_group" => match contract_type {
            ContractType::OneOnOne | ContractType::Anarchy => Ok(2),
            ContractType::Democracy => Ok(3),
            ContractType::Tyranny => Ok(4),
            ContractType::Oligarchy => Err("oligarchy uses create_oligarchy_group".to_string()),
        },
        "create_oligarchy_group" => Ok(6),
        "update_commitment" => match contract_type {
            ContractType::Anarchy => Ok(3),
            ContractType::Democracy | ContractType::Oligarchy => Ok(6),
            ContractType::Tyranny => Ok(5),
            ContractType::OneOnOne => {
                Err("oneonone contract does not support update_commitment".to_string())
            }
        },
        _ => Err(format!(
            "unsupported public inputs for function: {function}"
        )),
    }
}

fn bytes32_field_any(primary: &Value, secondary: &Value, keys: &[&str]) -> Result<Vec<u8>, String> {
    let value = field_value(primary, keys)
        .or_else(|| field_value(secondary, keys))
        .ok_or_else(|| format!("missing field: {}", keys.join("/")))?;
    let raw = value
        .as_str()
        .ok_or_else(|| format!("field must be a hex or base64 string: {}", keys.join("/")))?;
    decode_wire_bytes(raw, &keys.join("/"), Some(32))
}

fn scalar_field_any(primary: &Value, secondary: &Value, keys: &[&str]) -> Result<Vec<u8>, String> {
    let value = field_value(primary, keys)
        .or_else(|| field_value(secondary, keys))
        .ok_or_else(|| format!("missing field: {}", keys.join("/")))?;
    scalar_value(value, &keys.join("/"))
}

fn scalar_field_any_or_default(
    primary: &Value,
    secondary: &Value,
    keys: &[&str],
    default: u64,
) -> Result<Vec<u8>, String> {
    match field_value(primary, keys).or_else(|| field_value(secondary, keys)) {
        Some(value) => scalar_value(value, &keys.join("/")),
        None => Ok(u64_be32(default).to_vec()),
    }
}

fn scalar_value(value: &Value, field: &str) -> Result<Vec<u8>, String> {
    if let Some(value) = value.as_u64() {
        return Ok(u64_be32(value).to_vec());
    }
    if let Some(raw) = value.as_str() {
        return decode_wire_bytes(raw, field, Some(32));
    }
    Err(format!(
        "{field} must be a u64 or hex/base64 BytesN<32> string"
    ))
}

fn decode_wire_bytes(
    raw: &str,
    field: &str,
    expected_len: Option<usize>,
) -> Result<Vec<u8>, String> {
    let trimmed = raw.trim();
    let hex = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
        .unwrap_or(trimmed);
    let looks_like_hex = match expected_len {
        Some(expected_len) => {
            hex.len() == expected_len * 2 && hex.bytes().all(|b| b.is_ascii_hexdigit())
        }
        None => !hex.is_empty() && hex.len() % 2 == 0 && hex.bytes().all(|b| b.is_ascii_hexdigit()),
    };

    let bytes = if looks_like_hex {
        hex::decode(hex).map_err(|e| format!("invalid hex for {field}: {e}"))?
    } else {
        base64::engine::general_purpose::STANDARD
            .decode(trimmed)
            .map_err(|e| format!("invalid base64 for {field}: {e}"))?
    };

    if let Some(expected_len) = expected_len {
        if bytes.len() != expected_len {
            return Err(format!(
                "{field} must be {expected_len} bytes, got {}",
                bytes.len()
            ));
        }
    }
    Ok(bytes)
}

fn u64_be32(value: u64) -> [u8; 32] {
    let mut bytes = [0u8; 32];
    bytes[24..32].copy_from_slice(&value.to_be_bytes());
    bytes
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(nibble_to_hex(byte >> 4));
        out.push(nibble_to_hex(byte & 0x0f));
    }
    out
}

fn nibble_to_hex(nibble: u8) -> char {
    match nibble {
        0..=9 => (b'0' + nibble) as char,
        10..=15 => (b'a' + (nibble - 10)) as char,
        _ => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b64(bytes: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    fn b64_32(fill: u8) -> String {
        b64(&[fill; 32])
    }

    fn test_config() -> Config {
        Config {
            secret_key: String::new(),
            public_address: "GRELAYER".to_string(),
            contract_ids: std::collections::HashMap::new(),
            rpc_url: String::new(),
            network_passphrase: String::new(),
            network: String::new(),
            bind_address: String::new(),
            auth_tokens: std::collections::HashSet::new(),
            rate_limit_per_minute: 30,
            max_payload_size: 8192,
            identity_name: String::new(),
        }
    }

    // ---- hex_encode tests ----

    #[test]
    fn test_hex_encode_empty() {
        assert_eq!(hex_encode(&[]), "");
    }

    #[test]
    fn test_hex_encode_bytes() {
        assert_eq!(hex_encode(&[0x01, 0x23, 0xab, 0xcd]), "0123abcd");
    }

    #[test]
    fn test_hex_encode_all_ff() {
        assert_eq!(hex_encode(&[0xff, 0xff, 0xff]), "ffffff");
    }

    // ---- parse_bool_output tests ----

    #[test]
    fn test_parse_bool_output_true() {
        assert_eq!(parse_bool_output("true").unwrap(), true);
    }

    #[test]
    fn test_parse_bool_output_false() {
        assert_eq!(parse_bool_output("false").unwrap(), false);
    }

    #[test]
    fn test_parse_bool_output_case_insensitive() {
        assert_eq!(parse_bool_output("TRUE").unwrap(), true);
        assert_eq!(parse_bool_output("False").unwrap(), false);
        assert_eq!(parse_bool_output("TrUe").unwrap(), true);
    }

    #[test]
    fn test_parse_bool_output_with_whitespace() {
        assert_eq!(parse_bool_output("  true  \n").unwrap(), true);
        assert_eq!(parse_bool_output("\tfalse\t").unwrap(), false);
    }

    #[test]
    fn test_parse_bool_output_invalid() {
        assert!(parse_bool_output("yes").is_err());
        assert!(parse_bool_output("1").is_err());
        assert!(parse_bool_output("").is_err());
    }

    // ---- hex_to_base64_if_needed tests ----

    #[test]
    fn test_hex_to_base64_valid() {
        // 0xdeadbeef -> 3q2+7w==
        let result = hex_to_base64_if_needed("deadbeef").unwrap();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&result)
            .unwrap();
        assert_eq!(decoded, vec![0xde, 0xad, 0xbe, 0xef]);
    }

    #[test]
    fn test_hex_to_base64_accepts_0x_prefix() {
        let result = hex_to_base64_if_needed("0xdeadbeef").unwrap();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&result)
            .unwrap();
        assert_eq!(decoded, vec![0xde, 0xad, 0xbe, 0xef]);
    }

    #[test]
    fn test_hex_to_base64_odd_length_rejected() {
        // Odd-length strings are not valid hex, so the function returns the input unchanged
        let result = hex_to_base64_if_needed("abc").unwrap();
        assert_eq!(result, "abc");
    }

    // ---- normalize_bytes_fields tests ----

    #[test]
    fn test_normalize_commitment_flat_object() {
        let mut value = serde_json::json!({
            "commitment": "deadbeef",
            "epoch": 1
        });
        normalize_bytes_fields(&mut value).unwrap();

        // commitment should be base64 now
        let commitment = value["commitment"].as_str().unwrap();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(commitment)
            .unwrap();
        assert_eq!(decoded, vec![0xde, 0xad, 0xbe, 0xef]);

        // epoch should be unchanged
        assert_eq!(value["epoch"], 1);
    }

    #[test]
    fn test_normalize_commitment_nested_array() {
        let mut value = serde_json::json!([
            { "commitment": "0011ff", "name": "group1" },
            { "commitment": "aabb", "name": "group2" }
        ]);
        normalize_bytes_fields(&mut value).unwrap();

        // Both commitments should be converted
        let c0 = value[0]["commitment"].as_str().unwrap();
        let d0 = base64::engine::general_purpose::STANDARD
            .decode(c0)
            .unwrap();
        assert_eq!(d0, vec![0x00, 0x11, 0xff]);

        let c1 = value[1]["commitment"].as_str().unwrap();
        let d1 = base64::engine::general_purpose::STANDARD
            .decode(c1)
            .unwrap();
        assert_eq!(d1, vec![0xaa, 0xbb]);

        // Other fields should be untouched
        assert_eq!(value[0]["name"], "group1");
        assert_eq!(value[1]["name"], "group2");
    }

    // ---- field_value / dual-key lookup tests (#84 review) ----

    #[test]
    fn test_field_value_snake_case_wins_over_camelcase() {
        // Invariant relied on by every handler: when a payload
        // contains *both* snake_case and camelCase variants with
        // different values, the snake_case variant always wins because
        // the canonical key-order constants list it first.
        let payload = serde_json::json!({
            "group_id": "snake-wins",
            "groupID": "camel-loses"
        });
        assert_eq!(
            field_value(&payload, GROUP_ID_KEYS).and_then(|v| v.as_str()),
            Some("snake-wins")
        );
    }

    #[test]
    fn test_field_value_camelcase_fallback_when_only_camel_key_present() {
        let payload = serde_json::json!({ "groupID": "camel-only" });
        assert_eq!(
            field_value(&payload, GROUP_ID_KEYS).and_then(|v| v.as_str()),
            Some("camel-only")
        );
    }

    #[test]
    fn test_field_value_returns_none_when_no_key_matches() {
        let payload = serde_json::json!({ "unrelated": 1 });
        assert!(field_value(&payload, GROUP_ID_KEYS).is_none());
    }

    #[test]
    fn test_canonical_key_orders_are_snake_first() {
        // Locks the snake_case-first invariant across every field that
        // has a dual spelling. Adding a new dual-spelled field without
        // the same ordering should fail this test.
        for keys in [
            GROUP_ID_KEYS,
            GROUP_TYPE_KEYS,
            MEMBER_COUNT_KEYS,
            ADMIN_ROOT_KEYS,
            PUBLIC_INPUTS_KEYS,
            MAX_ENTRIES_KEYS,
        ] {
            assert_eq!(keys.len(), 2, "dual-key list must have exactly two entries");
            assert!(
                keys[0].contains('_') || keys[0].chars().all(|c| c.is_lowercase()),
                "first key must be snake_case or all-lowercase: {}",
                keys[0]
            );
            assert!(
                keys[1].chars().any(|c| c.is_uppercase()),
                "second key must be camelCase: {}",
                keys[1]
            );
        }
    }

    #[test]
    fn test_add_hex_arg_accepts_snake_case_payload() {
        let payload = serde_json::json!({ "group_id": b64_32(0) });
        let mut cmd = Command::new("true");
        add_hex_arg(&mut cmd, "--group-id", &payload, GROUP_ID_KEYS)
            .expect("snake_case payload must decode");
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        assert_eq!(args, vec!["--group-id".to_string(), "00".repeat(32)]);
    }

    #[test]
    fn test_add_hex_arg_accepts_camel_case_payload() {
        let payload = serde_json::json!({ "groupID": b64_32(0) });
        let mut cmd = Command::new("true");
        add_hex_arg(&mut cmd, "--group-id", &payload, GROUP_ID_KEYS)
            .expect("camelCase payload must decode");
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        assert_eq!(args, vec!["--group-id".to_string(), "00".repeat(32)]);
    }

    #[test]
    fn test_add_hex_arg_prefers_snake_case_when_both_present() {
        // Different base64 payloads under each key — verify we decode
        // the snake_case one.
        let mut snake_bytes = [0u8; 32];
        snake_bytes[0..3].copy_from_slice(&[0x01, 0x02, 0x03]);
        let payload = serde_json::json!({
            "group_id": b64(&snake_bytes),
            "groupID":  b64_32(0)
        });
        let mut cmd = Command::new("true");
        add_hex_arg(&mut cmd, "--group-id", &payload, GROUP_ID_KEYS)
            .expect("conflicting-keys payload must decode the snake_case variant");
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        assert_eq!(
            args,
            vec!["--group-id".to_string(), hex_encode(&snake_bytes)]
        );
    }

    #[test]
    fn test_add_int_arg_accepts_both_spellings() {
        let snake = serde_json::json!({ "member_count": 7 });
        let camel = serde_json::json!({ "memberCount": 7 });

        for payload in [&snake, &camel] {
            let mut cmd = Command::new("true");
            add_int_arg(&mut cmd, "--member-count", payload, MEMBER_COUNT_KEYS)
                .expect("both key spellings must parse");
            let args: Vec<_> = cmd
                .get_args()
                .map(|a| a.to_string_lossy().to_string())
                .collect();
            assert_eq!(args, vec!["--member-count", "7"]);
        }
    }

    #[test]
    fn test_add_proof_arg_uses_plonk_bytesn_shape() {
        let proof = vec![0xabu8; 1601];
        let payload = serde_json::json!({ "proof": b64(&proof) });
        let mut cmd = Command::new("true");

        add_proof_arg(&mut cmd, &payload).expect("1601-byte proof must decode");

        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        assert_eq!(args, vec!["--proof".to_string(), hex_encode(&proof)]);
    }

    #[test]
    fn test_democracy_create_public_inputs_object_builds_three_pi_vector() {
        let payload = serde_json::json!({
            "publicInputs": {
                "commitment": b64_32(1),
                "epoch": 0,
                "occupancy_commitment_initial": b64_32(2)
            }
        });
        let mut cmd = Command::new("true");

        add_public_inputs_arg(ContractType::Democracy, "create_group", &mut cmd, &payload)
            .expect("democracy create PI object must encode");

        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        assert_eq!(args[0], "--public-inputs");
        let pi: Vec<String> = serde_json::from_str(&args[1]).unwrap();
        assert_eq!(
            pi,
            vec![
                hex_encode(&[1u8; 32]),
                hex_encode(&u64_be32(0)),
                hex_encode(&[2u8; 32])
            ]
        );
    }

    #[test]
    fn test_oligarchy_create_public_inputs_object_builds_six_pi_vector() {
        let payload = serde_json::json!({
            "publicInputs": {
                "commitment": b64_32(1),
                "epoch": 0,
                "occupancy_commitment_initial": b64_32(2),
                "member_root": b64_32(3),
                "admin_root": b64_32(4),
                "salt_initial": b64_32(5)
            }
        });
        let public_inputs =
            public_inputs_bytes(ContractType::Oligarchy, "create_oligarchy_group", &payload)
                .expect("oligarchy create PI object must encode");

        assert_eq!(
            public_inputs,
            vec![
                vec![1u8; 32],
                u64_be32(0).to_vec(),
                vec![2u8; 32],
                vec![3u8; 32],
                vec![4u8; 32],
                vec![5u8; 32]
            ]
        );
    }

    #[test]
    fn test_create_oligarchy_group_args_match_current_contract_signature() {
        let payload = serde_json::json!({
            "group_id": b64_32(1),
            "commitment": b64_32(2),
            "member_tier": 0,
            "admin_threshold_numerator": 1,
            "occupancy_commitment_initial": b64_32(3),
            "proof": b64(&[0xabu8; 1601]),
            "publicInputs": {
                "commitment": b64_32(2),
                "epoch": 0,
                "occupancy_commitment_initial": b64_32(3),
                "member_root": b64_32(4),
                "admin_root": b64_32(5),
                "salt_initial": b64_32(6)
            }
        });
        let config = test_config();
        let mut cmd = Command::new("true");

        add_create_oligarchy_group_args(&config, &mut cmd, &payload)
            .expect("oligarchy create args must encode");

        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        assert!(args.contains(&"--proof".to_string()));
        assert!(args.contains(&"--public-inputs".to_string()));
        assert!(!args.contains(&"--member-root".to_string()));
        assert!(!args.contains(&"--admin-root".to_string()));
        assert!(!args.contains(&"--salt-initial".to_string()));
    }

    #[test]
    fn test_democracy_update_public_inputs_object_builds_threshold_pi() {
        let payload = serde_json::json!({
            "publicInputs": {
                "c_old": b64_32(1),
                "epoch_old": 1234,
                "c_new": b64_32(2),
                "occupancy_commitment_old": b64_32(3),
                "occupancy_commitment_new": b64_32(4),
                "threshold_numerator": 2
            }
        });
        let public_inputs =
            public_inputs_bytes(ContractType::Democracy, "update_commitment", &payload)
                .expect("democracy update PI object must encode");

        assert_eq!(
            public_inputs,
            vec![
                vec![1u8; 32],
                u64_be32(1234).to_vec(),
                vec![2u8; 32],
                vec![3u8; 32],
                vec![4u8; 32],
                u64_be32(2).to_vec()
            ]
        );
    }

    #[test]
    fn test_tyranny_create_public_inputs_default_group_id_fr_to_group_id() {
        let payload = serde_json::json!({
            "group_id": b64_32(9),
            "publicInputs": {
                "commitment": b64_32(1),
                "epoch": 0,
                "admin_pubkey_commitment": b64_32(7)
            }
        });
        let public_inputs = public_inputs_bytes(ContractType::Tyranny, "create_group", &payload)
            .expect("tyranny create PI object must encode");

        assert_eq!(
            public_inputs,
            vec![
                vec![1u8; 32],
                u64_be32(0).to_vec(),
                vec![7u8; 32],
                vec![9u8; 32]
            ]
        );
    }

    // ---- per-type entrypoint payload shape tests ----
    //
    // Full invocation requires an actual stellar CLI and a deployed contract,
    // which belongs in an environment-specific smoke test. Unit tests here
    // cover the payload-shape contract the relayer enforces before shelling out.

    #[test]
    fn test_democracy_create_group_payload_resolves_required_fields() {
        // Mixed-spelling payload: some fields snake_case, some camelCase.
        // Every one must still resolve under the canonical order.
        let payload = serde_json::json!({
            "group_id": "AAAA",
            "commitment": "AAAA",
            "tier": 0,
            "thresholdNumerator": 50,
            "occupancy_commitment_initial": "AAAA",
            "publicInputs": {
                "commitment": "AAAA",
                "epoch": 0
            }
        });

        assert!(field_value(&payload, GROUP_ID_KEYS).is_some(), "group_id");
        assert!(
            field_value(&payload, THRESHOLD_NUMERATOR_KEYS).is_some(),
            "threshold_numerator"
        );
        assert!(
            field_value(&payload, OCCUPANCY_COMMITMENT_INITIAL_KEYS).is_some(),
            "occupancy_commitment_initial"
        );
        assert!(
            field_value(&payload, PUBLIC_INPUTS_KEYS).is_some(),
            "public_inputs"
        );
        assert!(payload.get("commitment").is_some(), "commitment");
        assert!(payload.get("tier").is_some(), "tier");
    }

    #[test]
    fn test_create_oligarchy_group_payload_resolves_admin_root() {
        // admin_root is unique to create_oligarchy_group; verify the
        // dual-key lookup works for both spellings.
        let snake = serde_json::json!({ "admin_root": "AAAA" });
        let camel = serde_json::json!({ "adminRoot":  "AAAA" });

        assert!(field_value(&snake, ADMIN_ROOT_KEYS).is_some());
        assert!(field_value(&camel, ADMIN_ROOT_KEYS).is_some());
    }

    #[test]
    fn test_get_commitment_payload_shape() {
        let payload = serde_json::json!({ "group_id": "AAAA" });
        assert!(field_value(&payload, GROUP_ID_KEYS).is_some());

        // Missing group_id must surface as None (the handler turns it
        // into a `missing field` error).
        let empty = serde_json::json!({});
        assert!(field_value(&empty, GROUP_ID_KEYS).is_none());
    }

    #[test]
    fn test_get_history_max_entries_dual_spelling() {
        // `get_history` takes an optional `max_entries` / `maxEntries`;
        // both spellings must resolve, and missing defaults elsewhere.
        let snake = serde_json::json!({ "max_entries": 10 });
        let camel = serde_json::json!({ "maxEntries":  10 });
        let neither = serde_json::json!({});

        assert_eq!(
            field_value(&snake, MAX_ENTRIES_KEYS).and_then(|v| v.as_u64()),
            Some(10)
        );
        assert_eq!(
            field_value(&camel, MAX_ENTRIES_KEYS).and_then(|v| v.as_u64()),
            Some(10)
        );
        assert!(field_value(&neither, MAX_ENTRIES_KEYS).is_none());
    }
}
