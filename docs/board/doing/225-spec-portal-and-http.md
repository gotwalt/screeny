---
id: 225
title: Spec - strike 8.1 (serial console), rewrite 8.3 to end at the portal, add the HTTP API section
type: docs
hardware: no
depends: [223, 226, 232]
owner: worker-225
branch: card/225-spec-portal-http
---

## Goal

`docs/design/protocol-v1.md` is normative and is behind the device. Bring sections 8.x in
line with what fw 0.5.1 does, and give the HTTP API a normative home. **Describe what
exists; design nothing new.** If code and spec disagree in a way that is not simply "the
spec is old", do not pick a side: list it in the Log for the orchestrator.

## Context (read first)

- `docs/design/device-web.md`: decisions 5, 6 and 10, "What the research settled", the
  card 223 paragraphs on `wifi_state` vs the sticky trial result.
- `docs/board/done/223-firmware-portal.md`, last section (the phone test): the captive
  answer is the setup page as a `200`, not a `302`; no DHCP option 114; both HTTP workers
  follow the soft-AP; the QR screen does not alternate; the `connected` screen yields to a
  stream.
- The shapes are already code: `crates/device-api` (routes, JSON bodies, error codes,
  golden files), `crates/provision` (states, `Timing::SPEC`), `crates/settings`.
  `crates/probe/src/http.rs` has the 38 rules the device is held to - the spec section
  should be the prose those rules are checking, and should cite rule numbers where useful.

## Steps

1. **8.1**: strike the serial console; say what superseded it (the portal, the settings
   page, `SET_WIFI`).
2. **8.3**: rewrite the join sequence from `crates/provision`'s machine: stored
   credentials -> (the `bench-wifi` build's compiled-in pair, test builds only) -> the
   portal; the trial join before commit; credentials stored only after they join; the
   online-origin trial that falls back to the stored network with a sticky `FAILED`; the
   30 s AP grace window; the 10-minute portal retry when credentials exist and nobody is
   on the AP; the 60 s link-down rule. Constants from `Timing::SPEC`, by name.
3. **New section, the HTTP API**: transport (port 80, no TLS, no auth - decision 3,
   `Connection: close`), the route table, request/response bodies and the error shape from
   `crates/device-api`, limits (body sizes, rate limits that exist), what is served on the
   setup network vs the LAN (`/` and `/setup`, the captive catch-all), and that a firmware
   upload route is reserved for cards 240/241 without specifying it yet.
4. **8.4**: check the security posture text still matches (open setup AP, PSK in clear
   during setup, unauthenticated LAN).
5. Cross-check every claim against the code, not against older docs. Keep the spec's
   existing voice, numbering style and line width.

## Exit

The three edits above in `docs/design/protocol-v1.md`; a Log list of every code/spec
disagreement found; `README.md` / `docs/README.md` pointers updated if section titles
changed. No code changes.

## Rules

Branch `card/225-spec-portal-http` in your own worktree; commit and Log as you go; do not
merge or push; no hardware, no LAN; bounded commands; never write a real SSID or password
(examples use `Example-Wifi1` / `password9`); touch nothing outside `docs/` and the card.

## Log

### 2026-09-20 - what I read before editing

`crates/device-api` (all of `route.rs`, `error.rs`, `reply.rs`, `request.rs`,
`form.rs`, `text.rs`, README), `crates/provision` (`machine.rs` in full,
`lib.rs`, `screen.rs` head), `crates/settings/src/lib.rs`,
`firmware/src/http.rs` (all 2,250 lines), `firmware/src/provision.rs`,
`firmware/src/net.rs` (the control task and the screen composition),
`firmware/src/receiver.rs` (`set_wifi`), `firmware/src/store.rs`
(`Immediate`/`commit_immediate`), `firmware/src/mdns.rs` head, the rule names
and numbering of `crates/probe/src/http/rules.rs`, and `crates/sim/src/api.rs`
around the captive answer. Every claim in the edits below is from those files.

### Step 1 - 8.1, 8.2, 8.3, 8.4

- Section 8's title is now "Runtime provisioning and the device's HTTP API" and
  its opening says three paths, one store (`crates/settings`).
- **8.1** is the setup portal instead of the serial console: the soft-AP's name
  and open authentication, 192.168.4.1/24, the DHCP pool and lease, no option
  114, the DNS catch-all, the panel screen and the `PROVISIONING` overlay,
  `GET /setup` and `GET /` on the AP, why the answer is HTML and why there is
  no file input. A closing paragraph strikes the console by name (`wifi set`,
  `wifi get`, `wifi clear`, `info`, `stats`, `reboot`, Improv) and says what
  replaced each of them. There is no line reader anywhere in `firmware/src`.
- **8.2** keeps its body and its three steps; step 1 now points at 8.3 for the
  attempt count, the auth-failure exception and the fallback, and says that
  `POST /api/v1/wifi` and `POST /setup` enter the same trial with `persist`
  implied set (neither carries such a bit; `firmware/src/http.rs:756,1151`).
- **8.3** is rewritten from `crates/provision/src/machine.rs`: boot order
  (stored -> compile-time -> portal), `join_attempts` 3 x `join_attempt_ms`
  15 s, "a join is not a join until there is an address", the trial rules
  (nothing stored before it joins, `trial_attempts` 3 with no retry on an auth
  failure, portal-origin vs online-origin, the sticky `FAILED`, a second post
  cancels the first), commit + `ap_grace_ms` 30 s + `connected_screen_ms` 60 s
  and its yielding to a stream, the 10-minute `portal_retry_ms` gated on
  `ap_clients == 0` that does not reset when suppressed, `link_down_ms` 60 s,
  the button wipe (defined, not yet reachable in 0.5.1), and a table mapping
  the machine's states onto §6.3's `GET_WIFI` byte.
- **8.4** gains the HTTP posture (decision 3: every route unauthenticated,
  upload included; `pin`/`counter` parsed and ignored; `check_auth`;
  `unauthorized` reserved) and the open setup AP (decision 2). The no-PSK
  invariant now says "never carried by any HTTP reply" and "never written to a
  log line" instead of "printed by the serial console", and names the three
  places that enforce it.

### Step 2 - the HTTP API, sections 8.5-8.9

**Where it went, and why not section 9.** A new top-level section would have
renumbered 9, 10 and 11, and "spec 9.1", "spec 9.2", "spec 9.4" are cited from
about twenty places in `crates/screeny` (the pacer, the socket notes, the
discovery sequence) - which a docs-only card cannot fix. So the API is
**8.5 transport, 8.6 routes, 8.7 bodies and errors, 8.8 limits and methods,
8.9 the pages and the captive catch-all**, at the same `###` level as the rest
of section 8, and section 8's title now names it. §8.4 keeps its number, which
matters: "spec 8.4" is cited from `crates/device-api`, `crates/provision` and
`firmware/` as the no-PSK invariant.

What is in them, all checked against code:

- 8.5: TCP 80, no TLS, HTTP/1.1 with `Connection: close`, keep-alive off and
  why, at least two workers and why (no smoltcp backlog; iOS does not retry a
  refused SYN), the 3/5/5 s timeouts, `_http._tcp` on port 80, that every
  worker follows the soft-AP so the LAN has no HTTP while the AP is up, and
  that head + body must fit the request buffer (1,536) or be answered `413`.
- 8.6: the table (the seven JSON routes, `POST /api/v1/firmware` reserved, plus
  `/` and `/setup`), with each route's request bound, and a paragraph per route
  for the semantics that are not in the shape: `status.wifi_state` is the link,
  `boot_id`, telemetry's raw bytes, the scan cap and rate limit, `GET wifi`'s
  trial precedence and `reason`, reply-before-radio, `settings` returning the
  whole state and its `name: ""` rule, `"RBOO"`, `duration_ms` bounded by the
  `u16` on the wire. Ends with the note that `settings` and `identify` are
  carried out by building the §6.3 control request with `req_id` 0.
- 8.7: why two routes are urlencoded and not JSON, the form rules (`+`, `%XX`,
  unknown keys ignored, duplicate key refused, missing `psk` = open, empty
  `ssid` refused), SSID bytes-in/text-out, the one error shape and the
  status-per-code table, and §6.5's codes mapped onto it.
- 8.8: per-route bounds enforced on the route (not only the global 384), reply
  bounds are documentation because replies are streamed (with the six numbers),
  `unavailable` for a route a build cannot serve, 404/405 including a verb the
  API has no method for and `HEAD /`, and decoded-and-exact path matching.
- 8.9: `GET /` is the status page on the LAN and the setup page on the AP,
  `/setup` answers on both, and the catch-all - AP listener only, while the AP
  is up, `200` + the setup page + `no-store`, never a `302`, a property of the
  listener and not of `Host:` - with the iOS reason from card 223.
- Section 11 gains a "Closed by card 225" block, items 31-34, noting that
  nothing here changes a byte on the wire.

Probe rule numbers cited in the text: 8, 10, 11, 12, 14, 22, 25, 26, 27, 28,
29, 30, 34, 36, 37, 38 - each beside the sentence it pins.

### Step 3 - card 132's paragraph (orchestrator's addition, same file)

Asked for mid-card and done in its own commit; **card 132 itself is untouched**
and stays in `backlog/` for the orchestrator to close.

- Section 1 gains a paragraph after the 1472-byte-buffer rule: neither that
  rule nor §2.3's `len` ceiling is observable over WiFi, because an oversize
  payload is fragmented and the device does not reassemble, so the stack drops
  it and nothing counts it - `frames_rejected` unmoved is also what a violation
  looks like. It names the suite's behaviour (`LOOPBACK_ONLY`, the
  `SKIP  loopback only: the radio fragments it away` line) and says the
  simulator keeps the real assertions.
- Section 2.3 gains the distinction the probe's code actually draws
  (`crates/probe/src/suite/framing.rs:73-85, 222-235`): a `len` of 1465 that
  the datagram *backs up* needs a 1473-byte datagram and is loopback-only,
  while a `len` the datagram does not back up is the `8 + len <=
  datagram_length` check and is observable anywhere. Card 132's own wording
  did not separate the two; the two probe rules do, so the spec follows the
  code.
