#!/bin/bash
# Builds the .deb to publish. Rust records each dependency's source path in the binary, and on a
# normal build that path includes the builder's home folder, so this rewrites it and then checks.
# It also generates the licence notices the package must carry, and refuses if a crate has none.
# usage: tools/build_release.sh
set -euo pipefail
cd "$(dirname "$0")/.."
export RUSTFLAGS="--remap-path-prefix=$HOME=/build"
notices=$(tools/third_party_notices.py)
cargo deb -p booklub-app --output dist/

package=$(ls -t dist/*.deb | head -1)
unpacked=$(mktemp -d)
trap 'rm -rf "$unpacked"' EXIT
dpkg-deb -x "$package" "$unpacked"
binary="$unpacked/usr/bin/booklub"
shipped_notices="$unpacked/usr/share/doc/booklub/THIRD-PARTY-NOTICES.txt"
if [ ! -s "$binary" ] || ! cmp -s "$notices" "$shipped_notices"; then
    echo "REFUSING: $package is missing its binary or current licence notices" >&2
    exit 1
fi
# The home folder rather than the bare username, which could match ordinary text in the binary.
for leak in "$HOME/" BOOKLUB_E2E BOOKLUB_SHOT; do
    # Search the files directly: under pipefail, a pipe into grep -q hides the match it finds.
    if grep -aqF -- "$leak" "$binary" "$shipped_notices"; then
        echo "REFUSING: $package contains '$leak'" >&2
        exit 1
    fi
done
echo "$package has licence notices, and no local paths or test hooks."
