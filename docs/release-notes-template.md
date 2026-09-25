# screeny firmware <version>

Custom firmware that turns a **Tidbyt Gen 1** (ESP32 + 64x32 HUB75 panel) into a
network frame buffer. This is firmware for that specific board; a **Tidbyt Gen 2**
is different hardware and has not been tried.

## Back up the stock firmware first

This is the step that lets you change your mind. The Tidbyt's stock firmware is not
published anywhere, and it holds the device's identity for Tidbyt's service. Read it
out of the chip before writing anything else to it - it is the only way back.

Find the serial port (`ls /dev/cu.usbserial-*` on macOS, `/dev/ttyUSB*` on Linux),
then, at 230400 baud (faster rates have been seen to corrupt this link):

```bash
esptool --port <port> --baud 230400 read-flash 0 0x800000 tidbyt-stock-backup.bin
```

That should produce an 8,388,608-byte file. Keep it somewhere safe. To restore it
later: `esptool --port <port> --baud 230400 write-flash 0 tidbyt-stock-backup.bin`.

## Flash the full image

`screeny-fw-<version>-full.bin` is the bootloader, partition table and app merged
into one file, meant to be written starting at address 0:

```bash
esptool --port <port> --baud 230400 write-flash 0 screeny-fw-<version>-full.bin
```

or, with espflash:

```bash
espflash write-bin --chip esp32 --port <port> --baud 230400 --non-interactive \
  0x0 screeny-fw-<version>-full.bin
```

If the panel's test pattern comes up with the wrong colours (red and blue, or the
green channels, swapped), some Gen 1 units have their HUB75 colour lines wired the
other way round. Reflash with `screeny-fw-<version>-hdk-colours-full.bin` instead.

## Updating a panel already running screeny, over WiFi

No cable needed. `screeny-fw-<version>.bin` (or the `-hdk-colours` one, matching
whichever full image the panel is already running) is the app-only image for this:

```bash
cargo run --release -p screeny-probe -- --addr <device-ip> \
  fw-upload screeny-fw-<version>.bin --activate
```

The panel shows "updating", then "installing", then restarts. The new image is on
trial for up to three minutes before it confirms itself; if anything goes wrong, the
previous image comes back on its own.

## What the panel shows first

After flashing, the panel lights up with the **setup screen**: a QR code and a
`screeny-xxxxxx` name (the last three bytes of the device's MAC address - that is
also its mDNS name, `screeny-xxxxxx.local`). Scanning the code or joining that name's
WiFi network gets to the settings page where it joins your network. After that it
shows the **status screen** until something starts sending it frames. See
`docs/getting-started.md` in the repository for the full walkthrough.

## Checksums

`SHA256SUMS` covers every `.bin` file in this release:

```bash
shasum -a 256 -c SHA256SUMS
```
