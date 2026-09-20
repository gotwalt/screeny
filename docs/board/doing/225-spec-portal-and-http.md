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

### Step 4 - pointers, and one clause

`README.md` and `docs/README.md` needed **no change**: neither quotes a section
title from the spec. `README.md:90` is a bare link to the file and
`docs/README.md` only describes the board. Section 8's title changed anyway,
and every `§8.x` number a code comment cites still means what it meant -
8.2, 8.3 and 8.4 kept their numbers deliberately, and 8.4 in particular,
because `crates/device-api`, `crates/provision` and `firmware/` all cite it as
the no-PSK invariant.

One clause added to 8.3 while re-reading: an online-origin trial takes **no**
`PROVISIONING` overlay (`machine.rs:432-440`), where a portal trial does.

### Code/spec disagreements, for the orchestrator

Not fixed here. The first two are "the spec is old" and were fixed; everything
below is listed rather than decided.

1. **A non-UTF-8 PSK is accepted and then silently fails.** §8.2's body says
   `psk` is UTF-8, and nothing enforces it: `Psk::new`
   (`crates/settings/src/value.rs`) takes any bytes, `form::parse_wifi_form`
   takes any bytes, and the pair is accepted with `{"result":"trying"}` or an
   empty `SET_WIFI` reply. It is refused three attempts later at the radio -
   `station_config` (`firmware/src/main.rs:532`,
   `core::str::from_utf8(w.psk.as_bytes()).ok()?`) - as `FailReason::Other`,
   logged as "not expressible to the radio"
   (`firmware/src/provision.rs:883-893`). So the caller is told "could not
   join" for what is really `out_of_range` / `ERR_BAD_ARG`. Either §8.2 should
   stop saying UTF-8 and the refusal move to the parser, or the parser should
   enforce it. I did not pick.
2. **The SSID is *not* UTF-8, and §8.2 says it is.** The same line of §8.2
   calls `ssid` UTF-8; every implementation treats it as bytes on purpose
   (`Ssid` is a byte vector, `form.rs` refuses picoserve's `Form` for exactly
   this reason, `text::ssid_text` reports a non-UTF-8 one as `null`, a scan
   result that cannot be named is dropped). Here the code is plainly right and
   the spec is plainly stale - but it is the same sentence as item 1, where the
   answer may go the other way, so both are listed together.
3. **`GET_WIFI` can read `FAILED` while the device is connected.** §6.3 defines
   `3 FAILED` as "the last join attempt failed", which reads like a property of
   the link; the machine makes it sticky after an online-origin trial until the
   next post, a wipe or a reboot, *even once the previous network is back*
   (`machine.rs:449-465`, `trial_failed_sticky`). §8.3 now states the sticky
   behaviour, so the spec is self-consistent, but §6.3's one-line gloss and
   §8.3's rule are two readings of the same byte and the orchestrator may want
   §6.3 to point at §8.3.
4. **`GET /api/v1/networks` may never exist on the device.** The route is in
   the shared table and `crates/sim` serves it for real; firmware 0.5.1 answers
   `unavailable` (`firmware/src/http.rs:1399`) and device-web decision 10
   **dropped card 229**, which was the scan. I wrote it into §8.6 as a route
   with §8.8's `unavailable` escape. If the device is never going to scan, the
   route should probably be marked as such in the spec and in
   `crates/device-api`. `firmware/src/http.rs:53-54` still says the scan is
   "also 223", which is now stale.
5. **The simulator's captive answer is a different mechanism, not just a
   different status.** The firmware decides from the **listener** (the AP
   dispatch, `firmware/src/http.rs:1327-1329`) and answers the setup page,
   `200`, `no-store`. `crates/sim` decides from the **`Host:` header**
   (`host_is_ours`, `crates/sim/src/api.rs:153-179`) and answers a `302` with a
   body (`api.rs:78-87`, used at `api.rs:261`). §8.9 documents the firmware's,
   which is what the phone test settled. Card 235 is changing the status code;
   whoever takes it should decide whether the *rule* becomes listener-based
   too, or the spec has to describe two.
6. **The setup form cannot carry a 64-byte PSK.** `MAX_PSK_LEN` is 64
   (`crates/proto/src/control.rs:56`), §8.2 says `0..=64`, and
   `form::parse_wifi_form` accepts 64 - but the portal page's input is
   `maxlength=63` (`firmware/src/http.rs:1056`). 63 is a WPA2 passphrase; 64 is
   the hex form of a PSK. A one-character firmware change if anybody cares;
   not the spec's problem, but it is a limit the spec states and the page does
   not honour.
7. **Code comments cite "spec section 8.3" for the `GET_WIFI` state byte**,
   which §6.3 defines: `crates/device-api/src/enums.rs:65`,
   `crates/sim/src/wifi.rs:173` and `:271`, `firmware/src/main.rs:500`,
   `firmware/src/provision.rs:180`. Harmless, and now half-true - §8.3 carries
   a state table - but a docs-only card cannot fix a comment.
8. **`crates/device-api/src/request.rs:135` cites "spec 6.9"** for `REBOOT`'s
   magic word; 6.9 is "How a sender uses telemetry" and the magic is §6.3.
   Same category as 7.
9. **§7.3's "frame handling continues underneath"** is true of an `IDENTIFY`
   overlay and of the counters under a portal screen, but while the portal
   screen is up a decoded frame **does not reach the panel**
   (`firmware/src/net.rs:216-224`): it is counted and kept for the cross-fade
   and dropped at the swap. §8.1's table now says so in as many words. §7.3
   itself is unchanged and could be read the other way.

Nothing in this card changes a byte on the wire, and no code was touched.
