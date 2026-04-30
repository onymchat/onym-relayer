use std::collections::{HashMap, HashSet};
use std::env;
use std::fmt;
use std::str::FromStr;

use serde::de::{self, Visitor};
use serde::Deserialize;

/// Governance-specific contract type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContractType {
    Anarchy,
    OneOnOne,
    Democracy,
    Oligarchy,
    Tyranny,
}

impl ContractType {
    pub const ALL: [ContractType; 5] = [
        ContractType::Anarchy,
        ContractType::OneOnOne,
        ContractType::Democracy,
        ContractType::Oligarchy,
        ContractType::Tyranny,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ContractType::Anarchy => "anarchy",
            ContractType::OneOnOne => "oneonone",
            ContractType::Democracy => "democracy",
            ContractType::Oligarchy => "oligarchy",
            ContractType::Tyranny => "tyranny",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            ContractType::Anarchy => "sep-anarchy",
            ContractType::OneOnOne => "sep-oneonone",
            ContractType::Democracy => "sep-democracy",
            ContractType::Oligarchy => "sep-oligarchy",
            ContractType::Tyranny => "sep-tyranny",
        }
    }

    pub fn from_group_type_id(id: u32) -> Result<Self, String> {
        match id {
            0 => Ok(ContractType::Anarchy),
            1 => Ok(ContractType::OneOnOne),
            2 => Ok(ContractType::Democracy),
            3 => Ok(ContractType::Oligarchy),
            4 => Ok(ContractType::Tyranny),
            _ => Err(format!("unknown contract type id: {id}")),
        }
    }

    fn env_keys(self) -> &'static [&'static str] {
        match self {
            ContractType::Anarchy => {
                &["RELAYER_ANARCHY_CONTRACT_ID", "RELAYER_CONTRACT_ID_ANARCHY"]
            }
            ContractType::OneOnOne => &[
                "RELAYER_ONEONONE_CONTRACT_ID",
                "RELAYER_ONE_ON_ONE_CONTRACT_ID",
                "RELAYER_1V1_CONTRACT_ID",
                "RELAYER_CONTRACT_ID_ONEONONE",
                "RELAYER_CONTRACT_ID_1V1",
            ],
            ContractType::Democracy => &[
                "RELAYER_DEMOCRACY_CONTRACT_ID",
                "RELAYER_CONTRACT_ID_DEMOCRACY",
            ],
            ContractType::Oligarchy => &[
                "RELAYER_OLIGARCHY_CONTRACT_ID",
                "RELAYER_CONTRACT_ID_OLIGARCHY",
            ],
            ContractType::Tyranny => {
                &["RELAYER_TYRANNY_CONTRACT_ID", "RELAYER_CONTRACT_ID_TYRANNY"]
            }
        }
    }
}

impl fmt::Display for ContractType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

impl FromStr for ContractType {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let normalized = raw.trim().to_ascii_lowercase().replace(['-', '_', ' '], "");

        match normalized.as_str() {
            "0" | "anarchy" | "sepanarchy" => Ok(ContractType::Anarchy),
            "1" | "1v1" | "oneonone" | "onevone" | "sephoneonone" => Ok(ContractType::OneOnOne),
            "2" | "democracy" | "sepdemocracy" => Ok(ContractType::Democracy),
            "3" | "oligarchy" | "sepoligarchy" => Ok(ContractType::Oligarchy),
            "4" | "tyranny" | "septyranny" => Ok(ContractType::Tyranny),
            _ => Err(format!("unknown contract type: {raw}")),
        }
    }
}

impl<'de> Deserialize<'de> for ContractType {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct ContractTypeVisitor;

        impl Visitor<'_> for ContractTypeVisitor {
            type Value = ContractType;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a contract type string or numeric group type id")
            }

            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                let value = u32::try_from(value)
                    .map_err(|_| E::custom(format!("contract type id out of range: {value}")))?;
                ContractType::from_group_type_id(value).map_err(E::custom)
            }

            fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                let value = u32::try_from(value)
                    .map_err(|_| E::custom(format!("contract type id out of range: {value}")))?;
                ContractType::from_group_type_id(value).map_err(E::custom)
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                ContractType::from_str(value).map_err(E::custom)
            }
        }

        deserializer.deserialize_any(ContractTypeVisitor)
    }
}

/// Relayer configuration, loaded from environment variables.
pub struct Config {
    /// Stellar secret key (S...) for signing transactions.
    pub secret_key: String,
    /// Stellar public key (G...) derived from the secret key at startup.
    pub public_address: String,
    /// Whitelisted contract IDs by governance type.
    pub contract_ids: HashMap<ContractType, String>,
    /// Soroban RPC endpoint URL.
    pub rpc_url: String,
    /// Network passphrase used when invoking via explicit RPC URL.
    pub network_passphrase: String,
    /// Stellar network name (mainnet, testnet).
    pub network: String,
    /// HTTP bind address.
    pub bind_address: String,
    /// Valid bearer tokens. Empty = no auth required.
    pub auth_tokens: HashSet<String>,
    /// Rate limit: requests per minute per IP.
    pub rate_limit_per_minute: u32,
    /// Maximum request body size in bytes.
    pub max_payload_size: usize,
    /// Stellar CLI identity name (created from secret_key at startup).
    pub identity_name: String,
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        let secret_key = require_env("RELAYER_SECRET_KEY")?;
        let contract_ids = load_contract_ids()?;
        let rpc_url = env::var("RELAYER_RPC_URL")
            .unwrap_or_else(|_| "https://soroban.stellar.org".to_string());
        let network_passphrase = env::var("RELAYER_NETWORK_PASSPHRASE").unwrap_or_else(|_| {
            match env::var("RELAYER_NETWORK")
                .unwrap_or_else(|_| "mainnet".to_string())
                .as_str()
            {
                "testnet" => "Test SDF Network ; September 2015".to_string(),
                "futurenet" => "Test SDF Future Network ; October 2022".to_string(),
                "mainnet" => "Public Global Stellar Network ; September 2015".to_string(),
                _ => String::new(),
            }
        });
        let network = env::var("RELAYER_NETWORK").unwrap_or_else(|_| "mainnet".to_string());
        let bind_address = env::var("RELAYER_BIND").unwrap_or_else(|_| "0.0.0.0:8080".to_string());
        let auth_tokens: HashSet<String> = env::var("RELAYER_AUTH_TOKENS")
            .unwrap_or_default()
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        let rate_limit_per_minute: u32 = env::var("RELAYER_RATE_LIMIT")
            .unwrap_or_else(|_| "30".to_string())
            .parse()
            .map_err(|_| "RELAYER_RATE_LIMIT must be a number")?;
        let max_payload_size: usize = env::var("RELAYER_MAX_PAYLOAD_SIZE")
            .unwrap_or_else(|_| "8192".to_string())
            .parse()
            .map_err(|_| "RELAYER_MAX_PAYLOAD_SIZE must be a number")?;

        Ok(Config {
            secret_key,
            public_address: String::new(), // resolved at startup
            contract_ids,
            rpc_url,
            network_passphrase,
            network,
            bind_address,
            auth_tokens,
            rate_limit_per_minute,
            max_payload_size,
            identity_name: "onym-relayer".to_string(),
        })
    }

    pub fn auth_required(&self) -> bool {
        !self.auth_tokens.is_empty()
    }

    pub fn contract_id_for(&self, contract_type: ContractType) -> Option<&str> {
        self.contract_ids.get(&contract_type).map(String::as_str)
    }

    pub fn contract_type_for_id(&self, contract_id: &str) -> Option<ContractType> {
        self.contract_ids
            .iter()
            .find_map(|(contract_type, configured_id)| {
                (configured_id == contract_id).then_some(*contract_type)
            })
    }

    pub fn allowed_contracts(&self) -> Vec<(ContractType, &str)> {
        ContractType::ALL
            .into_iter()
            .filter_map(|contract_type| {
                self.contract_id_for(contract_type)
                    .map(|contract_id| (contract_type, contract_id))
            })
            .collect()
    }
}

fn require_env(key: &str) -> Result<String, String> {
    env::var(key).map_err(|_| format!("{key} environment variable is required"))
}

fn load_contract_ids() -> Result<HashMap<ContractType, String>, String> {
    let mut contract_ids = HashMap::new();

    for contract_type in ContractType::ALL {
        for key in contract_type.env_keys() {
            if let Ok(value) = env::var(key) {
                let value = value.trim();
                if !value.is_empty() {
                    insert_contract_id(&mut contract_ids, contract_type, value, key)?;
                    break;
                }
            }
        }
    }

    if let Ok(value) = env::var("RELAYER_CONTRACT_IDS") {
        parse_contract_ids_list(&value, &mut contract_ids)?;
    }

    if contract_ids.is_empty() {
        return Err(
            "at least one per-type contract ID is required (for example RELAYER_ANARCHY_CONTRACT_ID)"
                .to_string(),
        );
    }

    Ok(contract_ids)
}

fn parse_contract_ids_list(
    raw: &str,
    contract_ids: &mut HashMap<ContractType, String>,
) -> Result<(), String> {
    for entry in raw
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
    {
        let (left, right) = entry
            .split_once('=')
            .or_else(|| entry.split_once(':'))
            .ok_or_else(|| {
                format!(
                    "RELAYER_CONTRACT_IDS entry must be typed as type:id or type=id, got {entry}"
                )
            })?;

        let left_type = ContractType::from_str(left).ok();
        let right_type = ContractType::from_str(right).ok();
        let (contract_type, contract_id) = match (left_type, right_type) {
            (Some(contract_type), None) => (contract_type, right.trim()),
            (None, Some(contract_type)) => (contract_type, left.trim()),
            (Some(_), Some(_)) => {
                return Err(format!(
                "RELAYER_CONTRACT_IDS entry must include one type and one contract ID, got {entry}"
            ))
            }
            (None, None) => {
                return Err(format!(
                    "RELAYER_CONTRACT_IDS entry must include a known contract type, got {entry}"
                ))
            }
        };

        insert_contract_id(
            contract_ids,
            contract_type,
            contract_id,
            "RELAYER_CONTRACT_IDS",
        )?;
    }

    Ok(())
}

fn insert_contract_id(
    contract_ids: &mut HashMap<ContractType, String>,
    contract_type: ContractType,
    contract_id: &str,
    source: &str,
) -> Result<(), String> {
    if contract_id.is_empty() {
        return Err(format!(
            "{source} has an empty contract ID for {contract_type}"
        ));
    }

    if let Some(existing) = contract_ids.get(&contract_type) {
        if existing != contract_id {
            return Err(format!(
                "conflicting contract IDs for {contract_type}: {existing} and {contract_id}"
            ));
        }
        return Ok(());
    }

    if let Some((existing_type, _)) = contract_ids
        .iter()
        .find(|(_, existing_id)| existing_id.as_str() == contract_id)
    {
        return Err(format!(
            "{source} maps contract ID {contract_id} to both {existing_type} and {contract_type}"
        ));
    }

    contract_ids.insert(contract_type, contract_id.to_string());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_config(auth_tokens: HashSet<String>) -> Config {
        let mut contract_ids = HashMap::new();
        contract_ids.insert(
            ContractType::Anarchy,
            "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAD2KM".to_string(),
        );

        Config {
            secret_key: String::new(),
            public_address: String::new(),
            contract_ids,
            rpc_url: "https://soroban.stellar.org".to_string(),
            network_passphrase: "Test SDF Network ; September 2015".to_string(),
            network: "testnet".to_string(),
            bind_address: "0.0.0.0:8080".to_string(),
            auth_tokens,
            rate_limit_per_minute: 30,
            max_payload_size: 8192,
            identity_name: "onym-relayer".to_string(),
        }
    }

    #[test]
    fn test_auth_required_with_tokens() {
        let mut tokens = HashSet::new();
        tokens.insert("mytoken".to_string());
        let config = make_config(tokens);
        assert!(config.auth_required());
    }

    #[test]
    fn test_auth_not_required_without_tokens() {
        let config = make_config(HashSet::new());
        assert!(!config.auth_required());
    }
}
