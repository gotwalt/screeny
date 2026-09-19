//! Not repeating yourself. A composer that runs all day is judged less by its
//! best moment than by whether the tenth hour looks like the first.
//!
//! Everything performed is noted by name and by tags. Tags *wear*: each use
//! adds to a tag's wear and all wear fades with every performance, so a tag's
//! wear is roughly how much of the recent past it has filled. Candidates made
//! of worn tags score lower, which spreads use across the whole vocabulary
//! instead of favouring whatever the dice like. Names are remembered exactly,
//! so the same shape cannot come round again too soon.

use std::collections::{BTreeMap, VecDeque};

/// Wear fades by this factor per performance: a memory of about thirty.
const FADE: f32 = 0.967;

#[derive(Clone, Debug, Default)]
pub struct Variety {
    wear: BTreeMap<String, f32>,
    names: VecDeque<String>,
}

impl Variety {
    /// How many performances must pass before a name may be used again.
    pub const SPACING: usize = 30;

    pub fn note(&mut self, name: &str, tags: &[String]) {
        for w in self.wear.values_mut() {
            *w *= FADE;
        }
        for tag in tags {
            *self.wear.entry(tag.clone()).or_insert(0.0) += 1.0 - FADE;
        }
        self.wear.retain(|_, w| *w > 1e-4);
        self.names.push_back(name.to_string());
        while self.names.len() > Self::SPACING {
            self.names.pop_front();
        }
    }

    pub fn too_soon(&self, name: &str) -> bool {
        self.names.iter().any(|n| n == name)
    }

    /// 0 for tags never used lately, towards 1 for tags used every time.
    pub fn staleness(&self, tags: &[String]) -> f32 {
        if tags.is_empty() {
            return 0.0;
        }
        tags.iter().map(|t| self.wear.get(t).copied().unwrap_or(0.0)).sum::<f32>() / tags.len() as f32
    }

    /// The least worn of `options`, with `jitter` (0..1 per option) to break
    /// ties and keep it from being a fixed rotation.
    pub fn freshest<'a>(&self, options: &'a [String], mut jitter: impl FnMut() -> f32) -> Option<&'a String> {
        options
            .iter()
            .map(|o| (self.wear.get(o).copied().unwrap_or(0.0) + 0.08 * jitter(), o))
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, o)| o)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wear_tracks_recent_use() {
        let mut v = Variety::default();
        let (a, b) = (vec!["a".to_string()], vec!["b".to_string()]);
        for i in 0..200 {
            v.note(&format!("n{i}"), if i % 4 == 0 { &b } else { &a });
        }
        assert!((v.staleness(&a) - 0.75).abs() < 0.08, "{}", v.staleness(&a));
        assert!((v.staleness(&b) - 0.25).abs() < 0.08, "{}", v.staleness(&b));
        assert!(v.too_soon("n199") && !v.too_soon("n100"));
    }
}
