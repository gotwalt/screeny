//! The route table, as constants and as data.
//!
//! Card 222 routes on the constants (`picoserve::Router::route(route::STATUS,
//! ...)`), card 224 does the same in the simulator, and the Studio builds URLs
//! from them. [`ROUTES`] is the same table as data, so a test can prove the
//! server covers it and the README can be checked against it.

use crate::reply::{
    AcceptedReply, FirmwareReply, NetworksReply, PanicReply, SettingsReply, StatusReply,
    TelemetryReply, WifiReply,
};
use crate::request::{IdentifyRequest, RebootRequest, SettingsRequest};

/// The version in every path, and the `api` field of [`StatusReply`].
///
/// A breaking change to any shape here bumps this and moves the prefix; a new
/// optional field does not.
pub const API_VERSION: u8 = 1;

/// The prefix every route shares.
pub const PREFIX: &str = "/api/v1";

/// `GET /api/v1/status`: [`StatusReply`].
pub const STATUS: &str = "/api/v1/status";
/// `GET /api/v1/telemetry`: [`TelemetryReply`].
pub const TELEMETRY: &str = "/api/v1/telemetry";

/// The RTC panic breadcrumb (card 243). Its own route because the answer
/// cannot change while the device runs and because `StatusReply` is on the hot
/// path and is full - see [`crate::reply::StatusReply`].
pub const PANIC: &str = "/api/v1/panic";
/// `GET /api/v1/networks`: [`NetworksReply`]. Triggers a scan; rate-limited to
/// one per [`SCAN_MIN_INTERVAL_MS`], and a caller that asks sooner gets
/// [`ErrorCode::RateLimited`](crate::ErrorCode::RateLimited).
pub const NETWORKS: &str = "/api/v1/networks";

/// The shortest gap between two scans [`NETWORKS`] will really perform.
///
/// A scan takes the radio off the channel it is associated on for the better
/// part of a second per band, which on a device that is also receiving frames
/// is a visible stall. The portal page polls, so the number has to live
/// somewhere both implementations read rather than in each of their heads:
/// card 224 found the simulator and the firmware were about to pick it
/// separately. Pair it with [`RateLimit`].
pub const SCAN_MIN_INTERVAL_MS: u32 = 10_000;
/// `GET /api/v1/wifi`: [`WifiReply`].
/// `POST /api/v1/wifi`: an urlencoded [`WifiForm`](crate::form::WifiForm),
/// answered with [`AcceptedReply::TRYING`].
pub const WIFI: &str = "/api/v1/wifi";
/// `POST /api/v1/settings`: [`SettingsRequest`] in, [`SettingsReply`] out.
pub const SETTINGS: &str = "/api/v1/settings";
/// `POST /api/v1/firmware`: a raw `application/octet-stream` body,
/// [`FirmwareReply`] out.
pub const FIRMWARE: &str = "/api/v1/firmware";
/// `POST /api/v1/reboot`: [`RebootRequest`] in, [`AcceptedReply::REBOOTING`]
/// out, sent before the reboot.
pub const REBOOT: &str = "/api/v1/reboot";
/// `POST /api/v1/identify`: [`IdentifyRequest`] in,
/// [`AcceptedReply::IDENTIFYING`] out.
pub const IDENTIFY: &str = "/api/v1/identify";

/// An HTTP method, as much of one as this API uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// Safe, idempotent, no body.
    Get,
    /// Changes something, or costs something (a scan).
    Post,
}

/// What a route's request body is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Body {
    /// No body.
    None,
    /// `application/json`.
    Json,
    /// `application/x-www-form-urlencoded`.
    Form,
    /// `application/octet-stream`, streamed, never buffered whole.
    Stream,
}

/// One row of the route table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Route {
    /// The path.
    pub path: &'static str,
    /// The method.
    pub method: Method,
    /// What the request body is.
    pub body: Body,
    /// An upper bound on the request body's length, for
    /// [`Body::Json`] and [`Body::Form`]; `0` for the others. **This is the
    /// number that sizes picoserve's HTTP buffer**: its `Json` and `Form`
    /// extractors call `read_all()`, which needs the whole body in that
    /// buffer.
    pub max_request_len: usize,
    /// An upper bound on the reply body's length. Informational: picoserve
    /// streams JSON replies through a counting writer and needs no buffer for
    /// them (see the crate docs).
    pub max_reply_len: usize,
    /// Whether the route mutates, and so calls
    /// [`check_auth`](crate::request::check_auth).
    pub mutating: bool,
}

/// Every route, in the order the README lists them.
pub const ROUTES: &[Route] = &[
    Route {
        path: STATUS,
        method: Method::Get,
        body: Body::None,
        max_request_len: 0,
        max_reply_len: StatusReply::MAX_JSON_LEN,
        mutating: false,
    },
    Route {
        path: TELEMETRY,
        method: Method::Get,
        body: Body::None,
        max_request_len: 0,
        max_reply_len: TelemetryReply::MAX_JSON_LEN,
        mutating: false,
    },
    Route {
        path: PANIC,
        method: Method::Get,
        body: Body::None,
        max_request_len: 0,
        max_reply_len: PanicReply::MAX_JSON_LEN,
        mutating: false,
    },
    Route {
        path: NETWORKS,
        method: Method::Get,
        body: Body::None,
        max_request_len: 0,
        max_reply_len: NetworksReply::MAX_JSON_LEN,
        mutating: false,
    },
    Route {
        path: WIFI,
        method: Method::Get,
        body: Body::None,
        max_request_len: 0,
        max_reply_len: WifiReply::MAX_JSON_LEN,
        mutating: false,
    },
    Route {
        path: WIFI,
        method: Method::Post,
        body: Body::Form,
        max_request_len: crate::form::MAX_FORM_LEN,
        max_reply_len: AcceptedReply::MAX_JSON_LEN,
        mutating: true,
    },
    Route {
        path: SETTINGS,
        method: Method::Post,
        body: Body::Json,
        max_request_len: SettingsRequest::MAX_JSON_LEN,
        max_reply_len: SettingsReply::MAX_JSON_LEN,
        mutating: true,
    },
    Route {
        path: FIRMWARE,
        method: Method::Post,
        body: Body::Stream,
        max_request_len: 0,
        max_reply_len: FirmwareReply::MAX_JSON_LEN,
        mutating: true,
    },
    Route {
        path: REBOOT,
        method: Method::Post,
        body: Body::Json,
        max_request_len: RebootRequest::MAX_JSON_LEN,
        max_reply_len: AcceptedReply::MAX_JSON_LEN,
        mutating: true,
    },
    Route {
        path: IDENTIFY,
        method: Method::Post,
        body: Body::Json,
        max_request_len: IdentifyRequest::MAX_JSON_LEN,
        max_reply_len: AcceptedReply::MAX_JSON_LEN,
        mutating: true,
    },
];

/// The largest request body any route accepts.
///
/// Card 222 sizes picoserve's HTTP buffer from this: every extractor that
/// parses a body reads it all into that buffer first, and the streaming
/// firmware upload is the one route that does not.
pub const MAX_REQUEST_LEN: usize = {
    let mut max = 0;
    let mut i = 0;
    while i < ROUTES.len() {
        if ROUTES[i].max_request_len > max {
            max = ROUTES[i].max_request_len;
        }
        i += 1;
    }
    max
};

/// The row for `path` and `method`, if the API has one.
///
/// Every server was writing the same
/// `ROUTES.iter().find(|r| r.path == p && r.method == m)`; card 224 asked for
/// it to be written once. Pair it with [`path_is_known`] to tell a `405` from
/// a `404`:
///
/// ```
/// use screeny_device_api::route::{self, Method};
///
/// // Known pair: serve it, and size the body check from the row.
/// let r = route::find(route::WIFI, Method::Post).unwrap();
/// assert!(r.mutating);
///
/// // Known path, wrong method: 405, not 404.
/// assert!(route::find(route::STATUS, Method::Post).is_none());
/// assert!(route::path_is_known(route::STATUS));
///
/// // Unknown path: 404.
/// assert!(!route::path_is_known("/api/v1/nope"));
/// ```
#[must_use]
pub fn find(path: &str, method: Method) -> Option<&'static Route> {
    let mut i = 0;
    while i < ROUTES.len() {
        let r = &ROUTES[i];
        if r.path == path && r.method == method {
            return Some(r);
        }
        i += 1;
    }
    None
}

/// Whether any method is served at `path`.
///
/// This is the difference between "wrong method" (`405`) and "no such thing"
/// (`404`), and it is also the right answer for a verb this API has no
/// [`Method`] for at all: `PUT /api/v1/status` is a `405`.
#[must_use]
pub fn path_is_known(path: &str) -> bool {
    let mut i = 0;
    while i < ROUTES.len() {
        if ROUTES[i].path == path {
            return true;
        }
        i += 1;
    }
    false
}

// ---------------------------------------------------------------------------
// POST /api/v1/firmware?activate=... (card 241)
// ---------------------------------------------------------------------------

/// The one query parameter this API has.
///
/// `POST /api/v1/firmware` **activates by default**: the natural reading of
/// "send this firmware to the panel" is that the panel then runs it, and the
/// five-line instruction for the owner is one `curl`. Staging without
/// activating - which is all card 240's firmware could do, and which is what a
/// probe suite wants against a live device - is the thing you have to ask for.
pub const ACTIVATE_KEY: &str = "activate";

/// `?activate=` carried a value this API does not define.
///
/// Its own type rather than `()` so that a caller cannot confuse it with any
/// other kind of nothing; there is exactly one way this can go wrong, so it
/// carries no detail. Every server answers it
/// [`ErrorCode::OutOfRange`](crate::ErrorCode::OutOfRange).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BadActivate;

impl core::fmt::Display for BadActivate {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("activate must be 0 or 1")
    }
}

/// What `?activate=` said, if anything.
///
/// A query string, not a body, because the body of this route is a megabyte of
/// firmware streamed straight to flash: there is nowhere to put a flag in it,
/// and a header would be a second convention for one bit.
///
/// Accepts `1`/`true`/`yes` and `0`/`false`/`no`, in any case, and **refuses
/// anything else** rather than guessing. Guessing is how `?activate=maybe`
/// reboots a panel.
///
/// ```
/// use screeny_device_api::route::parse_activate;
///
/// assert_eq!(parse_activate(None), Ok(true), "the default is to activate");
/// assert_eq!(parse_activate(Some("activate=0")), Ok(false));
/// assert_eq!(parse_activate(Some("activate=false")), Ok(false));
/// assert_eq!(parse_activate(Some("other=1")), Ok(true), "unknown keys are ignored");
/// assert!(parse_activate(Some("activate=maybe")).is_err());
/// ```
///
/// # Errors
///
/// [`BadActivate`] when `activate` is present with a value this does not
/// recognise, which a server answers `out_of_range`.
pub fn parse_activate(query: Option<&str>) -> Result<bool, BadActivate> {
    let Some(q) = query else {
        return Ok(true);
    };
    let mut answer = Ok(true);
    for pair in q.split('&') {
        let (key, value) = match pair.split_once('=') {
            Some((k, v)) => (k, v),
            // A bare `?activate` is the HTML-form spelling of "yes".
            None => (pair, "1"),
        };
        if !key.eq_ignore_ascii_case(ACTIVATE_KEY) {
            continue;
        }
        answer = match value {
            v if v.eq_ignore_ascii_case("1")
                || v.eq_ignore_ascii_case("true")
                || v.eq_ignore_ascii_case("yes") =>
            {
                Ok(true)
            }
            v if v.eq_ignore_ascii_case("0")
                || v.eq_ignore_ascii_case("false")
                || v.eq_ignore_ascii_case("no") =>
            {
                Ok(false)
            }
            _ => Err(BadActivate),
        };
    }
    answer
}

// ---------------------------------------------------------------------------
// Rate limiting
// ---------------------------------------------------------------------------

/// "Not more often than every `interval_ms`", with no clock of its own.
///
/// [`NETWORKS`] is the reason it exists, but nothing here is scan-specific.
/// Like [`screeny_provision`](https://docs.rs/) and the receive rules, it
/// takes `now_ms` rather than reading a clock, so the whole of its behaviour -
/// including the 49.7-day `u32` wrap - is a unit test that runs in
/// microseconds.
///
/// **A refused request does not reset the timer.** Card 221 settled the same
/// question for the portal's retry: a caller that polls every second must not
/// be able to hold the window open forever, and a limiter that punished
/// polling would never let a busy page through at all.
///
/// ```
/// use screeny_device_api::route::{RateLimit, SCAN_MIN_INTERVAL_MS};
///
/// let mut limit = RateLimit::new(SCAN_MIN_INTERVAL_MS);
/// assert!(limit.allow(1_000), "the first one always goes through");
/// assert!(!limit.allow(5_000));
/// assert_eq!(limit.retry_after_ms(5_000), 6_000);
/// // Being refused at 5_000 did not push the window out to 15_000.
/// assert!(limit.allow(11_000));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimit {
    interval_ms: u32,
    /// When the last *allowed* call happened. `None` until there is one, so a
    /// device whose clock really is at 0 ms is not a special case.
    last: Option<u32>,
}

impl RateLimit {
    /// A limiter that allows one call per `interval_ms`, starting with the
    /// next one whenever it comes.
    #[must_use]
    pub const fn new(interval_ms: u32) -> Self {
        RateLimit {
            interval_ms,
            last: None,
        }
    }

    /// The gap this limiter enforces.
    #[must_use]
    pub const fn interval_ms(&self) -> u32 {
        self.interval_ms
    }

    /// Whether a call is allowed now, recording it if it is.
    ///
    /// `now_ms` is a free-running millisecond counter and is allowed to wrap;
    /// the comparison is wrapping, so a wrap costs at most one extra allowed
    /// call and never a permanently closed window.
    pub fn allow(&mut self, now_ms: u32) -> bool {
        if let Some(last) = self.last {
            if now_ms.wrapping_sub(last) < self.interval_ms {
                return false;
            }
        }
        self.last = Some(now_ms);
        true
    }

    /// How long until [`allow`](Self::allow) would say yes: `0` when it would
    /// say yes now. Good for the `detail` of a
    /// [`RateLimited`](crate::ErrorCode::RateLimited) reply.
    #[must_use]
    pub fn retry_after_ms(&self, now_ms: u32) -> u32 {
        match self.last {
            Some(last) => {
                let since = now_ms.wrapping_sub(last);
                self.interval_ms.saturating_sub(since)
            }
            None => 0,
        }
    }

    /// Forget the last call, so the next one is allowed whenever it comes.
    pub fn reset(&mut self) {
        self.last = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_path_starts_with_the_prefix_and_is_listed_once_per_method() {
        for (i, a) in ROUTES.iter().enumerate() {
            assert!(a.path.starts_with(PREFIX), "{}", a.path);
            for b in &ROUTES[i + 1..] {
                assert!(
                    a.path != b.path || a.method != b.method,
                    "{} listed twice",
                    a.path
                );
            }
        }
    }

    #[test]
    fn get_routes_have_no_body_and_post_routes_do_the_mutating() {
        for r in ROUTES {
            match r.method {
                Method::Get => {
                    assert_eq!(r.body, Body::None, "{}", r.path);
                    assert!(!r.mutating, "{}", r.path);
                    assert_eq!(r.max_request_len, 0, "{}", r.path);
                }
                Method::Post => assert!(r.mutating, "{}", r.path),
            }
        }
    }

    #[test]
    fn the_biggest_request_is_the_wifi_form() {
        assert_eq!(MAX_REQUEST_LEN, crate::form::MAX_FORM_LEN);
    }

    #[test]
    fn find_agrees_with_the_table_for_every_row() {
        for r in ROUTES {
            assert_eq!(find(r.path, r.method), Some(r), "{}", r.path);
            assert!(path_is_known(r.path));
        }
    }

    /// The whole point of the pair: a `405` is a known path and an unknown
    /// method, a `404` is neither.
    #[test]
    fn a_wrong_method_is_not_an_unknown_path() {
        // `GET /api/v1/status` exists; `POST` of it does not.
        assert!(find(STATUS, Method::Post).is_none());
        assert!(path_is_known(STATUS));
        // `/api/v1/wifi` is the one path that has both.
        assert!(find(WIFI, Method::Get).is_some());
        assert!(find(WIFI, Method::Post).is_some());
        // Nothing at all.
        for p in ["/api/v1/nope", "/status", "", "/api/v1/status/"] {
            assert!(!path_is_known(p), "{p:?}");
            assert!(find(p, Method::Get).is_none(), "{p:?}");
            assert!(find(p, Method::Post).is_none(), "{p:?}");
        }
    }

    #[test]
    fn the_first_call_is_always_allowed_and_the_next_one_waits() {
        let mut limit = RateLimit::new(SCAN_MIN_INTERVAL_MS);
        assert_eq!(limit.interval_ms(), 10_000);
        assert_eq!(limit.retry_after_ms(0), 0, "nothing has happened yet");
        assert!(
            limit.allow(0),
            "a clock that really is at zero is not special"
        );
        assert!(!limit.allow(0));
        assert!(!limit.allow(9_999));
        assert_eq!(limit.retry_after_ms(9_999), 1);
        assert!(limit.allow(10_000));
        assert_eq!(limit.retry_after_ms(10_000), 10_000);
    }

    /// Card 221's answer, applied here: polling must not hold the window open.
    #[test]
    fn a_refused_call_does_not_push_the_window_out() {
        let mut limit = RateLimit::new(100);
        assert!(limit.allow(1_000));
        for t in 1_001..1_100 {
            assert!(!limit.allow(t), "{t}");
        }
        assert!(limit.allow(1_100), "still 100 ms after the allowed one");
    }

    /// 49.7 days in, `now_ms` wraps. Wrapping arithmetic means the window is
    /// measured correctly straight through it.
    #[test]
    fn the_window_survives_the_u32_wrap() {
        let mut limit = RateLimit::new(SCAN_MIN_INTERVAL_MS);
        let last = u32::MAX - 5_000;
        assert!(limit.allow(last));
        // 5 s before the wrap and 4 999 ms after it: both inside the window.
        assert!(!limit.allow(u32::MAX));
        assert!(
            !limit.allow(4_998),
            "wrapped, but only 9 999 ms have passed"
        );
        assert_eq!(limit.retry_after_ms(4_998), 1);
        // One more millisecond and the window is open again.
        assert!(limit.allow(4_999));
        // ...and the new `last` is the wrapped value, so the next window is
        // measured from there.
        assert!(!limit.allow(14_998));
        assert!(limit.allow(14_999));
    }

    #[test]
    fn a_reset_opens_the_window_at_once() {
        let mut limit = RateLimit::new(SCAN_MIN_INTERVAL_MS);
        assert!(limit.allow(1_000));
        assert!(!limit.allow(1_001));
        limit.reset();
        assert!(limit.allow(1_001));
    }

    /// A zero interval is a limiter that limits nothing, not one that blocks
    /// everything.
    #[test]
    fn a_zero_interval_allows_everything() {
        let mut limit = RateLimit::new(0);
        for t in 0..5 {
            assert!(limit.allow(t));
            assert_eq!(limit.retry_after_ms(t), 0);
        }
    }
}
