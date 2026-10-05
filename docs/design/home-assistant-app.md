# Screeny as a Home Assistant app

The goal: someone with Home Assistant and a Gen 1 Tidbyt installs the **Screeny** app,
flashes the Tidbyt from their browser, types their WiFi password once, and from then on the
panel is a device in Home Assistant - its picture, patch, channel and brightness are HA
entities, and firmware updates arrive as an HA update. Nobody opens a terminal.

This file is the plan and the decisions. [`deployment.md`](deployment.md) stays the guide
for running the Studio as a plain Docker service; this is the other way to run it.

## The path a new user takes

| step | what happens | what makes it work |
|---|---|---|
| 1 | Add the screeny repository to HA's app store, install **Screeny** | `repository.yaml` at the repo root and the app in `ha-app/` (card 359); multi-arch images on ghcr (card 360) |
| 2 | Open **Screeny** in the HA sidebar | HA ingress in front of the Studio (card 359) |
| 3 | Plug the Tidbyt into the computer, open the flasher, click Install | a Web Serial flasher on HTTPS (card 363), one firmware image for every unit (card 361) |
| 4 | Pick the WiFi network and type the password in the same dialog | Improv Serial in the firmware (card 362) |
| 5 | The panel appears in the Studio and in HA | already true: mDNS discovery, a new panel joins Channel 1, the MQTT bridge publishes it as a device (cards 141, 353, 355) |
| 6 | Later, HA shows "update available" for the panel; Install | the Studio pushes OTA, published as an MQTT `update` entity, firmware bundled in the app image (card 364) |

## Decisions

| # | Decision | Why |
|---|---|---|
| 1 | **The app runs the existing Studio image** (Debian trixie + Mesa), not HA's Alpine base. | Mesa's lavapipe under musl segfaulted in the 2026-10-02 spike; the Debian build ran. Apps may use any base. |
| 2 | **Host networking.** | mDNS discovery and the UDP frame stream behave exactly as in `docker-compose.yml`. |
| 3 | **The Studio is reached through ingress only, by default.** It does not listen on the LAN unless an app option says so. | Ingress puts HA's login in front of a Studio that has none of its own (card 041). |
| 4 | **MQTT is configured by the Supervisor** when the user has not configured it on the Settings screen. | The Supervisor hands an app the broker's address and credentials (`services: mqtt:want`). A user's explicit settings win. |
| 5 | **No GPU is required.** On a machine whose only adapter is a software rasteriser, overland and ghosts are unavailable (card 357); everything else plays. | Measured on a Raspberry Pi 4 on HA OS, which exposes no GPU to apps. |
| 6 | **Flashing happens in the browser over Web Serial, on an HTTPS page** (GitHub Pages), using ESP Web Tools. Not inside the app. | Web Serial needs a secure context; HA on plain `http://homeassistant.local:8123` is not one. The Tidbyt's USB bridge is a CP2102N, which is serial, not WebUSB. |
| 7 | **No stock-firmware backup step** in the flasher. | The owner, 2026-10-02: the official Tidbyt firmware is easy to download and flash back. |
| 8 | **The flasher is for the first install; updates are OTA.** | The device has `POST /api/v1/firmware` since fw 0.6.0. |
| 9 | **The firmware that an app version offers is inside the app image.** | An app update is what offers a firmware update; the Studio never fetches from the internet. |
| 10 | **WiFi credentials over Improv Serial** follow the portal's rule: the password is written to the settings partition and never appears in a reply, a log line or on the panel. | Same posture as spec 8.4 and `crates/provision`. |

## Measured on the owner's HA (Raspberry Pi 4, 4 GB, HA OS 18.2), 2026-10-02

`/sys/class/drm` is empty: HA OS starts the Pi 4 without the v3d driver, so no app can have
a GPU there and wgpu comes up on lavapipe. Each patch paced at 30 fps for 20 s, with HA
running alongside; CPU is a percentage of one core (the Pi has four):

| patch | frames of 600 | CPU |
|---|---|---|
| clocks-numerals, clocks-dials, vesta, metaballs, flock, bats | 600 | 26-57% |
| knot, lattice | 584, 575 | 122%, 142% |
| leaves | 523 | 88% |
| overland | 320 | 322% |
| ghosts | 8 | 100% |

The Supervisor gives every app `TZ` and `SUPERVISOR_TOKEN`; Mosquitto is the usual broker.

## Cards

359 the app: `ha-app/`, ingress-safe URLs, options, Supervisor MQTT, ingress-only listen -
360 multi-arch images on ghcr from GitHub Actions - 361 colour order as a runtime setting
(one image) - 362 Improv Serial - 363 the web flasher - 364 firmware updates from the Studio
and HA - 365 the user guide and the end-to-end run on a real HA.

## The web flasher (card 363)

`site/` (page) + `tools/pages-build.sh` + `.github/workflows/pages.yml`, published at
`https://gotwalt.github.io/screeny/`. The build downloads the newest `fw-v*` release's
`screeny-fw-<v>-full.bin`, checks it against `SHA256SUMS`, writes `manifest.json` and vendors
ESP Web Tools 10.4.0 (checked against npm's sha512). Nothing binary is committed.

- **Baud: 115200, fixed.** ESP Web Tools constructs `ESPLoader({baudrate: 115200})`
  (`src/flash.ts`); esptool-js's ROM baud is also 115200, so it never changes baud, and
  the manifest has no key to change it. This is below the bench's 230400 limit.
- **Erase: default.** No `new_install_prompt_erase`. ESP Web Tools erases the whole chip when
  the device is not already running the manifest's firmware (stock Tidbyt, or no Improv) and
  does not erase when it is (`name` = `screeny-fw`, which the device reports over Improv), so
  updates keep WiFi. The stock layout has `app1` over our settings partition (0x410000);
  a first install clears that rather than relying on the store tolerating leftovers.
- **Needs one image**: the page installs `screeny-fw-<v>-full.bin`; colour order is a runtime
  setting (card 361), so the `-hdk-colours` variants are not published.

## Status, 2026-10-02 (paused)

| card | state |
|---|---|
| 357 software adapter hides overland/ghosts | merged |
| 359 the app (`ha-app/`, ingress-only, Supervisor MQTT) | merged; **not yet run on a real HA** - needs 360's images. Open: ingress port 8099 + the image HEALTHCHECK (replaces `watchdog`, card 366 follow-up) on a real Supervisor, websocket through HA's proxy, peer `172.30.32.2` |
| 360 multi-arch images on ghcr | workflow written, not yet run on GitHub (the ghcr package must be made public once); build `--target app`; image `ghcr.io/gotwalt/screeny-ha-app`, tag = `ha-app/config.yaml` `version`; the ghcr package must be made public |
| 361 colour order as a runtime setting | merged at the pause **without a full workspace test run** - run `cargo test --no-fail-fast` first. Setting `colour_order` = `rotated` (default) / `published`, device settings page, reboot to apply; `panel-hdk-colours` feature gone, one image. Not flashed: check both bench units with `screeny pattern` and `stack_free` |
| 362 Improv Serial | **verified on a Tidbyt, 2026-10-04, fw 0.11.1**: the web flasher offered the WiFi step before and after the install and the panel joined the owner's network. 0.11.0 shipped deaf: esp-hal ties UART0's RX input high when the driver is created, so GPIO3 has to be routed back (one GPIO-matrix write in `firmware/src/improv.rs`; `with_rx` cost ~2.3 KB of `.stack`). Not yet tried: a wrong password falling back |
| 363 web flasher (`site/`, `tools/pages-build.sh`) | merged; needs GitHub Pages enabled (Source: GitHub Actions) and a firmware release containing 361 + 362 (0.11.0) for the WiFi step to appear. ESP Web Tools flashes at 115200 (fine for the CP2102N) and erases on a first install |
| 364 firmware updates from HA | merged. **First real OTA, 2026-10-04**: the published `screeny-studio:edge` image on workbench (firmware file mounted, `SCREENY_FIRMWARE_FILE`) updated the Office panel 0.10.0 -> 0.11.0 from `POST /api/v1/device/firmware`: 1,043,152 bytes uploaded in ~23 s, restarted in ~9 s, trial confirmed ~55 s later (`fw_slot` ota_1, `fw_state` valid, `/api/v1/panic` `update.outcome: confirmed`). Then, from the HA app on the owner's Home Assistant, Install on the Living Room panel's `update` entity took it to 0.11.0 too (2026-10-04). The image carries `screeny-fw-<FIRMWARE_VERSION>.bin` (Dockerfile build arg, 0.11.1), reads its version with `screeny-fwimage`, and offers it only to panels on an older version; the HA entity and the Panel screen's *Update firmware* button call the same `firmware::start`. Details: `crates/studio/README.md`, *Firmware updates* |
| 368 HA devices after a move between Studios | on branch `card/368-ha-device-roles`. Found on the owner's HA (2026-10-04): (1) **the firmware `update` entity was Unknown on every panel** because the state JSON had `null` for `release_summary` (and unknown versions), and HA's `MQTT_JSON_UPDATE_SCHEMA` takes strings there, so HA dropped the whole message; now those keys are omitted / `""`. (2) device `sw_version` was the Studio's version ("Firmware 0.1.0" on every device); it is now the panel's installed firmware (omitted until known, and on channels), the Studio's version stays in `origin`. (3) the first panel's device is named after its panel, not the Studio; ids unchanged. (4) a stale retained config for the first panel's old later-panel id (`screeny_<id>_<key>`) is cleared at connect and whenever the first panel changes. (5) an import carries the file's panel order (`Panels::replace`), so its first panel stays first |
| 365 guide + end-to-end on a real HA | not started; decide first how the app takes the panels over from an existing Studio (both would stream to the same panels and publish to the same broker) |
