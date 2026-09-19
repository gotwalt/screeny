//! Measurement harness. `cargo run --release` regenerates every number in
//! `docs/research/002-frame-encoding.md` and every PNG under `lab/out/`.

use lab::content;
use lab::enc::{self, Codec, EncCtx};
use lab::frame::{Clip, Frame, NBYTES};
use lab::metrics::{Collector, Summary};
use lab::panel::{Panel, NOMINAL, PESSIMISTIC};
use lab::sheet::{self, Tile};
use lab::BUDGET;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::Instant;

struct Run {
    summary: Summary,
    de_pessimistic: f64,
    decoded: Vec<Frame>,
    modes: BTreeMap<&'static str, usize>,
    decode_ns: f64,
}

fn decode_frame(payload: &[u8]) -> Frame {
    let mut f = Frame::black();
    let dst: &mut [u8; NBYTES] = &mut f.px;
    lab::dec::decode(payload, dst).expect("decode failed");
    f
}

fn mode_label(payload: &[u8]) -> &'static str {
    use lab::dec::mode::*;
    match payload[0] {
        PAL4 => "pal4",
        PAL5 => "pal5",
        PAL8 => "pal8",
        PAL8_LZ => "pal8-lz",
        PAL4_LZ => "pal4-lz",
        BC1 => "bc1",
        BC1_I3 => "bc1-i3",
        BC1_E888 => "bc1-e888",
        BLK42 => "blk42",
        BLK84_I3 => "blk84-i3",
        CC4 => "cc4",
        CC2_42 => "cc2-42",
        CC2_44 => "cc2-44",
        BC1_DUAL => "bc1-dual",
        YCOCG_420 => "ycocg-420",
        YCOCG_410 => "ycocg-410",
        SOLID => "solid",
        _ => "?",
    }
}

fn run_codec(codec: &dyn Codec, clip: &Clip) -> Run {
    let mut ctx = EncCtx::default();
    let mut col = Collector::new(NOMINAL);
    let mut col_p = Collector::new(PESSIMISTIC);
    let mut decoded = Vec::with_capacity(clip.frames.len());
    let mut payloads = Vec::with_capacity(clip.frames.len());
    let mut modes: BTreeMap<&'static str, usize> = BTreeMap::new();

    for (i, f) in clip.frames.iter().enumerate() {
        ctx.frame_idx = i;
        let p = codec.encode(f, BUDGET, &mut ctx);
        assert!(
            p.len() <= BUDGET,
            "{} produced {} bytes, over budget",
            codec.name(),
            p.len()
        );
        let d = decode_frame(&p);
        col.add_frame(f, &d, p.len(), BUDGET);
        col_p.add_frame(f, &d, p.len(), BUDGET);
        *modes.entry(mode_label(&p)).or_insert(0) += 1;
        decoded.push(d);
        payloads.push(p);
    }
    col.add_temporal(&clip.frames, &decoded);

    // Decode timing: median of 7 runs of 500 decodes over a spread of frames.
    let mut dst = Box::new([0u8; NBYTES]);
    let mut times = Vec::new();
    for _ in 0..7 {
        let t0 = Instant::now();
        let mut n = 0u32;
        for r in 0..500 {
            let p = &payloads[(r * 7) % payloads.len()];
            lab::dec::decode(p, &mut dst).unwrap();
            n += dst[0] as u32;
        }
        std::hint::black_box(n);
        times.push(t0.elapsed().as_nanos() as f64 / 500.0);
    }
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());

    let de_pessimistic = col_p.finish().de_mean;
    Run {
        summary: col.finish(),
        de_pessimistic,
        decoded,
        modes,
        decode_ns: times[3],
    }
}

fn fmt_modes(m: &BTreeMap<&'static str, usize>, total: usize) -> String {
    if m.len() == 1 {
        return "-".into();
    }
    let mut v: Vec<_> = m.iter().collect();
    v.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    v.iter()
        .map(|(k, n)| format!("{k} {}%", **n * 100 / total))
        .collect::<Vec<_>>()
        .join(", ")
}

fn main() -> std::io::Result<()> {
    let out = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("out");
    std::fs::create_dir_all(&out)?;
    let mut report = String::new();

    // --- panel facts -------------------------------------------------------
    let mut panel_tbl = String::new();
    writeln!(panel_tbl, "| BCM bitplanes | distinct output levels from 256 sRGB codes | sRGB codes crushed to the bottom two levels |").unwrap();
    writeln!(panel_tbl, "|---|---|---|").unwrap();
    for bits in [4u32, 5, 6, 7, 8, 10] {
        let (lv, crushed) = Panel::new(bits).distinct_levels();
        writeln!(
            panel_tbl,
            "| {bits} | {lv} | {crushed} ({:.1}% of range) |",
            crushed as f64 / 256.0 * 100.0
        )
        .unwrap();
    }
    print!("{panel_tbl}");
    report.push_str("## Panel model\n\n");
    report.push_str(&panel_tbl);

    // --- content -----------------------------------------------------------
    eprintln!("generating content...");
    let t0 = Instant::now();
    let clips = content::all();
    eprintln!("  {:?}", t0.elapsed());
    report.push_str("\n## Content\n\n| clip | frames | distinct colours/frame (mean) | note |\n|---|---|---|---|\n");
    for c in &clips {
        let mean: f64 = c
            .frames
            .iter()
            .map(|f| f.distinct_colours() as f64)
            .sum::<f64>()
            / c.frames.len() as f64;
        report.push_str(&format!(
            "| `{}` | {} | {:.0} | {} |\n",
            c.name,
            c.frames.len(),
            mean,
            c.blurb
        ));
    }

    // --- measure -----------------------------------------------------------
    let codecs = enc::roster();
    report.push_str("\n## Codecs\n\n| codec | description |\n|---|---|\n");
    for c in &codecs {
        report.push_str(&format!("| `{}` | {} |\n", c.name(), c.note()));
    }

    let mut runs: Vec<Vec<Run>> = Vec::new();
    for clip in &clips {
        eprintln!("clip {}", clip.name);
        let mut row = Vec::new();
        for c in &codecs {
            let t = Instant::now();
            let r = run_codec(c.as_ref(), clip);
            eprintln!(
                "  {:<18} {:>6.0} B  dE {:>6.2}  {:>5.1} us  ({:.1}s)",
                c.name(),
                r.summary.bytes_mean,
                r.summary.de_mean,
                r.decode_ns / 1000.0,
                t.elapsed().as_secs_f64()
            );
            row.push(r);
        }
        runs.push(row);
    }

    // --- tables ------------------------------------------------------------
    for (ci, clip) in clips.iter().enumerate() {
        report.push_str(&format!("\n### `{}`\n\n", clip.name));
        report.push_str("| codec | bytes mean | bytes max | over budget | dE x1000 | dE p95 | dE blurred | dE @6-bit panel | SSIM | PSNR dB | px exact | flicker | dE t-avg4 | lossless | decode us | modes |\n");
        report.push_str("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|\n");
        for (i, c) in codecs.iter().enumerate() {
            let r = &runs[ci][i];
            let s = &r.summary;
            report.push_str(&format!(
                "| `{}` | {:.0} | {} | {:.0}% | **{:.2}** | {:.2} | {:.2} | {:.2} | {:.4} | {:.1} | {:.1}% | {:+.2} | {:.2} | {:.0}% | {:.1} | {} |\n",
                c.name(), s.bytes_mean, s.bytes_max, s.over_pct, s.de_mean, s.de_p95,
                s.de_blur, r.de_pessimistic, s.ssim, s.psnr, s.exact_pct, s.flicker, s.tavg,
                s.lossless_pct, r.decode_ns / 1000.0,
                fmt_modes(&r.modes, clip.frames.len())
            ));
        }
    }

    // --- aggregate ---------------------------------------------------------
    report.push_str("\n### All clips, mean of per-clip means\n\n");
    report.push_str("| codec | bytes mean | bytes max | dE x1000 | dE p95 | dE blurred | SSIM | flicker | dE t-avg4 | decode us |\n|---|---|---|---|---|---|---|---|---|---|\n");
    let mut ranked: Vec<(f64, usize)> = Vec::new();
    for (i, c) in codecs.iter().enumerate() {
        let n = clips.len() as f64;
        let g = |f: &dyn Fn(&Run) -> f64| runs.iter().map(|r| f(&r[i])).sum::<f64>() / n;
        let de = g(&|r| r.summary.de_mean);
        ranked.push((de, i));
        report.push_str(&format!(
            "| `{}` | {:.0} | {} | **{:.2}** | {:.2} | {:.2} | {:.4} | {:+.2} | {:.2} | {:.1} |\n",
            c.name(),
            g(&|r| r.summary.bytes_mean),
            runs.iter().map(|r| r[i].summary.bytes_max).max().unwrap(),
            de,
            g(&|r| r.summary.de_p95),
            g(&|r| r.summary.de_blur),
            g(&|r| r.summary.ssim),
            g(&|r| r.summary.flicker),
            g(&|r| r.summary.tavg),
            g(&|r| r.decode_ns) / 1000.0,
        ));
    }
    ranked.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    report.push_str("\nRanked by mean panel-aware dE: ");
    report.push_str(
        &ranked
            .iter()
            .map(|(d, i)| format!("`{}` {:.2}", codecs[*i].name(), d))
            .collect::<Vec<_>>()
            .join(" < "),
    );
    report.push('\n');

    // --- contact sheets ----------------------------------------------------
    eprintln!("writing sheets...");
    let pick = 24usize;
    for (ci, clip) in clips.iter().enumerate() {
        let mut tiles = vec![Tile {
            frame: &clip.frames[pick],
            label: format!("SOURCE {}", clip.name),
        }];
        for (i, c) in codecs.iter().enumerate() {
            let r = &runs[ci][i];
            tiles.push(Tile {
                frame: &r.decoded[pick],
                label: format!("{} dE{:.1}", c.name(), r.summary.de_mean),
            });
        }
        sheet::grid(
            &format!("{} - frame {} - budget {} B", clip.name, pick, BUDGET),
            &tiles,
            4,
            5,
        )
        .save(&out.join(format!("sheet-{}.png", clip.name)))?;

        // Close-up of the six best codecs on this clip.
        let mut order: Vec<usize> = (0..codecs.len()).collect();
        order.sort_by(|&a, &b| {
            runs[ci][a]
                .summary
                .de_mean
                .partial_cmp(&runs[ci][b].summary.de_mean)
                .unwrap()
        });
        let mut zt = vec![Tile {
            frame: &clip.frames[pick],
            label: "SOURCE".into(),
        }];
        for &i in order.iter().take(5) {
            zt.push(Tile {
                frame: &runs[ci][i].decoded[pick],
                label: format!("{} dE{:.2}", codecs[i].name(), runs[ci][i].summary.de_mean),
            });
        }
        sheet::grid(&format!("{} - best five", clip.name), &zt, 3, 9)
            .save(&out.join(format!("zoom-{}.png", clip.name)))?;
    }

    // Temporal-dither demonstration: four consecutive frames of the temporally
    // dithered codec plus their time average, against the source average.
    if let Some(ti) = codecs
        .iter()
        .position(|c| c.name() == "pal5-adapt-tdith")
    {
        let si = codecs.iter().position(|c| c.name() == "pal5-adapt-ord").unwrap();
        let ci = clips.iter().position(|c| c.name == "photo").unwrap();
        let avg = |fs: &[Frame], a: usize| -> Frame {
            let mut o = Frame::black();
            for p in 0..lab::frame::NPIX {
                let mut acc = [0f32; 3];
                for t in a..a + 4 {
                    let l = enc::lin(fs[t].at(p));
                    for k in 0..3 {
                        acc[k] += l[k] / 4.0;
                    }
                }
                o.px[p * 3] = lab::color::lin_to_srgb8(acc[0]);
                o.px[p * 3 + 1] = lab::color::lin_to_srgb8(acc[1]);
                o.px[p * 3 + 2] = lab::color::lin_to_srgb8(acc[2]);
            }
            o
        };
        let src_avg = avg(&clips[ci].frames, 20);
        let t_avg = avg(&runs[ci][ti].decoded, 20);
        let s_avg = avg(&runs[ci][si].decoded, 20);
        let tiles = vec![
            Tile { frame: &clips[ci].frames[20], label: "SOURCE f20".into() },
            Tile { frame: &runs[ci][ti].decoded[20], label: "tdith f20".into() },
            Tile { frame: &runs[ci][ti].decoded[21], label: "tdith f21".into() },
            Tile { frame: &src_avg, label: "SOURCE mean f20-23".into() },
            Tile { frame: &t_avg, label: "tdith mean f20-23".into() },
            Tile { frame: &s_avg, label: "static dither mean".into() },
        ];
        sheet::grid(
            "temporal dither: per-frame vs 4-frame average (photo)",
            &tiles,
            3,
            8,
        )
        .save(&out.join("temporal-dither.png"))?;
    }

    std::fs::write(out.join("results.md"), &report)?;
    eprintln!("wrote {}", out.join("results.md").display());
    Ok(())
}
