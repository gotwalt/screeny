//! Encoder benchmark (card 031), broken down by stage.
//!
//! `cargo bench -p screeny` - or, for a single number,
//! `cargo run --release -p screeny -- encode-stats --pattern gradient`.
//!
//! No criterion: the thing being measured takes milliseconds, not
//! nanoseconds, and a 30 fps budget is the only comparison that matters, so a
//! median of repeated passes over a fixed corpus answers the question without
//! a dependency tree. The corpus is generated here rather than checked in so
//! that the benchmark is reproducible from source; the content classes mirror
//! card 002's (`lab/src/content.rs`), which is what the 8 ms baseline was
//! measured on.

use std::time::Duration;

use screeny::encode::{EncodeConfig, Encoder, Profile, Stages};
use screeny::proto::MAX_PIXEL_PAYLOAD;
use screeny::{codec_name, Frame};

mod content;

const PASSES: usize = 5;

fn main() {
    let clips = content::all();
    println!(
        "encoder benchmark: {} clips x {} frames, budget {} B, {} passes\n",
        clips.len(),
        clips[0].frames.len(),
        MAX_PIXEL_PAYLOAD,
        PASSES
    );

    for profile in [Profile::Full, Profile::Fast] {
        println!("--- profile {profile:?}");
        println!(
            "{:<12} {:>7} {:>8} {:>8} {:>8} {:>7} {:>6}  codecs",
            "clip", "colours", "mean ms", "p50 ms", "max ms", "bytes", "fps",
        );
        let mut grand = Duration::ZERO;
        let mut grand_n = 0usize;
        let mut grand_max = Duration::ZERO;
        let mut stages = Stages::default();

        for clip in &clips {
            let mut per_frame: Vec<Duration> = Vec::new();
            let mut bytes = 0u64;
            let mut colours = 0usize;
            let mut codecs: std::collections::BTreeMap<u8, usize> = Default::default();

            for pass in 0..PASSES {
                let mut enc = Encoder::new(EncodeConfig {
                    profile,
                    measure_stages: pass == 0,
                    ..EncodeConfig::default()
                });
                for (i, f) in clip.frames.iter().enumerate() {
                    let out = enc.encode(f, MAX_PIXEL_PAYLOAD);
                    let st = enc.last_stats();
                    assert!(
                        out.payload.len() <= MAX_PIXEL_PAYLOAD,
                        "{} frame {i}: {} bytes over budget",
                        clip.name,
                        out.payload.len()
                    );
                    if pass == 0 {
                        bytes += out.payload.len() as u64;
                        colours += st.colours;
                        *codecs.entry(out.codec).or_insert(0) += 1;
                        add(&mut stages, &st.stages);
                    }
                    if pass > 0 {
                        per_frame.push(st.elapsed);
                    }
                    std::hint::black_box(&out.payload);
                }
            }

            per_frame.sort();
            let n = per_frame.len();
            let sum: Duration = per_frame.iter().sum();
            let nf = clip.frames.len();
            println!(
                "{:<12} {:>7} {:>8.2} {:>8.2} {:>8.2} {:>7.0} {:>6.0}  {}",
                clip.name,
                colours / nf,
                ms(sum / n as u32),
                ms(per_frame[n / 2]),
                ms(per_frame[n - 1]),
                bytes as f64 / nf as f64,
                n as f64 / sum.as_secs_f64(),
                codecs
                    .iter()
                    .map(|(c, k)| format!("{} {}%", codec_name(*c), k * 100 / nf))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
            grand += sum;
            grand_n += n;
            grand_max = grand_max.max(per_frame[n - 1]);
        }

        let mean = grand / grand_n as u32;
        println!(
            "{:<12} {:>7} {:>8.2} {:>8} {:>8.2} {:>7} {:>6.0}",
            "ALL",
            "",
            ms(mean),
            "",
            ms(grand_max),
            "",
            grand_n as f64 / grand.as_secs_f64()
        );
        println!(
            "  {:.1}% of a 33.3 ms frame period at the mean, {:.1}% at the worst frame",
            ms(mean) / 33.333 * 100.0,
            ms(grand_max) / 33.333 * 100.0
        );
        print_stages(&stages, clips.iter().map(|c| c.frames.len()).sum());
        println!();
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn add(a: &mut Stages, b: &Stages) {
    a.histogram += b.histogram;
    a.lab += b.lab;
    a.quantise += b.quantise;
    a.map += b.map;
    a.lz += b.lz;
    a.block += b.block;
    a.decode += b.decode;
    a.scoring += b.scoring;
}

fn print_stages(s: &Stages, frames: usize) {
    let all = [
        ("histogram", s.histogram),
        ("oklab", s.lab),
        ("quantise", s.quantise),
        ("map", s.map),
        ("lz", s.lz),
        ("block fit", s.block),
        ("decode", s.decode),
        ("scoring", s.scoring),
    ];
    let total: Duration = all.iter().map(|(_, d)| *d).sum();
    println!("  stage breakdown, mean us/frame over all clips:");
    for (name, d) in all {
        println!(
            "    {:<10} {:>7.0} us  {:>4.0}%",
            name,
            d.as_secs_f64() * 1e6 / frames as f64,
            if total.is_zero() {
                0.0
            } else {
                d.as_secs_f64() / total.as_secs_f64() * 100.0
            }
        );
    }
    println!(
        "    {:<10} {:>7.0} us",
        "accounted",
        total.as_secs_f64() * 1e6 / frames as f64
    );
}

/// A named sequence of frames.
pub struct Clip {
    /// Clip name, as printed.
    pub name: &'static str,
    /// The frames themselves.
    pub frames: Vec<Frame>,
}
