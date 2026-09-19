# Decoder test vectors

Checked in on purpose: they are the only thing tying `screeny-proto`'s
decoders to the encoders card 002 measured, and they let a firmware or sender
author check an implementation without building `lab/` at all.

## Files

For each vector `<name>`:

| file | contents |
|---|---|
| `<name>.bin` | the **wire** pixel payload: what goes in a `FRAME` datagram at offset 8, with no codec/mode byte |
| `<name>.rgb` | 6144 bytes, the RGB888 frame the **lab's own decoder** produced from that payload |

`manifest.tsv` is `name`, decimal codec id, payload length, and a sentence
saying what encoder and content produced it. It is what the tests read; the
file names are only for humans.

27 vectors, covering all five v1 codecs:

| codec | id | vectors |
|---|---|---|
| `PAL5` | 2 | 8 |
| `PAL8_LZ` | 16 | 5 |
| `PAL4_LZ` | 17 | 3 |
| `BC1_DUAL` | 40 | 8 |
| `SOLID` | 127 | 3 |

Content is the first frame of the lab's `plasma`, `textui`, `darkfade` and
`photo` clips, plus synthetic frames of exactly 2, 16, 100 and 256 distinct
colours in runs of four pixels, which is how the palette ladder is steered onto
a chosen rung. `SOLID` has no lab encoder - the mode is the
graceful-degradation floor, not something the measurement harness ever picks -
so those three are hand-built.

## How they were produced

`tools/gen-vectors` is a small package that depends on `lab/` by path and does
nothing else. `lab/` is frozen reference material and is not modified.

```
cd tools/gen-vectors && cargo run --release
```

For every vector it encodes a frame with a lab encoder, drops the leading mode
byte (on the wire the codec id lives in the packet header, not the payload -
spec section 4), decodes the full lab payload with the **lab's** decoder, and
writes both files. `tests/lab_vectors.rs` then asserts that `screeny-proto`
reproduces those pixels exactly, that truncating any vector never decodes, and
that appending a byte to any vector is rejected.

Regenerating should be a no-op unless `lab/` changes, which it should not. If a
regenerated vector differs, that is a finding, not a merge conflict: the lab is
the reference the codec measurements were made against.
