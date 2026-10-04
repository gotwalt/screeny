# screeny firmware <version>

Custom firmware that turns a **Tidbyt Gen 1** (ESP32 + 64x32 HUB75 panel) into a
network frame buffer. This is firmware for that specific board; a **Tidbyt Gen 2**
is different hardware and has not been tried.

## The easy way: the web flasher

Plug the Tidbyt into a computer with a USB-C data cable and open
<https://gotwalt.github.io/screeny/> in Chrome or Edge. It installs this release and
then sets up WiFi over the same cable.

To go back to the stock firmware later, Tidbyt's support pages have the official
firmware and instructions: <https://help.tidbyt.com>.

## Flash the full image by hand

`screeny-fw-<version>-full.bin` is the bootloader, partition table and app merged
into one file, meant to be written starting at address 0. Find the serial port
(`ls /dev/cu.usbserial-*` on macOS, `/dev/ttyUSB*` on Linux) and stay at 230400 baud
or below (faster rates have been seen to corrupt this link):

```bash
esptool --port <port> --baud 230400 write-flash 0 screeny-fw-<version>-full.bin
```

or, with espflash:

```bash
espflash write-bin --chip esp32 --port <port> --baud 230400 --non-interactive \
  0x0 screeny-fw-<version>-full.bin
```

Some Gen 1 units have their HUB75 colour lines wired in the other of two orders. Once
the panel is on your network, stream a test pattern (`screeny pattern`, from the
repository's host tools) and look: if red, green and blue come out as the wrong
colours, open the panel's settings page, switch "Colour order" to the other value and
reboot. It is a stored setting, so the same image fits every unit and the choice survives
reflashing.

## Updating a panel already running screeny, over WiFi

No cable needed. `screeny-fw-<version>.bin` is the app-only image for this:

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
