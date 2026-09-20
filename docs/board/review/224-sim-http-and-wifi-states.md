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

### 2026-09-20, worker-224: reading, and the shape of the thing

Read `CLAUDE.md`, `docs/README.md`, this card, `docs/design/device-web.md` ("The HTTP
API" and the build order), `docs/research/007-device-web-and-portal.md` sections 4.3/4.4
and 5.2, all of `crates/device-api/src/`, `crates/provision/src/{lib,machine,screen}.rs`,
all of `crates/sim/src/`, and `crates/receiver/src/lib.rs`'s `Host` trait and public
methods. Branch created off `main` at 2e04a4a (the worktree was based on b94ddcd, so a
`git merge main` came first, as the prompt said it would).

Decisions made before writing anything:

- **A hand-rolled HTTP/1.1 server over `TcpListener`, no crate.** `tiny_http` is not
  vendored under `~/.cargo/registry/src/`, so taking it means a network fetch and four
  new transitive crates (`ascii`, `chunked_transfer`, `httpdate`, `log`) for a server
  that needs GET, POST, `Content-Length`, one streamed body and a `Host` rule. The
  simulator is `crates/studio`'s dev-dependency, so its dependency list is everybody's.
  Hand-rolling also keeps the simulator honest about the firmware's constraints: no
  chunked replies, an explicit `Content-Length` on everything, `Connection: close`.
  New dependencies are therefore only `screeny-device-api` (`std` feature, for
  `std::error::Error`), `screeny-provision`, and `serde_json` (already in the workspace
  via `crates/studio`; `crates/device-api`'s golden tests prove it writes the same bytes
  as the firmware's `serde-json-core`).
- **HTTP mutations go through the *UDP* control path.** `POST /api/v1/settings`,
  `/identify` and `/reboot` build a `screeny_proto::control::Request`, write it with
  `Request::write` and feed it to the same `Core::control` a datagram would reach; the
  reply datagram is decoded for its error code and then dropped instead of being sent.
  So brightness clamping, name truncation and `REBOOT`'s magic word cannot differ between
  the browser and a UDP sender, because they are the same code. Card 222 should share
  its apply-path the same way rather than writing a second one.
- **`GET_WIFI`, the telemetry state byte, `GET /api/v1/wifi` and the panel all read one
  `screeny_provision::Provisioner`**, held inside `Core` under the existing
  `Mutex<Core>`, so they cannot disagree and no new lock order appears.

### 2026-09-20, worker-224: what was built

New files in `crates/sim/src/`: `http.rs` (the transport, ~350 lines), `api.rs` (the
routes), `wifi.rs` (the scripted radio around `screeny_provision`). New suites:
`tests/http_routes.rs` (18), `tests/http_wifi.rs` (14), and `tests/common/http.rs`, a
small HTTP client written from RFC 9112 rather than sharing the server's parser - a test
written with the server's own parser proves only that the simulator agrees with itself.

**CLI** (all additive; every existing flag unchanged):
`--http-port N` (0 = ephemeral; the binary defaults to 8080), `--no-http`,
`--wifi-result ok|fail|slow`, `--wifi-join-ms MS`, `--wifi-ssid SSID`, `--ap-ssid SSID`,
`--start-in-portal`, `--link-down`. The binary prints the HTTP address on a **second**
banner line, because `tests/cli.rs` has read the UDP addresses off the first one since
card 006.

**`Config`** gains `http`, `http_port`, `wifi_ssid`, `ap_ssid`, `wifi_outcome`,
`wifi_join_ms`, `wifi_timing`, `start_in_portal`; `Config::for_test()` sets
`http_port: 0`. Everything in the workspace builds its config with `..Config::for_test()`
or `..Config::default()`, so nothing outside needed touching.

**`SimHandle`** gains `http_addr`, `http_url`, `boot_id`, `set_wifi_outcome`,
`set_link_down`, `post_wifi`, `wifi_wipe`, `set_ap_client`, `wifi_phase`, `stored_ssid`,
`wifi_commits`, `wifi_reply`. `Snapshot` gains `wifi_phase`, `wifi_state`, `ssid`,
`portal`. `SimDevice` gains `http_addr`.

`tests/cli.rs` was changed in one place: the harness passes `--http-port 0`, for exactly
the reason it already passes `--frame-port 0 --control-port 0` - several of those tests
spawn a binary at once and would fight over 8080. No other existing test was touched.

### Why a hand-rolled server and not `tiny_http`

`tiny_http` is not vendored under `~/.cargo/registry/src/`, so it would be a network
fetch plus `ascii`, `chunked_transfer`, `httpdate` and `log`. `crates/sim` is
`crates/studio`'s dev-dependency: its dependency list is everyone's. The API needs GET,
POST, a `Content-Length` body, one streamed body, a `Host` header and an exact status
code, which is ~350 lines. It also keeps the simulator honest about what the firmware
can afford: no keep-alive, no chunked request bodies, an explicit `Content-Length` on
every reply. Net new dependencies: `screeny-device-api` (feature `std`),
`screeny-provision`, `serde` (for one `Serialize` bound) and `serde_json` - the last two
already in the workspace via `crates/studio`, and `crates/device-api`'s golden tests
prove `serde_json` writes the same bytes the firmware's `serde-json-core` does.

### Every HTTP mutation goes through the UDP control path

`POST /api/v1/settings`, `/identify` and `/reboot` build a
`screeny_proto::control::Request`, write it with `Request::write`, and feed it to the
same `Core::control` a datagram reaches; the reply datagram is decoded for its error code
and then **dropped instead of sent**. Brightness clamping, name truncation and `REBOOT`'s
magic word are therefore one implementation, not two that agree today.
`settings_are_clamped_by_the_same_code_udp_clamps_with` is the proof. Card 222 should do
the same rather than writing a second apply-path next to the UDP one.

### Feedback on `crates/provision` - the one real gap

**`Event::CredentialsPosted` is honoured only in `Portal` and `Trial`.** In `Online`,
`Joining` and `Boot` the `match` falls through to `_ => {}` and the post is silently
dropped. That matters twice:

- Spec 8.2 says `SET_WIFI` works from any state ("reply before disconnecting, then
  disconnect and attempt to join the new network").
- Card 223's own scope includes the **LAN** settings page with a scan list and a trial
  join, which is a post that arrives while the device is `Online`.

There is no way to express it in the machine's current vocabulary: `ButtonWipe` reaches
`Portal` but clears the store on the way, which is exactly the thing a trial must never
do. I did **not** invent a transition. The simulator reports the truth: `POST
/api/v1/wifi` in `Online` answers `503 unavailable` with the state named in `detail`, and
UDP `SET_WIFI` keeps answering `Reply::SetWifi` exactly as it always has (card rule 4
forbids changing UDP behaviour) while the machine ignores it. The asymmetry is
deliberate and is the loudest way I could flag this. **Proposed card 232** below.

### Other friction, smaller

1. **`route::NETWORKS`'s "one scan per 10 s" is prose in a doc comment, with no
   constant.** The firmware and the simulator will each pick their own number and nobody
   will notice they differ. `crates/sim/src/api.rs` has `SCAN_MIN_INTERVAL_MS` as a local
   copy; it belongs in `crates/device-api::route`. **Proposed card 233.**
2. **`FirmwareReply::ok` means "the whole image arrived and passed every check", but
   there is no way to say which checks ran.** A simulator (and, before card 240, the
   firmware) can only do two of research 006's five. I answer `ok: true` for a body that
   starts with `0xE9` and fits the slot, and say plainly in the README that the other
   three are not claimed. A `checks_run` field, or a `FirmwareError::Unchecked`, would
   let a caller know. Not urgent; recorded.
3. **`route::Route` has `max_request_len` but no helper to look a row up.** Every server
   writes the same `ROUTES.iter().find(|r| r.path == p && r.method == m)`. A
   `route::find(path, method) -> Option<&Route>` would be three lines in the crate and is
   the kind of thing two implementations should not each get right.
4. **`ErrorCode` has no `Unavailable`-with-a-reason and no `NotFound` for a *host***, so
   the captive-portal catch-all's non-portal answer is a plain `not_found`. Fine, but
   worth knowing it is a deliberate reuse.
5. **`screeny_receiver::Host::wifi` returns `(&'static str, u8)`.** On the device the
   SSID lives in a `static`; here it lives in the provisioning machine and changes. The
   simulator interns the string (one leak per *distinct* SSID a run reports, which is one
   in any run that is not a provisioning test) in `core.rs::intern`. `crates/receiver` is
   out of scope for this card, so it is not fixed. **Proposed card 234.**
6. **`screeny_provision::machine::Timing` has no `..Default` escape hatch in practice** -
   every field must be given, since giving all of them makes `..Timing::SPEC` a clippy
   `needless_update`. Not a bug, just a paper cut when writing a compressed test fixture.

### Decisions a reviewer should check

- **`POST /api/v1/reboot` does not restart the simulator.** The card's sentence reads
  both ways ("restarts ... the way `REBOOT` already does"). UDP `REBOOT` has always been
  accepted, logged and not acted on, and actually resetting the uptime would change what
  the simulator does on UDP, which deliverable 4 forbids. So HTTP reboot goes through the
  same `REBOOT` path and does exactly what it does - plus the new `boot_id`, which the
  orchestrator asked for mid-card and which changes no UDP byte. **Proposed card 235** if
  a real simulated restart is wanted.
- **The boot join with `--wifi-result ok` completes at time zero**, so the default
  simulator is `CONNECTED` from its first instant exactly as before. A 200 ms window of
  `CONNECTING` at startup would have been a behaviour change nobody asked for. Every
  later join takes `--wifi-join-ms`.
- **`IDENTIFY` beats `PROVISIONING`** when both overlays are up: identify lasts seconds
  and somebody is standing there asking "which one is this?", while the portal can be up
  for hours.
- **`--wifi-result slow` means "the radio never answers"**, so the attempt runs into the
  machine's own `join_attempt_ms` and fails with `other`. That exercises the `Tick`-driven
  timeout, which nothing else did.
- The soft-AP side proper (DHCP, the DNS catch-all) is out of scope per the card, but the
  HTTP catch-all **rule** is implemented and tested against all five OS probes of research
  007 section 4.1. No probe domain is named in the code: the rule is about the shape of
  the `Host`, as WLED, Tasmota and tzapu all do it.

### What card 222 should copy, and what it should avoid

**Copy:**

- Route on `route::ROUTES` as *data*, and write the table-walking test first. Mine found
  nothing because it was written with the code, but it is what stops route 10 being
  forgotten.
- Answer the captive-portal catch-all **before** routing, not as a 404 fallback:
  Microsoft's own portal guidance is explicit that a portal must not redirect some
  requests and drop others.
- Send the redirect **with a body**. iOS will not pop the sheet without one and Android
  rewrites a `Content-Length <= 4` answer to *failed*, not *portal*.
- Feed `POST /api/v1/wifi` and `SET_WIFI` into one `Provisioner`, and read `GET_WIFI`,
  the telemetry byte and `GET /api/v1/wifi` out of it. Three readers, one machine.
- Reply to `POST /api/v1/wifi` **before** stepping the machine (spec 8.2). In the
  simulator that is structural: the scripted radio can only answer on a later tick.
- Enforce `Route::max_request_len` per route rather than trusting the transport's buffer.
  On the device they are the same number; keeping the check explicit means the error is
  `payload_too_large` with a sentence instead of a dropped connection.

**Avoid:**

- Do not let `GET /` and the API share a 404 path: a browser wants HTML, the Studio wants
  `{"error":...}`.
- Do not parse `POST /api/v1/wifi` with picoserve's `Form` extractor (device-web says so
  already; it really does reject a non-UTF-8 body, and the simulator hits that case in
  `a_body_over_the_routes_bound_is_payload_too_large`).
- Do not answer `{"result":"trying"}` when the machine did not start a trial. See the
  `crates/provision` gap above - whatever card 232 decides, the reply must match what
  actually happened.
- Do not report `ok: true` from the firmware route until the checks behind it exist.

### Proposed follow-up cards

- **232 - `screeny_provision`: credentials posted while `Online` or `Joining`.** Add the
  transition spec 8.2 and card 223's LAN settings page both need: `StopJoin`, enter
  `Trial` **without** `RaiseAp`, and on failure return to `Online` on the old credentials
  with the store untouched. Then `POST /api/v1/wifi` answers `trying` in every state and
  the simulator's 503 goes away. Host-only; `crates/provision` + `crates/sim`.
- **233 - `crates/device-api`: the scan rate limit as a constant.** `route::NETWORKS`
  says "one per 10 s" in prose. Add `route::SCAN_MIN_INTERVAL_MS`, use it in the
  simulator, and have card 222 use it too. Fifteen minutes of work; two implementations
  that cannot drift.
- **234 - `screeny_receiver::Host::wifi` should not demand `&'static str`.** An
  associated lifetime, or a `&mut dyn FnMut(&str, u8)` callback. The simulator leaks a
  string per distinct SSID to satisfy it today.
- **235 - the simulator restarts for real.** `POST /api/v1/reboot` and UDP `REBOOT`
  reset the uptime, the counters and the state machines, so a sender's reconnect logic
  can be developed against it. Needs a decision first: it changes what the simulator does
  on UDP, which is why card 224 did not do it.
- **236 - `screeny-sim` serves the card-222 HTML page.** When the page exists it should
  be one file both the firmware and the simulator serve, so the portal can be styled and
  clicked through with no hardware. Depends on 222.
- **237 - `crates/studio/tests/fleet.rs` is flaky, and it is not card 224's doing.**
  `a_typed_address_becomes_a_device_and_starts_playing` fails about two runs in three
  with `telemetry should be arriving: ... "telemetry":null` while `frames_sent` is 6 and
  the panel is `up`. Measured on **`main` at 628d819's parent as well as on this
  branch**, three runs each, same assertion and same rate: it is a pre-existing race
  between the first piggybacked `TELEMETRY` and the dashboard's first `/api/v1/status`,
  not a regression. Belongs to the software session; `crates/studio` is out of scope
  here.

### Results

- `cargo test -p screeny-sim`: **112 tests, all green**, up from **75** on `main`
  (measured both ways, same command) - 37 added: 18 in `tests/http_routes.rs`, 14 in
  `tests/http_wifi.rs`, 5 unit tests in `src/api.rs` and `src/http.rs`. That includes
  `tests/conformance.rs`'s single wire-level suite - the 64 rules - still green in 36 s.
  Every pre-existing suite is unchanged in meaning; the only edit to an existing test
  file is the `--http-port 0` in `tests/cli.rs`'s spawn arguments, explained above.
  (The README's old "97 of them" was already stale; it now says 112.)
- Root `cargo test`: run three times. Every suite green except card 237's flake above,
  which reproduces identically on `main` (measured, twice in three runs each way). The
  `crates/screeny` pacing tests passed every time.
- `cargo clippy -p screeny-sim --all-targets`: **before 0, after 0** warnings from
  `crates/sim`. (The workspace run reports one warning throughout, from
  `crates/probe/src/vectors.rs`, on `main` and on this branch alike.)
- Acceptance: `screeny-sim --headless --http-port 8080 --start-in-portal` answered
  `curl localhost:8080/api/v1/status` with `"portal":true`, `"state":"provisioning"`,
  `"ssid":null`; `-H 'Host: captive.apple.com'` got `302 http://192.168.4.1/`; and
  `POST /api/v1/wifi` got `{"result":"trying"}`. Run under `timeout` with
  `--exit-after 8`; `pgrep -fl screeny-sim` afterwards found nothing.
