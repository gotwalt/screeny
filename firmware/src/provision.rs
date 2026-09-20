//! The setup portal: the radio, the soft-AP and its services, and the one
//! [`Provisioner`] everything else reads (card 223).
//!
//! ## Who decides what
//!
//! **Every rule about joining, retrying, the portal and the trial lives in
//! `crates/provision` and nowhere here.** This module is the half that crate
//! deliberately does not have: a clock, a radio, a flash store, a network
//! stack and a panel. It turns radio results and timers into
//! [`Event`]s, hands them to [`Provisioner::step`], and carries out the
//! [`Action`]s that come back. The simulator (`crates/sim/src/wifi.rs`) does
//! exactly the same thing with a scripted radio, which is what stops the two
//! from drifting apart.
//!
//! ```text
//!   radio / DHCP / timer / POST ──▶ Event ──▶ Provisioner ──▶ Action ──▶ radio,
//!                                                 │                      store,
//!                                                 │                      AP services,
//!                                                 ▼                      mDNS
//!                                    wifi_state(), trial(), screen(),
//!                                    overlay_state(), ap_up()
//!                                          │
//!                        http.rs, receiver.rs (GET_WIFI), net.rs (the panel)
//! ```
//!
//! ## The machine is shared, synchronously
//!
//! [`MACHINE`] is a blocking critical-section mutex, not an async one, because
//! [`crate::receiver::Host::wifi`] is a synchronous trait method that must
//! answer `GET_WIFI` without awaiting anything. Every reader takes it for the
//! few microseconds it needs to copy a value out; **nothing is rendered,
//! awaited or formatted while it is held**, because a critical section masks
//! the interrupts core 1's HUB75 DMA runs on. [`screen`] therefore copies the
//! machine's answer into an owned [`PanelScreen`] and the caller draws it with
//! the lock released.
//!
//! ## What the radio costs, and the one borrow that could not be had
//!
//! `esp-radio`'s access-point client events come from
//! `wait_for_access_point_connected_event_async(&self)`, and a join needs
//! `connect_async(&mut self)`. The two cannot be awaited together, so while a
//! join attempt is in flight the AP's associate/leave events are not observed.
//! That is a window of at most one attempt (15 s). It costs nothing where it
//! matters: the only rule that reads [`Provisioner::ap_clients`] is the
//! ten-minute portal retry, and that fires in `Portal`, where no join is ever
//! in flight.
//!
//! ## Credentials
//!
//! The machine has no field for a password. This module holds the posted pair
//! in RAM ([`Held`]) and writes it **only** when the machine answers
//! [`Action::CommitCredentials`] - which it only ever does after a join has
//! succeeded (`docs/design/device-web.md`, the credentials bench rule). No
//! flash write happens inside an HTTP handler, and no log line here prints a
//! station SSID or a PSK; the AP's own name (`screeny-<id>`) is not a secret
//! and is printed freely.

use core::cell::RefCell;
use core::net::{IpAddr, Ipv4Addr, SocketAddr};
use core::sync::atomic::{AtomicBool, Ordering};

use edge_nal::UdpBind;
use edge_nal_embassy::{Udp, UdpBuffers};
use embassy_futures::select::{select, select3, Either, Either3};
use embassy_net::{Runner, Stack, StackResources};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use embassy_time::{with_timeout, Duration, Instant, Timer};
use esp_radio::wifi::ap::AccessPointConfig;
use esp_radio::wifi::sta::StationConfig;
use esp_radio::wifi::{
    AuthenticationMethodConfig, Config as WifiConfig, ConnectionError, Interface, PowerSaveMode,
    WifiController,
};
use log::{info, warn};
use screeny_device_api::reply::WifiReply;
use screeny_device_api::WifiState;
use screeny_provision::machine::{Config as ProvConfig, FailReason, Timing, TrialOutcome};
use screeny_provision::{
    Action, Event, JoinTarget, Layout, Provisioner, Screen, State, UriForm, SSID_MAX,
};
use screeny_proto::Rgb888Frame;
use screeny_settings::Wifi;

use crate::{mk_static, station_config};

// ---------------------------------------------------------------------------
// The AP side's numbers
// ---------------------------------------------------------------------------

/// The soft-AP's own address, and the only address the portal answers on.
/// 192.168.4.1/24 is what every ESP soft-AP uses, so a phone that has met one
/// before already expects the number - and it is what
/// [`screeny_provision::PORTAL_IP`] draws on the panel.
pub const AP_IP: Ipv4Addr = Ipv4Addr::new(192, 168, 4, 1);
const AP_PREFIX: u8 = 24;

const _: () = assert!(matches!(
    AP_IP.octets(),
    [192, 168, 4, 1]
), "the panel's PORTAL_IP and this address are the same number");

/// smoltcp socket slots the AP stack is given.
///
/// Three are used - DHCP on UDP/67, the DNS catch-all on UDP/53, and the HTTP
/// listener - and one is spare, because `edge-nal-embassy`'s `Udp` has been
/// seen to hold more than the one socket its buffer type names (see
/// [`crate::NET_SOCKETS`], where the same surprise cost a boot loop). The
/// failure mode of getting this wrong is `SocketSet::add` panicking on the
/// first poll of a task, which is a boot loop, so the spare is not optional;
/// a second spare was, and it is a socket's ~408 bytes of core 0's stack.
///
/// Unlike the station's eight, these three are only *bound* while the AP is
/// up: each service task drops its socket when the AP goes away.
const AP_SOCKETS: usize = 4;

/// One datagram's worth each; neither protocol needs more. A BOOTP packet is
/// 576 bytes at the outside and a DNS query over UDP is capped at 512.
const DHCP_BUF: usize = 640;
const DNS_BUF: usize = 512;

/// Lease table. Four phones at once is generous for a setup network, and it is
/// also what [`AccessPointConfig::with_max_connections`] is set to.
const DHCP_LEASES: usize = 4;
const AP_MAX_CLIENTS: u16 = 4;

/// The DHCP pool: `192.168.4.50` to `.53`, one address per lease slot. The
/// `edge-dhcp` default is `.50`-`.200`, which is 150 addresses for a table
/// that holds four.
const POOL_FIRST: u8 = 50;
const POOL_LAST: u8 = POOL_FIRST + DHCP_LEASES as u8 - 1;

/// How long a lease lasts. Short: a phone is on this network for a minute.
const LEASE_SECS: u32 = 600;

/// The DNS answer's TTL. Short, so that a phone re-asks once the device has
/// left portal mode rather than holding 192.168.4.1 for a name it will need
/// again on the real network.
const DNS_TTL: core::time::Duration = core::time::Duration::from_secs(10);

// ---------------------------------------------------------------------------
// The machine, and the atomics that make it cheap to read
// ---------------------------------------------------------------------------

/// The one [`Provisioner`] on the device.
///
/// A *blocking* mutex: see the module docs. `None` until [`init`].
static MACHINE: BlockingMutex<CriticalSectionRawMutex, RefCell<Option<Provisioner>>> =
    BlockingMutex::new(RefCell::new(None));

/// Whether the soft-AP and its services should be up.
///
/// The machine is the authority ([`Provisioner::ap_up`]); this is the same bit
/// published for the three tasks that have to poll it, and for the HTTP
/// dispatch's captive-portal hook, which is on the request path and must not
/// take a lock to decide a 404.
static AP_UP: AtomicBool = AtomicBool::new(false);

/// How often a service task asks whether the AP has come up or gone down.
///
/// A poll rather than a signal on purpose: three tasks wait on this state
/// (DHCP, DNS and the HTTP worker that follows the AP), and
/// `embassy_sync::Signal` keeps one waker, so a second waiter would silently
/// displace the first. 200 ms of latency on a transition nobody is timing is
/// the right price for not having that bug.
const AP_POLL: Duration = Duration::from_millis(200);

/// Whether the soft-AP is up right now.
#[must_use]
pub fn ap_up() -> bool {
    AP_UP.load(Ordering::Relaxed)
}

/// Run `f` against the machine. Short, synchronous, no I/O: see the module docs.
fn with<R>(f: impl FnOnce(&Provisioner) -> R) -> Option<R> {
    MACHINE.lock(|c| c.borrow().as_ref().map(f))
}

/// The trailing byte of a `GET_WIFI` reply (spec section 8.3), straight from
/// the machine - including its sticky `FAILED` after a posted pair did not work.
#[must_use]
pub fn wifi_state() -> u8 {
    with(Provisioner::wifi_state).unwrap_or(screeny_proto::control::wifi_state::CONNECTING)
}

/// The telemetry `state` byte overlay of spec section 7.3: `PROVISIONING`
/// while the portal is up, `None` otherwise (frames keep being decoded
/// underneath either way).
#[must_use]
pub fn overlay_state() -> Option<u8> {
    with(Provisioner::overlay_state).flatten()
}

/// `status.wifi_state`: **the link**, never the sticky result of the last
/// credentials attempt (`docs/design/device-web.md`, the card 223 paragraph).
///
/// The same derivation the simulator makes in `WifiModel::link_state`, for the
/// same reason: card 228's rule 8 found the two answering differently.
#[must_use]
pub fn link_state() -> WifiState {
    with(|p| match p.state() {
        State::Boot => WifiState::Disconnected,
        State::Joining | State::Trial => WifiState::Connecting,
        State::Online => WifiState::Connected,
        // In `Portal` there is no link to describe, and the machine's own byte
        // - `disconnected`, or `failed` when something failed to get here - is
        // the honest answer.
        State::Portal => WifiState::from_u8(p.wifi_state()).unwrap_or(WifiState::Disconnected),
    })
    .unwrap_or(WifiState::Disconnected)
}

/// `GET /api/v1/wifi`'s body, built from the machine and nothing else.
///
/// A trial in flight or just finished is what the page that posted reads, so
/// it wins - and **the machine decides that**, through
/// [`Provisioner::trial_is_current`], not this function.
#[must_use]
pub fn wifi_reply() -> WifiReply {
    with(|p| {
        if p.trial_is_current()
            && let Some(t) = p.trial()
        {
            return WifiReply::from(t);
        }
        WifiReply {
            state: match p.state() {
                State::Boot => WifiState::Disconnected,
                State::Joining | State::Trial => WifiState::Connecting,
                State::Online => WifiState::Connected,
                State::Portal => {
                    WifiState::from_u8(p.wifi_state()).unwrap_or(WifiState::Disconnected)
                }
            },
            ssid: screeny_device_api::text::text(crate::current_ssid())
                .filter(|s: &_| !s.is_empty()),
            ip: p.ip().map(screeny_device_api::text::ipv4_text),
            reason: None,
        }
    })
    .unwrap_or(WifiReply {
        state: WifiState::Disconnected,
        ssid: None,
        ip: None,
        reason: None,
    })
}

/// What the setup page says about the last posted credentials.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TrialView {
    /// Still being tried; the page reloads itself and asks again.
    Trying,
    /// It worked, and this is where the device is now.
    Connected([u8; 4]),
    /// It did not, in words a person can act on.
    Failed(&'static str),
}

/// The last trial, when it is still the answer the portal page should be given.
#[must_use]
pub fn trial_view() -> Option<TrialView> {
    with(|p| {
        if !p.trial_is_current() {
            return None;
        }
        let t = p.trial()?;
        Some(match t.outcome {
            TrialOutcome::Trying => TrialView::Trying,
            TrialOutcome::Connected => TrialView::Connected(t.ip.unwrap_or([0, 0, 0, 0])),
            TrialOutcome::Failed(FailReason::AuthError) => TrialView::Failed("wrong password"),
            TrialOutcome::Failed(FailReason::NetworkNotFound) => {
                TrialView::Failed("network not found")
            }
            TrialOutcome::Failed(FailReason::Other) => TrialView::Failed("could not join"),
        })
    })
    .flatten()
}

// ---------------------------------------------------------------------------
// The panel
// ---------------------------------------------------------------------------

/// A [`Screen`] with its borrowed name copied out, so the caller can draw it
/// with [`MACHINE`] released. **Which** screen, and when, is still the
/// machine's answer; this is only the copy.
pub enum PanelScreen {
    /// The portal, in whichever of the two layouts the machine picked.
    Portal {
        ssid: heapless::String<SSID_MAX>,
        layout: Layout,
        form: UriForm,
    },
    /// The address a trial just acquired.
    Connected { ip: [u8; 4] },
}

/// What the panel should show, or `None` when it belongs to the normal
/// idle/stream path.
#[must_use]
pub fn screen(now_ms: u32) -> Option<PanelScreen> {
    with(|p| {
        Some(match p.screen(now_ms)? {
            Screen::Portal { ssid, layout, form } => {
                let mut s: heapless::String<SSID_MAX> = heapless::String::new();
                let _ = s.push_str(ssid);
                PanelScreen::Portal {
                    ssid: s,
                    layout,
                    form,
                }
            }
            Screen::Connected { ip } => PanelScreen::Connected { ip },
            // `Screen` is `#[non_exhaustive]`: a variant this firmware has
            // never seen means the panel stays the stream's, which is the safe
            // answer, rather than a panic on a device with no MMU.
            _ => return None,
        })
    })
    .flatten()
}

/// Draw one of those into the frame, through `crates/provision`'s renderer.
pub fn render(s: &PanelScreen, frame: &mut Rgb888Frame) {
    let screen = match s {
        PanelScreen::Portal { ssid, layout, form } => Screen::Portal {
            ssid: ssid.as_str(),
            layout: *layout,
            form: *form,
        },
        PanelScreen::Connected { ip } => Screen::Connected { ip: *ip },
    };
    if let Err(e) = screeny_provision::render(&screen, frame) {
        // The machine never asks for a layout `render` cannot draw, so this is
        // the backstop rather than a case: leave the frame as it is.
        warn!("provision: the portal screen could not be drawn: {:?}", e);
    }
}

// ---------------------------------------------------------------------------
// The AP's network stack
// ---------------------------------------------------------------------------

/// The AP interface's own `embassy-net` stack: static 192.168.4.1/24, no DHCP
/// client, its own resources.
///
/// **Built at boot and left idle**, rather than created when the AP goes up.
/// `StackResources` is `.bss` either way - an embassy task's future and a
/// `StaticCell` are allocated by the linker whether or not they are in use -
/// so creating it on demand would buy nothing in the pool that breaks first,
/// and would add a failure mode (a second `embassy_net::new` on the same
/// interface) to a path that only runs when something has already gone wrong.
pub fn ap_stack(seed: u64) -> (Stack<'static>, Runner<'static, Interface>) {
    let interface = Interface::access_point();
    let mut dns_servers = heapless::Vec::<Ipv4Addr, 3>::new();
    let _ = dns_servers.push(AP_IP);
    let config = embassy_net::Config::ipv4_static(embassy_net::StaticConfigV4 {
        address: embassy_net::Ipv4Cidr::new(AP_IP, AP_PREFIX),
        gateway: None,
        dns_servers,
    });
    embassy_net::new(
        interface,
        config,
        mk_static!(StackResources<AP_SOCKETS>, StackResources::<AP_SOCKETS>::new()),
        seed,
    )
}

#[embassy_executor::task]
pub async fn ap_net_task(mut runner: Runner<'static, Interface>) -> ! {
    runner.run().await
}

/// Block until the AP is (or is not) up. See [`AP_POLL`].
async fn wait_ap(want: bool) {
    while AP_UP.load(Ordering::Relaxed) != want {
        Timer::after(AP_POLL).await;
    }
}

/// [`wait_ap`], for the HTTP worker that follows the soft-AP.
pub async fn wait_ap_pub(want: bool) {
    wait_ap(want).await
}

/// DHCP on the AP interface: a small pool, and RFC 8910's option 114 so the
/// OSes that read it find the portal without a DNS round trip.
#[embassy_executor::task]
pub async fn dhcp_task(stack: Stack<'static>) {
    let buffers = mk_static!(UdpBuffers<1, DHCP_BUF, DHCP_BUF, 2>, UdpBuffers::new());
    let buf = mk_static!([u8; DHCP_BUF], [0u8; DHCP_BUF]);

    loop {
        wait_ap(true).await;
        let udp = Udp::new(stack, buffers);
        let socket = match udp
            .bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 67))
            .await
        {
            Ok(s) => s,
            Err(e) => {
                warn!("dhcp: bind failed: {:?}", e);
                Timer::after(Duration::from_secs(2)).await;
                continue;
            }
        };

        let mut server: edge_dhcp::server::Server<_, DHCP_LEASES> =
            edge_dhcp::server::Server::new(|| Instant::now().as_secs(), AP_IP);
        server.range_start = pool_addr(POOL_FIRST);
        server.range_end = pool_addr(POOL_LAST);
        let mut gw = [AP_IP];
        let mut options = edge_dhcp::server::ServerOptions::new(AP_IP, Some(&mut gw));
        let dns = [AP_IP];
        options.dns = &dns;
        // RFC 8910. The path is the portal's own, so an OS that follows it
        // lands on the setup form rather than on the status page.
        options.captive_url = Some("http://192.168.4.1/");
        options.lease_duration_secs = LEASE_SECS;

        info!(
            "dhcp: serving 192.168.4.{}-{} on the setup network",
            POOL_FIRST, POOL_LAST
        );
        let mut socket = Leases {
            inner: socket,
            last: Ipv4Addr::UNSPECIFIED,
        };
        let run = edge_dhcp::io::server::run(&mut server, &options, &mut socket, &mut buf[..]);
        if let Either::First(Err(e)) = select(run, wait_ap(false)).await {
            warn!("dhcp: stopped: {:?}", e);
            Timer::after(Duration::from_secs(1)).await;
        } else {
            info!("dhcp: the setup network went away, socket released");
        }
        // `socket` and `udp` drop here, which is what returns the smoltcp slot.
    }
}

fn pool_addr(last: u8) -> Ipv4Addr {
    let o = AP_IP.octets();
    Ipv4Addr::new(o[0], o[1], o[2], last)
}

/// A UDP socket that says which address the DHCP server just handed out.
///
/// `edge_dhcp::io::server::run` owns its loop, so this wraps the socket it
/// sends through and reads the reply back out of the bytes on their way past.
/// **The leased address and nothing else**: not the client's MAC, which is the
/// owner's phone.
struct Leases<S> {
    inner: S,
    last: Ipv4Addr,
}

impl<S: edge_nal::io::ErrorType> edge_nal::io::ErrorType for Leases<S> {
    type Error = S::Error;
}

impl<S: edge_nal::UdpReceive> edge_nal::UdpReceive for Leases<S> {
    async fn receive(&mut self, buf: &mut [u8]) -> Result<(usize, SocketAddr), Self::Error> {
        self.inner.receive(buf).await
    }
}

impl<S: edge_nal::UdpReceive + edge_nal::UdpSend> edge_nal::UdpSend for Leases<S> {
    async fn send(&mut self, remote: SocketAddr, data: &[u8]) -> Result<(), Self::Error> {
        if let Ok(p) = edge_dhcp::Packet::decode(data)
            && p.reply
            && p.yiaddr != Ipv4Addr::UNSPECIFIED
            && p.yiaddr != self.last
        {
            self.last = p.yiaddr;
            info!("dhcp: leased {}", p.yiaddr);
        }
        self.inner.send(remote, data).await
    }
}

/// The DNS catch-all: every name answers 192.168.4.1, which is what makes the
/// captive sheet open by itself (research 007 section 4.3).
#[embassy_executor::task]
pub async fn dns_task(stack: Stack<'static>) {
    let buffers = mk_static!(UdpBuffers<1, DNS_BUF, DNS_BUF, 2>, UdpBuffers::new());
    let rx = mk_static!([u8; DNS_BUF], [0u8; DNS_BUF]);
    let tx = mk_static!([u8; DNS_BUF], [0u8; DNS_BUF]);

    loop {
        wait_ap(true).await;
        let udp = Udp::new(stack, buffers);
        let mut socket = match udp
            .bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 53))
            .await
        {
            Ok(s) => s,
            Err(e) => {
                warn!("dns: bind failed: {:?}", e);
                Timer::after(Duration::from_secs(2)).await;
                continue;
            }
        };
        info!("dns: catch-all up, every name answers 192.168.4.1");
        if let Either::First(Err(e)) = select(
            dns_loop(&mut socket, &mut rx[..], &mut tx[..]),
            wait_ap(false),
        )
        .await
        {
            warn!("dns: stopped: {:?}", e);
            Timer::after(Duration::from_secs(1)).await;
        } else {
            info!("dns: the setup network went away, socket released");
        }
    }
}

/// `edge_captive::io::run`'s loop, with a counter.
///
/// The crate's own `run` is four lines shorter and answers exactly the same
/// bytes - `edge_captive::reply` is what does the work in both - but it logs
/// per query at `debug`, and a phone joining a captive network fires a dozen
/// probes a second. The card asks for a count per ten seconds instead, and
/// that is the only reason this loop is spelled out here.
async fn dns_loop<S: edge_nal::UdpReceive + edge_nal::UdpSend>(
    socket: &mut S,
    rx: &mut [u8],
    tx: &mut [u8],
) -> Result<(), S::Error> {
    const PERIOD: Duration = Duration::from_secs(10);
    let mut answered = 0u32;
    let mut refused = 0u32;
    let mut window = Instant::now();
    loop {
        let (len, remote) = socket.receive(rx).await?;
        match edge_captive::reply(&rx[..len], &AP_IP.octets(), DNS_TTL, tx) {
            Ok(n) => {
                socket.send(remote, &tx[..n]).await?;
                answered += 1;
            }
            Err(_) => refused += 1,
        }
        if window.elapsed() >= PERIOD {
            info!(
                "dns: {} answered, {} unparseable in the last {} s",
                answered,
                refused,
                PERIOD.as_secs()
            );
            answered = 0;
            refused = 0;
            window = Instant::now();
        }
    }
}

// ---------------------------------------------------------------------------
// The radio half
// ---------------------------------------------------------------------------

/// The credentials this device knows about, none of which the machine can see.
struct Held {
    /// What the store holds, kept in step with every commit.
    stored: Option<Wifi>,
    /// The compile-time pair, in a `bench-wifi` build only.
    builtin: Option<Wifi>,
    /// What was last posted to `POST /api/v1/wifi` or `SET_WIFI`. RAM only,
    /// until the machine says it joined.
    trial: Option<Wifi>,
    /// Spec 8.2's persist bit for that pair.
    trial_persist: bool,
}

impl Held {
    fn get(&self, which: JoinTarget) -> Option<&Wifi> {
        match which {
            JoinTarget::Stored => self.stored.as_ref(),
            JoinTarget::Builtin => self.builtin.as_ref(),
            JoinTarget::Trial => self.trial.as_ref(),
        }
    }
}

/// How long one join attempt is given before it is called a failure.
///
/// A little longer than [`Timing::SPEC`]'s `join_attempt_ms`, because this is
/// the *radio's* deadline and the machine's is the one that should decide:
/// with `ScanMethod::AllChannels` a scan alone is ~2 s.
const ATTEMPT_WAIT: Duration = Duration::from_millis(Timing::SPEC.join_attempt_ms as u64);

/// How long DHCP is given once the association is up, inside the same attempt.
const DHCP_WAIT: Duration = Duration::from_secs(8);

/// Feed the machine one event, carry nothing out: the actions are returned.
fn step(ev: Event<'_>, now_ms: u32) -> screeny_provision::Actions {
    MACHINE.lock(|c| {
        let mut b = c.borrow_mut();
        let Some(p) = b.as_mut() else {
            return screeny_provision::Actions::new();
        };
        let before = p.state();
        let actions = p.step(ev, now_ms);
        let after = p.state();
        if after != before {
            info!("provision: {} -> {}", before.name(), after.name());
        }
        // **`AP_UP` is deliberately not written here.** The machine flips its
        // own `ap_up` *inside* this call and then asks for `RaiseAp`/`DropAp`;
        // publishing it now would make `Driver::raise_ap` see the AP as
        // already up and return without ever configuring the radio. The two
        // actions own this flag, which is also what makes it mean "the radio
        // and the services agree" rather than "the machine intends to".
        actions
    })
}

/// Sort a radio disconnect reason into the machine's three buckets.
fn fail_reason(r: esp_radio::wifi::DisconnectReason) -> FailReason {
    use esp_radio::wifi::DisconnectReason as D;
    match r {
        D::NoAccessPointFound
        | D::NoAccessPointFoundWithCompatibleSecurity
        | D::NoAccessPointFoundInAuthmodeThreshold
        | D::NoAccessPointFoundInRssiThreshold => FailReason::NetworkNotFound,
        D::AuthenticationFailed
        | D::AuthenticationExpired
        | D::FourWayHandshakeTimeout
        | D::HandshakeTimeout
        | D::MicFailure
        | D::GroupKeyUpdateTimeout
        | D::_802_1xAuthenticationFailed => FailReason::AuthError,
        _ => FailReason::Other,
    }
}

/// Everything the driver loop carries between iterations.
struct Driver {
    held: Held,
    ap_ssid: &'static str,
    /// The station half of whatever the radio is configured for, so that
    /// raising or dropping the AP does not also forget the network.
    sta: StationConfig,
    /// A join the machine has asked for and this loop has not started yet.
    pending_join: Option<JoinTarget>,
    /// The radio was stopped and restarted by a mode change while the device
    /// was online, so the station has to be told to associate again. See
    /// [`Driver::drop_ap`].
    reassociate: bool,
    /// What the last tick thought of the link, for the `LinkUp`/`LinkDown`
    /// edges the machine's sixty-second rule is measured from.
    link_was_up: bool,
    /// The station's stack, for the address a join has to produce before the
    /// machine will call it a join.
    sta_stack: Stack<'static>,
}

impl Driver {
    fn ap_config(&self, channel: u8) -> AccessPointConfig {
        AccessPointConfig::default()
            .with_ssid(self.ap_ssid.try_into().expect("ap ssid <= 32 bytes"))
            // **Open** (device-web decision 2): the home PSK crosses it in
            // clear during setup, which spec 8.4 already accepts, and a
            // password on the setup network is a password nobody can be told.
            .with_authentication(AuthenticationMethodConfig::Open)
            .with_channel(channel)
            .with_max_connections(AP_MAX_CLIENTS)
    }

    /// Apply the station config, keeping the AP up if it is up.
    ///
    /// `set_config` only stops and restarts the radio when the *mode* changes,
    /// so re-applying the station half in APSTA leaves the soft-AP beaconing -
    /// which is the whole point of APSTA here.
    fn apply(&self, controller: &mut WifiController<'static>, channel: u8) {
        let conf = if ap_up() {
            WifiConfig::AccessPointStation(self.sta.clone(), self.ap_config(channel))
        } else {
            WifiConfig::Station(self.sta.clone())
        };
        if let Err(e) = controller.set_config(&conf) {
            warn!("provision: set_config failed {:?}", e);
        }
        // A mode change restarts the radio, and a restart resets power saving.
        // The frame path needs it off.
        if let Err(e) = controller.set_power_saving(PowerSaveMode::None) {
            warn!("provision: set_power_saving failed {:?}", e);
        }
    }

    /// The channel the station is on, or 1 before there is one. The ESP32 has
    /// one PHY: an AP on a different channel from the station only gets the
    /// off-time, so the AP follows the station (research 007 section 2).
    fn channel(controller: &WifiController<'static>) -> u8 {
        controller.channel().map(|(c, _)| c).unwrap_or(1)
    }

    async fn raise_ap(&mut self, controller: &mut WifiController<'static>) {
        if ap_up() {
            return;
        }
        AP_UP.store(true, Ordering::Relaxed);
        let ch = Self::channel(controller);
        self.apply(controller, ch);
        info!(
            "provision: soft-AP {} up on channel {}, portal at 192.168.4.1",
            self.ap_ssid, ch
        );
    }

    async fn drop_ap(&mut self, controller: &mut WifiController<'static>) {
        if !ap_up() {
            return;
        }
        AP_UP.store(false, Ordering::Relaxed);
        // Let the DHCP, DNS and HTTP tasks notice and release their sockets
        // before the interface goes away underneath them.
        Timer::after(AP_POLL * 2).await;
        let ch = Self::channel(controller);
        self.apply(controller, ch);
        // APSTA -> STA is a mode change, so `esp_wifi_stop`/`start` ran and the
        // station's association went with it. Nothing in the machine describes
        // that (it is `Online` and, as far as it knows, still is), so the
        // reconnect is this module's job; if it fails, the tick's link
        // reconciliation feeds `LinkDown` and the sixty-second rule takes over.
        self.reassociate = true;
        info!("provision: soft-AP down; re-associating the station");
    }

    async fn commit(&mut self, which: JoinTarget) {
        let Some(w) = self.held.get(which).cloned() else {
            warn!("provision: commit asked for credentials this build does not hold");
            return;
        };
        match which {
            JoinTarget::Trial => {
                if !self.held.trial_persist {
                    // Spec 8.2's persist bit was clear: try, do not keep.
                    info!("provision: the posted pair joined but asked not to be kept");
                    self.held.stored = Some(w);
                    return;
                }
                let what = crate::store::Immediate::Wifi {
                    wifi: w.clone(),
                    persist: true,
                };
                match crate::store::commit_immediate(&what).await {
                    Ok(()) => info!("provision: the new credentials joined and are now stored"),
                    Err(e) => {
                        crate::store::FAILURES.fetch_add(1, Ordering::Relaxed);
                        warn!("provision: joined, but storing the credentials failed: {:?}", e);
                    }
                }
                self.held.stored = Some(w);
            }
            // Device-web decision 6: the compile-time pair **seeds an empty
            // store**, and only an empty one. The machine reaches `Builtin`
            // down two roads - nothing stored at all, and a stored pair that
            // failed three times - and the second must not write: a stored
            // pair that stopped working is the owner's to replace, not this
            // build's to quietly overwrite with the bench network. That was
            // the rule `station_loop` carried ("deliberately *not* stored")
            // and it is carried here now, because this is the only place in
            // the firmware that can commit.
            JoinTarget::Builtin => {
                if self.held.stored.is_some() {
                    info!("provision: the build's credentials joined; the stored pair is left alone");
                } else {
                    crate::store::seed_wifi(&w).await;
                    self.held.stored = Some(w);
                }
            }
            // The store already holds them; that is where they came from.
            JoinTarget::Stored => {}
        }
    }

    async fn carry_out(&mut self, a: Action, controller: &mut WifiController<'static>) {
        info!("provision: action {:?}", a);
        match a {
            Action::StartJoin { which, .. } => self.pending_join = Some(which),
            Action::StopJoin => {
                // "If a connection attempt is currently in progress, it is
                // aborted" - and `NotConnected` here only means there was
                // nothing to abort.
                let _ = controller.disconnect_async().await;
            }
            Action::RaiseAp => self.raise_ap(controller).await,
            Action::DropAp => self.drop_ap(controller).await,
            Action::CommitCredentials { which } => self.commit(which).await,
            Action::ClearCredentials => {
                // Nothing in this firmware can produce `Event::ButtonWipe`
                // yet - the button is cards 230/231 - so this is unreachable,
                // and card 223 was told in as many words not to write a path
                // that erases the store. When the button lands, the erase goes
                // here and nowhere else.
                warn!("provision: ClearCredentials: no event in this build can ask for it (cards 230/231)");
            }
            Action::Announce => crate::net::INFO_CHANGED.signal(()),
            // `Action` is `#[non_exhaustive]`.
            _ => warn!("provision: an action this firmware does not know: {:?}", a),
        }
    }

    async fn apply_actions(
        &mut self,
        actions: screeny_provision::Actions,
        controller: &mut WifiController<'static>,
    ) {
        for a in actions {
            self.carry_out(a, controller).await;
        }
    }

    /// Somebody posted credentials. Hold the pair, tell the machine the SSID.
    async fn posted(&mut self, n: crate::NewWifi, controller: &mut WifiController<'static>) {
        let ssid = n.wifi.ssid.clone();
        self.held.trial = Some(n.wifi);
        self.held.trial_persist = n.persist;
        // Spec 8.4: the SSID is bytes, `Event::CredentialsPosted` is text. A
        // non-UTF-8 SSID reaches the radio intact all the same - the machine
        // only ever uses this string to report back what was tried.
        let name = core::str::from_utf8(ssid.as_bytes()).unwrap_or("");
        let acts = step(Event::CredentialsPosted { ssid: name }, crate::now_ms());
        self.apply_actions(acts, controller).await;
    }

    /// Run the join the machine asked for, to a `Joined` or a `JoinFailed`.
    async fn run_join(&mut self, which: JoinTarget, controller: &mut WifiController<'static>) {
        let Some(w) = self.held.get(which).cloned() else {
            warn!("provision: no {:?} credentials to join with", which);
            let acts = step(
                Event::JoinFailed {
                    reason: FailReason::Other,
                },
                crate::now_ms(),
            );
            self.apply_actions(acts, controller).await;
            return;
        };
        let Some(cfg) = station_config(&w) else {
            warn!("provision: those credentials are not expressible to the radio (SSID too long, or a non-UTF-8 PSK)");
            let acts = step(
                Event::JoinFailed {
                    reason: FailReason::Other,
                },
                crate::now_ms(),
            );
            self.apply_actions(acts, controller).await;
            return;
        };
        self.sta = cfg;
        crate::set_current_ssid(w.ssid.as_bytes());
        let ch = Self::channel(controller);
        self.apply(controller, ch);

        // A post that arrives mid-attempt is the user correcting a typo: the
        // machine turns it into `StopJoin` + a new `StartJoin`, so it has to be
        // able to interrupt this.
        let outcome = with_timeout(
            ATTEMPT_WAIT,
            select(controller.connect_async(), crate::NEW_WIFI.wait()),
        )
        .await;

        let ev = match outcome {
            Ok(Either::First(Ok(info))) => {
                // The one line in this firmware that says a station SSID out
                // loud, moved here from `station_loop` unchanged.
                info!(
                    "wifi: connected ssid {:?} ch {} bssid {:02x?}",
                    info.ssid, info.channel, info.bssid
                );
                // The AP follows the station's channel; re-apply so the beacon
                // moves with it rather than being left on channel 1.
                if ap_up() && info.channel != ch {
                    self.apply(controller, info.channel);
                }
                return self.await_address(controller).await;
            }
            Ok(Either::First(Err(ConnectionError::Failed(info)))) => {
                // Only the reason: it is the diagnostic, and it keeps one more
                // copy of the SSID out of the bench log.
                warn!("wifi: join failed: {:?}", info.reason);
                Event::JoinFailed {
                    reason: fail_reason(info.reason),
                }
            }
            Ok(Either::First(Err(e))) => {
                warn!("wifi: join failed: {:?}", e);
                Event::JoinFailed {
                    reason: FailReason::Other,
                }
            }
            // The machine turns this into `StopJoin` + a new `StartJoin`
            // itself. Nothing else is fed in between - a `Tick` here could
            // expire the attempt first and start a different join.
            Ok(Either::Second(n)) => return self.posted(n, controller).await,
            Err(_) => {
                warn!("wifi: the radio did not answer the attempt in {} s", ATTEMPT_WAIT.as_secs());
                let _ = controller.disconnect_async().await;
                Event::JoinFailed {
                    reason: FailReason::Other,
                }
            }
        };
        let acts = step(ev, crate::now_ms());
        self.apply_actions(acts, controller).await;
    }

    /// Associated: now wait for the address, because the machine's `Joined`
    /// carries one and "associated but no DHCP" is not online.
    async fn await_address(&mut self, controller: &mut WifiController<'static>) {
        let stack = self.sta_stack;
        let got = with_timeout(
            DHCP_WAIT,
            select(stack.wait_config_up(), crate::NEW_WIFI.wait()),
        )
        .await;
        let ev = match got {
            Ok(Either::First(())) => match stack.config_v4() {
                Some(c) => {
                    self.link_was_up = true;
                    Event::Joined {
                        ip: c.address.address().octets(),
                    }
                }
                None => Event::JoinFailed {
                    reason: FailReason::Other,
                },
            },
            Ok(Either::Second(n)) => return self.posted(n, controller).await,
            Err(_) => {
                warn!("wifi: associated, but DHCP did not answer in {} s", DHCP_WAIT.as_secs());
                Event::JoinFailed {
                    reason: FailReason::Other,
                }
            }
        };
        let acts = step(ev, crate::now_ms());
        self.apply_actions(acts, controller).await;
    }

    /// One second of nothing happening: the RSSI, the link edges, and the
    /// machine's own timers.
    async fn tick(&mut self, controller: &mut WifiController<'static>) {
        if let Ok(r) = controller.rssi() {
            crate::RSSI_DBM.store(r.clamp(-128, 0) as i8, Ordering::Relaxed);
        }
        // The link, from the two things that make it one: an association and
        // an address. Derived on a tick rather than taken from
        // `wait_for_disconnect_async` so that a radio restart - which is what
        // raising or dropping the soft-AP *is* - produces the same edge as an
        // access point going away, and self-heals the same way.
        let up = controller.is_connected() && self.sta_stack.config_v4().is_some();
        if up != self.link_was_up {
            self.link_was_up = up;
            if !up {
                crate::RSSI_DBM.store(0, Ordering::Relaxed);
            }
            info!("wifi: link {}", if up { "up" } else { "down" });
            let acts = step(
                if up { Event::LinkUp } else { Event::LinkDown },
                crate::now_ms(),
            );
            self.apply_actions(acts, controller).await;
        }
        let acts = step(Event::Tick, crate::now_ms());
        self.apply_actions(acts, controller).await;
    }
}

/// Build the machine. Called by `main` before any task can read it.
///
/// The station's `embassy-net` stack is **not** kept here and is a parameter
/// of [`provision_task`] instead: a `Stack<'d>` holds a `&RefCell<Inner>` and
/// is therefore not `Sync`, so it cannot live in a `static` at all - the same
/// constraint `crate::http::Ctx` documents.
pub fn init(ap_ssid: &'static str, has_stored: bool, has_builtin: bool) {
    MACHINE.lock(|c| {
        *c.borrow_mut() = Some(Provisioner::new(&ProvConfig {
            ap_ssid,
            has_stored,
            has_builtin,
            form: UriForm::NoPass,
            timing: Timing::SPEC,
        }));
    });
    info!(
        "provision: ap {} | stored credentials {} | built-in {}",
        ap_ssid, has_stored, has_builtin
    );
}

/// Owns the radio and drives the machine. Never returns.
#[embassy_executor::task]
pub async fn provision_task(
    mut controller: WifiController<'static>,
    stored: Option<Wifi>,
    ap_ssid: &'static str,
    sta_stack: Stack<'static>,
) {
    let mut d = Driver {
        held: Held {
            stored,
            builtin: crate::builtin_wifi(),
            trial: None,
            trial_persist: true,
        },
        ap_ssid,
        sta: StationConfig::default(),
        pending_join: None,
        reassociate: false,
        link_was_up: false,
        sta_stack,
    };

    let acts = step(Event::Boot, crate::now_ms());
    d.apply_actions(acts, &mut controller).await;

    loop {
        // A join the machine asked for outranks waiting for anything else: it
        // is the only thing with a deadline the machine is counting.
        if let Some(which) = d.pending_join.take() {
            d.run_join(which, &mut controller).await;
            continue;
        }
        // A mode change restarted the radio under an online station.
        if core::mem::take(&mut d.reassociate) {
            let _ = with_timeout(ATTEMPT_WAIT, controller.connect_async()).await;
            continue;
        }

        // Idle. All three of these borrow the controller immutably or not at
        // all, which is what lets them be awaited together; see the module
        // docs for the one event that cannot join them.
        match select3(
            controller.wait_for_access_point_connected_event_async(),
            crate::NEW_WIFI.wait(),
            Timer::after(Duration::from_secs(1)),
        )
        .await
        {
            Either3::First(Ok(ev)) => {
                use esp_radio::wifi::ap::EventInfo;
                // The MAC is not logged: it identifies the owner's phone, and
                // the count is what the machine's retry rule reads.
                let (e, word) = match ev {
                    EventInfo::Connected(_) => (Event::ApClientAssociated, "joined"),
                    EventInfo::Disconnected(_) => (Event::ApClientLeft, "left"),
                };
                let acts = step(e, crate::now_ms());
                d.apply_actions(acts, &mut controller).await;
                info!(
                    "provision: a device {} the setup network ({} now on it)",
                    word,
                    with(Provisioner::ap_clients).unwrap_or(0)
                );
            }
            Either3::First(Err(e)) => {
                warn!("provision: ap event subscription failed {:?}", e);
                Timer::after(Duration::from_secs(1)).await;
            }
            Either3::Second(n) => d.posted(n, &mut controller).await,
            Either3::Third(()) => d.tick(&mut controller).await,
        }
    }
}
