---
id: 240
title: Firmware - OTA staging: stream an upload into the inactive slot, validate it, and only then let it be booted
type: build
hardware: orchestrator flashes and uploads (the worker builds, host-tests and writes the bench procedure)
depends: [210, 242, 243, 236]
owner: worker-240
branch: card/240-ota-staging
---

## Goal

The owner chose **the full plan of `docs/research/006-flash-store-ota.md`** for OTA
(decision 10 in `docs/design/device-web.md`). This card is its first half: **getting a new
image safely into the inactive slot**. `POST /api/v1/firmware` (route, reply shape and
error codes already exist in `crates/device-api`, and the simulator already serves it)
streams a raw `application/octet-stream` body into the inactive OTA slot, never buffering
it, validates it with research 006 section 5's checks, and answers. **This card does not
switch the boot slot on its own initiative beyond what 006 says staging does** - activate /
confirm / revert, the health criterion and the "updating" screen's later states are card
241. If 006's split between the two cards is different from this sentence, **006 wins**;
say so in the Log.

## Context (read first, in this order)

- `docs/research/006-flash-store-ota.md`: **Conclusions**, section 4 (flash writes with the
  display on core 1: the two rules the build cards must not break, and the open risk -
  radio vs ~12 ms sector erases), section 5 (the API, what `esp-bootloader-esp-idf` gives
  and does not give, streaming the upload, every interruption state), section 6 (rollback,
  the app-side half), section 9 (the proposed build cards - this is 240 there too).
- `docs/design/device-web.md`: decisions 3, 7, 9, 10; "How to think about storage and RAM"
  (`.bss` is core 0's stack; nothing large across an `await`); the lessons list - in
  particular **never write flash inside an HTTP handler** (how does 006 reconcile that
  with a streamed upload? the store's `deferred_task` is the existing pattern; an upload
  needs its own answer, written down) and the card 243b lesson: **every byte added to a
  reply type that `Reply` carries is paid ~12x in the handler future** - check
  `FirmwareReply`'s size against `StatusReply`'s before and after.
- `docs/design/protocol-v1.md` 8.6: the firmware route is *reserved*; this card specifies
  it there (normative prose for what you build, nothing more).
- Code: `firmware/src/http.rs` (the dispatch, `BoundedSocket` and the accept loop from
  card 236, per-route body limits, `raw_body`), `firmware/src/store.rs` (`Flash`, `Parts`,
  `read_partitions`, how flash writes are serialised with the panel), `firmware/src/spike_ota.rs`
  (the research spike), `firmware/src/panic.rs` (the breadcrumb 241 will read),
  `crates/device-api` (`FirmwareReply`, `FirmwareError`, `route::FIRMWARE`),
  `crates/sim/src/api.rs` (the simulator's upload), `crates/probe/src/http/rules.rs`
  rules 23-24 (skipped on the device today: "this firmware does not serve that route yet").
- The image format facts: this project's images are built by `espflash`; the bootloader is
  ESP-IDF v6.1 with app rollback (`firmware/bootloader/README.md`).

## Steps

1. Design note in the Log first (short): where the bytes go from socket to flash, who owns
   the flash while an upload runs, what the frame path and the panel do meanwhile
   (decision 7 allows an "updating" screen to take the panel over; dither off per 006),
   what a second concurrent upload gets, what a stalled uploader costs (a worker is pinned:
   bound it), and what each abort leaves behind. Then build it.
2. The streaming writer: sector-aligned, erase-as-you-go or erase-ahead per 006, no buffer
   bigger than 006 allows, RAM numbers in the Log (`tools/fw-size.sh`: `.stack` >= 24,576
   on all builds, 27,232 now; say what the upload costs in `.bss` and in heap).
3. The validator: all five checks of 006 section 5, each with its own `FirmwareError` and a
   host test against real and deliberately broken images (a truncated image, a wrong chip,
   a non-screeny app, a bad checksum/hash, an image larger than the slot).
4. Instrument the per-sector timing (erase and write, min/avg/max) and what the radio did
   meanwhile (frames dropped, WiFi disconnects, `esp_wifi` errors), **logged once per
   upload, not per sector**, and reported in `FirmwareReply` only if it fits without
   growing the type. This is 006's open risk; the orchestrator measures it on the device.
5. The "updating" screen while an upload is in flight (owned by `crates/provision`'s screen
   module or a sibling - one implementation), and the panel/stream behaviour around it.
6. Unskip probe rules 23-24 for the device, add rules for the new refusals that are safe to
   run against a real device (nothing that leaves a bootable-but-wrong image active), keep
   the simulator and the device answering the same. A probe subcommand or documented
   `curl` line that uploads an image file, for the orchestrator.
7. `FW_VERSION` -> "0.6.0". Spec 8.6/8.7: specify the route.

## Exit

- All firmware builds clean and over the `fw-size.sh` floor; `timeout 1200 cargo test`
  green; numbers in the Log.
- A **bench procedure** for the orchestrator, bounded and ordered: what to flash, the exact
  upload commands (a good image, then each bad image), what serial lines and replies to
  expect, how to read the per-sector timing, what would mean "stop" (e.g. WiFi drops during
  erases), and how to get back to a known-good device in every case (serial flash with
  `tools/fw-run.sh` always wins: it erases `otadata`).
- The ELF in the scratchpad as `screeny-fw-0.6.0-default.elf`, and a second, distinguishable
  build to upload (e.g. the same tree with a version suffix) as
  `screeny-fw-0.6.0-upload.bin` in whatever form the route takes - say which form and how
  you made it.

## Rules

Branch `card/240-ota-staging` from current `main`, in your own worktree; commit and Log as
you go, by explicit path; do not merge, do not push. **You do not touch hardware**: no
flashing, no serial port, no LAN, no camera - the orchestrator runs your bench procedure.
Bounded commands only; nothing left running. Never write a real SSID or password
(dummies `Example-Wifi1` / `password9`; never quote `captures/` lines containing an SSID).
Leave `crates/art` and `crates/studio` alone; `crates/device-api`, `crates/sim` and
`crates/probe` are shared with the `software` session - list every change to them in your
report. **Never erase or write outside the inactive app slot and `otadata`**; the settings
partition and the running slot are untouchable, and a test must prove the writer refuses
an offset outside its slot.

## Log

### 1. The design, before building (worker-240, 2026-09-20)

**Where the bytes go.** `POST /api/v1/firmware` is the one route with
`route::Body::Stream`, so the dispatch never calls `read_all`. The handler takes
picoserve's `RequestBodyReader`, lengthens *its* deadline
(`with_different_timeout`, because picoserve's `read_request` is 5 s and a
750 KB upload over this radio is tens of seconds), and fills **one 4,096-byte
sector-aligned buffer on the heap** - `Box<Staging>`, allocated when the upload
starts and freed when it ends. Research 006 section 5 allows exactly that
("or allocate from the heap for the life of the update"); the alternative, a
buffer in the handler's future, would be `.bss` twice over (one per HTTP worker)
and `.bss` is core 0's stack. When the buffer is full, a **synchronous**
`#[inline(never)]` function erases that one sector and writes it, so the whole
esp-storage call chain is an ordinary stack frame that is gone before the next
`await`.

**Who owns the flash.** One `AtomicBool` claim (`FirmwareError::Busy` to the
loser), and then, per sector, the existing `store::STORE` mutex - the same lock
the settings store takes. Per sector rather than for the whole upload on
purpose: a settings commit or a `SET_NAME` waits ~58 ms, not ~15 s, and
esp-storage's park/unpark granularity is a sector anyway (006 section 4). Only
core 0 ever touches flash; `multicore_auto_park` is already how the store's
`FlashStorage` was built, and this path borrows that same handle, so there is
one `FlashStorage` on the device and one discipline.

**What it can reach.** The target is a `PartitionEntry` for the **inactive** app
slot, chosen once at boot in `store::read_partitions` by comparing each app
partition's offset with `booted_partition()`'s. Every write goes through
`entry.as_flash_region(flash)`, whose `read`/`write`/`erase` are
partition-relative and bounds-checked by the crate (`partitions.rs` line 793,
`in_range`), so the writer is *structurally* unable to address the running slot,
the bootloader, the partition table or `screeny`. On top of that our own
`screeny_fwimage::plan_write` refuses an offset or length outside the slot, and
a host test drives it. **`otadata` is not touched by this card at all** - 006's
staging half never writes it; the flip is `activate_next_partition` in card 241.

**The panel.** Decision 7 lets a firmware update take the panel: while an upload
is in flight `frames_task` draws a static "updating" screen (a new
`screeny_provision::Screen::Updating`, so it is one implementation with host
tests and the simulator can draw it too) and **dither is forced off** for the
duration and restored after, per 006 section 4. With dither off the display task
sleeps and the circular DMA loops, so a 50 ms core-1 stall costs literally
nothing. A sender streaming at 30 fps keeps sending; its frames are still
drained, decoded and counted (the receiver's `Intent` is untouched, so the
source lock and the telemetry keep working) but they do **not** reach the panel -
exactly the `Portal`/`Connected` overlay rule card 223 already built. When the
upload ends the next decoded frame publishes and the picture is back; the sender
never sees an error and never has to reconnect.

**Concurrency and aborts.** A second upload while one is in flight gets
`FirmwareError::Busy` and does not touch flash. A stalled uploader is bounded
twice: a per-read timeout (`UPLOAD_STALL_MS`) and an overall deadline on the
body (`UPLOAD_TOTAL_S`); either one ends the upload, releases the claim, frees
the heap buffer and gives the panel back. Every abort leaves the same thing
behind: a partly-written inactive slot and **nothing else changed** - `otadata`
untouched, so the next boot is the running slot, and the next upload re-erases
as it goes (006 section 5's table, rows 2 and 3).

**Where 006 and this card differ, and 006 wins.** 006 section 9's card D drives
staging "from a control opcode or the serial console so it can be tested before
card 201's server exists" - card 201 has shipped, so the driver is the HTTP
route, which is what this card says. 006 card F puts the "updating" screen in a
card of its own; this card's step 5 asks for it here, and 006 section 4's
recommendation is unambiguous about what it should be, so it is built here.
Nothing in 006's *staging* half is deferred to 241: activate, confirm, revert
and the health criterion are 241's and are not built.

### 2. What was built, and the numbers

**The shape, in ten lines.** `crates/fwimage` is research 006 section 5's checks
as a scanner that is fed the upload and never holds it (240-odd bytes: a copy of
the first 112, a segment cursor, a running XOR and a SHA-256 state).
`firmware/src/ota.rs` owns one `AtomicBool` claim, one 4,096-byte four-byte-aligned
heap buffer, and a synchronous `#[inline(never)] stage_sector` that erases and
writes one sector under the settings store's own `STORE` lock and its
`multicore_auto_park` `FlashStorage`. `firmware/src/http.rs`'s `post_firmware`
reads the socket **straight into that buffer** (`Upload::spare` / `took`), so the
body crosses no other memory between smoltcp and the ROM. The target is a
`store::InactiveSlot`, made only by `read_partitions` and only after comparing an
app partition's offset with the MMU's booted one. `otadata` is not written by this
card at all. The panel shows `screeny_provision::Screen::Updating` with dither off,
and a sender keeps streaming underneath. Every upload logs two lines: per-sector
timing, and what the radio did.

**Where 006 and the card differed.** 006 section 9's card D drives staging "from a
control opcode or the serial console so it can be tested before card 201's server
exists"; 201 has shipped, so the driver is the HTTP route, which is what this card
says - no conflict. 006 card F makes the "updating" screen a card of its own; this
card's step 5 asks for it here and 006 section 4's recommendation is unambiguous
about what it should be, so it was built here. Nothing of 006's *staging* half was
deferred: activate, confirm, revert and the health criterion are 241's and are
untouched.

**Two places this departs from 006's letter, both for card 227's rule that a 4 KB
frame must not appear inside an HTTP handler:**

1. 006 section 5 names `PartitionEntry::sha256()` as "the single most valuable call
   in the crate for us". It is, and it puts a `[u8; 4096]` on the caller's stack
   (`esp-bootloader-esp-idf-0.6.0/src/partitions.rs` line 751,
   `sha256_flash_contents`). `Upload::verify_flash` does the same arithmetic over
   the same bytes with the staging buffer, which is already on the heap: read the
   staged slot back 4 KB at a time and hash it. Same check, no stack.
2. For the same reason the read-back goes through `NorFlashRegion::read`, not
   `FlashRegion::read`: the latter reaches `esp_storage::FlashStorage::read`, which
   opens with `FlashSectorBuffer::uninit()` - another 4 KB stack frame
   (`esp-storage-0.10.0/src/storage.rs` line 25; it is the 4,160-byte symbol in this
   firmware's disassembly). `read_nor` on a word-aligned destination reads straight
   into it, which is what `Sector`'s `align(4)` is for. The write side already used
   `NorFlashRegion::write` because 006 says so.

Check 6 therefore runs **twice**: once on the bytes that came off the socket (the
scan) and once on the bytes that came out of the ROM's program routine (the
read-back). The second is the one that catches a flash write that went wrong, and
it costs one extra SHA-256 pass - a few hundred milliseconds on a fifteen-second
operation.

**RAM, measured** (`tools/fw-size.sh`, floor 24,576):

| build | `.stack` before (0.5.3) | `.stack` after (0.6.0) |
|---|---|---|
| default | 27,232 | **26,944** |
| `panic-test` | - | **26,880** |
| `http-selftest` | - | **26,528** |
| `start-in-portal` | - | **26,944** |

`.bss` 110,160 -> 110,208 (**+48**), `.data` 59,212 -> 59,444 (+232), app image
971,973 -> 998,693 (+26,720 of flash, which a 2 MB slot does not notice).
`.rwtext` (IRAM) unchanged at 66,548: nothing new is `#[ram]`.

**Heap:** one 4,096-byte allocation, for the life of one upload, freed by
`Upload::drop` on every exit. Nothing else. APSTA's measured worst instant leaves
~36 KB free (card 227), so 4 KB is under 12% of the worst-case slack; if the
allocator has nothing the route answers `flash` rather than panicking.

**Core 0's stack, by the card 243b method** (frames from `objdump -d` prologues,
chained; `entry a1, N` and `addmi a1, a1, -N`, **hex included** - the first pass of
this read `entry a1, 0x100` as zero and made sha2 look free):

* `route_request`'s `poll` frame, which **every** HTTP request pays:
  **1,552 -> 2,304 bytes, +752.** That is the whole of what the route costs a
  request that is not an upload. `#[inline(never)]` on `post_firmware` was tried and
  is worse - 3,808 - because an `async fn` the caller cannot see through is
  materialised as a value in the caller's `poll` frame instead of merged into its
  state machine. Inlined is cheaper, so inlined it stays.
* The synchronous tail, only while an upload runs, longest chain first:
  - erase: `Upload::flush` 80 + `stage_sector` 144 + `FlashRegion::erase` 64 +
    `internal_erase_sector` 48 + `spiflash_erase_sector` 32 +
    `esp_rom_spiflash_erase_sector` 32 = **400 bytes**
  - write: 80 + 144 + `internal_write` 48 + `spiflash_write` 32 +
    `esp_rom_spiflash_write` 48 = **352 bytes**
  - read-back: `read_back` 112 + `spiflash_read` 32 + `esp_rom_spiflash_read` 32 =
    **176 bytes**
  - hashing: `Rehash::matches` 272 + `sha2::compress256` 256 = **528**, and
    `Scan::finish` 320 + 256 = **576 bytes**, the deepest of the four.
* So the upload path is ~2,304 + ~576 = **~2.9 KB** at its deepest, against a
  measured boot-path high-water of ~13 KB and `stack_free` of 12.4 KB after a full
  HTTP suite on 0.5.2. Nothing here is within a kilobyte of trouble, and **no 4 KB
  frame appears anywhere in it** - which is the whole point of the two departures
  above.

**Reply size** (`crates/device-api/tests/sizes.rs`,
`no_reply_is_bigger_than_the_one_on_the_hot_path`, a new test that writes card
243b's lesson down as an assertion): `FirmwareReply` is **8 bytes** against
`StatusReply`'s 208, so the firmware's `ApiBody` enum does not grow and no frame
that carries one grows either. `MAX_JSON_LEN` 57.

**What a sender streaming at 30 fps experiences** (decision 7). Nothing on the
wire: its datagrams are drained, decoded, counted and answered exactly as before,
its hold on the source is untouched, and its telemetry keeps advancing. What
changes for the ~15 seconds of an upload is that the decoded frame is **not
published to the panel** - the same overlay rule card 223 built for the portal
screen - and the panel shows "updating / do not unplug" with a progress bar.
Dither is forced off for the duration and put back afterwards, so the display task
sleeps, the circular DMA loops on the buffer it has, and the ~50 ms core-1 stall
around each sector erase costs literally nothing (research 006 section 4). When the
upload ends the next decoded frame publishes and the picture is back; a redraw is
forced on the edge so the panel cannot be left showing a finished progress bar. A
sender never learns it happened.

**The other answers the design note promised.** A second concurrent upload gets
`{"ok":false,"written":0,"error":"busy"}` and touches nothing. A stalled uploader is
bounded twice: 5 s per read (`UPLOAD_STALL`) and 180 s for the whole body
(picoserve's `with_different_timeout`, which replaces its 5 s `read_request` for
this request only) - either ends the upload, releases the claim, frees the buffer
and gives the panel back. Every abort leaves the same thing: a partly written
inactive slot and nothing else changed, so the next boot is the running slot and
the next upload re-erases as it goes.

**The validator's checks, and their tests** (`crates/fwimage`, 28 host tests):

| # | check | error | tests |
|---|---|---|---|
| 1 | `0xE9` image magic | `bad_magic` | `a_body_that_is_not_an_image_is_bad_magic`, `sixty_four_zero_bytes_are_bad_magic`, `an_empty_body_is_bad_magic`, `a_body_that_stops_inside_the_header_is_bad_magic_not_something_else` |
| 2 | chip id is ESP32 | `wrong_chip` | `an_image_for_another_chip_is_wrong_chip`, `the_chip_is_decided_from_the_first_twenty_four_bytes` |
| 3 | header says a hash is appended | `bad_sha256` | `an_image_with_no_appended_hash_is_bad_sha256` |
| 4 | `esp_app_desc.project_name` is `screeny-fw` | `wrong_project` | `somebody_elses_app_is_wrong_project`, `an_image_with_no_app_descriptor_at_all_is_wrong_project`, `the_project_is_decided_from_the_first_sector` |
| 5 | segment table walks, XOR checksum right | `bad_checksum` | `a_flipped_bit_in_a_segment_is_bad_checksum`, `a_segment_that_is_not_a_whole_number_of_words_is_bad_checksum`, `a_header_claiming_more_segments_than_the_rom_allows_is_bad_checksum` |
| 6 | appended SHA-256 matches | `bad_sha256` | `a_flipped_bit_the_checksum_cannot_see_is_bad_sha256`, `an_image_whose_appended_digest_is_wrong_is_bad_sha256`, `a_truncated_upload_is_bad_sha256`, `rehashing_the_image_gives_the_same_answer_as_the_scan` |
| - | longer than the slot | `too_large` | `an_image_longer_than_the_slot_is_too_large`, `the_slot_bound_is_enforced_on_the_stream_and_not_only_on_content_length` |
| - | **outside the slot** | `too_large` / `flash` | `a_write_outside_the_slot_is_refused`, `every_offset_the_staging_loop_produces_is_inside_the_slot` |

Six rows for 006's five because 006 folds "a hash is appended" into the hash check;
they are kept apart because one is known from byte 23 and the other only at the
end. The good image is built by the same code with nothing broken, which is what
makes "this test names the check" true, and
`the_answer_does_not_depend_on_how_the_bytes_arrive` runs the whole thing at eleven
chunk sizes from 1 byte to 64 KB, because the socket decides where the boundaries
fall. `a_real_espflash_image_passes` scans a real `espflash save-image` output when
`SCREENY_FW_IMAGE` points at one - it does: `screeny-fw-0.6.0-default.bin`, 998,752
bytes, 5 segments, version `0.6.0`. There is deliberately no 950 KB binary fixture
in a repository meant to go public; `screeny-probe fw-scan` is the command-line
form of the same check.

**The safety property, stated plainly.** The writer cannot address anything but the
inactive slot, and that is true three times over: `InactiveSlot` is a newtype only
`read_partitions` can make, and only after comparing the entry's offset with
`booted_partition()`; every write goes through that entry's `FlashRegion`, which is
partition-relative and bounds-checked by `esp-bootloader-esp-idf`
(`partitions.rs` line 793, `in_range`); and `screeny_fwimage::plan_write` refuses an
offset or length outside the slot before the ROM is asked for anything. The third is
the one a host test can drive, and `a_write_outside_the_slot_is_refused` does. The
running slot, the bootloader, the partition table and `screeny` are unreachable -
and `otadata` is not written by this card at all.

**One thing `esp_app_desc` needed.** `main.rs` used the no-argument `esp_app_desc!`,
which takes `CARGO_PKG_VERSION` - `firmware/Cargo.toml`'s `0.1.0`, which has never
moved. Every image ever built said `0.1.0`, so an upload was indistinguishable from
the running build in the one field that exists to distinguish them. It now uses the
nine-argument form with `FW_VERSION`; everything else is the macro's own default,
copied from its definition. A real image now reports `0.6.0`, and the bench's upload
image reports `0.6.1`.

**Shared crates touched:** `crates/device-api` - one new test, no shape change;
`crates/sim` - `post_firmware` runs the shared scanner and drains a refused body,
two route tests rewritten to use real images, README corrected; `crates/probe` -
rules 40-43, `Client::post_bytes_declaring`, `Client::with_timeout`,
`Ctx::post_bytes_declaring`, `fw-scan`, `fw-upload`; `crates/provision` - one new
`#[non_exhaustive]` `Screen` variant and its renderer. `crates/proto` and
`crates/receiver` are untouched, and so are `crates/art` and `crates/studio`.

**One simulator bug this found, worth carrying forward.** The simulator's HTTP
transport wrote its response and closed **without reading the rest of a streamed
body it had refused**. With a 9 KB body, ~1.3 KB were left in the kernel's receive
buffer, the close became a RST, and the client lost a reply that had already been
written. picoserve's `finalize` does the draining on the device, so the fix is the
simulator doing what the device already does: keep reading after a refusal and throw
the bytes away. It is why probe rules 40 and 41 failed on their first run against
the simulator and pass now.

**Ride-along (its own commit, asked for by the orchestrator mid-card):**
`CLOSE_ACK_MS` 500 -> 1500 in `firmware/src/http.rs`, with the measurement in its
doc comment - fw 0.5.3 logged "the peer never acknowledged the close" three times in
~20 minutes of the Studio's 10 s poll (~120 polls, RSSI ~-57 dBm), so the FIN's ACK
took over 500 ms about 2.5% of the time. That is WiFi latency - a station asleep
between beacons, an access point holding a frame for a DTIM period - not a client
misbehaving.

### 3. The bench procedure (for the orchestrator - the worker touched no hardware)

Bounded, ordered, and every step says what "stop" looks like and how to get back.
**The recovery that always works is `tools/fw-run.sh`**: it passes
`--erase-data-parts ota`, so a serial flash beats any OTA selection - and this card
never writes `otadata` anyway, so nothing here can change which slot boots.

Before anything: `backup/tidbyt-stock-*.bin` must exist and be 8,388,608 bytes
(CLAUDE.md). Turn the bench Mac's WiFi off for the flashing and conformance work
(the en0/en5 same-subnet stall, card 223's Log). Take the source lock off the
Studio and check `screeny stats` says HOLD or IDLE:

```bash
curl -s -X POST http://workbench.local:8787/api/v1/player/set \
     -H 'content-type: application/json' -d '{"device":"4a00a4","on":false}'
```

**Artefacts** (in this session's scratchpad,
`/private/tmp/claude-501/-Users-aaron-src-screeny/81d1cc11-75c9-4f5f-a521-784097e6406f/scratchpad/`):

| file | what |
|---|---|
| `screeny-fw-0.6.0-default.elf` | the build to flash (`FW_VERSION` 0.6.0) |
| `screeny-fw-0.6.0-default.bin` | the same build as an image, 998,752 bytes |
| `screeny-fw-0.6.0-upload.bin` | **the image to upload**, 998,752 bytes, identical tree except `FW_VERSION` = `0.6.1`, so `esp_app_desc.version` inside it reads `0.6.1` and card 241 will be able to tell which one booted |
| `screeny-fw-0.6.1-upload.elf` | the ELF that `.bin` came from, for symbolising a backtrace if 241 ever boots it |

Both `.bin`s were made with, from the repository root and after `. ~/export-esp.sh`:

```bash
espflash save-image --chip esp32 --flash-size 8mb \
  --partition-table firmware/partitions.csv \
  firmware/target/xtensa-esp32-none-elf/release/screeny-fw <out>.bin
```

`--flash-size 8mb` is not optional here: without it espflash assumes 4 MB and
refuses the table. The output is the **app image** - exactly the bytes the route
takes, exactly the bytes espflash would write at the slot offset.

---

**Step 0 - build and flash 0.6.0 (about 2 minutes).**

```bash
. ~/export-esp.sh
cd firmware && cargo build --release && cd ..
tools/fw-size.sh firmware/target/xtensa-esp32-none-elf/release/screeny-fw
tools/fw-run.sh                     # flashes and monitors
```

Expect `.stack 26944`, over the 24,576 floor. On the serial log, beside the usual
boot lines, **one new line** and it is the one that matters:

```
store: running from 0x10000; an upload would stage into 0x210000 (2048 KB slot)
```

*Stop if* it says `store: no inactive app slot - firmware upload is unavailable`:
the partition table is not card 210's, and every upload will answer
`unavailable`. Recovery: reflash with `tools/fw-run.sh` (it passes
`--partition-table`), and if that does not fix it the table on the device is not
what the repository says.

Then the two regression suites, unchanged from every other flash:

```bash
cargo run --release -p screeny-probe -- --addr 192.168.7.221 conformance --slow
cargo run --release -p screeny-probe -- --addr 192.168.7.221 http
```

Expect UDP **60/0/4** and HTTP **43 passed, 0 failed** - note that rules 23, 24
and the new 40-43 no longer skip. *Stop if* anything else fails: that is a
regression in this card, not an OTA finding.

---

**Step 1 - the refusals, in order, cheapest first (about 30 seconds).**

Each of these is refused **before the device erases a sector**, so they are safe
to repeat and they leave the inactive slot untouched. The probe suite already ran
them; this is the by-hand version with the serial log in view.

```bash
# a. nothing at all
curl -s -X POST --data-binary '' \
     -H 'content-type: application/octet-stream' \
     http://192.168.7.221/api/v1/firmware
# -> {"ok":false,"written":0,"error":"bad_magic"}

# b. not an image
head -c 64 /dev/zero | curl -s -X POST --data-binary @- \
     -H 'content-type: application/octet-stream' \
     http://192.168.7.221/api/v1/firmware
# -> {"ok":false,"written":0,"error":"bad_magic"}
```

Serial, for each: `ota: upload started - staging into 0x210000 (2048 KB slot),
content-length Some(0)` then `ota: upload refused: BadMagic` and the two `ota:`
report lines with `0 sectors`. The panel flickers to "updating" for a few
milliseconds and comes straight back - that is honest and expected.

*Stop if* either answers `{"ok":true,...}`. That is the validator not running, and
nothing after this step should be attempted.

---

**Step 2 - the good upload, with a stream running (about 30 seconds of upload).**

This is the measurement the card exists for. Give the panel back to the Studio
first, so the upload happens **during a live 30 fps stream** - that is research
006 section 4's open risk in its real setting:

```bash
curl -s -X POST http://workbench.local:8787/api/v1/player/set \
     -H 'content-type: application/json' -d '{"device":"4a00a4","on":true}'
sleep 10
cargo run --release -p screeny-probe -- --addr 192.168.7.221 stats   # LIVE
```

Then, watching the serial monitor and the panel:

```bash
cargo run --release -p screeny-probe -- --addr 192.168.7.221 \
  fw-upload /private/tmp/.../scratchpad/screeny-fw-0.6.0-upload.bin
```

(`curl --max-time 240 -s -X POST --data-binary @<file> -H 'content-type:
application/octet-stream' http://192.168.7.221/api/v1/firmware` is the same thing;
`fw-upload` additionally scans the file locally first, so a bad file is caught in
a millisecond instead of after fifteen seconds of flash writes, and it prints the
throughput.)

**On the panel**: "updating", "do not unplug", and a progress bar filling left to
right. **On the wire**: the reply comes only at the end, and says
`{"ok":true,"written":998752}`.

**On the serial log**, four lines and they are the deliverable:

```
ota: upload started - staging into 0x210000 (2048 KB slot), content-length Some(998752)
ota: staged image accepted - 998752 bytes, 5 segments, version "0.6.1"
ota: upload accepted - 998752 bytes in 244 sectors, NNNNN ms wall, NNNNN ms flash-busy (NN%) | erase us min/mean/max A/B/C | write us min/mean/max D/E/F | slowest sector G us
ota: the radio meanwhile - frames +N, stream lost +N, link downs +N, rssi X -> Y dBm, render max Z us, heap H of S
```

**How to read them.** `erase mean` is research 006's ~50 ms guess, measured -
expect roughly 30,000-60,000 us. `flash-busy (NN%)` is the fraction of the upload
core 0 spent inside a ROM call with interrupts masked; 006 predicts ~11 s of busy
time for a 750 KB image, so ~14 s for this one, and the percentage says how much
of the wall clock that was. **The number that answers the open risk is
`link downs +N` and `frames +N`**: `link downs +0` with `frames` still advancing
means esp-radio survived 244 masked windows during a live stream, which is what
006 asked the bench to settle. `render max` is core 1's worst frame after an
unpark; with dither off it should be small, because the display task is asleep.

*Stop if* `link downs` is not 0, or if `frames` did not advance at all. Either
means the radio did not survive the erase windows, and 006 section 4 says that
would force smaller writes or a pause-and-resume upload - a finding for card 241
and possibly a new card, not something to work around here. The device is fine
either way: nothing booted, so reboot it (`screeny-probe reboot`) and carry on.

*Stop also if* the reply is `{"ok":false,...,"error":"bad_sha256"}` on the **good**
image. That is the read-back check saying the bytes in flash are not the bytes
that arrived, i.e. a flash write that went wrong - the most interesting possible
failure and worth the serial log verbatim.

**Nothing has booted.** This firmware never writes `otadata`. `GET
/api/v1/status` still reports `fw: "0.6.0"` and `fw_slot: "ota0"`, and a reboot
brings back 0.6.0. Confirm it, because it is the card's central safety claim:

```bash
curl -s http://192.168.7.221/api/v1/status | jq '{fw, fw_slot, fw_state}'
cargo run --release -p screeny-probe -- --addr 192.168.7.221 reboot
sleep 25
curl -s http://192.168.7.221/api/v1/status | jq '{fw, fw_slot, fw_state, boot_id}'
# fw is still "0.6.0", fw_slot still ota0
```

*Stop if* `fw` comes back `0.6.1`: something wrote `otadata`, which this card does
not do. Recovery: `tools/fw-run.sh` (it erases `otadata`), then say so, loudly -
it would mean the staging path is not what this Log says it is.

---

**Step 3 - a deliberately broken good-looking image (about 20 seconds).**

The truncated case, which is the one 006's interruption table is about and the
only bad-image test that actually reaches flash:

```bash
head -c 900000 /private/tmp/.../scratchpad/screeny-fw-0.6.0-upload.bin > /tmp/trunc.bin
cargo run --release -p screeny-probe -- --addr 192.168.7.221 \
  fw-upload /tmp/trunc.bin --force
# -> {"ok":false,"written":900000,"error":"bad_sha256"}
```

`--force` is needed because `fw-upload` scans locally first and would otherwise
refuse to send it - which is the right default and exactly what has to be
overridden for this test. The panel shows "updating" for the whole of it; the
serial log's `ota:` lines report the timing for a refusal too. The inactive slot
now holds half an image, **and that is the state 006's table row 2 says is safe**:
`otadata` untouched, next boot is the running slot, next upload re-erases as it
goes. Prove it with one more reboot and a `status`.

---

**Step 4 - stop, and put the bench back.**

```bash
curl -s -X POST http://workbench.local:8787/api/v1/player/set \
     -H 'content-type: application/json' -d '{"device":"4a00a4","on":true}'
```

Turn the Mac's WiFi back on. Nothing here leaves a background process; every
`curl` is one request and `fw-upload` exits.

**Recovery, by case, in one table:**

| what went wrong | the device is | do this |
|---|---|---|
| any upload refused | untouched (or with a partial image in the slot nothing boots) | nothing; it is the designed behaviour |
| upload hung, no reply | worker released after 5 s of silence or 180 s total | wait, then `screeny-probe stats`; if it answers, carry on |
| radio dropped mid-upload (`link downs` > 0) | rejoins by itself (the provisioning machine's job) | wait ~45 s, `screeny-probe wifi`; report the numbers |
| device unreachable after an upload | almost certainly a panic - `GET /api/v1/panic` after it comes back | if it does not come back, `tools/fw-run.sh` |
| `fw` reads `0.6.1` after a reboot | booting the staged image, which this card cannot cause | `tools/fw-run.sh` (erases `otadata`), and report it |
| anything at all, last resort | - | `tools/fw-run.sh`; it always wins, because it erases `otadata` |

**What this card does NOT do, and must not be tested for**: activating the staged
image, confirming it, reverting it, or the health criterion. All four are card
241. There is no command in this firmware that makes the staged slot the one that
boots.
