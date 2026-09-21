//! Headless runner. Renders a patch through the full pipeline and either
//! streams it to a panel, writes raw RGB frames to stdout, or writes a PNG of
//! what the panel should look like.

use screeny_art::output::{Output, PipeOutput};
use screeny_art::panel::Panel;
use screeny_art::patch::{self, Clock, Ctx, Params};
use screeny_art::snapshot::{self, Shot};
// `pipeline::Output` (how a frame is finished for the panel, card 150) is
// spelled out: the sink trait imported above is also called `Output`.
use screeny_art::{patches, pipeline, preview, Pipeline};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(feature = "sender")]
const PLAY_USAGE: &str = "\
  screeny-art play <patch> --to NAME|ADDR [--seed N] [--seconds S] [--panel MODEL] [--wait] [--set id=value]...
";
#[cfg(not(feature = "sender"))]
const PLAY_USAGE: &str = "\
  (this binary was built with --no-default-features, so `play` is not in it;
   the `sender` feature is on by default)
";

const USAGE_HEAD: &str = "\
usage:
  screeny-art list
";
const USAGE_TAIL: &str = "\
  screeny-art pipe <patch> [--seed N] [--seconds S] [--panel MODEL] [--time HH:MM[:SS]] [--set id=value]...
  screeny-art snapshot <patch> --out FILE.png [--at SECONDS] [--warmup 2] [--scale 12] [--seed N] [--panel MODEL] [--time HH:MM[:SS]] [--act ID[@SECONDS]] [--set id=value]...

Everything here runs at 30 frames a second: the panel's rate, and the only one
(card 161). There is no flag for it.

`play` streams to a panel: `--to` takes an mDNS instance name (preferred - the
link re-resolves it, so it follows the device across a DHCP lease) or an
`IP[:PORT]`. `--wait` starts without a panel and picks one up when it appears
instead of failing.

`pipe` writes 6144-byte sRGB frames (64x32, row-major R,G,B) to stdout, paced by
the wall clock. The seed is logged to stderr so a good run can be reproduced.

`--time` tells the patches that tell the time what time it is when the run
starts, instead of the machine's clock, so the same command draws the same
picture today and tomorrow. It is a time of day on a fixed day, and the run
goes forward from it: `snapshot --time 21:11:58 --at 4` renders the four
seconds into 21:12:02. With `--time`, `--at` defaults to 20 s and `--warmup`
to the whole run, because a clock has to be watched from its first frame and
the numerals patch needs about 16 s to dance onto the time it was born on;
give either yourself to change it.

  numerals, settled on 21:12:  snapshot clocks-numerals --time 21:12 --out x.png
  numerals, dancing into it:   snapshot clocks-numerals --time 21:11:20 --at 38 --seed 7 --out x.png
  dials, telling 10:10:        snapshot clocks-dials --time 10:09:50 --at 15 --out x.png

The settled one needs no --seed: once the hands have landed, what is on the
panel is the minute, not the dance that got there. The dancing one does -
`--seed` otherwise comes from the system clock, and the choreography with it.";

fn main() {
    if let Err(e) = run(std::env::args().skip(1).collect()) {
        eprintln!("screeny-art: {e}\n\n{USAGE_HEAD}{PLAY_USAGE}{USAGE_TAIL}");
        std::process::exit(2);
    }
}

struct Args {
    patch: &'static patch::PatchDef,
    params: Params,
    seed: u64,
    seconds: Option<f64>,
    at: f64,
    warmup: f64,
    /// Which of `--at` and `--warmup` have been given, so `--time` may change
    /// their defaults without overriding a choice.
    at_given: bool,
    warmup_given: bool,
    clock: Clock,
    /// One of the patch's own actions, and when to press it.
    act: Option<(String, f64)>,
    scale: usize,
    out: Option<String>,
    to: Option<String>,
    wait: bool,
    output: pipeline::Output,
}

fn run(argv: Vec<String>) -> Result<(), String> {
    let mut it = argv.into_iter();
    match it.next().as_deref() {
        Some("list") => {
            for d in patches::ALL {
                println!("{:<16} {}", d.id, d.blurb);
                for p in d.params {
                    println!("    {:<10} {:>7} .. {:<7} default {:<7} {}", p.id, p.min, p.max, p.default, p.label);
                    // Card 163: a parameter whose values are a list of named
                    // stops names them, rather than hiding the key in a label.
                    if p.switch {
                        println!("    {:<10} {:>7}    {:<7}         {:<7} off / on", "", "", "", "");
                    }
                    for (v, name) in p.choices.iter().enumerate() {
                        println!("    {:<10} {:>7}    {:<7}         {:<7} {v} = {name}", "", "", "", "");
                    }
                }
            }
            Ok(())
        }
        Some("pipe") => pipe(parse(it)?),
        Some("snapshot") => snapshot(parse(it)?),
        #[cfg(feature = "sender")]
        Some("play") => play(parse(it)?),
        #[cfg(not(feature = "sender"))]
        Some("play") => Err("built with --no-default-features, so the `sender` feature is off and there is no network stack in this binary".into()),
        Some(other) => Err(format!("unknown command `{other}`")),
        None => Err("no command".into()),
    }
}

fn parse(mut it: impl Iterator<Item = String>) -> Result<Args, String> {
    let id = it.next().ok_or("which patch? try `screeny-art list`")?;
    let patch = patch::find(&id).ok_or(format!("no patch called `{id}`; try `screeny-art list`"))?;
    let mut a = Args {
        patch,
        params: Params::defaults(patch.params),
        seed: SystemTime::now().duration_since(UNIX_EPOCH).map_or(1, |d| d.subsec_nanos() as u64),
        seconds: None,
        at: 5.0,
        warmup: 2.0,
        at_given: false,
        warmup_given: false,
        clock: Clock::Live,
        act: None,
        scale: 12,
        out: None,
        to: None,
        wait: false,
        output: pipeline::Output::default(),
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
            // Card 161 retired this. Taken and ignored rather than refused:
            // there is one rate now, and a command line somebody has in a
            // shell history or a note should still run - it just says so once,
            // here, instead of quietly doing something different.
            "--fps" => eprintln!("screeny-art: --fps is gone; everything runs at {} fps (card 161)", screeny_art::FPS),
            "--seconds" => a.seconds = Some(num()?),
            "--at" => {
                a.at = num()?;
                a.at_given = true;
            }
            "--warmup" => {
                a.warmup = num()?.max(0.0);
                a.warmup_given = true;
            }
            // Card 162: what time it is, for the patches that tell the time.
            "--time" => a.clock = Clock::parse(&value).map_err(|e| format!("--time: {e}"))?,
            // `--act flip` presses it as the run starts; `--act flip@3` three
            // seconds in, on the same axis as `--at`.
            "--act" => {
                let (id, when) = value.split_once('@').unwrap_or((value.as_str(), "0"));
                let when = when.parse::<f64>().map_err(|_| format!("--act: `{when}` is not a number of seconds"))?;
                if id.is_empty() {
                    return Err("--act needs an action id".to_string());
                }
                a.act = Some((id.to_string(), when));
            }
            "--scale" => a.scale = (num()? as usize).clamp(1, 64),
            // Card 102: the old `--levels 64|32|16` is gone. 32 and 16 were
            // the pre-card-020 "dimmed by scaling" panel, which this device
            // has never been; 64 is the panel without its temporal dither,
            // and that is what `--panel bit-planes` is for.
            "--panel" => {
                a.output.panel = match value.as_str() {
                    "dithered" | "device" => Panel::Dithered,
                    "bit-planes" | "bitplanes" => Panel::BitPlanes,
                    _ => return Err(format!("--panel: `{value}` is not `dithered` or `bit-planes`")),
                }
            }
            "--out" => a.out = Some(value),
            "--to" => a.to = Some(value),
            "--set" => {
                let (k, v) = value.split_once('=').ok_or("--set wants id=value")?;
                let v: f32 = v.parse().map_err(|_| format!("--set {k}: `{v}` is not a number"))?;
                if !a.params.set(patch.params, k, v) {
                    return Err(format!("{} has no parameter `{k}`", patch.id));
                }
            }
            _ => return Err(format!("unknown flag `{flag}`")),
        }
    }
    // Asking for a time of day is asking for a clock that has arrived at it,
    // so `--time` moves two defaults; either given explicitly still wins.
    //
    // `--at 20`, because the numerals patch dances onto the minute it is born
    // on and that takes up to about 16 s (measured over 60 seeds and all 13
    // choreographies: longest 15.4 s). At 20 s every one of them has landed
    // and settled, and none has set off for the next minute, so
    // `--time 21:12` alone is the settled picture of 21:12 - the same PNG
    // whatever the seed.
    //
    // The whole run as `--warmup`, because a clock has to be watched from its
    // first frame: two seconds of it would only show the patch being born.
    if matches!(a.clock, Clock::Pinned(_)) {
        if !a.at_given {
            a.at = 20.0;
        }
        if !a.warmup_given {
            a.warmup = a.at.max(0.0);
        }
    }
    Ok(a)
}

fn pipe(a: Args) -> Result<(), String> {
    eprintln!("screeny-art: patch={} seed={}", a.patch.id, a.seed);
    let mut patch = (a.patch.make)(a.seed);
    let mut pipeline = Pipeline::new(a.output);
    let mut out = PipeOutput(std::io::stdout().lock());
    let period = Duration::from_secs_f64(1.0 / screeny_art::FPS);
    let start = Instant::now();
    let mut next = start;
    let mut last_t = 0.0;
    loop {
        // Wall-clock time, not a frame count: if we fall behind, we skip.
        let t = start.elapsed().as_secs_f64();
        if a.seconds.is_some_and(|s| t >= s) {
            return Ok(());
        }
        let frame = patch.render(&Ctx { t, dt: t - last_t, now: a.clock.now(t), params: &a.params });
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

/// Stream a patch to a panel.
///
/// The loop is `pipe`'s, unchanged: render at [`screeny_art::FPS`] off the wall
/// clock and hand every frame over. That is the panel's own rate, so the link's
/// cadence ceiling has nothing to fold away and the device's superseded counter
/// stays at zero (card 161; before it this rendered at 60 and the link threw
/// half of it away).
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
    eprintln!("screeny-art: patch={} seed={} -> {}", a.patch.id, a.seed, out.status().target);

    // `Drop` sends FINAL, but a signal skips destructors and the panel would
    // then hold the last frame until its stream timeout. Catch it and fall out
    // of the loop instead.
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let _ = ctrlc::set_handler(move || flag.store(true, Ordering::Relaxed));

    let mut patch = (a.patch.make)(a.seed);
    let mut pipeline = Pipeline::new(a.output);
    let period = Duration::from_secs_f64(1.0 / screeny_art::FPS);
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

        let frame = patch.render(&Ctx { t, dt: t - last_t, now: a.clock.now(t), params: &a.params });
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
    // The run itself is `snapshot::take`, which is what the tests render
    // through too, so a pinned picture is checked by the same code the command
    // runs (card 162).
    let shot = Shot { seed: a.seed, at: a.at, warmup: a.warmup, clock: a.clock, output: a.output };
    let act = a.act.as_ref().map(|(id, when)| (id.as_str(), *when));
    let result = snapshot::take_acting(a.patch, &a.params, &shot, act);
    let (w, h, rgba) = preview::render_dots(&result.preview, a.scale);

    let file = std::fs::File::create(&path).map_err(|e| format!("{path}: {e}"))?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w as u32, h as u32);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()
        .and_then(|mut wr| wr.write_image_data(&rgba))
        .map_err(|e| format!("{path}: {e}"))?;

    let s = result.stats;
    // With a pinned clock, say what the panel's own clock reads at this frame:
    // the answer to "which minute did I actually get?" without opening the PNG.
    if let Clock::Pinned(_) = a.clock {
        let now = a.clock.now(a.at);
        let day = now.rem_euclid(86400.0);
        eprintln!(
            "screeny-art: clock pinned, reading {:02}:{:02}:{:05.2} at t={:.2} (run from t={:.2})",
            (day / 3600.0).floor(),
            (day / 60.0).floor() % 60.0,
            day % 60.0,
            a.at,
            (a.at - a.warmup).max(0.0),
        );
    }
    eprintln!(
        "screeny-art: patch={} seed={} t={:.2}  colours={} bytes={}/{} ({}, {})  apl={:.0}% (patch {:.0}%)  limiter x{:.2}",
        a.patch.id,
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
