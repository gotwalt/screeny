---
id: 226
title: crates/device-api - the device's HTTP JSON shapes, one definition for firmware, sim and Studio
type: build
hardware: no
depends: [201, 221]
owner: worker-226
branch: card/226-device-api-crate
---

## Goal

The device is getting an HTTP API (card 222 serves it from the firmware, card 224 from
the simulator, and the Studio will read it for device health). "One implementation of
each thing": the request and reply shapes live in one `no_std` crate that all three
depend on, with golden JSON tests, so the firmware cannot drift from what the Studio
parses. This card builds that crate and nothing else.

## Context

Read first: `CLAUDE.md`; `docs/design/device-web.md` (decisions 2, 3, 7 and the 201 /
221 summaries); `docs/research/007-device-web-and-portal.md` **section 7** (the route
table - it is the specification for this card) and section 4.4 (why the provisioning
path is a form post + full-page reload, not `fetch()`); `firmware/src/web_spike/http.rs`
(card 201's compile-only picoserve spike: how `picoserve 0.20.0` serialises replies and
extracts bodies at our pinned version - evidence, not code to keep);
`crates/proto/src/control.rs` (`Telemetry`, `wifi_state`, `state`, `IdleMode`, the
`GET_INFO` keys) and spec sections 6.3, 6.7; `crates/provision/src/machine.rs`
(`Trial`, `TrialOutcome`, `FailReason::as_str` already emits `auth` / `not_found` /
`other`); `crates/settings/src/value.rs` (the limits: SSID 1..=32 bytes, PSK 0..=64,
name <= 32 UTF-8).

Decided:

- **A new crate, not a feature of `crates/proto`.** Research 007 suggested putting the
  shapes in `crates/proto` behind a `serde` feature; `crates/proto` is shared surface
  with the software session and is compiled into everything, so the shapes get their own
  crate instead: `crates/device-api`, package `screeny-device-api`. It may depend on
  `screeny-proto` (for `Telemetry` and the constants) and `screeny-provision` (for the
  trial outcome); neither of those is changed.
- `#![no_std]`, **no alloc** in the default configuration: the firmware serialises with
  picoserve's JSON writer (any `serde::Serialize`) and parses request bodies with
  `serde-json-core` (no alloc), so every field is a number, a bool, an enum or a
  `heapless::String<N>` / `heapless::Vec<_, N>` with serde support. The same types must
  also round-trip through `serde_json` on the host (that is how the Studio will read
  them) - test both directions. An optional `std` feature may add conveniences
  (`Display`, `std::error::Error`), nothing structural.
- Routes and shapes per 007 section 7, with these adjustments:
  `GET /api/v1/status` also carries `fw_slot` (`"ota_0"`/`"ota_1"`), `fw_state`
  (`"valid"`, `"pending_verify"`, ... as strings), `reset_reason`, `stack_free`,
  `store_errors`, and `api: 1`; **there is no PSK field in any reply, ever** - make that
  a test that serialises every reply type and greps the output for `psk` / `pass`.
  `POST /api/v1/wifi` is `application/x-www-form-urlencoded` (`ssid=&psk=`), because the
  iOS captive mini-browser path is a plain form post: provide a no-alloc urlencoded
  parser for exactly that form (percent-decoding, `+` as space, SSID as bytes not
  necessarily UTF-8, the `crates/settings` limits, clear errors), and do not print the
  PSK in any `Debug`. `POST /api/v1/settings` is JSON `{name?, brightness?, idle_mode?}`.
  `POST /api/v1/firmware` replies `{ok, written, error?}` with a closed set of error
  strings (`bad_magic`, `wrong_chip`, `wrong_project`, `bad_checksum`, `bad_sha256`,
  `too_large`, `busy`, `flash`) matching research 006 section 5's five checks.
  `POST /api/v1/reboot` takes `{confirm:"RBOO"}`. Every mutating request type has an
  optional `pin` / `counter` pair that is parsed and ignored today (decision 3).
  Errors are one shape: `{error: "<code>", detail?: "<short text>"}` with an HTTP status
  the crate names as a constant next to each code.
- Size limits are part of the API: give every reply type a `const MAX_JSON_LEN` (proved
  by a test that serialises a worst-case value) so card 222 can size picoserve's buffers
  from constants instead of guesses. The networks list is capped (suggest 16 entries,
  strongest first; say why).
- A `From<&screeny_proto::control::Telemetry>` (or equivalent) for the telemetry reply so
  "a browser and a UDP sender see the same numbers" is code, not a promise.

## Deliverables

1. `crates/device-api/` as above, in the root workspace, with a route table in code
   (`pub const STATUS: &str = "/api/v1/status"` etc. and an `API_VERSION`).
2. Golden tests: for every request and reply, a checked-in JSON example under
   `crates/device-api/tests/golden/` that the type serialises to **exactly** and parses
   back from, with both `serde-json-core` and `serde_json`. These files are the API's
   documentation for the Studio side; keep them small and readable.
3. The urlencoded WiFi form parser with tests (empty PSK = open network, 32-byte SSID,
   33-byte SSID refused, percent-encoded UTF-8, `+`, a PSK containing `&` and `=`
   encoded, truncated escapes, duplicate keys, unknown keys ignored).
4. `crates/device-api/README.md`: the route table with one line each and a pointer to
   the golden files.
5. A short "HTTP API" section appended to `docs/design/device-web.md`?? **No** - that
   file is the orchestrator's. Put anything the design doc should say in your report.

## Out of scope

Serving anything (cards 222 and 224), `firmware/`, `crates/sim`, `crates/studio`,
`crates/proto`, `crates/receiver`, `crates/settings`, `crates/provision`, the spec. If
one of the crates you depend on is missing something you need (a `Serialize` impl, a
public constant), wrap or mirror it in your crate and say so in the Log; do not edit it.

## Acceptance

`cargo test -p screeny-device-api` green, `cargo clippy -p screeny-device-api
--all-targets` clean, the crate builds for a `no_std` target with default features,
root `cargo test` still green (the pacing tests in `crates/screeny` are timing-sensitive
under load: re-run once before believing a failure there).

## Log
