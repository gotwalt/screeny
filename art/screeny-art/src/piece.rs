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
    /// The time of day: seconds since the epoch, shifted into the local time
    /// zone (see [`local_now`]). Pieces that tell the time read this rather
    /// than the system clock, so a run can be simulated faster than real time.
    pub now: f64,
    pub params: &'a Params,
}

impl Ctx<'_> {
    pub fn get(&self, id: &str) -> f32 {
        self.params.get(id)
    }
}

/// The real time of day, for [`Ctx::now`].
pub fn local_now() -> f64 {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
    #[cfg(unix)]
    {
        let secs = now as libc::time_t;
        // SAFETY: `tm` is plain data and localtime_r writes all of it.
        let offset = unsafe {
            let mut tm: libc::tm = std::mem::zeroed();
            libc::localtime_r(&secs, &mut tm);
            tm.tm_gmtoff
        };
        now + offset as f64
    }
    #[cfg(not(unix))]
    now
}

/// Pieces may keep state between frames (trails, automata, simulations); the
/// panel never does, so every returned frame must be a complete picture.
pub trait Piece: Send {
    fn render(&mut self, ctx: &Ctx) -> Frame;

    /// For pieces that compose as they go: what is being performed right now,
    /// and what the person watching can do about it. The studio shows this as
    /// its "Now playing" panel.
    fn playing(&self) -> Option<Playing> {
        None
    }

    /// One of the actions offered by [`Piece::playing`] was chosen.
    fn act(&mut self, _action: &str) {}
}

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct Playing {
    /// What it is, in the piece's own words: "needles > open, from a point".
    pub title: String,
    /// What is happening to it: "dancing, 6 s to go".
    pub detail: String,
    pub actions: Vec<Action>,
    /// Anything worth telling: usually what has been learned from ratings.
    pub notes: Vec<String>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct Action {
    pub id: &'static str,
    pub label: &'static str,
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
