//! Card 224: every route in `screeny_device_api::route::ROUTES`, answered.
//!
//! The first test walks the table itself, so a route added to
//! `crates/device-api` and not served here is a failure with the route's name
//! in it rather than a gap nobody notices. The rest are the error shape, the
//! status codes, and the captive-portal catch-all of research 007 section 4.3.
//!
//! Everything is in-process, on an ephemeral port, and every request has a
//! five-second timeout (`common::http::TIMEOUT`).

mod common;

use std::net::SocketAddr;
use std::time::Duration;

use common::http::{self, Res};
use screeny_device_api::reply::{
    AcceptedReply, FirmwareReply, NetworksReply, SettingsReply, StatusReply, TelemetryReply,
    WifiReply,
};
use screeny_device_api::route::{self, Method, Route};
use screeny_device_api::{Accepted, FirmwareError, StreamState, WifiState};
use screeny_sim::{Config, SimDevice, WifiOutcome};

/// A device in the captive portal, which is the state in which every route -
/// including `POST /api/v1/wifi` - has something to do. `Slow` keeps the
/// trial join in flight for the whole test instead of completing under it.
fn portal_device() -> SimDevice {
    SimDevice::start(Config {
        start_in_portal: true,
        wifi_outcome: WifiOutcome::Slow,
        ..Config::for_test()
    })
    .expect("bind loopback")
}

fn online_device() -> SimDevice {
    SimDevice::start(Config::for_test()).expect("bind loopback")
}

fn api(dev: &SimDevice) -> SocketAddr {
    dev.http_addr().expect("the HTTP API is on by default")
}

/// A body that looks enough like an ESP32 image for the header checks.
/// A real ESP32 image of this project, built by the crate that validates one.
///
/// Card 240: the simulator runs the firmware's whole validator now, so a
/// `0xE9` followed by filler - which is what this used to be, and what the
/// old two-check simulator accepted - is no longer an image. `Builder` is
/// `screeny-fwimage`'s own, so a test that says "the simulator accepts this"
/// is saying it about something the device would accept too.
fn image() -> Vec<u8> {
    screeny_fwimage::build::Builder::good().build()
}

/// The smallest request each route will accept. The `_` arm is the point of
/// the whole file: a new row in `ROUTES` lands there and fails by name.
fn exercise(addr: SocketAddr, r: &Route) -> Res {
    match (r.method, r.path) {
        (Method::Get, path) => http::get(addr, path),
        (Method::Post, route::WIFI) => {
            http::post_form(addr, r.path, "ssid=Example-Wifi1&psk=password9")
        }
        (Method::Post, route::SETTINGS) => http::post_json(addr, r.path, r#"{"brightness":96}"#),
        (Method::Post, route::FIRMWARE) => http::post_bytes(addr, r.path, &image()),
        (Method::Post, route::REBOOT) => http::post_json(addr, r.path, r#"{"confirm":"RBOO"}"#),
        (Method::Post, route::IDENTIFY) => http::post_json(addr, r.path, r#"{"duration_ms":300}"#),
        (m, path) => panic!(
            "card 224: {m:?} {path} is in screeny_device_api::route::ROUTES and \
             this test does not exercise it - serve it in crates/sim/src/api.rs \
             and add it here"
        ),
    }
}

#[test]
fn every_route_in_the_table_is_served() {
    let dev = portal_device();
    let addr = api(&dev);
    for r in route::ROUTES {
        let res = exercise(addr, r);
        assert_ne!(
            res.status, 404,
            "{:?} {} answered not_found: {}",
            r.method,
            r.path,
            res.text()
        );
        assert_ne!(
            res.status, 405,
            "{:?} {} answered method_not_allowed: {}",
            r.method,
            r.path,
            res.text()
        );
        assert_eq!(
            res.status,
            200,
            "{:?} {} should have succeeded: {}",
            r.method,
            r.path,
            res.text()
        );
        assert_eq!(
            res.header("content-type"),
            Some("application/json"),
            "{} is not JSON",
            r.path
        );
    }
    dev.shutdown();
}

#[test]
fn every_reply_parses_as_the_type_the_firmware_will_send() {
    // The assertion that matters: the simulator's bytes go through the very
    // types the Studio and the firmware use, not through a `Value`.
    let dev = portal_device();
    let addr = api(&dev);

    let status: StatusReply = http::get(addr, route::STATUS).parse();
    assert_eq!(status.api, route::API_VERSION);
    assert_eq!(status.state, StreamState::Provisioning, "the portal is up");
    assert!(status.portal);
    assert_eq!(status.wifi_state, WifiState::Disconnected);
    assert_eq!(status.ssid, None, "an empty store has no SSID");
    assert_ne!(status.boot_id, 0);

    let _t: TelemetryReply = http::get(addr, route::TELEMETRY).parse();

    let nets: NetworksReply = http::get(addr, route::NETWORKS).parse();
    assert!(!nets.networks.is_empty());
    for w in nets.networks.windows(2) {
        assert!(w[0].rssi >= w[1].rssi, "strongest first");
    }
    assert!(
        nets.networks.len() <= screeny_device_api::reply::MAX_NETWORKS,
        "the cap is the crate's"
    );

    let wifi: WifiReply = http::get(addr, route::WIFI).parse();
    assert_eq!(wifi.state, WifiState::Disconnected);

    let accepted: AcceptedReply =
        http::post_form(addr, route::WIFI, "ssid=Example-Wifi1&psk=password9").parse();
    assert_eq!(accepted.result, Accepted::Trying);

    let settings: SettingsReply = http::post_json(addr, route::SETTINGS, r#"{"brightness":96}"#)
        .parse();
    assert_eq!(settings.brightness, 96);

    let img = image();
    let fw: FirmwareReply = http::post_bytes(addr, route::FIRMWARE, &img).parse();
    assert!(fw.ok);
    assert_eq!(fw.written as usize, img.len());

    let rebooting: AcceptedReply =
        http::post_json(addr, route::REBOOT, r#"{"confirm":"RBOO"}"#).parse();
    assert_eq!(rebooting.result, Accepted::Rebooting);

    let identifying: AcceptedReply =
        http::post_json(addr, route::IDENTIFY, r#"{"duration_ms":200}"#).parse();
    assert_eq!(identifying.result, Accepted::Identifying);

    dev.shutdown();
}

#[test]
fn the_telemetry_route_and_a_udp_sender_see_the_same_numbers() {
    let dev = online_device();
    let addr = api(&dev);
    let sim = dev.handle();
    let t: TelemetryReply = http::get(addr, route::TELEMETRY).parse();
    let udp = sim.telemetry();
    // Everything but the clock, which moved between the two reads.
    assert_eq!(t.frames_rx, udp.frames_rx);
    assert_eq!(t.frames_shown, udp.frames_shown);
    assert_eq!(t.brightness, udp.brightness);
    assert_eq!(t.state, udp.state);
    dev.shutdown();
}

#[test]
fn a_bad_body_is_the_crates_error_shape_with_the_crates_status() {
    let dev = portal_device();
    let addr = api(&dev);

    let res = http::post_json(addr, route::SETTINGS, "not json at all");
    assert_eq!(res.status, 400);
    assert_eq!(res.error_code(), "bad_json");
    // The shape is `{"error":..,"detail"?:..}` and nothing else.
    let obj = res.json();
    for key in obj.as_object().unwrap().keys() {
        assert!(matches!(key.as_str(), "error" | "detail"), "stray key {key}");
    }

    // A form that parses but breaks one of `parse_wifi_form`'s rules, with
    // that function's own code and its own sentence.
    let res = http::post_form(addr, route::WIFI, "psk=password9");
    assert_eq!(res.status, 400);
    assert_eq!(res.error_code(), "bad_form");
    assert_eq!(res.json()["detail"], "no ssid field");

    // Two `ssid=` keys is refused rather than resolved: writing one of two
    // credentials to flash on a coin toss is the one ambiguity this product
    // must not have.
    let res = http::post_form(addr, route::WIFI, "ssid=A&ssid=B");
    assert_eq!(res.error_code(), "bad_form");

    // `RBOO` is not optional.
    let res = http::post_json(addr, route::REBOOT, r#"{"confirm":"nope"}"#);
    assert_eq!(res.status, 400);
    assert_eq!(res.error_code(), "out_of_range");

    // `IDENTIFY`'s wire field is a u16 of milliseconds.
    let res = http::post_json(addr, route::IDENTIFY, r#"{"duration_ms":70000}"#);
    assert_eq!(res.status, 400);
    assert_eq!(res.error_code(), "out_of_range");

    dev.shutdown();
}

#[test]
fn a_body_over_the_routes_bound_is_payload_too_large() {
    let dev = portal_device();
    let addr = api(&dev);

    // `MAX_REQUEST_LEN` is the WiFi form's 384 bytes, and it is the number
    // card 222 sizes picoserve's buffer from, so one byte over must be
    // refused rather than accepted because a host could hold it.
    let mut body = String::from("ssid=Example-Wifi1&psk=");
    while body.len() <= route::MAX_REQUEST_LEN {
        body.push('x');
    }
    assert!(body.len() > route::MAX_REQUEST_LEN);
    let res = http::post_form(addr, route::WIFI, &body);
    assert_eq!(res.status, 413);
    assert_eq!(res.error_code(), "payload_too_large");

    // ...and exactly at the bound it is not refused for its length. (It is
    // refused for the PSK being longer than 64 bytes, which is the form
    // parser's own rule and a different code.)
    let mut at = String::from("ssid=Example-Wifi1&psk=");
    while at.len() < route::MAX_REQUEST_LEN {
        at.push('x');
    }
    assert_eq!(at.len(), route::MAX_REQUEST_LEN);
    let res = http::post_form(addr, route::WIFI, &at);
    assert_ne!(res.status, 413, "at the bound, not over it: {}", res.text());

    dev.shutdown();
}

#[test]
fn the_wrong_method_and_the_wrong_path_are_told_apart() {
    let dev = online_device();
    let addr = api(&dev);

    // A route that exists, with a method it does not have.
    let res = http::post_json(addr, route::STATUS, "{}");
    assert_eq!(res.status, 405);
    assert_eq!(res.error_code(), "method_not_allowed");

    // A verb the API does not use at all, on a route that exists.
    let res = http::request(addr, "DELETE", route::STATUS, &addr.to_string(), None, b"");
    assert_eq!(res.status, 405);

    // A GET on a POST-only route.
    let res = http::get(addr, route::SETTINGS);
    assert_eq!(res.status, 405);

    // No such route.
    let res = http::get(addr, "/api/v1/nope");
    assert_eq!(res.status, 404);
    assert_eq!(res.error_code(), "not_found");

    // A version that does not exist yet.
    assert_eq!(http::get(addr, "/api/v2/status").status, 404);

    dev.shutdown();
}

#[test]
fn a_second_scan_inside_ten_seconds_is_rate_limited() {
    let dev = online_device();
    let addr = api(&dev);
    assert_eq!(http::get(addr, route::NETWORKS).status, 200);
    let res = http::get(addr, route::NETWORKS);
    assert_eq!(res.status, 429);
    assert_eq!(res.error_code(), "rate_limited");
    dev.shutdown();
}

#[test]
fn the_firmware_route_checks_what_it_can_and_installs_nothing() {
    let dev = online_device();
    let addr = api(&dev);

    // Card 240: every one of research 006 section 5's checks, through the
    // firmware's own `screeny-fwimage`, so an image the simulator accepts is
    // one the device would stage.
    let img = image();
    let ok: FirmwareReply = http::post_bytes(addr, route::FIRMWARE, &img).parse();
    assert!(ok.ok);
    assert_eq!(ok.written as usize, img.len());
    assert_eq!(ok.error, None);

    let bad: FirmwareReply = http::post_bytes(addr, route::FIRMWARE, &[0x00; 64]).parse();
    assert!(!bad.ok);
    assert_eq!(bad.error, Some(FirmwareError::BadMagic));
    assert_eq!(bad.written, 0, "nothing reached the sink");

    // The checks the old two-check simulator could not run, each on an image
    // that is correct in every other respect - same checksum, same appended
    // hash. These are the ones that matter: a bad *image* is obvious, and a
    // good image for the wrong chip is not.
    let c3 = screeny_fwimage::build::Builder::wrong_chip().build();
    let r: FirmwareReply = http::post_bytes(addr, route::FIRMWARE, &c3).parse();
    assert_eq!(r.error, Some(FirmwareError::WrongChip));
    assert!(!r.ok);

    let theirs = screeny_fwimage::build::Builder::wrong_project().build();
    let r: FirmwareReply = http::post_bytes(addr, route::FIRMWARE, &theirs).parse();
    assert_eq!(r.error, Some(FirmwareError::WrongProject));

    let mut flipped = image();
    flipped[2000] ^= 0x01;
    let r: FirmwareReply = http::post_bytes(addr, route::FIRMWARE, &flipped).parse();
    assert_eq!(r.error, Some(FirmwareError::BadChecksum));

    let cut = &img[..img.len() - 200];
    let r: FirmwareReply = http::post_bytes(addr, route::FIRMWARE, cut).parse();
    assert_eq!(r.error, Some(FirmwareError::BadSha256), "a truncated upload");

    // An empty body has no magic byte to check.
    let empty: FirmwareReply = http::post_bytes(addr, route::FIRMWARE, &[]).parse();
    assert!(!empty.ok);
    assert_eq!(empty.written, 0);

    // ...and the device is untouched: nothing was installed, nothing
    // rebooted, and the panel is still the panel.
    assert_eq!(http::get(addr, route::STATUS).status, 200);
    dev.shutdown();
}

#[test]
fn settings_are_clamped_by_the_same_code_udp_clamps_with() {
    let dev = SimDevice::start(Config {
        brightness_cap: 100,
        ..Config::for_test()
    })
    .expect("bind loopback");
    let addr = api(&dev);
    let sim = dev.handle();

    let res: SettingsReply = http::post_json(addr, route::SETTINGS, r#"{"brightness":255}"#).parse();
    assert_eq!(res.brightness, 100, "the firmware cap, not what was asked");
    assert_eq!(sim.snapshot().brightness, 100, "and the device really moved");

    // A subset request leaves the rest alone, and the reply is the whole
    // state rather than an echo.
    let res: SettingsReply =
        http::post_json(addr, route::SETTINGS, r#"{"name":"Desk panel"}"#).parse();
    assert_eq!(res.name.as_str(), "Desk panel");
    assert_eq!(res.brightness, 100);
    assert_eq!(sim.snapshot().name, "Desk panel");

    let status: StatusReply = http::get(addr, route::STATUS).parse();
    assert_eq!(status.name.as_str(), "Desk panel");
    assert_eq!(status.brightness, 100);

    // An empty request is a no-op, not an error.
    let res = http::post_json(addr, route::SETTINGS, "{}");
    assert_eq!(res.status, 200);

    dev.shutdown();
}

#[test]
fn a_reboot_draws_a_new_boot_id_over_http_and_over_udp() {
    use screeny_proto::control::Request;

    let dev = online_device();
    let addr = api(&dev);
    let sim = dev.handle();

    let first: StatusReply = http::get(addr, route::STATUS).parse();
    let again: StatusReply = http::get(addr, route::STATUS).parse();
    assert_eq!(
        first.boot_id, again.boot_id,
        "the id is stable while the device is not restarting"
    );

    assert_eq!(
        http::post_json(addr, route::REBOOT, r#"{"confirm":"RBOO"}"#).status,
        200
    );
    let after: StatusReply = http::get(addr, route::STATUS).parse();
    assert_ne!(after.boot_id, first.boot_id, "HTTP reboot: a new boot_id");

    // The same must be true of the UDP op, because it is the same code path.
    let ctrl = common::Ctrl::new(sim.control_addr());
    ctrl.call(&Request::Reboot, 77).expect("reboot reply");
    let sim_id = sim
        .wait_until(Duration::from_secs(2), |_| true)
        .map(|_| sim.boot_id())
        .unwrap();
    assert_ne!(sim_id, after.boot_id, "UDP reboot: a new boot_id too");

    dev.shutdown();
}

// ---------------------------------------------------------------------------
// The captive-portal catch-all (research 007 section 4.3)
// ---------------------------------------------------------------------------

/// The probes of research 007 section 4.1, none of which is named in
/// `crates/sim/src/api.rs`: the rule is about the shape of the `Host`, not
/// about a list of domains that will be out of date next year.
const PROBES: &[(&str, &str)] = &[
    ("captive.apple.com", "/hotspot-detect.html"),
    ("connectivitycheck.gstatic.com", "/generate_204"),
    ("www.msftconnecttest.com", "/connecttest.txt"),
    ("nmcheck.gnome.org", "/check_network_status.txt"),
    ("firefox-portal-detection.com", "/generate_204"),
];

#[test]
fn in_the_portal_a_foreign_host_gets_the_setup_page_not_a_redirect() {
    // fw 0.5.1's answer, after the owner's phone test (card 223's Log, finding
    // 3): the page itself, so the captive sheet needs no further connection.
    let dev = portal_device();
    let addr = api(&dev);
    for (host, path) in PROBES {
        let res = http::get_with_host(addr, path, host);
        assert_eq!(res.status, 200, "{host}{path}");
        assert_eq!(res.header("location"), None, "{host}{path}: a redirect");
        assert!(
            res.header("content-type")
                .is_some_and(|c| c.starts_with("text/html")),
            "{host}{path}: {:?}",
            res.header("content-type")
        );
        // Not decoration: iOS needs content to pop the sheet, and Android
        // calls a Content-Length <= 4 answer a *failure* rather than a
        // portal (research 007 section 4.2).
        assert!(res.body.len() > 4, "{host}{path}: empty page");
        assert_eq!(
            res.header("content-length").map(str::parse::<usize>),
            Some(Ok(res.body.len())),
            "{host}{path}: the length has to be there for Android to read it"
        );
        // The sheet must not cache the setup page: it changes on every post.
        assert_eq!(
            res.header("cache-control"),
            Some("no-store"),
            "{host}{path}"
        );
    }
    dev.shutdown();
}

#[test]
fn off_the_portal_a_foreign_host_is_simply_not_found() {
    // There is no portal, so a setup page would be a lie.
    let dev = online_device();
    let addr = api(&dev);
    for (host, path) in PROBES {
        let res = http::get_with_host(addr, path, host);
        assert_eq!(res.status, 404, "{host}{path}");
        assert_eq!(res.error_code(), "not_found");
    }
    dev.shutdown();
}

#[test]
fn our_own_hosts_never_get_the_catch_all_even_in_the_portal() {
    let dev = portal_device();
    let addr = api(&dev);
    for host in [
        addr.to_string(),
        "192.168.4.1".to_string(),
        "localhost".to_string(),
        format!("{}.local", screeny_sim::DEFAULT_INSTANCE),
    ] {
        let res = http::get_with_host(addr, route::STATUS, &host);
        assert_eq!(res.status, 200, "{host}");
        // Both answers are a 200 now, so the *body* is what tells them apart:
        // the real route, not the setup page standing in for it.
        assert_eq!(
            res.header("content-type"),
            Some("application/json"),
            "{host} got the catch-all"
        );
    }
    dev.shutdown();
}

#[test]
fn the_index_is_a_placeholder_that_says_so_and_links_the_api() {
    let dev = online_device();
    let addr = api(&dev);
    let res = http::get(addr, "/");
    assert_eq!(res.status, 200);
    assert!(res
        .header("content-type")
        .is_some_and(|c| c.starts_with("text/html")));
    let body = res.text();
    assert!(body.contains("card 222"), "it should say whose page this is");
    for r in route::ROUTES {
        assert!(body.contains(r.path), "{} is not linked", r.path);
    }
    dev.shutdown();
}

// ---------------------------------------------------------------------------
// Hygiene
// ---------------------------------------------------------------------------

#[test]
fn shutdown_releases_the_port() {
    use std::net::TcpListener;

    let dev = SimDevice::start(Config {
        http_port: 0,
        ..Config::for_test()
    })
    .expect("bind loopback");
    let addr = api(&dev);
    assert_eq!(http::get(addr, route::STATUS).status, 200);
    dev.shutdown();

    // If the acceptor thread were still alive the listener would still hold
    // the port. This is the portable version of "no thread left behind".
    let re = TcpListener::bind(addr);
    assert!(
        re.is_ok(),
        "the HTTP listener outlived shutdown(): {:?}",
        re.err()
    );
}

/// Two simulators started with the *default* HTTP settings both come up.
///
/// The binary's default port is 8080, and several sessions run two or three
/// simulators at once without ever asking for HTTP. Before this, the second
/// one failed to start on a port nobody had chosen. Now the default port is a
/// preference: when it is taken, an ephemeral one is bound instead and a line
/// on stderr says so. The UDP ports here are ephemeral, so nothing but the
/// HTTP port is under test.
#[test]
fn two_simulators_with_default_http_settings_both_start() {
    let cfg = || Config {
        http_port: screeny_sim::DEFAULT_HTTP_PORT,
        ..Config::for_test()
    };
    // The first may itself fall back, if another process on this machine
    // already holds 8080. That is the behaviour, not a caveat.
    let a = SimDevice::start(cfg()).expect("the first simulator starts");
    let b = SimDevice::start(cfg()).expect("and so does the second");

    let (aa, ba) = (api(&a), api(&b));
    assert_ne!(aa, ba, "two servers cannot share one port");
    assert_eq!(http::get(aa, route::STATUS).status, 200);
    assert_eq!(http::get(ba, route::STATUS).status, 200);

    a.shutdown();
    b.shutdown();
}

/// ...but a port somebody named is a promise, so a busy one is an error.
#[test]
fn a_named_http_port_that_is_busy_is_an_error() {
    let first = SimDevice::start(Config::for_test()).expect("bind loopback");
    let taken = api(&first).port();

    let started = SimDevice::start(Config {
        http_port: taken,
        http_port_explicit: true,
        ..Config::for_test()
    });
    match started {
        Ok(dev) => {
            dev.shutdown();
            panic!("the port was asked for by name and is busy; it must not fall back");
        }
        Err(e) => assert_eq!(e.kind(), std::io::ErrorKind::AddrInUse, "{e}"),
    }

    first.shutdown();
}

#[test]
fn the_http_server_can_be_turned_off() {
    let dev = SimDevice::start(Config {
        http: false,
        ..Config::for_test()
    })
    .expect("bind loopback");
    assert_eq!(dev.http_addr(), None);
    assert_eq!(dev.handle().http_url(), None);
    // ...and the UDP half is entirely unaffected, which is the whole point of
    // the flag.
    assert_ne!(dev.control_addr().port(), 0);
    dev.shutdown();
}

#[test]
fn a_request_with_no_host_header_is_served_rather_than_caught() {
    // HTTP/1.0, or a script on a raw socket. Not a captive-portal probe.
    let dev = portal_device();
    let addr = api(&dev);
    let wire = http::raw(
        addr,
        format!("GET {} HTTP/1.1\r\nConnection: close\r\n\r\n", route::STATUS).as_bytes(),
    );
    let res = http::parse(&wire);
    assert_eq!(res.status, 200);
    assert_eq!(res.header("content-type"), Some("application/json"));
    dev.shutdown();
}

#[test]
fn a_chunked_request_body_is_refused_rather_than_misread() {
    let dev = online_device();
    let addr = api(&dev);
    let wire = http::raw(
        addr,
        format!(
            "POST {} HTTP/1.1\r\nHost: {addr}\r\nTransfer-Encoding: chunked\r\n\
             Connection: close\r\n\r\n2\r\n{{}}\r\n0\r\n\r\n",
            route::SETTINGS
        )
        .as_bytes(),
    );
    let res = http::parse(&wire);
    assert_eq!(res.status, 400);
    assert_eq!(res.error_code(), "bad_request");
    dev.shutdown();
}
