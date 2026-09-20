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
//! `store_errors` are constants from [`crate::core::Ident`]: there is no flash
//! here and no stack worth measuring. `POST /api/v1/firmware` accepts the
//! stream, discards it, and runs the two of research 006's five checks that
//! need no image parser - the `0xE9` magic and the slot's length - and says so
//! in the README rather than pretending the other three passed.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use screeny_device_api::error::{ErrorCode, ErrorReply};
use screeny_device_api::form::parse_wifi_form;
use screeny_device_api::reply::{
    AcceptedReply, FirmwareReply, NetworksReply, SettingsReply, StatusReply, TelemetryReply,
};
use screeny_device_api::request::{IdentifyRequest, Mutating, RebootRequest, SettingsRequest};
use screeny_device_api::{route, FirmwareError, FwSlot, FwState, ResetReason, StreamState};
use screeny_proto::control::{self as proto, Reply, Request};

use crate::core::Core;
use crate::device::Shared;
use crate::event::Event;
use crate::http::{Body, Handler, Head, Response, WantsStream};
use crate::wifi::Posted;

/// How often `GET /api/v1/networks` will really scan. The route's docs say
/// "one per 10 s" in prose; `crates/device-api` exposes no constant for it, so
/// this is the simulator's copy of that number. Card 224's log proposes the
/// shared constant.
pub const SCAN_MIN_INTERVAL_MS: u64 = 10_000;

/// The inactive app slot's size: research 006's `ota_0` at 0x10000 and `ota_1`
/// at 0x210000, so 2 MiB each. An upload longer than this is
/// [`FirmwareError::TooLarge`] on the device and here.
pub const SLOT_LEN: u64 = 0x20_0000;

/// The captive-portal answer's address, from `screeny_provision`.
const PORTAL_IP: &str = screeny_provision::PORTAL_IP;

/// The body of the captive-portal redirect.
///
/// **Not decoration.** ESP-IDF's own example says iOS needs content in the
/// response to detect a portal, and Android classifies a `Content-Length <= 4`
/// answer as *failed* rather than as a portal (research 007 section 4.2).
const REDIRECT_BODY: &str = concat!(
    "<html><head><title>screeny setup</title></head><body>",
    "<a href=\"http://192.168.4.1/\">Open the screeny setup page</a>",
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
#[derive(Debug)]
pub struct ApiState {
    boot: Instant,
    last_scan_ms: AtomicU64,
}

impl ApiState {
    /// A fresh one. `last_scan_ms` starts far enough back that the first
    /// request always scans.
    #[must_use]
    pub fn new() -> Self {
        ApiState {
            boot: Instant::now(),
            last_scan_ms: AtomicU64::new(0),
        }
    }

    /// True if a scan is allowed now, and records it if so.
    fn allow_scan(&self) -> bool {
        let now = self.boot.elapsed().as_millis() as u64;
        let last = self.last_scan_ms.load(Ordering::SeqCst);
        if last != 0 && now.saturating_sub(last) < SCAN_MIN_INTERVAL_MS {
            return false;
        }
        self.last_scan_ms.store(now.max(1), Ordering::SeqCst);
        true
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
    // Microsoft's portal guidance is explicit that a portal must not redirect
    // some requests and drop others.
    if !host_is_ours(head.host.as_deref(), shared.instance()) {
        let in_portal = {
            let core = shared.core().lock().unwrap();
            matches!(
                core.wifi().phase(),
                screeny_provision::State::Portal | screeny_provision::State::Trial
            )
        };
        return if in_portal {
            Response::redirect(&format!("http://{PORTAL_IP}/"), REDIRECT_BODY)
        } else {
            // Not provisioning: there is no portal to send anybody to, so the
            // honest answer is that the simulator does not serve that host.
            bare(ErrorCode::NotFound)
        };
    }

    if head.path == "/" || head.path == "/index.html" {
        return match head.method.as_str() {
            "GET" => Response::html(200, &index_html()),
            _ => bare(ErrorCode::MethodNotAllowed),
        };
    }

    let method = match head.method.as_str() {
        "GET" => route::Method::Get,
        "POST" => route::Method::Post,
        // Any other verb: the route may exist, but not like this.
        _ => {
            return if route::ROUTES.iter().any(|r| r.path == head.path) {
                bare(ErrorCode::MethodNotAllowed)
            } else {
                bare(ErrorCode::NotFound)
            }
        }
    };

    let known_path = route::ROUTES.iter().any(|r| r.path == head.path);
    if !known_path {
        return bare(ErrorCode::NotFound);
    }
    if !route::ROUTES
        .iter()
        .any(|r| r.path == head.path && r.method == method)
    {
        return bare(ErrorCode::MethodNotAllowed);
    }

    match (method, head.path.as_str()) {
        (route::Method::Get, route::STATUS) => status(shared),
        (route::Method::Get, route::TELEMETRY) => telemetry(shared),
        (route::Method::Get, route::NETWORKS) => networks(shared, state),
        (route::Method::Get, route::WIFI) => get_wifi(shared),
        (route::Method::Post, route::WIFI) => post_wifi(shared, head, body),
        (route::Method::Post, route::SETTINGS) => post_settings(shared, head, body),
        (route::Method::Post, route::FIRMWARE) => post_firmware(body),
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
    let core = shared.core().lock().unwrap();
    let t = core.telemetry(now);
    let ident = core.ident();
    let wifi = core.wifi();
    let reply = StatusReply {
        api: route::API_VERSION,
        id: screeny_device_api::text::text(&ident.id).unwrap_or_default(),
        name: screeny_device_api::text::text(core.name()).unwrap_or_default(),
        fw: screeny_device_api::text::text(&ident.fw).unwrap_or_default(),
        boot_id: ident.boot_id,
        uptime_ms: t.uptime_ms,
        heap_used: ident.heap_used,
        heap_size: ident.heap_size,
        stack_free: ident.stack_free,
        rssi_dbm: t.rssi_dbm,
        brightness: core.brightness(),
        idle_mode: core.idle_mode().into(),
        wifi_state: screeny_device_api::WifiState::from_u8(wifi.wifi_state())
            .unwrap_or(screeny_device_api::WifiState::Disconnected),
        ssid: screeny_device_api::text::text(wifi.ssid()).filter(|s: &_| !s.is_empty()),
        ip: wifi.ip().map(screeny_device_api::text::ipv4_text),
        state: StreamState::from_u8(core.state_byte()).unwrap_or(StreamState::Idle),
        portal: wifi.ap_up(),
        // No flash here, and no reset to have a reason. Constants, named as
        // such in `Ident`'s docs.
        fw_slot: FwSlot::Ota0,
        fw_state: FwState::Valid,
        reset_reason: ResetReason::PowerOn,
        store_errors: ident.store_errors,
    };
    ok_json(&reply)
}

fn telemetry(shared: &Shared) -> Response {
    let now = shared.now_us();
    let core = shared.core().lock().unwrap();
    ok_json(&TelemetryReply::from(&core.telemetry(now)))
}

fn networks(shared: &Shared, state: &ApiState) -> Response {
    if !state.allow_scan() {
        return error_response(ErrorCode::RateLimited, "one scan per 10 seconds");
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
    let bound = route::ROUTES
        .iter()
        .find(|r| r.path == path && r.method == method)
        .map_or(0, |r| r.max_request_len);
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
        // here means before any tick can deliver the scripted outcome.
        Posted::Trial => ok_json(&AcceptedReply::TRYING),
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

fn post_firmware(body: Body<'_>) -> Response {
    let Body::Stream { reader, len } = body else {
        return bare(ErrorCode::Internal);
    };
    if len.is_some_and(|n| n > SLOT_LEN) {
        return ok_json(&FirmwareReply::failed(0, FirmwareError::TooLarge));
    }
    // Read and discard. Nothing is installed, and nothing pretends to be:
    // the simulator has no flash and says so in its README.
    let mut buf = [0u8; 8 * 1024];
    let mut written: u64 = 0;
    let mut first: Option<u8> = None;
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if first.is_none() {
                    first = Some(buf[0]);
                }
                written += n as u64;
                if written > SLOT_LEN {
                    return ok_json(&FirmwareReply::failed(
                        u32::try_from(written).unwrap_or(u32::MAX),
                        FirmwareError::TooLarge,
                    ));
                }
            }
            Err(_) => break,
        }
    }
    let written32 = u32::try_from(written).unwrap_or(u32::MAX);
    // Research 006's first check, and the only one that needs no image
    // parser: an ESP32 image begins `0xE9`.
    match first {
        Some(0xE9) => ok_json(&FirmwareReply::ok(written32)),
        _ => ok_json(&FirmwareReply::failed(written32, FirmwareError::BadMagic)),
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
