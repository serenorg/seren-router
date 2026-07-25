#!/usr/bin/env bash
# ABOUTME: Verifies the pinned official AgentGateway OCI index and Linux manifests.
# ABOUTME: Keeps deployment image provenance aligned with the reviewed binary release.

set -euo pipefail

die() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

for tool in awk curl jq mktemp rm rmdir; do
    command -v "$tool" >/dev/null 2>&1 || die "required tool not found: $tool"
done

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/.." && pwd)"
image_pin="$repo_root/sidecar/PINNED_IMAGE"
binary_pin="$repo_root/sidecar/PINNED_VERSION"

[[ -f "$image_pin" ]] || die "image pin not found: $image_pin"
[[ -f "$binary_pin" ]] || die "binary pin not found: $binary_pin"

reference="$(awk 'NR == 1 { print; exit }' "$image_pin")"
[[ "$reference" =~ ^ghcr\.io/agentgateway/agentgateway:v[0-9]+\.[0-9]+\.[0-9]+[-0-9A-Za-z.]*$ ]] \
    || die "invalid official image reference"
repository_and_tag="${reference#ghcr.io/}"
repository="${repository_and_tag%:*}"
tag="${repository_and_tag##*:}"
binary_version="$(awk 'NR == 1 { print; exit }' "$binary_pin")"
[[ "$tag" == "$binary_version" ]] || die "image tag and binary version differ"

expected_index="$(awk '$1 == "index" { print $2 }' "$image_pin")"
[[ "$expected_index" =~ ^sha256:[0-9a-f]{64}$ ]] || die "invalid pinned image index digest"

temp_dir="$(mktemp -d "${TMPDIR:-/tmp}/seren-router-image.XXXXXX")"
headers="$temp_dir/headers"
manifest="$temp_dir/manifest.json"

cleanup() {
    rm -f "$headers" "$manifest"
    rmdir "$temp_dir" 2>/dev/null || true
}
trap cleanup EXIT

token="$(
    curl \
        --fail \
        --silent \
        --show-error \
        --max-time 20 \
        "https://ghcr.io/token?scope=repository:${repository}:pull" |
        jq -er '.token'
)"

curl \
    --fail \
    --silent \
    --show-error \
    --max-time 30 \
    --dump-header "$headers" \
    --header "Authorization: Bearer $token" \
    --header "Accept: application/vnd.oci.image.index.v1+json, application/vnd.docker.distribution.manifest.list.v2+json" \
    --output "$manifest" \
    "https://ghcr.io/v2/${repository}/manifests/${tag}"

actual_index="$(
    awk '
        BEGIN { IGNORECASE = 1 }
        /^docker-content-digest:/ {
            gsub("\r", "", $2)
            print $2
        }
    ' "$headers"
)"
[[ "$actual_index" == "$expected_index" ]] \
    || die "image index digest mismatch: expected $expected_index, got $actual_index"

media_type="$(jq -r '.mediaType' "$manifest")"
[[ "$media_type" == "application/vnd.oci.image.index.v1+json" ]] \
    || die "unexpected image manifest media type: $media_type"

for platform in linux-amd64 linux-arm64; do
    expected="$(awk -v platform="$platform" '$1 == platform { print $2 }' "$image_pin")"
    [[ "$expected" =~ ^sha256:[0-9a-f]{64}$ ]] || die "invalid pinned digest for $platform"
    os="${platform%-*}"
    architecture="${platform#*-}"
    actual="$(
        jq -er \
            --arg os "$os" \
            --arg architecture "$architecture" \
            '
                [
                    .manifests[]
                    | select(
                        .platform.os == $os
                        and .platform.architecture == $architecture
                    )
                    | .digest
                ]
                | if length == 1 then .[0] else error("expected one platform manifest") end
            ' \
            "$manifest"
    )"
    [[ "$actual" == "$expected" ]] \
        || die "$platform digest mismatch: expected $expected, got $actual"
done

printf 'Verified %s at %s\n' "$reference" "$expected_index"
