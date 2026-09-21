//! Card 246, item 2: `fw-upload --activate` must not decide before the device
//! has gone away.
//!
//! The bench failure this pins is a race, so it needs a device with a clock:
//! `SimHandle::model_ota` plays the four phases a real activation has - the
//! old image still answering, nothing answering, a new `boot_id` on trial, and
//! confirmed - and `screeny_probe::http::watch` is driven against it. With
//! firmware 0.7.0's logic the first phase alone is enough to make the probe
//! declare victory; with the `boot_id` rule it cannot.
//!
//! Everything is in-process and on loopback, and the timings are tenths of a
//! second, because what is being tested is an order of events.

mod common;

use std::time::{Duration, Instant};

use screeny_device_api::route;
use screeny_probe::http::update::{self, Outcome, Watch};
use screeny_probe::http::Client;
use screeny_sim::{Config, OtaTiming, SimDevice};

/// A real ESP32 image of this project, from the crate that validates one, with
/// a version a test can recognise on the other side of the update.
fn image() -> Vec<u8> {
    screeny_fwimage::build::Builder {
        version: "0.7.2",
        ..screeny_fwimage::build::Builder::good()
    }
    .build()
}

/// Tenths of a second, in the same order and proportion as a device's
/// two seconds / fifteen seconds / sixty seconds.
const FAST: OtaTiming = OtaTiming {
    old_image_for: Duration::from_millis(400),
    away_for: Duration::from_millis(400),
    trial_for: Duration::from_millis(600),
};

/// Patience in the same units, so a run of this file is a couple of seconds
/// and still bounded by something other than the test harness.
fn watch_fast() -> Watch {
    Watch {
        reappear: Duration::from_secs(10),
        decide: Duration::from_secs(20),
        poll: Duration::from_millis(50),
        no_record_grace: Duration::from_millis(500),
    }
}

fn device() -> SimDevice {
    SimDevice::start(Config::for_test()).expect("bind loopback")
}

fn client(dev: &SimDevice) -> Client {
    Client::new(
        dev.http_addr().expect("the HTTP API is on by default"),
        "127.0.0.1",
    )
    .with_timeout(Duration::from_secs(2))
}

#[test]
fn the_wait_follows_the_update_through_the_reboot_it_did_not_see() {
    let dev = device();
    dev.handle().model_ota(Some(FAST));
    let http = client(&dev);

    let before = update::boot_id(&http).expect("a status before the upload");
    let res = http.post_bytes(route::FIRMWARE, &image()).expect("upload");
    assert_eq!(res.status, 200, "{}", res.snippet(200));
    let reply: screeny_device_api::reply::FirmwareReply = res.parse().expect("a firmware reply");
    assert!(reply.ok, "{reply:?}");
    assert!(
        reply.activating,
        "with the model on, an activating upload really does start something"
    );

    // **The moment the bench got wrong**: the device is still the old image,
    // answering with the old boot_id, and it has no update record.
    let now = http.get(route::STATUS).expect("the old image still answers");
    let s: screeny_device_api::reply::StatusReply = now.parse().expect("a status");
    assert_eq!(s.boot_id, before, "the old image is still running");
    let p: screeny_device_api::reply::PanicReply = http
        .get(route::PANIC)
        .expect("panic")
        .parse()
        .expect("a panic reply");
    assert!(
        p.update.is_none(),
        "the old image has no record of an update that has not started"
    );

    let mut lines: Vec<String> = Vec::new();
    let t0 = Instant::now();
    let outcome = update::watch(&http, Some(before), &watch_fast(), &mut |l| lines.push(l));
    let transcript = lines.join("\n");

    assert_eq!(outcome, Outcome::Confirmed, "transcript:\n{transcript}");
    assert!(
        outcome.ok(),
        "a confirmed update is a zero exit: {transcript}"
    );
    // It waited for the reboot rather than through it.
    assert!(
        t0.elapsed() >= FAST.old_image_for + FAST.away_for,
        "the wait returned before the device had even come back: {transcript}"
    );
    assert!(
        transcript.contains("the old image, which has not restarted yet"),
        "it should say it saw the old image: {transcript}"
    );
    assert!(
        transcript.contains("0.7.2"),
        "the version it came back with is the one that was uploaded: {transcript}"
    );
    assert!(
        !transcript.contains("no update record"),
        "the record was there; this is the line 0.7.0 printed by mistake: {transcript}"
    );
}

#[test]
fn a_device_that_never_restarts_is_reported_and_not_waited_for_ever() {
    let dev = device();
    // The model is off: nothing activates, the boot_id never changes. That is
    // also what a device whose `otadata` write failed looks like from here.
    let http = client(&dev);
    let before = update::boot_id(&http).expect("a status");

    let mut lines: Vec<String> = Vec::new();
    let w = Watch {
        reappear: Duration::from_millis(600),
        ..watch_fast()
    };
    let t0 = Instant::now();
    let outcome = update::watch(&http, Some(before), &w, &mut |l| lines.push(l));
    let transcript = lines.join("\n");

    assert_eq!(outcome, Outcome::NeverCameBack, "{transcript}");
    assert!(!outcome.ok(), "that is not a success");
    assert!(
        t0.elapsed() < Duration::from_secs(5),
        "it is bounded by `reappear`, not by the trial deadline"
    );
}

#[test]
fn a_firmware_with_no_update_record_is_not_a_failure_but_is_not_decided_early() {
    // A device that restarts - so the `boot_id` really does change - and has
    // no `update` object at all, which is every firmware before 0.7.0. The
    // simulator answers `update: null` with the model off, and `POST
    // /api/v1/reboot` is what draws a new `boot_id`.
    let dev = device();
    let http = client(&dev);
    let before = update::boot_id(&http).expect("a status");
    let res = http
        .post_json(route::REBOOT, r#"{"confirm":"RBOO"}"#)
        .expect("reboot");
    assert_eq!(res.status, 200, "{}", res.snippet(120));

    let mut lines: Vec<String> = Vec::new();
    let w = watch_fast();
    let t0 = Instant::now();
    let outcome = update::watch(&http, Some(before), &w, &mut |l| lines.push(l));
    let transcript = lines.join("\n");

    assert_eq!(outcome, Outcome::NoRecord, "{transcript}");
    assert!(outcome.ok(), "not knowing is not a failure");
    assert!(
        t0.elapsed() >= w.no_record_grace,
        "it gave the new image time to write its record before concluding: {transcript}"
    );
}
