//! The device's HTTP server: the status page and the JSON API (card 222).
//!
//! `picoserve 0.20` on TCP 80, on core 0's executor beside `frames_task`. The
//! shapes are [`screeny_device_api`]'s, so the browser, the simulator and the
//! Studio all read the same JSON; nothing about the wire format is spelled
//! twice.
//!
//! ## One dispatch (card 233)
//!
//! There is no route table of nested `Router::route(..)` layers any more.
//! [`Dispatch`] is a single [`picoserve::routing::PathRouterService`]:
//! [`route_request`] turns `(method, path)` into one [`Reply`] through
//! [`route::find`], and [`Reply`]'s one [`IntoResponse`] writes it. picoserve
//! still does every byte of the protocol - accept, timeouts, request parsing,
//! body buffering, `Content-Length`, streaming, `Connection: close` - it just
//! no longer decides *which* handler runs. Three things fall out of owning
//! that decision, and all three were bugs the conformance suite found:
//! a verb this API has no method for is `405` in [`ErrorReply`]'s shape rather
//! than picoserve's plain text; a reboot without the magic word is
//! `out_of_range` rather than `bad_request`; and each route's own
//! `max_request_len` is enforced, because it is now a table lookup.
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
//! * **A worker's job is to be in `accept`.** smoltcp has no listen backlog, so
//!   a connection that arrives while neither worker is listening is refused,
//!   and the device never hears about it. Card 236 is the whole of that story:
//!   [`serve_on`] owns the accept loop and [`BoundedSocket`] bounds the close,
//!   so how long a worker stays away after answering is this device's decision
//!   and not the client's.
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
//! the captive-portal catch-all (card 223), and `GET /api/v1/networks` (card
//! 229, **dropped** by device-web decision 10 - the route stays and keeps
//! answering `ErrorCode::Unavailable` rather than 404, because the route
//! exists and the device is simply not able to serve it).
//!
//! `POST /api/v1/firmware` is card 240 and is here, in [`post_firmware`]. It
//! is the one route that does not buffer its body: `route::Body::Stream` means
//! no `max_request_len` check and no `read_all`, the handler reads the socket
//! itself into [`crate::ota::Upload`]'s heap buffer, and the flash work is
//! `crate::ota`'s. It is also the one route allowed to take the panel from a
//! streaming sender (decision 7).

use core::fmt::Write as _;
use core::sync::atomic::{AtomicU32, AtomicU8, Ordering};

use embassy_futures::select::{select, Either};
use embassy_net::tcp::{TcpReader, TcpSocket, TcpWriter};
use embassy_net::{IpAddress, IpEndpoint, Stack};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::once_lock::OnceLock;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer};
use log::{info, warn};
use picoserve::request::{Path, Request, RequestBody, RequestBodyConnection};
use picoserve::response::{Connection, IntoResponse, Json, Response, ResponseWriter, StatusCode};
use picoserve::routing::{PathRouterService, Router, ServicePathRouter};
use picoserve::{ResponseSent, Server};
use screeny_device_api::reply::{
    AcceptedReply, FirmwareReply, PanicRecord, PanicReply, SettingsReply, StatusReply,
    TelemetryReply, WifiReply,
};
use screeny_device_api::request::{
    IdentifyRequest, Mutating, RebootRequest, SettingsRequest, MIN_UNESCAPE_BUFFER,
};
use screeny_device_api::{
    form, route, text, ErrorCode, ErrorReply, FirmwareError, FwSlot, FwState, IdleMode,
    ResetReason, StreamState, WifiState,
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

/// Requests this server has served since boot.
///
/// Four bytes, and they exist for card 241's health criterion: research 006
/// section 6 confirms an image on trial only once "the HTTP server has accepted
/// one request **or** 120 s of uptime has passed", and "one request" has to be
/// counted somewhere. Counted in [`Dispatch`], so it covers the page, the API
/// and the setup form alike - anything that proves somebody reached this device
/// and got an answer.
pub static REQUESTS: AtomicU32 = AtomicU32::new(0);

/// Publish a new `fw_state`, for the one thing that can change it while the
/// device runs: an image on trial confirming itself (card 241).
pub fn set_fw_state(state: FwState) {
    FW_STATE.store(state as u8, Ordering::Relaxed);
}

// Card 223 removed `WIFI_FAIL_REASON` and its two setters. `GET /api/v1/wifi`
// reports the *machine's* trial - `Trial::outcome` already carries a
// `FailReason` and `trial_is_current()` already says when it is the answer - so
// a second copy of "why did the last join fail", written by the radio and read
// by a handler, was a second thing to keep in step for no gain.

/// Read `otadata` once, at boot, and remember what it said.
///
/// Once rather than per request, for three reasons: nothing can change it
/// before card 241 ships the confirm/revert state machine; it needs the
/// `STORE` lock, which does not belong in an HTTP handler; and a status request
/// should not touch flash.
///
/// Call it from `main` after [`crate::store::init`] and before the panel is
/// lit: it is a read, and at that point core 1 is not running, so nothing is
/// parked.
///
/// **It no longer reads the partition table itself** (card 243). It used to,
/// and the 3 KB buffer that took stayed alive underneath `Ota::new` and
/// `current_ota_state` - i.e. underneath esp-storage's read path, the deepest
/// chain the boot path has. `store::read_partitions` reads the table once for
/// everybody and keeps the 32-byte entries beside the flash handle; what is
/// left here is two flash reads of `otadata` with nothing large below them.
pub async fn read_fw_health() {
    use esp_bootloader_esp_idf::ota::{Ota, OtaImageState};
    use esp_bootloader_esp_idf::partitions::AppPartitionSubType;

    let mut guard = store::STORE.lock().await;
    let Some(f) = guard.as_mut() else {
        warn!("http: no flash handle - fw_slot and fw_state report unknown");
        return;
    };
    let parts = f.parts();
    let flash = f.raw();

    // The *booted* partition, not otadata's selection: after a rollback the
    // bootloader may run one while otadata still names the other.
    if let Some(offset) = parts.booted_offset {
        // By offset, from `firmware/partitions.csv`, and not by asking the
        // entry for its subtype: `PartitionEntry::partition_type()` `unwrap!`s
        // the conversion, and a panic in the boot path to put a word in a
        // status reply is a bad trade (research 006 section 3).
        let slot = match offset {
            0x10000 => FwSlot::Ota0 as u8,
            0x210000 => FwSlot::Ota1 as u8,
            _ => FW_UNKNOWN,
        };
        FW_SLOT.store(slot, Ordering::Relaxed);
    }

    let Some(ota_part) = parts.otadata else {
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
    // Which slot `otadata` *selected*, which is not always the one that
    // booted: `Ota::current_app_partition` works from the sequence numbers
    // alone and ignores the image states, so after a rollback it names the
    // slot that was rolled back from. Card 241 makes that disagreement the
    // detection, so it is read here whether or not the MMU answered.
    let selected = match ota.current_app_partition() {
        Ok(AppPartitionSubType::Ota0) => FwSlot::Ota0,
        Ok(AppPartitionSubType::Ota1) => FwSlot::Ota1,
        _ => FwSlot::Unknown,
    };
    // `Ota` borrows the flash handle and `note_boot` needs it back. It has no
    // `Drop`, so this is a borrow ending and not a destructor running.
    let _ = ota;
    info!("http: fw slot {:?} state {:?}", fw_slot(), fw_state());

    // Card 241: what kind of boot this is, the one line that says an update
    // was rolled back, and - on a trial - the promotion that makes an
    // interrupted activation safe. It is the last thing done under this lock
    // because it is the only one that may write.
    let state = crate::ota::note_boot(fw_slot(), selected, fw_state(), &parts, flash);
    set_fw_state(state);
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

/// Every JSON body this server can send, as one type.
///
/// Card 233. It is one type rather than six because picoserve's response
/// writer is generic over the body: six body types meant six monomorphised
/// copies of "measure it, write the headers, stream it", and the future that
/// held whichever one was in flight was as big as all of them put together in
/// the request path's frame. The `Serialize` impl below is untagged - each
/// variant serialises exactly as the reply type it holds - so **nothing
/// changes on the wire**; the shapes are still `screeny-device-api`'s and
/// still spelt once.
enum ApiBody {
    Status(StatusReply),
    /// The RTC breadcrumb (card 243). Smaller than [`ApiBody::Status`], so it
    /// costs this enum - and every frame that holds one - nothing.
    Panic(PanicReply),
    Telemetry(TelemetryReply),
    Wifi(WifiReply),
    Settings(SettingsReply),
    /// The three routes that answer `{"result":...}`.
    Accepted(AcceptedReply),
    /// `POST /api/v1/firmware` (card 240). **Eight bytes**, and that is the
    /// whole of what it costs this enum: `size_of::<FirmwareReply>()` is 8
    /// against `StatusReply`'s 208, so it fits inside the variant
    /// [`ApiBody::Status`] already pays for and no frame that holds one of
    /// these grows by a byte. Card 243b's lesson is that a reply type is paid
    /// about twelve times over in the request path; the way to honour it is to
    /// check, and `crates/device-api`'s `sizes.rs` is where it is checked
    /// (`no_reply_is_bigger_than_the_one_on_the_hot_path`).
    Firmware(FirmwareReply),
    /// Every refusal on this server, including the 404 for an unknown path and
    /// the 400 for malformed JSON, so a caller never has to parse two kinds of
    /// error body.
    Error(ErrorReply),
}

impl serde::Serialize for ApiBody {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            ApiBody::Status(v) => v.serialize(s),
            ApiBody::Panic(v) => v.serialize(s),
            ApiBody::Telemetry(v) => v.serialize(s),
            ApiBody::Wifi(v) => v.serialize(s),
            ApiBody::Settings(v) => v.serialize(s),
            ApiBody::Accepted(v) => v.serialize(s),
            ApiBody::Firmware(v) => v.serialize(s),
            ApiBody::Error(v) => v.serialize(s),
        }
    }
}

// ---------------------------------------------------------------------------
// One reply value, one response path
// ---------------------------------------------------------------------------

/// Everything this server can answer with: the page, or a JSON body and the
/// status it goes out with.
///
/// Before card 233 each handler returned its own `Result<Json<T>, ErrorReply>`
/// and picoserve generated a separate generic response-writing layer per
/// handler, nested inside a separate generic routing layer per route. The
/// dispatch computes **one** of these and writes it through **one**
/// [`IntoResponse`] implementation with **two** arms, so a request is the http
/// task's frame, the dispatch's frame and the writer's frame - rather than a
/// chain whose depth is the length of the route table.
enum Reply {
    /// `GET /`, the status page: HTML, streamed.
    Page(Page),
    /// The setup page (card 223): HTML, streamed, no JavaScript in it.
    Portal(PortalPage),
    /// Everything else.
    Api(u16, ApiBody),
}

impl Reply {
    /// A 200 with a JSON body.
    fn ok(body: ApiBody) -> Self {
        Reply::Api(200, body)
    }

    /// A refusal, with the status [`ErrorCode::status`] names for it.
    fn err(code: ErrorCode) -> Self {
        Reply::error(ErrorReply::new(code))
    }

    /// A refusal with a sentence saying what was wrong.
    fn detail(code: ErrorCode, detail: &str) -> Self {
        Reply::error(ErrorReply::with_detail(code, detail))
    }

    fn error(reply: ErrorReply) -> Self {
        Reply::Api(reply.status(), ApiBody::Error(reply))
    }
}

impl IntoResponse for Reply {
    async fn write_to<R: picoserve::io::Read, W: ResponseWriter<Error = R::Error>>(
        self,
        connection: Connection<'_, R>,
        response_writer: W,
    ) -> Result<ResponseSent, W::Error> {
        // Two arms, one frame. picoserve still does all of the writing: the
        // counting pass that sets `Content-Length`, the header block, the
        // streaming of the body and `Connection: close`.
        match self {
            Reply::Page(p) => Response::ok(p).write_to(connection, response_writer).await,
            Reply::Portal(p) => {
                Response::ok(p)
                    // A captive probe that is cached is a captive sheet that
                    // never opens again - and since the owner's phone test
                    // this page *is* the answer to the probe.
                    .with_header("Cache-Control", "no-store")
                    .write_to(connection, response_writer)
                    .await
            }
            Reply::Api(status, body) => {
                Json(body)
                    .into_response()
                    .with_status_code(StatusCode::new(status))
                    .write_to(connection, response_writer)
                    .await
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

/// A JSON request body, parsed with [`ErrorReply`]'s failures rather than
/// picoserve's.
///
/// This was an extractor (`impl FromRequest`) until card 233; it is a plain
/// async function now because the dispatch calls it directly and no longer
/// needs picoserve's higher-ranked handler bound. The parsing is unchanged,
/// including the part that matters: `serde_json_core::from_slice` **silently
/// does not unescape strings**, so a name posted as `café` would be stored
/// with those six characters in it. `from_slice_escaped` with a buffer as long
/// as the longest string any request holds is the fix, and naming the constant
/// means raising `MAX_NAME_LEN` raises the buffer.
async fn json_body<'a, R: picoserve::io::Read, T: serde::Deserialize<'a>>(
    body: RequestBody<'a, R>,
) -> Result<T, ErrorReply> {
    let fits = body.entire_body_fits_into_buffer();
    let bytes: &'a [u8] = body.read_all().await.map_err(|_| {
        if fits {
            ErrorReply::with_detail(ErrorCode::BadRequest, "the body did not arrive")
        } else {
            ErrorReply::with_detail(ErrorCode::PayloadTooLarge, "the body is too long")
        }
    })?;
    serde_json_core::from_slice_escaped(bytes, &mut [0; MIN_UNESCAPE_BUFFER])
        .map(|(value, _)| value)
        .map_err(|_| ErrorReply::with_detail(ErrorCode::BadJson, "the body is not the expected JSON"))
}

/// The raw bytes of a request body.
///
/// `POST /api/v1/wifi` cannot use picoserve's `Form` extractor: it rejects a
/// body that is not UTF-8, and an 802.11 SSID is a byte string.
/// [`form::parse_wifi_form`] takes the bytes.
///
/// Card 233 removed the 384-byte `heapless::Vec` this used to copy into. That
/// copy existed only because a handler *function* may not borrow from the
/// request under picoserve's higher-ranked bound; the dispatch calls the
/// handler inline, so the parse reads picoserve's own buffer in place.
async fn raw_body<'a, R: picoserve::io::Read>(
    body: RequestBody<'a, R>,
) -> Result<&'a [u8], ErrorReply> {
    body.read_all()
        .await
        .map(|b| &*b)
        .map_err(|_| ErrorReply::with_detail(ErrorCode::BadRequest, "the body did not arrive"))
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

/// `status.wifi_state`: **the link**, from the one `Provisioner`.
///
/// Never the sticky result of the last credentials attempt - that belongs to
/// `GET /api/v1/wifi` alone (`docs/design/device-web.md`, the card 223
/// paragraph; firmware 0.4.x reported the sticky value in both places and a
/// device that had fallen back successfully read `failed` while plainly
/// connected).
fn wifi_state() -> WifiState {
    crate::provision::link_state()
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
        // The soft-AP is up: somebody could be standing on the setup network.
        portal: crate::provision::ap_up(),
        fw_slot: fw_slot(),
        fw_state: fw_state(),
        reset_reason: reset_reason(),
        store_errors: store::FAILURES.load(Ordering::Relaxed),
    }
}

/// `GET /api/v1/panic`: the breadcrumb in full (card 243).
///
/// Its own route, and **not** three more fields on [`StatusReply`]: the bench
/// measured 44 bytes of `StatusReply` costing 3,488 bytes of core 0's stack,
/// because one of these is moved through picoserve's response chain many times
/// in a single inlined async frame. Nothing here takes a lock or touches
/// flash - it is a dozen volatile reads of RTC memory - and the answer cannot
/// change while the device runs, because a panic reboots it.
fn get_panic() -> Reply {
    let crumb = crate::panic::report();
    Reply::ok(ApiBody::Panic(PanicReply {
        boot_count: crumb.boots,
        panic_count: crumb.panics,
        last_panic: crumb.last.map(|p| PanicRecord {
            uptime_ms: p.uptime_ms,
            boot: p.boot,
            file: text::text(p.file()).unwrap_or_default(),
            line: p.line,
            consecutive: p.consecutive,
        }),
        // Card 241, and on this route for the same two reasons the breadcrumb
        // is: `StatusReply` is polled every few seconds and is full, and this
        // changes exactly once in the life of a boot - when an image on trial
        // confirms itself.
        update: crate::ota::update_record(),
        // Card 241b: the same register `status` reports, on the route that is
        // about why this device is running what it is running. `wdt` here is
        // the liveness watchdog having fired.
        last_reset: Some(crate::panic::reset_reason()),
    }))
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
async fn apply_control(reqs: &[ControlRequest<'_>]) -> Result<(), ErrorReply> {
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
                return Err(ErrorReply::with_detail(
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
        return Err(ErrorReply::with_detail(ErrorCode::Storage, "the write to flash failed"));
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
static WIFI_PENDING: Signal<CriticalSectionRawMutex, crate::NewWifi> = Signal::new();
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

async fn get_telemetry() -> Reply {
    let t = {
        let mut guard = CORE.lock().await;
        let core = guard.as_mut().expect("core exists");
        core.telemetry(now_us())
    };
    Reply::ok(ApiBody::Telemetry(TelemetryReply::from(&t)))
}

/// `GET /api/v1/wifi`, built by the machine.
///
/// Card 232: a trial in flight or just finished is what the page that posted
/// reads, and it keeps reading it after the previous network has come back -
/// [`Provisioner::trial_is_current`] decides that, in `crates/provision`, so
/// the firmware and the simulator cannot answer it differently. The `reason`
/// is the radio's, mapped in `crate::provision::fail_reason`, and is no longer
/// the `null` firmware 0.4.x reported.
///
/// [`Provisioner::trial_is_current`]: screeny_provision::Provisioner::trial_is_current
fn get_wifi() -> Reply {
    Reply::ok(ApiBody::Wifi(crate::provision::wifi_reply()))
}

/// `POST /api/v1/wifi`: urlencoded, and the reply leaves before the radio work.
fn post_wifi(body: &[u8]) -> Reply {
    let form = match form::parse_wifi_form(body) {
        Ok(f) => f,
        Err(e) => return Reply::error(e.reply()),
    };
    if let Err(c) = screeny_device_api::request::check_auth(form.auth()) {
        return Reply::err(c);
    }
    // The one line in this module that mentions the secret, and it says how
    // long it is (spec section 8.4).
    info!(
        "http: set-wifi for a {}-byte SSID, psk_len {}",
        form.ssid().len(),
        form.psk_len()
    );
    let wifi = match Wifi::new(form.ssid(), form.psk()) {
        Ok(w) => w,
        Err(e) => {
            warn!("http: set-wifi refused: {:?}", e);
            return Reply::detail(ErrorCode::OutOfRange, "those credentials do not fit");
        }
    };

    // Nothing is written here. The WiFi task stores the pair only after it has
    // joined (`crate::NewWifi`); firmware 0.4.0 wrote first, and one mistyped
    // password replaced the working credentials in flash.
    WIFI_PENDING.signal(crate::NewWifi { wifi, persist: true });
    Reply::ok(ApiBody::Accepted(AcceptedReply::TRYING))
}

/// `POST /api/v1/settings`: the same three opcodes the control port takes.
async fn post_settings(req: SettingsRequest) -> Reply {
    if let Err(c) = req.check_auth() {
        return Reply::err(c);
    }

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
    if let Err(e) = apply_control(&reqs).await {
        return Reply::error(e);
    }

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
    Reply::ok(ApiBody::Settings(SettingsReply {
        name,
        brightness: crate::BRIGHTNESS.load(Ordering::Relaxed),
        idle_mode,
    }))
}

async fn post_identify(req: IdentifyRequest) -> Reply {
    if let Err(c) = req.check_auth() {
        return Reply::err(c);
    }
    let duration_ms = match req.duration_u16() {
        Ok(d) => d,
        Err(c) => {
            return Reply::detail(c, "duration_ms is longer than the protocol can carry");
        }
    };
    if let Err(e) = apply_control(&[ControlRequest::Identify { duration_ms }]).await {
        return Reply::error(e);
    }
    Reply::ok(ApiBody::Accepted(AcceptedReply::IDENTIFYING))
}

fn post_reboot(req: RebootRequest) -> Reply {
    if let Err(c) = req.check_auth() {
        return Reply::err(c);
    }
    if !req.confirmed() {
        // **`out_of_range`, not `bad_request`** (card 233, probe rule 34). The
        // body parsed and `confirm` was there: what is wrong is its *value*,
        // which is the same judgement UDP `REBOOT`'s bad magic gets
        // (`ProtoError::BadArg` -> `ErrorCode::OutOfRange`) and what the
        // simulator answers. Both are 400, so only the machine-readable code
        // moves.
        return Reply::detail(ErrorCode::OutOfRange, "confirm must be \"RBOO\"");
    }
    REBOOT_PENDING.signal(());
    Reply::ok(ApiBody::Accepted(AcceptedReply::REBOOTING))
}

// ---------------------------------------------------------------------------
// POST /api/v1/firmware (card 240)
// ---------------------------------------------------------------------------

/// How long the whole body has to arrive.
///
/// picoserve's `read_request` timeout is 5 s, which is right for every other
/// route on this device and impossible for this one: a ~950 KB image over this
/// radio, interleaved with 232 sector erases, is tens of seconds of wall
/// clock. `RequestBodyReader::with_different_timeout` is picoserve's own
/// answer - its documentation names "uploading large files" as the case - and
/// it replaces the 5 s signal for the body of this request only.
///
/// 180 s is the worst case the bench has any reason to allow: 950 KB at the
/// ~70 KB/s this path can manage (one 4 KB read, then ~58 ms of flash, then
/// the next) is ~14 s, so the bound is an order of magnitude clear of a
/// healthy upload and still a bound. It is also the longest a client can hold
/// one HTTP worker, which is why it is not simply "no timeout": with two
/// workers the other one keeps answering throughout.
const UPLOAD_TOTAL_S: u64 = 180;

/// How long one read may stall before the upload is abandoned.
///
/// The 180 s above is the outer bound; this is the one that fires in practice.
/// A sender that has stopped sending - a laptop that went to sleep mid-`curl`,
/// a cable pulled - should not hold the flash, the panel and a worker for
/// three minutes to prove it. Five seconds is far longer than any gap a
/// healthy upload has, because the device is the slow end: the reads are
/// waiting on smoltcp, not the other way round.
const UPLOAD_STALL: Duration = Duration::from_secs(5);

/// A refusal in the route's own reply shape.
///
/// **200, not a 4xx**, and that is deliberate: `FirmwareReply` *is* this
/// route's reply, `error` is a field of it, and `written` - how far the upload
/// got before it was refused - is only there. A caller switching on the HTTP
/// status would learn less than one reading two fields of the body it already
/// has to parse. `crates/sim` has answered this way since card 224 and probe
/// rules 23 and 24 are written against it. The generic `{"error":...}` shape
/// of spec 8.7 is still what a request that never reached the validator gets:
/// `unavailable` when the device has no inactive slot, `bad_request` when the
/// body did not arrive at all.
fn firmware_failed(written: u32, e: FirmwareError) -> Reply {
    Reply::ok(ApiBody::Firmware(FirmwareReply::failed(written, e)))
}

/// Answer a refusal **now**, instead of after five seconds of body.
///
/// **Card 246, item 1, second half.** A refusal this route can decide from its
/// own state - `busy`, `too_large`, `unavailable` - is decided before a byte
/// of the body is read, and always was. What the bench measured was the
/// *reply* arriving 750 KB late, and that happens one layer up:
/// [`Dispatch::call`] must call `RequestBodyConnection::finalize` before it can
/// write anything, and `finalize`, faced with a body the handler did not read,
/// **drains it** until picoserve's `read_request` timeout - 5 s here, which on
/// this radio is about 750 KB - and only then hands the connection over.
///
/// Its other path is the one this function takes: when the handler has read
/// *past the end of picoserve's own request buffer*, `finalize` knows the body
/// is being abandoned and skips the drain entirely
/// (`picoserve-0.20.0/src/request.rs`, `finalize`, case 1 versus case 2). One
/// byte more than the buffer already holds is enough, so a refused upload
/// reads at most [`HTTP_BUF`] + 64 bytes - a millisecond of socket, never the
/// image - and the caller has its `{"ok":false,...}` while it is still
/// writing. The connection is closed after the response either way
/// (`close_connection_after_response`), so the bytes still in flight are
/// discarded by the close and never parsed as a second request.
///
/// A client that sent nothing after its headers cannot make this wait longer
/// than the drain would have: the reads are the request's own, and its 5 s
/// `read_request` signal ends them.
async fn refuse_at_once<R: picoserve::io::Read>(
    body: &mut RequestBodyConnection<'_, R>,
    reply: Reply,
) -> Reply {
    use embedded_io_async::Read as _;
    let request_body = body.body();
    // picoserve's buffer, from picoserve, rather than [`HTTP_BUF`] repeated
    // here: the number that matters is how much of *this* body is already in
    // it, and that is at most the buffer's length.
    let want = request_body.buffer_length().saturating_add(1);
    let mut reader = request_body.reader();
    // 64 bytes, and they are 64 bytes of every HTTP worker's future for the
    // sake of one refusal - which is the cheapest honest thing this could hold
    // (card 227: nothing large across an `await`).
    let mut scratch = [0u8; 64];
    let mut got = 0usize;
    while got < want {
        match reader.read(&mut scratch).await {
            Ok(0) | Err(_) => break,
            Ok(n) => got += n,
        }
    }
    reply
}

/// `POST /api/v1/firmware`: stream the body into the inactive slot.
///
/// The bytes go socket -> [`crate::ota::Upload`]'s heap buffer -> flash, one
/// sector at a time, and are never anywhere else. Nothing in this function
/// holds a buffer across an `await`: the only thing alive across the read is
/// the `Upload`, which is a pointer to the heap, the scanner and the counters.
///
/// What a caller gets back:
///
/// * a good image - `{"ok":true,"written":N}`, and the slot now holds a
///   firmware the bootloader could run. **It will not run it**: `otadata` is
///   untouched by this card, so the device keeps booting what it boots today
///   until card 241 lands. A reboot now changes nothing.
/// * anything the validator refused - `{"ok":false,"written":N,"error":...}`.
/// * a second upload while one is in flight - `error: "busy"`, nothing
///   touched.
/// * a device with no inactive slot - `unavailable`, in the generic shape.
///
/// One thing it deliberately does not do is drain a body it has refused.
/// picoserve's `finalize` does that, bounded by the request's own 5 s read
/// timeout, and then aborts the connection - so a client that declared a
/// megabyte and was refused on the header gets its JSON reply and a RST,
/// rather than the device spending fifteen seconds reading bytes it has
/// already decided to throw away.
///
/// ## What it costs the stack, and why `#[inline(never)]` is not the answer
///
/// LLVM inlines this into [`route_request`]'s `poll`, which is the one frame
/// every request on this device pays for: it grew from **1,552 bytes on
/// firmware 0.5.3 to 2,304 here**, +752 for the reader, the per-read timeout
/// and the `Upload`'s own scratch. `#[inline(never)]` was tried and is
/// **worse**: at 3,808 bytes, because an `async fn` the caller cannot see
/// through has to be materialised as a value in the caller's `poll` frame
/// instead of merged into its state machine. Inlined is the cheaper of the
/// two, so inlined it stays, and the 752 bytes are the honest price of the
/// route. Everything *below* this - the scanner, the erase, the write, the
/// SHA-256 - is in `#[inline(never)]` synchronous functions and adds at most
/// ~600 bytes on top, only while an upload is running.
async fn post_firmware<R: picoserve::io::Read>(
    activate: bool,
    body: &mut RequestBodyConnection<'_, R>,
) -> Reply {
    let declared = u32::try_from(body.content_length()).ok();
    // **Everything that can be refused from state alone is refused here**, in
    // one place, before the reader exists: no claim is taken, no sector is
    // erased and no byte of the image is read for any of them (card 246, item
    // 1). One `refuse_at_once` for the three of them and not three, because
    // this function is inlined into the frame every request on this device
    // pays for and each copy of that loop is 300 bytes of it.
    let start = match store::inactive_slot().await {
        // Decided once at boot, against the MMU (`store::read_partitions`).
        // No slot means no safe place to write, and the honest answer to that
        // is `unavailable` - the route exists and this device cannot serve it.
        None => Err(Reply::detail(
            ErrorCode::Unavailable,
            "this device has no inactive app slot",
        )),
        // Card 241. Refused **before** the megabyte rather than after it: a
        // device whose `otadata` could not be read can still stage an image
        // perfectly well and can never select it, so promising to activate one
        // and then spending twenty-five seconds discovering otherwise is the
        // wrong way round. Staging (`?activate=0`) is still offered, because
        // it still works.
        Some(_) if activate && !crate::ota::can_activate() => Err(Reply::detail(
            ErrorCode::Unavailable,
            "this device cannot select a boot slot",
        )),
        // `too_large` from the `Content-Length`, `busy` from two atomics.
        Some(slot) => crate::ota::Upload::start(slot, declared)
            .await
            .map_err(|e| firmware_failed(0, e)),
    };
    let mut up = match start {
        Ok(u) => u,
        Err(reply) => return refuse_at_once(body, reply).await,
    };

    let mut reader = body
        .body()
        .reader()
        .with_different_timeout(Duration::from_secs(UPLOAD_TOTAL_S));

    loop {
        // Straight into the staging buffer: see `Upload::spare`.
        // `picoserve::io::Read` is `embedded_io_async::Read`; the trait has to
        // be in scope for the one call this module makes to it.
        use embedded_io_async::Read as _;
        let read = embassy_time::with_timeout(UPLOAD_STALL, reader.read(up.spare())).await;
        let n = match read {
            Ok(Ok(0)) => break,
            Ok(Ok(n)) => n,
            Ok(Err(_)) => {
                // The body stopped early - the socket died, or the 180 s
                // outer bound fired. The scanner will call it a truncated
                // image, which is what it is.
                let written = up.written();
                up.failed(FirmwareError::BadSha256).await;
                return firmware_failed(written, FirmwareError::BadSha256);
            }
            Err(embassy_time::TimeoutError) => {
                warn!("ota: the uploader went quiet for {} s", UPLOAD_STALL.as_secs());
                let written = up.written();
                up.failed(FirmwareError::BadSha256).await;
                return firmware_failed(written, FirmwareError::BadSha256);
            }
        };
        if let Err(e) = up.took(n).await {
            let written = up.written();
            up.failed(e).await;
            return firmware_failed(written, e);
        }
    }

    let written = up.written();
    match up.finish().await {
        Ok(a) => {
            info!(
                "ota: staged image accepted - {} bytes, {} segments, version {:?}",
                a.image_len, a.segments, a.version
            );
            if activate {
                // **The reply is built here and the flash write happens
                // elsewhere**, and the order is the whole of card 227's lesson
                // and card 236's: `request_activation` only raises a signal,
                // so nothing in this handler's chain touches flash; the
                // `Upload` is dropped on the next line, giving the panel back;
                // `Dispatch` writes this reply; the worker's `BoundedSocket`
                // closes and waits for the peer's acknowledgement; and
                // `ota::activate_task`, two seconds later, is what writes
                // `otadata` and resets the chip.
                crate::ota::request_activation();
                Reply::ok(ApiBody::Firmware(FirmwareReply::activating(a.written)))
            } else {
                info!("ota: staged only (activate=0) - nothing about what boots has changed");
                Reply::ok(ApiBody::Firmware(FirmwareReply::ok(a.written)))
            }
        }
        Err(e) => firmware_failed(written, e),
    }
}

/// A route that exists in the API but not yet on this device.
///
/// `screeny-device-api` has no `not_implemented` code and should not grow one
/// for this: `unavailable` (503) is exactly "understood, and the device cannot
/// serve it in this state", and a caller retrying later is the right
/// behaviour for it. `GET /api/v1/networks` needs the scan card 229 was going
/// to bring and device-web decision 10 dropped, so it is the only route left
/// that answers this: `POST /api/v1/firmware` landed with card 240.
fn not_yet() -> Reply {
    Reply::detail(
        ErrorCode::Unavailable,
        "this firmware does not serve that route yet",
    )
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
        // Card 243, one line: the whole breadcrumb, read **here** rather than
        // carried in the reply. Two things make that safe where it would not
        // be for `uptime` or `heap`: picoserve formats a body twice (once to
        // measure it, once to send it) and the two passes must produce the
        // same bytes, and the breadcrumb is the one thing on this page that
        // *cannot* change while the device runs - a panic reboots it. Keeping
        // it out of `StatusReply` is what this card's stack fix is.
        let crumb = crate::panic::report();
        match crumb.last {
            None => row(
                f,
                "panic",
                format_args!("none in {} boot(s) since power-on", crumb.boots),
            )?,
            Some(p) => row(
                f,
                "panic",
                format_args!(
                    "{}:{} at {} s, boot {} of {} ({} in a row, {} total)",
                    p.file(),
                    p.line,
                    p.uptime_ms / 1000,
                    p.boot,
                    crumb.boots,
                    p.consecutive,
                    crumb.panics,
                ),
            )?,
        }
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
// The setup page (card 223)
// ---------------------------------------------------------------------------

/// The setup form's own path, on both interfaces.
///
/// A path of its own rather than `POST /api/v1/wifi`, for one reason: this one
/// answers **HTML**, because what posts to it is an ordinary `<form>` in a
/// captive mini-browser with no JavaScript, and a browser handed
/// `{"result":"trying"}` shows the person a page of JSON. The JSON route is
/// unchanged and is still what the Studio and `screeny-probe` use; both end up
/// in the same place, which is [`crate::NEW_WIFI`] and the one `Provisioner`.
const SETUP_PATH: &str = "/setup";

/// The head of the setup page. Inline CSS, no script, no external asset: the
/// iOS and Android captive mini-browsers are the audience.
const PORTAL_HEAD: &str = concat!(
    "<!doctype html><html lang=en><head><meta charset=utf-8>",
    "<meta name=viewport content=\"width=device-width,initial-scale=1\">",
    "<title>screeny setup</title><style>",
    ":root{color-scheme:light dark}",
    "body{margin:0;padding:20px;max-width:26rem;font:16px/1.5 system-ui,sans-serif}",
    "h1{font-size:1.25rem;margin:0 0 4px}",
    "p{margin:0 0 12px}",
    ".s{padding:10px 12px;border-radius:8px;border:1px solid #8884;margin:0 0 16px}",
    "label{display:block;margin:12px 0}",
    "label span{display:block;font-size:.85rem;opacity:.7}",
    "input{font:inherit;width:100%;padding:10px;border-radius:8px;border:1px solid #8886;",
    "background:transparent;color:inherit}",
    "button{font:inherit;padding:10px 18px;border-radius:8px;border:1px solid #06c;",
    "color:#06c;background:transparent;margin-top:8px}",
    "</style>"
);

/// The form. `method=post` and nothing else: it works with JavaScript off, and
/// a full-page navigation is the only thing the iOS mini-browser re-probes on.
/// No file input - they do not work in a captive browser at all.
const PORTAL_FORM: &str = concat!(
    "<form method=post action=\"/setup\">",
    "<label><span>Wi-Fi network name</span>",
    "<input name=ssid maxlength=32 required autocapitalize=none autocorrect=off spellcheck=false></label>",
    "<label><span>Password (leave empty for an open network)</span>",
    // 64, not 63: spec 8.2 types `psk_len` as `0..=64` and
    // `screeny_proto::control::MAX_PSK_LEN` is 64, which is the length of a
    // WPA2 PSK typed as 64 hex characters. A `maxlength` of 63 silently ate
    // the last one in a captive mini-browser, where there is no other way to
    // find out (card 243).
    "<input name=psk type=password maxlength=64 autocapitalize=none autocorrect=off></label>",
    "<button type=submit>Join</button></form>"
);

/// The setup page, rendered from the one [`Provisioner`]'s answer.
///
/// Streamed like [`Page`]: picoserve measures it into a counting writer and
/// then writes it, so there is no buffer here either.
///
/// [`Provisioner`]: screeny_provision::Provisioner
struct PortalPage {
    view: Option<crate::provision::TrialView>,
}

impl core::fmt::Display for PortalPage {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        use crate::provision::TrialView;
        f.write_str(PORTAL_HEAD)?;
        // **A timed full-page reload, not `fetch`.** The iOS captive
        // mini-browser only re-probes the network on a full navigation, so
        // polling with script would leave the sheet open forever after the
        // device had joined (research 007 section 4.4).
        if matches!(self.view, Some(TrialView::Trying)) {
            f.write_str("<meta http-equiv=refresh content=\"4;url=/setup\">")?;
        }
        f.write_str("</head><body><h1>screeny setup</h1>")?;
        match self.view {
            None => {
                write!(
                    f,
                    "<p>Tell this panel which Wi-Fi network to join.</p>{PORTAL_FORM}"
                )
            }
            Some(TrialView::Trying) => f.write_str(concat!(
                "<div class=s>Trying that network&hellip;</div>",
                "<p>This page checks again every few seconds. The setup network may ",
                "drop out for a moment while the panel looks for yours.</p>"
            )),
            Some(TrialView::Connected(ip)) => write!(
                f,
                "<div class=s>Connected. This panel is now at \
                 <a href=\"http://{}.{}.{}.{}/\">{}.{}.{}.{}</a>, and the address is \
                 on the panel too.</div><p>You can close this page. The setup \
                 network goes away in about half a minute.</p>",
                ip[0], ip[1], ip[2], ip[3], ip[0], ip[1], ip[2], ip[3]
            ),
            Some(TrialView::Failed(why)) => write!(
                f,
                "<div class=s>That did not work: {why}.</div>{PORTAL_FORM}"
            ),
        }?;
        f.write_str("</body></html>")
    }
}

impl picoserve::response::Content for PortalPage {
    fn content_type(&self) -> &'static str {
        "text/html; charset=utf-8"
    }

    fn content_length(&self) -> usize {
        let mut c = Counter(0);
        let _ = write!(c, "{self}");
        c.0
    }

    async fn write_content<W: picoserve::io::Write>(self, mut writer: W) -> Result<(), W::Error> {
        writer.write_fmt(format_args!("{self}")).await
    }
}

/// `GET /setup`, and `GET /` on the soft-AP: whatever the machine says about
/// the last posted credentials, plus the form unless it worked.
fn get_setup() -> Reply {
    Reply::Portal(PortalPage {
        view: crate::provision::trial_view(),
    })
}

/// `POST /setup`: the same bytes `POST /api/v1/wifi` takes, answered in HTML.
///
/// The credentials are **not** written to flash here, and nothing in this
/// function touches the store: the pair goes to [`deferred_task`], which hands
/// it to the provisioning task once the reply is on the air, and the store is
/// only written when the machine answers `CommitCredentials`.
fn post_setup(body: &[u8]) -> Reply {
    match form::parse_wifi_form(body) {
        Ok(f) => {
            info!(
                "http: setup form for a {}-byte SSID, psk_len {}",
                f.ssid().len(),
                f.psk_len()
            );
            match Wifi::new(f.ssid(), f.psk()) {
                Ok(wifi) => {
                    WIFI_PENDING.signal(crate::NewWifi { wifi, persist: true });
                    // The machine has not seen the post yet - `deferred_task`
                    // has the 400 ms - so the page it draws now would still be
                    // the *previous* answer. Say "trying" explicitly.
                    Reply::Portal(PortalPage {
                        view: Some(crate::provision::TrialView::Trying),
                    })
                }
                Err(e) => {
                    warn!("http: setup form refused: {:?}", e);
                    Reply::Portal(PortalPage {
                        view: Some(crate::provision::TrialView::Failed(
                            "that network name or password does not fit",
                        )),
                    })
                }
            }
        }
        Err(_) => Reply::Portal(PortalPage {
            view: Some(crate::provision::TrialView::Failed("the form was incomplete")),
        }),
    }
}

// ---------------------------------------------------------------------------
// The dispatch: one future from (method, path) to a reply
// ---------------------------------------------------------------------------

/// The whole router (card 233).
///
/// ## Why it is a [`PathRouterService`] and not a chain of `.route()` calls
///
/// `Router::new().route(..).route(..)` builds a left-nested
/// `Route<PD, MethodRouter<..>, Route<PD, .., ..>>` type. Every layer's future
/// holds the whole remaining chain by value and its `poll` calls the next
/// layer's, so the depth of a request was the length of the route table:
/// research 010 section 2 measured 2,416 and 2,192 bytes of frame for two of
/// those layers, on top of the http task's own, and section 3 concluded that
/// the only cheap win left in the request path was to stop doing that.
///
/// `Router::from_service` takes a single [`PathRouterService`] and forwards
/// every request to it, which is what this is. One `match` on
/// `route::find(path, method)` replaces nine nested `Either`s.
///
/// ## What picoserve still does
///
/// Everything except choosing the handler: the listener and accept loop, the
/// per-phase timeouts, parsing the request line and headers, buffering the
/// body, measuring each reply with a counting writer, writing
/// `Content-Type` / `Content-Length` / `Connection: close`, and streaming the
/// body. Nothing here parses or formats HTTP.
struct Dispatch {
    /// Whether this worker is listening on the **soft-AP**'s stack.
    ///
    /// It is a property of the listener, not of the request, and that is the
    /// point: a browser on the LAN gets the status page and a 404 for an
    /// unknown path, while a phone on the setup network gets the setup form
    /// and the captive-portal redirect, with no guessing from headers.
    ap: bool,
}

impl PathRouterService for Dispatch {
    async fn call_path_router_service<
        R: picoserve::io::Read,
        W: ResponseWriter<Error = R::Error>,
    >(
        &self,
        _state: &(),
        (): (),
        _path: Path<'_>,
        mut request: Request<'_, R>,
        response_writer: W,
    ) -> Result<ResponseSent, W::Error> {
        // `parts` is `Copy` and both of these borrow the request buffer, not
        // the `Request`, so the body connection can still be borrowed mutably.
        let method = request.parts.method();
        let path = request.parts.path();
        // Card 241's health criterion counts requests *served*, so it is
        // counted here, on the way in, for every route and both pages alike.
        // A `Relaxed` add of a `u32` is two instructions and the number only
        // ever has to answer "more than none".
        REQUESTS.fetch_add(1, Ordering::Relaxed);
        // The one query parameter this API has. picoserve keeps it apart from
        // the path, so `/api/v1/firmware?activate=0` still matches the route
        // table exactly.
        let query = request.parts.query();

        // One line per request **on the setup network only**: it exists for
        // minutes, a phone's captive probing is what goes wrong on it, and the
        // serial log is the only witness (the owner's phone test, 2026-09-20).
        // Never on the LAN, where the Studio polls for months.
        if self.ap {
            let headers = request.parts.headers();
            let header = |name: &str| {
                headers
                    .get(name)
                    .and_then(|v| v.as_str().ok())
                    .map_or("-", |s| crate::provision::cut_str(s, 40))
            };
            info!(
                "http: setup network {} {} | host {} | ua {}",
                method,
                crate::provision::cut_str(path.encoded(), 48),
                header("host"),
                header("user-agent")
            );
        }

        let reply =
            route_request(self.ap, method, path, query, &mut request.body_connection).await;

        // The handler future is finished and dropped *before* the reply is
        // written: at no point is a lock, a body borrow or a handler's state
        // alive while the socket is being written to.
        reply
            .write_to(request.body_connection.finalize().await?, response_writer)
            .await
    }
}

/// The request path as one of the server's own constants, or `None` for a path
/// this server does not have.
///
/// It exists so that the dispatch compares paths the way picoserve's `Route`
/// did, rather than the way `&str == &str` does: `PartialEq<&str> for Path`
/// decodes as it goes, so `/api/v1/%73tatus` is still `/api/v1/status` and
/// `/api/v1/status/` is still not. Card 233 changes the routing; it does not
/// change which requests match.
fn known_path(path: Path<'_>) -> Option<&'static str> {
    if path == "/" {
        return Some("/");
    }
    route::ROUTES.iter().map(|r| r.path).find(|p| path == *p)
}

/// `(method, path)` -> one [`Reply`].
///
/// The order is deliberate and is the API's whole error contract:
///
/// 1. **Card 223's captive-portal hook goes at the top of this function**, in
///    front of the route table: on the soft-AP interface an unknown path
///    is answered with the setup page itself, and it has to be decided before
///    a 404 is.
/// 2. `GET /` is the page, and it is the one path that is not in
///    [`route::ROUTES`].
/// 3. A verb this API has no [`route::Method`] for - `PUT`, `DELETE`, `HEAD`,
///    anything - falls through to the same test as a wrong method, so
///    `DELETE /api/v1/status` is `405 method_not_allowed` **in the API's error
///    shape** and not picoserve's plain-text `MethodNotAllowed`. That was
///    probe rules 26 and 37 on firmware 0.4.2, and it is what owning the
///    dispatch fixes.
/// 4. A known path with a method it does not have is `405`; an unknown path is
///    `404`; both through [`route::find`] and [`route::path_is_known`], which
///    are the simulator's and the probe's own answer to the same question.
/// 5. Each route's own `max_request_len` is a table lookup, so an oversize
///    body is `413 payload_too_large` on the route that declares the bound
///    rather than only at the global `MAX_REQUEST_LEN` (probe rule 29, the
///    decision recorded for card 223).
async fn route_request<R: picoserve::io::Read>(
    ap: bool,
    method: &str,
    path: Path<'_>,
    query: Option<picoserve::url_encoded::UrlEncodedString<'_>>,
    body: &mut RequestBodyConnection<'_, R>,
) -> Reply {
    // --- 1: card 223's captive-portal catch-all, before any routing --------
    //
    // On the soft-AP, and only while it is actually up, a path this server
    // does not have is **the setup page** rather than a 404. That is what
    // `captive.apple.com/hotspot-detect.html`, `connectivitycheck.gstatic.com`
    // and `msftconnecttest.com` are asking, and answering it is what makes the
    // captive sheet open by itself. `/setup` is exempt for the obvious reason;
    // so is every route in the table, so a phone can still read the API.
    let setup = path == SETUP_PATH;
    //
    // **The answer is the setup page itself, `200`, not a `302` to it** (the
    // owner's phone test, 2026-09-20, iOS 18.7). The captive sheet fetches
    // `hotspot-detect.html` with one connection and opens a second it never
    // uses, which holds one worker for `start_read_request`. A redirect made
    // it open a *third* to 192.168.4.1 within milliseconds, while the worker
    // that had just answered was still between `close` and `accept`; smoltcp
    // has no backlog, the SYN was refused, and iOS - which does not retry a
    // refused connection, where macOS does after a second - said "Hotspot
    // login cannot open the page because it could not connect to the server".
    // Any reply that is not Apple's `Success` page, not a 204 and not
    // Microsoft's text marks the network captive, so the form does that job
    // too, from the connection the sheet already has.
    if ap && crate::provision::ap_up() && !setup && known_path(path).is_none() {
        return get_setup();
    }

    // --- 2: the setup form, on both interfaces -----------------------------
    if setup {
        return match method {
            "GET" => get_setup(),
            "POST" => {
                if body.content_length() > route::MAX_REQUEST_LEN {
                    return Reply::detail(
                        ErrorCode::PayloadTooLarge,
                        "the body is longer than this route accepts",
                    );
                }
                match raw_body(body.body()).await {
                    Ok(bytes) => post_setup(bytes),
                    Err(e) => Reply::error(e),
                }
            }
            _ => Reply::err(ErrorCode::MethodNotAllowed),
        };
    }

    // A request path is URL-encoded, and picoserve's `Route` matched it
    // decoded, so the dispatch does too: `known_path` is the one place that
    // turns whatever the client wrote into one of `route::ROUTES`'s own
    // `&'static str`s. Everything after it is a known path, which is the
    // `route::path_is_known` half of the 404/405 question; `route::find` is
    // the other half.
    let Some(path) = known_path(path) else {
        return Reply::err(ErrorCode::NotFound);
    };

    if path == "/" {
        return match method {
            // On the setup network `/` **is** the setup form: a phone that was
            // dragged here by the catch-all or by the QR code
            // is here to type a network name, not to read a status table.
            "GET" if ap => get_setup(),
            "GET" => Reply::Page(Page::new(status().await)),
            _ => Reply::err(ErrorCode::MethodNotAllowed),
        };
    }

    // `route::Method` is Get and Post and nothing else, on purpose: those are
    // the only verbs this API defines. Every other verb is "a method this path
    // does not have", which is a 405 and not a 404 - `route::path_is_known`'s
    // own doc example says so.
    let wanted = match method {
        "GET" => Some(route::Method::Get),
        "POST" => Some(route::Method::Post),
        _ => None,
    };
    let Some(row) = wanted.and_then(|m| route::find(path, m)) else {
        return Reply::err(ErrorCode::MethodNotAllowed);
    };

    // The per-route body bound, from the shared table. `Body::Stream` is the
    // firmware upload, which is never buffered and declares no bound.
    if row.body != route::Body::Stream && body.content_length() > row.max_request_len {
        return Reply::detail(
            ErrorCode::PayloadTooLarge,
            "the body is longer than this route accepts",
        );
    }

    match (row.method, row.path) {
        (route::Method::Get, route::STATUS) => Reply::ok(ApiBody::Status(status().await)),
        (route::Method::Get, route::TELEMETRY) => get_telemetry().await,
        (route::Method::Get, route::PANIC) => get_panic(),
        (route::Method::Get, route::WIFI) => get_wifi(),
        // Needs the scan card 223 brings.
        (route::Method::Get, route::NETWORKS) => not_yet(),
        (route::Method::Post, route::WIFI) => match raw_body(body.body()).await {
            Ok(bytes) => post_wifi(bytes),
            Err(e) => Reply::error(e),
        },
        (route::Method::Post, route::SETTINGS) => match json_body(body.body()).await {
            Ok(req) => post_settings(req).await,
            Err(e) => Reply::error(e),
        },
        (route::Method::Post, route::IDENTIFY) => match json_body(body.body()).await {
            Ok(req) => post_identify(req).await,
            Err(e) => Reply::error(e),
        },
        (route::Method::Post, route::REBOOT) => match json_body(body.body()).await {
            Ok(req) => post_reboot(req),
            Err(e) => Reply::error(e),
        },
        // Card 240. The one route whose body is never buffered: `Body::Stream`
        // above means no `max_request_len` check and no `read_all`, and the
        // handler reads the socket itself. Card 241 gives it the one query
        // parameter this API has, decoded here because it is the only route
        // that looks at a query at all: a 32-byte buffer holds
        // `activate=false` three times over, and a query longer than that on
        // this route is refused rather than half-read.
        (route::Method::Post, route::FIRMWARE) => {
            let decoded = match query {
                None => None,
                Some(q) => match q.try_into_string::<32>() {
                    Ok(s) => Some(s),
                    Err(_) => {
                        return Reply::detail(
                            ErrorCode::OutOfRange,
                            "the query string is not one this route takes",
                        );
                    }
                },
            };
            match route::parse_activate(decoded.as_deref()) {
                Ok(activate) => post_firmware(activate, body).await,
                Err(_) => Reply::detail(ErrorCode::OutOfRange, "activate must be 0 or 1"),
            }
        }
        // Unreachable while this match covers `ROUTES`; a new row that nobody
        // wired up answers 404 rather than failing to compile, because a
        // firmware that panics on an unhandled path is worse than one that
        // says it has none.
        _ => Reply::err(ErrorCode::NotFound),
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

/// The whole router: one service, no nesting.
///
/// A `fn` rather than a `static` because `ServicePathRouter`'s field is
/// private, so [`Router::from_service`] is the only way to build one - but the
/// type is now nameable and both values are zero-sized, so this compiles to
/// nothing. The `http-selftest` build calls the same function, which is what
/// keeps the self-test honest: it exercises the router the device serves.
fn router(ap: bool) -> Router<ServicePathRouter<Dispatch>> {
    Router::from_service(Dispatch { ap })
}

/// Which stack a worker serves.
///
/// **Every worker follows the soft-AP**: while the AP is up they all listen on
/// 192.168.4.1, and the rest of the time they are the LAN workers exactly as
/// card 227 made them.
///
/// Card 223 moved only one, and the owner's phone test (2026-09-20) is why
/// that was wrong. smoltcp has no listen backlog: with one socket, a second
/// connection that arrives while the first is open is *refused*. An iPhone's
/// captive sheet opens a connection it does not use straight away next to the
/// one it does, the idle one held the only worker for `start_read_request`,
/// and the sheet said "error opening page". From the bench Mac the same thing
/// was every `curl` failing for as long as one idle `nc` was connected. It is
/// card 227's finding again, on the other interface. The LAN has no HTTP for
/// as long as the AP is up - which is the portal (no LAN at all) plus the 30 s
/// grace window after a join, when the one client that matters is on the AP.
///
/// A *third* worker bound to the AP stack was the obvious shape and is what
/// this card did not do: a worker is 7,504 bytes of `.bss`
/// ([`HTTP_TASKS`]'s documentation prices it), and `.bss` is core 0's stack, so
/// a third one would have spent the whole of the card's 2,432-byte lever three
/// times over. Moving one instead costs nothing measurable - the two arms of
/// the match below hold the same `listen_and_serve` future type, so the task's
/// own future is the size of one of them, not two - and the moment it matters
/// is the moment it is free: while the AP is up the station is, by the
/// machine's own rules, not on a network, so there is nobody on the LAN for
/// the borrowed worker to have served.
async fn serve_on(
    id: usize,
    stack: Stack<'static>,
    ap: bool,
    app: &Router<ServicePathRouter<Dispatch>>,
    config: &picoserve::Config,
    http_buf: &mut [u8],
    rx: &mut [u8],
    tx: &mut [u8],
) {
    info!(
        "net: http worker {} listening on tcp/{} ({})",
        id,
        HTTP_PORT,
        if ap { "setup network" } else { "lan" }
    );
    // Card 236: this is picoserve's own `listen_and_serve` loop
    // (`picoserve/src/lib.rs:703-780`) spelt out here, for two reasons, and
    // *only* those two - every byte of HTTP is still picoserve's.
    //
    // 1. The socket handed to `serve` is a [`BoundedSocket`], so the close is
    //    this device's decision and not the client's. That is the card, and the
    //    wrapper's documentation is where the reasoning lives.
    // 2. picoserve logs three `info!` lines per connection ("Received
    //    connection from", "N requests handled from", "Listening on TCP:80...")
    //    and two of them sit *between* the close and the next `accept`. That is
    //    ~150 bytes through `esp-println`'s **blocking** UART at 230,400 baud,
    //    ~6.5 ms of core 0 on exactly the path this card is shortening, on
    //    every request, forever. This module's rule is already that per-request
    //    logging happens on the setup network only (see [`Dispatch`], which
    //    still logs there); picoserve's lines were outside that rule only
    //    because picoserve wrote them.
    //
    // The two socket options below are picoserve's own, copied deliberately:
    // 45 s of smoltcp inactivity aborts a half-open connection that none of the
    // picoserve timeouts can see, and the 30 s keep-alive is what makes that
    // timer mean "the peer is gone" rather than "the peer is quiet".
    loop {
        let mut socket = TcpSocket::new(stack, rx, tx);
        socket.set_keep_alive(Some(Duration::from_secs(30)));
        socket.set_timeout(Some(Duration::from_secs(45)));
        if let Err(e) = socket.accept(HTTP_PORT).await {
            // picoserve logged and went straight round again. A fresh socket is
            // always in `Closed` and the port is a constant, so neither
            // `AcceptError` can actually happen - but "cannot happen" plus a
            // loop with a log line in it is how a device fills a serial log at
            // line rate, so this one pauses.
            warn!("net: http worker {} could not accept: {:?}", id, e);
            Timer::after(Duration::from_millis(100)).await;
            continue;
        }
        // The result is the connection's, not the server's: a client that
        // vanishes mid-request is an ordinary event on a LAN and the next
        // `accept` is the whole response to it.
        let _ = Server::new(app, config, http_buf)
            .serve(BoundedSocket(socket))
            .await;
    }
}

// ---------------------------------------------------------------------------
// Getting back to `accept` (card 236)
// ---------------------------------------------------------------------------

/// How long the close waits for the client to acknowledge the response.
///
/// **1,500 ms since the bench measured what 500 was costing.** Card 236 chose
/// 500 on the reasoning that on this LAN the acknowledgement is one round trip
/// - single-digit milliseconds - and that BSD and Linux both set `TF_ACKNOW` on
/// a FIN, so it is not subject to the delayed-ACK timer that makes everything
/// else about a Mac's TCP adaptive; the half-second was for smoltcp's first
/// retransmit of the FIN, so that one lost segment still ends in a clean close.
///
/// That reasoning was about the *wire* and left out the radio. On the device,
/// under the Studio's 10 s status poll over WiFi, firmware 0.5.3 logged
/// "the peer never acknowledged the close; dropping it" **three times in
/// ~20 minutes** - about 120 polls, at an RSSI of roughly -57 dBm. So the
/// FIN's acknowledgement took longer than 500 ms about 2.5% of the time, on a
/// quiet link with a well-behaved client. That is WiFi latency: a station that
/// has gone to sleep between beacons, or an access point holding a frame for
/// one DTIM period, both of which are hundreds of milliseconds and neither of
/// which is anything to do with the client's TCP.
///
/// 1,500 ms covers that with room, and it is still an eighth of what it
/// replaced: picoserve's own close could take the whole 5 s `read_request`
/// timeout *and* waited for the client's application to close. The worst case
/// a client can hold a worker is now `start_read_request` + `read_request` +
/// `write` + this = 13.5 s of stalling before the reply and 1.5 s after it -
/// and with two workers the other one is in `accept` throughout, which is the
/// property card 236 was really buying.
///
/// The `warn!` in [`BoundedSocket::settle`] stays as it is: after 1.5 s of
/// silence it really is a peer that is not coming back, and it should be said.
const CLOSE_ACK_MS: u64 = 1500;

/// An `embassy-net` TCP socket whose close is bounded by **this** device.
///
/// ## What picoserve's own socket does, and why it is wrong here
///
/// `impl Socket for TcpSocket` (`picoserve/src/io.rs:339`) performs a textbook
/// graceful shutdown: send the FIN, then read until the peer sends *its* FIN,
/// then wait for the last acknowledgement. The middle step is
/// `ReadExt::discard_all_data`, and embassy-net reports end-of-stream only when
/// smoltcp answers `RecvError::Finished` - which happens when **the client's
/// application** closes its socket. Nothing else ends that await but the 5 s
/// `read_request` timeout.
///
/// smoltcp has no listen backlog, so a worker parked there is a connection this
/// device *refuses*. With two workers, a sequential client only has to be
/// slower to close than one request takes and the third connection is refused
/// with nothing logged on the device - which is what `screeny-probe http`
/// measured on 2026-09-20: 9 to 17 refusals of ~35 requests, where the same
/// build an hour earlier had had none. There was no firmware change between
/// those runs because the variable was never in the firmware. It is the same
/// weakness the owner's iPhone hit on the setup network (card 223, finding 3),
/// seen from the other end of the connection.
///
/// ## What this one does instead
///
/// `close()`, then wait for the **acknowledgement** and nothing else, then
/// drop. The wait is `TcpSocket::flush`, which embassy-net
/// (`embassy-net/src/tcp.rs:629`) defines as "no unacknowledged data, and the
/// state is no longer `FinWait1 | Closing | LastAck`" - that is, every byte we
/// sent *and the FIN* have been acknowledged by the peer's TCP.
///
/// **It cannot truncate a response.** The FIN occupies the sequence number
/// after the last body byte, so an acknowledgement of the FIN is by definition
/// an acknowledgement of everything before it: when `flush` returns, the whole
/// reply is in the client's receive buffer. On top of that, every response this
/// server sends carries `Content-Length` - picoserve measures each body with a
/// counting writer before it writes a header, the streamed status page
/// included - so no client here has to see the close to know where the body
/// ended.
///
/// What is given up is the second half of the four-way close: we leave the
/// connection in `FIN-WAIT-2` and drop it, rather than waiting for the client's
/// FIN. `Drop` (`embassy-net/src/tcp.rs:467`) removes the socket from smoltcp's
/// set, so the client's FIN, when it eventually comes, matches nothing and
/// smoltcp answers it with a RST. By then that client has every byte and has
/// closed; a client still *reading* sends nothing, so it draws no RST at all.
/// The one case left - a client that has the bytes, has not read them, and
/// sends a window update - gets a RST with data already in its socket buffer,
/// and both Darwin's `soreceive` and Linux's `tcp_recvmsg` hand the buffered
/// bytes to the application before they report the error. That is the trade
/// decision 10 asks for: good enough and crash proof, not bomb-proof.
///
/// It costs no RAM. It is a newtype around the socket picoserve would have used
/// (the read and write halves are still `TcpReader`/`TcpWriter`, so the request
/// path monomorphises exactly as before), and it *removes* the 128-byte discard
/// buffer that `discard_all_data` held across an await.
struct BoundedSocket<'a>(TcpSocket<'a>);

impl BoundedSocket<'_> {
    /// Wait for everything queued - data, FIN or RST - to be acknowledged, or
    /// give up after [`CLOSE_ACK_MS`].
    ///
    /// Giving up abandons unacknowledged bytes, which is the honest thing to do
    /// and the reason the timeout is generous: half a second of silence from a
    /// peer on the same LAN, after smoltcp has already retransmitted, is a peer
    /// that is not coming back. Holding the worker for it would be the bug this
    /// card is about.
    async fn settle(&mut self) -> Result<(), picoserve::Error<embassy_net::tcp::Error>> {
        match embassy_time::with_timeout(
            Duration::from_millis(CLOSE_ACK_MS),
            self.0.flush(),
        )
        .await
        {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(picoserve::Error::Write(e)),
            Err(embassy_time::TimeoutError) => {
                warn!("http: the peer never acknowledged the close; dropping it");
                Ok(())
            }
        }
    }
}

impl picoserve::io::Socket<picoserve::EmbassyRuntime> for BoundedSocket<'_> {
    type Error = embassy_net::tcp::Error;
    type ReadHalf<'b>
        = TcpReader<'b>
    where
        Self: 'b;
    type WriteHalf<'b>
        = TcpWriter<'b>
    where
        Self: 'b;

    fn split(&mut self) -> (Self::ReadHalf<'_>, Self::WriteHalf<'_>) {
        self.0.split()
    }

    /// picoserve asks for this when the handler did not read the whole request
    /// body, i.e. when there are bytes on the wire that no reply accounts for.
    /// A RST is the right answer and is what picoserve's own socket sends; the
    /// only change is the bound on waiting for it to leave.
    async fn abort<T: picoserve::Timer<picoserve::EmbassyRuntime>>(
        mut self,
        _timeouts: &picoserve::Timeouts,
        _timer: &T,
    ) -> Result<(), picoserve::Error<Self::Error>> {
        // Spelt out because `picoserve::io::Socket::abort` is in scope here and
        // is what `self.0.abort()` would resolve to.
        TcpSocket::abort(&mut self.0);
        self.settle().await
    }

    /// The path every ordinary request takes. See the type's documentation.
    ///
    /// `_timeouts` is picoserve's set, and it is deliberately not used: the two
    /// it would offer here are `read_request` and `write`, both 5 s, both sized
    /// for a request that is still arriving rather than for a connection that
    /// is over.
    async fn shutdown<T: picoserve::Timer<picoserve::EmbassyRuntime>>(
        mut self,
        _timeouts: &picoserve::Timeouts,
        _timer: &T,
    ) -> Result<(), picoserve::Error<Self::Error>> {
        self.0.close();
        self.settle().await
    }
}

#[embassy_executor::task(pool_size = HTTP_TASKS)]
pub async fn http_task(id: usize, stack: Stack<'static>, ap_stack: Stack<'static>) -> ! {
    let lan = router(false);
    let portal = router(true);

    // A stalled client must not be able to pin the one worker, so every phase
    // has a deadline: 3 s to send a request line at all, 5 s to finish a
    // request that has started, 5 s for the reply to be accepted, and - card
    // 236, in [`BoundedSocket`] rather than here, because picoserve's `Config`
    // has no knob for it - [`CLOSE_ACK_MS`] for the close. The worst a client
    // can hold the server for is therefore ~9.5 s, and that needs it to have
    // connected and then gone quiet mid-header. **The close is no longer part
    // of that sum in any interesting way**: it used to be able to add 10 s of
    // its own, and it was the *ordinary* case, not the stalled one, that paid.
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

    // Card 222's bench self-test, on worker 0, with this worker's buffers
    // (card 243 - see [`selftest`] for what it used to cost as a task of its
    // own). It serves the LAN for the 75 s the self-test wants to wait, so the
    // device is answering normally right up to the moment it runs, and then
    // falls into the ordinary loop below for the rest of its life.
    #[cfg(feature = "http-selftest")]
    if id == 0 {
        let _ = select(
            serve_on(
                id,
                stack,
                false,
                &lan,
                &config,
                &mut http_buf[..],
                &mut rx[..],
                &mut tx[..],
            ),
            Timer::after(Duration::from_secs(75)),
        )
        .await;
        selftest(stack, &mut http_buf[..], &mut rx[..], &mut tx[..]).await;
    }

    loop {
        // Cancelling `listen_and_serve` drops whatever connection it was
        // serving, which is right in both directions: the AP going up means
        // the station is about to lose its network anyway, and the AP going
        // down means the interface under the socket is being taken away.
        if crate::provision::ap_up() {
            let _ = select(
                serve_on(
                    id,
                    ap_stack,
                    true,
                    &portal,
                    &config,
                    &mut http_buf[..],
                    &mut rx[..],
                    &mut tx[..],
                ),
                crate::provision::wait_ap_pub(false),
            )
            .await;
        } else {
            let _ = select(
                serve_on(
                    id,
                    stack,
                    false,
                    &lan,
                    &config,
                    &mut http_buf[..],
                    &mut rx[..],
                    &mut tx[..],
                ),
                crate::provision::wait_ap_pub(true),
            )
            .await;
        }
    }
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
/// `http_buf` and `out` are **the calling worker's own buffers** (card 243):
/// this runs on HTTP worker 0 while it is not listening, so its 1,536-byte
/// request buffer and its 1,024-byte receive buffer are free, and a second set
/// would be `.bss` - which is core 0's stack.
///
/// Returns `(status, body_len, micros)`.
#[cfg(feature = "http-selftest")]
async fn selftest_one(
    ap: bool,
    request: &str,
    out: &mut [u8],
    http_buf: &mut [u8],
) -> (u16, usize, u32) {
    let app = router(ap);
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
        let _ = Server::new(&app, &config, http_buf).serve(socket).await;
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
///
/// **Not a task of its own since card 243, and this is the whole point of the
/// change.** As a task it cost **5,640 bytes of `.bss`** - its own `rx`, `tx`,
/// read and reply buffers, its own picoserve request buffer, and a second copy
/// of picoserve's whole `serve` future held across an `await` - and `.bss` is
/// core 0's stack, so the `http-selftest` build had `.stack` at 20,552 against
/// a floor of 24,576 and could not honestly be flashed. It runs on **HTTP
/// worker 0**, after that worker has served for 75 s and before it goes back to
/// listening, and borrows the worker's buffers. Nothing here is a byte the
/// shipping build does not already own: the only `.bss` the feature still costs
/// is whatever the worker's own future grows by, and the two halves of a
/// generator that never run at once share the same bytes.
///
/// What is different about the device under test while this runs: worker 0 is
/// busy for the length of the self-test (a couple of seconds), so the server is
/// worker 1 alone. That is the same one-worker configuration card 222 shipped
/// and card 227 measured, and it is the price of not adding 5.6 KB of `.bss`
/// to a build whose whole purpose is to report how much room is left.
#[cfg(feature = "http-selftest")]
async fn selftest(
    stack: Stack<'static>,
    http_buf: &mut [u8],
    rx: &mut [u8],
    tx: &mut [u8],
) {
    use embedded_io_async::Write as _;

    // Three, not the forty the card suggested: the first run measured that a
    // station interface has no route to its own address, and repeating a
    // three-second timeout thirty-nine more times proves nothing. The
    // in-memory pass below is where the requests actually happen.
    const REQUESTS: usize = 3;
    const REQ: &[u8] =
        b"GET /api/v1/status HTTP/1.1\r\nHost: selftest\r\nConnection: close\r\n\r\n";

    // The 75 s wait is the caller's (`http_task`), which spends it *serving*
    // rather than idling, and it is after the telemetry task's 60 s `stack:`
    // line on purpose: that line is the baseline this run is compared against
    // and it should be measured with the server quiet.
    //
    // **Not an early return** (card 223). A `start-in-portal` build has no
    // station address at all - that is the whole point of it - and the TCP
    // half of this self-test was never the interesting half. Skipping it and
    // going on to the in-memory pass is what makes the portal build
    // measurable; returning here is what made the first `start-in-portal`
    // flash print nothing.
    let me = stack.config_v4().map(|c| c.address.address());

    // The window this is measured over: the frame path either noticed or it
    // did not, and these are the numbers that say which.
    let before = {
        let mut guard = CORE.lock().await;
        guard.as_mut().expect("core exists").telemetry(now_us())
    };
    crate::RENDER_US_MAX_WINDOW.store(0, Ordering::Relaxed);
    let t_window = Instant::now();

    let mut ok = 0usize;
    let mut failed = 0usize;
    let mut bytes_total = 0usize;
    let mut us_max = 0u32;
    let mut us_total = 0u64;
    let mut first_status: heapless::String<16> = heapless::String::new();

    for i in (0..REQUESTS).take_while(|_| me.is_some()) {
        let target = IpEndpoint::new(IpAddress::Ipv4(me.expect("checked")), HTTP_PORT);
        let t0 = Instant::now();
        // The worker's own smoltcp buffers, and its request buffer to read the
        // reply into: this worker is not listening while this runs, so all
        // three are free. See this function's docs.
        let mut sock = TcpSocket::new(stack, &mut rx[..], &mut tx[..]);
        sock.set_timeout(Some(Duration::from_secs(3)));
        let buf = &mut http_buf[..];
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
                    let line = core::str::from_utf8(&http_buf[..n.min(16)]).unwrap_or("");
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
                "selftest: embassy-net cannot reach its own address {:?} - no loopback on a station interface. Falling back to reporting readiness.",
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
            "selftest: fallback evidence - {} accept loop(s) listening on tcp/{} while this runs, station address {:?}, soft-AP {}",
            HTTP_TASKS - 1,
            HTTP_PORT,
            me,
            if crate::provision::ap_up() { "up" } else { "down" },
        );
    }

    // --- the router, exercised without a network ---------------------------
    //
    // "Can the device reach itself over TCP" is not the question; "does every
    // route answer the right thing on the real device" is, and that needs only
    // a `picoserve::io::Socket`. This one is two byte slices, so every route
    // below is the real router, the real handlers, the real locks and the real
    // JSON, running while the Studio streams.
    // The reply lands in the worker's 1,024-byte receive buffer (card 243:
    // nothing here has a buffer of its own). Every JSON reply fits - the
    // largest, `status`, is 520 bytes once it carries a panic record. `GET /`
    // does not, the page being ~5.5 KB, and that is fine: the status line is
    // what is being checked and the overflow is counted and reported rather
    // than silently dropped.
    let out = &mut rx[..];
    let mut worst_us = 0u32;
    let hw_before = crate::stack_probe::CORE0.high_water().unwrap_or(0);
    for (label, request, expect) in SELFTEST_ROUTES {
        let (status, bytes, us) = selftest_one(false, request, out, http_buf).await;
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

    // --- card 223: the portal-side routes and the catch-all ----------------
    //
    // A worker cannot reach the soft-AP (there is no phone and no route to
    // 192.168.4.x from this bench), so the same in-memory socket is pointed at
    // the **AP** dispatch instead. What it proves is exactly what the card
    // asks: a captive probe gets the setup page while `ap_up()` and a 404
    // when the AP is down, `GET /` is the setup form on that side and the
    // status page on the other, and the form's GET and POST answer HTML.
    //
    // The two arms of every pair are the same request, so the only variable is
    // whether the soft-AP is up - which is why the AP-down half is run first,
    // against the device exactly as it is running.
    let ap_now = crate::provision::ap_up();
    info!("selftest: portal pass, soft-AP is {}", if ap_now { "up" } else { "down" });
    for (label, ap, request, expect) in SELFTEST_PORTAL {
        let (status, bytes, us) = selftest_one(*ap, request, out, http_buf).await;
        // The catch-all only fires while the AP is actually up, so the
        // expected code for those two rows depends on the device's state and
        // not on the table.
        let want = match (*expect, ap_now) {
            (CAPTIVE, true) => 200,
            (CAPTIVE, false) => 404,
            (w, _) => w,
        };
        let head = core::str::from_utf8(&out[..bytes.min(out.len())]).unwrap_or("<binary>");
        info!(
            "selftest: {:<30} -> {} (want {}) {} {} bytes, {} us | location {:?}",
            label,
            status,
            want,
            if status == want { "OK  " } else { "WRONG" },
            bytes,
            us,
            head.split_once("Location: ")
                .and_then(|(_, r)| r.split_once('\r'))
                .map(|(l, _)| l)
                .unwrap_or("-"),
        );
        Timer::after(Duration::from_millis(200)).await;
    }

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
///
/// **Card 222's twelve cases are below unchanged**, which is the point: they
/// are the before/after evidence for card 233's dispatch. Eight more follow
/// them, and every one is a question only the new dispatch can be asked:
///
/// * `DELETE /api/v1/status` and `PUT /api/v1/settings` - a verb this API has
///   no [`route::Method`] for, which used to be picoserve's plain-text 405
///   (probe rules 26 and 37).
/// * `HEAD /` and `POST /` - the two methods `/` does not take. `HEAD` is a
///   405 *by decision*; see the module docs.
/// * `/api/v1/status/` and `/api/v1/%73tatus` - the two ways a path can nearly
///   be a route. The first must be a 404 and the second must be `status`,
///   because `known_path` compares the way picoserve's `Route` did.
/// * `POST /api/v1/identify` with 153 bytes and `POST /api/v1/wifi` with 385 -
///   one byte over the route's own `max_request_len` (152) and one over the
///   global `MAX_REQUEST_LEN` (384). The identify one is the interesting half:
///   153 is comfortably inside the global bound, so a server that knew only
///   that number would parse it and answer `400`, and `413` is the proof that
///   the per-route bound is being read from the table (probe rules 28 and 29).
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
        "POST reboot unconf (out_of_range)",
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
    // --- card 233 -------------------------------------------------------
    (
        "DELETE /api/v1/status (405)",
        "DELETE /api/v1/status HTTP/1.1\r\nHost: s\r\nConnection: close\r\n\r\n",
        405,
    ),
    (
        "PUT /api/v1/settings (405)",
        "PUT /api/v1/settings HTTP/1.1\r\nHost: s\r\nConnection: close\r\n\r\n",
        405,
    ),
    (
        "HEAD / (405, card 233)",
        "HEAD / HTTP/1.1\r\nHost: s\r\nConnection: close\r\n\r\n",
        405,
    ),
    (
        "POST / (405)",
        "POST / HTTP/1.1\r\nHost: s\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
        405,
    ),
    (
        "GET /api/v1/status/ (404)",
        "GET /api/v1/status/ HTTP/1.1\r\nHost: s\r\nConnection: close\r\n\r\n",
        404,
    ),
    (
        "GET /api/v1/%73tatus (200)",
        "GET /api/v1/%73tatus HTTP/1.1\r\nHost: s\r\nConnection: close\r\n\r\n",
        200,
    ),
    (
        "POST identify 153 > 152 (413)",
        "POST /api/v1/identify HTTP/1.1\r\nHost: s\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: 153\r\n\r\n{\"nothing\":\"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\"}",
        413,
    ),
    (
        "POST wifi 385 > 384 (413)",
        "POST /api/v1/wifi HTTP/1.1\r\nHost: s\r\nConnection: close\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: 385\r\n\r\nnothing=xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
        413,
    ),
];

/// Card 223's portal pass: `(label, on the AP side, request, expected status)`.
///
/// Each row is a question only the AP/LAN split can be asked. The
/// [`CAPTIVE`] rows are conditional on the soft-AP actually being up when the
/// self-test runs - see the loop - because that, and not the table, is what
/// the catch-all is gated on.
#[cfg(feature = "http-selftest")]
const CAPTIVE: u16 = 0;

#[cfg(feature = "http-selftest")]
static SELFTEST_PORTAL: &[(&str, bool, &str, u16)] = &[
    // The three captive probes, on the setup network: the setup page, `200`,
    // and **not** a redirect to it (see `route_request`, step 1).
    (
        "AP  captive.apple.com (form)",
        true,
        "GET /hotspot-detect.html HTTP/1.1\r\nHost: captive.apple.com\r\nConnection: close\r\n\r\n",
        CAPTIVE,
    ),
    (
        "AP  android generate_204 (form)",
        true,
        "GET /generate_204 HTTP/1.1\r\nHost: connectivitycheck.gstatic.com\r\nConnection: close\r\n\r\n",
        CAPTIVE,
    ),
    (
        "AP  windows ncsi (form)",
        true,
        "GET /connecttest.txt HTTP/1.1\r\nHost: www.msftconnecttest.com\r\nConnection: close\r\n\r\n",
        CAPTIVE,
    ),
    // The same probe on the LAN is an ordinary unknown path. This row is the
    // one that says the catch-all is a property of the listener and not of the
    // `Host:` header.
    (
        "LAN captive.apple.com (404)",
        false,
        "GET /hotspot-detect.html HTTP/1.1\r\nHost: captive.apple.com\r\nConnection: close\r\n\r\n",
        404,
    ),
    // `/` is the form on one side and the status page on the other.
    (
        "AP  GET / is the setup form",
        true,
        "GET / HTTP/1.1\r\nHost: 192.168.4.1\r\nConnection: close\r\n\r\n",
        200,
    ),
    (
        "LAN GET / is the status page",
        false,
        "GET / HTTP/1.1\r\nHost: s\r\nConnection: close\r\n\r\n",
        200,
    ),
    // The form itself, on both sides, and its POST. The POST uses an SSID that
    // cannot exist, so the trial it starts fails at `not_found` rather than
    // taking the device off its network - and `persist` is irrelevant because
    // nothing is stored until a join succeeds.
    (
        "AP  GET /setup",
        true,
        "GET /setup HTTP/1.1\r\nHost: 192.168.4.1\r\nConnection: close\r\n\r\n",
        200,
    ),
    (
        "LAN GET /setup",
        false,
        "GET /setup HTTP/1.1\r\nHost: s\r\nConnection: close\r\n\r\n",
        200,
    ),
    (
        "AP  POST /setup no ssid (200 html)",
        true,
        "POST /setup HTTP/1.1\r\nHost: 192.168.4.1\r\nConnection: close\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: 6\r\n\r\npsk=xy",
        200,
    ),
    (
        "AP  POST /setup 385 > 384 (413)",
        true,
        "POST /setup HTTP/1.1\r\nHost: 192.168.4.1\r\nConnection: close\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: 385\r\n\r\nnothing=xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
        413,
    ),
    (
        "AP  PUT /setup (405)",
        true,
        "PUT /setup HTTP/1.1\r\nHost: 192.168.4.1\r\nConnection: close\r\n\r\n",
        405,
    ),
];
