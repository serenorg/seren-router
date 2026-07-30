#!/usr/bin/env bash
# ABOUTME: Builds and exercises the local two-container production boundary.
# ABOUTME: Uses the official pinned sidecar with no provider traffic or real secrets.

set -euo pipefail

die() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

for tool in awk chmod curl docker grep mktemp rm sleep; do
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
grep -q '^seren_router_' <<<"$metrics" || die "Prometheus metrics prefix is missing"

rendered="$SEREN_ROUTER_SMOKE_CONFIG_DIR/agentgateway.yaml"
[[ -f "$rendered" ]] || die "renderer did not create AgentGateway configuration"
grep -Fq '$SEREN_ROUTER_KEY_OPENROUTER' "$rendered" \
    || die "rendered configuration omitted the OpenRouter environment reference"
grep -Fq '$SEREN_ROUTER_KEY_MODAL' "$rendered" \
    || die "rendered configuration omitted the Modal environment reference"
grep -Fq '$SEREN_ROUTER_KEY_DEEPINFRA' "$rendered" \
    || die "rendered configuration omitted the DeepInfra environment reference"
if grep -Fq 'deployment-smoke-only' "$rendered" \
    || grep -Fq 'deployment-smoke-beta-only' "$rendered" \
    || grep -Fq 'deployment-smoke-modal-only' "$rendered" \
    || grep -Fq 'deployment-smoke-deepinfra-only' "$rendered"; then
    die "rendered configuration resolved a secret value"
fi

production_kimi="$(
    curl \
        --fail \
        --silent \
        --max-time 3 \
        --header 'Authorization: Bearer deployment-smoke-only' \
        "http://${published}/api/v1/models/moonshotai/kimi-k3/endpoints"
)"
grep -Fq '"provider_name":"OpenRouter"' <<<"$production_kimi" \
    || die "production Kimi catalog omitted the OpenRouter fallback"
grep -Fq '"provider_name":"Seren Inference"' <<<"$production_kimi" \
    || die "production Kimi catalog omitted the active neutral direct route"
if grep -iq 'modal' <<<"$production_kimi"; then
    die "production Kimi catalog exposed the internal provider brand"
fi

beta_kimi="$(
    curl \
        --fail \
        --silent \
        --max-time 3 \
        --header 'Authorization: Bearer deployment-smoke-beta-only' \
        "http://${published}/api/v1/models/moonshotai/kimi-k3/endpoints"
)"
grep -Fq '"provider_name":"OpenRouter"' <<<"$beta_kimi" \
    || die "beta Kimi catalog omitted the active OpenRouter route"
grep -Fq '"provider_name":"Seren Inference"' <<<"$beta_kimi" \
    || die "beta Kimi catalog omitted the active neutral direct route"
if grep -iq 'modal' <<<"$beta_kimi"; then
    die "beta Kimi catalog exposed the internal provider brand"
fi

production_llama="$(
    curl \
        --fail \
        --silent \
        --max-time 3 \
        --header 'Authorization: Bearer deployment-smoke-only' \
        "http://${published}/api/v1/models/meta-llama/llama-3.3-70b-instruct/endpoints"
)"
grep -Fq '"provider_name":"OpenRouter"' <<<"$production_llama" \
    || die "production Llama catalog omitted the OpenRouter route"
if grep -Fq '"provider_name":"DeepInfra"' <<<"$production_llama"; then
    die "production Llama catalog exposed the beta-only DeepInfra route"
fi

beta_llama="$(
    curl \
        --fail \
        --silent \
        --max-time 3 \
        --header 'Authorization: Bearer deployment-smoke-beta-only' \
        "http://${published}/api/v1/models/meta-llama/llama-3.3-70b-instruct/endpoints"
)"
grep -Fq '"provider_name":"DeepInfra"' <<<"$beta_llama" \
    || die "beta Llama catalog omitted the DeepInfra direct route"
grep -Fq '"provider_name":"OpenRouter"' <<<"$beta_llama" \
    || die "beta Llama catalog omitted the OpenRouter fallback"

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
