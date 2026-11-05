#!/usr/bin/env bash
# Builds this Mac's Docket.app and publishes it to a channel's GitHub
# release, by hand; `.github/workflows/release.yml` does the same for both
# architectures on every push to main and every published release.
#
#   scripts/release.sh [--channel stable|main] [bundle.sh arguments…]
#
# See `scripts/package.sh` and `scripts/publish.sh`. Bump the version in
# Cargo.toml before a stable release: stable builds only update to a
# higher one.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

channel=stable
if [[ "${1:-}" == "--channel" ]]; then
  channel="${2:?--channel needs stable or main}"
  shift 2
fi

zip="$(scripts/package.sh --channel "$channel" "$@" | tail -n 1)"
scripts/publish.sh --channel "$channel" "$zip"
