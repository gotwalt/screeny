---
id: 198
title: Studio UI refresh - the picture on one screen, the panel on another
type: build
hardware: no
depends: [170, 180, 102]
owner: worker-198
branch: card/198-studio-two-screens
---

## Goal

The owner, 2026-09-20: "I would love a bit of a studio ui refresh to move things that
relate to controlling the panel & its status moved to a separate screen from the visual ui
stuff." He is going back to focused aesthetic work on the pieces; the page he does that on
should be about the picture and nothing else.

## Context

- Today one page (`crates/studio/ui/index.html`, `main.js` ~1250 lines, `style.css`) holds
  everything in one inspector column: Now playing, Parameters, **Panel** (name, discovery
  note, output switch, brightness, link facts, the Device block from card 180, repairs,
  Identify / Rename / Reboot, "Change which panel"), Time, Panel model, Limiter, View, and
  the four meters. Card 170 put the panel on the same page as the picture on purpose - one
  panel, one picture - and that product model **stands**: there is still one player, the
  canvas still shows the frames the panel is getting, controls still act on the panel and
  persist. This card changes where things are drawn, not what they do. No API route, state
  schema or WebSocket message changes.
- Rules that still hold: no Node toolchain, no bundler, no build step; files are embedded
  by `crates/studio/src/ui.rs`; `/dashboard` redirects to `/`. The `ui.rs` tests cross-check
  every `#id` `main.js` reaches for against the HTML - keep that honest for both screens.
- Card 120: a hidden tab asks for no pictures. A Panel screen that shows no canvas should
  likewise ask for no frames (`fps=0`), only state and status.
- The status payload contains the real WiFi SSID (it is shown in the Device block). Never
  put it in a test fixture, a Log, a screenshot or a commit; tests use `Example-Wifi1`.
- Card 197 (the Speed slider has no home position) is folded into this card, since both
  edit the same files: the owner was asked and did not answer, so take the default - a
  `<datalist>` on `#speed-slider` with stops at 0.5, 1 and 2 (card 183's `drawStops` does
  the drawing), no snapping, and a double-click on the slider returns it to 1.00x.

## The split (decided; do not re-litigate, do improve on details)

**Picture** (`/`, the default): the title block, the canvas, Now playing (pieces, seed),
Parameters, Time, Panel model, Limiter, View, the four meters. Everything that changes or
judges what the picture looks like. **Brightness stays reachable here** - it changes how the
picture looks on the LEDs and is part of judging a piece - as a compact control, not the
whole Panel section. One quiet status chip in the title block (the existing `#ro-panel`
pill is the natural place): panel name, LIVE/HOLD/away, fps; it is the link to the Panel
screen, and it takes a fault tone when the panel needs attention so that trouble is never
hidden behind a tab.

**Panel** (`/panel`): which panel, discovery state, the output switch, brightness (the same
control), link facts, the Device block and its health notes, state repairs, Identify /
Rename / Reboot, "Change which panel", and the Studio's own health (`ok`, `problems`, GPU,
state file, sockets - what `/api/v1/status` already carries and the page mostly does not
show). No canvas needed; a small static indication of what is playing is enough.

How the two screens are built is the worker's call - two HTML files sharing `style.css`
and a common script, or one document with two views and real URLs - but each screen must
have its own URL that survives a reload, the browser's back button must work, and a change
made on one screen must show on the other in another browser at once (it already does:
both are the same state stream).

## Deliverables

- The two screens as above, at 390 px and 1400 px widths. Use the existing visual language
  (`style.css` tokens); this is a refresh of layout, not a new brand.
- `ui.rs` serving and tests extended to both screens; the id cross-check covers both.
- `crates/studio/README.md` updated where it describes the page.
- Card 197 moved to `done/` with a Log line pointing here, its acceptance met.

## Acceptance

On the Picture screen nothing about devices, discovery, WiFi, heap or reboots is visible
except the one status chip and brightness. On the Panel screen everything that was in the
old Panel section is present and works, plus the Studio's health. All existing tests pass
unedited except where they name moved markup. Judged in a browser by the owner.

## Log

### Step 0 - claimed, and the reading (worker-198)

Branch `card/198-studio-two-screens`. The worktree's HEAD was `17b6573` (card 141), which
is **behind** `main` and does not contain this card at all - it was written onto `main` in
`4d793b0`. `HEAD` is an ancestor of `main`, so the branch is cut from `main` (`c471af0`)
rather than from the worktree's HEAD: same work, plus the card to do.

Read before designing: `CLAUDE.md`, `docs/README.md`, this card and card 197,
`crates/studio/README.md`, `ui/index.html`, `ui/main.js` (1245 lines), `ui/style.css`,
`src/ui.rs`, `src/page.rs`'s routing and `src/lib.rs`'s `fallback(ui::serve)`,
`tests/ui.rs`, and the Logs of cards 170, 180, 181, 173, 120 and 183.

The behaviours those six cards paid for, which the split must not lose - written down here
so that each one can be pointed at when the screens are built:

1. **170**: one panel, one picture; the two-column bench is behind `@media (min-width:
   1100px)` and everything narrower is an ordinary scrolling column in DOM order; no
   control whose effect on the panel is unclear.
2. **180**: the Device block is **absent**, not empty, for firmware with no HTTP API; every
   tone in it comes from a server-decided flag, never from a threshold in the browser.
3. **181**: the output switch stays live with no panel attached and says what it really
   does - *"Drive a panel as soon as one is found"* - and `set_panel`'s two bodies are
   untouched.
4. **173**: the discovery note tells the three "nothing here yet" apart, in the dim tone,
   never as a fault.
5. **120**: a screen that shows no pictures asks for none (`fps: 0`), and still gets the
   state and the heartbeat.
6. **183**: a slider's stops are drawn from its own `<datalist>` at the thumb's geometry
   (`3.5px + frac * (100% - 7px)`), and they do not snap.
