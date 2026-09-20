---
id: 233
title: Firmware - one HTTP dispatch instead of nine nested router futures; every refusal in the API's error shape
type: build
hardware: yes
depends: [227, 228]
owner: worker-233
branch: card/233-one-router-future
---

## Goal

Two things, one piece of code. (1) **Correctness**: the first device run of the HTTP
conformance suite found refusals that bypass the API's error shape (picoserve's own
plain-text 405) and one wrong error code; owning the dispatch fixes both and makes
per-route body limits a table lookup. (2) **RAM**: picoserve's `Router::new().route(..)`
builds a left-nested `Either<..>` type whose `poll` frame is 5,680 bytes - 43% of core
0's measured high-water - and each HTTP worker's pool entry is 7,504 bytes, twice. A
single dispatch should shrink one or both; card 223 (the portal) lands ~1.2 KB under the
`fw-size.sh` floor without another lever, and this is the first candidate. How much it
buys is to be **measured** (see "Card 227's correction" below - an earlier estimate of
7.8 KB was an upper bound from summing frames that do not coexist). No other behaviour
change on the wire.

## Context

Read first: `CLAUDE.md`; `docs/design/device-web.md` ("How to think about storage and
RAM", "The HTTP API", "Where core 0's stack goes", the bench rule about credentials);
`docs/research/010-stack-and-ram-levers.md` (the frame tables, the method - **objdump
prints `entry a1, N` in hex above 255**, the interrupt finding, the budget for 223);
`docs/board/done/227-*.md` and `docs/board/done/222-*.md`; `firmware/src/http.rs`;
`crates/device-api/src/route.rs` (`ROUTES`, `route::find(path, method)`,
`route::path_is_known`, per-route `max_request_len`); picoserve 0.20.0's sources under
`~/.cargo/registry/src/` - `src/routing.rs` (how `Router`, `PathRouter`, `MethodRouter`
and the `Either` nesting are built, and what trait a hand-written router must implement
to be served by `picoserve::Server` / `serve`), `src/request.rs`, `src/response/`,
`src/extract.rs`.

Already established:

- The regression gates exist and are one command each, run by the orchestrator over the
  LAN (you cannot reach it): `screeny-probe --addr 192.168.7.221 http` (38 rules,
  card 228) and `... conformance --slow` (64 UDP rules). On the device, the in-memory
  router self-test (`--features http-selftest`, card 222) runs the real router through a
  byte-slice socket and logs every route's status code, size and time: **it is your own
  before/after evidence** - extend it to cover all twelve cases it has today against the
  new dispatch, unchanged.
- `stack_probe` samples both cores' painted stacks; the 60 s `stack:` line and the 4 Hz
  growth trace (card 227) are in the default build. `tools/fw-size.sh` has a 24,576 floor.
- picoserve streams replies (measures with a counting writer, then writes); request
  bodies are read through the extractors; the WiFi form needs the raw body
  (`form::parse_wifi_form`) because SSIDs are bytes; JSON bodies need
  `JsonWithUnescapeBufferSize<T, { MIN_UNESCAPE_BUFFER }>` (or the equivalent call to
  `serde_json_core::from_slice_escaped`) - plain `from_slice` silently does not unescape.
- Credentials posted over HTTP are handed to the WiFi task as `NewWifi { wifi, persist }`
  and stored only after they join (fw 0.4.1). Do not reintroduce a write in the handler.
- Decision recorded for card 223, which you may make true now because it is the same
  code: per-route `max_request_len` enforced explicitly (413 `payload_too_large` with a
  sentence), since a single dispatch makes that a table lookup. It flips the probe's
  rule 29 from a known-223 skip to a pass; do not flip the probe's constant yourself.

What the first device run of `screeny-probe http` found on fw 0.4.2 (26 passed, 3
failed, 9 skipped) - **fix these here, they fall out of owning the dispatch**:

- Rules 26 and 37: a verb the API has no method for (`DELETE /api/v1/status`) is answered
  by picoserve's own plain-text `405 Method DELETE not allowed for ...`. Every refusal
  must be the API's error shape: `405 {"error":"method_not_allowed", ...}` for a known
  path (any verb, including ones picoserve's `MethodRouter` has no slot for), `404` for an
  unknown one.
- Rule 34: `POST /api/v1/reboot` without the magic word must answer `400 out_of_range`
  (today `bad_request`), the same mapping UDP `REBOOT`'s bad magic gets and what the
  simulator answers.

Card 227's correction to this card's premise: the frames listed in the Goal do **not**
all coexist (the outer `Either` contains the inner by value and its poll is inlined), and
under 2,378 real connections core 0 only reached 13.3 KB - the boot path sets the mark
(13,056). fw 0.4.2 has `stack_free` ~17-19 KB. So the prize here is smaller than first
thought and must be **measured, not assumed**: the 5,680-byte router frame is still 43%
of the high-water, the http task's pool entry is 7,504 bytes per worker (x2), and card
223 lands ~1.2 KB under the `fw-size.sh` floor without another lever. If the single
dispatch does not shrink the measured high-water or the pool entry by at least ~2 KB in
total, say so plainly, keep the correctness fixes above, and recommend cards 234/235
(frame-socket tx buffer, mDNS buffers) as 223's lever instead.

## Deliverables

1. **The dispatch.** One future, one `match` (or table walk over `route::ROUTES`) from
   `(method, path)` to a handler; handlers return through one response path rather than
   each being its own generic layer. Options to weigh, in the Log: implement picoserve's
   router/service trait by hand and keep `picoserve::serve` for the HTTP parsing and
   response writing; or keep a `Router` with a **single** catch-all route whose handler
   does the dispatch. Prefer whichever keeps picoserve doing the protocol work and
   removes the nesting; do not write an HTTP parser. Large per-handler futures should not
   be held by value in an enum that is as large as the biggest one *if* that recreates
   the problem - measure, do not assume: the evidence is `objdump` frame sizes before and
   after for every symbol on the request path, in a table in the card Log.
2. **Numbers.** `tools/fw-size.sh` before/after; the 60 s `stack:` line; the selftest's
   per-route times (they should not get worse); and, from the orchestrator's load run,
   `stack_free` under HTTP load. **Target: measured high-water down and/or the http pool entry down, >= 2 KB in total,
   on the default build with two HTTP workers** (fw 0.4.2 baseline: `.stack` 33,072,
   `stack_free` ~17-19 KB under load), and the http task's pool entry smaller than
   today's 7,504 bytes per worker if the restructure allows (report it either way).
3. Wire behaviour identical: same status codes, same bodies, same headers that matter
   (`Content-Type`, `Content-Length`, `Connection: close`), 405 vs 404 via
   `route::find` / `route::path_is_known`, the catch-all hook left in an obvious place
   for card 223's captive-portal redirect (which must run **before** routing).
4. `FW_VERSION` bumped by one patch level from whatever `main` has. Default build left
   running on the device.
5. A short section appended to `docs/research/010-stack-and-ram-levers.md`: what the
   dispatch looks like now, the before/after frame table, and the updated budget for 223.

## Bench discipline (orders)

Iterate on the host (build, `fw-size.sh`, objdump) first; expect 3-4 flashes; write in
the Log why each happened. Flash only with
`/Users/aaron/src/screeny/tools/fw-run.sh <elf> <name> [secs]`, wrapped in `timeout 400`,
secs <= 200. No other monitors. If a build does not boot, reflash the last good build
before diagnosing. `pgrep -fl espflash` empty when you finish. You cannot reach the LAN:
when you need HTTP load or the conformance suites run against your build, ask the
orchestrator (SendMessage to "main", say what and when; plan for at most two such
windows). Serial logs contain the real SSID and BSSIDs: never copy those lines. No
`bench-wifi`, no reading of any `wifi.env`, and **never post WiFi credentials to the
device**.

## Out of scope

The soft-AP, DHCP, DNS, the portal, scanning (223); OTA; the button; `crates/*`; the spec;
`docs/design/*`; `tools/`. New routes. Keep-alive.

## Acceptance

- Worker: frame tables, sizes, the selftest green on the device, every existing feature
  building, the default build left running and streaming.
- Orchestrator, after the merge: `screeny-probe http` with no new failures (rule 29 now
  passing), UDP conformance 60/0/4, the HTTP hammer during a stream (30 fps, zero drops,
  `render` max unchanged), `stack_free` >= 16 KB afterwards, back-to-back connects fast.

## Log

### 2026-09-20 - the design, and why

Read `picoserve 0.20.0`'s `src/routing.rs` before deciding. Three shapes were on the
table; the one that won is **`Router::from_service(Dispatch)`**, where `Dispatch` is a
single `PathRouterService`.

* **Hand-written `PathRouter`** - not possible, and this is worth recording: `PathRouter`
  is *sealed* (`routing::sealed::PathRouterIsSealed`, "Only `picoserve` may create types
  which implement PathRouter"). `PathRouterService` is **not** sealed, and
  `Router::from_service` wraps one in the `ServicePathRouter` that `picoserve::Server`
  takes. So the supported way to own the dispatch is exactly the one the library
  provides, and `Server::new(&app, ..)` / `listen_and_serve` are untouched.
* **A `Router` with a single catch-all route** - `route()` needs a `PathDescription`, and
  the only wildcard is `parse_path_segment`, which captures *one* segment. There is no
  "match everything" path description, so this would have needed one route per depth.
  Worse, it keeps the `Route<..>` layer that is the thing being removed.
* **What was chosen**: `Router::from_service(Dispatch)` - one `PathRouterService` whose
  `call_path_router_service` reads `parts.method()` and `parts.path()`, calls
  `route_request(..) -> Reply`, then writes that one `Reply`. The router value is a ZST,
  the route table is `route::ROUTES`, and the 404/405 split is `known_path` +
  `route::find`, the same pair the simulator and the probe use.

**What picoserve still does** (all of it, unchanged): the listener and accept loop, the
four per-phase timeouts, parsing the request line and headers, buffering the body,
`entire_body_fits_into_buffer` / `read_all`, measuring every reply with a counting writer,
writing `Content-Type` / `Content-Length` / `Connection: close`, streaming the body, and
draining an unread request body in `finalize()`. Nothing in `http.rs` parses or formats
HTTP. The card's "do not write an HTTP parser" is met by construction.

**Second decision, worth as much as the first: one reply type.** Nine nested routing
layers were only half the shape. Each handler also returned its own
`Result<Json<T>, ApiError>`, so picoserve monomorphised "measure it, write the headers,
stream it" **seven** times, and the request path's frame held whichever one was in flight.
`ApiBody` is now one enum with a hand-written untagged `Serialize` (each variant
serialises exactly as the reply type it holds, so **nothing changes on the wire**), and
`Reply` is `Page | Api(u16, ApiBody)` - two arms, one frame. `ApiError` is gone: it was a
newtype over `ErrorReply` that existed only to carry an `IntoResponse`, and `Reply::error`
does that now.

Collapsing the seven writers to two was worth **8,000 bytes of flash and ~530 bytes of
frame** on its own, measured as a separate build step before the percent-decoding fix.

**Percent-decoding kept.** picoserve's `Route` matched paths through
`PartialEq<&str> for Path`, which decodes as it goes, so `/api/v1/%73tatus` was
`/api/v1/status`. A plain `&str ==` on `path.encoded()` would have quietly changed that,
so `known_path()` canonicalises with picoserve's own comparison and everything downstream
works on `route::ROUTES`'s `&'static str`. Costs 1,400 bytes of flash; keeps the card's
"wire behaviour identical".

**HEAD: a deliberate, documented change.** picoserve's generated method router answered
`HEAD` by running the `GET` handler through a body-discarding `ResponseWriter`
(`routing::head_method_util::ignore_body`). That module is private *and cannot be
reimplemented outside picoserve*: `Response`'s `status_code` / `headers` / `body` fields
are `pub(crate)`, so no foreign `ResponseWriter` can rebuild a response with an empty
body, and `Json`'s `write_to`/`JsonBody` are private too, so the `Content` route to the
same end is closed. The dispatch therefore answers `HEAD` the way `screeny-device-api`
says to: `route::Method` is `Get` and `Post` and nothing else, and probe rule 26 says a
verb this API has no method for is `405 method_not_allowed`. So `HEAD /` is now
`405 {"error":"method_not_allowed"}` instead of `200` with headers and no body. It is
consistent with rules 26 and 37 and with the error-shape rule, it is the only refusal
shape on this server, and it deletes one of the two copies of the whole response path.
Flagged as follow-up **236** if the owner wants RFC-conformant HEAD back - it would need
an upstream change or a fork.

### 2026-09-20 - built, measured on the host

`.` `~/export-esp.sh` then `cargo build --release`, `tools/fw-size.sh`, and
`xtensa-esp32-elf-objdump -d --demangle` (largest `entry a1, N` per symbol, **N parsed as
hex above 255**, research 010's method).

**`tools/fw-size.sh`, default build:**

| | `.data` | `.bss` | `.stack` | image |
|---|---|---|---|---|
| before (fw 0.4.2, this worktree) | 58,388 | 105,136 | **33,072** | 957,185 |
| after (fw 0.4.3) | 58,380 | 103,872 | **34,352** | 910,041 |
| delta | -8 | **-1,264** | **+1,280** | **-47,144** |

**`http_task::POOL`** (`nm -S`), the two workers' `.bss`:

| | pool | per worker |
|---|---|---|
| before | 14,576 (0x38f0) | **7,288** |
| after | 13,312 (0x3400) | **6,656** |
| delta | **-1,264** | **-632** |

`.stack` and `.bss` are the same 1,264 bytes seen from the two ends of one DRAM region -
they are **not** two separate wins, and the report says so.

**The request path, before:**

| # | frame | bytes |
|---|---|---|
| 1 | `TaskStorage<http_task>::poll` (accept loop, `serve_and_shutdown` inlined) | 5,536 |
| 2 | `Router::handle_request::poll` | 784 |
| 3 | router `Either<..>::poll`, outer (settings/wifi/firmware/telemetry/status/page/404) | 2,416 |
| 4 | router `Either<..>::poll`, inner tail (telemetry/status/page/404) | 2,192 |
| 5 | `Result<Json<AcceptedReply>, ApiError>::write_to_with_state` | 1,040 |
| 5' | same, `Json<()>` + `IgnoreBody` (the HEAD copy) | 1,104 |
| 6 | `ApiError::write_to` / `get_page` | 464 / 608 |
| | **deepest chain 1+2+3+5+6** | **~10,240** |

**The request path, after:**

| # | frame | bytes |
|---|---|---|
| 1 | `TaskStorage<http_task>::poll` | **4,400** |
| 2 | `Router::handle_request::poll` (`Dispatch` and `Reply::write_to` inlined into it) | **1,712** |
| 3 | `route_request::poll` (the `match`, all handler bodies unioned) | **1,488** |
| 4 | `status()` / `apply_control()` / `get_wifi` | 368 / 208 / 176 |
| | **deepest chain 1+2+3** | **~7,600** |

Frames 2 and 3 are the whole router now. `Reply::write_to` and `route_request` are
alternatives in time, not a chain - the reply is computed, the handler future is dropped,
*then* it is written - so the worst case is 1+2+max(3, inlined writer), i.e. **7,600
against 10,240: -2,640 bytes of depth.** (Research 010's warning applies: a column of
objdump frames is an upper bound per function, not a call chain. The comparison is
like-for-like, which is what makes it worth quoting.)

Nine `call_method_handler` monomorphisations, two `Either::poll`s, seven
`write_to_with_state`s and the whole `IgnoreBody` duplicate of the response path are gone
from the disassembly. That is where the 47 KB of flash went.

Every feature still checks: `http-selftest`, `store-selftest`, `spike-ota`, `apsta-probe`,
`device-web-spike`, `display-on-core0`, `fb-on-stack`, and the `gpio_probe` binary. Zero
errors each. `bench-wifi` was **not** built (the orders forbid it); it touches `build.rs`,
`main.rs` and `store.rs` and nothing in `http.rs`. `cargo doc` adds no new warning (the
one it prints, an unresolved `[Psk]` link in `store.rs`, is pre-existing).

### 2026-09-20 - the self-test, extended, and flash 1 of 2

Card 222's twelve cases are unchanged - they are the before/after evidence - and eight
more were added for questions only the new dispatch can be asked: `DELETE` and `PUT` on
known paths, `HEAD /` and `POST /`, `/api/v1/status/` (must stay a 404) and
`/api/v1/%73tatus` (must still be `status`), and two oversize bodies. The self-test's own
HTTP buffer went 512 -> 768 to hold the longest of them; it is feature-gated, so the
default build is unaffected.

**Flash 1** (`card233-selftest`, `--features http-selftest`, 190 s). Why: the twenty route
cases and the on-device high-water across them, which is this card's own evidence for
everything that does not need real TCP.

All twenty green first try:

| case | got | want | | case | got | want |
|---|---|---|---|---|---|---|
| `GET /` | 200 | 200 | | `DELETE /api/v1/status` | **405** | 405 |
| `GET /api/v1/status` | 200 | 200 | | `PUT /api/v1/settings` | **405** | 405 |
| `GET /api/v1/telemetry` | 200 | 200 | | `HEAD /` | **405** | 405 |
| `GET /api/v1/wifi` | 200 | 200 | | `POST /` | **405** | 405 |
| `GET /api/v1/networks` | 503 | 503 | | `GET /api/v1/status/` | **404** | 404 |
| `POST /api/v1/identify` | 200 | 200 | | `GET /api/v1/%73tatus` | **200** | 200 |
| `POST /api/v1/settings` | 200 | 200 | | `POST identify 153 > 152` | **413** | 413 |
| `POST /api/v1/settings` bad | 400 | 400 | | `POST wifi 385 > 384` | **413** | 413 |
| `POST reboot` unconfirmed | 400 | 400 | | `GET /api/v1/settings` | 405 | 405 |
| `POST /api/v1/firmware` | 503 | 503 | | `GET /nope` | 404 | 404 |

Bodies worth quoting: the unconfirmed reboot now says
`{"error":"out_of_range","detail":"confirm must be \"RBOO\""}` (rule 34), `DELETE` says
`{"error":"method_not_allowed"}` in the API shape (rules 26 and 37), and the 153-byte
identify says `{"error":"payload_too_large","detail":"the body is longer than this route
accepts"}` - **153 is inside the global 384-byte bound and one byte over that route's own
152, so the 413 is proof the per-route number is being read from `route::ROUTES`**
(rule 29). Per-route times 2,364-7,151 us, against card 222's 6,567 and 7,339 us worst:
not worse.

**The number this flash existed for.** The self-test prints core 0's high-water either
side of the whole route table:

| build | across the whole route table |
|---|---|
| card 222 (`222-selftest-c` and `-d`, two runs) | 13,056 -> **14,208** (+1,152) |
| card 233 (`card233-selftest`) | 13,056 -> **13,056** (**+0**) |

The request path used to push the mark 1,152 bytes above the boot mark. It no longer
reaches the boot mark at all. Frame path during the window: 30 fps rx, 30 fps shown, zero
decode drops, zero rejected.

### 2026-09-20 - flash 2 of 2, the default build, and the orchestrator's window

**Flash 2** (`card233-default`, default build, 195 s). Why: the device must end on the
default build, and the conformance suites and the HTTP load must run against it and not
against a build carrying the self-test's extra `.bss`. Message sent to the orchestrator at
the moment the flash started.

**The 60 s `stack:` line, before and after.** Before is card 227's run C, the identical
0.4.2 default build on this bench (`captures/card227-c.log`):

```
before: stack: core 0 high-water 13056 of 33072 (+13056), 18992 free
after:  stack: core 0 high-water 13056 of 34352 (+13056), 20272 free
```

and core 1 is 1,392 of 6,144 at 60 s in both.

**`watch_task` logs a line every time a mark grows. Across the whole 195 s run - 904
requests from the LAN, 764 answered 200, plus the 38-rule conformance suite - there is not
one growth line for core 0.** The mark set at boot was never beaten. On 0.4.2 the same
load walked it to 13,232 (run C) and 13,328 (run A) in 16-to-112-byte steps. Research
010's "serving HTTP flat out is worth 272 bytes" is now worth zero.

**What the orchestrator measured over the LAN:**

* Loop: 904 requests, 764 answered 200, first 200 at +53 s, no failures once up.
* `screeny-probe --addr 192.168.7.221 http`: **30 passed, 0 failed, 8 skipped**
  (0.4.2 was 26/3/9). **Rules 26, 34 and 37 pass. Rule 29 passes**, flipped from its
  `KNOWN_223` skip with the probe's constant untouched. The 8 skips are the expected
  ones (11, 12 no scan; 19 `--cap-probe`; 23, 24 no uploads; 32, 33
  `--allow-wifi-trial`; 35 `--allow-reboot`). Everything it changed was restored.
* Back-to-back connects: 0.0246 / 0.0079 / 0.0073 s to connect. No SYN retransmit.
* The device's own status at 91 s: **`fw 0.4.3`, `stack_free 20272`**, heap 45,612 of
  90,112, `wifi_state connected`, `state live`, `store_errors 0`.
* Checked by hand: `HEAD /` -> 405 and `DELETE /api/v1/status` ->
  `{"error":"method_not_allowed"}`. The HEAD change is accepted; nothing of ours HEADs
  the device.

Frame path throughout: 30 fps rx, 30 fps shown, decode drops 0, rejected 0, `render`
3,091-3,142 us mean with a 3,231 us window max against a 3,428 us boot max - unchanged.
No panic, no guard trip, no backtrace in either log.

### 2026-09-20 - the exit test, and the honest answer

The card's target: **"measured high-water down and/or the http pool entry down, >= 2 KB
in total".**

| | before | after | delta |
|---|---|---|---|
| measured high-water under load | 13,232 (run C) / 13,328 (run A) | **13,056** | **-176 / -272** |
| `http_task::POOL`, both workers | 14,576 | **13,312** | **-1,264** |
| ...per worker | 7,288 | **6,656** | -632 |
| **total against the target** | | | **~1,440-1,536** |

**The target is missed, by roughly 500 bytes.** Saying it plainly, as the card asks: the
supply this bought is one saving of ~1,270 bytes seen from two ends - the pool entry came
out of `.bss`, `.bss` is core 0's `.stack` with extra steps, and `.stack` went up by the
same amount. Counting it twice would be dishonest.

Research 010 section 3's "roughly 5 KB of depth" was a guess made from a column of
objdump frames, and section 2's own lesson - a frame table is an upper bound per function,
not a call chain - is exactly why it was too big.

**What it did buy, which is the part card 223 actually needs:**

* **`stack_free` 18,816 -> 20,272 under load**, and it is now a number that does not
  creep: the high-water is set at boot and HTTP no longer touches it. Research 010
  conclusion 3 called the mark "a record of coincidences"; the coincidences no longer
  have a deep enough chain to land on.
* **-2,640 bytes of request-path depth** by objdump, and **+1,152 -> +0** on the device.
* **47 KB of flash** (957,185 -> 910,041 image), 5% of the image, which card 240's OTA
  path will want.
* Three conformance failures fixed and a fourth rule flipped from skip to pass.

**Recommendation for card 223's lever, as the card asks for if the target is missed:**
take **card 234** (the frame socket's transmit buffer, ~1.9 KB) as a dependency. It alone
covers the shortfall with room, it is behaviour-preserving, and card 235 (the mDNS
buffers, ~1.9 KB) is the alternative if reading the spec turns 234 down. Card 223 now
lands at about `.stack` 24,700 without either - just over the 24,576 floor with nothing
spare - and at ~26.6 KB with one of them. Research 010 section 8 has the arithmetic.

**Flashes: 2.** One `http-selftest` build for the twenty route cases and the on-device
depth measurement, one default build for the 60 s `stack:` line, the orchestrator's load
window and the state the device is left in. No build failed to boot, so no recovery flash
was needed. `pgrep -fl espflash` is empty; the device is running this branch's **default**
build (fw 0.4.3), joined, streaming 30 fps.

### Out of scope, deliberately not done

No new routes, no keep-alive, no soft-AP / DHCP / DNS / portal / OTA / button work.
Nothing under `crates/`, `docs/design/`, `tools/` or another card was touched -
`crates/device-api` had everything the dispatch needed (`route::find`,
`route::path_is_known`, `route::Route::max_request_len`, `ErrorReply`), so no local
wrapper was necessary. `tools/fw-size.sh`'s header still quotes research 010's old
"7,872 bytes before a handler runs"; the floor and the script are right, only the
justification is now historical, and changing a tool is another card's job.

### Proposed follow-ups (numbers for the orchestrator to assign; no card files written)

1. **RFC-conformant `HEAD`.** Needs a change to picoserve (either `Response`'s fields
   made public, a public `ignore_body`, or a `Response::into_parts`), so it is an upstream
   issue or a vendored patch, not a firmware card. Today's answer, `405
   method_not_allowed`, is what `route::Method` and probe rule 26 prescribe and is
   consistent with every other refusal, so this is a nicety and not a bug.
2. **A conformance rule for the dispatch's two near-misses.** `GET /api/v1/status/` must
   be 404 and `GET /api/v1/%73tatus` must be 200. The device self-test covers both now;
   the simulator and the probe do not, and they are exactly the cases a future router
   change would break silently. Wants `crates/probe` and `crates/sim`, which this card
   could not touch.
3. **Card 223 depends on card 234.** Already argued above and in research 010 section 8.
   Worth making a real dependency edge rather than a sentence.
4. **The boot path is now the whole high-water, so it is the next thing worth cutting.**
   Research 010 section 7's suggestion about the two 3 KB partition-table buffers was
   parked because the HTTP path was deeper. It is not any more: 13,056 of 13,056 is
   `main`'s 5,104-byte poll frame plus `store::find_partition`'s 3,200 plus
   esp-storage's ~4,150. One shared buffer is worth ~3 KB off the *only* thing setting
   the mark, which is now the cheapest demand lever left by a distance.

### Orchestrator, after the merge (2026-09-20)

`main` builds to `.stack` 34,352, image 909,665. On the device (fw 0.4.3): HTTP
conformance 30 passed / 0 failed / 8 skipped (was 26/3/9 on 0.4.2; rule 29 passing with
`CARD_223_LANDED` untouched); UDP conformance 60/0/4; 764 of 904 requests answered during
the flash-and-boot window with none failing once up; back-to-back connects 7-25 ms;
`stack_free` 20,272 and, per the worker's serial log, not one high-water growth line
under that load. Accepted: `HEAD /` is 405 (picoserve's body-less writer cannot be
reused from outside the crate); the >= 2 KB RAM target was missed by ~500 bytes and said
so plainly - card 223 takes the frame-socket tx buffer lever (ex-234) as its first step.
