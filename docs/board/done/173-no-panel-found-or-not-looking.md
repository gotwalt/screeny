---
id: 173
title: "No panel yet" does not say whether the studio is even looking
type: build
hardware: no
depends: [170]
owner: worker-173
branch: card/studio-page-tells-the-truth
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

### The card against today's tree

The Context still holds. Card 170 rewrote the page around one panel, but
`showFound()` is still there, still opens the chooser once when nothing is
attached, and still draws an empty list for all three reasons.
`DiscoveryHealth` is still on `/api/v1/status` unchanged, so **no API change
was needed for this card** - only the page reading what was already there.

### What I did

- `ui/index.html`: one `#discovery-note` line in the panel section, right under
  `#panel-name` / `#panel-help`, `hidden` until there is something to say. It
  is a `.hint`, so it is drawn in the dim tone and never in `bad`: a browse
  that finds nothing is the normal case (`crates/screeny/README.md`) and none
  of this reaches `/healthz`.
- `ui/main.js`: `discoveryLine(attached)` turns `status.discovery` into one
  sentence, and `showPanel()` puts it up. The five answers:
  - discovery off, nothing attached - "Not looking for panels: this studio was
    started with --no-discover. Type an address under “Change which panel”."
  - discovery off, a panel attached - the same fact, quietly, without the
    instruction: this is the card's second deliverable, for somebody wondering
    why their *second* panel never turns up.
  - `last_error` set - "Looking for panels is not working here: <error>. Type
    an address…" (avahi holding 5353; Docker on macOS).
  - `browses == 0` - "Looking for panels…" - the first thirty seconds.
  - browsed and still empty - "Looking: 3 browses, nothing found yet. Type an
    address…", and with something found, "Looking: 3 browses, 2 found."
  - attached, discovery on and working - **nothing**. No line for the case
    where there is nothing to explain.
- `tests/ui.rs`: `the_page_can_say_whether_it_is_looking_for_panels` (the
  element exists, the script branches on all three of `enabled`, `last_error`
  and `browses`, and the line is never given the `bad` tone) and
  `status_says_whether_discovery_is_on` (`test_config` is `--no-discover`, so
  `discovery.enabled` is false, `browses` 0, `last_error` null, and `ok` is
  still true).

### Rendered

A studio with `--no-discover` and no state, in Chrome against a loopback
simulator (the card's acceptance, exactly):

- `docs/research/img/18x-173-not-looking.png` - **"NO PANEL YET"** and, under
  it, "Not looking for panels: this studio was started with --no-discover.
  Type an address under “Change which panel”." The chooser below it is already
  open, because nothing is attached.
- With the simulator then attached, the same line becomes the quiet form -
  "Not looking for other panels: this studio was started with --no-discover."
  - and is visible in `18x-171-reconnects.png` and `18x-w900.png`.

Both are in the dim `.hint` tone; `/healthz` stayed 200 and `status.ok` true
throughout. Console clean.

### Orchestrator (2026-09-20)

Merged to `main` cleanly (with cards 176/125/186 already there). Root `cargo test --release
--no-fail-fast`: 671 passed, 0 failed; `cargo clippy --workspace --all-targets`: silent. Deployed to
workbench (state backed up first; v3 file untouched, `repaired: []`): the panel was back on `overland`
seed 4242 at once; `/api/v1/status` reports `gpu: Intel(R) Graphics (RPL-P), Vulkan`; `bootstrap` marks
`overland`/`lattice`/`knot` as `needs_gpu` and carries the named stops (`rest`: five treatments, `dance`:
fourteen, `hours24`: switch).
