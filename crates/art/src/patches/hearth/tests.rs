//! Card 372 acceptance tests.

use super::*;
use crate::patch::Params;

fn ctx_at<'a>(t: f64, dt: f64, params: &'a Params) -> Ctx<'a> {
    Ctx { t, dt, now: 0.0, params }
}

const DT: f64 = 1.0 / 30.0;

fn indices(frame: &Frame) -> &[u8] {
    match frame {
        Frame::Indexed { indices, .. } => indices,
        Frame::Linear(_) => panic!("hearth paints by index"),
    }
}

/// Mean linear light of a frame: the whole-frame brightness a viewer feels.
fn level(frame: &Frame) -> f32 {
    (0..N).map(|i| frame.pixel(i).luma()).sum::<f32>() / N as f32
}

#[test]
fn the_same_seed_burns_the_same_fire() {
    let params = Params::defaults(PARAMS);
    let (mut a, mut b) = (Hearth::new(7), Hearth::new(7));
    let mut c = Hearth::new(8);
    let mut differs = false;
    for i in 1..=240 {
        let ctx = ctx_at(i as f64 * DT, DT, &params);
        let (fa, fb, fc) = (a.render(&ctx), b.render(&ctx), c.render(&ctx));
        assert_eq!(indices(&fa), indices(&fb), "frame {i}");
        differs |= indices(&fa) != indices(&fc);
    }
    assert!(differs, "another seed is another fire");
}

/// The first frame is already a fire, not an empty grate catching.
#[test]
fn the_first_frame_is_already_burning() {
    let params = Params::defaults(PARAMS);
    let mut h = Hearth::new(3);
    let f = h.render(&ctx_at(0.0, 0.0, &params));
    let (lit, _, base, _) = measure(&f);
    assert!(lit > 0.01 && base > 5, "lit {lit}, base {base}");
}

/// Every frame is at most 32 colours - exact on the wire whatever it looks
/// like - and the pipeline agrees.
#[test]
fn every_frame_goes_out_exact() {
    let params = Params::defaults(PARAMS);
    let mut h = Hearth::new(5);
    let mut output = crate::pipeline::Output::default();
    output.limiter.enabled = false;
    let mut pipe = crate::Pipeline::new(output);
    for i in 1..=300 {
        let f = h.render(&ctx_at(i as f64 * DT, DT, &params));
        let Frame::Indexed { palette, .. } = &f else { panic!("indexed") };
        assert!(palette.len() <= crate::frame::GUARANTEED_PALETTE, "{} colours", palette.len());
        if i % 30 == 0 {
            assert!(pipe.process(f, DT).stats.exact, "frame {i}");
        }
    }
}

/// No white anywhere, at any setting: the hottest colour is a pale gold,
/// blue well under red, and nothing near full brightness.
#[test]
fn the_palette_has_no_white_and_color_zero_is_grey() {
    let mut h = Hearth::new(1);
    for c in h.palette(1.0) {
        assert!(c.b <= 0.6 * c.r + 1e-3 && c.luma() < 0.6, "{c:?}");
    }
    for c in h.palette(0.0) {
        assert!((c.r - c.g).abs() < 0.02 && (c.g - c.b).abs() < 0.02, "grayscale: {c:?}");
    }
}

/// HA moves `size` whenever it likes; the fire follows over tens of
/// seconds and never jumps.
#[test]
fn size_eases_and_never_jumps() {
    let mut params = Params::defaults(PARAMS);
    params.set(PARAMS, "size", 0.0);
    let mut h = Hearth::new(2);
    let mut t = 0.0;
    let mut tick = |h: &mut Hearth, params: &Params| {
        t += DT;
        h.render(&ctx_at(t, DT, params))
    };
    for _ in 0..300 {
        tick(&mut h, &params);
    }
    assert!(h.eased_size() < 0.01, "it started where it was told");
    params.set(PARAMS, "size", 1.0);
    let mut last = h.eased_size();
    for i in 0..30 * 120 {
        tick(&mut h, &params);
        let now = h.eased_size();
        assert!(now - last < 0.004, "a jump of {} at step {i}", now - last);
        last = now;
        if i == 30 * 5 {
            assert!(now < 0.3, "five seconds in it is still mostly the small fire: {now}");
        }
        if i == 30 * 30 {
            assert!(now > 0.6, "thirty seconds in it has mostly arrived: {now}");
        }
    }
    assert!(last > 0.98);
}

/// The default picture: flames at the bottom, black above. Over a minute
/// the top third is almost entirely dark (the odd ember and wisp only), and
/// there is fire along the bottom almost all the time.
#[test]
fn the_top_is_mostly_black_at_the_default() {
    let params = Params::defaults(PARAMS);
    for seed in [1, 2, 3] {
        let mut h = Hearth::new(seed);
        let (mut top_lit, mut frames, mut no_fire) = (0usize, 0usize, 0usize);
        for i in 1..=1800 {
            let f = h.render(&ctx_at(i as f64 * DT, DT, &params));
            let idx = indices(&f);
            top_lit += idx[..W * H / 3].iter().filter(|k| **k != 0).count();
            frames += 1;
            no_fire += usize::from(idx[(H - 1) * W..].iter().all(|k| *k == 0));
        }
        let share = top_lit as f32 / (frames * W * H / 3) as f32;
        assert!(share < 0.01, "seed {seed}: {:.2}% of the top third lit", share * 100.0);
        assert!(no_fire < frames / 50, "seed {seed}: {no_fire} frames with nothing burning at the bottom");
    }
}

/// Fire flickers locally, but the frame as a whole never pulses: the
/// part of the whole-frame brightness that changes faster than 3 Hz is a
/// small fraction of it.
#[test]
fn no_whole_frame_pulse_above_three_hertz() {
    for size in [0.5, 1.0] {
        let mut params = Params::defaults(PARAMS);
        params.set(PARAMS, "size", size);
        let mut h = Hearth::new(4);
        let levels: Vec<f32> = (1..=1800).map(|i| level(&h.render(&ctx_at(i as f64 * DT, DT, &params)))).collect();
        // A 3 Hz low-pass: a moving average over a third of a second (ten
        // frames). What is left over is the fast part.
        let mean = levels.iter().sum::<f32>() / levels.len() as f32;
        let mut worst = 0.0f32;
        for i in 5..levels.len() - 5 {
            let slow = levels[i - 5..i + 5].iter().sum::<f32>() / 10.0;
            worst = worst.max((levels[i] - slow).abs());
        }
        assert!(worst < 0.15 * mean, "size {size}: a {worst:.4} swing on a mean of {mean:.4}");
    }
}

/// Twenty minutes at the default: it never repeats itself, gusts and log
/// settles both happen (rarely), and embers stay few outside a settle's
/// burst.
#[test]
fn twenty_minutes_does_not_loop_and_the_rare_things_happen() {
    let params = Params::defaults(PARAMS);
    let mut h = Hearth::new(11);
    let dt = 0.1; // ten frames a second is plenty to watch; the sim steps at its own rate
    let mut prints: Vec<u64> = Vec::new();
    let (mut ember_frames, mut ember_sum, mut ember_max_calm) = (0usize, 0usize, 0usize);
    for i in 1..=(20 * 60 * 10) {
        let f = h.render(&ctx_at(i as f64 * dt, dt, &params));
        if !h.settling() {
            ember_sum += h.ember_count();
            ember_frames += 1;
            ember_max_calm = ember_max_calm.max(h.ember_count());
        }
        if i % 10 == 0 {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            indices(&f).hash(&mut hasher);
            prints.push(hasher.finish());
        }
    }
    for a in 0..prints.len() {
        for b in a + 30..prints.len() {
            assert_ne!(prints[a], prints[b], "second {b} repeats second {a}");
        }
    }
    let gusts = h.gusts_seen;
    let settles = h.settles_seen;
    eprintln!("twenty minutes: {gusts} gusts, {settles} settles, mean {:.2} embers (max {ember_max_calm} outside a burst)", ember_sum as f32 / ember_frames as f32);
    assert!((2..=20).contains(&gusts), "{gusts} gusts in twenty minutes");
    assert!((1..=15).contains(&settles), "{settles} settles in twenty minutes");
    let mean = ember_sum as f32 / ember_frames as f32;
    assert!(mean < 1.5, "{mean:.2} embers on average");
    assert!(ember_max_calm <= 5, "{ember_max_calm} embers at once outside a burst");
}

/// Not a test: `HEARTH_DUMP=dir cargo test -p screeny-art hearth::tests::dump -- --ignored`
/// writes the whole grid (logs included) as greyscale temperature and
/// smoke, for tuning by eye.
#[test]
#[ignore]
fn dump() {
    let Ok(dir) = std::env::var("HEARTH_DUMP") else { return };
    let at: f64 = std::env::var("HEARTH_AT").ok().and_then(|s| s.parse().ok()).unwrap_or(10.0);
    let size: f32 = std::env::var("HEARTH_SIZE").ok().and_then(|s| s.parse().ok()).unwrap_or(0.5);
    let mut params = Params::defaults(PARAMS);
    params.set(PARAMS, "size", size);
    let mut h = Hearth::new(1);
    let dt = 1.0 / 30.0;
    let n = (at / dt) as usize;
    for i in 1..=n {
        h.render(&ctx_at(i as f64 * dt, dt, &params));
    }
    let f = &h.fluid;
    let mut rgb = Vec::with_capacity(GW * GH * 3);
    for y in (0..GH).rev() {
        for x in 0..GW {
            let i = y * GW + x;
            let line = if y == BELOW { 60 } else { 0 };
            rgb.push(((f.temp[i] * 255.0) as u8).max(line));
            rgb.push((f.smoke[i].min(1.0) * 255.0) as u8);
            rgb.push((f.fuel[i].min(1.0) * 255.0) as u8);
        }
    }
    let path = format!("{dir}/grid-{at}-{size}.png");
    let file = std::fs::File::create(&path).expect("create");
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), GW as u32, GH as u32);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().and_then(|mut w| w.write_image_data(&rgb)).expect("png");
    let vmax = f.v.iter().fold(0.0f32, |a, b| a.max(b.abs()));
    let umax = f.u.iter().fold(0.0f32, |a, b| a.max(b.abs()));
    let tmax = f.temp.iter().fold(0.0f32, |a, b| a.max(*b));
    eprintln!("{path}: vmax {vmax:.1} umax {umax:.1} tmax {tmax:.2} embers {}", h.embers.live.len());
}

/// Per-frame measures of a painted frame: lit fraction, the height (in LED
/// rows from the bottom) of the tallest fire, how many columns have fire in
/// the bottom row, and how many pixels are smoke.
fn measure(frame: &Frame) -> (f32, usize, usize, usize) {
    let Frame::Indexed { indices, .. } = frame else { panic!("hearth paints by index") };
    let mut lit = 0;
    let mut tallest = 0;
    let mut smoke = 0;
    for (i, k) in indices.iter().enumerate() {
        let k = *k as usize;
        if k == 0 {
            continue;
        }
        lit += 1;
        if k > FIRE_STEPS {
            smoke += 1;
        } else {
            tallest = tallest.max(H - i / W);
        }
    }
    let base = (0..W).filter(|x| (1..=FIRE_STEPS).contains(&(indices[(H - 1) * W + x] as usize))).count();
    (lit as f32 / N as f32, tallest, base, smoke)
}

/// Not a test: how long a step and a paint take on this machine.
/// `cargo test -p screeny-art --release hearth::tests::cost -- --ignored --nocapture`
#[test]
#[ignore]
fn cost() {
    let params = Params::defaults(PARAMS);
    let mut h = Hearth::new(1);
    h.render(&ctx_at(0.0, 0.0, &params));
    let ctx = ctx_at(0.0, 0.0, &params);
    let start = std::time::Instant::now();
    for _ in 0..600 {
        h.step(&ctx);
    }
    let step = start.elapsed().as_secs_f64() / 600.0;
    let start = std::time::Instant::now();
    for _ in 0..300 {
        std::hint::black_box(h.paint(&ctx));
    }
    let paint = start.elapsed().as_secs_f64() / 300.0;
    eprintln!("step {:.3} ms, paint {:.3} ms", step * 1e3, paint * 1e3);
}

/// Not a test: prints how big the fire is over a minute at a few sizes.
/// `cargo test -p screeny-art hearth::tests::survey -- --ignored --nocapture`
#[test]
#[ignore]
fn survey() {
    let sizes: Vec<f32> = std::env::var("HEARTH_SIZES")
        .map(|s| s.split(',').filter_map(|v| v.parse().ok()).collect())
        .unwrap_or_else(|_| vec![0.0, 0.5, 1.0]);
    for size in sizes {
        for seed in [1, 2, 3] {
            let mut params = Params::defaults(PARAMS);
            params.set(PARAMS, "size", size);
            let mut h = Hearth::new(seed);
            let dt = 1.0 / 30.0;
            let (mut lit, mut tall, mut base, mut empty, mut tallmax, mut smoke, mut n) = (0.0, 0, 0, 0, 0, 0, 0);
            for i in 1..=1800 {
                let f = h.render(&ctx_at(i as f64 * dt, dt, &params));
                if i % 10 == 0 {
                    let (l, t, b, s) = measure(&f);
                    lit += l;
                    tall += t;
                    tallmax = tallmax.max(t);
                    base += b;
                    smoke += s;
                    empty += usize::from(t == 0);
                    n += 1;
                }
            }
            eprintln!(
                "size {size:.2} seed {seed}: lit {:.1}%  mean tallest {:.1} (max {tallmax})  base cols {:.1}  smoke px {:.1}  empty {empty}/{n}",
                100.0 * lit / n as f32,
                tall as f32 / n as f32,
                base as f32 / n as f32,
                smoke as f32 / n as f32
            );
        }
    }
}
