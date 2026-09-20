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
use embassy_time::{with_timeout, Duration, Instant, Timer};
use log::{info, warn};

use crate::display::Frame;
use crate::fb::Producer;
use crate::receiver::{Core, Intent, Offer, Outbox};
use crate::screens::{self, Net};
use crate::store;
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

/// How often the portal screen is recomposed while it is up.
///
/// Its content only changes when the machine's state does (the layouts no
/// longer alternate: a QR that keeps leaving the panel does not scan), so
/// anything faster is wasted work - and this one is not free: it re-encodes a
/// version 2-L QR each time. 250 ms keeps the change to the `connected`
/// screen looking instant without putting a QR encode inside every 20 ms tick.
const PORTAL_MS: u64 = 250;

/// The frame socket's **transmit** buffer, and card 223's RAM lever.
///
/// It was `2 * MAX_DATAGRAM` (2,944 bytes) from card 008 to card 223, by
/// symmetry with the receive side - and the symmetry was false. The frame port
/// *receives* datagrams of up to [`MAX_DATAGRAM`]; what it **sends** is only
/// ever one of section 6.2's unsolicited replies, and the largest of those is
/// the `TELEMETRY` of section 6.4/6.7: an 8-byte control header and a 48-byte
/// body, 56 bytes on the wire. `BUSY` is shorter still. The shared receive core
/// enforces that from the other end: every item in its [`Outbox`] is an
/// [`screeny_receiver::OUT_MAX`]-byte buffer (64), and the outbox holds four,
/// so **256 bytes is the most that can ever be queued here at once**.
///
/// 512 is that with the queue counted twice over, and it is still a
/// 2,432-byte refund to core 0's stack - which is what card 223's soft-AP,
/// DHCP server, DNS catch-all and second network stack are spent on. The four
/// `tx_meta` slots are unchanged: they, not the byte count, are what bounds the
/// number of datagrams in flight.
const FRAME_TX_BUF: usize = 8 * screeny_receiver::OUT_MAX;
const _: () = assert!(FRAME_TX_BUF >= 4 * screeny_receiver::OUT_MAX);

fn now_us() -> u64 {
    Instant::now().as_micros()
}

/// What the network looks like from here, for the status screen.
fn net_state(stack: Stack<'static>, had_address: &mut bool) -> Net {
    // The HTTP handlers want the same answer and cannot hold a `Stack` in a
    // `static` (it is not `Sync`), so this tick publishes it for them.
    crate::http::set_ipv4(stack.config_v4().map(|c| c.address.address().octets()));
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

/// How long one datagram is given to reach a socket's transmit queue.
///
/// Generous: what these two sockets send is a handful of 56-byte datagrams a
/// second, and the queue is four slots deep, so under any healthy condition
/// [`send_bounded`] returns on its first poll.
const SEND_TIMEOUT: Duration = Duration::from_millis(200);

/// Consecutive timeouts on one socket before it is closed and re-bound.
const SEND_TIMEOUTS_BEFORE_REBIND: u32 = 3;

/// Queue one datagram, and never wait forever for it (card 234).
///
/// `UdpSocket::send_to` waits for room in the transmit queue, and **smoltcp
/// will not make room by itself**. A datagram whose destination never answers
/// ARP is not discarded: `udp::Socket::dispatch` hands it to the interface,
/// the interface cannot resolve the hardware address and returns
/// `DispatchError::NeighborPending`, and `dequeue_with` consumes *zero* bytes
/// on an error - so the datagram stays at the head of the queue and is retried
/// once a second, forever (smoltcp 0.13.1, `socket/udp.rs` and
/// `iface/interface/mod.rs`'s `EgressError::Dispatch`). Four of those fill
/// `tx_meta` and the bare `.await` these two tasks used to do never returns
/// again - on the control port that is every reply to every peer, silently,
/// until a reset.
///
/// A timeout is not a lost MUST. Both datagrams the frame socket sends are
/// *unsolicited* (section 6.2), and a control reply is something a sender
/// retries - `screeny-probe` tries four times. After
/// [`SEND_TIMEOUTS_BEFORE_REBIND`] in a row the socket is closed and re-bound,
/// which resets both buffers and is the only way to drop a wedged head-of-line
/// datagram; the port answers again on the next request.
async fn send_bounded(
    socket: &mut UdpSocket<'_>,
    port: u16,
    what: &str,
    stuck: &mut u32,
    bytes: &[u8],
    to: IpEndpoint,
) {
    // Bound to a `let`, not a `match` scrutinee: the send future borrows the
    // socket, and the re-bind below needs it back.
    let r = with_timeout(SEND_TIMEOUT, socket.send_to(bytes, to)).await;
    match r {
        Ok(Ok(())) => *stuck = 0,
        Ok(Err(e)) => warn!("net: {} send to {} failed: {:?}", what, to, e),
        Err(_) => {
            *stuck += 1;
            warn!(
                "net: {} send to {} did not fit the transmit queue in {} ms ({} in a row)",
                what,
                to,
                SEND_TIMEOUT.as_millis(),
                *stuck
            );
            if *stuck >= SEND_TIMEOUTS_BEFORE_REBIND {
                *stuck = 0;
                socket.close();
                match socket.bind(port) {
                    Ok(()) => warn!("net: udp/{} was wedged; closed and re-bound", port),
                    Err(e) => warn!("net: udp/{} could not be re-bound: {:?}", port, e),
                }
            }
        }
    }
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
    // See [`FRAME_TX_BUF`]: this socket sends telemetry replies, not frames.
    let tx_buf = mk_static!([u8; FRAME_TX_BUF], [0u8; FRAME_TX_BUF]);
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
    let mut portal_at_ms = 0u64;
    let mut phase = 0u32;
    let mut link_was_up = true;
    let mut stuck_sends = 0u32;
    let mut ota_was = false;

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
        // Card 223's screens (see "compose" below). Asked for here because the
        // setup screen is an overlay in the full sense: while it is up a
        // decoded frame is counted and kept for the cross-fade but does **not**
        // reach the panel.
        let portal = crate::provision::screen((now_us() / 1_000) as u32);
        // Card 240. A firmware update outranks everything, including a live
        // stream: `docs/design/device-web.md` decision 7 says the frame path
        // is the product and nothing may take the panel from a sender -
        // "except a firmware update, which is allowed to take the panel over
        // with an 'updating' screen". So it is tested *before* the portal's
        // own screen and before `intent`, and it is the only thing in this
        // task that can be up while a sender is streaming.
        // Card 241 makes this two screens rather than one: the progress bar
        // while the bytes arrive, and `installing` for the two seconds between
        // the reply and the restart. One call, because they are mutually
        // exclusive and the second outranks the first.
        let ota = crate::ota::panel();
        let setup_screen_up =
            ota.is_some() || matches!(portal, Some(crate::provision::PanelScreen::Portal { .. }));
        if published {
            // The cross-fade needs the frame a sender last put up, and this is
            // the only moment it is reachable: after `publish` the slot
            // belongs to the consumer. 6 KB at 30 fps is 0.05% of a core.
            last.copy_from(producer.back());
            if !setup_screen_up {
                producer.publish();
            }
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
        // Card 223: the portal screen and the "connected, I am at x.y.z.w"
        // screen. **An overlay, like `IDENTIFY`** - frames from a sender on
        // the LAN are still drained, decoded and counted underneath; what
        // changes is only what reaches the panel. *Whether* there is one, and
        // which of the two portal layouts this instant wants, is the machine's
        // answer (`Provisioner::screen`); this task supplies the clock and the
        // frame, exactly as it does for every other screen.
        //
        // **The `connected` screen yields to a stream.** It is up for a minute
        // after a join from the portal, and the Studio finds the panel within
        // seconds of that join: both used to publish, and the panel showed the
        // address every few frames of the art (the owner's phone test,
        // 2026-09-20). The phone's page has the address too, and a panel that
        // is being sent a picture shows the picture.
        let portal = portal.filter(|s| {
            !(matches!(s, crate::provision::PanelScreen::Connected { .. })
                && intent == Intent::Stream)
        });
        let portal_due = (portal.is_some() || ota.is_some())
            && now_ms.wrapping_sub(portal_at_ms) >= PORTAL_MS;
        // An update ending has to redraw once even if nothing else is due:
        // until it does, the panel is still showing the progress bar of an
        // upload that finished.
        let ota_edge = ota.is_some() != ota_was;
        ota_was = ota.is_some();
        let due = animating
            || portal_due
            || ota_edge
            || core.redraw() != redraw_seen
            || (intent == Intent::Idle && now_ms.wrapping_sub(anim_at_ms) >= ANIM_MS);

        if due {
            redraw_seen = core.redraw();
            anim_at_ms = now_ms;
            phase = phase.wrapping_add(1);
            let hold = crate::PATTERN_HOLD.load(Ordering::Relaxed);
            let mut drew = true;
            if let Some(what) = ota {
                // Drawn by `crates/provision`, like the portal screens, so
                // the simulator and the device draw the same thing. Dither is
                // already off - `crate::ota::Upload` turned it off when it
                // took the claim - so core 1 sleeps between refreshes and the
                // 50 ms stalls around each sector erase cost nothing.
                portal_at_ms = now_ms;
                crate::provision::render_updating(what, &mut producer.back().px);
            } else if let Some(s) = portal.as_ref() {
                // Drawn with nothing locked: `provision::screen` copied the
                // name out of the machine and released it, because a QR encode
                // inside a critical section would mask core 1's HUB75 DMA
                // interrupt for the whole of it.
                portal_at_ms = now_ms;
                crate::provision::render(s, &mut producer.back().px);
            } else if hold != 0 {
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
            send_bounded(
                &mut socket,
                FRAME_PORT,
                "frame-socket",
                &mut stuck_sends,
                o.bytes(),
                o.to,
            )
            .await;
        }
    }
}

// ---------------------------------------------------------------------------
// Control
// ---------------------------------------------------------------------------

/// Rewrite a reply as `ERR_STORAGE` (spec section 6.5).
///
/// The opcode and `req_id` are re-read from the request rather than threaded
/// out of the handler: they are bytes 2 and 4-5 of a datagram that has already
/// parsed once, so this cannot answer the wrong request, and it keeps the
/// error path out of the shared `screeny-receiver` crate. `None` when the
/// request had no `req_id` and therefore wanted no reply at all (section 6.1).
fn err_storage(request: &[u8], out: &mut [u8]) -> Option<usize> {
    let pkt = screeny_proto::ControlPacket::parse(request).ok()?;
    if pkt.req_id == 0 {
        return None;
    }
    screeny_proto::control::Reply::Err {
        code: screeny_proto::control::ErrorCode::Storage.as_u8(),
    }
    .write(pkt.op, pkt.req_id, out)
    .ok()
}

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
    let mut stuck_sends = 0u32;

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
        let mut immediate: Option<store::Immediate> = None;
        let mut guard = CORE.lock().await;
        let core = guard.as_mut().expect("core built before tasks spawn");
        let mut len = core.control(
            now_us(),
            meta.endpoint,
            &buf[..n],
            &mut reply[..],
            &mut immediate,
        );
        let reboot = core.reboot_pending();
        let info_changed = core.take_info_changed();
        drop(guard);

        // Spec section 6.5: `ERR_STORAGE` is only an honest answer for a write
        // that has already happened, so `SET_NAME` and `SET_WIFI` are written
        // here, with the reply still in the buffer and the CORE lock already
        // released. The debounced settings cannot do this and do not try; their
        // failures are counted in `store::FAILURES` instead.
        let mut wifi_to_try = None;
        if let Some(what) = immediate {
            match what {
                // Credentials are **not** written here. They are handed to the
                // WiFi task, which stores them only once they have joined
                // (`crate::NewWifi`): writing first let one bad `SET_WIFI`
                // replace a working pair in flash. So `SET_WIFI` can no longer
                // answer `ERR_STORAGE`; a failed write after a good join is
                // counted in `store::FAILURES` instead.
                store::Immediate::Wifi { wifi, persist } => {
                    wifi_to_try = Some(crate::NewWifi { wifi, persist });
                }
                other => {
                    if let Err(e) = store::commit_immediate(&other).await {
                        warn!("control: storing the setting failed: {:?} -> ERR_STORAGE", e);
                        len = err_storage(&buf[..n], &mut reply[..]).or(len);
                    }
                }
            }
        }

        if info_changed {
            INFO_CHANGED.signal(());
        }
        if let Some(len) = len {
            send_bounded(
                &mut socket,
                CONTROL_PORT,
                "control reply",
                &mut stuck_sends,
                &reply[..len],
                meta.endpoint,
            )
            .await;
        }
        // Section 8.2: **after** the reply is on the air, because after the
        // disconnect it could not be sent. `send_to` returning only means the
        // datagram is queued, so give the stack the same tick `REBOOT` gets
        // below; without it the bench saw "no reply to op 0x0b" every time.
        if let Some(w) = wifi_to_try {
            Timer::after(Duration::from_millis(100)).await;
            crate::NEW_WIFI.signal(w);
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
