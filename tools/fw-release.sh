#!/usr/bin/env bash
# Build both firmware colour variants, package them as GitHub Release assets,
# and cut the release. Assets go under target/fw-release/<version>/.
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
PARTS=firmware/partitions.csv
ESP_ENV="$HOME/export-esp.sh"

# 3. Build + image each colour variant. Both build to the same ELF path, so a
# variant's images must be written before the next variant's build overwrites
# it - these two steps cannot be split across a loop. Paths are computed here,
# not returned from the function, so the function's own stdout (cargo's and
# espflash's) never has to be parsed.
APP_DEFAULT="$OUT/screeny-fw-$VERSION.bin"
FULL_DEFAULT="$OUT/screeny-fw-$VERSION-full.bin"
APP_HDK="$OUT/screeny-fw-$VERSION-hdk-colours.bin"
FULL_HDK="$OUT/screeny-fw-$VERSION-hdk-colours-full.bin"

build_variant() {
  local features=$1 app=$2 full=$3
  if [[ $NO_BUILD -eq 0 ]]; then
    [[ -f "$ESP_ENV" ]] || { echo "missing $ESP_ENV (run: espup install)" >&2; exit 1; }
    # shellcheck disable=SC1090
    source "$ESP_ENV"
    echo "building firmware${features:+ (--features $features)}"
    (cd firmware && timeout 900 cargo build --release ${features:+--features "$features"})
    # The app image: what fw-upload stages into the inactive OTA slot.
    timeout 120 espflash save-image --chip esp32 --flash-size 8mb \
      --partition-table "$PARTS" "$ELF" "$app"
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
    timeout 120 espflash save-image --merge --skip-padding --chip esp32 --flash-size 8mb \
      --partition-table "$PARTS" --bootloader "$BOOTLOADER" "$ELF" "$full"
  else
    [[ -f "$app" && -f "$full" ]] || { echo "--no-build: $app / $full missing" >&2; exit 1; }
  fi
}

build_variant "" "$APP_DEFAULT" "$FULL_DEFAULT"
build_variant "panel-hdk-colours" "$APP_HDK" "$FULL_HDK"

(cd "$OUT" && shasum -a 256 screeny-fw-"$VERSION"*.bin > SHA256SUMS)

# 4. Scan each app image the way the device would; refuse the release if it
# would refuse the image.
for img in "$APP_DEFAULT" "$APP_HDK"; do
  echo "fw-scan $img"
  timeout 120 cargo run --release -p screeny-probe -- fw-scan "$img"
done

# 5. Release body + the gh command. Printed before it runs, and --dry-run
# stops before it runs at all.
BODY="$OUT/release-notes.md"
sed "s/<version>/$VERSION/g" docs/release-notes-template.md > "$BODY"

GH_CMD=(gh release create "$TAG" --title "screeny firmware $VERSION" --notes-file "$BODY")
[[ $DRAFT -eq 1 ]] && GH_CMD+=(--draft)
GH_CMD+=("$OUT"/screeny-fw-"$VERSION"*.bin "$OUT/SHA256SUMS")

printf 'would run:'; printf ' %q' "${GH_CMD[@]}"; printf '\n'
if [[ $DRY_RUN -eq 0 ]]; then
  "${GH_CMD[@]}"
else
  echo "(--dry-run: not creating a tag or a release)"
fi
