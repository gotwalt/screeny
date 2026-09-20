---
id: 173
title: "No panel yet" does not say whether the studio is even looking
type: build
hardware: no
depends: [170]
owner:
branch:
---

## Goal

A studio with no panel attached says **No panel yet**, opens "Change which panel", and
lists what it has found. If it has found nothing, the list is empty and the page looks
the same whether:

- discovery is on and the browse simply has not finished yet (the first thirty seconds
  of a fresh container, which is normal);
- discovery is on and mDNS is broken - avahi holding 5353, or Docker on macOS, which
  has no multicast at all (`studio-vision.md`, "Docker: what still bites");
- discovery is **off** (`--no-discover`), in which case nothing will ever appear and
  the only way in is to type an address.

The first is "wait a moment", the third is "you have to type something", and the page
does not tell them apart. `/api/v1/status` already knows: `discovery.enabled`,
`discovery.browses`, `discovery.last_browse_unix` and `discovery.last_error`.

This is the first thing a new person sees, and it is the one screen where being unclear
costs the most.

## Context

- `crates/studio/src/devices.rs`: `DiscoveryHealth`, already on `/api/v1/status`.
- `crates/studio/ui/main.js`: `showFound()` draws the chooser and opens it once when
  nothing is attached.
- Keep it to a sentence, not a diagnostic panel. Something like "Looking… (2 browses,
  nothing found yet)" / "Not looking for panels - type an address below" / "Looking for
  panels is not working here: <error>. Type an address below."
- A browse that finds nothing is **normal, not an error** (`crates/screeny/README.md`),
  so none of this may be drawn in the `bad` tone or reach `/healthz`.

## Deliverables

- One line under "No panel yet" that says which of the three it is.
- The same line, quietly, when a panel *is* attached and discovery is off - so a person
  wondering why their second panel never turns up has an answer.
- `tests/ui.rs`'s element check extended to it.

## Acceptance

Start a studio with `--no-discover` and no state: the page says it is not looking, and
says what to do instead.

## Log
