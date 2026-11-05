#!/usr/bin/env bash
# Prints the code requirement Docket signed with a certificate meets, for
# `DOCKET_NEXT_SIGNER` (`scripts/bundle.sh`). The identity is a name or
# SHA-1, as `security find-identity -p codesigning` lists them.
#
#   scripts/signer.sh "Developer ID Application: Your Name (TEAMID)"
set -euo pipefail

identity="${1:?usage: scripts/signer.sh <signing identity>}"
dir="$(mktemp -d)"
trap 'rm -rf "$dir"' EXIT

# Any code signed as Docket is: the requirement is the identifier and the
# certificate.
cp /usr/bin/true "$dir/docket"
codesign --remove-signature "$dir/docket"
codesign --identifier dev.docket --sign "$identity" "$dir/docket"
requirement="$(codesign --display --requirements - "$dir/docket" 2>/dev/null |
  sed -n 's/^designated => //p')"
if [[ -z "$requirement" ]]; then
  echo "error: $identity signs ad hoc, which no other build meets" >&2
  exit 1
fi
echo "$requirement"
