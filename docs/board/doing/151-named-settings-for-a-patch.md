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

### `seeded`, read off each patch's own code (worker)

`PatchDef` gains `seeded: bool` - "is *another one like this* something a person can ask
this patch for?" - and every definition sets it with a comment saying what its seed does.
Nothing else in `crates/art` is touched (card 155's worker is adding `vesta` in the same
crate; its literal is the orchestrator's to reconcile).

| patch | seeded | what it uses its seed for |
|---|---|---|
| `plasma` | yes | three waves' directions, frequencies, speeds and phases, the ring centre and its frequency |
| `metaballs` | yes | every body's radius, path amplitude, frequency and phase |
| `knot` | yes | which of five (p, q) torus knots, and the tilt it is seen from |
| `lattice` | yes | `u.seed` in the shader: the roll phase and the per-cell hue hash |
| `overland` | yes | `u.seed`: the biome (green / savanna / red rock / alien / ice), every noise offset, the path |
| `testcard` | no | `make(_seed)` - it ignores it entirely |
| `clocks-numerals` | no | only which choreography a given minute is danced to; the picture is the time. It offers "Play it again" / "Compose another", which act at once |
| `clocks-dials` | no | the opening mood and the ambient field, but it wanders on by itself and offers "Move on" |

The clocks are the case the card names: their seed is not *nothing*, but it is not
"another one like this" either, and each already has better words of its own
(`patch_act`). Two tests in `patches/mod.rs` keep the claim honest: every CPU patch that
says `seeded` really does draw a different second frame on another seed, and the test
card really is the same card on any seed. (The GPU three are excluded: a test machine may
have no adapter; their `u.seed` use is in the `.wgsl` above.)

### The store: schema v5, the settings and the migration (worker)

`state.rs`. The file's `patches` map is still one entry per patch; the entry is now the
whole of what card 151 calls a patch:

```jsonc
"version": 5,
"patches": {
  "plasma": {
    "seed": 111, "params": { "scale": 2.5 }, "speed": 0.4,   // the working copy
    "setting": "Lava",                                       // what it was loaded from
    "settings": {                                            // the named ones
      "Lava":     { "seed": 111, "params": { "scale": 2.5 }, "speed": 0.4 },
      "Slow ink": { "seed": 222, "params": {},               "speed": 0.2 }
    }
  }
}
```

Decisions, and why:

- **`modified` is nowhere in the file.** It is `modified(memory, def, working)`: the
  working copy against the *usable* form of the setting it names. Usable, not raw - a
  setting that had to be repaired for this build would otherwise read as modified from
  the moment it was loaded, for ever.
- **Both sides go through one funnel.** `sparse(def, params)` drops anything equal to the
  patch's default, for the working copy and for a setting alike, which is what makes the
  comparison honest *and* makes "a parameter the patch has gained takes its default" true
  with no code for it. A parameter it has **lost** is dropped by `usable_setting` and said
  once in the existing `repaired` voice, prefixed with which setting it was in.
- **Default** is synthesised in `usable_setting`: no params, speed 1.0, seed
  `DEFAULT_SEED` = 1. Fixed rather than fresh, so Default is one picture; it is the
  studio's own starting seed because no patch declares one, and a `PatchDef` that later
  does can override it without this changing.
- **Bounds**: `MAX_SETTINGS` 64 a patch, names trimmed, 1..=40 characters, no control
  characters, unique case-insensitively, `Default` reserved in any case. Every refusal is
  a sentence a person can read; `check_name` is the one place they live, and the file
  reader runs a hand-edited name through the same rules.
- **Delete leaves what is playing alone**: the values stay, the name goes, so it is on
  Default and honestly modified.

Migration v4 -> v5 follows card 150's pattern: `back_up` first (`state.v4.json`, byte for
byte), then `migrate_to_v5` behind its own `was < 5` guard beside `was < 3`. The one thing
that *moves* is **speed**: it was a player's, it is now part of the working copy, so the
patch each player was on takes that player's speed - and only when it is not 1.00x, or a
file where nobody has ever tuned anything would come back with a memory entry invented for
it (`migrating_a_v3_file_invents_no_memory` is the test that says so). A v1 file runs
v1->v3 and then v4->v5 in one start.

Tests added in `state.rs` (74 unit tests pass, clippy silent on both crates):
save/load/modified round trip, Default is read-only in any spelling, the name rules, the
64 bound, rename-follows-the-name and delete-leaves-it-playing, **a setting older than the
patch** (lost / gained / out-of-range, and it must not read as modified after loading), a
realistic v4 file (dummy device names) -> v5 with the speed carried and the backup byte
for byte, a v5 file that does **not** run the migration again, v1 all the way up in one
start, a hand-edited `settings` block where every way of being wrong costs that value
alone, and a file with more than 64 settings cut to the bound.

### The page (worker)

`index.html`, `picture.js`, `style.css` - and two small things on the Panel screen.

**The seed's number is off both screens.** The `Seed` readout in the Picture screen's
title block and the `#seed` number box are gone; so is the Panel screen's `Seed` readout,
which was the same opaque number in the same place and would have looked like an
oversight. In its place the Panel screen says **which setting** the panel is on
(`Setting: Lava, modified`), which is something a person can act on. The number is still
in `/api/v1/status`, in `state.json`, on the API, and in the Another button's tooltip.

**Another** is one quiet button in the Parameters section head, shown only when
`patch.seeded`. The `N` key is its shortcut and goes where it goes - on a patch that is
not seeded there is no button, and the key would rebuild the patch for nothing anybody
could see. (The card says "the N key still works"; this is it still working, on the
patches where it does anything.)

**The settings control** heads the Parameters section, above the sliders it holds:

```
 [ Default            v ]  MODIFIED
 [ Save ][ Save as… ][ Rename ][ Delete ][ Revert ]
 Save as  [ Lava            ] [ Keep ] Cancel        <- inline, only while naming
 Delete "Lava"?  [ Delete ] Keep it                  <- inline, only while confirming
 There is already a setting called `Lava`.           <- inline, beside the control
```

- The list is `Default` and then the patch's own settings; **picking one loads it**, which
  is what the old "Reset" button became (Default is a load like any other).
- On Default, `Save` *is* `Save as…` (there is nothing to write over), and Rename and
  Delete are disabled - said by shape rather than by refusing after the fact. `Revert` is
  disabled when there is nothing to revert.
- No `prompt()` and no `confirm()`: a name is typed inline (Escape or Cancel closes it), a
  delete is confirmed inline. `tests/ui.rs` holds the Picture screen to that. The Panel
  screen still uses `window.confirm` for rename / reboot / forget - card 198's code, left
  alone here and written up as card 159.
- Refusals land on a line **beside the control**, not on the shared notice line at the
  other end of the page: they are about the name just typed.
- Every action is one POST whose answer is the whole state, which is also broadcast, so a
  second browser sees a save, a load, a rename or a delete at once with no extra read.

`tests/ui.rs`: the seed's number is gone from both screens, the control's ids all exist
and sit between the heading and `#params`, no `prompt`/`confirm` on the Picture screen,
the page's `'Default'` and its `maxlength` are the server's `DEFAULT_SETTING` and
`MAX_NAME_CHARS` (one spelling, one bound), the whole save/load/modified/rename/delete
sequence over the API, every refusal a 400 whose sentence ends in a full stop, and
`bootstrap` carrying each patch's own `seeded`.

One existing test had to move: `the_routes_a_piece_had_still_answer` (card 150) asserted a
reply has no `settings` key at all. Card 150 freed that word *for this card*, so the claim
is now "`settings` is the patch's named settings, a list of names" - still never the
output block under its old name. Two in `tests/memory.rs` hard-coded `version == 4`; they
read `state::SCHEMA_VERSION` now.
