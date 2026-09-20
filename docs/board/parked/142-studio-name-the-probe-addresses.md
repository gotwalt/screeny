---
id: 142
title: Name the probe addresses from the command line, for a container with no broadcast route
type: build
hardware: no
depends: [141]
owner:
branch:
---

## Goal

Card 141 gave the studio a `GET_INFO` probe that follows a panel to a new address, and
`Config::probe_to` to say **where** it asks - empty means the subnet broadcast address of
every interface. Nothing outside the library can set it yet: there is no flag and no
environment variable, so a deployment where broadcast does not reach the panel has the
mechanism and no way to point it.

That deployment is not hypothetical - it is the one the studio is built for. A container
on a bridge network, or `workbench.local` with the panel on another segment, broadcasts
to its own subnet and nowhere near the panel.

## Context

- `crates/studio/src/lib.rs`: `Config::probe_to: Vec<SocketAddr>` (card 141) and its doc
  comment, which states the rule: empty = the subnet broadcasts and the probe follows
  `discover`; non-empty = ask exactly these control addresses.
- `crates/studio/src/fleet.rs`: `probing()` and `probe_targets()`.
- `crates/studio/src/main.rs`: the flag/env table, and `--no-discover`, which must keep
  turning the probe off.
- `crates/studio/tests/moved.rs` already drives `probe_to` from a test.

## Deliverables

- A flag and an environment variable - `--probe-to A,B,...` / `SCREENY_PROBE_TO`,
  addresses separated by commas, a bare IP meaning that IP on the default control port.
- `--no-discover` still turns the probe off, whatever was named.
- The parse tested in `main.rs`'s own tests (a good address, a list, a bare IP, a bad
  one), and the flag in `crates/studio/README.md`'s table and in the deployment notes.

## Acceptance

`screeny-studio --probe-to 192.168.7.255` on a machine whose own broadcast address is
something else probes the named address and nothing else, and says so once at startup.

## Log

### Parked 2026-09-20 (owner: "let's prune what doesn't need to be done"; focus is aesthetic work)

The Config field exists; the deployed container finds the panel by mDNS and by name. Add the flag the day a deployment has no broadcast route.
