---
id: 170
title: Studio - one panel, one picture; the page is a window onto the device
type: build
hardware: no
depends: [106, 165]
owner: worker-170
branch: card/170-one-panel-one-picture
---

## Goal

Make the Studio what the owner means it to be. His words (2026-09-19):

> The objective of the web application is to almost always be connected to a panel, only
> one panel, and there's almost always only ever going to be one panel on a given network.
> So when you set up the web application, you connect it to a panel, and then the web UI
> will allow you to preview it in case you don't have the panel within eyesight. We want
> the thing that's on screen to generally show what the device is also doing at the same
> time.

## Context

- Card 106 built the opposite emphasis, from an earlier reading of the vision: a *preview*
  engine behind the design view with its own piece/params/seed, a separate *player* per
  device, a "Send to panel" switch on the design view, "Play my preview" to promote, and a
  separate `/dashboard`. The two can even fight over the panel's source lock (old card 144,
  deleted - this card dissolves the question instead of answering it).
- What card 106 got right and must survive: the device registry keyed by stable id; the
  player owning the link; containment (panic/stall -> fallback); the state store and its
  recovery rules; `/healthz` semantics (a missing panel is never 503); the supervisor,
  telemetry poll and brightness policy; auto-adopting the first panel found. Read its Log
  in `docs/board/done/106-studio-players-devices-state.md`, and card 165's (per-piece
  settings memory, one shared map).
- `docs/design/studio-vision.md`, "One panel, one picture", is the requirement. The data
  model stays a collection of devices and players (the owner's decision 3: several panels
  must not be precluded); the **UI and the defaults assume one**.
- The service is live on the owner's Linux box with a v2 state file (after card 165). This
  card changes what the state means; migrate, never destroy.

## Deliverables

- **One engine per panel, and the page shows it.** The design view's canvas shows the
  attached panel's player: the same frames that go to the panel (the decoded datagram, as
  today's preview does), by the same WebSocket. Piece, params, seed, playback, pipeline
  settings: the page's controls act on that player, apply to the panel at once, and
  persist. The separate preview engine, its state, the "Send to panel" switch and "Play my
  preview" go away. Two browsers still stay in step.
- **Setup, once.** With no panel attached, the page says so and offers what it has found
  by mDNS plus "enter an address"; the first panel found is still adopted automatically
  (card 106) so the zero-click case stays zero clicks. Attached panel away (unplugged,
  rebooting): the page keeps rendering and showing the picture, says plainly that the
  panel is away, and the link comes back by itself. Changing which panel the Studio is
  attached to is possible but tucked away (it is a setup action, not a daily one).
- **Panel status and controls on the same page**, not a separate app: connection state,
  fps, drops, RSSI, firmware, uptime; brightness, identify, rename, reboot (behind a
  confirm). Fold `/dashboard` in; keep `/dashboard` as a redirect. Several panels, if
  present, get a plain chooser - nothing more.
- **A way to look without touching the panel is NOT a goal** - do not build a sandbox mode.
  The one exception worth keeping: a "panel output off" control (the panel goes to its own
  idle screen, the page keeps showing the piece), because people turn displays off.
- **Works on a phone and in a narrow window** (this absorbs card 161: the owner's first
  screenshot showed the preview overlapping the inspector at ~600 px). Check at 390, 600,
  900, 1400 px in a real browser, screenshots in the Log. Static files only, no CDN, keep
  the design language. Parameters that are named stops (card 163) can stay sliders.
- **State**: migrate the v2 file (preview + players) to the new shape: the attached panel's
  player is the truth; the old preview block's piece/params are dropped after being merged
  into the per-piece memory. Same recovery rules.
- **API**: keep `/api/v1` working for the routes the firmware session and scripts use today
  (`set_panel {"on":false|true}` to release/retake the panel, `status`, `healthz`,
  `player/set`); the old preview routes become aliases onto the attached panel's player.
  Document the surface in the README.
- Tests for all of it against `screeny-sim`; the restart/reboot acceptance from card 106
  still passes.

## Acceptance

On the deployed service: open the page on a laptop and a phone - both show what the panel
is showing; drag a slider and the panel and both pages change together; unplug the panel
and the page says it is away and keeps showing the piece; plug it in and it resumes;
restart the container and everything comes back as it was. There is no control on the page
whose effect on the panel is unclear.

## Log
