---
id: 232
title: Credentials posted while Online or Joining - the state machine handles them; small API-crate refinements from the first consumer
type: build
hardware: no
depends: [221, 224, 226]
owner: worker-232
branch: card/232-provision-post-while-online
---

## Goal

Card 224 (the simulator) was the first real consumer of `crates/provision` and
`crates/device-api` and found one real gap and a few paper cuts. Close them before the
firmware's portal card (223) builds on the same crates.

## Context

Read first: `CLAUDE.md`; `docs/design/device-web.md`; the "Crate feedback" section of
`docs/board/done/224-sim-http-and-wifi-states.md`; `crates/provision/src/machine.rs`,
its README and `tests/transitions.rs`; `crates/device-api/src/route.rs`;
`crates/sim/src/api.rs` and `src/http.rs` (the consumer you will simplify); spec section
8.2; `firmware/src/main.rs`'s `SET_WIFI` handling (card 212: what the device really does
today when new credentials arrive while it is online).

**The gap.** `Event::CredentialsPosted` is honoured only in `Portal` and `Trial`; in
`Online`, `Joining` and `Boot` it falls through and is dropped. But spec 8.2 says
`SET_WIFI` works from any state, and card 223's LAN settings page posts credentials
while `Online`. The simulator currently answers `503 unavailable` over HTTP for that
case, deliberately and loudly.

**Decided semantics** (they match what firmware 0.3.0 does for `SET_WIFI`):

- `Online` + `CredentialsPosted` -> `Trial`, **without raising the AP** (the device has
  one station: it must drop the current association to try the new network, and there is
  no phone on a setup network to keep informed). Actions: `StopJoin` if needed, then
  `StartJoin { which: Trial, attempt: 1 }`. Same attempt rules as a portal trial (an
  authentication error fails at once; other reasons retry up to 3).
- That trial succeeding: `CommitCredentials { Trial }`, `Announce`, `Online`. No AP was
  up, so no 30 s grace and no `DropAp`.
- That trial failing: **back to the previous network, not to the portal**:
  `StartJoin { which: Stored, .. }` and state `Joining`, with the failed trial's outcome
  kept for `trial()` / `GET /api/v1/wifi` and `wifi_state()` reading `FAILED` until the
  next post or reboot (sticky, as firmware 0.3.0 does - otherwise the old network
  reconnecting a few seconds later reads as "your new network worked"). If the stored
  credentials then also fail 3 times, the ordinary `Joining` -> `Portal` rule applies.
- `Joining` + `CredentialsPosted`: same as `Online` (cancel the current attempt first).
  `Boot` + `CredentialsPosted`: cannot happen before `Event::Boot` is processed; keep
  ignoring it, and say so in a comment and a test.
- A trial that began from `Portal` behaves exactly as today. The machine must remember
  where a trial came from; add that to `Trial` state or a field, your call.
- The telemetry overlay: `PROVISIONING` is shown for portal-origin trials (as today);
  for an online-origin trial do **not** set the overlay (frames may still be arriving
  around the rejoin and nothing about the panel is "in setup").

**The paper cuts** (all in `crates/device-api`, additive only):

- `route::SCAN_MIN_INTERVAL_MS: u32 = 10_000` with the doc comment's "one scan per 10 s"
  pointing at it; and a tiny `no_std`, wrapping-safe `RateLimit { .. }` taking `now_ms`
  (a refused request does **not** reset the timer - same answer card 221 gave for the
  portal retry), with tests including the u32 wrap.
- `route::find(path, method) -> Option<&'static Route>`, plus a way to tell "path known,
  wrong method" (405) from "unknown path" (404).
- `FirmwareReply`: leave the shape alone; in the docs say `ok: true` means *every check
  the implementation claims* passed and that a build which cannot run all five checks
  must answer with the crate's not-implemented/unavailable error instead. Do not add
  fields.

Then make `crates/sim` use all of it: the 503 for a post while `Online` goes away (it
now runs a real online-origin trial through the machine, over HTTP **and** its `wifi_*`
handle methods; UDP `SET_WIFI` behaviour on the wire stays byte-for-byte what it is -
the 64-rule conformance test is the gate), the local `SCAN_MIN_INTERVAL_MS` and the
hand-written route lookup are replaced by the crate's.

## Deliverables

1. `crates/provision`: the transitions above, README diagram/table updated, new rows in
   `tests/transitions.rs` (every new transition; sticky `FAILED`; stored credentials
   untouched by a failed online-origin trial; a second post during an online-origin
   trial; `ButtonWipe` during one; the u32 time wrap across one).
2. `crates/device-api`: the constant, `RateLimit`, `route::find`, the doc note; tests.
3. `crates/sim`: uses them; its HTTP test for "post while online" now asserts the trial
   and the fallback instead of the 503.

## Out of scope

`firmware/` (a hardware worker, card 222, owns it right now), `crates/proto`,
`crates/receiver`, `crates/settings`, `crates/probe`, `crates/studio`, the spec,
`docs/design/*`. `Host::wifi`'s `&'static str` (card 224's proposal 234) is shared
surface and is not yours.

## Acceptance

`cargo test -p screeny-provision -p screeny-device-api -p screeny-sim` green, including
the simulator's 64-rule conformance test unchanged; clippy clean on the three crates;
both `no_std` crates still check for `thumbv7em-none-eabi`; root `cargo test` green
apart from the known-flaky `crates/studio/tests/fleet.rs`
`a_typed_address_becomes_a_device_and_starts_playing` (the software session's; if it
fails, re-run once and report, do not touch it).

## Log

### 2026-09-20 - `crates/provision`: the transitions

Merged `main` into the worktree first (the branch was based on 597c702, well before
card 224 landed) so `crates/provision`, `crates/device-api` and the simulator's HTTP
server were all present.

**The origin lives on `Trial`.** The card left the choice open; `Trial::origin:
TrialOrigin` won over a private field on `Provisioner` because it is the same lifetime
as the thing it describes (a trial), it survives into `trial()` for free, and there is
then exactly one place that knows where a trial came from. `Provisioner::trial_origin()`
reads it back. `TrialOrigin::{Portal, Online}`; `Portal` is everything that happens
today.

New transitions:

| from | event | to | actions |
|---|---|---|---|
| `Online` | `CredentialsPosted` | `Trial` (origin `Online`) | `StartJoin { Trial, 1 }` |
| `Joining` | `CredentialsPosted` | `Trial` (origin `Online`) | `StopJoin`, `StartJoin { Trial, 1 }` |
| `Trial` (origin `Online`) | `Joined` | `Online` | `CommitCredentials { Trial }`, `Announce` |
| `Trial` (origin `Online`) | `JoinFailed`/timeout, attempts left and not `AuthError` | `Trial` | `StartJoin { Trial, n }` |
| `Trial` (origin `Online`) | `JoinFailed`/timeout, no attempts left, `has_stored` | `Joining` | `StartJoin { Stored, 1 }` |
| `Trial` (origin `Online`) | ditto, no store but `has_builtin` | `Joining` | `StartJoin { Builtin, 1 }` |
| `Trial` (origin `Online`) | ditto, neither | `Portal` | `RaiseAp` |
| `Trial` (origin `Online`) | `CredentialsPosted` | `Trial` (origin kept) | `StopJoin`, `StartJoin { Trial, 1 }` |
| `Boot` | `CredentialsPosted` | `Boot` | none (ignored, with a comment and a test) |

No `RaiseAp` anywhere on that path, and no `DropAp` either: `go_online` only arms the
grace window when the AP is actually up, which on this path it is not.

**Cases the card left open, and what I chose.**

1. *Does an `Online`-origin trial that succeeds put the new address on the panel?*
   **No.** `connected_since` is set only for a portal trial. The person who posted is
   reading the address in their own HTTP reply; the panel belongs to the stream. This is
   the same argument the card makes for not setting the `PROVISIONING` overlay.
2. *What does `screen()` return during an `Online`-origin trial?* **`None`.** The portal
   layout points at an AP that was never raised, so drawing it would be a lie.
3. *What happens to `ip()` when the station leaves the old network for the trial?*
   **Cleared**, along with `connected_since` and `link_down_since`. We hold no address
   during the trial, and a stale one in `/api/v1/status` would be wrong.
4. *A failed `Online`-origin trial with an empty store* (only reachable by posting while
   `Joining` on a build with compile-time credentials): falls back to `Builtin`, and to
   the portal if there is nothing at all.
5. *What clears the sticky `FAILED`?* A new `CredentialsPosted`, `ButtonWipe`, or a
   reboot. **Not** `go_online`, which is the entire point - the old network coming back
   must not read as success. `portal_after_failure` is untouched and still behaves as it
   did.
6. *A post that arrives while the soft-AP is in its 30 s post-trial grace window*: still
   an `Online`-origin post. The state decides, not the radio. The grace timer is only read
   in `Online`, so it pauses for the trial and restarts from the new `Joined`.
7. *A second post during a trial* keeps the origin of the trial it replaces: it arrived
   down the same channel.
8. *Reaching the portal never clears the store* on any of these paths (asserted).

New public items: `TrialOrigin`, `Trial::origin`, `Provisioner::trial_origin()`,
`Provisioner::trial_is_current()`. `trial_is_current()` exists so the simulator and the
firmware do not each re-derive "is `trial()` still the answer `GET /api/v1/wifi` should
give" - it is true while `Trial`/`Portal` and also for a sticky failure.

`cargo test -p screeny-provision`: **47 + 12 + 1 doc = 60**, up from 32 + 12 + 1. Clippy
clean, `cargo check --target thumbv7em-none-eabi` clean, and
`the_firmware_knows_exactly_what_this_crate_costs_it` still passes unchanged:
`Provisioner` is still <= 176 bytes (the `bool` and the origin byte land in padding).

### 2026-09-20 - `crates/device-api`: the three paper cuts, additive only

New public items, all additions - nothing existing renamed, removed or changed in
meaning, and no golden file touched (`git diff --stat crates/device-api/tests/` is
empty):

| item | what |
|---|---|
| `route::SCAN_MIN_INTERVAL_MS: u32 = 10_000` | the number `NETWORKS`'s doc comment used to spell out in prose; the comment now links to it |
| `route::RateLimit` | `new(interval_ms)`, `allow(now_ms) -> bool`, `retry_after_ms(now_ms)`, `interval_ms()`, `reset()` |
| `route::find(path, method) -> Option<&'static Route>` | the `ROUTES.iter().find(..)` every server was about to write |
| `route::path_is_known(path) -> bool` | the 405-vs-404 half of the pair |
| `RateLimit` re-exported at the crate root | next to `Method`, `Route`, `ROUTES` |

`RateLimit` holds `last: Option<u32>` rather than a sentinel, so a device whose clock
really is at 0 ms is not a special case (the simulator's local copy used `last != 0` and
would have scanned twice in the first 10 s of a run that started at exactly 0). Every
comparison is `wrapping_sub`, and a refused call leaves `last` alone - card 221's answer
for the portal retry, applied here: a page that polls every second must not be able to
hold the window shut forever.

`FirmwareReply` keeps its shape exactly; only the docs changed. They now say `ok: true`
means every check *that build runs at all* ran and passed, and that a build which cannot
run all five of research 006's checks answers `ErrorCode::Unavailable` with the missing
ones in `detail` rather than a qualified success. No `checks_run` field, and the reason
is written down: a caller that has to ask which checks ran cannot act on the answer.

One knock-on edit outside `route.rs`: `reply.rs`'s own unit test builds a `Trial` literal
and needed the new `origin` field. That is a test, not the reply shape.

`cargo test -p screeny-device-api`: **71 + 3 doc**, up from 64 + 1. Clippy clean,
`thumbv7em-none-eabi` clean.

### 2026-09-20 - `crates/sim`: the 503 goes away, and the crate's own lookup replaces the local one

- **`POST /api/v1/wifi` while `Online` answers `200 {"result":"trying"}`** and runs a
  real online-origin trial. `Posted::Ignored` is kept but is now only reachable from
  `Boot`, which the simulator never serves from (it boots the machine inside
  `WifiModel::new`); the arm and its 503 stay because card 224's rule - never answer
  `trying` when no trial started - is right whatever state is left.
- `WifiModel::wifi_reply` now asks `Provisioner::trial_is_current()` instead of matching
  on the state itself, so the sticky `FAILED` reaches `GET /api/v1/wifi` without the
  simulator re-deriving the rule.
- `api::SCAN_MIN_INTERVAL_MS` is now `route::SCAN_MIN_INTERVAL_MS`, kept under the same
  name and type so nothing that used it breaks. `ApiState` holds a `route::RateLimit`
  instead of an `AtomicU64` + `last != 0` sentinel, and the 429's `detail` now says how
  long to wait.
- `dispatch` and `within_bound` use `route::find` / `route::path_is_known`; the
  hand-written `ROUTES.iter().any(..)` walks are gone.

**UDP is deliberately unchanged.** `core.rs::set_wifi` only feeds the machine when the
phase is `Portal` or `Trial` - exactly the cases that reached it before. Letting UDP use
the new transition would make a `GET_WIFI` after a `SET_WIFI` read `CONNECTING` where it
has always read `CONNECTED`, and my instructions are that the simulator's UDP behaviour
stays as it is because other sessions' tests are written against it.
`tests/control.rs`'s "SET_WIFI is accepted, logged, and not acted on" still holds, and
`udp_set_wifi_while_online_is_still_accepted_and_not_acted_on` pins the asymmetry so it
cannot be lost by accident. **Proposed card 238** closes it on purpose.

### Orchestrator addition, 2026-09-20 - a second simulator must still start

`Config` gains `http_port_explicit: bool` (default `false`); `http_port: u16` is
untouched, so `..Config::default()` keeps working. `SimDevice::start` now treats the
default port as a *preference*: `AddrInUse` on a port that was not named, and is not 0,
falls back to an ephemeral one with a line on stderr, and the banner's second line
prints the address it really got. `--http-port N` sets the flag, so a busy named port is
still an error. `Config::for_test()` is unchanged (port 0). CLI help and the README say
so. Two tests: `two_simulators_with_default_http_settings_both_start` and
`a_named_http_port_that_is_busy_is_an_error`.

### The flake I found on the way, and fixed: macOS inherits O_NONBLOCK across accept()

Running `cargo test -p screeny-sim -p screeny-provision -p screeny-device-api` failed
about one run in three with `common/http.rs:169: no header terminator in ""` - an empty
HTTP response - on a *different* test each time, including tests card 224 wrote and I
had not touched. `http_wifi` alone was green 8/8, and the workspace run with my four new
tests skipped was green 5/5, so the extra devices were making something pre-existing
fire, not breaking something new.

The cause is in `crates/sim/src/http.rs`. `Server::start` puts the listener in
non-blocking mode so the accept loop can poll the stop flag. **On the BSDs, macOS
included, the socket returned by `accept()` inherits `O_NONBLOCK`; on Linux it does
not.** So a connection whose first bytes had not yet arrived gave `WouldBlock`,
`read_head` read that as "a connection that said nothing", and `serve_one` closed it
with no response at all. One line fixes it: `stream.set_nonblocking(false)?` before the
timeouts `serve_one` already sets. Six workspace runs after the fix: **6/6 green**.

Worth knowing for card 222: picoserve on embassy does not have this problem, but any
host-side acceptor written the same way does, and it fails *only* under load and *only*
on macOS.

### Results

`cargo test -p screeny-sim`: **120 + 1 doc**, up from 112 + 1 - 8 added (4 in
`tests/http_wifi.rs`, 2 in `tests/http_routes.rs`, and the 503 test replaced rather than
deleted, plus 2 more from the port work). `tests/conformance.rs`'s **64-rule wire suite
is unchanged and green in 36 s**; `tests/control.rs` and every other pre-existing suite
are unchanged in meaning and in text apart from the one 503 test the card told me to
replace. Clippy clean (`crates/probe`'s one pre-existing `is_multiple_of` warning is not
mine and not in scope).
