//! Encoder/decoder agreement. The encoders and decoders are written
//! independently (different files, different languages of thought), so these
//! tests are what stops a packing bug from quietly showing up as a "codec
//! quality" result.

use lab::dec::{self, mode, DecErr, NBYTES};
use lab::enc::{block::BlockCodec, lz, Codec, EncCtx};
use lab::frame::{Frame, H, NPIX, W};

fn rt(payload: &[u8]) -> Frame {
    let mut f = Frame::black();
    let dst: &mut [u8; NBYTES] = &mut f.px;
    dec::decode(payload, dst).expect("decode");
    f
}

fn rng(seed: &mut u32) -> u32 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 17;
    *seed ^= *seed << 5;
    *seed
}

#[test]
fn lz_roundtrip() {
    let mut seed = 12345u32;
    for case in 0..8 {
        let mut src = vec![0u8; 2048];
        for (i, v) in src.iter_mut().enumerate() {
            *v = match case {
                0 => 0,
                1 => (i % 7) as u8,
                2 => (rng(&mut seed) & 0xff) as u8,
                3 => ((i / 64) % 3) as u8,
                4 => (rng(&mut seed) & 3) as u8,
                _ => ((i as u32).wrapping_mul(case as u32) >> 3) as u8,
            };
        }
        let packed = lz::deflate(&src);
        let mut out = vec![0u8; src.len()];
        let n = dec::lz::inflate(&packed, &mut out, src.len()).expect("inflate");
        assert_eq!(n, src.len(), "case {case}");
        assert_eq!(out, src, "case {case}");
    }
}

#[test]
fn ycocg_r_is_reversible() {
    for r in (0..=255u16).step_by(17) {
        for g in (0..=255u16).step_by(17) {
            for b in (0..=255u16).step_by(17) {
                let c = [r as u8, g as u8, b as u8];
                let (y, co, cg) = lab::enc::ycocg::forward(c);
                let t = y - (cg >> 1);
                let gg = cg + t;
                let bb = t - (co >> 1);
                let rr = bb + co;
                assert_eq!([rr as u8, gg as u8, bb as u8], c);
            }
        }
    }
}

/// A frame whose every 4x4 block holds at most two colours must survive
/// `cc2-44` bit-exactly -- that is the whole reason the mode exists.
#[test]
fn cc2_44_is_exact_on_two_colour_blocks() {
    let mut f = Frame::black();
    let mut seed = 999u32;
    for by in 0..H / 4 {
        for bx in 0..W / 4 {
            let a = [
                (rng(&mut seed) & 0xff) as u8,
                (rng(&mut seed) & 0xff) as u8,
                (rng(&mut seed) & 0xff) as u8,
            ];
            let b = [
                (rng(&mut seed) & 0xff) as u8,
                (rng(&mut seed) & 0xff) as u8,
                (rng(&mut seed) & 0xff) as u8,
            ];
            for j in 0..16 {
                let pick = rng(&mut seed) & 1 == 1;
                f.set(
                    bx * 4 + (j & 3),
                    by * 4 + (j >> 2),
                    if pick { a } else { b },
                );
            }
        }
    }
    let p = BlockCodec::cc2_44().encode(&f, lab::BUDGET, &mut EncCtx::default());
    assert_eq!(p.len(), 1025);
    assert_eq!(p[0], mode::CC2_44);
    assert_eq!(rt(&p).px, f.px, "cc2-44 must be lossless here");
}

/// A frame with at most 16 distinct colours must survive the palette modes.
#[test]
fn palette_modes_are_exact_on_few_colours() {
    let cols: Vec<[u8; 3]> = (0..16u8)
        .map(|i| [i * 17, 255 - i * 17, (i as u32 * 37 % 256) as u8])
        .collect();
    let mut f = Frame::black();
    for p in 0..NPIX {
        let c = cols[(p * 7 + p / 13) % 16];
        f.set(p % W, p / W, c);
    }
    for c in lab::enc::roster() {
        if !c.name().starts_with("pal4-adapt") && !c.name().starts_with("pal5-adapt") {
            continue;
        }
        let p = c.encode(&f, lab::BUDGET, &mut EncCtx::default());
        assert_eq!(rt(&p).px, f.px, "{} should be lossless here", c.name());
    }
    // And the variable-rate ladder must both be lossless and beat the fixed
    // rate modes on size for content this simple.
    let p = lz::ladder(&f, lab::BUDGET, 0);
    assert_eq!(p[0], mode::PAL4_LZ);
    assert_eq!(rt(&p).px, f.px);
    assert!(p.len() < 1073, "got {} bytes", p.len());
}

/// Blocks that are a single RGB565-representable colour must come back exactly
/// through the endpoint-ramp modes: this pins the endpoint packing.
#[test]
fn flat_blocks_survive_ramp_modes() {
    let q565 = |v: u8| ((v >> 3) << 3) | (v >> 5);
    let q5 = |v: u8| ((v >> 3) << 3) | (v >> 5);
    let q6 = |v: u8| ((v >> 2) << 2) | (v >> 6);
    let mut seed = 7u32;
    let mut f = Frame::black();
    for by in 0..H / 4 {
        for bx in 0..W / 4 {
            let c = [
                q5((rng(&mut seed) & 0xff) as u8),
                q6((rng(&mut seed) & 0xff) as u8),
                q565((rng(&mut seed) & 0xff) as u8),
            ];
            for j in 0..16 {
                f.set(bx * 4 + (j & 3), by * 4 + (j >> 2), c);
            }
        }
    }
    for c in [
        BlockCodec::bc1(),
        BlockCodec::bc1_i3(),
        BlockCodec::bc1_e888(),
    ] {
        let p = c.encode(&f, lab::BUDGET, &mut EncCtx::default());
        assert_eq!(p.len(), c.payload_len());
        assert_eq!(rt(&p).px, f.px, "{} on flat 565 blocks", c.name());
    }
}

/// Every codec must stay inside the budget on real content, and every payload
/// must decode.
#[test]
fn everything_fits_and_decodes() {
    let clips = [
        lab::content::plasma(),
        lab::content::textui(),
        lab::content::darkfade(),
    ];
    for c in lab::enc::roster() {
        for clip in &clips {
            let mut ctx = EncCtx::default();
            for (i, fr) in clip.frames.iter().enumerate().take(6) {
                ctx.frame_idx = i;
                let p = c.encode(fr, lab::BUDGET, &mut ctx);
                assert!(
                    p.len() <= lab::BUDGET,
                    "{} on {} frame {i}: {} bytes",
                    c.name(),
                    clip.name,
                    p.len()
                );
                let _ = rt(&p);
            }
        }
    }
}

/// Truncated or corrupt payloads must return an error, never panic. The
/// firmware will be fed whatever the network hands it.
#[test]
fn short_and_bogus_payloads_are_rejected() {
    let mut dst = Box::new([0u8; NBYTES]);
    assert_eq!(dec::decode(&[], &mut dst), Err(DecErr::Short));
    assert_eq!(dec::decode(&[0xee], &mut dst), Err(DecErr::BadMode));
    let f = lab::content::plasma().frames.remove(0);
    for c in lab::enc::roster() {
        let p = c.encode(&f, lab::BUDGET, &mut EncCtx::default());
        for cut in [1usize, 2, p.len() / 3, p.len() / 2, p.len() - 1] {
            // Must not panic; may or may not error depending on the mode.
            let _ = dec::decode(&p[..cut], &mut dst);
        }
    }
    // An LZ stream claiming a match before the start of the output.
    let bogus = [mode::PAL8_LZ, 0, 1, 2, 3, 0b0000_0000, 0xff, 0xff];
    assert!(dec::decode(&bogus, &mut dst).is_err());
}
