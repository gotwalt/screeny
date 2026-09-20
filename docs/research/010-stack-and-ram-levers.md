# 010 - Where core 0's stack goes, what core 1 was holding, and the levers that bought it back

Card 227. Every number here came either from `tools/fw-size.sh` /
`xtensa-esp32-elf-objdump` / `xtensa-esp32-elf-nm` on a build in this worktree,
or from the device itself over the serial port. Card 220
(`docs/research/009-ram-headroom.md`) is the research this continues; sections
2 and 4 there are the parts card 222 and the orchestrator's over-the-wire run
then falsified.

---

## Conclusions first

1. **The 18 KB is not a buffer. It is picoserve's router, one frame per
   route.** `Router::from_service(..).route(..).route(..)` builds a
   left-nested `Route<path, MethodRouter, Fallback>`, and each layer's
   `Future::poll` gets a frame big enough to hold the whole remaining chain by
   value. Two of those frames survive into the disassembly at **5,680 and
   2,192 bytes**, so 7,872 bytes of stack are spent on *dispatch* before a
   handler has run, on top of the http task's own 5,968-byte poll frame.
   Nothing in the request path is a large array, a big by-value reply or a
   serialisation temporary; there was no cheap win to take (section 3).

2. **Interrupts land on whichever stack was running, and they are not free.**
   `xtensa-lx-rt 0.23.0`'s `SAVE_CONTEXT` opens with
   `addmi sp, sp, -XT_STK_FRMSZ`, `XT_STK_FRMSZ = 256`, on the interrupted
   stack; `esp-rtos 0.4.0` has no separate interrupt stack anywhere (its only
   stack bookkeeping is the per-task guard word). So core 0 pays 256 bytes of
   context plus the handler's own frames for every WiFi and timer interrupt,
   and core 1 pays the same for the HUB75 DMA completion at `Priority3`. This
   is why the high-water keeps creeping for an hour after boot: it is not one
   deep call, it is the *coincidence* of an interrupt arriving while an
   already-deep chain is in flight, and the deepest coincidence so far is the
   only thing a high-water mark can report.

3. **Core 1's 16 KB stack was three-quarters empty.** Measured for the first
   time here (section 4). See the table for the number and what it was set to.

4. **The heap was the wrong thing to worry about and still is - but it had
   8 KB spare.** Card 220 said keep 64 + 32 KB and it was right at the time:
   there was never a heap *problem*. But the second arena is ordinary `.bss`,
   which is core 0's stack with extra steps, and the APSTA watermark says
   24 KB is still clear of the worst instant. That is the lever that pays for
   the second HTTP worker.

5. **The second HTTP connection worker is in** (`HTTP_TASKS = 2`), with
   `NET_SOCKETS` raised 7 -> 8 to match: each worker holds its own
   `TcpSocket`, seven would have left no spare slot at all, and the failure
   mode of getting that wrong is `SocketSet::add` panicking on the first poll
   - a boot loop, not a slow server.

6. **`tools/fw-size.sh`'s floor was below the measured demand and is now
   above it.** 16,384 against a real high-water of ~17,900 meant a build could
   pass the check and still die on the guard. The floor is 28,672.

---

## 1. The method, and the bug in the first version of it

`firmware/src/stack_probe.rs` is now a `Region` - bottom, top, and the last
mark reported - instantiated twice: `CORE0` from the linker symbols
(`_stack_end_cpu0` .. `_stack_start_cpu0`) as card 220 had it, and **`CORE1`**
over `crate::APP_CORE_STACK`, painted from core 0 in `main` while the second
core has not started and nothing is live on its region. `APP_CORE_STACK.take()`
is hoisted out of the `start_second_core` call so that `bottom()` and `top()`
can be asked for the bounds.

Both are read the same way: scan **up from the bottom** and stop at the first
word that is no longer the paint. `watch_task` does that for both cores four
times a second and logs only when a mark has **grown**, with the delta, so the
line lands next to whatever caused it - picoserve logs every accepted
connection, the WiFi task logs every join and disconnect. A high-water mark is
monotonic, so the log volume is bounded by the growth itself.

### The bug worth writing down

The first version of `watch_task` tried to make the 4 Hz sample free by
remembering a cursor: the mark only moves one way, so why rescan? It is wrong
twice over, and the first flash of this card proved it by reporting **exactly
`size - GUARD_RESERVE` on both cores at once** - core 0 "22,120 of 23,144, 0
free" and core 1 "15,360 of 16,384, 0 free", neither of which had happened,
and neither of which had tripped the guard.

* The scan direction was inverted. Painted memory is at the *bottom* and used
  memory at the *top*, so the scan must stop at the first word that is **not**
  the paint; stopping at the first word that **is** the paint stops at the
  first word it looks at, and reports the whole region as used.
* Even written the right way round, a remembered cursor cannot work. The mark
  moves **down** as the stack deepens, so it cannot be resumed upward at all;
  and resuming downward walks newly-written memory, where a word that happens
  to hold the paint value would stop the scan early and silently under-report.
  Scanning up from the bottom only ever crosses memory that really is still
  painted, so it lands on the true mark and a coincidence in the used region
  cannot reach it.

The full scan costs one word read per four bytes of *unused* stack - a few
thousand reads, tens of microseconds - which at 4 Hz is not measurable. The
optimisation was never needed. It cost a flash and a bench window to find out,
which is the price of optimising a measurement instead of measuring it.

---

## 2. Where core 0's stack goes

`xtensa-esp32-elf-objdump -d --demangle`, one `entry a1, N` per function.

**objdump prints that immediate in hex once it is over 255.** A first pass that
parsed decimal reported a largest frame of 240 bytes for the entire binary and
missed every single one of the frames below. If a frame table for this chip
looks suspiciously flat, that is why.

### The HTTP request path, deepest chain

| # | frame | bytes |
|---|---|---|
| 1 | `TaskStorage<http_task>::poll` (the accept loop, `serve_and_shutdown` inlined) | 5,968 |
| 2 | `Router::handle_request::poll` | 800 |
| 3 | router `Either<..>::poll`, outer: settings / wifi / firmware / telemetry / status / page / 404 | 5,680 |
| 4 | router `Either<..>::poll`, inner tail: telemetry / status / page / 404 | 2,192 |
| 5 | `Result<Json<..>, ApiError>::write_to_with_state` | 1,104 |
| 6 | `get_page` / `ApiError::write_to` / `Json<WifiReply>::into_response` | 608 / 464 / 144 |
| | **sum of 1-5** | **15,744** |
| | plus an interrupt landing on top (context + handler) | 256 + handler |

The measured high-water under real TCP traffic is ~17,900, which is this chain
plus the executor below it plus whatever interrupt was unlucky. The shape
matches: it is not one fat thing, it is dispatch.

### Everything else on core 0, for comparison

| frame | bytes | when |
|---|---|---|
| `main`'s poll closure | 5,104 | boot |
| `esp_storage FlashStorage::read` | 4,160 | boot (store + `read_fw_health`) |
| `NorFlashRegion::read` | 4,144 | boot |
| `store::find_partition` (3 KB partition-table buffer) | 3,200 | boot |
| `TaskStorage<frames_task>::poll` | 3,008 | every frame |
| `smoltcp Interface::poll` | 2,480 | every packet |
| `TxTokenAdapter::consume` | 1,584 | every egress |
| `InterfaceInner::dispatch_ethernet` | 1,536 | every egress |
| `TaskStorage<control_task>::poll` | 1,216 | every control datagram |
| `TaskStorage<wifi_task>::poll` / `try_join` / `hold` | 784 / 672 / 592 | join, rejoin |

The boot path (`main` + `find_partition` + the flash reads) is the reason card
222 measured 13,056 before any request had been served. It is transient and it
is *below* the HTTP path, so shrinking it would not move the high-water mark -
which is why card 227 did not bother, and why the two 3 KB partition-table
buffers are still there.

### Do interrupts land here?

Yes. `~/.cargo/registry/src/index.crates.io-*/xtensa-lx-rt-0.23.0/src/exception/asm.rs`:

```
    .set XT_STK_FRMSZ,         256

    .macro SAVE_CONTEXT level:req
    mov     a0, a1                     // save a1/sp
    addmi   sp, sp, -XT_STK_FRMSZ      // only allow multiple of 256
```

`sp` there is the interrupted task's stack pointer. A grep of all of
`esp-rtos 0.4.0` finds no interrupt stack, only the per-task guard word at
`stack_bottom + ESP_HAL_CONFIG_STACK_GUARD_OFFSET`. So the answer is 256 bytes
of context per interrupt level, plus the handler's frames, wherever the
interrupt lands - and the paint-and-scan method counts them for free, which is
the one advantage it has over any per-task watermark the RTOS could offer.

---

## 3. Cheap wins in the request path: there are none

Deliverable 3 of the card asked for a large by-value struct, a stack buffer
that could be a task-owned static, a handler that serialises into a stack
temporary, or an `#[inline(always)]` chain. The frame table above is the
search, and it found none of those:

* The replies are small. `StatusReply` and friends are a few `&str`s and some
  integers; `Json<..>::into_response` is 144 bytes of frame.
* The only stack arrays on the path are `apply_control`'s `[0u8; CTL_MAX]`
  (a 208-byte frame) and nothing else.
* The two 3 KB partition-table buffers are on the **boot** path, not the
  request path, and are below the high-water anyway.

What is actually expensive is structural, and it is not a patch:

**The router should be one future, not nine nested ones.** Each `.route()`
adds a layer whose `poll` frame has to hold the whole remaining chain. A
hand-written path match that dispatches to one of N handlers and returns a
small enum would collapse 7,872 bytes of dispatch into one frame. It is a
rewrite of `firmware/src/http.rs`'s dispatch half and it changes no behaviour
on the wire, which makes it a good card and a bad thing to do inside this one.
See the follow-ups.

---

## 4. Core 1's stack, measured

PENDING-CORE1

---

## 5. The levers, before and after

PENDING-LEVERS

---

## 6. The budget for card 223

PENDING-BUDGET

---

## Appendix: reproducing this

```
. ~/export-esp.sh
cd firmware
cargo build --release
xtensa-esp32-elf-size -A target/xtensa-esp32-none-elf/release/screeny-fw
cd .. && tools/fw-size.sh firmware/target/xtensa-esp32-none-elf/release/screeny-fw
```

The frame table is `xtensa-esp32-elf-objdump -d --demangle <elf>`, taking the
largest `entry a1, N` per symbol **and parsing N as hex when it is written
`0x..`**.

and, hardware owner only, from the main checkout:

```
tools/fw-run.sh <abs path to the ELF> <name> <secs>
```

The serial log contains the station's SSID and the access points' BSSIDs;
quote the boot, `stack:`, heap and telemetry lines only.
