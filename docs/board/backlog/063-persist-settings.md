---
id: 063
title: persist SET_BRIGHTNESS, SET_IDLE and SET_NAME across reboot
type: build
hardware: yes
depends: [008, 014]
---

## Goal

Make the three settings protocol-v1 section 6.3 says persist actually persist.

## Context

Section 6.3, note 3: "`SET_BRIGHTNESS`, `SET_IDLE` and `SET_NAME` persist across
reboot." Card 008 implements all three at runtime and none of them across a
reboot - the card said persisting brightness was optional, and the other two came
along for the ride. Today a device reboots to brightness 96, idle mode `STATUS`
and the name `screeny-<id>`, silently discarding whatever a user set.

This is small work on top of card 014, which is already bringing up
`esp-storage` + `sequential-storage` on a dedicated flash partition for Wi-Fi
credentials. The three settings are 1 + 1 + 33 bytes. The interesting parts are
not the bytes:

- **Write amplification.** `SET_BRIGHTNESS` is the one a slider sends sixty times
  a second. It must not reach flash sixty times a second. Debounce (a few
  seconds of quiet before a commit) and skip a write when the value is unchanged.
- **`ERR_STORAGE` exists** (section 6.5) and is currently never returned by
  anything. A failed commit should use it rather than pretending the setting
  took.
- `SET_NAME` also changes the mDNS instance name, which card 008 already
  re-announces; a persisted name must be in the record on the next boot too.

State lives in `firmware/src/receiver.rs` (`Core::brightness`, `idle_mode`,
`name`), and brightness is mirrored into the `BRIGHTNESS` atomic for the display.

## Deliverables

1. The three settings loaded at boot before the first frame is composed, and
   committed with debounce after a change.
2. `ERR_STORAGE` on a failed commit.
3. A note in the card 008 log's "left undone" marked done.

## Acceptance

Set a name, an idle mode and a brightness over the control port, `REBOOT`, and
`GET_INFO` / `TELEMETRY` / the panel all come back with what was set. A 60 s
brightness sweep at 30 changes a second produces at most a handful of flash
writes.

## Log
