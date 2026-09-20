---
id: 190
title: Should the Studio's device controls go over HTTP rather than UDP?
type: design
hardware: no
depends: [180, 222]
owner:
branch:
---

## Goal

Decide - and then either do it or write down why not - whether the Studio's five device
controls (`brightness`, `identify`, `name`, `reboot`, `stats`) should be `POST`s to the
panel's HTTP API instead of UDP control datagrams.

## Context

- Card 180 made the Studio read `GET /api/v1/status` over HTTP and left the control path
  exactly where it was, on purpose: the card said so, and changing both at once would
  have made a regression in either impossible to attribute.
- The Studio now has both. `crates/studio/src/api.rs`'s `on_device` helper is the single
  place every control request goes through (`screeny::ControlClient`, 400 ms, three
  retries inside that), and `crates/studio/src/devhttp.rs` is the HTTP client.
  `screeny-device-api` already defines every request and reply shape for
  `POST /api/v1/{settings,identify,reboot}`.
- What HTTP would buy: a **reply that says what happened**, in one round trip, with an
  error shape (`ErrorCode`) instead of a timeout. `POST /api/v1/settings` answers the
  whole settings state, including the brightness the firmware capped it to - which the
  Studio currently learns from `SET_BRIGHTNESS`'s reply and has to reconcile by
  comparing against telemetry (`fleet::supervise`'s brightness policy).
- What it would cost, and why this is a design card rather than a build one:
  - **The panel has one connection worker and no listen backlog.** A control request
    would have to queue behind the ten-second status poll, or be allowed to pre-empt it.
    Today they cannot collide at all, because one is UDP and one is TCP. That is a real
    property being given up.
  - A panel on firmware 0.2.0 has no HTTP API. The UDP path would have to stay anyway,
    so this is a second implementation unless the old one is deleted - and deleting it
    means the Studio stops working with any panel that has not been updated.
  - `identify` and `reboot` both answer *before* doing the thing, which is right, but
    means a caller learns nothing more than UDP already tells it.
- `POST /api/v1/wifi` is a separate question and is **not** in this card: credentials go
  through the captive portal (`docs/design/device-web.md`), and the Studio has no
  business carrying a PSK.

## Deliverables

- A short note in `docs/research/` weighing the two, with the one-connection-worker cost
  stated plainly, and a recommendation.
- If the answer is yes: the change, behind the existing `on_device` seam, with the UDP
  path kept as the fallback for firmware that serves no HTTP - and the collision with
  the status poller resolved explicitly rather than by luck.
- If the answer is no: a paragraph in `crates/studio/README.md` saying why, so this is
  not reopened every six months.

## Acceptance

Either a decision written down with its reasoning, or a change that keeps every control
working against `screeny-sim` with the HTTP API on **and** off, and that cannot open two
connections to a panel at once (`crates/studio/tests/device_status.rs` has the test that
measures this).

## Log

### Parked 2026-09-20 (owner: "let's prune what doesn't need to be done"; focus is aesthetic work)

Decided: no, for now. UDP control and the HTTP status poll cannot collide today because one is UDP and one is TCP, and the panel has one connection worker and no listen backlog; moving controls to HTTP gives that up to gain an error shape for five controls that already work, and the UDP path would have to stay anyway for firmware with no HTTP API. Revisit when OTA (cards 240-243) gives the Studio a reason to make HTTP requests that change the device.
