//! A blocking HTTP/1.1 client, small enough to read in one sitting.
//!
//! # Why this and not a crate
//!
//! `crates/probe` is a dev-dependency of `crates/sim`, which is a
//! dev-dependency of the Studio, so whatever this links everybody compiles.
//! What the device's API needs of a client is: `GET` and `POST`, a
//! `Content-Length` body, a `Host` header of our choosing, the exact status
//! code, the headers, and a timeout on every step. That is this file. Every
//! HTTP client crate worth the name brings an async runtime, a TLS stack or
//! both, for none of that.
//!
//! It is also deliberately **not** shared with `screeny_sim::http`, the
//! simulator's server, or with `crates/sim/tests/common/http.rs`: a
//! conformance suite written with the server's own parser would prove only
//! that the simulator agrees with itself. This is written from RFC 9112 the
//! way `curl` would do it.
//!
//! # What the device's server forces on it
//!
//! Card 222's firmware serves HTTP from **one** connection worker with no
//! listen backlog, and smoltcp drops the SYN of a second connection rather
//! than queueing it; macOS retransmits it a second later. So:
//!
//! * one connection at a time, never two - this client has no pool and no
//!   threads, so that is true by construction;
//! * `Connection: close` on every request, one request per connection;
//! * a connect timeout far longer than that 1 s retransmit, so a *slow*
//!   connect is never read as a failure;
//! * a **refused** connect is retried and counted (card 236). Card 227's
//!   second worker made the dropped-SYN case rare and turned it into the
//!   refused-SYN case instead: two workers busy is a RST, not silence, and the
//!   client learns immediately. See [`CONNECT_TRIES`].

use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long to wait for the TCP handshake.
///
/// Generously longer than the 1 s SYN retransmit a busy one-worker firmware
/// costs (card 222): waiting a second is the device behaving, not failing.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);

/// How many times to try the TCP handshake when it is **refused**.
///
/// A refusal is not the same failure as a timeout, and card 236 is why it has
/// its own handling. smoltcp has no listen backlog: a SYN that arrives while
/// neither of the device's two HTTP workers is in `accept` is answered with a
/// RST rather than queued, and the client sees `Connection refused` at once -
/// macOS does not retransmit a SYN that was *answered*, the way it does one
/// that was dropped. There is nothing wrong with the device in that moment; it
/// is simply busy, and a suite that reports it as a failure is measuring the
/// bench's timing rather than the firmware's conformance.
///
/// So the connect is retried - and **counted**, which is the more important
/// half. [`Client::connect_refusals`] is reported in the suite's last line, so
/// "the run passed" and "the device refused eleven connections on the way" are
/// two facts the orchestrator sees rather than one that hides the other.
pub const CONNECT_TRIES: usize = 3;

/// How long to wait between a refused connect and the next try.
///
/// Long enough for a worker that was closing to finish and call `accept`
/// (which, after card 236's firmware, is bounded by the device); short enough
/// that three tries add at most 200 ms to a rule.
pub const CONNECT_RETRY_DELAY: Duration = Duration::from_millis(100);

/// How long a request may be silent once connected.
pub const IO_TIMEOUT: Duration = Duration::from_secs(10);

/// The largest response this client will read. `GET /` is ~6 KB on the device
/// and the biggest JSON reply is `NetworksReply`'s 3.7 KB, so anything past
/// this is a server that will not stop talking.
pub const MAX_RESPONSE_LEN: usize = 1 << 20;

/// A parsed response.
#[derive(Debug, Clone)]
pub struct Res {
    /// The status code.
    pub status: u16,
    /// Headers, names lowercased, in the order they arrived.
    pub headers: Vec<(String, String)>,
    /// The body.
    pub body: Vec<u8>,
    /// How long the whole request took, connect included.
    pub elapsed: Duration,
}

impl Res {
    /// One header's value.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// The `Content-Type`, without any parameters, lowercased.
    #[must_use]
    pub fn content_type(&self) -> String {
        self.header("content-type")
            .unwrap_or("")
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase()
    }

    /// True when the reply says it is JSON.
    #[must_use]
    pub fn is_json(&self) -> bool {
        self.content_type() == "application/json"
    }

    /// The body as text, lossily. For a detail line, never for a decision.
    #[must_use]
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// The first `n` characters of the body, for a one-line detail.
    #[must_use]
    pub fn snippet(&self, n: usize) -> String {
        let t = self.text();
        let t = t.replace(['\n', '\r'], " ");
        if t.chars().count() <= n {
            t
        } else {
            let head: String = t.chars().take(n).collect();
            format!("{head}...")
        }
    }

    /// The body parsed as one of `screeny_device_api`'s types.
    ///
    /// This is the assertion that matters: the suite defines no shapes of its
    /// own, so "it parses" means the Studio and the firmware agree about it.
    ///
    /// # Errors
    /// The parse error and what the body said, as one line.
    pub fn parse<T: serde::de::DeserializeOwned>(&self) -> Result<T, String> {
        serde_json::from_slice(&self.body)
            .map_err(|e| format!("does not parse as the API type: {e}: {}", self.snippet(120)))
    }

    /// The body parsed as the API's one error shape.
    ///
    /// # Errors
    /// When the body is not `{"error":...}`, or when the status does not
    /// match the code's own status - which is the same failure, because the
    /// status is a property of the code (`crates/device-api/src/error.rs`).
    pub fn error(&self) -> Result<screeny_device_api::ErrorReply, String> {
        let reply: screeny_device_api::ErrorReply = self.parse()?;
        if reply.status() != self.status {
            return Err(format!(
                "{} came back with HTTP {}, but that code's status is {}",
                reply.error,
                self.status,
                reply.status()
            ));
        }
        Ok(reply)
    }
}

/// Where the requests go, and what `Host` they carry.
#[derive(Debug, Clone)]
pub struct Client {
    addr: SocketAddr,
    host: String,
    /// How many connects this client has had **refused** (card 236).
    ///
    /// Shared between clones on purpose: the suite hands a clone to its restore
    /// guard, and the number the last line reports has to be the run's, not one
    /// arbitrary copy's.
    refusals: Arc<AtomicU32>,
}

impl Client {
    /// A client for one device.
    ///
    /// `host` is what goes in the `Host` header; the device's captive-portal
    /// catch-all answers on a `Host` that is its own name or any bare IP
    /// (research 007 section 4.3), so this is normally just the address.
    #[must_use]
    pub fn new(addr: SocketAddr, host: impl Into<String>) -> Self {
        Client {
            addr,
            host: host.into(),
            refusals: Arc::new(AtomicU32::new(0)),
        }
    }

    /// How many connects were refused, over every clone of this client.
    ///
    /// Counts every refusal, including the ones a retry recovered from - that
    /// is the number card 236 wants visible. A rule whose [`CONNECT_TRIES`] are
    /// all refused still fails, and contributes all of them to this count.
    #[must_use]
    pub fn connect_refusals(&self) -> u32 {
        self.refusals.load(Ordering::Relaxed)
    }

    /// One TCP connection, retrying a **refusal** and nothing else.
    ///
    /// A timeout, an unreachable host or a reset are all left to fail on the
    /// first try: they say something is wrong. Only `ECONNREFUSED` means "the
    /// server is there and had no worker free", which is a wait, not a fault.
    fn connect(&self) -> Result<TcpStream, String> {
        let mut last = String::new();
        for try_n in 1..=CONNECT_TRIES {
            match TcpStream::connect_timeout(&self.addr, CONNECT_TIMEOUT) {
                Ok(s) => return Ok(s),
                Err(e) if e.kind() == ErrorKind::ConnectionRefused => {
                    self.refusals.fetch_add(1, Ordering::Relaxed);
                    last = format!("connect {}: {e} (refused, try {try_n}/{CONNECT_TRIES})", self.addr);
                    if try_n < CONNECT_TRIES {
                        std::thread::sleep(CONNECT_RETRY_DELAY);
                    }
                }
                Err(e) => return Err(format!("connect {}: {e}", self.addr)),
            }
        }
        Err(last)
    }

    /// The address requests go to.
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The `Host` header value.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// `GET path`.
    ///
    /// # Errors
    /// Anything that stops a response coming back.
    pub fn get(&self, path: &str) -> Result<Res, String> {
        self.request("GET", path, None, &[])
    }

    /// `POST path` with a JSON body.
    ///
    /// # Errors
    /// Anything that stops a response coming back.
    pub fn post_json(&self, path: &str, body: &str) -> Result<Res, String> {
        self.request("POST", path, Some("application/json"), body.as_bytes())
    }

    /// `POST path` with an urlencoded form body.
    ///
    /// # Errors
    /// Anything that stops a response coming back.
    pub fn post_form(&self, path: &str, body: &str) -> Result<Res, String> {
        self.request(
            "POST",
            path,
            Some("application/x-www-form-urlencoded"),
            body.as_bytes(),
        )
    }

    /// `POST path` with a raw octet-stream body.
    ///
    /// # Errors
    /// Anything that stops a response coming back.
    pub fn post_bytes(&self, path: &str, body: &[u8]) -> Result<Res, String> {
        self.request("POST", path, Some("application/octet-stream"), body)
    }

    /// One request, spelled out. Any method, including ones this API has no
    /// [`Method`](screeny_device_api::Method) for.
    ///
    /// # Errors
    /// Anything that stops a response coming back.
    pub fn request(
        &self,
        method: &str,
        path: &str,
        content_type: Option<&str>,
        body: &[u8],
    ) -> Result<Res, String> {
        let mut head = format!(
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n",
            self.host
        );
        if let Some(ct) = content_type {
            head.push_str(&format!("Content-Type: {ct}\r\n"));
        }
        // A `Content-Length` on every POST, even an empty one: a server with a
        // fixed buffer has no other way to know the body is over.
        if !body.is_empty() || method != "GET" {
            head.push_str(&format!("Content-Length: {}\r\n", body.len()));
        }
        head.push_str("\r\n");
        let mut wire = head.into_bytes();
        wire.extend_from_slice(body);
        let (bytes, elapsed) = self.exchange(&wire)?;
        let mut res = parse(&bytes)?;
        res.elapsed = elapsed;
        Ok(res)
    }

    /// Send bytes, read the whole response back. Public for the rules that
    /// need to say something a well-formed request cannot say.
    ///
    /// # Errors
    /// Anything that stops a response coming back.
    pub fn raw(&self, wire: &[u8]) -> Result<Vec<u8>, String> {
        self.exchange(wire).map(|(b, _)| b)
    }

    fn exchange(&self, wire: &[u8]) -> Result<(Vec<u8>, Duration), String> {
        let t0 = Instant::now();
        let mut s = self.connect()?;
        s.set_read_timeout(Some(IO_TIMEOUT))
            .and_then(|()| s.set_write_timeout(Some(IO_TIMEOUT)))
            .map_err(|e| format!("timeouts: {e}"))?;
        s.write_all(wire).map_err(|e| format!("write: {e}"))?;
        s.flush().map_err(|e| format!("flush: {e}"))?;

        // Read the head, then exactly as much body as `Content-Length` says -
        // or, when there is none, to EOF. Reading to EOF unconditionally would
        // turn a server that forgot to close into a ten-second stall.
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        let mut want: Option<usize> = None;
        loop {
            if let Some(total) = want {
                if buf.len() >= total {
                    break;
                }
            }
            let n = match s.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) => {
                    if buf.is_empty() {
                        return Err(format!("read: {e}"));
                    }
                    // Something came back; let the parser say what is wrong
                    // with it rather than reporting the timeout.
                    break;
                }
            };
            buf.extend_from_slice(&chunk[..n]);
            if buf.len() > MAX_RESPONSE_LEN {
                return Err(format!("response longer than {MAX_RESPONSE_LEN} bytes"));
            }
            if want.is_none() {
                if let Some(end) = head_end(&buf) {
                    want = content_length(&buf[..end]).map(|len| end + len);
                }
            }
        }
        Ok((buf, t0.elapsed()))
    }
}

/// The offset just past the `\r\n\r\n` that ends the head, if it is there yet.
fn head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}

/// `Content-Length` out of a head, if it parses.
fn content_length(head: &[u8]) -> Option<usize> {
    let head = String::from_utf8_lossy(head);
    head.split("\r\n")
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.trim().eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse().ok())
}

/// Parse a whole response off the wire.
///
/// # Errors
/// A response with no status line or no header terminator.
pub fn parse(wire: &[u8]) -> Result<Res, String> {
    if wire.is_empty() {
        return Err("the connection closed without a response".into());
    }
    let end = head_end(wire).ok_or_else(|| {
        format!(
            "no header terminator in {:?}",
            String::from_utf8_lossy(&wire[..wire.len().min(120)])
        )
    })?;
    let head = String::from_utf8_lossy(&wire[..end - 4]).into_owned();
    let body = wire[end..].to_vec();

    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| format!("no status code in {status_line:?}"))?;
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();

    Ok(Res {
        status,
        headers,
        body,
        elapsed: Duration::ZERO,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_response_is_a_status_lowercased_headers_and_the_bytes_after_the_head() {
        let res = parse(
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 9\r\n\r\n{\"api\":1}",
        )
        .unwrap();
        assert_eq!(res.status, 200);
        assert_eq!(res.header("content-type"), Some("application/json"));
        assert!(res.is_json());
        assert_eq!(res.text(), "{\"api\":1}");
    }

    #[test]
    fn a_content_type_with_parameters_is_still_recognised() {
        let res = parse(b"HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<p>")
            .unwrap();
        assert_eq!(res.content_type(), "text/html");
        assert!(!res.is_json());
    }

    #[test]
    fn a_truncated_response_is_an_error_and_not_a_panic() {
        assert!(parse(b"").is_err());
        assert!(parse(b"HTTP/1.1 200 OK\r\nContent-Type: x\r\n").is_err());
        assert!(parse(b"garbage\r\n\r\n").is_err());
    }

    #[test]
    fn the_error_shape_is_checked_against_its_own_status() {
        let ok = parse(
            b"HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\n\r\n{\"error\":\"not_found\"}",
        )
        .unwrap();
        assert_eq!(
            ok.error().unwrap().error,
            screeny_device_api::ErrorCode::NotFound
        );
        // The same body with the wrong status is a failure, not a parse.
        let wrong = parse(
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"error\":\"not_found\"}",
        )
        .unwrap();
        assert!(wrong.error().unwrap_err().contains("404"));
    }

    #[test]
    fn content_length_is_found_whatever_the_case() {
        assert_eq!(content_length(b"HTTP/1.1 200 OK\r\ncontent-length: 7"), Some(7));
        assert_eq!(content_length(b"HTTP/1.1 200 OK\r\nContent-Length:  7 "), Some(7));
        assert_eq!(content_length(b"HTTP/1.1 200 OK"), None);
    }

    /// Card 236: a refused connect is tried [`CONNECT_TRIES`] times, every one
    /// of them is counted, and the last error says so.
    ///
    /// The port is one a listener held and let go, which is the one way to make
    /// `ECONNREFUSED` happen on demand: nothing is listening, so the host's own
    /// stack answers the SYN with a RST.
    #[test]
    fn a_refused_connect_is_retried_and_every_refusal_is_counted() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a port");
        let addr = listener.local_addr().expect("its address");
        drop(listener);

        let c = Client::new(addr, addr.to_string());
        assert_eq!(c.connect_refusals(), 0);

        let t0 = Instant::now();
        let err = c.get("/api/v1/status").expect_err("nothing is listening");
        assert!(err.contains("refused"), "{err}");
        assert!(
            err.contains(&format!("{CONNECT_TRIES}/{CONNECT_TRIES}")),
            "the last try is named: {err}"
        );
        assert_eq!(c.connect_refusals(), CONNECT_TRIES as u32);
        assert!(
            t0.elapsed() >= CONNECT_RETRY_DELAY * (CONNECT_TRIES as u32 - 1),
            "the tries are spaced out"
        );

        // A clone shares the count, because the suite hands one to its restore
        // guard and the last line has to report the run's total.
        assert_eq!(c.clone().connect_refusals(), CONNECT_TRIES as u32);
    }

    #[test]
    fn a_snippet_is_one_line_and_bounded() {
        let res = Res {
            status: 200,
            headers: Vec::new(),
            body: b"a\r\nb".repeat(100).to_vec(),
            elapsed: Duration::ZERO,
        };
        let s = res.snippet(20);
        assert!(!s.contains('\n'));
        assert_eq!(s.chars().count(), 23, "20 characters and the ellipsis");
    }
}
