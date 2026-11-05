#!/usr/bin/env bash
# Publishes this Mac's build of Docket.app to the GitHub release for the
# version in Cargo.toml (tag `v<version>`), which Docket updates itself
# from (`src/updater.rs`). Creates the release, from HEAD, if it doesn't
# exist; otherwise adds or replaces this architecture's build, so building
# on an Apple silicon Mac and an Intel one fills in both. Arguments go to
# `scripts/bundle.sh`, e.g. `--no-default-features` to precompile Metal
# shaders. Needs the GitHub CLI, signed in.
#
# Bump `version` in Cargo.toml first: Docket only updates to a newer
# version than it is.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

if [[ -n "$(git status --porcelain)" ]]; then
  echo "error: commit or stash your changes first" >&2
  exit 1
fi

version="$(cargo metadata --no-deps --format-version 1 |
  sed -n 's/.*"name":"docket","version":"\([^"]*\)".*/\1/p')"
tag="v$version"

# Rust's names, as `std::env::consts::ARCH` gives them.
case "$(uname -m)" in
arm64) arch=aarch64 ;;
x86_64) arch=x86_64 ;;
*)
  echo "error: unsupported architecture $(uname -m)" >&2
  exit 1
  ;;
esac

app="$(scripts/bundle.sh "$@" | tail -n 1)"
zip="$root/target/release/Docket-macos-$arch.zip"
rm -f "$zip"
# `ditto` keeps the bundle's signature and attributes; Docket unpacks it
# with `ditto` too.
ditto -c -k --keepParent "$app" "$zip"

if gh release view "$tag" >/dev/null 2>&1; then
  gh release upload "$tag" "$zip" --clobber
else
  gh release create "$tag" "$zip" \
    --target "$(git rev-parse HEAD)" \
    --title "Docket $version" \
    --generate-notes
fi
gh release view "$tag" --json url --jq .url
