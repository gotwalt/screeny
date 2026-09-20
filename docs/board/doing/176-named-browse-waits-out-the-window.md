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
