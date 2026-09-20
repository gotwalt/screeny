---
id: 144
title: The preview and a player can fight over the same panel
type: build
hardware: no
depends: [106]
owner:
branch:
---

## Goal

The design view can be pointed at a panel ("Send to panel") that also has a player. Both
then stream to it, and the firmware gives the source lock to one sender at a time
(spec 7.4): the loser gets `BUSY`, and whichever sent last keeps taking it back. Nothing
crashes and nothing is lost, but the picture is whichever of the two won the last round,
which is nobody's intention.

## Context

Found while building card 106; left alone because deciding what *should* happen is a
product question, not a bug fix. Two candidates:

1. **The preview wins, loudly.** Pointing the design view at a panel pauses that panel's
   player and says so on both pages; taking it off resumes the player. "I am working on
   this panel" is what a human means by aiming the preview at it.
2. **The player wins, quietly.** The preview refuses a panel that has a player, and
   offers "Play my preview" instead - which is the explicit promotion card 106 built.

(1) matches how the design view is used on the bench; (2) is safer for a panel in a
room. Ask the owner.

## Deliverables

- Whichever is chosen, in one place: the preview's `set_panel` and the supervisor's
  `aim` are the only two things that open a link.
- Both pages say what is happening rather than showing a fight.
- A test with one simulator, a player and the preview aimed at it: the picture is one
  of the two, deliberately, and `SendStats::busy` stays at zero.

## Acceptance

Point the design view at the desk panel while it is playing something else, and it is
obvious which one is in charge.

## Log
