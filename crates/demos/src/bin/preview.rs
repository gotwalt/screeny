//! Panel-model preview: render a piece and look at what the *panel* would
//! show, not at the framebuffer.
//!
//!     cargo run --release -p screeny-demos --bin preview -- <piece> [options]
//!
//! Pieces:
//!     fractal     the endless zoom
//!     clock       the word clock
//!     testcard    hue sweep, ramps, lines and type, for judging the preview
//!     targets     every fractal tour target at several depths (curation check)
//!     phrases     the word clock's awkward layouts on one sheet
//!     probe       what iteration budget each depth needs (prints, writes nothing)
//!
//! Options:
//!     --seconds N     span of time covered by the sheet (default 4)
//!     --frames N      number of frames on the sheet (default 6)
//!     --start S       time offset of the first frame, in seconds
//!     --at HH:MM[:SS] wall-clock time for the clock (default: now)
//!     --fps N         frame rate for --apng (default 30)
//!     --apng          also write an animated PNG beside the sheet
//!     --scale N       preview pixels per LED (default 12)
//!     --levels N      panel levels per channel (default 64; try 32)
//!     --squint        render the blurred "across the room" view instead
//!     --indexed       render through the piece's <= 32-colour indexed path
//!     --ss N          fractal supersampling per axis (default 4)
//!     --band N        fractal palette cycle, in sqrt(iterations) (default 11)
//!     --seed N        fractal seed
//!     --slide         clock: slide words sideways instead of rolling them
//!     --cols N        sheet columns (default 2)
//!     --single        one big frame instead of a sheet
//!     --bloom F       halo strength (default 0.20; 0 for small PNGs)
//!     --mask F        light level of an unlit LED (default 0.01)
//!     --bench N       render N frames and report ms/frame, write nothing
//!     --out PATH      output PNG (default /tmp/preview-<piece>.png)
//!
//! Judge pieces at something near actual size: the panel is 19 x 10 cm, so a
//! `--scale 12` preview at 100% on a typical laptop screen is about right.

use chrono::{Local, NaiveDateTime, NaiveTime};
use screeny_demos::clock::{Transition, WordClock};
use screeny_demos::fractal::{self, FractalZoom};
use screeny_demos::frame::{Frame, Indexed, Piece};
use screeny_demos::panel::Panel;
use screeny_demos::preview::{self, PreviewOpts, Tile};
use screeny_demos::stats;
use screeny_demos::testcard::TestCard;
use std::path::PathBuf;
use std::time::{Duration, Instant};

struct Args {
    piece: String,
    seconds: f64,
    frames: usize,
    start: f64,
    at: Option<String>,
    fps: u16,
    apng: bool,
    scale: usize,
    levels: u32,
    squint: bool,
    indexed: bool,
    ss: usize,
    seed: u64,
    band: f32,
    bloom: f32,
    mask: f32,
    cols: usize,
    single: bool,
    bench: usize,
    transition: Transition,
    out: Option<PathBuf>,
}

fn parse_args() -> Args {
    let mut a = Args {
        piece: "testcard".into(),
        seconds: 4.0,
        frames: 6,
        start: 0.0,
        at: None,
        fps: 30,
        apng: false,
        scale: 12,
        levels: 64,
        squint: false,
        indexed: false,
        ss: 4,
        seed: 1,
        band: 0.0,
        bloom: -1.0,
        mask: -1.0,
        cols: 2,
        single: false,
        bench: 0,
        transition: Transition::Roll,
        out: None,
    };
    let mut it = std::env::args().skip(1);
    let mut first = true;
    while let Some(arg) = it.next() {
        let mut val = || it.next().expect("missing value");
        match arg.as_str() {
            "--seconds" => a.seconds = val().parse().unwrap(),
            "--frames" => a.frames = val().parse().unwrap(),
            "--start" => a.start = val().parse().unwrap(),
            "--at" => a.at = Some(val()),
            "--fps" => a.fps = val().parse().unwrap(),
            "--apng" => a.apng = true,
            "--scale" => a.scale = val().parse().unwrap(),
            "--levels" => a.levels = val().parse().unwrap(),
            "--squint" => a.squint = true,
            "--indexed" => a.indexed = true,
            "--ss" => a.ss = val().parse().unwrap(),
            "--seed" => a.seed = val().parse().unwrap(),
            "--band" => a.band = val().parse().unwrap(),
            "--bloom" => a.bloom = val().parse().unwrap(),
            "--mask" => a.mask = val().parse().unwrap(),
            "--cols" => a.cols = val().parse().unwrap(),
            "--single" => a.single = true,
            "--bench" => a.bench = val().parse().unwrap(),
            "--slide" => a.transition = Transition::Slide,
            "--out" => a.out = Some(PathBuf::from(val())),
            other if first && !other.starts_with("--") => a.piece = other.to_string(),
            other => panic!("unknown argument {other}"),
        }
        first = false;
    }
    a
}

fn base_time(at: &Option<String>) -> NaiveDateTime {
    let now = Local::now().naive_local();
    match at {
        None => now,
        Some(s) => {
            let t = NaiveTime::parse_from_str(s, "%H:%M:%S")
                .or_else(|_| NaiveTime::parse_from_str(s, "%H:%M"))
                .expect("--at wants HH:MM or HH:MM:SS");
            now.date().and_time(t)
        }
    }
}

fn make_piece(a: &Args) -> Box<dyn Piece> {
    match a.piece.as_str() {
        "fractal" => {
            let mut f = FractalZoom::new(a.seed);
            f.ss = a.ss;
            f.panel = Panel::new(a.levels);
            if a.band > 0.0 {
                f.band_period = a.band;
            }
            Box::new(f)
        }
        "clock" => {
            let mut c = WordClock::at(base_time(&a.at));
            c.panel = Panel::new(a.levels);
            c.transition = a.transition;
            Box::new(c)
        }
        "testcard" => Box::new(TestCard),
        other => panic!("unknown piece {other}"),
    }
}

fn render_at(piece: &mut dyn Piece, t: Duration, indexed: bool, out: &mut Frame) -> Option<usize> {
    if indexed {
        let mut idx = Indexed::default();
        if piece.render_indexed(t, &mut idx) {
            *out = idx.to_frame();
            return Some(idx.palette.len());
        }
    }
    piece.render(t, out);
    None
}

/// A sheet of the fractal tour's deepest view for every curated target: the
/// curation check, by eye.
fn targets_sheet(a: &Args, opts: &PreviewOpts) -> preview::Img {
    let mut z = FractalZoom::new(a.seed);
    z.ss = a.ss;
    if a.band > 0.0 {
        z.band_period = a.band;
    }
    let mut tiles = Vec::new();
    let ages: Vec<f64> = (0..a.frames)
        .map(|i| z.leg_secs * (i as f64 + 1.0) / a.frames as f64)
        .collect();
    for t in z.targets.clone() {
        for age in &ages {
            let mut f = Frame::black();
            fractal::render_target(&mut z, t, *age, &mut f);
            let s = stats::frame_stats(&f, &opts.panel);
            tiles.push(Tile {
                label: format!(
                    "{} {:.0}S {} COLORS APL {:.0}%",
                    t.name.to_uppercase(),
                    age,
                    s.colors,
                    s.apl * 100.0
                ),
                img: preview::render(&f, opts),
            });
        }
    }
    preview::sheet("FRACTAL TOUR TARGETS", tiles, ages.len())
}

/// The clock's awkward cases on one sheet: the longest phrase, the shortest,
/// both layouts, noon and midnight.
fn phrases_sheet(a: &Args, opts: &PreviewOpts) -> preview::Img {
    const TIMES: &[(u32, u32)] = &[
        (11, 35),
        (0, 0),
        (12, 0),
        (9, 0),
        (6, 30),
        (3, 5),
        (1, 20),
        (7, 55),
        (22, 25),
        (17, 40),
    ];
    let mut tiles = Vec::new();
    for (h, m) in TIMES {
        let base = Local::now()
            .naive_local()
            .date()
            .and_time(NaiveTime::from_hms_opt(*h, *m, 0).unwrap());
        let mut c = WordClock::at(base);
        c.panel = Panel::new(a.levels);
        let mut f = Frame::black();
        render_at(&mut c, Duration::from_secs_f64(120.0), a.indexed, &mut f);
        let s = stats::frame_stats(&f, &opts.panel);
        tiles.push(Tile {
            label: format!("{:02}:{:02} {}", h, m, s.label()),
            img: preview::render(&f, opts),
        });
    }
    preview::sheet("WORD CLOCK PHRASES", tiles, a.cols)
}

/// How many iterations the tour actually needs, per target and depth. The
/// iteration budget is the one number that decides whether a deep view is a
/// picture or a black rectangle, so it is measured rather than guessed: for
/// each depth, escape counts for every pixel of the view at a very high
/// ceiling, reported as percentiles.
fn probe(a: &Args) {
    let z = FractalZoom::new(a.seed);
    const CEIL: u32 = 200_000;
    println!("depth  target          p50    p90    p99   p99.9   interior%");
    for t in z.targets.clone() {
        for oct in [4.0f64, 8.0, 12.0, 16.0, 20.0, 24.0] {
            let scale = 1.6 * (-oct * std::f64::consts::LN_2).exp();
            let (mut counts, mut interior) = (Vec::new(), 0usize);
            for y in 0..32 {
                for x in 0..64 {
                    let cr = t.cx + (x as f64 - 31.5) / 32.0 * scale;
                    let ci = t.cy + (y as f64 - 15.5) / 32.0 * scale;
                    match fractal::escape(cr, ci, CEIL) {
                        Some(nu) => counts.push(nu),
                        None => interior += 1,
                    }
                }
            }
            counts.sort_by(|p, q| p.partial_cmp(q).unwrap());
            let pct = |f: f64| {
                if counts.is_empty() {
                    0.0
                } else {
                    counts[((counts.len() - 1) as f64 * f) as usize]
                }
            };
            println!(
                "{:5.0}  {:14} {:6.0} {:6.0} {:6.0} {:7.0}   {:4.0}%",
                oct,
                t.name,
                pct(0.5),
                pct(0.9),
                pct(0.99),
                pct(0.999),
                interior as f64 / 2048.0 * 100.0
            );
        }
    }
}

fn main() {
    let a = parse_args();
    if a.piece == "probe" {
        probe(&a);
        return;
    }
    let mut opts = PreviewOpts {
        panel: Panel::new(a.levels),
        scale: a.scale,
        ..Default::default()
    };
    if a.bloom >= 0.0 {
        opts.bloom = a.bloom;
    }
    if a.mask >= 0.0 {
        opts.mask = a.mask;
    }

    if a.bench > 0 {
        let mut piece = make_piece(&a);
        let mut f = Frame::black();
        let mut times = Vec::new();
        // One warm-up frame, then the measurement.
        render_at(
            piece.as_mut(),
            Duration::from_secs_f64(a.start),
            a.indexed,
            &mut f,
        );
        for i in 0..a.bench {
            let t = Duration::from_secs_f64(a.start + a.seconds * i as f64 / a.bench.max(1) as f64);
            let t0 = Instant::now();
            render_at(piece.as_mut(), t, a.indexed, &mut f);
            times.push(t0.elapsed().as_secs_f64() * 1000.0);
        }
        times.sort_by(|x, y| x.partial_cmp(y).unwrap());
        let mean: f64 = times.iter().sum::<f64>() / times.len() as f64;
        println!(
            "{}: {} frames  mean {:.2} ms  median {:.2} ms  p95 {:.2} ms  max {:.2} ms  ({:.0} fps worst case)",
            a.piece,
            times.len(),
            mean,
            times[times.len() / 2],
            times[times.len() * 95 / 100],
            times[times.len() - 1],
            1000.0 / times[times.len() - 1]
        );
        return;
    }

    let out = a
        .out
        .clone()
        .unwrap_or_else(|| PathBuf::from(format!("/tmp/preview-{}.png", a.piece)));

    if a.piece == "phrases" {
        let img = phrases_sheet(&a, &opts);
        img.save(&out).unwrap();
        println!("wrote {} ({}x{})", out.display(), img.w, img.h);
        return;
    }

    if a.piece == "targets" {
        let img = targets_sheet(&a, &opts);
        img.save(&out).unwrap();
        println!("wrote {} ({}x{})", out.display(), img.w, img.h);
        return;
    }

    let mut piece = make_piece(&a);
    let n = a.frames.max(1);
    let mut tiles = Vec::new();
    let mut prev: Option<Frame> = None;
    let mut max_delta = 0f32;
    for i in 0..n {
        let t = Duration::from_secs_f64(if n == 1 {
            a.start
        } else {
            a.start + a.seconds * i as f64 / (n - 1) as f64
        });
        let mut f = Frame::black();
        let pal = render_at(piece.as_mut(), t, a.indexed, &mut f);
        let s = stats::frame_stats(&f, &opts.panel);
        if let Some(p) = &prev {
            max_delta = max_delta.max(stats::luma_delta(p, &f, &opts.panel));
        }
        prev = Some(f.clone());
        let extra = match pal {
            Some(p) => format!(" PAL{}", p),
            None => String::new(),
        };
        let img = if a.squint {
            preview::render_squint(&f, &opts)
        } else {
            preview::render(&f, &opts)
        };
        tiles.push(Tile {
            label: format!("T+{:.2}S {}{}", t.as_secs_f64(), s.label(), extra),
            img,
        });
    }

    let title = format!(
        "{} - {} LEVELS{}{}",
        a.piece.to_uppercase(),
        a.levels,
        if a.squint { " - SQUINT" } else { "" },
        if a.indexed { " - INDEXED" } else { "" }
    );
    let img = if a.single && tiles.len() == 1 {
        tiles.pop().unwrap().img
    } else {
        preview::sheet(&title, tiles, a.cols)
    };
    img.save(&out).unwrap();
    println!(
        "wrote {} ({}x{})  max frame-to-frame luma delta {:.3}",
        out.display(),
        img.w,
        img.h,
        max_delta
    );

    if a.apng {
        let mut piece = make_piece(&a);
        let nf = (a.seconds * a.fps as f64) as usize;
        let mut imgs = Vec::with_capacity(nf);
        let mut f = Frame::black();
        for i in 0..nf {
            let t = Duration::from_secs_f64(a.start + i as f64 / a.fps as f64);
            render_at(piece.as_mut(), t, a.indexed, &mut f);
            imgs.push(preview::render(&f, &opts));
        }
        let apath = out.with_extension("apng.png");
        preview::save_apng(&apath, &imgs, a.fps).unwrap();
        println!("wrote {} ({} frames)", apath.display(), nf);
    }
}
