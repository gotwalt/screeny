---
id: 005
title: crates/proto - shared no_std wire types and decoders
type: build
hardware: no
depends: [004]
owner:
branch:
---

## Goal

The single shared definition of the wire protocol, usable unchanged by the firmware
(`no_std`, no alloc, no float, Xtensa) and by host tools. Everything else in the
build phase depends on this crate, so favour a small, boring, well-tested API that
lands quickly over cleverness.

## Context

- Spec: `docs/design/protocol-v1.md` (normative). Architecture: `docs/design/architecture.md`.
- Decoders exist and are measured: `lab/src/dec/` (`mod.rs`, `pal.rs`, `lz.rs`,
  `block.rs`). `lab/nostd-check` proves they build as `#![no_std]`. Lift the five v1
  codecs (`PAL5 0x02`, `PAL8_LZ 0x10`, `PAL4_LZ 0x11`, `BC1_DUAL 0x28`, `SOLID 0x7F`).
  Leave the lab-only ones behind.
- **One deliberate difference from the lab:** on the wire the codec id lives in the
  header `codec` byte and the payload does NOT start with a mode byte (spec section 4).
  So the API is `decode(codec: u8, payload: &[u8], dst: &mut Rgb888Frame)`.
  `lab/` stays frozen as it is; do not edit it.
- The lab's encoders stay out of this crate (they use float and alloc). Card 009 lifts
  them into `crates/screeny`. To let 009 and the lab cross-check, provide a tiny
  `std`-gated helper or test that shows a lab-encoded payload (minus its first byte)
  decodes identically here.

## Deliverables

`crates/proto` (package name `screeny-proto`), `#![no_std]`, `#![forbid(unsafe_code)]`
unless a measured need says otherwise, zero dependencies if practical:

- Constants: ports, magic, version, W/H/NPIX, MAX_PAYLOAD, codec ids.
- Frame types from architecture.md (`Rgb888Frame`, `IndexedFrame`).
- Header: parse (`&[u8] -> Result<Packet<'_>, Reject>`) and build (into `&mut [u8]`)
  for FRAME and CONTROL, including flags, `HAS_TS`, `len` validation, version/type
  checks exactly as the spec's MUSTs say. `seq` helpers (`newer`, gap).
- Control ops: request/reply encode+decode for every op in the spec, the telemetry
  struct (fixed layout, `len`-tolerant for future growth), error codes.
- DNS-SD TXT wire-format encode/parse for the keys in the spec (shared by mDNS and
  `GET_INFO`).
- Decoders for the five codecs. Every decoder must be total: **no input may panic,
  index out of bounds, loop forever, or write outside `dst`**. This code parses
  network input on a device with no MMU.
- Tests: golden byte vectors for every packet type; decoder vectors generated from the
  lab encoders (check them in as files under `crates/proto/tests/vectors/`, with a
  note on how they were produced); a randomized/mutation test that throws thousands of
  corrupt and truncated payloads at every decoder and the packet parser under
  `cargo test` (plain loops with a seeded PRNG are fine; no fuzzing infra needed).
- Prove `no_std`: `cargo build -p screeny-proto --target thumbv7em-none-eabi` (or any
  installed bare-metal target; add it with rustup if needed) must succeed, and say so
  in the README. If you can also build it for `xtensa-esp32-none-elf` with the `esp`
  toolchain (`. ~/export-esp.sh`), do, and record the result.
- Replace the "normative reference decoder" hand-waves in spec sections 4.4 and 4.5
  with actual prose + bit layouts, now that you have read the code closely. Edit
  `docs/design/protocol-v1.md` directly for that section only.
- `crates/proto/README.md`: the API in one screen.

## Acceptance

`cargo test -p screeny-proto` passes; the crate builds for a bare-metal target; a
firmware or sender author can implement against it without reading the lab.

## Log
