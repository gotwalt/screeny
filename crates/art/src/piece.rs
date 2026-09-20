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
    /// Anything else worth a line.
    pub notes: Vec<String>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct Action {
    pub id: &'static str,
    pub label: &'static str,
}

/// A tunable number. The studio builds a slider from this.
///
/// Some parameters are not really numbers, though: their values are a list of
/// named things, and before card 163 the names lived in the label -
/// `"Resting dials (0: as it was, 1: quiet, 2: hatched quiet, ...)"` - so the
/// person moving the slider was reading a legend and counting stops. A spec
/// can now **declare** those names ([`choice`]) or say that it is a plain
/// switch ([`toggle`]), and the studio draws a list or a switch instead.
///
/// The value stays an `f32` throughout: on the wire, in the state file and in
/// the per-piece memory (card 165). Nothing downstream of a piece knows the
/// difference, and no piece's ids, ranges or defaults changed.
#[derive(Clone, Copy, Debug)]
pub struct ParamSpec {
    pub id: &'static str,
    pub label: &'static str,
    pub min: f32,
    pub max: f32,
    pub step: f32,
    pub default: f32,
    /// The name of each stop, `choices[v as usize]`, for a parameter whose
    /// values are a list. Empty for an ordinary number.
    pub choices: &'static [&'static str],
    /// This is off or on, not a range of one. Drawn as a switch.
    pub switch: bool,
}

pub const fn param(id: &'static str, label: &'static str, min: f32, max: f32, step: f32, default: f32) -> ParamSpec {
    ParamSpec { id, label, min, max, step, default, choices: &[], switch: false }
}

/// A parameter whose values are a list of named stops: `0..=choices.len()-1`,
/// step 1, and the label goes back to being a label.
///
/// The names are the piece's own words - `RESTS[i].name`, a mood's `name` -
/// so the page shows what the piece would call the thing, not an index.
///
/// # Panics
///
/// At compile time, if `choices` is empty: a choice of nothing is a mistake,
/// not a parameter.
pub const fn choice(id: &'static str, label: &'static str, choices: &'static [&'static str], default: f32) -> ParamSpec {
    assert!(!choices.is_empty(), "a choice parameter needs at least one named stop");
    ParamSpec { id, label, min: 0.0, max: (choices.len() - 1) as f32, step: 1.0, default, choices, switch: false }
}

/// A parameter that is off or on: `0.0` or `1.0`, drawn as a switch.
pub const fn toggle(id: &'static str, label: &'static str, default: bool) -> ParamSpec {
    ParamSpec {
        id,
        label,
        min: 0.0,
        max: 1.0,
        step: 1.0,
        default: if default { 1.0 } else { 0.0 },
        choices: &[],
        switch: true,
    }
}

impl ParamSpec {
    /// A value fit to hand to a piece.
    ///
    /// Out of range is clamped, which is what the sliders do anyway. A value
    /// that is not a number at all becomes the default: `f32::clamp` returns a
    /// NaN unchanged, so without this a remembered or hand-edited NaN would
    /// reach a piece's arithmetic and paint a black frame for ever.
    #[must_use]
    pub fn sanitise(&self, value: f32) -> f32 {
        if value.is_finite() {
            value.clamp(self.min, self.max)
        } else {
            self.default
        }
    }

    /// What this value is called, for a parameter with named stops. `None`
    /// for an ordinary number, and for a value with no stop of its own.
    #[must_use]
    pub fn name_of(&self, value: f32) -> Option<&'static str> {
        if self.choices.is_empty() || !value.is_finite() {
            return None;
        }
        self.choices.get(self.sanitise(value).round() as usize).copied()
    }
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
    ///
    /// A value the spec does not allow is corrected rather than refused - see
    /// [`ParamSpec::sanitise`] - so the answer is only ever about the *id*.
    pub fn set(&mut self, specs: &[ParamSpec], id: &str, value: f32) -> bool {
        match specs.iter().find(|s| s.id == id) {
            Some(s) => {
                self.0.insert(s.id, s.sanitise(value));
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

#[cfg(test)]
mod tests {
    use super::*;

    const SPECS: &[ParamSpec] = &[param("scale", "Scale", 0.5, 4.0, 0.1, 1.0)];

    /// Card 165: a remembered or hand-edited value reaches a piece through
    /// here, so "no piece ever sees a value its own spec forbids" has to be
    /// true of every path into `Params`, not just of the sliders.
    #[test]
    fn a_spec_corrects_a_value_it_cannot_allow() {
        let s = SPECS[0];
        assert_eq!(s.sanitise(2.0), 2.0);
        assert_eq!(s.sanitise(9.0), 4.0, "above the range is clamped");
        assert_eq!(s.sanitise(-9.0), 0.5, "below the range is clamped");
        assert_eq!(s.sanitise(f32::NAN), 1.0, "not a number at all is the default");
        assert_eq!(s.sanitise(f32::INFINITY), 1.0);
        assert_eq!(s.sanitise(f32::NEG_INFINITY), 1.0);
    }

    #[test]
    fn setting_a_parameter_never_leaves_a_piece_with_a_nan() {
        let mut p = Params::defaults(SPECS);
        assert!(p.set(SPECS, "scale", f32::NAN), "a NaN is a value problem, not an unknown id");
        assert_eq!(p.get("scale"), 1.0);
        assert!(p.set(SPECS, "scale", 100.0));
        assert_eq!(p.get("scale"), 4.0);
        assert!(!p.set(SPECS, "nonesuch", 1.0), "an unknown id is still refused");
    }

    // ---------------- card 163: parameters that are lists ----------------

    /// A choice is an ordinary `f32` parameter that happens to have names: the
    /// range, the step and `sanitise` all behave exactly as before, so nothing
    /// downstream of a piece - the wire, the state file, the per-piece memory
    /// (card 165) - knows the difference.
    #[test]
    fn a_choice_is_still_a_number() {
        const C: ParamSpec = choice("mood", "Mood", &["wander", "drift", "sway"], 0.0);
        assert_eq!((C.min, C.max, C.step, C.default), (0.0, 2.0, 1.0, 0.0));
        assert_eq!(C.sanitise(9.0), 2.0);
        assert_eq!(C.sanitise(f32::NAN), 0.0);
        assert_eq!(C.name_of(1.0), Some("drift"));
        assert_eq!(C.name_of(9.0), Some("sway"), "a value out of range is the stop it clamps to");
        assert_eq!(C.name_of(f32::NAN), None);

        const T: ParamSpec = toggle("hours24", "24-hour", true);
        assert_eq!((T.min, T.max, T.step, T.default), (0.0, 1.0, 1.0, 1.0));
        assert!(T.switch && T.choices.is_empty());
        assert_eq!(SPECS[0].name_of(1.0), None, "an ordinary number has no names");
    }

    /// The names on a choice are the **piece's own** names, not a second copy
    /// that can drift. Since card 182 there is no second copy left: `RESTS`,
    /// `dance::NAMES` and `ambient::MOOD_NAMES` are each read directly by the
    /// `PARAMS` block that declares the stops.
    ///
    /// What is left to check is the part that is still written by hand and
    /// still could be wrong: the **offsets** - that `dance` is "vary", then the
    /// repertoire, then "composed", and `mood` is "wander" and then the moods -
    /// and that the ranges a piece shipped have not moved under them. So this
    /// builds every dance and every mood and asks each one its name, exactly as
    /// it did when the lists were duplicated.
    #[test]
    fn the_named_stops_are_the_pieces_own_names() {
        use crate::pieces::clocks::{ambient, dance, dials, DANCE_CHOICES, RESTS};

        let by_id = |def: &'static PieceDef, id: &str| *def.params.iter().find(|p| p.id == id).expect(id);

        // rest: five treatments, named as the piece names them.
        let rest = by_id(&crate::pieces::clocks::DEF, "rest");
        assert_eq!(rest.choices.len(), RESTS.len());
        for (i, treatment) in RESTS.iter().enumerate() {
            assert_eq!(rest.choices[i], treatment.name);
            assert_eq!(rest.name_of(i as f32), Some(treatment.name));
        }

        // dance: "vary", then the repertoire in order, then "composed".
        let d = by_id(&crate::pieces::clocks::DEF, "dance");
        assert_eq!(d.choices.len(), dance::DANCES + 2, "vary, every dance, and composed");
        assert_eq!(d.max, 13.0, "the range card 160 shipped");
        for which in 0..dance::DANCES {
            let (name, _) = dance::dance(which, &mut crate::rng::Rng::new(which as u64));
            assert_eq!(DANCE_CHOICES[which + 1], name, "dance {which}");
        }
        assert_eq!(*DANCE_CHOICES.first().expect("vary"), "vary");
        assert_eq!(*DANCE_CHOICES.last().expect("composed"), "composed");

        // mood: "wander", then the eight moods in `Mood::new` order.
        let mood = by_id(&crate::pieces::clocks::dials::DEF, "mood");
        assert_eq!(mood.choices.len(), ambient::MOODS + 1);
        assert_eq!(mood.max, ambient::MOODS as f32, "the range the piece shipped");
        for which in 0..ambient::MOODS {
            let named = ambient::Mood::new(which, &mut crate::rng::Rng::new(which as u64)).name;
            assert_eq!(dials::MOOD_CHOICES[which + 1], named, "mood {which}");
        }

        // grid: the three grids, written the way a person says them.
        let grid = by_id(&crate::pieces::clocks::dials::DEF, "grid");
        assert_eq!(grid.choices, ["4 x 2", "6 x 3", "8 x 4"]);
        assert_eq!((grid.min, grid.max, grid.default), (0.0, 2.0, 1.0), "the range and default are card 100's");
    }

    /// Nothing card 163 touched changed a piece's behaviour: every id, range,
    /// step and default is what it was.
    #[test]
    fn no_pieces_ids_ranges_or_defaults_moved() {
        let want: &[(&str, &str, f32, f32, f32, f32)] = &[
            ("clocks-numerals", "dance", 0.0, 13.0, 1.0, 0.0),
            ("clocks-numerals", "rest", 0.0, 4.0, 1.0, 2.0),
            ("clocks-numerals", "hours24", 0.0, 1.0, 1.0, 1.0),
            ("clocks-dials", "grid", 0.0, 2.0, 1.0, 1.0),
            ("clocks-dials", "mood", 0.0, 8.0, 1.0, 0.0),
        ];
        for (piece, id, min, max, step, default) in want {
            let def = find(piece).unwrap_or_else(|| panic!("{piece}"));
            let spec = def.params.iter().find(|p| p.id == *id).unwrap_or_else(|| panic!("{piece}.{id}"));
            assert_eq!((spec.min, spec.max, spec.step, spec.default), (*min, *max, *step, *default), "{piece}.{id}");
            assert!(!spec.label.contains('('), "{piece}.{id}: the label is a label again, not a legend");
        }
    }
}
