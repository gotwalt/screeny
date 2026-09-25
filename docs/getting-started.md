# Getting started

From nothing to generative art on a Tidbyt. Each step says what you need, what to
run, and what you should see. If you do not have a Tidbyt yet, step 1 works without
one.

## What you need

- **A Tidbyt Gen 1.** The wooden one with a blue reset button on the back and a
  USB-C port. Gen 2 is untested; see [Gen 2](#tidbyt-gen-2) below before you try.
- **A USB-C cable that carries data.** The Tidbyt's USB port is also its serial
  console (a CP2102N bridge), so the same cable powers it, flashes it and shows its
  logs. Some charging cables have no data lines.
- **A Mac or a Linux machine** on the same WiFi network the Tidbyt will join. The
  host tools are plain Rust and build on both; the firmware toolchain is installed by
  `espup` on both. Windows is untested.
- **A phone** for the WiFi setup step (it scans a QR code on the panel).
- **Rust, stable**, via [rustup](https://rustup.rs). The root `rust-toolchain.toml`
  picks stable; the firmware directory picks the `esp` toolchain you install in step 2.

The panel is powered from the USB port the whole time. The firmware caps its
brightness and the art system limits how much of the panel is lit at once, so a
laptop port is fine.

## 1. Try it without hardware

Everything on the host side can be exercised against a simulated panel that speaks
the whole protocol, including the device's HTTP API and the captive portal.

```bash
git clone https://github.com/gotwalt/screeny && cd screeny
cargo run --release -p screeny-sim
```

A window opens showing a 64x32 grid of LEDs drawing the idle status screen. In a
second terminal:

```bash
cargo run --release -p screeny-studio
```

Open <http://127.0.0.1:8787/>. The Studio browses for panels by mDNS, finds the
simulator, adopts it, and the page shows what the simulator is showing. Pick a patch,
drag a slider, and both the page and the simulator window change together.

Or skip the Studio and drive the simulator from the command line:

```bash
cargo run --release -p screeny-art -- list                          # the patches and their parameters
cargo run --release -p screeny-art -- play flock --to screeny-sim   # stream one
cargo run --release -p screeny -- clock --name screeny-sim          # the word clock demo
cargo run --release -p screeny -- stats --name screeny-sim          # what the panel reports
```

Use `--release` for anything that streams. The art patches are per-pixel float
maths and the encoder is about ten times slower in a debug build; neither holds
30 fps unoptimised.

`cargo test` at the root runs every host crate, about three hundred tests, and needs
no hardware.

**macOS:** the first time a binary browses the network, macOS asks for Local Network
permission. An unsigned Rust binary gets a new identity on every rebuild and the
permission does not stick; if discovery keeps failing after you said yes, see
[macOS signing](#macos-local-network-permission).

## 2. Install the firmware toolchain

The firmware is `no_std` Rust for the ESP32's Xtensa core, which needs Espressif's
fork of the compiler. `espup` installs it.

```bash
cargo install espup espflash
espup install                 # installs the `esp` toolchain and writes ~/export-esp.sh
. ~/export-esp.sh             # once per shell, before building in firmware/
pip install esptool           # or: brew install esptool; pipx install esptool
```

Versions this was built with: espup 0.17, espflash 4.6, esptool 5.4. If
`cargo install espup` fails to build on your machine, the
[release binaries](https://github.com/esp-rs/espup/releases) work the same.

Check it: `cd firmware && cargo build --release` should finish with an ELF at
`firmware/target/xtensa-esp32-none-elf/release/screeny-fw`. The first build is slow
(it compiles `core` for the target).

## 3. Back up the stock firmware

This is the step that lets you change your mind. The Tidbyt's stock firmware is not
published anywhere, and it holds the device's identity for Tidbyt's service. Read it
out of the chip before writing anything.

Plug the Tidbyt in and find its serial port:

```bash
ls /dev/cu.usbserial-*        # macOS
ls /dev/ttyUSB*               # Linux (add yourself to the dialout group if it is not readable)
```

Then:

```bash
export SCREENY_PORT=/dev/cu.usbserial-XXXX     # yours
tools/backup-flash.sh "$SCREENY_PORT" backup/tidbyt-stock-mine.bin
```

It reads the whole 8 MB in retried chunks at 230400 baud (higher rates corrupted
long reads on the author's bench) and takes a few minutes. The result is 8,388,608
bytes. **Copy it somewhere outside the repository too.** The flashing script refuses to
run unless a file named `backup/tidbyt-stock-*.bin` of the right size exists, and
`backup/` is gitignored, so the file stays on your machine.

To go back to stock at any point:

```bash
esptool --port "$SCREENY_PORT" --baud 230400 write-flash 0 backup/tidbyt-stock-mine.bin
```

## 4. Build and flash

```bash
. ~/export-esp.sh
(cd firmware && cargo build --release)
tools/fw-run.sh firmware/target/xtensa-esp32-none-elf/release/screeny-fw first-boot 30
```

`fw-run.sh` flashes the bootloader (a rollback-capable build of ESP-IDF's, see
`firmware/bootloader/README.md`), the partition table and the app, then follows the
serial log for 30 seconds into `captures/first-boot.log`. The chip resets into the
bootloader over DTR/RTS; no button is needed.

What you should see: the panel lights with **the setup screen**, a QR code on the
left and `screeny-xxxxxx` beside it, where the six hex digits are the last three
bytes of the device's MAC address. That is the device's name from now on: its mDNS
name is `screeny-xxxxxx.local`. The serial log says the same, plus the free RAM and
the state of the settings store.

Never erase the flash by hand (`esptool erase-flash`); the flashing script writes
only what it needs, and the settings partition survives reflashes on purpose.

## 5. Give it WiFi

The device stores its WiFi credentials in its own flash partition. A fresh build
contains none, so it comes up as an open access point and asks.

1. On your phone, scan the QR code on the panel. It joins the open network
   `screeny-xxxxxx` (or join it by hand from the WiFi list).
2. A setup page opens by itself, as captive portals do. Pick your network, type the
   password, submit.
3. The device tries the network with the access point still up. The page reports
   either the address it got, or that the password was wrong (at once) or the network
   was not found (after three tries, about 45 seconds); nothing is saved until a join
   succeeds.
4. The panel switches to the **idle status screen**: its name, its address and its
   signal strength, with a slow ambient sweep so you can tell an idle panel from a
   frozen one. The setup network goes away thirty seconds later.

The credentials persist across reflashes and power cycles. To change networks
later, either open the device's page (next step) or **hold the button** on the back:
after a second the panel counts down, at five seconds it forgets the network and
returns to the setup screen. Letting go during the countdown cancels. A short press
shows the status screen over whatever is playing, for ten seconds.

There is no way to compile credentials into the firmware, on purpose: a build never
contains a network name or password, and the only way in is the portal, the device's
settings page, or the `SET_WIFI` control message on the LAN.

## 6. Stream to it

```bash
cargo run --release -p screeny -- discover
```

lists every panel on the network, real and simulated, with its name and addresses;
`screeny --name screeny-xxxxxx info` adds the firmware version. Then:

```bash
cargo run --release -p screeny -- --name screeny-xxxxxx pattern         # a test pattern: are the colours right?
cargo run --release -p screeny-art -- play clocks-numerals              # the first panel found
cargo run --release -p screeny-art -- play flock --to screeny-xxxxxx    # by name
cargo run --release -p screeny -- --name screeny-xxxxxx brightness 120
cargo run --release -p screeny -- --name screeny-xxxxxx stats           # fps received, drops, signal, heap
```

Look at the test pattern first. If red, green and blue come out as the wrong colours,
your unit's colour lines are in the other of the two orders seen on Gen 1 boards:
rebuild the firmware with `cargo build --release --features panel-hdk-colours` and
flash again (step 4).

`screeny stats` is the device's own account of what it is receiving. Thirty frames a
second with zero decode drops is normal on a decent WiFi link.

Any program can drive the panel: write 6144-byte frames (64x32, row-major RGB888) to
`screeny pipe`'s stdin and it paces, encodes and sends them.

The device's own page is at `http://screeny-xxxxxx.local/`: status, the network
settings, and the firmware upload form. `GET /api/v1/status` gives the same as JSON.

## 7. The Studio

Locally, as in step 1, but now it finds the real panel:

```bash
cargo run --release -p screeny-studio
```

The page at <http://127.0.0.1:8787/> has two screens. **Picture** (`/`) is the
patch, its parameters, the seed, brightness and how the panel is modelled: everything
that changes what the LEDs show, and the canvas shows the same decoded frames the
panel is being sent. **Panel** (`/panel`) is which panel, discovery, the output switch,
identify, rename, reboot, and what the device reports about itself. Save a patch's
parameters under a name and it is there next time.

To make it a service that survives reboots and runs for months, put it in Docker on a
Linux box: [`docs/design/deployment.md`](design/deployment.md) is the runbook.
The one-line version, on the host itself:

```bash
docker compose -p screeny up -d --build      # docker-compose.yml: host networking, GPU passed through
```

On a Mac, or any host without host networking, use `docker-compose.portable.yml`; mDNS
does not cross the VM boundary there, so add the panel by address on the Panel screen.

There is no password on the Studio or on the device. That is the design for a home
LAN; do not forward either port.

## 8. Update the firmware over WiFi

Once a device is on the network, a cable is optional.

```bash
. ~/export-esp.sh && (cd firmware && cargo build --release)
espflash save-image --chip esp32 --flash-size 8mb --partition-table firmware/partitions.csv \
    firmware/target/xtensa-esp32-none-elf/release/screeny-fw new.bin
cargo run --release -p screeny-probe -- --name screeny-xxxxxx fw-upload new.bin --activate
```

The image is checked before a byte is written, goes into the spare slot, and the
device restarts into it **on trial**. It confirms itself once it has an address, has
drawn a frame and has served a request, never sooner than a minute. If it crashes,
never joins or wedges, the bootloader boots the previous image within about three and
a half minutes, and `GET /api/v1/panic` says which version was rejected and why. The
cable path (`tools/fw-run.sh`) always wins if you need it.

## macOS: Local Network permission

macOS keys Local Network permission to a binary's code-signing identity. The `screeny`
CLI and `screeny-art` browse the network by mDNS, and an unsigned or ad-hoc-signed
Rust binary changes identity on every build, so macOS keeps asking or silently
refuses. If you have a Developer ID certificate:

```bash
SCREENY_SIGN_IDENTITY="Developer ID Application: Your Name (TEAMID)" tools/sign-macos.sh
```

builds and signs `target/release/screeny` with an embedded `Info.plist` (the usage
string and the Bonjour service type), and the permission sticks. Without a certificate,
`--addr` works for everything: `cargo run --release -p screeny -- --addr 192.168.1.50 stats`.

## Tidbyt Gen 2

Untested, and expected to need small changes in `firmware/`. From Tidbyt's published
firmware (`tidbyt/hdk`), Gen 2 differs in:

- **The HUB75 pin map.** Every signal moves; the Gen 2 column of the table in
  [`research/001-firmware-stack.md`](research/001-firmware-stack.md) has them. The
  constants live in `firmware/src/tidbyt.rs` and the pin block in `firmware/src/main.rs`.
- **Clock phase.** Gen 2 runs the panel with the opposite pixel-clock phase
  (`clkphase false` in Tidbyt's driver config). `esp-hub75` has a feature for this.
- **The button.** Gen 2 has a capacitive touch pad on GPIO33 (which is Gen 1's HUB75
  clock) instead of a mechanical button on GPIO15. `firmware/src/button.rs` will need
  a touch reading instead of a GPIO edge, or the button gestures can be dropped.
- **Board identification.** The stock firmware reads two ADC straps (GPIO13, GPIO15)
  to tell generations and revisions apart; this firmware does not, it is built for
  one board.

The chip is the same ESP32 family, so the toolchain, the partition layout, the
bootloader and everything above the pin map should carry over. If you get a Gen 2
running, a pull request with the pin map behind a cargo feature would be very welcome.

## When something is off

- **The panel does not answer `ping`.** By design; it answers ARP, UDP and HTTP.
- **`discover` finds nothing on macOS** but `--addr` works: Local Network permission,
  see above. Also, the very first browse after signing a new binary has been seen to
  return nothing once; try again.
- **Serial corrupts or the flash fails partway.** Stay at 230400 baud; the scripts do.
  Try another cable or port.
- **The setup page does not open on the phone.** Open any http (not https) address in
  the phone's browser while joined to `screeny-xxxxxx`; every name resolves to the
  device. iOS opens the page by itself, Android sometimes needs a tap on the
  "sign in to network" notification.
- **Colours are wrong on the test pattern.** The other pin order: step 6.
- **A patch shows black in the Studio in Docker.** The GPU patches need a Vulkan
  device. On Linux pass `/dev/dri` (the default compose file does); on a Mac there is
  none, so build with `SCREENY_FEATURES=none` for the CPU patches only.
- **You want the stock firmware back.** Step 3's last command.
