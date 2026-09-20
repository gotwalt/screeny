---
id: 151
title: A patch has named settings - save, load, rename, delete, and a mark when they are modified
type: build
hardware: no
depends: [150]
owner: worker (Claude Opus 5)
branch: card/151-named-settings
---

## Goal

The owner, 2026-09-20: "each patch can have multiple named persisted settings", "sort of
like many audio plugins do". Asked, he chose: **a setting holds the patch's parameters, the
seed and the speed**; **every patch has a built-in read-only "Default"**, and he can Save
(overwrite), Save as... (new name), Rename and Delete his own; moving a control after
loading shows the name as **modified** until he saves or reverts; the panel remembers
which setting each patch was on; unsaved tweaks survive a restart as they do today.

## Context

- After card 150 the words are: *patch* (metaballs), *settings* (this card), *output* (the
  panel kind / dither / limiter - not part of a setting: it is about the panel, not the
  patch).
- What exists: `crates/studio/src/state.rs` keeps one memory per patch (`seed`, `params`;
  card 165) so that coming back to a patch restores how it was left. That memory is, in
  effect, the *working copy*. This card keeps it exactly as it is and adds beside it:
  the patch's named settings, and the name of the one the working copy was loaded from.
  "Modified" is then not a stored flag but a comparison - the working copy differs from
  the setting it names - which cannot go stale.
- "Default" is not stored: it is the patch's declared parameter defaults (`ParamSpec`),
  speed 1.0, and the seed - decide and say: a fixed seed per patch is what makes "Default"
  one picture; a patch that has no opinion uses the Studio's existing default seed.
  It cannot be overwritten, renamed or deleted; "Save" on it is "Save as...".
- A parameter a patch has gained since a setting was saved takes its default; one it has
  lost is dropped on load and said once in `repaired`-style wording, never an error.
  (Card 115 added `tip` to both clocks this morning: that case is real.)
- State schema: v4 (card 150) -> v5 by the existing migration pattern, backup first.
  Bound it: at most 64 settings a patch, names trimmed, 1..=40 characters, unique per
  patch case-insensitively; a state file is still small enough to write whole.
- API, in card 150's style (one `on_page` change path, broadcast to the other browsers,
  persisted): list comes with the state (`StudioState`), plus `settings/load {name}`,
  `settings/save {name?}` (no name = overwrite the current one), `settings/rename`,
  `settings/delete`. Loading a setting is one change, one broadcast, one write - not one
  per parameter (card 196 paces a burst, but a load should not be a burst).
- The page (card 198's Picture screen, `picture.js`): a settings control at the head of
  the Parameters section - the current name with a modified mark, a list to load from,
  Save / Save as... / Rename / Delete / Revert. Quiet and typographic like the rest; works
  at 390 px; keyboard reachable. The existing "Reset" becomes "load Default". No
  `prompt()`/`confirm()` dialogs (the browser tooling cannot drive them, and they are ugly):
  inline name field, inline confirm for delete.
- **The seed is not a control for humans** (the owner, the same afternoon: "i don't think
  the 'seed' value is really interesting to humans as it's super opaque as to what it
  affects"). It stays what it is underneath - part of what makes a picture reproducible,
  stored in a setting and in the working copy, accepted by the API and the CLI - but the
  page stops presenting a number: the Seed readout in the title block and the numeric
  `#seed` input go. What a person wants from it is "show me another one like this": keep
  that as one quiet button beside the settings control ("Another" - the `N` key still
  works), which picks a new seed and therefore marks the setting modified like any other
  change. Show that button only for a patch whose picture actually depends on its seed:
  add a `seeded: bool` (or similar) to the patch definition, set honestly per patch by
  reading the code - the clocks seed only a dance order, plasma may use none - and hide the
  button where it would do nothing a person could see. A patch that has better words for
  its own randomness can already offer an action (`patch_act`, as the clocks' "again" /
  "another" do); prefer those where they exist. The number remains visible in
  `/api/v1/status` and in the `title` tooltip of the button, for the day somebody needs to
  reproduce a frame.
- The CLI: `screeny-art play|snapshot <patch> --setting NAME` would need the Studio's
  state file; out of scope here - write it as a follow-up card if it looks worthwhile.
- Promotion of a good setting into the repo as a factory setting was offered to the owner
  and **not** chosen. Do not build it; do not make it impossible (a setting is plain data).

## Deliverables

- `state.rs`: the settings store + migration + tests (round trip, bounds, name rules, a
  setting from an older patch version, v4 file loads).
- `api.rs`/`player.rs`: the four routes, the state fields, tests including "two browsers
  see each other's save and load".
- `picture.js` / `index.html` / `style.css`: the control, and the seed's number gone from
  the page; `tests/ui.rs` id checks. `crates/art`: the `seeded` flag on each patch definition
  (tell the orchestrator: card 155's worker is adding a patch in the same crate).
- `crates/studio/README.md`: the model in a short section - patch, working copy, settings,
  Default, modified - and the routes.

## Acceptance

On the panel: tune metaballs, Save as "Lava"; tune again, Save as "Slow ink"; switch to
clocks-dials and back - metaballs returns as it was left, naming the setting it was on;
load "Lava" - one change, the panel follows at once; move a slider - the name shows
modified; Revert - it clears; restart the container - all of it is still there.

## Log

### Claimed (worker, 2026-09-20)

Branch `card/151-named-settings` off `main` at b081736. Read the card, cards 150, 165,
196, 198 and 170, `crates/studio/README.md`, and the code it touches: `state.rs`,
`player.rs`, `api.rs`, `page.rs`, `ui/index.html`, `ui/picture.js`, `ui/common.js`,
`ui/style.css`, `tests/ui.rs`, and every patch in `crates/art/src/patches`.

The shape I am building, decided from the card and the code:

- **Stored** per patch, in the one `patches` map of `state.json`: the working copy
  (`seed`, `params` sparse against the defaults, and - new - `speed`), the name it was
  loaded from (`setting`), and `settings`: name -> `{seed, params, speed}`.
- **Computed, never stored**: `modified`. The working copy compared against the *usable*
  form of the setting it names (or against Default when it names none), so a setting that
  had to be repaired for this build does not read as modified for ever.
- **Default** is synthesised, not stored: the patch's `ParamSpec` defaults, speed 1.0 and
  a fixed seed. No patch declares a seed of its own today, so that seed is the studio's
  own default, `StoredPlayer::default().seed` = 1 - fixed, so Default is one picture.
- `speed` joins the per-patch memory, because a setting carries it: without that,
  switching patch and back would lose the speed a setting set and `modified` would lie.
  `fps` and `paused` stay per player - they are about playback, not about the patch.
