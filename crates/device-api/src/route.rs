//! The route table, as constants and as data.
//!
//! Card 222 routes on the constants (`picoserve::Router::route(route::STATUS,
//! ...)`), card 224 does the same in the simulator, and the Studio builds URLs
//! from them. [`ROUTES`] is the same table as data, so a test can prove the
//! server covers it and the README can be checked against it.

use crate::reply::{
    AcceptedReply, FirmwareReply, NetworksReply, SettingsReply, StatusReply, TelemetryReply,
    WifiReply,
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
/// `GET /api/v1/networks`: [`NetworksReply`]. Triggers a scan; rate-limited to
/// one per 10 s, and a caller that asks sooner gets
/// [`ErrorCode::RateLimited`](crate::ErrorCode::RateLimited).
pub const NETWORKS: &str = "/api/v1/networks";
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
}
