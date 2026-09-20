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
