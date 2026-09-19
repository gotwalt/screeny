//! What a [`SimDevice`](crate::SimDevice) is configured with.

use std::net::{IpAddr, Ipv4Addr};

use screeny_proto::control::IdleMode;

/// The spec section 7.2 constants, overridable so a test does not have to wait
/// ten real seconds to watch `HOLD` expire.
///
/// [`Timing::SPEC`] is the device's behaviour and is the default. Anything
/// else is a test fixture, and [`crate::SimDevice`] says so in its events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    /// After the last accepted frame, the active source keeps exclusivity
    /// this long.
    pub lock_ms: u32,
    /// No frames for this long and the stream is considered stopped.
    pub stream_timeout_ms: u32,
    /// How long the last frame stays lit after the stream stops.
    pub hold_ms: u32,
    /// Cross-fade duration into the idle screen.
    pub fade_ms: u32,
    /// Minimum gap between `BUSY` packets to one source.
    pub busy_min_interval_ms: u32,
    /// Minimum gap between piggybacked `TELEMETRY` packets to one source.
    pub telemetry_min_interval_ms: u32,
    /// Minimum gap between *new* `GET_INFO` replies to one source address
    /// (spec section 5.5). A retry of a `req_id` already answered inside the
    /// window is exempt.
    pub info_min_interval_ms: u32,
}

impl Timing {
    /// The constants exactly as spec section 7.2 and 5.5 give them.
    pub const SPEC: Timing = Timing {
        lock_ms: screeny_proto::LOCK_MS,
        stream_timeout_ms: screeny_proto::STREAM_TIMEOUT_MS,
        hold_ms: screeny_proto::HOLD_MS,
        fade_ms: screeny_proto::FADE_MS,
        busy_min_interval_ms: screeny_proto::BUSY_MIN_INTERVAL_MS,
        telemetry_min_interval_ms: screeny_proto::TELEMETRY_MIN_INTERVAL_MS,
        info_min_interval_ms: 1_000,
    };
}

impl Default for Timing {
    fn default() -> Self {
        Timing::SPEC
    }
}

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
}

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
            mdns: false,
            ..Config::default()
        }
    }
}
