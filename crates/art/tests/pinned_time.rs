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
/// holds it for eight, and `vesta` is born reading the time and holds it until
/// the minute turns (card 155).
const TELLERS: [(&str, f64); 3] = [("clocks-numerals", 20.0), ("clocks-dials", 15.0), ("vesta", 3.0)];

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

/// Every teller's shot at `clock`, each on its own thread: a shot is seconds
/// of rendering and the three share nothing.
fn tellers_at(clock: Clock) -> Vec<Vec<u8>> {
    std::thread::scope(|s| {
        let shots: Vec<_> = TELLERS.iter().map(|(id, at)| s.spawn(move || pixels(id, *at, clock, 7))).collect();
        shots.into_iter().map(|h| h.join().unwrap_or_else(|p| std::panic::resume_unwind(p))).collect()
    })
}

/// The one the card is about. Two renders of the same pinned shot, more than a
/// second of real time apart - long enough that a live run would have moved
/// its hands, shifted the moment a dance sets off, and possibly changed the
/// minute - must be the same picture.
#[test]
fn a_pinned_time_draws_the_same_picture_whenever_it_is_rendered() {
    let clock = at("21:12");
    let before = tellers_at(clock);
    // Let the machine's clock move on.
    std::thread::sleep(Duration::from_millis(1100));
    for ((id, _), (was, now)) in TELLERS.iter().zip(before.iter().zip(tellers_at(clock))) {
        assert_eq!(*was, now, "{id}: a pinned run must not depend on when it is run");
    }
}

/// And it is really the *clock* doing it: the same run at a different pinned
/// time is a different picture. (21:12 and 10:10 differ in every digit and put
/// the dials' hands in different quadrants, so no patch can pass this by
/// ignoring `--time`.)
#[test]
fn two_pinned_times_are_two_pictures() {
    let (a, b) = (tellers_at(at("21:12")), tellers_at(at("10:10")));
    for ((id, _), (a, b)) in TELLERS.iter().zip(a.iter().zip(&b)) {
        assert_ne!(a, b, "{id}: the pinned time has to reach the patch");
    }
}

/// `snapshot clocks-numerals --time 21:12 --out x.png` is the card's
/// acceptance and the README's one obvious command, and it carries no seed:
/// once the hands have landed on 21:12 and the time is being held, what is on
/// the panel is the minute, not the dance that got there.
///
/// This is that command's shot - the binary's `--time` defaults, 20 s rendered
/// from engine time zero - rendered with two unrelated seeds and with the
/// first and last of the thirteen choreographies pinned. One picture. 20 s is after the longest
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
    // dance 0 is "vary", so the two seeds are two different choreographies;
    // and the first and last of the thirteen pinned ones stand for the rest
    // (each is a 20 s render, and all thirteen were most of this file's time).
    // One thread each.
    let runs = [(1, 0_u8), (7, 0), (7, 1), (7, 13)];
    let shots: Vec<Vec<u8>> = std::thread::scope(|s| {
        let shots: Vec<_> = runs.iter().map(|&(seed, dance)| s.spawn(move || render(seed, f32::from(dance)))).collect();
        shots.into_iter().map(|h| h.join().unwrap_or_else(|p| std::panic::resume_unwind(p))).collect()
    });
    for ((seed, dance), shot) in runs.iter().zip(&shots).skip(1) {
        assert_eq!(shots[0], *shot, "seed {seed}, dance {dance}: the settled minute must not depend on the choreography");
    }
}

/// A patch that does not tell the time does not notice: `--time` is about
/// `Ctx::now` and nothing else, so pinning it cannot move a picture that never
/// reads it.
#[test]
fn a_patch_that_does_not_tell_the_time_is_untouched() {
    // Card 178 took `plasma` and `testcard` out of this list with the patches
    // themselves; `flock` is the other one that never reads `Ctx::now`.
    for id in ["metaballs", "flock"] {
        assert_eq!(
            pixels(id, 3.0, at("21:12"), 7),
            pixels(id, 3.0, at("04:05:06"), 7),
            "{id}: the clock is not one of its inputs"
        );
    }
}
