---
id: 223
title: Firmware - the setup portal: soft-AP, DHCP, DNS catch-all, the state machine wired in, the QR screen on the panel, the WiFi form
type: build
hardware: yes
depends: [212, 221, 222, 226, 232, 233]
owner: worker-223
branch: card/223-firmware-portal
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

### The lever, first (deliverable 1)

Baseline, fw 0.4.3 as merged, `tools/fw-size.sh`:

```
  .data      58380   .bss  103872   .stack  34352   image 910041
```

**The lever: the frame socket's transmit buffer.** It was `2 * MAX_DATAGRAM`
(2,944 bytes) since card 008, by symmetry with the receive side - and the
symmetry is false. The frame port *receives* frames; the only things it ever
**sends** are section 6.2's unsolicited replies, and spec 6.4/6.7 makes the
largest of them a `TELEMETRY`: an 8-byte control header plus a 48-byte body =
**56 bytes on the wire**. `BUSY` is shorter. The shared receive core bounds it
from the other end too: `screeny_receiver::OUT_MAX` is 64 and the `Outbox` holds
four of them, so 256 bytes is the most that can ever be queued at once.

Chosen: **512 bytes** (`8 * OUT_MAX`) - the whole queue counted twice over. The
four `tx_meta` slots are untouched; they, not the byte count, bound how many
datagrams are in flight.

```
  .data      58380   .bss  101440   .stack  36784   image 910033
```

`.stack` 34,352 -> **36,784**, +2,432 bytes, exactly the arithmetic. The
`fw-size.sh` floor (24,576) is untouched. This is the budget card 223's AP side
is spent out of.

### The portal itself, built (deliverables 2-6), first build

`firmware/src/provision.rs` is the new half: it owns the `Provisioner`, the
radio and the AP's services, and it implements none of the machine's rules.
`crates/provision` and `crates/device-api` were **not** touched.

Shape, in brief:

* One `Provisioner` behind a *blocking* critical-section mutex, because
  `receiver::Host::wifi` is synchronous and must answer `GET_WIFI` without
  awaiting. Readers hold it for a copy and nothing else - a QR encode inside a
  critical section would mask core 1's HUB75 DMA interrupt for its duration -
  so `provision::screen()` copies the machine's answer into an owned
  `PanelScreen` and `net.rs` draws it with the lock released.
* `provision_task` owns the `WifiController`. `StartJoin` -> `connect_async`
  bounded by the machine's own 15 s, then `wait_config_up` for the address (a
  `Joined` carries one), then `Joined` / `JoinFailed`. `LinkUp` / `LinkDown`
  are *derived on a 1 s tick* from `is_connected() && config_v4().is_some()`
  rather than taken from `wait_for_disconnect_async`, so a radio restart -
  which is what raising or dropping the soft-AP is - produces the same edge as
  an access point going away, and heals the same way.
* The one borrow that could not be had: AP client events are
  `wait_for_access_point_connected_event_async(&self)` and a join is
  `connect_async(&mut self)`, so while an attempt is in flight the AP's
  associate/leave events are not observed. Harmless: the only rule that reads
  `ap_clients()` is the ten-minute portal retry, which fires in `Portal`, where
  no join is ever in flight. Written down in the module docs.
* AP side: one `embassy-net` stack at 192.168.4.1/24 built at boot and left
  idle (`.bss` is paid either way, so building it on demand buys nothing in the
  pool that breaks first), `StackResources<4>`; `dhcp_task` and `dns_task` bind
  their sockets only while `ap_up()` and drop them when it goes away.
* HTTP on both sides with **no third worker**: `Dispatch` carries an `ap` flag,
  worker 0 stays on the LAN and **worker 1 follows the soft-AP**. A third
  worker is 7.5 KB of `.bss` - three times the whole lever - and while the AP
  is up the station is, by the machine's rules, not on a network, so the
  borrowed worker has nobody to have served.
* `/setup` (GET and POST) on both interfaces answers **HTML**, because what
  posts to it is a plain `<form>` in a captive mini-browser; `POST
  /api/v1/wifi` is unchanged and still answers JSON. Both end at
  `crate::NEW_WIFI` and the one machine. `GET /` is the form on the AP side and
  the status page on the LAN side. The catch-all returns a 302 **with a body**
  to `http://192.168.4.1/` for an unknown path, and only while `ap_up()`.

**A second lever was needed.** The AP side cost 11,144 bytes, not the ~9.7 KB
the card projected, which put `.stack` at 25,640 - over the 24,576 floor but
under the card's 26 KB exit. So the card's named fallback was pulled too:
`MDNS_BUF` 1500 -> **1024** (it is spent four times over: the socket's rx and
tx and two `VecBufAccess`es), worth 1,904 bytes, and `AP_SOCKETS` 5 -> 4, worth
another ~408. 1024 is not a guess: what this device sends is under 600 bytes
(two services' PTR/SRV/TXT/A, TXT bounded by `INFO_MAX` = 224) and 1024 is
comfortably over the 576-byte message every DNS implementation must accept.

```
fw 0.4.3 baseline   .data 58380  .bss 103872  .stack 34352  image 910041
+ frame tx lever    .data 58380  .bss 101440  .stack 36784  image 910033
+ the whole portal  .data 58988  .bss 111976  .stack 25640  image 968405
+ mdns + ap sockets .data 58988  .bss 109664  .stack 27952  image 968397
```

**Exit `.stack` 27,952** against the card's 26,624 (26 KB) and the script's
24,576 floor. Flash: +58 KB of image, which a 2 MB slot has.

`FW_VERSION` -> **0.5.0**.

**Retired**, as the card instructed: the cargo features `device-web-spike`,
`spike-ap`, `spike-portal` and `spike-qr`, and with them `firmware/src/web_spike.rs`
and `firmware/src/web_spike/{ap,portal,qr}.rs`. `edge-nal`, `edge-captive` and
`edge-dhcp` stop being optional; `qrcodegen-no-heap` stops being a direct
dependency at all and arrives through `screeny-provision`.

Every other feature still builds, checked one at a time: default,
`start-in-portal`, `http-selftest`, `store-selftest`, `apsta-probe`,
`spike-ota`, `display-on-core0`, `fb-on-stack`, `gpio-probe` (both binaries)
and `bench-wifi`. `bench-wifi` was type-checked with the documented dummies in
the environment (`build.rs` prefers the environment over either file), so no
real credential was read, built or written at any point. `apsta-probe` lost its
call into `station_loop`, which card 223 deleted; it has its own three-line
"connect, then poll the RSSI" instead, and the heap measurement either side of
`set_config(AccessPointStation)` is unchanged.

### Three bugs found by reading it back before the first flash

Read before flashing, not after, because a boot loop on this bench costs a
flash and a reflash:

1. **`step()` published `AP_UP` and that broke `RaiseAp`.** The machine flips
   its own `ap_up` *inside* `step`, so publishing it there made
   `Driver::raise_ap` see the AP as already up and return without ever putting
   the radio into APSTA - no soft-AP at all, which is the whole card. The two
   actions own the flag now, which also makes it mean "the radio and the
   services agree" rather than "the machine intends to".
2. **`CommitCredentials { Builtin }` would have overwritten a stored pair.**
   The machine reaches `Builtin` down two roads - nothing stored, and a stored
   pair that failed three times - and `store::seed_wifi` has no emptiness guard
   in it (the guard was in `main`, in the boot-time seeding this card
   removed). On a `bench-wifi` build whose stored pair had gone bad, that would
   have quietly replaced the owner's credentials with the bench network, which
   is exactly what `station_loop`'s "deliberately *not* stored" comment existed
   to prevent. The guard is now in the one place that can commit.
3. **A stray `Tick` when a post interrupted a join attempt.** The machine turns
   `CredentialsPosted` into `StopJoin` + a new `StartJoin` by itself; feeding a
   `Tick` first could expire the attempt and start a *different* join between
   the two.

`.stack` unchanged at 27,952 after the fixes.

### Flash 1 (`c223-selftest`, default + `http-selftest`): it booted, and it could not get online

Boot, panel, store, the machine and the AP all came up - and the station never
joined. Three attempts, every one of them *associated*, and then:

```
WARN - wifi: associated, but DHCP did not answer in 8 s
WARN - wifi: the radio did not answer the attempt in 15 s
WARN - wifi: associated, but DHCP did not answer in 8 s
INFO - provision: joining -> portal
```

Two faults, and the second is one this project has already paid for once:

1. **Eight seconds was under the measurement.** The capture logs of cards 227
   and 233 have two or three five-second telemetry lines between the
   "connected" line and the mDNS announcement, so **this mesh answers DHCP in
   ten to twelve seconds**. `DHCP_WAIT` is 20 s now, and it is a window of the
   firmware's own on top of the machine's 15 s rather than a share of it: the
   machine's deadline exists so a *silent radio* still advances, and a radio
   that has associated is not silent.
2. **The retry called `connect_async` while still associated, and it hung** -
   which is exactly the failure card 212 recorded on this bench ("without this,
   `connect_async` was called while still associated and never returned"). The
   DHCP timeout leaves the association up, so every retry walked into it.
   `run_join` now drops any association it finds before configuring the radio,
   and so does the re-associate after the soft-AP goes down.

While in there: `ap_net_task` now runs the AP stack's `embassy-net` runner
**only while the AP is up**. A runner polls its driver, and the soft-AP
interface has nothing to poll for the months this device spends online; core 0
also decodes thirty frames a second. `.stack` 27,952 -> 27,928 (the guard costs
24 bytes).

### Flash 2 (`c223-selftest2`, the same build corrected): green

```
provision: boot -> joining
provision: action StartJoin { which: Stored, attempt: 1 }
mdns: screeny-4a00a4.local -> 192.168.7.221 ...
provision: joining -> online
provision: action Announce
```

Joined from the store on the **first** attempt, announced, and streamed.

* **Self-test, LAN pass: 20 of 20 right**, including card 222's twelve and card
  233's eight, unchanged.
* **Self-test, portal pass: 11 of 11 right.** The soft-AP is down on this
  build, so the three captive probes correctly fall back to `404` - the
  `302`-with-a-body half of that pair is the `start-in-portal` run below. The
  rows that do not depend on the AP being up all pass: `GET /` is the **setup
  form on the AP listener (1,297 bytes) and the status page on the LAN one
  (6,950)**, `GET /setup` and `POST /setup` answer HTML on both, an oversize
  form is `413`, `PUT /setup` is `405`, and a `Host: captive.apple.com` request
  on the **LAN** listener is a plain `404` - which is the row that proves the
  redirect is a property of the listener and not of a header.
* `GET /api/v1/wifi` -> `{"state":"connected","ssid":"<ssid>","ip":"192.168.7.221","reason":null}`
  (the self-test's own redaction; the firmware still says an SSID out loud in
  exactly one line, the WiFi task's "connected").
* **The frame path did not notice.** Over the self-test window, with 31
  requests through the real router and the real handlers: `31 fps rx, 31 fps
  shown, decode drops 0, rejected 0, render max 3301 us` - the same numbers as
  fw 0.4.3 with no server running.
* The 60 s line: `stack: core 0 main high-water 13056 of 22552 bytes, 8472
  free`; core 1 `1872 of 6144`. **The demand has not moved** - 13,056 is the
  boot path, the same number cards 227 and 233 measured - and the whole HTTP
  route table plus the portal pass added *nothing* to it (`13056 -> 13056`
  across the in-memory run). The headroom is 8.3 KB rather than 233's 14.6 KB
  because that is what the AP side cost.

### Flash 3 (`c223-portal`, `start-in-portal` + `http-selftest`): the portal, on the device

One fix went in first: `selftest_task` used to `return` when the stack had no
address, which is precisely the `start-in-portal` case - the first attempt at
this run printed nothing at all. The TCP half is skipped now and the in-memory
pass goes ahead, which is the half that was ever interesting.

```
WARN - provision: BENCH BUILD - ignoring the stored credentials (they are NOT erased)
INFO - provision: ap screeny-4a00a4 | stored credentials false | built-in false
INFO - provision: boot -> portal
INFO - provision: action RaiseAp
INFO - provision: soft-AP screeny-4a00a4 up on channel 1, portal at 192.168.4.1
INFO - dhcp: serving 192.168.4.50-53 on the setup network
INFO - dns: catch-all up, every name answers 192.168.4.1
```

**Portal pass, 11 of 11 right, this time with the AP actually up:**

```
selftest: portal pass, soft-AP is up
AP  captive.apple.com (302)     -> 302 OK 283 bytes | location "http://192.168.4.1/"
AP  android generate_204 (302)  -> 302 OK 283 bytes | location "http://192.168.4.1/"
AP  windows ncsi (302)          -> 302 OK 283 bytes | location "http://192.168.4.1/"
LAN captive.apple.com (404)     -> 404 OK 109 bytes
AP  GET / is the setup form     -> 200 OK 1297 bytes
LAN GET / is the status page    -> 200 OK 6935 bytes
```

283 bytes of body on the redirect, which is the thing research 007 section 4.3
insists on (Android calls `Content-Length <= 4` a failure and iOS wants
something to render). The LAN row is the one that matters most: the same
request, the same `Host:` header, a plain 404 - so the catch-all is a property
of **which listener answered** and not of anything a client can claim.

The 20 LAN routes are right too, on a device with no address: `GET
/api/v1/wifi` reads `{"state":"disconnected","ssid":null,"ip":null,"reason":null}`,
which is the machine's `Portal` answer with nothing having failed yet.

Numbers with the soft-AP up, DHCP, DNS and the HTTP worker on it, and a
self-test client running:

* telemetry `state 4` - the `PROVISIONING` overlay, from `overlay_state()`.
* `stack: core 0 main high-water 13056 of 22600 bytes, 8520 free`; core 1
  `1648 of 6144`. **The demand is still 13,056**, the boot path's, and the
  in-memory pass across all 31 routes moved it by zero.
* `heap 47932/90112` sampled throughout, against 45,416 on the station-only
  build: **the whole AP side costs ~2.5 KB of heap**, and 42 KB was free at
  every sample. (esp-alloc's true all-allocations watermark needs
  `internal-heap-stats`, which only the `apsta-probe` build turns on; card
  227's APSTA watermark was 54,040 of 90,112.)
* No panic, no backtrace, no `ERROR`, and the only `WARN` is the bench build
  announcing itself.

What this run cannot show is the panel and a phone, which is what the phone
test is for.

### The phone test, for after the merge

**Build:**

```
. ~/export-esp.sh
cd firmware && cargo build --release --features start-in-portal
```

(add `,http-selftest` for the route pass on the serial log too). Flash with
`tools/fw-run.sh <elf> <name> <secs>` as usual. **Nothing about this build
erases or overwrites the store** - it tells the machine `has_stored: false` and
never reads the credentials - so the owner's real pair is still there to be
typed into the form and really joined with.

What the owner should do and see, step by step:

1. The panel shows the setup screen, alternating every 4 s between a QR code on
   the left with `screeny-4a00a4` beside it, and a text-only screen naming the
   network and `192.168.4.1`. **There is no stream on this build** - the
   station deliberately never joins - and that is expected.
2. Scan the QR with the phone camera, or join the open network `screeny-4a00a4`
   by hand.
3. On iOS the captive sheet should open by itself within a few seconds, showing
   "screeny setup" and two fields, Wi-Fi network name and Password. On Android
   it is a "Sign in to network" notification. If neither appears, open
   `http://192.168.4.1/` in a browser.
4. **Type a wrong password first**, with the right network name, and submit.
   The page says "Trying that network..." and reloads itself every 4 s - a full
   page navigation, no JavaScript. Within ~15 s it comes back with "That did
   not work: wrong password." and the form again. The panel stays on the setup
   screen. **Nothing is written to flash on this path**, which is the thing
   being tested.
5. Then the real network name and the real password. Same "Trying that
   network..." page. The phone may briefly lose the setup network while the
   radio changes channel; the page's own reload recovers from that.
6. On success the page shows "Connected. This panel is now at
   http://192.168.7.x/" and says the setup network goes away in about half a
   minute. **The panel shows the same address** for a minute - it is the one
   channel that survives the radio moving.
7. About 30 s later the setup network disappears by itself, the device
   re-associates, and the Studio's stream comes back within a minute or so.

### Flash 4 (`c223-default`): the default build, left running

The owner was not at the keyboard (early morning), so on the orchestrator's
instruction the portal build came off at once rather than leaving an open AP on
the air in the house, and the phone test moves to after the merge.

### The default build, left running (`c223-default`), and the bench window

```
provision: boot -> joining
provision: action StartJoin { which: Stored, attempt: 1 }
mdns: screeny-4a00a4.local -> 192.168.7.221 ...
provision: joining -> online
provision: action Announce
lock: taken by 192.168.7.6:48745
state: IDLE -> LIVE
telemetry: 30 fps rx, 30 fps shown, 155 swaps/s | drops stale 0 superseded 0
  decode 0 rejected 0 gaps 1 | ... | state 1 codec 0x10 rssi -54 bright 96 |
  heap 45540/90112
```

Joined from the store on the first attempt, announced, the Studio took the
lock, and it held 30 fps with **zero decode drops and zero rejects** through the
orchestrator's whole window. The one `gaps 1` is the stream's own reconnect.

The 60 s line: `stack: core 0 main high-water 13056 of 27928 bytes, 13848
free`; core 1 `1872 of 6144`. Under the probe's HTTP load the mark moved once,
to 14,416 (`12488 free`) - which is exactly the `stack_free` the orchestrator
read out of `/api/v1/status` a moment later.

Orchestrator's window, fw 0.5.0: 120 s alternating loop **1,136 requests, 1,043
answered 200**, no failures once up; `screeny-probe http` **30 pass, 0 fail, 8
skip - identical to 0.4.3**, the eight being the expected ones; connects
0.0104/0.0357, 0.0126/0.0903, 0.0107/0.0382 s; `/api/v1/status` at 110 s uptime
`fw 0.5.0, stack_free 12488, heap_used 45540/90112, wifi_state connected,
portal false, state live, store_errors 0`; `GET /api/v1/wifi` `state connected,
reason null`; and a `Host: captive.apple.com` probe **on the LAN side answered
404**, over the wire, which is the self-test's row confirmed by a real client.

The one blip the orchestrator saw (an empty body on a `GET /api/v1/wifi` fired
immediately after another request, not reproducible in three retries): **there
is nothing in the serial log at that time.** No `WARN`, no reset, no refused
connection, no drop - the only lines around that uptime are the probe's own
`SET_NAME` re-announcing mDNS as `probe-228` and back. Both workers were on the
LAN (the soft-AP is down on this build, so the AP-following worker is an
ordinary second LAN worker), so it is not the stack switch. Left as an
unexplained one-off; if it recurs it wants its own card with a packet capture.

### `.stack` versus the painted region: the plain answer

The orchestrator read `22,600` off a `stack:` line and reasonably feared a
floor failure. The two numbers **are** the same quantity - `stack_probe`
paints `_stack_end_cpu0.._stack_start_cpu0`, which is the `.stack` section -
but they came from two different **builds**:

| build | `.bss` | `.stack` |
|---|---|---|
| default (what is on the device) | 109,688 | **27,928** |
| `--features http-selftest` | 114,968 | 22,600 |
| `--features http-selftest,start-in-portal` | 114,976 | 22,600 |

The card's exit is the default build's, and it is 27,928: 3,352 over the 24,576
floor, 1,304 over the card's 26 KB. **But note the second row.** The
`http-selftest` feature carries a second, separately monomorphised copy of
picoserve's whole `serve` machinery, and that is 5,328 bytes of `.bss` - which
now puts a *bench* build under the floor for the first time (on 0.4.3 the same
feature landed at ~29 KB). `tools/fw-size.sh` exits non-zero on it. That build
ran fine on the device twice, high-water 13,056 of 22,600, but it is worth a
follow-up card: either the self-test shares the workers' buffers instead of
declaring its own, or the script learns that the floor is a rule about builds
that get flashed as the product.

### What `crates/provision` and `crates/device-api` were missing, from this side

Neither crate was touched (the card said not to). Both held up: every rule
about joining, retrying, the trial, the sticky failure, the AP grace and the
two portal layouts came out of the machine and none of them had to be
re-decided here. Five things were awkward, all small, all news:

1. **There is no `link_state()` on the `Provisioner`.** `status.wifi_state`
   means the link, and the derivation from `p.state()` is a four-arm match that
   now exists **twice** - in `crates/sim/src/wifi.rs` and in
   `firmware/src/provision.rs` - written to be identical on purpose. That is
   exactly the drift card 228's rule 8 caught the first time. It belongs in the
   machine.
2. **`Screen<'a>` borrows the machine.** A caller that cannot hold the lock
   while rendering has to copy the name out by hand, which is what
   `PanelScreen` in `firmware/src/provision.rs` is. Here the lock is a critical
   section and a QR encode inside one would mask core 1's HUB75 DMA interrupt,
   so this was not optional. An owned form in the crate would delete that type.
3. **`FailReason` has `as_str()` (the API's words) but no sentence for a
   person.** "wrong password", "network not found", "could not join" are in
   the firmware's HTML, where the simulator cannot test them.
4. **`Timing` has no DHCP phase.** `join_attempt_ms` models a *radio* attempt;
   on this bench association takes ~3 s and DHCP another 10-12, so the firmware
   had to put a second window on top. The machine's "attempt" is in practice
   association-only, and the doc comment does not say so.
5. **`device-api`'s `route::ROUTES` has no portal path**, so `/setup` is
   handled in front of the table. Fine while only the firmware serves it; the
   moment the simulator should serve the same page, the path wants to be in the
   shared table with its own `max_request_len`.

### Proposed follow-up cards

* **The boot path's two 3 KB partition-table buffers** (the orchestrator asked
  for this at the top). `read_fw_health` and `store::init` each put a
  `PARTITION_TABLE_MAX_LEN` buffer on `main`'s stack; sharing one, or moving it
  to the heap for the duration, is the next lever and it is worth ~3 KB of the
  transient the region has to be big enough for.
* **`http-selftest` costs 5,328 bytes of `.bss` and now fails the floor.** A
  second monomorphised copy of picoserve's `serve`. Either it shares the
  workers' buffers and router, or `fw-size.sh` learns which builds the floor is
  a rule about.
* **The AP client count is blind during a join attempt.** `esp-radio` offers AP
  associate/leave only through a `&self` future, and a join needs `&mut self`.
  Restructuring the driver around `controller.subscribe()`, or asking upstream
  for a `&self` join, would close the one approximation in this card.
* **Move the two duplicated derivations into `crates/provision`** - items 1 and
  2 above - and delete the firmware's and the simulator's copies.
* **The simulator should serve the setup page.** The portal HTML has no test at
  all today; the sim already drives the same machine and could serve the same
  bytes, so the page could be opened in a browser and the wording reviewed
  without a device.
* **Re-announce mDNS when the station's address changes**, not only on
  `Action::Announce`. After the soft-AP goes down the radio restarts and the
  station re-associates; if DHCP hands out a different address, nothing tells
  mDNS.
* **A sender-visible reason for `PROVISIONING`.** The telemetry overlay says
  the device is in setup but not why; a sender that has lost its panel to the
  portal has to guess between "it never joined" and "somebody posted
  credentials".

### Summary

* Exit `.stack` **27,928** on the default build (floor 24,576 untouched, card's
  exit 26 KB). Levers: the frame socket's tx buffer 2,944 -> 512 (+2,432, its
  own commit), `MDNS_BUF` 1500 -> 1024 (+1,904), `AP_SOCKETS` 5 -> 4 (+408).
* Core 0 high-water **13,056** - unmoved from cards 227 and 233, and unmoved by
  all 31 routes of the self-test. `stack_free` 13,848 idle, 12,488 under load,
  8,520 with the soft-AP and its three services up.
* Heap 45,540 of 90,112 station-only, 47,932 with the AP up: **the whole AP
  side is ~2.5 KB of heap.**
* Self-tests on the device: 20/20 LAN routes, 11/11 portal routes, run twice
  (AP down and AP up).
* `screeny-probe http` 30/0/8, identical to fw 0.4.3. 1,136-request loop, no
  failures once up. 30 fps rx / 30 fps shown / zero drops throughout.
* Four flashes: the first found a DHCP window that was under the measurement,
  the second was green, the third was the portal, the fourth put the default
  build back.
* **The store's credentials were never read, erased or overwritten**, and no
  SSID, BSSID or credential appears anywhere in this branch.

### Orchestrator, after the merge (2026-09-20)

`main` builds to the worker's numbers (`.stack` 27,928, floor 24,576, image 968,409).
fw 0.5.0 is on the device.

**Verified over the wire**
- `screeny-probe http --allow-wifi-trial --allow-reboot` with `CARD_223_LANDED` now
  **true** (rules 8, 14, 29, 33 enforced): 32 passed, 1 failed, 5 skipped - and the one
  failure (rule 33) was the *probe's*: it polled inside the ~100 ms between the `trying`
  reply and the start of the radio work, read the old `connected` and gave its verdict at
  "0 s". Fixed in the probe (a 5 s "not started yet" grace); re-run of the section: 2/2.
  Rule 35 (confirmed reboot, device returns with a new `boot_id`) passed.
- By hand, a wrong-credentials POST while online: `trying`, then within 7 s
  `GET /api/v1/wifi` = `failed` / `reason: not_found` and sticky, while
  `status.wifi_state` stayed `connected` - **and the device never left the network**: it
  answered every poll through the whole trial. Then the bench rule: reboot -> rejoined
  from the store, `wifi_state connected`, the sticky result gone.
- A captive probe on the LAN listener is a 404. 1,043 of 1,136 requests answered during
  the flash-and-boot window, none failing once up; connects ~10 ms; `stack_free`
  12.5-13.8 KB.
- The Studio on workbench streams to it at 30-31 fps with zero drops and reads its
  status API every 10 s.

**NOT yet verified: the 64-rule UDP conformance suite on 0.5.0.** Five attempts gave
11-37 passes with failures of the form `no reply to op ...` and
`Can't assign requested address (os error 49)`, starting at a different rule each time.
That pattern is this Mac's network, not the firmware: a baseline `ping` from this Mac to
the router and to workbench **with nothing else running** showed 24 timeouts in 80 s and
round trips of up to 51 s, the same sections passed 11/11 on 0.5.0 in a quiet window, an
A/B against 0.4.3 passed 11/11 twice in another quiet window, and through all of it the
Studio (on workbench, a different host) streamed to the device without a dropped frame.
Serial flashing was flaky in the same period (`Bad data checksum` on about half the
attempts at 230400 baud). Both point at this Mac's Thunderbolt/USB path. **Re-run
`screeny-probe --addr 192.168.7.221 conformance --slow` when `ping 192.168.7.1` from this
Mac is clean, and record the result here.**

**Still to do with the owner: the phone test** (steps and the build command are in this
card's Log under "The phone test, for after the merge").

Follow-ups accepted from the worker's list: share the boot path's two 3 KB
partition-table buffers (the only thing setting the 13 KB stack demand); `http-selftest`
is 5.3 KB of `.bss` and its bench build now fails the `fw-size.sh` floor (share the
workers' buffers); `link_state()` and an owned screen description belong in
`crates/provision` (the firmware and the simulator each carry a copy today); the
simulator should serve the setup page so its HTML has a test; re-announce mDNS when the
station's address changes; `Timing` should model the DHCP phase (this mesh takes 10-12 s).

**Orchestrator, 2026-09-20 later: the UDP suite is green on 0.5.0.** The owner turned the
Mac's WiFi (en0) off, leaving wired en5 as its only interface on the subnet. Baseline
first: `ping` to the router, 40 of 40 answered, max 1.3 ms (earlier the same day: 24
timeouts in 80 s). Panel released from the Studio, `screeny stats` said HOLD then IDLE,
then `screeny-probe --addr 192.168.7.221 conformance --slow`, first attempt:
**60 passed, 0 failed, 4 skipped** (the usual four: three loopback-only oversize rules
and the `--cap-probe` brightness rule). Panel given back; the Studio was streaming at
30 fps with zero drops within seconds, device uptime 5 h 35 min across all of it. So the
earlier failures were this Mac's two-interfaces-on-one-subnet path, not the firmware.
Lesson for the bench: with en0 and en5 both up on 192.168.7.0/24 this Mac's path stalls
for seconds at a time; turn WiFi off before conformance or flashing sessions.
**Still open: the phone test.**
