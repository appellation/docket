#!/usr/bin/env bash
# Rasterizes the SVG sources in assets/icon into the PNGs and .icns the app
# ships. Uses headless Chrome (rendering each size natively rather than
# downscaling) and macOS's iconutil. Run from anywhere: scripts/render-icons.sh
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
src="$root/assets/icon"
out="$src/png"
chrome="${CHROME:-/Applications/Google Chrome.app/Contents/MacOS/Google Chrome}"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# render <svg> <output px> <out.png>
# Draws the SVG into a window of exactly the output size. (Chrome clamps
# --force-device-scale-factor at 0.5, so scaling a full-size canvas down
# doesn't work for small icons.)
render() {
  local svg="$1" px="$2" png="$3"
  local page="$work/render.html"
  printf '<html><body style="margin:0"><img src="file://%s" style="display:block;width:%dpx;height:%dpx"></body></html>' \
    "$src/$svg" "$px" "$px" >"$page"
  "$chrome" --headless=new --disable-gpu --hide-scrollbars \
    --default-background-color=00000000 \
    --force-device-scale-factor=1 \
    --window-size="$px,$px" \
    --screenshot="$png" "file://$page" >/dev/null 2>&1
}

rm -rf "$out"
mkdir -p "$out"

# macOS app icon. 16 and 32px slots use the simplified variant.
iconset="$work/docket.iconset"
mkdir -p "$iconset"
for size in 16 32 128 256 512; do
  for mult in 1 2; do
    px=$((size * mult))
    name="icon_${size}x${size}"
    [[ $mult == 2 ]] && name="${name}@2x"
    svg=docket.svg
    (( px <= 32 )) && svg=docket-small.svg
    render "$svg" "$px" "$iconset/$name.png"
  done
done
iconutil -c icns "$iconset" -o "$src/docket.icns"

# Plain PNGs for Linux/Windows packaging and general use.
for px in 16 32; do render docket-small.svg "$px" "$out/docket-$px.png"; done
for px in 64 128 256 512 1024; do render docket.svg "$px" "$out/docket-$px.png"; done

# Full-bleed square for the GitHub OAuth App logo and similar.
render docket-square.svg 1024 "$out/docket-square-1024.png"
render docket-square.svg 512 "$out/docket-square-512.png"

# Monochrome mark.
render docket-mark.svg 512 "$out/docket-mark-512.png"

# macOS menu bar template images (18pt, plus @2x).
for name in trayTemplate trayIdleTemplate; do
  render "$name.svg" 18 "$out/$name.png"
  render "$name.svg" 36 "$out/$name@2x.png"
done

echo "Wrote $src/docket.icns and $out/"
