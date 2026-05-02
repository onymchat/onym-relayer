use std::collections::{HashMap, HashSet};
use std::env;
use std::fmt;
use std::str::FromStr;

use serde::de::{self, Visitor};
use serde::Deserialize;
use serde_json::Value;

/// Stellar network selected by each relayer request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Network {
    Testnet,
    Public,
}

impl Network {
    pub const ALL: [Network; 2] = [Network::Testnet, Network::Public];

    pub fn label(self) -> &'static str {
        match self {
            Network::Testnet => "testnet",
            Network::Public => "public",
        }
    }

    pub fn cli_network(self) -> &'static str {
        match self {
            Network::Testnet => "testnet",
            Network::Public => "mainnet",
        }
    }

    fn default_rpc_url(self) -> &'static str {
        match self {
            Network::Testnet => "https://soroban-testnet.stellar.org",
            Network::Public => "https://soroban.stellar.org",
        }
    }

    fn default_passphrase(self) -> &'static str {
        match self {
            Network::Testnet => "Test SDF Network ; September 2015",
            Network::Public => "Public Global Stellar Network ; September 2015",
        }
    }

    fn rpc_url_env(self) -> &'static str {
        match self {
            Network::Testnet => "RELAYER_TESTNET_RPC_URL",
            Network::Public => "RELAYER_PUBLIC_RPC_URL",
        }
    }

    fn passphrase_env(self) -> &'static str {
        match self {
            Network::Testnet => "RELAYER_TESTNET_NETWORK_PASSPHRASE",
            Network::Public => "RELAYER_PUBLIC_NETWORK_PASSPHRASE",
        }
    }
}

impl fmt::Display for Network {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

impl FromStr for Network {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let normalized = raw.trim().to_ascii_lowercase().replace(['-', '_', ' '], "");
        match normalized.as_str() {
            "testnet" | "test" => Ok(Network::Testnet),
            "public" | "mainnet" | "pubnet" => Ok(Network::Public),
            _ => Err(format!("unknown network: {raw}")),
        }
    }
}

impl<'de> Deserialize<'de> for Network {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct NetworkVisitor;

        impl Visitor<'_> for NetworkVisitor {
            type Value = Network;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("testnet, public, or mainnet")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Network::from_str(value).map_err(E::custom)
            }
        }

        deserializer.deserialize_str(NetworkVisitor)
    }
}

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
            "1" | "1v1" | "oneonone" | "onevone" | "seponeonone" => Ok(ContractType::OneOnOne),
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

#[derive(Debug, Clone)]
pub struct NetworkConfig {
    pub rpc_url: String,
    pub network_passphrase: String,
    pub cli_network: String,
}

type ContractAllowlist = HashMap<Network, HashMap<ContractType, HashSet<String>>>;

/// Relayer configuration, loaded from environment variables.
pub struct Config {
    /// Stellar secret key (S...) for signing transactions.
    pub secret_key: String,
    /// Stellar public key (G...) derived from the secret key at startup.
    pub public_address: String,
    /// Whitelisted contract IDs by network and governance type.
    pub contract_allowlist: ContractAllowlist,
    /// Per-network Soroban RPC configuration.
    pub networks: HashMap<Network, NetworkConfig>,
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
        let contract_allowlist = load_contract_allowlist()?;
        let networks = load_network_configs()?;
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
            contract_allowlist,
            networks,
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

    pub fn network_config(&self, network: Network) -> &NetworkConfig {
        self.networks
            .get(&network)
            .expect("network configs are initialized for every supported network")
    }

    pub fn contract_allowed(
        &self,
        network: Network,
        contract_type: ContractType,
        contract_id: &str,
    ) -> bool {
        self.contract_allowlist
            .get(&network)
            .and_then(|contracts| contracts.get(&contract_type))
            .is_some_and(|contract_ids| contract_ids.contains(contract_id))
    }

    pub fn contract_type_for_id(
        &self,
        network: Network,
        contract_id: &str,
    ) -> Option<ContractType> {
        self.contract_allowlist
            .get(&network)?
            .iter()
            .find_map(|(contract_type, contract_ids)| {
                contract_ids.contains(contract_id).then_some(*contract_type)
            })
    }

    pub fn allowed_contracts(&self) -> Vec<(Network, ContractType, &str)> {
        let mut allowed = Vec::new();
        for network in Network::ALL {
            for contract_type in ContractType::ALL {
                if let Some(contract_ids) = self
                    .contract_allowlist
                    .get(&network)
                    .and_then(|contracts| contracts.get(&contract_type))
                {
                    let mut contract_ids: Vec<&str> =
                        contract_ids.iter().map(String::as_str).collect();
                    contract_ids.sort_unstable();
                    for contract_id in contract_ids {
                        allowed.push((network, contract_type, contract_id));
                    }
                }
            }
        }
        allowed
    }
}

fn require_env(key: &str) -> Result<String, String> {
    env::var(key).map_err(|_| format!("{key} environment variable is required"))
}

fn load_network_configs() -> Result<HashMap<Network, NetworkConfig>, String> {
    let mut networks = HashMap::new();
    for network in Network::ALL {
        networks.insert(
            network,
            NetworkConfig {
                rpc_url: env::var(network.rpc_url_env())
                    .unwrap_or_else(|_| network.default_rpc_url().to_string()),
                network_passphrase: env::var(network.passphrase_env())
                    .unwrap_or_else(|_| network.default_passphrase().to_string()),
                cli_network: network.cli_network().to_string(),
            },
        );
    }

    let legacy_rpc_url = non_empty_env("RELAYER_RPC_URL");
    let legacy_passphrase = non_empty_env("RELAYER_NETWORK_PASSPHRASE");
    if legacy_rpc_url.is_some() || legacy_passphrase.is_some() {
        let network = legacy_network()?;
        let network_config = networks
            .get_mut(&network)
            .expect("network configs are initialized for every supported network");
        if let Some(rpc_url) = legacy_rpc_url {
            network_config.rpc_url = rpc_url;
        }
        if let Some(passphrase) = legacy_passphrase {
            network_config.network_passphrase = passphrase;
        }
    }

    Ok(networks)
}

fn load_contract_allowlist() -> Result<ContractAllowlist, String> {
    if let Some(raw) = non_empty_env("RELAYER_CONTRACT_ALLOWLIST") {
        let parsed: Value = serde_json::from_str(&raw)
            .map_err(|e| format!("RELAYER_CONTRACT_ALLOWLIST must be valid JSON: {e}"))?;
        return parse_contract_allowlist(&parsed);
    }

    load_legacy_contract_allowlist()
}

fn parse_contract_allowlist(value: &Value) -> Result<ContractAllowlist, String> {
    let networks = value
        .as_object()
        .ok_or("RELAYER_CONTRACT_ALLOWLIST must be a JSON object keyed by network")?;
    let mut allowlist: ContractAllowlist = HashMap::new();

    for (network_key, contracts_value) in networks {
        let network = Network::from_str(network_key)?;
        let contracts = contracts_value.as_object().ok_or_else(|| {
            format!(
                "RELAYER_CONTRACT_ALLOWLIST.{network_key} must be an object keyed by contract type"
            )
        })?;

        for (contract_type_key, contract_ids_value) in contracts {
            let contract_type = ContractType::from_str(contract_type_key)?;
            let contract_ids = contract_ids_value.as_array().ok_or_else(|| {
                format!(
                    "RELAYER_CONTRACT_ALLOWLIST.{network_key}.{contract_type_key} must be an array"
                )
            })?;
            for contract_id_value in contract_ids {
                let contract_id = contract_id_value.as_str().ok_or_else(|| {
                    format!(
                        "RELAYER_CONTRACT_ALLOWLIST.{network_key}.{contract_type_key} entries must be strings"
                    )
                })?;
                insert_contract_id(&mut allowlist, network, contract_type, contract_id)?;
            }
        }
    }

    if allowlist
        .values()
        .flat_map(HashMap::values)
        .all(HashSet::is_empty)
    {
        return Err("RELAYER_CONTRACT_ALLOWLIST must contain at least one contract ID".to_string());
    }

    Ok(allowlist)
}

fn load_legacy_contract_allowlist() -> Result<ContractAllowlist, String> {
    let network = legacy_network()?;
    let mut allowlist: ContractAllowlist = HashMap::new();

    for (contract_type, key) in LEGACY_CONTRACT_ENV_KEYS {
        if let Some(raw_contract_ids) = non_empty_env(key) {
            for contract_id in raw_contract_ids.split(',') {
                insert_contract_id(&mut allowlist, network, contract_type, contract_id)?;
            }
        }
    }

    if allowlist
        .values()
        .flat_map(HashMap::values)
        .all(HashSet::is_empty)
    {
        return Err(
            "RELAYER_CONTRACT_ALLOWLIST or legacy RELAYER_*_CONTRACT_ID environment variables are required"
                .to_string(),
        );
    }

    Ok(allowlist)
}

const LEGACY_CONTRACT_ENV_KEYS: [(ContractType, &str); 5] = [
    (ContractType::Anarchy, "RELAYER_ANARCHY_CONTRACT_ID"),
    (ContractType::OneOnOne, "RELAYER_ONEONONE_CONTRACT_ID"),
    (ContractType::Democracy, "RELAYER_DEMOCRACY_CONTRACT_ID"),
    (ContractType::Oligarchy, "RELAYER_OLIGARCHY_CONTRACT_ID"),
    (ContractType::Tyranny, "RELAYER_TYRANNY_CONTRACT_ID"),
];

fn legacy_network() -> Result<Network, String> {
    match non_empty_env("RELAYER_NETWORK") {
        Some(raw) => Network::from_str(&raw),
        None => Ok(Network::Public),
    }
}

fn non_empty_env(key: &str) -> Option<String> {
    env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn insert_contract_id(
    allowlist: &mut ContractAllowlist,
    network: Network,
    contract_type: ContractType,
    contract_id: &str,
) -> Result<(), String> {
    let contract_id = contract_id.trim();
    if contract_id.is_empty() {
        return Err(format!("empty contract ID for {network}/{contract_type}"));
    }
    if !contract_id.starts_with('C') {
        return Err(format!(
            "contract ID for {network}/{contract_type} must start with C: {contract_id}"
        ));
    }
    if let Some(existing_type) = allowlist.get(&network).and_then(|contracts| {
        contracts.iter().find_map(|(existing_type, contract_ids)| {
            contract_ids.contains(contract_id).then_some(*existing_type)
        })
    }) {
        if existing_type != contract_type {
            return Err(format!(
                "{network} contract ID {contract_id} is mapped to both {existing_type} and {contract_type}"
            ));
        }
    }

    allowlist
        .entry(network)
        .or_default()
        .entry(contract_type)
        .or_default()
        .insert(contract_id.to_string());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_network_accepts_public_and_mainnet_aliases() {
        assert_eq!(Network::from_str("testnet").unwrap(), Network::Testnet);
        assert_eq!(Network::from_str("mainnet").unwrap(), Network::Public);
        assert_eq!(Network::from_str("public").unwrap(), Network::Public);
    }

    #[test]
    fn test_contract_type_accepts_sep_oneonone_alias() {
        assert_eq!(
            ContractType::from_str("sep-oneonone").unwrap(),
            ContractType::OneOnOne
        );
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

    #[test]
    fn test_parse_contract_allowlist_by_network_and_type() {
        let value = serde_json::json!({
            "testnet": {
                "anarchy": ["CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAD2KM"],
                "sep-oneonone": ["CBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBQ6L2"]
            },
            "public": {
                "tyranny": ["CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCQI2M"]
            }
        });
        let allowlist = parse_contract_allowlist(&value).unwrap();

        assert!(allowlist[&Network::Testnet][&ContractType::Anarchy]
            .contains("CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAD2KM"));
        assert!(allowlist[&Network::Testnet][&ContractType::OneOnOne]
            .contains("CBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBQ6L2"));
        assert!(allowlist[&Network::Public][&ContractType::Tyranny]
            .contains("CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCQI2M"));
    }

    #[test]
    fn test_parse_contract_allowlist_rejects_cross_type_duplicate_on_same_network() {
        let value = serde_json::json!({
            "testnet": {
                "anarchy": ["CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAD2KM"],
                "tyranny": ["CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAD2KM"]
            }
        });
        let err = parse_contract_allowlist(&value).unwrap_err();
        assert!(err.contains("mapped to both"));
    }

    fn make_config(auth_tokens: HashSet<String>) -> Config {
        let value = serde_json::json!({
            "testnet": {
                "anarchy": ["CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAD2KM"]
            }
        });
        let mut networks = HashMap::new();
        networks.insert(
            Network::Testnet,
            NetworkConfig {
                rpc_url: String::new(),
                network_passphrase: String::new(),
                cli_network: "testnet".to_string(),
            },
        );
        networks.insert(
            Network::Public,
            NetworkConfig {
                rpc_url: String::new(),
                network_passphrase: String::new(),
                cli_network: "mainnet".to_string(),
            },
        );
        Config {
            secret_key: String::new(),
            public_address: String::new(),
            contract_allowlist: parse_contract_allowlist(&value).unwrap(),
            networks,
            bind_address: "0.0.0.0:8080".to_string(),
            auth_tokens,
            rate_limit_per_minute: 30,
            max_payload_size: 8192,
            identity_name: "onym-relayer".to_string(),
        }
    }
}
