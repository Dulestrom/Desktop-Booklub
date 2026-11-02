#!/bin/bash
# Builds Booklub from this source and installs it, on Ubuntu 26.04 or newer.
# usage: ./install.sh
set -euo pipefail
cd "$(dirname "$0")"

echo "Installing what the build needs. This may ask for your password."
sudo apt install -y rustc cargo libgtk-4-dev libadwaita-1-dev libsqlite3-dev pkg-config build-essential fonts-manrope

if ! pkg-config --atleast-version=4.22 gtk4 || ! pkg-config --atleast-version=1.9 libadwaita-1; then
    echo "Booklub needs GTK 4.22 and libadwaita 1.9 or newer, which Ubuntu 26.04 provides." >&2
    echo "BUILDING.md has notes for other systems." >&2
    exit 1
fi

export PATH="$HOME/.cargo/bin:$PATH"
if ! command -v cargo-deb >/dev/null; then
    echo "Installing cargo-deb, which makes the package."
    cargo install cargo-deb --locked
fi

echo "Building Booklub. The first build takes a few minutes."
tools/build_release.sh

package=$(ls -t dist/*.deb | head -1)
sudo apt install -y "./$package"
echo "Booklub is installed. Look for it in your application grid."
