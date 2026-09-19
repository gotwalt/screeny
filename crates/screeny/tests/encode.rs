//! The encoder's contract, checked against `screeny_proto`'s decoders - the
//! same code the firmware runs.
//!
//! Three promises are pinned here:
//!
//! 1. **Everything decodes.** Whatever the chooser picks, proto decodes it,
//!    for every frame and every budget.
//! 2. **Nothing exceeds the budget.** Ever, for any content, down to the
//!    1376-byte floor the ladder was designed for and below it.
//! 3. **Low-colour frames come out exact**, because a frame the ladder can
//!    carry losslessly is one no block codec can beat.

mod common;

use common::{adversarial_frames, Rng};
use screeny::encode::{EncodeConfig, Encoder, Profile};
use screeny::proto::dec::{codec, BC1_DUAL_LEN, PAL5_LEN, SOLID_LEN};
use screeny::proto::{decode, IndexedFrame, NBYTES, NPIX};
use screeny::{codec_name, Frame, Pattern};

fn encoder(profile: Profile) -> Encoder {
    Encoder::new(EncodeConfig {
        profile,
        ..EncodeConfig::default()
    })
}

/// Encode, decode through proto, return the decoded frame.
fn round_trip(enc: &mut Encoder, f: &Frame, budget: usize) -> (u8, usize, Frame) {
    let out = enc.encode(f, budget);
    assert!(
        out.payload.len() <= budget,
        "{} produced {} bytes with a budget of {budget}",
        codec_name(out.codec),
        out.payload.len()
    );
    let mut dst = Box::new([0u8; NBYTES]);
    decode(out.codec, &out.payload, &mut dst).unwrap_or_else(|e| {
        panic!(
            "{} payload of {} bytes did not decode: {e:?}",
            codec_name(out.codec),
            out.payload.len()
        )
    });
    (out.codec, out.payload.len(), Frame::from_bytes(&dst[..]).unwrap())
}

#[test]
fn every_pattern_round_trips_at_every_budget() {
    // 1464 is the protocol ceiling, 1376 the PAL5 floor the ladder is
    // designed around, and the two below it check that dropping under the
    // floor degrades rather than breaks.
    for budget in [1464usize, 1440, 1400, 1376, 1300, 1024] {
        for p in Pattern::ALL {
            let mut enc = encoder(Profile::Full);
            for t in 0..4u64 {
                let f = p.frame(t);
                let (c, n, _) = round_trip(&mut enc, &f, budget);
                assert!(
                    screeny::proto::dec::is_supported(c),
                    "{}: unsupported codec {c:#04x}",
                    p.name()
                );
                assert!(n <= budget);
            }
        }
    }
}

#[test]
fn adversarial_frames_never_exceed_the_budget() {
    let mut rng = Rng(0x1234_5678);
    let frames = adversarial_frames(&mut rng);
    for budget in [1464usize, 1400, 1376] {
        for profile in [Profile::Full, Profile::Fast] {
            let mut enc = encoder(profile);
            for (name, f) in &frames {
                let (c, n, _) = round_trip(&mut enc, f, budget);
                assert!(
                    n <= budget,
                    "{name} at budget {budget}: {} bytes of {}",
                    n,
                    codec_name(c)
                );
            }
        }
    }
}

#[test]
fn random_frames_never_exceed_the_budget() {
    let mut rng = Rng(0x9e37_79b9);
    let mut enc = encoder(Profile::Full);
    for i in 0..60 {
        // A spread of colour counts, because that is what decides which rung
        // of the ladder is reached.
        let k = [2usize, 3, 16, 17, 32, 33, 64, 200, 256, 257, 1024, 2048][i % 12];
        let pal: Vec<[u8; 3]> = (0..k)
            .map(|_| [rng.byte(), rng.byte(), rng.byte()])
            .collect();
        let mut f = Frame::black();
        for p in 0..NPIX {
            f.set_at(p, pal[rng.below(k as u32) as usize]);
        }
        let budget = 1376 + (rng.below(89) as usize);
        let (_, n, _) = round_trip(&mut enc, &f, budget);
        assert!(n <= budget, "k={k} budget={budget} bytes={n}");
    }
}

/// Up to 32 colours the ladder is exact **whatever the content**: both LZ
/// rungs may overflow on incompressible indices, but a raw `PAL5` of the
/// frame's own colours is fixed-rate and cannot.
#[test]
fn low_colour_frames_are_always_exact() {
    let mut rng = Rng(0xdead_beef);
    for k in [1usize, 2, 8, 16, 17, 32] {
        let pal: Vec<[u8; 3]> = (0..k)
            .map(|_| [rng.byte(), rng.byte(), rng.byte()])
            .collect();
        // Random placement, so the index plane does not compress at all.
        let mut f = Frame::black();
        for p in 0..NPIX {
            f.set_at(p, pal[rng.below(k as u32) as usize]);
        }
        let mut enc = encoder(Profile::Full);
        let (c, n, back) = round_trip(&mut enc, &f, 1464);
        let st = enc.last_stats();
        assert_eq!(
            back.as_bytes(),
            f.as_bytes(),
            "k={k}: {} of {n} bytes was not lossless",
            codec_name(c)
        );
        assert!(st.exact, "k={k}: the encoder did not know it was exact");
        // And it should not have bothered scoring anything.
        assert_eq!(st.candidates, 0, "k={k}: scored candidates unnecessarily");
        match k {
            1 => assert_eq!(c, codec::SOLID),
            2..=16 => assert_eq!(c, codec::PAL4_LZ, "k={k}"),
            _ => assert!(
                c == codec::PAL8_LZ || c == codec::PAL5,
                "k={k}: got {}",
                codec_name(c)
            ),
        }
    }
}

/// Between 33 and 256 colours the ladder is exact when the index plane
/// compresses into the budget, which is the normal case for real content:
/// anything pixel-authored, anything with runs or repeats. A repeating
/// sequence here, so the LZ coder has something to find.
#[test]
fn repeating_frames_up_to_256_colours_are_exact() {
    let mut rng = Rng(0xc0ff_ee11);
    for k in [33usize, 64, 200, 256] {
        let pal: Vec<[u8; 3]> = (0..k)
            .map(|_| [rng.byte(), rng.byte(), rng.byte()])
            .collect();
        let mut f = Frame::black();
        for p in 0..NPIX {
            f.set_at(p, pal[p % k]);
        }
        let mut enc = encoder(Profile::Full);
        let (c, n, back) = round_trip(&mut enc, &f, 1464);
        assert!(enc.last_stats().exact, "k={k}: not exact");
        assert_eq!(c, codec::PAL8_LZ, "k={k}: {} of {n} bytes", codec_name(c));
        assert_eq!(back.as_bytes(), f.as_bytes(), "k={k}");
    }
}

/// Above 32 colours an incompressible frame has no lossless rung that fits,
/// so the chooser has to do its job. The result must still fit and decode.
#[test]
fn incompressible_many_colour_frames_are_lossy_but_legal() {
    let mut rng = Rng(0x00c0_ffee);
    for k in [64usize, 200, 256, 2048] {
        let pal: Vec<[u8; 3]> = (0..k)
            .map(|_| [rng.byte(), rng.byte(), rng.byte()])
            .collect();
        let mut f = Frame::black();
        for p in 0..NPIX {
            f.set_at(p, pal[rng.below(k as u32) as usize]);
        }
        let mut enc = encoder(Profile::Full);
        let (_, n, _) = round_trip(&mut enc, &f, 1464);
        assert!(n <= 1464);
        assert!(
            enc.last_stats().candidates > 1,
            "k={k}: the chooser did not consider alternatives"
        );
    }
}

/// An incompressible 32-colour frame overflows both LZ rungs; raw `PAL5` is
/// still exact and is fixed-rate, so the ladder must land there rather than
/// giving up on losslessness.
#[test]
fn incompressible_32_colour_frame_falls_to_exact_pal5() {
    let mut rng = Rng(0x0bad_f00d);
    let pal: Vec<[u8; 3]> = (0..32).map(|_| [rng.byte(), rng.byte(), rng.byte()]).collect();
    let mut f = Frame::black();
    for p in 0..NPIX {
        f.set_at(p, pal[rng.below(32) as usize]);
    }
    let mut enc = encoder(Profile::Full);
    let (c, n, back) = round_trip(&mut enc, &f, 1464);
    assert_eq!(c, codec::PAL5, "got {} of {n} bytes", codec_name(c));
    assert_eq!(n, PAL5_LEN);
    assert_eq!(back.as_bytes(), f.as_bytes());
}

#[test]
fn solid_frames_use_three_bytes() {
    let mut enc = encoder(Profile::Full);
    for c in [[0u8, 0, 0], [255, 255, 255], [3, 200, 17]] {
        let f = Frame::solid(c);
        let out = enc.encode(&f, 1464);
        assert_eq!(out.codec, codec::SOLID);
        assert_eq!(out.payload, c.to_vec());
        assert_eq!(out.payload.len(), SOLID_LEN);
    }
}

/// With the palette codecs withheld the encoder must still produce something
/// legal, and must never emit a codec the device did not advertise (spec 4.7).
#[test]
fn honours_the_advertised_codec_list() {
    let sets: [(&str, Vec<u8>); 4] = [
        ("block only", vec![codec::BC1_DUAL]),
        ("pal5 only", vec![codec::PAL5]),
        ("solid only", vec![codec::SOLID]),
        (
            "no pal8",
            vec![codec::PAL4_LZ, codec::PAL5, codec::BC1_DUAL, codec::SOLID],
        ),
    ];
    for (name, codecs) in sets {
        let mut enc = Encoder::new(EncodeConfig {
            codecs: codecs.clone(),
            ..EncodeConfig::default()
        });
        for p in Pattern::ALL {
            let f = p.frame(3);
            let out = enc.encode(&f, 1464);
            assert!(
                codecs.contains(&out.codec),
                "{name}/{}: emitted {}",
                p.name(),
                codec_name(out.codec)
            );
            let mut dst = Box::new([0u8; NBYTES]);
            decode(out.codec, &out.payload, &mut dst).expect("decodes");
        }
    }
}

/// Each fixed-rate codec has to give way to the next one down when the budget
/// no longer holds it, ending at `SOLID`, which always fits.
///
/// Tested one codec at a time: with the variable-rate rungs available they
/// usually fit a tight budget too, and picking one of those is the *right*
/// answer, so it would not test the floor.
#[test]
fn fixed_rate_codecs_give_way_to_the_next_one_down() {
    let f = Pattern::Gradient.frame(0);

    let mut enc = Encoder::new(EncodeConfig {
        codecs: vec![codec::PAL5, codec::SOLID],
        ..EncodeConfig::default()
    });
    assert_eq!(enc.encode(&f, PAL5_LEN).codec, codec::PAL5);
    assert_eq!(enc.encode(&f, PAL5_LEN - 1).codec, codec::SOLID);

    let mut enc = Encoder::new(EncodeConfig {
        codecs: vec![codec::BC1_DUAL, codec::SOLID],
        ..EncodeConfig::default()
    });
    assert_eq!(enc.encode(&f, BC1_DUAL_LEN).codec, codec::BC1_DUAL);
    assert_eq!(enc.encode(&f, BC1_DUAL_LEN - 1).codec, codec::SOLID);
}

/// Sweep the whole budget range, on the hardest content there is. Nothing may
/// overrun and everything must decode - the two promises this module exists
/// to pin.
#[test]
fn every_budget_from_three_bytes_upwards_is_respected() {
    let mut rng = Rng(0x1010_1010);
    let mut f = Frame::black();
    for p in 0..NPIX {
        f.set_at(p, [rng.byte(), rng.byte(), rng.byte()]);
    }
    let mut enc = encoder(Profile::Full);
    let mut budget = SOLID_LEN;
    while budget <= 1464 {
        let (_, n, _) = round_trip(&mut enc, &f, budget);
        assert!(n <= budget);
        budget += 37;
    }
}

#[test]
fn indexed_frames_skip_the_chooser() {
    let mut rng = Rng(0x5eed_1234);
    for k in [1usize, 16, 32, 200, 256] {
        let pal: Vec<[u8; 3]> = (0..k)
            .map(|_| [rng.byte(), rng.byte(), rng.byte()])
            .collect();
        // Repeating rather than random, so the LZ rung fits for the larger
        // palettes; `low_colour_frames_are_always_exact` covers the random
        // case, where only 32 colours or fewer can be promised.
        let mut idx = Box::new([0u8; NPIX]);
        for p in 0..NPIX {
            idx[p] = (p % k) as u8;
        }
        let f = IndexedFrame {
            palette: &pal,
            indices: &idx,
        };
        let mut enc = encoder(Profile::Full);
        let out = enc.encode_indexed(&f, 1464).expect("encodes");
        assert!(enc.last_stats().exact, "k={k} was not exact");

        let mut dst = Box::new([0u8; NBYTES]);
        decode(out.codec, &out.payload, &mut dst).expect("decodes");
        let mut want = Box::new([0u8; NBYTES]);
        f.expand(&mut want).unwrap();
        assert_eq!(&dst[..], &want[..], "k={k} via {}", codec_name(out.codec));
    }
}

#[test]
fn indexed_frames_reject_out_of_range_indices() {
    let pal = [[1u8, 2, 3], [4, 5, 6]];
    let mut idx = Box::new([0u8; NPIX]);
    idx[7] = 9;
    let f = IndexedFrame {
        palette: &pal,
        indices: &idx,
    };
    let mut enc = encoder(Profile::Full);
    assert!(enc.encode_indexed(&f, 1464).is_err());
}

/// The chooser's hysteresis is meant to stop the codec flapping between two
/// near-equal candidates, because a change in the *character* of the error is
/// visible even when its magnitude is not.
#[test]
fn hysteresis_keeps_the_codec_stable() {
    let mut rng = Rng(0xfeed_face);
    let base = Pattern::Gradient.frame(0);
    let mut enc = encoder(Profile::Full);
    let mut switches = 0;
    let mut prev = None;
    for _ in 0..40 {
        // Nudge a few pixels each frame: the content barely changes, so the
        // codec should not either.
        let mut f = base.clone();
        for _ in 0..8 {
            let p = rng.below(NPIX as u32) as usize;
            f.set_at(p, [rng.byte(), rng.byte(), rng.byte()]);
        }
        let out = enc.encode(&f, 1464);
        if prev.is_some_and(|c| c != out.codec) {
            switches += 1;
        }
        prev = Some(out.codec);
    }
    assert!(switches <= 2, "codec changed {switches} times in 40 frames");
}

/// Payload sizes are what the spec says they are, exactly (spec 4).
#[test]
fn fixed_rate_payloads_are_exactly_the_right_size() {
    let f = Pattern::Gradient.frame(0);
    let mut enc = Encoder::new(EncodeConfig {
        codecs: vec![codec::PAL5],
        ..EncodeConfig::default()
    });
    assert_eq!(enc.encode(&f, 1464).payload.len(), PAL5_LEN);

    let mut enc = Encoder::new(EncodeConfig {
        codecs: vec![codec::BC1_DUAL],
        ..EncodeConfig::default()
    });
    assert_eq!(enc.encode(&f, 1464).payload.len(), BC1_DUAL_LEN);
}

/// The fast profile is allowed to be worse, but not by much, and never
/// illegal. This is the documented trade card 031 asks for.
#[test]
fn fast_profile_stays_close_to_full() {
    use screeny::encode::score::mean_de;
    use screeny::panel::TEMPORAL;

    let mut full = encoder(Profile::Full);
    let mut fast = encoder(Profile::Fast);
    let (mut sf, mut sq) = (0f64, 0f64);
    let mut n = 0;
    for p in Pattern::ALL {
        for t in 0..4u64 {
            let f = p.frame(t);
            let (_, _, a) = round_trip(&mut full, &f, 1464);
            let (_, _, b) = round_trip(&mut fast, &f, 1464);
            sf += mean_de(&TEMPORAL, &f, &a);
            sq += mean_de(&TEMPORAL, &f, &b);
            n += 1;
        }
    }
    let (full_de, fast_de) = (sf / f64::from(n) * 1000.0, sq / f64::from(n) * 1000.0);
    assert!(
        fast_de <= full_de * 1.6 + 0.5,
        "fast profile dE {fast_de:.2} against full {full_de:.2}"
    );
}
