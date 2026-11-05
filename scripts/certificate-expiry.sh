#!/usr/bin/env bash
# Says when a signing certificate expires, and whether that's soon. Used by
# the certificate-expiry workflow; also works on a .p12 you've exported
# from Keychain Access, with its password in CERTIFICATE_PASSWORD:
#
#   CERTIFICATE_PASSWORD=… scripts/certificate-expiry.sh [--days 60] <file>.p12
#
# Prints the expiry date of the certificate that goes with the .p12's key
# (not its chain), then `ok`, `expiring` (within --days, 60 by default), or
# `expired`:
#
#   2027-12-05 ok
set -euo pipefail

days=60
if [[ "${1:-}" == "--days" ]]; then
  days="${2:?--days needs a number of days}"
  shift 2
fi
if [[ $# -ne 1 ]]; then
  echo "usage: scripts/certificate-expiry.sh [--days N] <file>.p12" >&2
  exit 1
fi
p12="$1"
export CERTIFICATE_PASSWORD="${CERTIFICATE_PASSWORD:-}"

# Keychain Access may encrypt the certificates with RC2, which OpenSSL 3
# only reads with `-legacy`.
certificates() {
  local args=(pkcs12 -in "$p12" -nokeys -passin env:CERTIFICATE_PASSWORD "$@")
  openssl "${args[@]}" 2>/dev/null || openssl "${args[@]}" -legacy
}

# `-clcerts` is the one with the key; without one marked, the first.
certificate="$(certificates -clcerts)"
if [[ "$certificate" != *"BEGIN CERTIFICATE"* ]]; then
  certificate="$(certificates)"
fi
if [[ "$certificate" != *"BEGIN CERTIFICATE"* ]]; then
  echo "error: $p12 has no certificate in it" >&2
  exit 1
fi

expires="$(openssl x509 -noout -enddate -dateopt iso_8601 <<<"$certificate")"
expires="${expires#notAfter=}"
expires="${expires%% *}"

if ! openssl x509 -noout -checkend 0 <<<"$certificate" >/dev/null; then
  state=expired
elif ! openssl x509 -noout -checkend $((days * 86400)) <<<"$certificate" >/dev/null; then
  state=expiring
else
  state=ok
fi
echo "$expires $state"
