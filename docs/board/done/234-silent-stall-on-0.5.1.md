---
id: 234
title: Firmware - fw 0.5.1 went silent once (HTTP first, then UDP, then the serial log) - find out why, by reading
type: research
hardware: no
depends: [223]
owner: worker-234
branch: card/234-silent-stall
---

## Goal

This device has to run unattended for months. On 2026-09-20 the default build of fw 0.5.1
(commit 86fd2f5) **stopped answering everything, once**, about 85-105 s after boot, and
did not come back until it was reset. It did not reproduce on the next run. Find the
candidate causes **by reading the code**, rank them, and say what instrument would catch
it next time. No hardware: the orchestrator owns the bench and will run anything you ask
for.

## Context: exactly what is known (read card 223's Log, last section, first)

- Build: default features, `main` at 86fd2f5. Boot, joined from the store first attempt,
  Studio (192.168.7.6) streaming at 30 fps and polling `GET /api/v1/status` every 10 s.
- `screeny-probe http` ran against it and passed 30/0/8 (this creates and drops many
  HTTP connections and posts settings, including an idle-mode commit to the store).
- Then the panel was released from the Studio (state HOLD) at uptime ~80 s and
  `screeny-probe conformance --slow` started from the bench Mac. Rules 1-19 passed
  (GET_INFO, SET_NAME, SET_BRIGHTNESS, SET_IDLE, IDENTIFY, GET_WIFI...), rule 20 onward
  was `no reply to op ... after 4 attempts`.
- The Mac's WiFi had come back on during that run (two interfaces on one subnet - the
  known cause of Mac-side `no reply`), so the probe may have been sending from **two
  source addresses** (192.168.7.203 wired, 192.168.7.158 WiFi). That alone would explain
  the Mac's failures. It does **not** explain the rest:
- **The Studio on workbench lost the panel too**: its last contact was at device uptime
  ~85 s, i.e. ~20 s *before* the Mac's UDP rules started failing. The Studio keeps
  hearing a released panel (verified afterwards: `last_seen_ago` 4 s on a released,
  living panel).
- A passive serial read (no reset; method verified afterwards on a live device, ~100 B/s
  of telemetry) returned **0 bytes in 10 s**. Caveat: a failed
  `espflash monitor --before no-reset-no-sync` attach ran just before it and may have
  held the chip in reset, so weigh this less than the Studio's silence.
- Not a reboot: a reboot rejoins in ~15 s and both witnesses would have seen it return.
  Several minutes passed.
- After a reset with the serial monitor attached, the same suite ran 60/0/4 and the
  device stayed up; that log is `captures/fw-0.5.1-repro.raw.log` (not in git; ask the
  orchestrator for excerpts - **never quote a line containing the SSID**).
- Order of death: **HTTP (Studio's poll) first, UDP ~20 s later, then the log.** The
  telemetry log line is printed from the net task on core 0. That reads like a
  progressive stall of core 0's executor or a lock that one task holds and the others
  queue behind - not a radio drop.

## What changed between 0.5.0 (ran 5 h 35 min under the Studio, passed the same suite) and 0.5.1

`git diff 3e4b2cd 86fd2f5 -- firmware crates/provision`:

1. `firmware/src/http.rs`: **both** HTTP workers now run
   `select(serve_on(lan), wait_ap_pub(true))` in a loop (worker 0 used to call
   `serve_on` once and never return). `AP_SOCKETS` 4 -> 5. The catch-all answers with the
   setup page instead of a 302; `Reply::Redirect` removed; `Cache-Control: no-store` on
   `Reply::Portal`; one `info!` per request on the AP side only.
2. `firmware/src/net.rs`: `provision::screen()` is now called before the publish step
   (it was already called once per loop), a streamed frame is not published while the
   setup screen is up, and the `connected` screen yields to `Intent::Stream`.
3. `crates/provision`: `trial_is_current()` is also true in `Online` while the AP is up;
   the portal layouts no longer alternate (`screen_alternate_ms` removed).
4. `firmware/src/provision.rs`: no DHCP option 114; `cut_str`.

Do not assume it is one of these. 0.5.0 was never run through `screeny-probe http`
immediately followed by UDP conformance with a second source address in play, so a
latent bug in 0.5.0 (or earlier) is equally on the table - e.g. the receiver's
per-source GET_INFO / BUSY rate-limit tables, the control task, the deferred store
commit (`deferred_task`, 400 ms after a settings post), mDNS, or the MACHINE / core
mutexes being taken in different orders by the HTTP status handler and the net task.

## Steps

1. Map every lock, critical section and blocking mutex on core 0 (`MACHINE`, the
   receiver core's guard in `net.rs`, the store, `WIFI_PENDING`, signals/channels), who
   takes each, in what order, and whether anything is held across an `.await`. A lock
   held across an await on a single-threaded executor plus a second taker is the classic
   shape of "HTTP first, then everything".
2. Walk the HTTP status handler and the settings/idle-mode post end to end, including
   the deferred store commit, looking for anything that can wait forever (a channel with
   no receiver, a `Signal` overwritten, a socket `flush`/`close` with no timeout, a flash
   write with interrupts masked colliding with the HUB75 DMA ISR on core 1).
3. Walk what a **second source address** does to the control and frame paths (rate-limit
   tables, BUSY, adopt/lock, `out` buffer capacity in `net.rs`), and what the conformance
   rules around 17-20 send (`crates/probe`), since the stall began near them.
4. Check the `select(serve_on, wait_ap_pub)` change for a way to wedge: picoserve's
   `listen_and_serve` cancelled mid-accept, a socket left in a state that never returns
   to LISTEN, `AP_POLL` timer starvation.
5. Where the simulator (`crates/sim`) shares the code path (`crates/receiver`,
   `crates/provision`, `crates/device-api`), write a host test that tries to reproduce
   your top candidate (two sources, the probe's rule 17-20 sequence, interleaved HTTP
   polls). `cargo test` must stay green.
6. Propose the cheapest **instrument** that would turn the next occurrence into a
   diagnosis instead of a mystery (the RTC panic breadcrumb planned for card 243, a
   core-0 heartbeat from core 1 that logs which task last ran, a hardware watchdog with a
   reason code...). Propose; do not build it unless it is under ~40 lines and costs no
   `.bss` (`tools/fw-size.sh`: `.stack` must stay >= 24,576; it is 27,520 now).

## Exit

- A ranked list of candidate causes in this card's Log, each with the code path
  (file:line), why it fits or does not fit "HTTP first, UDP 20 s later, log last, once",
  and how to confirm or kill it.
- Any host test you wrote, green.
- A concrete bench procedure for the orchestrator to run (commands, how long, what to
  look for), bounded - no loops of long runs.
- If you find a definite bug, fix it on your branch with a test, and say how sure you are
  that it is *the* bug.

## Rules

Branch `card/234-silent-stall` in your own worktree; commit and Log as you go; do not
merge, do not push; no hardware, no serial port, no LAN; every command bounded with a
timeout; never write the real SSID/PSK anywhere (tests use `Example-Wifi1` /
`password9`); leave `crates/art` and `crates/studio` alone (the `software` session's);
tell the orchestrator before touching `crates/proto`, `crates/receiver` behaviour or
`docs/design/protocol-v1.md`.

## Log

### 2026-09-20, worker-234, step 1: what was read, and the lock map

Read, in full: `firmware/src/{main,net,http,provision,store,receiver,mdns,fb,stack_probe}.rs`,
`crates/receiver/src/lib.rs`, `crates/provision/src/machine.rs` (the `Online`/`Portal`
arms), `crates/probe/src/suite/{mod,control}.rs`, `crates/sim/src/device.rs`. Also the
vendored dependencies where the answer was not in our code: `picoserve-0.20.0/src/lib.rs`,
`smoltcp-0.13.1/src/socket/udp.rs` and `src/iface/interface/mod.rs`,
`embassy-sync-0.7.2/src/mutex.rs` and `src/waitqueue/waker_registration.rs`.

**Correction to the card's premise, and it matters.** The "HTTP first, UDP ~20 s later"
gap is softer than it reads. The Studio's `seen_unix` is bumped by UDP telemetry
(`crates/studio/src/devices.rs:714`), by an mDNS resolve, and by each
`GET /api/v1/status` (`devices.rs:750`) - but the panel had been *released* at ~80 s, so
no STATS_REQ telemetry was flowing and the only thing still touching it was the 10 s HTTP
poll. So "last contact 85 s" means "the poll at 85 s worked and the poll at ~95 s did
not": HTTP died somewhere in (85, 95]. On the other side, `screeny-probe conformance`'s
19 control rules are the whole of `control::rules()` (19 rules, `secs` summing to
**12.4 s**), and rule 20 is the first *framing* rule - so on the estimate alone rules 1-19
finish at 80 + ~13..20 = **93-100 s**, not 105. The two windows overlap. **A single event
at ~93-95 s that killed HTTP and UDP together fits the evidence as well as a progressive
one does.** The serial silence is the card's own weakest datum (the failed
`espflash monitor` attach may have held the chip in reset) and telemetry would in any case
have gone quiet within 5 s of `CORE` or the executor going. I have ranked accordingly:
mechanisms that kill both at once are not excluded.

**The lock map (card step 1).** There are three shared things on core 0 and one of them
is not a lock:

| what | kind | taken by | held across an `await`? |
|---|---|---|---|
| `net::CORE` (`net.rs:38`) | `embassy_sync::Mutex<CriticalSectionRawMutex, Option<Core>>` | `frames_task` (`net.rs:177`), `control_task` (`net.rs:376`), `telemetry_task` (`main.rs:608`), `mdns_task` (`mdns.rs:147`), `http::status` (`http.rs:572`), `http::get_telemetry` (`http.rs:708`), `http::apply_control` (`http.rs:632`), `http::post_settings` (`http.rs:795`), `store::commit` (`store.rs:618`) | **no.** Every region between a `lock().await` and its `drop` is synchronous. I checked all nine. |
| `store::STORE` (`store.rs:187`) | same | `store::commit` (`store.rs:629`), `commit_immediate` (`store.rs:549`), `seed_wifi` (`store.rs:503`), `read_fw_health` (`http.rs:231`) | **yes, deliberately**: across `f.save_*().await`, which is `BlockingAsync` over the ROM flash routine - core 1 parked, core 0's interrupts masked ~50 ms per sector (`docs/design/device-web.md:100`). |
| `provision::MACHINE` (`provision.rs:149`) | *blocking* `BlockingMutex<CriticalSectionRawMutex, RefCell<Option<Provisioner>>>` | `with()` (`provision.rs:176`), `step()` (`provision.rs:639`), `init()` | cannot be - it is not async. |

**Lock order is uniform: `CORE` then `STORE`, never the reverse.** `store::commit` copies
the name and idle mode out of `CORE`, drops it, *then* takes `STORE`
(`store.rs:617-629`); `control_task` (`net.rs:387` then `net.rs:407`) and
`http::apply_control` (`http.rs:646` then `http.rs:649`) do the same. `MACHINE` is taken
*inside* the `CORE` region once (`net.rs:215`, `provision::screen` on every 20 ms frame
tick) and outside it everywhere else, but since `MACHINE` is a synchronous critical
section that can never be held by a suspended task, that is not an inversion that can
deadlock. **So there is no lock-order deadlock in this firmware.** That kills the card's
own leading hypothesis.

Two things the map does turn up:

1. `provision::step()` calls `info!("provision: {} -> {}", ...)` **inside** the `MACHINE`
   critical section (`provision.rs:648`), which the module's own docs forbid ("nothing is
   rendered, awaited or formatted while it is held, because a critical section masks the
   interrupts core 1's HUB75 DMA runs on", `provision.rs:31-35`). It only fires on a state
   change, so it is a latency bug and not a stall, but it is the file contradicting itself.
2. `CORE` has **five to nine** contenders and `embassy_sync::Mutex` stores exactly **one**
   waker (`embassy-sync-0.7.2/src/mutex.rs:78` -> `WakerRegistration::register`,
   `waker_registration.rs:29-40`, whose own comment says two waiters "wake each other in a
   loop fighting over this WakerRegistration. This wastes CPU but things will still
   work"). It is not a deadlock - `wake()` takes the waker and every displaced waiter is
   re-woken - but it does mean that whenever two tasks want `CORE` at once, core 0 burns
   CPU ping-ponging until the holder lets go. Worth knowing when reading a latency number.

### Step 2-4: what can actually wedge, and the one thing that certainly could

**The shape of the end state is itself evidence, and it had not been used.**
`firmware/Cargo.toml:125` takes `esp-backtrace` with `panic-handler` and **without**
`halt-cores` or `custom-halt`, so its `abort()` ends in
`arch::interrupt_free(|| loop {})` (`esp-backtrace-0.20.0/src/lib.rs:223-227`): a panic
on core 0 spins core 0 forever with interrupts off and **leaves core 1 running**. Core 1
is the display task and the HUB75 DMA, so the panel stays lit and keeps dithering. That
is exactly what was seen: everything on the network dead, no reboot, several minutes,
recovered only by a reset. It also means the backtrace *was* printed - to a UART nobody
was attached to. **Whatever else this card concludes, never run this sequence again
without the monitor attached and logging.**

**The receiver is not it (card step 3).** I walked every per-source table for what a
second source address does. `Limiter` is a fixed four-slot array with oldest-entry
eviction and a loop that cannot fail to terminate
(`crates/receiver/src/lib.rs:465-501`); `stats_req` is a `heapless::Vec<A, 2>` fully
drained in `flush_frames` (`lib.rs:996-999`); the firmware's `Outbox` is a
`heapless::Vec<Out, 4>` whose overflow is discarded (`firmware/src/receiver.rs:122`).
Nothing there allocates, grows or blocks. What a second source address *does* cost the
device is more unsolicited `BUSY` datagrams - and that is the trigger for candidate 1,
not a cause of its own.

**picoserve does not time out a handler.** `serve_and_shutdown` selects
`app.handle_request(..)` against
`read_request_timeout.map(ignore_output).then(futures::pend_forever)`
(`picoserve-0.20.0/src/lib.rs:431-443`) - the timeout branch deliberately never completes,
it only arms the socket to error on later *reads* - and the other arm is
`shutdown_signal`, which is `core::future::pending()` for `listen_and_serve`. Every other
phase is bounded (`start_read_request` 3 s, `read_request` 5 s, `write` 5 s per write,
`shutdown` 5 s each way, plus embassy-net's own `set_timeout(45 s)` on the socket). So **a
handler that blocks is a worker lost forever, silently**, and this firmware has exactly
two workers and four handlers that take a lock (`http.rs:572`, `:632`, `:708`, `:795`).

#### Ranked candidates

**1. `frames_task` and `control_task` awaiting `UdpSocket::send_to` with no bound.**
`firmware/src/net.rs:321` and `:419` as they stood at 86fd2f5. *Fixed on this branch.*
smoltcp will not make room in a transmit queue by itself: `udp::Socket::dispatch` hands
the head datagram to the interface, the interface cannot resolve the hardware address and
returns `DispatchError::NeighborPending`, and `PacketBuffer::dequeue_with` consumes
**zero** bytes when the closure errs (`smoltcp-0.13.1/src/storage/packet_buffer.rs:196`,
`src/socket/udp.rs:545`, `src/iface/interface/mod.rs:802`). The datagram therefore stays
at the head of the queue and is retried once a second forever (`DISCOVERY_SILENT_TIME`,
`src/iface/socket_meta.rs:46`); four of them fill `tx_meta` and `send_to` never returns.
On `control_task` that is **every reply to every peer**, because the task is then stuck
before its next `recv_from` - which is precisely "rule 20 onward: no reply to op ... after
4 attempts", for 45 consecutive rules, with nothing in the log.
*Fit:* excellent for the UDP half and for "it never came back"; **none** for HTTP dying
first; against the serial log going quiet, since `telemetry_task` is independent of both
sockets. *Confidence the bug is real: certain. That it is the whole story: ~25%.*
*Killed or confirmed by:* the new `net: ... did not fit the transmit queue` warning.

**2. A panic on core 0 that nobody was there to read.** Mechanism certain (see above).
*Fit:* the best single-event fit there is - HTTP, UDP and the log stop together, the panel
stays lit, only a reset recovers, and it would not reproduce. The candidate panics in this
build are `embassy_net::tcp::TcpSocket::new` / `UdpSocket::new` when the station's
`StackResources<8>` is full (smoltcp's `SocketSet::add` panics; `main.rs:414` counts 8
against a steady demand of 7-8: DHCP, DNS, frames, control, mDNS's `edge-nal-embassy`
`Udp` - which `main.rs:399-403` records as holding more than one - and two HTTP workers),
a `RefCell` double-borrow on `MACHINE`, and `esp_sync: lock is not reentrant`. I could not
find a path that produces any of them with the AP down, so the trigger is
**unidentified**. *Confidence: ~30%, and an instrument settles it in one line.*

**3. A flash write that did not come back.** `store::commit` holds `STORE` across
`f.save_*(..).await` (`store.rs:629-637`), which is `BlockingAsync` over the ROM routine:
core 1 parked, core 0's interrupts masked ~50 ms per sector.
`docs/design/device-web.md:100-102` already flags this as an **open risk, bench only** -
"whether esp-radio's WiFi survives that is the first thing to measure on hardware" - and
it still has not been measured under load. The window fits: the `screeny-probe http` run
posts settings, and conformance rules 13-15 are two `SET_BRIGHTNESS` and four `SET_IDLE`,
each of which marks the store dirty (`firmware/src/receiver.rs:192, :197`), so several
debounced commits land in 85-105 s. If a ROM call ever fails to return, core 0 stops
inside a critical section and the end state is indistinguishable from candidate 2 - except
that nothing is printed at all. *Fit: good for a single event; none for HTTP first.
Confidence: ~15%.*

**4. The soft-AP went up.** The only candidate that is **new in 0.5.1** and the only one
that naturally produces "HTTP first". Since 86fd2f5 *both* HTTP workers follow the AP
(`http.rs:1538-1568`), where 0.5.0 moved one and left the other on the LAN, so an AP raise
now takes LAN HTTP away completely within one `AP_POLL` (200 ms). `Driver::raise_ap`
(`provision.rs:740-751`) then re-applies the config, and STA -> APSTA is a mode change,
which restarts the radio and drops the station - so UDP would follow within a second or
two rather than ten. Getting there from `Online` also takes 60 s of `LinkDown`
(`machine.rs:686-708`) plus three 15 s join attempts, which is ~130 s at the earliest, not
90. *Confidence: ~10%.* *Killed in five seconds at the bench:* is the panel showing the
QR, and is an open network named `screeny-4a00a4` beaconing?

**5. Both HTTP workers pinned in a handler on `CORE` or `STORE`.** The mechanism is
certain (picoserve, above) and the consequence is permanent and unlogged, but **no `CORE`
region in this firmware contains an `await`** (see step 1's table), so nothing can hold it
while suspended. This is therefore not a cause on its own - it is the amplifier that turns
any of 1, 3 or a future mistake into "the page died and stayed dead".
*Confidence as the cause: ~10%. As a hazard that needs a guard: high.*

**6. Rejected: the unfair `CORE` mutex.** `embassy_sync`'s single `WakerRegistration`
makes contending tasks wake each other in a loop (`waker_registration.rs:29-40`), which
burns core 0 but cannot deadlock: `wake()` takes the waker and every displaced waiter is
re-woken. A latency finding, not a stall.

**7. Rejected: a lock-order deadlock.** See step 1. The order is uniform and `MACHINE`
cannot be held across a suspension.

### The fix on this branch

`firmware/src/net.rs`: `send_bounded` gives each datagram 200 ms, warns by destination
when it does not fit, and after three in a row closes and re-binds the socket - which
resets both smoltcp buffers and is the only way to drop a wedged head-of-line datagram.
Not a spec change: both frame-socket datagrams are unsolicited (section 6.2) and a control
reply is something a sender retries. Worst case it costs the frame loop 600 ms once, on a
device that is already broken, and `crates/receiver/tests/outbox_bound.rs` is what pins
the "four datagrams per drain" that price is computed from.

`.stack` **27,520 -> 27,376** (floor 24,576). Firmware clippy warnings unchanged (9, all
pre-existing, none in `net.rs`).

**How sure am I that this is *the* bug? Not sure.** It is certainly *a* bug, it explains
the UDP half completely, and it is the only candidate whose mechanism I could verify line
by line in the dependency sources. It does not explain HTTP dying first, and it does not
explain the serial log. If the bench sees the new warning, the card is answered; if it
sees a panic backtrace instead, candidate 2 is - and the fix was still worth making.

### Host tests

* `crates/sim/tests/stall_234.rs` - the three things the bench was doing at once, which no
  test had ever combined: the HTTP suite, a release, then the UDP suite, with
  `GET /api/v1/status` polled at 5 Hz throughout; and separately two frame sources (one
  refused, settled deterministically rather than by a race) plus control traffic plus the
  page. Asserts no port goes silent - a *run* of missed polls, not a single one, because
  the simulator drops a connection when it cannot get a thread
  (`crates/sim/src/http.rs`, "Out of threads").
* `crates/receiver/tests/outbox_bound.rs` - what one drain can ask the caller to send.
  **It found something.** The bound is *not* section 6.2's rate limits: `Limiter` has four
  slots and evicts the oldest, so twelve new sources in one drain get twelve `BUSY`s. The
  bound is the firmware's `heapless::Vec<Out, 4>`, whose overflow `firmware/src/receiver.rs`
  discards with `let _ = out.push(item)`. So `FRAME_TX_BUF`'s reasoning in `net.rs:60-77`
  is load-bearing on a number in a different file, and the simulator, whose `Outbox` is a
  plain `Vec` (`crates/sim/src/core.rs:51`), sends all twelve where the firmware sends
  four. A divergence, not a stall; a backlog card is suggested below.
* `crates/sim/tests/telemetry.rs` - polls for its `TELEMETRY` instead of looking once.
  `drain()` settles for 5 ms and the reply leaves after the core lock is released; the two
  new multi-second tests were enough load to make that the flake in a whole-workspace run.

`cargo test` (whole workspace) green on three consecutive runs;
`cargo clippy --workspace --all-targets` silent. **One flake seen once and not mine to
fix:** `crates/studio/tests/fleet.rs:185`
(`device_of(&s)["player"]["panel"]["connected"]`) failed in the first loaded run and
passed in isolation and in the next two runs. It is a timing-sensitive assertion about a
live stream and it belongs to the `software` session - worth telling them, since this card
made `cargo test` measurably busier.

### The instrument (card step 6)

**Build it now - it is ~25 lines and costs zero `.bss`.** Take `esp-backtrace`'s
`custom-halt` feature and give it a `custom_halt()` that writes a breadcrumb into **RTC
memory** and then reboots:

```rust
#[esp_hal::ram(rtc_fast, persistent)]
static CRUMB: [u32; 6] = [0; 6];   // magic, uptime_ms, and four task counters
```

* `#[ram(rtc_fast, persistent)]` is its **own** memory region: not `.data`, not `.bss`,
  and therefore not core 0's `.stack`. **Cost: 0 bytes of the pool that breaks first**,
  24 bytes of RTC_FAST (8 KB, empty today), ~300 bytes of flash.
* `custom_halt()` stamps a magic word and `now_ms`, waits ~50 ms for the UART to drain,
  then `esp_hal::system::software_reset()`. `main` reads the crumb before anything else,
  logs it, and clears the magic.
* What it buys, for nothing: **a panic stops being silence.** The device reboots in 15 s
  instead of sitting there for minutes, the Studio sees a new `boot_id`, and the first
  line of the next boot log says a panic happened and at what uptime - whether or not
  anybody was attached to the UART when it did. It settles candidate 2 outright.

This *is* card 243's RTC panic breadcrumb. **Pull it forward**: it is the cheapest thing
on this board and it is the difference between this card happening again and it being
diagnosed in one line.

**Second, only if the breadcrumb comes back empty** (a stall with no panic): a core-0
liveness watchdog. `esp_hal::rtc_cntl::Rwdt`, stage 0 an interrupt at 15 s that writes the
crumb, stage 1 a reset at 19 s, **fed from `telemetry_task`'s existing 5 s loop so there
is no new task and no new task future**; each core-0 task bumps a `u32` in the same RTC
array on every iteration, so the reboot's first log line says *which task last ran and how
many times*. That is the difference between "one task wedged in `send_to`" and "the
executor stopped". ~30 lines, ~600 bytes of flash, another 24 bytes of RTC_FAST, **0 bytes
of `.bss`**. Do not arm it before 60 s of uptime, and do not build it until the breadcrumb
has ruled a panic out: if it *is* a panic the breadcrumb alone is the whole answer, and a
watchdog on a device that must run unattended for months is a risk taken for nothing.

### The bench procedure I want run (card step 7)

Bounded, once, ~5 minutes of device time. **No hardware was touched by this card.**

0. Merge this branch. Build the default firmware (`.stack` must read >= 24,576; it is
   27,376 here) and flash with `tools/fw-run.sh`.
1. **Attach the monitor and log it, capped, for the whole run.** This is the single
   biggest change to how this is observed - last time the evidence was printed into a UART
   nobody was reading:
   `timeout 420 espflash monitor ... | head -c 5000000 > captures/fw-0.5.2-card234.raw.log`
2. Let it join and let the Studio pick the panel up. Note the uptime.
3. Then, in this order, once:
   a. `timeout 180 cargo run --release -p screeny-probe -- --addr 192.168.7.221 http`
   b. release the panel from the Studio (state `HOLD`)
   c. **turn the Mac's WiFi on, so both interfaces are on 192.168.7.0/24**, and put both
      addresses in the log (`ifconfig | grep 192.168.7`). This was the accident last time
      and it is the one condition that has never been reproduced deliberately.
   d. `timeout 300 cargo run --release -p screeny-probe -- --addr 192.168.7.221 conformance --slow`
4. Read the log for these, in this order:
   * `net: control reply to ... did not fit the transmit queue in 200 ms`, or
     `net: udp/49375 was wedged; closed and re-bound` -> **candidate 1 confirmed**, and
     the fix caught it. One line answers the card.
   * a **panic backtrace** anywhere -> candidate 2; the frame it names is the answer.
   * `provision: Online -> Joining`, `provision: soft-AP ... up`, or
     `net: http worker N listening on tcp/80 (setup network)` -> candidate 4.
   * a `store: ... committed in N us` line with no successor, or a gap in the 5 s
     telemetry cadence around one -> candidate 3.
   * a `stack: core 0 high-water ...` line that jumps -> none of them, but worth knowing.
5. **If it goes silent again, do these three things before resetting it** - they cost
   nothing and each one kills a candidate:
   * Look at the panel. Lit *and moving* (the idle screen animates at 10 Hz) means core 0
     is alive and one task is wedged -> candidate 1 or 5. **Lit and frozen** means core 0
     stopped and core 1 is still scanning the last DMA buffer -> candidate 2 or 3.
   * `arp -n 192.168.7.221` and `ping -c 3 192.168.7.221` from the wired interface. A
     device whose core 0 has halted answers neither.
   * Is an open network named `screeny-4a00a4` in the Mac's WiFi list? -> candidate 4.
   Then keep the monitor log and reset.
6. **Do not repeat the run if it passes.** A passing long run is not repeated without a
   firmware change, and this one did not reproduce in the first place.

### Backlog cards this turned up (not done here)

* **The simulator and the firmware disagree about how many `BUSY`s one drain sends.** The
  firmware caps at four and silently discards the rest; the simulator's `Outbox` is an
  unbounded `Vec`. Either give the simulator the same four-slot cap, or give
  `screeny-receiver` the cap and let both inherit it - the second is the "one
  implementation of each thing" answer, and it changes `crates/receiver`, which is why
  this card did not do it.
* **`provision::step` logs inside the `MACHINE` critical section** (`provision.rs:648`),
  which the module's own docs forbid because it masks core 1's HUB75 DMA interrupt for the
  length of a formatted line. Move the `info!` out of the closure. Two lines.
* **Card 243's RTC breadcrumb should be pulled forward** - see the instrument above.
* `crates/studio/tests/fleet.rs:185` is timing-sensitive under a loaded `cargo test` (the
  `software` session's).

**Orchestrator, after the merge (2026-09-20):** merged to `main` as 1678e0a. Accepted as
written, including the correction to the card's premise: "HTTP first, UDP 20 s later" is
within the error of the estimates, so one event at ~93-95 s fits. The bounded send is a
real bug fixed whatever else happened. The instrument (custom-halt + RTC breadcrumb +
reset) is card 243, pulled forward and next; the bench procedure above runs **once, on
243's build** (fw 0.5.2), so the same run has both the bounded send and the breadcrumb.
Meanwhile fw 0.5.1 ran 56 min under the Studio at 30 fps with no drops and nothing but
INFO lines on a passive serial watch - the stall is rare, which is why the instrument
matters more than another attempt to provoke it. Backlog items: the `MACHINE`-section
logging fix rides with 243; the sim/firmware BUSY-count divergence and the Studio
`fleet.rs` flake are told to the `software` session and otherwise left (decision 10).
