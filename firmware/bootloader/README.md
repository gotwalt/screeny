# The second-stage bootloader

`esp32-rollback-bootloader.bin` (26,192 bytes) is Espressif's ESP-IDF **v6.1**
second-stage bootloader for the ESP32, built from the unmodified `hello_world` example
with the two lines in `sdkconfig.defaults`. It is Apache-2.0 licensed output of ESP-IDF,
the same thing `espflash` bundles, with one option changed:

    CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE=y

Why: espflash's bundled bootloader selects OTA slots but never rolls back. With this
option the bootloader marks a newly activated image `PENDING_VERIFY` on its first boot
and, if the image has not confirmed itself by the next reset, marks it `ABORTED` and
boots the previous slot. That is the only thing that rescues a device from an update
that crashes before its own health check can run
(`docs/research/006-flash-store-ota.md` section 6).

After a serial flash (`tools/fw-run.sh` clears `otadata`) the bootloader writes
`otadata[0]` as `VALID`, so firmware that never calls "confirm" is unaffected; only
images activated by an OTA go through `NEW -> PENDING_VERIFY`.

    sha256 528eddf566b4285e6cbe86f54147a54d29a8c42ab23c9e15e08c9c10a9d4beba

Rebuild with `tools/build-bootloader.sh` (docker only, nothing installed on the host;
the `espressif/idf:v6.1` image is about 13 GB - remove it afterwards). espflash patches
the flash size/mode/frequency bytes in the header when it writes the file, so the
stock 2 MB / DIO / 40 MHz header in the blob is not what ends up on the chip.
