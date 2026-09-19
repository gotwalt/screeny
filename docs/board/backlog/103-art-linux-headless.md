---
id: 103
title: Run art/ headless on the Linux GPU box
type: test
hardware: no
depends: [100]
status: parked
owner:
branch:
---

## Goal

The end state is `screeny-art` running unattended on a Linux box with a GPU. That
path has never been run: wgpu's Vulkan and GLES-over-EGL backends do not exist on the
Mac it was built on. Parked until the owner says which box.

## Context

`art/README.md`, "GPU and 3D pieces". `WGPU_BACKEND=gl|vulkan` forces a backend; the
adapter is logged at start-up; device limits are held to `downlevel_defaults`.
`libc::localtime_r` is used for local time (`piece.rs`).

## Deliverables / Acceptance

`screeny-art pipe overland` and `pipe clocks-dials` hold the target frame rate with no
X or Wayland session; note the GPU, backend, driver and any fixes in the log. If the
GPU cannot hold 8x8 supersampling, record what it can.

## Log
