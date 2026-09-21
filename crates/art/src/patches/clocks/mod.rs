//! Twenty-four clocks: kinetic choreography, after Humans since 1982's
//! ClockClock 24. A 3 x 8 grid of two-handed analog clocks whose hands line up
//! to draw the digits of the time, and dance their way to the next minute.
//!
//! It fits this panel exactly: 8 clocks x 8 LEDs wide, 3 x 8 tall, with black
//! margins above and below. Hands reach their cell's edge, so neighbouring
//! clocks' strokes join into continuous digit lines, which the physical clock
//! cannot do and which is what makes a 16 x 24 LED digit legible.
//!
//! There is no physics and no tweening. The real hands are on stepper motors,
//! and what makes them look mechanical is a motor's constraint: every hand has
//! the same top speed and the same acceleration, so a hand with further to go
//! simply takes longer. Each choreography is a plan of such moves, timed to land
//! on the new time exactly as the minute turns.

pub(crate) mod ambient;
pub(crate) mod dance;
pub(crate) mod dials;
pub(crate) mod draw;

use crate::color::Rgb;
use crate::frame::Frame;
use crate::patch::{choice, param, toggle, Action, Ctx, ParamSpec, Patch, PatchDef, Playing};
use crate::rng::Rng;
use dance::Motor;

pub const DEF: PatchDef = PatchDef {
    id: "clocks-numerals",
    name: "Clocks: numerals",
    blurb: "After ClockClock 24: 24 small dials whose hands draw the time in digits, and dance to the next minute. For a clock you read from across the room.",
    params: PARAMS,
    make,
    // The seed only shifts which choreography a given minute is danced to
    // (`Rng::new(self.seed ^ minute * K ^ asked * K)`); the picture is the
    // time, and it is the same time. "Play it again" and "Compose another" are
    // this patch's own words for what a person means, and they act at once
    // without restarting the clock.
    seeded: false,
};

const PARAMS: &[ParamSpec] = &[
    param("pace", "Seconds per minute (60 = real clock)", 5.0, 60.0, 1.0, 60.0),
    param("still", "Seconds the time is held", 3.0, 60.0, 1.0, 15.0),
    choice("dance", "Choreography", DANCE_CHOICES, 0.0),
    choice("rest", "Resting dials", REST_CHOICES, DEFAULT_REST as f32),
    choice("dark", "Dark ramp", DARK_CHOICES, DEFAULT_DARK as f32),
    param("speed", "Hand speed (deg/s)", 30.0, 360.0, 1.0, 100.0),
    toggle("hours24", "24-hour", true),
    param("offset", "Time offset (minutes)", 0.0, 1439.0, 1.0, 0.0),
    param("weight", "Hand weight (LEDs)", 1.0, 2.4, 0.05, 2.0),
    param("tip", "Hand tip while dancing (1 = blunt)", 0.2, 1.0, 0.05, 0.45),
    param("dials", "Dial rings", 0.0, 1.0, 0.01, 0.0),
    param("hue", "Hour hand hue", 0.0, 360.0, 1.0, 80.0),
    param("chroma", "Hour hand colour", 0.0, 0.2, 0.005, 0.05),
    param("hue2", "Minute hand hue", 0.0, 360.0, 1.0, 80.0),
    param("chroma2", "Minute hand colour", 0.0, 0.2, 0.005, 0.05),
    param("light", "Hand lightness (lower = more saturated)", 0.6, 0.95, 0.01, 0.93),
];

pub(crate) const COLS: usize = 8;
pub(crate) const ROWS: usize = 3;
pub(crate) const CLOCKS: usize = COLS * ROWS;
/// LEDs per clock; the grid is 64 x 24, centred in 64 x 32.
const CELL: f32 = 8.0;

/// Both hands at 7:30: how the original's clocks rest, and how this patch
/// rested until card 160. Also where every hand starts before the first dance.
const REST: f32 = 225.0;
/// Degrees clockwise from 12 o'clock.
pub(crate) type Hands = [f32; 2];

/// How the dials that are not part of a digit are posed and drawn.
///
/// They must stay clock hands - visible, in an unobtrusive rest position (the
/// owner, 2026-09-19) - without reading as a glyph. Three of them stacked in a
/// column is the hard case: that is what the digit `1` leaves beside its bar,
/// and at 8 LEDs a repeated short stroke there is punctuation. `1` is also the
/// commonest digit on a clock face, so the wrong rest pose is on the panel more
/// often than not.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct Rest {
    /// What it is called, for the studio's "now playing".
    pub name: &'static str,
    /// Hour and minute hand, degrees clockwise from 12 o'clock.
    hands: Hands,
    /// Odd rows mirror the pose about the vertical, so the three resting dials
    /// beside a `1` never repeat one mark down a column.
    zigzag: bool,
    /// Ink, and hand length as a fraction of a digit hand's, while the time is
    /// being held. Full (`1.0`) during the dances: the rest pose only has to be
    /// unobtrusive when there is a time to read.
    ink: f32,
    reach: f32,
}

const fn rest(name: &'static str, hands: Hands, zigzag: bool, ink: f32, reach: f32) -> Rest {
    Rest { name, hands, zigzag, ink, reach }
}

/// The minute hand's rest angle in the treatments that draw a diagonal: 1:30,
/// opposite the hour hand's 7:30. The dial reads about 1:37 and its two hands
/// make one stroke corner to corner across the cell.
///
/// A diagonal is the one direction the digits do not use - every digit stroke
/// is up, down, left or right - so a resting dial cannot be mistaken for part
/// of one, and it survives being looked at from across the room: three blurred
/// dots in a column are a colon, three blurred slashes are a texture.
const HATCH: f32 = 45.0;

/// The treatments, switchable live (the `rest` parameter) so they can be
/// judged on the panel rather than in a preview. `0` is what the patch did
/// before card 160; `DEFAULT_REST` is the recommendation.
pub(crate) const RESTS: &[Rest] = &[
    rest("as it was", [REST, REST], false, 1.0, 1.0),
    rest("quiet", [REST, REST], false, 0.20, 1.0),
    rest("hatched, quiet", [REST, HATCH], false, 0.20, 1.0),
    rest("hatched, faint", [REST, HATCH], false, 0.10, 0.9),
    rest("zigzag, quiet", [REST, REST], true, 0.20, 1.0),
];
pub(crate) const DEFAULT_REST: usize = 2;

/// Card 163: the `rest` parameter's stops, in the treatments' own words.
/// Taken from `RESTS` itself, so the page and the patch cannot disagree about
/// what a treatment is called.
pub(crate) const REST_CHOICES: &[&str] = &[RESTS[0].name, RESTS[1].name, RESTS[2].name, RESTS[3].name, RESTS[4].name];

/// And the `dance` parameter's: `0` varies, `1..=DANCES` name a dance from
/// `dance::dance`, and one past them is "always composed".
///
/// Card 182: the middle **is** `dance::NAMES`, so a new dance is one name in
/// one place. What is still written by hand is the two ends and the offset,
/// and `patch::tests::the_named_stops_are_the_patches_own_names` guards those.
pub(crate) const DANCE_CHOICES: &[&str] = &dance_choices();

const fn dance_choices() -> [&'static str; dance::DANCES + 2] {
    let mut out = [""; dance::DANCES + 2];
    out[0] = "vary";
    let mut i = 0;
    while i < dance::DANCES {
        out[i + 1] = dance::NAMES[i];
        i += 1;
    }
    out[dance::DANCES + 1] = "composed";
    out
}

/// A resting dial's held, dark ink (card 188, brief 2.1.1). Index 0, **the
/// default**, is `"as it was"`: the old continuous `tint.scale(ink)`,
/// untouched - the review after this card's first cut found the default
/// picture had moved (the warm, graded rest shade the owner had already
/// tuned became a much dimmer, nearly flat one), which the card's own rule
/// forbids. Every other entry is a short, hand-picked list of level triples,
/// chosen **nearest the levels the old continuous colour actually measures
/// at** (see the Log for the numbers) rather than computed or guessed - the
/// same look, steadied, not a different one. `screeny_art::panel::level_triple`
/// (card 188 deliverable 2) is the snap that turns a triple into the exact
/// colour the panel shows.
pub(crate) const DARK_CHOICES: &[&str] = &["as it was", "aligned warm", "aligned neutral", "aligned dim"];
pub(crate) const DEFAULT_DARK: usize = 0;

/// One ramp per [`DARK_CHOICES`] entry *after* `"as it was"` - `DARK_RAMPS[0]`
/// is `DARK_CHOICES[1]`, and so on. Each is darkest-first (ink ~0.10,
/// `"hatched, faint"`, then ink ~0.20, the other three treatments), so
/// [`dark_shade`] can pick further in as a treatment's `ink` asks for more
/// light.
pub(crate) const DARK_RAMPS: &[&[[u32; 3]]] = &[
    // "aligned warm": the nearest aligned triple to what the old continuous
    // ink actually measured (card 188 log, 2026-09-21 - "measured, after the
    // owner's review"): ink 0.10 -> levels (6, 5, 3); ink 0.20 -> (12, 10, 7).
    // Keeps the hour hand's own warm hue as far as per-channel alignment
    // allows - the graded look the owner had tuned, steadied rather than
    // replaced.
    &[[6, 5, 3], [12, 10, 7]],
    // "aligned neutral": the same two rungs with the channels equalised
    // (their rounded average) - no hue, same overall brightness.
    &[[5, 5, 5], [10, 10, 10]],
    // "aligned dim": this card's first cut, kept as a fourth, deliberately
    // much darker choice now that it is not the default.
    &[[1, 1, 1], [2, 2, 2], [3, 3, 3]],
];

/// The `dark` parameter's pick. `variant == 0` (`"as it was"`, the default)
/// is `None` - the caller keeps the old continuous `tint.scale(ink)` - and
/// every other variant indexes [`DARK_RAMPS`], further in as `ink` calls for
/// more light: a treatment's `ink` is small (0.10 or 0.20 today), so this
/// mostly picks the ramp's darkest step, but stays proportional if a future
/// treatment asks for more.
fn dark_shade(variant: usize, ink: f32) -> Option<Rgb> {
    if variant == 0 {
        return None;
    }
    let ramp = DARK_RAMPS[(variant - 1).min(DARK_RAMPS.len() - 1)];
    let idx = ((ink * ramp.len() as f32).ceil() as usize).clamp(1, ramp.len()) - 1;
    Some(crate::panel::level_triple(ramp[idx]))
}

impl Rest {
    /// The treatment a `rest` parameter value asks for.
    pub(crate) fn of(v: f32) -> Rest {
        RESTS[(v.max(0.0) as usize).min(RESTS.len() - 1)]
    }

    /// The pose for a resting dial in `row`.
    fn hands(&self, row: usize) -> Hands {
        match self.zigzag && row % 2 == 1 {
            true => self.hands.map(|a| (360.0 - a).rem_euclid(360.0)),
            false => self.hands,
        }
    }

    /// `[ink, length]` for `draw::Dials`, `settled` of the way from a dancing
    /// dial (full) to a resting one.
    fn scale(&self, settled: f32) -> [f32; 2] {
        let to = |x: f32| 1.0 + (x - 1.0) * settled.clamp(0.0, 1.0);
        [to(self.ink), to(self.reach)]
    }
}

/// Each digit is 2 clocks wide and 3 tall, read in rows. U R D L are hand
/// directions and N is the rest pose. Where a junction would need three hands,
/// something has to give. 0, 2, 5, 6 and 9 were checked against footage of
/// the original (its 9 keeps the vertical and lets the bar fall short); the
/// rest follow manu.ninja's table, drawn from the studio's promotional films,
/// including the "cyclops" 8: a closed box over a cup.
const DIGITS: [[&str; 6]; 10] = [
    ["RD", "LD", "UD", "UD", "UR", "UL"],
    ["NN", "DD", "NN", "UD", "NN", "UU"],
    ["RR", "LD", "RD", "LU", "UR", "LL"],
    ["RR", "LD", "RR", "LU", "RR", "LU"],
    ["DD", "DD", "UR", "UD", "NN", "UU"],
    ["RD", "LL", "UR", "LD", "RR", "LU"],
    ["RD", "LL", "UD", "LD", "UR", "UL"],
    ["RR", "LD", "NN", "UD", "NN", "UU"],
    ["RD", "LD", "UR", "UL", "UR", "UL"],
    ["RD", "LD", "UR", "UD", "RR", "LU"],
];

fn direction(c: u8) -> f32 {
    match c {
        b'U' => 0.0,
        b'R' => 90.0,
        b'D' => 180.0,
        b'L' => 270.0,
        _ => REST,
    }
}

/// Hand angles for all 24 clocks showing `hh:mm`. Four digits, leading zeros,
/// no separator: `09:05` is `0905`. That is the original's format and the
/// owner's decision (card 160); a colon is not this patch's to add.
pub(crate) fn pose(hh: u32, mm: u32, rest: Rest) -> [Hands; CLOCKS] {
    let mut out = [[REST; 2]; CLOCKS];
    for (d, digit) in [hh / 10, hh % 10, mm / 10, mm % 10].into_iter().enumerate() {
        for (k, cell) in DIGITS[digit as usize % 10].iter().enumerate() {
            let (row, col) = (k / 2, d * 2 + k % 2);
            let (b, at_rest) = (cell.as_bytes(), rest.hands(row));
            out[row * COLS + col] = [0, 1].map(|h| match b[h] {
                b'N' => at_rest[h],
                c => direction(c),
            });
        }
    }
    out
}

/// Which of the 24 dials are not part of a digit at `hh:mm`, and so are drawn
/// as being at rest.
pub(crate) fn resting(hh: u32, mm: u32) -> [bool; CLOCKS] {
    let mut out = [false; CLOCKS];
    for (d, digit) in [hh / 10, hh % 10, mm / 10, mm % 10].into_iter().enumerate() {
        for (k, cell) in DIGITS[digit as usize % 10].iter().enumerate() {
            let (row, col) = (k / 2, d * 2 + k % 2);
            out[row * COLS + col] = cell.bytes().all(|b| b == b'N');
        }
    }
    out
}

/// Every hand's moves for one transition.
struct Plan {
    began: f64,
    from: [Hands; CLOCKS],
    to: [Hands; CLOCKS],
    moves: dance::Moves,
    total: f32,
    minute: i64,
}

/// Something the person at the studio asked for, to do as soon as possible.
#[derive(Clone, Copy, PartialEq)]
enum Request {
    /// Perform the last dance again.
    Again,
    /// Compose a new dance and perform it now.
    Another,
}

struct Clocks {
    seed: u64,
    /// The time of day on the first frame; a sped-up clock (pace < 60) runs on
    /// from here.
    born: Option<f64>,
    angles: [Hands; CLOCKS],
    /// The minute the hands currently show, or are moving towards.
    minute: Option<i64>,
    /// How far the picture has settled onto the time, 0 while the hands are
    /// dancing or drifting and 1 while it is being held. Resting dials recede
    /// by this much, so a dance is at full strength throughout and the digits
    /// are left alone on the panel once it lands.
    settled: f32,
    plan: Option<Plan>,
    /// Engine time at which the hands last landed on a time.
    landed: f64,
    /// Ambient motion between holding the time and the next dance.
    ambient: Option<ambient::Ambient>,
    /// The dance chosen for a minute, kept so it is composed only once.
    chosen: Option<(i64, usize, dance::Composition)>,
    /// The dance being performed, or the last one that was.
    performed: Option<dance::Composition>,
    /// What has been performed, so the composer does not repeat itself.
    variety: crate::variety::Variety,
    request: Option<Request>,
    /// How many dances have been asked for by hand, to vary them.
    asked: u64,
    /// What the hands are doing, for the studio.
    doing: String,
    /// The rest treatment in force, for the studio: it is a parameter, and
    /// `playing` does not see parameters.
    treatment: &'static str,
}

fn make(seed: u64) -> Box<dyn Patch> {
    Box::new(Clocks {
        seed,
        born: None,
        angles: [[REST; 2]; CLOCKS],
        minute: None,
        settled: 0.0,
        plan: None,
        landed: 0.0,
        ambient: None,
        chosen: None,
        performed: None,
        variety: Default::default(),
        request: None,
        asked: 0,
        doing: String::new(),
        treatment: RESTS[DEFAULT_REST].name,
    })
}

impl Clocks {
    /// The hours and minutes a minute-of-day shows, in the patch's format.
    fn digits(minute: i64, hours24: bool) -> (u32, u32) {
        let (h, m) = ((minute.div_euclid(60).rem_euclid(24)) as u32, minute.rem_euclid(60) as u32);
        (if hours24 { h } else { (h + 11) % 12 + 1 }, m)
    }

    fn target(minute: i64, hours24: bool, rest: Rest) -> [Hands; CLOCKS] {
        let (h, m) = Self::digits(minute, hours24);
        pose(h, m, rest)
    }

    /// The dance for the move to `minute`. Asked for by number it comes from
    /// the repertoire (or, at 13, is always composed). Left to vary, most are
    /// composed afresh and the rest drawn from the repertoire.
    fn dance_for(&mut self, minute: i64, choice: usize, to: &[Hands; CLOCKS], motor: Motor) -> dance::Composition {
        if let Some((m, c, composition)) = &self.chosen {
            if (*m, *c) == (minute, choice) {
                return composition.clone();
            }
        }
        let mut rng = Rng::new(self.seed ^ (minute as u64).wrapping_mul(0x9e37_79b9) ^ self.asked.wrapping_mul(0x85eb_ca6b));
        let composed = match choice {
            0 => rng.u64() % 10 < 6,
            n => n > dance::DANCES,
        };
        let composition = if composed {
            dance::compose(&mut rng, &self.angles, to, motor, &self.variety)
        } else if choice == 0 {
            // From the repertoire, whichever has gone longest unperformed.
            let names: Vec<String> = (0..dance::DANCES).map(|i| dance::named(i, &mut Rng::new(0)).tags[0].clone()).collect();
            let freshest = self.variety.freshest(&names, || rng.f32()).and_then(|n| names.iter().position(|m| m == n));
            dance::named(freshest.unwrap_or(0), &mut rng)
        } else {
            dance::named(choice - 1, &mut rng)
        };
        self.chosen = Some((minute, choice, composition.clone()));
        composition
    }

    fn perform(&mut self, ctx: &Ctx, composition: dance::Composition, to: [Hands; CLOCKS], minute: i64, motor: Motor) {
        let (moves, total) = dance::plan(&composition.phases, &self.angles, &to, motor);
        eprintln!(
            "numerals: t={:.1} {:02}:{:02} by {}, {total:.1} s",
            ctx.t,
            minute.div_euclid(60).rem_euclid(24),
            minute.rem_euclid(60),
            composition.name
        );
        for hands in &mut self.angles {
            *hands = hands.map(|a| a.rem_euclid(360.0));
        }
        self.ambient = None;
        self.variety.note(&composition.name, &composition.tags);
        self.performed = Some(composition);
        self.chosen = None;
        self.plan = Some(Plan { began: ctx.t, from: self.angles, to, moves, total, minute });
    }

    /// Ease the picture towards "the time is being held" and back, so that the
    /// resting dials recede once the hands land and are at full strength for
    /// every dance. Six tenths of a second: slow enough not to read as a blink,
    /// quick enough that the digits are clean by the time anyone looks up.
    fn settle(&mut self, dt: f32) {
        const SETTLING: f32 = 0.6;
        let holding = self.minute.is_some() && self.plan.is_none() && self.ambient.is_none();
        let by = (dt / SETTLING).max(0.0);
        self.settled = if holding { (self.settled + by).min(1.0) } else { (self.settled - by).max(0.0) };
    }

    fn step(&mut self, ctx: &Ctx) {
        let pace = ctx.get("pace");
        // Display seconds per engine second. At pace 60 the wall clock is used
        // directly, so the time is right however long the patch has run.
        let rate = 60.0 / pace as f64;
        let born = *self.born.get_or_insert(ctx.now - ctx.t);
        let clock = if pace >= 59.5 { ctx.now } else { born + ctx.t * rate } + ctx.get("offset") as f64 * 60.0;
        let motor = Motor { speed: ctx.get("speed"), acc: ctx.get("speed") * 1.6 };
        let hours24 = ctx.get("hours24") >= 0.5;
        let rest = Rest::of(ctx.get("rest"));
        self.treatment = rest.name;

        if let Some(p) = self.plan.take() {
            let tau = (ctx.t - p.began) as f32;
            for i in 0..CLOCKS {
                for h in 0..2 {
                    self.angles[i][h] = dance::angle_at(p.from[i][h], &p.moves[i][h], motor, tau);
                }
            }
            self.doing = format!("dancing, {:.0} s to go", (p.total - tau).max(0.0));
            if tau >= p.total {
                self.angles = p.to;
                self.minute = Some(p.minute);
                self.landed = ctx.t;
            } else {
                self.plan = Some(p);
            }
            return;
        }

        // A dance asked for from the studio is performed at once, to the time
        // already showing, and the minute's own dance follows when it is due.
        if let (Some(request), Some(shown)) = (self.request.take(), self.minute) {
            let to = Self::target(shown, hours24, rest);
            let composition = match (request, &self.performed) {
                (Request::Again, Some(last)) => last.clone(),
                _ => {
                    self.asked += 1;
                    self.chosen = None;
                    self.dance_for(shown, dance::DANCES + 1, &to, motor)
                }
            };
            self.perform(ctx, composition, to, shown, motor);
            return;
        }

        // Not dancing. On the first frame go straight to the current time;
        // after that, set off early enough to land on the next minute as it turns.
        let now_minute = (clock / 60.0).floor() as i64;
        let next = match self.minute {
            None => now_minute,
            Some(shown) if shown < now_minute => now_minute,
            Some(shown) => shown + 1,
        };
        let to = Self::target(next, hours24, rest);
        let composition = self.dance_for(next, ctx.get("dance") as usize, &to, motor);
        let (_, total) = dance::plan(&composition.phases, &self.angles, &to, motor);
        // Engine seconds until the dance has to begin.
        let slack = (next as f64 * 60.0 - clock) / rate - total as f64;
        let ready = self.ambient.as_ref().is_none_or(|a| a.at_rest());
        if self.minute.is_none() || (slack <= 0.0 && ready) {
            self.perform(ctx, composition, to, next, motor);
            return;
        }
        self.doing = match &self.ambient {
            Some(a) => format!("drifting ({}), next dance in {:.0} s", a.name(), slack.max(0.0)),
            None => format!("holding the time, next dance in {:.0} s", slack.max(0.0)),
        };

        // Between the time and the next dance: hold still for a while, then
        // drift. The ambient field is asked to settle with room to spare, so
        // the hands are at rest when the dance has to leave.
        const SETTLE: f64 = 4.0;
        let held = ctx.get("still") as f64 * pace as f64 / 60.0;
        match &mut self.ambient {
            Some(ambient) => ambient.step(&mut self.angles, motor, ctx.dt as f32, slack < SETTLE),
            None if ctx.t - self.landed >= held && slack > SETTLE + 6.0 => {
                let mut rng = Rng::new(self.seed ^ (next as u64).wrapping_mul(0x51ed_270b));
                let moods: Vec<String> = (0..ambient::MOODS).map(|m| format!("mood:{m}")).collect();
                let mood = self.variety.freshest(&moods, || rng.f32()).and_then(|n| moods.iter().position(|m| m == n)).unwrap_or(0);
                self.variety.note("", std::slice::from_ref(&moods[mood]));
                let ambient = ambient::Ambient::new(&mut rng, mood, COLS, ROWS, CELL);
                eprintln!("numerals: t={:.1} ambient {}", ctx.t, ambient.name());
                self.ambient = Some(ambient);
            }
            None => {}
        }
    }
}

impl Patch for Clocks {
    fn playing(&self) -> Option<Playing> {
        let performed = self.performed.as_ref()?;
        let actions = vec![
            Action { id: "again", label: "Play it again" },
            Action { id: "another", label: "Compose another" },
        ];
        Some(Playing { title: performed.name.clone(), detail: self.doing.clone(), actions, notes: vec![format!("resting dials: {}", self.treatment)] })
    }

    fn act(&mut self, action: &str) {
        match action {
            "again" => self.request = Some(Request::Again),
            "another" => self.request = Some(Request::Another),
            _ => {}
        }
    }

    fn render(&mut self, ctx: &Ctx) -> Frame {
        self.step(ctx);
        self.settle(ctx.dt as f32);
        let tint = |hue: &str, chroma: &str| draw::Tint { hue: ctx.get(hue), chroma: ctx.get(chroma), light: ctx.get("light") };
        let look = Look {
            half: ctx.get("weight") * 0.5,
            tip: ctx.get("tip"),
            tints: [tint("hue", "chroma"), tint("hue2", "chroma2")],
            ring: ctx.get("dials"),
            dark: (ctx.get("dark") as usize).min(DARK_CHOICES.len() - 1),
        };
        // Which dials are not part of the time, so are drawn as being at rest.
        // Only while there is a time to read: during a dance every hand is a
        // dancer and the grid is at full strength.
        let idle = match self.minute {
            Some(m) if self.settled > 0.0 => {
                let (h, mm) = Self::digits(m, ctx.get("hours24") >= 0.5);
                resting(h, mm)
            }
            _ => [false; CLOCKS],
        };
        picture(&self.angles, &idle, Rest::of(ctx.get("rest")), self.settled, look)
    }
}

/// Everything about the picture that is not the hands: hand thickness, the two
/// hands' colours, the dial rings, and the dark ramp a resting dial lands on.
#[derive(Clone, Copy)]
struct Look {
    half: f32,
    /// The hands' taper while they dance: `draw::Dials::tip`.
    tip: f32,
    tints: [draw::Tint; 2],
    ring: f32,
    /// Index into [`DARK_CHOICES`]: the `dark` parameter. `0` is `"as it
    /// was"`; `DARK_RAMPS[dark - 1]` is every other one.
    dark: usize,
}

/// Draw the grid. `idle` marks the dials that are not part of a digit; they are
/// drawn back towards `rest` as the picture settles onto the time.
fn picture(angles: &[Hands; CLOCKS], idle: &[bool; CLOCKS], rest: Rest, settled: f32, look: Look) -> Frame {
    let scale = rest.scale(settled);
    // Card 188: only once a resting dial has actually landed - not while it
    // is still fading towards rest - does its ink become the hand-picked,
    // aligned shade. The fade itself is moving content (brief 2.1.1: "leave
    // everything else to the dither"), so it stays the cheap continuous ramp
    // the whole way; only the held, dark end of it needs steadying.
    let shade = (settled >= 1.0 && rest.ink < 1.0).then(|| dark_shade(look.dark, rest.ink)).flatten();
    let scales: Vec<draw::RestScale> = idle
        .iter()
        .map(|at_rest| {
            if *at_rest {
                draw::RestScale { ink: scale[0], reach: scale[1], shade }
            } else {
                draw::RestScale::FULL
            }
        })
        .collect();
    draw::Dials {
        angles,
        cols: COLS,
        rows: ROWS,
        cell: CELL,
        // The rounded tip ends exactly on the cell edge, meeting its neighbour's.
        lens: [CELL * 0.5 - look.half; 2],
        half: look.half,
        // A digit is drawn by hands meeting end to end across the cell edges,
        // and a stroke that pinched at every joint would not read as one. So
        // the taper is the dancers': it goes as the picture settles onto the
        // time, and the hands are blunt again by the time they are a numeral.
        tip: look.tip + (1.0 - look.tip) * settled.clamp(0.0, 1.0),
        rest: &scales,
        tints: look.tints,
        ring: look.ring,
        mark: 0.0,
    }
    .draw()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::Rgb;
    use crate::frame::{GUARANTEED_PALETTE, W};

    /// The times the owner found this patch failing at (card 160), and what
    /// each must draw. Every one of them contains a digit that leaves dials
    /// resting: `1`, `4` or `7`.
    const AWKWARD: [(u32, u32, [usize; 4]); 7] = [
        (21, 12, [2, 1, 1, 2]),
        (11, 11, [1, 1, 1, 1]),
        (10, 15, [1, 0, 1, 5]),
        (14, 47, [1, 4, 4, 7]),
        (9, 5, [0, 9, 0, 5]),
        (0, 0, [0, 0, 0, 0]),
        (1, 11, [0, 1, 1, 1]),
    ];

    fn look() -> Look {
        let tint = |hue| draw::Tint { hue, chroma: 0.05, light: 0.93 };
        Look { half: 1.0, tip: 0.45, tints: [tint(80.0), tint(80.0)], ring: 0.0, dark: DEFAULT_DARK }
    }

    /// What one cell of a glyph asks of its dial, so a pose can be read back.
    fn cell(glyph: &str, row: usize, rest: Rest) -> Hands {
        let (b, at_rest) = (glyph.as_bytes(), rest.hands(row));
        [0, 1].map(|h| match b[h] {
            b'N' => at_rest[h],
            c => direction(c),
        })
    }

    /// Which numeral each pair of columns is drawing. Panics if a digit
    /// position is not a numeral, or is two numerals at once.
    fn read_back(angles: &[Hands; CLOCKS], rest: Rest) -> [usize; 4] {
        [0, 1, 2, 3].map(|d| {
            let matches: Vec<usize> = (0..10)
                .filter(|g| {
                    (0..6).all(|k| {
                        let (row, col) = (k / 2, d * 2 + k % 2);
                        angles[row * COLS + col] == cell(DIGITS[*g][k], row, rest)
                    })
                })
                .collect();
            assert_eq!(matches.len(), 1, "digit {d} under `{}` reads as {matches:?}", rest.name);
            matches[0]
        })
    }

    /// Light drawn in each dial's 8 x 8 cell, as a share of a full hand's ink.
    fn ink(frame: &Frame) -> [f32; CLOCKS] {
        let top = (crate::frame::H - ROWS * CELL as usize) / 2;
        let mut out = [0.0; CLOCKS];
        for (i, cell) in out.iter_mut().enumerate() {
            let (row, col) = (i / COLS, i % COLS);
            for y in 0..CELL as usize {
                for x in 0..CELL as usize {
                    let p = (top + row * CELL as usize + y) * W + col * CELL as usize + x;
                    *cell += frame.pixel(p).luma();
                }
            }
        }
        out
    }

    #[test]
    fn digits_are_distinct_and_well_formed() {
        for (a, da) in DIGITS.iter().enumerate() {
            assert!(da.iter().all(|c| c.len() == 2 && c.bytes().all(|b| b"URDLN".contains(&b))));
            for db in &DIGITS[a + 1..] {
                assert_ne!(da, db);
            }
        }
    }

    /// The format, pinned. Four digits, leading zeros kept, no separator of any
    /// kind: `09:05` is `0905` and the 24 dials are four numerals and nothing
    /// else. This is the original's format and the owner's decision (card 160);
    /// the patch is not to grow a colon.
    #[test]
    fn the_time_is_four_digits_with_no_punctuation() {
        for &rest in RESTS {
            for (hh, mm, expected) in AWKWARD {
                assert_eq!(read_back(&pose(hh, mm, rest), rest), expected, "{hh:02}:{mm:02} under `{}`", rest.name);
            }
            // Nothing is set aside for a separator: every one of the 24 dials
            // belongs to one of the four numerals.
            let idle = resting(21, 12);
            assert_eq!(idle.len(), CLOCKS);
            // 12-hour is the same four digits: 13:11 is `0111`, 00:00 is `1200`.
            assert_eq!(Clocks::digits(13 * 60 + 11, false), (1, 11));
            assert_eq!(Clocks::digits(0, false), (12, 0));
            assert_eq!(Clocks::digits(0, true), (0, 0));
            assert_eq!(read_back(&Clocks::target(13 * 60 + 11, false, rest), rest), [0, 1, 1, 1]);
        }
    }

    /// The defect itself, at every awkward time: the dials that are not part of
    /// a digit must not draw as much light as the ones that are. Three strokes
    /// of equal weight stacked beside a `1` is what made `21:12` read as
    /// `2:1:12`.
    #[test]
    fn resting_dials_are_quieter_than_digits() {
        for (v, rest) in RESTS.iter().enumerate() {
            for (hh, mm, _) in AWKWARD {
                let (angles, idle) = (pose(hh, mm, *rest), resting(hh, mm));
                let ink = ink(&picture(&angles, &idle, *rest, 1.0, look()));
                let share = |want: bool| {
                    let cells: Vec<f32> = (0..CLOCKS).filter(|i| idle[*i] == want).map(|i| ink[i]).collect();
                    cells.iter().sum::<f32>() / cells.len().max(1) as f32
                };
                let (at_rest, drawing) = (share(true), share(false));
                if !idle.iter().any(|r| *r) {
                    continue;
                }
                assert!(at_rest > 0.0, "rest {v} (`{}`) at {hh:02}:{mm:02}: resting dials went dark", rest.name);
                let ratio = at_rest / drawing;
                if v == 0 {
                    // The treatment that is kept only for comparison.
                    assert!(ratio > 0.5, "rest 0 is supposed to be what the patch did before");
                } else {
                    assert!(ratio < 0.5, "rest {v} (`{}`) at {hh:02}:{mm:02}: rest/digit ink {ratio:.2}", rest.name);
                }
            }
        }
    }

    /// Every treatment leaves the frame an exact one: a dimmed hand is its own
    /// ink scaled in linear light, which is what its anti-aliasing ramp already
    /// is, so it costs no palette entry. 31 colours, well under the 32 that go
    /// on the wire exactly. With the default `dark` ("as it was"), that stays
    /// true in every case - the default draws exactly the old picture, card
    /// 188's follow-up fix. `aligned_dark_variants_add_the_shade_entry` below
    /// is where 32 (the held, aligned shade's own exact entry) shows up.
    #[test]
    fn every_treatment_keeps_the_palette_exact() {
        for (v, rest) in RESTS.iter().enumerate() {
            for settled in [0.0, 0.5, 1.0] {
                for (hh, mm, _) in AWKWARD {
                    let frame = picture(&pose(hh, mm, *rest), &resting(hh, mm), *rest, settled, look());
                    let Frame::Indexed { palette, indices } = &frame else { panic!("rest {v}: not an indexed frame") };
                    assert_eq!(palette.len(), 31, "rest {v} settled {settled}: palette grew");
                    assert!(palette.len() <= GUARANTEED_PALETTE, "and exact whatever the indices do");
                    assert!(indices.iter().all(|i| (*i as usize) < palette.len()));
                }
            }
        }
    }

    /// An aligned `dark` variant (anything but `"as it was"`) adds its shade
    /// as its own palette entry, 32 colours, once a treatment that actually
    /// dims (`ink < 1.0`) has landed (`settled >= 1.0`) on a time that leaves
    /// at least one dial resting - still `<= GUARANTEED_PALETTE`, still exact.
    /// `9:05` and `0:00` draw no resting dials at all (digits `0`, `5`, `9`
    /// have no `N` cells), so `want` has to check that too.
    #[test]
    fn aligned_dark_variants_add_the_shade_entry() {
        for (dark, name) in DARK_CHOICES.iter().enumerate().skip(1) {
            let look = Look { dark, ..look() };
            for (v, rest) in RESTS.iter().enumerate() {
                for settled in [0.0, 0.5, 1.0] {
                    for (hh, mm, _) in AWKWARD {
                        let frame = picture(&pose(hh, mm, *rest), &resting(hh, mm), *rest, settled, look);
                        let Frame::Indexed { palette, indices } = &frame else { panic!("rest {v}: not an indexed frame") };
                        let has_idle = resting(hh, mm).iter().any(|r| *r);
                        let want = if settled >= 1.0 && rest.ink < 1.0 && has_idle { 32 } else { 31 };
                        assert_eq!(palette.len(), want, "`{name}` rest {v} settled {settled}: palette grew");
                        assert!(palette.len() <= GUARANTEED_PALETTE, "and exact whatever the indices do");
                        assert!(indices.iter().all(|i| (*i as usize) < palette.len()));
                    }
                }
            }
        }
    }

    /// The taper belongs to the dance. A numeral is strokes meeting end to end
    /// across the cell edges, so once the time has landed the picture is the
    /// one blunt hands draw, whatever `tip` says - and while they dance a
    /// tapered hand is a lighter one.
    #[test]
    fn a_numeral_is_drawn_with_blunt_hands_whatever_the_tip() {
        let rest = RESTS[DEFAULT_REST];
        let (angles, idle) = (pose(21, 12, rest), resting(21, 12));
        let with = |tip, settled| picture(&angles, &idle, rest, settled, Look { tip, ..look() });
        let indices = |f: Frame| match f {
            Frame::Indexed { indices, .. } => indices,
            Frame::Linear(_) => panic!("not an indexed frame"),
        };
        assert_eq!(indices(with(0.3, 1.0)), indices(with(1.0, 1.0)), "the taper survived into the numeral");
        let total = |f: &Frame| ink(f).iter().sum::<f32>();
        assert!(total(&with(0.3, 0.0)) < total(&with(1.0, 0.0)) * 0.9, "a tapered dancer is not lighter than a blunt one");
    }

    /// A resting dial is drawn at full strength while the hands are dancing and
    /// fades back only once the time has landed, so a dance is never half lit.
    #[test]
    fn dials_only_recede_once_the_time_has_landed() {
        let rest = RESTS[DEFAULT_REST];
        let (angles, idle) = (pose(21, 12, rest), resting(21, 12));
        let ink_at = |settled| {
            let ink = ink(&picture(&angles, &idle, rest, settled, look()));
            (0..CLOCKS).filter(|i| idle[*i]).map(|i| ink[i]).sum::<f32>()
        };
        let (dancing, half, held) = (ink_at(0.0), ink_at(0.5), ink_at(1.0));
        assert!(dancing > half && half > held, "the fade is not monotonic: {dancing} {half} {held}");
        assert!(held / dancing < 0.5, "resting dials hardly faded: {:.2}", held / dancing);
    }

    /// A rest pose must stay a pose the choreography can pass through: every
    /// named dance, to and from the digits of every treatment, still lands on
    /// the time exactly and within the motor's limits.
    #[test]
    fn every_rest_treatment_dances() {
        const MOTOR: dance::Motor = dance::Motor { speed: 100.0, acc: 160.0 };
        for (v, rest) in RESTS.iter().enumerate() {
            let (from, to) = (pose(21, 11, *rest), pose(21, 12, *rest));
            for which in 0..dance::DANCES {
                let (name, phases) = dance::dance(which, &mut Rng::new(3));
                let (moves, total) = dance::plan(&phases, &from, &to, MOTOR);
                assert!((2.0..45.0).contains(&total), "rest {v}: {name} takes {total}s");
                for i in 0..CLOCKS {
                    for h in 0..2 {
                        let end = dance::angle_at(from[i][h], &moves[i][h], MOTOR, total + 1.0);
                        assert!(dance::shortest(end, to[i][h]).abs() < 0.01, "rest {v}: {name}, clock {i} hand {h}");
                        let mut prev = from[i][h];
                        for k in 1..=(total * 30.0) as usize {
                            let a = dance::angle_at(from[i][h], &moves[i][h], MOTOR, k as f32 / 30.0);
                            assert!((a - prev).abs() * 30.0 <= MOTOR.speed * 2.05, "rest {v}: {name} overdrives clock {i}");
                            prev = a;
                        }
                    }
                }
            }
        }
    }

    /// A rest pose varies by row at most, never by column: the three dials of a
    /// resting column are one decision, and the grid a dance passes through
    /// stays left-right symmetric.
    #[test]
    fn a_rest_pose_is_the_same_all_along_a_row() {
        for rest in RESTS {
            let angles = pose(11, 11, *rest);
            let idle = resting(11, 11);
            for row in 0..ROWS {
                let mut seen: Option<Hands> = None;
                for col in 0..COLS {
                    let i = row * COLS + col;
                    if idle[i] {
                        let hands = *seen.get_or_insert(angles[i]);
                        assert_eq!(angles[i], hands, "`{}`: row {row} rests two ways", rest.name);
                    }
                }
                assert!(seen.is_some(), "11:11 should leave row {row} with resting dials");
            }
        }
    }

    /// A resting dial must not be mistakable for a digit stroke. The digits are
    /// drawn only up, down, left and right, so a rest angle on one of those
    /// axes is a stroke; the shipped treatments keep off them.
    #[test]
    fn the_default_rest_pose_is_off_the_digits_axes() {
        let rest = RESTS[DEFAULT_REST];
        for row in 0..ROWS {
            for hand in rest.hands(row) {
                let off = [0.0, 90.0, 180.0, 270.0].iter().map(|a| dance::shortest(hand, *a).abs()).fold(f32::MAX, f32::min);
                assert!(off > 30.0, "a resting hand at {hand} deg is only {off} deg off a digit stroke");
            }
        }
        // And black is not an option: they stay visible clock hands.
        assert!(rest.ink > 0.05 && rest.reach > 0.5);
    }

    #[test]
    fn the_ramp_carries_a_dimmed_hand() {
        // A dimmed hand - resting (the default `dark`, "as it was") or a
        // digit's anti-aliased, rounded tip - is on its own ink's ray, so it
        // lands on steps of the ramp rather than needing colours of its own.
        // An *aligned* `dark` variant is different (see
        // `aligned_dark_variants_add_the_shade_entry`): its resting shade is
        // its own exact palette entry, not a ramp step.
        let rest = RESTS[DEFAULT_REST];
        let frame = picture(&pose(21, 12, rest), &resting(21, 12), rest, 1.0, look());
        let Frame::Indexed { palette, indices } = &frame else { panic!("not indexed") };
        let used: std::collections::BTreeSet<u8> = indices.iter().copied().collect();
        assert!(used.len() > 2, "the dim hands should be using ramp steps of their own");
        for i in used {
            let c: Rgb = palette[i as usize];
            assert!(c.r >= 0.0 && c.g >= 0.0 && c.b >= 0.0);
        }
    }

    /// Card 188 follow-up (owner's review, 2026-09-21): the first cut of this
    /// card made the aligned shade the default and moved the picture - the
    /// warm, graded rest shade the owner had already tuned became a much
    /// dimmer, nearly flat one - which the card's own rule ("the default must
    /// not change any existing patch's pixels") forbids. Fixed by making
    /// `DARK_CHOICES[0]`, `"as it was"`, the default and giving it `None`
    /// (`dark_shade`), so `draw::RestScale.shade` stays `None` and
    /// `draw.rs::draw()` falls through to the untouched `tint.scale(ink)` -
    /// exactly the pre-card-188 formula. Pinned two ways so neither the
    /// constant nor the function can drift back:
    #[test]
    fn the_default_dark_choice_draws_the_old_continuous_shade() {
        assert_eq!(DEFAULT_DARK, 0, "index 0 must stay \"as it was\"");
        assert_eq!(DARK_CHOICES[0], "as it was");
        for ink in [1.0, 0.5, 0.20, 0.10, 0.0] {
            assert_eq!(dark_shade(0, ink), None, "\"as it was\" must never produce a shade");
        }

        // No shade entry reaches the palette for any treatment or moment,
        // default `look()` - `every_treatment_keeps_the_palette_exact` above
        // already covers every `(rest, settled, time)` combination at 31; this
        // is the same fact from the picture-building side, for the specific
        // frame `screeny-art snapshot clocks-numerals --time 21:12 --seed 7`
        // renders (the one the review compared byte for byte against the
        // pre-card-188 render - see the Log).
        let rest = RESTS[DEFAULT_REST];
        let frame = picture(&pose(21, 12, rest), &resting(21, 12), rest, 1.0, look());
        let Frame::Indexed { palette, .. } = &frame else { panic!("not indexed") };
        assert_eq!(palette.len(), 31, "the default must not add a shade entry");
    }

    /// Card 188's acceptance: once the resting dials have landed (`settled` =
    /// 1, not mid-fade) and the frame has gone through the snap
    /// (`Panel::AlignedDark`, card 188 deliverable 2), no channel below the
    /// dark-end threshold sits more than 2/16 off its level - the same bound
    /// the aligned table itself promises (`screeny_panel::aligned_levels`).
    /// Every treatment, `"as it was"` included: that one does not use the
    /// patch's own hand-picked dark ramp (it is kept undimmed, for
    /// comparison), so it is what proves the pipeline-level snap catches
    /// held, dark pixels a patch has not aligned itself - the hand-picked
    /// ramp is the better-looking fix, this is the backstop.
    #[test]
    fn held_dark_shades_land_within_two_sixteenths_of_a_level() {
        let output = crate::pipeline::Output { panel: crate::panel::Panel::AlignedDark, ..Default::default() };
        for rest in RESTS {
            let frame = picture(&pose(21, 12, *rest), &resting(21, 12), *rest, 1.0, look());
            let out = crate::pipeline::Pipeline::new(output).process(frame, 1.0 / 30.0);
            for &code in &out.wire.rgb {
                let (level, offset) = screeny_panel::nearest_level(screeny_panel::duty_16ths(code));
                if level < crate::panel::DARK_ALIGN_LEVEL {
                    assert!(
                        offset.abs() <= 2,
                        "`{}`: code {code} (level {level}) is {offset} sixteenths off",
                        rest.name
                    );
                }
            }
        }
    }
}
