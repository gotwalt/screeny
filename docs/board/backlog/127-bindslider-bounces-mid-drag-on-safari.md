---
id: 127
title: bindSlider's parameter sliders bounce mid-drag on Safari, the same bug card 126 fixed for brightness
type: build
hardware: no
depends: [126]
owner:
branch:
---

## Goal

Found while fixing card 126 (the brightness slider bouncing), not fixed there because the
card scoped the fix to `bindBrightness` only. `bindSlider` (`crates/studio/ui/common.js`,
~line 380) has the identical defect: its `refresh()` is

```js
return { refresh() { if (!busy(input)) { input.value = get(); } show(); }, show };
```

`busy(el)` is `el === document.activeElement`. **Safari - desktop and iOS both - does not
focus an `<input type="range">` on a click or a touch, only on Tab.** So on Safari,
`busy(input)` is `false` for the whole of a drag, and `refresh()` will happily overwrite
`input.value` from `get()` while the person still has hold of the thumb, every time
something calls it. `picture.js`'s own comment on `sync()` promises the opposite: "a
slider being dragged here keeps its grip" - a promise `busy()` alone cannot keep on Safari.

`bindSlider` binds every patch parameter slider (`paramControl` in `picture.js`), plus the
rate slider (card 183), the speed slider (card 197), and the APL/rise/dot/bloom sliders -
anything with a numeric range and no named stops. `refresh()` on all of them is called from
`sync(next)` whenever a `state` message arrives that is not this browser's own change (the
server already excludes a browser's own edits by `CLIENT` id - see `common.js`'s `CLIENT`
and its doc comment - so the trigger is specifically **a second browser, a second tab, or
the CLI changing the same patch's parameters while this one is mid-drag**, not a lone
person's own gesture fighting itself).

## Context

Card 126's Log has the confirmed cause and the fix pattern for the same defect in
`bindBrightness`: a `holding` flag set on `pointerdown`/`touchstart`/`keydown`/`input` and
cleared on `change` (with `pointerup`/`pointercancel`/`blur` as a backstop for a gesture
that never fires `change`), checked by `show()`/`refresh()` alongside `busy(input)` rather
than instead of it. Reuse that pattern rather than inventing a second one - `bindSlider` and
`bindBrightness` should end up agreeing on what "the person has hold of this control" means,
even if the code is not shared line for line (`bindBrightness` has its own hold-value logic
on top that `bindSlider` does not need: a slider's `get()` is the page's own optimistic
policy state, no device round trip to go stale, so only the drag-detection half applies
here, not `brightnessHoldWins`).

Card 126 did not fix this itself: its card scoped changes to `bindBrightness`, one control,
and the brightness fix's own review was already two rounds; a `bindSlider` fix touches every
patch's parameter sliders, the rate slider, the speed slider and the limiter sliders, which
is a wider blast radius that deserves its own card, its own before/after check, and its own
review.

## Deliverables

- `bindSlider`'s `refresh()` (and anywhere else `busy(input)` alone gates a slider's own
  overwrite) gets the same `holding` treatment as `bindBrightness`.
- Whatever `crates/studio/tests/ui.rs` can pin without a browser - `bindSlider` has no
  device-round-trip hold to test the way `brightnessHoldWins` was, but the wiring (the
  drag-tracking listeners exist, `refresh()` checks both `busy` and the flag) can still be
  checked textually, the way this file already checks other things about `common.js`.
- Confirm which other `busy(el)`-gated controls in `common.js` share the defect while this
  card is open (`dropdown`'s `select.value` write in `picture.js` was checked in card 126's
  review and does not - a `<select>` is not dragged - but it is worth one more pass with
  fresh eyes rather than assumed).

## Acceptance

Against a real browser (Safari, since that is the one this bug is specific to): drag a
patch parameter slider (or the rate/speed slider) while a second tab changes the same
parameter, and the dragged slider does not jump.

## Log
