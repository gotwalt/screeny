//! One `GET /api/v1/status` on one device, over TCP, with a deadline.
//!
//! # Why this is hand-written
//!
//! It is one request, on one route, against a server with **one connection
//! worker and no listen backlog**: a second simultaneous connection is dropped
//! at SYN and retried by the OS a second later (firmware card 222). What that
//! device needs of a client is the opposite of what an HTTP client crate is
//! for - no pool, no keep-alive, no retries of its own, no redirects, no
//! chunked decoding, one connection at a time and a hard ceiling on how long
//! the whole thing may take. Forty lines of `std::net` say that exactly;
//! `reqwest` would add ~90 crates to an image that builds on a slow path and
//! would still have to be argued out of every one of those behaviours.
//!
//! The precedent is already in the tree twice: `crates/sim/src/http.rs` is a
//! hand-written server for the same reason, and `crates/studio/tests/common`
//! is a hand-written client.
//!
//! # What it promises
//!
//! * **Blocking.** Call it from `spawn_blocking`, never from a render thread.
//! * **One connection, `Connection: close`, no reuse.** The caller ([`crate::fleet`])
//!   runs exactly one of these at a time across every device.
//! * **Bounded by construction.** [`TIMEOUT`] caps connect, write and read
//!   together, not each separately; a reply longer than [`MAX_REPLY`] is
//!   refused rather than read.
//! * **The SSID in the answer is never logged.** [`Fault`] carries no payload
//!   bytes, and [`crate::devices::DeviceFacts`] redacts its SSID in `Debug`.
//! * **It says what it cost** (card 164). [`get_status_counted`] hands back the
//!   bytes written and the bytes read, *however the read went* - a request that
//!   timed out still went out, and a reply that turned out to be somebody
//!   else's web server still came down the wire. Bytes on the socket and
//!   nothing more: TCP's retransmissions, its ACKs and its handshake are
//!   invisible from user space, so no overhead is guessed at here.

use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::{Duration, Instant};

use screeny_device_api::reply::StatusReply;

/// The port the device serves its HTTP API on (`docs/design/device-web.md`).
/// Overridable per device and per studio, which is how a simulator - which
/// cannot bind 80 without root - is talked to.
pub const DEFAULT_PORT: u16 = 80;

/// How long **the whole request** may take: connect, write and read together.
///
/// The probe's rule 31 measures a lone request on the real device at 25-37 ms,
/// and inside 1 s when the one connection worker is already busy. Two seconds
/// is that worst case with room to spare, and still far below the 10 s poll
/// period, so a device that has gone away can never make the poller late.
pub const TIMEOUT: Duration = Duration::from_secs(2);

/// The most of a reply that will be read before it is refused.
///
/// `StatusReply::MAX_JSON_LEN` is under 1 KB even with every text field
/// escaping to six bytes a character, so 4 KB is generous for the body and the
/// head together - and it means a wrong address that happens to be a web
/// server cannot pull a page into this process.
pub const MAX_REPLY: usize = 4 * 1024;

/// Why a read did not produce a status.
///
/// Neither kind is ever a fault in `/healthz`: a panel with no HTTP server is
/// normal (older firmware, or the portable profile pointed at a simulator
/// started with `--no-http`), and a panel that is switched off is normal life.
#[derive(Clone, Debug)]
pub struct Fault {
    /// True when the answer means **this device does not serve the API**:
    /// nothing is listening on the port, or what answered is not this API. The
    /// Studio says that once per device and then falls silent.
    pub absent: bool,
    /// What happened, for that one line. Never carries reply bytes.
    pub why: String,
}

impl Fault {
    fn absent(why: impl Into<String>) -> Self {
        Fault { absent: true, why: why.into() }
    }

    fn reached(why: impl Into<String>) -> Self {
        Fault { absent: false, why: why.into() }
    }
}

impl std::fmt::Display for Fault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.why)
    }
}

/// What one read put on the socket and took off it (card 164).
///
/// Bytes only, and only the ones this process handed to `write` or got back
/// from `read`. There is no packet count because there is no packet: a stream
/// socket does not tell user space how the bytes were carried.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cost {
    /// Request bytes written.
    pub out: u64,
    /// Reply bytes read, head and body together.
    pub inbound: u64,
}

/// Read `GET /api/v1/status` from the device at `addr`.
///
/// Blocking, and bounded by `patience` end to end.
///
/// # Errors
///
/// [`Fault`], which says whether the device simply has no HTTP server.
pub fn get_status(addr: SocketAddr, patience: Duration) -> Result<StatusReply, Fault> {
    get_status_counted(addr, patience).1
}

/// [`get_status`], saying what it cost on the wire (card 164).
///
/// The [`Cost`] comes back whether the read worked or not, which is the point:
/// a panel that is refusing connections costs a few bytes every ten seconds,
/// and a studio that could not say so would be under-reporting exactly the
/// case somebody is looking at the page about.
pub fn get_status_counted(addr: SocketAddr, patience: Duration) -> (Cost, Result<StatusReply, Fault>) {
    let mut cost = Cost::default();
    let out = read_status(addr, patience, &mut cost);
    (cost, out)
}

fn read_status(addr: SocketAddr, patience: Duration, cost: &mut Cost) -> Result<StatusReply, Fault> {
    let started = Instant::now();
    let left = || patience.checked_sub(started.elapsed()).filter(|d| !d.is_zero());

    let mut stream = TcpStream::connect_timeout(&addr, patience).map_err(|e| match e.kind() {
        // Nothing is listening there. On a LAN a device that is *off* answers
        // this way too, which is why it is said once and then dropped.
        ErrorKind::ConnectionRefused => Fault::absent("nothing is listening on its HTTP port"),
        ErrorKind::TimedOut => Fault::reached("timed out connecting to its HTTP port"),
        _ => Fault::reached(format!("connecting to its HTTP port: {e}")),
    })?;
    // Nagle would hold the request head back waiting for more to send; there
    // is no more, and the device is waiting for it.
    let _ = stream.set_nodelay(true);

    let head = format!(
        "GET {path} HTTP/1.1\r\nHost: {addr}\r\nAccept: application/json\r\n\
         User-Agent: screeny-studio/{version}\r\nConnection: close\r\n\r\n",
        path = screeny_device_api::route::STATUS,
        version = env!("CARGO_PKG_VERSION"),
    );
    let deadline = left().ok_or_else(|| Fault::reached("ran out of time connecting"))?;
    stream.set_write_timeout(Some(deadline)).map_err(|e| Fault::reached(e.to_string()))?;
    // Counted before the write is attempted: a head that went halfway out
    // still went halfway out, and the honest figure is the one that does not
    // depend on the far end having been polite about it.
    cost.out += head.len() as u64;
    stream.write_all(head.as_bytes()).map_err(|e| Fault::reached(format!("asking for its status: {e}")))?;

    let raw = read_reply(&mut stream, &left, cost)?;
    let (status, body) = split_reply(&raw)?;
    if status != 200 {
        // A 404 is something else's web server on that address; anything else
        // is a device that is unhappy rather than absent.
        return Err(if status == 404 {
            Fault::absent(format!("its HTTP port answered {status} for {}", screeny_device_api::route::STATUS))
        } else {
            Fault::reached(format!("its status route answered {status}"))
        });
    }
    // The error is serde's, which names a field and an offset - never a value,
    // so no SSID can reach a log line through here.
    serde_json::from_slice(body).map_err(|e| Fault::absent(format!("its status route did not answer this API: {e}")))
}

/// Read until the server closes, or until `Content-Length` is satisfied, or
/// until [`MAX_REPLY`] - whichever comes first.
fn read_reply(stream: &mut TcpStream, left: &dyn Fn() -> Option<Duration>, cost: &mut Cost) -> Result<Vec<u8>, Fault> {
    let mut raw: Vec<u8> = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        let deadline = left().ok_or_else(|| Fault::reached("timed out reading its status"))?;
        stream.set_read_timeout(Some(deadline)).map_err(|e| Fault::reached(e.to_string()))?;
        let n = match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {
                return Err(Fault::reached("timed out reading its status"));
            }
            Err(e) => return Err(Fault::reached(format!("reading its status: {e}"))),
        };
        raw.extend_from_slice(&chunk[..n]);
        // Every byte read, including the ones of a reply that is about to be
        // refused for being too long: they crossed the network either way.
        cost.inbound += n as u64;
        if raw.len() > MAX_REPLY {
            return Err(Fault::absent("its HTTP port sent more than a status reply can be"));
        }
        // A server that did not honour `Connection: close` would otherwise
        // hold this open until the deadline, once per poll, for ever.
        if complete(&raw) {
            break;
        }
    }
    Ok(raw)
}

/// True once the head has arrived and the body is as long as the head said.
fn complete(raw: &[u8]) -> bool {
    let Some(end) = find_head_end(raw) else { return false };
    let head = String::from_utf8_lossy(&raw[..end]);
    let Some(len) = content_length(&head) else { return false };
    raw.len() - (end + 4) >= len
}

fn find_head_end(raw: &[u8]) -> Option<usize> {
    raw.windows(4).position(|w| w == b"\r\n\r\n")
}

fn content_length(head: &str) -> Option<usize> {
    head.lines()
        .find_map(|l| l.split_once(':').filter(|(k, _)| k.trim().eq_ignore_ascii_case("content-length")))
        .and_then(|(_, v)| v.trim().parse().ok())
}

/// `(status, body)` from a whole reply.
fn split_reply(raw: &[u8]) -> Result<(u16, &[u8]), Fault> {
    if raw.is_empty() {
        return Err(Fault::absent("its HTTP port accepted the connection and said nothing"));
    }
    let end = find_head_end(raw).ok_or_else(|| Fault::absent("its HTTP port sent no complete reply"))?;
    let head = String::from_utf8_lossy(&raw[..end]);
    let status = head
        .lines()
        .next()
        .filter(|l| l.starts_with("HTTP/1."))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| Fault::absent("its HTTP port did not answer HTTP"))?;
    Ok((status, &raw[end + 4..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reply_is_split_into_its_status_and_its_body() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}";
        assert_eq!(split_reply(raw).expect("split"), (200, &b"{}"[..]));
        assert!(complete(raw));
        assert!(!complete(b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\n{}"));
        // No Content-Length: the only end is the server closing, which is what
        // `Connection: close` is asked for.
        assert!(!complete(b"HTTP/1.1 200 OK\r\n\r\n{}"));
    }

    #[test]
    fn something_that_is_not_this_api_is_absent_rather_than_a_fault() {
        assert!(split_reply(b"<html>hello</html>\r\n\r\n").expect_err("not HTTP").absent);
        assert!(split_reply(b"").expect_err("nothing").absent);
        assert!(split_reply(b"HTTP/1.1 200 OK\r\nno end of head").expect_err("truncated").absent);
    }

    /// Nothing listening is "this device has no HTTP server", not an error to
    /// put on the page - and it is answered inside the deadline rather than
    /// after it.
    #[test]
    fn a_port_with_nothing_on_it_is_absent() {
        // A port nobody is on: bind one, learn its number, and drop it.
        let taken = std::net::TcpListener::bind("127.0.0.1:0").expect("a port");
        let addr = taken.local_addr().expect("its address");
        drop(taken);
        let started = Instant::now();
        let fault = get_status(addr, TIMEOUT).expect_err("nothing is there");
        assert!(fault.absent, "{fault}");
        assert!(started.elapsed() < TIMEOUT, "a refused connection should not wait out the deadline");
    }
}
