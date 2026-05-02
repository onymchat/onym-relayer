#!/usr/bin/env bash
set -euo pipefail

DROPLET_NAME="${RELAYER_DROPLET_NAME:-onym-relayer}"
DROPLET_REGION="${RELAYER_DROPLET_REGION:-nyc3}"
DROPLET_SIZE="${RELAYER_DROPLET_SIZE:-s-1vcpu-1gb}"
DROPLET_IMAGE="${RELAYER_DROPLET_IMAGE:-ubuntu-24-04-x64}"
DROPLET_TAG="${RELAYER_DROPLET_TAG:-onym-relayer}"
DROPLET_ID="${RELAYER_DROPLET_ID:-}"
SSH_USER="${RELAYER_DROPLET_SSH_USER:-root}"
SSH_KEY_ID="${RELAYER_DROPLET_SSH_KEY_ID:-${DROPLET_SSH_KEY_ID:-}}"
SSH_PRIVATE_KEY="${RELAYER_DROPLET_SSH_PRIVATE_KEY:-${DROPLET_SSH_PRIVATE_KEY:-}}"
TOKEN="${DIGITALOCEAN_ACCESS_TOKEN:-${DIGITALOCEAN_TOKEN:-}}"
ENV_FILE=""
IMAGE_TAR=""
IMAGE_REF=""
DEFAULT_CADDY_HOSTS="relayer-testnet.onym.chat, relayer.onym.chat"
CADDY_HOSTS="${RELAYER_CADDY_HOSTS:-$DEFAULT_CADDY_HOSTS}"

usage() {
    cat <<'USAGE'
Usage:
  scripts/deploy-droplet.sh --env-file <path> --image-tar <path> --image-ref <name:tag>

Required environment:
  DIGITALOCEAN_ACCESS_TOKEN
  RELAYER_DROPLET_SSH_KEY_ID or DROPLET_SSH_KEY_ID
  RELAYER_DROPLET_SSH_PRIVATE_KEY or DROPLET_SSH_PRIVATE_KEY

Optional environment:
  RELAYER_DROPLET_ID          Reuse a known droplet by ID.
  RELAYER_DROPLET_NAME        Default: onym-relayer.
  RELAYER_DROPLET_REGION      Default: nyc3.
  RELAYER_DROPLET_SIZE        Default: s-1vcpu-1gb.
  RELAYER_DROPLET_IMAGE       Default: ubuntu-24-04-x64.
  RELAYER_CADDY_HOSTS         Default: relayer-testnet.onym.chat, relayer.onym.chat.
USAGE
}

log() {
    printf '[deploy:droplet] %s\n' "$*" >&2
}

die() {
    printf '[deploy:droplet] error: %s\n' "$*" >&2
    exit 1
}

require_cmd() {
    command -v "$1" >/dev/null 2>&1 || die "missing required command: $1"
}

normalize_caddy_hosts() {
    local raw="$1"
    local normalized=""
    local host trimmed
    local -a hosts

    if [[ "$raw" =~ (^|,)[[:space:]]*(,|$) ]]; then
        die "RELAYER_CADDY_HOSTS contains an empty host; remove leading/trailing commas"
    fi

    IFS=',' read -r -a hosts <<< "$raw"
    for host in "${hosts[@]}"; do
        trimmed="$(printf '%s' "$host" | sed 's/^[[:space:]]*//; s/[[:space:]]*$//')"
        [ -n "$trimmed" ] || die "RELAYER_CADDY_HOSTS contains an empty host"
        case "$trimmed" in
            *[[:space:]]*|*"{"*|*"}"*)
                die "invalid Caddy host in RELAYER_CADDY_HOSTS: $trimmed"
                ;;
        esac
        if [ -z "$normalized" ]; then
            normalized="$trimmed"
        else
            normalized="$normalized, $trimmed"
        fi
    done

    [ -n "$normalized" ] || die "RELAYER_CADDY_HOSTS must contain at least one host"
    printf '%s\n' "$normalized"
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --env-file)
            [ "$#" -ge 2 ] || die "--env-file requires a value"
            ENV_FILE="$2"
            shift 2
            ;;
        --image-tar)
            [ "$#" -ge 2 ] || die "--image-tar requires a value"
            IMAGE_TAR="$2"
            shift 2
            ;;
        --image-ref)
            [ "$#" -ge 2 ] || die "--image-ref requires a value"
            IMAGE_REF="$2"
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            die "unknown argument: $1"
            ;;
    esac
done

[ -n "$TOKEN" ] || die "DIGITALOCEAN_ACCESS_TOKEN is required"
[ -n "$SSH_PRIVATE_KEY" ] || die "RELAYER_DROPLET_SSH_PRIVATE_KEY is required"
[ -n "$ENV_FILE" ] || die "--env-file is required"
[ -n "$IMAGE_TAR" ] || die "--image-tar is required"
[ -n "$IMAGE_REF" ] || die "--image-ref is required"
[ -f "$ENV_FILE" ] || die "env file not found: $ENV_FILE"
[ -f "$IMAGE_TAR" ] || die "image tar not found: $IMAGE_TAR"

require_cmd doctl
require_cmd ssh
require_cmd scp
require_cmd base64

CADDY_HOSTS="$(normalize_caddy_hosts "$CADDY_HOSTS")"
CADDY_HOSTS_B64="$(printf '%s' "$CADDY_HOSTS" | base64 | tr -d '\n')"

SSH_KEY_FILE="$(mktemp "${TMPDIR:-/tmp}/onym-relayer-ssh.XXXXXX")"
cleanup() {
    rm -f "$SSH_KEY_FILE"
}
trap cleanup EXIT
printf '%s\n' "$SSH_PRIVATE_KEY" > "$SSH_KEY_FILE"
chmod 600 "$SSH_KEY_FILE"

doctl_cmd() {
    doctl --access-token "$TOKEN" "$@"
}

find_droplet_by_name() {
    doctl_cmd compute droplet list --format ID,Name --no-header \
        | awk -v name="$DROPLET_NAME" '$2 == name {print $1; exit}'
}

droplet_ip() {
    doctl_cmd compute droplet get "$1" --format PublicIPv4 --no-header | awk 'NF {print $1; exit}'
}

if [ -z "$DROPLET_ID" ]; then
    DROPLET_ID="$(find_droplet_by_name || true)"
fi

if [ -z "$DROPLET_ID" ]; then
    [ -n "$SSH_KEY_ID" ] || die "RELAYER_DROPLET_SSH_KEY_ID is required to create a droplet"
    log "creating droplet: $DROPLET_NAME"
    create_output="$(
        doctl_cmd compute droplet create "$DROPLET_NAME" \
            --region "$DROPLET_REGION" \
            --size "$DROPLET_SIZE" \
            --image "$DROPLET_IMAGE" \
            --ssh-keys "$SSH_KEY_ID" \
            --tag-names "$DROPLET_TAG" \
            --wait \
            --format ID,PublicIPv4 \
            --no-header
    )"
    DROPLET_ID="$(printf '%s\n' "$create_output" | awk 'NF {print $1; exit}')"
    DROPLET_IP="$(printf '%s\n' "$create_output" | awk 'NF {print $2; exit}')"
else
    log "using existing droplet: $DROPLET_ID"
    DROPLET_IP="$(droplet_ip "$DROPLET_ID" || true)"
fi

[ -n "$DROPLET_ID" ] || die "failed to determine droplet ID"
for _ in $(seq 1 30); do
    if [ -n "${DROPLET_IP:-}" ]; then
        break
    fi
    DROPLET_IP="$(droplet_ip "$DROPLET_ID" || true)"
    sleep 5
done
[ -n "$DROPLET_IP" ] || die "failed to determine droplet public IP"
log "droplet IP: $DROPLET_IP"

SSH_OPTS=(
    -i "$SSH_KEY_FILE"
    -o StrictHostKeyChecking=accept-new
    -o UserKnownHostsFile="${HOME}/.ssh/known_hosts"
)
REMOTE="$SSH_USER@$DROPLET_IP"

for _ in $(seq 1 60); do
    if ssh "${SSH_OPTS[@]}" -o ConnectTimeout=5 "$REMOTE" true >/dev/null 2>&1; then
        break
    fi
    sleep 5
done

ssh "${SSH_OPTS[@]}" "$REMOTE" "mkdir -p /opt/onym-relayer"
scp "${SSH_OPTS[@]}" "$IMAGE_TAR" "$REMOTE:/opt/onym-relayer/image.tar.gz"
scp "${SSH_OPTS[@]}" "$ENV_FILE" "$REMOTE:/opt/onym-relayer/relayer.env"

log "installing runtime and restarting relayer"
ssh "${SSH_OPTS[@]}" "$REMOTE" bash -s -- "$IMAGE_REF" "$CADDY_HOSTS_B64" <<'REMOTE_SCRIPT'
set -euo pipefail
IMAGE_REF="$1"
CADDY_HOSTS="$(printf '%s' "$2" | base64 -d)"

export DEBIAN_FRONTEND=noninteractive
apt-get update
apt-get install -y ca-certificates curl gnupg

if ! command -v docker >/dev/null 2>&1; then
    apt-get install -y docker.io
fi
systemctl enable --now docker

if ! command -v caddy >/dev/null 2>&1; then
    apt-get install -y debian-keyring debian-archive-keyring apt-transport-https
    install -d -m 0755 /usr/share/keyrings
    curl -fsSL https://dl.cloudsmith.io/public/caddy/stable/gpg.key \
        | gpg --dearmor --yes -o /usr/share/keyrings/caddy-stable-archive-keyring.gpg
    curl -fsSL https://dl.cloudsmith.io/public/caddy/stable/debian.deb.txt \
        > /etc/apt/sources.list.d/caddy-stable.list
    apt-get update
    apt-get install -y caddy
fi

docker load -i /opt/onym-relayer/image.tar.gz
docker rm -f onym-relayer >/dev/null 2>&1 || true
docker run -d \
    --restart unless-stopped \
    --name onym-relayer \
    --env-file /opt/onym-relayer/relayer.env \
    --publish 127.0.0.1:8080:8080 \
    "$IMAGE_REF"

cat > /etc/caddy/Caddyfile <<CADDY
${CADDY_HOSTS} {
    encode zstd gzip
    reverse_proxy 127.0.0.1:8080
}
CADDY

caddy validate --config /etc/caddy/Caddyfile

systemctl enable --now caddy
systemctl reload caddy || systemctl restart caddy
REMOTE_SCRIPT

log "deployment complete: droplet_id=$DROPLET_ID ip=$DROPLET_IP"
if [ -n "${GITHUB_OUTPUT:-}" ]; then
    {
        printf 'droplet_id=%s\n' "$DROPLET_ID"
        printf 'droplet_ip=%s\n' "$DROPLET_IP"
    } >> "$GITHUB_OUTPUT"
fi
