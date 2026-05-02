# Onym Relayer

Standalone HTTP relayer for Onym Soroban contract calls.

## Requirements

- Rust toolchain
- `stellar` CLI available on `PATH`
- A funded Stellar account secret key for the networks the relayer will submit to

## Configuration

Copy `.env.example` to `.env` and edit the values:

```sh
cp .env.example .env
```

Required:

- `RELAYER_SECRET_KEY`: Stellar secret key used to sign transactions.
- `RELAYER_CONTRACT_ALLOWLIST`: JSON object keyed by network and contract type.

```json
{
  "testnet": {
    "anarchy": ["C..."],
    "oneonone": ["C..."],
    "democracy": ["C..."],
    "oligarchy": ["C..."],
    "tyranny": ["C..."]
  },
  "public": {
    "anarchy": [],
    "oneonone": [],
    "democracy": [],
    "oligarchy": [],
    "tyranny": []
  }
}
```

Optional:

- `RELAYER_TESTNET_RPC_URL`: Testnet Soroban RPC endpoint.
- `RELAYER_PUBLIC_RPC_URL`: Public/mainnet Soroban RPC endpoint.
- `RELAYER_TESTNET_NETWORK_PASSPHRASE`: Testnet passphrase override.
- `RELAYER_PUBLIC_NETWORK_PASSPHRASE`: Public/mainnet passphrase override.
- `RELAYER_BIND`: HTTP bind address.
- `RELAYER_AUTH_TOKENS`: Comma-separated bearer tokens. Empty disables auth.
- `RELAYER_RATE_LIMIT`: Requests per minute per IP.
- `RELAYER_MAX_PAYLOAD_SIZE`: Maximum JSON payload size in bytes.

Legacy single-network env vars are still accepted when
`RELAYER_CONTRACT_ALLOWLIST` is unset: `RELAYER_NETWORK`, `RELAYER_RPC_URL`,
`RELAYER_NETWORK_PASSPHRASE`, and the five `RELAYER_*_CONTRACT_ID` variables.
When `RELAYER_NETWORK` is unset, the legacy fallback follows the old mainnet
default. Official deployment uses the generated allowlist.

## Run

```sh
./run.sh
```

Or run directly:

```sh
cargo run
```

Build the container image:

```sh
docker build -t onym-relayer .
```

## Release Deployment

The release workflow generates `RELAYER_CONTRACT_ALLOWLIST` from all
`onymchat/onym-contracts` GitHub Releases, separated into `testnet` and
`public`, deploys one Dockerized relayer to one DigitalOcean droplet, and
publishes `relayers.json` to the latest relayer release.

Required GitHub secrets:

- `DIGITALOCEAN_ACCESS_TOKEN`
- `RELAYER_SECRET_KEY`
- `RELAYER_DROPLET_SSH_PRIVATE_KEY`
- `RELAYER_DROPLET_SSH_KEY_ID` when the workflow must create the droplet

Optional GitHub secrets:

- `RELAYER_DROPLET_ID`: force reuse of a known droplet ID.
- `RELAYER_AUTH_TOKENS`

Optional GitHub variables:

- `RELAYER_DROPLET_NAME`, `RELAYER_DROPLET_REGION`, `RELAYER_DROPLET_SIZE`
- `RELAYER_CADDY_HOSTS`
- `RELAYER_TESTNET_RPC_URL`, `RELAYER_PUBLIC_RPC_URL`
- `RELAYER_TESTNET_NETWORK_PASSPHRASE`,
  `RELAYER_PUBLIC_NETWORK_PASSPHRASE`
- `RELAYER_RATE_LIMIT`, `RELAYER_MAX_PAYLOAD_SIZE`

Run:

```sh
gh workflow run release.yml -f tag=v0.1.0 -f deploy=true
```

Re-running the workflow reuses `RELAYER_DROPLET_ID` when configured, otherwise
it reuses a droplet with the configured name. DNS for `relayer-testnet.onym.chat`
and `relayer.onym.chat` must point at the droplet IP for Caddy TLS to issue.

The release asset is always named `relayers.json` and is fetched from:

```text
https://github.com/onymchat/onym-relayer/releases/latest/download/relayers.json
```

Wire format:

```json
{
  "version": 1,
  "relayers": [
    {
      "name": "Onym Official Testnet",
      "url": "https://relayer-testnet.onym.chat",
      "network": "testnet"
    },
    {
      "name": "Onym Official Mainnet",
      "url": "https://relayer.onym.chat",
      "network": "public"
    }
  ]
}
```

## API

The service accepts `POST /` requests with:

```json
{
  "network": "testnet",
  "contractID": "C...",
  "contractType": "anarchy",
  "function": "get_commitment",
  "payload": {
    "group_id": "base64-encoded-32-byte-group-id"
  }
}
```

`network` is required and accepts `testnet`, `public`, or `mainnet`.
`contractID` and `contractType` are required; the ID must be allowlisted for
that type on that network. `contractType` may be `anarchy`, `oneonone`,
`democracy`, `oligarchy`, or `tyranny`.

Allowed public functions are the current per-type Soroban entrypoints:
`create_group`, `create_oligarchy_group`, `update_commitment`,
`verify_membership`, `get_commitment`, `get_history`, `bump_group_ttl`, and
tyranny-only `get_admin_commitment` where supported by the target contract.
`set_restricted_mode` is exposed only when `RELAYER_AUTH_TOKENS` is configured,
because the relayer signs that admin operation.

Byte fields may be sent as base64 or hex. The relayer forwards Soroban
`BytesN` arguments to `stellar contract invoke` as hex.

Proof-carrying calls use the PLONK contract surface:

- `proof`: 1601-byte PLONK proof, base64 or hex.
- `publicInputs`: `Vec<BytesN<32>>`, preferably a JSON array of 32-byte hex or
  base64 strings. Object form is also accepted and is normalized into the
  contract's ordered vector.

Example:

```json
{
  "network": "testnet",
  "contractID": "C...",
  "contractType": "anarchy",
  "function": "update_commitment",
  "payload": {
    "group_id": "base64-or-hex-32-byte-group-id",
    "proof": "base64-or-hex-1601-byte-proof",
    "publicInputs": [
      "hex-or-base64-c-old",
      "hex-or-base64-epoch-old-be32",
      "hex-or-base64-c-new"
    ]
  }
}
```
