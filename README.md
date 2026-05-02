# Onym Relayer

Standalone HTTP relayer for Onym Soroban contract calls.

## Requirements

- Rust toolchain
- `stellar` CLI available on `PATH`
- A funded Stellar account secret key for the configured network

## Configuration

Copy `.env.example` to `.env` and edit the values:

```sh
cp .env.example .env
```

Required:

- `RELAYER_SECRET_KEY`: Stellar secret key used to sign transactions.
- Per-type contract IDs for the five contracts:
  `RELAYER_ANARCHY_CONTRACT_ID`, `RELAYER_ONEONONE_CONTRACT_ID`,
  `RELAYER_DEMOCRACY_CONTRACT_ID`, `RELAYER_OLIGARCHY_CONTRACT_ID`,
  `RELAYER_TYRANNY_CONTRACT_ID`.

Optional:

- `RELAYER_RPC_URL`: Soroban RPC endpoint.
- `RELAYER_NETWORK_PASSPHRASE`: Explicit network passphrase.
- `RELAYER_NETWORK`: `mainnet`, `testnet`, or `futurenet`.
- `RELAYER_BIND`: HTTP bind address.
- `RELAYER_AUTH_TOKENS`: Comma-separated bearer tokens. Empty disables auth.
- `RELAYER_RATE_LIMIT`: Requests per minute per IP.
- `RELAYER_MAX_PAYLOAD_SIZE`: Maximum JSON payload size in bytes.

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

Deploy to DigitalOcean App Platform:

```sh
scripts/deploy-digitalocean.sh "$DIGITALOCEAN_TOKEN" --env-file .env
```

The deploy script is idempotent by app name and registry name: it reuses or
creates the DigitalOcean Container Registry, pushes `onym-relayer:current`, then
creates or updates the App Platform app.

## API

The service accepts `POST /` requests with:

```json
{
  "contractID": "C...",
  "contractType": "anarchy",
  "function": "get_commitment",
  "payload": {
    "group_id": "base64-encoded-32-byte-group-id"
  }
}
```

`contractID` must be one of the configured per-type IDs. `contractType` may be
`anarchy`, `oneonone`, `democracy`, `oligarchy`, or `tyranny`.

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
  base64 strings. Object form is also accepted for compatibility and is
  normalized into the contract's ordered vector.

Example:

```json
{
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
