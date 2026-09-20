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

### What was built

`crates/device-api` (`screeny-device-api`), `no_std`, no alloc, no float, no clock, no
I/O. `serde` + `heapless 0.9.3` only; `serde-json-core` and `serde_json` are
dev-dependencies, because the crate itself never serialises anything - picoserve's
writer and the Studio's `serde_json` do that.

| module | what |
|---|---|
| `route` | `API_VERSION`, `PREFIX`, the nine path constants, and `ROUTES`: the table as data, with each route's method, body kind and size bounds. `MAX_REQUEST_LEN` is the one number that sizes picoserve's HTTP buffer. |
| `reply` | `StatusReply` `TelemetryReply` `NetworksReply`/`Network` `WifiReply` `SettingsReply` `FirmwareReply` `AcceptedReply` |
| `request` | `SettingsRequest` `RebootRequest` `IdentifyRequest`, `Auth`, `check_auth`, `Mutating`, `MIN_UNESCAPE_BUFFER`, `REBOOT_CONFIRM` |
| `error` | `ErrorCode` (15, closed) with `status()` and `From<proto::ErrorCode>`; `ErrorReply` |
| `enums` | `IdleMode` `WifiState` `StreamState` `FailReason` `FwSlot` `FwState` `ResetReason` `FirmwareError` `Accepted`, with conversions to and from the `screeny-proto` bytes and `screeny-provision`'s strings |
| `form` | `parse_wifi_form`, `WifiForm`, `FormError`, `MAX_FORM_LEN` |
| `text` | the bounded string aliases, `text`, `ssid_text`, `ipv4_text` |

64 tests plus the crate-doc example. Clippy (`--all-targets`, with
`#![warn(clippy::pedantic)]` on) clean; `cargo doc` clean;
`cargo check --target thumbv7em-none-eabi` green with default features; root
`cargo test` green first time, the pacing tests included.

### The route table, with the size bounds the tests prove

| method | path | request | req bound | reply | reply bound |
|---|---|---|---|---|---|
| GET | `/api/v1/status` | - | - | `StatusReply` | 1018 |
| GET | `/api/v1/telemetry` | - | - | `TelemetryReply` | 426 |
| GET | `/api/v1/networks` | - | - | `NetworksReply` | 3710 |
| GET | `/api/v1/wifi` | - | - | `WifiReply` | 345 |
| POST | `/api/v1/wifi` | urlencoded `WifiForm` | 384 (`MAX_FORM_LEN`) | `AcceptedReply` | 24 |
| POST | `/api/v1/settings` | `SettingsRequest` | 373 | `SettingsReply` | 247 |
| POST | `/api/v1/firmware` | raw octet-stream, streamed | - | `FirmwareReply` | 57 |
| POST | `/api/v1/reboot` | `RebootRequest` | 188 | `AcceptedReply` | 24 |
| POST | `/api/v1/identify` | `IdentifyRequest` | 152 | `AcceptedReply` | 24 |
| any | any, on failure | - | - | `ErrorReply` | 330 |

`route::MAX_REQUEST_LEN` = 384. Every bound is asserted for **equality** against the
longest value the type can hold (`tests/sizes.rs`), not `<=`: an over-estimate rots
quietly and nobody notices.

### Where research 007 section 7 or the card was ambiguous, and what I decided

1. **A reply's `MAX_JSON_LEN` is not picoserve's buffer size.** The card asks for the
   constants "so card 222 can size picoserve's buffers from constants instead of
   guesses". picoserve does not buffer replies at all: `Content::content_length`
   serialises into a counting writer and `write_content` streams. The number that does
   size a buffer is on the *request* side, because the `Json` and `Form` extractors
   call `read_all()`. So every request type got a bound too, and `MAX_REQUEST_LEN` is
   named for that job.
2. **A non-UTF-8 SSID has no JSON representation.** 007 only says `ssid` is the SSID.
   Decided: `ssid` is `Option<String<32>>`, `null` both for "nothing stored" and for
   "not UTF-8"; a scan result that cannot be named is left out of `networks` rather
   than shown wrongly. Lossy conversion would change its length and mislead whoever
   compares it with what they typed. `crates/provision`'s `Trial.ssid` is already a
   `String<SSID_MAX>`, so the display path in this repo already assumes UTF-8.
3. **Telemetry keeps the raw bytes.** 007 wants a browser and a UDP sender to see "the
   same numbers", so `TelemetryReply.state` and `.last_codec` are `u8`, while
   `StatusReply.state` spells the same byte as a word. Asymmetric on purpose, and said
   so in both places.
4. **`slot` became `fw_slot`, and gained an `unknown` variant.** The card names only
   `"ota_0"` and `"ota_1"`. A build flashed before card 210's partition table, or one
   that cannot read its running partition, has to say something; `"unknown"` beats a
   lie.
5. **Golden files are pretty-printed.** The card wants files the type "serialises to
   **exactly**" and also "small and readable". A twenty-field compact line is not
   readable, so each file is exactly `serde_json::to_string_pretty` of the value, and
   the compact wire form is checked in the same test and asserted byte-identical from
   `serde_json` and `serde-json-core`. Both properties, no compromise.
6. **Duplicate form keys are refused, not resolved.** The card lists "duplicate keys"
   as a test case without saying the outcome. Every other form parser takes the last
   value; this one returns `DuplicateKey`, because `psk=right&psk=wrong` must not be a
   coin toss about what reaches flash.
7. **A missing `psk` key means an open network**, the same as `psk=`. A browser form
   always sends the key; `curl -d 'ssid=Foo'` does not.
8. **`error?` and `detail?` are omitted when absent; `ssid`, `ip` and `reason` are an
   explicit `null`.** That is what the sources say: the card writes `error?` and
   `detail?` with a question mark, and 007 section 7 writes `reason` as
   "`null` / `auth` / `not_found`".
9. **`ErrorCode` is fifteen generic codes, not one per failure.** The card asks for
   "one shape ... with an HTTP status the crate names as a constant next to each code".
   A code per failure would make every new message an API change; the machine reads the
   code, the person reads `detail`. `From<proto::ErrorCode>` keeps the UDP and HTTP
   names in step.
10. **`IDENTIFY` is a `u16` on the wire** (spec 6.6) but `duration_ms` is a `u32` in
    JSON, with `duration_u16()` returning `OutOfRange` above 65535. A `u16` field would
    have turned 65536 into a parse error with no useful message.

### Two things the dependencies do that nobody would guess

Both are now tests (`tests/writers.rs`) rather than memory.

1. **`serde_json_core::from_slice` does not unescape strings, and reports no error.**
   `Deserializer::new(slice, None)` skips unescaping (`src/de/mod.rs:500`), so a name
   posted with a `\u00e9` in it is stored holding those six characters. The only correct
   call is `from_slice_escaped(body, &mut buf)`. That buffer holds one *unescaped*
   string at a time, so it must be at least the longest string a request can carry:
   `MIN_UNESCAPE_BUFFER` = `MAX_NAME_LEN` = 32. picoserve's default `Json` extractor is
   `JsonWithUnescapeBufferSize<T, 32>` - exactly enough, with no margin at all. A test
   asserts that 32 works and 31 fails.
2. **The three writers do not produce identical bytes.** picoserve escapes `/` as
   `\/` (`src/response/json.rs:80`); `serde-json-core` writes `\u00XX` in **upper**
   case (`src/ser/mod.rs:232`, `hex_4bit`); `serde_json` does neither. All three parse
   all of it, so no consumer is affected - but the golden files are byte comparisons,
   so they contain neither character and the harness enforces that.

### Other notes

- `screeny-proto` and `screeny-provision` are dependencies and neither was touched. The
  provision dependency buys one thing - `From<provision::FailReason>`, with a test that
  the JSON strings really are `FailReason::as_str`'s - and costs the Studio
  `embedded-graphics` and `qrcodegen-no-heap` on a host build, which is free.
- `Cargo.lock` gains `serde-json-core 0.6.0` (dev-only) and `serde_core` under
  `heapless 0.9.3`, because this crate turns on heapless's `serde` feature. Feature
  unification means `crates/settings` and `crates/provision` now build heapless with
  that feature too; it adds a module of trait impls and nothing else.
- Nothing in the crate can hold a PSK except `form::WifiForm`, which is a *request*.
  Its `Debug` prints the SSID and `psk_len`, and there is no `Display`, no `Deref` and
  no `as_str`; `WifiForm::psk` is the one greppable way to the bytes - the same shape
  `crates/settings`'s `Psk` takes, for the same reason.
- Examples use `Example-Wifi1` / `password9` throughout: in the code, the tests and the
  golden files.

### Deliverable 5: what `docs/design/device-web.md` should say

Not written here - the card says that file is the orchestrator's. The proposed text is
in the report. In short: a "The HTTP API" subsection under "What the research settled"
that names `crates/device-api` as the single definition, carries the route table above,
and records the four things a future reader will otherwise re-litigate - reply bounds
are not buffer sizes, a non-UTF-8 SSID is `null`, duplicate form keys are refused, and
`MAX_NAME_LEN` is coupled to picoserve's unescape buffer.

### Proposed cards (not done)

- **227 - one rate limiter, host-tested.** 007 section 7 rate-limits
  `GET /api/v1/networks` to one scan per 10 s. Cards 222 and 224 will each invent that,
  and the two will disagree about whether a refused request resets the timer - card 221
  had exactly this question about the portal's 10-minute retry and answered "no". A
  thirty-line `no_std` `RateLimit { last_ms, period_ms }` taking `now_ms`, wrapping-safe,
  with the answer written down, belongs beside the route it guards.
- **228 - `screeny-probe http`: a conformance run over the HTTP API.** The UDP side has
  `screeny-probe conformance` (60 pass, 0 fail, 4 skip) and it is how every flash is
  regression-checked. The HTTP side will have nine routes, an error shape and a set of
  size bounds, and no equivalent. A subcommand that walks `route::ROUTES` against a
  device or the simulator, parses every reply with these types and checks the bounds
  would make cards 222, 223 and 224 verifiable in one command.
- **229 - the Studio's device view reads `screeny-device-api`** instead of its own
  shapes. That is the software session's crate, so it needs their agreement first; the
  golden files are the handover document.

### Hazards worth recording where the next person looks

- If `MAX_NAME_LEN` ever rises above 32, picoserve's **default** `Json` extractor
  quietly stops being big enough for an escaped name. `MIN_UNESCAPE_BUFFER` exists so
  card 222 can spell the dependency; nothing enforces it from here.
- `GET /api/v1/networks` at its worst case is 3.7 KB of JSON. picoserve streams it, so
  it costs no buffer, but it is several TCP segments while the panel is running - the
  bench card that proves "HTTP costs no frame" should use that route, not `/status`.
