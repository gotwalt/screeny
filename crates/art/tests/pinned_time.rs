//! Card 162: a pinned clock makes a time-telling piece repeatable.
//!
//! The promise is "the same command draws the same picture today and
//! tomorrow", so what is checked here is the snapshot path the command runs
//! ([`screeny_art::snapshot::take`]), not a re-implementation of it: the same
//! shot rendered at two different real moments, the same shot rendered with
//! two different seeds, and two different pinned times, which must differ.

use screeny_art::piece::{self, Clock, Params};
use screeny_art::snapshot::{take, Shot};
use std::time::Duration;

/// The pieces that read `Ctx::now`, and a run long enough to have told the
/// time: the numerals piece dances onto the minute it is born on (about 9-16
/// s), the dials piece gathers over the six seconds before the minute and
/// holds it for eight.
const TELLERS: [(&str, f64); 2] = [("clocks-numerals", 20.0), ("clocks-dials", 15.0)];

/// Every frame of the run is rendered (`warmup == at`): a clock has to be
/// watched from its first frame, which is what `--time` makes the default.
fn pixels(id: &str, at: f64, clock: Clock, seed: u64) -> Vec<u8> {
    let def = piece::find(id).unwrap_or_else(|| panic!("no piece `{id}`"));
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
/// the dials' hands in different quadrants, so no piece can pass this by
/// ignoring `--time`.)
#[test]
fn two_pinned_times_are_two_pictures() {
    for (id, at_s) in TELLERS {
        assert_ne!(
            pixels(id, at_s, at("21:12"), 7),
            pixels(id, at_s, at("10:10"), 7),
            "{id}: the pinned time has to reach the piece"
        );
    }
}

/// The settled numeral picture for a minute is the one command in the README,
/// and it carries no seed: once the hands have landed on 21:12 and the time is
/// being held, what is on the panel is the minute, not the dance that got
/// there. Four unrelated seeds, one picture.
///
/// `still=60` holds the time for the whole minute, so the frame at 40 s is
/// after the longest opening dance and before the next one sets off.
#[test]
fn the_settled_numerals_picture_is_the_minute_not_the_dance() {
    let def = piece::find("clocks-numerals").expect("the piece");
    let mut params = Params::defaults(def.params);
    assert!(params.set(def.params, "still", 60.0));
    let render = |seed| {
        let shot = Shot { seed, at: 40.0, warmup: 40.0, clock: at("21:12"), ..Shot::default() };
        take(def, &params, &shot).preview
    };
    let want = render(1);
    for seed in [7, 42, 999] {
        assert_eq!(want, render(seed), "seed {seed}: the settled minute must not depend on the choreography");
    }
}

/// A piece that does not tell the time does not notice: `--time` is about
/// `Ctx::now` and nothing else, so pinning it cannot move a picture that never
/// reads it.
#[test]
fn a_piece_that_does_not_tell_the_time_is_untouched() {
    for id in ["plasma", "metaballs", "testcard"] {
        assert_eq!(
            pixels(id, 3.0, at("21:12"), 7),
            pixels(id, 3.0, at("04:05:06"), 7),
            "{id}: the clock is not one of its inputs"
        );
    }
}
