---
id: 227
title: Firmware RAM levers - find where core 0's 18 KB of stack goes, measure core 1's, buy back headroom, add the second HTTP worker
type: build
hardware: yes
depends: [222]
owner: worker-227
branch: card/227-ram-levers-second-http-worker
---

## Goal

Firmware 0.4.0 works, and it is closer to the edge than the card 222 budget said. After
real TCP traffic `GET /api/v1/status` reports `stack_free` **5,176** of a 23,240-byte
core-0 stack (high-water ~18 KB; the in-memory self-test saw 14.2 KB), and card 223 (the
soft-AP, DHCP, DNS, the portal) needs ~9 KB more `.bss`, which comes out of the same
region. Separately, the single HTTP worker makes every back-to-back connection pay a
1-second SYN retransmit. This card finds out where the stack goes, pulls the RAM levers
that are safe, and spends part of what it buys on a second HTTP worker.

**Exit numbers** (default build, after 60 s of streaming plus an HTTP hammer run by the
orchestrator): `.stack` >= 34 KB with two HTTP workers, *or* a written, measured
explanation of why not and what the next lever is; measured `stack_free` >= 14 KB under
HTTP load; `tools/fw-size.sh` floor raised to match what you establish.

## Context

Read first: `CLAUDE.md`; `docs/design/device-web.md` ("How to think about storage and
RAM", the build order); `docs/research/009-ram-headroom.md`; the Logs of
`docs/board/done/220-*.md` and `docs/board/done/222-*.md` (both workers' measurements,
the orchestrator's over-the-wire findings at the end of 222, and card 222's note that
`mk_static!` inside a `pool_size > 1` task is one shared buffer); `firmware/src/main.rs`
(heap allocators, `APP_CORE_STACK`, task spawn), `firmware/src/stack_probe.rs`,
`firmware/src/http.rs`, `firmware/src/store.rs` (`find_partition`'s 3 KB stack buffer),
`firmware/src/net.rs`, `tools/fw-size.sh`.

What is known:

- One DRAM region holds `.data` + `.bss` + core 0's stack; `.stack` is the remainder.
  Today: `.data` 58,164, `.bss` 115,192, `.stack` 23,240. Core 0 runs **one** embassy
  executor on that stack, so the high-water mark is the deepest single `poll` chain of
  any task plus interrupt frames on top.
- The biggest `.bss` items (card 220's `nm`): the 32 KB heap arena, `SLOTS` 18.4 KB
  (three 6 KB frame slots), `APP_CORE_STACK` 16.4 KB (core 1's stack - **never
  measured**; core 1 runs only the display task and the HUB75 DMA interrupt), embassy
  task pools (`main` ~1.5 KB, `frames` ~12 KB across two entries, `http` 7.5 KB per
  worker, mdns ~7.7 KB, control, wifi, store).
- Heap: 98,304 total, ~45.6 KB used steady, 50.6 KB peak in station mode, +3.3 KB in
  APSTA (card 220). The second arena (32 KB) is ordinary `.bss`; the first (64 KB) is
  reclaimed ROM RAM outside this region and costs the stack nothing.
- Boot-time transients on the main stack: `find_partition`'s 3,072-byte partition-table
  buffer (card 212) and `read_fw_health`'s 3 KB buffer (card 222). Both are read once.
  They set the *boot* high-water (13 KB) but are gone before HTTP runs, so they do not
  explain 18 KB under load - something in the request path does.
- The 24 KB of framebuffers live in `.data` since card 220 (const-initialised); card 220
  noted they could go back to `.bss` via in-place init for one `unsafe` block. That
  saves flash, **not** RAM: `.data` and `.bss` share the region. Not a lever here.

## Deliverables

1. **Where the 18 KB goes.** Instrument, measure, explain. Options: read the painted
   stack's high-water at more points (after boot, after WiFi join, after first frame,
   after the first HTTP request - `stack_probe` can be sampled any time); inspect frame
   sizes with `xtensa-esp32-elf-objdump -d` (`entry a1, N` gives each function's frame)
   along the HTTP path (picoserve `serve` -> router -> handler -> serde -> smoltcp
   egress inside `embassy-net`); check whether the WiFi/timer **interrupts** land on this
   stack and how deep esp-radio's ISR path is. The orchestrator can drive real HTTP load
   for you on request (SendMessage to "main": say what to run and when) - you cannot
   reach the LAN. Write the answer as a table in the research doc: call chain, bytes.
2. **Core 1's stack, measured.** Paint `APP_CORE_STACK` before core 1 starts and report
   its high-water after 60 s of streaming with dither on (the DMA interrupt at Priority3
   lands on it). If the high-water is H, set the size to `max(2 * H, 6 KB)` rounded up
   to 1 KB, with a comment that says how it was measured. esp-rtos checks the guard on
   context switch and panics with the range, so an undersized stack fails loudly, not
   silently - but do not rely on that: leave real margin.
3. **Cheap wins in the request path**, if deliverable 1 finds any: a large by-value
   struct, a buffer on the stack that could be a task-owned static used under a mutex, a
   handler that serialises into a stack temporary, a `#[inline(always)]` chain that
   stacks frames. Fix what is clearly safe; list what is not.
4. **The heap arena**, only if 2 and 3 do not reach the exit numbers: 32 KB -> 24 KB,
   with a bench run that shows `esp_alloc` max usage in station mode *and* with the
   `apsta-probe` build (that feature still builds; one ~3 minute run), so the decision is
   made against the APSTA peak, not the station one. Do not go below 24 KB.
5. **The second HTTP worker** (`pool_size = 2`), with card 222's `mk_static!` trap
   avoided as it already is. The orchestrator verifies over the wire that back-to-back
   connections stop paying 1 s.
6. `tools/fw-size.sh`: floor raised to what you establish (suggest 28 KB) and the header
   comment updated with the reasoning. `docs/research/010-stack-and-ram-levers.md`:
   conclusions first, the tables, and the **budget for card 223** (AP interface + its
   `StackResources`, `edge-dhcp`, `edge-captive`, `screeny-provision`, the QR - card
   201's attribution table is the starting point; say what `.stack` and measured
   `stack_free` card 223 should expect to end at).
7. `FW_VERSION` -> `0.4.1`. Default build left running on the device.

## Bench discipline (orders)

Iterate on the host (build, `tools/fw-size.sh`, objdump) first; expect 4-6 flashes; write
in the Log why each happened. Wrap every `fw-run.sh` call in `timeout 400`, secs <= 200;
call it by its absolute path `/Users/aaron/src/screeny/tools/fw-run.sh`. No other
monitors. If a build does not boot, reflash the last good build before diagnosing.
`pgrep -fl espflash` must be empty when you finish. The Studio streams around the clock
and reconnects ~15-20 s after each boot; a stream gap of a few minutes with the device
otherwise healthy is the Studio being redeployed. Serial logs contain the real SSID and
BSSIDs: never copy those lines anywhere. No `bench-wifi`, no reading of any `wifi.env`.

## Out of scope

The soft-AP, DHCP, DNS, the portal (card 223); OTA; the button; `crates/*`; the spec;
`docs/design/*`; `tools/` other than `fw-size.sh`. No behaviour change on the wire.

## Acceptance

- Worker: the exit numbers above on serial evidence, every existing feature still
  building, the research doc, the device left on the default build and streaming.
- Orchestrator, after the merge: the HTTP hammer during a stream (30 fps, zero drops,
  `render` max unchanged), `stack_free` under that load, back-to-back `curl` connect
  times, conformance 60/0/4.

## Log

### worker-227

**Step 1 - the card, the reading, and the static picture.**

Branch `card/227-ram-levers-second-http-worker`, merged `main` (the worktree was
based on 72d9646, before the card existed). Read `CLAUDE.md`, `docs/README.md`,
the card, `docs/research/009-ram-headroom.md`, the full Log of
`docs/board/done/222-firmware-http-lan.md`, `firmware/src/{main,http,stack_probe}.rs`
and `tools/fw-size.sh`; and, from `~/.cargo/registry/src/`, `esp-rtos 0.4.0`
(`lib.rs`, `task/mod.rs`, `scheduler.rs`), `esp-hal 1.2.2`'s
`system/multi_core.rs`, `xtensa-lx-rt 0.23.0`'s `exception/asm.rs` and
`picoserve 0.20.0`'s `lib.rs`.

**Before**, default `cargo build --release` at e3a146c (fw 0.4.0):

```
  .data      58164  (incl. .data.wifi 540)
  .bss      115192
  .stack     23240   <- the remainder of main DRAM; floor 16384
  .rwtext    66548  (incl. .rwtext.wifi 51416)  IRAM
  image     956357  (loadable sections only)
```

**Do interrupts land on core 0's main stack? Yes, and it is not incidental.**
`xtensa-lx-rt 0.23.0`, `src/exception/asm.rs`, `SAVE_CONTEXT`:

```
    mov     a0, a1                     // save a1/sp
    addmi   sp, sp, -XT_STK_FRMSZ      // XT_STK_FRMSZ = 256
```

There is no separate interrupt stack anywhere in `esp-rtos 0.4.0` (grepped:
the only stack bookkeeping it has is the per-task guard word at
`stack_bottom + ESP_HAL_CONFIG_STACK_GUARD_OFFSET`). So every interrupt level
costs a **256-byte context frame plus its handler's own frames, on whichever
stack was running** - core 0's main stack for WiFi and the timer, core 1's for
the HUB75 DMA completion.

**Where the stack goes, statically.** `xtensa-esp32-elf-objdump -d` and `entry
a1, N` per symbol. (Worth writing down: objdump prints that immediate in
**hex** once it is over 255, so a first pass that parsed decimal reported a
largest frame of 240 bytes for the whole binary and missed every interesting
one.) The frames on the request path, largest first:

| frame | bytes |
|---|---|
| `TaskStorage<http_task>::poll` | 5968 |
| router `Either<..>` poll, outer (settings/wifi/firmware/telemetry/status/page/404) | 5680 |
| router `Either<..>` poll, inner tail (telemetry/status/page/404) | 2192 |
| `Result<Json<()>, ApiError>::write_to_with_state` | 1104 |
| `Router::handle_request::poll` | 800 |
| `get_page` | 608 |
| `ApiError::write_to` | 464 |

and the rest of core 0, for comparison: `main`'s poll closure 5104,
`store::find_partition` 3200, `TaskStorage<frames_task>::poll` 3008,
`smoltcp Interface::poll` 2480, `esp_storage FlashStorage::read` 4160 /
`NorFlashRegion::read` 4144 (boot only), `TxTokenAdapter::consume` 1584,
`dispatch_ethernet` 1536, `TaskStorage<control_task>::poll` 1216.

Nothing here is one fat buffer: the HTTP depth is picoserve's **nested-`Either`
router**, where each `Route` layer's `poll` gets a frame big enough to hold the
whole remaining chain by value, so the two router frames alone are 7,872 bytes
before a handler has run.

`.bss`/`.data` fat, `xtensa-esp32-elf-nm -S --size-sort`: the 64 KB reclaimed
heap (free, above 0x3ffe0000), `APP_CORE_STACK` **16,400**, the 32 KB arena
**32,768**, `SLOTS` 18,440 (`.data`), `FB0`/`FB1` 12,316 each (`.data`),
`frames_task`'s cells 6,145 + 5,889 + 2,945 + 1,473 x2, `http_task::POOL`
7,504, `mdns` 4,640 + 3,184, `main`'s cell 5,056.

**Step 2 - the instrumentation.**

`firmware/src/stack_probe.rs` rewritten around a `Region` (bottom, top, a
remembered scan cursor, the last mark reported), with two of them: `CORE0`
from the linker symbols as before, and **`CORE1`**, painted from core 0 in
`main` while the second core has not started and nothing is live on its
region. `APP_CORE_STACK.take()` is hoisted out of the `start_second_core`
call so `bottom()`/`top()` can be asked for the bounds.

The cursor is what makes a high rate affordable: a painted word is destroyed
by a frame and never painted again, so the mark is monotonic and a scan can
resume where the last one stopped. A sample that finds no growth is one
`read_volatile`. So `watch_task` samples both cores at 4 Hz and logs **only on
growth**, with the delta - bounded log volume, and the line lands next to
whatever caused it (picoserve logs every accepted connection; the WiFi task
logs every join and disconnect). That is the tool for the orchestrator's
observation that `stack_free` keeps falling with uptime: a single 60 s reading
cannot say what went deep.

Cost of the instrumentation: **96 bytes** of `.bss` (`.stack` 23240 -> 23144).
