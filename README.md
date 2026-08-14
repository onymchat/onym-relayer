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

The contract allowlist is resolved in this order — first match wins:

1. **Remote manifest (default for the deployed droplet).** The relayer
   pulls `https://github.com/onymchat/onym-contracts/releases/latest/download/contracts-manifest.json`
   on startup and on a periodic timer (default: every 15 min). The
   manifest is cumulative — it carries the union of every historical
   release's contracts, not just the latest tag — so newly-deployed
   contracts and existing ones both stay allowlisted across releases.
   `POST /admin/refresh` (auth required) triggers an immediate refresh,
   which is what the `onym-contracts` release workflow does at the end
   of every release.
2. **`RELAYER_CONTRACT_ALLOWLIST`** (JSON env var, mutually exclusive
   with #1): static allowlist baked at startup, no remote refresh.
   Useful for offline development and tests.

   ```json
   {
     "testnet": { "anarchy": ["C..."], "oneonone": ["C..."], "democracy": ["C..."], "oligarchy": ["C..."], "tyranny": ["C..."] },
     "public":  { "anarchy": [],       "oneonone": [],       "democracy": [],       "oligarchy": [],       "tyranny": [] }
   }
   ```
3. **Legacy single-network vars** (`RELAYER_NETWORK`, `RELAYER_RPC_URL`,
   `RELAYER_NETWORK_PASSPHRASE`, and the five `RELAYER_*_CONTRACT_ID`)
   when neither #1 nor #2 is set.

Optional:

- `RELAYER_TESTNET_RPC_URL`: Testnet Soroban RPC endpoint.
- `RELAYER_PUBLIC_RPC_URL`: Public/mainnet Soroban RPC endpoint.
- `RELAYER_TESTNET_NETWORK_PASSPHRASE`: Testnet passphrase override.
- `RELAYER_PUBLIC_NETWORK_PASSPHRASE`: Public/mainnet passphrase override.
- `RELAYER_BIND`: HTTP bind address.
- `RELAYER_AUTH_TOKENS`: Comma-separated bearer tokens. Empty disables
  auth on `POST /` and disables `POST /admin/refresh` and
  `set_restricted_mode` entirely.
- `RELAYER_RATE_LIMIT`: Requests per minute per IP.
- `RELAYER_MAX_PAYLOAD_SIZE`: Maximum JSON payload size in bytes.
- `RELAYER_OPERATOR_MANIFEST`: Path to the signed notary-operator
  manifest (see `onym-system/notary/UI-Notary-BNB.md` §8.1), signed
  offline with the `onym-discovery` CLI. When set, it is verified at
  boot (the process refuses to start on an invalid signature) and
  served byte-for-byte at `GET /manifest.json`, with the detached
  signature at `GET /manifest.json.sig`. Unset: the endpoints return
  404 and nothing else changes.
- `RELAYER_CONTRACTS_MANIFEST_URL`: override the manifest URL (default:
  the `onymchat/onym-contracts` latest-release asset above).
- `RELAYER_CONTRACTS_MANIFEST_REFRESH_SECS`: timer interval, default `900`.
- `RELAYER_MANIFEST_CACHE_PATH`: last-known-good cache file. Survives
  GitHub-outage restarts. Default:
  `/var/lib/onym-relayer/manifest-cache.json`. Set to `-` to disable.

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

The release workflow deploys one Dockerized relayer to one DigitalOcean
droplet and publishes the repo's validated `relayers.json` to the latest
relayer release. The allowlist is **not** baked into the image — the
running relayer fetches `contracts-manifest.json` from
`onymchat/onym-contracts/releases/latest` on boot and on a 15-min timer,
so a new contract release is picked up automatically (or instantly via
`POST /admin/refresh` from the contracts release workflow).

This means relayer releases are now only needed for **relayer code
changes** — they're decoupled from contract releases.

Required GitHub secrets:

- `DO_TOKEN` (org-wide DigitalOcean API token)
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
it reuses a droplet with the configured name. DNS for `relayer-testnet.onym.app`
and `relayer.onym.app` must point at the droplet IP for Caddy TLS to issue.

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
      "name": "Onym Official",
      "url": "https://relayer.onym.app",
      "networks": ["testnet", "public"]
    }
  ]
}
```

The relayer URL is an origin; the request body still selects the Stellar
network with its required `network` field. Third-party operators can add their
own relayer by opening a PR that edits the tracked `relayers.json`; the release
workflow validates HTTPS URLs, unique origins, and supported networks before
publishing the asset.

### Transport manifests (Nostr relays + Blossom servers)

The same release publishes two more client-discovery manifests, fetched the
same way from `releases/latest/download/<asset>`:

```text
https://github.com/onymchat/onym-relayer/releases/latest/download/nostr-relays.json
https://github.com/onymchat/onym-relayer/releases/latest/download/blossom-servers.json
```

Both share the shape `{ "version": 1, "<key>": [ { "name", "url", "isDefault" } ] }`
and are validated by `scripts/validate-server-manifest.py` (Nostr URLs must be
`wss://`/`ws://`; Blossom URLs `https://`/`http://`; at most one `isDefault`).

`nostr-relays.json` — the apps connect to **every** listed relay (fan-out):

```json
{
  "version": 1,
  "relays": [
    { "name": "Onym Official", "url": "wss://nostr.onym.app", "isDefault": true }
  ]
}
```

`blossom-servers.json` — the apps upload/download media via the **first** listed
server (so order matters; put the primary first):

```json
{
  "version": 1,
  "servers": [
    { "name": "Onym Official", "url": "https://blossom.onym.app", "isDefault": true }
  ]
}
```

The apps seed a hardcoded default at first launch (offline-proof), then refresh
from these manifests in the background; a user's own custom entries are never
overwritten, and "Restore default" re-fetches the published list.

## Operator manifest signing (notary seat)

The relayer is a declared notary-seat operator
(`onym-system/notary/UI-Notary-BNB.md` §8): it serves a signed
operator manifest byte-for-byte at `GET /manifest.json`, and group
bindings and entitlements pin the SHA-256 of exactly those bytes.

The pieces:

- `operator-manifest.src.json` — the unsigned manifest source
  (component id, declared powers, networks, offers, `validUntil`).
  Changing terms means editing this file and re-running the signing
  workflow: a new manifest version and hash, never an edit in place.
  The `networks[].submitterAccount` entries declare the fee-payer
  account per network — the same `G...` account is used on both
  testnet and public, and it must be funded **on both networks**
  before clients pin the manifest (an unfunded fee-payer means every
  relayed transaction on that network fails). Confirm before signing:

  ```sh
  stellar keys fund <G...> --network testnet   # testnet only (friendbot)
  stellar balance --id <G...> --network testnet
  stellar balance --id <G...> --network mainnet  # fund manually first
  ```
  `powers.setRestrictedMode: ["manifest-mirror"]` declares that the
  deployments the operator can restrict are exactly the ones in the
  contracts manifest it mirrors (same semantics as
  `allowlistControl`), instead of enumerating contract IDs that churn
  with every contracts release.
- `manifest-signed/manifest.json` (+ `.sig`) — the signed bytes,
  committed to `main` by the workflow so what the service serves is
  auditable in-repo. The Dockerfile copies `manifest-signed/` to
  `/srv/operator-manifest/` inside the image;
  `RELAYER_OPERATOR_MANIFEST=/srv/operator-manifest/manifest.json`
  wires the service to it.
- `.github/workflows/sign-manifest.yml` — manual dispatch (type
  `sign-and-deploy` in the confirm input, on `main`): builds the
  `onym-discovery` CLI from source at a pinned revision
  (`PINNED_SIGNER_REV` in the workflow — never a floating branch, and
  never a cached binary, since the job holds the signing seed),
  signs with the `RELAYER_OPERATOR_SEED` secret,
  commits the signed bytes to `main`, redeploys, and verifies that
  `https://relayer.onym.app/manifest.json` serves the committed bytes
  stably (two fetches, both SHA-256-equal to the committed file). The
  operator id and fingerprint are printed in the job summary for
  out-of-band publication — and must also be copied over
  `REPLACE-RELAYER-KEY` in onym-discovery's
  `deploy/onym/catalogs/onym-services.config.json`, which is what the
  Discovery catalog pins.

### The `RELAYER_OPERATOR_SEED` secret

Generate the seed **offline**, not in CI:

```sh
cargo install --git https://github.com/onymchat/onym-discovery --locked
onym-discovery keygen --out relayer-operator.seed
```

- The secret value is the file's contents: 64 hex characters.
- Keep `relayer-operator.seed` offline as the custody copy.
- Publish the printed fingerprint out of band (site, announcement) so
  clients can check what they pin.
- **No rotation in v1.** A lost or replaced key is a *new* operator:
  existing bindings keep verifying against the old manifest bytes
  forever, but clients must re-pin to trust anything new. The workflow
  enforces this by failing when the seed no longer matches the
  operator key committed in `operator-manifest.src.json`.

### Environment protection (recommended)

The workflow runs in the `production` environment. GitHub
auto-creates that environment **unprotected** on first dispatch, so
before the first run: repo Settings → Environments → `production` →
add required reviewers, and scope `RELAYER_OPERATOR_SEED` to that
environment. Then every signing run waits for an approval before the
seed is exposed to the job.

### Deploy paths

- **App Platform** (default; `scripts/deploy-digitalocean.sh`
  conventions, org-level `DO_API_KEY` secret): the workflow pushes a
  fresh image to DOCR, ensures `RELAYER_OPERATOR_MANIFEST` in the app
  spec, waits, and verifies strictly.
- **onym-infra droplet** (where `relayer.onym.app` currently runs;
  the relayer is a git submodule of the compose stack): dispatch with
  `skip_deploy=true`, then follow these steps **in this order** —
  the order matters, because the relayer exits at boot
  (`std::process::exit(1)` in `src/main.rs`) when
  `RELAYER_OPERATOR_MANIFEST` points at a file that does not exist or
  does not verify. Setting the env var before the image contains
  `manifest-signed/manifest.json` puts the container in a crash loop.

  1. Dispatch `sign-manifest` with `skip_deploy=true`; it commits the
     signed bytes to `main` (the byte check will only *warn* — the
     droplet has not picked the commit up yet).
  2. **First**, in onym-infra: bump the `relayer` submodule to that
     signing commit (so the image built from it actually contains
     `/srv/operator-manifest/manifest.json`).
  3. **Only then** set
     `RELAYER_OPERATOR_MANIFEST=/srv/operator-manifest/manifest.json`
     in `relayer.env`. Do these two in one onym-infra commit if you
     like — what must never happen is deploying the env var against
     an image built from a pre-signing submodule rev.
  4. Deploy onym-infra.
  5. Re-run `sign-manifest` with `skip_deploy=true` for a green
     byte-for-byte verification of the served manifest.

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

### `POST /admin/refresh`

Re-pulls `contracts-manifest.json` and atomically swaps the live
allowlist. Bearer auth required (one of `RELAYER_AUTH_TOKENS`); returns
503 if no auth tokens are configured, 409 when the running source is
the static `RELAYER_CONTRACT_ALLOWLIST` env var, 200 with
`{"ok": true, "source": "...", "contractIds": N}` on success, 502 on
fetch/parse failure (the previous allowlist keeps serving).

```sh
curl -X POST -H "Authorization: Bearer $TOKEN" https://relayer.onym.app/admin/refresh
```

The `onym-contracts` release workflow calls this at the tail of every
release so newly-deployed contract addresses are usable immediately
instead of waiting for the next periodic refresh tick.
