#!/usr/bin/env bash
# Cached download with sha256 verification.
# Usage: source scripts/lib/download.sh; download_to <url> <dest> <sha256>
set -euo pipefail

VENDOR_DIR="${VENDOR_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../../vendor" && pwd)}"

download_to() {
    local url="$1" dest="$2" sha256="$3"
    local cache_file="$VENDOR_DIR/$(basename "$dest")"

    if [[ -f "$cache_file" ]]; then
        local actual
        actual=$(sha256sum "$cache_file" | awk '{print $1}')
        if [[ "$actual" == "$sha256" ]]; then
            cp -f "$cache_file" "$dest"
            return 0
        fi
        echo "  cache file $cache_file failed checksum, redownloading" >&2
    fi

    echo "  downloading $(basename "$dest")..." >&2
    curl --fail --location --silent --show-error --output "$cache_file" "$url"

    local actual
    actual=$(sha256sum "$cache_file" | awk '{print $1}')
    if [[ "$actual" != "$sha256" ]]; then
        echo "  ERROR: checksum mismatch for $cache_file" >&2
        echo "  expected: $sha256" >&2
        echo "  actual:   $actual" >&2
        rm -f "$cache_file"
        exit 1
    fi
    cp -f "$cache_file" "$dest"
}
