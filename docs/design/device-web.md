# Device web: status, settings, firmware update, captive portal, the button

**Status: decisions recorded, design pending research (cards 200, 201, 202).** This
file is the source of truth for the device-web track (cards 200-249, coordinated by
the `firmware` Claude session). The sections marked *pending* are filled in from
`docs/research/006`, `007` and `008` when those cards land.

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
| 6 | **Compile-time credentials become optional**: when present they seed an empty store (bench convenience); a build without them boots straight to the portal. That is what a public repo needs. | orchestrator, 2026-09-20 |
| 7 | **The frame path is the product.** No HTTP request, flash write or portal activity may cost a frame at 30 fps, except a firmware update, which is allowed to take the panel over with an "updating" screen. | standing |

## Working agreement with the software session

`firmware/`, the serial port and flashing belong to the firmware session; cards
200-249. `crates/proto`, `crates/receiver` and `docs/design/protocol-v1.md` are
shared: either session tells the other before changing them. After any flash the
regression check is `cargo run --release -p screeny-probe -- --addr 192.168.7.221
conformance --slow` (firmware 0.2.0: 60 pass, 0 fail, 4 skip). Once the Studio runs
on workbench it holds the source lock around the clock; release it with
`POST http://workbench.local:8787/api/v1/set_panel {"on":false}` before bench work
and give it back with `{"on":true,"to":"screeny-4a00a4"}`.

## Design (pending)

- Flash layout, settings store, OTA and rollback - from card 200.
- HTTP server, routes and JSON shapes, soft-AP, DHCP/DNS, the join/portal state
  machine, the portal screen layout, RAM budget - from card 201.
- The button's GPIO, the gestures, the power-cycle fallback - from card 202.
- Build order and cards - written by the orchestrator once the three are in.
