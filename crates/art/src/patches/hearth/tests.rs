//! Card 372 acceptance tests.

use super::*;
use crate::patch::Params;

fn ctx_at<'a>(t: f64, dt: f64, params: &'a Params) -> Ctx<'a> {
    Ctx { t, dt, now: 0.0, params }
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
