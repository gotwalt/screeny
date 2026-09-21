---
id: 187
title: Senders - brightness has a floor and 25 real steps now, and the CLI and the Studio still talk as if it had a cap and 256
type: build
hardware: no (the orchestrator checks the wording against the real panel afterwards)
depends: [136]
owner: worker (sonnet)
branch: card/187-senders-brightness-floor
---

## Goal

Card 136 (firmware session, 2026-09-21, on the device since fw 0.8.0) changed what
`SET_BRIGHTNESS` answers: a **nonzero request that would light zero output-enable slots
(1..=5 today) is raised to the dimmest lit level**, and `applied` reports that level (6).
0 stays off; above the floor nothing changed; the cap still clamps from above. The rule
and its constants live in one place, `screeny_receiver::{clamp_brightness,
BRIGHTNESS_FLOOR}`, and the spec says it in `docs/design/protocol-v1.md` section 6.3 (and
one clause in 8.6 for `POST /api/v1/settings`).

The senders were written when `applied != asked` could only mean "the cap is lower". Now
it can also mean "the floor is higher". Make the host side say the true thing. Small
card: wording, one inference, and the control's steps.

## What is wrong today, as found (2026-09-21)

1. **The sender CLI explains a raise as a cap.** On the real panel:
   `screeny brightness 3` prints `brightness 6 (asked for 3; the firmware cap is lower)`.
   `crates/screeny/src/main.rs`, `cmd_brightness` (around line 404): any `applied !=
   level` gets the cap sentence. `applied > level` is the floor ("raised to the dimmest
   level the panel can show"); `applied < level` is the cap, as now.
2. **The Studio keeps a policy the panel will never show.**
   `crates/studio/src/player.rs`, `brightness_applied(asked, applied)` (around line 915)
   handles only `applied < asked` (learns `brightness_cap`, rewrites `cfg.brightness` so
   "the state file and the page both say the true number"). With `applied > asked` the
   policy stays at, say, 3 while the panel runs at 6, so the page and the state file say
   a number that is not true - the thing that comment says it exists to prevent. Apply
   the same honesty upward. It must **not** record a raise as a cap.
   For the record, the re-assert loop in `crates/studio/src/fleet.rs` (around line 154)
   is already safe: it compares telemetry against what the device *said it applied*, so
   a raised value does not make it retry for ever. Keep it that way and pin it with a
   test (asked 3, applied 6, telemetry 6 -> no new job).
3. **The brightness control has 256 positions and 25 pictures.** The device dims by
   shortening the output-enable window: `slots = (b * 25 + 127) / 255`, so e.g. 129 and
   130 are the same picture (card 136's Context; `screeny_panel::{MAX_OE_SLOTS, oe_slots,
   oe_light}` is the host copy of the arithmetic, card 066). Card 136's third
   deliverable was left for this side: **a brightness control in the Studio steps in
   slots** - each stop is a level that changes the picture, the lowest nonzero stop is
   the floor, 0 is off, and the top stop is the learned cap when there is one. Card 183
   (the rate slider's stops) and 197 (the speed slider's home position) are the house
   style for a slider with real stops.

## Not in this card

No change to `crates/receiver`, `crates/proto`, the firmware or the spec: the device's
behaviour is decided (owner, card 136) and on the panel. If something there looks wrong,
tell the firmware session (or write a 200-series card) rather than changing it here.
Cost metrics are not goals; this is about the numbers on the page being true.

## Exit

- `screeny brightness 3` against the simulator says it was raised to the floor, and
  `screeny brightness 255` against a capped device still says the cap is lower. Wording
  takes the floor value from the reply, not from a literal 6.
- Studio: tests for `brightness_applied` with `applied > asked` (policy becomes the
  applied value, no cap learned), for the unchanged `applied < asked` case, and the
  fleet no-retry case above. The page's control steps in slots, built from
  `screeny_panel`'s arithmetic (one implementation - do not write the formula again in
  JavaScript if the server can hand the page its stops).
- `timeout 1200 cargo test` green; clippy clean for what was touched.
- A note in the Log of anything else on the host side found still assuming
  "`applied` differs only because of the cap" (`crates/probe` was already fixed by card
  136: its `brightness_applies` rule predicts with `clamp_brightness`).

## Rules

Branch `card/187-senders-brightness-floor` from current `main`, own worktree; commit and
Log as you go by explicit path; do not merge or push. No hardware, no serial port, no
LAN, no camera: use `crates/sim`. Every command bounded with `timeout`; no background
processes left behind. Never write a real SSID or password (dummies `Example-Wifi1` /
`password9`). List every change to shared crates.

## Log

Worktree started at 127c829 (stale) rather than `main`'s 6bdcc1c; the coordinator
confirmed the fix - `git checkout -b card/187-senders-brightness-floor 6bdcc1c` - which
is what this branch is built on.

Read order followed: `CLAUDE.md`, `docs/README.md`, this card, `docs/design/protocol-v1.md`
6.3 (already updated by card 136: the floor rule and the 8.6 settings clause are both
there), `crates/receiver` (`clamp_brightness`, `BRIGHTNESS_FLOOR = 6`, derived from
`oe_slots` rather than written as a literal), `crates/panel::model` (`MAX_OE_SLOTS = 25`,
`oe_slots`, `oe_light` - read-only, no edits), then `crates/screeny/src/main.rs
cmd_brightness`, `crates/studio/src/player.rs brightness_applied`, `crates/studio/src/fleet.rs`
(the re-assert loop at `supervise`, ~line 154, and `apply_brightness`).

Finding 2 fixed first (`crates/studio/src/player.rs`, `brightness_applied`): the
`brightness_cap` learning stays keyed to `applied < asked` only (a raise must never be
recorded as a cap), but the policy rewrite that keeps `cfg.brightness` truthful now
fires on `applied != asked` in either direction, not just `applied < asked`. Added two
unit tests in `player.rs`'s own `mod tests`: a raise (asked 3, applied 6) moves the
policy to 6 and learns no cap; the unchanged cap case (asked 200, applied 120) still
moves the policy to 120 and learns the cap.

Finding 1 (`crates/screeny/src/main.rs`, `cmd_brightness`): added an `applied > level`
arm alongside the existing `applied < level` one. Wording: `"brightness {applied}
(asked for {level}; raised to the dimmest level the panel can show)"` - `applied` comes
from the reply, never a literal 6, so the wording stays true if the floor ever moves.
Tested against `screeny-sim` (not the crate's own in-process fake receiver in
`tests/common`, which only implements a flat `min(60)` cap and knows nothing of the
floor): new test `brightness_wording_tells_a_raise_from_a_cap` in
`crates/screeny/tests/cli.rs`, covering both `screeny brightness 3` (raised to 6) and
`screeny brightness 255` against a `brightness_cap: 120` sim (still says the cap is
lower). Needed a helper for a free **consecutive** port pair (`--addr` guesses the
control port as frame + 1) - copied `embed.rs`'s `free_port_pair` rather than sharing
it, since the two test binaries do not share code today and this is a two-line
function. `cargo test -p screeny --test cli` and `cargo clippy -p screeny --all-targets`
both clean.

Finding 2's other half (`crates/studio/src/fleet.rs`, the re-assert loop in `supervise`,
~line 154): confirmed already safe, as the card said - it compares telemetry against
`applied` (what the device said it did), never `want`, so a raise does not retry for
ever. Pinned it: pulled the one-line decision out to a private `brightness_drifted(applied,
heard)` so it is unit-testable without the whole `AppState` machinery (the minimum the
card allowed touching `fleet.rs` for beyond the test itself), and added `mod tests` with
the card's own scenario (asked 3, applied 6, telemetry 6 -> no drift, no job) plus the
one that must still retry (telemetry disagrees with `applied`). `cargo test -p
screeny-studio --lib fleet::` and `cargo clippy -p screeny-studio --all-targets` both
clean. Only change in `fleet.rs`: this extraction plus the test module - the HTTP status
poller (card 199's) is untouched.

Finding 3 (the brightness control): built the stops server-side, one implementation, in
`crates/studio/src/page.rs`, `pub fn brightness_stops() -> Vec<u8>` - `0`, then the lowest
`u8` at which `screeny_panel::model::oe_slots` first reaches each of `MAX_OE_SLOTS` (25)
lit slots. 26 stops total; `stops[1]` is pinned against `screeny_receiver::BRIGHTNESS_FLOOR`
rather than written down again (needed a new `screeny-panel` dependency and a test-only
`screeny-receiver` one in `crates/studio/Cargo.toml` - the latter mirrors `crates/panel`'s
own dev-dependency on `screeny-receiver` for the same reason). Added to `page::Bootstrap`
as `brightness_stops`, filled in by the `bootstrap` handler in `api.rs`; the page already
fetches `bootstrap` once on load in both screens.

Surprise found while writing the test: the *top* real stop is not 255. `oe_slots` reaches
25/25 slots at 250, and 251..=255 are five more values landing on the same picture - so
250 is the correct top of the list, not 255. First draft of the test assumed 255 and
failed; fixed the test, not the function.

The page (`common.js`'s `bindBrightness`, shared by `index.html`'s and `panel.html`'s
`#bright`): the control is now an **index** into the stops the page was handed
(`boot.brightness_stops`), not the raw level - a plain `0..255` range would let a user
drag through ~230 positions that change nothing, which is the thing this finding was
about. `cappedStops(stops, cap)` trims the server's list to a device's own learned cap and
appends the cap itself if it does not already land on a stop (a cap is a raw byte
`clamp_brightness` can hand straight back, so it is not always one of the boundary
values) - confirmed by hand: `brightness_cap: 120` in `fleet.rs`'s existing sim fixture is
not itself a stop (118 is the boundary below it), so `cappedStops` has to append it, not
just filter. With no learned cap yet, the raw server list is used unfiltered (so the
default top is 250, not an artificial 255). Deleted the client-side `BRIGHTNESS_FLOOR = 6`
/ `snapBrightness` workaround entirely - card 136's own note said to delete it once the
firmware had the floor, and it does.

Also fixed while touching this control (in scope: "the brightness control" is named
explicitly, and this is the same host-side "applied differs only because of the cap"
assumption as finding 1, just in the UI rather than the CLI): the change handler's
success message always said "This panel caps brightness at N", even when N was a raise.
Now branches on `applied > asked` / `applied < asked` / equal, same three cases as the
CLI.

`crates/studio/tests/ui.rs`: replaced `the_screens_keep_their_promises`'s
`BRIGHTNESS_FLOOR` assertions (which pinned the very workaround this finding removes)
with ones for the new shape - no file mentions `BRIGHTNESS_FLOOR` any more, `common.js`
has `cappedStops`, both screens hand `bindBrightness` `boot.brightness_stops`. Added
`bootstrap_carries_the_brightness_stops`, an end-to-end check that the route really
serves the list (the formula itself is `page::tests`, next to the function). All 28
tests in `tests/ui.rs` green, including the two pre-existing ones this touches most
closely (`every_element_each_screen_reaches_for_exists`,
`every_slider_that_declares_stops_declares_reachable_ones` - the brightness control
does not use the `list=`/`drawStops` mechanism those check, since its stops are not
uniform enough for a plain datalist overlay; it snaps by construction instead, unlike
Rate and Speed, which deliberately do not snap). `cargo test -p screeny-studio --lib`
(91 passed) and `--test fleet` (12 passed) also rerun clean after this change; `cargo
clippy -p screeny-studio --all-targets` clean.

Files touched in `crates/studio/ui`: `common.js`, `picture.js`, `panel.js`, `index.html`,
`panel.html`. None of `panel.js`'s Device block or `panel.html`'s Device block (card
199's) - only the brightness control markup/script, which lives in its own section on
each page.
