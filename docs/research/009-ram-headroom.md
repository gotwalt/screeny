# 009 - Firmware RAM headroom: the stack the framebuffers were eating, and what APSTA costs

Card 220. Every number here was printed either by `xtensa-esp32-elf-size -A` for a
build in this worktree, or by the device itself over the serial port on the card 210
partition table. Card 201 (`docs/research/007-device-web-and-portal.md`) is the
research this answers; sections 6 and 2 there are the parts that were waiting on it.

---

## Conclusions first

1. **The framebuffers were eating 20 KB of core 0's stack, and now they are not.**
   `DmaFrameBuffer::new()` turns out to be a `const fn`, so two
   `ConstStaticCell`s let the compiler produce the 12 KB values instead of
   `main` materialising them as stack temporaries. Measured on the device, core
   0's main-stack high-water went from **26,508 bytes to 6,304** of a 37,512-byte
   region. There is no `unsafe` in the fix.
2. **Card 201's "this build would very likely panic on the stack guard" was right,
   and it was not close.** The full web spike would have left `.stack` at 13,688
   bytes. The firmware as it stood touched **26,508**. It would have died at boot,
   in `main`, every time.
3. **APSTA costs 3,328 bytes of heap. That is all.** Station-only 45,540 used of
   98,304; APSTA idle 48,868. esp-alloc's all-allocations watermark moved by
   exactly the same 3,328 (50,640 -> 53,968), so the worst moment of the run still
   left **44,336 bytes free**. `esp-radio`'s own documentation suggests 47-57 KB
   for a station and 53-63 KB for an open AP; the two together are nowhere near
   additive, which is the number nobody had.
4. **Keep the heap at 64 + 32 KB.** Do not take card 201's lever 3. There was
   never a heap problem: at the tightest instant of an APSTA run, 45% of the heap
   is free. The problem was the stack, and the stack is fixed.
5. **The web track fits, with room.** Card 201's attribution wants ~17.1 KB more
   `.bss` for picoserve + DHCP/DNS + the AP stack + the QR encoder, which comes
   straight off `.stack` and leaves it at ~20.4 KB against a measured demand of
   6,304. Flash goes to ~880 KB of a 2 MB slot.
6. **One thing to watch, not yet explained: the station's RSSI fell 24 dB at the
   moment the soft-AP came up** (-53 dBm for the whole station-only stage,
   -76..-78 dBm for the whole APSTA stage) and stayed there. The stream did not
   care - 30 fps, zero drops, unchanged render time - but -78 dBm is not a lot of
   margin to give away, and the portal is exactly the feature that needs the
   station to associate reliably. Observed once, in one boot; the cause is not
   established. See "Open questions".
7. **A probe that measures the stack must not be inlined into the frame it is
   measuring.** The first flash of this card panic-looped because it was. The
   method is in section 2 and the mistake is worth the paragraph.

---

## 1. The framebuffers

`main` built its two DMA framebuffers like this:

```rust
let fb0 = mk_static!(FrameBuffer, FrameBuffer::new());
let fb1 = mk_static!(FrameBuffer, FrameBuffer::new());
```

`mk_static!` is `StaticCell::uninit().write(value)`, so the value has to exist
*before* it can be moved into the static - as a 12,312-byte temporary, on
whatever stack is running the constructor. The comment at the site already knew
this ("core 1's 16 KB stack will not survive twice - the first flash of this
firmware died exactly there"); the response had been to build them on core 0's
stack instead, which is larger, and stop thinking about it.

The vendored `hub75-framebuffer` makes the real fix trivial.
`bitplane::plain::frame::DmaFrameBuffer::new()` - which is what
`esp_hub75::framebuffer::bitplane::plain::DmaFrameBuffer` re-exports - is a
`const fn`: an all-zero `PlaneData` array followed by the `const fn format()`
that writes the row-address, latch and blanking bits. So:

```rust
static FB0: static_cell::ConstStaticCell<FrameBuffer> =
    static_cell::ConstStaticCell::new(FrameBuffer::new());
```

The compiler produces the value. Nothing constructs anything at runtime, and
`main` only calls `take()`.

They still come up dimmed the way the old comment demanded: `new()` formats for
the widest output-enable window the build can produce, which is well over the
power cap, so `main` calls `set_oe_slots` on both before `Hub75::new_async`
touches either. One refresh at that duty is a current spike, and the panel runs
off laptop USB.

### What it did and did not move

| build | `.text` | `.rodata` | `.data` | `.bss` | `.stack` | flash image |
|---|---|---|---|---|---|---|
| `main` at a2536bc | 531205 | 73064 | 31492 | 127040 | 37536 | 743408 |
| branch, `fb-on-stack` | 531621 | 73248 | 31492 | 127064 | 37512 | 744048 |
| branch, **default** | 531209 | 73176 | **56124** | **102424** | **37512** | 768192 |
| branch, `apsta-probe` | 534797 | 74184 | 56164 | 106432 | 33472 | 772800 |

24,632 bytes moved from `.bss` into `.data`, and **`.stack` did not change**.
That is not a disappointment, it is the mechanism: `.data`, `.bss` and core 0's
main stack are laid out back to back in one DRAM region ending at 0x3ffe0000,
and `.stack` is the remainder. Moving bytes from one section to its neighbour
cannot change what is left over.

What changes is the **transient**: what the remainder has to be big enough for,
which is the only thing that ever mattered. Section 2 has that number.

The price is 24 KB of flash, because a `.data` section is initialised from the
image: 744,048 -> 768,192 bytes, 36.6% of a 2 MB OTA slot. If that ever becomes
the binding constraint, the same 24 KB can go back into `.bss` by leaving the
static zeroed and calling the `const fn format()` on it in place - all-zero
bytes are a valid `FrameBuffer` - at the cost of one `unsafe` block. It is not
worth it today.

`fb-on-stack` is kept as a cargo feature so the A/B is reproducible without
checking out an older commit, in the same spirit as `display-on-core0`.

---

## 2. Measuring the stack, and getting it wrong first

`esp-rtos 0.4.0` has no per-task stack watermark. It snapshots one word at
`stack_bottom + ESP_HAL_CONFIG_STACK_GUARD_OFFSET` and compares it on every
context switch, which tells you the stack has been blown, not how close it came.
So: paint and scan (`firmware/src/stack_probe.rs`).

`_stack_end_cpu0` (0x3ffd6d60, the same address as `_bss_end`) and
`_stack_start_cpu0` (0x3ffe0000) bracket the region. The paint runs as the first
statement of `main`, filling from `bottom + 1024` - leaving the guard word at
`bottom + 60` untouched - up to just below the current frame. The scan runs once,
at the 60 s telemetry tick, and reports `_stack_start_cpu0` minus the address of
the first word that is no longer the pattern. Interrupts are included for free:
on this chip an ISR runs on whatever stack is current.

It is in the **default** firmware, not behind a feature. One pass over 36 KB at
boot and one at 60 s is not a cost worth optimising, and a stack number is only
worth having if it is the number the shipping build produces.

### The mistake

The first version bounded the paint at "the address of a local in this function,
minus 512 bytes". That is only below the frame if the frame is small. `paint()`
was inlined into `main`'s poll function - and in the `fb-on-stack` build that
frame *is* the 24 KB of framebuffer temporaries this card exists to remove. The
local landed near the top of it, so the bound was thousands of bytes **above**
the stack pointer, and the paint went through the live frame and its saved
return addresses. The device panic-looped on the first poll of `main`:

```
panicked at esp-sync-0.3.0/src/lib.rs:449: lock is not reentrant
  <embassy_executor::raw::TaskStorage<screeny_fw::__main::...>>::poll
```

The fix is to take the bound from a frame we own:

```rust
#[inline(never)]
fn frame_mark() -> usize {
    let probe = 0u32;
    core::hint::black_box((&raw const probe) as usize)
}
```

It is a *callee* of `paint`, so its stack pointer is strictly below `paint`'s,
whatever `paint` was inlined into. `xtensa-esp32-elf-objdump` confirms `entry a1,
32` with the local at `a1+4`, so the bound is SP-508: below everything live, and
below the 16-byte register-window spill area, which is the only thing Xtensa
hardware writes beneath a stack pointer.

Reading `a1` directly would be exact and is two lines, but inline assembly on
Xtensa still needs `#![feature(asm_experimental_arch)]` and one probe is not
worth putting the firmware crate on a nightly feature gate.

### The numbers

Device, card 210 partition table, boot + WiFi join + 60 s of the Studio's stream:

| build | `.stack` | high-water | free |
|---|---|---|---|
| `fb-on-stack` (before) | 37512 | **26508** | 9980 |
| default (after) | 37512 | **6304** | 30184 |
| `apsta-probe`, before and after the AP | 33472 | **6256** | 26192 |

**20,204 bytes came off.** Slightly less than the 24,624 the two buffers weigh,
so the old build was already reusing part of one temporary's slot for the other -
which is exactly why "how big is that temporary" was never safe to reason about
from the source, and why this had to be measured rather than argued.

6,304 bytes is now the true depth of everything else core 0 does: `esp_hal::init`,
`esp_rtos::start`, the HUB75 construction, the association, DHCP, mDNS, the
decode path, and the interrupts that land on it. Raising the soft-AP does not
move it (6,256 in the probe build both before and after the switch, the 48-byte
difference being a different build, not a different depth).

---

## 3. APSTA, on the device

`firmware/src/apsta_probe.rs`, feature `apsta-probe`, off by default. One task
owns the controller and the AP runner so nothing has to be signalled across
tasks, and both regimes come from one boot:

1. 60 s of station only, running the *same* `station_loop` the default build runs
   (extracted from `wifi_task`, not copied), with the Studio streaming.
2. `set_config(Config::AccessPointStation(station, ap))` with an **open** AP named
   `screeny-c0ffee` on channel 1. The mode change stops and restarts the radio, so
   the station drops and re-associates; power saving is re-disabled afterwards
   because the restart resets it.
3. The second `embassy-net` stack on `Interface::access_point()`, static
   192.168.4.1/24, `StackResources<4>`, **nothing listening** - no DHCP, no DNS,
   no HTTP. Those are later cards and their cost is a later measurement.

The feature also turns on `esp-alloc/internal-heap-stats`, which tracks a true
`max_usage` across every allocation rather than only the ones a 5 s sample
happens to catch. The radio's transients are exactly what a sampler misses. It
costs CPU on every alloc and dealloc, which is why the default build does not
have it.

### Heap

```
apsta-probe: stage 1 done: station-only baseline | heap used 45540 of 98304 (52764 free) | max usage 50640
apsta-probe: stage 2: APSTA mode set        | heap used 48936 of 98304 (49368 free) | max usage 50640
apsta-probe: stage 3: APSTA idle            | heap used 48868 of 98304 (49436 free) | max usage 53876
```

| | steady used | free | esp-alloc `max_usage` | free at the worst instant |
|---|---|---|---|---|
| station only | 45,540 | 52,764 | 50,640 | 47,664 |
| APSTA idle | 48,868 | 49,436 | 53,968 | **44,336** |
| delta | **+3,328** | | **+3,328** | |

The steady-state cost and the watermark cost agree to the byte, which is a good
sign that the AP's allocations are structural (control blocks, a beacon buffer,
the second interface's queues) rather than churn. The lowest free the 5 s
sampler ever saw across the whole run was 47,840; esp-alloc's own watermark says
the true floor was 44,336.

`esp-radio`'s documentation quotes 47-57 KB for a station and 53-63 KB for an
open AP. Read additively that is 100-120 KB and the feature is impossible on a
98 KB heap. Measured, the second interface costs 3.3 KB. **The documentation is
describing two alternative totals, not two addends**, and that misreading is the
single thing that most threatened the device-web track.

No allocator warnings, no radio warnings, no errors of any kind in the whole
195-second run.

### The frame path during APSTA

Unchanged.

```
telemetry: 30 fps rx, 30 fps shown, 154 swaps/s | drops stale 0 superseded 8 decode 0 rejected 0 gaps 1 | ia 33113 us jit 6274 us | decode 582 us (max 2955) | render 3085 us (max 3271 window, 4241 boot) | state 1 codec 0x10 ...
```

30 fps received, 30 fps shown, 154-155 swaps/s, **zero** stale, decode and
rejected drops, render 3,085-3,092 us against 3,086 us in the station-only
default build. The `superseded` counter climbing is the receiver doing its job
(newest-wins on a drained socket), not a loss.

The switch itself is graceful: the station drops, the receiver goes
`LIVE -> HOLD` and releases the source lock, the station re-associates, the
Studio reconnects and the lock is retaken about 10 seconds later.
`HOLD -> LIVE`, back to 30 fps, zero drops.

### The RSSI step, which is not explained

| stage | RSSI |
|---|---|
| station only, 11 samples | -53, -53, -53, -53, -53, -53, -53, -54, -53, -53, -54 |
| APSTA, 26 samples | -78, -77, -76, -77, -77, -78, -78, -78, -77, ... -78, -77, -76, -78 |

A 24 dB step, at the instant of the switch, held for two minutes. Candidates, in
no particular order: the restart re-associated to a different or more distant AP
in the house; the soft-AP's beacons share the single PHY and the station's beacon
RSSI estimate is taken from a worse duty; TX power or the receive chain is
configured differently in APSTA mode.

It did not cost a frame here. But the portal's whole job is to get a station
associated on a network that was just typed in, sometimes from the far side
of a house, and giving away 24 dB while doing it would matter. It is one
observation from one boot and it should be reproduced before anyone designs
around it.

---

## 4. The budget for the build cards

Start from the branch's default build: `.data` 56,124, `.bss` 102,424,
`.stack` 37,512 with 6,304 used, heap 98,304 with 44,336 free at the worst
instant of an APSTA run.

`.bss` is the currency, because every byte of it comes off `.stack`. Card 201's
attribution table, with this card's measurement substituted where it has one:

| piece | `.bss` | source |
|---|---|---|
| AP interface + second `embassy-net` stack (`StackResources<4>`) | **4,008** | measured here (card 201 predicted ~3.9 KB) |
| picoserve + its buffers (2 KB http, 2 KB rx, 1 KB tx) + serde JSON | ~7,600 | card 201 |
| `edge-dhcp` + `edge-captive` + their UDP buffers | ~5,400 | card 201 |
| `qrcodegen-no-heap` (the spike's 6 KB `Frame` is dropped: the portal screen draws into the existing triple buffer) | ~60 | card 201 |
| **total** | **~17,070** | |

`.stack` would land at about **20,400 bytes** against a measured demand of
**6,304**. picoserve's request handling and `serde-json-core` will add to that
demand - they are ordinary synchronous call chains, and anything held across an
`await` becomes `.bss` instead - but they would have to be twice as deep as
everything the firmware does today before the margin got interesting.

**Heap: keep 64 + 32 KB.** Specifically:

- Do **not** take card 201's lever 3 (shrink the heap by 8-16 KB to buy `.bss`).
  It was written as the reluctant third option and it is not needed: the stack
  fix bought more than the whole web track needs. Shrinking the heap would also
  cut into the 44 KB APSTA floor for no reason.
- Do **not** grow the heap either. The second `heap_allocator!` is carved out of
  the same region `.bss` and `.stack` share, so heap growth is stack shrinkage
  with extra steps. The first one (64 KB of reclaimed ROM DRAM above
  0x3ffe0000) is free, but it is already fully claimed.
- An OTA staging buffer of 4 KB on the heap (card 200's shape) fits trivially:
  44,336 - 4,096 = 40,240 free at the worst instant.
- The one heap consumer the portal adds that is **not** a fixed buffer is
  `scan_async`'s `Vec` of access points, for the settings page's network list.
  That is unmeasured and it scales with how many networks are in range. It
  should be measured, or bounded, before it ships.

Flash: 768,192 bytes today, ~880 KB with card 201's full +112 KB, against a
2 MB OTA slot. 42%. Not a constraint.

---

## 5. Open questions, and cards worth writing

Numbers 221-229 are suggestions, not card files.

- **221 - Reproduce the APSTA RSSI step.** Two or three boots, and a station-only
  control that restarts the radio with `set_config(Config::Station(..))` so the
  restart is separated from the AP. If the AP is really costing 24 dB, the portal
  has to know before it is designed.
- **222 - Make the AP follow the station's channel explicitly, and find out what
  a phone does with the CSA.** Card 201 section 2's single-PHY trap is still
  untested against real client behaviour; this card set channel 1 and never had a
  client associate.
- **223 - A stack floor the build cannot silently cross.** We now know `.stack`
  and we know the demand. A `const` assert or a `tools/` check that fails the
  build when `.stack` drops below, say, 16 KB would have turned card 201's
  13,688-byte spike into a compile error instead of a boot panic. Cheap, and this
  is the moment we know what number to put in it.
- **224 - Measure the stack again with picoserve actually serving.** The 6,304
  here is the frame path. The number that matters for the portal is the one with
  a request in flight, and it needs `wrk` and the LAN, which is a hardware
  bench measurement.
- **225 - Bound `scan_async`'s heap use.** See section 4.
- **226 - The 24 KB of `.data`, if flash or boot time ever matters.** A zeroed
  `.bss` static plus an in-place `const fn format()` puts it back, for one
  `unsafe` block. Written down so nobody has to rediscover that it is possible.

---

## Appendix: reproducing this

```
. ~/export-esp.sh
cd firmware
cargo build --release                        # the shipping build; logs the stack line at 60 s
cargo build --release --features fb-on-stack # the A/B: framebuffers back on the stack
cargo build --release --features apsta-probe # station-only for 60 s, then APSTA idle
xtensa-esp32-elf-size -A target/xtensa-esp32-none-elf/release/screeny-fw
```

and, hardware owner only, from the main checkout:

```
tools/fw-run.sh <abs path to the ELF> <name> <secs>
```

The `apsta-probe` run needs about 195 seconds to reach the stage 3 line. The
serial log contains the station's SSID; quote the boot, stack, heap and
telemetry lines only.
