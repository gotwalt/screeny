//! What a [`SimDevice`](crate::SimDevice) is configured with.

use std::net::{IpAddr, Ipv4Addr};

use screeny_proto::control::IdleMode;

/// The spec section 7.2 constants, overridable so a test does not have to wait
/// ten real seconds to watch `HOLD` expire.
///
/// `Timing::SPEC` is the device's behaviour and is the default. Anything else
/// is a test fixture, and [`crate::SimDevice`] says so in its events. The type
/// belongs to the shared receiver core (card 016), so the simulator and the
/// firmware cannot disagree about what the constants are.
pub use screeny_receiver::Timing;

pub use crate::wifi::{WifiOutcome, WifiTiming};

/// Deliberate misbehaviour, so a sender can be tested against a bad network
/// and a slow device without either being real.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Faults {
    /// Percentage of arriving frame datagrams to discard before they are even
    /// parsed, 0.0..=100.0. Models loss on the air: the packet never happened,
    /// so nothing is counted.
    pub drop_pct: f32,
    /// Hold each arriving frame datagram this long before processing it.
    /// Models a bursty link; raises `jitter_us`.
    pub delay_ms: u32,
    /// Pretend decoding takes this long. The drain loop really does sleep, so
    /// packets really do pile up and `frames_dropped_superseded` really does
    /// rise - which is the point.
    pub decode_ms: u32,
}

impl Faults {
    /// True if nothing is being injected.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.drop_pct <= 0.0 && self.delay_ms == 0 && self.decode_ms == 0
    }
}

/// How the simulated panel converts sRGB to what you see.
///
/// This is `docs/design/generative-art-brief.md` section 5's panel model:
/// sRGB -> linear -> quantise each channel to `levels` -> back to sRGB, with
/// brightness applied in linear light.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PanelModel {
    /// Quantisation levels per channel. 64 is the panel; 32 is the stress test.
    pub levels: u16,
}

impl Default for PanelModel {
    fn default() -> Self {
        PanelModel { levels: 64 }
    }
}

/// Everything a [`SimDevice`](crate::SimDevice) needs to start.
#[derive(Debug, Clone)]
pub struct Config {
    /// Address to bind both sockets to. `0.0.0.0` on the bench, `127.0.0.1`
    /// in tests.
    pub bind: IpAddr,
    /// Frame port. **0 binds an ephemeral port**, which is what tests want;
    /// ask the running device for the port it actually got.
    pub frame_port: u16,
    /// Control port. 0 binds an ephemeral port.
    pub control_port: u16,
    /// Friendly name, the `name=` TXT key. `SET_NAME` changes it.
    pub name: String,
    /// mDNS instance name. **Never `screeny`** - that is the real device.
    pub instance: String,
    /// Stable short device id, the `id=` TXT key.
    pub id: String,
    /// Firmware version string, the `fw=` TXT key.
    pub fw: String,
    /// Advertise over mDNS. Off in tests: they use loopback and a known port.
    pub mdns: bool,
    /// The brightness ceiling `SET_BRIGHTNESS` clamps to, which is how a
    /// sender learns the device's cap. The real panel runs off laptop USB and
    /// caps much lower; the simulator does not, so this defaults to 255.
    pub brightness_cap: u8,
    /// Brightness at startup.
    pub brightness: u8,
    /// Idle behaviour at startup.
    pub idle_mode: IdleMode,
    /// The RSSI to report in telemetry and draw on the status screen.
    pub rssi_dbm: i8,
    /// Spec timing constants, or a test's compressed versions of them.
    pub timing: Timing,
    /// Injected faults.
    pub faults: Faults,
    /// The panel model used for the window and for `panel` snapshots.
    pub panel: PanelModel,
    /// Seed for the fault-injection RNG, so a dropped-frame test repeats.
    pub fault_seed: u64,

    // --- card 224: the HTTP API and the WiFi life ---------------------------
    /// Serve the device's HTTP API (`crates/device-api`'s routes). On by
    /// default: a simulator that does not answer the same questions the device
    /// answers is not a simulator.
    pub http: bool,
    /// HTTP port. **0 binds an ephemeral one**, which is what tests want; ask
    /// [`SimHandle::http_addr`](crate::SimHandle::http_addr) for the port it
    /// actually got. The binary defaults to [`DEFAULT_HTTP_PORT`], never 80:
    /// nothing on this bench runs as root.
    pub http_port: u16,
    /// Whether [`http_port`](Self::http_port) was asked for **by name**.
    ///
    /// `false` - the default - makes it a preference: if it is taken, an
    /// ephemeral port is bound instead and the binary says so on stderr. That
    /// is what lets two or three simulators run side by side on one machine
    /// without anybody having thought about HTTP at all, which is how several
    /// sessions actually use them.
    ///
    /// `true` means somebody named the port (`--http-port N`) and is going to
    /// connect to it, so a busy one is an error rather than a surprise.
    pub http_port_explicit: bool,
    /// The SSID the simulator claims its store holds, and so what `GET_WIFI`
    /// and `/api/v1/status` report. Never a PSK; the simulator keeps none.
    pub wifi_ssid: String,
    /// The soft-AP's name, which is what the portal screen and its QR carry.
    /// Spec-shaped: `screeny-<id>`.
    pub ap_ssid: String,
    /// What the scripted radio does with a join attempt.
    pub wifi_outcome: WifiOutcome,
    /// How long a scripted join attempt takes before it is answered. The boot
    /// join with [`WifiOutcome::Ok`] is instant whatever this says - see
    /// [`crate::wifi`].
    pub wifi_join_ms: u32,
    /// Research 007 section 5.2's timing, or a test's compressed version of
    /// it. [`WifiTiming::SPEC`] is a device's behaviour and is the default.
    pub wifi_timing: WifiTiming,
    /// Boot straight into the captive portal: an empty store and no
    /// compile-time credentials, which is what a factory-fresh device is.
    pub start_in_portal: bool,
}

/// The port the `screeny-sim` binary serves HTTP on by default.
///
/// Not 80: binding it needs root, and nothing on this bench runs as root. The
/// device itself serves port 80; a simulator sharing a host with a browser
/// cannot.
pub const DEFAULT_HTTP_PORT: u16 = 8080;

impl Default for Config {
    fn default() -> Self {
        Config {
            bind: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            frame_port: screeny_proto::DEFAULT_FRAME_PORT,
            control_port: screeny_proto::DEFAULT_CONTROL_PORT,
            name: "screeny sim".into(),
            instance: crate::DEFAULT_INSTANCE.into(),
            id: "515151".into(),
            fw: concat!("sim-", env!("CARGO_PKG_VERSION")).into(),
            mdns: false,
            brightness_cap: 255,
            brightness: 255,
            idle_mode: IdleMode::Status,
            rssi_dbm: -55,
            timing: Timing::SPEC,
            faults: Faults::default(),
            panel: PanelModel::default(),
            fault_seed: 0x5EED_5CEE,
            http: true,
            http_port: DEFAULT_HTTP_PORT,
            http_port_explicit: false,
            wifi_ssid: crate::core::SIM_SSID.into(),
            ap_ssid: "screeny-515151".into(),
            wifi_outcome: WifiOutcome::Ok,
            wifi_join_ms: 200,
            wifi_timing: WifiTiming::SPEC,
            start_in_portal: false,
        }
    }
}

impl Config {
    /// A configuration for tests: loopback, ephemeral ports, no mDNS.
    #[must_use]
    pub fn for_test() -> Self {
        Config {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            frame_port: 0,
            control_port: 0,
            http_port: 0,
            mdns: false,
            ..Config::default()
        }
    }
}
