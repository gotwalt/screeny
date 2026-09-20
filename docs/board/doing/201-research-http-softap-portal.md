---
id: 201
title: Research - an HTTP server, a soft-AP and a captive portal on the firmware's stack
type: research
hardware: no
depends: [008]
owner: worker-201
branch: card/201-research-http-softap-portal
---

## Goal

The owner wants (2026-09-20) three things from the firmware:

1. a web server on the device for diagnostics/status, network settings and firmware
   upload;
2. when the device has no network to join (no credentials, or they stopped working),
   it brings up its own WiFi network with a captive portal, and the panel shows the
   network name and instructions (a QR code if 64x32 allows it);
3. a button-driven WiFi reset (card 202).

Find out how to build 1 and 2 on *this* stack, prove the pieces link and fit in RAM,
and recommend a design the build cards can follow.

This is the device-web track: cards 200-249, coordinated by the `firmware` session.
Sibling research cards running in parallel: 200 (partition table, settings store,
OTA - it owns everything about flash) and 202 (the button). Stay out of their
questions; where you need the store, assume a `load_wifi() -> Option<(ssid, psk)>`
/ `save_wifi(..)` API exists.

## Context

Read first: `CLAUDE.md`, `firmware/src/main.rs`, `firmware/src/net.rs`,
`firmware/src/mdns.rs`, `firmware/src/screens.rs`, `firmware/Cargo.toml`,
`docs/research/001-firmware-stack.md`, `docs/design/protocol-v1.md` sections 6.3,
6.7, 7.3 and 8, `docs/board/parked/081-sim-wifi-and-provisioning.md`.

What is already known:

- `no_std` embassy: `esp-hal =1.2.2`, `esp-rtos =0.4.0`, `esp-radio =1.0.0-beta.1`
  (pinned: beta.2 conflicts with `esp-hub75 0.17`, see research 001),
  `embassy-net 0.9.1` with `udp` only today, `StackResources<6>`, `edge-mdns 0.8` +
  `edge-nal-embassy 0.9` already in the tree. Read crate sources in
  `~/.cargo/registry`; do not trust memory of other versions' APIs.
- Core 0 does WiFi, net, decode, control, mDNS; core 1 only renders. The 30 fps UDP
  frame path is the product: **an HTTP request must never cost a frame**. Say how
  you know (task priorities, socket buffer sizes, what blocks).
- Memory trap: `.bss` and core 0's main stack share one region; heap is 64 KB
  reclaimed + 32 KB and telemetry reports ~45 KB in use. TCP sockets, an HTTP
  server's buffers and AP mode all cost RAM. The budget is the central risk here.
- CLAUDE.md: "The plan for provisioning is a captive-portal setup with an HTTP
  settings page; do not build other schemes." The serial console of spec 8.1 is
  superseded and will be struck from the spec; `SET_WIFI` (8.2) stays.
- The PSK invariant of spec 8.4 holds for HTTP too: never returned, never logged,
  never on the panel.
- Workers cannot reach the LAN or the device. Everything here is source reading and
  compile-only proof; the orchestrator does device runs after the build cards.

## Questions to answer

1. **HTTP server.** `picoserve` vs `edge-http` (we already carry the edge-* family)
   vs hand-rolled. For each: `no_std`/no-alloc fit, embassy-net 0.9 compatibility at
   the versions we can use, RAM per connection, streaming request bodies (a ~1 MB
   firmware upload must stream to a sink in chunks - card 200 owns the sink),
   concurrent connections, keep-alive, and how a captive-portal redirect is
   expressed. Recommend one. How many TCP sockets, what buffer sizes, the new
   `StackResources<N>`.
2. **Soft-AP in `esp-radio 1.0.0-beta.1`.** The AP config API in this exact version;
   whether AP+STA (`ApSta`/mixed mode) exists and is sound, or whether the design
   should be exclusive modes (STA *or* AP, switching by re-configuring or by
   reboot). Can the controller scan for networks while in AP mode (the settings
   page wants a network list)? Can `embassy-net` run a second stack on the AP
   interface with a static address (192.168.4.1/24)? What it costs in RAM to have
   both interfaces alive versus switching.
3. **DHCP server and DNS catch-all.** `edge-dhcp` / `edge-captive` (same family as
   `edge-mdns`) or minimal hand-written ones; versions compatible with
   `edge-nal-embassy 0.9`. DHCP option 114 (captive-portal URI, RFC 8910) - worth it?
4. **Captive-portal detection.** What iOS, Android, macOS and Windows probe for
   (`captive.apple.com/hotspot-detect.html`, `connectivitycheck.gstatic.com/generate_204`,
   `msftconnecttest.com`, ...) and what the device must answer so the OS pops the
   sign-in sheet: DNS answers everything with 192.168.4.1, HTTP answers unknown
   hosts with a 302 to `http://192.168.4.1/`. Known traps (HTTPS probes, iOS's
   mini-browser limits: no JS popups, small viewport; Android's "no internet, stay
   connected?" prompt).
5. **State machine.** Propose it: boot -> stored credentials? -> join (N attempts,
   how long) -> connected / fall to portal; portal -> credentials submitted ->
   try them *while telling the user what happened* (the phone is on the AP, which
   may drop when the radio changes mode - how do other firmwares (WLED, Tasmota,
   ESPHome, Tidbyt itself) report success or failure?); a network that disappears
   for an hour at 3 am must **not** leave the device sitting in portal mode forever
   - how does it get back (periodic retry of stored credentials while the AP is
   idle)? Map this onto the `PROVISIONING` telemetry state byte and `GET_WIFI`
   states in the spec.
6. **AP security.** The default the orchestrator intends: an open AP named
   `screeny-<id>` (the home PSK then crosses an open network in clear; spec 8.4
   records that the owner does not treat it as a secret). Give the cost of the
   alternative (WPA2 AP with a per-device passphrase shown on the panel / in the
   QR) so the owner can choose.
7. **The page.** One self-contained HTML page (no external assets, works in the iOS
   captive mini-browser), gzip-embedded or plain, size budget. Sections: status
   (the `GET_INFO` and telemetry numbers, uptime, heap, RSSI, reset reason, firmware
   version, partition/slot), network (scan list, SSID, PSK, name), firmware upload
   with progress, reboot. A JSON API underneath (`GET /api/v1/status`,
   `POST /api/v1/wifi`, `POST /api/v1/firmware`, ...) so the Studio can show device
   health later: propose the routes and shapes. mDNS: advertise `_http._tcp` too?
   Auth: the orchestrator is asking the owner; design so a PIN can be added.
8. **The portal screen.** 64x32 pixels. Feasibility of a WiFi QR
   (`WIFI:T:nopass;S:screeny-4a00a4;;` is 32 bytes: version 2-L is 25x25 modules and
   holds exactly 32 bytes in byte mode; version 3-L is 29x29) at one LED per module
   with a reduced quiet zone, beside the SSID in the 4x6-ish font `screens.rs`
   already has. A `no_std`, no-alloc QR encoder (`qrcodegen-no-heap`?) and its code
   size. Propose the layout(s) as ASCII art or a PNG rendered by a host test; the
   owner will judge a real one on the panel with a phone (the orchestrator is
   testing scan-ability separately by streaming a QR frame).
9. **Host-testability.** Workers and CI have no device. What can live in a `no_std`
   crate under `crates/` and be tested on the host (routing, form parsing, JSON,
   the state machine as a pure function of events, the portal screen renderer), and
   can `crates/sim` serve the same HTTP API so senders and the Studio can be
   developed against it? Keep "one implementation of each thing" in mind.
10. **Proof it links.** A compile-only spike in your worktree: enable `tcp` (and
    whatever else) in embassy-net, add the HTTP server crate and AP config, spawn a
    trivial server task, build release (`. ~/export-esp.sh && cd firmware && cargo
    build --release`), and report image size and `.bss`/`.data` growth
    (`xtensa-esp32-elf-size`) against `main`. Do not flash. Keep the spike on your
    branch behind a cargo feature or under `lab/`; it is evidence, not the
    implementation.

## Deliverables

- `docs/research/007-device-web-and-portal.md`: conclusions first, then evidence with
  file and line references into the crate sources you read. End with a recommended
  design, the RAM budget table, and a list of proposed build cards (titles + one
  paragraph each; do not write the card files).
- The compile-only spike, on your branch.

## Acceptance

The orchestrator can write the build cards from the research doc alone, and every
claim about a crate's behaviour cites the source line that shows it.

## Log

### 2026-09-19 — worker-201

- Claimed the card, branch `card/201-research-http-softap-portal`.
- Read `firmware/src/{main.rs,net.rs,mdns.rs,screens.rs}`, `firmware/Cargo.toml`,
  `docs/README.md`, card 200 (to stay out of its lane), parked card 081.
- Read `esp-radio 1.0.0-beta.1` wifi sources in `~/.cargo/registry`. First findings:
  - `Config::AccessPointStation(StationConfig, AccessPointConfig)` exists
    (`src/wifi/mod.rs:376`), maps to `WIFI_MODE_APSTA` (`mod.rs:3044`), and
    `set_config` applies AP then STA config (`mod.rs:3064`).
  - `Interface::access_point()` / `try_access_point()` exist (`mod.rs:1584`), and the
    STA/AP interfaces are separate singletons guarded by `STA_BIT`/`AP_BIT`
    (`mod.rs:1523`). So two `embassy_net::new()` stacks, one per interface, is
    expressible.
  - `set_config` calls `esp_wifi_stop()` only when the *mode* changes
    (`mod.rs:3049`), so STA->APSTA is a stop/start of the radio, but APSTA->APSTA
    with new credentials is not.
  - Scanning: `scan_async` (`mod.rs:3277`) returns `alloc::vec::Vec` (heap!), and
    the doc says "Scanning is not supported in AccessPoint-only mode"
    (`mod.rs:3261`). That is the single strongest argument for APSTA over exclusive
    AP mode: the settings page wants a network list.
  - esp-radio's own heap figures (`mod.rs:26-27`): Station 47-57 KB, Open Access
    Point 53-63 KB. These are the numbers the RAM budget has to start from.
  - `AccessPointConfig::default()` is SSID `iot-device`, channel 1, open,
    `max_connections: 255`, `dtim_period: 2`, `beacon_timeout: 300`
    (`src/wifi/ap.rs:87`). Soft-AP rejects WEP and an empty password (`ap.rs:64`).
- Crate survey (read the crates.io index directly, then the actual sources, unpacked
  into the scratchpad):
  - `picoserve 0.20.0` depends on `embassy-net ^0.9.1`, `embassy-time ^0.5.1`,
    `heapless 0.9.3`, `embedded-io-async 0.7` - **our exact pins**. MSRV 1.93;
    the `esp` toolchain here is `rustc 1.97.0-nightly (8ea53bcd7 2026-07-08)`, so
    that is fine.
  - `edge-http 0.8.0` / `edge-captive 0.8.0` / `edge-dhcp 0.8.0` all want
    `edge-nal ^0.7`, which is what `edge-nal-embassy 0.9.0` (already in the tree)
    provides, and `domain ^0.12.1`, which `edge-mdns 0.8.0` already pulls in.
  - **`edge-dhcp 0.8.0`'s server needs only a plain UDP socket**, not a raw one
    (`src/io.rs:30-47` says so in as many words). Older versions needed `edge-raw`.
    `edge-nal-embassy` has no raw socket, so this is what makes the DHCP server
    possible at all.
  - `edge-dhcp` already implements DHCP option 114 (RFC 8910):
    `ServerOptions::captive_url` (`src/server.rs:29`), `CAPTIVE_URL: u8 = 114`
    (`src/lib.rs:822`).
  - **`edge-nal-embassy 0.9.0`'s default feature set is `all`**, which includes
    `tcp = ["embassy-net/tcp"]` (its `Cargo.toml`). `firmware/Cargo.toml:66` takes
    it with default features, so smoltcp's TCP is *already compiled into the
    current image*. The flash cost of "adding TCP" is therefore mostly already paid.
- Built the compile-only spike (`firmware/src/web_spike*`, features `spike-ap`,
  `spike-http`, `spike-portal`, `spike-qr`, umbrella `device-web-spike`). It
  builds clean on the `esp` toolchain, release, LTO fat. **Never flashed.**
  Measured with `xtensa-esp32-elf-size -A` and `espflash save-image`:

  | config | .text | .rodata | .data | .bss | .stack (what is left) |
  |---|---|---|---|---|---|
  | baseline (main) | 531205 | 73064 | 31492 | 127040 | **37536** |
  | +AP stack | 533117 | 73264 | 31492 | 131008 | 33568 |
  | +AP +picoserve | 602917 | 82872 | 31892 | 138640 | 25536 |
  | +AP +dhcp/dns | 549389 | 74952 | 31708 | 136424 | 27928 |
  | +QR only | 541693 | 75056 | 31556 | 133240 | 31272 |
  | everything | 629269 | 86408 | 32172 | 150200 | **13688** |

  Flash image: 743,408 -> 855,488 bytes (+112,080, +15%). 20.7% of a 4 MB slot.
- **The surprise, and the headline risk**: `.stack` is not a constant, it is
  whatever is left between `_bss_end` and 0x3ffe0000. The full spike takes core
  0's main stack from 36.7 KB down to **13.4 KB**, and `main` builds two 12 KB
  `FrameBuffer` temporaries on that stack. This build would very likely die on
  the stack guard at boot. The budget, not the API, is the hard part of this
  card - see the doc's RAM table for what has to move to the heap.
- The 6 KB `Frame` in the QR column is spike-only scaffolding (the real portal
  screen draws into the existing triple buffer), so the honest steady-state
  `.bss` cost is ~17 KB, not 23 KB.
- Portal screen: `lab/src/bin/portal-mock.rs` renders the layouts into
  `docs/research/img/201-portal-*.png` with the same encoder the spike links.
  Confirms the owner's bench result exactly: the payload is 32 bytes, version
  2-L, 25x25. With a 3-pixel quiet zone the block is 31x31 and leaves 32
  columns = eight `FONT_4X6` characters. The version-2 budget runs out at a
  17-character SSID; 18 characters and up need version 3 (29x29), which with a
  1-pixel quiet zone is 31 of 32 rows and leaves no room for text.
- Two more facts pinned down for the doc:
  - `embassy-net 0.9.1` always adds a DNS socket to the `SocketSet`
    (`src/lib.rs:350`) and adds a DHCPv4 socket when configured
    (`src/lib.rs:707-710`). The STA stack's `StackResources<6>` is therefore
    already holding DNS + DHCP + frames + control + mDNS = 5 of 6. A TCP
    listener on the LAN side needs the seventh.
  - `esp-rtos 0.4.0` has `InterruptExecutor<SWI>` with a priority
    (`src/embassy/mod.rs:317,392`), and SWI 2 and 3 are free (0 is
    `esp_rtos::start`, 1 is `start_second_core`). That is the escape hatch if
    HTTP ever costs a frame.
  - ESP-IDF, `esp_wifi_set_config` attention: "ESP devices are limited to only
    one channel, so when in the soft-AP+station mode, the soft-AP will adjust
    its channel automatically to be the same as the channel of the station."
    That is *the* trap in the state machine: joining a network on another
    channel moves the AP under the phone that is standing on it.
