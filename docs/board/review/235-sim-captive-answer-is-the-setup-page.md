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

### 2026-09-20, worker-235

The simulator now answers a captive probe the way fw 0.5.1 does. Five files, no new
concepts.

- `crates/sim/src/api.rs`: `REDIRECT_BODY` -> `PORTAL_PAGE` (it is the page now, not a
  link to one), and the portal arm of `dispatch`'s catch-all returns
  `Response::html(200, PORTAL_PAGE)`. The comment carries the reason from card 223's
  finding 3. `Cache-Control: no-store` needed no work: `write_response` puts it on every
  response this server writes, so the header the phone test wants was already there.
  The `Host`-keyed catch-all is untouched - it is the simulator's model of "on the setup
  network" and the card says to keep it - and off the portal it is still `404 not_found`.
- `crates/sim/src/http.rs`: `Response::redirect`, the `302 => "Found"` reason arm and the
  `a_redirect_always_carries_a_body` unit test are gone. Nothing else sent a 302;
  `reason()` still has the generic `s if s < 400 => "Redirect"` fallback, so a future
  redirect would not be nameless, it would just be unnamed.
- `crates/sim/tests/http_routes.rs`: `in_the_portal_a_foreign_host_gets_a_redirect_with_a_body`
  -> `..._gets_the_setup_page_not_a_redirect`, asserting `200`, `text/html`,
  `Cache-Control: no-store`, a body over 4 bytes, `Content-Length` = the body, and
  **`Location: None`**. Two neighbours needed strengthening rather than renaming: both
  answers are a `200` now, so `our_own_hosts_never_get_the_catch_all_even_in_the_portal`
  and `a_request_with_no_host_header_is_served_rather_than_caught` would have passed on
  the catch-all's own page. They now also assert `Content-Type: application/json`, which
  only the real route sends. (Without that, the status assertion proves nothing.)
- `crates/sim/README.md` and `docs/design/device-web.md`: the one sentence each that
  described the answer. In `device-web.md` the sentence is inside "What the research
  settled", so rather than rewrite what card 201's research said, the bullet now states
  the answer and says in one italic line that the phone test overruled the research.

Checked, as the card asked: **nothing relied on the 302.** `crates/probe`'s HTTP rules
never look at status 3xx or `Location`; the only portal-shaped rule there
(`rules.rs:1189`) is "the setup page is self-contained", which is about absolute URLs in
the body and is happier with a 200 than it was with a redirect. `crates/studio` does not
mention the captive path at all - its `307` in `src/ui.rs` is the `/dashboard` -> `/`
redirect from card 170, its own concern, not touched.

`timeout 900 cargo test -p screeny-sim -p screeny-probe`: exit 0, everything green
(`http_routes` 20 passed, `http_wifi` 17, `http_conformance` 5, sim lib 26, probe lib 14,
conformance 1 in 36 s). `cargo clippy -p screeny-sim -p screeny-probe --all-targets` and
`cargo check --workspace --all-targets` both say nothing.

One thing worth recording: on one of three runs `tests/telemetry.rs
a_sender_can_tell_network_loss_from_a_slow_device` failed, and passed on its own and on
a re-run of the whole suite. It is a timing test over loopback UDP and has nothing to do
with this card's code path (no HTTP in it), but it is a flake that exists.
