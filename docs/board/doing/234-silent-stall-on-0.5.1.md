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
