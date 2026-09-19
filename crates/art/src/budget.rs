//! The byte budget, as far as the brief pins it down.
//!
//! PROVISIONAL. The real codec set belongs to the firmware/sender work and is
//! not final. This module only encodes the *shape* the brief says will hold:
//! <= 16 colours is exact, <= 32 colours is exact, anything more is lossy. The
//! lossy simulation is a stand-in (median cut + ordered dither) to make codec
//! damage visible in the preview; it is not the sender's algorithm. Replace this
//! module with calls into the sender library when that exists.

use crate::color::Rgb;
use crate::dither::Dither;
use crate::frame::{MAX_PALETTE, N, W};
use crate::panel::Panel;

pub const PAYLOAD_BYTES: u32 = 1464;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    /// 4 bits/pixel + 16-entry palette. Exact.
    Index4,
    /// 5 bits/pixel + 32-entry palette. Exact.
    Index5,
    /// More than 32 colours: the sender will have to throw something away.
    Lossy,
}

impl Encoding {
    pub fn for_colours(distinct: usize) -> Self {
        match distinct {
            0..=16 => Encoding::Index4,
            17..=32 => Encoding::Index5,
            _ => Encoding::Lossy,
        }
    }

    /// Estimated pixel payload, from brief section 2.3.
    pub fn bytes(self) -> u32 {
        match self {
            Encoding::Index4 => 1024 + 48,
            Encoding::Index5 => 1280 + 96,
            Encoding::Lossy => PAYLOAD_BYTES,
        }
    }
}

pub fn distinct_colours(rgb: &[u8]) -> usize {
    let mut keys: Vec<u32> = rgb
        .chunks_exact(3)
        .map(|c| (c[0] as u32) << 16 | (c[1] as u32) << 8 | c[2] as u32)
        .collect();
    keys.sort_unstable();
    keys.dedup();
    keys.len()
}

/// Reduce an sRGB frame to `MAX_PALETTE` colours in place, the way an adaptive
/// palette codec might: median cut over the frame's colours, palette snapped to
/// panel levels, pixels remapped through a fixed ordered dither.
pub fn simulate_lossy(rgb: &mut [u8], panel: Panel, dither: Dither) {
    let mut colours: Vec<([u8; 3], u32)> = Vec::new();
    {
        let mut all: Vec<[u8; 3]> = rgb.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect();
        all.sort_unstable();
        for c in all {
            match colours.last_mut() {
                Some((last, n)) if *last == c => *n += 1,
                _ => colours.push((c, 1)),
            }
        }
    }
    if colours.len() <= MAX_PALETTE {
        return;
    }
    // Off LEDs are the house style: true black always keeps its own entry and
    // is never dithered into.
    let has_black = colours.first().is_some_and(|(c, _)| *c == [0, 0, 0]);
    if has_black {
        colours.remove(0);
    }
    let target = MAX_PALETTE - has_black as usize;

    let mut boxes = vec![colours];
    while boxes.len() < target {
        // Split the box with the most (extent x population) along its longest axis.
        let pick = boxes
            .iter()
            .enumerate()
            .filter(|(_, b)| b.len() > 1)
            .max_by_key(|(_, b)| {
                let (_, extent) = longest_axis(b);
                extent as u64 * b.iter().map(|(_, n)| *n as u64).sum::<u64>()
            })
            .map(|(i, _)| i);
        let Some(i) = pick else { break };
        let mut b = boxes.swap_remove(i);
        let (axis, _) = longest_axis(&b);
        b.sort_unstable_by_key(|(c, _)| c[axis]);
        let half = b.iter().map(|(_, n)| *n).sum::<u32>() / 2;
        let mut acc = 0;
        let mut cut = 1;
        for (j, (_, n)) in b.iter().enumerate() {
            acc += n;
            if acc >= half {
                cut = (j + 1).clamp(1, b.len() - 1);
                break;
            }
        }
        let tail = b.split_off(cut);
        boxes.push(b);
        boxes.push(tail);
    }

    let mut palette: Vec<[u8; 3]> = boxes
        .iter()
        .map(|b| {
            let total: f32 = b.iter().map(|(_, n)| *n as f32).sum();
            let mut mean = Rgb::BLACK;
            for (c, n) in b {
                mean = mean.add(Rgb::from_srgb8(*c).scale(*n as f32 / total));
            }
            panel.snap(mean, 0.0).to_srgb8()
        })
        .collect();
    if has_black {
        palette.push([0, 0, 0]);
    }

    // Spread of the dither, in sRGB code values: roughly the spacing between
    // palette entries in a frame that needed all 32.
    const SPREAD: f32 = 28.0;
    for i in 0..N {
        if rgb[i * 3..i * 3 + 3] == [0, 0, 0] {
            continue;
        }
        let t = dither.threshold(i % W, i / W) * SPREAD;
        let p = [0, 1, 2].map(|k| rgb[i * 3 + k] as f32 + t);
        let best = palette
            .iter()
            .min_by(|a, b| dist(&p, a).total_cmp(&dist(&p, b)))
            .expect("palette is not empty");
        rgb[i * 3..i * 3 + 3].copy_from_slice(best);
    }
}

fn longest_axis(b: &[([u8; 3], u32)]) -> (usize, u8) {
    (0..3)
        .map(|k| {
            let lo = b.iter().map(|(c, _)| c[k]).min().unwrap_or(0);
            let hi = b.iter().map(|(c, _)| c[k]).max().unwrap_or(0);
            (k, hi - lo)
        })
        .max_by_key(|(_, extent)| *extent)
        .expect("three axes")
}

fn dist(p: &[f32; 3], c: &[u8; 3]) -> f32 {
    // Green errors are the most visible, blue the least.
    let w = [0.30, 0.59, 0.11];
    (0..3).map(|k| w[k] * (p[k] - c[k] as f32).powi(2)).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lossy_lands_within_the_palette() {
        let mut rgb: Vec<u8> = (0..N).flat_map(|i| [(i % 251) as u8, (i % 83) as u8 * 3, (i / 9) as u8]).collect();
        assert!(distinct_colours(&rgb) > MAX_PALETTE);
        simulate_lossy(&mut rgb, Panel::default(), Dither::Bayer4);
        assert!(distinct_colours(&rgb) <= MAX_PALETTE);
    }

    #[test]
    fn budget_matches_the_brief() {
        assert_eq!(Encoding::for_colours(16).bytes(), 1072);
        assert_eq!(Encoding::for_colours(32).bytes(), 1376);
        assert_eq!(Encoding::for_colours(33), Encoding::Lossy);
    }
}
