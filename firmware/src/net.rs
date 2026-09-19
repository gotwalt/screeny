//! The two UDP tasks, and the one thing they share.
//!
//! [`frames_task`] owns the frame socket, the [`Core`] state machine's clock,
//! and the panel: it is the only writer to the display's triple buffer, which
//! is why the idle screen and the `IDENTIFY` overlay are composed here rather
//! than in a task of their own. Card 007's firmware had a separate `status`
//! task racing the stream for one frame mutex; with a state machine that knows
//! whether a source holds the lock, there is nothing left to race about — the
//! state machine simply says what to draw.
//!
//! [`control_task`] owns the control socket and nothing else.
//!
//! Both reach the state machine through one `CORE` mutex. They are both on
//! core 0, so the mutex is only ever contended between two tasks on one
//! executor and its critical sections are a few microseconds; the display on
//! core 1 never touches it.

use core::sync::atomic::Ordering;
use core::task::{Context, Poll, Waker};

use embassy_futures::select::{select, Either};
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{IpEndpoint, Stack};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer};
use log::{info, warn};

use crate::display::Frame;
use crate::fb::Producer;
use crate::receiver::{Core, Intent, Offer, Outbox};
use crate::screens::{self, Net};
use crate::{mk_static, CONTROL_PORT, FRAME_PORT, MAX_DATAGRAM};

/// The receive state machine, shared by the two tasks on core 0.
pub static CORE: Mutex<CriticalSectionRawMutex, Option<Core>> = Mutex::new(None);

/// Raised when `SET_NAME` changed the TXT record, so mDNS re-announces.
pub static INFO_CHANGED: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// How often the frame task wakes when nothing is arriving. Fast enough that
/// `STREAM_TIMEOUT_MS`, `HOLD_MS` and the 500 ms cross-fade all land within a
/// frame time of where the spec puts them, and cheap enough to ignore.
const TICK: Duration = Duration::from_millis(20);

/// How often the idle screen's ambient animation advances.
const ANIM_MS: u64 = 100;

fn now_us() -> u64 {
    Instant::now().as_micros()
}

/// What the network looks like from here, for the status screen.
fn net_state(stack: Stack<'static>, had_address: &mut bool) -> Net {
    if let Some(cfg) = stack.config_v4() {
        *had_address = true;
        Net::Address(cfg.address.address().octets())
    } else if stack.is_link_up() {
        Net::Associated
    } else if *had_address {
        Net::Lost
    } else {
        Net::Joining
    }
}

/// One non-blocking receive, for the "drain until it would block" of §3.3.
enum Drained {
    Got(usize, IpEndpoint),
    /// The datagram did not fit in a 1472-byte buffer, so it is longer than
    /// the protocol allows (§1) and the bytes we have are not the bytes that
    /// were sent. Count it; never parse the prefix.
    Oversize,
    Empty,
}

fn try_recv(socket: &UdpSocket<'_>, buf: &mut [u8]) -> Drained {
    // A no-op waker is safe here only because every `Pending` this produces is
    // followed, in the loop below, by an `await` on `recv_from`, which
    // registers the real waker before the task sleeps. smoltcp keeps one
    // receive waker per socket and replaces it, so nothing is lost.
    let mut cx = Context::from_waker(Waker::noop());
    match socket.poll_recv_from(buf, &mut cx) {
        Poll::Ready(Ok((n, meta))) => Drained::Got(n, meta.endpoint),
        Poll::Ready(Err(_)) => Drained::Oversize,
        Poll::Pending => Drained::Empty,
    }
}

// ---------------------------------------------------------------------------
// Frames
// ---------------------------------------------------------------------------

/// UDP 49374: drain, validate, newest wins, decode, display (spec §3.3).
#[embassy_executor::task]
pub async fn frames_task(
    stack: Stack<'static>,
    mut producer: Producer,
    hostname: &'static str,
) {
    // The socket's receive buffer is deliberately shallow (§3.3): four packet
    // slots, so that when the decoder falls behind the stack drops packets
    // instead of accumulating latency. esp-radio already queues up to
    // `rx_queue_size` frames below this.
    let rx_meta = mk_static!([PacketMetadata; 4], [PacketMetadata::EMPTY; 4]);
    let rx_buf = mk_static!([u8; 4 * MAX_DATAGRAM], [0u8; 4 * MAX_DATAGRAM]);
    let tx_meta = mk_static!([PacketMetadata; 4], [PacketMetadata::EMPTY; 4]);
    let tx_buf = mk_static!([u8; 2 * MAX_DATAGRAM], [0u8; 2 * MAX_DATAGRAM]);
    let mut socket = UdpSocket::new(stack, rx_meta, rx_buf, tx_meta, tx_buf);
    socket.bind(FRAME_PORT).expect("bind frame port");
    info!("net: frames on udp/{}", FRAME_PORT);

    // Two datagram buffers and no copying: `rx` is where the socket writes,
    // `keep` holds the drain's survivor, and accepting a frame swaps the two.
    let mut rx: &'static mut [u8; MAX_DATAGRAM] = mk_static!([u8; MAX_DATAGRAM], [0u8; MAX_DATAGRAM]);
    let mut keep: &'static mut [u8; MAX_DATAGRAM] =
        mk_static!([u8; MAX_DATAGRAM], [0u8; MAX_DATAGRAM]);

    // The last frame a sender put on the panel. Only the cross-fade into the
    // idle screen reads it, but it has to be captured while it is current.
    let last: &'static mut Frame = mk_static!(Frame, Frame::new());

    let mut out = Outbox::new();
    let mut had_address = false;
    let mut redraw_seen = 0u32;
    let mut anim_at_ms = 0u64;
    let mut phase = 0u32;
    let mut link_was_up = true;

    loop {
        let mut keep_len = 0usize;
        let mut have_keep = false;
        out.clear();

        // --- wait for work, without holding the lock --------------------
        let first = match select(socket.recv_from(&mut rx[..]), Timer::after(TICK)).await {
            Either::First(r) => Some(r),
            Either::Second(()) => None,
        };

        let mut guard = CORE.lock().await;
        let core = guard.as_mut().expect("core built before tasks spawn");

        // --- step 1: drain the socket, newest wins ----------------------
        if let Some(r) = first {
            match r {
                Ok((n, meta)) => {
                    if core.offer_frame(now_us(), meta.endpoint, &rx[..n], &mut out) == Offer::Keep
                    {
                        core::mem::swap(&mut rx, &mut keep);
                        keep_len = n;
                        have_keep = true;
                    }
                }
                Err(_) => core.count_oversize(),
            }
        }
        loop {
            match try_recv(&socket, &mut rx[..]) {
                Drained::Got(n, from) => {
                    if core.offer_frame(now_us(), from, &rx[..n], &mut out) == Offer::Keep {
                        core::mem::swap(&mut rx, &mut keep);
                        keep_len = n;
                        have_keep = true;
                    }
                }
                Drained::Oversize => core.count_oversize(),
                Drained::Empty => break,
            }
        }

        // --- steps 2 and 3: decode the survivor, then swap --------------
        let survivor = have_keep.then(|| &keep[..keep_len]);
        let published = core.flush_frames(now_us(), survivor, &mut producer.back().px, &mut out);
        if published {
            // The cross-fade needs the frame a sender last put up, and this is
            // the only moment it is reachable: after `publish` the slot
            // belongs to the consumer. 6 KB at 30 fps is 0.05% of a core.
            last.copy_from(producer.back());
            producer.publish();
        }

        // --- timers -----------------------------------------------------
        let now = now_us();
        let link_up = stack.is_link_up();
        if link_was_up && !link_up {
            core.link_down(now);
        }
        link_was_up = link_up;
        core.tick(now);

        // --- compose whatever is not a streamed frame -------------------
        let net = net_state(stack, &mut had_address);
        let intent = core.intent(now);
        let now_ms = now / 1_000;
        let animating = matches!(intent, Intent::Identify | Intent::Fade { .. });
        let due = animating
            || core.redraw != redraw_seen
            || (intent == Intent::Idle && now_ms.wrapping_sub(anim_at_ms) >= ANIM_MS);

        if due {
            redraw_seen = core.redraw;
            anim_at_ms = now_ms;
            phase = phase.wrapping_add(1);
            let hold = crate::PATTERN_HOLD.load(Ordering::Relaxed);
            let mut drew = true;
            if hold != 0 {
                // Bench only: a held test pattern outranks everything, so a
                // test card stays up while something else is still sending.
                if let Some(p) = crate::patterns::Pattern::from_u8(hold - 1) {
                    crate::patterns::draw(producer.back(), p);
                } else {
                    drew = false;
                }
            } else {
                match intent {
                    Intent::Stream => drew = false,
                    Intent::Identify => {
                        screens::identify(producer.back(), core.name(), net, phase)
                    }
                    Intent::Idle => core.draw_idle(producer.back(), last, hostname, net, phase),
                    Intent::Fade { t } => {
                        core.draw_idle(producer.back(), last, hostname, net, phase);
                        // In place, so the fade needs no third buffer:
                        // back = lerp(last, target, t).
                        let dst = producer.back();
                        let t = t.min(256) as u32;
                        for i in 0..dst.px.len() {
                            let a = last.px[i] as u32;
                            let b = dst.px[i] as u32;
                            dst.px[i] = ((a * (256 - t) + b * t + 128) >> 8) as u8;
                        }
                    }
                }
            }
            if drew {
                producer.publish();
            }
        }

        let info_changed = core.take_info_changed();
        drop(guard);

        if info_changed {
            INFO_CHANGED.signal(());
        }
        // Section 6.2: both unsolicited packets leave by the frame socket,
        // addressed to the frame datagram's source address and port.
        for o in out.iter() {
            if socket.send_to(o.bytes(), o.to).await.is_err() {
                warn!("net: frame-socket send to {} failed", o.to);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Control
// ---------------------------------------------------------------------------

/// UDP 49375: every opcode in spec section 6.3.
#[embassy_executor::task]
pub async fn control_task(stack: Stack<'static>) {
    let rx_meta = mk_static!([PacketMetadata; 4], [PacketMetadata::EMPTY; 4]);
    let rx_buf = mk_static!([u8; 4 * 512], [0u8; 4 * 512]);
    let tx_meta = mk_static!([PacketMetadata; 4], [PacketMetadata::EMPTY; 4]);
    let tx_buf = mk_static!([u8; 1024], [0u8; 1024]);
    let mut socket = UdpSocket::new(stack, rx_meta, rx_buf, tx_meta, tx_buf);
    socket.bind(CONTROL_PORT).expect("bind control port");
    info!("net: control on udp/{}", CONTROL_PORT);

    let buf = mk_static!([u8; 512], [0u8; 512]);
    let reply = mk_static!([u8; 512], [0u8; 512]);

    loop {
        let (n, meta) = match socket.recv_from(&mut buf[..]).await {
            Ok(v) => v,
            // A truncated control datagram has no usable header we can trust.
            // Unlike the frame port there is nothing to count: section 2.2
            // says a datagram discarded on the control port is counted
            // nowhere, because folding control-port noise into a counter a
            // sender reads to diagnose its *video* stream would ruin it.
            Err(_) => continue,
        };
        let mut guard = CORE.lock().await;
        let core = guard.as_mut().expect("core built before tasks spawn");
        let len = core.control(now_us(), meta.endpoint, &buf[..n], &mut reply[..]);
        let reboot = core.reboot_pending;
        let info_changed = core.take_info_changed();
        drop(guard);

        if info_changed {
            INFO_CHANGED.signal(());
        }
        if let Some(len) = len {
            if socket.send_to(&reply[..len], meta.endpoint).await.is_err() {
                warn!("net: control reply to {} failed", meta.endpoint);
            }
        }
        if reboot {
            // Section 6.3: the reply goes out before the reboot. Give the
            // stack a tick to actually put it on the air.
            info!("control: REBOOT");
            Timer::after(Duration::from_millis(100)).await;
            esp_hal::system::software_reset();
        }
    }
}
