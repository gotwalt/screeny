---
id: 005
title: crates/proto - shared no_std wire types and decoders
type: build
hardware: no
depends: [004]
owner: worker (agent-ac8b379c)
branch: card/005-proto-crate
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

2026-09-19, worker on `card/005-proto-crate`.

**Shipped.** `crates/proto` (`screeny-proto`), `#![no_std]`,
`#![forbid(unsafe_code)]`, zero dependencies, four modules: `packet` (header,
FRAME/CONTROL parse+build, `newer`/`gap`), `control` (every opcode's request
and reply, telemetry, error codes), `txt` (DNS-SD TXT, `DeviceInfo`), `dec`
(the five codecs behind `decode(codec, payload, dst)`). The API is in
`crates/proto/README.md`, one screen of it.

**Tests: 38, about 2.5 s.** 16 golden (byte vectors written by hand from the
spec), 9 bit-layout (payloads built by hand from the section 4 prose), 5 lab
cross-check, 7 mutation, 1 doctest. The mutation tests throw ~260k corrupt
payloads at the decoders, 120k datagrams at the packet parser, 80k bodies at
the control ops, 60k LZ streams and 60k TXT records, all from a seeded
xorshift, and require that anything which parses also re-encodes to the same
bytes. Nothing panics, and `Rgb888Frame` being a fixed-size array means a
stray index inside a decoder is a debug-build panic, so "the suite passes" is
the out-of-bounds proof.

**Vectors.** 27, under `crates/proto/tests/vectors/`, produced by
`tools/gen-vectors` - a separate package that depends on `lab/` by path and
writes both the wire payload and the pixels the *lab's* decoder produced from
it. `lab/` is untouched. All five codecs covered; `SOLID` is hand-built
because the lab has no encoder for it (the measurement harness never picks the
degradation floor).

**Bare metal.** `thumbv7em-none-eabi` on `stable` builds. So does
`xtensa-esp32-none-elf` on the `esp` toolchain, but it needs
`-Zbuild-std=core` - the toolchain ships no prebuilt `core` for that target;
`lab/xtensa-bench/.cargo/config.toml` does the same thing via a config file,
and firmware/ will want to copy that.

**Two contracts tightened relative to the lab**, both now in the spec:

- A payload must be **exactly** its codec's size. The lab decoders accept
  trailing bytes; section 2.3 already says padding lives beyond `len`, so
  accepting it inside `len` only hides sender bugs. `lz::inflate` now returns
  bytes consumed so the LZ codecs can enforce it too. Every real lab payload
  still decodes, so this cost nothing.
- `lz::inflate` produces exactly the requested length or fails, rather than
  returning a short count for the caller to check.

**Spec edits** (all in this branch, all called out in the report): sections 4.4
and 4.5 replaced with actual prose and bit layouts; the exact-length rule added
to section 4; the `codecs=` example in 5.2 was `1,3,5`, which are not v1 codec
ids, now `16,17,40,2,127`; `GET_WIFI`'s `state` byte and `BUSY`'s `reason` byte
were undefined and are now assigned in 6.3; the section 10 worked example used
lab-only codec 3 and now uses `0x10`. Section 11 records 11-13 as closed.

**Surprises.** Two. First, `PAL5` and `BC1_DUAL` have no corrupt case at all -
every bit pattern of a correctly sized payload is a legal frame - so their only
failure mode is length, which makes the firmware's error accounting simpler
than expected. Second, the bit-layout tests written from my own prose passed
first try against the lifted code, which is the strongest evidence I have that
sections 4.4 and 4.5 now say what the decoders do.

**No new cards.** Nothing turned up that belongs to someone else.

