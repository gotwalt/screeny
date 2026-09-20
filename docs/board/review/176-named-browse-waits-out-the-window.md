---
id: 176
title: A browse for a named instance waits out the whole window after it has found it
type: build
hardware: no
depends: [146]
owner: worker-176
branch: card/176-125-browse-and-clippy
---

## Goal

Finding the device you asked for by name is as quick as finding the first
device on the network.

## Context

Found while doing card 146, in code that predates it. `Target::browse_for`
(was `Target::resolve`) asks for:

```rust
browse(timeout, if want.is_none() { Some(1) } else { None })
```

`want: Some(1)` is "stop as soon as one instance resolves"; `None` is "collect
until the window closes". So an unnamed target returns the moment a device
answers - typically tens of milliseconds - and a **named** one always takes the
full `Target::timeout`, three seconds by default, even though the instance it
wants usually resolves in the first few hundred milliseconds. The name is only
matched after the browse has finished.

It is paid every time, not once: `Link` re-resolves its target on every
reconnect (`Aim::Find`), so a link named by instance - which is the *preferred*
way to name a panel, because it follows a DHCP lease - spends three seconds on
every reconnection attempt where an address-pinned link spends none. The studio
reconnects by name.

Collecting the whole window is not pointless: `Error::NoSuchDevice` lists the
names that *did* answer, which is a good error. So the fix is to stop early
only when the wanted instance has resolved, and to keep browsing otherwise.

## Deliverables

- `browse` learns "stop when this predicate matches", or takes the wanted name,
  so a named browse returns as soon as that instance resolves and only waits
  out the window when it does not. `crates/screeny/src/discover.rs`.
- The `NoSuchDevice` error keeps listing every name that answered, which is
  only possible on the path that did wait out the window - so the two cases
  must stay distinguishable.
- A test, against `screeny-sim` advertising over mDNS if that can be done
  hermetically on this bench; otherwise a unit test of the stop condition with
  the browse faked, plus a measurement in the log.

## Acceptance

`screeny --name <instance> info` against a device that is advertising returns
in well under the browse window, and against one that is not still waits the
window out and lists what did answer.

## Log

### Done, 2026-09-20 (worker-176)

**The rule, decided and written down.** The three ways a device can answer to a
name are not equally knowable, so they do not get the same treatment:

| `--name` matched | unique? | when it can be decided |
|---|---|---|
| the DNS-SD **instance name**, in full | yes - RFC 6762 §9 conflict resolution makes it unique on a link | the moment that instance resolves |
| the friendly **`name=`** in the TXT record | no - two panels can be given one name | only when the window closes |
| a **prefix** of an instance name | only if nothing else starts with it | only when the window closes |

So a browse for a full instance name stops the moment that instance resolves,
and nothing else stops early. That is the whole of the fix, and it is the case
the studio is in: it reconnects to `screeny-4a00a4` by instance name.

This also settles the "first match vs unique prefix" contradiction the card
warned about. The old code's predicate was one `find` over an OR of the three,
so the *alphabetically first* device matching any of them won, and an ambiguous
prefix silently picked one. Now `pick` takes the strongest kind of match
present (instance, then friendly, then prefix) and, if two devices tie on that
kind, returns the new `Error::AmbiguousName` - "`screeny-4a00` matches more
than one device (screeny-4a00a4, screeny-4a00b7); name one of them in full" -
rather than guessing. An exact instance name can never reach that error, so
naming a panel in full always works. `Error` is `#[non_exhaustive]`, so the new
variant is not a breaking change.

**Before and after.** Measured on `collect_until`, the browse loop itself,
driven by a scripted stream of resolves - no daemon, no multicast, nothing that
could see or be seen by the panel on this bench (the card allowed a real
`SimDevice` only if the real panel could be kept out of it; this keeps it out by
construction). Three devices answer at 20/60/100 ms into the default 3 s window
and the one wanted is the second:

| browse | returns after |
|---|---|
| named, before (`want: None`, collect the window) | **3.006 s** |
| named, after (stop at that instance) | **0.070 s** |
| unnamed / collect-everything, unchanged | 3.008 s, all three devices |

A ~43x cut on every reconnect, and the studio re-resolves on every one of them.

**Still paid, deliberately.** A name nothing answers to waits the window out, so
`Error::NoSuchDevice` can still list every instance that did answer - tested.

**Code.** `crates/screeny/src/discover.rs`:

- `collect_until(timeout, next, stop)` is the loop, over any source of a new
  private `Step` (`Resolved` / `Other` / `Done`). Split out precisely so the
  stop condition is testable without a network.
- `browse_until` is `collect_until` driven by a real `ServiceDaemon`.
- `pub fn browse_for_name(timeout, want)` is the new entry point: stop when
  `want` is a key in what has resolved. `pub fn browse(timeout, want:
  Option<usize>)` is unchanged in signature and behaviour.
- `match_kind` / `Match` / `pick` are the matching rule, pure and unit-tested.
- `Target::browse_for` now picks `browse_for_name` for a named target and
  `browse(_, Some(1))` for an unnamed one.

**Tests** (`discover::tests`, all hermetic):
`a_named_browse_returns_when_that_instance_answers` (the table above, and that
an unnamed browse still returns every device),
`a_name_nothing_answers_to_still_costs_the_window` (window paid, names listed),
`a_prefix_must_be_unique_but_a_full_instance_name_never_is_ambiguous` (the
precedence and the new error).

**Docs.** `crates/screeny/README.md`: the API summary, the `--name`/`--timeout`
rows, and a new bullet in the notes stating the rule. `Target::name`'s doc
comment and `browse_for_name`'s carry it in the code.

One `#[allow(clippy::large_enum_variant)]` with a reason on `Step`: it is a
return value that is matched and dropped immediately, so boxing the `Device`
would buy an allocation per resolve and save nothing.

`cargo test -p screeny`: green (31 tests across lib, integration and doc tests).
