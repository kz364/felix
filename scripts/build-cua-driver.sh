#!/usr/bin/env bash
# Build Cua Driver (MIT, github.com/trycua/cua) for bundling inside Handy.app.
# Felix's computer tasks run it as Handy's child ("embedded" mode), so it uses
# Handy's Accessibility and Screen Recording grants.
#
#   scripts/build-cua-driver.sh [path to a cua checkout]
#
# Defaults to ../vendor/cua next to this repo. Builds with the default stable
# toolchain (the repo's pinned toolchain can fail to load proc macros).
set -euo pipefail
here="$(cd "$(dirname "$0")/.." && pwd)"
cua="${1:-$here/../vendor/cua}"
target="${CARGO_TARGET_DIR:-$HOME/.cache/cua-driver-target}"
(cd "$cua/libs/cua-driver/rust" && CARGO_TARGET_DIR="$target" cargo +stable build -p cua-driver --release)
mkdir -p "$here/src-tauri/binaries"
cp "$target/release/cua-driver" "$here/src-tauri/binaries/cua-driver-$(rustc +stable -vV | sed -n 's/^host: //p')"
echo "Bundled cua-driver $("$target/release/cua-driver" --version 2>/dev/null | head -1)"
