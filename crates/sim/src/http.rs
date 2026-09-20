//! A blocking HTTP/1.1 server, small enough to read in one sitting.
//!
//! # Why this and not a crate
//!
//! The simulator is threaded and has no async runtime, and it is
//! `crates/studio`'s dev-dependency, so whatever it links everybody links.
//! What the device's API needs of a server is: `GET` and `POST`, a
//! `Content-Length` body, one route whose body is streamed rather than
//! buffered, a `Host` header, and an exact status code. That is this file.
//! Taking `tiny_http` instead would add four transitive crates for the same
//! behaviour, and would hide the one thing the simulator exists to model
//! honestly - what the *firmware* can afford (card 222 serves this API on
//! picoserve, with no chunked encoding and no allocator).
//!
//! # What it deliberately does not do
//!
//! * **No keep-alive.** Every response says `Connection: close`. The device
//!   will do better; nothing that talks to the simulator cares, and a
//!   connection per request is one fewer state machine to get wrong.
//! * **No chunked request bodies.** `Transfer-Encoding: chunked` is refused
//!   with 400, which is what a `no_std` server on 4 KB of buffer will also do.
//! * **No TLS, no compression, no ranges, no cookies.**
//!
//! # Hygiene
//!
//! One acceptor thread, one short-lived thread per connection, every socket
//! with a read and a write timeout, and [`Server::shutdown`] joins all of
//! them. The simulator's tests assert that nothing is left behind, and this
//! follows that style rather than detaching threads and hoping.

use std::collections::BTreeMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// How long a connection may sit silent before it is dropped.
const IO_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the acceptor waits for a connection before re-checking the stop
/// flag.
const ACCEPT_POLL: Duration = Duration::from_millis(20);
/// The largest request head (request line plus headers) that will be read.
/// picoserve's whole buffer on the device is smaller than this.
pub const MAX_HEAD_LEN: usize = 8 * 1024;
/// The largest non-streamed request body that will be buffered.
///
/// Far above `screeny_device_api::route::MAX_REQUEST_LEN` (384 bytes) on
/// purpose: a body between the route's bound and this one has to be *read* to
/// be refused with `payload_too_large` rather than by hanging up, which is
/// what a client needs to see the error at all.
pub const MAX_BUFFERED_BODY: usize = 64 * 1024;

/// A parsed request line and header block.
#[derive(Debug, Clone)]
pub struct Head {
    /// The method, uppercased: `GET`, `POST`, or whatever was sent.
    pub method: String,
    /// The path, with any query string removed.
    pub path: String,
    /// The query string, without the `?`. Empty when there was none.
    pub query: String,
    /// The `Host` header, lowercased, or `None`.
    pub host: Option<String>,
    /// The peer.
    pub peer: SocketAddr,
    /// `Content-Length`, if it parsed.
    pub content_length: Option<u64>,
    /// Whether the client asked for `100-continue`.
    pub expect_continue: bool,
    /// Whether the body is chunked, which this server refuses.
    pub chunked: bool,
}

/// What to answer with.
#[derive(Debug, Clone)]
pub struct Response {
    /// The status code.
    pub status: u16,
    /// The `Content-Type`.
    pub content_type: &'static str,
    /// The body. May be empty; `Content-Length` is always sent.
    pub body: Vec<u8>,
    /// Extra headers, in order.
    pub headers: Vec<(&'static str, String)>,
}

impl Response {
    /// A JSON reply.
    #[must_use]
    pub fn json(status: u16, body: Vec<u8>) -> Self {
        Response {
            status,
            content_type: "application/json",
            body,
            headers: Vec::new(),
        }
    }

    /// An HTML reply.
    #[must_use]
    pub fn html(status: u16, body: &str) -> Self {
        Response {
            status,
            content_type: "text/html; charset=utf-8",
            body: body.as_bytes().to_vec(),
            headers: Vec::new(),
        }
    }

    /// The reason phrase for the status. Only the codes this API uses are
    /// named; anything else gets a generic one, which is legal and which no
    /// client reads.
    #[must_use]
    pub fn reason(&self) -> &'static str {
        match self.status {
            200 => "OK",
            204 => "No Content",
            400 => "Bad Request",
            401 => "Unauthorized",
            403 => "Forbidden",
            404 => "Not Found",
            405 => "Method Not Allowed",
            409 => "Conflict",
            411 => "Length Required",
            413 => "Payload Too Large",
            429 => "Too Many Requests",
            500 => "Internal Server Error",
            503 => "Service Unavailable",
            s if s < 300 => "OK",
            s if s < 400 => "Redirect",
            s if s < 500 => "Error",
            _ => "Server Error",
        }
    }
}

/// The body of a request, as the handler sees it.
pub enum Body<'a> {
    /// Already read; at most [`MAX_BUFFERED_BODY`] bytes.
    Buffered(Vec<u8>),
    /// Not read. The handler drains it. This is `POST /api/v1/firmware`,
    /// which a device streams to flash and must never buffer.
    Stream {
        /// The reader, positioned at the first body byte.
        reader: &'a mut dyn Read,
        /// How many bytes the client said it would send.
        len: Option<u64>,
    },
}

/// What the transport hands the routes.
pub type Handler = dyn Fn(&Head, Body<'_>) -> Response + Send + Sync;

/// Whether a route wants its body streamed rather than buffered.
pub type WantsStream = dyn Fn(&Head) -> bool + Send + Sync;

/// A running server.
pub struct Server {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Server {
    /// Bind and start serving.
    ///
    /// # Errors
    /// Anything that stops the listener binding.
    pub fn start(
        bind: SocketAddr,
        handler: Arc<Handler>,
        wants_stream: Arc<WantsStream>,
    ) -> io::Result<Self> {
        let listener = TcpListener::bind(bind)?;
        let addr = listener.local_addr()?;
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let stop = Arc::clone(&stop);
            thread::Builder::new()
                .name("sim-http".into())
                .spawn(move || accept_loop(&listener, &stop, &handler, &wants_stream))?
        };
        Ok(Server {
            addr,
            stop,
            thread: Some(thread),
        })
    }

    /// The address the listener is actually bound to.
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Stop accepting and join every thread.
    pub fn shutdown(mut self) {
        self.stop_and_join();
    }

    fn stop_and_join(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop_and_join();
    }
}

fn accept_loop(
    listener: &TcpListener,
    stop: &Arc<AtomicBool>,
    handler: &Arc<Handler>,
    wants_stream: &Arc<WantsStream>,
) {
    let mut workers: Vec<JoinHandle<()>> = Vec::new();
    while !stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, peer)) => {
                // Reap the finished ones. Dropping a `JoinHandle` whose
                // thread has already exited detaches nothing: the thread is
                // over. The ones still running are joined at shutdown.
                workers.retain(|w| !w.is_finished());
                let handler = Arc::clone(handler);
                let wants_stream = Arc::clone(wants_stream);
                match thread::Builder::new()
                    .name("sim-http-conn".into())
                    .spawn(move || {
                        let _ = serve_one(stream, peer, &handler, &wants_stream);
                    }) {
                    Ok(w) => workers.push(w),
                    // Out of threads: the connection is dropped, which is what
                    // a device out of sockets does too.
                    Err(_) => continue,
                }
            }
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(ACCEPT_POLL);
            }
            Err(_) => break,
        }
    }
    for w in workers {
        let _ = w.join();
    }
}

fn serve_one(
    stream: TcpStream,
    peer: SocketAddr,
    handler: &Arc<Handler>,
    wants_stream: &Arc<WantsStream>,
) -> io::Result<()> {
    // **The listener is non-blocking, and on the BSDs - macOS included - the
    // accepted socket inherits that flag**, where on Linux it does not. Left
    // alone, a connection whose first bytes have not arrived by the time this
    // thread reads gives `WouldBlock`, `read_head` reads it as "a connection
    // that said nothing", and the client gets a closed socket and no response
    // at all. That is a load-dependent flake in every HTTP test, and it was
    // one: card 232 chased it down. Blocking mode plus `IO_TIMEOUT` is what
    // the rest of this function already assumes.
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);

    let head = match read_head(&mut reader, peer) {
        Ok(Some(h)) => h,
        // A connection that said nothing, or said something unparseable.
        Ok(None) => return Ok(()),
        Err(e) => {
            let _ = write_response(&mut writer, &Response::json(400, e.into_bytes()));
            return Ok(());
        }
    };

    if head.expect_continue {
        writer.write_all(b"HTTP/1.1 100 Continue\r\n\r\n")?;
        writer.flush()?;
    }

    let response = if head.chunked {
        // A `no_std` server on a fixed buffer will not decode chunked either.
        crate::api::error_response(
            screeny_device_api::ErrorCode::BadRequest,
            "chunked request bodies are not accepted",
        )
    } else if wants_stream(&head) {
        let len = head.content_length;
        let mut limited = LimitedRead {
            inner: &mut reader,
            left: len.unwrap_or(0),
        };
        handler(
            &head,
            Body::Stream {
                reader: &mut limited,
                len,
            },
        )
    } else {
        match read_body(&mut reader, head.content_length) {
            Ok(body) => handler(&head, Body::Buffered(body)),
            Err(r) => r,
        }
    };

    write_response(&mut writer, &response)
}

/// Read up to `\r\n\r\n`. `Ok(None)` is a connection that closed politely.
fn read_head(reader: &mut BufReader<TcpStream>, peer: SocketAddr) -> Result<Option<Head>, String> {
    let mut lines: Vec<String> = Vec::new();
    let mut total = 0usize;
    loop {
        let mut line = String::new();
        let n = match reader.read_line(&mut line) {
            Ok(n) => n,
            Err(_) => return Ok(None),
        };
        if n == 0 {
            return if lines.is_empty() {
                Ok(None)
            } else {
                Err("unterminated request head".into())
            };
        }
        total += n;
        if total > MAX_HEAD_LEN {
            return Err("request head is too long".into());
        }
        let trimmed = line.trim_end_matches(['\r', '\n']).to_string();
        if trimmed.is_empty() {
            break;
        }
        lines.push(trimmed);
    }

    let mut it = lines.into_iter();
    let Some(request_line) = it.next() else {
        return Ok(None);
    };
    let mut parts = request_line.split_whitespace();
    let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
        return Err("malformed request line".into());
    };

    let mut headers: BTreeMap<String, String> = BTreeMap::new();
    for line in it {
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }

    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target.to_string(), String::new()),
    };

    Ok(Some(Head {
        method: method.to_ascii_uppercase(),
        path,
        query,
        host: headers.get("host").map(|h| h.to_ascii_lowercase()),
        peer,
        content_length: headers.get("content-length").and_then(|v| v.parse().ok()),
        expect_continue: headers
            .get("expect")
            .is_some_and(|v| v.eq_ignore_ascii_case("100-continue")),
        chunked: headers
            .get("transfer-encoding")
            .is_some_and(|v| v.to_ascii_lowercase().contains("chunked")),
    }))
}

fn read_body(reader: &mut BufReader<TcpStream>, len: Option<u64>) -> Result<Vec<u8>, Response> {
    let Some(len) = len else {
        return Ok(Vec::new());
    };
    if len > MAX_BUFFERED_BODY as u64 {
        // Not read: refusing before the bytes arrive is exactly what a device
        // with a 4 KB buffer does, and the client learns why.
        return Err(crate::api::error_response(
            screeny_device_api::ErrorCode::PayloadTooLarge,
            "body is longer than the simulator will buffer",
        ));
    }
    let mut body = vec![0u8; len as usize];
    match reader.read_exact(&mut body) {
        Ok(()) => Ok(body),
        Err(_) => Err(crate::api::error_response(
            screeny_device_api::ErrorCode::BadRequest,
            "request body was shorter than content-length",
        )),
    }
}

fn write_response(w: &mut TcpStream, r: &Response) -> io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\
         Cache-Control: no-store\r\n",
        r.status,
        r.reason(),
        r.content_type,
        r.body.len()
    );
    for (k, v) in &r.headers {
        head.push_str(k);
        head.push_str(": ");
        head.push_str(v);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    w.write_all(head.as_bytes())?;
    w.write_all(&r.body)?;
    w.flush()
}

/// A reader that stops after `left` bytes, so a streamed body cannot run into
/// the next request.
struct LimitedRead<'a> {
    inner: &'a mut dyn Read,
    left: u64,
}

impl Read for LimitedRead<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.left == 0 {
            return Ok(0);
        }
        let want = buf.len().min(usize::try_from(self.left).unwrap_or(usize::MAX));
        let n = self.inner.read(&mut buf[..want])?;
        self.left -= n as u64;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_response_names_the_statuses_this_api_uses() {
        for (status, reason) in [
            (200, "OK"),
            (400, "Bad Request"),
            (404, "Not Found"),
            (405, "Method Not Allowed"),
            (413, "Payload Too Large"),
            (429, "Too Many Requests"),
            (503, "Service Unavailable"),
        ] {
            assert_eq!(Response::json(status, Vec::new()).reason(), reason);
        }
    }
}
