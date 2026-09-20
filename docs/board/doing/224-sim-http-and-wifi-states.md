---
id: 224
title: screeny-sim - the device's HTTP API and the WiFi join/portal states, so nobody needs the panel to build against them
type: build
hardware: no
depends: [221, 226]
owner: worker-224
branch: card/224-sim-http-and-wifi-states
---

## Goal

The simulator is the project's second implementation of everything the device does on
the wire, and the thing every worker and the Studio develop against. The device is
getting an HTTP API and a WiFi join/portal life; give the simulator both, driven by the
same crates the firmware will use, so the firmware's HTTP cards (222, 223) have a
reference to be compared with and the Studio's device page can be built with no
hardware. This delivers parked card 081 as part of it.

## Context

Read first: `CLAUDE.md`; `docs/design/device-web.md` (all of it: decisions, the 201/221
summaries, "The HTTP API"); `crates/device-api/README.md`, `src/route.rs` and
`tests/golden/` (the shapes you serve - **use these types, define none of your own**);
`crates/provision/README.md` and `src/machine.rs` (the state machine you drive - **use
it, do not restate it**) and `src/screen.rs` (the portal screen renderer);
`docs/board/parked/081-sim-wifi-and-provisioning.md` (the deliverables you are
absorbing); `docs/research/007-device-web-and-portal.md` sections 4.3 (the captive-portal
answers) and 5.2; `crates/sim/src/{device,core,event,screens,config}.rs` and
`crates/sim/src/bin/screeny-sim.rs`; spec sections 6.3, 6.7, 7.3 and 8.

Decided:

- The simulator is std, threaded and has no async runtime: keep it that way. Serve HTTP
  with a small blocking server (`tiny_http` or a hand-rolled one over `TcpListener`; your
  call, justify it in the Log - the dependency must be light and must not pull an async
  runtime into `crates/sim`). Port: `--http-port` (default 0 = ephemeral in tests, 8080
  for the binary - not 80, no root), `Config` field for the library, and
  `SimHandle::http_addr()`.
- Every route in `screeny_device_api::route::ROUTES` is served, with the crate's types,
  error shape and status codes. A test walks `ROUTES` and fails if one is not covered.
  `POST /api/v1/firmware` accepts and discards the stream, runs the *header-level* checks
  it can without flash (ESP image magic `0xE9`, length bounds) and reports
  `written`; it never pretends to install anything. `POST /api/v1/reboot` restarts the
  simulated device's uptime and state the way `REBOOT` already does.
- The HTML page is **not** this card: serve a one-line placeholder at `GET /` that says
  so and links the API routes. (The real page is card 222's and will be shared later.)
- WiFi is scripted, per card 081: a join outcome the test or CLI chooses
  (`--wifi-result ok|fail|slow`, `SimHandle::set_wifi_outcome(..)`), a link the test can
  take down (`SimHandle::set_link_down(bool)`), driven through
  `screeny_provision::Provisioner` with simulated `Joined` / `JoinFailed` events on a
  timer. Both `SET_WIFI` (UDP, spec 8.2: reply first) and `POST /api/v1/wifi` feed the
  same path. `GET_WIFI`, the telemetry `state` byte (`PROVISIONING` overlay) and
  `GET /api/v1/wifi` all read the machine, so they cannot disagree. Section 7.3's "link
  down -> HOLD, then the idle screen says the network is down" happens.
- In `Portal`/`Trial` the simulated panel shows `screeny_provision::screen::render`'s
  output (the QR screen), so the sim's window and `--dump-dir` PNGs show what the device
  will show. A `--start-in-portal` flag (empty store, no built-ins) boots straight there.
- The PSK invariant (spec 8.4): `Event::SetWifi` has no PSK field and must not grow one;
  the HTTP path must not log, store in an event, or return one either. Test it.
- The "soft-AP side" (DHCP, DNS catch-all, the 302-with-a-body answers for captive-portal
  probes) cannot be simulated honestly on a host and is **out of scope**; but the HTTP
  server's *catch-all rule* can be: a request whose `Host` is not the device's own gets
  the 302-with-non-empty-body answer of 007 section 4.3 when the machine is in
  `Portal`/`Trial`, and a 404 otherwise. Implement and test that.

## Deliverables

1. The HTTP server in `crates/sim` as above, started by `SimDevice::start` when the
   config asks for it, shut down cleanly by `shutdown()` (no thread left behind - the
   existing tests check for that style of hygiene; follow them).
2. The scripted WiFi model through `screeny-provision`, with the `SimHandle` controls
   and CLI flags above, and the "network is down" idle screen in `crates/sim/src/screens.rs`.
3. Tests (in-process, ephemeral ports, each bounded by a timeout): every route's happy
   path parsed with `screeny-device-api` types; the error shape for a bad body, an
   oversize body (`MAX_REQUEST_LEN`), a wrong method; a failed join end to end over HTTP
   (`POST wifi` -> `trying` -> `GET wifi` says `auth` -> stored credentials untouched)
   and over UDP `SET_WIFI`; a successful trial join; link down -> `HOLD` -> idle; the
   portal screen is what the panel shows in `Portal` (compare the frame with
   `screeny_provision`'s render); the catch-all redirect; no PSK in any event, log line
   or reply.
4. The existing suites stay green untouched in meaning: `cargo test -p screeny-sim`
   (including `tests/conformance.rs`, 64 rules) - the default configuration of the
   simulator (WiFi `ok`, HTTP on) must behave on UDP exactly as it does today.
5. `crates/sim/README.md` (or the crate docs, wherever the CLI is documented today): the
   new flags and a `curl` example per route.

## Out of scope

`firmware/`, `crates/proto`, `crates/receiver`, `crates/device-api`, `crates/provision`,
`crates/settings`, `crates/studio`, `crates/probe` (an HTTP conformance subcommand for
`screeny-probe` is the next card, 228 - do not start it), the spec. If one of the crates
you depend on is missing something, wrap it locally and say so in the Log.

## Acceptance

`cargo test -p screeny-sim` green; root `cargo test` green (the pacing tests in
`crates/screeny` are timing-sensitive under load: re-run once before believing a failure
there); `cargo clippy -p screeny-sim --all-targets` no worse than before your change
(record the before/after warning counts); `screeny-sim --headless --http-port 8080
--start-in-portal` answers `curl localhost:8080/api/v1/status` with `"portal":true`
(run it with a timeout and kill it; leave nothing running).

## Log
