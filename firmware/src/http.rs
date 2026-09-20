//! The device's HTTP server: the status page and the JSON API (card 222).
//!
//! `picoserve 0.20` on TCP 80, on core 0's executor beside `frames_task`. The
//! shapes are [`screeny_device_api`]'s, so the browser, the simulator and the
//! Studio all read the same JSON; nothing about the wire format is spelled
//! twice.
//!
//! ## What this file is careful about
//!
//! * **The frame path is the product** (`docs/design/device-web.md`, decision
//!   7). No handler holds [`crate::net::CORE`] or [`crate::store::STORE`]
//!   across network I/O: it takes the lock, copies the numbers out, drops the
//!   guard, and only then builds a reply. Nothing here awaits a socket with a
//!   lock in hand.
//! * **RAM is the currency.** Everything a task holds across an `await` is
//!   `.bss`, and `.bss` comes straight out of core 0's stack, so the sizes at
//!   the top of this file are the whole budget and they are justified there.
//! * **The reply leaves before the thing happens.** `POST /api/v1/wifi` and
//!   `POST /api/v1/reboot` both do work that drops the connection that would
//!   have carried their answer, so they hand it to [`deferred_task`] and
//!   return. That is spec section 8.2's rule, which card 212 paid for.
//! * **Never the PSK** (spec section 8.4). The one route that receives one
//!   passes it to [`crate::store`] and to the radio and nowhere else; the log
//!   line says how long it was.
//!
//! ## Why the mutating routes go through the UDP control core
//!
//! `POST /api/v1/settings` and `POST /api/v1/identify` build a control
//! datagram and feed it to [`crate::receiver::Core::control`], the same
//! function the control socket calls. The brightness cap, the name
//! truncation, the idle-mode set, the `redraw` bump, the debounced store
//! write and the mDNS re-announce are then *one* implementation with two
//! front doors, rather than two that agree until somebody changes one.
//! `req_id` is 0, which spec section 6.1 defines as "no reply wanted", so the
//! core does the work and hands back no datagram.
//!
//! Out of scope here and named where they belong: the soft-AP, DHCP, DNS and
//! the captive-portal catch-all (card 223), `GET /api/v1/networks` (also 223,
//! it needs the scan), and `POST /api/v1/firmware` (card 240). The last two
//! answer `ErrorCode::Unavailable` today rather than 404, because the route
//! exists and the device is simply not able to serve it yet.

use core::fmt::Write as _;
use core::sync::atomic::{AtomicU32, AtomicU8, Ordering};

use embassy_futures::select::{select, Either};
use embassy_net::{IpAddress, IpEndpoint, Stack};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::once_lock::OnceLock;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer};
use log::{info, warn};
use picoserve::extract::FromRequest;
use picoserve::request::{RequestBody, RequestParts};
use picoserve::response::{Connection, IntoResponse, Response, ResponseWriter, StatusCode};
use picoserve::routing::{get, Router};
use picoserve::{ResponseSent, Server};
use screeny_device_api::reply::{
    AcceptedReply, SettingsReply, StatusReply, TelemetryReply, WifiReply,
};
use screeny_device_api::request::{
    IdentifyRequest, Mutating, RebootRequest, SettingsRequest, MIN_UNESCAPE_BUFFER,
};
use screeny_device_api::{
    form, route, text, ErrorCode, ErrorReply, FailReason, FwSlot, FwState, IdleMode, ResetReason,
    StreamState, WifiState,
};
use screeny_proto::control::Request as ControlRequest;
use screeny_settings::Wifi;

use crate::net::{CORE, INFO_CHANGED};
use crate::store;

// ---------------------------------------------------------------------------
// Sizes. This is the RAM budget, and it is the whole design decision.
// ---------------------------------------------------------------------------

/// How many connections are served at once.
///
/// **Two, since card 227 paid for the second one.** Card 222 wanted two and
/// could not have them: the pool is **7,504 bytes** of `.bss` per worker -
/// [`HTTP_BUF`] + [`TCP_RX`] + [`TCP_TX`] is 3,584 of that and picoserve's
/// `serve` future, which holds the router and whichever handler future is in
/// flight, is the other ~3,920 - and a second one put `.stack` at 15,344,
/// below the 16,384 floor. Card 227 bought the room back from core 1's stack
/// (measured, and it was three quarters empty) and the heap arena (measured
/// against the APSTA peak, not the station one), and spent part of it here.
///
/// What the second worker buys, measured over the wire by the orchestrator at
/// the end of card 222: smoltcp has **no listen backlog**, so with one worker
/// a SYN arriving between two `accept()`s is simply unanswered and the client
/// retransmits. macOS's first retransmit is at one second, and that is exactly
/// what back-to-back connections paid: `time_connect` 1.007 s against 25-37 ms
/// for a lone request. A browser that loads the page and then polls
/// `/api/v1/status` hits it every time. Two workers means one is listening
/// while the other answers.
///
/// **Keep-alive stays off.** picoserve's own documentation says to enable it
/// "only if multiple sockets are handling HTTP connections", which is now
/// true - but with *two* sockets, two browsers holding kept-alive connections
/// are still the whole server, and the page's 4 s poll would hold one open
/// indefinitely. Closing after each response bounds the worst wait to one
/// response time, which is a handful of milliseconds. Turning it on is a
/// one-line change and a measurement, not a guess; it is not this card's.
pub const HTTP_TASKS: usize = 2;

/// picoserve's own buffer: the request line, all the headers, and the whole
/// body of any route that parses one.
///
/// The body side is settled: `route::MAX_REQUEST_LEN` is 384 (the WiFi form),
/// and picoserve's body extractors need it contiguous in here. The header side
/// is what sets the number - a desktop Chrome `GET /` carries ~700 bytes of
/// `User-Agent`, `Accept`, `sec-ch-ua*` and `sec-fetch-*`. 1536 covers that
/// with room for the form on top, and a request that overruns it is answered
/// (`payload_too_large`) rather than dropped.
const HTTP_BUF: usize = 1536;
const _: () = assert!(HTTP_BUF >= route::MAX_REQUEST_LEN + 512);

/// smoltcp's receive buffer. One segment plus slack: every request this API
/// takes is smaller than a single MSS, and a body larger than [`HTTP_BUF`] is
/// refused anyway.
const TCP_RX: usize = 1024;

/// smoltcp's send buffer. The status page is the biggest reply and picoserve
/// streams it, so this is a window size and not a reply bound: 1024 is two
/// thirds of an MTU and keeps a page to a handful of segments.
const TCP_TX: usize = 1024;

/// The port. Fixed, because "open the device's address in a browser" is the
/// feature.
pub const HTTP_PORT: u16 = 80;

// ---------------------------------------------------------------------------
// The page
// ---------------------------------------------------------------------------

/// The whole status page: one file, inline CSS and JS, no external assets.
const PAGE: &str = include_str!("http_page.html");

/// Where the server-rendered status table goes. The page is split here at
/// request time so that a browser with JavaScript disabled still sees the
/// status as of page load; the script replaces the same table every few
/// seconds when it is enabled.
const PAGE_SPLIT: &str = "<!--STATUS-->";

const _: () = assert!(PAGE.len() < 8192, "the status page has got out of hand");

// ---------------------------------------------------------------------------
// What the server needs from `main`, and what it learns at boot
// ---------------------------------------------------------------------------

/// The handful of `&'static` things a handler cannot reach through an atomic.
///
/// Deliberately **not** the `embassy_net::Stack`: a `Stack<'d>` holds a
/// `&RefCell<Inner>` and is therefore not `Sync`, so it cannot live in a
/// `static`. The one thing a handler wants from it is the station's address,
/// and [`crate::net::frames_task`] already asks for that on every 20 ms tick
/// for the status screen - so it publishes it into [`IPV4`] on the way past
/// and the handlers read an atomic.
pub struct Ctx {
    /// The device id of spec section 5.1: the lowercase MAC suffix.
    pub id: &'static str,
    /// `screeny-<id>`, the mDNS host name.
    pub host: &'static str,
}

static CTX: OnceLock<Ctx> = OnceLock::new();

/// The station's IPv4 address as a big-endian `u32`, or 0 for "none yet".
///
/// Written by [`crate::net::frames_task`], read by the status routes. An
/// address is four bytes, so one atomic is the whole story and there is no
/// torn-read to reason about.
pub static IPV4: AtomicU32 = AtomicU32::new(0);

/// Publish the station's address. Called from the frame task's tick.
pub fn set_ipv4(octets: Option<[u8; 4]>) {
    IPV4.store(
        octets.map_or(0, u32::from_be_bytes),
        Ordering::Relaxed,
    );
}

/// A random number drawn once at boot. Same value as last time means the link
/// flapped; a different one means the device rebooted. `main` fills it from
/// the hardware RNG.
pub static BOOT_ID: AtomicU32 = AtomicU32::new(0);

/// `fw_slot`, as a [`FwSlot`] discriminant; [`FW_UNKNOWN`] until read.
static FW_SLOT: AtomicU8 = AtomicU8::new(FW_UNKNOWN);
/// `fw_state`, as a [`FwState`] discriminant; [`FW_UNKNOWN`] until read.
static FW_STATE: AtomicU8 = AtomicU8::new(FW_UNKNOWN);
const FW_UNKNOWN: u8 = 0xff;

/// Why the last join attempt failed, as a [`FailReason`] discriminant, or
/// [`FW_UNKNOWN`] for "no failure to report". Written by the WiFi task.
pub static WIFI_FAIL_REASON: AtomicU8 = AtomicU8::new(FW_UNKNOWN);

/// Record a join failure for `GET /api/v1/wifi`'s `reason`.
pub fn note_wifi_failure(reason: FailReason) {
    WIFI_FAIL_REASON.store(
        match reason {
            FailReason::Auth => 0,
            FailReason::NotFound => 1,
            FailReason::Other => 2,
        },
        Ordering::Relaxed,
    );
}

/// Forget any recorded join failure: a join that worked is not a failure.
pub fn clear_wifi_failure() {
    WIFI_FAIL_REASON.store(FW_UNKNOWN, Ordering::Relaxed);
}

fn wifi_failure() -> Option<FailReason> {
    match WIFI_FAIL_REASON.load(Ordering::Relaxed) {
        0 => Some(FailReason::Auth),
        1 => Some(FailReason::NotFound),
        2 => Some(FailReason::Other),
        _ => None,
    }
}

/// Read `otadata` once, at boot, and remember what it said.
///
/// Once rather than per request, for three reasons: nothing can change it
/// before card 241 ships the confirm/revert state machine; it needs the
/// `STORE` lock and a 3 KB partition-table buffer, neither of which belongs in
/// an HTTP handler; and a status request should not touch flash.
///
/// Call it from `main` after [`crate::store::init`] and before the panel is
/// lit - it is a read, and at that point core 1 is not running, so nothing is
/// parked.
pub async fn read_fw_health() {
    use esp_bootloader_esp_idf::ota::{Ota, OtaImageState};
    use esp_bootloader_esp_idf::partitions::{
        self, AppPartitionSubType, DataPartitionSubType, PartitionType, PARTITION_TABLE_MAX_LEN,
    };

    let mut guard = store::STORE.lock().await;
    let Some(f) = guard.as_mut() else {
        warn!("http: no flash handle - fw_slot and fw_state report unknown");
        return;
    };
    let flash = f.raw();

    // 3 KB, on `main`'s stack, once, before the framebuffers are lit and long
    // before any task future exists. It is deliberately not a static.
    let mut buf = [0u8; PARTITION_TABLE_MAX_LEN];
    let table = match partitions::read_partition_table(flash, &mut buf) {
        Ok(t) => t,
        Err(e) => {
            warn!("http: partition table unreadable ({:?}) - fw health is unknown", e);
            return;
        }
    };

    // The *booted* partition, not otadata's selection: after a rollback the
    // bootloader may run one while otadata still names the other.
    if let Ok(Some(p)) = table.booted_partition() {
        // By offset, from `firmware/partitions.csv`, and not by asking the
        // entry for its subtype: `PartitionEntry::partition_type()` `unwrap!`s
        // the conversion, and a panic in the boot path to put a word in a
        // status reply is a bad trade (research 006 section 3).
        let slot = match p.offset() {
            0x10000 => FwSlot::Ota0 as u8,
            0x210000 => FwSlot::Ota1 as u8,
            _ => FW_UNKNOWN,
        };
        FW_SLOT.store(slot, Ordering::Relaxed);
    }

    let Ok(Some(ota_part)) = table.find_partition(PartitionType::Data(DataPartitionSubType::Ota))
    else {
        warn!("http: no otadata partition - fw_state reports unknown");
        return;
    };
    let Ok(mut ota) = Ota::new(ota_part.as_flash_region(flash), 2) else {
        return;
    };
    // If the booted partition could not be read above, otadata's selection is
    // the next best answer and is right whenever no rollback happened.
    if FW_SLOT.load(Ordering::Relaxed) == FW_UNKNOWN
        && let Ok(sel) = ota.current_app_partition()
    {
        FW_SLOT.store(
            match sel {
                AppPartitionSubType::Ota0 => FwSlot::Ota0 as u8,
                AppPartitionSubType::Ota1 => FwSlot::Ota1 as u8,
                _ => FW_UNKNOWN,
            },
            Ordering::Relaxed,
        );
    }
    if let Ok(state) = ota.current_ota_state() {
        FW_STATE.store(
            match state {
                OtaImageState::New => FwState::New as u8,
                OtaImageState::PendingVerify => FwState::PendingVerify as u8,
                OtaImageState::Valid => FwState::Valid as u8,
                OtaImageState::Invalid => FwState::Invalid as u8,
                OtaImageState::Aborted => FwState::Aborted as u8,
                OtaImageState::Undefined => FwState::Undefined as u8,
            },
            Ordering::Relaxed,
        );
    }
    info!(
        "http: fw slot {:?} state {:?}",
        fw_slot(),
        fw_state()
    );
}

fn fw_slot() -> FwSlot {
    match FW_SLOT.load(Ordering::Relaxed) {
        x if x == FwSlot::Ota0 as u8 => FwSlot::Ota0,
        x if x == FwSlot::Ota1 as u8 => FwSlot::Ota1,
        _ => FwSlot::Unknown,
    }
}

fn fw_state() -> FwState {
    match FW_STATE.load(Ordering::Relaxed) {
        x if x == FwState::New as u8 => FwState::New,
        x if x == FwState::PendingVerify as u8 => FwState::PendingVerify,
        x if x == FwState::Valid as u8 => FwState::Valid,
        x if x == FwState::Invalid as u8 => FwState::Invalid,
        x if x == FwState::Aborted as u8 => FwState::Aborted,
        _ => FwState::Undefined,
    }
}

/// Why the chip last restarted.
///
/// The ESP32's reason register is coarser than [`ResetReason`]: a panic is a
/// software reset here (the backtrace on the serial log is the real evidence,
/// and card 243's RTC breadcrumb is the one that will survive a reboot), the
/// external reset pin reads as a power-on, and the interrupt and task
/// watchdogs are not told apart from the other timer-group watchdogs. Those
/// three variants of the enum are therefore never produced by this device.
fn reset_reason() -> ResetReason {
    use esp_hal::rtc_cntl::SocResetReason as R;
    use esp_hal::system::Cpu;
    match esp_hal::rtc_cntl::reset_reason(Cpu::ProCpu) {
        Some(R::ChipPowerOn) => ResetReason::PowerOn,
        Some(R::CoreSw | R::Cpu0Sw) => ResetReason::Software,
        Some(R::CoreDeepSleep) => ResetReason::DeepSleep,
        Some(R::CoreSdio) => ResetReason::Sdio,
        Some(R::SysBrownOut) => ResetReason::Brownout,
        Some(
            R::CoreMwdt0
            | R::CoreMwdt1
            | R::CpuMwdt0
            | R::CoreRtcWdt
            | R::Cpu0RtcWdt
            | R::SysRtcWdt,
        ) => ResetReason::Wdt,
        _ => ResetReason::Unknown,
    }
}

// ---------------------------------------------------------------------------
// One error shape for every failure
// ---------------------------------------------------------------------------

/// [`ErrorReply`] as a picoserve response, with the status
/// [`ErrorCode::status`] names. Every refusal on this server is one of these,
/// including the 404 for an unknown path and the 400 for malformed JSON, so a
/// caller never has to parse two kinds of error body.
pub struct ApiError(ErrorReply);

impl ApiError {
    fn new(code: ErrorCode) -> Self {
        Self(ErrorReply::new(code))
    }

    fn detail(code: ErrorCode, detail: &str) -> Self {
        Self(ErrorReply::with_detail(code, detail))
    }
}

impl IntoResponse for ApiError {
    async fn write_to<R: picoserve::io::Read, W: ResponseWriter<Error = R::Error>>(
        self,
        connection: Connection<'_, R>,
        response_writer: W,
    ) -> Result<ResponseSent, W::Error> {
        let status = StatusCode::new(self.0.status());
        picoserve::response::Json(self.0)
            .into_response()
            .with_status_code(status)
            .write_to(connection, response_writer)
            .await
    }
}

/// `Ok` in a JSON reply.
type Json<T> = picoserve::response::Json<T>;
/// What every JSON handler returns.
type Api<T> = Result<Json<T>, ApiError>;

// ---------------------------------------------------------------------------
// Extractors
// ---------------------------------------------------------------------------

/// A JSON request body, parsed with [`ErrorReply`]'s failures rather than
/// picoserve's.
///
/// picoserve's own `Json` extractor would do the parsing, but its rejection is
/// a plain-text body with picoserve's status code, and this API answers one
/// shape everywhere. It is also the place `MIN_UNESCAPE_BUFFER` belongs:
/// `serde_json_core::from_slice` **silently does not unescape strings**, so a
/// name posted as `café` would be stored with those six characters in it.
/// `from_slice_escaped` with a buffer as long as the longest string any
/// request holds is the fix, and naming the constant means raising
/// `MAX_NAME_LEN` raises the buffer.
struct ApiJson<T>(T);

impl<'r, State, T: serde::Deserialize<'r>> FromRequest<'r, State, ApiJson<T>> for ApiJson<T> {
    type Rejection = ApiError;

    async fn from_request<R: picoserve::io::Read>(
        _state: &'r State,
        _parts: RequestParts<'r>,
        body: RequestBody<'r, R>,
    ) -> Result<Self, ApiError> {
        let fits = body.entire_body_fits_into_buffer();
        let bytes = body.read_all().await.map_err(|_| {
            if fits {
                ApiError::detail(ErrorCode::BadRequest, "the body did not arrive")
            } else {
                ApiError::detail(ErrorCode::PayloadTooLarge, "the body is too long")
            }
        })?;
        serde_json_core::from_slice_escaped(bytes, &mut [0; MIN_UNESCAPE_BUFFER])
            .map(|(value, _)| ApiJson(value))
            .map_err(|_| ApiError::detail(ErrorCode::BadJson, "the body is not the expected JSON"))
    }
}

/// A raw request body, copied into an owned buffer.
///
/// `POST /api/v1/wifi` cannot use picoserve's `Form` extractor: it rejects a
/// body that is not UTF-8, and an 802.11 SSID is a byte string.
/// [`form::parse_wifi_form`] takes the bytes. Owned rather than borrowed
/// because a handler *function* may not borrow from the request (picoserve's
/// higher-ranked bound); [`form::MAX_FORM_LEN`] is 384 bytes.
struct RawForm(heapless::Vec<u8, { form::MAX_FORM_LEN }>);

impl<'r, State> FromRequest<'r, State, RawForm> for RawForm {
    type Rejection = ApiError;

    async fn from_request<R: picoserve::io::Read>(
        _state: &'r State,
        _parts: RequestParts<'r>,
        body: RequestBody<'r, R>,
    ) -> Result<Self, ApiError> {
        if body.content_length() > form::MAX_FORM_LEN {
            return Err(ApiError::detail(
                ErrorCode::PayloadTooLarge,
                form::FormError::TooLong.detail(),
            ));
        }
        let bytes = body
            .read_all()
            .await
            .map_err(|_| ApiError::detail(ErrorCode::BadRequest, "the body did not arrive"))?;
        let mut out = heapless::Vec::new();
        out.extend_from_slice(bytes).map_err(|_| {
            ApiError::detail(ErrorCode::PayloadTooLarge, form::FormError::TooLong.detail())
        })?;
        Ok(RawForm(out))
    }
}

// ---------------------------------------------------------------------------
// Reading the device's state
// ---------------------------------------------------------------------------

fn now_us() -> u64 {
    Instant::now().as_micros()
}

fn ctx() -> &'static Ctx {
    // `main` calls `init` before it spawns anything that can reach a handler,
    // so this is set by the time any of them runs.
    CTX.try_get().expect("http: ctx set before the tasks spawn")
}

/// The station's address, or `None` while DHCP is still out.
fn ip() -> Option<text::IpText> {
    match IPV4.load(Ordering::Relaxed) {
        0 => None,
        v => Some(text::ipv4_text(v.to_be_bytes())),
    }
}

/// The SSID as JSON text, or `None` when there is none.
///
/// [`crate::current_ssid`] is already lossily ASCII-folded for `GET_WIFI`,
/// which is a device-side compromise the spec makes; an empty one means "no
/// join has been attempted".
fn ssid() -> Option<text::SsidText> {
    let s = crate::current_ssid();
    if s.is_empty() {
        None
    } else {
        text::text(s)
    }
}

fn wifi_state() -> WifiState {
    WifiState::from_u8(crate::wifi_report_state()).unwrap_or(WifiState::Disconnected)
}

/// Everything `GET /api/v1/status` and the page's status table report.
///
/// The `CORE` lock is held for the three reads that need it and dropped before
/// anything is serialised, let alone written to a socket.
async fn status() -> StatusReply {
    let (name, idle_mode, state) = {
        let mut guard = CORE.lock().await;
        let core = guard.as_mut().expect("core exists");
        let t = core.telemetry(now_us());
        (
            text::text(core.name()).unwrap_or_default(),
            IdleMode::from(core.idle_mode()),
            StreamState::from_u8(t.state).unwrap_or(StreamState::Idle),
        )
    };
    let heap = esp_alloc::HEAP.stats();
    StatusReply {
        api: screeny_device_api::API_VERSION,
        id: text::text(ctx().id).unwrap_or_default(),
        name,
        fw: text::text(crate::FW_VERSION).unwrap_or_default(),
        boot_id: BOOT_ID.load(Ordering::Relaxed),
        uptime_ms: crate::now_ms(),
        heap_used: heap.current_usage as u32,
        heap_size: heap.size as u32,
        stack_free: crate::stack_probe::CORE0.headroom().unwrap_or(0) as u32,
        rssi_dbm: crate::RSSI_DBM.load(Ordering::Relaxed),
        brightness: crate::BRIGHTNESS.load(Ordering::Relaxed),
        idle_mode,
        wifi_state: wifi_state(),
        ssid: ssid(),
        ip: ip(),
        state,
        // Card 223 brings the soft-AP; until then the honest answer is "no".
        portal: false,
        fw_slot: fw_slot(),
        fw_state: fw_state(),
        reset_reason: reset_reason(),
        store_errors: store::FAILURES.load(Ordering::Relaxed),
    }
}

// ---------------------------------------------------------------------------
// Handing a request to the UDP control core
// ---------------------------------------------------------------------------

/// Where a synthetic control datagram claims to come from.
///
/// The address only reaches the control core's log events and its `GET_INFO`
/// rate limiter; no opcode this module sends touches the frame-source lock, so
/// a fixed local address cannot take the panel away from a streaming sender.
const FROM_HTTP: IpEndpoint = IpEndpoint::new(IpAddress::v4(127, 0, 0, 1), HTTP_PORT);

/// Longest synthetic request: `SET_NAME` is 8 header bytes + 1 + 32.
const CTL_MAX: usize = 48;

/// Run a list of control requests under one `CORE` lock, then perform whatever
/// flash write they asked for.
///
/// Order matters and is card 212's: take `CORE`, do the work, **drop it**,
/// then take `STORE`. Never both.
async fn apply_control(reqs: &[ControlRequest<'_>]) -> Result<(), ApiError> {
    let mut imm: Option<store::Immediate> = None;
    let info_changed = {
        let mut buf = [0u8; CTL_MAX];
        let mut reply = [0u8; CTL_MAX];
        let mut guard = CORE.lock().await;
        let core = guard.as_mut().expect("core exists");
        for req in reqs {
            // `req_id` 0 is section 6.1's "no reply wanted": the core carries
            // the request out and writes nothing back.
            let Ok(n) = req.write(0, &mut buf) else {
                return Err(ApiError::detail(
                    ErrorCode::Internal,
                    "could not encode the request",
                ));
            };
            core.control(now_us(), FROM_HTTP, &buf[..n], &mut reply, &mut imm);
        }
        core.take_info_changed()
    };

    if let Some(what) = imm
        && let Err(e) = store::commit_immediate(&what).await
    {
        warn!("http: storing the setting failed: {:?}", e);
        return Err(ApiError::detail(ErrorCode::Storage, "the write to flash failed"));
    }
    if info_changed {
        INFO_CHANGED.signal(());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The deferred half: what must happen *after* the reply is on the air
// ---------------------------------------------------------------------------

/// Credentials accepted by `POST /api/v1/wifi`, waiting for the reply to leave.
static WIFI_PENDING: Signal<CriticalSectionRawMutex, Wifi> = Signal::new();
/// A `POST /api/v1/reboot` that has been answered.
static REBOOT_PENDING: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// How long to wait before doing the thing the reply promised.
///
/// picoserve has written the response into smoltcp's send buffer by the time
/// the handler's future is done, but "written into the buffer" is not "on the
/// air": card 212 found a `SET_WIFI` reply that never left because the radio
/// was dropped in the same tick. The UDP path waits 100 ms; this waits longer
/// because a TCP reply also wants its ACK, and a browser that is told
/// `{"result":"rebooting"}` and then sees a reset connection reports an error
/// to the person watching.
const DEFERRED_MS: u64 = 400;

/// The one task that owns "reply first, then do it".
///
/// One task rather than two: both jobs are a signal, a short sleep and one
/// call, and an idle `select` costs less `.bss` than a second task future.
#[embassy_executor::task]
pub async fn deferred_task() {
    loop {
        match select(WIFI_PENDING.wait(), REBOOT_PENDING.wait()).await {
            Either::First(w) => {
                Timer::after(Duration::from_millis(DEFERRED_MS)).await;
                info!("http: applying the posted credentials");
                crate::NEW_WIFI.signal(w);
            }
            Either::Second(()) => {
                Timer::after(Duration::from_millis(DEFERRED_MS)).await;
                info!("http: REBOOT");
                esp_hal::system::software_reset();
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn get_status() -> Api<StatusReply> {
    Ok(picoserve::response::Json(status().await))
}

async fn get_telemetry() -> Api<TelemetryReply> {
    let t = {
        let mut guard = CORE.lock().await;
        let core = guard.as_mut().expect("core exists");
        core.telemetry(now_us())
    };
    Ok(picoserve::response::Json(TelemetryReply::from(&t)))
}

async fn get_wifi() -> Api<WifiReply> {
    let state = wifi_state();
    Ok(picoserve::response::Json(WifiReply {
        state,
        ssid: ssid(),
        ip: ip(),
        // Spec-shaped: `reason` is meaningful only for `failed`, and the crate
        // documents it as explicitly `null` otherwise.
        reason: (state == WifiState::Failed).then(wifi_failure).flatten(),
    }))
}

/// `POST /api/v1/wifi`: urlencoded, and the reply leaves before the radio work.
async fn post_wifi(RawForm(body): RawForm) -> Api<AcceptedReply> {
    let form = form::parse_wifi_form(&body).map_err(|e| ApiError(e.reply()))?;
    screeny_device_api::request::check_auth(form.auth()).map_err(ApiError::new)?;
    // The one line in this module that mentions the secret, and it says how
    // long it is (spec section 8.4).
    info!(
        "http: set-wifi for a {}-byte SSID, psk_len {}",
        form.ssid().len(),
        form.psk_len()
    );
    let wifi = Wifi::new(form.ssid(), form.psk()).map_err(|e| {
        warn!("http: set-wifi refused: {:?}", e);
        ApiError::detail(ErrorCode::OutOfRange, "those credentials do not fit")
    })?;

    // Written to flash *before* the reply, exactly as `SET_WIFI` is: a store
    // failure is then an honest `storage`, and nothing is attempted with
    // credentials that would be gone at the next boot. The write does not drop
    // the connection; the join does, which is why the join is deferred.
    store::commit_immediate(&store::Immediate::Wifi {
        wifi: wifi.clone(),
        persist: true,
    })
    .await
    .map_err(|e| {
        warn!("http: storing the credentials failed: {:?}", e);
        ApiError::detail(ErrorCode::Storage, "the write to flash failed")
    })?;

    WIFI_PENDING.signal(wifi);
    Ok(picoserve::response::Json(AcceptedReply::TRYING))
}

/// `POST /api/v1/settings`: the same three opcodes the control port takes.
async fn post_settings(ApiJson(req): ApiJson<SettingsRequest>) -> Api<SettingsReply> {
    req.check_auth().map_err(ApiError::new)?;

    // `name: ""` means "go back to `screeny-<id>`" (the crate documents it, and
    // the page's field says so). The UDP `SET_NAME` has no such rule - it takes
    // the string literally, and an empty one would leave the receiver, the
    // `GET_INFO` body and the mDNS instance name all blank - so the
    // substitution happens here, before the opcode is built, rather than in the
    // shared state machine where it would change the wire protocol's meaning.
    let default_name = ctx().host;
    let mut reqs: heapless::Vec<ControlRequest<'_>, 3> = heapless::Vec::new();
    if let Some(name) = req.name.as_deref() {
        let _ = reqs.push(ControlRequest::SetName(if name.is_empty() {
            default_name
        } else {
            name
        }));
    }
    if let Some(b) = req.brightness {
        let _ = reqs.push(ControlRequest::SetBrightness(b));
    }
    if let Some(m) = req.idle_mode {
        let _ = reqs.push(ControlRequest::SetIdle(m.into()));
    }
    apply_control(&reqs).await?;

    // The reply is the whole settings state, not an echo: a caller that set
    // only the brightness still learns the name, and one whose brightness was
    // capped learns the capped value.
    let (name, idle_mode) = {
        let guard = CORE.lock().await;
        let core = guard.as_ref().expect("core exists");
        (
            text::text(core.name()).unwrap_or_default(),
            IdleMode::from(core.idle_mode()),
        )
    };
    Ok(picoserve::response::Json(SettingsReply {
        name,
        brightness: crate::BRIGHTNESS.load(Ordering::Relaxed),
        idle_mode,
    }))
}

async fn post_identify(ApiJson(req): ApiJson<IdentifyRequest>) -> Api<AcceptedReply> {
    req.check_auth().map_err(ApiError::new)?;
    let duration_ms = req.duration_u16().map_err(|c| {
        ApiError::detail(c, "duration_ms is longer than the protocol can carry")
    })?;
    apply_control(&[ControlRequest::Identify { duration_ms }]).await?;
    Ok(picoserve::response::Json(AcceptedReply::IDENTIFYING))
}

async fn post_reboot(ApiJson(req): ApiJson<RebootRequest>) -> Api<AcceptedReply> {
    req.check_auth().map_err(ApiError::new)?;
    if !req.confirmed() {
        return Err(ApiError::detail(
            ErrorCode::BadRequest,
            "confirm must be \"RBOO\"",
        ));
    }
    REBOOT_PENDING.signal(());
    Ok(picoserve::response::Json(AcceptedReply::REBOOTING))
}

/// A route that exists in the API but not yet on this device.
///
/// `screeny-device-api` has no `not_implemented` code and should not grow one
/// for this: `unavailable` (503) is exactly "understood, and the device cannot
/// serve it in this state", and a caller retrying later is the right
/// behaviour for both of these. `GET /api/v1/networks` needs the scan card 223
/// brings; `POST /api/v1/firmware` is card 240.
async fn not_yet() -> Api<()> {
    Err(ApiError::detail(
        ErrorCode::Unavailable,
        "this firmware does not serve that route yet",
    ))
}

/// The answer to `GET` on a `POST`-only route and vice versa.
///
/// picoserve's built-in `MethodNotAllowed` writes a plain-text body; this
/// keeps every failure on the API one shape.
async fn wrong_method() -> Api<()> {
    Err(ApiError::new(ErrorCode::MethodNotAllowed))
}

// ---------------------------------------------------------------------------
// GET /: the page
// ---------------------------------------------------------------------------

/// Counts formatted bytes, for `Content::content_length`.
struct Counter(usize);

impl core::fmt::Write for Counter {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        self.0 += s.len();
        Ok(())
    }
}

/// The page with its status table already filled in.
///
/// Server-rendered rather than fetched, so that the page **works with
/// JavaScript disabled**: without it you see the status as of page load, with
/// it the script replaces the same rows every few seconds. picoserve measures
/// a response into a counting writer and then streams it, so nothing here
/// needs a buffer.
struct Page {
    head: &'static str,
    tail: &'static str,
    st: StatusReply,
}

impl Page {
    fn new(st: StatusReply) -> Self {
        let (head, tail) = PAGE.split_once(PAGE_SPLIT).unwrap_or((PAGE, ""));
        Page { head, tail, st }
    }
}

/// One `<tr>` per field, with a `data-k` the script can find again.
fn row(f: &mut impl core::fmt::Write, key: &str, value: core::fmt::Arguments<'_>) -> core::fmt::Result {
    write!(f, "<tr><th>{key}</th><td data-k=\"{key}\">{value}</td></tr>")
}

impl core::fmt::Display for Page {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let s = &self.st;
        f.write_str(self.head)?;
        row(f, "name", format_args!("{}", s.name))?;
        row(f, "id", format_args!("{}", s.id))?;
        row(f, "host", format_args!("{}.local", ctx().host))?;
        row(f, "fw", format_args!("{}", s.fw))?;
        row(f, "state", format_args!("{}", as_word(s.state)))?;
        row(f, "wifi", format_args!("{}", wifi_word(s.wifi_state)))?;
        row(
            f,
            "ssid",
            format_args!("{}", s.ssid.as_deref().unwrap_or("-")),
        )?;
        row(f, "ip", format_args!("{}", s.ip.as_deref().unwrap_or("-")))?;
        row(f, "rssi", format_args!("{} dBm", s.rssi_dbm))?;
        row(f, "brightness", format_args!("{}", s.brightness))?;
        row(f, "idle_mode", format_args!("{}", idle_word(s.idle_mode)))?;
        row(f, "uptime", format_args!("{} s", s.uptime_ms / 1000))?;
        row(f, "heap", format_args!("{} / {}", s.heap_used, s.heap_size))?;
        row(f, "stack_free", format_args!("{}", s.stack_free))?;
        row(f, "store_errors", format_args!("{}", s.store_errors))?;
        row(f, "boot_id", format_args!("{:08x}", s.boot_id))?;
        row(
            f,
            "firmware",
            format_args!("{} / {}", slot_word(s.fw_slot), state_word(s.fw_state)),
        )?;
        row(f, "reset", format_args!("{}", reset_word(s.reset_reason)))?;
        f.write_str(self.tail)
    }
}

impl picoserve::response::Content for Page {
    fn content_type(&self) -> &'static str {
        "text/html; charset=utf-8"
    }

    fn content_length(&self) -> usize {
        let mut c = Counter(0);
        let _ = write!(c, "{self}");
        c.0
    }

    async fn write_content<W: picoserve::io::Write>(self, mut writer: W) -> Result<(), W::Error> {
        // picoserve may format this more than once (once per send-buffer
        // chunk), which is safe because the rendering is a pure function of
        // the `StatusReply` captured when the request arrived.
        writer.write_fmt(format_args!("{self}")).await
    }
}

async fn get_page() -> impl IntoResponse {
    Response::ok(Page::new(status().await))
}

// The enums serialise through serde, which is not reachable from a `Display`
// impl without a writer; these are the same strings, and the page is the only
// caller.
const fn as_word(s: StreamState) -> &'static str {
    match s {
        StreamState::Idle => "idle",
        StreamState::Live => "live",
        StreamState::Hold => "hold",
        StreamState::Identify => "identify",
        StreamState::Provisioning => "provisioning",
    }
}

const fn wifi_word(s: WifiState) -> &'static str {
    match s {
        WifiState::Disconnected => "disconnected",
        WifiState::Connecting => "connecting",
        WifiState::Connected => "connected",
        WifiState::Failed => "failed",
    }
}

const fn idle_word(m: IdleMode) -> &'static str {
    match m {
        IdleMode::Status => "status",
        IdleMode::HoldForever => "hold_forever",
        IdleMode::Dim => "dim",
        IdleMode::Black => "black",
    }
}

const fn slot_word(s: FwSlot) -> &'static str {
    match s {
        FwSlot::Ota0 => "ota_0",
        FwSlot::Ota1 => "ota_1",
        FwSlot::Unknown => "unknown",
    }
}

const fn state_word(s: FwState) -> &'static str {
    match s {
        FwState::New => "new",
        FwState::PendingVerify => "pending_verify",
        FwState::Valid => "valid",
        FwState::Invalid => "invalid",
        FwState::Aborted => "aborted",
        FwState::Undefined => "undefined",
    }
}

const fn reset_word(r: ResetReason) -> &'static str {
    match r {
        ResetReason::PowerOn => "power_on",
        ResetReason::External => "external",
        ResetReason::Software => "software",
        ResetReason::Panic => "panic",
        ResetReason::IntWdt => "int_wdt",
        ResetReason::TaskWdt => "task_wdt",
        ResetReason::Wdt => "wdt",
        ResetReason::DeepSleep => "deep_sleep",
        ResetReason::Brownout => "brownout",
        ResetReason::Sdio => "sdio",
        ResetReason::Unknown => "unknown",
    }
}

// ---------------------------------------------------------------------------
// Unknown paths
// ---------------------------------------------------------------------------

/// The router's fallback: a 404 in the same shape as every other failure.
///
/// Card 223 replaces this with the captive-portal redirect **for the AP stack
/// only**; on the LAN a 404 stays a 404.
struct NotFoundJson;

impl<State, PathParameters> picoserve::routing::PathRouterService<State, PathParameters>
    for NotFoundJson
{
    async fn call_path_router_service<
        R: picoserve::io::Read,
        W: ResponseWriter<Error = R::Error>,
    >(
        &self,
        _state: &State,
        _path_parameters: PathParameters,
        _path: picoserve::request::Path<'_>,
        request: picoserve::request::Request<'_, R>,
        response_writer: W,
    ) -> Result<ResponseSent, W::Error> {
        ApiError::new(ErrorCode::NotFound)
            .write_to(request.body_connection.finalize().await?, response_writer)
            .await
    }
}

// ---------------------------------------------------------------------------
// The server
// ---------------------------------------------------------------------------

/// Called by `main` before the tasks are spawned.
pub fn init(ctx: Ctx) {
    if CTX.init(ctx).is_err() {
        warn!("http: ctx initialised twice");
    }
}

/// One connection worker. [`HTTP_TASKS`] of them share the port.
/// The route table, in one place.
///
/// Returned rather than declared in a `static`: naming this type needs
/// `#![feature(impl_trait_in_assoc_type)]` (that is what picoserve's
/// `AppBuilder` is for, and its own docs say it "requires the nightly Rust
/// toolchain"), and this firmware does not gate on nightly for a router. A
/// function keeps the table from being written twice - the `http-selftest`
/// build runs the same one over an in-memory socket.
///
/// Every path that exists answers the JSON error shape for the method it does
/// not take, rather than picoserve's plain-text `MethodNotAllowed`.
fn router() -> Router<impl picoserve::routing::PathRouter> {
    Router::from_service(NotFoundJson)
        .route("/", get(get_page))
        .route(route::STATUS, get(get_status).post(wrong_method))
        .route(route::TELEMETRY, get(get_telemetry).post(wrong_method))
        .route(route::NETWORKS, get(not_yet).post(wrong_method))
        .route(route::WIFI, get(get_wifi).post(post_wifi))
        .route(route::SETTINGS, get(wrong_method).post(post_settings))
        .route(route::FIRMWARE, get(wrong_method).post(not_yet))
        .route(route::REBOOT, get(wrong_method).post(post_reboot))
        .route(route::IDENTIFY, get(wrong_method).post(post_identify))
}

#[embassy_executor::task(pool_size = HTTP_TASKS)]
pub async fn http_task(id: usize, stack: Stack<'static>) -> ! {
    let app = router();

    // A stalled client must not be able to pin the one worker, so every phase
    // has a deadline: 3 s to send a request line at all, 5 s to finish a
    // request that has started, 5 s for the reply to be accepted. The worst a
    // client can hold the server for is therefore ~8 s, and that needs it to
    // have connected and then gone quiet mid-header.
    //
    // **Not** `keep_connection_alive()`, even with [`HTTP_TASKS`] at two: a
    // kept-alive connection is one of the two workers owned by one client
    // until its idle timeout, and the status page polls every four seconds,
    // which would hold one open for as long as the tab is. Closing after each
    // response is what bounds the worst wait to one response time.
    // `persistent_start_read_request` is therefore unused; it is left at a
    // short value so that turning keep-alive on stays a one-line change.
    let config = picoserve::Config::new(picoserve::Timeouts {
        start_read_request: Duration::from_secs(3),
        persistent_start_read_request: Duration::from_secs(2),
        read_request: Duration::from_secs(5),
        write: Duration::from_secs(5),
    })
    .close_connection_after_response();

    // Task locals, **not** `mk_static!`. A `StaticCell` declared inside a
    // `pool_size > 1` task is one cell shared by every instance of it: the
    // second worker's `uninit()` panics ("already full"), and had it not
    // panicked the two would have been scribbling on one buffer. Locals held
    // across the `await` land in the task's own future, which is per instance
    // and is `.bss` either way - the same bytes, correctly divided.
    let mut http_buf = [0u8; HTTP_BUF];
    let mut rx = [0u8; TCP_RX];
    let mut tx = [0u8; TCP_TX];

    info!("net: http on tcp/{} (worker {})", HTTP_PORT, id);
    Server::new(&app, &config, &mut http_buf[..])
        .listen_and_serve(id, stack, HTTP_PORT, &mut rx[..], &mut tx[..])
        .await
        .into_never()
}

// ---------------------------------------------------------------------------
// The bench self-test (feature `http-selftest`)
// ---------------------------------------------------------------------------

/// A picoserve socket that is two byte slices.
///
/// The TCP half of the self-test cannot work on this bench - a WiFi station
/// has no loopback and the access point does not hairpin a frame back to its
/// sender, which is exactly what the first run measured. But "can the device
/// reach itself over TCP" is not the question the card is really asking;
/// "does every route answer the right thing on the real device" is, and that
/// only needs [`picoserve::Server::serve`], which takes any
/// [`picoserve::io::Socket`]. So this is one: it hands the server a canned
/// request and keeps the bytes it writes back.
///
/// What it does *not* prove is the TCP path. The accept loop being up, plus
/// the orchestrator's `curl` run after the merge, cover that.
#[cfg(feature = "http-selftest")]
mod mem_socket {
    // **`picoserve::io::`, not `embedded_io_async::`.** There are two versions
    // of that crate in this binary - picoserve pins 0.6.1 and embassy-net
    // 0.7.0 - so the `Read`/`Write` a picoserve socket must implement are the
    // ones picoserve re-exports, and nothing else will satisfy the bound.
    // `BaseWrite` is `embedded_io_async::Write`; `picoserve::io::Write` is
    // picoserve's own trait on top of it, and both are needed.
    use picoserve::io::{BaseWrite, ErrorType, Read};
    use picoserve::mem::{BorrowedBuffer, BorrowedCursor};

    /// The request, handed out a few bytes at a time.
    pub struct Reader<'a> {
        pub data: &'a [u8],
    }

    /// The reply, kept. `len` and `overflow` are borrowed from the caller so
    /// that they are still readable once `serve` has consumed the socket.
    pub struct Writer<'a> {
        pub buf: &'a mut [u8],
        pub len: &'a mut usize,
        /// Bytes that did not fit. Reported, never silently dropped.
        pub overflow: &'a mut usize,
    }

    pub struct MemSocket<'a> {
        pub r: Reader<'a>,
        pub w: Writer<'a>,
    }

    impl ErrorType for Reader<'_> {
        type Error = core::convert::Infallible;
    }

    impl Read for Reader<'_> {
        async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
            let n = buf.len().min(self.data.len());
            buf[..n].copy_from_slice(&self.data[..n]);
            self.data = &self.data[n..];
            Ok(n)
        }
    }

    impl ErrorType for Writer<'_> {
        type Error = core::convert::Infallible;
    }

    impl Writer<'_> {
        fn append(&mut self, bytes: &[u8]) {
            let at = *self.len;
            let n = (self.buf.len() - at).min(bytes.len());
            self.buf[at..at + n].copy_from_slice(&bytes[..n]);
            *self.len += n;
            *self.overflow += bytes.len() - n;
        }
    }

    impl BaseWrite for Writer<'_> {
        async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
            self.append(buf);
            Ok(buf.len())
        }

        async fn flush(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    impl picoserve::io::Write for Writer<'_> {
        // The same shape as picoserve's own test writer: lend a scratch
        // cursor, then copy whatever the caller filled.
        async fn write_with<F: FnOnce(BorrowedCursor<'_>) -> R, R>(
            &mut self,
            f: F,
        ) -> Result<R, Self::Error> {
            let mut scratch = [0u8; 256];
            let mut buffer = BorrowedBuffer::new(&mut scratch);
            let out = f(buffer.unfilled());
            let filled = buffer.filled().len();
            let mut copy = [0u8; 256];
            copy[..filled].copy_from_slice(buffer.filled());
            self.append(&copy[..filled]);
            Ok(out)
        }
    }

    impl<'a, Runtime> picoserve::io::Socket<Runtime> for MemSocket<'a> {
        type Error = core::convert::Infallible;
        type ReadHalf<'b>
            = &'b mut Reader<'a>
        where
            Self: 'b;
        type WriteHalf<'b>
            = &'b mut Writer<'a>
        where
            Self: 'b;

        fn split(&mut self) -> (Self::ReadHalf<'_>, Self::WriteHalf<'_>) {
            (&mut self.r, &mut self.w)
        }

        async fn abort<T: picoserve::time::Timer<Runtime>>(
            self,
            _timeouts: &picoserve::Timeouts,
            _timer: &T,
        ) -> Result<(), picoserve::Error<Self::Error>> {
            Ok(())
        }

        async fn shutdown<T: picoserve::time::Timer<Runtime>>(
            self,
            _timeouts: &picoserve::Timeouts,
            _timer: &T,
        ) -> Result<(), picoserve::Error<Self::Error>> {
            Ok(())
        }
    }
}

/// A reply body with the station's SSID taken out of it.
///
/// `GET /api/v1/status` and `GET /api/v1/wifi` both carry the SSID, which is
/// right for the reply and wrong for the serial log: the firmware says an SSID
/// out loud in exactly one place (the WiFi task's "connected" line), and a
/// bench feature must not become a second. The first run of this self-test did
/// print it, which is how this exists.
#[cfg(feature = "http-selftest")]
struct Redacted<'a>(&'a str);

#[cfg(feature = "http-selftest")]
impl core::fmt::Display for Redacted<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let secret = crate::current_ssid();
        if secret.is_empty() {
            return f.write_str(self.0);
        }
        let mut rest = self.0;
        while let Some(i) = rest.find(secret) {
            f.write_str(&rest[..i])?;
            f.write_str("<ssid>")?;
            rest = &rest[i + secret.len()..];
        }
        f.write_str(rest)
    }
}

/// Cut a string to at most `max` bytes, on a character boundary.
#[cfg(feature = "http-selftest")]
fn clip(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Run one canned request through the real router and report what came back.
///
/// Returns `(status, body_len, micros)`.
#[cfg(feature = "http-selftest")]
async fn selftest_one(request: &str, out: &mut [u8]) -> (u16, usize, u32) {
    let app = router();
    let config = picoserve::Config::new(picoserve::Timeouts {
        start_read_request: Duration::from_secs(3),
        persistent_start_read_request: Duration::from_secs(2),
        read_request: Duration::from_secs(5),
        write: Duration::from_secs(5),
    })
    .close_connection_after_response();

    let mut written = 0usize;
    let mut overflow = 0usize;
    let t0 = Instant::now();
    {
        // Not `HTTP_BUF`: the canned requests below are ~150 bytes of request
        // line, headers and body, and this buffer is `.bss` in a build that
        // already carries a second copy of the whole serve machinery.
        let mut http_buf = [0u8; 512];
        let socket = mem_socket::MemSocket {
            r: mem_socket::Reader {
                data: request.as_bytes(),
            },
            w: mem_socket::Writer {
                buf: &mut out[..],
                len: &mut written,
                overflow: &mut overflow,
            },
        };
        let _ = Server::new(&app, &config, &mut http_buf[..])
            .serve(socket)
            .await;
    }
    let us = t0.elapsed().as_micros() as u32;

    // "HTTP/1.1 200 OK": the code is characters 9..12 of the status line.
    let status = core::str::from_utf8(&out[..written.min(16)])
        .ok()
        .and_then(|s| s.get(9..12))
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);
    (status, written + overflow, us)
}

/// Ask the device to make the request a worker cannot make.
///
/// Off by default. See the feature's comment in `Cargo.toml` for why it exists
/// and what its fallback is.
#[cfg(feature = "http-selftest")]
#[embassy_executor::task]
pub async fn selftest_task(stack: Stack<'static>) {
    use embassy_net::tcp::TcpSocket;
    use embedded_io_async::Write as _;

    // Three, not the forty the card suggested: the first run measured that a
    // station interface has no route to its own address, and repeating a
    // three-second timeout thirty-nine more times proves nothing. The
    // in-memory pass below is where the requests actually happen.
    const REQUESTS: usize = 3;
    const REQ: &[u8] =
        b"GET /api/v1/status HTTP/1.1\r\nHost: selftest\r\nConnection: close\r\n\r\n";

    // After the telemetry task's 60 s `stack:` line, not before it: that line
    // is the baseline this run is compared against, and it should be measured
    // with the server idle.
    Timer::after(Duration::from_secs(75)).await;
    let Some(cfg) = stack.config_v4() else {
        warn!("selftest: no address, nothing to connect to");
        return;
    };
    let me = cfg.address.address();
    let target = IpEndpoint::new(IpAddress::Ipv4(me), HTTP_PORT);

    // The window this is measured over: the frame path either noticed or it
    // did not, and these are the numbers that say which.
    let before = {
        let mut guard = CORE.lock().await;
        guard.as_mut().expect("core exists").telemetry(now_us())
    };
    crate::RENDER_US_MAX_WINDOW.store(0, Ordering::Relaxed);
    let t_window = Instant::now();

    let mut rx = [0u8; 256];
    let mut tx = [0u8; 192];
    let mut buf = [0u8; 256];
    let mut ok = 0usize;
    let mut failed = 0usize;
    let mut bytes_total = 0usize;
    let mut us_max = 0u32;
    let mut us_total = 0u64;
    let mut first_status: heapless::String<16> = heapless::String::new();

    for i in 0..REQUESTS {
        let t0 = Instant::now();
        let mut sock = TcpSocket::new(stack, &mut rx, &mut tx);
        sock.set_timeout(Some(Duration::from_secs(3)));
        let r = async {
            embassy_time::with_timeout(Duration::from_secs(3), sock.connect(target))
                .await
                .map_err(|_| ())?
                .map_err(|_| ())?;
            sock.write_all(REQ).await.map_err(|_| ())?;
            let mut n = 0usize;
            loop {
                match sock.read(&mut buf[n..]).await {
                    Ok(0) => break,
                    Ok(k) => {
                        n += k;
                        if n == buf.len() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            Ok::<usize, ()>(n)
        }
        .await;
        sock.close();

        match r {
            Ok(n) if n > 12 => {
                let us = t0.elapsed().as_micros() as u32;
                us_max = us_max.max(us);
                us_total += us as u64;
                bytes_total += n;
                ok += 1;
                if first_status.is_empty() {
                    let line = core::str::from_utf8(&buf[..n.min(16)]).unwrap_or("");
                    let _ = first_status.push_str(line.trim_end());
                }
            }
            _ => failed += 1,
        }

        if i == 0 && failed == 1 {
            // A WiFi station has no loopback and an access point need not
            // hairpin a frame back to its sender, so this is the expected
            // outcome and not a firmware fault. Say so, and fall back.
            warn!(
                "selftest: embassy-net cannot reach its own address {} - no loopback on a station interface. Falling back to reporting readiness.",
                me
            );
            break;
        }
    }

    info!(
        "selftest: over TCP - {} ok, {} failed of {} | first status line {:?} | {} bytes | {} us mean, {} us max",
        ok,
        failed,
        REQUESTS,
        first_status.as_str(),
        bytes_total,
        if ok > 0 { (us_total / ok as u64) as u32 } else { 0 },
        us_max,
    );
    if ok == 0 {
        info!(
            "selftest: fallback evidence - {} accept loop(s) listening on tcp/{}, stack address {}",
            HTTP_TASKS, HTTP_PORT, me
        );
    }

    // --- the router, exercised without a network ---------------------------
    //
    // "Can the device reach itself over TCP" is not the question; "does every
    // route answer the right thing on the real device" is, and that needs only
    // a `picoserve::io::Socket`. This one is two byte slices, so every route
    // below is the real router, the real handlers, the real locks and the real
    // JSON, running while the Studio streams.
    // 640 bytes: every JSON reply fits (the largest, `status`, is under 450).
    // `GET /` does not - the page is ~5.5 KB - and that is fine: the status
    // line is what is being checked and the overflow is counted and reported
    // rather than silently dropped.
    let mut out = [0u8; 640];
    let mut worst_us = 0u32;
    let hw_before = crate::stack_probe::CORE0.high_water().unwrap_or(0);
    for (label, request, expect) in SELFTEST_ROUTES {
        let (status, bytes, us) = selftest_one(request, &mut out).await;
        worst_us = worst_us.max(us);
        let body = core::str::from_utf8(&out[..bytes.min(out.len())])
            .ok()
            .and_then(|s| s.split_once("\r\n\r\n").map(|(_, b)| b))
            .unwrap_or("<binary>");
        let ok = status == *expect;
        info!(
            "selftest: {:<26} -> {} (want {}) {} {} bytes, {} us | {}",
            label,
            status,
            expect,
            if ok { "OK  " } else { "WRONG" },
            bytes,
            us,
            Redacted(clip(body, 140)),
        );
        // Let the frame task and the display breathe between requests, so the
        // window below measures a server answering beside a stream rather than
        // a server monopolising the executor.
        Timer::after(Duration::from_millis(200)).await;
    }
    // **The number this build exists for as much as the status codes.** Until
    // now nothing had ever run a request through the router on the device, so
    // the 60 s `stack:` line had never seen the HTTP path's depth. This is the
    // before/after across the whole route table.
    info!(
        "selftest: slowest in-memory request {} us | core 0 stack high-water {} -> {} of {} bytes",
        worst_us,
        hw_before,
        crate::stack_probe::CORE0.high_water().unwrap_or(0),
        crate::stack_probe::CORE0.size(),
    );

    let after = {
        let mut guard = CORE.lock().await;
        guard.as_mut().expect("core exists").telemetry(now_us())
    };
    let secs = (t_window.elapsed().as_millis() as u32 / 1000).max(1);
    info!(
        "selftest: over the window - {} fps rx, {} fps shown, decode drops {} (was {}), rejected {} (was {}), render max {} us",
        after.frames_rx.wrapping_sub(before.frames_rx) / secs,
        after.frames_shown.wrapping_sub(before.frames_shown) / secs,
        after.frames_dropped_decode,
        before.frames_dropped_decode,
        after.frames_rejected,
        before.frames_rejected,
        crate::RENDER_US_MAX_WINDOW.load(Ordering::Relaxed),
    );
}

/// Every route, with the status code it must answer.
///
/// Deliberately includes the three that are refusals - an unknown path, a
/// method a route does not take, and a route this firmware cannot serve yet -
/// because those are the ones a hand test forgets. The mutating requests use
/// values that change nothing observable: the brightness and idle mode that
/// are already in force cannot be known here, so `identify` asks for 1 ms and
/// `settings` sets the brightness the device is already at.
#[cfg(feature = "http-selftest")]
static SELFTEST_ROUTES: &[(&str, &str, u16)] = &[
    (
        "GET /",
        "GET / HTTP/1.1\r\nHost: s\r\nConnection: close\r\n\r\n",
        200,
    ),
    (
        "GET /api/v1/status",
        "GET /api/v1/status HTTP/1.1\r\nHost: s\r\nConnection: close\r\n\r\n",
        200,
    ),
    (
        "GET /api/v1/telemetry",
        "GET /api/v1/telemetry HTTP/1.1\r\nHost: s\r\nConnection: close\r\n\r\n",
        200,
    ),
    (
        "GET /api/v1/wifi",
        "GET /api/v1/wifi HTTP/1.1\r\nHost: s\r\nConnection: close\r\n\r\n",
        200,
    ),
    (
        "GET /api/v1/networks",
        "GET /api/v1/networks HTTP/1.1\r\nHost: s\r\nConnection: close\r\n\r\n",
        503,
    ),
    (
        "POST /api/v1/identify",
        "POST /api/v1/identify HTTP/1.1\r\nHost: s\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: 17\r\n\r\n{\"duration_ms\":1}",
        200,
    ),
    (
        "POST /api/v1/settings",
        "POST /api/v1/settings HTTP/1.1\r\nHost: s\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: 18\r\n\r\n{\"brightness\":96}\n",
        200,
    ),
    (
        "POST /api/v1/settings bad",
        "POST /api/v1/settings HTTP/1.1\r\nHost: s\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: 7\r\n\r\n{nope:}",
        400,
    ),
    (
        "POST /api/v1/reboot unconf",
        "POST /api/v1/reboot HTTP/1.1\r\nHost: s\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: 16\r\n\r\n{\"confirm\":\"NO\"}",
        400,
    ),
    (
        "POST /api/v1/firmware",
        "POST /api/v1/firmware HTTP/1.1\r\nHost: s\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
        503,
    ),
    (
        "GET /api/v1/settings (405)",
        "GET /api/v1/settings HTTP/1.1\r\nHost: s\r\nConnection: close\r\n\r\n",
        405,
    ),
    (
        "GET /nope (404)",
        "GET /nope HTTP/1.1\r\nHost: s\r\nConnection: close\r\n\r\n",
        404,
    ),
];
