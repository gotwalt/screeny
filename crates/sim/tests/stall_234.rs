//! Card 234: the bench sequence that ended in a silent device, in process.
//!
//! On 2026-09-20 firmware 0.5.1 stopped answering, once, ~85-105 s after boot.
//! The order on the bench was: `screeny-probe http` (many short connections and
//! a settings post that commits to the store), the panel released, then
//! `screeny-probe conformance --slow` from a host that had acquired a **second
//! source address** part-way through - while the Studio kept polling
//! `GET /api/v1/status` every ten seconds. **No test had ever run those three
//! things at once**, on either implementation: `conformance.rs` runs the UDP
//! suite alone, `http_conformance.rs` runs the HTTP suite alone, and neither
//! has a second sender in play.
//!
//! This file is that combination, against the simulator, which shares
//! `crates/receiver`, `crates/device-api` and the same "lock the core, copy,
//! unlock, *then* send" shape the firmware has (`crates/sim/src/device.rs`,
//! `firmware/src/net.rs`). What it can prove is that the **shared** half holds
//! up: every port keeps answering, the suites still pass, and the number of
//! unsolicited datagrams one drain can ask the caller to send stays bounded -
//! which is what the firmware's send loop pays for, 200 ms at a time, since
//! this card gave it a deadline.
//!
//! What it cannot prove, and nothing on a host can: the firmware half. The
//! simulator's sockets are `std`'s, non-blocking, and drop a datagram nobody
//! can receive; smoltcp's do not (see `send_bounded` in `firmware/src/net.rs`).
//! The bench procedure in the card's Log is what covers that.
//!
//! Bounded: ephemeral loopback ports, a fixed number of iterations, every wait
//! has a deadline, and the worker threads are joined before the test returns.

mod common;

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use common::http;
use common::Sender;
use screeny_device_api::reply::StatusReply;
use screeny_probe::link::{Control, OwnedReply};
use screeny_probe::suite;
use screeny_proto::control::{op, Request};
use screeny_proto::dec::codec;
use screeny_proto::F_KEY;
use screeny_sim::{Config, SimDevice};

/// The longest any one poll in this file may take before the test calls the
/// device stalled. The simulator answers in single-digit milliseconds; this is
/// two orders of magnitude of slack so that a loaded CI box is not a failure.
const POLL_DEADLINE: Duration = Duration::from_secs(2);

/// What a background poller saw.
///
/// **A single missed poll is not the thing this file is looking for.** A
/// `cargo test` of the whole workspace runs twenty binaries at once, and the
/// simulator's HTTP server drops a connection rather than queueing it when it
/// cannot get a thread (`crates/sim/src/http.rs`, "Out of threads"), so one
/// closed socket under that load says nothing about the device. What said
/// something on the bench was the page going *and staying* silent, so what is
/// asserted here is the longest **run** of consecutive misses.
struct Polled {
    stop: Arc<AtomicBool>,
    ok: Arc<AtomicU32>,
    miss: Arc<AtomicU32>,
    worst_run: Arc<AtomicU32>,
    worst_us: Arc<AtomicU64>,
    handle: thread::JoinHandle<()>,
}

impl Polled {
    /// Poll `GET /api/v1/status` every `every`, the way the Studio does, until
    /// [`Polled::finish`].
    fn start(addr: SocketAddr, every: Duration) -> Polled {
        let stop = Arc::new(AtomicBool::new(false));
        let ok = Arc::new(AtomicU32::new(0));
        let miss = Arc::new(AtomicU32::new(0));
        let worst_run = Arc::new(AtomicU32::new(0));
        let worst_us = Arc::new(AtomicU64::new(0));
        let handle = {
            let (stop, ok, miss, worst_run, worst_us) = (
                stop.clone(),
                ok.clone(),
                miss.clone(),
                worst_run.clone(),
                worst_us.clone(),
            );
            thread::spawn(move || {
                let mut run = 0u32;
                while !stop.load(Ordering::Relaxed) {
                    let t0 = Instant::now();
                    // The test client panics on a transport failure rather
                    // than returning one, and a panic on this thread would be
                    // an unhelpful way to fail.
                    let res = std::panic::catch_unwind(|| http::get(addr, "/api/v1/status"));
                    let took = t0.elapsed();
                    worst_us.fetch_max(took.as_micros() as u64, Ordering::Relaxed);
                    // Parsed with the API's own type, not a `Value`: a 200
                    // carrying something the Studio could not read is not an
                    // answer.
                    let answered = res.is_ok_and(|r| {
                        r.status == 200 && serde_json::from_slice::<StatusReply>(&r.body).is_ok()
                    });
                    if answered {
                        ok.fetch_add(1, Ordering::Relaxed);
                        run = 0;
                    } else {
                        miss.fetch_add(1, Ordering::Relaxed);
                        run += 1;
                        worst_run.fetch_max(run, Ordering::Relaxed);
                    }
                    thread::sleep(every);
                }
            })
        };
        Polled {
            stop,
            ok,
            miss,
            worst_run,
            worst_us,
            handle,
        }
    }

    /// Stop, join, and assert the page never went silent.
    fn finish(self, least: u32, what: &str) {
        self.stop.store(true, Ordering::Relaxed);
        self.handle.join().expect("the poller thread");
        let ok = self.ok.load(Ordering::Relaxed);
        let miss = self.miss.load(Ordering::Relaxed);
        let run = self.worst_run.load(Ordering::Relaxed);
        let worst = Duration::from_micros(self.worst_us.load(Ordering::Relaxed));
        assert!(
            run < 3,
            "{what}: {run} status polls in a row went unanswered ({miss} of {} in all) - \
             the page went silent and stayed silent, which is card 234's symptom",
            ok + miss
        );
        assert!(
            ok >= least,
            "{what}: only {ok} of {} status polls got through",
            ok + miss
        );
        assert!(
            worst < POLL_DEADLINE,
            "{what}: the slowest status poll took {worst:?}, over the {POLL_DEADLINE:?} deadline"
        );
    }
}

/// **The bench order, with the page being read throughout.**
///
/// `screeny-probe http`, then the panel released, then the UDP conformance
/// suite - and a `GET /api/v1/status` every 200 ms for the whole of it, which
/// is fifty times the rate the Studio polls at. Both suites must still pass,
/// and the page must never go silent.
#[test]
fn the_bench_sequence_keeps_every_port_answering() {
    let dev = SimDevice::start(Config::for_test()).expect("bind loopback");
    let api = dev.http_addr().expect("the HTTP API is on by default");
    let poll = Polled::start(api, Duration::from_millis(200));

    // 1. The HTTP suite: many short connections, and the settings posts that
    //    on the device are debounced store commits.
    let http_opts = screeny_probe::http::Opts {
        ctrl_addr: Some(dev.control_addr()),
        ctrlc: false,
        ..screeny_probe::http::Opts::new(api, api.to_string())
    };
    let http_summary = screeny_probe::http::run(&http_opts).expect("the HTTP suite ran");
    assert_eq!(
        http_summary.failed, 0,
        "the HTTP suite failed {} rules under a concurrent poller",
        http_summary.failed
    );

    // 2. The panel is released, exactly as the Studio released it at ~80 s.
    let mut ctrl = Control::connect(dev.control_addr()).expect("control socket");
    ctrl.request(Request::Release).expect("RELEASE answered");

    // 3. The UDP suite, over the top of the same poller.
    let udp_opts = suite::Opts {
        frame_addr: dev.frame_addr(),
        ctrl_addr: dev.control_addr(),
        only: None,
        // The two HOLD_MS rules are 13 s each of waiting; `core_rules.rs`
        // pins that transition on a virtual clock. `conformance.rs` makes the
        // same trade for the same reason.
        slow: false,
        cap_probe: false,
        restore_idle: 0,
    };
    let udp_summary = suite::run(&udp_opts).expect("the UDP suite ran");
    assert_eq!(
        udp_summary.failed, 0,
        "the UDP suite failed {} rules under a concurrent poller",
        udp_summary.failed
    );
    assert!(
        udp_summary.passed >= 60,
        "only {} UDP rules ran; the catalogue has shrunk",
        udp_summary.passed
    );

    poll.finish(20, "the bench sequence");
    dev.shutdown();
}

/// **Two sources, control traffic and the page, all at once.**
///
/// The half of the bench state the suites never reproduce: one sender holding
/// the lock while a second is refused, which is the only thing that makes the
/// device emit *unsolicited* datagrams (`BUSY`, section 6.2) - the ones the
/// firmware's frame task has to push out of a four-slot transmit queue before
/// it can go round its loop again. Everything must stay answered, and the
/// locked-out sender must be rate-limited rather than answered per frame.
#[test]
fn a_locked_out_second_source_does_not_stall_the_other_ports() {
    let dev = SimDevice::start(Config::for_test()).expect("bind loopback");
    let api = dev.http_addr().expect("the HTTP API is on by default");
    let poll = Polled::start(api, Duration::from_millis(200));

    let holder_frames = 90u32; // three seconds at 30 fps
    let (frame, _) = screeny_probe::enc::solid([8, 8, 8]);

    // **Settle which source holds the lock before either thread starts.**
    // Two threads racing for it is a coin flip, and the loser is the one this
    // test wants to watch.
    let mut a = Sender::new(dev.frame_addr());
    let b = Sender::new(dev.frame_addr());
    a.send(codec::SOLID, F_KEY, &frame);
    let live = dev
        .handle()
        .wait_until(Duration::from_secs(5), |s| s.active_source == Some(a.addr()))
        .expect("the first sender took the lock");
    assert_eq!(live.active_source, Some(a.addr()));

    let holder = {
        let payload = frame.clone();
        thread::spawn(move || {
            let mut a = a;
            for _ in 0..holder_frames {
                a.send(codec::SOLID, F_KEY, &payload);
                thread::sleep(Duration::from_millis(33));
            }
            a
        })
    };
    // The source that is refused, sending just as hard.
    let refused = {
        let payload = frame.clone();
        thread::spawn(move || {
            let mut b = b;
            for _ in 0..holder_frames {
                b.send(codec::SOLID, F_KEY, &payload);
                thread::sleep(Duration::from_millis(33));
            }
            b
        })
    };

    // The control port, asked for telemetry throughout. `request` retries and
    // then gives up, so an unanswered port is an `Err` here and not a hang.
    let mut ctrl = Control::connect(dev.control_addr()).expect("control socket");
    let mut telemetries = 0u32;
    let deadline = Instant::now() + Duration::from_secs(6);
    while !holder.is_finished() && Instant::now() < deadline {
        match ctrl.request(Request::Telemetry) {
            Ok(OwnedReply::Telemetry(_)) => telemetries += 1,
            Ok(other) => panic!("expected telemetry, got {other:?}"),
            Err(e) => panic!("the control port stopped answering under load: {e}"),
        }
        thread::sleep(Duration::from_millis(50));
    }
    let _holder = holder.join().expect("the holding sender");
    let refused = refused.join().expect("the refused sender");

    assert!(
        telemetries >= 20,
        "only {telemetries} telemetry replies in six seconds"
    );

    // Section 6.2: a locked-out source is told, and told no more often than
    // `BUSY_MIN_INTERVAL_MS`. That bound is the whole reason the firmware's
    // send loop is short enough to give each datagram a deadline.
    let back = refused.drain();
    assert!(
        !back.is_empty(),
        "the refused source was never sent a BUSY, so section 6.2 was not exercised"
    );
    let busies = back
        .iter()
        .filter(|b| b.len() >= 3 && b[2] == op::BUSY)
        .count();
    assert!(busies > 0, "nothing that came back was a BUSY: {back:?}");
    // One per second at the outside, over three seconds of being refused
    // thirty times a second, plus slack for the drain's boundaries.
    let ceiling = (3_000 / screeny_proto::BUSY_MIN_INTERVAL_MS + 2) as usize;
    assert!(
        busies <= ceiling,
        "{busies} BUSY packets for one refused source in three seconds; \
         BUSY_MIN_INTERVAL_MS is {} ms, so at most {ceiling} were due",
        screeny_proto::BUSY_MIN_INTERVAL_MS
    );

    poll.finish(10, "two sources");
    dev.shutdown();
}
