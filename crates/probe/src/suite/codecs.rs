//! Spec section 4: the five codecs a v1 device MUST decode, the payloads it
//! MUST refuse, and the counter that tells the two apart.
//!
//! What the wire can say is "this payload was decoded and put on the panel,
//! and the device says its `last_codec` was the one I sent". What it cannot
//! say is whether the *pixels* are right; that needs `--dump-dir` against the
//! simulator or the camera harness, and it is why `crates/sim/tests/codecs.rs`
//! keeps its bit-exact assertions.
//!
//! The payloads come from `crate::enc`, which is written from section 4's
//! prose rather than from `screeny-proto`'s decoders, so a device that agrees
//! with them is agreeing with a second reading of the spec.

use screeny_proto::dec::codec;
use screeny_proto::F_KEY;

use super::{verdict, Ctx, Outcome, Rule, RETRY};
use crate::{enc, vectors};

pub fn rules() -> Vec<Rule> {
    vec![
        Rule {
            section: "4.1",
            name: "every codec the device advertises decodes a spec-built payload",
            secs: 2.0,
            flags: RETRY,
            run: every_codec,
        },
        Rule {
            section: "4.2",
            name: "PAL8_LZ at 1, 17 and 256 palette entries",
            secs: 1.0,
            flags: RETRY,
            run: pal8_sizes,
        },
        Rule {
            section: "4",
            name: "the 27 checked-in card-005 vectors all decode",
            secs: 6.0,
            flags: RETRY,
            run: checked_in_vectors,
        },
        Rule {
            section: "4.7",
            name: "a reserved or unadvertised codec id -> frames_dropped_decode",
            secs: 1.0,
            flags: RETRY,
            run: reserved_codecs,
        },
        Rule {
            section: "4",
            name: "a payload that is not exactly its codec's size -> decode drop",
            secs: 1.5,
            flags: RETRY,
            run: wrong_length,
        },
        Rule {
            section: "4.4",
            name: "trailing bytes after the LZ stream -> decode drop",
            secs: 0.6,
            flags: RETRY,
            run: lz_trailing,
        },
    ]
}

/// One frame per drain, spaced out. Sent back to back they would be a single
/// drain and all but one would be *superseded* before anything tried to
/// decode them - correct behaviour, and a useless test.
fn send_each(
    cx: &mut Ctx,
    frames: &[(u8, Vec<u8>, &'static str)],
) -> Result<(u32, u32, u32, Vec<String>), String> {
    let mut link = cx.claim()?;
    let mut last = Vec::new();
    for (c, p, what) in frames {
        link.send(*c, F_KEY, p).map_err(|e| e.to_string())?;
        cx.settle();
        let t = cx.telemetry()?;
        if t.last_codec != *c {
            last.push(format!("{what}: last_codec {:#04x}", t.last_codec));
        }
    }
    cx.settle();
    let t = cx.telemetry()?;
    Ok((
        t.frames_shown,
        t.frames_dropped_decode,
        t.frames_rejected,
        last,
    ))
}

fn every_codec(cx: &mut Ctx) -> Result<Outcome, String> {
    // Section 4.7: "A v1 device MUST support all five."
    let f32c = enc::indexed_runs(32);
    let f16 = enc::indexed_runs(16);
    let frames: Vec<(u8, Vec<u8>, &'static str)> = vec![
        (codec::SOLID, enc::solid([30, 40, 50]).0, "SOLID"),
        (codec::PAL5, enc::pal5(&f32c), "PAL5"),
        (codec::PAL4_LZ, enc::pal4_lz(&f16), "PAL4_LZ"),
        (codec::PAL8_LZ, enc::pal8_lz(&f32c), "PAL8_LZ"),
        (codec::BC1_DUAL, enc::bc1_mixed().0, "BC1_DUAL"),
    ];
    let n = frames.len() as u32;
    let (shown, dec, rej, wrong) = send_each(cx, &frames)?;
    verdict(
        shown == n && dec == 0 && rej == 0 && wrong.is_empty(),
        if wrong.is_empty() {
            format!("{shown}/{n} shown, decode drops {dec}, rejected {rej}")
        } else {
            wrong.join("; ")
        },
    )
}

fn pal8_sizes(cx: &mut Ctx) -> Result<Outcome, String> {
    // Section 4.2: `n` is 1..=256 and every decoded index MUST be `< n`. The
    // ends of that range are where an off-by-one in `n-1` lives.
    let mut frames: Vec<(u8, Vec<u8>, &'static str)> = Vec::new();
    for (n, label) in [(1usize, "n=1"), (17, "n=17"), (256, "n=256")] {
        let f = enc::indexed_runs(n);
        let p = enc::pal8_lz(&f);
        if p.len() > screeny_proto::MAX_PIXEL_PAYLOAD {
            return verdict(false, format!("{label}: {} bytes is over budget", p.len()));
        }
        frames.push((codec::PAL8_LZ, p, label));
    }
    let (shown, dec, rej, wrong) = send_each(cx, &frames)?;
    verdict(
        shown == 3 && dec == 0 && rej == 0 && wrong.is_empty(),
        format!("{shown}/3 shown, decode drops {dec}, rejected {rej}"),
    )
}

fn checked_in_vectors(cx: &mut Ctx) -> Result<Outcome, String> {
    // These are what tie a device's decoders to the encoders card 002
    // actually measured: payloads nobody wrote for this device, decoded by it.
    let vs = vectors::load(&vectors::default_dir())?;
    let frames: Vec<(u8, Vec<u8>, &'static str)> = vs
        .iter()
        .map(|v| (v.codec, v.payload.clone(), "vector"))
        .collect();
    let n = frames.len() as u32;
    let (shown, dec, rej, _) = send_each(cx, &frames)?;
    verdict(
        shown == n && dec == 0 && rej == 0,
        format!("{shown}/{n} shown, decode drops {dec}, rejected {rej}"),
    )
}

fn reserved_codecs(cx: &mut Ctx) -> Result<Outcome, String> {
    // Section 4: 0x00 and 0xFF are reserved and MUST be rejected; 0x03 is a
    // lab-only id a v1 device does not advertise. Section 4.7: all three are
    // `frames_dropped_decode` - the frame was *admitted*, it just did not
    // decode - and none of them is a rejected packet.
    let good = enc::solid([30, 40, 50]).0;
    let frames: Vec<(u8, Vec<u8>, &'static str)> = vec![
        (codec::SOLID, good.clone(), "a good frame first"),
        (0x00, good.clone(), "reserved 0x00"),
        (0xFF, good.clone(), "reserved 0xFF"),
        (0x03, good.clone(), "lab-only 0x03"),
    ];
    let (shown, dec, rej, _) = send_each(cx, &frames)?;
    verdict(
        shown == 1 && dec == 3 && rej == 0,
        format!("shown {shown} (expected 1), decode {dec} (expected 3), rejected {rej}"),
    )
}

fn wrong_length(cx: &mut Ctx) -> Result<Outcome, String> {
    // Section 4: "A payload MUST be **exactly** the size its codec calls
    // for... A receiver MUST reject a payload that is longer as well as one
    // that is shorter." Padding, where a sender wants it, goes beyond the
    // header's `len`, which is section 2.3's rule and is checked elsewhere.
    let good = enc::solid([30, 40, 50]).0;
    let frames: Vec<(u8, Vec<u8>, &'static str)> = vec![
        (codec::SOLID, good.clone(), "a good frame first"),
        (codec::SOLID, vec![1, 2], "SOLID one byte short"),
        (codec::SOLID, vec![1, 2, 3, 4], "SOLID one byte long"),
        (codec::PAL5, good.clone(), "PAL5 far too short"),
        (codec::BC1_DUAL, good.clone(), "BC1_DUAL far too short"),
        (codec::PAL8_LZ, vec![0u8], "PAL8_LZ palette, no stream"),
    ];
    let (shown, dec, rej, _) = send_each(cx, &frames)?;
    verdict(
        shown == 1 && dec == 5 && rej == 0,
        format!("shown {shown} (expected 1), decode {dec} (expected 5), rejected {rej}"),
    )
}

fn lz_trailing(cx: &mut Ctx) -> Result<Outcome, String> {
    // Section 4.4: "A sender MUST NOT emit trailing bytes after the item that
    // completes the output; a receiver MUST reject a payload that has them,
    // because padding belongs beyond the header's `len`, not inside it."
    let f = enc::indexed_runs(16);
    let mut p = enc::pal4_lz(&f);
    p.push(0x00); // one byte past the item that completed the index plane
    let good = enc::solid([30, 40, 50]).0;
    let frames: Vec<(u8, Vec<u8>, &'static str)> = vec![
        (codec::SOLID, good, "a good frame first"),
        (codec::PAL4_LZ, p, "PAL4_LZ with one trailing byte"),
    ];
    let (shown, dec, rej, _) = send_each(cx, &frames)?;
    verdict(
        shown == 1 && dec == 1 && rej == 0,
        format!("shown {shown} (expected 1), decode {dec} (expected 1), rejected {rej}"),
    )
}
