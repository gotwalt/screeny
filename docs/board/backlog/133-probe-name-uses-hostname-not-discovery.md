---
id: 133
title: `screeny-probe --name` resolves a hostname instead of browsing
type: build
hardware: no
depends: [080]
owner:
branch:
---

## Goal

Give `screeny-probe` the same discovery `crates/screeny` has, so `--name`
means the same thing in both tools.

## Context

Card 080 gave the probe a `--name NAME` so the orchestrator can run the
conformance suite the way it runs everything else. It is not the same
mechanism:

- `crates/screeny` browses `_screeny._udp.local.` with `mdns-sd`
  (`src/discover.rs`), matches the DNS-SD **instance** name, and takes the
  frame port from the `SRV` record and the control port from the `ctrl=` TXT
  key - which is what spec section 5.4 says senders must do.
- `screeny-probe --name X` turns `X` into `X.local` and hands it to
  `to_socket_addrs`, which resolves the **host** name through the OS
  responder, and then assumes the default ports.

For the bench device the two agree: the instance is `screeny-4a00a4` and the
host is `screeny-4a00a4.local`, on the default ports. They stop agreeing the
moment a device is renamed with `SET_NAME` (which changes the instance, not
the host), or runs on non-default ports, or when two simulators are up.

The probe deliberately depends on `screeny-proto` and nothing else, which is
why this was not done in card 080: pulling in `mdns-sd`, or depending on
`crates/screeny`, is a real decision about what an instrument is allowed to
be. Worth making deliberately.

Options: (a) leave it, and document `--name` as "the mDNS *host* name";
(b) add `mdns-sd` to the probe and copy the twenty lines of browse;
(c) lift `crates/screeny/src/discover.rs` into a small shared crate both use.

## Deliverables

- The decision, and whichever of the three it is.
- `screeny-probe --help` and the module docs say what `--name` matches.

## Acceptance

`screeny-probe --name <instance> info` and `screeny discover --name <instance>`
find the same device, or the probe's help says plainly that they do not.

## Log
