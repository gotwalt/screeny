---
id: 228
title: screeny-probe http - a conformance run over the device's HTTP API, the same suite against the sim and the device
type: test
hardware: no
depends: [224, 226, 232]
owner: worker-228
branch: card/228-probe-http-conformance
---

## Goal

The UDP side has `screeny-probe conformance`: 64 rules, one line each, safe to point at
the real panel, and the regression gate after every flash. The HTTP side now has nine
routes, an error shape, size bounds and a provisioning flow - implemented twice (the
firmware's picoserve server and the simulator's) - and nothing that asks both the same
questions. Build that: `screeny-probe http`, run by tests against the in-process
simulator and by the orchestrator against the device.

## Context

Read first: `CLAUDE.md`; `docs/design/device-web.md` ("The HTTP API", the working
agreement, decisions 3 and 7); `crates/device-api/README.md`, `src/route.rs` (`ROUTES`,
`find`, `RateLimit`, per-route bounds), `src/error.rs` and `tests/golden/`;
`crates/probe/src/main.rs` and `crates/probe/src/suite/` (card 080's suite: how rules
are declared, numbered, cited, reported, skipped, and how state is restored on **every**
exit path including ctrl-c - copy that discipline exactly);
`crates/sim/tests/conformance.rs` (how the UDP suite is run against an in-process sim)
and `crates/sim/tests/http_routes.rs` / `http_wifi.rs` (what the sim's HTTP side already
asserts about itself); `docs/board/done/222-firmware-http-lan.md` (the firmware server's
real shape: one worker, no keep-alive, no listen backlog - **a connection made while
another is being served is dropped at SYN and retried by the OS after 1 s**, so the
suite must use one connection at a time, `Connection: close`, and must not read a 1 s
connect as a failure); `docs/board/done/224-*.md` and `232-*.md`.

Decided:

- **Safe to point at the real device by default.** No credentials post, no reboot, no
  firmware upload, nothing that takes the device off its network or interrupts a stream,
  unless the caller passes an explicit flag: `--allow-reboot`, `--allow-wifi-trial`
  (posts the dummies `Example-Wifi1` / `password9`, waits for the fallback, asserts the
  sticky `failed` + `reason`, and says in its output that the device was off the network
  for about a minute). Settings it changes (name, brightness only ever stepped **down**,
  idle mode) are restored on every exit path, and the last line says what it restored
  to - as the UDP suite does.
- Rules are data with a number, the route, a citation (`device-web.md` section or
  `crates/device-api` item) and a one-line result, `PASS` / `FAIL` / `SKIP <reason>`;
  exit status non-zero on any failure; `--list`, `--only`. Replies are parsed with the
  `screeny-device-api` types - the suite defines no shapes of its own.
- What to check, at least: every `ROUTES` entry's happy path and its reply type; wrong
  method -> 405 and unknown path -> 404 in the error shape; bad JSON -> 400; a body over
  `max_request_len` -> 413; every reply within its `MAX_JSON_LEN`; **no reply contains a
  PSK** (post a settings/wifi-shaped body where allowed and grep every reply); `status`
  invariants (`api == 1`, `boot_id` stable across two reads, `uptime_ms` increasing,
  `heap_used <= heap_size`, `ip` parses, `fw_slot`/`fw_state` are known values);
  `telemetry` agrees with a UDP `TELEMETRY` taken around the same moment (frame counters
  within a tolerance; same `state`); `settings` round-trip and clamping (brightness above
  the firmware cap is applied as the cap and echoed so); `identify` raises the
  `IDENTIFY` overlay as seen by UDP telemetry; `networks` is either a valid list or the
  crate's `unavailable`; the scan rate limit answers 429 with a wait; `firmware` with an
  empty or non-image body is refused (or `unavailable`) and **never** answers `ok`;
  `GET /` is HTML, non-empty, and contains no external URL; timing: a lone request
  completes within a generous bound (report the median, do not fail on it over WiFi).
  Rules that a given target legitimately cannot satisfy are `SKIP` with the reason.
- Known, intended differences between the two servers become explicit `SKIP`s or
  target-aware expectations, each with a comment: the firmware answers `unavailable` for
  `networks` and `firmware` until cards 223/240; the simulator's `firmware` runs two of
  the five image checks; in firmware 0.4.0 `status.wifi_state` can read the sticky
  `failed` and `wifi.reason` can be `null` (both fixed by card 223 - mark the rules so
  they flip from SKIP to enforced with one constant).
- HTTP client: `crates/probe` is a dev-dependency of `crates/sim` and is compiled by
  everyone; keep it light. A small blocking HTTP/1.1 client over `std::net::TcpStream`
  (one request per connection, `Connection: close`, `Content-Length` bodies, timeouts)
  is preferred over a client crate; if you take a dependency, justify it in the Log and
  make sure it pulls no async runtime and no TLS stack.

## Deliverables

1. `screeny-probe http [--http HOST[:PORT]] [--only N|SECTION] [--list] [--allow-reboot]
   [--allow-wifi-trial]`, sharing `--addr`/`--name` with the existing commands (the HTTP
   address defaults to the device address, port 80).
2. The suite as a library module so tests can run it: `crates/sim/tests/http_conformance.rs`
   (or the equivalent in `crates/probe`, wherever the dependency direction allows - the
   UDP suite's arrangement is the precedent) runs **all** rules, including the
   `--allow-*` ones, against an in-process simulator with ephemeral ports.
3. README/usage text updated; the orchestrator's one-liner for the bench written at the
   top of the module docs:
   `cargo run --release -p screeny-probe -- --addr 192.168.7.221 http`.

## Out of scope

`firmware/` (a hardware worker owns it), `crates/device-api`, `crates/provision`,
`crates/proto`, `crates/receiver`, `crates/studio`, the spec, `docs/design/*`. In
`crates/sim` you may add the test file and fix a genuine simulator bug the suite exposes
(say so loudly in the Log); you may not change its UDP behaviour, CLI or library API. If
the suite exposes a disagreement between the simulator and the documented API, the
documented API wins; if the *documentation* is ambiguous, write the question in the Log
and make the rule a SKIP rather than deciding it.

## Acceptance

`cargo test -p screeny-probe -p screeny-sim` green (the 64-rule UDP conformance test
unchanged); clippy no worse than before on both crates; root `cargo test` green. You
cannot reach the device or the LAN; do not try. The orchestrator runs it against the
device after the merge and appends the result to this card.

## Log
