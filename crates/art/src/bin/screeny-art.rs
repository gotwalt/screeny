//! Headless runner. Renders a piece through the full pipeline and either
//! streams it to a panel, writes raw RGB frames to stdout, or writes a PNG of
//! what the panel should look like.

use screeny_art::output::{Output, PipeOutput};
use screeny_art::piece::{self, local_now, Ctx, Params};
use screeny_art::{pieces, preview, Pipeline, Settings};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(feature = "sender")]
const PLAY_USAGE: &str = "\
  screeny-art play <piece> --to NAME|ADDR [--seed N] [--fps 60] [--seconds S] [--levels 64] [--wait] [--set id=value]...
";
#[cfg(not(feature = "sender"))]
const PLAY_USAGE: &str = "\
  (`play` needs the `sender` feature: cargo run --release -p screeny-art --features sender)
";

const USAGE_HEAD: &str = "\
usage:
  screeny-art list
";
const USAGE_TAIL: &str = "\
  screeny-art pipe <piece> [--seed N] [--fps 60] [--seconds S] [--levels 64] [--set id=value]...
  screeny-art snapshot <piece> --out FILE.png [--at SECONDS] [--warmup 2] [--scale 12] [--seed N] [--levels 64] [--set id=value]...

`play` streams to a panel: `--to` takes an mDNS instance name (preferred - the
link re-resolves it, so it follows the device across a DHCP lease) or an
`IP[:PORT]`. It renders at `--fps` and lets the link decimate to whatever the
panel is keeping up with. `--wait` starts without a panel and picks one up when
it appears instead of failing.

`pipe` writes 6144-byte sRGB frames (64x32, row-major R,G,B) to stdout, paced by
the wall clock. The seed is logged to stderr so a good run can be reproduced.";

fn main() {
    if let Err(e) = run(std::env::args().skip(1).collect()) {
        eprintln!("screeny-art: {e}\n\n{USAGE_HEAD}{PLAY_USAGE}{USAGE_TAIL}");
        std::process::exit(2);
    }
}

struct Args {
    piece: &'static piece::PieceDef,
    params: Params,
    seed: u64,
    fps: f64,
    seconds: Option<f64>,
    at: f64,
    warmup: f64,
    scale: usize,
    out: Option<String>,
    to: Option<String>,
    wait: bool,
    settings: Settings,
}

fn run(argv: Vec<String>) -> Result<(), String> {
    let mut it = argv.into_iter();
    match it.next().as_deref() {
        Some("list") => {
            for d in pieces::ALL {
                println!("{:<16} {}", d.id, d.blurb);
                for p in d.params {
                    println!("    {:<10} {:>7} .. {:<7} default {:<7} {}", p.id, p.min, p.max, p.default, p.label);
                }
            }
            Ok(())
        }
        Some("pipe") => pipe(parse(it)?),
        Some("snapshot") => snapshot(parse(it)?),
        #[cfg(feature = "sender")]
        Some("play") => play(parse(it)?),
        #[cfg(not(feature = "sender"))]
        Some("play") => Err("built without the `sender` feature, so there is no network stack in this binary".into()),
        Some(other) => Err(format!("unknown command `{other}`")),
        None => Err("no command".into()),
    }
}

fn parse(mut it: impl Iterator<Item = String>) -> Result<Args, String> {
    let id = it.next().ok_or("which piece? try `screeny-art list`")?;
    let piece = piece::find(&id).ok_or(format!("no piece called `{id}`; try `screeny-art list`"))?;
    let mut a = Args {
        piece,
        params: Params::defaults(piece.params),
        seed: SystemTime::now().duration_since(UNIX_EPOCH).map_or(1, |d| d.subsec_nanos() as u64),
        fps: 60.0,
        seconds: None,
        at: 5.0,
        warmup: 2.0,
        scale: 12,
        out: None,
        to: None,
        wait: false,
        settings: Settings::default(),
    };
    while let Some(flag) = it.next() {
        // The one flag that stands alone.
        if flag == "--wait" {
            a.wait = true;
            continue;
        }
        let value = it.next().ok_or(format!("{flag} needs a value"))?;
        let num = || value.parse::<f64>().map_err(|_| format!("{flag}: `{value}` is not a number"));
        match flag.as_str() {
            "--seed" => a.seed = value.parse().map_err(|_| format!("--seed: `{value}` is not an integer"))?,
            "--fps" => a.fps = num()?.clamp(1.0, 60.0),
            "--seconds" => a.seconds = Some(num()?),
            "--at" => a.at = num()?,
            "--warmup" => a.warmup = num()?.max(0.0),
            "--scale" => a.scale = (num()? as usize).clamp(1, 64),
            "--levels" => a.settings.levels = num()? as u32,
            "--out" => a.out = Some(value),
            "--to" => a.to = Some(value),
            "--set" => {
                let (k, v) = value.split_once('=').ok_or("--set wants id=value")?;
                let v: f32 = v.parse().map_err(|_| format!("--set {k}: `{v}` is not a number"))?;
                if !a.params.set(piece.params, k, v) {
                    return Err(format!("{} has no parameter `{k}`", piece.id));
                }
            }
            _ => return Err(format!("unknown flag `{flag}`")),
        }
    }
    Ok(a)
}

fn pipe(a: Args) -> Result<(), String> {
    eprintln!("screeny-art: piece={} seed={}", a.piece.id, a.seed);
    let mut piece = (a.piece.make)(a.seed);
    let mut pipeline = Pipeline::new(a.settings);
    let mut out = PipeOutput(std::io::stdout().lock());
    let period = Duration::from_secs_f64(1.0 / a.fps);
    let start = Instant::now();
    let mut next = start;
    let mut last_t = 0.0;
    loop {
        // Wall-clock time, not a frame count: if we fall behind, we skip.
        let t = start.elapsed().as_secs_f64();
        if a.seconds.is_some_and(|s| t >= s) {
            return Ok(());
        }
        let frame = piece.render(&Ctx { t, dt: t - last_t, now: local_now(), params: &a.params });
        let result = pipeline.process(frame, t - last_t);
        last_t = t;
        match out.send(&result.wire) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => return Ok(()),
            Err(e) => return Err(format!("writing frame: {e}")),
        }
        next += period;
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        } else {
            next = now;
        }
    }
}

/// Stream a piece to a panel.
///
/// The loop is `pipe`'s, unchanged: render at `--fps` off the wall clock and
/// hand every frame over. The link owns the cadence and folds away the frames
/// the panel has no slot for, so rendering at 60 into a 30 fps panel puts 30 on
/// the wire and the device's superseded counter stays at zero.
#[cfg(feature = "sender")]
fn play(a: Args) -> Result<(), String> {
    use screeny_art::output::{target_for, SenderOutput};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let to = a.to.as_deref().ok_or("play needs --to NAME|ADDR")?;
    let target = target_for(to);
    let mut out = if a.wait {
        eprintln!("screeny-art: waiting for {to}");
        SenderOutput::deferred(target)
    } else {
        SenderOutput::open(target).map_err(|e| format!("connecting to {to}: {e}"))?
    };
    eprintln!("screeny-art: piece={} seed={} -> {}", a.piece.id, a.seed, out.status().target);

    // `Drop` sends FINAL, but a signal skips destructors and the panel would
    // then hold the last frame until its stream timeout. Catch it and fall out
    // of the loop instead.
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let _ = ctrlc::set_handler(move || flag.store(true, Ordering::Relaxed));

    let mut piece = (a.piece.make)(a.seed);
    let mut pipeline = Pipeline::new(a.settings);
    let period = Duration::from_secs_f64(1.0 / a.fps);
    let start = Instant::now();
    let mut next = start;
    let mut last_t = 0.0;
    let mut last_report = start;
    // Measure the frame against the device that is actually connected, not the
    // defaults: budget and codec set both move under a running link (spec 6.9).
    // Re-read once a second; it is a lock and a clone, not something to do per
    // frame.
    let mut limits_at = start;

    while !stop.load(Ordering::Relaxed) {
        let t = start.elapsed().as_secs_f64();
        if a.seconds.is_some_and(|s| t >= s) {
            break;
        }
        if limits_at.elapsed() >= Duration::from_secs(1) {
            limits_at = Instant::now();
            let lim = out.limits();
            if lim.connected {
                pipeline.meter().set_limits(lim.budget, lim.codecs.clone());
            }
        }

        let frame = piece.render(&Ctx { t, dt: t - last_t, now: local_now(), params: &a.params });
        let result = pipeline.process(frame, t - last_t);
        last_t = t;
        out.send(&result.wire).map_err(|e| format!("sending frame: {e}"))?;

        // One line a second, never one a frame.
        if last_report.elapsed() >= Duration::from_secs(1) {
            last_report = Instant::now();
            let s = out.status();
            eprintln!(
                "  {} {:>5.1} fps  {} offered / {} sent / {} coalesced / {} dropped  last {} {} B {}  exact {} / fallback {}",
                s.state,
                s.fps,
                s.frames_offered,
                s.frames_sent,
                s.frames_coalesced,
                s.frames_dropped,
                s.codec_name.unwrap_or("-"),
                s.bytes.unwrap_or(0),
                if s.exact.unwrap_or(false) { "exact" } else { "lossy" },
                s.indexed_exact,
                s.indexed_fallback,
            );
        }

        next += period;
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        } else {
            next = now;
        }
    }

    let s = out.status();
    eprintln!(
        "screeny-art: {} offered, {} sent, {} coalesced, {} dropped; indexed exact {} / fallback {}; last {} at {} bytes; link {}",
        s.frames_offered,
        s.frames_sent,
        s.frames_coalesced,
        s.frames_dropped,
        s.indexed_exact,
        s.indexed_fallback,
        s.codec_name.unwrap_or("-"),
        s.bytes.unwrap_or(0),
        s.state,
    );
    // Explicit rather than relying on the drop below, so the panel is released
    // before this process spends any time shutting down.
    out.close();
    Ok(())
}

fn snapshot(a: Args) -> Result<(), String> {
    let path = a.out.ok_or("snapshot needs --out FILE.png")?;
    let mut piece = (a.piece.make)(a.seed);
    let mut pipeline = Pipeline::new(a.settings);
    // Run up to the requested moment at 30 fps so stateful pieces and the
    // limiter are where they would be in a live run. Pieces with long-lived
    // state want a longer --warmup.
    let dt = 1.0 / 30.0;
    let first = (a.at - a.warmup).max(0.0);
    let steps = ((a.at - first) / dt).round() as usize;
    // The time of day is simulated too, starting from the real one, so pieces
    // that tell the time see a consistent clock however fast this runs.
    let began = local_now();
    let mut result = None;
    for i in 0..=steps {
        let t = first + i as f64 * dt;
        let frame = piece.render(&Ctx { t, dt, now: began + (t - first), params: &a.params });
        result = Some(pipeline.process(frame, dt));
    }
    let result = result.expect("at least one frame");
    let (w, h, rgba) = preview::render_dots(&result.preview, a.scale);

    let file = std::fs::File::create(&path).map_err(|e| format!("{path}: {e}"))?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w as u32, h as u32);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()
        .and_then(|mut wr| wr.write_image_data(&rgba))
        .map_err(|e| format!("{path}: {e}"))?;

    let s = result.stats;
    eprintln!(
        "screeny-art: piece={} seed={} t={:.2}  colours={} bytes={}/{} ({}, {})  apl={:.0}% (piece {:.0}%)  limiter x{:.2}",
        a.piece.id,
        a.seed,
        a.at,
        s.distinct_colours,
        s.encoded_bytes,
        screeny_art::meter::PAYLOAD_BYTES,
        result.measured.codec_name(),
        if s.exact { "exact" } else { "lossy" },
        s.apl * 100.0,
        s.apl_in * 100.0,
        s.limiter_gain,
    );
    Ok(())
}
