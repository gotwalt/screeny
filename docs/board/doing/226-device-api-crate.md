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

### Reading the dependencies before writing a line (source, not memory)

Read from `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/`:

- **`picoserve 0.20.0` depends on `serde-json-core 0.6.0`** (`[dependencies.serde-json-core]
  version = "0.6.0"`, behind its `json` feature), on `heapless 0.9.3` **with the `serde`
  feature already on**, on `heapless 0.8.0` as well (aliased `heapless-0-8`), and on
  `serde 1.0.229` with `default-features = false, features = ["derive"]`. So this crate
  pins `serde-json-core = "0.6"` and `heapless = "0.9"` and the firmware links one copy
  of each.
- `heapless 0.9.3`'s serde impls are written against **`serde_core`**, not `serde`.
  That is fine: `serde 1.0.229/src/lib.rs:252` is `pub use serde_core::{de, ser,
  Deserialize, Deserializer, Serialize, Serializer}` - the traits are literally the same
  items, so `#[derive(Serialize)]` on a struct holding `heapless::String<N>` works.
- `serde-json-core 0.6.0`'s `src/lib.rs` doc comment still says *"Serialization of
  strings doesn't escape stuff"*. **That comment is stale**: `src/ser/mod.rs:99
  push_char` escapes `\`, `"`, `\b`, `\t`, `\n`, `\f`, `\r` and `\u00xx`. Checked the
  code, not the docs.
- `serde-json-core 0.6.0`'s `default = ["heapless"]` feature pulls **heapless 0.8**
  (its optional dep is `heapless = "0.8"`). This crate takes it with
  `default-features = false` so it does not add a second heapless on its own account;
  the firmware gets 0.8 anyway through picoserve, but that is picoserve's business.
- **picoserve's reply serialiser is its own** (`src/response/json.rs`), not
  serde-json-core's, and it needs **no output buffer**: `Content::content_length`
  (line 737) serialises into a counting writer `MeasureFormatSize` and `write_content`
  streams. So a reply's `MAX_JSON_LEN` is *not* what sizes picoserve's buffer.
- **One divergence between the three serialisers**: picoserve escapes `/` as `\/`
  (`src/response/json.rs:80`); `serde_json` and `serde-json-core` do not. Everything
  else agrees (same field order, same `is_first` comma logic so `skip_serializing_if`
  is safe on all three, `None` -> `null`, integers printed identically, no floats
  anywhere in this API). Decision: no golden file contains a `/` inside a string, and a
  test enforces it, so every golden is byte-exact under picoserve too.
- picoserve's `extract::Json` uses `serde_json_core::from_slice_escaped` with a **32-byte**
  unescape buffer by default (`src/extract.rs:478`, `JsonWithUnescapeBufferSize<T, 32>`);
  `JsonWithUnescapeBufferSize<T, N>` is how card 222 raises it.
- picoserve's `extract::Form` deserialises through its own urlencoded deserialiser and
  requires the whole body to be UTF-8 (`FormRejection::BodyIsNotUtf8`). An 802.11 SSID
  is bytes, so card 222 cannot use `Form` for `POST /api/v1/wifi`; that is exactly why
  this card asks for our own parser over the raw body.
- Installed no_std target for the check: `thumbv7em-none-eabi` (`rustup target list
  --installed`). Nothing installed.
