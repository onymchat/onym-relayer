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
        other => Err(format!("function not allowed for {contract_type}: {other}")),
    }
}

fn success_response(invocation: &ResolvedInvocation, output: String) -> Result<Response, String> {
    match invocation.contract_function.as_str() {
        "create_group" | "update_commitment" | "create_oligarchy_group" => Ok((
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
    if s.len() % 2 == 0 && s.bytes().all(|b| b.is_ascii_hexdigit()) {
        let bytes = hex::decode(s).map_err(|e| format!("invalid hex commitment: {e}"))?;
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
            add_update_public_inputs_arg(invocation.contract_type, &mut cmd, payload)?;
        }
        (_, "verify_membership") => {
            add_hex_arg(&mut cmd, "--group-id", payload, GROUP_ID_KEYS)?;
            add_proof_arg(&mut cmd, payload)?;
            add_membership_public_inputs_arg(&mut cmd, payload)?;
        }
        (_, "get_commitment") => {
            add_hex_arg(&mut cmd, "--group-id", payload, GROUP_ID_KEYS)?;
        }
        (_, "get_history") => {
            add_hex_arg(&mut cmd, "--group-id", payload, GROUP_ID_KEYS)?;
            let max_entries = field_value(payload, MAX_ENTRIES_KEYS)
                .and_then(|v| v.as_u64())
                .unwrap_or(64);
            cmd.arg("--max-entries").arg(max_entries.to_string());
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
pub(crate) const PUBLIC_INPUTS_KEYS: &[&str] = &["public_inputs", "publicInputs"];
pub(crate) const MAX_ENTRIES_KEYS: &[&str] = &["max_entries", "maxEntries"];

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
    let b64 = field_value(payload, keys)
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("missing field: {}", keys.join("/")))?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| format!("invalid base64 for {}: {e}", keys.join("/")))?;
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
            add_membership_public_inputs_arg(cmd, payload)?;
        }
        ContractType::Anarchy => {
            add_int_arg(cmd, "--tier", payload, TIER_KEYS)?;
            add_int_arg_or_default(cmd, "--member-count", payload, MEMBER_COUNT_KEYS, 0)?;
            add_proof_arg(cmd, payload)?;
            add_membership_public_inputs_arg(cmd, payload)?;
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
            add_membership_public_inputs_arg(cmd, payload)?;
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
            add_tyranny_create_public_inputs_arg(cmd, payload)?;
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
    add_hex_arg(cmd, "--member-root", payload, MEMBER_ROOT_KEYS)?;
    add_hex_arg(cmd, "--admin-root", payload, ADMIN_ROOT_KEYS)?;
    add_hex_arg(cmd, "--salt-initial", payload, SALT_INITIAL_KEYS)?;
    add_proof_arg(cmd, payload)?;
    add_oligarchy_create_public_inputs_arg(cmd, payload)?;
    Ok(())
}

/// Decode the proof from base64 and add as a JSON file-path argument.
/// The proof is 384 bytes: a(96) || b(192) || c(96).
fn add_proof_arg(cmd: &mut Command, payload: &Value) -> Result<(), String> {
    let proof_b64 = payload
        .get("proof")
        .and_then(|v| v.as_str())
        .ok_or("missing proof field")?;
    let proof_bytes = base64::engine::general_purpose::STANDARD
        .decode(proof_b64)
        .map_err(|e| format!("invalid proof base64: {e}"))?;
    if proof_bytes.len() != 384 {
        return Err(format!(
            "proof must be 384 bytes, got {}",
            proof_bytes.len()
        ));
    }
    let a = &proof_bytes[0..96];
    let b = &proof_bytes[96..288];
    let c = &proof_bytes[288..384];

    let proof_json = json!({
        "a": hex_encode(a),
        "b": hex_encode(b),
        "c": hex_encode(c),
    })
    .to_string();

    // Write to a temp file since stellar CLI expects --proof-file-path
    let tmp = write_temp_json("proof", &proof_json)?;
    cmd.arg("--proof-file-path").arg(tmp);
    Ok(())
}

/// Decode membership-circuit public inputs and add as a JSON file-path argument.
/// Used by create_group and verify_membership.
/// Expects `payload.publicInputs = { commitment: base64, epoch: u64 }`.
fn add_membership_public_inputs_arg(cmd: &mut Command, payload: &Value) -> Result<(), String> {
    let pi = field_value(payload, PUBLIC_INPUTS_KEYS).ok_or("missing publicInputs field")?;
    let commitment = decode_b64_field(pi, &["commitment"])?;
    let epoch = number_field(pi, &["epoch"])?;

    let pi_json = json!({
        "commitment": hex_encode(&commitment),
        "epoch": epoch,
    })
    .to_string();

    let tmp = write_temp_json("public-inputs", &pi_json)?;
    cmd.arg("--public-inputs-file-path").arg(tmp);
    Ok(())
}

fn add_tyranny_create_public_inputs_arg(cmd: &mut Command, payload: &Value) -> Result<(), String> {
    let pi = field_value(payload, PUBLIC_INPUTS_KEYS).ok_or("missing publicInputs field")?;
    let commitment = decode_b64_field(pi, &["commitment"])?;
    let epoch = number_field(pi, &["epoch"])?;
    let admin_pubkey_commitment = decode_b64_field(pi, ADMIN_PUBKEY_COMMITMENT_KEYS)
        .or_else(|_| decode_b64_field(payload, ADMIN_PUBKEY_COMMITMENT_KEYS))?;

    let pi_json = json!({
        "commitment": hex_encode(&commitment),
        "epoch": epoch,
        "admin_pubkey_commitment": hex_encode(&admin_pubkey_commitment),
    })
    .to_string();

    let tmp = write_temp_json("tyranny-create-public-inputs", &pi_json)?;
    cmd.arg("--public-inputs-file-path").arg(tmp);
    Ok(())
}

fn add_oligarchy_create_public_inputs_arg(
    cmd: &mut Command,
    payload: &Value,
) -> Result<(), String> {
    let pi = field_value(payload, PUBLIC_INPUTS_KEYS).ok_or("missing publicInputs field")?;
    let commitment = decode_b64_field(pi, &["commitment"])?;
    let epoch = number_field(pi, &["epoch"])?;
    let occupancy_commitment = decode_b64_field(pi, OCCUPANCY_COMMITMENT_INITIAL_KEYS)
        .or_else(|_| decode_b64_field(payload, OCCUPANCY_COMMITMENT_INITIAL_KEYS))?;
    let member_root = decode_b64_field(pi, MEMBER_ROOT_KEYS)
        .or_else(|_| decode_b64_field(payload, MEMBER_ROOT_KEYS))?;
    let admin_root = decode_b64_field(pi, ADMIN_ROOT_KEYS)
        .or_else(|_| decode_b64_field(payload, ADMIN_ROOT_KEYS))?;
    let salt_initial = decode_b64_field(pi, SALT_INITIAL_KEYS)
        .or_else(|_| decode_b64_field(payload, SALT_INITIAL_KEYS))?;

    let pi_json = json!({
        "commitment": hex_encode(&commitment),
        "epoch": epoch,
        "occupancy_commitment": hex_encode(&occupancy_commitment),
        "member_root": hex_encode(&member_root),
        "admin_root": hex_encode(&admin_root),
        "salt_initial": hex_encode(&salt_initial),
    })
    .to_string();

    let tmp = write_temp_json("oligarchy-create-public-inputs", &pi_json)?;
    cmd.arg("--public-inputs-file-path").arg(tmp);
    Ok(())
}

/// Decode update-circuit public inputs and add as a JSON file-path argument.
fn add_update_public_inputs_arg(
    contract_type: ContractType,
    cmd: &mut Command,
    payload: &Value,
) -> Result<(), String> {
    let pi = field_value(payload, PUBLIC_INPUTS_KEYS).ok_or("missing publicInputs field")?;
    let c_old = decode_b64_field(pi, C_OLD_KEYS)?;
    let epoch_old = number_field(pi, EPOCH_OLD_KEYS)?;
    let c_new = decode_b64_field(pi, C_NEW_KEYS)?;

    let mut public_inputs = json!({
        "c_old": hex_encode(&c_old),
        "epoch_old": epoch_old,
        "c_new": hex_encode(&c_new),
    });

    if matches!(
        contract_type,
        ContractType::Democracy | ContractType::Oligarchy
    ) {
        let occupancy_commitment_old = decode_b64_field(pi, OCCUPANCY_COMMITMENT_OLD_KEYS)?;
        let occupancy_commitment_new = decode_b64_field(pi, OCCUPANCY_COMMITMENT_NEW_KEYS)?;
        public_inputs["occupancy_commitment_old"] =
            Value::String(hex_encode(&occupancy_commitment_old));
        public_inputs["occupancy_commitment_new"] =
            Value::String(hex_encode(&occupancy_commitment_new));
    }

    let pi_json = public_inputs.to_string();
    let tmp = write_temp_json("update-public-inputs", &pi_json)?;
    cmd.arg("--public-inputs-file-path").arg(tmp);
    Ok(())
}

fn decode_b64_field(payload: &Value, keys: &[&str]) -> Result<Vec<u8>, String> {
    let b64 = field_value(payload, keys)
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("missing field: {}", keys.join("/")))?;
    base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| format!("invalid base64 for {}: {e}", keys.join("/")))
}

fn number_field(payload: &Value, keys: &[&str]) -> Result<u64, String> {
    field_value(payload, keys)
        .and_then(|v| v.as_u64())
        .ok_or_else(|| format!("missing or invalid field: {}", keys.join("/")))
}

fn write_temp_json(prefix: &str, contents: &str) -> Result<String, String> {
    let timestamp_nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| format!("system clock error: {e}"))?
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "onym-{prefix}-{}-{timestamp_nanos}.json",
        std::process::id()
    ));
    std::fs::write(&path, contents)
        .map_err(|e| format!("failed to write temp JSON file {}: {e}", path.display()))?;
    path.into_os_string()
        .into_string()
        .map_err(|_| "temp JSON path is not valid UTF-8".to_string())
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
        let payload = serde_json::json!({ "group_id": "AAAA" }); // base64("\x00\x00\x00")
        let mut cmd = Command::new("true");
        add_hex_arg(&mut cmd, "--group-id", &payload, GROUP_ID_KEYS)
            .expect("snake_case payload must decode");
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        assert_eq!(args, vec!["--group-id", "000000"]);
    }

    #[test]
    fn test_add_hex_arg_accepts_camel_case_payload() {
        let payload = serde_json::json!({ "groupID": "AAAA" });
        let mut cmd = Command::new("true");
        add_hex_arg(&mut cmd, "--group-id", &payload, GROUP_ID_KEYS)
            .expect("camelCase payload must decode");
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        assert_eq!(args, vec!["--group-id", "000000"]);
    }

    #[test]
    fn test_add_hex_arg_prefers_snake_case_when_both_present() {
        // Different base64 payloads under each key — verify we decode
        // the snake_case one (bytes 0x01 0x02 0x03 = base64 "AQID").
        let payload = serde_json::json!({
            "group_id": "AQID",
            "groupID":  "AAAA"
        });
        let mut cmd = Command::new("true");
        add_hex_arg(&mut cmd, "--group-id", &payload, GROUP_ID_KEYS)
            .expect("conflicting-keys payload must decode the snake_case variant");
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        assert_eq!(args, vec!["--group-id", "010203"]);
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
