---
id: 181
title: "Show it on the panel" is switched on when there is no panel
type: build
hardware: no
depends: [170, 173]
owner: worker-180
branch: card/180-device-http-status
---

## Goal

With no panel attached, the panel section reads:

```
PANEL                                    [ NO PANEL ]
NO PANEL YET
Nothing is being sent. The picture above is what the panel will show when one is found.
Not looking for panels: this studio was started with --no-discover. …
(•==)  Show it on the panel
```

The switch is **on**. Two lines above it the page says nothing is being sent,
and the pill says there is no panel. So the one control in that section whose
name is a promise is making one it is not keeping.

It is not wrong in the machine's terms - `state.on` is the unbound player's
`on`, which is true, and it means "start driving the panel the moment one is
attached" - but nobody reads a switch that way. Card 170's standard is that
there is no control on the page whose effect on the panel is unclear.

## Context

- `crates/studio/ui/main.js`: `outSwitch` is bound to `state.on`;
  `panelPill()` already knows there is no panel (`attachedId()` is empty).
- `crates/studio/src/player.rs`: `StoredPlayer.on` defaults to true, and the
  unbound page player carries it until a panel is adopted - which is what
  makes the first panel found start playing without anybody pressing anything.
  **That behaviour should not change**; only what the switch says about it.
- Found while doing card 173, which put the "not looking" line right above it
  and made the contradiction impossible to miss. Screenshot:
  `docs/research/img/18x-173-not-looking.png`.

## Deliverables

Pick one and say why in the commit:

- disable the switch while nothing is attached, with the hint saying that
  whatever panel is attached will be driven straight away; or
- keep it live and change its label while nothing is attached ("Drive a panel
  as soon as one is found"), so what it promises is what it does.

Whichever, `set_panel {"on":...}` keeps its shape and behaviour exactly: a
script uses it to borrow the panel for firmware tests.

## Acceptance

Start a studio with `--no-discover` and no state: no control in the panel
section claims something is reaching a panel. Attach one: the switch means
what it says, and `{"on":false}` / `{"on":true}` still work unchanged.

## Log

- **2026-09-20, worker-180.** Folded into card 180's branch: same section of the page,
  and both change what that section says rather than what it does.

  **Chose the second option: keep it live, change what it says.** With no panel
  attached the switch reads *"Drive a panel as soon as one is found"*; with one, *"Show
  it on the panel"*.

  Why not disable it. The switch is not decorative when there is no panel - `state.on`
  on the unbound player is precisely what makes the first panel found start playing
  without anybody pressing anything, which is the zero-click case the owner asked for.
  Greying it out would take that choice away from the one person who might want to make
  it before plugging anything in, and the page would *still* have had to explain the
  behaviour in a hint somewhere. Relabelling puts the explanation in the control, which
  is card 170's standard read forwards: a control whose name is what it does.

  One line of HTML (`<span id="panel-out-label">`) and three of `main.js`, inside
  `showPanel()` where the pill and the help line are already decided. `set_panel`'s two
  bodies are untouched - the change handler still posts `{on:true,to:''}` and
  `{on:false}` - and `tests/panel.rs::set_panel_hands_the_panel_over_and_takes_it_back`,
  which asserts on what the *device* sees, passes unchanged.

  Test: `tests/ui.rs::the_output_switch_says_what_it_does_when_there_is_no_panel` pins
  both labels, that the switch is never disabled, and the exact `set_panel` bodies.

  Evidence: `docs/research/img/181-no-panel-switch.png` - a studio started with
  `--no-discover` and no state, in Chrome. The section reads `PANEL [NO PANEL] / NO
  PANEL YET / Nothing is being sent… / Not looking for panels… / (•==) Drive a panel as
  soon as one is found`. Nothing in it claims anything is reaching a panel.

### Orchestrator: merged, deployed, verified on the live service (2026-09-20)

Merged to `main` cleanly. Root `cargo test --release --no-fail-fast`: 685 passed, 0 failed; clippy silent.
Deployed to workbench (state backed up first). Nine seconds after the deploy the Studio had read the real
panel: firmware 0.4.3, slot `ota_0` `valid`, reset `power_on`, heap 45612/90112, stack_free 20272,
`wifi_state connected`, `store_errors 0`, every warning flag false, `http: {absent: false, reads: 1}`;
one log line (`serves its own status API: firmware 0.4.3, slot ota_0`). Checked on the live container,
without printing it: the SSID is in `/api/v1/status` and in neither the container log nor
`/data/state.json` (0 matches each). The orchestrator no longer polls the panel's port 80 by hand - the
Studio is the one reader.
