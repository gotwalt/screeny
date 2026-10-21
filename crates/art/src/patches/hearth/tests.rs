//! Card 372 acceptance tests.

use super::*;
use crate::patch::Params;

fn ctx_at<'a>(t: f64, dt: f64, params: &'a Params) -> Ctx<'a> {
    Ctx { t, dt, now: 0.0, params }
}

const DT: f64 = 1.0 / 30.0;

/// The fire needs a graphics adapter; a machine without one skips these
/// (as `tests/rate.rs` skips GPU patches), saying so. With one, the guard
/// runs this module's GPU tests one at a time: they share one GPU, and
/// `the_first_frame_comes_quickly` measures wall time.
fn gpu() -> Option<std::sync::MutexGuard<'static, ()>> {
    static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());
    if crate::gpu::Gpu::shared().is_err() {
        eprintln!("hearth: no graphics adapter here; skipping");
        return None;
    }
    Some(ONE_AT_A_TIME.lock().unwrap_or_else(std::sync::PoisonError::into_inner))
}

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
    let Some(_gpu) = gpu() else { return };
    let params = Params::defaults(PARAMS);
    let (mut a, mut b) = (Hearth::new(7), Hearth::new(7));
    let mut c = Hearth::new(8);
    let mut differs = false;
    for i in 1..=90 {
        let ctx = ctx_at(i as f64 * DT, DT, &params);
        let (fa, fb, fc) = (a.render(&ctx), b.render(&ctx), c.render(&ctx));
        assert_eq!(indices(&fa), indices(&fb), "frame {i}");
        differs |= indices(&fa) != indices(&fc);
    }
    assert!(differs, "another seed is another fire");
}

/// Card 373's bug: the Studio drops a patch whose frame takes 5 s
/// (`crates/studio/src/channel.rs`, `WATCHDOG`), and the old five-second
/// warm-up inside the first render took ~9 s on an M4. There is no warm-up
/// now; the first frame - pipelines and all - is quick. (The GPU device is
/// opened before the clock starts: the Studio has it open already.)
#[test]
fn the_first_frame_comes_quickly() {
    let Some(_gpu) = gpu() else { return };
    let params = Params::defaults(PARAMS);
    let mut h = Hearth::new(1);
    let start = std::time::Instant::now();
    h.render(&ctx_at(0.0, 0.0, &params));
    let took = start.elapsed().as_secs_f64();
    eprintln!("hearth: first frame in {took:.2} s");
    assert!(took < 1.0, "the first frame took {took:.2} s; the Studio's watchdog is 5 s");
}

/// Not a test: where the first frame's time goes.
#[test]
#[ignore]
fn first_frame_parts() {
    let Ok(gpu) = crate::gpu::Gpu::shared() else { return };
    let params = Params::defaults(PARAMS);
    let mut h = Hearth::new(1);
    let t = std::time::Instant::now();
    h.sim = Some(Some(Sim::new(gpu, h.grid.dims(), h.grid.sweeps, &h.ramp.table())));
    let built = t.elapsed().as_secs_f64();
    let t = std::time::Instant::now();
    h.render(&ctx_at(0.0, 0.0, &params));
    eprintln!("pipelines {built:.2} s, first step and picture {:.2} s", t.elapsed().as_secs_f64());
}

/// With no warm-up the fire lights on screen (card 373, the owner's call):
/// within a few seconds of the first frame it is burning along the bottom.
#[test]
fn the_fire_lights_within_seconds() {
    let Some(_gpu) = gpu() else { return };
    let params = Params::defaults(PARAMS);
    let mut h = Hearth::new(3);
    let mut lit_at = None;
    for i in 0..=30 * 6 {
        let f = h.render(&ctx_at(i as f64 * DT, if i == 0 { 0.0 } else { DT }, &params));
        let (lit, _, base, _) = measure(&f);
        if lit > 0.03 && base > 15 {
            lit_at = Some(i as f64 * DT);
            break;
        }
    }
    let at = lit_at.expect("a fire within six seconds");
    eprintln!("hearth: burning {at:.2} s after the first frame");
    assert!(at < LIGHTS_WITHIN, "burning only after {at:.2} s");
}

/// How long the fire may take to catch, seconds.
const LIGHTS_WITHIN: f64 = 3.0;

/// Card 373: soot is neutral. Its palette entries are greys (OKLCH chroma
/// near 0) whatever `color` is, and none of them is a fire colour.
#[test]
fn smoke_is_grey() {
    let mut h = Hearth::new(1);
    for color in [1.0, 0.5, 0.0] {
        let pal = h.palette(color, 0).colours;
        for c in &pal[FIRE_STEPS + 1..] {
            let lab = crate::color::to_oklab(*c);
            let chroma = (lab[1] * lab[1] + lab[2] * lab[2]).sqrt();
            assert!(chroma < 0.01, "smoke entry {c:?} has chroma {chroma}");
        }
    }
}

/// Card 373: the firebox ceiling is solid. After a few seconds of stove at
/// full size and draft, nothing moves in the ceiling and nothing has got
/// into it - no heat, no smoke - while the flue above it carries gas up.
#[test]
fn the_ceiling_is_solid_and_the_flue_draws() {
    let Some(_gpu) = gpu() else { return };
    let mut params = Params::defaults(PARAMS);
    params.set(PARAMS, "size", 1.0);
    params.set(PARAMS, "draft", 1.0);
    let mut h = Hearth::new(3);
    for i in 1..=120 {
        h.render(&ctx_at(i as f64 * DT, DT, &params));
    }
    let Some(Some(sim)) = &h.sim else { return };
    let (vel, scal) = (sim.velocities(), sim.scalars());
    let flue = h.flue * h.grid.k;
    let (gw, gh, gd) = (h.grid.gw(), h.grid.gh(), h.grid.gd());
    let in_flue = |x: usize, z: usize| {
        let (dx, dz) = (x as f32 - 0.5 * (gw - 1) as f32, z as f32 - 0.5 * (gd - 1) as f32);
        dx * dx + dz * dz < flue * flue
    };
    let (mut up, mut n) = (0.0f32, 0);
    for z in 0..gd {
        for y in h.grid.ceiling()..gh {
            for x in 0..gw {
                let i = (z * gh + y) * gw + x;
                if in_flue(x, z) {
                    up += vel[i][1];
                    n += 1;
                } else {
                    assert_eq!(vel[i], [0.0; 4], "gas moving inside the ceiling at ({x}, {y}, {z})");
                    assert_eq!(scal[i], [0.0; 4], "heat or smoke inside the ceiling at ({x}, {y}, {z})");
                }
            }
        }
    }
    let up = up / n as f32;
    assert!(up > 10.0, "the flue draws up at {up:.1} cells/s on average");
}

/// The new parameters are as deterministic as the old.
#[test]
fn the_stove_is_deterministic() {
    let Some(_gpu) = gpu() else { return };
    let mut params = Params::defaults(PARAMS);
    params.set(PARAMS, "draft", 0.9);
    params.set(PARAMS, "flue", 0.8);
    params.set(PARAMS, "smoke", 0.7);
    let (mut a, mut b) = (Hearth::new(9), Hearth::new(9));
    for i in 1..=60 {
        let ctx = ctx_at(i as f64 * DT, DT, &params);
        assert_eq!(indices(&a.render(&ctx)), indices(&b.render(&ctx)), "frame {i}");
    }
}

/// Card 375: with any level ramp, at any `color`, every channel of every
/// palette entry below level 16 is on the code the panel shows steadily, and
/// the soot is neutral and no brighter than level 2.
#[test]
fn the_dark_ramps_sit_on_panel_levels() {
    use screeny_panel::model::{duty_16ths, nearest_level};
    let mut h = Hearth::new(1);
    let ceiling = crate::panel::level_triple([2, 2, 2]).luma();
    for (dark, name) in DARK_CHOICES.iter().enumerate().skip(1) {
        for color in [1.0, 0.6, 0.0] {
            let p = h.palette(color, dark);
            for (i, c) in p.colours.iter().enumerate() {
                for v in [c.r, c.g, c.b] {
                    let code = crate::color::linear_to_srgb8(v);
                    let (level, _) = nearest_level(duty_16ths(code));
                    if level < crate::panel::DARK_ALIGN_LEVEL {
                        assert_eq!(code, crate::panel::level_code(level), "{name} entry {i}: {c:?}");
                    }
                }
            }
            for c in &p.colours[FIRE_STEPS + 1..] {
                assert!(c.r == c.g && c.g == c.b, "soot is neutral: {c:?}");
                assert!(c.luma() <= ceiling + 1e-6, "soot no brighter than (2,2,2): {c:?}");
            }
        }
    }
}

/// Card 375: hearth chooses its own dark shades, so the output's
/// `aligned_dark` backstop has nothing left to do: a frame goes out the same
/// through it as through the plain dithered panel.
#[test]
fn aligned_dark_changes_no_hearth_pixel() {
    let Some(_gpu) = gpu() else { return };
    let mut params = Params::defaults(PARAMS);
    params.set(PARAMS, "smoke", 1.0);
    params.set(PARAMS, "size", 1.0);
    let mut h = Hearth::new(4);
    let pipe = |panel| {
        let mut o = crate::pipeline::Output { panel, ..Default::default() };
        o.limiter.enabled = false;
        crate::Pipeline::new(o)
    };
    let (mut plain, mut aligned) = (pipe(crate::panel::Panel::Dithered), pipe(crate::panel::Panel::AlignedDark));
    for i in 1..=120 {
        let f = h.render(&ctx_at(i as f64 * DT, DT, &params));
        if i % 10 == 0 {
            let a = plain.process(f.clone(), DT).preview;
            let b = aligned.process(f, DT).preview;
            assert_eq!(a, b, "frame {i}");
        }
    }
}

/// Card 375: every slider but `flue` and `color` runs to 2. At the all-2
/// corner the fire must stay a fire for minutes: burning every frame,
/// nothing non-finite in the fields, no white in what goes out.
#[test]
fn the_all_max_corner_stays_stable() {
    let Some(_gpu) = gpu() else { return };
    let mut params = Params::defaults(PARAMS);
    for id in ["size", "gusts", "embers", "smoke", "draft"] {
        params.set(PARAMS, id, 2.0);
    }
    let mut h = Hearth::new(6);
    let mut pipe = crate::Pipeline::new(crate::pipeline::Output::default());
    // Three minutes, a frame every quarter second (the sim still steps
    // every 1/30 s).
    let dt = 0.25;
    let (mut apl, mut gain, mut n) = (0.0f32, 1.0f32, 0);
    for i in 1..=(3 * 60 * 4) {
        let f = h.render(&ctx_at(i as f64 * dt, dt, &params));
        if i as f64 * dt > LIGHTS_WITHIN {
            let bottom = &indices(&f)[(H - 1) * W..];
            assert!(bottom.iter().any(|k| *k != 0), "nothing burning at {:.1} s", i as f64 * dt);
        }
        let Frame::Indexed { palette, .. } = &f else { panic!("indexed") };
        for c in palette {
            assert!(c.b <= 0.6 * c.r + 1e-3 || (c.r - c.b).abs() < 1e-3, "white-ish entry {c:?}");
        }
        let out = pipe.process(f, dt);
        apl += out.stats.apl;
        gain = gain.min(out.stats.limiter_gain);
        n += 1;
    }
    let Some(Some(sim)) = &h.sim else { return };
    let bad = sim.scalars().iter().chain(sim.velocities().iter()).filter(|c| c.iter().any(|v| !v.is_finite())).count();
    assert_eq!(bad, 0, "non-finite cells");
    eprintln!("hearth all-max: mean APL {:.1}%, lowest limiter gain {gain:.2}", 100.0 * apl / n as f32);
}

/// Every frame is at most 32 colours - exact on the wire whatever it looks
/// like - and the pipeline agrees.
#[test]
fn every_frame_goes_out_exact() {
    let Some(_gpu) = gpu() else { return };
    let params = Params::defaults(PARAMS);
    let mut h = Hearth::new(5);
    let mut output = crate::pipeline::Output::default();
    output.limiter.enabled = false;
    let mut pipe = crate::Pipeline::new(output);
    for i in 1..=150 {
        let f = h.render(&ctx_at(i as f64 * DT, DT, &params));
        let Frame::Indexed { palette, .. } = &f else { panic!("indexed") };
        assert!(palette.len() <= crate::frame::GUARANTEED_PALETTE, "{} colours", palette.len());
        if i % 30 == 0 {
            assert!(pipe.process(f, DT).stats.exact, "frame {i}");
        }
    }
}

/// No white anywhere, at any setting: the hottest colour is a pale gold,
/// blue well under red, and nothing near full brightness. (The fire's
/// entries; card 373's soot greys are dim neutrals, checked in
/// `smoke_is_grey`.)
#[test]
fn the_palette_has_no_white_and_color_zero_is_grey() {
    let mut h = Hearth::new(1);
    let pal = h.palette(1.0, 0).colours;
    for c in &pal[..=FIRE_STEPS] {
        assert!(c.b <= 0.6 * c.r + 1e-3 && c.luma() < 0.6, "{c:?}");
    }
    for c in &pal[FIRE_STEPS + 1..] {
        assert!(c.luma() < 0.06, "soot is dim: {c:?}");
    }
    for c in h.palette(0.0, 0).colours {
        assert!((c.r - c.g).abs() < 0.02 && (c.g - c.b).abs() < 0.02, "grayscale: {c:?}");
    }
}

/// HA moves `size` whenever it likes; the fire follows over tens of
/// seconds and never jumps. The easing is the CPU's: stepped here without a
/// GPU attached (a `Hearth` that has not rendered yet steps everything but
/// the fluid), so a minute and a half costs nothing.
#[test]
fn size_eases_and_never_jumps() {
    let mut params = Params::defaults(PARAMS);
    params.set(PARAMS, "size", 0.0);
    let mut h = Hearth::new(2);
    h.size = 0.0;
    let ctx = ctx_at(0.0, DT, &params);
    h.step(&ctx);
    assert!(h.eased_size() < 0.01, "it started where it was told");
    params.set(PARAMS, "size", 1.0);
    let ctx = ctx_at(0.0, DT, &params);
    let mut last = h.eased_size();
    for i in 1..=30 * 90 {
        h.step(&ctx);
        let now = h.eased_size();
        assert!(now - last < 0.004, "a jump of {} at step {i}", now - last);
        last = now;
        match i {
            150 => assert!(now < 0.3, "five seconds in it is still mostly the small fire: {now}"),
            900 => assert!(now > 0.6, "thirty seconds in it has mostly arrived: {now}"),
            _ => {}
        }
    }
    assert!(last > 0.98, "a minute and a half in it has arrived: {last}");
}

/// And on the GPU: the frame after HA moves `size` all the way is the same
/// picture as the frame before, near enough - the fire grows, it does not
/// jump.
#[test]
fn the_picture_does_not_jump_when_size_moves() {
    let Some(_gpu) = gpu() else { return };
    let mut params = Params::defaults(PARAMS);
    params.set(PARAMS, "size", 0.0);
    let mut h = Hearth::new(2);
    let mut t = 0.0;
    let mut lit = Vec::new();
    for i in 0..60 {
        if i == 30 {
            params.set(PARAMS, "size", 1.0);
        }
        t += DT;
        lit.push(measure(&h.render(&ctx_at(t, DT, &params))).0);
    }
    let natural = (1..30).map(|i| (lit[i] - lit[i - 1]).abs()).fold(0.0f32, f32::max);
    let across = (lit[30] - lit[29]).abs();
    assert!(across <= natural + 0.01, "the change of size moved the picture by {across} (it moves by up to {natural} on its own)");
}

/// The occasional things happen and stay occasional: over twenty minutes at
/// the default (card 373 made the default card 372's maximum), a gust about
/// once a minute, a log settling every few minutes, and a spark every couple
/// of seconds. The dice are the CPU's, so this needs no GPU.
#[test]
fn the_rare_things_happen_rarely() {
    let params = Params::defaults(PARAMS);
    for seed in [11] {
        let mut h = Hearth::new(seed);
        let ctx = ctx_at(0.0, DT, &params);
        for _ in 0..30 * 60 * 20 {
            h.step(&ctx);
            // Nothing reads the GPU back here, so free the slots by hand.
            h.pending.clear();
        }
        let (gusts, settles, sparks) = (h.gusts_seen, h.settles_seen, h.sparks_thrown);
        eprintln!("seed {seed}, twenty minutes: {gusts} gusts, {settles} settles, {sparks} sparks");
        assert!((10..=40).contains(&gusts), "{gusts} gusts");
        assert!((3..=20).contains(&settles), "{settles} settles");
        assert!((300..=1000).contains(&sparks), "{sparks} sparks: about one every couple of seconds");
    }
}

/// The default picture: flames at the bottom, black above. Over a minute
/// the top third is almost entirely dark (the odd ember and wisp only), and
/// there is fire along the bottom almost all the time.
#[test]
fn the_top_is_mostly_black_at_the_default() {
    let Some(_gpu) = gpu() else { return };
    let params = Params::defaults(PARAMS);
    for seed in [1] {
        let mut h = Hearth::new(seed);
        let (mut top_lit, mut frames, mut no_fire) = (0usize, 0usize, 0usize);
        for i in 1..=450 {
            let f = h.render(&ctx_at(i as f64 * DT, DT, &params));
            let idx = indices(&f);
            top_lit += idx[..W * H / 3].iter().filter(|k| **k != 0).count();
            frames += 1;
            // Not while it is still catching (card 373: no warm-up).
            no_fire += usize::from(i as f64 * DT >= LIGHTS_WITHIN && idx[(H - 1) * W..].iter().all(|k| *k == 0));
        }
        let share = top_lit as f32 / (frames * W * H / 3) as f32;
        // 1% for card 372's default; card 373's default is card 372's
        // maximum, whose tallest tongues and sparks do reach the top third.
        assert!(share < 0.03, "seed {seed}: {:.2}% of the top third lit", share * 100.0);
        assert!(no_fire < frames / 50, "seed {seed}: {no_fire} frames with nothing burning at the bottom");
    }
}

/// Fire flickers locally, but the frame as a whole never pulses: the
/// part of the whole-frame brightness that changes faster than 3 Hz is a
/// small fraction of it.
#[test]
fn no_whole_frame_pulse_above_three_hertz() {
    let Some(_gpu) = gpu() else { return };
    for size in [1.0] {
        let mut params = Params::defaults(PARAMS);
        params.set(PARAMS, "size", size);
        // Seed 2 at size 1 is the run where, before the velocity guard
        // (`tame` in the shader), the fire froze into its logs and the
        // frame went black from 28 s to 32 s.
        let mut h = Hearth::new(2);
        let frames: Vec<Frame> = (1..=1050).map(|i| h.render(&ctx_at(i as f64 * DT, DT, &params))).collect();
        let catching = (LIGHTS_WITHIN / DT) as usize;
        let dark = frames.iter().skip(catching).filter(|f| indices(f)[(H - 1) * W..].iter().all(|k| *k == 0)).count();
        assert_eq!(dark, 0, "size {size}: {dark} frames with nothing burning");
        let levels: Vec<f32> = frames.iter().map(level).collect();
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
///
/// Ignored by default: twenty minutes of a 1.2-million-cell fire is a few
/// minutes of GPU. `cargo test -p screeny-art hearth::tests::twenty -- --ignored --nocapture`
#[test]
#[ignore]
fn twenty_minutes_does_not_loop_and_the_rare_things_happen() {
    let Some(_gpu) = gpu() else { return };
    let params = Params::defaults(PARAMS);
    let mut h = Hearth::new(11);
    let dt = 0.1; // ten frames a second is plenty to watch; the sim steps at its own rate
    let mut prints: Vec<u64> = Vec::new();
    let (mut ember_frames, mut ember_sum, mut ember_max_calm) = (0usize, 0usize, 0usize);
    let (mut settles_so_far, mut last_settle) = (0u32, -100.0f64);
    for i in 1..=(20 * 60 * 10) {
        let f = h.render(&ctx_at(i as f64 * dt, dt, &params));
        // A burst's sparks outlive the settle that threw them by a few
        // seconds: count from fifteen seconds after a settle began.
        if h.settles_seen > settles_so_far {
            settles_so_far = h.settles_seen;
            last_settle = i as f64 * dt;
        }
        if i as f64 * dt - last_settle > 15.0 {
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
    assert!((10..=40).contains(&gusts), "{gusts} gusts in twenty minutes");
    assert!((3..=20).contains(&settles), "{settles} settles in twenty minutes");
    let mean = ember_sum as f32 / ember_frames as f32;
    // Card 373 made the default card 372's maximum: a spark every couple of
    // seconds, so a few in the air at once is the look, not a fault.
    assert!(mean < 4.0, "{mean:.2} embers on average");
    assert!(ember_max_calm <= 9, "{ember_max_calm} embers at once outside a burst");
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

/// Not a test: `HEARTH_RAW=dir cargo test -p screeny-art hearth::tests::raw -- --ignored --nocapture`
/// writes the march's continuous output before painting - the fire's light
/// (times `HEARTH_GAIN`) on the left, the lit smoke on the right - and
/// prints its peaks, for tuning by eye.
#[test]
#[ignore]
fn raw() {
    let Ok(dir) = std::env::var("HEARTH_RAW") else { return };
    let env = |k: &str, d: f64| std::env::var(k).ok().and_then(|s| s.parse().ok()).unwrap_or(d);
    let (at, size, gain) = (env("HEARTH_AT", 10.0), env("HEARTH_SIZE", 0.5) as f32, env("HEARTH_GAIN", 1.0) as f32);
    let mut params = Params::defaults(PARAMS);
    params.set(PARAMS, "size", size);
    for kv in std::env::var("HEARTH_SET").unwrap_or_default().split(',') {
        if let Some((k, v)) = kv.split_once('=') {
            params.set(PARAMS, k.trim(), v.trim().parse().expect("a number"));
        }
    }
    let mut h = Hearth::with_grid(env("HEARTH_SEED", 1.0) as u64, grid_from_env());
    for i in 1..=(at / DT) as usize {
        h.render(&ctx_at(i as f64 * DT, DT, &params));
    }
    let p = h.params();
    let Some(Some(sim)) = &h.sim else { return };
    let (leds, _) = sim.picture(&p).expect("a picture");
    let (s, w) = (8usize, 2 * W + 2);
    let mut rgb = vec![40u8; w * s * H * s * 3];
    let enc = |v: f32| (crate::color::linear_to_srgb(v.clamp(0.0, 1.0)) * 255.0) as u8;
    let (mut fmax, mut smax) = (0.0f32, 0.0f32);
    for y in 0..H {
        for x in 0..W {
            let l = leds[y * W + x];
            fmax = fmax.max(l[0]);
            smax = smax.max(l[3]);
            for (ox, c) in [(x, [l[0] * gain, l[1] * gain, l[2] * gain]), (x + W + 2, [l[3]; 3])] {
                for dy in 0..s {
                    for dx in 0..s {
                        let o = ((y * s + dy) * w * s + ox * s + dx) * 3;
                        rgb[o..o + 3].copy_from_slice(&c.map(enc));
                    }
                }
            }
        }
    }
    let path = format!("{dir}/raw-{at}-{size}.png");
    let file = std::fs::File::create(&path).expect("create");
    let mut e = png::Encoder::new(std::io::BufWriter::new(file), (w * s) as u32, (H * s) as u32);
    e.set_color(png::ColorType::Rgb);
    e.set_depth(png::BitDepth::Eight);
    e.write_header().and_then(|mut wr| wr.write_image_data(&rgb)).expect("png");
    eprintln!("{path}: fire red peak {fmax:.3}, smoke peak {smax:.3}");

    // The volume itself, from the front (max over depth) and from the side
    // (max over x): temperature red, smoke green, fuel blue.
    let scal = sim.scalars();
    let (gw, gh, gd) = (h.grid.gw(), h.grid.gh(), h.grid.gd());
    let (vw, vh) = (gw + 4 + gd, gh);
    let mut img = vec![0u8; vw * vh * 3];
    for z in 0..gd {
        for y in 0..gh {
            for x in 0..gw {
                let c = scal[(z * gh + y) * gw + x];
                let v = [c[0], c[2].min(1.0), c[1].min(1.0)].map(|v| (v * 255.0) as u8);
                for (px, py) in [(x, gh - 1 - y), (gw + 4 + z, gh - 1 - y)] {
                    let o = (py * vw + px) * 3;
                    for k in 0..3 {
                        img[o + k] = img[o + k].max(v[k]);
                    }
                }
            }
        }
    }
    for x in 0..vw {
        let o = ((gh - 1 - (BELOW * h.grid.k) as usize) * vw + x) * 3;
        img[o + 2] = img[o + 2].max(120);
    }
    let path = format!("{dir}/vol-{at}-{size}.png");
    let file = std::fs::File::create(&path).expect("create");
    let mut e = png::Encoder::new(std::io::BufWriter::new(file), vw as u32, vh as u32);
    e.set_color(png::ColorType::Rgb);
    e.set_depth(png::BitDepth::Eight);
    e.write_header().and_then(|mut wr| wr.write_image_data(&img)).expect("png");
    let tmax = scal.iter().fold(0.0f32, |a, c| a.max(c[0]));
    let nans = scal.iter().filter(|c| c.iter().any(|v| !v.is_finite())).count();
    let lit = leds.iter().filter(|l| !l.iter().all(|v| v.is_finite())).count();
    eprintln!("{path}: temperature peak {tmax:.2}, {nans} non-finite cells, {lit} non-finite LEDs");
}

/// Not a test: when the gusts and settles of a seed fall, in snapshot time
/// (`--at`), for photographing them. `HEARTH_SEED=n cargo test -p
/// screeny-art hearth::tests::timeline -- --ignored --nocapture`
#[test]
#[ignore]
fn timeline() {
    let seed = std::env::var("HEARTH_SEED").ok().and_then(|s| s.parse().ok()).unwrap_or(1);
    let params = Params::defaults(PARAMS);
    let mut h = Hearth::new(seed);
    let ctx = ctx_at(0.0, DT, &params);
    let (mut gusts, mut settles) = (0, 0);
    for _ in 0..30 * 60 * 10 {
        h.step(&ctx);
        h.pending.clear();
        let at = h.t;
        if h.gusts_seen > gusts {
            gusts = h.gusts_seen;
            let g = h.gust.as_ref().expect("a gust");
            eprintln!("gust at {at:.1} s for {:.1} s, peak {:.1}", g.length, g.peak);
        }
        if h.settles_seen > settles {
            settles = h.settles_seen;
            eprintln!("settle at {at:.1} s");
        }
    }
}

/// Not a test: one run, photographed at several moments, through the same
/// pipeline and dot preview as `screeny-art snapshot --scale 12`. Writes
/// `HEARTH_STRIP/<HEARTH_NAME>-t<seconds>.png` for each of `HEARTH_TIMES`
/// (comma-separated), with `HEARTH_SET="size=1,smoke=0.5"` and
/// `HEARTH_SEED`.
#[test]
#[ignore]
fn strip() {
    let Ok(dir) = std::env::var("HEARTH_STRIP") else { return };
    let name = std::env::var("HEARTH_NAME").unwrap_or_else(|_| "strip".into());
    let seed = std::env::var("HEARTH_SEED").ok().and_then(|s| s.parse().ok()).unwrap_or(1);
    let mut times: Vec<f64> =
        std::env::var("HEARTH_TIMES").unwrap_or_default().split(',').filter_map(|s| s.trim().parse().ok()).collect();
    times.sort_by(f64::total_cmp);
    let mut params = Params::defaults(PARAMS);
    for kv in std::env::var("HEARTH_SET").unwrap_or_default().split(',') {
        if let Some((k, v)) = kv.split_once('=') {
            params.set(PARAMS, k.trim(), v.trim().parse().expect("a number"));
        }
    }
    let mut h = Hearth::with_grid(seed, grid_from_env());
    let mut pipe = crate::Pipeline::new(crate::pipeline::Output::default());
    let last = times.last().copied().unwrap_or(0.0);
    let mut next = 0;
    let mut i = 0usize;
    loop {
        let t = i as f64 * DT;
        if t > last + 1e-6 {
            break;
        }
        let out = pipe.process(h.render(&ctx_at(t, if i == 0 { 0.0 } else { DT }, &params)), DT);
        while next < times.len() && (times[next] - t).abs() < DT * 0.5 {
            let (w, hgt, rgba) = crate::preview::render_dots(&out.preview, 12);
            let path = format!("{dir}/{name}-t{:07.2}.png", times[next]);
            let file = std::fs::File::create(&path).expect("create");
            let mut e = png::Encoder::new(std::io::BufWriter::new(file), w as u32, hgt as u32);
            e.set_color(png::ColorType::Rgba);
            e.set_depth(png::BitDepth::Eight);
            e.write_header().and_then(|mut wr| wr.write_image_data(&rgba)).expect("png");
            eprintln!("{path}: {} colours, exact {}, embers {}", out.stats.distinct_colours, out.stats.exact, h.ember_count());
            next += 1;
        }
        i += 1;
    }
}

/// A device of the same kind [`crate::gpu::Gpu`] opens, plus timestamp
/// queries, for the cost tool.
fn profiling_gpu() -> Option<&'static crate::gpu::Gpu> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle().with_env());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).ok()?;
    if !adapter.features().contains(wgpu::Features::TIMESTAMP_QUERY) {
        eprintln!("hearth: this adapter has no timestamp queries");
        return None;
    }
    let info = adapter.get_info();
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("hearth cost"),
        required_features: wgpu::Features::TIMESTAMP_QUERY,
        required_limits: wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits()),
        ..Default::default()
    }))
    .ok()?;
    Some(Box::leak(Box::new(crate::gpu::Gpu {
        device,
        queue,
        adapter: info.name.clone(),
        backend: format!("{:?}", info.backend),
        software: false,
    })))
}

/// Not a test: card 374's cost table. GPU time per stage per frame, median
/// and 95th percentile over `HEARTH_FRAMES` (default 300) frames after
/// `HEARTH_AT` (default 10) seconds of fire, with `HEARTH_SET` params, and
/// the wall clock per frame alongside.
/// `cargo test -p screeny-art hearth::tests::stages -- --ignored --nocapture`
#[test]
#[ignore]
fn stages() {
    let Some(gpu) = profiling_gpu() else { return };
    let env = |k: &str, d: f64| std::env::var(k).ok().and_then(|s| s.parse().ok()).unwrap_or(d);
    let mut params = Params::defaults(PARAMS);
    for kv in std::env::var("HEARTH_SET").unwrap_or_default().split(',') {
        if let Some((k, v)) = kv.split_once('=') {
            params.set(PARAMS, k.trim(), v.trim().parse().expect("a number"));
        }
    }
    let mut h = Hearth::with_grid(1, grid_from_env());
    let mut sim = Sim::new(gpu, h.grid.dims(), h.grid.sweeps, &h.ramp.table());
    sim.profile = Some(std::cell::RefCell::new(gpu::Profile::new(&gpu.device)));
    h.sim = Some(Some(sim));
    let warm = (env("HEARTH_AT", 10.0) / DT) as usize;
    let frames = env("HEARTH_FRAMES", 300.0) as usize;
    let mut i = 0;
    let mut tick = |h: &mut Hearth| {
        i += 1;
        h.render(&ctx_at(i as f64 * DT, DT, &params));
    };
    for _ in 0..warm {
        tick(&mut h);
    }
    let take = |h: &Hearth| -> Vec<(&'static str, f64)> {
        let Some(Some(sim)) = &h.sim else { return Vec::new() };
        std::mem::take(&mut sim.profile.as_ref().expect("profiling").borrow_mut().times)
    };
    take(&h);
    let mut per_frame: Vec<std::collections::BTreeMap<&'static str, f64>> = Vec::new();
    let mut wall = Vec::new();
    for _ in 0..frames {
        let t = std::time::Instant::now();
        tick(&mut h);
        wall.push(t.elapsed().as_secs_f64() * 1e3);
        let mut f = std::collections::BTreeMap::new();
        for (name, ns) in take(&h) {
            *f.entry(name).or_insert(0.0) += ns / 1e6;
        }
        per_frame.push(f);
    }
    let stat = |mut v: Vec<f64>| {
        v.sort_by(f64::total_cmp);
        (v[v.len() / 2], v[(v.len() * 95 / 100).min(v.len() - 1)])
    };
    let names: Vec<&str> = per_frame[0].keys().copied().collect();
    eprintln!("{:<12} {:>9} {:>9}   (ms per frame, {} frames)", "stage", "median", "p95", frames);
    for name in &names {
        let (m, p) = stat(per_frame.iter().map(|f| f.get(name).copied().unwrap_or(0.0)).collect());
        eprintln!("{name:<12} {m:>9.2} {p:>9.2}");
    }
    let (m, p) = stat(per_frame.iter().map(|f| f.values().sum()).collect());
    eprintln!("{:<12} {m:>9.2} {p:>9.2}", "gpu total");
    let (m, p) = stat(wall);
    eprintln!("{:<12} {m:>9.2} {p:>9.2}", "wall clock");
}

/// Not a test: how long a frame takes on this machine (two steps and a
/// picture). `cargo test -p screeny-art hearth::tests::cost -- --ignored --nocapture`
#[test]
#[ignore]
fn cost() {
    let mut params = Params::defaults(PARAMS);
    for kv in std::env::var("HEARTH_SET").unwrap_or_default().split(',') {
        if let Some((k, v)) = kv.split_once('=') {
            params.set(PARAMS, k.trim(), v.trim().parse().expect("a number"));
        }
    }
    let mut h = Hearth::with_grid(1, grid_from_env());
    // Ten seconds of fire first, then 300 frames timed one by one, as a
    // player would run them: no per-stage waits.
    for i in 0..300 {
        h.render(&ctx_at(i as f64 * DT, if i == 0 { 0.0 } else { DT }, &params));
    }
    let mut times = Vec::new();
    for i in 300..600 {
        let t = std::time::Instant::now();
        h.render(&ctx_at(i as f64 * DT, DT, &params));
        times.push(t.elapsed().as_secs_f64() * 1e3);
    }
    times.sort_by(f64::total_cmp);
    let pick = |q: f64| times[((times.len() - 1) as f64 * q) as usize];
    eprintln!("frame ms: median {:.2}  p95 {:.2}  max {:.2}", pick(0.5), pick(0.95), pick(1.0));
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
            let mut h = Hearth::with_grid(seed, grid_from_env());
            let dt = 1.0 / 30.0;
            let (mut lit, mut tall, mut base, mut empty, mut tallmax, mut smoke, mut n) = (0.0, 0, 0, 0, 0, 0, 0);
            let (mut light, mut gold) = (0.0f32, 0usize);
            for i in 1..=900 {
                let f = h.render(&ctx_at(i as f64 * dt, dt, &params));
                if i % 10 == 0 && i > 90 {
                    let (l, t, b, s) = measure(&f);
                    lit += l;
                    tall += t;
                    tallmax = tallmax.max(t);
                    base += b;
                    smoke += s;
                    empty += usize::from(t == 0);
                    light += level(&f);
                    gold += indices(&f).iter().filter(|k| (FIRE_STEPS - 5..=FIRE_STEPS).contains(&(**k as usize))).count();
                    n += 1;
                }
            }
            eprintln!(
                "size {size:.2} seed {seed}: lit {:.1}%  mean tallest {:.1} (max {tallmax})  base cols {:.1}  smoke px {:.1}  empty {empty}/{n}  level {:.4}  gold px {:.1}",
                100.0 * lit / n as f32,
                tall as f32 / n as f32,
                base as f32 / n as f32,
                smoke as f32 / n as f32,
                light / n as f32,
                gold as f32 / n as f32
            );
        }
    }
}

/// Not a test: the palette's entries as sRGB codes and panel levels.
/// `cargo test -p screeny-art hearth::tests::palette_levels -- --ignored --nocapture`
#[test]
#[ignore]
fn palette_levels() {
    let mut h = Hearth::new(1);
    for (i, c) in h.palette(1.0, 0).colours.iter().enumerate() {
        let code = c.to_srgb8();
        let lv = code.map(|v| screeny_panel::model::nearest_level(screeny_panel::model::duty_16ths(v)));
        eprintln!(
            "{i:>2}  srgb {:>3} {:>3} {:>3}   levels {:>2}{:+} {:>2}{:+} {:>2}{:+}   L {:.3}",
            code[0], code[1], code[2], lv[0].0, lv[0].1, lv[1].0, lv[1].1, lv[2].0, lv[2].1,
            crate::color::to_oklab(*c)[0]
        );
    }
}

/// Not a test: card 375's swatch sheet. Each `dark` choice is a band of
/// eight rows: its 24 fire entries two LEDs wide, a gap, then its soot.
/// Shown through the panel model; writes `HEARTH_SWATCH`.
#[test]
#[ignore]
fn swatches() {
    let Ok(path) = std::env::var("HEARTH_SWATCH") else { return };
    let mut h = Hearth::new(1);
    let mut px = vec![Rgb::BLACK; N];
    for (dark, name) in DARK_CHOICES.iter().enumerate() {
        let p = h.palette(1.0, dark);
        for y in dark * 8 + 1..dark * 8 + 7 {
            for (k, c) in p.colours[1..].iter().enumerate() {
                let x = if k < FIRE_STEPS { 2 * k } else { 2 * k + 4 };
                if x + 1 < W {
                    px[y * W + x] = *c;
                    px[y * W + x + 1] = *c;
                }
            }
        }
        let codes: Vec<String> = p.colours[1..].iter().map(|c| format!("{:?}", c.to_srgb8())).collect();
        eprintln!("{name}: {}", codes.join(" "));
    }
    let out = crate::Pipeline::new(crate::pipeline::Output::default()).process(Frame::Linear(px), DT);
    let (w, hgt, rgba) = crate::preview::render_dots(&out.preview, 12);
    let file = std::fs::File::create(&path).expect("create");
    let mut e = png::Encoder::new(std::io::BufWriter::new(file), w as u32, hgt as u32);
    e.set_color(png::ColorType::Rgba);
    e.set_depth(png::BitDepth::Eight);
    e.write_header().and_then(|mut wr| wr.write_image_data(&rgba)).expect("png");
}

/// For the tools: `HEARTH_GRID="k,depth,sweeps,samples,march"`, any prefix
/// of it, the rest as shipped.
fn grid_from_env() -> Grid {
    let mut g = Grid::SHIPPED;
    let v: Vec<f32> = std::env::var("HEARTH_GRID").unwrap_or_default().split(',').filter_map(|s| s.trim().parse().ok()).collect();
    if let Some(x) = v.first() {
        g.k = *x;
    }
    if let Some(x) = v.get(1) {
        g.depth = *x;
    }
    if let Some(x) = v.get(2) {
        g.sweeps = *x as usize;
    }
    if let Some(x) = v.get(3) {
        g.samples = *x as u32;
    }
    if let Some(x) = v.get(4) {
        g.march = *x;
    }
    g
}
