//! One error shape, one closed set of codes, one HTTP status per code.
//!
//! Every route that can fail answers `{"error":"<code>"}` or
//! `{"error":"<code>","detail":"<short text>"}` and nothing else. The status
//! is a property of the code, named right next to it, so the firmware, the
//! simulator and the Studio cannot disagree about which 400 is which.

use core::fmt;

use screeny_proto::control::ErrorCode as ProtoError;
use serde::{Deserialize, Serialize};

use crate::text::{text, DetailText, MAX_DETAIL_LEN};

/// Why a request was refused.
///
/// Deliberately small and generic: the machine reads the code, the human reads
/// [`ErrorReply::detail`]. Growing this set is an API change; putting a new
/// sentence in `detail` is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// Malformed: not parseable as the route's body type at all.
    BadRequest,
    /// The body is urlencoded but not a well-formed form (see
    /// [`crate::form::FormError`]).
    BadForm,
    /// The body is not well-formed JSON, or has the wrong types.
    BadJson,
    /// A value parsed but is outside its allowed range or length.
    OutOfRange,
    /// No such route.
    NotFound,
    /// That route exists but not with this method.
    MethodNotAllowed,
    /// The body is longer than the route accepts.
    PayloadTooLarge,
    /// Something else holds the resource: an upload is in flight, or a sender
    /// holds the panel lock.
    Busy,
    /// A rate limit: the scan is one per 10 s.
    RateLimited,
    /// A PIN is required and was absent or wrong. Reserved for parked card
    /// 041; nothing returns this today (decision 3).
    Unauthorized,
    /// Understood, allowed to ask, refused anyway.
    Forbidden,
    /// A flash read or write failed.
    Storage,
    /// The radio refused.
    Wifi,
    /// The feature is not available in this state: firmware upload from the
    /// captive portal, for instance (research 007 section 4.4).
    Unavailable,
    /// The device broke. A bug here, not there.
    Internal,
}

impl ErrorCode {
    /// The HTTP status that goes with this code.
    ///
    /// This is the constant card 222 writes into the response; it is not a
    /// suggestion, because the Studio switches on the code and a browser
    /// switches on the status and they have to agree.
    #[must_use]
    pub const fn status(self) -> u16 {
        match self {
            ErrorCode::BadRequest
            | ErrorCode::BadForm
            | ErrorCode::BadJson
            | ErrorCode::OutOfRange => 400,
            ErrorCode::Unauthorized => 401,
            ErrorCode::Forbidden => 403,
            ErrorCode::NotFound => 404,
            ErrorCode::MethodNotAllowed => 405,
            ErrorCode::Busy => 409,
            ErrorCode::PayloadTooLarge => 413,
            ErrorCode::RateLimited => 429,
            ErrorCode::Storage | ErrorCode::Wifi | ErrorCode::Internal => 500,
            ErrorCode::Unavailable => 503,
        }
    }

    /// The code as it appears in JSON, without the quotes.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            ErrorCode::BadRequest => "bad_request",
            ErrorCode::BadForm => "bad_form",
            ErrorCode::BadJson => "bad_json",
            ErrorCode::OutOfRange => "out_of_range",
            ErrorCode::NotFound => "not_found",
            ErrorCode::MethodNotAllowed => "method_not_allowed",
            ErrorCode::PayloadTooLarge => "payload_too_large",
            ErrorCode::Busy => "busy",
            ErrorCode::RateLimited => "rate_limited",
            ErrorCode::Unauthorized => "unauthorized",
            ErrorCode::Forbidden => "forbidden",
            ErrorCode::Storage => "storage",
            ErrorCode::Wifi => "wifi",
            ErrorCode::Unavailable => "unavailable",
            ErrorCode::Internal => "internal",
        }
    }

    /// Every code, for the tests and for the README's table.
    pub const ALL: &'static [ErrorCode] = &[
        ErrorCode::BadRequest,
        ErrorCode::BadForm,
        ErrorCode::BadJson,
        ErrorCode::OutOfRange,
        ErrorCode::NotFound,
        ErrorCode::MethodNotAllowed,
        ErrorCode::PayloadTooLarge,
        ErrorCode::Busy,
        ErrorCode::RateLimited,
        ErrorCode::Unauthorized,
        ErrorCode::Forbidden,
        ErrorCode::Storage,
        ErrorCode::Wifi,
        ErrorCode::Unavailable,
        ErrorCode::Internal,
    ];

    pub(crate) const MAX_JSON_LEN: usize = 2 + "method_not_allowed".len();
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(feature = "std")]
impl std::error::Error for ErrorCode {}

/// The UDP control protocol's errors, mapped onto this API's.
///
/// The same refusal reaches a sender over UDP and a browser over HTTP, so it
/// should have the same name in both places.
impl From<ProtoError> for ErrorCode {
    fn from(e: ProtoError) -> Self {
        match e {
            ProtoError::BadLength | ProtoError::Version => ErrorCode::BadRequest,
            ProtoError::UnknownOp => ErrorCode::NotFound,
            ProtoError::Busy => ErrorCode::Busy,
            ProtoError::BadArg => ErrorCode::OutOfRange,
            ProtoError::Storage => ErrorCode::Storage,
            ProtoError::Wifi => ErrorCode::Wifi,
            ProtoError::NotPermitted => ErrorCode::Forbidden,
            ProtoError::RateLimited => ErrorCode::RateLimited,
        }
    }
}

/// The body of every failed request, on every route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorReply {
    /// What went wrong, for the program.
    pub error: ErrorCode,
    /// A short sentence, for the person. Absent, not `null`, when there is
    /// nothing useful to add.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<DetailText>,
}

impl ErrorReply {
    /// `{"error":"method_not_allowed","detail":"<48 characters>"}`.
    pub const MAX_JSON_LEN: usize = 1                       // {
        + "\"error\":".len() + ErrorCode::MAX_JSON_LEN
        + 1                                                 // ,
        + "\"detail\":".len() + 2 + MAX_DETAIL_LEN * crate::ESCAPE_MAX
        + 1; // }

    /// An error with no sentence attached.
    #[must_use]
    pub const fn new(error: ErrorCode) -> Self {
        Self {
            error,
            detail: None,
        }
    }

    /// An error with a short sentence. A `detail` that does not fit is
    /// dropped rather than truncated: half a sentence is worse than none.
    #[must_use]
    pub fn with_detail(error: ErrorCode, detail: &str) -> Self {
        Self {
            error,
            detail: text(detail),
        }
    }

    /// The HTTP status to send this with.
    #[must_use]
    pub const fn status(&self) -> u16 {
        self.error.status()
    }
}

impl fmt::Display for ErrorReply {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.detail {
            Some(d) => write!(f, "{} ({})", self.error, d),
            None => write!(f, "{}", self.error),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for ErrorReply {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::format;

    #[test]
    fn every_code_has_a_plausible_status_and_its_own_string() {
        for (i, &a) in ErrorCode::ALL.iter().enumerate() {
            assert!((400..=599).contains(&a.status()), "{a} -> {}", a.status());
            // The serde name and `as_str` are the same string.
            assert_eq!(serde_json::to_string(&a).unwrap(), format!("\"{a}\""));
            for &b in &ErrorCode::ALL[i + 1..] {
                assert_ne!(a.as_str(), b.as_str());
            }
        }
    }

    #[test]
    fn detail_that_does_not_fit_is_dropped_not_truncated() {
        let long = "x".repeat(MAX_DETAIL_LEN + 1);
        assert_eq!(
            ErrorReply::with_detail(ErrorCode::BadForm, &long).detail,
            None
        );
        let fits = "x".repeat(MAX_DETAIL_LEN);
        assert_eq!(
            ErrorReply::with_detail(ErrorCode::BadForm, &fits)
                .detail
                .unwrap()
                .len(),
            MAX_DETAIL_LEN
        );
    }
}
