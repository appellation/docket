#!/usr/bin/env bash
# Builds Docket.app for a channel and zips it for a release, as
# `Docket-macos-<arch>.zip` in target/release (the name `src/updater.rs`
# looks for), and prints the zip's path. Used by `scripts/release.sh` and
# the release workflow.
#
#   scripts/package.sh [--channel stable|main] [bundle.sh arguments…]
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
echo "$zip"
