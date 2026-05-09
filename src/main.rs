mod config;
mod handler;
mod manifest;
mod validation;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use axum::routing::post;
use axum::Router;
use tower_http::limit::RequestBodyLimitLayer;

use config::{AllowlistSource, Config};
use handler::AppState;
use validation::RateLimiter;

#[tokio::main]
async fn main() {
    let mut config = match Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Configuration error: {e}");
            eprintln!();
            eprintln!("Required environment variables:");
            eprintln!(
                "  RELAYER_SECRET_KEY     Stellar secret key (S...) for signing transactions"
            );
            eprintln!();
            eprintln!("Allowlist source (first match wins):");
            eprintln!(
                "  RELAYER_CONTRACT_ALLOWLIST  JSON allowlist keyed by network and contract type"
            );
            eprintln!(
                "  RELAYER_*_CONTRACT_ID       Legacy single-network env vars (with RELAYER_NETWORK)"
            );
            eprintln!("  RELAYER_CONTRACTS_MANIFEST_URL  Cumulative onym-contracts manifest URL");
            eprintln!(
                "                              Default: {}",
                config::DEFAULT_MANIFEST_URL
            );
            eprintln!();
            eprintln!("Optional:");
            eprintln!(
                "  RELAYER_TESTNET_RPC_URL  Soroban testnet RPC (default: https://soroban-testnet.stellar.org)"
            );
            eprintln!(
                "  RELAYER_PUBLIC_RPC_URL   Soroban public RPC (default: https://soroban.stellar.org)"
            );
            eprintln!("  RELAYER_TESTNET_NETWORK_PASSPHRASE  Testnet passphrase override");
            eprintln!("  RELAYER_PUBLIC_NETWORK_PASSPHRASE   Public passphrase override");
            eprintln!(
                "  RELAYER_RPC_URL / RELAYER_NETWORK_PASSPHRASE / RELAYER_NETWORK  Legacy single-network overrides"
            );
            eprintln!("  RELAYER_BIND           Listen address (default: 0.0.0.0:8080)");
            eprintln!("  RELAYER_AUTH_TOKENS    Comma-separated bearer tokens (default: none)");
            eprintln!("  RELAYER_RATE_LIMIT     Requests/minute per IP (default: 30)");
            eprintln!("  RELAYER_MAX_PAYLOAD_SIZE  Max body size in bytes (default: 8192)");
            eprintln!(
                "  RELAYER_CONTRACTS_MANIFEST_REFRESH_SECS  Manifest poll interval in seconds (default: {})",
                config::DEFAULT_MANIFEST_REFRESH_SECS
            );
            eprintln!(
                "  RELAYER_MANIFEST_CACHE_PATH  Last-known-good cache file (default: {}; '-' to disable)",
                config::DEFAULT_MANIFEST_CACHE_PATH
            );
            std::process::exit(1);
        }
    };

    // Add the secret key into stellar CLI as a named identity.
    // Current CLI versions use `keys add --secret-key` instead of `keys import`.
    eprintln!("Adding relayer identity to stellar CLI...");
    let add_output = Command::new("stellar")
        .arg("keys")
        .arg("add")
        .arg(&config.identity_name)
        .arg("--secret-key")
        .arg("--overwrite")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(ref mut stdin) = child.stdin {
                stdin.write_all(config.secret_key.as_bytes())?;
                stdin.write_all(b"\n")?;
            }
            child.wait_with_output()
        });

    match add_output {
        Ok(o) if o.status.success() => {}
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            // "already exists" is fine — we asked for overwrite
            if !stderr.contains("already exists") {
                eprintln!("Warning: stellar keys add: {stderr}");
            }
        }
        Err(e) => {
            eprintln!("Failed to run 'stellar keys add --secret-key': {e}");
            eprintln!("Make sure the 'stellar' CLI is installed and in PATH.");
            std::process::exit(1);
        }
    }

    // Resolve the public address from the identity
    let pubkey_output = Command::new("stellar")
        .arg("keys")
        .arg("public-key")
        .arg(&config.identity_name)
        .output();

    match pubkey_output {
        Ok(o) if o.status.success() => {
            config.public_address = String::from_utf8_lossy(&o.stdout).trim().to_string();
        }
        _ => {
            eprintln!("Failed to resolve public key for relayer identity.");
            eprintln!("Check that RELAYER_SECRET_KEY is a valid Stellar secret key.");
            std::process::exit(1);
        }
    }

    eprintln!("Relayer address: {}", config.public_address);

    // Allowlist bootstrap. For Remote sources we do an initial fetch
    // before binding the listener — better to fail loudly at startup
    // than to serve traffic with an empty allowlist. If GitHub is
    // unreachable (rare), fall back to the on-disk last-known-good
    // cache so a transient outage doesn't take the relayer down.
    if let AllowlistSource::Remote {
        url, cache_path, ..
    } = config.allowlist_source.clone()
    {
        bootstrap_remote_allowlist(&config, &url, cache_path.as_deref()).await;
    }

    eprintln!("Allowed contracts ({} ids):", config.allowlist_size());
    for (network, contract_type, contract_id) in config.allowed_contracts() {
        eprintln!(
            "  {:<7} {:<14} {}",
            network,
            contract_type.display_name(),
            contract_id
        );
    }
    match &config.allowlist_source {
        AllowlistSource::Static => {
            eprintln!("Allowlist source: static (env var)");
        }
        AllowlistSource::Remote {
            url,
            refresh_interval,
            cache_path,
        } => {
            eprintln!(
                "Allowlist source: {} (refresh every {}s, cache: {})",
                url,
                refresh_interval.as_secs(),
                cache_path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "<disabled>".to_string()),
            );
        }
    }
    eprintln!("Networks:");
    for network in config::Network::ALL {
        let network_config = config.network_config(network);
        eprintln!(
            "  {:<7} {} ({})",
            network, network_config.rpc_url, network_config.cli_network
        );
    }
    eprintln!("Auth required:  {}", config.auth_required());
    eprintln!(
        "Rate limit:     {} req/min per IP",
        config.rate_limit_per_minute
    );

    let bind_address: SocketAddr = config
        .bind_address
        .parse()
        .expect("RELAYER_BIND must be a valid socket address");

    let max_payload = config.max_payload_size;
    let state = Arc::new(AppState {
        rate_limiter: RateLimiter::new(config.rate_limit_per_minute),
        config,
    });

    // Spawn the periodic refresh once we have AppState in an Arc — both
    // the timer and /admin/refresh share the same state and atomically
    // swap the allowlist via Config::replace_allowlist.
    if let AllowlistSource::Remote {
        url,
        refresh_interval,
        cache_path,
    } = state.config.allowlist_source.clone()
    {
        spawn_manifest_refresh_loop(state.clone(), url, refresh_interval, cache_path);
    }

    let app = Router::new()
        .route("/", post(handler::handle_invoke))
        .route("/admin/refresh", post(handler::handle_admin_refresh))
        .layer(RequestBodyLimitLayer::new(max_payload))
        .with_state(state)
        .into_make_service_with_connect_info::<SocketAddr>();

    eprintln!("Listening on {bind_address}");

    let listener = tokio::net::TcpListener::bind(bind_address)
        .await
        .expect("failed to bind");
    axum::serve(listener, app).await.expect("server error");
}

/// Try the manifest URL once at boot. Falls back to the disk cache on
/// network failure, then exits if both are empty — better to refuse to
/// boot than to silently serve every request as `unknown contract`.
async fn bootstrap_remote_allowlist(
    config: &Config,
    url: &str,
    cache_path: Option<&std::path::Path>,
) {
    eprintln!("[manifest] bootstrap fetch: {url}");
    match manifest::fetch_and_parse(url).await {
        Ok((bytes, allowlist)) => {
            config.replace_allowlist(allowlist);
            if let Some(path) = cache_path {
                manifest::save_cache(path, &bytes).await;
            }
            eprintln!(
                "[manifest] bootstrap ok ({} contract IDs)",
                config.allowlist_size()
            );
            return;
        }
        Err(e) => {
            eprintln!("[manifest] bootstrap fetch failed: {e}");
        }
    }

    if let Some(path) = cache_path {
        match manifest::load_cache(path).await {
            Ok(Some(allowlist)) => {
                config.replace_allowlist(allowlist);
                eprintln!(
                    "[manifest] bootstrap fell back to cache {} ({} contract IDs)",
                    path.display(),
                    config.allowlist_size()
                );
                return;
            }
            Ok(None) => {
                eprintln!(
                    "[manifest] no cache at {} — bootstrap has nothing to load",
                    path.display()
                );
            }
            Err(e) => {
                eprintln!("[manifest] cache read at {} failed: {e}", path.display());
            }
        }
    }

    eprintln!(
        "[manifest] FATAL: no allowlist available. Set RELAYER_CONTRACT_ALLOWLIST for offline operation, or check network access to {url}."
    );
    std::process::exit(1);
}

/// Background task: re-fetch the manifest on a fixed cadence. Failures
/// log but never panic — the live allowlist keeps serving until the
/// next successful refresh (or an explicit /admin/refresh poke).
fn spawn_manifest_refresh_loop(
    state: Arc<AppState>,
    url: String,
    interval: Duration,
    cache_path: Option<PathBuf>,
) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        // Skip the immediate tick — bootstrap_remote_allowlist already
        // ran one fetch synchronously before we got here.
        ticker.tick().await;
        loop {
            ticker.tick().await;
            match manifest::fetch_and_parse(&url).await {
                Ok((bytes, allowlist)) => {
                    let total = allowlist
                        .values()
                        .flat_map(|by_type| by_type.values())
                        .map(|ids| ids.len())
                        .sum::<usize>();
                    state.config.replace_allowlist(allowlist);
                    if let Some(path) = cache_path.as_deref() {
                        manifest::save_cache(path, &bytes).await;
                    }
                    eprintln!("[manifest] periodic refresh ok ({} contract IDs)", total);
                }
                Err(e) => {
                    eprintln!(
                        "[manifest] periodic refresh failed (keeping previous allowlist): {e}"
                    );
                }
            }
        }
    });
}
