//! Temporary (card 178): what one studio tick costs, patch by patch.
//!
//! The studio's engine renders a patch and runs the pipeline over it every
//! tick, so that pair is the number the load-sensitive tests pay for.
use screeny_art::patch::{Ctx, Params};
use screeny_art::patches::{needs_gpu, ALL};
use screeny_art::pipeline::{Output, Pipeline};
use std::time::Instant;

fn main() {
    println!("{:<18} {:>10} {:>10} {:>10}", "patch", "render", "+pipeline", "tick");
    for def in ALL.iter().filter(|d| !needs_gpu(d.id)) {
        let params = Params::defaults(def.params);
        let mut patch = (def.make)(7);
        let mut pipeline = Pipeline::new(Output::default());
        let tick = |patch: &mut Box<dyn screeny_art::patch::Patch>, pipeline: &mut Pipeline, i: u32| {
            let t = f64::from(i) / 30.0;
            let f = patch.render(&Ctx { t, dt: 1.0 / 30.0, now: 1_700_000_000.0 + t, params: &params });
            std::hint::black_box(pipeline.process(f, 1.0 / 30.0));
        };
        for i in 0..30 {
            tick(&mut patch, &mut pipeline, i);
        }
        let n = 300u32;

        let start = Instant::now();
        for i in 0..n {
            let t = f64::from(i) / 30.0;
            std::hint::black_box(patch.render(&Ctx { t, dt: 1.0 / 30.0, now: 1_700_000_000.0 + t, params: &params }));
        }
        let render = start.elapsed().as_secs_f64() * 1000.0 / f64::from(n);

        let start = Instant::now();
        for i in 0..n {
            tick(&mut patch, &mut pipeline, i);
        }
        let whole = start.elapsed().as_secs_f64() * 1000.0 / f64::from(n);

        println!(
            "{:<18} {render:>9.3}ms {:>9.3}ms {whole:>9.3}ms   ({:.1}% of a 33.3 ms tick)",
            def.id,
            whole - render,
            whole / 33.333 * 100.0
        );
    }
}
