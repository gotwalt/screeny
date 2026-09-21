//! The rules, in the order they run.
//!
//! Numbering is stable: `--only 17` is rule 17 today and tomorrow. The order
//! is chosen so that everything cheap and safe is measured before anything
//! that disturbs the device - the credentials trial (32, 33) and the reboot
//! (35) come last but for the three rules that are about the whole run, which
//! read back every reply the suite saw and so have to follow all of them.

use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use screeny_device_api::reply::{
    AcceptedReply, FirmwareReply, NetworksReply, PanicReply, SettingsReply, StatusReply, TelemetryReply,
    WifiReply, MAX_NETWORKS,
};
use screeny_device_api::request::MAX_IDENTIFY_MS;
use screeny_device_api::{
    route, Accepted, ErrorCode, ErrorReply, FirmwareError, IdleMode, WifiState,
};

// The name the settings rules set, and take back off. **One definition**
// (card 246, item 4): the next run recognises it by `PROBE_NAME_PREFIX`, and a
// second spelling here is how that would quietly stop working.
use super::PROBE_NAME;
use super::{
    idle_name, verdict, Ctx, Outcome, Res, Rule, ALLOW_REBOOT, ALLOW_WIFI_TRIAL, CAP_PROBE,
    KNOWN_223, NEEDS_UDP,
};

/// The SSID and PSK every test on this bench uses. Never a real pair: CLAUDE.md
/// forbids one in a tracked file, and this one is in every fixture already.
const DUMMY_SSID: &str = "Example-Wifi1";
/// The dummy PSK. Also what rule 38 greps every reply for.
const DUMMY_PSK: &str = "password9";

/// How long to wait for a device that has been taken off its network or
/// restarted. A trial is three attempts of 15 s plus the fallback join; a
/// reboot is ~25 s on the bench.
const PATIENCE: Duration = Duration::from_secs(150);

/// Every rule, in the order they run.
pub fn all() -> Vec<Rule> {
    vec![
        // --- GET /api/v1/status ------------------------------------------
        Rule {
            n: 1,
            section: "status",
            route: route::STATUS,
            name: "GET /api/v1/status answers 200 JSON and parses as StatusReply",
            cite: "device-web.md, the HTTP API; reply::StatusReply",
            secs: 0.5,
            flags: 0,
            run: status_parses,
        },
        Rule {
            n: 2,
            section: "status",
            route: route::STATUS,
            name: "status.api is the version in the paths",
            cite: "route::API_VERSION",
            secs: 0.3,
            flags: 0,
            run: status_api,
        },
        Rule {
            n: 3,
            section: "status",
            route: route::STATUS,
            name: "boot_id is the same in two reads a moment apart",
            cite: "device-web.md: boot_id tells a reboot from a flap",
            secs: 0.6,
            flags: 0,
            run: status_boot_id,
        },
        Rule {
            n: 4,
            section: "status",
            route: route::STATUS,
            name: "uptime_ms moves forward between two reads",
            cite: "reply::StatusReply::uptime_ms",
            secs: 1.2,
            flags: 0,
            run: status_uptime,
        },
        Rule {
            n: 5,
            section: "status",
            route: route::STATUS,
            name: "heap_used <= heap_size, and neither is zero",
            cite: "device-web.md, the RAM table",
            secs: 0.3,
            flags: 0,
            run: status_heap,
        },
        Rule {
            n: 6,
            section: "status",
            route: route::STATUS,
            name: "ip parses as an IPv4 address when it is not null",
            cite: "text::ipv4_text",
            secs: 0.3,
            flags: 0,
            run: status_ip,
        },
        Rule {
            n: 7,
            section: "status",
            route: route::STATUS,
            name: "fw_slot, fw_state and reset_reason are values this API defines",
            cite: "enums::{FwSlot, FwState, ResetReason}",
            secs: 0.3,
            flags: 0,
            run: status_fw_fields,
        },
        Rule {
            n: 8,
            section: "status",
            route: route::STATUS,
            name: "wifi_state describes the link, never the sticky trial result",
            cite: "device-web.md, the card 223 paragraph",
            secs: 0.3,
            flags: KNOWN_223,
            run: status_wifi_state_is_the_link,
        },
        // --- GET /api/v1/telemetry ---------------------------------------
        Rule {
            n: 9,
            section: "telemetry",
            route: route::TELEMETRY,
            name: "GET /api/v1/telemetry answers 200 and parses as TelemetryReply",
            cite: "reply::TelemetryReply; spec 6.7",
            secs: 0.4,
            flags: 0,
            run: telemetry_parses,
        },
        Rule {
            n: 10,
            section: "telemetry",
            route: route::TELEMETRY,
            name: "a browser and a UDP sender see the same numbers",
            cite: "research 007 section 7; reply::TelemetryReply",
            secs: 0.8,
            flags: NEEDS_UDP,
            run: telemetry_agrees_with_udp,
        },
        // --- GET /api/v1/networks ----------------------------------------
        Rule {
            n: 11,
            section: "networks",
            route: route::NETWORKS,
            name: "networks is a capped, strongest-first list, or unavailable",
            cite: "reply::NetworksReply; MAX_NETWORKS",
            secs: 1.5,
            flags: 0,
            run: networks_list,
        },
        Rule {
            n: 12,
            section: "networks",
            route: route::NETWORKS,
            name: "a second scan inside the window is rate_limited",
            cite: "route::SCAN_MIN_INTERVAL_MS, route::RateLimit",
            secs: 1.5,
            flags: 0,
            run: networks_rate_limited,
        },
        // --- GET /api/v1/wifi, and the form that cannot join --------------
        Rule {
            n: 13,
            section: "wifi",
            route: route::WIFI,
            name: "GET /api/v1/wifi answers 200 and parses as WifiReply",
            cite: "reply::WifiReply",
            secs: 0.4,
            flags: 0,
            run: wifi_parses,
        },
        Rule {
            n: 14,
            section: "wifi",
            route: route::WIFI,
            name: "reason is set exactly when state is failed",
            cite: "reply::WifiReply::reason; device-web.md card 223",
            secs: 0.4,
            flags: KNOWN_223,
            run: wifi_reason,
        },
        Rule {
            n: 15,
            section: "wifi",
            route: route::WIFI,
            name: "a form with no ssid is bad_form, and the reply has no PSK in it",
            cite: "form::parse_wifi_form; spec 8.4",
            secs: 0.4,
            flags: 0,
            run: wifi_form_without_ssid,
        },
        // --- POST /api/v1/settings ---------------------------------------
        Rule {
            n: 16,
            section: "settings",
            route: route::SETTINGS,
            name: "an empty settings request is a no-op that answers the whole state",
            cite: "request::SettingsRequest::is_empty; reply::SettingsReply",
            secs: 0.4,
            flags: 0,
            run: settings_empty,
        },
        Rule {
            n: 17,
            section: "settings",
            route: route::SETTINGS,
            name: "name and idle_mode round-trip, status agrees, both are put back",
            cite: "reply::SettingsReply; spec 7.5",
            secs: 1.5,
            flags: 0,
            run: settings_round_trip,
        },
        Rule {
            n: 18,
            section: "settings",
            route: route::SETTINGS,
            name: "brightness is echoed as applied, and the device really moved",
            cite: "reply::SettingsReply::brightness; spec 7.2",
            secs: 1.2,
            flags: 0,
            run: settings_brightness,
        },
        Rule {
            n: 19,
            section: "settings",
            route: route::SETTINGS,
            name: "a brightness above the firmware cap comes back as the cap",
            cite: "reply::SettingsReply; spec 7.2",
            secs: 1.2,
            flags: CAP_PROBE,
            run: settings_brightness_cap,
        },
        Rule {
            n: 20,
            section: "settings",
            route: route::SETTINGS,
            name: "a body that is not JSON is bad_json",
            cite: "error::ErrorCode::BadJson",
            secs: 0.4,
            flags: 0,
            run: settings_bad_json,
        },
        // --- POST /api/v1/identify ---------------------------------------
        Rule {
            n: 21,
            section: "identify",
            route: route::IDENTIFY,
            name: "identify answers identifying, raises the overlay, and 0 stops it",
            cite: "reply::AcceptedReply::IDENTIFYING; spec 7.3",
            secs: 2.0,
            flags: 0,
            run: identify_overlay,
        },
        Rule {
            n: 22,
            section: "identify",
            route: route::IDENTIFY,
            name: "a duration the wire cannot carry is out_of_range",
            cite: "request::MAX_IDENTIFY_MS; spec 6.6",
            secs: 0.4,
            flags: 0,
            run: identify_out_of_range,
        },
        // --- POST /api/v1/firmware ---------------------------------------
        Rule {
            n: 23,
            section: "firmware",
            route: route::FIRMWARE,
            name: "an empty upload is refused and never answers ok",
            cite: "reply::FirmwareReply, 'What ok: true promises'",
            secs: 0.6,
            flags: 0,
            run: firmware_empty,
        },
        Rule {
            n: 24,
            section: "firmware",
            route: route::FIRMWARE,
            name: "a body that is not an ESP32 image is refused and never answers ok",
            cite: "research 006's five checks; enums::FirmwareError",
            secs: 0.6,
            flags: 0,
            run: firmware_not_an_image,
        },
        // --- the refusals -------------------------------------------------
        Rule {
            n: 25,
            section: "errors",
            route: route::SETTINGS,
            name: "a known path with a method it does not have is method_not_allowed",
            cite: "route::find + route::path_is_known",
            secs: 0.4,
            flags: 0,
            run: wrong_method,
        },
        Rule {
            n: 26,
            section: "errors",
            route: route::STATUS,
            name: "a verb this API has no method for is method_not_allowed",
            cite: "route::path_is_known's own doc example",
            secs: 0.4,
            flags: 0,
            run: unknown_verb,
        },
        Rule {
            n: 27,
            section: "errors",
            route: "/api/v1/nope",
            name: "an unknown path is not_found, in the error shape",
            cite: "error::ErrorCode::NotFound",
            secs: 0.6,
            flags: 0,
            run: unknown_path,
        },
        Rule {
            n: 28,
            section: "errors",
            route: route::WIFI,
            name: "a body over MAX_REQUEST_LEN is payload_too_large",
            cite: "route::MAX_REQUEST_LEN; form::MAX_FORM_LEN",
            secs: 0.8,
            flags: 0,
            run: oversize_body,
        },
        Rule {
            n: 29,
            section: "errors",
            route: route::SETTINGS,
            name: "each route's own max_request_len is enforced on that route",
            cite: "route::Route::max_request_len; device-web.md card 223",
            secs: 1.2,
            flags: KNOWN_223,
            run: oversize_body_per_route,
        },
        // --- GET / ---------------------------------------------------------
        Rule {
            n: 30,
            section: "index",
            route: "/",
            name: "GET / is HTML, not empty, and carries no external URL",
            cite: "card 222 deliverable 3: one self-contained page",
            secs: 0.8,
            flags: 0,
            run: index_page,
        },
        // --- timing ---------------------------------------------------------
        Rule {
            n: 31,
            section: "timing",
            route: route::STATUS,
            name: "a lone request comes back well inside the client's patience",
            cite: "card 222: 25-37 ms a lone request, 1 s if a worker is busy",
            secs: 3.0,
            flags: 0,
            run: timing,
        },
        // --- the two that disturb the device ---------------------------------
        Rule {
            n: 32,
            section: "wifi-trial",
            route: route::WIFI,
            name: "posted credentials are answered trying, before the radio work",
            cite: "spec 8.2; reply::AcceptedReply::TRYING",
            secs: 1.0,
            flags: ALLOW_WIFI_TRIAL,
            run: wifi_post_credentials,
        },
        Rule {
            n: 33,
            section: "wifi-trial",
            route: route::WIFI,
            name: "the device comes back and says the attempt failed, and why",
            cite: "device-web.md card 232: the sticky failed result",
            secs: 60.0,
            flags: ALLOW_WIFI_TRIAL | KNOWN_223,
            run: wifi_after_the_trial,
        },
        Rule {
            n: 34,
            section: "reboot",
            route: route::REBOOT,
            name: "a reboot without the magic word is out_of_range and nothing restarts",
            cite: "request::REBOOT_CONFIRM; spec 6.9",
            secs: 1.0,
            flags: 0,
            run: reboot_unconfirmed,
        },
        Rule {
            n: 35,
            section: "reboot",
            route: route::REBOOT,
            name: "a confirmed reboot answers rebooting and the device comes back new",
            cite: "reply::AcceptedReply::REBOOTING",
            secs: 30.0,
            flags: ALLOW_REBOOT,
            run: reboot_confirmed,
        },
        // --- the whole run ---------------------------------------------------
        Rule {
            n: 36,
            section: "all",
            route: "-",
            name: "every reply the suite saw is inside its own MAX_JSON_LEN",
            cite: "crates/device-api: 'MAX_JSON_LEN: what it is for'",
            secs: 0.1,
            flags: 0,
            run: every_reply_within_its_bound,
        },
        Rule {
            n: 37,
            section: "all",
            route: "-",
            name: "every failure was the one error shape, with that code's status",
            cite: "error::ErrorReply; ErrorCode::status",
            secs: 0.1,
            flags: 0,
            run: every_failure_is_the_error_shape,
        },
        Rule {
            n: 38,
            section: "all",
            route: "-",
            name: "no reply the suite saw carries a PSK",
            cite: "spec 8.4; crates/device-api's no_psk test",
            secs: 0.1,
            flags: 0,
            run: no_reply_carries_a_psk,
        },
        // --- GET /api/v1/panic (card 243) --------------------------------
        //
        // Last, and numbered after the three cross-cutting rules, because the
        // numbers above are cited by name in `firmware/src/http.rs` and in
        // spec 8.7 ("rules 37, 27") and renumbering them to make room in the
        // middle would break every one of those references.
        Rule {
            n: 39,
            section: "panic",
            route: route::PANIC,
            name: "the panic breadcrumb parses, and says nothing when nothing crashed",
            cite: "reply::PanicReply; card 243",
            secs: 0.3,
            flags: 0,
            run: panic_breadcrumb,
        },
        // --- POST /api/v1/firmware, the rest of it (card 240) ------------
        //
        // Numbered after 39 for the same reason 39 comes after 38: the numbers
        // above are cited by name in `firmware/src/http.rs` and in spec 8.7,
        // and renumbering to make room in the middle would break every one of
        // those references. Rules 23 and 24 stay where they are.
        //
        // **Every one of these is refused before the device erases a sector.**
        // That is not luck, it is the design: research 006 section 5's first
        // four checks are answerable from the first 4,096 bytes and the
        // staging loop runs them before it touches flash, and the length check
        // runs on `Content-Length` before the body is read at all. So this
        // whole section can be run against the bench device, repeatedly, and
        // it leaves the *inactive* slot exactly as it found it - let alone the
        // running one. Nothing here can make a wrong image bootable, and
        // nothing here writes `otadata`, which this firmware never does.
        Rule {
            n: 40,
            section: "firmware",
            route: route::FIRMWARE,
            name: "an image for another chip is refused as wrong_chip",
            cite: "research 006 section 5 check 2; enums::FirmwareError",
            secs: 0.6,
            flags: 0,
            run: firmware_wrong_chip,
        },
        Rule {
            n: 41,
            section: "firmware",
            route: route::FIRMWARE,
            name: "somebody else's app is refused as wrong_project",
            cite: "research 006 section 5 check 4; fwimage::PROJECT_NAME",
            secs: 0.6,
            flags: 0,
            run: firmware_wrong_project,
        },
        Rule {
            n: 42,
            section: "firmware",
            route: route::FIRMWARE,
            name: "a body longer than the slot is refused on Content-Length alone",
            cite: "research 006 section 5, 'before a single sector is erased'",
            secs: 6.0,
            flags: 0,
            run: firmware_too_large,
        },
        Rule {
            n: 43,
            section: "firmware",
            route: route::FIRMWARE,
            name: "a truncated good-looking image is refused, and never ok",
            cite: "research 006 section 5's interruption table; check 6",
            secs: 1.0,
            flags: 0,
            run: firmware_truncated,
        },
        // --- card 241's query flag ---------------------------------------
        //
        // Both of these are **safe on a device somebody is using**: neither
        // sends an image that could be staged, let alone activated. What they
        // pin is the half of activation a probe can check without rebooting
        // anything - that the flag is parsed, and that a spelling the device
        // does not understand is refused rather than guessed at. The half that
        // does reboot is `screeny-probe fw-upload --activate`, which is a
        // command somebody types.
        Rule {
            n: 44,
            section: "firmware",
            route: route::FIRMWARE,
            name: "an activate flag this API does not define is out_of_range",
            cite: "route::parse_activate; spec 8.6",
            secs: 0.4,
            flags: 0,
            run: firmware_bad_activate,
        },
        Rule {
            n: 45,
            section: "firmware",
            route: route::FIRMWARE,
            name: "activate=0 is accepted and never activates anything",
            cite: "reply::FirmwareReply::activating; card 241",
            secs: 0.4,
            flags: 0,
            run: firmware_stage_only,
        },
    ]
}

/// Card 243: `GET /api/v1/panic` is the RTC breadcrumb, and it is
/// self-consistent.
///
/// The interesting assertion is the invariant rather than the values: a device
/// that reports no panic must report `panic_count` 0, and one that reports a
/// record must name a file. A panel that has panicked is not a failure of this
/// rule - the breadcrumb doing its job is the whole point of the card - so the
/// verdict says what it found and passes either way.
fn panic_breadcrumb(cx: &mut Ctx) -> Result<Outcome, String> {
    let res = cx.get(route::PANIC)?;
    if res.status != 200 {
        return verdict(false, format!("HTTP {}: {}", res.status, res.snippet(80)));
    }
    let p: PanicReply = res.parse()?;
    match &p.last_panic {
        None => verdict(
            p.panic_count == 0,
            format!(
                "no panic on record, {} panic(s) counted, boot {}",
                p.panic_count, p.boot_count
            ),
        ),
        Some(r) => verdict(
            p.panic_count > 0 && !r.file.is_empty() && r.boot <= p.boot_count,
            format!(
                "last panic {}:{} at {} ms, boot {} of {}, {} in a row",
                r.file, r.line, r.uptime_ms, r.boot, p.boot_count, r.consecutive
            ),
        ),
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Check a refusal: the status, the code, and that the body really is the
/// crate's error shape.
fn refusal(res: &super::Res, want: ErrorCode) -> Result<Outcome, String> {
    let reply = match res.error() {
        Ok(r) => r,
        Err(e) => return verdict(false, format!("HTTP {}: {e}", res.status)),
    };
    verdict(
        reply.error == want,
        format!(
            "HTTP {} {} {}",
            res.status,
            reply.error,
            reply.detail.as_deref().unwrap_or("(no detail)")
        ),
    )
}

/// True when a reply is the API's way of saying "this build cannot do that".
fn is_unavailable(res: &super::Res) -> bool {
    res.status == ErrorCode::Unavailable.status()
        && res
            .parse::<ErrorReply>()
            .is_ok_and(|r| r.error == ErrorCode::Unavailable)
}

/// The detail of an `unavailable`, for a SKIP line.
fn unavailable_detail(res: &super::Res) -> String {
    res.parse::<ErrorReply>()
        .ok()
        .and_then(|r| r.detail.map(|d| d.as_str().to_string()))
        .unwrap_or_else(|| "no detail".into())
}

// ---------------------------------------------------------------------------
// GET /api/v1/status
// ---------------------------------------------------------------------------

fn status_parses(cx: &mut Ctx) -> Result<Outcome, String> {
    let res = cx.get(route::STATUS)?;
    if res.status != 200 {
        return verdict(false, format!("HTTP {}: {}", res.status, res.snippet(80)));
    }
    if !res.is_json() {
        return verdict(false, format!("content-type {:?}", res.content_type()));
    }
    let s: StatusReply = res.parse()?;
    verdict(
        true,
        format!(
            "{} bytes, fw {} id {} state {:?}",
            res.body.len(),
            s.fw,
            s.id,
            s.state
        ),
    )
}

fn status_api(cx: &mut Ctx) -> Result<Outcome, String> {
    let s = cx.status()?;
    verdict(
        s.api == route::API_VERSION,
        format!("api {} (this suite speaks {})", s.api, route::API_VERSION),
    )
}

fn status_boot_id(cx: &mut Ctx) -> Result<Outcome, String> {
    let a = cx.status()?;
    let b = cx.status()?;
    verdict(
        a.boot_id == b.boot_id && a.boot_id != 0,
        format!("{} then {}", a.boot_id, b.boot_id),
    )
}

fn status_uptime(cx: &mut Ctx) -> Result<Outcome, String> {
    let a = cx.status()?;
    std::thread::sleep(Duration::from_millis(600));
    let b = cx.status()?;
    // Wrapping, because it is a u32 of milliseconds and wraps at 49.7 days.
    let moved = b.uptime_ms.wrapping_sub(a.uptime_ms);
    verdict(
        moved > 0 && moved < 60_000,
        format!("{} ms -> {} ms (+{moved} ms)", a.uptime_ms, b.uptime_ms),
    )
}

fn status_heap(cx: &mut Ctx) -> Result<Outcome, String> {
    let s = cx.status()?;
    verdict(
        s.heap_used <= s.heap_size && s.heap_size > 0,
        format!(
            "heap {}/{} bytes, stack_free {}",
            s.heap_used, s.heap_size, s.stack_free
        ),
    )
}

fn status_ip(cx: &mut Ctx) -> Result<Outcome, String> {
    let s = cx.status()?;
    match &s.ip {
        None => Ok(Outcome::Pass("ip is null: not on a network".into())),
        Some(ip) => {
            let parsed = ip.parse::<Ipv4Addr>();
            verdict(
                parsed.is_ok(),
                match parsed {
                    Ok(a) => format!("ip {a}"),
                    Err(e) => format!("ip {ip:?}: {e}"),
                },
            )
        }
    }
}

fn status_fw_fields(cx: &mut Ctx) -> Result<Outcome, String> {
    // Every one of these is a closed enum in `crates/device-api`, so a value
    // outside the set fails to parse and never reaches here. What this rule
    // adds is saying which values they are, which is what the bench wants out
    // of a firmware check anyway.
    let s = cx.status()?;
    verdict(
        !s.fw.is_empty() && !s.id.is_empty(),
        format!(
            "fw {} slot {:?} state {:?} reset {:?} store_errors {}",
            s.fw, s.fw_slot, s.fw_state, s.reset_reason, s.store_errors
        ),
    )
}

fn status_wifi_state_is_the_link(cx: &mut Ctx) -> Result<Outcome, String> {
    // `device-web.md`: `wifi_state` means the link; the sticky result of the
    // last credentials attempt belongs to `GET /api/v1/wifi` alone. The
    // observable form of that: a device holding an address is not `failed`.
    let s = cx.status()?;
    let ip = s.ip.as_deref().unwrap_or("null").to_string();
    let bad = s.ip.is_some() && s.wifi_state == WifiState::Failed;
    verdict(!bad, format!("wifi_state {:?} with ip {ip}", s.wifi_state))
}

// ---------------------------------------------------------------------------
// GET /api/v1/telemetry
// ---------------------------------------------------------------------------

fn telemetry_parses(cx: &mut Ctx) -> Result<Outcome, String> {
    let res = cx.get(route::TELEMETRY)?;
    if res.status != 200 {
        return verdict(false, format!("HTTP {}: {}", res.status, res.snippet(80)));
    }
    let t: TelemetryReply = res.parse()?;
    verdict(
        true,
        format!(
            "{} bytes, rx {} shown {} state {}",
            res.body.len(),
            t.frames_rx,
            t.frames_shown,
            crate::state_name(t.state)
        ),
    )
}

fn telemetry_agrees_with_udp(cx: &mut Ctx) -> Result<Outcome, String> {
    // The counters only ever rise, so an honest HTTP read taken between two
    // UDP reads must lie between them. That is a far tighter statement than
    // "within a tolerance", and it holds at 30 fps with a stream running.
    let before = cx.telemetry()?;
    let res = cx.get(route::TELEMETRY)?;
    let http: TelemetryReply = res.parse()?;
    let after = cx.telemetry()?;

    let between = |name: &str, lo: u32, mid: u32, hi: u32| {
        (mid >= lo && mid <= hi).then_some(()).ok_or_else(|| {
            format!("{name}: UDP {lo} then HTTP {mid} then UDP {hi}")
        })
    };
    let mut wrong = Vec::new();
    for (name, lo, mid, hi) in [
        ("frames_rx", before.frames_rx, http.frames_rx, after.frames_rx),
        (
            "frames_shown",
            before.frames_shown,
            http.frames_shown,
            after.frames_shown,
        ),
        (
            "frames_rejected",
            before.frames_rejected,
            http.frames_rejected,
            after.frames_rejected,
        ),
        ("uptime_ms", before.uptime_ms, http.uptime_ms, after.uptime_ms),
    ] {
        if let Err(e) = between(name, lo, mid, hi) {
            wrong.push(e);
        }
    }
    // The state byte is the same byte; it may legitimately change between the
    // reads, so either side of the sandwich is accepted.
    if http.state != before.state && http.state != after.state {
        wrong.push(format!(
            "state: UDP {} then HTTP {} then UDP {}",
            crate::state_name(before.state),
            crate::state_name(http.state),
            crate::state_name(after.state)
        ));
    }
    if http.brightness != before.brightness && http.brightness != after.brightness {
        wrong.push(format!(
            "brightness: {} vs {}",
            before.brightness, http.brightness
        ));
    }
    verdict(
        wrong.is_empty(),
        if wrong.is_empty() {
            format!(
                "rx {}<={}<={}; state {}; brightness {}",
                before.frames_rx,
                http.frames_rx,
                after.frames_rx,
                crate::state_name(http.state),
                http.brightness
            )
        } else {
            wrong.join("; ")
        },
    )
}

// ---------------------------------------------------------------------------
// GET /api/v1/networks
// ---------------------------------------------------------------------------

fn networks_list(cx: &mut Ctx) -> Result<Outcome, String> {
    let res = cx.get(route::NETWORKS)?;
    if is_unavailable(&res) {
        return Ok(Outcome::Skip(format!(
            "this build cannot scan: {}",
            unavailable_detail(&res)
        )));
    }
    if res.status == ErrorCode::RateLimited.status() {
        return Ok(Outcome::Skip(
            "something scanned inside the window just before this run".into(),
        ));
    }
    if res.status != 200 {
        return verdict(false, format!("HTTP {}: {}", res.status, res.snippet(80)));
    }
    let n: NetworksReply = res.parse()?;
    let mut wrong = Vec::new();
    if n.networks.len() > MAX_NETWORKS {
        wrong.push(format!("{} networks, cap is {MAX_NETWORKS}", n.networks.len()));
    }
    for w in n.networks.windows(2) {
        if w[0].rssi < w[1].rssi {
            wrong.push(format!("{} before {}: not strongest first", w[0].rssi, w[1].rssi));
            break;
        }
    }
    if n.networks.iter().any(|x| x.ssid.is_empty()) {
        wrong.push("an entry with an empty ssid".into());
    }
    verdict(
        wrong.is_empty(),
        if wrong.is_empty() {
            format!(
                "{} networks, {} bytes, strongest {} dBm",
                n.networks.len(),
                res.body.len(),
                n.networks.first().map_or(0, |x| x.rssi)
            )
        } else {
            wrong.join("; ")
        },
    )
}

fn networks_rate_limited(cx: &mut Ctx) -> Result<Outcome, String> {
    let first = cx.get(route::NETWORKS)?;
    if is_unavailable(&first) {
        return Ok(Outcome::Skip(format!(
            "this build cannot scan: {}",
            unavailable_detail(&first)
        )));
    }
    // Rule 11 has just scanned, so this one is usually already the refusal.
    let res = if first.status == 200 {
        cx.get(route::NETWORKS)?
    } else {
        first
    };
    if res.status != ErrorCode::RateLimited.status() {
        return verdict(
            false,
            format!(
                "two scans inside {} ms answered HTTP {}: {}",
                route::SCAN_MIN_INTERVAL_MS,
                res.status,
                res.snippet(80)
            ),
        );
    }
    let out = refusal(&res, ErrorCode::RateLimited)?;
    // The wait belongs in `detail` (`route::RateLimit::retry_after_ms` is
    // there for it), but the shape makes `detail` optional, so its absence is
    // reported rather than failed.
    Ok(match out {
        Outcome::Pass(d) => Outcome::Pass(format!(
            "{d}{}",
            if res
                .parse::<ErrorReply>()
                .is_ok_and(|r| r.detail.is_some())
            {
                ""
            } else {
                " (no wait in the detail)"
            }
        )),
        other => other,
    })
}

// ---------------------------------------------------------------------------
// GET /api/v1/wifi, and the form that cannot join
// ---------------------------------------------------------------------------

fn wifi_parses(cx: &mut Ctx) -> Result<Outcome, String> {
    let res = cx.get(route::WIFI)?;
    if res.status != 200 {
        return verdict(false, format!("HTTP {}: {}", res.status, res.snippet(80)));
    }
    let w: WifiReply = res.parse()?;
    verdict(
        true,
        format!(
            "state {:?} ssid {:?} ip {:?} reason {:?}",
            w.state,
            w.ssid.as_deref(),
            w.ip.as_deref(),
            w.reason
        ),
    )
}

fn wifi_reason(cx: &mut Ctx) -> Result<Outcome, String> {
    let res = cx.get(route::WIFI)?;
    let w: WifiReply = res.parse()?;
    let ok = (w.state == WifiState::Failed) == w.reason.is_some();
    verdict(
        ok,
        format!("state {:?} reason {:?}", w.state, w.reason),
    )
}

fn wifi_form_without_ssid(cx: &mut Ctx) -> Result<Outcome, String> {
    // Safe on the real device by default: a form with no `ssid` cannot start
    // a join, so nothing the device is doing is interrupted. It is also the
    // cheapest place to ask the question of spec 8.4 - does a refusal quote
    // back what it was sent?
    let res = cx.post_form(route::WIFI, &format!("psk={DUMMY_PSK}"))?;
    let out = refusal(&res, ErrorCode::BadForm)?;
    if res.text().contains(DUMMY_PSK) {
        return verdict(false, "the refusal quoted the PSK back".to_string());
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// POST /api/v1/settings
// ---------------------------------------------------------------------------

fn settings_empty(cx: &mut Ctx) -> Result<Outcome, String> {
    let res = cx.post_json(route::SETTINGS, "{}")?;
    if res.status != 200 {
        return verdict(false, format!("HTTP {}: {}", res.status, res.snippet(80)));
    }
    let s: SettingsReply = res.parse()?;
    // The reply is the whole state, not an echo of what was sent.
    verdict(
        s.brightness == cx.found.brightness,
        format!(
            "name {:?} brightness {} idle {:?} (found brightness {})",
            s.name.as_str(),
            s.brightness,
            s.idle_mode,
            cx.found.brightness
        ),
    )
}

fn settings_round_trip(cx: &mut Ctx) -> Result<Outcome, String> {
    let other = if cx.found.idle_mode == IdleMode::Dim {
        IdleMode::Status
    } else {
        IdleMode::Dim
    };
    // The `pin` is decision 3's parsed-and-ignored field, and it is sent with
    // a credential-shaped value on purpose: rule 38 greps every reply for it.
    let body = format!(
        r#"{{"name":"{PROBE_NAME}","idle_mode":"{}","pin":"{DUMMY_PSK}"}}"#,
        idle_name(other)
    );
    let res = cx.post_json(route::SETTINGS, &body)?;
    if res.status != 200 {
        return verdict(false, format!("HTTP {}: {}", res.status, res.snippet(80)));
    }
    let applied: SettingsReply = res.parse()?;
    let status = cx.status()?;

    // Put it back before judging, so a failure here still leaves the device
    // as it was found.
    let restore = format!(
        r#"{{"name":{},"idle_mode":"{}"}}"#,
        super::json_string(&cx.found.name),
        idle_name(cx.found.idle_mode)
    );
    let back = cx.set_settings(&restore)?;

    let mut wrong = Vec::new();
    if applied.name.as_str() != PROBE_NAME {
        wrong.push(format!("name came back {:?}", applied.name.as_str()));
    }
    if applied.idle_mode != other {
        wrong.push(format!("idle_mode came back {:?}", applied.idle_mode));
    }
    if status.name.as_str() != PROBE_NAME {
        wrong.push(format!("status said name {:?}", status.name.as_str()));
    }
    if status.idle_mode != other {
        wrong.push(format!("status said idle_mode {:?}", status.idle_mode));
    }
    if back.name.as_str() != cx.found.name {
        wrong.push(format!("restore left name {:?}", back.name.as_str()));
    }
    verdict(
        wrong.is_empty(),
        if wrong.is_empty() {
            format!(
                "name and idle {:?} set and put back to {:?}/{}",
                other,
                cx.found.name,
                idle_name(cx.found.idle_mode)
            )
        } else {
            wrong.join("; ")
        },
    )
}

fn settings_brightness(cx: &mut Ctx) -> Result<Outcome, String> {
    // Stepped **down** from what the suite found, never up: this runs against
    // a panel on laptop USB.
    let want = cx.found.brightness.saturating_sub(1);
    let res = cx.post_json(route::SETTINGS, &format!(r#"{{"brightness":{want}}}"#))?;
    if res.status != 200 {
        return verdict(false, format!("HTTP {}: {}", res.status, res.snippet(80)));
    }
    let applied: SettingsReply = res.parse()?;
    let status = cx.status()?;
    let udp = cx.telemetry().ok().map(|t| t.brightness);
    let found = cx.found.brightness;
    cx.set_settings(&format!(r#"{{"brightness":{found}}}"#))?;

    let mut wrong = Vec::new();
    if applied.brightness != want {
        wrong.push(format!("asked {want}, echoed {}", applied.brightness));
    }
    if status.brightness != want {
        wrong.push(format!("status said {}", status.brightness));
    }
    if let Some(b) = udp {
        if b != want {
            wrong.push(format!("UDP telemetry said {b}"));
        }
    }
    verdict(
        wrong.is_empty(),
        if wrong.is_empty() {
            format!(
                "{found} -> {want} everywhere{}, put back to {found}",
                udp.map_or(String::new(), |b| format!(" (UDP {b})"))
            )
        } else {
            wrong.join("; ")
        },
    )
}

fn settings_brightness_cap(cx: &mut Ctx) -> Result<Outcome, String> {
    let res = cx.post_json(route::SETTINGS, r#"{"brightness":255}"#)?;
    if res.status != 200 {
        return verdict(false, format!("HTTP {}: {}", res.status, res.snippet(80)));
    }
    let applied: SettingsReply = res.parse()?;
    let status = cx.status()?;
    let found = cx.found.brightness;
    cx.set_settings(&format!(r#"{{"brightness":{found}}}"#))?;
    verdict(
        applied.brightness == status.brightness,
        format!(
            "asked 255, applied {} and status agrees ({}){}",
            applied.brightness,
            status.brightness,
            if applied.brightness == 255 {
                ", so this build caps at 255"
            } else {
                ", which is the firmware cap"
            }
        ),
    )
}

fn settings_bad_json(cx: &mut Ctx) -> Result<Outcome, String> {
    let res = cx.post_json(route::SETTINGS, "not json at all")?;
    refusal(&res, ErrorCode::BadJson)
}

// ---------------------------------------------------------------------------
// POST /api/v1/identify
// ---------------------------------------------------------------------------

fn identify_overlay(cx: &mut Ctx) -> Result<Outcome, String> {
    // Spec 7.3: an overlay, not a stream state, and it wins over every other
    // overlay while it is up. Kept short, and stopped explicitly.
    let res = cx.post_json(route::IDENTIFY, r#"{"duration_ms":2000}"#)?;
    if res.status != 200 {
        return verdict(false, format!("HTTP {}: {}", res.status, res.snippet(80)));
    }
    let a: AcceptedReply = res.parse()?;
    std::thread::sleep(Duration::from_millis(200));
    let during_http = cx.status()?.state;
    let during_udp = cx.telemetry().ok().map(|t| t.state);
    cx.post_json(route::IDENTIFY, r#"{"duration_ms":0}"#)?;
    std::thread::sleep(Duration::from_millis(200));
    let after_http = cx.status()?.state;

    let mut wrong = Vec::new();
    if a.result != Accepted::Identifying {
        wrong.push(format!("answered {:?}", a.result));
    }
    if during_http != screeny_device_api::StreamState::Identify {
        wrong.push(format!("status.state read {during_http:?} during"));
    }
    if after_http == screeny_device_api::StreamState::Identify {
        wrong.push("still identifying after duration_ms 0".into());
    }
    if let Some(s) = during_udp {
        if s != screeny_proto::control::state::IDENTIFY {
            wrong.push(format!("UDP telemetry read {} during", crate::state_name(s)));
        }
    }
    verdict(
        wrong.is_empty(),
        if wrong.is_empty() {
            format!("{during_http:?} then {after_http:?}")
        } else {
            wrong.join("; ")
        },
    )
}

fn identify_out_of_range(cx: &mut Ctx) -> Result<Outcome, String> {
    let res = cx.post_json(
        route::IDENTIFY,
        &format!(r#"{{"duration_ms":{}}}"#, MAX_IDENTIFY_MS + 1),
    )?;
    refusal(&res, ErrorCode::OutOfRange)
}

// ---------------------------------------------------------------------------
// POST /api/v1/firmware
// ---------------------------------------------------------------------------

/// Is the device refusing because it is in the middle of an OTA trial?
///
/// Card 241: while an image is on trial the inactive slot holds the image the
/// device may have to roll back to, so an upload is refused `busy` rather than
/// allowed to overwrite the escape hatch. That is correct behaviour and not a
/// failure of any rule here - it just means this suite cannot ask its question
/// for the next couple of minutes, which is what a skip is for.
fn busy_with_a_trial(res: &Res) -> Option<Outcome> {
    if res.status != 200 {
        return None;
    }
    let f: FirmwareReply = res.parse().ok()?;
    (f.error == Some(FirmwareError::Busy)).then(|| {
        Outcome::Skip(
            "the device is busy with an upload or an OTA trial; try again in three minutes"
                .to_owned(),
        )
    })
}

/// The one shape of firmware upload this suite will ever send: a body that
/// **cannot** pass the first of research 006's checks, so nothing can be
/// installed by running the suite.
fn firmware_refuses(cx: &mut Ctx, body: &[u8], what: &str) -> Result<Outcome, String> {
    let res = cx.post_bytes(route::FIRMWARE, body)?;
    if is_unavailable(&res) {
        return Ok(Outcome::Skip(format!(
            "this build does not take uploads: {}",
            unavailable_detail(&res)
        )));
    }
    if let Some(s) = busy_with_a_trial(&res) {
        return Ok(s);
    }
    if res.status != 200 {
        // Any other refusal is fine too, as long as it is the error shape.
        return match res.error() {
            Ok(r) => Ok(Outcome::Pass(format!("HTTP {} {}", res.status, r.error))),
            Err(e) => verdict(false, format!("HTTP {}: {e}", res.status)),
        };
    }
    let f: FirmwareReply = res.parse()?;
    verdict(
        !f.ok,
        format!(
            "{what}: ok {} written {} error {:?}",
            f.ok, f.written, f.error
        ),
    )
}

fn firmware_empty(cx: &mut Ctx) -> Result<Outcome, String> {
    firmware_refuses(cx, &[], "empty body")
}

fn firmware_not_an_image(cx: &mut Ctx) -> Result<Outcome, String> {
    // 64 bytes of zeroes: no `0xE9` magic, so the first check fails before
    // anything could be written anywhere.
    firmware_refuses(cx, &[0u8; 64], "64 bytes of zeroes")
}

/// The same, but insisting on *which* refusal (card 240).
///
/// Rules 23 and 24 accept any refusal, because they were written when no
/// firmware served the route at all. The four below know what the answer
/// should be, and a device that refuses an ESP32-C3 image as `bad_magic` has a
/// bug worth finding even though it refused it.
fn firmware_refuses_with(
    cx: &mut Ctx,
    body: &[u8],
    want: FirmwareError,
    what: &str,
) -> Result<Outcome, String> {
    let res = cx.post_bytes(route::FIRMWARE, body)?;
    if is_unavailable(&res) {
        return Ok(Outcome::Skip(format!(
            "this build does not take uploads: {}",
            unavailable_detail(&res)
        )));
    }
    if let Some(s) = busy_with_a_trial(&res) {
        return Ok(s);
    }
    if res.status != 200 {
        return match res.error() {
            Ok(r) => verdict(
                false,
                format!(
                    "HTTP {} {} - this route answers 200 with ok:false",
                    res.status, r.error
                ),
            ),
            Err(e) => verdict(false, format!("HTTP {}: {e}", res.status)),
        };
    }
    let f: FirmwareReply = res.parse()?;
    verdict(
        !f.ok && f.error == Some(want),
        format!(
            "{what}: ok {} written {} error {:?} (wanted {:?})",
            f.ok, f.written, f.error, want
        ),
    )
}

/// Rule 40. A perfectly well-formed image, with a correct checksum and a
/// correct appended SHA-256, built for an ESP32-C3.
///
/// This is the upload that would brick the panel if the chip check were not
/// there, and it is **safe to send**: research 006 section 5 puts the chip
/// check second, on the header's own bytes, and the firmware runs it before it
/// erases anything.
fn firmware_wrong_chip(cx: &mut Ctx) -> Result<Outcome, String> {
    let image = screeny_fwimage::build::Builder::wrong_chip().build();
    firmware_refuses_with(cx, &image, FirmwareError::WrongChip, "an ESP32-C3 image")
}

/// Rule 41. A correct ESP32 image of a different project.
fn firmware_wrong_project(cx: &mut Ctx) -> Result<Outcome, String> {
    let image = screeny_fwimage::build::Builder::wrong_project().build();
    firmware_refuses_with(cx, &image, FirmwareError::WrongProject, "somebody else's app")
}

/// Rule 42. `Content-Length` bigger than the slot, and almost no body.
///
/// Research 006 section 5 asks for this refusal "before a single sector is
/// erased", and this proves it the only way that is cheap: *declare* a body
/// larger than a 2 MiB slot, send a few bytes, half-close. A device that reads
/// `Content-Length` first answers straight away; one that had started erasing
/// would have had to read the body to know how long it was.
///
/// The half-close matters. A server that refuses without reading still has to
/// account for the rest of the body before it replies, and the end-of-stream
/// is what lets it stop at once instead of waiting out its read timeout. The
/// 6 s budget is for a server that ignores it.
fn firmware_too_large(cx: &mut Ctx) -> Result<Outcome, String> {
    // One byte past the slot in `firmware/partitions.csv`.
    const DECLARED: usize = 0x20_0000 + 1;
    let image = screeny_fwimage::build::Builder::good().build();
    let res = cx.post_bytes_declaring(route::FIRMWARE, DECLARED, &image[..64])?;
    if is_unavailable(&res) {
        return Ok(Outcome::Skip(format!(
            "this build does not take uploads: {}",
            unavailable_detail(&res)
        )));
    }
    if res.status != 200 {
        return match res.error() {
            // `payload_too_large` in the generic shape is a defensible answer
            // too - it is what a server that bounds the route before routing
            // would say. Accept it, and say which one it was.
            Ok(r) if r.error == ErrorCode::PayloadTooLarge => {
                Ok(Outcome::Pass("HTTP 413 payload_too_large".into()))
            }
            Ok(r) => verdict(false, format!("HTTP {} {}", res.status, r.error)),
            Err(e) => verdict(false, format!("HTTP {}: {e}", res.status)),
        };
    }
    let f: FirmwareReply = res.parse()?;
    verdict(
        !f.ok && f.error == Some(FirmwareError::TooLarge) && f.written == 0,
        format!(
            "declared {DECLARED} bytes: ok {} written {} error {:?} (wanted written 0, too_large)",
            f.ok, f.written, f.error
        ),
    )
}

/// Rule 43. A good image with its last 200 bytes missing.
///
/// Research 006 section 5's interruption table, as a test: the connection that
/// dies mid-upload. **This one does reach flash** - the header is ours, so the
/// device stages what arrives - and it is still safe, because the slot it
/// stages into is the one nothing boots and this firmware never writes
/// `otadata`. What it must not be is `ok`.
fn firmware_truncated(cx: &mut Ctx) -> Result<Outcome, String> {
    let image = screeny_fwimage::build::Builder::good().build();
    // Everything but the tail of the appended digest.
    let cut = image.len() - 200;
    firmware_refuses_with(
        cx,
        &image[..cut],
        FirmwareError::BadSha256,
        "a good image with 200 bytes missing",
    )
}

/// Card 241: `?activate=` is parsed, and a value this API does not define is
/// refused rather than guessed at.
///
/// The body is 64 bytes of zeroes - an image that cannot pass check 1 - so a
/// device that ignored the query entirely would answer `bad_magic` and fail
/// this rule for the right reason. **Nothing here can be staged**, and the
/// refusal happens on the query before the body is read at all.
fn firmware_bad_activate(cx: &mut Ctx) -> Result<Outcome, String> {
    let path = format!("{}?activate=maybe", route::FIRMWARE);
    let res = cx.post_bytes(&path, &[0u8; 64])?;
    if is_unavailable(&res) {
        return Ok(Outcome::Skip(format!(
            "this build does not take uploads: {}",
            unavailable_detail(&res)
        )));
    }
    refusal(&res, ErrorCode::OutOfRange)
}

/// Card 241: `?activate=0` is the staging-only upload card 240 shipped, and an
/// accepted or refused one says `activating: false`.
///
/// Sent with the same unstageable body as every other rule in this section, so
/// what is being checked is that the flag parses and that the reply carries the
/// field - not that anything was installed, which this suite must never do.
fn firmware_stage_only(cx: &mut Ctx) -> Result<Outcome, String> {
    let path = format!("{}?activate=0", route::FIRMWARE);
    let res = cx.post_bytes(&path, &[0u8; 64])?;
    if is_unavailable(&res) {
        return Ok(Outcome::Skip(format!(
            "this build does not take uploads: {}",
            unavailable_detail(&res)
        )));
    }
    if let Some(s) = busy_with_a_trial(&res) {
        return Ok(s);
    }
    if res.status != 200 {
        return verdict(false, format!("HTTP {} - expected 200", res.status));
    }
    let f: FirmwareReply = res.parse()?;
    verdict(
        !f.ok && !f.activating && f.error == Some(FirmwareError::BadMagic),
        format!(
            "ok {} activating {} error {:?}",
            f.ok, f.activating, f.error
        ),
    )
}

// ---------------------------------------------------------------------------
// The refusals
// ---------------------------------------------------------------------------

fn wrong_method(cx: &mut Ctx) -> Result<Outcome, String> {
    // `/api/v1/settings` exists, but not as a GET.
    let res = cx.get(route::SETTINGS)?;
    refusal(&res, ErrorCode::MethodNotAllowed)
}

fn unknown_verb(cx: &mut Ctx) -> Result<Outcome, String> {
    // `route::path_is_known`'s own doc says a `PUT /api/v1/status` is a 405
    // and not a 404: the path exists, the verb does not.
    let res = cx.request("DELETE", route::STATUS, None, b"")?;
    refusal(&res, ErrorCode::MethodNotAllowed)
}

fn unknown_path(cx: &mut Ctx) -> Result<Outcome, String> {
    let mut wrong = Vec::new();
    let mut detail = String::new();
    for path in ["/api/v1/nope", "/api/v2/status"] {
        let res = cx.get(path)?;
        match refusal(&res, ErrorCode::NotFound)? {
            Outcome::Pass(d) => detail = d,
            Outcome::Fail(d) => wrong.push(format!("{path}: {d}")),
            Outcome::Skip(d) => wrong.push(format!("{path}: {d}")),
        }
    }
    verdict(
        wrong.is_empty(),
        if wrong.is_empty() {
            format!("/api/v1/nope and /api/v2/status: {detail}")
        } else {
            wrong.join("; ")
        },
    )
}

/// A body one byte over `len`, for `path`, that a server which parsed it
/// instead of refusing it would still do nothing with: no `ssid`, no
/// `confirm`, no settings field.
fn oversize_for(r: &route::Route) -> (String, &'static str) {
    let pad = "x".repeat(r.max_request_len);
    if r.body == route::Body::Form {
        (format!("nothing={pad}"), "application/x-www-form-urlencoded")
    } else {
        (format!(r#"{{"nothing":"{pad}"}}"#), "application/json")
    }
}

fn oversize_body(cx: &mut Ctx) -> Result<Outcome, String> {
    // `MAX_REQUEST_LEN` is the WiFi form's 384 bytes and it is the number card
    // 222 sizes picoserve's buffer from, so this is the one length bound every
    // build has to enforce however it routes.
    let r = route::find(route::WIFI, route::Method::Post)
        .ok_or("POST /api/v1/wifi is not in ROUTES")?;
    let (body, ct) = oversize_for(r);
    let res = cx.request("POST", route::WIFI, Some(ct), body.as_bytes())?;
    if res.status != ErrorCode::PayloadTooLarge.status() {
        return verdict(
            false,
            format!(
                "{} bytes (bound {}) answered HTTP {}: {}",
                body.len(),
                route::MAX_REQUEST_LEN,
                res.status,
                res.snippet(60)
            ),
        );
    }
    let out = refusal(&res, ErrorCode::PayloadTooLarge)?;
    Ok(match out {
        Outcome::Pass(d) => Outcome::Pass(format!(
            "{} bytes over the {}-byte bound: {d}",
            body.len(),
            route::MAX_REQUEST_LEN
        )),
        other => other,
    })
}

fn oversize_body_per_route(cx: &mut Ctx) -> Result<Outcome, String> {
    // Every other route that buffers a body, one byte over **its own** bound.
    // A server that only knows the global `MAX_REQUEST_LEN` parses these
    // instead of refusing them, which is what card 223 wires up on the device
    // ("use route::find, route::RateLimit and per-route max_request_len").
    let mut wrong = Vec::new();
    let mut sizes = Vec::new();
    for r in route::ROUTES {
        if r.max_request_len == 0 || r.max_request_len == route::MAX_REQUEST_LEN {
            continue;
        }
        let (body, ct) = oversize_for(r);
        let res = cx.request("POST", r.path, Some(ct), body.as_bytes())?;
        sizes.push(format!("{} {}>{}", r.path, body.len(), r.max_request_len));
        if res.status != ErrorCode::PayloadTooLarge.status() {
            wrong.push(format!(
                "{}: {} bytes over {} answered HTTP {}: {}",
                r.path,
                body.len(),
                r.max_request_len,
                res.status,
                res.snippet(60)
            ));
        } else if let Err(e) = res.error() {
            wrong.push(format!("{}: {e}", r.path));
        }
    }
    verdict(
        wrong.is_empty(),
        if wrong.is_empty() {
            format!("413 for {}", sizes.join(", "))
        } else {
            wrong.join("; ")
        },
    )
}

// ---------------------------------------------------------------------------
// GET /
// ---------------------------------------------------------------------------

fn index_page(cx: &mut Ctx) -> Result<Outcome, String> {
    let res = cx.get("/")?;
    if res.status != 200 {
        return verdict(false, format!("HTTP {}: {}", res.status, res.snippet(80)));
    }
    if res.content_type() != "text/html" {
        return verdict(false, format!("content-type {:?}", res.content_type()));
    }
    if res.body.is_empty() {
        return verdict(false, "the page is empty".to_string());
    }
    // Self-contained: nothing on it may need the internet, because the device
    // is often the only thing the phone looking at it can reach. The device's
    // own address and the portal's (`screeny_provision::PORTAL_IP`) are not
    // external.
    let text = res.text();
    let mine = [
        format!("http://{}", cx.http.host()),
        format!("https://{}", cx.http.host()),
        "http://192.168.4.1".to_string(),
    ];
    let mut external = Vec::new();
    // An absolute URL anywhere on the page...
    for pat in ["http://", "https://"] {
        for (i, _) in text.match_indices(pat) {
            let url = url_at(&text[i..]);
            if !mine.iter().any(|m| url.starts_with(m.as_str())) {
                external.push(url);
            }
        }
    }
    // ...and a scheme-relative one, which is how a CDN is usually pulled in.
    for pat in ["=\"//", "='//", "(//", "@import \"//"] {
        for (i, _) in text.match_indices(pat) {
            external.push(url_at(&text[i + pat.len() - 2..]));
        }
    }
    external.sort();
    external.dedup();
    verdict(
        external.is_empty(),
        if external.is_empty() {
            format!("{} bytes of HTML, no external URL", res.body.len())
        } else {
            format!("external: {}", external.join(", "))
        },
    )
}

/// The URL starting at the front of `s`, up to whatever ends it in HTML.
fn url_at(s: &str) -> String {
    s.chars()
        .take(80)
        .take_while(|c| !c.is_whitespace() && !matches!(c, '"' | '\'' | ')' | '<' | '>' | ';'))
        .collect()
}

// ---------------------------------------------------------------------------
// Timing
// ---------------------------------------------------------------------------

fn timing(cx: &mut Ctx) -> Result<Outcome, String> {
    // Reported, not judged: over WiFi this number is about the air, and card
    // 222 measured 25-37 ms for a lone request and a flat 1 s when the one
    // connection worker was busy. The rule only fails if a request does not
    // come back at all, or takes long enough to mean something is wrong.
    let mut ms = Vec::new();
    for _ in 0..5 {
        let t0 = Instant::now();
        let res = cx.get(route::STATUS)?;
        if res.status != 200 {
            return verdict(false, format!("HTTP {}", res.status));
        }
        ms.push(t0.elapsed().as_secs_f64() * 1000.0);
        // Let the device's single worker go idle, so each of these really is
        // a lone request rather than a queue.
        std::thread::sleep(Duration::from_millis(300));
    }
    ms.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = ms[ms.len() / 2];
    verdict(
        ms[ms.len() - 1] < 5_000.0,
        format!(
            "median {median:.1} ms (min {:.1}, max {:.1}, n {})",
            ms[0],
            ms[ms.len() - 1],
            ms.len()
        ),
    )
}

// ---------------------------------------------------------------------------
// The two that disturb the device
// ---------------------------------------------------------------------------

fn wifi_post_credentials(cx: &mut Ctx) -> Result<Outcome, String> {
    // Spec 8.2: the reply goes out **before** the radio work, so this comes
    // back promptly even though the device is about to leave the network.
    let t0 = Instant::now();
    let res = cx.post_form(
        route::WIFI,
        &format!("ssid={DUMMY_SSID}&psk={DUMMY_PSK}"),
    )?;
    if is_unavailable(&res) {
        return Ok(Outcome::Skip(format!(
            "no trial from this state: {}",
            unavailable_detail(&res)
        )));
    }
    if res.status != 200 {
        return verdict(false, format!("HTTP {}: {}", res.status, res.snippet(80)));
    }
    let a: AcceptedReply = res.parse()?;
    verdict(
        a.result == Accepted::Trying,
        format!(
            "{:?} in {:.0} ms; the device is now off its network",
            a.result,
            t0.elapsed().as_secs_f64() * 1000.0
        ),
    )
}

/// How long after the post a `connected` answer is read as "the trial has not
/// started yet" rather than as its result.
const NOT_STARTED_GRACE: Duration = Duration::from_secs(5);

fn wifi_after_the_trial(cx: &mut Ctx) -> Result<Outcome, String> {
    // Two things have to settle, and they are not the same thing: the device
    // has to be reachable again (on the bench it is away for about a minute
    // while it tries the dummy pair and falls back), and the attempt has to
    // have *resolved*. Polling until the state is no longer `connecting` is
    // the only wait that is right for both a device and a simulator, where
    // the whole trial is over in 40 ms.
    let t0 = Instant::now();
    let mut last = String::from("nothing answered at all");
    let mut result: Option<WifiReply> = None;
    while t0.elapsed() < PATIENCE {
        match cx.get(route::WIFI) {
            Ok(res) if res.status == 200 => match res.parse::<WifiReply>() {
                Ok(w) if w.state == WifiState::Connecting => {
                    last = "still connecting".into();
                }
                // The device answers `trying` *first* and starts the radio work
                // a moment later (spec 8.2; firmware 0.5.0 waits 100 ms so the
                // reply really leaves). A poll inside that window still reads
                // the old `connected`, which is "not started yet", not a
                // verdict: the first device run of this rule failed on exactly
                // that, at "0 s", while the device went on to report
                // failed/not_found seven seconds later.
                Ok(w) if w.state == WifiState::Connected && t0.elapsed() < NOT_STARTED_GRACE => {
                    last = "still reads the old connection".into();
                }
                Ok(w) => {
                    result = Some(w);
                    break;
                }
                Err(e) => last = e,
            },
            Ok(res) => last = format!("HTTP {}: {}", res.status, res.snippet(60)),
            Err(e) => last = e,
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    let away = t0.elapsed();
    let Some(w) = result else {
        return verdict(
            false,
            format!(
                "the attempt had not resolved after {:.0} s ({last})",
                away.as_secs_f32()
            ),
        );
    };
    let status = cx.status()?;

    let mut wrong = Vec::new();
    if w.state != WifiState::Failed {
        wrong.push(format!("GET /api/v1/wifi says {:?}", w.state));
    }
    if w.reason.is_none() {
        wrong.push("reason is null: the device knows why and did not say".into());
    }
    // ...and the sticky result stayed on the route it belongs to.
    if status.ip.is_some() && status.wifi_state == WifiState::Failed {
        wrong.push(format!(
            "status.wifi_state is failed while holding {}",
            status.ip.as_deref().unwrap_or("an address")
        ));
    }
    verdict(
        wrong.is_empty(),
        if wrong.is_empty() {
            format!(
                "answered again after {:.0} s: wifi {:?}/{:?}, status {:?} ip {:?}",
                away.as_secs_f32(),
                w.state,
                w.reason,
                status.wifi_state,
                status.ip.as_deref()
            )
        } else {
            format!("after {:.0} s: {}", away.as_secs_f32(), wrong.join("; "))
        },
    )
}

fn reboot_unconfirmed(cx: &mut Ctx) -> Result<Outcome, String> {
    let before = cx.status()?;
    let res = cx.post_json(route::REBOOT, r#"{"confirm":"nope"}"#)?;
    let out = refusal(&res, ErrorCode::OutOfRange)?;
    // ...and it really did not reboot.
    let after = cx.status()?;
    if after.boot_id != before.boot_id {
        return verdict(false, "the device restarted anyway".to_string());
    }
    Ok(out)
}

fn reboot_confirmed(cx: &mut Ctx) -> Result<Outcome, String> {
    let before = cx.status()?;
    let res = cx.post_json(route::REBOOT, r#"{"confirm":"RBOO"}"#)?;
    if res.status != 200 {
        return verdict(false, format!("HTTP {}: {}", res.status, res.snippet(80)));
    }
    let a: AcceptedReply = res.parse()?;
    if a.result != Accepted::Rebooting {
        return verdict(false, format!("answered {:?}", a.result));
    }
    // Give it time to actually go, so that "it answered again" is not just the
    // request that beat the restart.
    std::thread::sleep(Duration::from_secs(2));
    let t0 = Instant::now();
    let after = cx.wait_for_device(PATIENCE)?;
    // Answering again at a LAN address is the whole of the bench rule
    // `device-web.md` states after a wrong-credentials test: it rebooted and
    // it rejoined, because there is no other way this request arrived.
    let rejoined = if cx.http.addr().ip().is_loopback() {
        ""
    } else {
        ", so it rejoined its network"
    };
    verdict(
        after.boot_id != before.boot_id,
        format!(
            "back in {:.0} s{rejoined}, boot_id {} -> {}, reset_reason {:?}, uptime {} ms",
            t0.elapsed().as_secs_f32(),
            before.boot_id,
            after.boot_id,
            after.reset_reason,
            after.uptime_ms
        ),
    )
}

// ---------------------------------------------------------------------------
// The whole run
// ---------------------------------------------------------------------------

fn every_reply_within_its_bound(cx: &mut Ctx) -> Result<Outcome, String> {
    let mut wrong = Vec::new();
    let mut worst = (0usize, 0usize, String::new());
    for s in &cx.seen {
        if s.content_type != "application/json" {
            continue;
        }
        let bound = if s.status >= 400 {
            ErrorReply::MAX_JSON_LEN
        } else {
            let method = match s.method.as_str() {
                "GET" => route::Method::Get,
                "POST" => route::Method::Post,
                _ => continue,
            };
            match route::find(&s.path, method) {
                Some(r) => r.max_reply_len,
                None => continue,
            }
        };
        if s.body.len() > bound {
            wrong.push(format!(
                "{} {}: {} bytes, bound {bound}",
                s.method,
                s.path,
                s.body.len()
            ));
        } else if s.body.len() * 100 / bound.max(1) > worst.0 * 100 / worst.1.max(1) {
            worst = (
                s.body.len(),
                bound,
                format!("{} {}", s.method, s.path),
            );
        }
    }
    verdict(
        wrong.is_empty(),
        if wrong.is_empty() {
            format!(
                "{} JSON replies checked; fullest {} at {}/{} bytes",
                cx.seen.iter().filter(|s| s.content_type == "application/json").count(),
                worst.2,
                worst.0,
                worst.1
            )
        } else {
            wrong.join("; ")
        },
    )
}

fn every_failure_is_the_error_shape(cx: &mut Ctx) -> Result<Outcome, String> {
    let mut wrong = Vec::new();
    let mut codes = Vec::new();
    let mut n = 0usize;
    for s in &cx.seen {
        if s.status < 400 {
            continue;
        }
        n += 1;
        let reply: ErrorReply = match serde_json::from_slice(&s.body) {
            Ok(r) => r,
            Err(e) => {
                wrong.push(format!("{} {} HTTP {}: {e}", s.method, s.path, s.status));
                continue;
            }
        };
        if reply.status() != s.status {
            wrong.push(format!(
                "{} {}: {} came back as HTTP {} and not {}",
                s.method,
                s.path,
                reply.error,
                s.status,
                reply.status()
            ));
        }
        // `{"error":..,"detail"?:..}` and nothing else.
        if let Ok(serde_json::Value::Object(o)) = serde_json::from_slice(&s.body) {
            for key in o.keys() {
                if key != "error" && key != "detail" {
                    wrong.push(format!("{} {}: stray key {key:?}", s.method, s.path));
                }
            }
        }
        let name = reply.error.to_string();
        if !codes.contains(&name) {
            codes.push(name);
        }
    }
    codes.sort();
    verdict(
        wrong.is_empty(),
        if wrong.is_empty() {
            format!("{n} refusals, codes: {}", codes.join(", "))
        } else {
            wrong.join("; ")
        },
    )
}

fn no_reply_carries_a_psk(cx: &mut Ctx) -> Result<Outcome, String> {
    let mut wrong = Vec::new();
    for s in &cx.seen {
        // The value first: whatever it was called, the dummy PSK this suite
        // sent must not come back.
        if s.body
            .windows(DUMMY_PSK.len())
            .any(|w| w == DUMMY_PSK.as_bytes())
        {
            wrong.push(format!("{} {} echoed the PSK", s.method, s.path));
        }
        // ...and no JSON reply may have a field for one. Not checked on the
        // HTML page, which has a password *input* and should.
        if s.content_type == "application/json" {
            if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&s.body) {
                let mut found = Vec::new();
                credential_keys(&v, &mut found);
                for k in found {
                    wrong.push(format!("{} {} has a {k:?} field", s.method, s.path));
                }
            }
        }
    }
    wrong.sort();
    wrong.dedup();
    verdict(
        wrong.is_empty(),
        if wrong.is_empty() {
            format!(
                "{} replies grepped for {DUMMY_PSK:?} and for a psk field",
                cx.seen.len()
            )
        } else {
            wrong.join("; ")
        },
    )
}

/// Every key anywhere in a JSON value that looks like a credential.
fn credential_keys(v: &serde_json::Value, out: &mut Vec<String>) {
    match v {
        serde_json::Value::Object(o) => {
            for (k, value) in o {
                let lower = k.to_ascii_lowercase();
                if lower.contains("psk") || lower.contains("pass") || lower.contains("secret") {
                    out.push(k.clone());
                }
                credential_keys(value, out);
            }
        }
        serde_json::Value::Array(a) => {
            for value in a {
                credential_keys(value, out);
            }
        }
        _ => {}
    }
}
