//! The ten-line embedding.
//!
//! Everything below the `stream` function is argument parsing and a palette
//! animation to have something to look at; `stream` itself is the whole API
//! an embedder needs.
//!
//! ```sh
//! # against the simulator, which is what you should develop against
//! cargo run -p screeny-sim -- --headless &
//! cargo run --release -p screeny --example embed -- --addr 127.0.0.1:49374
//!
//! # against whatever is advertising on the LAN
//! cargo run --release -p screeny --example embed
//! ```
//!
//! Use `--release`. A debug build's encoder is roughly ten times slower.

use std::time::Duration;

use screeny::{Link, LinkConfig, Pixels, Target};

// ---------------------------------------------------------------------------
// The ten lines
// ---------------------------------------------------------------------------

fn stream(target: Target, art: &mut impl FnMut(f64) -> (Vec<[u8; 3]>, Vec<u8>)) -> screeny::Result<()> {
    let mut link = Link::open(target, LinkConfig::default())?;
    let mut pace = link.pacer();
    loop {
        let t = pace.tick();
        let (palette, indices) = art(t.secs());
        link.send(Pixels::indexed(&palette, &indices))?;
    }
}

// ---------------------------------------------------------------------------
// Everything else is this example having something to send
// ---------------------------------------------------------------------------

/// Sixteen colours of moving diagonal stripes: an exact frame, about 300
/// bytes on the wire, and the palette rotates so the motion costs nothing.
fn stripes(t: f64) -> (Vec<[u8; 3]>, Vec<u8>) {
    const N: usize = 16;
    let phase = t * 0.35;
    let palette: Vec<[u8; 3]> = (0..N)
        .map(|i| {
            let a = (i as f64 / N as f64 + phase) * std::f64::consts::TAU;
            let f = |o: f64| (((a + o).sin() * 0.5 + 0.5) * 220.0 + 20.0) as u8;
            [f(0.0), f(2.1), f(4.2)]
        })
        .collect();
    let indices: Vec<u8> = (0..64 * 32)
        .map(|p| {
            let (x, y) = (p % 64, p / 64);
            ((x + y * 2) / 3 % N) as u8
        })
        .collect();
    (palette, indices)
}

fn main() {
    // `--addr IP[:PORT]` skips discovery; with nothing, browse `_screeny._udp`.
    let mut args = std::env::args().skip(1);
    let mut target = Target {
        timeout: Some(Duration::from_secs(3)),
        ..Target::default()
    };
    while let Some(a) = args.next() {
        match a.as_str() {
            "--addr" => {
                let v = args.next().unwrap_or_default();
                let v = if v.contains(':') { v } else { format!("{v}:49374") };
                target.addr = Some(v.parse().expect("--addr IP[:PORT]"));
            }
            "--name" => target.name = args.next(),
            other => {
                eprintln!("usage: embed [--addr IP[:PORT]] [--name NAME]\nunexpected {other:?}");
                std::process::exit(2);
            }
        }
    }

    println!("streaming 16-colour stripes; ctrl-c to stop");
    if let Err(e) = stream(target, &mut stripes) {
        eprintln!("screeny: {e}");
        if let Some(hint) = e.hint() {
            eprintln!("\n{hint}");
        }
        std::process::exit(1);
    }
}
