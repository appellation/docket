#!/usr/bin/env bash
# Builds Docket.app (macOS) in target/release, with its icon and the
# `docket://` URL scheme. Arguments go to `cargo build`, e.g.
# `scripts/bundle.sh --no-default-features` to precompile Metal shaders.
# Open the app once (or move it to /Applications) for macOS to route
# `docket://` links to it. Run from anywhere: scripts/bundle.sh
#
# `DOCKET_CHANNEL` (`stable`, the default, or `main`) is the releases the
# build updates from (`src/updater.rs`). The build also records the commit
# it's from, which main builds update by.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

channel="${DOCKET_CHANNEL:-stable}"
case "$channel" in
stable | main) ;;
*)
  echo "error: DOCKET_CHANNEL must be stable or main, not $channel" >&2
  exit 1
  ;;
esac
commit="$(git rev-parse HEAD)"

DOCKET_CHANNEL="$channel" DOCKET_COMMIT="$commit" cargo build --release "$@"

version="$(cargo metadata --no-deps --format-version 1 |
  sed -n 's/.*"name":"docket","version":"\([^"]*\)".*/\1/p')"
app="$root/target/release/Docket.app"

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp target/release/docket "$app/Contents/MacOS/docket"
cp assets/icon/docket.icns "$app/Contents/Resources/docket.icns"

cat >"$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key>
  <string>Docket</string>
  <key>CFBundleDisplayName</key>
  <string>Docket</string>
  <key>CFBundleIdentifier</key>
  <string>dev.docket</string>
  <key>CFBundleExecutable</key>
  <string>docket</string>
  <key>CFBundleIconFile</key>
  <string>docket</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleShortVersionString</key>
  <string>$version</string>
  <key>CFBundleVersion</key>
  <string>$version</string>
  <key>DocketChannel</key>
  <string>$channel</string>
  <key>DocketCommit</key>
  <string>$commit</string>
  <key>LSMinimumSystemVersion</key>
  <string>10.15.7</string>
  <key>NSHighResolutionCapable</key>
  <true/>
  <key>CFBundleURLTypes</key>
  <array>
    <dict>
      <key>CFBundleURLName</key>
      <string>dev.docket</string>
      <key>CFBundleURLSchemes</key>
      <array>
        <string>docket</string>
      </array>
    </dict>
  </array>
</dict>
</plist>
PLIST

plutil -lint "$app/Contents/Info.plist" >/dev/null
# Ad hoc, so the bundle's signature covers its Info.plist and resources.
codesign --force --sign - "$app"
echo "$app"
