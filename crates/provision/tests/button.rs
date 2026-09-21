//! Card 230: the gesture recogniser, against the cases the card names.
//!
//! Every test drives the recogniser the way the firmware task does - a poll on
//! every level change and a poll every [`POLL_MS`] while a gesture is in
//! flight - so what passes here is what the device does.

use screeny_provision::button::{
    ButtonEvent, Level, Recognizer, DEBOUNCE_MS, HOLD_MS, STATUS_MS, WIPE_MS,
};

/// What the firmware's button task uses while a gesture is in flight.
const POLL_MS: u32 = 100;

/// A recogniser that has settled on a pin at rest, the way an ordinary boot
/// leaves it.
fn ready(at: u32) -> Recognizer {
    let mut b = Recognizer::new();
    assert_eq!(b.poll(Level::High, at, true), ButtonEvent::None);
    assert!(!b.active(), "a pin at rest is not a gesture");
    b
}

/// Press at `down`, release at `up`, polling every [`POLL_MS`] in between, and
/// collect everything the recogniser said.
fn press(b: &mut Recognizer, down: u32, up: u32, wipe_allowed: bool) -> Vec<ButtonEvent> {
    let mut out = Vec::new();
    let mut push = |e: ButtonEvent| {
        if e != ButtonEvent::None {
            out.push(e);
        }
    };
    push(b.poll(Level::Low, down, wipe_allowed));
    let mut t = down;
    loop {
        t = t.wrapping_add(POLL_MS);
        // `up` may be before the next poll instant; the release is fed at its
        // own timestamp either way.
        if since(up, down) < since(t, down) {
            break;
        }
        push(b.poll(Level::Low, t, wipe_allowed));
    }
    push(b.poll(Level::High, up, wipe_allowed));
    // The release has to settle before it is believed.
    push(b.poll(Level::High, up.wrapping_add(DEBOUNCE_MS), wipe_allowed));
    out
}

fn since(now: u32, then: u32) -> u32 {
    now.wrapping_sub(then)
}

#[test]
fn a_short_press_is_one_short_press() {
    let mut b = ready(0);
    assert_eq!(
        press(&mut b, 1_000, 1_200, true),
        vec![ButtonEvent::ShortPress]
    );
    assert!(!b.active());
}

#[test]
fn a_bounce_is_not_a_press() {
    let mut b = ready(0);
    // 11 ms of chatter, which is what card 203 photographed on this unit.
    assert_eq!(b.poll(Level::Low, 1_000, true), ButtonEvent::None);
    assert_eq!(b.poll(Level::High, 1_011, true), ButtonEvent::None);
    assert_eq!(b.poll(Level::Low, 1_022, true), ButtonEvent::None);
    // Settled low at last: this is the press, timed from here.
    assert_eq!(b.poll(Level::Low, 1_060, true), ButtonEvent::None);
    // ... and a bounce on the way back up is not a second press either.
    assert_eq!(b.poll(Level::High, 1_200, true), ButtonEvent::None);
    assert_eq!(b.poll(Level::Low, 1_211, true), ButtonEvent::None);
    assert_eq!(b.poll(Level::Low, 1_260, true), ButtonEvent::None);
    assert_eq!(b.poll(Level::High, 1_300, true), ButtonEvent::None);
    assert_eq!(
        b.poll(Level::High, 1_340, true),
        ButtonEvent::ShortPress,
        "the whole thing is one press"
    );
}

/// The card's "999 ms vs 1,000 ms". The 999 ms press is a short press and
/// nothing is ever shown on the panel about wiping anything; the 1,000 ms one
/// starts the countdown and then cancels.
#[test]
fn nine_hundred_and_ninety_nine_milliseconds_is_still_a_press() {
    let mut b = ready(0);
    let evs = press(&mut b, 10_000, 10_000 + HOLD_MS - 1, true);
    assert_eq!(evs, vec![ButtonEvent::ShortPress]);

    let mut b = ready(0);
    let evs = press(&mut b, 10_000, 10_000 + HOLD_MS, true);
    assert_eq!(
        evs,
        vec![
            ButtonEvent::HoldStarted { seconds_left: 4 },
            ButtonEvent::HoldCancelled
        ],
        "at exactly the threshold the countdown starts, and letting go cancels it"
    );
}

/// The card's "release at 4.9 s vs hold to 5.0 s".
#[test]
fn releasing_before_five_seconds_changes_nothing() {
    let mut b = ready(0);
    let evs = press(&mut b, 1_000, 1_000 + 4_900, true);
    assert!(
        !evs.contains(&ButtonEvent::WipeWifi),
        "4.9 s must not wipe: {evs:?}"
    );
    assert_eq!(evs.last(), Some(&ButtonEvent::HoldCancelled));
    // The countdown counted 4, 3, 2, 1 on the way.
    let ticks: Vec<u8> = evs
        .iter()
        .filter_map(|e| match e {
            ButtonEvent::HoldStarted { seconds_left } | ButtonEvent::HoldTick { seconds_left } => {
                Some(*seconds_left)
            }
            _ => None,
        })
        .collect();
    assert_eq!(ticks, vec![4, 3, 2, 1]);
}

#[test]
fn holding_to_five_seconds_wipes_exactly_once() {
    let mut b = ready(0);
    let evs = press(&mut b, 1_000, 1_000 + 8_000, true);
    assert_eq!(
        evs.iter().filter(|e| **e == ButtonEvent::WipeWifi).count(),
        1,
        "leaning on the button wipes once, not once a second: {evs:?}"
    );
    assert_eq!(evs.last(), Some(&ButtonEvent::WipeWifi), "and nothing after");
    // A second press afterwards still works.
    let evs = press(&mut b, 20_000, 20_200, true);
    assert_eq!(evs, vec![ButtonEvent::ShortPress]);
}

/// A hold that straddles the `u32` wrap at 49.7 days behaves like any other.
#[test]
fn a_press_across_the_millisecond_wrap_is_an_ordinary_press() {
    let start = u32::MAX - 2_000;
    let mut b = ready(start);
    let evs = press(&mut b, start.wrapping_add(500), start.wrapping_add(5_700), true);
    assert_eq!(evs.last(), Some(&ButtonEvent::WipeWifi));

    // And the short press on the other side of it.
    let mut b = ready(start);
    let evs = press(&mut b, start.wrapping_add(1_900), start.wrapping_add(2_100), true);
    assert_eq!(evs, vec![ButtonEvent::ShortPress]);
}

/// The card's stuck-low pin. A button that is already down when the firmware
/// starts - a jammed switch, or somebody holding it while the cable goes in -
/// must not wipe the credentials of a device nobody chose to reset.
#[test]
fn a_pin_low_at_boot_never_wipes_until_it_has_been_released() {
    let mut b = Recognizer::new();
    let mut evs = Vec::new();
    // Ten seconds held from the first instant the firmware can read the pin.
    let mut t = 0;
    while t <= 10_000 {
        let e = b.poll(Level::Low, t, true);
        if e != ButtonEvent::None {
            evs.push(e);
        }
        t += POLL_MS;
    }
    assert_eq!(evs, vec![], "nothing at all happens: {evs:?}");
    assert!(b.active(), "and the task keeps watching");

    // Released at last. Still nothing - that release is not a short press.
    assert_eq!(b.poll(Level::High, 10_100, true), ButtonEvent::None);
    assert_eq!(b.poll(Level::High, 10_140, true), ButtonEvent::None);
    assert!(!b.active());

    // From here it is an ordinary button.
    assert_eq!(
        press(&mut b, 11_000, 11_200, true),
        vec![ButtonEvent::ShortPress]
    );
    let evs = press(&mut b, 12_000, 12_000 + WIPE_MS + 100, true);
    assert_eq!(evs.last(), Some(&ButtonEvent::WipeWifi));
}

/// The card's "repeated presses during the 10 s screen". The recogniser has no
/// idea what the caller drew; each press is its own event, and the caller
/// extends the screen.
#[test]
fn presses_during_the_status_screen_are_each_a_short_press() {
    let mut b = ready(0);
    let mut at = 1_000;
    for _ in 0..4 {
        assert_eq!(
            press(&mut b, at, at + 150, true),
            vec![ButtonEvent::ShortPress]
        );
        at += 900;
    }
    assert!(
        at - 1_000 < STATUS_MS,
        "all four presses are inside one status screen"
    );
}

/// Card 230's OTA question. While an upload is in flight or an image is on
/// trial, the hold is refused: no countdown, no wipe, and one event the caller
/// can put on the panel.
#[test]
fn a_hold_during_an_update_is_refused_and_never_wipes() {
    let mut b = ready(0);
    let evs = press(&mut b, 1_000, 1_000 + WIPE_MS + 2_000, false);
    assert_eq!(evs, vec![ButtonEvent::HoldRefused]);

    // A short press is still a short press: showing the status screen costs
    // the update nothing.
    let mut b = ready(0);
    assert_eq!(
        press(&mut b, 1_000, 1_150, false),
        vec![ButtonEvent::ShortPress]
    );
}

/// An upload that starts *during* the countdown stops the wipe at the last
/// moment: the gate is read again when the wipe would fire.
#[test]
fn an_update_that_starts_mid_countdown_stops_the_wipe() {
    let mut b = ready(0);
    assert_eq!(b.poll(Level::Low, 1_000, true), ButtonEvent::None);
    assert_eq!(b.poll(Level::Low, 1_040, true), ButtonEvent::None);
    assert_eq!(
        b.poll(Level::Low, 2_000, true),
        ButtonEvent::HoldStarted { seconds_left: 4 }
    );
    assert_eq!(
        b.poll(Level::Low, 3_100, true),
        ButtonEvent::HoldTick { seconds_left: 3 }
    );
    // The upload starts here.
    assert_eq!(
        b.poll(Level::Low, 6_100, false),
        ButtonEvent::HoldRefused,
        "the gate is read again at the moment of the wipe"
    );
    assert_eq!(b.poll(Level::Low, 8_000, true), ButtonEvent::None);
    assert_eq!(b.poll(Level::High, 8_100, true), ButtonEvent::None);
    assert_eq!(b.poll(Level::High, 8_140, true), ButtonEvent::None);
}

/// A poll that arrives very late (the executor was busy for over a second)
/// still shows the countdown before it wipes: the panel always says what is
/// about to happen.
#[test]
fn a_late_poll_still_shows_the_countdown_before_it_wipes() {
    let mut b = ready(0);
    assert_eq!(b.poll(Level::Low, 1_000, true), ButtonEvent::None);
    assert_eq!(b.poll(Level::Low, 1_040, true), ButtonEvent::None);
    // Nothing ran for six seconds.
    let e = b.poll(Level::Low, 7_500, true);
    assert!(
        matches!(e, ButtonEvent::HoldStarted { .. }),
        "the countdown starts first: {e:?}"
    );
    assert_eq!(b.poll(Level::Low, 7_600, true), ButtonEvent::WipeWifi);
}

/// `active()` is what lets the firmware task sleep on the pin's interrupt
/// rather than poll: it must be true for every instant of a gesture and false
/// the moment one is over.
#[test]
fn active_says_when_the_task_has_to_keep_polling() {
    let mut b = ready(0);
    assert!(!b.active());
    b.poll(Level::Low, 1_000, true);
    assert!(b.active(), "a press that has not settled yet");
    b.poll(Level::Low, 1_040, true);
    assert!(b.active());
    b.poll(Level::High, 1_200, true);
    assert!(b.active(), "the release has not settled yet");
    b.poll(Level::High, 1_240, true);
    assert!(!b.active());
}
