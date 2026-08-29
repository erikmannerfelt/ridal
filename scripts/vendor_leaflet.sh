#!/usr/bin/env bash
#
# Vendor Leaflet into src/server/assets/vendor/.
#
# Ridal embeds its frontend assets in the binary (tracking issue #115), so
# Leaflet is committed to the repository rather than fetched at build time or
# loaded from a CDN. Leaflet is BSD-2-Clause, which permits redistribution in
# source form provided the copyright notice and disclaimer are retained; the
# licence is vendored alongside the code as LICENSE-leaflet.
#
# Files are fetched individually rather than from the release zip so that each
# one carries its own pinned digest, and so the script needs nothing beyond
# curl and sha256sum.
#
# To update:
#   1. bump LEAFLET_VERSION
#   2. run with --print-digests
#   3. paste the new block over FILES below
#   4. run again to fetch and verify
#
# Usage: scripts/vendor_leaflet.sh [--print-digests]
set -euo pipefail

LEAFLET_VERSION="1.9.4"

# "<remote path>  <sha256>"; installed path is derived from the remote path.
FILES=(
    "dist/leaflet.js                db49d009c841f5ca34a888c96511ae936fd9f5533e90d8b2c4d57596f4e5641a"
    "dist/leaflet.css               a7837102824184820dfa198d1ebcd109ff6d0ff9a2672a074b9a1b4d147d04c6"
    "dist/images/marker-icon.png    574c3a5cca85f4114085b6841596d62f00d7c892c7b03f28cbfa301deb1dc437"
    "dist/images/marker-icon-2x.png 00179c4c1ee830d3a108412ae0d294f55776cfeb085c60129a39aa6fc4ae2528"
    "dist/images/marker-shadow.png  264f5c640339f042dd729062cfc04c17f8ea0f29882b538e3848ed8f10edb4da"
    "dist/images/layers.png         1dbbe9d028e292f36fcba8f8b3a28d5e8932754fc2215b9ac69e4cdecf5107c6"
    "dist/images/layers-2x.png      066daca850d8ffbef007af00b06eac0015728dee279c51f3cb6c716df7c42edf"
    "LICENSE                        53e8dc25862014e4324741ca18fbe3611e11d42ef69f59f86ea8c5389647d4cb"
)

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VENDOR_DIR="${REPO_ROOT}/src/server/assets/vendor"
BASE_URL="https://unpkg.com/leaflet@${LEAFLET_VERSION}"

print_digests=0
if [ "${1:-}" = "--print-digests" ]; then
    print_digests=1
fi

workdir="$(mktemp -d)"
trap 'rm -rf "${workdir}"' EXIT

# Local path a remote path installs to: dist/ is stripped, LICENSE is renamed.
install_path() {
    case "$1" in
        LICENSE) echo "LICENSE-leaflet" ;;
        dist/*)  echo "${1#dist/}" ;;
        *)       echo "$1" ;;
    esac
}

fetched=()
for entry in "${FILES[@]}"; do
    # shellcheck disable=SC2086
    set -- ${entry}
    remote="$1"
    expected="${2:-}"

    tmp="${workdir}/$(echo "${remote}" | tr '/' '_')"
    curl -fsSL "${BASE_URL}/${remote}" -o "${tmp}"
    observed="$(sha256sum "${tmp}" | cut -d' ' -f1)"

    if [ "${print_digests}" -eq 1 ]; then
        printf '"%-30s %s"\n' "${remote}" "${observed}"
        continue
    fi

    if [ "${observed}" != "${expected}" ]; then
        echo "ERROR: sha256 mismatch for ${BASE_URL}/${remote}" >&2
        echo "  expected ${expected}" >&2
        echo "  observed ${observed}" >&2
        exit 1
    fi
    fetched+=("${remote}")
done

if [ "${print_digests}" -eq 1 ]; then
    exit 0
fi

# Only replace the vendor tree once every digest has been verified, so a failed
# update cannot leave a half-written directory behind.
rm -rf "${VENDOR_DIR}"
mkdir -p "${VENDOR_DIR}/images"
for remote in "${fetched[@]}"; do
    tmp="${workdir}/$(echo "${remote}" | tr '/' '_')"
    cp "${tmp}" "${VENDOR_DIR}/$(install_path "${remote}")"
done

# Recorded so the embedded asset version is greppable and reportable at runtime.
printf '%s\n' "${LEAFLET_VERSION}" > "${VENDOR_DIR}/VERSION"

echo "Vendored Leaflet ${LEAFLET_VERSION} (${#fetched[@]} files, all digests verified)"
