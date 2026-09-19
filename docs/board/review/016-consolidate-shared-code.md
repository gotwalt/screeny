---
id: 016
title: Consolidate duplicated code (receiver core, frame types, panel model) and remove dead weight
type: build
hardware: yes
depends: [006, 008, 010]
owner: worker (card 016)
branch: card/016-consolidate-shared-code
---

## Goal

The build phase was done by parallel workers who were told not to touch each other's
crates, so the same logic now exists in several places. Before the art system merges
in and this becomes a long-lived codebase, put each thing in exactly one place.

## Context - the duplication, as found by the orchestrator

1. **Receive state machine, three copies' worth.** `crates/sim/src/core.rs` and
   `stats.rs` were hand-ported to `firmware/src/receiver.rs` and `rxstats.rs` (card
   008, with attribution; allocation strategy is the only intended difference). Any
   spec change now has to be made twice and will drift. Target: one `no_std`,
   no-alloc crate (`crates/receiver`, package `screeny-receiver`) that both `sim` and
   `firmware` use. The firmware's static-buffer/`heapless` shape is the constraint; the
   sim adapts to it, not the other way round. The sim's 97 tests and the probe's
   conformance suite are the safety net.
2. **Frame types.** `crates/demos/src/frame.rs` defines its own `Frame`/`Indexed`;
   `crates/screeny/src/frame.rs` has `Frame`; `crates/proto` owns `Rgb888Frame` /
   `IndexedFrame`. Make demos use proto's types (or screeny's thin wrapper) so
   `PieceSource` in `crates/screeny/src/main.rs` stops copying 6 KB per frame.
   NOTE: card 011 is changing `crates/screeny/src/sender.rs`, `frame.rs` and the
   README in parallel. Stay out of those files; if demos needs a type from screeny,
   depend on proto instead. Leave `main.rs`'s `PieceSource` adapter alone unless the
   change is two lines.
3. **Panel model / colour maths** exists in `crates/screeny` (`panel.rs`, `color.rs`),
   `crates/demos` (`panel.rs`, `color.rs`), `crates/sim` (panel LUT) and `lab/`.
   `lab/` is frozen - ignore it. For the three live crates: one implementation, in the
   lowest crate that makes sense (demos and sim may depend on `screeny`'s lib, or a
   small new `crates/panel`; your call, justify it). Card 071 (demos' byte estimator
   disagrees with the real encoder) dies with this: use the real encoder.
4. **Dead weight.** `spike/fw-skeleton/` is superseded by `firmware/` - delete it
   (history keeps it) and fix references. `firmware/` still has a
   `display-on-core0` A/B feature: keep (it is documented and cheap). Look for
   leftover throwaway code, unused deps (`cargo machete`-style by hand), stale
   comments that reference cards as future work that is done.
5. Tiny firmware fix while you are flashing anyway: `IDENTIFY` truncates the name one
   character early (`cut(name, 13)` where 14 fit) - card 008 left it deliberately.

## Bench rules (hardware: yes - only for verifying the firmware after the refactor)

- Read "Bench rules" in `docs/board/done/007-firmware-display.md` and
  `docs/board/done/008-firmware-network.md`. Tools by ABSOLUTE path in the main
  checkout (`/Users/aaron/src/screeny/tools/fw-run.sh <abs elf> NAME [secs]`), capture
  names prefixed `c016-`, baud <= 230400, one serial process at a time, never erase
  flash, never touch `backup/`. The device is 192.168.7.221.
- **Fast checks vs long evidence.** Iterate with host tests and the simulator. Flash
  only when the host side is green. On the device run `screeny-probe conformance`,
  `lock-test`, and ONE 60 s `stream --codec all`-style pass (or one 60 s run on the
  heaviest codec if `all` does not exist). No soaks. Never repeat a passing long run
  unless the firmware changed, and if you do, write why in the Log.
- Append results to the Log right after each run; commit after each step. Leave no
  background processes (`pgrep -lf 'screeny|espflash|qemu'`), and leave the device
  running the final firmware showing its status screen.

## Deliverables

- One receiver core used by `sim` and `firmware`; both copies in `firmware/src/`
  deleted; all sim tests pass; firmware builds; device passes probe conformance,
  lock-test and one 60 s stream with zero decode drops; flash/heap numbers before and
  after in the log (the refactor must not cost meaningful RAM).
- Frame types and panel/colour maths de-duplicated as described; card 071 closed.
- `spike/` removed, references fixed; unused dependencies removed.
- `cargo test --workspace` green; `cargo clippy --workspace` has no new warnings.
- `docs/design/architecture.md` updated to the new layout.

## Acceptance

Grepping the workspace finds one implementation each of: the receive state machine,
the panel transfer function, sRGB/Oklab conversion, the frame types. The device runs
firmware built from the shared core and behaves as it did in card 008.

## Log

### 2026-09-19 - step 1a: the shared receiver core, simulator side

New crate `crates/receiver` (`screeny-receiver`): `no_std`, no alloc, no float,
deps `screeny-proto` + `heapless`. It holds the receive state machine (sections
3.3, 4.7, 6, 7), the counters and EWMAs of 6.8, `Timing`, `State`, `DropCause`,
`ReleaseReason`, `Offer`, `Intent`, `FrameMeta` and the fixed-slot rate
limiters. Everything a receiver cannot decide for itself - the clock, where a
datagram goes, what happens to a decoded frame, whether there is a radio - is a
method on one `Host` trait. `Receiver<A>` is generic over the address type only
(`SocketAddr` here, `IpEndpoint` there), so the host can be a short-lived
borrow of an outbox.

The firmware's shape won, as the card says: the caller keeps the survivor
datagram and `offer_frame` answers `Keep`/`Drop`; `flush_frames` re-parses it
and decodes into a caller-owned `back`. The simulator adapts - `crates/sim`'s
`Core` is now a ~350-line adapter that owns the three frame buffers, the panel
model and the `Vec` outbox, and its public API is byte-for-byte what it was.

`crates/sim/src/stats.rs` is now a re-export; `config::Timing` is a re-export;
`event::{State, DropCause, ReleaseReason}` are re-exports and `Event` keeps its
owned `String` form with a `from_shared` conversion.

**No test was changed.** `cargo test --workspace`: 209 passed, 0 failed, same
as before the refactor. The sim's own count moves 97 -> 92 only because the
five EWMA unit tests moved with the code into `screeny-receiver` (97 = 92 + 5
either way).

Three deliberate, recorded behaviour differences, all of them the firmware's
shape winning:
* the simulator's three unbounded `HashMap` rate limiters become four-slot
  tables with oldest-entry eviction. Eviction can only make the device *more*
  generous to a source it forgot, which is the right direction to fail.
* `stats_req` holds two sources, not any number. Only the lock holder can have
  a frame accepted, so the second slot is for the drain in which a takeover
  happens; a third cannot arise.
* the firmware answered queued `STATS_REQ`s LIFO (`Vec::pop`), the simulator
  FIFO. The shared core is FIFO. It can only matter when two *different*
  sources ask in one drain, and only for the order of two datagrams to two
  different hosts.

One new event, `IdentifyExpired`, exists for the firmware's repaint; the
simulator drops it in `from_shared` so its event stream is unchanged.

### 2026-09-19 - step 1b: the firmware on the shared core

`firmware/src/receiver.rs` is 940 lines -> 330, and is now only what a device
has that a simulator does not: the serial log lines, the atomics the display
task reads, the fixed `Outbox`, the bench opcode `0x80` and the idle screen.
`firmware/src/rxstats.rs` (174 lines, a hand copy of the simulator's
`stats.rs` carrying the comment "the two files should be diffed if either
changes") is **deleted**. Call sites: `core.redraw` / `core.reboot_pending`
became accessor calls; nothing else in `net.rs`, `main.rs` or `mdns.rs` moved.

Also done before flashing, so that the binary on the bench is the final one:
* **card 016 item 5**: `screens::identify` used `cut(name, 13)`; 14 fit (text
  starts at x=4, the 4x6 font is 4 px wide, the chevron border starts at 62).
* **unused dependency**: `edge-nal` was in `firmware/Cargo.toml` and named
  nowhere in `firmware/src`. Removed; the build is unaffected (`edge-mdns`
  brings its own).

Firmware size, same toolchain, release profile, both built from a clean tree
(baseline rebuilt from commit df970f6 in a scratch checkout):

| | before | after | delta |
|---|---|---|---|
| flash image (text+rodata+data+rwtext+vectors+appdesc) | 740,537 B | 743,293 B | **+2,756 B (+0.37%)** |
| `.text` | 528,513 | 531,205 | +2,692 |
| `.rodata` | 73,192 | 73,064 | -128 |
| `.data` | 31,300 | 31,492 | +192 |
| `.bss` | 127,040 | 127,040 | **0** |
| core 0 main `.stack` | 37,728 | 37,536 | -192 |

`.bss` is unchanged and the 192 bytes `.data` grew come straight out of the
main stack, because architecture.md's "same pocket" is the linker giving the
stack whatever is left below `0x3ffe0000`. 37,536 B still holds the 12 KB
`FrameBuffer::new()` temporary with room to spare. The 2.7 KB of flash is the
generic `Receiver<IpEndpoint>` monomorphised plus the `Params`/`Timing`
constructor the device now runs at boot: worth it, and nowhere near a
constraint on a 8 MB part.

### 2026-09-19 - bench: the device on the shared core

Flashed `firmware/target/xtensa-esp32-none-elf/release/screeny-fw` (743,408 B
app image) with `/Users/aaron/src/screeny/tools/fw-run.sh ... c016-flash 25`,
one `espflash` at a time, 230400 baud, stock backup present, no erase. Boot log
`captures/c016-flash.log`: display on core 1, 6 planes, 154 Hz, OE slots 0..=55
(cap 25); wifi up; mDNS `screeny-4a00a4.local -> 192.168.7.221`, 10 TXT keys;
**heap 45,344 / 98,304 in use**, which is card 008's 45.4 KB unchanged.

The camera still failed: the daemon is running, but ffmpeg reports "Video
device not found / Anker PowerConf C200". Not my hardware to fix, so no stills
this card; everything below is measured over the wire instead.

`screeny-probe --addr 192.168.7.221 conformance` - **all PASS** (22 checks:
sections 2.2, 3.2, 3.3, 4.7, 5.5, 6.1, 6.2, 6.3, 6.5). Worth naming, because
they are exactly what a rewritten receive path could have broken: the three
GET_INFO rate-limit cases including the retransmission exemption, the four
frame-port reject counters, `rx = shown + superseded + decode`, the stale and
gap counters, and SET_BRIGHTNESS clamping to 160.

`screeny-probe --addr 192.168.7.221 lock-test` - **all PASS** (11 checks): the
whole of section 7.4 including BUSY rate limiting (2 packets for ~63 rejected
frames), takeover at LOCK_MS, FINAL releasing the lock, RELEASE from the same
IP, and the stream timeout.

`screeny-probe --addr 192.168.7.221 stream --codec all --fps 30 --secs 60` -
**PASS**, one run, on the final firmware:

```
sent 1801 (30.0 fps, 30 kB/s, 0 pacer skips)
frames_rx 1801 (100.00%)   frames_shown 1797 (99.78%)
stale 0   superseded 4   decode 0   rejected 0   seq_gaps 0   loss 0
interarrival/jitter 33474/4501 us   decode 517 us ewma, 2643 us max
render_us_max 3308 us   telemetry 61 replies, 0 BUSY
identity rx = shown+sup+dec  1801 vs 1801  OK
verdict PASS (no decode drops, <1% loss, nothing rejected)
```

Zero decode drops, as the card requires. The four superseded frames are
section 3.3 doing its job. Card 008's 30 fps row was 1801/1797-1800 with 0-4
superseded, so this is the same device behaving the same way.

### 2026-09-19 - steps 2 and 3: frame types, panel/colour, and card 071

**Frame types.** `crates/demos` now depends on `screeny-proto`: `Frame` is a
`Box<Rgb888Frame>`, `W`/`H`/`NPIX` are proto's constants re-exported, and
`Indexed::as_proto()` hands out proto's borrowed `IndexedFrame` with no copy.
A unit test in `demos/src/frame.rs` pins the owned and borrowed forms to the
same pixels.

**Panel and colour.** New crate `crates/panel` (`screeny-panel`), std, depends
on proto only. It owns `color` (sRGB EOTF and inverse, Oklab, OKLCH,
`cbrt_fast`, the packed-colour key, `LabCache`) and `model` (the `Panel`
transfer function and the brightness `Lut`). `crates/screeny/src/color.rs`,
`panel.rs`, `crates/demos/src/color.rs`, `panel.rs` and `crates/sim/src/panel.rs`
are now re-export shims, so every public path that existed still exists - which
matters because card 011's branch and the art system both build on
`screeny::panel` and `screeny::color`.

Why a new crate and not "demos depends on screeny's lib", which the card
offered as the other option: **it would be a dependency cycle.** `crates/screeny`
depends on `crates/demos` for the `fractal` and `clock` subcommands, so demos
cannot depend on screeny. The shared code has to go *below* both.

The three `Panel` models were the same function wearing different clothes:
screeny counted bitplanes (`steps = (2^bits - 1) * subframes`), demos counted
levels (`max = levels - 1`), and sim's `Lut` quantised against `levels - 1`
with brightness applied in linear light first. One `Panel { steps }` covers
all three - `Panel::new(6)`, `Panel::levels(64)` and `Panel::dithered(6, 1)`
are now literally equal, and a unit test says so. Verified bit-identical:
`(x + 0.5).floor()` and `.round()` agree for x >= 0, the clamps were no-ops
because `lin * scale` is already in 0..=1, and `SRGB_TO_LIN[v]` is built from
the same expression sim computed inline.

**Card 071 is closed.** The estimator is gone; `demos::stats::frame_stats`
runs `screeny_encode` - the code that will really encode the frame - and
reports its payload size and whether it is bit-exact. To make that possible
without the cycle, the encoders moved out of `crates/screeny/src/encode/` into
`crates/encode` (`screeny-encode`), which `screeny` re-exports as
`screeny::encode`, so the sender, the CLI, the benches and card 011's branch
see no change at all. `codec_name` moved with them, since a codec's name
belongs next to the codecs.

Card 010's open question is answered. Its note said the estimator "calls half
the full-colour fractal frames over budget; the lab saw 98-100% go out
exactly. One of us is wrong." The estimator was: it modelled a generic
byte-oriented LZSS rather than `PAL4_LZ`/`PAL8_LZ`, and it never tried the
block codec or the palette ladder. With the real encoder, *nothing* is ever
over budget - the encoder is not allowed to overrun - and the fractal's
full-colour path reports 398 colours mean with 3 of 24 sampled frames bit
exact, the rest fitting the budget by reducing the palette, which is exactly
what the sender does at 30 fps. The clock's indexed path is `pal4-lz` and
exact at every one of the 1440 slot/age combinations the test sweeps.

**One test changed, and here is why.** `demos/tests/clock.rs` asserted
`Wire::Exact("PAL4_LZ")`. The real encoder picks the same codec - `PAL4_LZ`,
exact - but labels it with the sender's own `codec_name` spelling,
`"pal4-lz"`. The assertion's meaning is unchanged; only the spelling of the
label is, and having two spellings of a codec name in one workspace is the
sort of thing this card exists to remove. Nothing else in any test moved.

Unexpected bonus: the demos' test suite got *faster*, because the real
encoder's fast profile beats the hand-rolled LZSS estimate it replaced
(clock 3.97 s -> 1.41 s in debug).

`cargo test --workspace`: **213 passed, 0 failed** (209 before this card; the
four new ones are the panel model's bitplanes-are-levels test, the two that
replaced sim's five moved panel tests, and demos' frame-type test).

### 2026-09-19 - step 4: dead weight

* **`spike/fw-skeleton/` deleted** (469 lines of `main.rs` plus its panel
  init, Cargo.toml and lock). `firmware/` superseded it and history keeps it.
  References fixed: the root `Cargo.toml` exclude list, and a line at the top
  of `docs/research/001-firmware-stack.md` and `004-first-bringup.md` telling
  a reader that the directory those findings describe is gone and what
  replaced it. Card 001 in `done/` is left as written - a finished card is a
  record of what happened.
* **Unused dependency**: `edge-nal` in `firmware/Cargo.toml` (removed in the
  step 1b commit, before flashing). Checked every other dependency of every
  crate by hand against grep: all of them are used.
* **Duplicate dependency**: `crates/demos` pinned `png = "0.17"` while
  `crates/sim` had `0.18.1`, so the workspace built both. Demos is on 0.18.1
  too now; the one API change is `Compression::Best` -> `Compression::High`,
  the same setting renamed.
* **Stale comments**: `encode/score.rs` said a codec might "look worse when
  card 030 lands" - card 030 has landed, so it now says the scoring was right
  in advance. `demos/frame.rs`'s "card 009 reconciles them", `demos/panel.rs`'s
  "card 020 may change how dimming works" and `screeny/color.rs`'s
  "crates/screeny will eventually own one copy" went with the files they were
  in.
* **`crates/probe/src/vectors.rs` had `let off = 1u16 - 1;`**, which is
  `clippy::eq_op`, a **deny**-level lint: `cargo clippy --workspace
  --all-targets` did not merely warn on main, it failed. It is now `0u16` with
  the bias explained in the comment.

Clippy, `--workspace --all-targets`, before and after this card:

| | errors | warnings |
|---|---|---|
| before (main) | 1 (eq_op in probe) | 7 |
| after | **0** | **7, the identical set** |

The seven are pre-existing style lints in `crates/demos`' renderers and in
`crates/probe`, in code this card did not rewrite. Nothing new was introduced:
the one warning the new code did produce (a `Default::default()` field
assignment in the new frame-type test) is fixed.

### 2026-09-19 - step 5: architecture.md, and what I did not consolidate

`docs/design/architecture.md` now describes the layout that exists: the two new
crates in the repository map, the dependency graph drawn as a line with the
`screeny -> demos` edge called out (it is the reason `panel/` and `encode/` are
crates and not modules), a "one implementation of each shared thing" section, a
receiver-core section with the drain/flush contract, a note that `screeny::Frame`
and `demos::Frame` are both just `Box<Rgb888Frame>`, the card 016 RAM figures
next to card 008's, and `spike/` gone from the tree.

#### Test counts, before and after

| crate | before | after |
|---|---|---|
| proto | 39 | 39 |
| receiver | - | **5** (the EWMA tests, moved from sim) |
| panel | - | **6** (5 moved from sim's panel.rs, 1 new) |
| encode | - | 1 doctest (moved from screeny) |
| screeny | 54 | 53 (the encode doctest left with the code) |
| demos | 16 | **17** (1 new: owned and borrowed indexed frames agree) |
| sim | 97 | **92** (5 left for receiver, 5 left for panel, 2 new local ones) |
| probe | 3 | 3 |
| **total** | **209** | **213**, 0 failed |

Nothing was deleted. The only test whose *text* changed is the clock's codec
label, explained above.

#### Things I did not consolidate, and why

1. **`PieceSource` still copies 6 KB a frame.** The card allowed two lines for
   it; removing it honestly needs `Piece::render` to take a borrowed frame,
   which is every renderer in `crates/demos`, or `screeny::Frame` to hand out
   its `Box`, which is card 011's file. Filed as **card 067** with the analysis.
2. **`crates/sim/src/screens.rs` + `font.rs` are a second status screen**, 650
   lines against `firmware/src/screens.rs`' 200, with their own hand-made
   fonts. Now that the state machine agrees about *when* the idle screen is up,
   what it *looks like* is the next place the simulator and the device can
   drift. Out of this card's scope (it named the receive state machine, the
   frame types, the panel model and the dead weight). Filed as **card 068**.
3. **The simulator's brightness model is the pre-card-020 one**: it scales
   before quantising and so predicts banding the device no longer has. Moving
   the code made the comment saying so impossible to miss. Changing the model
   changes what every sim brightness test expects, which is a card of its own -
   **card 066**.
4. **`crates/demos/src/font.rs` stays separate** from the simulator's fonts. It
   is a proportional hand-drawn font that exists because "TWENTY FIVE" does not
   fit on a 5 px monospace grid. It is art, not chrome, and merging it with a
   status-screen font would be a regression.
5. **`lab/` untouched**, as the card says: frozen reference.
6. **`firmware/src/gamma.rs` stays its own thing.** It is the integer sRGB8 ->
   duty LUT with no `f32` in it; `crates/panel` is `f32` and host-only. They
   model the same transfer function in two arithmetics on purpose, and the
   device's is the one the panel obeys.
7. **`display-on-core0`** kept, as the card instructs: documented and cheap.

#### Bench hygiene

One flash, one `conformance`, one `lock-test`, one 60 s stream. No soak, and
nothing repeated: the firmware has not changed since it was flashed (`cargo
build --release` after every later commit is a no-op), so the 60 s pass above
is a pass on the final firmware. Serial: one `espflash` at a time, 230400 baud,
stock backup verified before flashing, flash never erased, `backup/` untouched,
brightness cap untouched (the device reports 96 of a 160 cap).

**The camera never worked this session.** During the flash, `cam-request.sh`
reached the daemon but ffmpeg reported "Video device not found / Anker
PowerConf C200"; at the end of the card the daemon itself reports it is not
running. Per the card's instruction I skipped the still rather than starting
ffmpeg myself. So there is no `c016-final.jpg`. What can be said without it:
the device answers on 192.168.7.221 with `state 0 (IDLE)`, `brightness 96`,
uptime 22 minutes, and the counters from the 60 s run still standing - IDLE
with the default `Status` idle mode is the status screen, composed by the
`frames` task from the shared core's `Intent::Idle`.
