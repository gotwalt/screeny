//! Card 162: a pinned clock makes a time-telling patch repeatable.
//!
//! The promise is "the same command draws the same picture today and
//! tomorrow", so what is checked here is the snapshot path the command runs
//! ([`screeny_art::snapshot::take`]), not a re-implementation of it: the same
//! shot rendered at two different real moments, the same shot rendered with
//! two different seeds, and two different pinned times, which must differ.

use screeny_art::patch::{self, Clock, Params};
use screeny_art::snapshot::{take, Shot};
use std::time::Duration;

/// The patches that read `Ctx::now`, and a run long enough to have told the
/// time: the numerals patch dances onto the minute it is born on (about 9-16
/// s), the dials patch gathers over the six seconds before the minute and
/// holds it for eight.
const TELLERS: [(&str, f64); 2] = [("clocks-numerals", 20.0), ("clocks-dials", 15.0)];

/// Every frame of the run is rendered (`warmup == at`): a clock has to be
/// watched from its first frame, which is what `--time` makes the default.
fn pixels(id: &str, at: f64, clock: Clock, seed: u64) -> Vec<u8> {
    let def = patch::find(id).unwrap_or_else(|| panic!("no patch `{id}`"));
    let shot = Shot { seed, at, warmup: at, clock, ..Shot::default() };
    take(def, &Params::defaults(def.params), &shot).preview
}

fn at(hhmm: &str) -> Clock {
    Clock::parse(hhmm).unwrap_or_else(|e| panic!("{hhmm}: {e}"))
}

/// The one the card is about. Two renders of the same pinned shot, more than a
/// second of real time apart - long enough that a live run would have moved
/// its hands, shifted the moment a dance sets off, and possibly changed the
/// minute - must be the same picture.
#[test]
fn a_pinned_time_draws_the_same_picture_whenever_it_is_rendered() {
    let clock = at("21:12");
    let before: Vec<Vec<u8>> = TELLERS.iter().map(|(id, at)| pixels(id, *at, clock, 7)).collect();
    // Let the machine's clock move on.
    std::thread::sleep(Duration::from_millis(1100));
    for (i, (id, at)) in TELLERS.iter().enumerate() {
        assert_eq!(before[i], pixels(id, *at, clock, 7), "{id}: a pinned run must not depend on when it is run");
    }
}

/// And it is really the *clock* doing it: the same run at a different pinned
/// time is a different picture. (21:12 and 10:10 differ in every digit and put
/// the dials' hands in different quadrants, so no patch can pass this by
/// ignoring `--time`.)
#[test]
fn two_pinned_times_are_two_pictures() {
    for (id, at_s) in TELLERS {
        assert_ne!(
            pixels(id, at_s, at("21:12"), 7),
            pixels(id, at_s, at("10:10"), 7),
            "{id}: the pinned time has to reach the patch"
        );
    }
}

/// `snapshot clocks-numerals --time 21:12 --out x.png` is the card's
/// acceptance and the README's one obvious command, and it carries no seed:
/// once the hands have landed on 21:12 and the time is being held, what is on
/// the panel is the minute, not the dance that got there.
///
/// This is that command's shot - the binary's `--time` defaults, 20 s rendered
/// from engine time zero - rendered with four unrelated seeds and with each of
/// the thirteen choreographies pinned. One picture. 20 s is after the longest
/// opening dance (15.4 s, measured over 60 seeds) and long before the patch
/// sets off for 21:13, so nothing here is near an edge.
#[test]
fn the_time_alone_is_the_settled_minute_whatever_the_dance() {
    let def = patch::find("clocks-numerals").expect("the patch");
    let render = |seed, dance| {
        let mut params = Params::defaults(def.params);
        assert!(params.set(def.params, "dance", dance));
        let shot = Shot { seed, at: 20.0, warmup: 20.0, clock: at("21:12"), ..Shot::default() };
        take(def, &params, &shot).preview
    };
    // dance 0 is "vary", so these four are four different choreographies.
    let want = render(1, 0.0);
    for seed in [7, 42, 999] {
        assert_eq!(want, render(seed, 0.0), "seed {seed}: the settled minute must not depend on the choreography");
    }
    for dance in 1..=13_u8 {
        assert_eq!(want, render(7, f32::from(dance)), "dance {dance}: it lands on the same picture");
    }
}

/// A patch that does not tell the time does not notice: `--time` is about
/// `Ctx::now` and nothing else, so pinning it cannot move a picture that never
/// reads it.
#[test]
fn a_patch_that_does_not_tell_the_time_is_untouched() {
    for id in ["plasma", "metaballs", "testcard"] {
        assert_eq!(
            pixels(id, 3.0, at("21:12"), 7),
            pixels(id, 3.0, at("04:05:06"), 7),
            "{id}: the clock is not one of its inputs"
        );
    }
}
