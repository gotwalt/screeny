//! What the person in front of the studio likes, learned from thumbs up and
//! down, so a composer can lean towards it.
//!
//! Anything rateable describes itself with *tags* ("motif:rings", "op:weave",
//! "theme:point"). A verdict nudges the weight of every tag it carried; a
//! candidate's score is the sum of its tags' weights. That is deliberately
//! simple: it is legible (the studio shows the weights), it generalises from a
//! few ratings, and it cannot do anything surprising.
//!
//! Weights live in `$SCREENY_ART_HOME` or `~/.screeny-art/<name>.taste`, one
//! `tag<TAB>weight` per line, so the headless runner can share the studio's
//! taste by copying a file.

use std::collections::BTreeMap;
use std::path::PathBuf;

/// No tag counts for more than this, however often it is rated.
const LIMIT: f32 = 3.0;

#[derive(Clone, Debug, Default)]
pub struct Taste {
    path: Option<PathBuf>,
    weights: BTreeMap<String, f32>,
}

impl Taste {
    /// Kept in memory only: for tests, and for runs that should not learn.
    pub fn in_memory() -> Taste {
        Taste::default()
    }

    /// Load `<name>.taste`, or start empty if there is none yet.
    pub fn load(name: &str) -> Taste {
        let dir = std::env::var_os("SCREENY_ART_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".screeny-art")));
        let Some(path) = dir.map(|d| d.join(format!("{name}.taste"))) else { return Taste::default() };
        let weights = std::fs::read_to_string(&path)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.split_once('\t'))
            .filter_map(|(tag, w)| Some((tag.to_string(), w.trim().parse().ok()?)))
            .collect();
        Taste { path: Some(path), weights }
    }

    /// `verdict` is +1 for more like this, -1 for less.
    pub fn rate(&mut self, tags: &[String], verdict: f32) {
        for tag in tags {
            let w = self.weights.entry(tag.clone()).or_insert(0.0);
            *w = (*w + verdict).clamp(-LIMIT, LIMIT);
        }
        self.weights.retain(|_, w| w.abs() > 1e-3);
        self.save();
    }

    pub fn forget(&mut self) {
        self.weights.clear();
        self.save();
    }

    /// Mean weight of `tags`: comparable between things with few tags and many.
    pub fn score(&self, tags: &[String]) -> f32 {
        if tags.is_empty() {
            return 0.0;
        }
        tags.iter().map(|t| self.weights.get(t).copied().unwrap_or(0.0)).sum::<f32>() / tags.len() as f32
    }

    pub fn weight(&self, tag: &str) -> f32 {
        self.weights.get(tag).copied().unwrap_or(0.0)
    }

    /// Strongest opinions first.
    pub fn opinions(&self) -> Vec<(String, f32)> {
        let mut all: Vec<_> = self.weights.iter().map(|(t, w)| (t.clone(), *w)).collect();
        all.sort_by(|a, b| b.1.abs().total_cmp(&a.1.abs()).then(a.0.cmp(&b.0)));
        all
    }

    fn save(&self) {
        let Some(path) = &self.path else { return };
        let text: String = self.weights.iter().map(|(t, w)| format!("{t}\t{w}\n")).collect();
        let written = path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| std::fs::write(path, text));
        if let Err(e) = written {
            eprintln!("screeny-art: could not save {}: {e}", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ratings_accumulate_and_saturate() {
        let mut t = Taste::in_memory();
        let rings = vec!["motif:rings".to_string(), "theme:point".to_string()];
        for _ in 0..10 {
            t.rate(&rings, 1.0);
        }
        t.rate(&["theme:point".to_string()], -1.0);
        assert_eq!(t.weight("motif:rings"), LIMIT);
        assert_eq!(t.weight("theme:point"), LIMIT - 1.0);
        assert!(t.score(&rings) > t.score(&["motif:fan".to_string()]));
        assert_eq!(t.opinions()[0].0, "motif:rings");
    }
}
