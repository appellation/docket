#!/usr/bin/env bash
# Uploads the zips and disk images from `scripts/package.sh` to a
# channel's GitHub release, which Docket updates from (`src/updater.rs`). Used by
# `scripts/release.sh` and the release workflow. Needs the GitHub CLI,
# signed in (or `GH_TOKEN`).
#
#   scripts/publish.sh --channel stable|main <file>…
#
# - stable: the release for the version in Cargo.toml, tag `v<version>`,
#   from HEAD.
# - main: the `main-latest` prerelease, from the tip of origin's `main`,
#   which HEAD must be. A release at an older commit goes, tag and all, and
#   comes back at this one.
#
# Creates the release if it isn't there, and otherwise adds the files to
# it, replacing any of the same name.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

if [[ "${1:-}" != "--channel" ]]; then
  echo "usage: scripts/publish.sh --channel stable|main <file>…" >&2
  exit 1
fi
channel="${2:?--channel needs stable or main}"
shift 2
if [[ $# -eq 0 ]]; then
  echo "error: nothing to publish" >&2
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
  published="$(git ls-remote origin "refs/tags/$tag" | cut -f1)"
  if [[ -n "$published" && "$published" != "$head" ]]; then
    gh release delete "$tag" --cleanup-tag --yes
  fi
  ;;
*)
  echo "error: --channel must be stable or main, not $channel" >&2
  exit 1
  ;;
esac

if gh release view "$tag" >/dev/null 2>&1; then
  gh release upload "$tag" "$@" --clobber
elif [[ "$channel" == main ]]; then
  gh release create "$tag" "$@" \
    --target "$head" \
    --prerelease \
    --latest=false \
    --title "Docket (main)" \
    --notes "The latest build of \`main\`, at $head. Docket's main builds update from this release."
else
  gh release create "$tag" "$@" \
    --target "$head" \
    --title "Docket $version" \
    --generate-notes
fi
gh release view "$tag" --json url --jq .url
