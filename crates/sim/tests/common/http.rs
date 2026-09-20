//! A blocking HTTP client small enough to be obviously correct.
//!
//! Deliberately not a crate, and deliberately *not* sharing code with
//! `screeny_sim::http`: a test written with the server's own parser would
//! prove only that the simulator agrees with itself. This one is written from
//! RFC 9112 the way `curl` would do it, and every response it parses is one a
//! browser would have to parse too.
//!
//! Every call has a timeout. Nothing here can hang a test.

#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

/// How long any one request may take.
pub const TIMEOUT: Duration = Duration::from_secs(5);

/// A parsed response.
#[derive(Debug, Clone)]
pub struct Res {
    /// The status code.
    pub status: u16,
    /// Headers, names lowercased.
    pub headers: Vec<(String, String)>,
    /// The body, as bytes.
    pub body: Vec<u8>,
}

impl Res {
    /// The body as text. Panics if it is not UTF-8, which for this API is a
    /// test failure rather than a condition to handle.
    #[must_use]
    pub fn text(&self) -> String {
        String::from_utf8(self.body.clone()).expect("the body is UTF-8")
    }

    /// The body parsed as JSON.
    #[must_use]
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("not JSON: {e}: {:?}", self.text()))
    }

    /// The body parsed as one of `screeny_device_api`'s types. This is the
    /// assertion that matters: the simulator's bytes are what the firmware's
    /// consumers will parse.
    #[must_use]
    pub fn parse<T: serde::de::DeserializeOwned>(&self) -> T {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("does not parse as the API type: {e}: {:?}", self.text()))
    }

    /// The `error` field of an error reply, as a string.
    #[must_use]
    pub fn error_code(&self) -> String {
        self.json()["error"]
            .as_str()
            .unwrap_or_else(|| panic!("no error field in {:?}", self.text()))
            .to_string()
    }

    /// One header's value.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

/// `GET path`, with `Host` set to the server's own address.
#[must_use]
pub fn get(addr: SocketAddr, path: &str) -> Res {
    request(addr, "GET", path, &addr.to_string(), None, &[])
}

/// `GET path` with a `Host` of your choosing: the captive-portal probes.
#[must_use]
pub fn get_with_host(addr: SocketAddr, path: &str, host: &str) -> Res {
    request(addr, "GET", path, host, None, &[])
}

/// `POST path` with a JSON body.
#[must_use]
pub fn post_json(addr: SocketAddr, path: &str, body: &str) -> Res {
    request(
        addr,
        "POST",
        path,
        &addr.to_string(),
        Some("application/json"),
        body.as_bytes(),
    )
}

/// `POST path` with an urlencoded form body.
#[must_use]
pub fn post_form(addr: SocketAddr, path: &str, body: &str) -> Res {
    request(
        addr,
        "POST",
        path,
        &addr.to_string(),
        Some("application/x-www-form-urlencoded"),
        body.as_bytes(),
    )
}

/// `POST path` with a raw octet-stream body.
#[must_use]
pub fn post_bytes(addr: SocketAddr, path: &str, body: &[u8]) -> Res {
    request(
        addr,
        "POST",
        path,
        &addr.to_string(),
        Some("application/octet-stream"),
        body,
    )
}

/// One request, spelled out.
#[must_use]
pub fn request(
    addr: SocketAddr,
    method: &str,
    path: &str,
    host: &str,
    content_type: Option<&str>,
    body: &[u8],
) -> Res {
    let mut head = format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\n");
    if let Some(ct) = content_type {
        head.push_str(&format!("Content-Type: {ct}\r\n"));
    }
    if !body.is_empty() || method == "POST" {
        head.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    head.push_str("Connection: close\r\n\r\n");
    let mut wire = head.into_bytes();
    wire.extend_from_slice(body);
    parse(&raw(addr, &wire))
}

/// Send bytes, read everything back. For the malformed-request tests.
#[must_use]
pub fn raw(addr: SocketAddr, bytes: &[u8]) -> Vec<u8> {
    let mut s = TcpStream::connect_timeout(&addr, TIMEOUT).expect("connect");
    s.set_read_timeout(Some(TIMEOUT)).unwrap();
    s.set_write_timeout(Some(TIMEOUT)).unwrap();
    s.write_all(bytes).expect("write request");
    s.flush().unwrap();
    let mut out = Vec::new();
    // `Connection: close` on every reply, so reading to EOF is the whole
    // response and cannot block past the timeout.
    let _ = s.read_to_end(&mut out);
    out
}

/// Parse a whole response off the wire.
#[must_use]
pub fn parse(wire: &[u8]) -> Res {
    let split = wire
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .unwrap_or_else(|| panic!("no header terminator in {:?}", String::from_utf8_lossy(wire)));
    let head = String::from_utf8_lossy(&wire[..split]).to_string();
    let body = wire[split + 4..].to_vec();

    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("no status in {status_line:?}"));
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();

    // Every reply this API sends carries a Content-Length and closes, so the
    // body is simply what came after the head.
    Res {
        status,
        headers,
        body,
    }
}
