#!/usr/bin/env bash
# Rebuild firmware/bootloader/esp32-rollback-bootloader.bin: the ESP-IDF second-stage
# bootloader with CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE=y, which espflash's bundled
# bootloader does not have (docs/research/006-flash-store-ota.md section 6).
# Needs docker; nothing is installed on the host. The image is ~13 GB:
#   docker rmi espressif/idf:v6.1      when you are done.
# Usage: tools/build-bootloader.sh
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
IMAGE=espressif/idf:v6.1
# Under $HOME on purpose: colima and Docker Desktop only mount the home directory.
OUT="$ROOT/target/bootloader-build"
rm -rf "$OUT"; mkdir -p "$OUT"
cp "$ROOT/firmware/bootloader/sdkconfig.defaults" "$OUT/"

docker run --rm -v "$OUT":/out "$IMAGE" bash -c '
  set -e
  cp -r "$IDF_PATH/examples/get-started/hello_world" /tmp/p && cd /tmp/p
  cp /out/sdkconfig.defaults .
  idf.py set-target esp32 >/dev/null
  idf.py bootloader >/dev/null
  grep -q "^CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE=y" sdkconfig
  cp build/bootloader/bootloader.bin /out/
'
cp "$OUT/bootloader.bin" "$ROOT/firmware/bootloader/esp32-rollback-bootloader.bin"
shasum -a 256 "$ROOT/firmware/bootloader/esp32-rollback-bootloader.bin"
