---
id: 163
title: parameters that are a list of choices are sliders with the list in their label
type: build
hardware: no
depends: [100, 106]
owner: worker-173
branch: card/studio-page-tells-the-truth
---

## Goal

A parameter whose values are named stops should say so, once, in a way the studio can
draw as a list rather than as a slider with the list crammed into its label.

## Context

`ParamSpec` is a number with a min, max, step and default, and several parameters are
really enumerations dressed as one. Their labels have grown to carry the key:

- `clocks-numerals`: `"Resting dials (0: as it was, 1: quiet, 2: hatched quiet, 3:
  hatched faint, 4: zigzag quiet)"` (card 160), and `"Choreography (0 = vary, 13 = always
  composed)"` - which does not even try to name the twelve.
- `clocks-dials`: `"Dials (0: 4x2, 1: 6x3, 2: 8x4)"`, `"Mood (0 = wander)"` - eight moods
  with names the piece already knows and the slider cannot show.

So the person moving the slider is reading a legend, counting stops, and in the `dance`
and `mood` cases guessing. The pieces already hold the names (`RESTS[i].name`,
`dance::named`, `Mood::new(..).name`).

Not urgent: every one of these works. It is worth doing once the studio's controls are
settled (card 106), and it would pay for itself in the clock pieces alone.

## Deliverables

- A `ParamSpec` that can carry named stops (an extra `choices: &'static [&'static str]`,
  empty for an ordinary number, is probably enough; the value stays an `f32` so nothing
  downstream changes).
- The studio draws those as a list or a segmented control, and `screeny-art list` prints
  the names.
- The clock pieces' labels go back to being labels.

## Acceptance

`screeny-art list` names every stop of `rest`, `dance`, `grid` and `mood`; the studio
shows names rather than numbers; no piece's behaviour changes.

## Log

### The card against today's tree

Unchanged, and the four labels the card quotes were word for word still there.
Card 170 changed which player the sliders act on, not how they are built.

### The design

Additive to `ParamSpec`, and **the value stays an `f32`** everywhere - wire,
state file, per-piece memory (card 165), `Params`, `sanitise`. Nothing
downstream of a piece knows the difference; what changed is which control the
page draws and what it says.

```rust
pub struct ParamSpec {
    // id, label, min, max, step, default: untouched
    pub choices: &'static [&'static str],  // named stops; empty for a number
    pub switch: bool,                      // off or on
}

pub const fn param(..)                            // a number -> slider (signature unchanged)
pub const fn choice(id, label, choices, default)  // a list   -> segmented control / select
pub const fn toggle(id, label, default: bool)     // 0 or 1   -> switch
```

`choice` derives `min: 0.0`, `max: choices.len() - 1`, `step: 1.0`, so a spec
cannot declare five names over a range of three. `param`'s signature did not
change, so the fifty-odd ordinary parameters were not touched at all.
`ParamSpec::name_of(value)` gives a stop's name, for anything that wants one.

**Which control, and why.** Three stops or fewer is a segmented control - the
design language already has `.seg`, and three fit across the inspector at
390 px. More than three is a `<select>`: fourteen choreographies in a flex row
would either wrap into a muddle or scroll sideways, and a select says the
chosen name at any width. `:root` gained `color-scheme: dark` so the select's
own native popup comes up dark like the rest of the page. A `switch` reuses
`.switch`, which the panel-model section already uses.

### The five declarations

| piece | param | was | is |
|---|---|---|---|
| `clocks-numerals` | `rest` | `"Resting dials (0: as it was, 1: quiet, 2: hatched quiet, 3: hatched faint, 4: zigzag quiet)"`, 0..4 | `choice("rest", "Resting dials", REST_CHOICES, 2.0)` - **the owner's example**: five named treatments |
| `clocks-numerals` | `dance` | `"Choreography (0 = vary, 13 = always composed)"`, 0..13 | `choice(.., DANCE_CHOICES, 0.0)` - "vary", the twelve dances by name, "composed" |
| `clocks-numerals` | `hours24` | `param(.., 0.0, 1.0, 1.0, 1.0)` | `toggle("hours24", "24-hour", true)` |
| `clocks-dials` | `grid` | `"Dials (0: 4x2, 1: 6x3, 2: 8x4)"`, 0..2 | `choice(.., GRID_CHOICES, 1.0)` - a segmented control |
| `clocks-dials` | `mood` | `"Mood (0 = wander)"`, 0..8, eight moods it could not name | `choice(.., MOOD_CHOICES, 0.0)` - "wander" and the eight by name |

`REST_CHOICES` is built **from `RESTS` itself** (`RESTS[0].name, ...`), so the
page and the piece cannot disagree about what a treatment is called. The dances
and the moods are generated with an rng, so their names cannot be read in a
const; those two lists are written out and checked against the pieces' own in
`piece::tests::the_named_stops_are_the_pieces_own_names`, which calls
`dance::dance(i, ..)` and `Mood::new(i, ..)` and compares every one.

Every id, range, step and default is exactly what it was, guarded by
`piece::tests::no_pieces_ids_ranges_or_defaults_moved` - which also asserts
that none of the five labels contains a `(` any more.

### What else

- `screeny-art list` prints every stop under its parameter (`0 = vary`,
  `1 = formation`, ...) and `off / on` for a switch. The card's acceptance.
- `bootstrap` carries `choices` and `switch` per parameter. Additive; a script
  reading `min`/`max`/`step`/`default` is unaffected.
- `crates/art/README.md`, "Adding a piece": use `choice`/`toggle` when a
  parameter's values have names, and take the names from the piece's own words.

### Evidence

```
$ screeny-art list
    dance            0 .. 13      default 0       Choreography
                                                  0 = vary
                                                  1 = formation
                                                  ...
                                                  13 = composed
    rest             0 .. 4       default 2       Resting dials
                                                  0 = as it was
                                                  1 = quiet
                                                  2 = hatched, quiet
                                                  3 = hatched, faint
                                                  4 = zigzag, quiet
    hours24          0 .. 1       default 1       24-hour
                                                  off / on
```

- `tests/ui.rs::a_parameter_that_is_a_list_of_choices_carries_its_names`:
  `bootstrap` gives `rest` the label "Resting dials" and exactly the five
  treatment names, `dance` fourteen stops, `hours24` `switch: true`, and `pace`
  - an ordinary number - an empty `choices`. Then `set_param rest=4` is 4.0 and
  `rest=9` clamps to 4.0, because a choice is still a number.
- `cargo test -p screeny-art --lib piece::` - five tests, green.

### Rendered

In Chrome, against a loopback simulator, every changed control exercised and
checked against `/api/v1/bootstrap` afterwards:

| control | shape | did |
|---|---|---|
| `clocks-numerals` **Resting dials** | select, 5 stops | picked "zigzag, quiet" -> server `rest: 4`, and the piece's own "now playing" note read `resting dials: zigzag, quiet` |
| `clocks-numerals` **Choreography** | select, 14 stops | picked "magnet" -> server `dance: 5` |
| `clocks-numerals` **24-hour** | switch | clicked off -> `hours24: 0`; clicked on -> `1` |
| `clocks-dials` **Dials** | segmented, 3 stops | clicked "8 x 4" -> server `grid: 2` |
| `clocks-dials` **Mood** | select, 9 stops | picked "streamlines" -> server `mood: 8`, and "Now playing" read **streamlines** |

The last two rows are the card's point in one line: the person picked a thing
by name and the piece said the same name back.

- `docs/research/img/18x-163-dials-choices.png` - the segmented "DIALS
  4 x 2 | 6 x 3 | 8 x 4" and the "MOOD [wander]" select.
- `docs/research/img/18x-w390-choices.png` - the same two at 390 px, where the
  three-stop rule earns itself.
- `docs/research/img/18x-w1400.png` - both selects and the switch in the
  340 px bench sidebar.

Console clean, no errors or warnings.

### The four widths

Measured in the browser at real viewport widths (the window would not resize in
this environment, so the page was loaded in a same-origin iframe of each exact
width - the media queries see the iframe's width, so the layout is the real
one):

| width | layout | horizontal overflow | anything wider than the viewport |
|---|---|---|---|
| 390 | scrolling column | no | none |
| 600 | scrolling column | no | none |
| 900 | scrolling column | no | none |
| 1400 | two-column bench | no | none |

Screenshots: `18x-w390-choices.png`, `18x-w600.png`, `18x-w900.png`,
`18x-w1400.png`.
