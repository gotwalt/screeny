---
id: 159
title: The Panel screen asks inline too - no prompt(), no confirm()
type: build
hardware: no
depends: [151]
---

## Goal

Three things on the Panel screen still stop the page with a browser dialog:

- `window.prompt('What should this panel be called?', …)` - **rename** (`panel.js`);
- `window.confirm('Reboot …?')` - **reboot**;
- `window.confirm('Forget …? Its player goes with it.')` - **forget a panel**.

Card 151 ruled those out for the settings control it built, for reasons that are not
about settings: a `prompt()` cannot be driven by the browser tooling (so the control
cannot be checked at all), cannot be styled, blocks the page and its socket, and on a
phone is a system sheet that looks like nothing else on the screen. The same three
reasons apply here. Make them inline in the same shapes card 151 used - a name field
beside the control, an inline confirm with the two words spelled out - and then
`tests/ui.rs` can hold **both** screens to "no `prompt(`, no `confirm(`" instead of only
the Picture screen.

## Context

- `crates/studio/ui/panel.js`: `rename` (around the `window.prompt`), `reboot` and the
  per-device "forget" button in the device list.
- The shapes and the CSS already exist: `.setting__name` (a label, an input, a confirm
  button and a quiet Cancel) and `.setting__confirm` (a sentence, a loud button, a quiet
  one) in `style.css`, and card 151's `openName` / `openConfirm` in `picture.js`. If the
  second use makes the pattern worth sharing, it belongs in `common.js` - which may
  reach for no element by id but `#notice`, so it must be *handed* its elements (card
  198's rule).
- Reboot keeps its second lock either way: `POST /device/reboot` requires
  `confirm: true` in the body (card 195), and that is not what this card is about.

## Deliverables

- `crates/studio/ui/panel.html`, `panel.js`, `style.css`: the three asks, inline.
- `crates/studio/tests/ui.rs`: the "no `prompt(`/`confirm(`" check covers `panel.js` too -
  delete the exemption card 151 left there, which names this card.
- `crates/studio/README.md`: the sentence about inline asks stops being about one screen.

## Acceptance

Rename a panel, reboot one and forget one, each from the page, with the browser tooling
driving it - which is itself the point, because today it cannot. Console clean, 390 px and
1400 px, and a second browser sees the outcome of each.

## Log
