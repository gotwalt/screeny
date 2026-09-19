---
id: 064
title: the device is screeny-4a00a4.local now, not screeny.local - decide and fix the docs
type: design
hardware: no
depends: [008]
---

## Goal

Pick one host name and make every document say it.

## Context

Card 007's firmware answered to **`screeny.local`**, and that name is written
into `docs/research/000-bench-notes.md`, several card files, and the status
screen. Protocol-v1 section 5.1 says the host name is
`screeny-<xxxxxx>.local.`, where `xxxxxx` is the MAC suffix, so card 008
implemented that and this unit is now **`screeny-4a00a4.local`**.
`screeny.local` no longer resolves at all.

That was the right call against the spec and the wrong call against everyone
else's muscle memory, so it needs settling rather than leaving as a surprise:

- The spec's reason for a per-device host name is that two panels on one LAN
  must not collide, and they would.
- The counter-argument is that one panel is the normal case and
  `screeny.local` is what a human types.
- `edge-mdns`'s `Host` carries a single host name, so answering both would mean
  two responders or a custom handler. Worth an hour to find out which.
- The *instance* name (what `dns-sd -B` lists, and what `SET_NAME` changes) is
  already separate from the host name, so a friendly name is not the issue.

Card 008's own status screen shows the host name on its fourth line, so whatever
is decided is visible on the panel without a serial cable.

## Deliverables

1. A decision, written into `protocol-v1.md` section 5.1 if it changes anything.
2. If both names are to work: the firmware change, and a check that macOS
   resolves both.
3. Either way: every mention of `screeny.local` in `docs/` updated, and a line in
   `docs/research/000-bench-notes.md` under **Device** giving the current name.

## Acceptance

`grep -r screeny.local docs/` returns nothing that is wrong.

## Log
