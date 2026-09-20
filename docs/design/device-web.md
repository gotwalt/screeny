# Device web: status, settings, firmware update, captive portal, the button

**Status (2026-09-20, late): research done (200-202); on the device: the partition
table (210), the rollback bootloader (242, part), framebuffers off the stack (220),
strongest-mesh-node join; host crates done: `crates/settings` (211), `crates/provision`
(221); `crates/device-api` (226), the simulator's HTTP API and WiFi states (224); on the device: fw 0.3.0 with the settings store (212); fw 0.4.0 with the HTTP server on the LAN (222); fw 0.4.2 (227: RAM levers, two HTTP workers); `screeny-probe http` (228); fw 0.4.3 (233: one HTTP dispatch); in flight: 223 (the setup portal, hardware).** This file is the
source of truth for the device-web track (cards 200-249, coordinated by the `firmware`
Claude session): decisions, what the research settled, and the build order at the end.

## What the owner asked for (2026-09-20)

1. A web server on the device: diagnostics and status, network settings, and a
   **safe** firmware update.
2. A use for the Tidbyt's button, specifically resetting WiFi.
3. A captive-portal WiFi network when the device is not joined to one, with the
   panel showing the network name and instructions, and a QR code if the panel can
   carry one.

## Decisions

| # | Decision | Who, when |
|---|---|---|
| 1 | **The portal screen carries a WiFi QR code.** Measured on the real panel: version 2-L (25x25 modules), `WIFI:T:nopass;S:screeny-4a00a4;;` (exactly the 32 bytes 2-L holds), one LED per module, standard polarity (lit white background, dark modules off), 3-pixel lit quiet zone, default brightness. The owner's phone scanned it "easily". | owner's phone, 2026-09-20 |
| 2 | **The setup AP is an open network** named `screeny-<id>`. The home PSK crosses it in clear during setup; the owner does not treat that PSK as a secret (spec 8.4). | owner, 2026-09-20 |
| 3 | **HTTP is unauthenticated on the LAN**, settings and firmware upload included - the same posture as `SET_WIFI` and `REBOOT` (spec 8.4). The API is shaped so a PIN can be added (parked card 041). An upload is still validated as a `screeny-fw` image before the boot slot changes. | owner, 2026-09-20 |
| 4 | **The button is real and reachable** (on the back; the owner can press it). Its GPIO is unknown: card 202 finds it statically and ships a probe firmware; the orchestrator runs the probe with the owner pressing. | owner, 2026-09-20 |
| 5 | **The serial console of spec 8.1 is superseded** by the portal and the settings page (CLAUDE.md: "do not build other schemes"). `SET_WIFI` (8.2) stays and writes the same store. The spec is edited when the store lands, with notice to the software session. | orchestrator, 2026-09-20 |
| 6 | **Compiled-in WiFi credentials are removed.** A default build contains none and `build.rs` does not even look for them; the device gets its network from the store (portal, settings page, `SET_WIFI`). The only override is the off-by-default cargo feature `bench-wifi`, for testing: it embeds the credentials from outside the repo, seeds an empty store with them and keeps them as the spec 8.3 step-2 fallback. The settings partition survives a reflash, so one `bench-wifi` flash seeds the bench device and default builds run from the store thereafter. Lands with card 212; spec 8.3 is rewritten in card 225. | owner, 2026-09-20 |
| 7 | **The frame path is the product.** No HTTP request, flash write or portal activity may cost a frame at 30 fps, except a firmware update, which is allowed to take the panel over with an "updating" screen. | standing |
| 8 | **Button gestures**: short press = status/identify screen (IP, name, RSSI, version) for 10 s; held past 1 s an on-panel countdown starts and release cancels; 5 s wipes WiFi and opens the portal; **15 s factory-resets all settings**. The pin is GPIO15, confirmed on the bench with the owner pressing (card 203). | owner, 2026-09-20 |
| 9 | **Build a rollback-capable bootloader and commit the blob** (`firmware/bootloader/`, ESP-IDF v6.1 in docker, recipe in `tools/build-bootloader.sh`). | owner, 2026-09-20 |

## Working agreement with the software session

`firmware/`, the serial port and flashing belong to the firmware session; cards
200-249. `crates/proto`, `crates/receiver` and `docs/design/protocol-v1.md` are
shared: either session tells the other before changing them. After any flash the
regression check is `cargo run --release -p screeny-probe -- --addr 192.168.7.221
conformance --slow` (firmware 0.2.0: 60 pass, 0 fail, 4 skip). Once the Studio runs
on workbench it holds the source lock around the clock; release it with
`POST http://workbench.local:8787/api/v1/player/set {"device":"4a00a4","on":false}`
before bench work and give it back with `"on":true`. `POST /api/v1/set_panel
{"on":false}` / `{"on":true,"to":"screeny-4a00a4"}` also works again since the Studio's
card 170 (between cards 106 and 170 it answered 200 and did nothing, which read as ~35
conformance failures, all "LIVE -> LIVE"). Either way: **check `screeny stats` says HOLD
or IDLE before starting a conformance run.** The choice survives a Studio
restart, so always put it back.

## How to think about storage and RAM on this device

**Flash (storage) is not a constraint.** 8 MB chip; the firmware image is 0.74 MB and
is projected at ~0.9 MB with HTTP, the portal, DHCP/DNS, the QR encoder and OTA linked
in. Each of the two app slots is 2 MB, the settings partition is 64 KB (the settings
are under 200 bytes), and 3.9 MB of the chip is unallocated.

**RAM is the constraint, and it is three separate pools** (ESP32: 520 KB of SRAM on
paper, far less in practice, no PSRAM in use):

| pool | size | what is in it today | headroom |
|---|---|---|---|
| Main data RAM (`.data` + `.bss` + core 0's stack, one 196 KB region) | 31 + 127 + **37.5 KB stack** | the 32 KB heap arena, three 6 KB frame slots (18 KB), core 1's 16 KB stack, two 12 KB DMA framebuffers, ~12 KB of task state, WiFi driver statics | **the stack is whatever is left over**: every static byte added comes straight out of core 0's stack. This is the pool that breaks first. |
| Heap (64 KB of reclaimed ROM RAM + the 32 KB arena above = 96 KB) | ~45 KB used in station mode | the WiFi driver's buffers, almost entirely | ~50 KB free today; soft-AP + station together is unmeasured (card 220) |
| Instruction RAM (code that must run with flash off) | 63 KB used of ~128 KB | the WiFi blobs (51 KB), the panel refresh ISR, flash-write helpers (+3.7 KB with OTA) | comfortable |

What the device-web features cost, measured by the research spikes: HTTP server ~7.6 KB,
DHCP + DNS ~5.4 KB, second network stack for the AP ~3.9 KB, QR encoder ~0.1 KB - about
**17 KB of main RAM**, against 37.5 KB of slack that is also the stack. Hence the
order of work: first stop `main` from needing 24 KB of transient stack for the
framebuffers (card 220), then measure the heap with the AP up, then add features one at
a time with `xtensa-esp32-elf-size` as the gate on every card. The rule for all
firmware cards: big short-lived buffers go on the heap for the duration of the
operation; nothing large is held across an `await` (it silently becomes `.bss`).

## What the research settled

### Flash, store, OTA (card 200, `docs/research/006-flash-store-ota.md`)

- Partition table: `nvs` 0x9000+0x5000, `otadata` 0xE000+0x2000, `ota_0`
  0x10000+2 MB, `ota_1` 0x210000+2 MB, `screeny` (settings) 0x410000+64 KB. No
  `factory`, no coredump. Today's image is 743 KB, 35% of a slot.
- **`espflash` never touches `otadata`.** With no `factory` partition a serial flash
  writes `ota_0` while a stale `otadata` may still select `ota_1`. `tools/fw-run.sh`
  must always pass `--partition-table firmware/partitions.csv --erase-data-parts ota`.
- **Every flash write must park core 1**: `esp-storage`'s default returns
  `OtherCoreRunning` while the display owns core 1. Use `multicore_auto_park()` and
  the `critical-section` feature; never `multicore_ignore()`; only core 0 touches
  flash. The park is per 4 KB sector (~50 ms): the circular DMA keeps the panel lit,
  only the dither phase freezes. Small config writes need no special handling; an OTA
  shows a static "updating" screen with dither off.
- **Open risk, bench only**: core 0 has interrupts masked for the same ~50 ms per
  sector erase. Whether esp-radio's WiFi survives that during an upload is the first
  thing to measure on hardware.
- **Rollback**: espflash's bundled bootloader has `APP_ROLLBACK_ENABLE` off. App-side
  revert (mark the running slot `Invalid`, reboot) covers every image that boots but
  never becomes healthy. Only a rebuilt ESP-IDF v6.1 bootloader covers an image that
  crashes before the confirm code runs. **Owner decision pending**: build that
  bootloader (ESP-IDF is not installed here; a docker image would do) and commit the
  26 KB blob.
- Health criterion (proposed): WiFi + DHCP, and one HTTP request or 120 s uptime, and
  at least one display swap; never before 60 s; revert at 180 s.
- Buffers held across an `await` in a task land in `.bss` and come out of core 0's
  stack (the spike's 11 KB did). Scratch buffers are heap-allocated for the duration
  of the operation, never in a future.
- Settings: `sequential-storage 8.0.1` map over the `screeny` partition through
  `BlockingAsync`; record logic in `crates/settings` (card 211).

### The button (card 202, `docs/research/008-button.md`)

- **GPIO15, active low, internal pull-up**, read out of the stock image's
  `gpio_config_t` and confirmed by a stock boot log on Tidbyt's forum. Not `EN`, not
  GPIO0: holding it at boot cannot strand the device in the ROM bootloader. GPIO15 is
  also the MTDO strap (a held button silences the ROM boot log - harmless) and one of
  the two board-ID ADC straps.
- `BUTTON_GPIO` stays `None` until the bench confirms it: `cargo build --release
  --features gpio-probe --bin gpio_probe` in `firmware/`, flash, the owner presses.
  Phase B of the probe (pull-down) says whether anything external holds the pin up.
- Gestures (proposed): short press = identify/status screen for 10 s; held past 1 s
  starts an on-panel countdown, release cancels; 5 s wipes WiFi and reboots into the
  portal; 15 s factory-resets all settings. Held at boot runs the same ladder. One
  embassy task on core 0 in `wait_for_any_edge`, 30 ms debounce.
- Fallback (three quick power cycles) is parked unless the probe says the button is
  unusable.

### HTTP, soft-AP, portal (card 201, `docs/research/007-device-web-and-portal.md`)

- **HTTP server: `picoserve 0.20.0`** (matches our embassy-net/embassy-time/heapless
  pins; streaming request bodies with a per-request timeout, which the firmware upload
  needs; router, forms, JSON). ~78 KB flash, ~7.6 KB `.bss`.
- **Radio mode: APSTA** (`Config::AccessPointStation`, second `Interface::access_point()`
  with its own `embassy-net` stack at 192.168.4.1/24). Chosen on function: AP-only mode
  cannot scan, and the settings page wants a network list. Single PHY: the AP follows
  the station's channel, so the phone may briefly lose the AP when a trial join starts.
- **DHCP + DNS catch-all: `edge-dhcp 0.8` + `edge-captive 0.8`** (same family as our
  `edge-mdns`; DHCP runs over a plain UDP socket and emits RFC 8910 option 114).
- **State machine**: BOOT -> JOINING (3 tries, ~45 s) -> ONLINE, else PORTAL (APSTA,
  QR on the panel, DHCP + DNS + HTTP on 192.168.4.1). Posted credentials go to TRIAL:
  nothing committed, the AP stays up, and a full-page reload reports "connected, I am
  at 192.168.x.y" or "wrong password / network not found"; commit and drop the AP only
  on success. PORTAL is never terminal: every 10 minutes, if no phone is associated,
  retry the stored credentials (the 3 a.m. router reboot heals itself). Telemetry
  state is `PROVISIONING` in PORTAL and TRIAL.
- **Captive-portal rules**: DNS answers everything with 192.168.4.1; unknown hosts get
  a 302 **with a non-empty body** (Android treats `Content-Length <= 4` as failure;
  iOS needs content). The iOS mini-browser only re-probes on a full-page navigation, so
  the provisioning path uses a plain form post and `setTimeout(location.href=...)`, no
  `fetch()` polling. File inputs do not work in captive mini-browsers: **firmware
  upload is on the LAN page only.**
- **Portal screen**: QR version 2-L, 31x31 block including the 3-pixel quiet zone,
  leaving 32 columns = eight `FONT_4X6` characters per line. `qrcodegen-no-heap 1.8.1`,
  ~10.5 KB flash, no allocator. Mock-ups: `docs/research/img/201-portal-*.png`.
  With the measured `WIFI:T:nopass;S:...;;` form the SSID is at most 14 characters, and
  `screeny-4a00a4` is exactly 14: **the AP name is always `screeny-<id>`**, never the
  friendly name. (A bench experiment with the short form `WIFI:S:...;;` could raise
  that to 23.)
- **The RAM gate**: with everything linked, core 0's `.stack` falls from 37.5 KB to
  13.7 KB while `main` puts two 12 KB `FrameBuffer` temporaries on it, and APSTA's heap
  use is unmeasured (~45 of 96 KB is used in station mode). Card 220 fixes the first
  and measures the second before anything else is built.

Refinements made while building the state machine (card 221, `crates/provision`; its
README has the diagram and the action list) - these are now the design:

- Empty store + compile-time credentials: join with the built-ins, and success commits
  them to the store (that is the seeding of decision 6). Neither present: portal.
- A trial join fails **immediately** on an authentication error (a wrong password is
  deterministic); other reasons retry up to 3 times. Boot-time joins always take 3.
- The machine owns a 15 s per-attempt deadline (3 x 15 s = the 45 s), so a silent radio
  still advances.
- The AP stays up during the 10-minute retry; it is only ever dropped 30 s after a
  successful join (uniformly, not just after a trial). A suppressed retry does not
  reset its timer: it fires on the first tick after the last phone leaves.
- A second credentials POST during a trial cancels and restarts with the new values.
- After a button wipe `GET_WIFI` reads `DISCONNECTED`, not `FAILED`: a wipe is not a
  failure.
- The machine never sees a PSK: events carry the SSID only, and the caller holds the
  credential until `CommitCredentials`.
- Footprint: no statics; `Provisioner` 168 B, ~400 B of stack at peak in `render`.

### The HTTP API (card 226, `crates/device-api`)

The request and reply shapes are one `no_std` crate, `screeny-device-api`, that the
firmware (222), the simulator (224) and the Studio all depend on - not a feature of
`crates/proto`, which is shared surface with the software session. Its README has the
route table; `crates/device-api/tests/golden/` has a checked-in example of every request
and reply, verified through `serde_json` and `serde-json-core` on every `cargo test`.

| method | path | request | reply |
|---|---|---|---|
| GET | `/api/v1/status` | - | the `GET_INFO` and telemetry numbers plus `api`, `fw_slot`, `fw_state`, `reset_reason`, `stack_free`, `store_errors` |
| GET | `/api/v1/telemetry` | - | the 48 bytes of spec 6.7 as named fields |
| GET | `/api/v1/networks` | - | at most 16, strongest first; one scan per 10 s |
| GET | `/api/v1/wifi` | - | `{state, ssid, ip, reason}` - what the portal page's reload reads |
| POST | `/api/v1/wifi` | urlencoded `ssid=&psk=` | `{"result":"trying"}`, sent before the radio work |
| POST | `/api/v1/settings` | `{name?, brightness?, idle_mode?}` | the applied values |
| POST | `/api/v1/firmware` | raw octet-stream, streamed | `{ok, written, error?}` |
| POST | `/api/v1/reboot` | `{"confirm":"RBOO"}` | `{"result":"rebooting"}` |
| POST | `/api/v1/identify` | `{duration_ms}` | `{"result":"identifying"}` |

Failures are one shape: `{"error":"<code>", "detail"?:"..."}`, the HTTP status being a
property of the code. Every mutating request carries an optional `pin`/`counter`, parsed
and ignored today (decision 3); `check_auth` is the single hook for parked card 041.

Requested by the software session (2026-09-20), to land with card 222: `StatusReply`
gains **`boot_id`**, a random `u32` drawn once at boot, so the Studio can tell "the device
rebooted" from "the link flapped" without inferring it from uptime going backwards. A
random id rather than a persistent counter on purpose: it costs no flash write per boot.
(Add it to `crates/device-api` and its golden files after card 224 merges, so the
simulator picks it up in the same change.)

Decided after the Studio first read the live API (2026-09-20), to land with card 223:
in `GET /api/v1/status`, **`wifi_state` means the link** (`connected` / `connecting` /
`disconnected`) and never the sticky result of the last credentials attempt. That result
(`failed` + `reason`, sticky until the next post or a reboot, which is how spec 8.2's
`ERR_WIFI` is reported over `GET_WIFI`) belongs to `GET /api/v1/wifi` only. Firmware
0.4.0 reports the sticky value in both places, so a device that fell back successfully
reads `wifi_state: failed` while plainly connected - it looks like a fault and is not.
Also for 223: `GET /api/v1/wifi`'s `reason` is `null` after a failed attempt in 0.4.0;
it must carry `auth` / `not_found` / `other` from the state machine.

**Credentials are stored only after they have joined** (fw 0.4.1, spec 8.2 corrected
2026-09-20). Firmware 0.4.0 and the spec stored first; the orchestrator's own
wrong-credentials test over HTTP replaced the working pair in flash, the device ran on
its in-RAM fallback until the next reboot and then could join nothing. The handlers now
hand `NewWifi { wifi, persist }` to the WiFi task, which commits after a successful join;
a store failure then is counted (`store_errors`), not reported in the reply. Recovery, if
a device ever has a bad pair in flash: flash a `--features bench-wifi` build (stored pair
fails three times, the built-ins join), send `screeny-probe set-wifi SSID PSK --persist`,
flash the default build. **Bench rule that follows: after any wrong-credentials test,
reboot the device and see it rejoin before calling the test passed.**

Where core 0's stack goes (card 227, `docs/research/010-stack-and-ram-levers.md`),
measured on fw 0.4.2: **the boot path sets the mark (13,056 bytes)**; 2,378 real HTTP
connections added 272 bytes and a forced WiFi join failure + rejoin added none. Interrupts
land on the interrupted stack (256 bytes of context per level; esp-rtos has no interrupt
stack), which is the 16-112 byte creep. The ~18 KB seen on fw 0.4.0 was, by strong
inference, the flash write that `POST /api/v1/wifi` used to do *inside the HTTP handler*
(esp-storage's 4 KB frames on top of the router chain) - moving that write into the WiFi
task fixed the credentials bug and removed the deepest call chain at once. Rule for
firmware cards: **never write flash from inside an HTTP handler.** picoserve's nested
`Either` router is still a 5,680-byte frame (43% of the high-water) and each HTTP worker
costs 7,504 bytes of `.bss`; card 233 replaces it with one dispatch and *measures* what
that buys.

RAM levers pulled in 0.4.2: core 1's stack 16 KB -> 6 KB (measured high-water 1,872, the
same number from two region sizes), second heap arena 32 -> 24 KB (APSTA watermark 54,040
of 90,112: 36 KB free at the worst instant), a second HTTP worker (+7.5 KB) with an eighth
socket. `.stack` 33,072, `stack_free` 17-19 KB under load, `tools/fw-size.sh` floor 24,576.
**Card 223 priced by building the spike: 15.8 KB of `.stack`, not the ~9 KB estimated**;
minus the spike's 6 KB scratch frame it lands at ~23.4 KB, ~1.2 KB under the floor. The
floor stays; 223 takes one more lever first (233's result, or the frame-socket tx buffer /
mDNS buffers at ~1.9 KB each).

What card 222 must not rediscover:

- **picoserve does not buffer replies** (it measures into a counting writer, then
  streams), so reply bounds are documentation; the RAM-relevant bound is the request
  side, `route::MAX_REQUEST_LEN` = 384 bytes (the WiFi form).
- **`serde_json_core::from_slice` silently does not unescape strings**: use
  `from_slice_escaped` / picoserve's `JsonWithUnescapeBufferSize<T, { MIN_UNESCAPE_BUFFER }>`
  (32 bytes = the longest name; raising `MAX_NAME_LEN` raises it).
- **picoserve's `Form` extractor cannot be used for `POST /api/v1/wifi`**: it demands a
  UTF-8 body and an SSID is bytes. Read the raw body, call `form::parse_wifi_form`.
  Duplicate keys are refused; a missing `psk` means an open network.
- A non-UTF-8 SSID is reported as `null` and left out of the scan list, never lossily
  converted.
- `GET /api/v1/networks` at worst case is 3.7 KB, several TCP segments: that is the route
  the "HTTP costs no frame" bench proof should hammer, not `/status`.

Credentials posted while the device is online (card 232) - now part of the machine:

- `Online` or `Joining` + credentials posted -> a trial **without the AP**
  (`TrialOrigin::Online`): the one station drops its association and tries the new
  network. Success commits and announces; failure goes back to the **stored** network
  (then the built-ins, then the portal only if there is nothing at all), never clearing
  the store. No overlay, no portal screen, no address on the panel: the stream keeps the
  panel through the rejoin. `ip()` is `None` for the length of the trial.
- The failed result is sticky until the next post, a button wipe or a reboot
  (`trial_is_current()`, `wifi_state()`); a later card may add an explicit acknowledge.
- What the firmware must do (card 223): feed **every** `POST /api/v1/wifi` and `SET_WIFI`
  into the one `Provisioner` whatever its state, reply first, then `step`; read
  `GET /api/v1/wifi` from `trial_is_current()`/`trial()`, `GET_WIFI` from `wifi_state()`;
  use `route::find`, `route::RateLimit` with `SCAN_MIN_INTERVAL_MS`, and per-route
  `max_request_len`.
- The simulator keeps UDP `SET_WIFI` "accepted, logged, not acted on" outside the portal
  on purpose (other sessions' tests pin it); HTTP runs the real trial. Closing that
  asymmetry is a later card, with notice to the software session.

Orchestrator's defaults for card 201's open questions (the owner can overrule): the
portal has **no time limit** (the 10-minute retry makes that safe); the LAN web server
**does answer while a sender is streaming**, and a bench card proves it costs no frame;
the device advertises **`_http._tcp`** in mDNS; JSON shapes for the HTTP API live in
their own crate, `crates/device-api` (card 226), which the firmware, the sim and the
Studio all depend on - `crates/proto` is not touched.

## Build order

| card | what | hardware |
|---|---|---|
| 210 | partition table + `tools/fw-run.sh` flags - **done**, on the device, conformance 60/0/4 | yes (orchestrator) |
| 211 | `crates/settings`, host-tested against the real map - **done** (42 tests) | no |
| 220 | framebuffers off core 0's stack; APSTA heap measured - **done**: stack high-water 26.5 KB -> ~6 KB of 37.5 KB; APSTA costs 3.3 KB of heap (44 KB free at the worst instant); keep the 64 + 32 KB heap; the full track leaves ~20 KB of stack against ~6 KB of demand (`docs/research/009-ram-headroom.md`) | yes |
| 203 | bench: GPIO15 confirmed with the probe, owner pressing - **done** | yes (orchestrator + owner) |
| 212 | **done, on the device (fw 0.3.0)** - firmware: the store on the `screeny` partition, settings loaded at boot, debounce task, `ERR_STORAGE`, `SET_WIFI` wired, compile-time credentials optional (delivers 063) | yes |
| 221 | `crates/provision`: the join/portal state machine, the `WIFI:` URI builder, the portal-screen renderer - **done** (60 tests; the rendered QR decodes with an independent decoder) | no |
| 226 | `crates/device-api`: the HTTP JSON shapes in one `no_std` crate for firmware, sim and Studio - **done** (64 tests, golden JSON files; its own crate rather than a `crates/proto` feature, so the shared wire crate is untouched) | no |
| 222 | **done, on the device (fw 0.4.0)**: http://192.168.7.221/ - 200 requests in 60 s during a stream cost no frame; one worker, so back-to-back connections pay a 1 s SYN retransmit; `stack_free` fell to 5.2 KB under load -> card 227. Was: firmware: picoserve on the LAN - `GET /api/v1/status`, the status page, `_http._tcp`; bench proof that HTTP costs no frame | yes |
| 228 | `screeny-probe http`: 38 rules over the HTTP API, the same suite against the sim and the device - **done**; `cargo run --release -p screeny-probe -- --addr 192.168.7.221 http` is the check after every flash, beside the UDP `conformance`. Known firmware-0.4.x gaps are skips behind one constant, `screeny_probe::http::CARD_223_LANDED` | no |
| 227 | **done (fw 0.4.2)** - RAM levers: where core 0's 18 KB of stack goes, core 1's 16 KB measured and resized, the second HTTP worker; **gates 223** | yes |
| 233 | **done (fw 0.4.3)** - HTTP conformance 30/0/8, `stack_free` 20 KB and no longer creeping, 47 KB of flash back; the RAM target missed by ~500 bytes, so 223 takes the frame-socket tx buffer lever first. Was: one HTTP dispatch instead of picoserve's nested router: fixes the three findings of the first `screeny-probe http` device run (plain-text 405 for unknown verbs, `bad_request` vs `out_of_range` on an unconfirmed reboot), per-route body limits, and measures the RAM it buys; **gates 223** | yes |
| 223 | (in flight, hardware; scanning split out to 229) firmware: APSTA soft-AP, DHCP, DNS catch-all, the portal state machine wired to the store, the portal screen, the settings page (scan list, trial join) | yes |
| 224 | `crates/sim` serves the same HTTP API and models the WiFi/portal states through `crates/provision` - **done** (delivers 081; `screeny-sim --headless --http-port 8080 --start-in-portal`; sim suites 116 green, 64-rule conformance unchanged) | no |
| 232 | **done** - from 224's feedback: credentials posted while `Online`/`Joining` run a trial **without** the AP and fall back to the stored network, not the portal, with a sticky `FAILED`; `crates/device-api` gains the scan rate limit constant + `RateLimit` and `route::find` | no |
| 225 | spec: strike 8.1, rewrite 8.3 to end at the portal, add the HTTP API section (shared surface: notice to the software session) | no |
| 230 | button task: debounce, short press = identify screen | yes |
| 231 | hold ladder with the on-panel countdown; 5 s wipes WiFi -> portal; held-at-boot | yes |
| 240 | OTA staging over HTTP into the inactive slot, the five-check validator, per-sector timing (the esp-radio interrupt-window measurement) | yes |
| 241 | OTA activate / confirm / revert state machine, the "updating" screen, the health criterion | yes |
| 242 | rollback-capable bootloader - **built, committed, flashed by `fw-run.sh`**, boots, conformance 60/0/4; the app-side confirm/revert is card 241 | yes |
| 243 | panic breadcrumb in RTC memory, shown on the status page | yes |

Firmware cards touch the same files and share one device, so they run one at a time;
the `no` rows (211, 221, 224, 225) run in parallel with them.
