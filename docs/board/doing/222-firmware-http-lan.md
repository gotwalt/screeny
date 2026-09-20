---
id: 222
title: Firmware - an HTTP server on the LAN: status page, JSON API, settings, identify, reboot, _http._tcp
type: build
hardware: yes
depends: [212, 220, 226]
owner: worker-222
branch: card/222-firmware-http-lan
---

## Goal

The device answers HTTP on port 80 on the LAN: one self-contained status/settings page
and the JSON API of `crates/device-api`, served by `picoserve`, without costing the
30 fps frame path anything. No soft-AP, no captive portal, no DHCP/DNS (card 223), no
firmware upload (card 240) - but build it so those cards add routes and a second
listener rather than restructure.

## Context

Read first: `CLAUDE.md`; `docs/design/device-web.md` - all of it, and especially
decisions 3 and 7, "How to think about storage and RAM", "The HTTP API" and **"What card
222 must not rediscover"**; `crates/device-api/README.md`, `src/route.rs`, `src/reply.rs`,
`src/request.rs`, `src/form.rs` and `tests/golden/`; `docs/research/007-device-web-and-portal.md`
sections 1 (picoserve at our pins, sockets and buffers), 7 and 10 ("does an HTTP request
cost a frame?"); `docs/research/009-ram-headroom.md`; `firmware/src/web_spike/http.rs`
(card 201's compile-only spike: proof that `picoserve =0.20.0` links and how its router,
`Json` response and extractors are spelled at this version - evidence, not code to keep;
**retire `spike-http` and the http half of `device-web-spike` when the real server
exists**, keep the other spike parts building); `firmware/src/store.rs`,
`firmware/src/net.rs`, `firmware/src/receiver.rs`, `firmware/src/mdns.rs`,
`firmware/src/main.rs`.

Already established - do not rediscover:

- RAM is the constraint and `.bss` is the currency: every static byte comes out of core
  0's stack. Today (fw 0.3.0): `.stack` 32,504, measured high-water 10,688, so ~20.8 KB
  free. Card 201's spike measured picoserve at ~7.6 KB of `.bss` with 2 KB http / 2 KB rx
  / 1 KB tx buffers and **one** connection; every extra concurrent connection is another
  task future with its own buffers. Card 223 will add ~9.4 KB more (AP stack, DHCP, DNS).
  **Budget for this card: `.stack` must stay >= 22 KB.** If your design cannot meet that,
  the levers, cheapest first: smaller/fewer connection tasks; core 1's 16 KB
  `APP_CORE_STACK` (never measured - paint it with `stack_probe` and report its
  high-water; if it is under 4 KB, propose 8 KB but do not change it without saying so
  loudly); the second heap arena 32 KB -> 24 KB (heap has 44 KB free at its worst
  instant). Report what you did and the numbers.
- picoserve streams replies (measure, then write): reply bounds are documentation. The
  RAM-relevant number is the request buffer: `route::MAX_REQUEST_LEN` (384) plus headers.
  `serde_json_core::from_slice` silently does not unescape: use picoserve's
  `JsonWithUnescapeBufferSize<T, { MIN_UNESCAPE_BUFFER }>`. picoserve's `Form` extractor
  cannot take the WiFi form (SSIDs are bytes): raw body + `form::parse_wifi_form`.
- Nothing large is held across an `await` in a task (it becomes `.bss`); big short-lived
  buffers go on the heap for the duration.
- The frame path is the product: the HTTP tasks run on core 0's executor beside
  `frames_task`. Nothing in a handler may hold the `CORE` lock or the `STORE` lock across
  network I/O; take the lock, copy the numbers out, release, then serialise. Lock order
  is `CORE` -> release -> `STORE`, never both (card 212).
- Settings writes go through `firmware/src/store.rs` exactly as the UDP control handlers
  do (`note_dirty` for brightness/idle, `commit_immediate` for the name and WiFi), and
  `POST /api/v1/wifi` feeds the same `NEW_WIFI` path as `SET_WIFI` - reply first, wait
  100 ms so it really leaves, then rejoin (card 212 found the reply never left without
  that). The PSK is never logged, returned or drawn; log its length if anything.
- You cannot reach the device over the LAN from a worker environment. Your evidence is
  the serial log (server listening, requests cannot be made by you). The orchestrator
  runs the over-the-wire acceptance after the merge. To make your own evidence possible,
  add a bench feature `http-selftest` (off by default) that, 30 s after boot, opens a TCP
  connection **from the device to its own address** port 80 through embassy-net, issues
  `GET /api/v1/status` a few dozen times while the Studio streams, and logs status
  codes, bytes, per-request time and the telemetry line's fps/drops/`render` max over
  that window. If embassy-net cannot connect to its own address, say so and fall back to
  logging the server task's accept loop being ready; do not spend long on it.
- Flash only with `/Users/aaron/src/screeny/tools/fw-run.sh <elf> <name> [secs]` by that
  absolute path. Its log (`/Users/aaron/src/screeny/captures/<name>.log`, git-ignored)
  contains the real SSID and BSSIDs: never copy those lines anywhere. The device's
  credentials are in its settings store; a default build joins from there. Do not use
  `bench-wifi` and do not read any `wifi.env`.

## Deliverables

1. `firmware/src/http.rs` (+ the page): picoserve on TCP 80, `embassy-net` `tcp`
   feature, `StackResources` sized and justified, N connection tasks (start with 2;
   justify), keep-alive and timeouts chosen so a stalled browser cannot pin a task
   forever.
2. Routes, with `screeny-device-api` types and its error shape/status codes:
   `GET /api/v1/status` (every field real: `boot_id` drawn once at boot from the RNG,
   `stack_free` from `stack_probe`, `store_errors` from `store::FAILURES`, `fw_slot` /
   `fw_state` from `esp-bootloader-esp-idf`'s otadata reader if that is cheap - else
   `unknown` and say so, `reset_reason` from esp-hal), `GET /api/v1/telemetry`,
   `GET /api/v1/wifi`, `POST /api/v1/wifi`, `POST /api/v1/settings`,
   `POST /api/v1/identify`, `POST /api/v1/reboot`. `GET /api/v1/networks` and
   `POST /api/v1/firmware` answer with the crate's `ErrorReply` "not implemented" code
   (pick the closest existing code; do not edit `crates/device-api` - report if none
   fits). Unknown paths: 404 in the crate's error shape.
3. `GET /`: one self-contained HTML page (inline CSS/JS, no external assets, small -
   report its size): status (auto-refreshing from `/api/v1/status` every few seconds),
   settings (name, brightness, idle mode), identify, reboot with a confirm. It must also
   work with JavaScript disabled enough to read the status. The WiFi form and firmware
   upload are later cards: leave clearly marked places for them.
4. mDNS: advertise `_http._tcp` port 80 alongside `_screeny._udp`, same instance name.
5. `tools/fw-size.sh <elf>`: prints `.data`, `.bss`, `.stack`, `.rwtext`, image size, and
   **exits non-zero if `.stack` < 16384** (the floor; say so in its header). Use it for
   your before/after numbers. (This one file under `tools/` is yours; nothing else there.)
6. The `http-selftest` run on the device, and the default build left running.
   `FW_VERSION` -> `0.4.0`.

## Bench discipline (orders)

Iterate on the host (build, `tools/fw-size.sh`) first; expect 3-5 flashes; write in the
Log why each happened. Wrap every `fw-run.sh` call in `timeout 400`, secs <= 200. No
other monitors. If a build does not boot, reflash the last good build before diagnosing.
`pgrep -fl espflash` must be empty when you finish. A Studio service streams to the
panel around the clock and reconnects ~15-20 s after each boot; ~30 fps rx with zero
decode drops on the telemetry line is your frame-path evidence. A gap of a few minutes
with the device otherwise healthy is the Studio being redeployed, not your firmware.

## Out of scope

Soft-AP, DHCP, DNS, the captive-portal catch-all, the portal screen, network scanning
(card 223); OTA (240/241); the button (230/231); `crates/*` (if `crates/device-api`
lacks something, wrap it locally and report); the spec; `tools/` other than
`fw-size.sh`.

## Acceptance

- Worker: builds clean with every existing feature still building; `.stack` >= 22 KB
  and the 60 s `stack:` line reported before and after; the selftest log (or the stated
  fallback); default build left running and streaming.
- Orchestrator, over the wire after the merge: every route with `curl`, replies parsed
  against the golden shapes; the page in a browser; settings changed over HTTP survive a
  reboot; `POST /api/v1/wifi` with wrong credentials falls back and `GET /api/v1/wifi`
  says why; **a loop of `GET /api/v1/status` and of `/` at several requests a second for
  60 s while the Studio streams: 30 fps shown, zero decode drops, `render` max
  unchanged**; conformance 60/0/4; `dns-sd -B _http._tcp` finds it.

## Log

### worker-222

**Step 1 - the card, the reading, and the "before" numbers.**

Merged `main` into the worktree (the branch was based on a commit that predated
the card), read `CLAUDE.md`, `docs/README.md`, the card, all of
`docs/design/device-web.md`, `crates/device-api`'s README and its five source
files, `firmware/src/{main,net,mdns,store,receiver,stack_probe}.rs` and the
card 201 spike, plus the registry sources for `picoserve 0.20.0`
(`lib.rs`, `routing.rs`, `extract.rs`, `response.rs`, `response/json.rs`),
`edge-mdns 0.8.0` (`host.rs`, the `ChainedHostAnswers` chain),
`esp-bootloader-esp-idf 0.6.0` (`ota.rs`) and `esp-hal 1.2.2`'s
`SocResetReason`. Nothing about these APIs was taken from memory.

Wrote `tools/fw-size.sh`: one `xtensa-esp32-elf-size -A` pass, parsed with awk,
printing `.data` / `.bss` / `.stack` / `.rwtext` / image, and exiting non-zero
when `.stack` is below the 16384 floor (the header says why that is the floor
and why a card's own budget is tighter).

**Before** (default `cargo build --release`, fw 0.3.0, commit 328eae7):

```
  .data      57388  (incl. .data.wifi 540)
  .bss      106712
  .stack     32504   <- the remainder of main DRAM; floor 16384
  .rwtext    66540  (incl. .rwtext.wifi 51416)  IRAM
  image     816305  (loadable sections only)
  main DRAM: .data + .bss + .stack = 196604
```

So the card's `.stack >= 22 KB` budget leaves **9976 bytes** of new `.data` +
`.bss` for the whole server. That is the number every sizing decision below is
measured against.

**Step 2 - the server, and the RAM argument it lost.**

`firmware/src/http.rs` + `firmware/src/http_page.html`; `picoserve` and
`screeny-device-api` are now ordinary (non-optional) firmware dependencies;
`embassy-net` gains `tcp`; `FW_VERSION` -> `0.4.0`. `spike-http` and
`src/web_spike/http.rs` are gone, `web_spike_portal.html` with them, and
`device-web-spike` is now `spike-ap + spike-portal + spike-qr`.

**The card asked for two connection workers and the RAM said no.** Built with
two first, and measured:

| build | `.stack` | `http_task` POOL |
|---|---|---|
| before (fw 0.3.0) | 32504 | - |
| two workers | 15344 | 15008 (7504 each) |
| one worker | **23648** | 7504 |

7504 bytes a worker: 3584 of buffers (http 1536 / rx 1024 / tx 1024) and ~3920
of picoserve's `serve` future, which holds the router and whichever handler
future is in flight. Two of them put `.stack` at 15344 - under the 16384 floor
`tools/fw-size.sh` refuses, never mind the card's 22 KB budget. So the first
and cheapest lever on the card's list is the one pulled, and it is the **only**
one: core 1's `APP_CORE_STACK` and the 32 KB heap arena are untouched. With one
worker keep-alive is off (picoserve's own docs: enable it only when several
sockets serve), so one browser cannot own the server.

Total cost of the whole card: **8856 bytes** of `.data` + `.bss`, leaving 1120
bytes of margin against the 22528 budget.

Surprises worth writing down:

1. **`mk_static!` inside a `pool_size > 1` task is one buffer shared by every
   instance of it.** The spike's `http_task` used it with `HTTP_TASKS = 1`, so
   nobody had met this; with two workers the second one's
   `StaticCell::uninit()` panics ("already full") on the first poll, which
   would have been a boot panic loop on the bench. The buffers are task locals
   now - the same `.bss`, correctly divided, and accounted in the task pool
   where `xtensa-esp32-elf-nm` can see it.
2. **`embassy_net::Stack` is not `Sync`** (it holds a `&RefCell<Inner>`), so it
   cannot live in the `static` the handlers read. `frames_task` already asks
   the stack for its address on every 20 ms tick for the status screen, so it
   publishes it into an `AtomicU32` on the way past and the handlers read that.
3. **`SET_NAME` with an empty string is not "go back to the default".** The
   receiver takes it literally, which would leave the `GET_INFO` body and the
   mDNS instance name blank. `crates/device-api` documents `name: ""` as "go
   back to `screeny-<id>`", so the substitution is done in the handler, before
   the opcode is built, rather than in the shared state machine where it would
   change what the wire protocol means.

Every remaining feature still builds: `display-on-core0`, `spike-ota`,
`store-selftest`, `apsta-probe`, `fb-on-stack`, `device-web-spike`,
`gpio-probe`, and the new `http-selftest`. `bench-wifi` was deliberately not
built (the card forbids it, and the device's credentials are already in flash).

**Step 3 - the bench. Six flashes, and why each one happened.**

| # | build | why | outcome |
|---|---|---|---|
| 1 | `http-selftest` | first run of the whole card | **panic**: `SocketSet::add`, the set was full. Everything before it was healthy (joined, LIVE, streaming); the panic was the self-test's client socket, at ~45 s |
| 2 | default | the card's rule: a build that does not stay up is followed **immediately** by the last known-good one. This one doubled as the first `stack:` measurement | healthy, 30 fps, `stack: 13056 of 23648` |
| 3 | `http-selftest`, `StackResources` 6 -> 7 | the fix for (1) | healthy; the TCP self-connect failed as the card predicted, fallback logged |
| 4 | `http-selftest` + the in-memory router pass | the TCP half proves nothing here, so give the card the evidence it actually wants | all twelve routes right; frame path untouched |
| 5 | `http-selftest`, SSID redacted | (4)'s log printed the station's SSID in the echoed reply bodies. That is a second place the firmware says an SSID out loud, which the card forbids | redaction confirmed (`"ssid":"<ssid>"`) |
| 6 | default | the build the device must be left running, and the final numbers | healthy, 30 fps rx / 30 fps shown, zero decode drops |

**Socket slots.** Six is the obvious count - frame, control, mDNS, DHCP, HTTP,
one spare - and it panicked. `edge-nal-embassy`'s `Udp` holds more than the one
socket its buffer type names, so six were already in use before the self-test
asked for a seventh. `NET_SOCKETS` is 7 now: the measured count plus one really
spare slot. It costs 408 bytes (`.stack` 23648 -> 23240) and it takes a
panic-on-full out of the default build, not just out of the bench one.

**The self-test.** The TCP half did what the card guessed it would:

```
WARN  selftest: embassy-net cannot reach its own address 192.168.7.221 -
      no loopback on a station interface. Falling back to reporting readiness.
INFO  selftest: fallback evidence - 1 accept loop(s) listening on tcp/80
```

"Can the device reach itself over TCP" is not the question the card is really
asking, though - "does every route answer the right thing on the real device"
is, and that needs only a `picoserve::io::Socket`. So the self-test provides
one made of two byte slices and runs the **real router** through it, on the
device, while the Studio streams. Every case passed first time:

```
GET /                      -> 200 (want 200) OK  6233 bytes, 7339 us
GET /api/v1/status         -> 200 (want 200) OK   472 bytes, 4609 us
GET /api/v1/telemetry      -> 200 (want 200) OK   443 bytes, 3536 us
GET /api/v1/wifi           -> 200 (want 200) OK   167 bytes, 5057 us
GET /api/v1/networks       -> 503 (want 503) OK   166 bytes, 2715 us
POST /api/v1/identify      -> 200 (want 200) OK   112 bytes, 3310 us
POST /api/v1/settings      -> 200 (want 200) OK   150 bytes, 3627 us
POST /api/v1/settings bad  -> 400 (want 400) OK   153 bytes, 2895 us
POST /api/v1/reboot unconf -> 400 (want 400) OK   147 bytes, 3164 us
POST /api/v1/firmware      -> 503 (want 503) OK   166 bytes, 2757 us
GET /api/v1/settings (405) -> 405 (want 405) OK   118 bytes, 2817 us
GET /nope (404)            -> 404 (want 404) OK   109 bytes, 2465 us
```

2.5-7.3 ms a request, the page being the slow one (it is ~6.2 KB rendered and
picoserve formats it once per send-buffer chunk). Over that window the frame
path did not notice: **33 fps rx, 33 fps shown, 0 decode drops, 0 rejected,
`render` max 3182 us** against 3241-3326 us idle on the same build. (33 rather
than 30 is the window's integer-second division, not the device going fast.)

**Stack.** The self-test also answers a question nobody could answer before:
until now no request had ever run through the router on the device, so the 60 s
`stack:` line had never seen the HTTP path's depth.

```
selftest: core 0 stack high-water 13056 -> 14208 of 17272 bytes
```

The whole route table costs **1152 bytes** of stack depth. So on the shipping
build the deepest this firmware goes is ~14208 of 23240, leaving ~8 KB.

| | before (0.3.0) | after (0.4.0) |
|---|---|---|
| `.data` | 57388 | 58164 |
| `.bss` | 106712 | 115192 |
| `.stack` | 32504 | **23240** |
| `.rwtext` | 66540 | 66548 |
| image | 816305 | 956357 |
| 60 s `stack:` high-water | 10688 | **13056** of 23240, 9160 free |

The high-water moved 10688 -> 13056 because of one thing: `read_fw_health`'s
3 KB partition-table buffer. It is transient, on `main`'s real stack (there is
no `await` after it is declared, so it never becomes a future's field, and the
main task's pool is unchanged at 1464 bytes), and it buys `fw_slot` and
`fw_state` without a flash read per status request.

**Core 1's stack was not painted**, and its 16 KB was not touched: the card
lists it as the *second* lever and the first one was enough. Measuring it is a
follow-up.

**Fields that are real, and the one class that is not.** Every field of
`StatusReply` is measured: `boot_id` (hardware RNG, and it does change per boot
- 835003802 then 236509247 on two runs), `stack_free` from `stack_probe`,
`store_errors` from `store::FAILURES`, `fw_slot` / `fw_state` from the real
`otadata` (`Ota0` / `Valid` on this device), `reset_reason` from esp-hal. No
field reports `unknown` on this device today. The one honest caveat is
`reset_reason`'s *resolution*: the ESP32's reason register cannot tell a panic
from any other software reset, cannot see the external reset pin separately
from a power-on, and does not distinguish the interrupt and task watchdogs from
the other timer-group ones, so `panic`, `external`, `int_wdt` and `task_wdt`
are variants this device will never produce. `portal` is hard `false` until
card 223.

**The page** is 5128 bytes of source and **6233 bytes as served** with the
status table rendered in. Self-contained: inline CSS and JS, no external asset,
no font, no framework. The status table is rendered by the firmware, so it is
readable with JavaScript disabled; the script replaces the same cells every
4 s. Settings, identify and reboot need JavaScript. The Wi-Fi form and the
firmware upload are marked-out sections naming cards 223 and 240.
