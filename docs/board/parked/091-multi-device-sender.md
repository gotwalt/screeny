---
id: 091
title: Stream to several panels from one screeny process
type: build
hardware: no
depends: [009]
owner:
branch:
---

## Goal

Drive more than one panel from a single `screeny` process, which the protocol
already allows and nothing currently implements.

## Context

`docs/design/protocol-v1.md` section 11 closed "multiple devices from one
sender" as **sender-side only; no wire change**, and then no card picked it
up. Card 009 built `Sender` as one device, one socket, one encoder, one
pacer - which is right for one panel and is the only thing the CLI can ask
for today.

What makes this more than a loop:

- **Encoder state is per device**, because the codec set, the payload budget
  and the hysteresis incumbent all come from that device's `GET_INFO`. Two
  panels with different `mtu` values need two `Encoder`s even when they are
  showing the same frame.
- **The frame can often be encoded once.** Two devices advertising the same
  codecs and the same `mtu` can share a payload, and for a wall of identical
  panels that is the difference between one encode per tick and N. The
  encoder is 1.5 ms per frame (card 031), so four panels of unshared encoding
  would still fit 30 fps, but sharing is nearly free to implement: key the
  cache on `(codecs, budget)`.
- **One pacer, N sends.** The schedule is shared; the sends are not. Sending
  N datagrams back to back on one tick is fine - they go to different
  destinations - but the telemetry, adaptation and fps ladder are per device,
  and a device stepping down to 24 fps while another stays at 30 means the
  shared schedule has to become "send to this device every k-th tick".
- **Failure is per device.** One panel going away must not stop the others,
  and `BUSY` from one is not a reason to stop sending to the rest.
- Unicast only, still: section 1 forbids broadcast and multicast for frame
  data, so this is N unicast streams however it is arranged.

## Deliverables

- `Group` (or similar) in `crates/screeny`: several `Sender`s under one pacer,
  with the payload shared between devices whose codec set and budget match.
- `--addr` and `--name` accepting a list, plus `--all` to take everything a
  browse finds.
- Per-device lines in the live stats, and a summary that does not hide one
  panel's loss behind another's health.
- Tests against several in-process receivers at once, including one with a
  smaller `mtu` than the others and one that stops answering mid-stream.

## Acceptance

`screeny pattern bars --all` drives three in-process receivers at a steady
30 fps each, encoding once per tick when they are identical and twice when one
of them advertises a smaller `mtu`.

## Log
