//! The choreographer: picks walks and gestures at random over a long run,
//! weighted so nothing repeats soon and every gesture gets used
//! (`crate::variety::Variety`, the same tool the clocks' dances and
//! `skeletons`' actors use), with a held rest between performances - the
//! standing direction across this whole autumn set: "a form of visual
//! poetry... busyness is a thing we are trying to avoid."
//!
//! Stepped on a fixed internal timestep, exactly as `skeletons` is
//! (`mod.rs`'s `STEP`/`CATCHUP`): the choreographer's own decisions
//! (`advance`) happen once a step, and [`Choreographer::snapshot`] is what
//! the render reads, twice a step apart, to interpolate for motion blur
//! (`mod.rs`'s doc explains why that is the right split - the player's
//! contact pinning is real, once-per-step state, not a pure function of
//! time the way `skeletons`' `Actor::pos_at` is).

use super::clip::{self, Clip};
use super::player::{ClipPlayer, Posed};
use crate::rng::Rng;
use crate::variety::Variety;
use std::collections::BTreeMap;

/// Roughly how long a hold between performances lasts, seconds - "held
/// moments, stillness and empty time between performances" (the card).
/// Wide on purpose: the owner's own bar across this set is that some
/// moments read weaker than have it feel busy.
const REST_RANGE: (f32, f32) = (2.5, 7.0);
/// How long a *looping* performance (a walk, `wave`) runs before the
/// choreographer moves it along - a one-shot gesture instead runs exactly
/// its own length.
const LOOP_RANGE: (f32, f32) = (2.0, 5.5);

use super::hand_rig::HAND_H;

#[derive(Clone, Debug)]
enum State {
    Resting { until: f64 },
    Performing { name: String, end_at: f64 },
}

pub struct Choreographer {
    clips: BTreeMap<String, Clip>,
    player: ClipPlayer,
    variety: Variety,
    rng: Rng,
    state: State,
    /// One request queued by [`Choreographer::act`] ("Next").
    skip: bool,
    /// The name of the performance before whichever is current (or
    /// pending) - `self.state` has already moved on to `Resting` by the
    /// time `pick_next` needs to know what to avoid repeating.
    last_performed: Option<String>,
    prev: Option<Posed>,
    curr: Option<Posed>,
}

impl Choreographer {
    #[must_use]
    pub fn new(seed: u64) -> Choreographer {
        Choreographer {
            clips: clip::all_clips(),
            player: ClipPlayer::new(HAND_H, seed),
            variety: Variety::default(),
            rng: Rng::new(seed),
            state: State::Resting { until: 0.0 },
            skip: false,
            last_performed: None,
            prev: None,
            curr: None,
        }
    }

    /// The names a performance may be chosen from - every clip but `rest`
    /// itself, which is the choreographer's own interstitial, not a
    /// performance to weigh against the others.
    fn repertoire(&self) -> Vec<String> {
        self.clips.keys().filter(|n| n.as_str() != "rest").cloned().collect()
    }

    fn pick_next(&mut self) -> String {
        let mut options = self.repertoire();
        // Never the same performance twice running - with a repertoire
        // this small (a handful of clips today, more once the footage
        // lands), `Variety`'s wear-based `SPACING` (30 performances) would
        // exclude everything at once rather than just the last one.
        if options.len() > 1 {
            if let Some(name) = &self.last_performed {
                options.retain(|o| o != name);
            }
        }
        let fresh = self.variety.freshest(&options, || self.rng.f32()).cloned();
        fresh.unwrap_or_else(|| {
            let i = ((self.rng.f32() * options.len() as f32) as usize).min(options.len() - 1);
            options[i].clone()
        })
    }

    /// One fixed step of choreography: decide whether to move on, then
    /// sample the player once and cache it for [`Choreographer::render_pose`]
    /// to interpolate against.
    pub fn advance(&mut self, sim_t: f64, dt: f32) {
        let due = match &self.state {
            State::Resting { until } => sim_t >= *until,
            State::Performing { end_at, .. } => sim_t >= *end_at,
        };
        if due || self.skip {
            self.skip = false;
            match &self.state {
                State::Resting { .. } => {
                    let name = self.pick_next();
                    let tags = vec![name.clone()];
                    self.variety.note(&name, &tags);
                    let end_at = sim_t + f64::from(self.performance_length(&name));
                    self.player.switch_to(&self.clips, &name, sim_t);
                    self.last_performed = Some(name.clone());
                    self.state = State::Performing { name, end_at };
                }
                State::Performing { .. } => {
                    let rest = f64::from(self.rng.range(REST_RANGE.0, REST_RANGE.1));
                    self.player.switch_to(&self.clips, "rest", sim_t);
                    self.state = State::Resting { until: sim_t + rest };
                }
            }
        }
        self.prev = self.curr.take();
        self.curr = self.player.sample(&self.clips, sim_t, dt);
    }

    fn performance_length(&mut self, name: &str) -> f32 {
        let clip = &self.clips[name];
        if clip.loopable {
            self.rng.range(LOOP_RANGE.0, LOOP_RANGE.1)
        } else {
            clip.duration()
        }
    }

    /// The two most recent steps' poses, for the render to interpolate
    /// between at however many moments it wants across the frame's own
    /// interval (see `mod.rs`'s motion blur).
    #[must_use]
    pub fn snapshot(&self) -> (Option<&Posed>, Option<&Posed>) {
        (self.prev.as_ref(), self.curr.as_ref())
    }

    #[must_use]
    pub fn playing(&self) -> crate::patch::Playing {
        let (name, detail) = match &self.state {
            State::Resting { .. } => ("resting".to_string(), "between performances".to_string()),
            State::Performing { name, .. } => {
                let label = if clip::is_placeholder(name) { format!("{name} (placeholder motion)") } else { name.clone() };
                (label, "performing".to_string())
            }
        };
        crate::patch::Playing {
            title: name,
            detail,
            actions: vec![crate::patch::Action { id: "next", label: "Next" }],
            notes: Vec::new(),
        }
    }

    pub fn act(&mut self, action: &str) {
        if action == "next" {
            self.skip = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The card's own "10-minute run does not settle into a loop": every
    /// clip in the repertoire gets performed, and no single one dominates,
    /// over a long simulated run.
    #[test]
    fn a_long_run_uses_every_clip_and_does_not_repeat_too_soon() {
        let mut c = Choreographer::new(7);
        let mut counts: BTreeMap<String, u32> = BTreeMap::new();
        let dt = 1.0 / 30.0;
        let mut t = 0.0_f64;
        let mut entered: Option<String> = None; // the performance most recently *entered*
        let mut sequence: Vec<String> = Vec::new();
        for _ in 0..(600 * 30) {
            // ten minutes
            c.advance(t, dt);
            if let State::Performing { name, .. } = &c.state {
                if entered.as_deref() != Some(name.as_str()) {
                    *counts.entry(name.clone()).or_insert(0) += 1;
                    sequence.push(name.clone());
                    entered = Some(name.clone());
                }
            }
            t += f64::from(dt);
        }
        let repertoire = c.repertoire();
        for name in &repertoire {
            assert!(counts.get(name).copied().unwrap_or(0) > 0, "{name} was never performed in ten minutes");
        }
        for w in sequence.windows(2) {
            assert_ne!(w[0], w[1], "a clip should not follow itself immediately: {sequence:?}");
        }
    }

    #[test]
    fn playing_labels_a_placeholder_honestly() {
        let mut c = Choreographer::new(1);
        // Force straight into a performance.
        c.state = State::Resting { until: -1.0 };
        c.advance(0.0, 1.0 / 30.0);
        let p = c.playing();
        if let State::Performing { name, .. } = &c.state {
            if clip::is_placeholder(name) {
                assert!(p.title.contains("placeholder"), "{}", p.title);
            }
        }
    }

    #[test]
    fn the_next_action_moves_on_immediately() {
        let mut c = Choreographer::new(2);
        c.advance(0.0, 1.0 / 30.0);
        let before = format!("{:?}", c.state);
        c.act("next");
        c.advance(0.01, 1.0 / 30.0);
        let after = format!("{:?}", c.state);
        assert_ne!(before, after, "acting on `next` should change state at once");
    }
}
