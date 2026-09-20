---
id: 157
title: Two things in crates/art are called Output
type: build
hardware: no
depends: [150]
owner:
branch:
---

## Goal

`screeny_art::output::Output` (the frame-sink trait) and `screeny_art::pipeline::Output`
(card 150's name for the panel kind / dither / limiter block) share a name, and three files
import both and disambiguate by hand. Rename the sink trait to `output::Sink`.

## Context

Found by card 150's worker. Small and mechanical; only `crates/art` and `crates/studio`
implement the trait (`studio/src/player.rs` already imports it `as FrameSink`). Do it when
nobody else is in those crates - not while 151 or 155 is in flight.

## Acceptance

One `Output` in `crates/art`; tests and clippy clean; no behaviour change.

## Log
