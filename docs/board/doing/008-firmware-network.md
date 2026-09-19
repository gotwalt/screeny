---
id: 008
title: firmware networking on protocol v1 (frames, control, telemetry, mDNS, lock/idle)
type: build
hardware: yes
depends: [005, 006, 007]
owner: claude-fable-5.1 (bench worker)
branch: card/008-firmware-network
---

## Goal

Replace the throwaway raw-RGB test receiver in `firmware/` with the real thing: the
device speaks `docs/design/protocol-v1.md` completely, using `crates/proto` for every
byte of wire logic, and holds 30 fps with all five codecs on real WiFi while the panel
keeps refreshing cleanly. After this card a spec-conforming sender can drive the panel.

## Context

- Spec: `docs/design/protocol-v1.md` (now includes the 17 clarifications the simulator
  found - section 11 items 14-30). Architecture: `docs/design/architecture.md`.
- `crates/proto` (README has the API): `Packet::parse`, `peek`, `dec::decode(codec,
  payload, dst)`, `control::{Request, Reply, Telemetry}`, `txt::DeviceInfo`. It builds
  for `xtensa-esp32-none-elf` with `-Zbuild-std=core` (already how firmware builds).
  Depend on it by path. `decode_pal8_lz` needs ~768 B of stack: size the task stack.
- **Reference implementation:** `crates/sim/src/core.rs` is the receive state machine
  (seq, newest-wins, source lock, idle, counters, control ops, rate limits) written as
  a pure core with no I/O and no clock. Port its logic; do not reinvent it. If it can be
  made `no_std` and shared cheaply, even better - but do not destabilise the sim to get
  there; copying with attribution is acceptable.
- Card 007's findings that shape this card (read its log in `docs/board/done/`):
  1. **The display task holds the frame lock across a synchronous ~3 ms render, on the
     same single-threaded executor as the frame task**, so "drain the socket, newest
     wins" can never actually run concurrently, and temporal dithering rewrites the
     framebuffer every refresh, costing ~40% of a core permanently. The ESP32 has two
     cores: **move the display/dither work to core 1** (esp-rtos second-core executor)
     and keep WiFi, net, decode and control on core 0. Hand frames across with a
     lock-free or very short critical-section swap of the decoded RGB888 buffer. Measure
     refresh stability and decode latency before/after.
  2. There is a ~640 ms stall once per boot during WiFi association. Find out whether
     it can recur on reconnect; it must not freeze the panel (core split should fix).
  3. Ghosting was a camera artefact; brightness is OE-duty (25 real steps, depth
     intact); temporal dithering works. Keep all of that.
- Bench: device is `192.168.7.221` / `screeny.local`, this Mac is wired to the same
  LAN. The simulator (`cargo run -p screeny-sim`) is the known-good receiver to compare
  against. A sender crate (`crates/screeny`, card 009) may land on main while you work;
  do not wait for it.

### Bench rules (you have hardware access; these are not optional)

Same as card 007 - read `docs/board/done/007-firmware-display.md` "Bench rules" and
`docs/research/000-bench-notes.md`. In short: bench tools by ABSOLUTE path in the main
checkout (`/Users/aaron/src/screeny/tools/fw-run.sh <abs elf> NAME [secs]`,
`/Users/aaron/src/screeny/tools/cam-request.sh NAME [clip N]`), capture names prefixed
`c008-`, baud <= 230400, one serial process at a time, never erase flash, never touch
`backup/`, brightness stays capped, no flashing above 3 Hz, camera daemon problems ->
stop and report. A still cannot photograph the dithered panel honestly below ~sRGB 40;
use a clip and ffmpeg `tmix`.

## Deliverables

1. `firmware/` tasks per architecture.md: `frames` (UDP 49374: drain, validate with
   proto, newest valid frame for the active source, decode into a back buffer that is
   never partially visible, hand to display), `control` (UDP 49375: every op in the
   spec; `SET_WIFI` may return `ERR_UNSUPPORTED` until card 014; `REBOOT` works),
   source lock + idle state machine (hold, then status screen; never blank to black),
   telemetry with every counter the spec defines, piggybacked `STATS_REQ` replies,
   `BUSY` replies rate-limited, mDNS TXT built from the same `DeviceInfo` bytes as
   `GET_INFO`, runtime brightness via the control op (persisting it is optional).
2. Display on core 1 as described above, with before/after measurements.
3. `screeny-probe`: a small host tool for bench testing, as a second binary in
   `crates/sim` (it already has the in-process sender test helpers) or a tiny new crate:
   `--addr HOST[:PORT]`, subcommands `info`, `ping N`, `stats`, `brightness N`,
   `stream --codec <id|all> --fps 30 --secs N` (streams pre-encoded payloads from
   `crates/proto/tests/vectors/` plus a generated moving `SOLID`/`PAL4_LZ` pattern, with
   `STATS_REQ` every 30 frames, prints sent vs device telemetry), `lock-test` (second
   source gets `BUSY`, takeover after timeout, `FINAL` releases). Prove it against the
   simulator first, then the device.
4. Evidence on real hardware, in the card log: `info` output; `ping` RTT distribution
   (n >= 200); for each codec a 60 s 30 fps run with sent / rx / shown / dropped-by-cause
   / decode_us / inter-arrival jitter from telemetry; a 10-minute soak on the heaviest
   codec with heap and refresh rate at start and end; lock-test passing; idle fallback
   captured on camera; a camera clip of a moving pattern showing no tearing or flicker
   while streaming; behaviour across a WiFi drop if you can provoke one safely (e.g.
   `REBOOT` mid-stream and watch the sender-side recovery).
5. Update `docs/design/architecture.md` (firmware section) to match what you built.

## Acceptance

Every codec streams at 30 fps for 60 s with zero decode drops and < 1% total loss on
the bench network; the 10-minute soak ends with stable heap and refresh; control ops
and the lock state machine behave as the spec says on the real device; the device is
left running this firmware showing its status screen.

## Log

### What was built

`firmware/` no longer parses a byte of its own. `crates/proto` is a path
dependency and every datagram in or out goes through `FramePacket`,
`ControlPacket`, `Request`/`Reply`, `Telemetry`, `txt::DeviceInfo` and
`dec::decode`. The spike's `testcmd.rs` is deleted.

| file | what it is |
|---|---|
| `src/receiver.rs` | the receive state machine: a `no_std` port of `crates/sim/src/core.rs` |
| `src/rxstats.rs` | counters and the section 6.8 EWMAs: a `no_std` copy of `crates/sim/src/stats.rs` |
| `src/net.rs` | the `frames` and `control` tasks |
| `src/fb.rs` | the lock-free triple buffer between core 0 and core 1 |
| `src/screens.rs` | idle status screen and the `IDENTIFY` overlay (was `status.rs`) |
| `src/mdns.rs` | TXT built by parsing the `GET_INFO` body back out |
| `crates/probe/` | `screeny-probe`, the bench instrument |

Three decisions worth stating, because a later reader will wonder:

1. **The simulator's core was ported, not reinvented.** Its comments came
   along, because those comments are where card 006 recorded the seventeen
   readings of the spec it had to settle, and they are the specification's
   real meaning. What changed is allocation and nothing else: the `Vec`
   payload becomes two static datagram buffers the frame task swaps pointers
   between (no copy at all), three `HashMap` rate limiters become three
   four-entry arrays with oldest-entry eviction, and `Vec<Vec<u8>>` becomes a
   `heapless::Vec<Out, 4>`. `rxstats.rs` is a near-verbatim copy; copying was
   what the card asked for, since lifting it into `crates/proto` would have
   meant changing `crates/sim` while card 009 is in flight there.

2. **The frame task owns the panel.** Card 007 had a `status` task racing the
   frame task for one mutex. Once there is a state machine that knows whether
   a source holds the lock, there is nothing to race about: the state machine
   says what to draw, so the idle screen, the cross-fade and the `IDENTIFY`
   overlay are all composed by `frames_task` and there is exactly one writer
   to the display.

3. **The bench controls moved off the frame port.** Card 007's `testcmd`
   listened for a `"SX"` magic on UDP 49374. That cannot coexist with
   `frames_rejected` meaning anything, and `frames_rejected` is the counter a
   sender reads to tell "another sender has the panel" from "my packets are
   malformed" (section 6.9). The bench levers - gamma, dither, the
   output-enable window, held test patterns - are now private control opcode
   `0x80`, which section 6.3 reserves for exactly this.

### Two stack overflows, and where DRAM actually comes from

Both cost a flash cycle and both are worth writing down, because the second
one will bite anyone who adds a buffer to this firmware.

**`c008-boot1` - core 1's stack.** `esp_rtos` panicked with
`Stack overflow detected ... Task stack range: 3ffb7c70 ..= 3ffb9c70`, an 8 KB
stack. The second core's entry point was building both DMA framebuffers, and
`FrameBuffer::new()` materialises a 12 KB value on the stack before
`StaticCell::write` moves it - twice. Fix: build them on core 0, move only the
two `&'static mut`s across, and give core 1 16 KB anyway for the `Priority3`
DMA handler.

**`c008-boot2` - core 0's stack.** Then `Detected a write to the stack guard
value on ProCpu`, inside `main`. On this chip **`.bss` and core 0's main stack
come out of the same pocket**: the linker puts the stack between `_bss_end`
and `0x3ffe0000`, and the 64 KB reclaimed-ROM heap sits *above*
`_stack_start_cpu0` where it cannot help. Card 008 adds about 40 KB of `.bss` -
three 6 KB frame slots, the cross-fade source, core 1's stack, two more
sockets - which had squeezed the main stack to 21,360 bytes, not enough for
two 12 KB temporaries. Fix: 16 KB off the non-reclaimed heap (48 -> 32 KB).

| | before | after |
|---|---|---|
| `_stack_end_cpu0 .. _stack_start_cpu0` | 21,360 B | **37,744 B** |
| heap total | 112 KB | 96 KB |
| heap in use (measured, steady state) | 45,416 B | 45,416 B |
| heap free | 69,272 B | **52,888 B** |

### Deliverable 4 - evidence on real hardware

Device `192.168.7.221`, firmware `0.2.0`, brightness 96 (9 of 64
output-enable slots), all runs from this Mac on the same LAN.

#### `info`, and one spec change that shows up on the bench

```
GET_INFO body: 109 bytes
  txtvers=1  proto=1  w=64  h=32  codecs=16,17,40,2,127
  mtu=1464   ctrl=49375  fw=0.2.0  id=4a00a4  name=screeny-4a00a4
parsed: 64x32 proto 1 mtu 1464 ctrl 49375
codecs: PAL8_LZ (16), PAL4_LZ (17), BC1_DUAL (40), PAL5 (2), SOLID (127)
```

`dns-sd -L screeny-4a00a4 _screeny._udp` resolves to
`screeny-4a00a4.local.:49374` with the same ten keys in the same order, which
is the point of section 6.6: the TXT record is literally the `GET_INFO` body
parsed back out by `txt::iter`, so the two cannot drift.

**The host name changed and the bench needs to know.** Card 007's firmware
answered to `screeny.local`; spec section 5.1 says the host name is
`screeny-<xxxxxx>.local.`, so it is now **`screeny-4a00a4.local`**. The
instance name is the friendly name and `SET_NAME` changes it; the host name
does not. `screeny.local` no longer resolves. Every doc that says
`screeny.local` needs updating, and `--addr 192.168.7.221` always works.

#### `ping`, n = 300

```
ping n=300 lost=0 | min 3.81 p50 5.22 p90 8.41 p99 21.03 max 37.35 mean 6.22 ms
      3 ms   120  ################
      5 ms   143  ###################
      8 ms    26  ###
     12 ms     7  #
     20 ms     4  #
```

Zero lost in 300 round trips. The distribution is the shape WiFi always has:
a tight body under 8 ms carrying 96% of the traffic and a thin tail to 37 ms,
which is a beacon interval away. One-way latency is therefore about 2.6 ms
median, well inside a 33 ms frame period.

#### Per-codec, 60 s at 30 fps

Streamed from `crates/proto/tests/vectors/`, cycling that codec's vectors, one
frame per datagram, `STATS_REQ` on every 30th. Counters are deltas across the
run, taken with `RESET_STATS` before and a `TELEMETRY` request after.

| codec | sent | rx | shown | stale | superseded | decode drops | rejected | loss | decode us (ewma / max) | jitter us |
|---|---|---|---|---|---|---|---|---|---|---|
| `PAL5` (2) | 1801 | 1800 | 1800 | 0 | 0 | **0** | 0 | 0.06% | 444 / 2743 | 2844 |
| `PAL8_LZ` (16) | 1801 | 1799 | 1799 | 0 | 0 | **0** | 0 | 0.11% | 562 / 2398 | 2924 |
| `PAL4_LZ` (17) | 1801 | 1801 | 1801 | 0 | 0 | **0** | 0 | 0.00% | 411 / 2872 | 1992 |
| `BC1_DUAL` (40) | 1801 | 1800 | 1800 | 0 | 0 | **0** | 0 | 0.06% | 745 / 2791 | 4413 |
| `SOLID` (127) | 1801 | 1801 | 1801 | 0 | 0 | **0** | 0 | 0.00% | 237 / 1140 | 4069 |
| generated pattern | 1801 | 1801 | 1801 | 0 | 0 | **0** | 0 | 0.00% | 429 / 2326 | 2536 |

Zero decode drops and zero rejections everywhere; worst loss 0.11%, against
the card's 1% budget. `frames_rx = frames_shown + superseded + decode` held in
every run, which is the section 3.3 identity the whole of section 6.9's
diagnosis table rests on.

Two things the decode column says. First, `SOLID` costs 237 us to paint 6144
bytes - about 9 cycles a byte at 240 MHz - because the decoder writes
bounds-checked bytes one at a time and the build is `opt-level = "s"`. That is
the floor for every codec: they all end in a per-pixel write. Second, even
`BC1_DUAL`, the dearest, is 745 us of a 33,333 us frame period - **2.2%**. The
decode path is nowhere near being the constraint, which matters for reading
the core-split result below.

#### 10-minute soak, `BC1_DUAL`

| | value |
|---|---|
| sent / rx / shown | 18001 / 17914 / 17905 |
| loss | 87 frames, **0.48%** |
| dropped stale / **superseded** / decode / rejected | 0 / **9** / **0** / 0 |
| decode | 781 us ewma, 3800 us max |
| jitter | 1844 us |
| identity `rx = shown + superseded + decode` | 17914 = 17905 + 9 + 0, **OK** |
| heap at start / at end | 45,540 / 98,304 -> **45,540 / 98,304** |
| refresh at start / at end | 153 swaps/s -> 153 swaps/s (range 149-154) |

Heap did not move by one byte across ten minutes and 18,000 frames, which is
what you would hope for from a firmware that allocates nothing after boot.
Refresh never left 149-154.

**The nine superseded frames are the most interesting number in this card.**
That counter is the "drain the socket, keep the newest" path of section 3.3
firing - two frames arriving inside one drain. Card 007 established that on
its single-executor design this path *could not* fire, because the display
held the frame lock across a synchronous render and the frame task never got
to run while one was in flight; its log warned that the zero in its counters
should not be read as evidence that the path worked. It fires now, nine times
in ten minutes, and each time the older frame was dropped rather than queued.

