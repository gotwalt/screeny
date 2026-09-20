---
id: 147
title: "`screeny discover` tells a Linux container to open macOS System Settings"
type: build
hardware: no
depends: []
owner: worker-146
branch: card/146-147-153-sender-fixes
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

### Done, 2026-09-19 (worker-146)

`Error::hint()` is now a thin wrapper over a pure `Error::hint_for(Platform)`,
and `Platform::HOST` is the only `cfg` left in `crates/screeny/src/error.rs`.
`Platform` is `MacOs | Linux | Other`, `#[non_exhaustive]`, public so that a
test (and a caller printing advice for a machine that is not this one) can ask
for either text. Both texts are therefore compiled and asserted on *this* Mac -
the card's "neither branch can rot unnoticed", achieved by passing the platform
in rather than by `cfg`-ing the assertion, which would only have moved the rot.

What each says now, for the three errors that carry advice:

| error | macOS | Linux | Other |
|---|---|---|---|
| `NotFound` | System Settings paragraph + `dns-sd -B _screeny._udp` + escape hatches | bridge networking (`--network host`), multicast route (`ip link`/`ip maddr`), UDP 5353, `avahi-browse -rt _screeny._udp` as a second opinion + escape hatches | escape hatches only |
| `EHOSTUNREACH`/`ENETUNREACH` | "on a local network and the OS refused to route" + System Settings | no route from this namespace; `--network host` | check the interface and the route |
| `Timeout` to a private address | "did not answer ... `screeny discover`" + System Settings | the same first sentence, without the macOS paragraph | as Linux |

Three deliberate choices:

- **`--addr IP[:port]` and `--broadcast` are in all three.** They are the
  escape hatch on every platform and the card asked for them to stay in both.
  One `ESCAPE` constant, asserted present for every variant.
- **The Linux text says avahi is not required.** It is the thing an operator
  will otherwise assume from being told to run `avahi-browse`: this crate
  browses with `mdns-sd`, which binds 5353 itself (`SO_REUSEPORT`), so avahi is
  a second opinion and never a dependency. It names what is actually wrong in
  a container instead - the default bridge carries no LAN multicast.
- **The Linux text names no Apple tool and the macOS text no Linux one**, and
  the test asserts the absence both ways, including that the Linux advice
  contains no "mac" at all.

Also changed, because the old wording was the same bug in prose:

- `crates/screeny/tests/loopback.rs` asserted the timeout hint contains "Local
  Network", which would fail on Linux. It now asserts the platform-independent
  half (`screeny discover`) unconditionally and the macOS/Linux halves under
  `cfg`, which is the shape the card asks for in a test that cannot use
  `hint_for`.
- `crates/screeny/README.md`: the "a browse that finds nothing is normal" bullet
  and the API sketch (`hint_for`, `Platform`).
- `docs/design/deployment.md` verification (a) told the operator to *ignore* the
  advice the container prints. It is now the Linux advice, so the paragraph says
  so and keeps the `avahi-browse` check as the host-side second opinion.

Evidence: `cargo test -p screeny --lib` 9 passed (3 of them new here),
`--test loopback` 10 passed, `--test cli` 13 passed.

### Orchestrator (2026-09-20)

Merged to `main` cleanly. `cargo test --release -p screeny -p screeny-art`: green (203 passed across the
run). The only failures seen were two timing-sensitive `crates/studio/tests/fleet.rs` tests that fail
about one run in four on a loaded host and pass alone - not this branch; handed to card 170, which owns
those tests. Decisions accepted as made: resolver before browse for `.local`; `mean_bytes` keeps `FINAL`,
`mean_encode` drops it. Reaches the deployed Studio with the next deploy (card 170).
