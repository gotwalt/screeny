# 010 - Where core 0's stack goes, what core 1 was holding, and the levers that bought it back

Card 227. Every number here came either from `tools/fw-size.sh` /
`xtensa-esp32-elf-objdump` / `xtensa-esp32-elf-nm` on a build in this worktree,
or from the device itself over the serial port. Card 220
(`docs/research/009-ram-headroom.md`) is the research this continues; sections
2 and 4 there are the parts card 222 and the orchestrator's over-the-wire run
then falsified.

---

## Conclusions first

1. **The HTTP request path is not the deepest thing this firmware does. The
   boot path is.** This is the opposite of what the card assumed and of what
   the disassembly suggested. Measured on the device under 2,378 HTTP
   connections in 200 seconds, core 0's high-water was **13,328 bytes**, of
   which **13,056 was already reached at boot** before a single request
   arrived. Serving HTTP flat out adds **272 bytes**, in steps of 16 to 112 at
   a time. The boot path is `main`'s 5,104-byte poll frame plus
   `store::find_partition`'s 3,200 plus esp-storage's ~4,150 - the two 3 KB
   partition-table reads - and the request path simply never gets that deep.

2. **The 18 KB that started this card no longer exists, and the most likely
   reason is that somebody else fixed it.** Firmware 0.4.0 reported
   `stack_free` 5,176 and later 4,312 of a 23,240-byte stack, i.e. a
   high-water of 17,040-17,904. On 0.4.2, two independent 200-second runs -
   one with 2,378 connections, one with 930 connections **and a forced
   join-failure and rejoin** - both top out at 13,232-13,328. The leading
   explanation is section 2.3: 0.4.0's `POST /api/v1/wifi` called
   `store::commit_immediate` **from inside the HTTP handler**, putting
   esp-storage's ~4,150-byte flash frames on top of the whole router chain,
   and 0.4.1 moved that write into the WiFi task. It is inference, not a
   measurement, and section 2.3 says what would settle it.

3. **Interrupts land on whichever stack was running, and they are not free.**
   `xtensa-lx-rt 0.23.0`'s `SAVE_CONTEXT` opens with
   `addmi sp, sp, -XT_STK_FRMSZ`, `XT_STK_FRMSZ = 256`, on the interrupted
   stack; `esp-rtos 0.4.0` has no separate interrupt stack anywhere (its only
   stack bookkeeping is the per-task guard word). So core 0 pays 256 bytes of
   context plus the handler's own frames for every WiFi and timer interrupt,
   and core 1 pays the same for the HUB75 DMA completion at `Priority3`. This
   is what the creep is: the 16-to-112-byte steps in conclusion 1 are an
   interrupt landing at a slightly different point in the *same* chain, not a
   new, deeper call. A high-water mark can only ever report the deepest
   coincidence so far, which is why it keeps inching up for an hour.

4. **Core 1's 16 KB stack was seven-eighths empty: 1,872 bytes.** Measured
   three times, in two different region sizes, always the same number
   (section 4). It is now 6 KB, and the 10,240 bytes released are what paid
   for the second HTTP worker.

5. **The heap was the wrong thing to worry about and still is - but it had
   8 KB spare.** The second arena is ordinary `.bss`, which is core 0's stack
   with extra steps. At 24 KB the APSTA all-allocations watermark is
   **54,040 of 90,112, leaving 36,072 free at the worst instant** - and it is
   only 72 bytes above what card 220 measured with a 32 KB arena, which says
   the allocator's demand never depended on the ceiling.

6. **The second HTTP connection worker is in** (`HTTP_TASKS = 2`), with
   `NET_SOCKETS` raised 7 -> 8 to match: each worker holds its own
   `TcpSocket`, seven would have left no spare slot at all, and the failure
   mode of getting that wrong is `SocketSet::add` panicking on the first poll
   - a boot loop, not a slow server. Over the wire it is worth **4x the
   throughput** (13 requests/s against 3.3) and it removes the 1-second SYN
   retransmit entirely: back-to-back `time_connect` went from 1.007 s to
   7-13 ms.

7. **`tools/fw-size.sh`'s floor was below the measured demand and is now
   above it.** 16,384 against a real high-water of ~17,900 meant a build could
   pass the check and still die on the guard. The floor is **24,576**, and
   section 6 explains why it is not the 28,672 the card suggested and why
   card 223 having to take one more cheap lever to clear it is the floor
   working rather than failing.

8. **`.stack` is 33,072, not the 34 KB the card asked for**, and the gap is
   arithmetic rather than effort: section 5 lists every byte. What the card
   was really protecting is met with room - `stack_free` under load is
   **18,816** against the 14 KB it asked for.

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

**Do not add that column up.** The first draft of this section did, got
15,744, and predicted a high-water the device then refused to produce. Frames
3 and 4 do not both exist at once: the outer `Either` *contains* the inner one
by value, so LLVM sizes the outer frame to hold it and inlines the inner poll
into it, and the 2,192-byte function in the disassembly is the same code
reached by a different path. The real chain is roughly frames 1 + 2 + 3 plus a
handler - about 13 KB - and that is what the device measured.

The lesson is worth more than the number: **a frame table read off objdump is
an upper bound per function, not a call chain.** It is the right tool for
finding *what* is expensive and the wrong one for saying *how deep*. Only the
device can say that, which is what section 2.2 is.

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

### 2.2 What the device actually did

Two 200-second runs on fw 0.4.2, the Studio streaming 30 fps throughout, the
orchestrator driving HTTP from the LAN. Every line below is `watch_task`
reporting a mark that had just grown.

**Run A** - `.stack` 22,832 (core 1 still 16 KB), 2,576 requests in 240 s,
2,434 answered 200:

| when | high-water | grew by |
|---|---|---|
| boot, before any request | 13,056 | - |
| during an accepted connection | 13,168 | +112 |
| during an accepted connection | 13,200 | +32 |
| during an accepted connection | 13,232 | +32 |
| during an accepted connection | 13,296 | +64 |
| during an accepted connection | **13,328** | +32 |

**Run C** - `.stack` 33,072, 930 requests, **plus a forced join failure and
rejoin** (a `POST /api/v1/wifi` with the dummy pair at +120 s: three
`NoAccessPointFound` attempts, fallback, re-association):

| when | high-water | grew by |
|---|---|---|
| boot | 13,056 | - |
| during connections | 13,072 / 13,184 / 13,200 | +16 / +112 / +16 |
| during connections | **13,232** | +32 |
| the whole SET_WIFI failure and rejoin | *no growth at all* | - |

Two things fall out of this:

* **Boot sets the mark.** 13,056 of the 13,232-13,328 ceiling is reached
  before the first packet. Serving HTTP flat out is worth 272 bytes.
* **The WiFi rejoin path is not deep.** It was the leading suspect - the
  orchestrator raised it, and `try_join` does a full `AllChannels` scan - and
  it moved the mark by exactly zero.

Every post-boot step is 16 to 112 bytes. That is not a new call chain; it is
an interrupt taking its 256-byte context at a slightly different point in a
chain the firmware had already been executing. Which is also why the mark
never settles: it is a record of coincidences, not of code.

### 2.3 The 18 KB that is no longer there

Card 227 exists because 0.4.0 reported `stack_free` **5,176** and later
**4,312** of a 23,240-byte `.stack` - high-waters of 17,040 and 17,904. On
0.4.2 the same hardware, under more load, will not go past 13,328.

The difference is almost certainly **not** this card's levers, which change
how much stack there is and not how deep it goes. The leading explanation is
the fix that landed on `main` in between, for an unrelated bug:

0.4.0's `POST /api/v1/wifi` called `store::commit_immediate(..).await`
**inside the HTTP handler**. That reaches `esp_storage`'s flash path, whose
frames are `FlashStorage::read` 4,160 and `NorFlashRegion::read` 4,144 - on
top of the http task's poll frame, the router chain and the handler. Roughly
13 KB of request path plus roughly 4.2 KB of flash write is roughly 17.2 KB,
which is the number. 0.4.1 moved the write out of the handler and into the
WiFi task (`persist_joined`, called only after a successful join), where it
sits on a 784-byte task frame instead.

So **fixing "credentials are committed before they are proved" also removed
the deepest call chain in the firmware**, and neither card noticed at the
time. It is consistent with all three of 0.4.0's readings, including the
5,176 one, because the orchestrator's card 222 verification ran the
wrong-credentials POST before the hammer.

It is inference. What would settle it: flash 0.4.0 with this card's
`watch_task` in it and POST a bad credential pair - the growth line would name
the moment. That is card 239's job, and it is not worth a bench window now
that the chain is gone.

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
adds a layer whose `poll` frame has to hold the whole remaining chain by
value, which is why the outermost one is **5,680 bytes** - about 43% of the
whole 13,232-byte high-water, for deciding which of nine paths was asked for.
A hand-written path match that dispatches to one of N handlers and returns a
small enum would collapse it into one frame. It is a rewrite of
`firmware/src/http.rs`'s dispatch half and it changes no behaviour on the
wire, which makes it a good card and a bad thing to do inside this one. See
the follow-ups.

---

## 4. Core 1's stack, measured

It had been 16 KB since the first flash of this firmware, on the reasoning
that 16 KB is generous for one display task. It is generous:

| run | region | load | high-water | free |
|---|---|---|---|---|
| A | 16,384 | 200 s of 30 fps, dither on, 2,378 HTTP connections on core 0 | **1,872** | 13,488 |
| B | **6,144** | 200 s `apsta-probe`, station then APSTA | **1,872** | 3,248 |
| C | 6,144 | 200 s of 30 fps plus 930 connections and a rejoin | **1,872** | 3,248 |

**The same number three times, from two different region sizes.** That is the
check worth doing: a high-water read out of a 6,144-byte region agreeing to
the byte with one read out of a 16,384-byte region says the measurement is of
the code and not of the container.

1,872 bytes is `display::render`'s 192-byte row buffer, the embassy executor
and esp-rtos task frames under it, the entry closure esp-hal copies onto the
stack at start-up, and the HUB75 DMA completion interrupt at `Priority3`
taking its 256-byte context on top - which lands here, not on core 0, because
`Hub75::new_async` is called from inside core 1's entry point precisely so
that it would.

Card 227's rule is `max(2 * high_water, 6 KB)` rounded up to a kilobyte:
`max(3,744, 6,144)` = **6,144**. The 6 KB floor binds, not the measurement, so
the margin over anything ever observed is 3.2x. **10,240 bytes released**, and
because core 1's stack is ordinary `.bss` in the same DRAM region as core 0's
`.stack`, all 10,240 went straight to core 0.

Run B is the one that matters for confidence: 200 seconds on the resized
stack, through a radio restart into APSTA and back, with esp-rtos checking the
guard on every context switch. No panic, 3,248 bytes still painted.

---

## 5. The levers, before and after

`tools/fw-size.sh`, default build, each step measured:

| build | `.data` | `.bss` | `.stack` | delta |
|---|---|---|---|---|
| e3a146c, fw 0.4.0 | 58,164 | 115,192 | **23,240** | - |
| + card 227's two-core stack probe and `watch_task` | 58,164 | 115,288 | 23,144 | **-96** |
| + `HTTP_TASKS` 1 -> 2 | 58,164 | 122,784 | 15,648 | **-7,496** |
| + `NET_SOCKETS` 7 -> 8 *(derived)* | - | - | 15,240 | **-408** |
| + heap arena 32 KB -> 24 KB | 58,164 | 115,000 | 23,432 | **+8,192** |
| + merge of `main` 1c9021e (the WiFi commit-before-trial fix) | 58,388 | 115,376 | 22,832 | **-600** |
| + `APP_CORE_STACK` 16,384 -> 6,144 | 58,388 | 105,136 | **33,072** | **+10,240** |

Net: **+9,832 bytes of `.stack`** while adding a second HTTP worker, an eighth
socket slot, a permanent two-core stack probe, and absorbing somebody else's
600-byte bug fix.

### Measured `stack_free`

`stack_free` in `GET /api/v1/status` is `.stack - (high_water + 1024)`, the
1,024 being the guard reserve the paint leaves alone.

| build | `.stack` | high-water | `stack_free` |
|---|---|---|---|
| 0.4.0, after the orchestrator's hammer | 23,240 | 17,040 | 5,176 |
| 0.4.0, an hour later | 23,240 | 17,904 | 4,312 |
| **0.4.2 run A** (core 1 not yet resized) | 22,832 | 13,328 | 8,480 |
| **0.4.2 run C**, hammer + forced rejoin | **33,072** | **13,232** | **18,816** |

The card asked for `stack_free >= 14 KB` under HTTP load. It is **18,816**,
read back from the device's own JSON at 139 s of uptime after 930 requests and
a join failure.

### The one exit number that is missed

The card asked for `.stack >= 34 KB` with two HTTP workers. It is **33,072**,
short by 1,744 bytes (or 744 if "34 KB" means 34,000). The arithmetic is the
table above and there is no slack in it: the only `.bss`/`.data` items left
that are bigger than a kilobyte are `SLOTS` (18,440, the triple buffer),
`FB0`/`FB1` (12,316 each, the DMA framebuffers), the frame socket's receive
and transmit buffers (5,888 and 2,944), the mDNS buffers (~7.8 KB across the
task's pool and cell), and the heap arena at its 24 KB floor. Every one of
them either changes behaviour on the wire or was placed out of scope for this
card. Cards 234 and 235 are the two that could be taken safely, and together
they are worth about 3.8 KB - which would clear 34 KB, and which matters more
for card 223 than for this one (section 6).

What the 34 KB was protecting is met anyway: the demand is 13,232, not the
17,900 the card was written against, so the margin is 18,816 rather than the
16,096 that a 34 KB `.stack` at the old demand would have given.

### Heap, at 24 KB

`apsta-probe` with `esp-alloc/internal-heap-stats`, so `max_usage` is a true
all-allocations watermark rather than a 5-second sample:

| stage | used | free | `max_usage` | free at the worst instant |
|---|---|---|---|---|
| station only | 45,540 of 90,112 | 44,572 | 50,132 | 39,980 |
| APSTA, mode set | 48,152 | 41,960 | 50,132 | 39,980 |
| APSTA idle | 45,540 | 44,572 | **54,040** | **36,072** |

The abort condition this card was given was "if the watermark leaves less than
24 KB free at the worst instant, go back to 32 KB". It leaves **36,072**.

The interesting part is the comparison with card 220, which measured the same
stages on a **32 KB** arena (98,304 total) and got an APSTA watermark of
53,968. The new watermark is 54,040 - **72 bytes higher**. The allocator's
demand never depended on the size of the arena; the 8 KB was simply never
being asked for. That is the clearest possible evidence that this lever was
free, and it is also a warning: it means the remaining 36 KB of headroom is
not "spare capacity the radio might grow into", it is genuinely unused, and
the next person to want `.bss` should look here again before looking at the
stack.

---

## 6. The budget for card 223

Card 201 estimated the portal's cost from a parts list. Card 220 measured one
part of it. This card can do better than either, because the
**`device-web-spike` feature builds the whole thing** - the AP interface and
its second `embassy-net` stack, picoserve's spike server, `edge-dhcp`,
`edge-captive` and `qrcodegen-no-heap` - and the linker will price it:

| build | `.data` | `.bss` | `.stack` |
|---|---|---|---|
| 0.4.2 default | 58,388 | 105,136 | **33,072** |
| 0.4.2 `--features device-web-spike` | 58,660 | 120,664 | **17,272** |
| **cost of the spike** | +272 | +15,528 | **-15,800** |

**15,800 bytes, not the ~9 KB card 201's table predicted.** The estimate was
low by two thirds, which is the whole argument for pricing a feature by
building it.

Two corrections before that becomes card 223's number:

* The spike allocates its own `display::Frame` for the QR screen
  (`mk_static!(display::Frame, ..)`, **6,144 bytes**). Research 009 already
  established the real portal draws into the existing triple buffer instead,
  so card 223 does not pay this.
* The spike server is a *second* picoserve instance. Card 223 should serve the
  portal from the two workers that already exist, which is a design decision
  it should make deliberately and which this card has not priced.

Taking only the first correction, **card 223 should expect to land at about
`.stack` 23,400**, and it should expect `stack_free` in the region of
**8-9 KB** - the current 13,232 demand plus whatever the portal's own handlers
and the DHCP/DNS tasks add, against a 23,400 ceiling.

### That is below the floor, and the floor should stay

23,400 does not clear `tools/fw-size.sh`'s 24,576. That is deliberate and it
is not the same mistake as the 28,672 this card rejected.

* **28,672 was wrong** because card 223 could not have cleared it by any means
  available - the shortfall was ~5 KB and there was no identified lever for
  it. A floor the next card must fail is one somebody deletes.
* **24,576 is right** because the shortfall is ~1,200 bytes and there are two
  identified, cheap, behaviour-preserving levers for it: card 234 (the frame
  socket's transmit buffer, ~1.9 KB) and card 235 (the mDNS buffers, ~1.9 KB).
  Either one clears it alone.

A floor that forces one more cheap lever before a feature ships is doing its
job. Card 223 should take card 234 or 235 as a dependency rather than lower
the number.

### And card 233 should come first

Card 223's real risk is not `.stack`, it is `stack_free`: ~8-9 KB of margin on
a device whose high-water is a record of interrupt coincidences and still
creeping hours after boot. **Card 233 (one router future instead of nine
nested ones) attacks the demand rather than the supply** - it is worth roughly
5 KB off the deepest chain, which is margin card 223 cannot buy any other way.
Scheduling 233 before 223 costs a card's delay and buys back more headroom
than every lever in section 5 combined.

---

## 7. What is left, and cards worth writing

Numbers 233-239 are suggestions, not card files. **233 is the one that
matters**, and note that it and 239 buy *demand* while the rest buy *supply* -
for card 223, demand is the scarcer of the two.

* **233 - One router future, not nine nested ones.** The outermost router
  `poll` frame is **5,680 bytes**, about 43% of the entire 13,232-byte
  high-water, spent deciding which of nine paths was asked for - because every
  `.route()` adds a layer whose frame holds the whole remaining chain by
  value. A hand-written path match dispatching to one of N handlers and
  returning a small enum collapses it into one frame, changes nothing on the
  wire, and is worth roughly 5 KB of *depth*, which is margin no `.bss` lever
  can buy. It is a rewrite of `firmware/src/http.rs`'s dispatch half, which is
  why card 227 did not do it inside a card about RAM levers. **Card 223 should
  depend on it.**
* **234 - The frame socket's transmit buffer is sized for two full MTUs.**
  `2 * MAX_DATAGRAM` = 2,944 bytes of `.bss` for a socket whose outgoing
  traffic is `Outbox` replies, which are small. Sizing it from the spec's
  largest frame-port reply instead of from the MTU looks like ~1.9 KB, but it
  needs the spec read first, and getting it wrong drops replies under load, so
  it is a card and not a guess.
* **235 - `MDNS_BUF` is 1500 twice over.** `UdpBuffers<1, 1500, 1500, 2>` plus
  edge-mdns's two `VecBufAccess<_, 1500>` is ~7.8 KB across the task's pool
  and cell. Real mDNS traffic on this LAN is a few hundred bytes. Worth ~1.9 KB
  at 1024, and the risk is a truncated incoming query breaking discovery, so
  it wants a measurement of what actually arrives before it is cut.
* **236 - A `const` assert on `.stack`, inside the build.** `tools/fw-size.sh`
  is a check somebody has to run. The linker already enforces a hard 8,192-byte
  minimum ("Main stack is smaller than 8192 bytes", which is how the
  `device-web-spike` feature fails to link when `.stack` gets thin) - the same
  idea at the project's own floor would turn a bad build into a build error.
* **237 - The boot path's two 3 KB partition-table buffers.**
  `store::find_partition` and `http::read_fw_health` each put
  `[0u8; PARTITION_TABLE_MAX_LEN]` on `main`'s stack. They do not set the
  high-water today, because the HTTP path is deeper - but card 223's portal
  runs on the same stack and the margin is thinner afterwards, so one shared
  buffer is cheap insurance.
* **238 - Does keep-alive pay for itself now there are two workers?**
  picoserve's docs say to enable it only with several sockets serving, which
  is now true. Against it: the status page polls every four seconds, so one
  open tab would hold one of the two workers indefinitely. It is a one-line
  change and a measurement.
* **239 - Reproduce the high-water creep deliberately.** The mark rises for an
  hour after boot because the deepest *coincidence* of an interrupt on an
  already-deep chain has not happened yet. A bench run that drives HTTP and
  forces disconnect/rejoin repeatedly would find the real ceiling in minutes
  instead of hours, and that is the number a floor should be set from.

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
