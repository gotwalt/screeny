//! A piece turns (time, seed, parameters) into frames. It knows nothing about
//! where frames go.

use crate::frame::Frame;
use std::collections::BTreeMap;

pub struct Ctx<'a> {
    /// Elapsed seconds. Drive all animation from this, never from a frame count:
    /// a dropped frame must cause a skip, not a slowdown.
    pub t: f64,
    /// Seconds since the previous `render`; 0 while paused. For pieces that
    /// integrate state (feedback, simulations).
    pub dt: f64,
    pub params: &'a Params,
}

impl Ctx<'_> {
    pub fn get(&self, id: &str) -> f32 {
        self.params.get(id)
    }
}

/// Pieces may keep state between frames (trails, automata, simulations); the
/// panel never does, so every returned frame must be a complete picture.
pub trait Piece: Send {
    fn render(&mut self, ctx: &Ctx) -> Frame;
}

/// A tunable number. The studio builds a slider from this.
#[derive(Clone, Copy, Debug)]
pub struct ParamSpec {
    pub id: &'static str,
    pub label: &'static str,
    pub min: f32,
    pub max: f32,
    pub step: f32,
    pub default: f32,
}

pub const fn param(id: &'static str, label: &'static str, min: f32, max: f32, step: f32, default: f32) -> ParamSpec {
    ParamSpec { id, label, min, max, step, default }
}

#[derive(Clone, Debug, Default)]
pub struct Params(BTreeMap<&'static str, f32>);

impl Params {
    pub fn defaults(specs: &[ParamSpec]) -> Self {
        Params(specs.iter().map(|s| (s.id, s.default)).collect())
    }

    pub fn get(&self, id: &str) -> f32 {
        self.0.get(id).copied().unwrap_or_else(|| panic!("piece asked for unknown parameter `{id}`"))
    }

    /// Returns false if the piece has no such parameter.
    pub fn set(&mut self, specs: &[ParamSpec], id: &str, value: f32) -> bool {
        match specs.iter().find(|s| s.id == id) {
            Some(s) => {
                self.0.insert(s.id, value.clamp(s.min, s.max));
                true
            }
            None => false,
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (&'static str, f32)> + '_ {
        self.0.iter().map(|(k, v)| (*k, *v))
    }
}

pub struct PieceDef {
    pub id: &'static str,
    pub name: &'static str,
    /// One line on what it is and which panel strength it leans on.
    pub blurb: &'static str,
    pub params: &'static [ParamSpec],
    pub make: fn(seed: u64) -> Box<dyn Piece>,
}

pub fn find(id: &str) -> Option<&'static PieceDef> {
    crate::pieces::ALL.iter().find(|d| d.id == id)
}
