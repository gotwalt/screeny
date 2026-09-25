//! Card 230: the button, end to end on a host.
//!
//! The simulator runs the firmware's gesture recogniser
//! (`screeny_provision::button`), so these tests are about what the *device*
//! does with the gestures: the status overlay, the countdown on the panel, the
//! hold that changes nothing, and the five seconds that forget the network and
//! raise the setup portal.
//!
//! Every hold is played against the recogniser's own clock, so a five-second
//! hold costs this file microseconds. Nothing here sleeps.

use screeny_proto::control::state;
use screeny_proto::{NBYTES, W};
use screeny_sim::{Config, SimDevice, WifiPhase, WifiTiming};

/// Timings compressed so nothing waits on a real join.
fn quick_timing() -> WifiTiming {
    WifiTiming {
        join_attempt_ms: 300,
        join_attempts: 3,
        trial_attempts: 3,
        ap_grace_ms: 200,
        portal_retry_ms: 1_000,
        link_down_ms: 200,
        connected_screen_ms: 1_000,
    }
}

fn online() -> SimDevice {
    let dev = SimDevice::start(Config {
        wifi_join_ms: 40,
        wifi_timing: quick_timing(),
        ..Config::for_test()
    })
    .expect("bind loopback");
    assert_eq!(dev.handle().wifi_phase(), WifiPhase::Online);
    assert!(dev.handle().stored_ssid().is_some());
    dev
}

/// The card's headline: a five-second hold on an online device ends at the
/// portal with the credentials cleared.
#[test]
fn a_five_second_hold_forgets_the_network_and_raises_the_portal() {
    let dev = online();
    let sim = dev.handle();

    sim.press_button(5_000);

    assert_eq!(sim.wifi_phase(), WifiPhase::Portal);
    assert_eq!(sim.stored_ssid(), None, "the credentials are gone");
    assert!(sim.snapshot().portal, "the setup network is up");
    // A wipe is not a failure: `GET_WIFI` reads `disconnected` (card 221).
    assert_eq!(
        sim.snapshot().wifi_state,
        screeny_proto::control::wifi_state::DISCONNECTED
    );
    dev.shutdown();
}

/// And the card's other half: four seconds changes nothing at all.
#[test]
fn a_four_second_hold_changes_nothing() {
    let dev = online();
    let sim = dev.handle();
    let before = sim.stored_ssid();

    sim.press_button(4_000);

    assert_eq!(sim.wifi_phase(), WifiPhase::Online, "still on the network");
    assert_eq!(sim.stored_ssid(), before, "and it still knows which one");
    assert!(!sim.snapshot().portal, "no setup network");
    dev.shutdown();
}

/// Even one poll short of five seconds. The recogniser's tests pin the
/// threshold; this pins that the device does nothing with it.
#[test]
fn a_hold_let_go_just_before_the_end_changes_nothing() {
    let dev = online();
    let sim = dev.handle();
    sim.press_button(4_950);
    assert_eq!(sim.wifi_phase(), WifiPhase::Online);
    assert!(sim.stored_ssid().is_some());
    dev.shutdown();
}

/// A short press raises the status/identify overlay, which is an overlay and
/// not a state (spec 7.3) - so the telemetry byte says so.
#[test]
fn a_short_press_raises_the_status_overlay() {
    let dev = online();
    let sim = dev.handle();
    assert_ne!(sim.snapshot().state_byte, state::IDENTIFY);

    sim.press_button(200);

    assert_eq!(sim.snapshot().state_byte, state::IDENTIFY);
    assert_eq!(sim.wifi_phase(), WifiPhase::Online, "and nothing else moved");
    assert!(sim.stored_ssid().is_some());
    dev.shutdown();
}

/// Pressing again during the ten seconds is a second short press, not a
/// second gesture: the overlay is still up afterwards.
#[test]
fn pressing_again_during_the_status_screen_keeps_it_up() {
    let dev = online();
    let sim = dev.handle();
    for _ in 0..4 {
        sim.press_button(150);
        assert_eq!(sim.snapshot().state_byte, state::IDENTIFY);
    }
    dev.shutdown();
}

/// The panel says what happened. After a hold that was let go, it is the
/// "cancelled" screen - the same pixels `screeny_provision` draws - and not
/// the idle screen or the portal.
#[test]
fn a_cancelled_hold_says_so_on_the_panel() {
    let dev = online();
    let sim = dev.handle();

    sim.press_button(4_000);

    let shown = sim.render_display();
    let mut want = [0u8; NBYTES];
    screeny_provision::render(&screeny_provision::Screen::WipeCancelled, &mut want).unwrap();
    assert_eq!(&shown[..], &want[..], "the panel shows 'cancelled'");
    dev.shutdown();
}

/// A wipe takes the panel to the portal screen, which is the whole point of
/// the gesture: the QR to join the setup network is what the author needs next.
#[test]
fn a_wipe_leaves_the_portal_screen_on_the_panel() {
    let dev = online();
    let sim = dev.handle();

    sim.press_button(5_000);

    let shown = sim.render_display();
    // The portal screen's QR block is 31 lit columns down the left; the idle
    // screen and the cancelled screen have nothing like it.
    let lit_top_row = (0..31).filter(|x| shown[x * 3] > 0x80).count();
    assert_eq!(lit_top_row, 31, "the QR's quiet zone is not on the panel");
    assert!(
        (31..W).all(|x| shown[x * 3] < 0x80),
        "and it is the portal layout, not something white"
    );
    dev.shutdown();
}
