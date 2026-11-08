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

pub(crate) use screeny_clock::{ambient, dance};
pub(crate) mod dials;
pub(crate) mod draw;

use crate::color::Rgb;
use crate::frame::Frame;
use crate::patch::{choice, param, toggle, Action, Ctx, ParamSpec, Patch, PatchDef, Playing};
pub(crate) use screeny_clock::face::{
    dark_levels, Hands, Rest, CELL, CLOCKS, COLS, DARK_CHOICES, DEFAULT_DARK, DEFAULT_REST, RESTS, ROWS,
};
#[cfg(test)]
pub(crate) use screeny_clock::face::DARK_RAMPS;
#[cfg(test)]
pub(crate) use screeny_clock::face::{pose, resting};
use screeny_clock::engine::{Doing, Engine, Knobs, Mode, Request, Tick};

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

// The defaults are the owner's "Gold and Blue" (2026-10-28), the setting that
// lived on the panel when the studio ran on Home Assistant: a brass hour hand
// and a slate minute hand, muted, quick to settle, thin hands, faint rests.
const PARAMS: &[ParamSpec] = &[
    // Art: the time from :00 to :10, then drifting. Time: from :00 to :45.
    choice("mode", "Mode", &Mode::NAMES, 0.0),
    param("fade", "Seconds the resting dials take to dim", 0.2, 5.0, 0.1, 1.5),
    toggle("calm", "Reduce motion", false),
    param("limit", "Reduce motion: top hand speed (deg/s)", 20.0, 120.0, 1.0, 60.0),
    choice("dance", "Choreography", DANCE_CHOICES, 0.0),
    choice("rest", "Resting dials", REST_CHOICES, DEFAULT_REST as f32),
    choice("dark", "Dark ramp", DARK_CHOICES, DEFAULT_DARK as f32),
    param("speed", "Hand speed (deg/s)", 30.0, 360.0, 1.0, 100.0),
    toggle("hours24", "24-hour", true),
    param("weight", "Hand weight (LEDs)", 1.0, 2.4, 0.05, 1.55),
    param("tip", "Hand tip while dancing (1 = blunt)", 0.2, 1.0, 0.05, 0.2),
    param("dials", "Dial rings", 0.0, 1.0, 0.01, 0.0),
    param("hue", "Hour hand hue", 0.0, 360.0, 1.0, 74.0),
    param("chroma", "Hour hand colour", 0.0, 0.2, 0.005, 0.065),
    param("hue2", "Minute hand hue", 0.0, 360.0, 1.0, 240.0),
    param("chroma2", "Minute hand colour", 0.0, 0.2, 0.005, 0.065),
    param("light", "Hand lightness (lower = more saturated)", 0.6, 0.95, 0.01, 0.67),
];

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

/// The `dark` parameter's pick. `variant == 0` (`"as it was"`, the default)
/// is `None` - the caller keeps the old continuous `tint.scale(ink)` - and
/// every other variant indexes [`DARK_RAMPS`], further in as `ink` calls for
/// more light: a treatment's `ink` is small (0.10 or 0.20 today), so this
/// mostly picks the ramp's darkest step, but stays proportional if a future
/// treatment asks for more.
fn dark_shade(variant: usize, ink: f32) -> Option<Rgb> {
    dark_levels(variant, ink).map(crate::panel::level_triple)
}

/// The instrument and its engine are `screeny-clock`, shared with the firmware;
/// this patch is the Studio's parameters and picture on top of them.
struct Clocks {
    engine: Engine,
    /// The rest treatment in force, for the studio: it is a parameter, and
    /// `playing` does not see parameters.
    treatment: &'static str,
}

fn make(seed: u64) -> Box<dyn Patch> {
    Box::new(Clocks { engine: Engine::new(seed), treatment: RESTS[DEFAULT_REST].name })
}

/// The words the studio has shown since before the engine moved out.
fn doing_words(doing: Doing) -> String {
    match doing {
        Doing::Starting => String::new(),
        Doing::Dancing { to_go } => format!("dancing, {to_go:.0} s to go"),
        Doing::Drifting { mood, next } => format!("drifting ({mood}), next dance in {next:.0} s"),
        Doing::Holding { next } => format!("holding the time, next dance in {next:.0} s"),
    }
}

impl Patch for Clocks {
    fn playing(&self) -> Option<Playing> {
        let performed = self.engine.performed()?;
        let actions =
            vec![Action { id: "again", label: "Play it again" }, Action { id: "another", label: "Compose another" }];
        Some(Playing {
            title: performed.name.clone(),
            detail: doing_words(self.engine.doing()),
            actions,
            notes: vec![format!("resting dials: {}", self.treatment)],
        })
    }

    fn act(&mut self, action: &str) {
        match action {
            "again" => self.engine.ask(Request::Again),
            "another" => self.engine.ask(Request::Another),
            _ => {}
        }
    }

    fn render(&mut self, ctx: &Ctx) -> Frame {
        let rest = Rest::of(ctx.get("rest"));
        self.treatment = rest.name;
        let hours24 = ctx.get("hours24") >= 0.5;
        let knobs = Knobs {
            mode: Mode::of(ctx.get("mode")),
            dance: ctx.get("dance") as usize,
            speed: ctx.get("speed"),
            hours24,
            rest,
            fade: ctx.get("fade"),
            calm: ctx.get("calm") >= 0.5,
            limit: ctx.get("limit"),
        };
        self.engine.step(Tick { t: ctx.t, dt: ctx.dt, now: ctx.now }, &knobs);
        let tint = |hue: &str, chroma: &str| draw::Tint {
            hue: ctx.get(hue),
            chroma: ctx.get(chroma),
            light: ctx.get("light"),
        };
        let look = Look {
            half: ctx.get("weight") * 0.5,
            tip: ctx.get("tip"),
            tints: [tint("hue", "chroma"), tint("hue2", "chroma2")],
            ring: ctx.get("dials"),
            dark: (ctx.get("dark") as usize).min(DARK_CHOICES.len() - 1),
        };
        picture_dims(self.engine.angles(), self.engine.dims(), rest, self.engine.settled(), look)
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
#[cfg(test)]
fn picture(angles: &[Hands; CLOCKS], idle: &[bool; CLOCKS], rest: Rest, settled: f32, look: Look) -> Frame {
    picture_dims(angles, &idle.map(|i| if i { settled } else { 0.0 }), rest, settled, look)
}

/// [`picture`], with each dial's own distance towards its rest.
fn picture_dims(angles: &[Hands; CLOCKS], dims: &[f32; CLOCKS], rest: Rest, settled: f32, look: Look) -> Frame {
    // Card 188: only once a resting dial has actually landed - not while it
    // is still fading towards rest - does its ink become the hand-picked,
    // aligned shade. The fade itself is moving content (brief 2.1.1: "leave
    // everything else to the dither"), so it stays the cheap continuous ramp
    // the whole way; only the held, dark end of it needs steadying.
    let shade = (rest.ink < 1.0).then(|| dark_shade(look.dark, rest.ink)).flatten();
    let scales: Vec<draw::RestScale> = dims
        .iter()
        .map(|dim| {
            let [ink, reach] = rest.scale(*dim);
            draw::RestScale { ink, reach, shade: if *dim >= 1.0 { shade } else { None } }
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

/// The firmware's rasteriser draws the same picture as this patch.
///
/// The Studio supersamples and palette-maps and the device does neither, so
/// they are not expected to match to the bit. They are expected to match to
/// the eye: same strokes in the same places in the same colours.
#[cfg(test)]
mod device_picture {
    use super::*;
    use screeny_clock::raster::{Look as DeviceLook, Raster, Tint as DeviceTint, FRAME_BYTES};

    fn studio(angles: &[Hands; CLOCKS], idle: &[bool; CLOCKS], settled: f32) -> Vec<u8> {
        let tint = |hue, chroma| draw::Tint { hue, chroma, light: 0.67 };
        let look = Look {
            half: 0.775,
            tip: 0.2,
            tints: [tint(74.0, 0.065), tint(240.0, 0.065)],
            ring: 0.0,
            dark: 0,
        };
        let rest = RESTS[DEFAULT_REST];
        let mut out = Vec::new();
        for c in picture(angles, idle, rest, settled, look).to_linear() {
            out.extend_from_slice(&c.to_srgb8());
        }
        out
    }

    fn device(angles: &[Hands; CLOCKS], idle: &[bool; CLOCKS], settled: f32) -> Vec<u8> {
        let mut out = vec![0u8; FRAME_BYTES];
        let dims = idle.map(|i| if i { settled } else { 0.0 });
        Raster::new(DeviceLook::default()).draw(&mut out, angles, &dims, RESTS[DEFAULT_REST], settled);
        out
    }

    #[test]
    fn the_defaults_are_the_same_gold_and_blue() {
        let look = DeviceLook::default();
        let want = |hue, chroma, h: DeviceTint| assert_eq!((h.hue, h.chroma, h.light), (hue, chroma, 0.67));
        want(74.0, 0.065, look.tints[0]);
        want(240.0, 0.065, look.tints[1]);
        let knob = |id: &str| PARAMS.iter().find(|p| p.id == id).unwrap().default;
        assert_eq!((knob("hue"), knob("hue2"), knob("light")), (74.0, 240.0, 0.67));
        assert_eq!((knob("chroma"), knob("chroma2"), knob("tip")), (0.065, 0.065, 0.2));
        assert_eq!(knob("weight") * 0.5, look.half);
    }

    #[test]
    fn a_held_time_and_a_mid_dance_match_the_studio_to_the_eye() {
        let rest = RESTS[DEFAULT_REST];
        let mut scattered = [[0.0; 2]; CLOCKS];
        for (i, h) in scattered.iter_mut().enumerate() {
            *h = [i as f32 * 37.0 + 11.0, i as f32 * 91.0 + 3.0];
        }
        let cases = [
            (pose(10, 42, rest), resting(10, 42), 1.0),
            (pose(23, 58, rest), resting(23, 58), 1.0),
            (scattered, [false; CLOCKS], 0.0),
        ];
        for (angles, idle, settled) in cases {
            let (a, b) = (studio(&angles, &idle, settled), device(&angles, &idle, settled));
            let lit = a.iter().filter(|v| **v > 8).count() as f32;
            let diff: u64 = a.iter().zip(&b).map(|(x, y)| x.abs_diff(*y) as u64).sum();
            let per_lit = diff as f32 / lit;
            assert!(per_lit < 22.0, "{per_lit} per lit channel");
            let worst = a.iter().zip(&b).map(|(x, y)| x.abs_diff(*y)).max().unwrap();
            assert!(worst < 110, "one pixel is {worst} out");
        }
    }

    /// `screeny-panel` is host-only, so the device carries these as a table.
    #[test]
    fn the_devices_level_table_is_the_panels() {
        assert_eq!(screeny_clock::raster::DARK_ALIGN_LEVEL, crate::panel::DARK_ALIGN_LEVEL as usize);
        for (level, code) in screeny_clock::raster::LEVEL_CODES.iter().enumerate() {
            assert_eq!(*code, crate::panel::level_code(level as u32), "level {level}");
        }
    }

    /// How many dark pixels leave work for the panel's temporal dither: a
    /// channel between two levels alternates them, and held dark that shows.
    fn dark_dithering(frame: &[u8]) -> (usize, usize) {
        let q = |v: u8| (crate::color::srgb8_to_linear(v) * 1008.0).round() as i32;
        let dark = frame.chunks(3).filter(|p| {
            let top = p.iter().map(|v| q(*v)).max().unwrap();
            top > 0 && top < 16 * 8
        });
        let (mut all, mut dithering) = (0, 0);
        for p in dark {
            all += 1;
            dithering += usize::from(p.iter().any(|v| (1..15).contains(&(q(*v) % 16))));
        }
        (dithering, all)
    }

    /// Why the table exists. Without it every dark pixel of a held time was
    /// dithering, against a quarter in the Studio's frames, and the owner saw
    /// the difference on the panel (2026-11-07).
    #[test]
    fn the_dark_end_is_as_steady_as_the_studios() {
        let rest = RESTS[DEFAULT_REST];
        let (angles, idle) = (pose(10, 42, rest), resting(10, 42));
        let tint = |hue, chroma| draw::Tint { hue, chroma, light: 0.67 };
        let look = Look { half: 0.775, tip: 0.2, tints: [tint(74.0, 0.065), tint(240.0, 0.065)], ring: 0.0, dark: 0 };
        let output = crate::pipeline::Output { panel: crate::panel::Panel::AlignedDark, ..Default::default() };
        let sent = crate::pipeline::Pipeline::new(output).process(picture(&angles, &idle, rest, 1.0, look), 1.0 / 30.0);
        let (studio, of) = dark_dithering(&sent.wire.rgb);
        let (ours, our_of) = dark_dithering(&device(&angles, &idle, 1.0));
        println!("dark pixels dithering: studio {studio} of {of}, device {ours} of {our_of}");
        assert!(ours as f32 / our_of as f32 <= studio as f32 / of as f32 + 0.1, "{ours}/{our_of} against {studio}/{of}");
    }

    /// `SCREENY_CLOCK_PREVIEW=<dir> cargo test -p screeny-art device_picture`
    /// writes side-by-side enlargements to look at.
    #[test]
    fn preview() {
        let Ok(dir) = std::env::var("SCREENY_CLOCK_PREVIEW") else { return };
        let rest = RESTS[DEFAULT_REST];
        let (angles, idle) = (pose(10, 42, rest), resting(10, 42));
        let mut mid = pose(10, 42, rest);
        for (i, h) in mid.iter_mut().enumerate() {
            *h = [h[0] + i as f32 * 13.0, h[1] - i as f32 * 21.0];
        }
        const S: usize = 10;
        let (w, h) = (64 * S * 2 + 20, 32 * S * 2 + 20);
        let mut px = vec![30u8; w * h * 3];
        let mut blit = |f: &[u8], ox: usize, oy: usize| {
            for y in 0..32 * S {
                for x in 0..64 * S {
                    let (sx, sy) = (x / S, y / S);
                    let i = (sy * 64 + sx) * 3;
                    let gap = x % S == 0 || y % S == 0;
                    for c in 0..3 {
                        px[((y + oy) * w + x + ox) * 3 + c] = if gap { 0 } else { f[i + c] };
                    }
                }
            }
        };
        blit(&studio(&angles, &idle, 1.0), 0, 0);
        blit(&device(&angles, &idle, 1.0), 64 * S + 20, 0);
        blit(&studio(&mid, &[false; CLOCKS], 0.0), 0, 32 * S + 20);
        blit(&device(&mid, &[false; CLOCKS], 0.0), 64 * S + 20, 32 * S + 20);
        let file = std::fs::File::create(format!("{dir}/clock-studio-vs-device.png")).unwrap();
        let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w as u32, h as u32);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().unwrap().write_image_data(&px).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use crate::rng::Rng;
    use screeny_clock::face::{direction, DIGITS};
    use super::*;
    use crate::color::Rgb;
    use crate::frame::{GUARANTEED_PALETTE, W};

    /// The times the author found this patch failing at (card 160), and what
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

    /// The format, pinned. Four digits, no separator of any kind: `09:05` is
    /// `0905` and the 24 dials are four numerals and nothing else. This is the
    /// original's format and the author's decision (card 160); the patch is not
    /// to grow a colon. One exception, the owner's (2026-11-07): in 12-hour
    /// time an hour before 10 drops its leading zero, and those six dials rest
    /// (`crates/clock`'s `a_twelve_hour_clock_drops_its_leading_zero`).
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
            // 12-hour reads the hour the same way - 13:11 is hour 1, 00:00 is
            // 12 - and draws an hour before 10 without its leading zero.
            assert_eq!(Engine::digits(13 * 60 + 11, false), (1, 11));
            assert_eq!(Engine::digits(0, false), (12, 0));
            assert_eq!(Engine::digits(0, true), (0, 0));
            assert_eq!(Engine::shown(13 * 60 + 11, false), [screeny_clock::face::BLANK, 1, 1, 1]);
            assert_eq!(read_back(&Engine::target(13 * 60 + 11, true, rest), rest), [1, 3, 1, 1]);
            assert_eq!(read_back(&Engine::target(22 * 60 + 11, false, rest), rest), [1, 0, 1, 1]);
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
                        let Frame::Indexed { palette, indices } = &frame else {
                            panic!("rest {v}: not an indexed frame")
                        };
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
        assert!(
            total(&with(0.3, 0.0)) < total(&with(1.0, 0.0)) * 0.9,
            "a tapered dancer is not lighter than a blunt one"
        );
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
                            assert!(
                                (a - prev).abs() * 30.0 <= MOTOR.speed * 2.05,
                                "rest {v}: {name} overdrives clock {i}"
                            );
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
                let off =
                    [0.0, 90.0, 180.0, 270.0].iter().map(|a| dance::shortest(hand, *a).abs()).fold(f32::MAX, f32::min);
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

    /// Card 188 follow-up (author's review, 2026-09-21): the first cut of this
    /// card made the aligned shade the default and moved the picture - the
    /// warm, graded rest shade the author had already tuned became a much
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
