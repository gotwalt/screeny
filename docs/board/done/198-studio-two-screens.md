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

### Step 1 - two documents, one shared module

**Two documents, not one document with two views.** `index.html` is the Picture and
`panel.html` is the Panel; `src/ui.rs` serves the second at the tidy URL `/panel`, beside
`/`. The card left the choice open, and this is the one that makes the acceptance a fact
about the files rather than a CSS rule: *"nothing about devices is on the Picture screen"*
is true because that markup is in the other file, not because something is
`display: none`. Reload, the back button and a bookmark are then the browser's own job and
not a `popstate` handler of ours, and a screen costs only its own markup. The state stream
is untouched, so a change made on one screen shows on the other, in another browser, at
once - both are the same `/api/v1/ws` and the same `/api/v1/status`.

**What they share is a module.** `ui/main.js` (1245 lines) became three files:

| file | what is in it |
|---|---|
| `common.js` | the socket, the status poll, the notice line, the formatting, `drawStops`/`bindSlider`/`bindRadios`/`bindSwitch`, the brightness control, and the one judgement of what the panel is doing (`panelState`, `attention`) |
| `picture.js` | the WebGL renderer, the frame pump, pieces, parameters, seed, time, panel model, limiter, view, the meters, the status chip |
| `panel.js` | which panel, discovery, the link facts, the device block, identify / rename / reboot, the chooser, and the studio's own health |

Plain ES modules (`<script type="module">`, one relative `import`): no Node, no bundler,
no build step - the browser fetches `common.js` itself, and `ui.rs` lists the five files
as it always has. The rule `common.js` lives by, and a test now enforces: **it reaches for
no element by id except `#notice`, which both screens have.** Everything else is handed
the element it works on, so a shared function cannot half-work on the screen that has not
got it.

**The id cross-check is per pair now** (`every_element_each_screen_reaches_for_exists`):
`picture.js` against `index.html`, `panel.js` against `panel.html`, and `common.js`
against **both**.

Also in this step, and each one a deliberate choice rather than a move:

- **The status chip** (`#ro-panel`) is the Picture screen's one panel-shaped thing and the
  link to the other screen (`a.pill`, a chevron from `::after` because the text is
  rewritten on every heartbeat). It reads `NAME · live · 30 fps`, `NAME · output off`,
  `NAME · away`, `NAME · stopped`, or `No panel`. When the panel needs attention it takes
  the **fault tone and says why** - `NAME · live · out of stack`. `attention()` in
  `common.js` picks one phrase, worst first, and every flag it reads is one the *server*
  decided (`store_errors`, `stack_fault`, `low_heap`, `odd_reset`, `bad_fw_state`,
  `last_error`, a player that gave up). Only the fault-level ones: a margin going
  (`stack_warn`) is amber on the Panel screen and is not a reason to colour the other
  screen. This is the one thing a second screen could have cost - trouble hiding behind a
  tab - so it is the chip's whole reason to exist.
- **Brightness** is on both screens and is **one binding** (`bindBrightness`), so the
  device's cap, card 136's floor of 6 and "what the device says it applied" cannot drift
  apart. On the Picture screen it carries a static line the old page never had: *"The
  panel's own brightness, which is why the picture above does not change with it."* -
  true, and previously something you had to know. With no panel attached the note now says
  so instead of staying on the last panel's wording.
- **The Panel screen asks for no frames** (`noFrames`, `fps: 0`): it has no canvas, so
  every frame sent to it would be received and thrown away. That is card 120's rule for a
  hidden tab, applied to a screen that draws none. It still gets the state messages and
  the half-second heartbeat, which is what it is made of.
- **The studio's own health** is on the Panel screen (`#sec-studio`), which
  `/api/v1/status` has always carried and no page ever showed: `ok` and, when it is not,
  the problems in words - the same judgement `/healthz` makes, so a person can see *why* a
  container is unhealthy without curl - the version and uptime, the adapter or why there
  is none, the state file and its writes, what the browsers are costing, and card 167's
  repairs (`#state-repairs`, renamed from `#panel-repairs`: it is the studio's state file,
  not the panel's).
- **The socket's URL is root-absolute** (`new URL('/api/v1/ws', location.href)`), and so
  are both pages' `href`/`src`. `/panel` and `/panel/` are the same screen, and a relative
  URL would have aimed the second one's socket at `/panel/api/v1/ws`.
- **The wide-width bench is scoped to the Picture screen** (`body.picture-page`). It used
  to be `html, body { height: 100%; overflow: hidden }` at every page above 1100 px, which
  would have stopped the Panel screen scrolling. The Panel screen is an ordinary scrolling
  document at every width, and splits into two columns - the panel's controls left, what
  it and the studio say about themselves right - above the same 1100 px, held to a
  readable measure rather than stretched across a bench.

Rust touched: `src/ui.rs` (the file list, the second tidy URL, the doc comment),
`src/page.rs` (two doc comments naming `ui/main.js`), `tests/ui.rs` and one line of
`tests/api.rs` that named the old script. No route, no state schema, no socket message.

`cargo test -p screeny-studio --test ui`: **20 passed**.

### Step 2 - card 197, folded in

A `<datalist id="speed-stops">` on `#speed` with **0.5x, 1.00x and 2x**, drawn by card
183's `drawStops` because it is a `.slider` with a `list` and that mechanism was built to
be general. **They do not snap**, for card 172's reason and card 183's: a magnet at 1.00
makes 0.95 and 1.05 unreachable with a mouse, and a speed a script set must be shown
exactly. What the card really asked - is a mark enough, or does a slider with an obvious
home want a way *back* to it - is answered with the default the orchestrator named: a
**double-click on the slider returns it to 1.00x**, pushed to the player like any other
change, with `title="Double-click for 1.00×"` on the input so the affordance is not a
secret.

The test is the general one card 197 asked for rather than a second special case:
`every_slider_that_declares_stops_declares_reachable_ones` walks **every** `list=` on
either screen, pulls the input's own `min`/`max` out of the same tag, and asserts each
declared stop is inside it - so the next slider that declares stops is checked by the
test that already exists. It also pins Speed's three, the double-click, and that what
draws the marks never touches the value.

Measured in Chrome at 1400 px, reading the resolved positions back (the same check card
183 did): track `left 1189, width 291`, thumb 7 px, so the formula puts 0.5x at 1221.6,
1.00x at 1258.0 and 2x at 1330.9 - the three marks measured **1221, 1258, 1330**. Both
ends land on the arithmetic and 1.00x is within 0.1 px. A drag to 2.75x read `2.75×` on
the page and `state.speed 2.75` on the API; a double-click read `1.00×` and `1`.

### Step 3 - in a real browser, and what could not be checked there

A studio and a simulator of my own on loopback, both under `timeout`, nothing near the
bench panel or the LAN: `screeny-sim --bind 127.0.0.1 --no-mdns --headless --frame-port
50971 --control-port 50972 --http-port 50973` and `screeny-studio --listen
127.0.0.1:50970 --no-discover --device-http-port 50973 --ui-dir crates/studio/ui` with a
state dir in the scratchpad. The simulator's SSID was set to the dummy `Example-Wifi1`.

**Every URL, over real HTTP**, with the content type the browser is given:

| | | |
|---|---|---|
| `/` `/index.html` | 200 | `text/html; charset=utf-8`, 11862 B |
| `/panel` `/panel/` `/panel.html` | 200 | `text/html; charset=utf-8`, 4336 B |
| `/common.js` `/picture.js` `/panel.js` | 200 | `text/javascript; charset=utf-8` |
| `/style.css` | 200 | `text/css; charset=utf-8` |
| `/dashboard` | 307 | to `/`, as card 170 left it |
| `/main.js` `/nope.js` `/../Cargo.toml` `/sub/dir.js` | 404 | |

`node --check` on all three scripts: clean. And, because a missing export is a link
error rather than a syntax error, each screen's module was imported in node with
`document` undefined: **both linked** (every named import resolves), then failed at
evaluation where they touch the DOM, which is what that check can show.

**Rendered in Chrome**, both screens, console clean on load and after a reload:

| | what happened |
|---|---|
| `/` | the chip reads `DESK · LIVE · 30 FPS ›` in the amber "on" tone; brightness 120 with both hint lines; no horizontal scroll |
| the chip | clicking it goes to `/panel`; the browser's **back button** returns to `/` with the chip already right |
| `/panel` | `DESK`, *Sending to 127.0.0.1:50971*, the pill `ON THE PANEL`, the output switch, brightness, Identify/Rename/Reboot, the chooser folded, the link facts, the Device block (slot, up, memory, free stack, WiFi, reboots, last reset, when idle) and Studio (ok, version, adapter, state file, browsers) |
| output off, on the Panel screen | pill `Output off`, help *"The panel is on its own idle screen…"*, and the **simulator stopped receiving**: `frames_rx` 5679 -> 5679 over two seconds. On again: `On the panel`, *Sending to…* |
| the other screen, at once | with the Picture screen open in a **second tab**, the switch on the Panel screen changed its chip to `Desk · output off` / `away` and dimmed the stage frame - the two screens are one state stream, as the card requires |
| Identify | the notice line says *Identifying* in the "say" tone |
| brightness | typing 3 snapped to **6** (card 136); 80 applied, the note became *"Kept at 80 across reconnects."* |
| no panel at all | `NO PANEL YET`, *"Nothing is being sent. The studio is still playing the piece; the Picture screen shows it."*, card 173's *"Not looking for panels: this studio was started with --no-discover…"*, card 181's *"Drive a panel as soon as one is found"*, the chooser opened itself, **no Device block**, and the Picture screen's chip read `No panel`. Nothing on either screen claimed anything was reaching a panel |
| a panel that needs attention | a simulator restarted with `--store-errors 2`: the chip went to the fault tone and read **`DESK · LIVE · 30 FPS · STORE ERRORS ›`**. This is the thing a second screen could have cost, and it does not |

**Both widths, both screens**, in a same-origin iframe harness (the extension's own
window cannot be resized; this is how cards 170 and 183 did it), each measured as well as
looked at - `innerWidth`, `matchMedia('(min-width: 1100px)')` and
`scrollWidth === clientWidth`:

| | 390 px | 1400 px |
|---|---|---|
| `/` | one column: stage 390 wide, then the inspector, meters last; bench **off**; no horizontal scroll | bench **on**: stage 1060, inspector 340 on the right, meters under the stage; no horizontal scroll |
| `/panel` | one column: Panel, Link + Device, Studio, in that order; no horizontal scroll | two columns: Panel 420 with a rule down its right, Link 620 beside it, Studio under the Link; the title block held to the same 1040 so the readouts sit over the columns rather than adrift |

**What I could not check, honestly.** The extension drives a window that is genuinely in
the background, so `document.hidden` was `true` throughout: `requestAnimationFrame` never
ran, **the canvas stayed black**, and the two-second status poll was (correctly) not
repeating - only the reads an action forces. So the *picture* was not judged by eye here,
exactly as in cards 120 and 170, and the owner's glance is that check. It also means one
thing that looks like a bug is not: with the tab hidden, the brightness slider kept a
stale value between actions, and a forced read put it right at once (`80`).

Cleanup: both tabs closed, both servers stopped, `ps` clean of anything of mine.

### Step 4 - the README, and the evidence

`crates/studio/README.md`: the "one page" paragraph is now the two screens and what is on
each, the status chip and the Panel screen asking for no frames; "Editing the UI"
describes six files, the module split and `common.js`'s rule; the truth-telling paragraph
gains 181, 197 and the brightness line; and the `tests/ui.rs` row says what it now pins.

Root `cargo test --release --no-fail-fast`: **742 passed, 0 failed, 1 ignored**.
`cargo clippy --workspace --all-targets`: **silent**. Neither of card 117's two known
flakes recurred, though another worktree was running its soak in a loop at the time.

### Acceptance, against the card

| the card asked for | where it is |
|---|---|
| Picture: title block, canvas, now playing, parameters, time, panel model, limiter, view, the meters | `ui/index.html`, in that order; `each_screen_is_in_the_order_it_should_stack_in` |
| brightness stays reachable there, as a compact control | `#sec-bright`: one slider, two lines of hint, the same binding the Panel screen uses |
| one quiet status chip in the title block: name, state, fps; the link to the Panel screen; a fault tone when the panel needs attention | `#ro-panel` as an `a.pill`; `panelState` + `attention` in `common.js`; seen red as `DESK · LIVE · 30 FPS · STORE ERRORS` |
| Panel: which panel, discovery, output switch, brightness, link facts, the Device block and its notes, state repairs, identify/rename/reboot, change which panel, and the studio's own health | `ui/panel.html` + `panel.js`; `the_two_screens_hold_what_the_split_says_they_do` names every id that moved |
| no canvas needed; a small static indication of what is playing | the title block's *Showing* / *Seed*; no `<canvas>`, no frame pump, `fps: 0` |
| each screen its own URL that survives a reload; the back button works | two documents at `/` and `/panel`; reload and back exercised in Chrome |
| a change on one screen shows on the other in another browser at once | two tabs: the switch on `/panel` changed `/`'s chip and its stage at once |
| the existing visual language, at 390 and 1400 px | `style.css`'s tokens unchanged; the table in step 3 |
| `ui.rs` serving and tests extended to both screens; the id cross-check covers both | `PAGES` has two entries; `every_element_each_screen_reaches_for_exists` checks three scripts against two screens |
| `crates/studio/README.md` updated | step 4 |
| card 197 to `done/` with a Log line pointing here | done, with its acceptance in step 2 |
| no API route, state schema or WebSocket message change | none: the only Rust outside `ui.rs` is two doc comments in `page.rs` and one line of `tests/api.rs` |
| nothing about devices, discovery, WiFi, heap or reboots on the Picture screen | asserted by id **and** by word in `the_two_screens_hold_what_the_split_says_they_do` |

**Not done, on purpose:** the picture itself was not judged by eye (see step 3); no new
card was needed. Two backlog cards (113, 121) name `ui/main.js` in their Context and will
want re-reading against the three files that replaced it.

### Orchestrator, after the merge (2026-09-20)

Reviewed the scope (UI files, `ui.rs`, two doc comments, one test line, README; no route,
schema or message change) and merged `--no-ff`. Root `cargo test --release --no-fail-fast`:
752 passed, 1 failed - the studio soak at `soak.rs:138`, "the panel comes back on the same
address": the re-bind of a fixed port while another worktree was running the same suite
(card 117, whose worker is on it now; the same line failed the same way for card 162's
worker). Alone: passes in 61 s. Clippy silent. Deployed to workbench with card 162; the
first look in a visible browser tab is the owner's.
