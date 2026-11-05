#!/usr/bin/env bash
# Publishes this Mac's build of Docket.app to GitHub, for Docket to update
# itself from (`src/updater.rs`), on one of two channels:
#
#   scripts/release.sh [--channel stable|main] [bundle.sh arguments…]
#
# - stable (the default): the release for the version in Cargo.toml, tag
#   `v<version>`, from HEAD. Bump the version first: stable builds only
#   update to a higher one.
# - main: the `main-latest` prerelease, moved to the tip of origin's
#   `main`, which HEAD must be. Main builds update whenever it moves.
#
# Either creates the release if it isn't there (or, for main, is at an
# older commit), and otherwise adds or replaces this architecture's build,
# so building on an Apple silicon Mac and an Intel one fills in both. Other
# arguments go to `scripts/bundle.sh`, e.g. `--no-default-features` to
# precompile Metal shaders. Needs the GitHub CLI, signed in.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

channel=stable
if [[ "${1:-}" == "--channel" ]]; then
  channel="${2:?--channel needs stable or main}"
  shift 2
fi

if [[ -n "$(git status --porcelain)" ]]; then
  echo "error: commit or stash your changes first" >&2
  exit 1
fi
head="$(git rev-parse HEAD)"

case "$channel" in
stable)
  version="$(cargo metadata --no-deps --format-version 1 |
    sed -n 's/.*"name":"docket","version":"\([^"]*\)".*/\1/p')"
  tag="v$version"
  ;;
main)
  # Keep in sync with `MAIN_TAG` in src/updater.rs.
  tag=main-latest
  git fetch --quiet origin main
  if [[ "$head" != "$(git rev-parse FETCH_HEAD)" ]]; then
    echo "error: HEAD isn't origin's main; check it out and pull first" >&2
    exit 1
  fi
  ;;
*)
  echo "error: --channel must be stable or main, not $channel" >&2
  exit 1
  ;;
esac

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

if [[ "$channel" == main ]]; then
  # The tag moves with main, so a release at another commit goes, tag and
  # all, and comes back at this one.
  published="$(git ls-remote origin "refs/tags/$tag" | cut -f1)"
  if [[ -n "$published" && "$published" != "$head" ]]; then
    gh release delete "$tag" --cleanup-tag --yes
  fi
fi

if gh release view "$tag" >/dev/null 2>&1; then
  gh release upload "$tag" "$zip" --clobber
elif [[ "$channel" == main ]]; then
  gh release create "$tag" "$zip" \
    --target "$head" \
    --prerelease \
    --latest=false \
    --title "Docket (main)" \
    --notes "The latest build of \`main\`, at $head. Docket's main builds update from this release."
else
  gh release create "$tag" "$zip" \
    --target "$head" \
    --title "Docket $version" \
    --generate-notes
fi
gh release view "$tag" --json url --jq .url
