---
id: 233
title: Firmware - one HTTP dispatch instead of nine nested router futures (about 7.8 KB of stack depth back)
type: build
hardware: yes
depends: [227, 228]
owner:
branch:
---

## Goal

Card 227 found where core 0's stack goes under HTTP load, and it is not a buffer: it is
the shape of picoserve's router. `Router::new().route(..).route(..)` builds a left-nested
`Either<..>` type, and each layer's `poll` frame holds the rest of the chain by value.
Measured frames on the request path: 5,968 (the http task) + 800 + 5,680 + 2,192 + 1,104
= 15.7 KB, of which **7,872 bytes are dispatch before any handler runs**. With interrupts
landing on the same stack, that is why `stack_free` was 4-8 KB on firmware 0.4.x, and why
the portal (card 223, ~9 KB more `.bss`) does not comfortably fit. Replace the nested
router with a single dispatch so the depth of a request is the http task plus one handler.
No behaviour change on the wire.

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
