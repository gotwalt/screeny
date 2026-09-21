---
id: 126
title: The brightness slider bounces after it is moved
type: build
hardware: no
depends: [187]
owner: worker (sonnet)
branch: card/126-brightness-slider-bounces
---

## Goal

The owner, 2026-09-21, on the deployed Studio: "the UI for brightness on the web is janky -
I move the slider and it bounces around. It does seem to work, but not ideally." Make the
brightness control on both screens (`/` and `/panel`) stay where it was put.

## Context

- `crates/studio/ui/common.js`, `bindBrightness` (card 187): the control is an index into
  `boot.brightness_stops`. Its `show(d)` runs on every state message and, when the input is
  not `busy`, writes the slider from `d.telemetry.brightness` when there is telemetry, else
  from `player.health.brightness_applied ?? player.brightness`.
- The likely cause, to be confirmed not assumed: the panel's telemetry is periodic, so for a
  second or so after a `change` the newest telemetry still holds the OLD brightness. The
  moment the control stops being `busy`, `show` writes the old value back (the slider jumps
  back), and when the next telemetry arrives it jumps forward again. A drag that fires
  several `change`s, or a raise/cap where `applied != asked`, makes it worse. State messages
  are paced at up to 20 a second (card 196), so every stale one is a chance to bounce.
- Other sliders on the page (`bindSlider`, the rate slider of card 183, speed of card 197)
  may already have a rule for "the person just set this; do not fight them" - read them
  first and use the house pattern if there is one.
- What the control should mean: after the person lets go, it shows what they chose (or what
  the panel said it applied, when that differs - the floor and the cap are real) and holds
  it until the panel's telemetry agrees or clearly disagrees for good (say a few seconds),
  and only then follows telemetry again. A change made elsewhere (a second browser, the
  CLI) must still show up within a few seconds. The re-assert loop in `fleet.rs` is not
  part of this and must not change.

## Deliverables

- The fix in `common.js` (both screens use the one binding; keep it one).
- A test that pins it. `crates/studio/tests/ui.rs` holds the UI to textual rules; if the
  hold logic can be a small pure function in `common.js`, exercise it with `node` when node
  is on the machine (skip cleanly when it is not - no Node dependency for the build), or
  pin it server-side if the honest fix turns out to be there (e.g. the state message
  carrying "applied at" so the page can tell stale telemetry from fresh).
- `crates/studio/README.md` if the control's behaviour is described there.

## Acceptance

Against `screeny-sim` and a local Studio: set a brightness from the page's API path the way
the page does and watch the state stream - no state message after the change makes the
control show the old value. The owner moves the slider on the deployed page and it stays.

## Log

Read order: `CLAUDE.md`, `docs/README.md`, this card, `docs/board/done/187-senders-and-the-brightness-floor.md`
(built the control and its Log explains the stops), `crates/studio/ui/common.js`
(`bindBrightness`, `bindSlider`, `busy`, `connect`), `picture.js`/`panel.js`'s calls into
`bindBrightness`, `crates/studio/tests/ui.rs`, and how state reaches the page
(`crates/studio/src/api.rs`, `devices.rs`'s `Telem`, `player.rs`'s `brightness_applied`/
`brightness_policy`, `fleet.rs`'s `supervise`/`apply_brightness` - card 196's pacing).
`bindSlider` (the rate/speed/APL/rise sliders) has no hold rule at all beyond `busy()` -
those controls are pure policy with no device round trip to go stale, so there was no
house pattern to reuse; brightness is the only control whose displayed value depends on
the *device's* confirmation.

**Cause, confirmed against `screeny-sim` and a local Studio** (both `screeny-sim
--headless --no-mdns --bind 127.0.0.1 --frame-port 54074 --control-port 54075
--http-port 61386` and `screeny-studio --listen 127.0.0.1:61387 --state-dir <tmp>
--no-discover --device-http-port 61386` under `timeout 180`, attached with
`POST /api/v1/set_panel {"on":true,"to":"127.0.0.1:54074"}`, then a brightness change
via `POST /api/v1/device/brightness {"device":"<id>","level":80}` and `GET
/api/v1/status` polled every 0.3 s): the card's guess was right. Sequence observed
(seconds since the poll started, change fired at t≈3.3):

```
t=3.08  telemetry.brightness=30  health.brightness_applied=30
t=3.38  telemetry.brightness=30  health.brightness_applied=80   <- POST resolved: health already true
t=3.68  telemetry.brightness=30  health.brightness_applied=80
...
t=4.61  telemetry.brightness=30  health.brightness_applied=80   <- still stale telemetry, 1.6s after the change
t=4.92  telemetry.brightness=80  health.brightness_applied=80   <- telemetry finally caught up
```

`player.health.brightness_applied` is written synchronously inside the API handler the
moment `POST /api/v1/device/brightness` resolves (`player.rs`'s `brightness_applied`,
called from `api.rs:711`), so it is already correct at t=3.38. But `bindBrightness`'s
`show(d)` did `t ? t.brightness : (health.brightness_applied ?? player.brightness)` -
it always preferred telemetry when telemetry existed *at all*, stale or not, so for the
1.6 s between t=3.38 and t=4.92 it painted the slider back to 30 (the old value) the
moment the input stopped being `busy`, then forward to 80 once fresh telemetry arrived -
exactly the bounce the owner saw. (Studio's default `telemetry_every` is 5 s, so the
worst case is close to a full poll interval, not "a second or so" as the card guessed -
noted for the record, changes nothing about the fix.)

**Fix**, `crates/studio/ui/common.js` only: `bindBrightness` now holds the true applied
value (`done.applied` from the POST's reply, not the raw ask - the floor and the cap are
real) for `BRIGHTNESS_HOLD_MS` (5000 ms, comfortably bracketing one telemetry poll)
after a `change`, and releases the hold the moment a fresher reading agrees with it or
the hold's clock runs out, whichever comes first - then `show()` goes back to painting
whatever telemetry (or, absent telemetry, `health.brightness_applied`/`player.brightness`)
says, same as before. The decision is a small pure function, `brightnessHoldWins(now,
until, held, reading)`, exported so it can be pinned without a DOM. No change to
`player.rs`, `fleet.rs`, `api.rs`, or the wire: `health.brightness_applied` was already
true the instant it mattered, so no "applied at" field had to be added to the state
message - the honest fix is the client trusting the fact it was already being handed,
for a bounded while, instead of always preferring a periodic reading over it.

Cross-browser/CLI case: any change through `/api/v1/device/brightness` updates the one
shared `player.health.brightness_applied` server-side, which every connected browser's
next state message carries - so a second browser or another API caller shows up for a
browser that made no change of its own well inside the 5 s hold window (it is not itself
holding anything, so `reading` alone decides what it paints).

Test: `brightnessHoldWins` exercised with `node` in `crates/studio/tests/ui.rs`
(`node --input-type=module`) when `node` is on the `PATH`; the test is skipped (with a
printed reason, not a silent no-op) when it is not, so a machine with no Node toolchain
still gets a clean `cargo test`. Cases pinned: released immediately once the reading
agrees with the hold, still holding just before `until`, released at/after `until` even
though the reading still disagrees (the "elsewhere" case), and holding when there is no
reading yet (`null`/`undefined`).

`crates/studio/README.md`: added a paragraph after "Brightness is a policy..." describing
the hold-then-release rule and its bound as the answer to "how fast does a change made
elsewhere show up"; added a clause to the `tests/ui.rs` row of the file map.

No hardware, no serial, no camera, no LAN touched - `screeny-sim` and `screeny-studio`
both ran on loopback only, both under `timeout`, both confirmed killed (`ps` clean)
before moving on.

Finish: `timeout 300 cargo clippy -p screeny-studio --all-targets` clean (no warnings).
`timeout 900 cargo test -p screeny-studio --release`: every test binary green, including
`tests/ui.rs`'s 29 (up from 28 - the new `brightness_hold_wins_pins_the_hold_and_release_rules`),
`tests/soak.rs`'s bounded soak, and the doc-tests; exit code 0. Node was on this machine
(`v24.19.0`), so the new test actually ran rather than skipping - both are exercised in
practice, but only the run was confirmed here. No Rust outside test files changed, so the
full-workspace run the card's Finish step reserves for that case was not needed.

Not seen in a real browser: the actual slider element never stopping mid-drag or
snapping back on the deployed page - everything above is the data path (`/api/v1/status`,
the pure hold function) and the DOM-touching half of `show()` (`input.value =`,
`busy()`) was not driven through an actual `<input type="range">` in a browser. The
Acceptance line "the owner moves the slider on the deployed page and it stays" is for
the orchestrator to check after this is merged and deployed.

### Review round 2 (coordinator): two gaps, both in `busy()`

The coordinator's review named the real remaining bug: `busy(el)` is `el ===
document.activeElement`, and **Safari - desktop and iOS both - does not focus an
`<input type="range">` on a click or a touch, only on Tab.** So on Safari,
`busy(input)` was `false` for the *entire* drag, not just the gap after it: `show()`
would rewrite `input.value` from telemetry under the person's mouse or finger every time
it ran, which is very likely the "I move the slider and it bounces around" half of the
owner's complaint, separate from and worse than the post-`change` staleness window this
card started with. The second gap was narrower: between the `change` event firing and
the POST resolving, `held` was still `null` (it was only ever set once `done` came back),
so on Safari specifically - where nothing else was holding the value either - `show()`
could paint a reading from before the change during that gap too.

Fix, same file, same function: a `holding` flag, tracked directly rather than through
focus. Set on `pointerdown`, `touchstart` (`{ passive: true }`), `keydown` and `input`
itself (a backstop for a gesture none of the other three caught); cleared on `change`
(the gesture landed) and, as a backstop for a gesture that ends without ever firing
`change`, on `pointerup`/`pointercancel`/`blur`. `show()` now skips the paint on
`!busy(input) && !holding` rather than `!busy(input)` alone - `busy` is kept too, since
it is still correct for keyboard focus and costs nothing to keep. `held` is now set at
`change` itself, with the *asked* level (`held = { value: level, until: now + HOLD }`),
and only replaced with `done.applied` once the POST resolves - closing the second gap
without waiting on the network for anything to be held at all. A POST that throws (not
just one that resolves with nothing) now also clears `held`, wrapped in `try`/`catch`
inside the `attempt` callback and rethrown so `attempt`'s own notice/finally still runs -
this was a gap the first round left too: an optimistic `held` with nothing ever
correcting it if the request itself failed outright.

Kept as-is per the coordinator's instruction: `brightnessHoldWins` and its node test -
the release rule itself (agree, or the clock runs out) did not change, only when `held`
starts and what else can suppress a paint alongside it.

**Checked `bindSlider` (`common.js` ~line 380) for the same defect, per the coordinator's
ask - it has it.** `refresh() { if (!busy(input)) { input.value = get(); } show(); }` -
same `busy()`-only guard, same Safari gap. It binds every patch parameter slider, the
rate slider (183), the speed slider (197), and the limiter's APL/rise sliders.
`refresh()` runs on `sync(next)` in `picture.js` whenever a `state` message arrives that
is not this browser's own edit (the server already excludes a browser's own changes by
`CLIENT` id), so the trigger is a second browser, a second tab or the CLI changing the
same parameter while this one is mid-drag on Safari - the same shape of bug, on a wider
set of controls. **Not fixed here**, per the coordinator: filed as
`docs/board/backlog/127-bindslider-bounces-mid-drag-on-safari.md`, pointing back at this
card's Log for the confirmed cause and the fix pattern to reuse.

Re-ran: `cargo test -p screeny-studio --release --test ui` - 29 passed (same count; no
test changed, since the release rule the test pins did not change), `cargo clippy -p
screeny-studio --all-targets` - clean. `ps` clean, nothing left running (no sim/studio
was started for this round - the change and its test are both client-side / node-only).
