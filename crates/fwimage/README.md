# screeny-fwimage

What makes an ESP32 app image one *this* device will run.

`esp-bootloader-esp-idf` will happily flip `otadata` to a slot holding an
ESP32-C3 build, somebody else's project, or half an image — it never looks
(research 006, conclusion 3). The gate has to be ours. This crate is that gate,
written once and linked by everything that needs it:

- **`firmware/`** runs it on the bytes as they come off the socket
  (`POST /api/v1/firmware`, card 240);
- **`crates/sim`** runs it on the bytes it reads and discards, so an image the
  simulator refuses is one the device refuses, with the same error code;
- **`crates/probe`** runs it before it uploads anything (`screeny-probe fw-scan`,
  `fw-upload`), and *builds* the deliberately-broken images its rules send.

`no_std`, no float, no clock, no I/O. `sha2` is the only dependency besides
`screeny-device-api`, and it is pinned to exactly the version
`esp-bootloader-esp-idf` already pulls into the firmware, so the binary carries
one SHA-256 implementation.

## The checks

Research 006 section 5's, cheapest first, each with its own
`screeny_device_api::FirmwareError`:

| # | check | error |
|---|---|---|
| 1 | byte 0 is the ESP image magic `0xE9` | `bad_magic` |
| 2 | the header's chip id is ESP32 (`0x0000`) | `wrong_chip` |
| 3 | the header says a SHA-256 is appended (byte 23) | `bad_sha256` |
| 4 | `esp_app_desc.project_name` is `screeny-fw` | `wrong_project` |
| 5 | the segment table walks and the one-byte XOR checksum is right | `bad_checksum` |
| 6 | the appended SHA-256 matches the bytes it covers | `bad_sha256` |

Plus `too_large` for anything past the slot. Six rows for 006's five because 006
folds "a hash is appended" into the hash check: an image without one cannot be
verified at all, so refusing it *is* the SHA-256 check failing.

**The first four are decidable from the first 4,096 bytes**, and that is the
property the firmware's whole design rests on: a wrong-chip or wrong-project
upload is refused before a single flash sector has been erased. `Scan::push`
answers 1–3 from the header and `Scan::check_front` answers 4 as soon as 112
bytes have arrived; the staging loop calls both before it touches flash.

## Using it

```rust
use screeny_fwimage::{Scan, SECTOR};

let mut scan = Scan::new(0x20_0000);       // the slot's length
for piece in upload.chunks(SECTOR) {        // any sizes; the socket decides
    scan.push(piece)?;
    scan.check_front()?;                    // before you erase anything
}
let image = scan.finish()?;                 // len, version, segments, digest
```

`Scan` is about 240 bytes and never holds the image: a copy of the first 112, a
segment cursor, a running XOR and a SHA-256 state. `Rehash` is the same hash
over bytes a caller supplies from somewhere else — the firmware uses it to
verify the staged slot *read back out of flash*, which is the half that catches
a flash write that went wrong.

`plan_write(slot_len, offset, len)` is the writer's bound: an offset or length
outside the slot, or one that is not a whole sector, is refused before the ROM
is asked for anything.

## Building an image (feature `build`, on by default)

`build::Builder` makes a real ESP32 image byte by byte so a test can break
exactly one thing about it — `Builder::good()`, `Builder::wrong_chip()`,
`Builder::wrong_project()`, or any field set by hand. It is the only part of
this crate that needs `alloc`, and the firmware takes the crate with
`default-features = false`.

## Tests

`cargo test -p screeny-fwimage`: 28 of them, one deliberate break per check, the
good image built by the same code with nothing broken, and the whole scan run at
eleven chunk sizes from one byte to 64 KB because the transport decides where
the boundaries fall.

Against a real image:

```sh
espflash save-image --chip esp32 --flash-size 8mb \
  --partition-table firmware/partitions.csv <elf> /tmp/fw.bin
SCREENY_FW_IMAGE=/tmp/fw.bin cargo test -p screeny-fwimage -- --nocapture
# or, the same check from the command line:
cargo run -p screeny-probe -- fw-scan /tmp/fw.bin
```

There is no checked-in binary fixture on purpose: a 950 KB blob replaced on
every firmware change does not belong in a repository meant to go public.
