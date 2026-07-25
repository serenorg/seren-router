#!/usr/bin/env bash
# ABOUTME: Builds and exercises the local two-container production boundary.
# ABOUTME: Uses the official pinned sidecar with no provider traffic or real secrets.

set -euo pipefail

die() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

for tool in awk chmod curl docker mktemp rg rm sleep; do
    command -v "$tool" >/dev/null 2>&1 || die "required tool not found: $tool"
done
docker compose version >/dev/null 2>&1 || die "docker compose is required"

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/.." && pwd)"
compose_file="$repo_root/deploy/docker-compose.smoke.yaml"
image_pin="$repo_root/sidecar/PINNED_IMAGE"

smoke_id="seren-router-smoke-$$"
export SEREN_ROUTER_SMOKE_IMAGE="$smoke_id:local"
export SEREN_ROUTER_SMOKE_CONFIG_DIR="$(
    mktemp -d "${TMPDIR:-/tmp}/seren-router-deployment-smoke.XXXXXX"
)"
chmod 0777 "$SEREN_ROUTER_SMOKE_CONFIG_DIR"
pinned_reference="$(awk 'NR == 1 { print; exit }' "$image_pin")"
pinned_digest="$(awk '$1 == "index" { print $2 }' "$image_pin")"
export SEREN_ROUTER_SIDECAR_IMAGE="${pinned_reference}@${pinned_digest}"

cleanup() {
    docker compose \
        --project-name "$smoke_id" \
        --file "$compose_file" \
        down \
        --volumes \
        --remove-orphans \
        >/dev/null 2>&1 || true
    docker image rm --force "$SEREN_ROUTER_SMOKE_IMAGE" >/dev/null 2>&1 || true
    rm -rf "$SEREN_ROUTER_SMOKE_CONFIG_DIR"
}
trap cleanup EXIT

docker build --tag "$SEREN_ROUTER_SMOKE_IMAGE" "$repo_root"
docker compose \
    --project-name "$smoke_id" \
    --file "$compose_file" \
    config \
    --quiet
docker compose \
    --project-name "$smoke_id" \
    --file "$compose_file" \
    up \
    --detach

deadline=$((SECONDS + 90))
published=""
while ((SECONDS < deadline)); do
    published="$(
        docker compose \
            --project-name "$smoke_id" \
            --file "$compose_file" \
            port \
            agentgateway \
            8000 \
            2>/dev/null || true
    )"
    if [[ -n "$published" ]] && curl --fail --silent --max-time 3 "http://${published}/readyz" >/dev/null; then
        break
    fi
    sleep 1
done

if [[ -z "$published" ]] || ! curl --fail --silent --max-time 3 "http://${published}/readyz" >/dev/null; then
    docker compose \
        --project-name "$smoke_id" \
        --file "$compose_file" \
        ps
    docker compose \
        --project-name "$smoke_id" \
        --file "$compose_file" \
        logs \
        --no-color
    die "deployment readiness did not become healthy"
fi

curl --fail --silent --max-time 3 "http://${published}/livez" >/dev/null
metrics="$(curl --fail --silent --max-time 3 "http://${published}/metrics")"
rg -q '^seren_router_' <<<"$metrics" || die "Prometheus metrics prefix is missing"

rendered="$SEREN_ROUTER_SMOKE_CONFIG_DIR/agentgateway.yaml"
[[ -f "$rendered" ]] || die "renderer did not create AgentGateway configuration"
rg -q '\$SEREN_ROUTER_KEY_OPENROUTER' "$rendered" \
    || die "rendered configuration omitted the provider environment reference"
if rg -q 'deployment-smoke-only' "$rendered"; then
    die "rendered configuration resolved a secret value"
fi

renderer_id="$(
    docker compose \
        --project-name "$smoke_id" \
        --file "$compose_file" \
        ps \
        --all \
        --quiet \
        render-sidecar-config
)"
[[ -n "$renderer_id" ]] || die "renderer container was not created"
renderer_status="$(docker inspect --format '{{.State.ExitCode}}' "$renderer_id")"
[[ "$renderer_status" == "0" ]] || die "renderer container did not exit successfully"

printf 'deployment_smoke=green url=http://%s sidecar=%s\n' \
    "$published" \
    "$SEREN_ROUTER_SIDECAR_IMAGE"
