# Project docs

```
docs/
  design/       settled decisions. The source of truth when code and docs disagree.
  research/     how we got there: one file per question, conclusions first.
  media/        renders used by the README (see tools/render-media.sh).
```

## Design

| File | What it settles |
|---|---|
| [`design/protocol-v1.md`](design/protocol-v1.md) | The wire protocol: one frame per UDP datagram, the codecs, the control channel, discovery, telemetry. Normative. |
| [`design/architecture.md`](design/architecture.md) | How the crates fit together and why the split is where it is. |
| [`design/generative-art-brief.md`](design/generative-art-brief.md) | What the art system is for and the rules a patch has to live by (the panel's colour model, the wire budget, brightness safety). |
| [`design/studio-vision.md`](design/studio-vision.md) | Screeny Studio as a server-first web app that runs unattended. |
| [`design/deployment.md`](design/deployment.md) | Running the Studio as a Docker service on a Linux box. |
| [`design/device-web.md`](design/device-web.md) | The device's own web page, the captive-portal WiFi setup, safe over-the-air updates, and the button. |

## Research

Numbered in the order the questions came up. Each one opens with its conclusions.

| File | Question |
|---|---|
| `research/000-bench-notes.md` | Bench setup: serial, flashing, the camera. |
| `research/001-firmware-stack.md` | Can `no_std` Rust on embassy drive this hardware? What does the Tidbyt actually contain? |
| `research/002-frame-encoding.md` | How to fit a 64x32 frame into one datagram: the codec lab. |
| `research/003-protocol-transport.md` | UDP framing, pacing, discovery, what loss looks like over WiFi. |
| `research/004-first-bringup.md` | The first day on real hardware. |
| `research/005-end-to-end.md` | Measured results of the whole stack. |
| `research/006-flash-store-ota.md` | Partition table, a settings store in flash, over-the-air updates with rollback. |
| `research/007-device-web-and-portal.md` | An HTTP server, a soft-AP and a captive portal on the device, within the RAM budget. |
| `research/008-button.md` | Where the Tidbyt's button is wired and what to do with it. |
| `research/009-ram-headroom.md`, `research/010-stack-and-ram-levers.md` | Where the ESP32's RAM goes and how to get some back. |
| `research/010-numerals-rest-pose.md` | A design study for the numerals clock. |

**About the numbers you will see.** Development was tracked as numbered work items
("card 212", "card 080"), and the research docs, the design docs and many code comments
cite them. The numbers are stable identifiers for a piece of work and its evidence;
there is no separate document per number in this tree.

## Conventions

- `cargo clippy --workspace --all-targets` is expected to say nothing. An `#[allow]`
  carries a one-line reason beside it.
- Some crates are pixel-exact on the wire and have golden tests to prove it. In
  `crates/art` and `crates/demos` a clippy suggestion that reassociates float arithmetic
  is applied only when the rendered output is shown to be byte-identical.
- One implementation of each thing: wire format and decoders live in `crates/proto`
  and nowhere else.
- The spec is normative. If code and `design/protocol-v1.md` disagree, one of them has
  a bug; fix it and say which.
