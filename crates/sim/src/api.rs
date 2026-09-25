//! The device's HTTP API, served by the simulator.
//!
//! Every route in [`screeny_device_api::route::ROUTES`], answered with that
//! crate's own types, its error shape and its status codes.
//! `tests/http_routes.rs` walks the table and fails if one of them is not
//! covered, so this file cannot fall behind the definition.
//!
//! # Three rules this file keeps
//!
//! 1. **No shape is defined here.** Every request and reply is a
//!    `screeny_device_api` type. If something is missing there it is wrapped
//!    locally *and* written down in card 224's log, never quietly invented.
//! 2. **Every mutation goes through the UDP control path.** `POST
//!    /api/v1/settings`, `/identify` and `/reboot` build a
//!    [`screeny_proto::control::Request`], write it with `Request::write` and
//!    hand it to the same [`Core::control`](crate::Core::control) a datagram
//!    reaches; the reply datagram is decoded for its error code and then
//!    dropped rather than sent. So brightness clamping, name truncation and
//!    `REBOOT`'s magic word cannot differ between a browser and a UDP sender,
//!    because they are one implementation.
//! 3. **No PSK, anywhere.** `POST /api/v1/wifi` hands the parsed form's SSID
//!    to the provisioning machine and drops the [`WifiForm`] on the next line.
//!    Nothing keeps it, no event has a field for it, no reply has a field for
//!    it, and `tests/http_wifi.rs` greps every byte the simulator produced.
//!
//! # What a simulator cannot answer honestly
//!
//! `fw_slot`, `fw_state`, `reset_reason`, `stack_free`, `heap_*` and
//! `store_errors` are made up in [`crate::core::Ident`]: there is no flash
//! here and no stack worth measuring. Card 192 lets a run *choose* them -
//! `--reset-reason brownout`, `--store-errors 3` and the rest, or
//! [`SimHandle::set_health`](crate::SimHandle::set_health) on a running
//! device - so that the rows of a status page nobody ever sees can be seen.
//! They stay reports and nothing else: no behaviour here reads them back, a
//! `pending_verify` slot changes nothing and a `brownout` reason reboots
//! nothing. The single exception is the one a device has too - a simulated
//! `REBOOT`, over UDP or `POST /api/v1/reboot`, sets the reset reason to
//! `software` from then on. `POST /api/v1/firmware` accepts the stream,
//! discards it, and runs **every one of research 006 section 5's checks**
//! through `screeny-fwimage` - the same scanner the firmware runs, on the
//! same bytes, in the same order (card 240). What it cannot do is keep the
//! image: it has no flash, so an accepted upload changes nothing and a restart
//! brings back the same simulator. Refusing, though, it does exactly as the
//! device does.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use screeny_device_api::error::{ErrorCode, ErrorReply};
use screeny_device_api::form::parse_wifi_form;
use screeny_device_api::reply::{
    AcceptedReply, FirmwareReply, NetworksReply, SettingsReply, StatusReply, TelemetryReply,
};
use screeny_device_api::request::{IdentifyRequest, Mutating, RebootRequest, SettingsRequest};
use screeny_device_api::{route, FirmwareError, StreamState};
use screeny_proto::control::{self as proto, Reply, Request};

use crate::core::Core;
use crate::device::Shared;
use crate::event::Event;
use crate::http::{Body, Handler, Head, Response, WantsStream};
use crate::wifi::Posted;

/// How often `GET /api/v1/networks` will really scan.
///
/// Card 224 kept the simulator's own copy of the number and proposed moving it
/// into the shared crate; card 232 did. This is now
/// [`route::SCAN_MIN_INTERVAL_MS`] under the name the simulator's callers
/// already use, so the firmware and the simulator cannot drift.
pub const SCAN_MIN_INTERVAL_MS: u64 = route::SCAN_MIN_INTERVAL_MS as u64;

/// The inactive app slot's size: research 006's `ota_0` at 0x10000 and `ota_1`
/// at 0x210000, so 2 MiB each. An upload longer than this is
/// [`FirmwareError::TooLarge`] on the device and here.
pub const SLOT_LEN: u64 = 0x20_0000;

/// The captive-portal answer's address, from `screeny_provision`.
const PORTAL_IP: &str = screeny_provision::PORTAL_IP;

/// What a captive probe gets while the portal is up: the setup page itself.
///
/// A stand-in for the firmware's real form, which the simulator deliberately
/// does not serve (`docs/design/device-web.md`, decision 10) - what matters
/// here is the *answer*, not the markup. **The body is not decoration.**
/// ESP-IDF's own example says iOS needs content in the response to detect a
/// portal, and Android classifies a `Content-Length <= 4` answer as *failed*
/// rather than as a portal (research 007 section 4.2).
const PORTAL_PAGE: &str = concat!(
    "<html><head><title>screeny setup</title></head><body>",
    "<h1>screeny setup</h1>",
    "<p>This is the simulator standing in for the panel's setup page.</p>",
    "</body></html>\n"
);

/// The one-line placeholder at `GET /`.
///
/// The real page is card 222's, and it will be shared with the firmware rather
/// than written twice. Until then this says so and links the API, so that
/// somebody who opened the simulator in a browser is not left guessing.
fn index_html() -> String {
    let mut s = String::from(
        "<!doctype html><html><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <title>screeny-sim</title></head><body>\
         <h1>screeny-sim</h1>\
         <p>This is the simulator. The device's HTML page is card 222's and will be \
         served here when it exists; the JSON API below is real and is the same \
         <code>screeny-device-api</code> the firmware serves.</p><ul>",
    );
    for r in route::ROUTES {
        let method = match r.method {
            route::Method::Get => "GET",
            route::Method::Post => "POST",
        };
        if matches!(r.method, route::Method::Get) {
            s.push_str(&format!(
                "<li>{method} <a href=\"{path}\"><code>{path}</code></a></li>",
                path = r.path
            ));
        } else {
            s.push_str(&format!("<li>{method} <code>{}</code></li>", r.path));
        }
    }
    s.push_str("</ul></body></html>\n");
    s
}

/// An error reply in the crate's shape, with the crate's status code.
#[must_use]
pub fn error_response(code: ErrorCode, detail: &str) -> Response {
    let reply = ErrorReply::with_detail(code, detail);
    Response::json(
        reply.status(),
        serde_json::to_vec(&reply).unwrap_or_else(|_| b"{\"error\":\"internal\"}".to_vec()),
    )
}

fn bare(code: ErrorCode) -> Response {
    let reply = ErrorReply::new(code);
    Response::json(
        reply.status(),
        serde_json::to_vec(&reply).unwrap_or_else(|_| b"{\"error\":\"internal\"}".to_vec()),
    )
}

fn ok_json<T: serde::Serialize>(value: &T) -> Response {
    match serde_json::to_vec(value) {
        Ok(body) => Response::json(200, body),
        Err(_) => bare(ErrorCode::Internal),
    }
}

/// The simulator's answer to "is this `Host` mine?", research 007 section 4.3.
///
/// A **rule**, not a list of probe domains: the portal IP, any bare IP
/// literal, `localhost`, and this device's own mDNS names. Everything else is
/// somebody's captive-portal probe.
#[must_use]
pub fn host_is_ours(host: Option<&str>, instance: &str) -> bool {
    let Some(host) = host else {
        // HTTP/1.0 with no Host, or curl on a raw socket. Not a probe.
        return true;
    };
    // Lowercased here rather than trusting the caller: this is the rule, and
    // a rule that is only right for one caller is not one.
    let host = host.trim().to_ascii_lowercase();
    let host = host.as_str();
    if host.is_empty() {
        return true;
    }
    // Strip the port, taking care not to cut an unbracketed IPv6 literal.
    let name = if let Some(rest) = host.strip_prefix('[') {
        rest.split(']').next().unwrap_or(rest)
    } else if host.matches(':').count() == 1 {
        host.split(':').next().unwrap_or(host)
    } else {
        host
    };
    if name == PORTAL_IP || name == "localhost" || name.parse::<std::net::IpAddr>().is_ok() {
        return true;
    }
    let name = name.strip_suffix('.').unwrap_or(name);
    let bare = name.strip_suffix(".local").unwrap_or(name);
    bare.eq_ignore_ascii_case(instance)
}

// ---------------------------------------------------------------------------
// The handler
// ---------------------------------------------------------------------------

/// State the API keeps that the device does not: when the last scan was.
///
/// The window itself is [`route::RateLimit`], so the simulator and the
/// firmware enforce the same rule with the same code rather than two
/// hand-rolled comparisons that agree today. It takes `now_ms`, which is what
/// lets `crates/device-api` test the 49.7-day wrap in microseconds; here the
/// clock is `Instant::now()` since the process started.
#[derive(Debug)]
pub struct ApiState {
    boot: Instant,
    scan: Mutex<route::RateLimit>,
}

impl ApiState {
    /// A fresh one. Nothing has scanned yet, so the first request always does.
    #[must_use]
    pub fn new() -> Self {
        ApiState {
            boot: Instant::now(),
            scan: Mutex::new(route::RateLimit::new(route::SCAN_MIN_INTERVAL_MS)),
        }
    }

    /// Milliseconds since this state was built, as the `u32` the limiter takes.
    fn now_ms(&self) -> u32 {
        self.boot.elapsed().as_millis() as u32
    }

    /// True if a scan is allowed now, and records it if so. A refused request
    /// does **not** push the window out - see [`route::RateLimit`].
    fn allow_scan(&self) -> bool {
        let now = self.now_ms();
        self.scan
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .allow(now)
    }

    /// How long until the next scan would be allowed, for the refusal's
    /// `detail`.
    fn scan_retry_after_ms(&self) -> u32 {
        let now = self.now_ms();
        self.scan
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retry_after_ms(now)
    }
}

impl Default for ApiState {
    fn default() -> Self {
        Self::new()
    }
}

/// Build the closure the HTTP transport calls.
pub(crate) fn handler(shared: Arc<Shared>) -> Arc<Handler> {
    let state = Arc::new(ApiState::new());
    Arc::new(move |head: &Head, body: Body<'_>| {
        let response = dispatch(&shared, &state, head, body);
        shared.publish(&[Event::Http {
            method: head.method.clone(),
            path: head.path.clone(),
            status: response.status,
        }]);
        response
    })
}

/// Which routes stream their body. Only the firmware upload, which a device
/// writes to flash as it arrives and must never buffer.
pub(crate) fn wants_stream() -> Arc<WantsStream> {
    Arc::new(|head: &Head| head.method == "POST" && head.path == route::FIRMWARE)
}

fn dispatch(shared: &Shared, state: &ApiState, head: &Head, body: Body<'_>) -> Response {
    // The captive-portal catch-all comes first, before routing: research 007
    // section 4.3's rule is about the `Host`, not about the path, and
    // Microsoft's portal guidance is explicit that a portal must not answer
    // some probes and drop others.
    if !host_is_ours(head.host.as_deref(), shared.instance()) {
        let in_portal = {
            let core = shared.core().lock().unwrap();
            matches!(
                core.wifi().phase(),
                screeny_provision::State::Portal | screeny_provision::State::Trial
            )
        };
        return if in_portal {
            // **The setup page itself, `200`, never a `302` to it**, which is
            // what fw 0.5.1 does after the phone test (card 223, finding 3):
            // a redirect made iOS open a *further* connection, and
            // smoltcp has no backlog, so the SYN was refused and iOS - unlike
            // macOS - does not retry one. `Cache-Control: no-store` is on
            // every response this server writes.
            Response::html(200, PORTAL_PAGE)
        } else {
            // Not provisioning: there is no portal to send anybody to, so the
            // honest answer is that the simulator does not serve that host.
            bare(ErrorCode::NotFound)
        };
    }

    // Card 246: a device that is rebooting into a new image answers nothing
    // at all. `NO_ANSWER` is the transport's "close it without a reply", which
    // is as close to unplugged as a loopback server gets.
    if shared.ota().phase() == crate::ota::Phase::Away {
        return crate::http::no_answer();
    }

    if head.path == "/" || head.path == "/index.html" {
        return match head.method.as_str() {
            "GET" => Response::html(200, &index_html()),
            _ => bare(ErrorCode::MethodNotAllowed),
        };
    }

    // The table is walked by `crates/device-api`, not here: `route::find` and
    // `route::path_is_known` are the crate's own lookup (card 232), so the
    // 405-vs-404 rule is one implementation the firmware shares rather than
    // two that happen to agree.
    let method = match head.method.as_str() {
        "GET" => route::Method::Get,
        "POST" => route::Method::Post,
        // Any other verb: the route may exist, but not like this.
        _ => {
            return if route::path_is_known(&head.path) {
                bare(ErrorCode::MethodNotAllowed)
            } else {
                bare(ErrorCode::NotFound)
            }
        }
    };

    if route::find(&head.path, method).is_none() {
        return if route::path_is_known(&head.path) {
            bare(ErrorCode::MethodNotAllowed)
        } else {
            bare(ErrorCode::NotFound)
        };
    }

    match (method, head.path.as_str()) {
        (route::Method::Get, route::STATUS) => status(shared),
        (route::Method::Get, route::TELEMETRY) => telemetry(shared),
        (route::Method::Get, route::PANIC) => panic_breadcrumb(shared),
        (route::Method::Get, route::NETWORKS) => networks(shared, state),
        (route::Method::Get, route::WIFI) => get_wifi(shared),
        (route::Method::Post, route::WIFI) => post_wifi(shared, head, body),
        (route::Method::Post, route::SETTINGS) => post_settings(shared, head, body),
        (route::Method::Post, route::FIRMWARE) => post_firmware(shared, head, body),
        (route::Method::Post, route::REBOOT) => post_reboot(shared, head, body),
        (route::Method::Post, route::IDENTIFY) => post_identify(shared, head, body),
        // Unreachable: the table walk above proved the pair is in `ROUTES`.
        _ => bare(ErrorCode::NotFound),
    }
}

// ---------------------------------------------------------------------------
// The reads
// ---------------------------------------------------------------------------

fn status(shared: &Shared) -> Response {
    let now = shared.now_us();
    // Card 246: while a modelled update is on trial, the three fields that
    // describe the running image are the *new* image's. Read before the core
    // lock is taken, because it has a lock of its own.
    let running = shared.ota().running_image();
    let core = shared.core().lock().unwrap();
    let t = core.telemetry(now);
    let ident = core.ident();
    let wifi = core.wifi();
    let reply = StatusReply {
        api: route::API_VERSION,
        id: screeny_device_api::text::text(&ident.id).unwrap_or_default(),
        name: screeny_device_api::text::text(core.name()).unwrap_or_default(),
        fw: screeny_device_api::text::text(
            running.as_ref().map_or(ident.fw.as_str(), |r| r.version.as_str()),
        )
        .unwrap_or_default(),
        boot_id: running.as_ref().map_or(ident.boot_id, |r| r.boot_id),
        uptime_ms: t.uptime_ms,
        heap_used: ident.heap_used,
        heap_size: ident.heap_size,
        stack_free: ident.stack_free,
        rssi_dbm: t.rssi_dbm,
        brightness: core.brightness(),
        idle_mode: core.idle_mode().into(),
        // The **link**, not the sticky result of the last credentials attempt:
        // `docs/design/device-web.md`'s card 223 paragraph says that result
        // belongs to `GET /api/v1/wifi` alone, and a device that fell back
        // successfully must not read `failed` here while it is plainly
        // connected. Card 228's rule 8 caught the simulator doing exactly
        // that; `wifi.link_state()` is now the one derivation this route and
        // `GET /api/v1/wifi` share. UDP `GET_WIFI` still answers
        // `wifi.wifi_state()`, which is spec 8.3 and is unchanged.
        wifi_state: wifi.link_state(),
        ssid: screeny_device_api::text::text(wifi.ssid()).filter(|s: &_| !s.is_empty()),
        ip: wifi.ip().map(screeny_device_api::text::ipv4_text),
        state: StreamState::from_u8(core.state_byte()).unwrap_or(StreamState::Idle),
        portal: wifi.ap_up(),
        // No flash here, and no reset to have a reason: these four are made
        // up, and since card 192 they are made up *on purpose* - the flags
        // and `SimHandle::set_health` choose them, so the unhappy rows of a
        // status page can be seen. Reported and nothing more: no behaviour
        // anywhere in the simulator reads them back. See `crate::Health`.
        fw_slot: running.as_ref().map_or(ident.fw_slot, |r| r.slot),
        fw_state: running.as_ref().map_or(ident.fw_state, |r| r.state),
        reset_reason: ident.reset_reason,
        store_errors: ident.store_errors,
    };
    ok_json(&reply)
}

/// `GET /api/v1/panic` (card 243).
///
/// The simulator has no RTC memory and no panic path - a panic here is a
/// process that stops, and the operating system is the one that says so - so
/// the honest answer is "this is my first boot and nothing has crashed".
/// Nothing in the simulator reads it back; the route exists so that a client
/// written against the sim meets the same shape the device sends.
fn panic_breadcrumb(shared: &Shared) -> Response {
    ok_json(&screeny_device_api::reply::PanicReply {
        boot_count: 1,
        panic_count: 0,
        last_panic: None,
        // Card 241, and `null` unless card 246's model is playing one out:
        // this simulator has no `otadata` and no slots, so it has never
        // activated a firmware image and saying otherwise would be inventing
        // one. `SimHandle::model_ota` is a test asking for exactly that
        // invention, with a clock attached.
        update: shared.ota().update_record(),
        // Card 241b: a simulator is a process. If it stops, the operating
        // system is what says so, and there is no reset register to read.
        last_reset: None,
    })
}

fn telemetry(shared: &Shared) -> Response {
    let now = shared.now_us();
    let core = shared.core().lock().unwrap();
    ok_json(&TelemetryReply::from(&core.telemetry(now)))
}

fn networks(shared: &Shared, state: &ApiState) -> Response {
    if !state.allow_scan() {
        return error_response(
            ErrorCode::RateLimited,
            &format!(
                "one scan per {} s; try again in {} ms",
                route::SCAN_MIN_INTERVAL_MS / 1_000,
                state.scan_retry_after_ms()
            ),
        );
    }
    let mut reply = NetworksReply::new();
    {
        let core = shared.core().lock().unwrap();
        let wifi = core.wifi();
        // A scan a simulator can honestly produce: its own soft-AP, the
        // network it claims to be on, and two fixtures. The list is built with
        // `NetworksReply::offer`, so the cap, the ordering and the
        // "not UTF-8 is left out" rule are the firmware's and not a second
        // copy of them.
        reply.offer(wifi.ap_ssid().as_bytes(), -30, false);
        if !wifi.ssid().is_empty() {
            reply.offer(wifi.ssid().as_bytes(), -52, true);
        }
        reply.offer(b"Example-Wifi1", -67, true);
        reply.offer(b"Example-Wifi2", -81, false);
        // A neighbour whose SSID is not UTF-8: `offer` drops it, and this is
        // how the simulator proves it rather than claiming it.
        reply.offer(&[0xff, 0xfe, 0x00, 0x41], -74, true);
    }
    ok_json(&reply)
}

fn get_wifi(shared: &Shared) -> Response {
    let core = shared.core().lock().unwrap();
    ok_json(&core.wifi().wifi_reply())
}

// ---------------------------------------------------------------------------
// The mutations
// ---------------------------------------------------------------------------

fn buffered(body: Body<'_>) -> Result<Vec<u8>, Response> {
    match body {
        Body::Buffered(b) => Ok(b),
        // The transport only streams the firmware route.
        Body::Stream { .. } => Err(bare(ErrorCode::Internal)),
    }
}

/// Refuse a body longer than the route's own bound, before parsing it. This
/// is the number card 222 sizes picoserve's buffer from, so the simulator
/// enforces it rather than accepting what a host happens to be able to hold.
fn within_bound(path: &str, method: route::Method, len: usize) -> Result<(), Response> {
    let bound = route::find(path, method).map_or(0, |r| r.max_request_len);
    if bound > 0 && len > bound {
        return Err(error_response(
            ErrorCode::PayloadTooLarge,
            "body is longer than this route accepts",
        ));
    }
    Ok(())
}

fn post_wifi(shared: &Shared, _head: &Head, body: Body<'_>) -> Response {
    let body = match buffered(body) {
        Ok(b) => b,
        Err(r) => return r,
    };
    if let Err(r) = within_bound(route::WIFI, route::Method::Post, body.len()) {
        return r;
    }
    let form = match parse_wifi_form(&body) {
        Ok(f) => f,
        Err(e) => {
            let reply = e.reply();
            return Response::json(
                reply.status(),
                serde_json::to_vec(&reply).unwrap_or_default(),
            );
        }
    };
    if let Err(code) = screeny_device_api::request::check_auth(form.auth()) {
        return bare(code);
    }
    // An SSID is bytes; the machine takes text. One that is not UTF-8 cannot
    // be carried to it, and saying so is better than joining something else.
    let Ok(ssid) = std::str::from_utf8(form.ssid()) else {
        return error_response(ErrorCode::OutOfRange, "ssid is not UTF-8");
    };
    let ssid = ssid.to_string();
    // ...and here the form, and with it the only copy of the PSK the
    // simulator ever held, is dropped. Nothing below this line can leak it
    // because nothing below this line can see it.
    drop(form);

    let now = shared.now_us();
    let mut events = Vec::new();
    let posted = {
        let mut core = shared.core().lock().unwrap();
        core.wifi_mut().post_credentials(&ssid, now, &mut events)
    };
    shared.publish(&events);
    match posted {
        // Spec section 8.2: the reply goes out before the radio work, which
        // here means before any tick can deliver the scripted outcome. Since
        // card 232 this is the answer from `Portal`, `Trial`, `Joining` **and**
        // `Online` - the 503 card 224 raised for a post while online is gone,
        // because the machine now has the transition it was flagging.
        Posted::Trial => ok_json(&AcceptedReply::TRYING),
        // Only `Boot` is left, which the simulator never serves from: the
        // machine is booted inside `WifiModel::new`. Kept because a reply that
        // says `trying` when nothing is being tried would be a lie, and card
        // 224's "do not answer `trying` when the machine did not start a
        // trial" is the rule whatever the remaining state turns out to be.
        Posted::Ignored(phase) => error_response(
            ErrorCode::Unavailable,
            &format!("no trial join from {}", phase.name()),
        ),
    }
}

fn post_settings(shared: &Shared, head: &Head, body: Body<'_>) -> Response {
    let body = match buffered(body) {
        Ok(b) => b,
        Err(r) => return r,
    };
    if let Err(r) = within_bound(route::SETTINGS, route::Method::Post, body.len()) {
        return r;
    }
    let req: SettingsRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(_) => return error_response(ErrorCode::BadJson, "not a settings request"),
    };
    if let Err(code) = req.check_auth() {
        return bare(code);
    }

    let now = shared.now_us();
    let mut events = Vec::new();
    let result = {
        let mut core = shared.core().lock().unwrap();
        let mut apply = |r: Request<'_>| control(&mut core, head.peer, now, r, &mut events);
        let mut err = None;
        if let Some(b) = req.brightness {
            err = err.or(apply(Request::SetBrightness(b)).err());
        }
        if let Some(m) = req.idle_mode {
            err = err.or(apply(Request::SetIdle(m.into())).err());
        }
        if let Some(name) = &req.name {
            err = err.or(apply(Request::SetName(name.as_str())).err());
        }
        err.map_or_else(
            || {
                Ok(SettingsReply {
                    name: screeny_device_api::text::text(core.name()).unwrap_or_default(),
                    brightness: core.brightness(),
                    idle_mode: core.idle_mode().into(),
                })
            },
            Err,
        )
    };
    shared.publish(&events);
    match result {
        Ok(reply) => ok_json(&reply),
        Err(code) => bare(code),
    }
}

fn post_reboot(shared: &Shared, head: &Head, body: Body<'_>) -> Response {
    let body = match buffered(body) {
        Ok(b) => b,
        Err(r) => return r,
    };
    if let Err(r) = within_bound(route::REBOOT, route::Method::Post, body.len()) {
        return r;
    }
    let req: RebootRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(_) => return error_response(ErrorCode::BadJson, "not a reboot request"),
    };
    if let Err(code) = req.check_auth() {
        return bare(code);
    }
    if !req.confirmed() {
        return error_response(ErrorCode::OutOfRange, "confirm must be \"RBOO\"");
    }
    let now = shared.now_us();
    let mut events = Vec::new();
    let result = {
        let mut core = shared.core().lock().unwrap();
        control(&mut core, head.peer, now, Request::Reboot, &mut events)
    };
    shared.publish(&events);
    match result {
        // Exactly what UDP `REBOOT` does, which is accept it, log it and not
        // act on it: the simulator has never restarted itself, and card 224
        // may not change what it does on UDP. See the card's log.
        Ok(()) => ok_json(&AcceptedReply::REBOOTING),
        Err(code) => bare(code),
    }
}

fn post_identify(shared: &Shared, head: &Head, body: Body<'_>) -> Response {
    let body = match buffered(body) {
        Ok(b) => b,
        Err(r) => return r,
    };
    if let Err(r) = within_bound(route::IDENTIFY, route::Method::Post, body.len()) {
        return r;
    }
    let req: IdentifyRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(_) => return error_response(ErrorCode::BadJson, "not an identify request"),
    };
    if let Err(code) = req.check_auth() {
        return bare(code);
    }
    let duration_ms = match req.duration_u16() {
        Ok(d) => d,
        Err(code) => return error_response(code, "duration_ms must fit in 16 bits"),
    };
    let now = shared.now_us();
    let mut events = Vec::new();
    let result = {
        let mut core = shared.core().lock().unwrap();
        control(
            &mut core,
            head.peer,
            now,
            Request::Identify { duration_ms },
            &mut events,
        )
    };
    shared.publish(&events);
    match result {
        Ok(()) => ok_json(&AcceptedReply::IDENTIFYING),
        Err(code) => bare(code),
    }
}

/// `POST /api/v1/firmware`: read the upload, run every check, discard it.
///
/// Card 240 replaced the simulator's two-check stand-in with the real thing.
/// [`screeny_fwimage::Scan`] is the firmware's own validator, fed the same
/// bytes in the same order, so the two cannot disagree about whether an image
/// is acceptable - which is what "develop against the simulator" is worth.
///
/// What the simulator still cannot do is *keep* it: there is no flash here, so
/// `ok: true` means "this image would have been staged", the slot is imaginary
/// and a restart brings back the same simulator. The README says so.
///
/// `written` is deliberately "bytes that reached the sink", the same as the
/// device's, so a truncated upload reports where it stopped.
///
/// **Activation** (card 241) is parsed here and goes no further. The query flag
/// is read with the shared [`route::parse_activate`], so a caller that spells
/// it wrongly is refused `out_of_range` by the simulator exactly as it is by
/// the device - which is the half of this route a simulator *can* be held to.
/// The other half it cannot: there is no `otadata` here, no second slot and no
/// bootloader to hand over to, so an accepted image always answers
/// `activating: false` and nothing restarts.
fn post_firmware(shared: &Shared, head: &Head, body: Body<'_>) -> Response {
    let activate = match route::parse_activate(Some(&head.query)) {
        Ok(a) => a,
        Err(e) => return error_response(ErrorCode::OutOfRange, &e.to_string()),
    };
    let Body::Stream { reader, len } = body else {
        return bare(ErrorCode::Internal);
    };
    // Before a byte is read, exactly as research 006 section 5 asks and as the
    // device does: an upload that cannot fit the slot is refused for free.
    if len.is_some_and(|n| n > SLOT_LEN) {
        return ok_json(&FirmwareReply::failed(0, FirmwareError::TooLarge));
    }
    let slot = u32::try_from(SLOT_LEN).unwrap_or(u32::MAX);
    let mut scan = screeny_fwimage::Scan::new(slot);
    // One sector at a time, because that is what the device stages in: a
    // scanner that only worked on the host's convenient buffer size would be a
    // scanner the device does not run.
    let mut buf = [0u8; screeny_fwimage::SECTOR];
    let mut written: u64 = 0;
    // The first refusal, and how far the upload had got when it happened -
    // which is what `written` means on this route.
    let mut failed: Option<(u32, FirmwareError)> = None;
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };
        if failed.is_some() {
            // **Keep reading after a refusal.** The bytes are thrown away, but
            // they have to leave the socket: a server that stops reading and
            // then closes leaves unread data in the kernel's receive buffer,
            // and both Darwin and Linux answer that close with a RST, which
            // takes the reply with it. picoserve does the same thing on the
            // device (`RequestBodyConnection::finalize`), so the simulator
            // doing it is the two agreeing rather than the simulator being
            // polite.
            continue;
        }
        // `written` counts bytes that reached the sink, so it is bumped only
        // once this piece has passed the checks the device runs before it
        // erases anything. An image refused on its header reports `written: 0`
        // here and on the device, which is the truth in both: nothing was
        // staged.
        let w = u32::try_from(written).unwrap_or(u32::MAX);
        if let Err(e) = scan.push(&buf[..n]) {
            failed = Some((w, e));
            continue;
        }
        written += n as u64;
        // The device asks this after each sector it has buffered and before it
        // erases anything; here it costs nothing, and keeping the order the
        // same is how the two stay one implementation.
        if let Err(e) = scan.check_front() {
            failed = Some((w, e));
        }
    }
    if let Some((w, e)) = failed {
        return ok_json(&FirmwareReply::failed(w, e));
    }
    let written32 = u32::try_from(written).unwrap_or(u32::MAX);
    match scan.finish() {
        // **`activating` is always false here, whatever the caller asked**, and
        // that is the honest answer rather than a missing feature: this process
        // has one "slot" and it is the running binary. A simulator that claimed
        // to be rebooting into an image it threw away would be the one thing a
        // simulator must never be - a different answer from the device's about
        // something a caller acts on. `FirmwareReply` carries the flag for
        // exactly this: the caller learns the image was accepted *and* that
        // nothing is restarting, from the reply it already parses.
        Ok(image) => {
            // Card 246: with the model on, an activating upload really does
            // start something - a clock, and a device that goes away and comes
            // back on trial. With it off (the default) nothing has changed:
            // `activating: false`, because this process has one slot and it is
            // the running binary.
            let running = shared.core().lock().unwrap().ident().fw_slot;
            let version = image.version.as_str().unwrap_or("");
            if activate && shared.ota().activate(version, running) {
                ok_json(&FirmwareReply::activating(written32))
            } else {
                ok_json(&FirmwareReply::ok(written32))
            }
        }
        Err(e) => ok_json(&FirmwareReply::failed(written32, e)),
    }
}

/// Run one control request through the *UDP* path and read its answer.
///
/// The reply datagram is decoded for its error code and dropped: this is how
/// the HTTP routes and the UDP ops stay one implementation. See the module
/// docs, rule 2.
fn control(
    core: &mut Core,
    peer: SocketAddr,
    now_us: u64,
    request: Request<'_>,
    events: &mut Vec<Event>,
) -> Result<(), ErrorCode> {
    let mut packet = [0u8; screeny_proto::MAX_UDP_PAYLOAD];
    let Ok(n) = request.write(1, &mut packet) else {
        return Err(ErrorCode::Internal);
    };
    let mut out = crate::core::Outbox::default();
    core.control(now_us, peer, &packet[..n], &mut out);
    events.append(&mut out.events);
    // The reply never goes to the socket: `out.from_control_sock` is dropped
    // with `out`. What it says is what this route answers with.
    let code = out.from_control_sock.first().and_then(|(_, datagram)| {
        let body = datagram.get(8..)?;
        let op = *datagram.get(4)?;
        let flags = *datagram.get(5)?;
        match Reply::decode(op, flags, body) {
            Ok(Reply::Err { code }) => Some(code),
            _ => None,
        }
    });
    match code {
        None => Ok(()),
        Some(c) => Err(proto::ErrorCode::from_u8(c).map_or(ErrorCode::Internal, ErrorCode::from)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_host_rule_is_a_rule_and_not_a_list_of_probe_domains() {
        let me = "screeny-sim";
        // Ours: the portal address, any bare IP literal, loopback, our names.
        for h in [
            "192.168.4.1",
            "192.168.4.1:80",
            "192.168.7.221",
            "127.0.0.1:8080",
            "localhost",
            "localhost:8080",
            "screeny-sim.local",
            "screeny-sim.local.",
            "SCREENY-SIM.LOCAL",
            "screeny-sim",
            "[::1]:8080",
        ] {
            assert!(host_is_ours(Some(h), me), "{h} should be ours");
        }
        // Not ours: every captive-portal probe there is, without one of them
        // being named anywhere in the code.
        for h in [
            "captive.apple.com",
            "connectivitycheck.gstatic.com",
            "www.msftconnecttest.com",
            "nmcheck.gnome.org",
            "firefox-portal-detection.com",
            "example.com:8080",
        ] {
            assert!(!host_is_ours(Some(h), me), "{h} should not be ours");
        }
        // No Host at all is not a probe.
        assert!(host_is_ours(None, me));
    }

    #[test]
    fn the_index_links_every_route() {
        let html = index_html();
        for r in route::ROUTES {
            assert!(html.contains(r.path), "{} is not on the index", r.path);
        }
    }

    #[test]
    fn a_body_over_the_routes_bound_is_refused_before_it_is_parsed() {
        assert!(within_bound(route::WIFI, route::Method::Post, 384).is_ok());
        let r = within_bound(route::WIFI, route::Method::Post, 385).unwrap_err();
        assert_eq!(r.status, 413);
        // A GET has no bound and is never refused for its length.
        assert!(within_bound(route::STATUS, route::Method::Get, 99_999).is_ok());
    }
}
