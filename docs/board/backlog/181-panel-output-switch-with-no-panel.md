---
id: 181
title: "Show it on the panel" is switched on when there is no panel
type: build
hardware: no
depends: [170, 173]
owner:
branch:
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
