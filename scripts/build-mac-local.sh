#!/usr/bin/env bash
# Build a release Handy.app signed with a local Apple Development certificate,
# install it to /Applications and relaunch it.
#
# Signing with a stable certificate (instead of the default ad-hoc "-") keeps
# macOS privacy grants (Accessibility, Microphone, Input Monitoring) across
# rebuilds. Override the identity with HANDY_SIGNING_IDENTITY.
set -euo pipefail

cd "$(dirname "$0")/.."
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
export CMAKE_POLICY_VERSION_MINIMUM=3.5

identity="${HANDY_SIGNING_IDENTITY:-$(security find-identity -v -p codesigning \
  | awk -F'"' '/Apple Development/ {print $2; exit}')}"
if [[ -z "$identity" ]]; then
  echo "No Apple Development signing identity found (see: security find-identity -v -p codesigning)" >&2
  exit 1
fi
echo "Signing with: $identity"

config=$(printf '{"bundle":{"createUpdaterArtifacts":false,"macOS":{"signingIdentity":"%s"}}}' "$identity")
bun run tauri build --bundles app --config "$config"

app="src-tauri/target/release/bundle/macos/Handy.app"
codesign --verify --deep --strict "$app"

osascript -e 'tell application id "com.pais.handy" to quit' >/dev/null 2>&1 || true
pkill -f "/Applications/Handy.app/Contents/MacOS/handy" 2>/dev/null || true
sleep 1
rm -rf /Applications/Handy.app
ditto "$app" /Applications/Handy.app
open /Applications/Handy.app
echo "Installed and launched /Applications/Handy.app"
