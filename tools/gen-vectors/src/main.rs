//! Regenerates `crates/proto/tests/vectors/` from the frozen card-002 lab.
//!
//! Run with `cargo run --release` from this directory. It writes, for every
//! vector, the **wire** payload (the lab payload minus its leading mode byte,
//! because on the wire the codec id lives in the packet header) and the
//! 6144-byte RGB888 frame the *lab's own decoder* produces from it. The test
//! `crates/proto/tests/lab_vectors.rs` then checks that `screeny-proto`
//! reproduces those pixels exactly.
//!
//! Nothing in `lab/` is modified: this is a separate package that depends on
//! it by path.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::PathBuf;

use lab::dec::{self, mode, NBYTES};
use lab::enc::{block::BlockDual, lz, pal::PalCodec, Codec, Dither, EncCtx};
use lab::frame::{Frame, NPIX, W};

/// The five codec ids protocol v1 defines. Anything else the lab can emit is
/// lab-only and must not end up in a vector.
const V1: [u8; 5] = [
    mode::PAL5,
    mode::PAL8_LZ,
    mode::PAL4_LZ,
    mode::BC1_DUAL,
    mode::SOLID,
];

fn codec_name(id: u8) -> &'static str {
    match id {
        mode::PAL5 => "pal5",
        mode::PAL8_LZ => "pal8_lz",
        mode::PAL4_LZ => "pal4_lz",
        mode::BC1_DUAL => "bc1_dual",
        mode::SOLID => "solid",
        _ => "unknown",
    }
}

/// A frame of exactly `n` distinct colours, laid out so neighbouring pixels
/// repeat often enough that the LZ stream has something to chew on. Used to
/// steer `lz::ladder` onto a chosen rung: it takes the exact-palette rung when
/// the frame has at most 256 colours, and PAL4_LZ when it has at most 16.
fn n_colour_frame(n: usize, seed: u32) -> Frame {
    let mut s = seed;
    let mut rng = move || {
        s ^= s << 13;
        s ^= s >> 17;
        s ^= s << 5;
        s
    };
    let cols: Vec<[u8; 3]> = (0..n)
        .map(|i| {
            let r = rng();
            [
                (r & 0xff) as u8,
                ((r >> 8) & 0xff) as u8,
                (i * 251 % 256) as u8,
            ]
        })
        .collect();
    let mut f = Frame::black();
    for p in 0..NPIX {
        // Runs of 4 identical pixels, so the index plane compresses.
        f.set(p % W, p / W, cols[(p / 4) % n]);
    }
    f
}

fn main() -> std::io::Result<()> {
    let out = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../crates/proto/tests/vectors");
    std::fs::create_dir_all(&out)?;

    // (name, frame, what it is)
    let mut sources: Vec<(String, Frame, String)> = Vec::new();
    for (clip_name, mut clip) in [
        ("plasma", lab::content::plasma()),
        ("textui", lab::content::textui()),
        ("darkfade", lab::content::darkfade()),
        ("photo", lab::content::photo()),
    ] {
        let f = clip.frames.remove(0);
        let ncol = f.distinct_colours();
        sources.push((
            clip_name.to_string(),
            f,
            format!("lab::content::{clip_name}() frame 0, {ncol} distinct colours"),
        ));
    }
    for n in [2usize, 16, 100, 256] {
        let f = n_colour_frame(n, 0x1234_5678 + n as u32);
        sources.push((
            format!("synth{n}"),
            f,
            format!("synthetic frame of exactly {n} distinct colours in runs of 4 pixels"),
        ));
    }

    let mut rows: Vec<(String, u8, usize, String)> = Vec::new();
    let mut seen: BTreeSet<u8> = BTreeSet::new();

    let mut emit = |name: String, payload: &[u8], how: String| -> std::io::Result<()> {
        let id = payload[0];
        assert!(V1.contains(&id), "{name}: lab-only codec 0x{id:02x}");

        // The lab decoder is the reference: whatever it produces from this
        // payload is what `screeny-proto` has to reproduce.
        let mut px = Box::new([0u8; NBYTES]);
        dec::decode(payload, &mut px).expect("lab decode");

        let file = format!("{}_{}", codec_name(id), name);
        std::fs::write(out.join(format!("{file}.bin")), &payload[1..])?;
        std::fs::write(out.join(format!("{file}.rgb")), &px[..])?;
        rows.push((file, id, payload.len() - 1, how));
        seen.insert(id);
        Ok(())
    };

    for (name, f, how) in &sources {
        // PAL5: adaptive 32-colour palette, ordered dither.
        let p = PalCodec::adaptive(32, Dither::Ordered, false).encode(
            f,
            lab::BUDGET,
            &mut EncCtx::default(),
        );
        emit(
            name.clone(),
            &p,
            format!("PalCodec::adaptive(32, Ordered, false) on {how}"),
        )?;

        // BC1_DUAL.
        let p = BlockDual.encode(f, lab::BUDGET, &mut EncCtx::default());
        emit(name.clone(), &p, format!("BlockDual on {how}"))?;

        // Whichever rung of the palette ladder this frame lands on.
        let p = lz::ladder(f, lab::BUDGET, 0);
        emit(
            name.clone(),
            &p,
            format!("lz::ladder(budget, t=0) -> {} on {how}", lz::rung_name(&p)),
        )?;
    }

    // SOLID has no lab encoder -- the mode is the graceful-degradation floor,
    // not something the measurement harness ever picks. Three bytes, built here.
    for (name, c) in [
        ("black", [0u8, 0, 0]),
        ("white", [255u8, 255, 255]),
        ("teal", [0u8, 128, 128]),
    ] {
        let payload = [mode::SOLID, c[0], c[1], c[2]];
        emit(
            name.to_string(),
            &payload,
            format!("hand-built, RGB {},{},{}", c[0], c[1], c[2]),
        )?;
    }

    for id in V1 {
        assert!(seen.contains(&id), "no vector for codec 0x{id:02x}");
    }

    rows.sort();
    let mut manifest = String::new();
    manifest.push_str(
        "# Generated by tools/gen-vectors -- do not edit by hand.\n\
         # <name>.bin is the wire payload (no codec/mode byte); <name>.rgb is the\n\
         # 6144-byte RGB888 frame the lab decoder produces from it.\n\
         # name\tcodec\tpayload_len\tsource\n",
    );
    for (file, id, len, how) in &rows {
        writeln!(manifest, "{file}\t{id}\t{len}\t{how}").unwrap();
    }
    std::fs::write(out.join("manifest.tsv"), &manifest)?;

    eprintln!("wrote {} vectors to {}", rows.len(), out.display());
    Ok(())
}
