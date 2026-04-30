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

Allowed public functions are the fresh per-type contract entrypoints:
`create_group`, `create_oligarchy_group`, `update_commitment`,
`verify_membership`, `get_commitment`, and `get_history` where supported by the
target contract.
