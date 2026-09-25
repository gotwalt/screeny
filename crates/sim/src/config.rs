//! What a [`SimDevice`](crate::SimDevice) is configured with.

use std::net::{IpAddr, Ipv4Addr};

use screeny_device_api::{FwSlot, FwState, ResetReason};
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

/// The half of `GET /api/v1/status` a host cannot measure, chosen rather
/// than discovered (card 192).
///
/// There is no flash here, no `otadata`, no reset to have a reason and no
/// stack worth measuring, so these seven were constants. They are still not
/// measurements - **nothing else in the simulator acts on them**: a
/// `pending_verify` slot changes no behaviour and a `brownout` reset reason
/// reboots nothing. They exist so the unhappy rows of the Studio's device
/// page, which nobody sees in the ordinary course of things, can be seen at
/// all.
///
/// [`Health::default()`] is what the simulator has always reported.
///
/// The one thing that does move on its own is [`reset_reason`](Self::reset_reason):
/// a simulated `REBOOT` - UDP or `POST /api/v1/reboot` - sets it to
/// [`ResetReason::Software`] from then on, exactly as the device's own does.
/// A test that wants a different reason after a reboot sets it again with
/// [`SimHandle::set_health`](crate::SimHandle::set_health).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Health {
    /// Why the chip last restarted.
    pub reset_reason: ResetReason,
    /// Which app slot is running.
    pub fw_slot: FwSlot,
    /// The running slot's `otadata` state.
    pub fw_state: FwState,
    /// Settings-store errors since boot. Non-zero means the `screeny`
    /// partition is unhappy and the status page should say so.
    pub store_errors: u32,
    /// Heap in use, bytes.
    pub heap_used: u32,
    /// Heap total, bytes. The firmware's 64 + 32 KB (card 220).
    pub heap_size: u32,
    /// Stack never touched, bytes.
    pub stack_free: u32,
}

impl Default for Health {
    fn default() -> Self {
        Health {
            reset_reason: ResetReason::PowerOn,
            fw_slot: FwSlot::Ota0,
            fw_state: FwState::Valid,
            store_errors: 0,
            heap_used: 64 * 1024,
            heap_size: 96 * 1024,
            stack_free: 20 * 1024,
        }
    }
}

/// Every [`ResetReason`] the API defines.
///
/// A list the *error message* needs; the parse itself goes through the type.
/// [`reset_reason_is_listed`] is why it cannot fall behind the enum.
const RESET_REASONS: &[ResetReason] = &[
    ResetReason::PowerOn,
    ResetReason::External,
    ResetReason::Software,
    ResetReason::Panic,
    ResetReason::IntWdt,
    ResetReason::TaskWdt,
    ResetReason::Wdt,
    ResetReason::DeepSleep,
    ResetReason::Brownout,
    ResetReason::Sdio,
    ResetReason::Unknown,
];

/// Every [`FwSlot`] the API defines.
const FW_SLOTS: &[FwSlot] = &[FwSlot::Ota0, FwSlot::Ota1, FwSlot::Unknown];

/// Every [`FwState`] the API defines.
const FW_STATES: &[FwState] = &[
    FwState::New,
    FwState::PendingVerify,
    FwState::Valid,
    FwState::Invalid,
    FwState::Aborted,
    FwState::Undefined,
];

// The three lists above are the only place in this crate that enumerates the
// API's variants, and these three matches are exhaustive: a variant added to
// `screeny-device-api` stops this file compiling, which is the reminder to add
// it to the list. Nothing here spells a *name*; those come from serde.
const fn reset_reason_is_listed(r: ResetReason) -> bool {
    match r {
        ResetReason::PowerOn
        | ResetReason::External
        | ResetReason::Software
        | ResetReason::Panic
        | ResetReason::IntWdt
        | ResetReason::TaskWdt
        | ResetReason::Wdt
        | ResetReason::DeepSleep
        | ResetReason::Brownout
        | ResetReason::Sdio
        | ResetReason::Unknown => true,
    }
}

const fn fw_slot_is_listed(s: FwSlot) -> bool {
    match s {
        FwSlot::Ota0 | FwSlot::Ota1 | FwSlot::Unknown => true,
    }
}

const fn fw_state_is_listed(s: FwState) -> bool {
    match s {
        FwState::New
        | FwState::PendingVerify
        | FwState::Valid
        | FwState::Invalid
        | FwState::Aborted
        | FwState::Undefined => true,
    }
}

/// The serde name of one value, which is the name the flag takes and the name
/// the JSON carries. Asked of the type rather than written down here.
fn api_name<T: serde::Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(s)) => s,
        // Unreachable for these three enums, which are unit-variant enums with
        // `rename_all`. Not a panic: a CLI error message is not worth one.
        _ => String::new(),
    }
}

/// Parse one of the API's enum names **through the API's own type**, so the
/// simulator cannot accept a name the API does not have.
fn parse_api_name<T>(s: &str, flag: &str, all: &[T]) -> Result<T, String>
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    let name = s.trim();
    serde_json::from_value::<T>(serde_json::Value::String(name.to_string())).map_err(|_| {
        format!(
            "{flag}: unknown value {name:?}; valid values are {}",
            names(all).join(", ")
        )
    })
}

/// The serde names of a list of values, in the order the enum declares them.
fn names<T: serde::Serialize>(all: &[T]) -> Vec<String> {
    all.iter().map(api_name).collect()
}

impl Health {
    /// The names `--reset-reason` accepts, for help text and error messages.
    #[must_use]
    pub fn reset_reason_names() -> Vec<String> {
        debug_assert!(RESET_REASONS.iter().copied().all(reset_reason_is_listed));
        names(RESET_REASONS)
    }

    /// The names `--fw-slot` accepts.
    #[must_use]
    pub fn fw_slot_names() -> Vec<String> {
        debug_assert!(FW_SLOTS.iter().copied().all(fw_slot_is_listed));
        names(FW_SLOTS)
    }

    /// The names `--fw-state` accepts.
    #[must_use]
    pub fn fw_state_names() -> Vec<String> {
        debug_assert!(FW_STATES.iter().copied().all(fw_state_is_listed));
        names(FW_STATES)
    }

    /// `--reset-reason NAME`.
    ///
    /// # Errors
    ///
    /// A name [`ResetReason`] does not have, with every name it does have.
    pub fn parse_reset_reason(s: &str) -> Result<ResetReason, String> {
        parse_api_name(s, "--reset-reason", RESET_REASONS)
    }

    /// `--fw-slot NAME`.
    ///
    /// # Errors
    ///
    /// A name [`FwSlot`] does not have, with every name it does have.
    pub fn parse_fw_slot(s: &str) -> Result<FwSlot, String> {
        parse_api_name(s, "--fw-slot", FW_SLOTS)
    }

    /// `--fw-state NAME`.
    ///
    /// # Errors
    ///
    /// A name [`FwState`] does not have, with every name it does have.
    pub fn parse_fw_state(s: &str) -> Result<FwState, String> {
        parse_api_name(s, "--fw-state", FW_STATES)
    }

    /// One line naming all seven, in the API's own names, for the binary's
    /// banner. What is printed is what `GET /api/v1/status` will say.
    #[must_use]
    pub fn describe(&self) -> String {
        format!(
            "reset {} slot {} state {} store_errors {} heap {}/{} B stack_free {} B",
            api_name(&self.reset_reason),
            api_name(&self.fw_slot),
            api_name(&self.fw_state),
            self.store_errors,
            self.heap_used,
            self.heap_size,
            self.stack_free,
        )
    }

    /// Refuse a combination the device could not report.
    ///
    /// Only one of those exists: more heap in use than there is heap. It is a
    /// CLI error rather than a clamp because `screeny-probe`'s HTTP rule 5
    /// (`heap_used <= heap_size`) is a rule the simulator should be able to
    /// *pass*, and silently fixing the numbers up would hide the typo that
    /// produced them.
    ///
    /// # Errors
    ///
    /// If [`heap_used`](Self::heap_used) is greater than
    /// [`heap_size`](Self::heap_size).
    pub fn check(&self) -> Result<(), String> {
        if self.heap_used > self.heap_size {
            return Err(format!(
                "--heap-used {} is more than --heap-size {}: \
                 a device cannot use more heap than it has",
                self.heap_used, self.heap_size
            ));
        }
        Ok(())
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
    /// Boot straight into the captive portal: an empty store, which is what
    /// a factory-fresh device is.
    pub start_in_portal: bool,

    // --- card 192: the half of the status a host cannot measure -------------
    /// What `GET /api/v1/status` reports about the device's health.
    /// [`Health::default()`] is what it has always reported.
    pub health: Health,
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
            health: Health::default(),
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
