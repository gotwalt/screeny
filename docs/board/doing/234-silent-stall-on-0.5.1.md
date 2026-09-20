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
