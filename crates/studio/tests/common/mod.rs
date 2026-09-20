//! A browser, in about as few lines as one can be written: enough HTTP/1.1 to
//! call the API and enough of RFC 6455 to read what the server pushes.
//!
//! Hand-written on purpose. A test client is the one place where borrowing the
//! server's own library would hide the bug worth catching, and the alternative
//! (an HTTP client crate and a WebSocket crate) is a large dependency tree for
//! two dozen lines of framing.

#![allow(dead_code)]

use std::net::SocketAddr;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use screeny_studio::{Config, Running, Studio};
use std::path::{Path, PathBuf};

/// Nothing in these tests waits longer than this for a thing that should
/// happen at once.
pub const PATIENCE: Duration = Duration::from_secs(5);

/// A studio on an ephemeral loopback port. Dropping the returned handle stops
/// the engine thread, releases the panel and stops the server.
pub async fn studio() -> Running {
    Studio::bind(test_config()).await.expect("bind an ephemeral loopback port").spawn()
}

/// The configuration every test in this crate starts from.
///
/// Loopback, an ephemeral port, **no discovery** and **no state file**: a
/// worker's environment cannot reach the LAN and must never try, and a test
/// must never leave a file behind. Everything a test wants beyond that it
/// turns on itself.
#[must_use]
pub fn test_config() -> Config {
    Config {
        listen: SocketAddr::from(([127, 0, 0, 1], 0)),
        // Fast enough that a test is seconds rather than minutes, slow enough
        // that the loops are still loops.
        supervise_every: Duration::from_millis(200),
        telemetry_every: Duration::from_millis(200),
        ..Config::default()
    }
}

/// A studio that keeps its state in `dir`, so it can be restarted.
pub async fn studio_in(dir: &Path, faults: bool) -> Running {
    let cfg = Config { state_dir: Some(dir.to_path_buf()), fault_patches: faults, ..test_config() };
    Studio::bind(cfg).await.expect("bind an ephemeral loopback port").spawn()
}

/// A directory of this test's own, removed when the test ends.
pub struct Temp(pub PathBuf);

impl Temp {
    #[must_use]
    pub fn new(tag: &str) -> Temp {
        let p = std::env::temp_dir().join(format!("screeny-studio-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("a temp dir");
        Temp(p)
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Poll `f` until it is true, or give up after [`PATIENCE`] (or `patience`).
///
/// Every wait in these tests goes through here, so none of them can hang.
pub async fn until<F, Fut>(patience: Duration, what: &str, mut f: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + patience;
    loop {
        if f().await {
            return;
        }
        assert!(tokio::time::Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Poll a JSON route until it says what the test is waiting for, and hand back
/// **the answer that said it**.
///
/// The point of returning it is that it removes a whole class of flake: a test
/// that waits for a condition and then fetches again is asserting on a
/// different moment from the one it waited for, and on a loaded host those two
/// moments are far enough apart to matter. Everything a test wants to know
/// about one moment should come out of one read of it.
///
/// The timeout message carries the last answer, so a failure on a busy machine
/// says what the server actually thought rather than only that time ran out.
pub async fn until_json<F>(
    at: SocketAddr,
    patience: Duration,
    what: &str,
    path: &str,
    mut ok: F,
) -> serde_json::Value
where
    F: FnMut(&serde_json::Value) -> bool,
{
    let deadline = tokio::time::Instant::now() + patience;
    loop {
        let last = get(at, path).await.json();
        if ok(&last) {
            return last;
        }
        assert!(tokio::time::Instant::now() < deadline, "timed out waiting for {what}; the last answer was {last}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
}

impl Response {
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|e| {
            panic!("expected JSON, got {:?}: {e}", String::from_utf8_lossy(&self.body))
        })
    }
}

/// One request, one connection, `Connection: close` - so the whole response is
/// "everything up to EOF" and there is no framing to get wrong.
async fn request(addr: SocketAddr, head: String, body: &[u8]) -> Response {
    let mut stream = TcpStream::connect(addr).await.expect("connect to the studio");
    stream.write_all(head.as_bytes()).await.expect("write the request");
    stream.write_all(body).await.expect("write the body");
    let mut raw = Vec::new();
    // A reset is only a failure if it cost us the answer: a server that closes
    // a connection with anything still unread on it resets rather than closes,
    // and the response may already be here.
    let read = tokio::time::timeout(PATIENCE, stream.read_to_end(&mut raw)).await.expect("the studio answered");
    if let Err(e) = read {
        assert!(!raw.is_empty(), "no answer from the studio: {e}");
    }

    let split = raw.windows(4).position(|w| w == b"\r\n\r\n").expect("a complete response");
    let head = String::from_utf8_lossy(&raw[..split]).to_string();
    let status = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .expect("a status line");
    Response { status, body: raw[split + 4..].to_vec() }
}

pub async fn get(addr: SocketAddr, path: &str) -> Response {
    request(addr, format!("GET {path} HTTP/1.1\r\nHost: studio\r\nConnection: close\r\n\r\n"), b"").await
}

pub async fn post(addr: SocketAddr, path: &str, body: &str) -> Response {
    post_as(addr, path, body, None).await
}

/// A change made by a named browser, as the UI tags its own.
pub async fn post_as(addr: SocketAddr, path: &str, body: &str, client: Option<&str>) -> Response {
    let id = client.map_or(String::new(), |c| format!("x-studio-client: {c}\r\n"));
    let head = format!(
        "POST {path} HTTP/1.1\r\nHost: studio\r\nConnection: close\r\ncontent-type: application/json\r\n\
         content-length: {}\r\n{id}\r\n",
        body.len()
    );
    request(addr, head, body.as_bytes()).await
}

// ---------------- the preview socket ----------------

#[derive(Debug)]
pub enum Msg {
    Text(String),
    Binary(Vec<u8>),
    Close,
}

pub struct Ws {
    stream: TcpStream,
    buf: Vec<u8>,
}

impl Ws {
    /// `client` is the browser id the server uses to skip echoing a change
    /// back to whoever made it.
    pub async fn connect(addr: SocketAddr, client: Option<&str>) -> Ws {
        Self::connect_on(TcpStream::connect(addr).await.expect("connect to the studio"), client).await
    }

    /// A socket that says on the way in what pace it wants (card 120).
    pub async fn connect_asking(addr: SocketAddr, query: &str) -> Ws {
        let stream = TcpStream::connect(addr).await.expect("connect to the studio");
        Self::open(stream, format!("?{query}")).await
    }

    /// As [`Ws::connect`], on a socket the caller has already made - which is
    /// how the stalled-browser test gives itself a tiny receive buffer.
    pub async fn connect_on(stream: TcpStream, client: Option<&str>) -> Ws {
        let query = client.map_or(String::new(), |c| format!("?client={c}"));
        Self::open(stream, query).await
    }

    async fn open(stream: TcpStream, query: String) -> Ws {
        let mut ws = Ws { stream, buf: Vec::new() };
        let head = format!(
            "GET /api/v1/ws{query} HTTP/1.1\r\nHost: studio\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n"
        );
        ws.stream.write_all(head.as_bytes()).await.expect("write the upgrade");
        // Read up to the end of the handshake; anything after it is the first
        // frames, and stays in the buffer.
        loop {
            if let Some(end) = ws.buf.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&ws.buf[..end]).to_string();
                assert!(head.starts_with("HTTP/1.1 101"), "the studio refused the upgrade: {head}");
                ws.buf.drain(..end + 4);
                return ws;
            }
            ws.fill().await.expect("the studio completed the handshake");
        }
    }

    /// Say something to the server: one masked text frame, as a browser sends
    /// them. Card 120's `{"type":"preview",...}` is the only thing a page ever
    /// sends, and a client frame **must** be masked (RFC 6455 5.1).
    pub async fn say(&mut self, text: &str) {
        let payload = text.as_bytes();
        assert!(payload.len() < 126, "the test client only writes short frames");
        let mask = [0x37u8, 0xfa, 0x21, 0x3d];
        let mut out = vec![0x81, 0x80 | payload.len() as u8];
        out.extend_from_slice(&mask);
        out.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
        self.stream.write_all(&out).await.expect("write a client frame");
    }

    async fn fill(&mut self) -> std::io::Result<()> {
        let mut chunk = [0u8; 16 * 1024];
        let n = self.stream.read(&mut chunk).await?;
        if n == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        self.buf.extend_from_slice(&chunk[..n]);
        Ok(())
    }

    async fn need(&mut self, n: usize) -> std::io::Result<()> {
        while self.buf.len() < n {
            self.fill().await?;
        }
        Ok(())
    }

    /// The next message, or `None` when the server has closed the socket.
    /// Messages here are never fragmented: the studio sends whole frames.
    pub async fn next(&mut self) -> Option<Msg> {
        self.need(2).await.ok()?;
        let opcode = self.buf[0] & 0x0f;
        let short = usize::from(self.buf[1] & 0x7f);
        assert_eq!(self.buf[1] & 0x80, 0, "a server frame must not be masked");
        let (len, head) = match short {
            126 => {
                self.need(4).await.ok()?;
                (usize::from(u16::from_be_bytes([self.buf[2], self.buf[3]])), 4)
            }
            127 => {
                self.need(10).await.ok()?;
                let mut b = [0u8; 8];
                b.copy_from_slice(&self.buf[2..10]);
                (u64::from_be_bytes(b) as usize, 10)
            }
            n => (n, 2),
        };
        self.need(head + len).await.ok()?;
        let payload: Vec<u8> = self.buf[head..head + len].to_vec();
        self.buf.drain(..head + len);
        match opcode {
            0x1 => Some(Msg::Text(String::from_utf8(payload).expect("text frames are UTF-8"))),
            0x2 => Some(Msg::Binary(payload)),
            0x8 => Some(Msg::Close),
            other => panic!("unexpected websocket opcode {other}"),
        }
    }

    /// The next message of a kind the caller cares about, within [`PATIENCE`].
    pub async fn next_matching(&mut self, want: impl Fn(&Msg) -> bool) -> Msg {
        let deadline = tokio::time::Instant::now() + PATIENCE;
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            let msg = tokio::time::timeout(left, self.next())
                .await
                .expect("the studio kept sending")
                .expect("the socket stayed open");
            if want(&msg) {
                return msg;
            }
        }
    }

    /// The next frame packet.
    pub async fn frame(&mut self) -> Vec<u8> {
        match self.next_matching(|m| matches!(m, Msg::Binary(_))).await {
            Msg::Binary(b) => b,
            _ => unreachable!(),
        }
    }

    /// The next text message whose `"type"` is `kind`.
    pub async fn event(&mut self, kind: &str) -> serde_json::Value {
        let wanted = kind.to_string();
        let msg = self
            .next_matching(move |m| match m {
                Msg::Text(t) => serde_json::from_str::<serde_json::Value>(t)
                    .is_ok_and(|v| v["type"] == serde_json::Value::String(wanted.clone())),
                _ => false,
            })
            .await;
        match msg {
            Msg::Text(t) => serde_json::from_str(&t).expect("valid JSON"),
            _ => unreachable!(),
        }
    }

    /// How many frames arrive in `window`. The measure of whether a browser is
    /// being served properly.
    pub async fn count_frames(&mut self, window: Duration) -> usize {
        self.measure(window).await.frames
    }

    /// Everything that arrives in `window`, and what it weighs: card 120's
    /// question is bytes per second per tab, so a test can ask it directly.
    pub async fn measure(&mut self, window: Duration) -> Traffic {
        let deadline = tokio::time::Instant::now() + window;
        let mut t = Traffic::default();
        while let Some(left) = deadline.checked_duration_since(tokio::time::Instant::now()) {
            match tokio::time::timeout(left, self.next()).await {
                Ok(Some(Msg::Binary(b))) => {
                    t.frames += 1;
                    t.frame_bytes += b.len();
                }
                Ok(Some(Msg::Text(s))) => {
                    t.texts += 1;
                    t.text_bytes += s.len();
                }
                Ok(Some(Msg::Close)) | Ok(None) => break,
                Err(_) => break,
            }
        }
        t.window = window;
        t
    }
}

/// What one browser was sent over a window.
#[derive(Clone, Copy, Debug, Default)]
pub struct Traffic {
    pub frames: usize,
    pub frame_bytes: usize,
    pub texts: usize,
    pub text_bytes: usize,
    pub window: Duration,
}

impl Traffic {
    #[must_use]
    pub fn bytes_per_s(&self) -> f64 {
        (self.frame_bytes + self.text_bytes) as f64 / self.window.as_secs_f64().max(f64::EPSILON)
    }

    #[must_use]
    pub fn fps(&self) -> f64 {
        self.frames as f64 / self.window.as_secs_f64().max(f64::EPSILON)
    }
}

/// The sequence number in a frame packet's header.
#[must_use]
pub fn seq_of(packet: &[u8]) -> u32 {
    u32::from_le_bytes(packet[..4].try_into().expect("a frame packet"))
}

/// The 64x32 sRGB picture in a frame packet: what the browser draws.
#[must_use]
pub fn preview_of(packet: &[u8]) -> Vec<u8> {
    packet[screeny_studio::page::HEADER..].to_vec()
}
