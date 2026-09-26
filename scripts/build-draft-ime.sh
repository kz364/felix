#!/usr/bin/env bash
# Build Felix Draft, the input method that shows the live draft as marked
# text in the focused field, into src-tauri/resources/draft-ime/ so it ships
# inside Felix.app. Felix installs it to ~/Library/Input Methods on first use.
#
#   APPLE_SIGNING_IDENTITY="Apple Development: …" scripts/build-draft-ime.sh
set -euo pipefail
here="$(cd "$(dirname "$0")/.." && pwd)"
src="$here/src-tauri/draft-ime"
out="$here/src-tauri/resources/draft-ime/FelixDraft.app"
rm -rf "$out"
mkdir -p "$out/Contents/MacOS" "$out/Contents/Resources"
cp "$src/Info.plist" "$out/Contents/Info.plist"
xcrun swiftc -O -target arm64-apple-macos13.0 \
  -framework Cocoa -framework InputMethodKit \
  -o "$out/Contents/MacOS/FelixDraft" "$src/main.swift"
sips -s format tiff -z 32 32 "$here/src-tauri/icons/32x32.png" \
  --out "$out/Contents/Resources/icon.tiff" >/dev/null
codesign --force --options runtime --timestamp=none \
  --sign "${APPLE_SIGNING_IDENTITY:?set APPLE_SIGNING_IDENTITY}" "$out"
echo "Built $out"
