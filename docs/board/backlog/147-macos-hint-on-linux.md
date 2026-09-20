---
id: 147
title: "`screeny discover` tells a Linux container to open macOS System Settings"
type: build
hardware: no
depends: []
---

## Goal

The "nothing found" advice fits the machine it is printed on.

## Context

Found while containerising the studio (card 107). `crates/screeny/src/error.rs`,
`Error::hint()`, attaches the macOS Local Network paragraph to every
`Error::NotFound` and to `EHOSTUNREACH`/`ENETUNREACH`, with no `cfg`. It is excellent
advice on the bench Mac - it is spec 9.3's named failure mode and it has saved real
time - and it is wrong everywhere else. Run inside the studio's container:

```
$ docker exec screeny-studio screeny discover
no devices found.

If this Mac is refusing local network access, open System Settings > Privacy &
Security > Local Network and enable the terminal or binary you are running, ...
... run `dns-sd -B _screeny._udp`, which uses Apple's own responder.
```

There is no Mac, no System Settings and no `dns-sd`. This is not cosmetic: `screeny
discover` inside the container is the documented check for "does mDNS work next to
the host's avahi?" (`docs/design/deployment.md`, verification (a)), so the first time
anyone runs it on workbench and gets an empty answer, the program sends them to a
settings panel that does not exist instead of to `avahi-browse`.

## Deliverables

- `Error::hint()` picks its advice per platform. macOS keeps exactly what it says
  now. On Linux the equivalent is the useful one: `avahi-browse -rt _screeny._udp`
  to ask the host's own responder, whether the process is in a container or a
  network namespace that cannot see multicast, and the same `--addr` / `--broadcast`
  escape hatches which are platform-independent and should stay in both.
- A test per platform that the hint names tools that exist there. `#[cfg]` on the
  assertion, not on the test, so neither branch can rot unnoticed.

## Acceptance

`screeny discover` with nothing on the network prints advice a person on that
platform can follow. `cargo test -p screeny` covers both branches.

## Log
