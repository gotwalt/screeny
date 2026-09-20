---
id: 180
title: Studio reads the device's own HTTP status, when the firmware serves it
type: build
hardware: no
depends: [170, 222]
owner: worker-180
branch: card/180-device-http-status
---

## Goal

The panel section of the Studio page shows what only the device knows - heap, free stack,
WiFi state and SSID, IP, firmware slot and state, reset reason, settings-store errors -
next to what the UDP telemetry already gives it.

## Context

- The firmware session defined the device's HTTP API in `crates/device-api` (package
  `screeny-device-api`): routes, serde types, one error shape, golden JSON for every request
  and reply in `crates/device-api/tests/golden/` (start with `status.json`). Design: "The
  HTTP API" in `docs/design/device-web.md`. The types round-trip through plain `serde_json`,
  so the Studio should **depend on the crate** and not restate the shapes (one
  implementation of each thing).
- The firmware does not serve it yet: card 222 (firmware session) is what makes it real, and
  card 224 adds the same API to `crates/sim`, which is what this card's tests run against.
  Both have merged (2026-09-20).
- Card 106 left exactly one seam for this: `crates/studio/src/fleet.rs`, the telemetry poll
  ("the single place the studio asks a device about itself"). Card 170 may have moved it;
  find where it lives now. The UDP control-port telemetry stays the fallback for firmware
  that has no HTTP server, and stays the source for frame counters.

- 2026-09-20: firmware card 224 has merged - `screeny-sim` serves the HTTP API
  (`--http-port`, `--start-in-portal`, `--no-http`; `SimHandle::http_addr()` / `http_url()`
  in tests; `Config::for_test()` uses an ephemeral port), `boot_id` included. So this card
  can be built against the sim as soon as card 170 has merged; the real device follows with
  firmware card 222.
- 2026-09-20: **unparked.** Firmware 0.4.0 (their card 222) serves the API on the real device,
  port 80, LAN side only: `GET /api/v1/status` (all fields real, `boot_id` included),
  `/telemetry`, `/wifi`, `POST /settings|identify|reboot|wifi`; `/networks` and `POST
  /firmware` answer 503 `unavailable` for now. Constraints from the firmware session, to be
  designed around: the device has ONE connection worker and no listen backlog, so a second
  simultaneous connection is dropped at SYN and retried by the OS a second later; keep-alive is
  off. So: poll `/api/v1/status` **no faster than every 10 s, one connection at a time,
  `Connection: close`, ~2 s timeout, capped backoff**, never from more than one task, and never
  let a browser trigger a poll (browsers read the Studio's cached copy). Measured by them: 200
  requests in 60 s during a 30 fps stream cost no frame.
- `wifi_state` in `/api/v1/status` can read `failed` while the device is online: until the
  firmware session's card 223 it is the sticky result of the last credentials *attempt*, not
  the link. Rule until then: a non-null `ip` means connected; show "last WiFi change failed"
  only as a note, never as a fault. After 223 it means the link (connected / connecting /
  disconnected) and the attempt's outcome lives in `GET /api/v1/wifi`.
- 2026-09-20: `screeny-probe --addr HOST http --list` (firmware card 228) documents, rule by
  rule, what a conforming device HTTP server does (38 rules, run against the sim in
  `crates/sim/tests/http_conformance.rs`); read it before writing the client. The sim's
  `/api/v1/status` now reports the *link* in `wifi_state`, as the design says.
- **The status payload contains the real WiFi SSID.** Show it on the owner's page; never write
  it into a tracked file, a fixture, a log line, a card or a screenshot (`CLAUDE.md`). Tests
  use the simulator, whose SSID is a dummy.
- Agreed with the firmware session (2026-09-19): the status payload will carry `boot_id`, a
  random u32 drawn once at boot (no flash wear). Same id = the link flapped; different id =
  the device rebooted. Use that to count reboots; do not infer them from uptime.

## Deliverables

- An HTTP poll of `GET /api/v1/status` on the attached device (bounded: timeout, capped
  backoff, one in flight, never on the render thread), merged with UDP telemetry into the
  status the page shows; absent or failing HTTP is normal and silent after the first note.
- The panel section shows the new fields plainly; a settings-store error or an unexpected
  `reset_reason` (brownout, panic, watchdog) is the one thing that should stand out.
- Tests against `screeny-sim` with the HTTP API on and off.

## Acceptance

With firmware that serves the API, the page shows the device's heap, WiFi and firmware slot;
with firmware 0.2.0 it looks exactly as it does today.

## Log

- **2026-09-20, worker-180.** Claimed, with card 181 folded in (same section of the
  page). Branch `card/180-device-http-status`, worktree of its own, no hardware and no
  LAN: everything below is against `screeny-sim` on loopback.

  Read first: the card's Context notes, `docs/design/device-web.md` ("The HTTP API"),
  `crates/device-api` (`StatusReply` and `tests/golden/status.json`), the 38 rules
  `screeny-probe --addr 127.0.0.1 http --list` prints, and the seam card 106 left in
  `crates/studio/src/fleet.rs`.

  What the rules changed in the plan: rule #31 says a lone request comes back in
  25-37 ms and inside 1 s when the one worker is busy, so a 2 s total deadline is
  generous rather than tight; rule #3 says `boot_id` is stable across two reads, which
  is what makes "count reboots from `boot_id`" honest.

- **The server half.** `crates/studio/src/devhttp.rs` is the client: one
  `GET /api/v1/status`, blocking, `Connection: close`, 2 s for connect + write + read
  together, a 4 KB ceiling on the reply, and a `Fault` that says whether the device
  simply has no HTTP server. **No HTTP client dependency** - the whole file is 200
  lines over `std::net`, and what this device needs of a client is the opposite of
  what a client crate is for: no pool, no keep-alive, no retries of its own. The
  precedent is already in the tree twice (`crates/sim/src/http.rs`, and this crate's
  own test client).

  `crates/studio/src/fleet.rs` grew `spawn_device_http`, beside the telemetry poll it
  was always going to sit next to. Every rule the firmware session asked for is a
  property of that one task rather than a comment: one poller, and because the loop
  `await`s each read before starting the next, **one connection in flight across the
  whole fleet** - not merely one per device. Backoff is the telemetry poll's own
  `fail()`; a device that has no server is pushed straight to the cap (2 min) rather
  than climbing to it, since only a firmware update changes that answer.

  `crates/studio/src/devices.rs` grew `DeviceFacts` (the reply `#[serde(flatten)]`ed,
  so a field the firmware adds reaches the page in the same commit), `HttpHealth`, and
  `DeviceRecord::http_addr` - **derived** from the resolved frame address and port 80,
  never stored, for the same reason the registry is keyed by id and not by address.

  Two numbers picked, with the owner's healthy device (`stack_free 4312`,
  `heap 47240/98304`) as the anchor: `LOW_STACK = 2048` (half the healthy reading -
  about one exception frame plus picoserve's buffer, and card 222 already watched it
  fall to 5.2 KB under load) and `HIGH_HEAP = 0.85` (the device sits at 48% and the
  simulator at 67%, so a line at 85% is a real change and still leaves ~14 KB).

  **The SSID.** It is in the payload, it goes on the owner's page and into the
  studio's own `/api/v1/status`, and nowhere else. `DeviceFacts` has a hand-written
  `Debug` that prints `ssid: <redacted>`, because `DeviceRecord` derives `Debug` and
  that would otherwise be one `{:?}` from stderr; the poller hands the reply straight
  to the registry and never holds it; `heard_http` returns counts rather than the
  payload so the log lines cannot reach it; and nothing new is persisted.

- **The tests.** `crates/studio/tests/device_status.rs` (5) and `tests/ssid.rs` (2), all
  against the simulator on loopback, ports 50800-51440, mDNS off, every wait on a
  condition with a deadline:

  - *with HTTP*, one read carries heap, free stack, slot, state, reset reason, WiFi and
    the device's own `boot_id`, **and** the UDP telemetry beside it - asserted out of the
    same read, because a wait satisfied by the HTTP half alone is a different moment;
  - *without HTTP* (`http: false`, the sim's `--no-http`), `facts` is null, telemetry
    carries on alone, `last_error` stays null, `problems` is empty and `/healthz` is 200;
  - *a reboot* - the simulator stopped and another put on the same ports, which draws a
    fresh `boot_id` - is counted once, and reading the same device again is not a second;
  - *one connection at a time*: a counting server that holds each connection open for
    400 ms against a 100 ms poll period. If the poller started a request per tick rather
    than awaiting the last, it would overlap immediately. Measured: **at most 1 open at
    once**, every request carrying `Connection: close` and a `Host` naming the device;
  - *an endless reply* (a server that sends `x` for ever with no `Content-Length`) is
    refused at the 4 KB ceiling rather than read into this process.

  The SSID test is run **against the real binary as a subprocess**, because `eprintln!`
  goes to a file descriptor and reading that descriptor is the only honest test of "it
  never reaches the log". A studio with `--no-discover`, its own temp state dir, an
  ephemeral port and `--device-http-port` at the sim; the network is called
  `Zzyzx-Not-A-Real-Network-41`, which appears nowhere else in the repo. It asserts the
  name **is** on `/api/v1/status` (that is the point) and is in neither stdout, stderr
  nor `state.json` - and that no device fact at all is in `state.json`, since these are
  live data. A `Guard` with a `Drop` kills the child however the test ends; it is stopped
  with SIGTERM, as `docker stop` does, so the state file is written the way it is in life.

- **The page.** A "Device" block at the foot of the panel section, under a rule, in the
  same facts grid and the same three tones the section already uses: **Slot** (`ota 0 ·
  valid`), **Up**, **Memory**, **Free stack**, **WiFi** (`<network> · -54 dBm`),
  **Reboots**, **Last reset**, **Store errors** (only when there are any), **When idle**.
  Quiet by default; the four things that catch an eye are the four that mean something
  *happened to* the panel rather than in it - an unexpected reset reason, a store error,
  a slot that is not `valid`, and memory running out.

  Which is which is decided **once, on the server**, beside the reasoning: the page reads
  `low_stack`, `low_heap`, `bad_fw_state`, `odd_reset` by name and carries no threshold
  of its own. A test greps `showDevice` for `2048` and `0.85` to keep it that way.

  No duplication: with facts present, `Up` and `Signal` leave the list above (they are in
  the Device block now), and the firmware *version* stays above while the block says
  `Slot`. With no facts, the block is `hidden` and the page is byte-for-byte the page it
  was. Two quiet notes under the block: "the last WiFi change failed; it is still on the
  network it had" (the card's rule until firmware 223 - never a fault), and the portal
  being up.

- **Card 181: kept live, relabelled.** With no panel attached the switch reads *"Drive a
  panel as soon as one is found"*; with one, *"Show it on the panel"*. Disabling it was
  the other option and is the wrong one: the switch is not decorative, `state.on` on the
  unbound player is what makes the first panel found start playing without anybody
  pressing anything, and greying it out would take that choice away while the page still
  had to explain the behaviour somewhere. Relabelling makes the control *say* the rule
  instead. `set_panel`'s two bodies (`{"on":false}`, `{"on":true,"to":"..."}`) and their
  effect are untouched, and `tests/panel.rs::set_panel_hands_the_panel_over_and_takes_it_back`
  still passes unchanged.
