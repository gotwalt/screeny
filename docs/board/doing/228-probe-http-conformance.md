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

### worker-228

**Step 1 - read, then design (2026-09-20).**

Read `CLAUDE.md`, `docs/README.md`, `docs/design/device-web.md`, `crates/device-api`
(`route.rs`, `error.rs`, `reply.rs`, `request.rs`, `enums.rs`), `crates/probe/src/suite/`,
`crates/sim/src/{api,http,wifi}.rs`, `crates/sim/tests/{conformance,http_routes,http_wifi}.rs`
and the done logs of cards 222 and 232. Merged `main` into the worktree first: the base
was two commits behind and did not have the card.

Shape settled before writing anything:

- `crates/probe/src/http/` - `mod.rs` (rule type, `Ctx`, restore, the runner),
  `client.rs` (the HTTP client), `rules.rs` (the catalogue). The UDP suite's `suite/` is
  the precedent for every bit of it: rules are data, one line of output each, `--list`,
  `--only`, a `RestoreGuard` plus a ctrl-c handler.
- **How the suite tells a simulator from a device: it does not have to.** Every route a
  build cannot serve answers `503 unavailable` in the crate's own error shape, and the
  rules that could hit one read that and `SKIP` with the reply's `detail`. That covers
  `GET /api/v1/networks` and `POST /api/v1/firmware` on firmware 0.4.0, and it will keep
  covering them when 223 and 240 land without a line changing.
- **One constant for the known firmware differences.** `http::CARD_223_LANDED` (false).
  While it is false, a rule tagged `KNOWN_223` that *fails* is reported as `SKIP` with
  the card number instead; a **pass is still a pass**, so the simulator is held to those
  rules today and the device is not. Flipping the one constant enforces them everywhere.
  Four rules carry it: 8 (`status.wifi_state` is the link), 14 (`wifi.reason` set exactly
  when failed), 29 (per-route `max_request_len`), 33 (after the trial).
- **HTTP client: written here, no crate.** ~300 lines over `std::net::TcpStream`, one
  request per connection, `Connection: close`, `Content-Length` bodies, an 8 s connect
  timeout (card 222: a second connection to the one-worker firmware waits out a 1 s SYN
  retransmit - that is the device behaving, not failing) and a 10 s IO timeout. It reads
  exactly `Content-Length` bytes when there is one and to EOF when there is not, so a
  server that forgets to close costs a parse error and not a ten-second stall.
- New dependencies, both already compiled by anything that compiles `crates/sim`:
  `screeny-device-api` (the reply types - the suite defines no shapes of its own) and
  `serde_json` + `serde`. No async runtime, no TLS, no HTTP crate.

**Step 2 - the client, the runner and 38 rules.** `cargo test -p screeny-probe`: 14
tests green, including two guards - every row of `route::ROUTES` is mentioned by a rule
(the same guard `crates/sim/tests/http_routes.rs` puts on the server), and the rule
numbers are unique and ascending. Clippy on `crates/probe` adds no warning (the two it
prints are pre-existing, in `vectors.rs` and `main.rs`).

**Step 3 - the CLI, the in-process run, and a simulator bug.**

`screeny-probe http` shares `--addr`/`--name` with every other command and defaults the
HTTP address to that host on port 80; `--http HOST[:PORT]` points it elsewhere (a
simulator on `127.0.0.1:8080`) while the UDP ports stay where they were, because two
rules compare the two halves of one device against each other. `http --list` is answered
before any address is resolved, so it works with no device and no DNS - the existing
`conformance --list` still resolves first, and I did not touch it.

`crates/sim/tests/http_conformance.rs` runs **all 38 rules**, `--allow-reboot`,
`--allow-wifi-trial` and `--cap-probe` included, against an in-process simulator on
ephemeral ports. The simulator is shaped so each opt-in rule has something to measure: a
brightness cap of 100 under a starting brightness of 96, and the scripted radio set to
`WifiOutcome::Fail` after the boot join so the posted credentials are refused the way
`Example-Wifi1` is refused on the bench. Four more tests: `--only` by number and by
section, a target that is not there being one clear error rather than 38 failures, the
device being as it was found afterwards, and the one below.

**The suite found a bug in the simulator, and I fixed it.** `docs/design/device-web.md`
(the card 223 paragraph) says `GET /api/v1/status`'s `wifi_state` means **the link** and
never the sticky result of the last credentials attempt - that result belongs to
`GET /api/v1/wifi`. `crates/sim/src/api.rs` built it from `wifi.wifi_state()`, which is
the provisioning machine's sticky byte (correctly, for UDP `GET_WIFI`, spec 8.3). So a
simulator that ran a trial, failed it and fell back onto its stored network read
`wifi_state: failed` while online and holding an address - **exactly the firmware 0.4.0
behaviour card 223 exists to fix**. Rule 8 caught it the moment a test put the simulator
in that state.

The fix is one derivation in one place: `WifiModel::link_state()` (`pub(crate)`, no
public API change), used by `wifi_reply()`'s non-trial branch and by `api.rs`'s status
route, so the two cannot drift apart again. **UDP `GET_WIFI` is untouched** - it still
answers `wifi.wifi_state()`, sticky `FAILED` and all, and `crates/sim/tests/http_wifi.rs`
still pins that. `crates/sim`: 131 tests green, the 64-rule UDP conformance test
unchanged and still green.

Open question I did **not** decide (card 233 below): in `Portal`, `link_state()` still
reports the machine's byte, so a device sitting in the portal after a failed trial reads
`wifi_state: failed` rather than `disconnected`. That is what `wifi_reply()` has always
done and nothing documents which is right; a device in the portal has no link to
describe, so `disconnected` is arguably the honest answer. Left alone.

One more thing the in-process run taught me: rule 33 originally waited only for the
device to *answer*, which on a simulator is instantly, and then read a trial that had not
resolved yet (`connecting`). It now polls `GET /api/v1/wifi` until the state is no longer
`connecting`, bounded by 150 s. That is the wait that is right for both targets: on the
bench the device is unreachable for about a minute first, and connection failures during
that window are not counted as failures.

**Step 5 - what the orchestrator runs on the bench.**

`main` moved under me while I worked, and one of the things it brought matters here:
firmware before 0.4.1 stored posted credentials **before** they had joined, and the
orchestrator's own wrong-credentials test over HTTP once replaced the working pair in
flash. So `--allow-wifi-trial` now prints that warning before it runs, names the
firmware version, and - when `--allow-reboot` is not also given - prints the bench rule
that follows it ("reboot the device and see it rejoin before calling the test passed").
Rule 35 *is* that check: on a LAN address, a device that answers `GET /api/v1/status`
again after a reboot has rejoined, because there is no other way the request arrived.
Its detail says so.

```sh
D=192.168.7.221

# 0. what it will do, before doing it. Talks to nothing; no device needed.
cargo run --release -p screeny-probe -- http --list

# 1. THE ONE TO RUN AFTER EVERY FLASH. Safe by default: nothing leaves the
#    network, nothing restarts, nothing is written to flash, and brightness only
#    ever steps down. Name, brightness, idle mode and the identify overlay are
#    put back on every exit path and the last line says what they went back to.
#    It does take the panel for ~2 s with the identify overlay (rule 21), and it
#    does NOT need the source lock - running it while the Studio streams is
#    better evidence, not worse. ~60-90 s on the bench.
cargo run --release -p screeny-probe -- --addr $D http

# 2. when something failed: one section, or one rule, by number.
cargo run --release -p screeny-probe -- --addr $D http --only settings
cargo run --release -p screeny-probe -- --addr $D http --only 10

# 3. the brightness cap. Drives the panel to the firmware cap for a moment and
#    then back to what it found - the same opt-in, and the same reason, as the
#    UDP suite's --cap-probe. Only with the panel in view.
cargo run --release -p screeny-probe -- --addr $D http --cap-probe

# 4. the credentials trial AND the reboot the bench rule requires, in one run
#    and in the right order (rules 32, 33, then 35). FIRMWARE 0.4.1 OR LATER.
#    The device is off its network for about a minute, then restarts; the run
#    takes ~3 minutes. Do not start it without being able to reach the device.
cargo run --release -p screeny-probe -- --addr $D http --allow-wifi-trial --allow-reboot
```

What to expect on firmware 0.4.x, so that a SKIP is not read as a hole: rules 11 and 12
(`networks`) and 23 and 24 (`firmware`) skip themselves on the route's own
`503 unavailable` until cards 223 and 240; rule 29 will probably report the per-route
length bound as a known difference, because 0.4.x enforces only the global 384-byte one
(and `{"nothing":"..."}` parses as an empty settings request, so that route may answer
200 rather than any refusal at all). Rules 8, 14, 29 and 33 are the `KNOWN_223` four.

**Test counts.** Root `cargo test`: **657 passed, 0 failed** (exit 0). `crates/probe`
14 unit tests, 10 of them new; `crates/sim` 131, 5 of them new in
`tests/http_conformance.rs`. **The 64-rule UDP conformance suite is untouched**:
`git diff` against the merge base shows no change under `crates/probe/src/suite/`,
`crates/sim/tests/conformance.rs`, `crates/sim/src/core.rs` or `crates/sim/src/device.rs`,
and `tests/conformance.rs` still passes (36 s). Clippy on `crates/probe` and
`crates/sim`: no new warning, the two it prints are the pre-existing
`manual_is_multiple_of` ones in `vectors.rs` and `main.rs`.

**Proposed cards** (233, 234, 235 and 239 are already spoken for):

- **236 - what `status.wifi_state` means in the portal.** The question rule 8 raised and
  that I did not answer. In `Portal` both `GET /api/v1/wifi` and (now) `link_state()`
  report the provisioning machine's sticky byte, so a device sitting in the portal after
  a failed trial reads `wifi_state: failed` where "the link" argues for `disconnected`.
  `device-web.md` decides it, then the firmware, the simulator and rule 8 follow. Small,
  no hardware.
- **237 - the HTTP suite as the "HTTP costs no frame" gate.** Decision 7 is the standing
  rule and card 222 proved it once, by hand, with a shell loop and an eye on telemetry.
  The suite already holds both halves of the device: a mode that samples UDP telemetry
  across the whole run and reports the `frames_rx`/`frames_shown` deltas, the decode and
  stale counters and `render_us_max` against an idle baseline would make that a rule
  that runs after every flash instead of a thing somebody remembers to do. Hardware.
- **238 - `GET /` with the headers a real browser sends.** Card 222 flagged it and
  nobody has measured it: `HTTP_BUF` is 1536 bytes and a desktop Chrome request head is
  larger. This suite sends a minimal head, so it cannot see the problem. A rule that
  replays a realistic ~1.3 KB header block, and one deliberately over the buffer, would
  answer "does a real browser get the page" without a browser. Small, no hardware.
