#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"

APP_NAME="onym-relayer"
APP_REGION="nyc"
SERVICE_NAME="relayer"
HTTP_PORT="8080"
INSTANCE_SIZE="apps-s-1vcpu-1gb"
INSTANCE_COUNT="1"
REGISTRY_NAME=""
REGISTRY_REGION="nyc3"
REGISTRY_TIER="basic"
REGISTRY_NAME_EXPLICIT="false"
IMAGE_REPOSITORY="onym-relayer"
IMAGE_TAG=""
ENV_FILE="$ROOT_DIR/.env"
DOCKER_PLATFORM="${DOCKER_PLATFORM:-linux/amd64}"
WAIT="true"
TOKEN="${DIGITALOCEAN_TOKEN:-}"

usage() {
    cat <<'USAGE'
Usage:
  scripts/deploy-digitalocean.sh <digitalocean-api-token> [options]

Options:
  --token <token>              DigitalOcean API token. Can also use DIGITALOCEAN_TOKEN.
  --env-file <path>            Relayer env file. Default: .env
  --app-name <name>            App Platform app name. Default: onym-relayer
  --app-region <slug>          App Platform region. Default: nyc
  --instance-size <slug>       App Platform instance size. Default: apps-s-1vcpu-1gb
  --instance-count <count>     Number of app instances. Default: 1
  --registry-name <name>       DO Container Registry name. Default: existing registry, or app name.
  --registry-region <slug>     Registry region for new registries. Default: nyc3
  --registry-tier <slug>       Registry tier for new registries. Default: basic
  --image-repository <name>    DOCR image repository. Default: onym-relayer
  --tag <tag>                  Image tag. Default: current.
  --no-wait                    Do not wait for App Platform deployment completion.
  -h, --help                   Show this help.

Example:
  scripts/deploy-digitalocean.sh "$DIGITALOCEAN_TOKEN" --env-file .env

Required tools:
  doctl, docker, python3
USAGE
}

log() {
    printf '[deploy:digitalocean] %s\n' "$*" >&2
}

die() {
    printf '[deploy:digitalocean] error: %s\n' "$*" >&2
    exit 1
}

require_cmd() {
    command -v "$1" >/dev/null 2>&1 || die "missing required command: $1"
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --token)
            [ "$#" -ge 2 ] || die "--token requires a value"
            TOKEN="$2"
            shift 2
            ;;
        --env-file)
            [ "$#" -ge 2 ] || die "--env-file requires a value"
            ENV_FILE="$2"
            shift 2
            ;;
        --app-name)
            [ "$#" -ge 2 ] || die "--app-name requires a value"
            APP_NAME="$2"
            shift 2
            ;;
        --app-region)
            [ "$#" -ge 2 ] || die "--app-region requires a value"
            APP_REGION="$2"
            shift 2
            ;;
        --instance-size)
            [ "$#" -ge 2 ] || die "--instance-size requires a value"
            INSTANCE_SIZE="$2"
            shift 2
            ;;
        --instance-count)
            [ "$#" -ge 2 ] || die "--instance-count requires a value"
            INSTANCE_COUNT="$2"
            shift 2
            ;;
        --registry-name)
            [ "$#" -ge 2 ] || die "--registry-name requires a value"
            REGISTRY_NAME="$2"
            REGISTRY_NAME_EXPLICIT="true"
            shift 2
            ;;
        --registry-region)
            [ "$#" -ge 2 ] || die "--registry-region requires a value"
            REGISTRY_REGION="$2"
            shift 2
            ;;
        --registry-tier)
            [ "$#" -ge 2 ] || die "--registry-tier requires a value"
            REGISTRY_TIER="$2"
            shift 2
            ;;
        --image-repository)
            [ "$#" -ge 2 ] || die "--image-repository requires a value"
            IMAGE_REPOSITORY="$2"
            shift 2
            ;;
        --tag)
            [ "$#" -ge 2 ] || die "--tag requires a value"
            IMAGE_TAG="$2"
            shift 2
            ;;
        --no-wait)
            WAIT="false"
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        --*)
            die "unknown option: $1"
            ;;
        *)
            if [ -z "$TOKEN" ]; then
                TOKEN="$1"
                shift
            else
                die "unexpected argument: $1"
            fi
            ;;
    esac
done

[ -n "$TOKEN" ] || die "DigitalOcean API token is required"
[ -f "$ENV_FILE" ] || die "env file not found: $ENV_FILE"

require_cmd doctl
require_cmd docker
require_cmd python3

list_registry_names() {
    if doctl registries list --access-token "$TOKEN" --format Name --no-header 2>/dev/null; then
        return 0
    fi
    doctl registry get --access-token "$TOKEN" --format Name --no-header 2>/dev/null || true
}

if [ -z "$IMAGE_TAG" ]; then
    IMAGE_TAG="current"
fi

if [ -z "$REGISTRY_NAME" ]; then
    REGISTRY_NAME="$(list_registry_names | awk 'NF {print $1; exit}')"
    if [ -z "$REGISTRY_NAME" ]; then
        REGISTRY_NAME="$APP_NAME"
    fi
fi

if list_registry_names \
    | awk -v name="$REGISTRY_NAME" '$1 == name {found=1} END {exit found ? 0 : 1}'; then
    log "using existing DigitalOcean Container Registry: $REGISTRY_NAME"
else
    if [ "$REGISTRY_NAME_EXPLICIT" = "true" ]; then
        log "creating requested DigitalOcean Container Registry: $REGISTRY_NAME"
    else
        log "creating DigitalOcean Container Registry: $REGISTRY_NAME"
    fi
    create_output="$(
        doctl registry create "$REGISTRY_NAME" \
            --access-token "$TOKEN" \
            --region "$REGISTRY_REGION" \
            --subscription-tier "$REGISTRY_TIER" 2>&1
    )" || {
        if printf '%s\n' "$create_output" | grep -qiE 'already exists|name.*taken|registry.*exists'; then
            log "registry already exists, continuing: $REGISTRY_NAME"
        else
            printf '%s\n' "$create_output" >&2
            die "failed to create DigitalOcean Container Registry: $REGISTRY_NAME"
        fi
    }
fi

log "logging Docker into DigitalOcean Container Registry"
if ! doctl registry login --access-token "$TOKEN" --expiry-seconds 3600 >/dev/null 2>&1; then
    doctl registries login "$REGISTRY_NAME" \
        --access-token "$TOKEN" \
        --expiry-seconds 3600 >/dev/null
fi

IMAGE_REF="registry.digitalocean.com/$REGISTRY_NAME/$IMAGE_REPOSITORY:$IMAGE_TAG"

log "building image: $IMAGE_REF"
docker build --pull --platform "$DOCKER_PLATFORM" -t "$IMAGE_REF" "$ROOT_DIR"

log "pushing image to DOCR"
docker push "$IMAGE_REF"

SPEC_FILE="$(mktemp "${TMPDIR:-/tmp}/onym-relayer-do-app.XXXXXX.json")"
cleanup() {
    rm -f "$SPEC_FILE"
}
trap cleanup EXIT

log "writing App Platform spec"
APP_NAME="$APP_NAME" \
APP_REGION="$APP_REGION" \
SERVICE_NAME="$SERVICE_NAME" \
HTTP_PORT="$HTTP_PORT" \
INSTANCE_SIZE="$INSTANCE_SIZE" \
INSTANCE_COUNT="$INSTANCE_COUNT" \
IMAGE_REPOSITORY="$IMAGE_REPOSITORY" \
IMAGE_TAG="$IMAGE_TAG" \
ENV_FILE="$ENV_FILE" \
SPEC_FILE="$SPEC_FILE" \
python3 <<'PY'
import json
import os
import re
import sys

env_file = os.environ["ENV_FILE"]
spec_file = os.environ["SPEC_FILE"]

required = [
    "RELAYER_SECRET_KEY",
    "RELAYER_ANARCHY_CONTRACT_ID",
    "RELAYER_ONEONONE_CONTRACT_ID",
    "RELAYER_DEMOCRACY_CONTRACT_ID",
    "RELAYER_OLIGARCHY_CONTRACT_ID",
    "RELAYER_TYRANNY_CONTRACT_ID",
]
secret_keys = {"RELAYER_SECRET_KEY", "RELAYER_AUTH_TOKENS"}
key_re = re.compile(r"^[_A-Za-z][_A-Za-z0-9]*$")


def strip_quotes(value: str) -> str:
    value = value.strip()
    if len(value) >= 2 and value[0] == value[-1] and value[0] in ("'", '"'):
        return value[1:-1]
    return value


env = {}
with open(env_file, "r", encoding="utf-8") as handle:
    for raw_line in handle:
        line = raw_line.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("export "):
            line = line[len("export "):].strip()
        if "=" not in line:
            continue
        key, value = line.split("=", 1)
        key = key.strip()
        if not key_re.match(key):
            continue
        env[key] = strip_quotes(value)

missing = [key for key in required if not env.get(key)]
if missing:
    print(
        "env file is missing required relayer variables: " + ", ".join(missing),
        file=sys.stderr,
    )
    sys.exit(2)

# App Platform must reach the process through the container network.
env["RELAYER_BIND"] = "0.0.0.0:8080"

envs = []
for key in sorted(k for k in env if k.startswith("RELAYER_")):
    value = env[key]
    if value == "":
        continue
    envs.append(
        {
            "key": key,
            "value": value,
            "scope": "RUN_TIME",
            "type": "SECRET" if key in secret_keys else "GENERAL",
        }
    )

spec = {
    "name": os.environ["APP_NAME"],
    "region": os.environ["APP_REGION"],
    "services": [
        {
            "name": os.environ["SERVICE_NAME"],
            "image": {
                "registry_type": "DOCR",
                "repository": os.environ["IMAGE_REPOSITORY"],
                "tag": os.environ["IMAGE_TAG"],
                "deploy_on_push": {"enabled": True},
            },
            "http_port": int(os.environ["HTTP_PORT"]),
            "instance_size_slug": os.environ["INSTANCE_SIZE"],
            "instance_count": int(os.environ["INSTANCE_COUNT"]),
            "envs": envs,
        }
    ],
    "ingress": {
        "rules": [
            {
                "match": {"path": {"prefix": "/"}},
                "component": {"name": os.environ["SERVICE_NAME"]},
            }
        ]
    },
    "alerts": [{"rule": "DEPLOYMENT_FAILED"}],
}

with open(spec_file, "w", encoding="utf-8") as handle:
    json.dump(spec, handle, indent=2)
    handle.write("\n")
PY

APP_ID="$(doctl apps list \
    --access-token "$TOKEN" \
    --format ID,Spec.Name \
    --no-header 2>/dev/null | awk -v name="$APP_NAME" '$2 == name {print $1; exit}')"

WAIT_FLAG=()
if [ "$WAIT" = "true" ]; then
    WAIT_FLAG=(--wait)
fi

if [ -n "$APP_ID" ]; then
    log "updating App Platform app: $APP_NAME ($APP_ID)"
    doctl apps update "$APP_ID" \
        --access-token "$TOKEN" \
        --spec "$SPEC_FILE" \
        --update-sources \
        --format ID,DefaultIngress,Updated \
        --no-header \
        "${WAIT_FLAG[@]}"
else
    log "creating App Platform app: $APP_NAME"
    doctl apps create \
        --access-token "$TOKEN" \
        --spec "$SPEC_FILE" \
        --format ID,DefaultIngress,Created \
        --no-header \
        "${WAIT_FLAG[@]}"
fi

log "deployed image: $IMAGE_REF"
