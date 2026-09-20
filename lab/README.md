# lab - frame encoding lab (card 002)

Measures candidate pixel encodings for a 64x32 HUB75 LED panel fed one frame
per UDP datagram. The conclusions live in
[`docs/research/002-frame-encoding.md`](../docs/research/002-frame-encoding.md).

```
cargo run --release      # regenerates every number and PNG (~8 s)
cargo test --release     # encoders and decoders agree; nothing exceeds budget
```

Output lands in `lab/out/` (git-ignored): `results.md` with the full tables,
`sheet-<clip>.png` contact sheets of every codec on one frame, `zoom-<clip>.png`
close-ups of the best five, `temporal-dither.png`, and `payloads/` for the
Xtensa bench. A few representative sheets are copied into
`docs/research/img/`.

## Layout

| | |
|---|---|
| `src/dec/` | **the decoders.** `no_std`, no allocation, no floating point, integer only. This is the half that gets lifted into `crates/proto` for the firmware |
| `src/enc/` | encoders. Host-side, free to use f32 and to be slow |
| `src/content.rs` | synthetic test clips for the card's content classes |
| `src/panel.rs` | the LED panel model: gamma LUT, BCM bit depth, optional device-side temporal dithering |
| `src/metrics.rs` | panel-aware Oklab dE, SSIM, flicker, temporal-average error |
| `src/sheet.rs`, `src/font.rs` | PNG contact sheets and the 5x7 bitmap font |
| `src/bin/portal-mock.rs` | card 201: the 64x32 captive-portal screen. Renders the candidate layouts and the WiFi QR with the same `qrcodegen-no-heap` the firmware links, into `docs/research/img/201-portal-*.png`. `cargo run --release --bin portal-mock -- ../docs/research/img` |
| `nostd-check/` | compiles `src/dec/` as `#![no_std]` with no allocator. If it builds, the decoders are firmware-ready |
| `xtensa-bench/` | counts Xtensa instructions per decode under `qemu-system-xtensa`. See its `run.sh` |

## Adding a codec

1. Write the decoder in `src/dec/`, add a mode byte in `dec::mode` and a match
   arm in `dec::decode`. Keep it integer-only and allocation-free.
2. Write the encoder in `src/enc/`, implementing `enc::Codec`.
3. Add it to `enc::roster()`.
4. Add a round-trip test to `tests/roundtrip.rs` that pins the packing — the
   encoder and decoder are written separately on purpose, and that test is what
   stops a packing bug from being reported as a quality result.

`cargo build -p nostd-check` must still pass.

## Running the Xtensa instruction count

Needs the `esp` rustup toolchain, the Xtensa GCC from `~/export-esp.sh` (for
its linker), and `qemu-system-xtensa`.

```
cargo run --release        # writes out/payloads/
cd xtensa-bench && ./run.sh
```

It does not touch hardware.
