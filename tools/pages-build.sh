#!/usr/bin/env bash
# Assemble the web flasher site (GitHub Pages) into an output directory.
#
# Usage: tools/pages-build.sh [--tag fw-vX.Y.Z] [--out DIR]
#   --tag  firmware release to publish (default: the newest published,
#          non-draft, non-prerelease fw-v* release)
#   --out  output directory (default: target/pages)
#
# Needs: gh (authenticated for the repo), curl, tar, openssl, shasum, python3.
# Nothing here is committed: the firmware image comes from the release, is
# checked against the release's SHA256SUMS, and is written next to a manifest
# that names its version. ESP Web Tools is vendored from the npm registry at a
# pinned version, checked against the registry's published sha512 integrity.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"

# ESP Web Tools: pinned. To bump: `npm view esp-web-tools@<v> dist.integrity`.
EWT_VERSION=10.4.0
EWT_INTEGRITY='sha512-3pwkeFFm5Fj7UQo8SJNYK5RXrtNCpq6X9QoI6bMT4GBZWgrJqjn0YvM9ihG74BtMoSFYXfmDtkehuxe50PTMPQ=='

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
cp site/index.html site/flasher.css "$OUT/"

# 1. Firmware: the merged image (bootloader + partition table + app, unpadded so
# the settings partition is never overwritten). One image for every unit.
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
IMG="screeny-fw-$VERSION-full.bin"
gh release download "$TAG" --pattern "$IMG" --pattern SHA256SUMS --dir "$WORK"
(cd "$WORK" && grep -F " $IMG" SHA256SUMS | shasum -a 256 -c -) \
  || { echo "SHA256SUMS does not match $IMG" >&2; exit 1; }
cp "$WORK/$IMG" "$OUT/firmware.bin"

# 2. Manifest. `name` must equal the firmware name the device reports over
# Improv (command 3, "screeny-fw"): ESP Web Tools compares them to tell an
# update (no erase, WiFi kept) from a first install (full erase).
python3 - "$OUT/manifest.json" "$VERSION" <<'PY'
import json, sys
out, version = sys.argv[1:3]
json.dump({
    "name": "screeny-fw",
    "version": version,
    "new_install_improv_wait_time": 20,
    "builds": [{
        "chipFamily": "ESP32",
        "improv": True,
        "parts": [{"path": "firmware.bin", "offset": 0}],
    }],
}, open(out, "w"), indent=2)
open(out, "a").write("\n")
PY

# 3. ESP Web Tools, vendored (no third-party CDN on a page that flashes devices).
curl -fsSL "https://registry.npmjs.org/esp-web-tools/-/esp-web-tools-$EWT_VERSION.tgz" -o "$WORK/ewt.tgz"
GOT="sha512-$(openssl dgst -sha512 -binary "$WORK/ewt.tgz" | openssl base64 -A)"
[[ "$GOT" == "$EWT_INTEGRITY" ]] || { echo "esp-web-tools integrity mismatch: $GOT" >&2; exit 1; }
tar -xzf "$WORK/ewt.tgz" -C "$WORK" package/dist/web package/LICENSE
mkdir -p "$OUT/vendor/esp-web-tools"
cp -R "$WORK/package/dist/web/." "$OUT/vendor/esp-web-tools/"
cp "$WORK/package/LICENSE" "$OUT/vendor/esp-web-tools/LICENSE"
touch "$OUT/.nojekyll"

echo "flasher site written to $OUT"
