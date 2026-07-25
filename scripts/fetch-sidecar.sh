#!/usr/bin/env bash
# ABOUTME: Downloads the pinned agentgateway release for the current platform.
# ABOUTME: Verifies the official SHA-256 digest before atomically installing it.

set -euo pipefail

die() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

for tool in awk curl install mkdir mktemp mv rmdir rm uname; do
    command -v "$tool" >/dev/null 2>&1 || die "required tool not found: $tool"
done

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/.." && pwd)"
pin_file="$repo_root/sidecar/PINNED_VERSION"
install_dir="$repo_root/sidecar/bin"
install_path="$install_dir/agentgateway"

[[ -f "$pin_file" ]] || die "pin file not found: $pin_file"

version="$(awk 'NR == 1 { print; exit }' "$pin_file")"
[[ "$version" =~ ^v[0-9]+\.[0-9]+\.[0-9]+[-0-9A-Za-z.]*$ ]] \
    || die "invalid pinned version: $version"

case "$(uname -s)" in
    Darwin) os="darwin" ;;
    Linux) os="linux" ;;
    *) die "unsupported operating system: $(uname -s)" ;;
esac

case "$(uname -m)" in
    arm64 | aarch64) arch="arm64" ;;
    x86_64 | amd64) arch="amd64" ;;
    *) die "unsupported architecture: $(uname -m)" ;;
esac

platform="$os-$arch"
case "$platform" in
    darwin-arm64 | linux-amd64 | linux-arm64) ;;
    *) die "unsupported platform: $platform" ;;
esac

record_count="$(awk -v platform="$platform" 'NR > 1 && $1 == platform { count++ } END { print count + 0 }' "$pin_file")"
[[ "$record_count" == "1" ]] || die "expected one checksum for $platform, found $record_count"

expected_checksum="$(awk -v platform="$platform" 'NR > 1 && $1 == platform { print $2 }' "$pin_file")"
[[ ${#expected_checksum} -eq 64 && ! "$expected_checksum" =~ [^0-9a-f] ]] \
    || die "invalid SHA-256 checksum for $platform"

if command -v shasum >/dev/null 2>&1; then
    checksum_command=(shasum -a 256)
elif command -v sha256sum >/dev/null 2>&1; then
    checksum_command=(sha256sum)
else
    die "required SHA-256 tool not found: install shasum or sha256sum"
fi

asset="agentgateway-$platform"
url="https://github.com/agentgateway/agentgateway/releases/download/$version/$asset"
temp_dir="$(mktemp -d "${TMPDIR:-/tmp}/seren-router-sidecar.XXXXXX")"
download_path="$temp_dir/$asset"
staged_path=""

cleanup() {
    rm -f "$download_path"
    if [[ -n "$staged_path" ]]; then
        rm -f "$staged_path"
    fi
    rmdir "$temp_dir" 2>/dev/null || true
}
trap cleanup EXIT

printf 'Downloading %s for %s...\n' "$version" "$platform"
curl \
    --fail \
    --location \
    --retry 3 \
    --show-error \
    --silent \
    --output "$download_path" \
    "$url"

actual_checksum="$("${checksum_command[@]}" "$download_path" | awk '{ print $1 }')"
[[ "$actual_checksum" == "$expected_checksum" ]] \
    || die "checksum mismatch for $asset: expected $expected_checksum, got $actual_checksum"

mkdir -p "$install_dir"
staged_path="$install_dir/.agentgateway.$$.tmp"
install -m 0755 "$download_path" "$staged_path"
mv -f "$staged_path" "$install_path"
staged_path=""

printf 'Installed %s to %s\n' "$version" "$install_path"
