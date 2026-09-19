//! Panel-model preview: render a piece and look at what the *panel* would
//! show, not at the framebuffer.
//!
//!     cargo run --release -p screeny-demos --bin preview -- <piece> [options]
//!
//! Pieces: `fractal`, `clock`, `testcard`, `targets` (a sheet of the fractal
//! tour's deepest view per target).
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
//!     --seed N        fractal seed
//!     --cols N        sheet columns (default 2)
//!     --single        one big frame instead of a sheet
//!     --bench N       render N frames and report ms/frame, write nothing
//!     --out PATH      output PNG (default /tmp/preview-<piece>.png)

use chrono::{Local, NaiveDateTime, NaiveTime, Timelike};
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
    let mut tiles = Vec::new();
    let ages: Vec<f64> = (0..a.frames)
        .map(|i| z.leg_secs * (i as f64 + 1.0) / a.frames as f64)
        .collect();
    for t in fractal::TARGETS {
        for age in &ages {
            let mut f = Frame::black();
            fractal::render_target(&mut z, *t, *age, &mut f);
            let s = stats::frame_stats(&f, &opts.panel);
            tiles.push(Tile {
                label: format!("{} {:.0}S {}", t.name.to_uppercase(), age, s.label()),
                img: preview::render(&f, opts),
            });
        }
    }
    preview::sheet("FRACTAL TOUR TARGETS", tiles, ages.len())
}

fn main() {
    let a = parse_args();
    let opts = PreviewOpts {
        panel: Panel::new(a.levels),
        scale: a.scale,
        ..Default::default()
    };

    if a.bench > 0 {
        let mut piece = make_piece(&a);
        let mut f = Frame::black();
        let mut times = Vec::new();
        // One warm-up frame, then the measurement.
        render_at(piece.as_mut(), Duration::from_secs_f64(a.start), a.indexed, &mut f);
        for i in 0..a.bench {
            let t = Duration::from_secs_f64(
                a.start + a.seconds * i as f64 / a.bench.max(1) as f64,
            );
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

    let out = a.out.clone().unwrap_or_else(|| {
        PathBuf::from(format!("/tmp/preview-{}.png", a.piece))
    });

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
