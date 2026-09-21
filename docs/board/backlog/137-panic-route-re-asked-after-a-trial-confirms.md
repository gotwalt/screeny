---
id: 137
title: The Studio does not re-ask GET /api/v1/panic after it sees "trial", so an update confirming itself is not noticed until the next reboot
type: build
hardware: no
depends: [199]
owner:
branch:
---

## Goal

Spec `docs/design/protocol-v1.md` section 8.6 says a reader of `GET /api/v1/panic`
"asks once per `boot_id`, and again a minute or two later if it saw `trial`" - because a
trial confirming itself (`panic.update.outcome` going from `trial` to `confirmed`) is the
one other thing this route's answer can do besides a reboot. Card 199 built and tested only
the first half: `crates/studio`'s `Registry::want_panic`/`heard_panic` (`devices.rs`) never
re-ask for the same `boot_id`, so once a trial has been seen, the Studio's `/panel` and
`/api/v1/status` keep showing `"trial"` for the rest of that boot even after the device has
long since confirmed the image.

## Context

Card 199's own hard rules (from the coordinating session) asked for the simpler behaviour on
purpose - "the panic read happens once per new `boot_id` ... never on the 10 s poll" - and
the card's acceptance test proves exactly that ("read once per boot and not again"). This
card is the follow-up the 199 worker's Log flagged: build the spec's fuller rule without
breaking that test's guarantee (a device must still never cost more than one extra HTTP
connection per `boot_id`, except for this one deliberate re-check).

What is needed: after `want_panic` returns `false` because a panic read for this `boot_id`
already came back with `update.outcome == "trial"`, ask again once, a minute or two later
(spec says "a minute or two"; `screeny_provision::machine::Trial` and card 246's OTA model
in `crates/sim` may already have a concrete number worth matching), and then never again for
that `boot_id` whatever the second answer says (`confirmed`, or still `trial` if the device
is slow to confirm - the spec does not ask for a third read).

## Deliverables

- `crates/studio/src/devices.rs`: `Registry` tracks, per device, whether the last panic read
  was a `trial` and when it was taken, and a new predicate (or an extension of
  `want_panic`) that becomes true again once the wait has passed - once, not on a timer that
  fires repeatedly.
- `crates/studio/src/fleet.rs`: `status_once` asks the extra question the same way it asks
  the first one - sequential, on the same task, never a second connection in flight.
- A test against `screeny-sim`'s `SimHandle::model_ota` (`crates/sim/src/ota.rs`, off by
  default) proving the re-ask happens once, at roughly the right delay, and that
  `panic.reply.update.outcome` on `/api/v1/status` moves from `"trial"` to `"confirmed"`
  without a further device reboot.

## Acceptance

A simulator modelling an OTA trial (`SimHandle::model_ota`) shows `"trial"` on `/panel`
right after the (simulated) reboot, and `"confirmed"` a short wait later, with no second
`boot_id` and no more than the two panic connections the spec allows for that boot.

## Log
