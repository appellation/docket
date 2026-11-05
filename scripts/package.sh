#!/usr/bin/env bash
# Builds Docket.app for a channel and packages it for a release, in
# target/release: `Docket-macos-<arch>.zip`, which Docket updates from
# (the name `src/updater.rs` looks for), and `Docket-macos-<arch>.dmg`,
# which people install it from. Prints the zip's path, then the disk
# image's. Used by `scripts/release.sh` and the release workflow.
#
#   scripts/package.sh [--channel stable|main] [bundle.sh arguments…]
#
# `DOCKET_SIGN_IDENTITY` signs it (see `scripts/bundle.sh`). With a
# Developer ID, `DOCKET_NOTARY_KEY` (the path of an App Store Connect API
# key, `AuthKey_<id>.p8`), `DOCKET_NOTARY_KEY_ID`, and `DOCKET_NOTARY_ISSUER`
# notarize both too, and staple their tickets, so macOS opens them without
# a warning when they're downloaded in a browser.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

channel=stable
if [[ "${1:-}" == "--channel" ]]; then
  channel="${2:?--channel needs stable or main}"
  shift 2
fi

# The build records HEAD as its commit, so it must be what's built.
if [[ -n "$(git status --porcelain)" ]]; then
  echo "error: commit or stash your changes first" >&2
  exit 1
fi

# Rust's names, as `std::env::consts::ARCH` gives them.
case "$(uname -m)" in
arm64) arch=aarch64 ;;
x86_64) arch=x86_64 ;;
*)
  echo "error: unsupported architecture $(uname -m)" >&2
  exit 1
  ;;
esac

app="$(DOCKET_CHANNEL="$channel" scripts/bundle.sh "$@" | tail -n 1)"
zip="$root/target/release/Docket-macos-$arch.zip"
rm -f "$zip"
# `ditto` keeps the bundle's signature and attributes; Docket unpacks it
# with `ditto` too.
ditto -c -k --keepParent "$app" "$zip"

# Docket only updates to Developer ID builds that Gatekeeper accepts, which
# means notarized.
if [[ -n "${DOCKET_SIGN_IDENTITY:-}" && -z "${DOCKET_NOTARY_KEY:-}" ]]; then
  signer="$(security find-identity -p codesigning | grep -F -- "$DOCKET_SIGN_IDENTITY" || true)"
  if [[ "$signer" == *'"Developer ID Application:'* ]]; then
    echo "warning: not notarizing, so Docket won't update to this build; set DOCKET_NOTARY_KEY" >&2
  fi
fi

notarize() {
  echo "Notarizing ${1}…" >&2
  # It may exit with an error when it's rejected; the status says so either
  # way.
  local result status id
  result="$(xcrun notarytool submit "$1" "${notary[@]}" --wait --output-format json)" || true
  status="$(plutil -extract status raw - <<<"$result" 2>/dev/null || echo "not submitted")"
  if [[ "$status" != Accepted ]]; then
    echo "error: notarization was $status" >&2
    if id="$(plutil -extract id raw - <<<"$result" 2>/dev/null)"; then
      xcrun notarytool log "$id" "${notary[@]}" >&2 || true
    fi
    exit 1
  fi
}

if [[ -n "${DOCKET_NOTARY_KEY:-}" ]]; then
  if [[ -z "${DOCKET_SIGN_IDENTITY:-}" ]]; then
    echo "error: notarizing needs DOCKET_SIGN_IDENTITY, a Developer ID" >&2
    exit 1
  fi
  : "${DOCKET_NOTARY_KEY_ID:?notarizing needs DOCKET_NOTARY_KEY_ID}"
  : "${DOCKET_NOTARY_ISSUER:?notarizing needs DOCKET_NOTARY_ISSUER}"
  notary=(--key "$DOCKET_NOTARY_KEY" --key-id "$DOCKET_NOTARY_KEY_ID" --issuer "$DOCKET_NOTARY_ISSUER")
  notarize "$zip"
  # The ticket goes in the bundle, so it opens offline too; zipped again
  # with it, and it's this bundle that goes in the disk image.
  xcrun stapler staple "$app" >&2
  rm -f "$zip"
  ditto -c -k --keepParent "$app" "$zip"
fi

# The disk image: the app beside a link to /Applications, to drag it onto.
staging="$root/target/release/dmg"
rm -rf "$staging"
mkdir -p "$staging"
ditto "$app" "$staging/Docket.app"
ln -s /Applications "$staging/Applications"
dmg="$root/target/release/Docket-macos-$arch.dmg"
rm -f "$dmg"
# `hdiutil` sometimes finds the device busy on CI runners, so it has a few
# tries.
for attempt in 1 2 3; do
  if hdiutil create -quiet -volname Docket -srcfolder "$staging" -fs HFS+ -format UDZO -ov "$dmg"; then
    break
  fi
  if [[ "$attempt" == 3 ]]; then
    echo "error: couldn't create $dmg" >&2
    exit 1
  fi
  sleep 5
done
rm -rf "$staging"
# Signed and notarized itself too, so Gatekeeper accepts it as it's
# opened, not only the app once it's out.
if [[ -n "${DOCKET_SIGN_IDENTITY:-}" ]]; then
  codesign --force --timestamp --sign "$DOCKET_SIGN_IDENTITY" "$dmg"
fi
if [[ -n "${DOCKET_NOTARY_KEY:-}" ]]; then
  notarize "$dmg"
  xcrun stapler staple "$dmg" >&2
fi

echo "$zip"
echo "$dmg"
