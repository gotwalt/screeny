#!/usr/bin/env bash
# Build the firmware for every board, package it as GitHub Release assets, and
# cut the release. Assets go under target/fw-release/<version>/.
#
# Boards (card 377; each image carries its board in esp_app_desc.idf_ver and
# firmware from 0.13.0 refuses any other):
#   tidbyt     every Gen 1 unit (one image since card 361: the colour order is a
#              stored setting). 8 MB, firmware/partitions.csv.
#              screeny-fw-<v>.bin, screeny-fw-<v>-full.bin (names unchanged).
#   art-panel  the LED Art Panel: --features board-art-panel,
#              ESP_HAL_CONFIG_MIN_CHIP_REVISION=100 (its ESP32 is rev 1.0),
#              4 MB, firmware/partitions-4mb.csv.
#              screeny-fw-art-panel-<v>.bin, screeny-fw-art-panel-<v>-full.bin.
#
# Usage: tools/fw-release.sh [--dry-run] [--draft] [--no-build]
#   --dry-run  print every command; refuse nothing about the tag, branch or
#              working tree; still build and write images (so the assets can
#              be inspected), but never create a tag or a release.
#   --draft    create the GitHub release as a draft.
#   --no-build skip the cargo build + espflash steps and reuse images already
#              under target/fw-release/<version>/ (for iterating on fw-scan /
#              the gh command without rebuilding).
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"

DRY_RUN=0 DRAFT=0 NO_BUILD=0
for arg in "$@"; do
  case "$arg" in
    --dry-run) DRY_RUN=1 ;;
    --draft) DRAFT=1 ;;
    --no-build) NO_BUILD=1 ;;
    *) echo "usage: tools/fw-release.sh [--dry-run] [--draft] [--no-build]" >&2; exit 1 ;;
  esac
done

# 1. Version and tag. FW_VERSION (not Cargo.toml's, see the comment above it
# in firmware/src/main.rs) is what every artifact and the device itself call
# this build.
VERSION=$(grep -oE 'FW_VERSION: &str = "[0-9]+\.[0-9]+\.[0-9]+"' firmware/src/main.rs \
  | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' || true)
[[ -n "$VERSION" ]] || { echo "could not read FW_VERSION from firmware/src/main.rs" >&2; exit 1; }
TAG="fw-v$VERSION"
echo "firmware version: $VERSION (tag $TAG)"

# 2. Refusals, skipped in --dry-run so it can run from any branch/state.
if [[ $DRY_RUN -eq 0 ]]; then
  if git rev-parse --verify --quiet "refs/tags/$TAG" >/dev/null; then
    echo "refusing: tag $TAG already exists locally" >&2; exit 1
  fi
  if git ls-remote --tags origin "refs/tags/$TAG" | grep -q "$TAG"; then
    echo "refusing: tag $TAG already exists on origin" >&2; exit 1
  fi
  [[ -z "$(git status --porcelain)" ]] || { echo "refusing: working tree is dirty" >&2; exit 1; }
  [[ "$(git rev-parse --abbrev-ref HEAD)" == "main" ]] || { echo "refusing: HEAD is not on main" >&2; exit 1; }
else
  echo "(--dry-run: not checking the tag, branch or working tree)"
fi

OUT="target/fw-release/$VERSION"
mkdir -p "$OUT"
ELF=firmware/target/xtensa-esp32-none-elf/release/screeny-fw
BOOTLOADER=firmware/bootloader/esp32-rollback-bootloader.bin
ESP_ENV="$HOME/export-esp.sh"

# 3. Build + image the firmware. Paths are computed here, not returned from the
# function, so the function's own stdout (cargo's and espflash's) never has to
# be parsed.
APP_DEFAULT="$OUT/screeny-fw-$VERSION.bin"
FULL_DEFAULT="$OUT/screeny-fw-$VERSION-full.bin"
APP_ART="$OUT/screeny-fw-art-panel-$VERSION.bin"
FULL_ART="$OUT/screeny-fw-art-panel-$VERSION-full.bin"

# build_variant <features> <min-chip-rev or ""> <flash size> <partition table> <app> <full>
# The ELF path is shared, so each variant is imaged before the next is built.
build_variant() {
  local features=$1 min_rev=$2 flash=$3 parts=$4 app=$5 full=$6
  if [[ $NO_BUILD -eq 0 ]]; then
    [[ -f "$ESP_ENV" ]] || { echo "missing $ESP_ENV (run: espup install)" >&2; exit 1; }
    # shellcheck disable=SC1090
    source "$ESP_ENV"
    echo "building firmware${features:+ (--features $features)}${min_rev:+ (MIN_CHIP_REVISION=$min_rev)}"
    (cd firmware && ${min_rev:+env ESP_HAL_CONFIG_MIN_CHIP_REVISION="$min_rev"} \
      timeout 900 cargo build --release ${features:+--features "$features"})
    # The app image: what fw-upload stages into the inactive OTA slot.
    timeout 120 espflash save-image --chip esp32 --flash-size "$flash" \
      --partition-table "$parts" "$ELF" "$app"
    # The full image: bootloader + partition table + app, merged so it can be
    # written at flash address 0 in one shot. espflash pads the gaps between
    # segments with 0xFF, including 0xE000..0x10000, which is the otadata
    # partition (see partitions.csv) - so this file carries a *clean* otadata,
    # the same "nothing selected yet" state tools/fw-run.sh gets by passing
    # --erase-data-parts ota. A device booting this image always starts ota_0,
    # no extra erase step needed. --skip-padding: without it espflash pads the
    # file out to the full 8 MB, and writing that would also erase ota_1 and
    # the settings partition at 0x410000 - the stored WiFi credentials - on
    # every reflash. The file ends where the app ends, and everything past it
    # on the chip is left alone.
    timeout 120 espflash save-image --merge --skip-padding --chip esp32 --flash-size "$flash" \
      --partition-table "$parts" --bootloader "$BOOTLOADER" "$ELF" "$full"
    # Card 378: keep the ELF beside the images, to symbolise a device's
    # `last_panic.what` pc later. Local only: ~14 MB of debug info, and the
    # uploads below take *.bin.
    cp "$ELF" "${app%.bin}.elf"
  else
    [[ -f "$app" && -f "$full" ]] || { echo "--no-build: $app / $full missing" >&2; exit 1; }
  fi
}

build_variant "" "" 8mb firmware/partitions.csv "$APP_DEFAULT" "$FULL_DEFAULT"
build_variant board-art-panel 100 4mb firmware/partitions-4mb.csv "$APP_ART" "$FULL_ART"

(cd "$OUT" && shasum -a 256 screeny-fw-*"$VERSION"*.bin > SHA256SUMS)

# 4. Scan each app image the way the device would, and check it names the
# board its file name says; refuse the release otherwise. A Tidbyt image under
# the art panel's name is exactly what card 377 exists to stop.
for pair in "tidbyt:$APP_DEFAULT" "art-panel:$APP_ART"; do
  board=${pair%%:*} img=${pair#*:}
  echo "fw-scan $img (expect board $board)"
  scan=$(timeout 120 cargo run -q --release -p screeny-probe -- fw-scan "$img") || { echo "$scan" >&2; exit 1; }
  echo "$scan"
  grep -q "^  board $board " <<<"$scan" || { echo "refusing: $img is not tagged board=$board" >&2; exit 1; }
done

# 5. Release body + the gh command. Printed before it runs, and --dry-run
# stops before it runs at all.
BODY="$OUT/release-notes.md"
sed "s/<version>/$VERSION/g" docs/release-notes-template.md > "$BODY"

GH_CMD=(gh release create "$TAG" --title "screeny firmware $VERSION" --notes-file "$BODY")
[[ $DRAFT -eq 1 ]] && GH_CMD+=(--draft)
GH_CMD+=("$OUT"/screeny-fw-*"$VERSION"*.bin "$OUT/SHA256SUMS")

printf 'would run:'; printf ' %q' "${GH_CMD[@]}"; printf '\n'
if [[ $DRY_RUN -eq 0 ]]; then
  "${GH_CMD[@]}"
else
  echo "(--dry-run: not creating a tag or a release)"
fi
