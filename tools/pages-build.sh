#!/usr/bin/env bash
# Assemble the web flasher site (GitHub Pages) into an output directory.
#
# Usage: tools/pages-build.sh [--tag fw-vX.Y.Z] [--out DIR]
#   --tag  firmware release to publish (default: the newest published,
#          non-draft, non-prerelease fw-v* release)
#   --out  output directory (default: target/pages)
#
# Needs: gh (authenticated for the repo), curl, tar, openssl, shasum, python3.
# Nothing here is committed: the firmware images (one per board) come from the
# release, are checked against the release's SHA256SUMS, and are written next to
# manifests that name their version. ESP Web Tools is vendored from the npm registry at a
# pinned version, checked against the registry's published sha512 integrity.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"

# ESP Web Tools: pinned. To bump: `npm view esp-web-tools@<v> dist.integrity`.
EWT_VERSION=10.4.0
EWT_INTEGRITY='sha512-3pwkeFFm5Fj7UQo8SJNYK5RXrtNCpq6X9QoI6bMT4GBZWgrJqjn0YvM9ihG74BtMoSFYXfmDtkehuxe50PTMPQ=='
# esptool-js (backup and restore, site/stock.js): pinned the same way.
EJS_VERSION=0.7.0
EJS_INTEGRITY='sha512-9wHuW/s9t4Vs6HKcr3pra3gbRVWwaN0eabMWodWH4KEJHEYZxgmWwZ3s0F4xcWVCZOc/L98bGZ4uWoZvR8KBKA=='

TAG="" OUT="target/pages"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --tag) TAG=$2; shift 2 ;;
    --out) OUT=$2; shift 2 ;;
    *) echo "usage: tools/pages-build.sh [--tag fw-vX.Y.Z] [--out DIR]" >&2; exit 1 ;;
  esac
done

if [[ -z "$TAG" ]]; then
  TAG=$(gh release list --exclude-drafts --exclude-pre-releases --limit 50 \
          --json tagName,publishedAt \
          --jq '[.[] | select(.tagName | startswith("fw-v"))] | sort_by(.publishedAt) | last | .tagName // empty')
  [[ -n "$TAG" ]] || { echo "no published fw-v* release found" >&2; exit 1; }
fi
VERSION=${TAG#fw-v}
echo "flasher: firmware $VERSION (tag $TAG)"

rm -rf "$OUT"
mkdir -p "$OUT/vendor"
cp site/index.html site/flasher.css site/stock.js "$OUT/"

# 1. Firmware: the merged images (bootloader + partition table + app, unpadded so
# the settings partition is never overwritten), one per board, each checked
# against the release's SHA256SUMS. The Tidbyt's keeps the original names
# (firmware.bin, manifest.json) so existing links hold. The LED Art Panel's
# (4 MB flash) sits beside it from release 0.13.0 on; an older release has
# none, and the page then offers the Tidbyt only.
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
gh release download "$TAG" --pattern SHA256SUMS --dir "$WORK"

# fetch <release asset> <published name>: download one image and verify it.
fetch() {
  local img=$1 out=$2
  gh release download "$TAG" --pattern "$img" --dir "$WORK"
  (cd "$WORK" && grep -F " $img" SHA256SUMS | shasum -a 256 -c -) \
    || { echo "SHA256SUMS does not match $img" >&2; exit 1; }
  cp "$WORK/$img" "$OUT/$out"
}

# 2. Manifests, one per board. `name` must equal the firmware name the device
# reports over Improv (command 3, "screeny-fw") on every board: ESP Web Tools
# compares them to tell an update (no erase, WiFi kept) from a first install
# (full erase). A manifest cannot tell the two boards apart (both are
# chipFamily ESP32, and ESP Web Tools matches on nothing else), so the page
# checks the chip's flash size itself (site/stock.js): the Tidbyt's backup step
# needs 8 MB, and the art panel's image is offered only after a check finds 4 MB.
manifest() {
  python3 - "$OUT/$1" "$VERSION" "$2" <<'PY'
import json, sys
out, version, image = sys.argv[1:4]
json.dump({
    "name": "screeny-fw",
    "version": version,
    "new_install_improv_wait_time": 20,
    "builds": [{
        "chipFamily": "ESP32",
        "improv": True,
        "parts": [{"path": image, "offset": 0}],
    }],
}, open(out, "w"), indent=2)
open(out, "a").write("\n")
PY
}

fetch "screeny-fw-$VERSION-full.bin" firmware.bin
manifest manifest.json firmware.bin

ART="screeny-fw-art-panel-$VERSION-full.bin"
if grep -qF " $ART" "$WORK/SHA256SUMS"; then
  fetch "$ART" firmware-art-panel.bin
  manifest manifest-art-panel.json firmware-art-panel.bin
else
  echo "flasher: $TAG has no LED Art Panel image; the page offers the Tidbyt only" >&2
fi

# 3. ESP Web Tools, vendored (no third-party CDN on a page that flashes devices).
curl -fsSL "https://registry.npmjs.org/esp-web-tools/-/esp-web-tools-$EWT_VERSION.tgz" -o "$WORK/ewt.tgz"
GOT="sha512-$(openssl dgst -sha512 -binary "$WORK/ewt.tgz" | openssl base64 -A)"
[[ "$GOT" == "$EWT_INTEGRITY" ]] || { echo "esp-web-tools integrity mismatch: $GOT" >&2; exit 1; }
tar -xzf "$WORK/ewt.tgz" -C "$WORK" package/dist/web package/LICENSE
mkdir -p "$OUT/vendor/esp-web-tools"
cp -R "$WORK/package/dist/web/." "$OUT/vendor/esp-web-tools/"
cp "$WORK/package/LICENSE" "$OUT/vendor/esp-web-tools/LICENSE"
rm -rf "$WORK/package"

# 4. esptool-js, vendored the same way: its self-contained ES module bundle.
curl -fsSL "https://registry.npmjs.org/esptool-js/-/esptool-js-$EJS_VERSION.tgz" -o "$WORK/ejs.tgz"
GOT="sha512-$(openssl dgst -sha512 -binary "$WORK/ejs.tgz" | openssl base64 -A)"
[[ "$GOT" == "$EJS_INTEGRITY" ]] || { echo "esptool-js integrity mismatch: $GOT" >&2; exit 1; }
tar -xzf "$WORK/ejs.tgz" -C "$WORK" package/bundle.js package/LICENSE
mkdir -p "$OUT/vendor/esptool-js"
cp "$WORK/package/bundle.js" "$WORK/package/LICENSE" "$OUT/vendor/esptool-js/"
touch "$OUT/.nojekyll"

echo "flasher site written to $OUT"
