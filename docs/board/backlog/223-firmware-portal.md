---
id: 223
title: Firmware - the setup portal: soft-AP, DHCP, DNS catch-all, the state machine wired in, the QR screen on the panel, the WiFi form
type: build
hardware: yes
depends: [212, 221, 222, 226, 232, 233]
owner:
branch:
---

## Goal

A device with no usable WiFi brings up its own open network `screeny-<id>`, shows the QR
code and the network name on the panel, captures a phone into a one-page setup form, tries
the credentials it is given **without committing them**, tells the person what happened,
and only then stores them and takes the setup network down. A device that is online and
loses its network for an hour heals itself. This is the card the whole device-web track
has been building toward; most of its logic already exists and is tested on the host -
this card is the wiring, the radio, and the RAM.

Not in this card: network **scanning** / the scan list (`GET /api/v1/networks` stays
`unavailable`; the form takes a typed SSID - card 229), the button (230/231), OTA (240+).

## Context

Read first, in this order: `CLAUDE.md`; **`docs/design/device-web.md` - all of it**; it is
the design, including decisions 1, 2, 6, 7, "HTTP, soft-AP, portal (card 201)", the
card-221 refinements, "Credentials posted while the device is online (card 232)", "The
HTTP API" with the `wifi_state`-means-the-link decision, the credentials bench rule, and
the RAM sections. Then `crates/provision/README.md` and `src/machine.rs` (**the state
machine you drive - you implement none of its rules**), `src/screen.rs` (the portal screen
renderer) and `src/uri.rs`; `crates/device-api/src/route.rs` and `src/form.rs`;
`crates/sim/src/wifi.rs` and `src/api.rs` (card 224/232/228: a complete, tested reference
for driving the `Provisioner` from `SET_WIFI`, `POST /api/v1/wifi`, join results and
timers, and for reading `GET_WIFI`, `GET /api/v1/wifi`, `status.wifi_state` and the
telemetry overlay out of it - **read it before writing the firmware's version**);
`docs/research/007-device-web-and-portal.md` sections 2 (APSTA API at our `esp-radio`
version, the single-PHY channel rule), 3 (`edge-dhcp` / `edge-captive` at our pins, over
plain UDP sockets), 4.3-4.4 (what each OS probes and what we must answer: DNS answers
everything with 192.168.4.1; unknown hosts get a **302 with a non-empty body**; no
`fetch()` polling on the provisioning path - plain form post and a timed full-page
reload; no file inputs in captive browsers); `docs/research/009-ram-headroom.md` and
`010-stack-and-ram-levers.md` (section 8 and the budget for this card);
`firmware/src/apsta_probe.rs` (card 220: a **working, device-proven** APSTA bring-up with
a second `embassy-net` stack on 192.168.4.1/24 - start from it) and
`firmware/src/web_spike/{ap,portal,qr}.rs` (card 201's compile-only spikes of `edge-dhcp`,
`edge-captive` and the QR: evidence that the calls type-check - **retire `spike-ap`,
`spike-portal`, `spike-qr` and `device-web-spike` when the real thing exists**);
`firmware/src/main.rs` (`station_loop`, `NewWifi`, `persist_joined`), `http.rs`
(`Dispatch`, the catch-all hook card 233 left before routing), `net.rs`, `receiver.rs`
(`Host::wifi`), `screens.rs`, `store.rs`, `mdns.rs`.

Hard facts already paid for - do not rediscover:

- **Credentials are stored only after they have joined**, by the WiFi task
  (`persist_joined`). Never write flash from inside an HTTP handler (it also puts 4 KB
  flash frames on top of the request path). The `Provisioner`'s `CommitCredentials`
  action is the *only* thing that stores WiFi credentials once it is wired in; the
  existing ad-hoc fallback logic in `station_loop` (`active`, `persist_on_join`,
  `WIFI_SET_FAILED`) is replaced by the machine, not kept beside it.
- The machine never sees a PSK. The firmware holds the posted pair (RAM only) keyed by the
  trial, and writes it on `CommitCredentials { Trial }`.
- Feed **every** `POST /api/v1/wifi` and `SET_WIFI` to the one `Provisioner` whatever its
  state; reply first, wait the 100 ms the reply needs to leave, then `step`. A post while
  `Online` is a `TrialOrigin::Online` trial: no AP, no overlay, no portal screen, falls
  back to the stored pair.
- `status.wifi_state` = the link; the sticky trial result lives in `GET /api/v1/wifi`
  (`trial_is_current()`, `trial()`, with `reason` = `auth` / `not_found` / `other` from
  the radio's failure reason - today it is `null`); UDP `GET_WIFI` = `wifi_state()`.
  Telemetry state = `overlay_state()`. When this lands the probe's `CARD_223_LANDED`
  becomes true - **the orchestrator flips it, not you**.
- `bench-wifi` builds keep their meaning: `has_builtin = true`.
- RAM: fw 0.4.3 is `.stack` 34,352 with ~13 KB of demand; the full spike cost 15.8 KB of
  `.stack` (6 KB of it a scratch `Frame` the real portal does not need: the portal screen
  renders into the existing triple buffer like every other screen). Without a lever this
  card lands at ~24.7 KB against the 24,576 floor of `tools/fw-size.sh`. **First
  deliverable is therefore a lever**: the frame socket's transmit buffer is
  `2 * MAX_DATAGRAM` (2,944 bytes) for replies that are tens of bytes (a piggybacked
  `TELEMETRY`, a `BUSY`): read spec sections 6.4 and 6.7 for the largest thing the frame
  port ever sends, size it to that with margin, and say what you chose. If that is not
  available for a reason you find, the fallback levers are `MDNS_BUF` (1500 twice over)
  and sharing the boot path's two 3 KB partition-table buffers. **Exit: `.stack` >= 26 KB
  on the default build**, floor untouched.
- The AP and its services exist only while the machine says `ap_up()`: `RaiseAp` /
  `DropAp` start and stop the AP interface's DHCP server, DNS catch-all and the HTTP
  listener on 192.168.4.1. Whether the AP-side network stack and its tasks are created at
  boot and idle, or created on demand, is your call - **measure both if it is cheap**;
  static tasks that idle are simpler and `.bss` is the cost. HTTP on the AP side can be a
  third `http_task` bound to the AP stack or the same pool listening on both stacks -
  your call, justify it with numbers.
- The frame path is the product: APSTA idle was measured at 30 fps with zero drops and
  unchanged `render` (card 220). A phone associating, DHCP, DNS and page loads on the AP
  must not change that for a sender on the LAN side; your serial telemetry is the
  evidence.
- You cannot reach the LAN or the AP from a worker environment, and you have no phone.
  So: (a) extend the on-device `http-selftest` style of evidence: an in-memory pass over
  the portal-side routes and the catch-all (a `Host: captive.apple.com` request gets the
  302-with-body only while `ap_up()`, a 404 otherwise; the WiFi form GET and POST); (b) a
  bench feature **`start-in-portal`** (off by default): boot as if the store held no
  credentials and there are no built-ins - *without erasing anything* - so the device
  comes up in `Portal` with the AP raised, the QR on the panel, and (since the real
  credentials are still in the store) able to complete a real trial when the owner types
  them into the form. The orchestrator and the owner do the phone test with that build.
  (c) Log, at `info`, every `Action` the machine emits and every state change (never an
  SSID other than the AP's own, never a PSK), AP client associate/leave, DHCP leases (the
  leased address only), and DNS queries answered (count per 10 s, not per query).

## Deliverables

1. The lever above, first, as its own commit with `tools/fw-size.sh` before/after.
2. `firmware/src/provision.rs` (or similar): owns the `Provisioner`, turns radio events
   and timers into `Event`s, carries out `Action`s (join with
   `ScanMethod::AllChannels`, stop, raise/drop the AP via `Config::AccessPointStation`
   <-> `Station`, commit/clear through `store.rs`, announce), and exposes what `http.rs`,
   `receiver.rs` and the display read. The 10-minute portal retry, the 30 s AP grace, the
   60 s link-down rule and the u32 wrap all come from the machine; you supply `now_ms`
   and a tick.
3. The AP side: open AP `screeny-<id>` (channel follows the station when there is one),
   static 192.168.4.1/24, `edge-dhcp` leasing a small pool with option 114, `edge-captive`
   answering every name with 192.168.4.1, HTTP on the AP address.
4. HTTP: the catch-all before routing; `GET /` on the AP side serves the **setup page**
   (SSID text field, password field, submit - plain form post, works with JavaScript off
   and inside the iOS/Android captive mini-browsers; after the post, a page that reloads
   itself every few seconds with a full navigation and shows "trying...", then "connected
   - this device is now at 192.168.x.y, you can close this" or "wrong password" /
   "network not found" with the form again). The LAN-side page gains the same WiFi form
   in the place card 222 marked. Sizes reported.
5. The panel: `screeny_provision::screen::render` output whenever `screen(now_ms)` is
   `Some`, through the existing idle-screen path (`Intent`/overlay rules: the portal
   screen is an overlay like `IDENTIFY`; frames from a LAN sender are still decoded
   underneath). Brightness as set; no full-white frame.
6. `status.wifi_state`, `status.portal`, `GET /api/v1/wifi` (`reason` real), `GET_WIFI`,
   the telemetry overlay - all read from the machine. The failure idle screen's job is now
   the portal's.
7. The `start-in-portal` bench feature and the in-memory portal self-test; one device run
   of each, plus the default build left running. `FW_VERSION` -> `0.5.0`.
8. Sizes and stack: `tools/fw-size.sh` before/after each major step, the 60 s `stack:`
   line, heap max usage with the AP up and a (self-test) client active.

## Bench discipline (orders)

Iterate on the host first; expect 5-8 flashes; write in the Log why each happened. Flash
only with `/Users/aaron/src/screeny/tools/fw-run.sh <elf> <name> [secs]` wrapped in
`timeout 400`, secs <= 200. No other monitors. If a build does not boot, or does not
**rejoin WiFi from the store**, reflash the last good default build before diagnosing -
and if the device will not join with a known-good build either, stop and tell the
orchestrator at once (SendMessage to "main"): recovering a bad stored pair needs the
orchestrator. **You never post, type, read or log the real credentials.** `pgrep -fl
espflash` empty when you finish. An open AP named `screeny-4a00a4` will be on the air in
the owner's home during your portal runs: keep those runs bounded (the `secs` window) and
finish on the default build, which raises no AP while it is online. Serial logs contain
the real SSID and BSSIDs: never copy those lines.

Ask the orchestrator (SendMessage to "main") for: HTTP load / both conformance suites
against a build (standard offer, as in cards 227/233); the **phone test** of the
`start-in-portal` build - say when that build is on the device and what the owner should
see at each step; at most three windows in total.

## Out of scope

Scanning and the scan list (229); the button (230/231); OTA (240+); `crates/*` (if
`crates/provision` or `crates/device-api` lacks something, wrap it locally and report -
they were each shaken down by the simulator, so a gap is news); the spec (card 225
rewrites 8.1/8.3 afterwards); `docs/design/*`; `tools/` other than nothing.

## Acceptance

- Worker: exit `.stack` >= 26 KB; every existing feature building (minus the retired
  spikes); self-tests green on the device; with `start-in-portal`, serial shows Portal ->
  AP up -> (self-test client) -> the machine's actions; the default build left running,
  joined from the store, streaming at 30 fps with zero drops.
- Orchestrator + owner, after: the phone test end to end (QR scan joins the AP, the
  captive sheet opens by itself on iOS, the form, a wrong password reported as such, the
  right one joins, the page shows the new address, the AP goes away 30 s later, the
  stream resumes); `screeny-probe http` with `CARD_223_LANDED` true and
  `--allow-wifi-trial --allow-reboot`; UDP conformance 60/0/4; the HTTP hammer during a
  stream.

## Log
