---
id: 235
title: Simulator - a captive probe gets a 200 page like the firmware, not a 302
type: build
hardware: no
depends: [223]
owner: worker-235
branch: card/235-sim-captive-200
---

## Goal

One implementation of each thing: since fw 0.5.1 the device answers a captive probe on the
setup network with **the setup page itself, `200`, `Cache-Control: no-store`**, never a
redirect (card 223's Log, "The phone test", finding 3: iOS does not retry the connection a
`302` makes it open, and smoltcp has no backlog). `crates/sim` still answers a foreign
`Host` in the portal with a `302` + `Location`. Make the simulator say what the device says.

## Context

- Firmware: `firmware/src/http.rs`, `route_request` step 1 and `Reply::Portal`.
- Simulator: `crates/sim/src/api.rs` (`dispatch`, `REDIRECT_BODY`, `Response::redirect` in
  `crates/sim/src/http.rs`), tests in `crates/sim/tests/http_routes.rs`
  (`in_the_portal_a_foreign_host_gets_a_redirect_with_a_body` and its neighbours).
- The simulator keys the catch-all on the `Host` header (it has one listener); the firmware
  keys it on the listener. **Keep that difference** - it is the simulator's model of "on
  the setup network" - and change only the answer.
- **Out of scope (owner's decision 10 in `docs/design/device-web.md`)**: the simulator does
  not need to serve the firmware's real setup form, and nothing moves into
  `crates/provision`. A small honest HTML body that says it is the setup page stands in.

## Steps

1. In the portal (`Portal` | `Trial`), a foreign host gets `200`, `text/html`, a non-empty
   body (Android calls `Content-Length <= 4` a failure), `Cache-Control: no-store`, and
   **no `Location`**. Off the portal it stays `404 not_found`.
2. Remove `Response::redirect` and the 302 plumbing if nothing else uses them.
3. Rewrite the tests to assert the new contract, including "no `Location` header".
4. Check `crates/probe`'s HTTP rules and `crates/studio` for anything that relied on the
   302 (expected: nothing). Do not edit `crates/studio`; report instead.
5. Update the one sentence in `docs/design/device-web.md` / `crates/sim` README that
   describes the captive answer, if there is one.

## Exit

`timeout 900 cargo test -p screeny-sim -p screeny-probe` green; the diff is small.

## Rules

Branch `card/235-sim-captive-200` in your own worktree; commit and Log as you go; do not
merge or push; no hardware, no LAN; bounded commands; dummies `Example-Wifi1` /
`password9` only; leave `crates/art` and `crates/studio` alone.

## Log
