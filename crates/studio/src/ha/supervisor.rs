//! Card 359: the broker, from the Home Assistant Supervisor.
//!
//! When the studio runs as a Home Assistant app with `services: [mqtt:want]`,
//! the Supervisor hands it the MQTT broker's address and credentials:
//!
//! ```text
//! GET http://supervisor/services/mqtt        Authorization: Bearer $SUPERVISOR_TOKEN
//! { "result": "ok", "data": { "host", "port", "ssl", "username", "password", "protocol", "addon"/"app" } }
//! ```
//!
//! (Home Assistant developer docs, `api/supervisor/endpoints#service`; the
//! envelope and the field schema are `supervisor/api/utils.py` and
//! `supervisor/services/modules/mqtt.py` in the Supervisor source. The docs'
//! example shows `port` as a string; the schema says an integer. Both are
//! read.) A 400 answer means no MQTT service is provided yet - Mosquitto is
//! not installed or not started - which is normal and retried.
//!
//! **The owner's settings win** ([`super::Ha::effective`]), and **the
//! password is never logged, shown or returned**: [`super::MqttConfig`]'s
//! `Debug` redacts it and nothing here prints a body.
//!
//! One hand-written `GET` over a plain TCP socket with a deadline, for the
//! reason `devhttp` gives: it is one request, and a client crate is a large
//! tree for it. The Supervisor speaks plain HTTP on its internal network.

use super::{MqttConfig, DEFAULT_INSTANCE, DEFAULT_NAME, DEFAULT_PREFIX};
use crate::AppState;
use serde::Deserialize;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Looked at again this often once there is a broker: credentials can change
/// when the MQTT app is reconfigured.
pub const RECHECK: Duration = Duration::from_secs(300);
/// ...and this often while there is none, which is the usual state for the
/// first minute after a boot: Mosquitto starts after this app may.
pub const RETRY: Duration = Duration::from_secs(20);
/// The whole request.
const TIMEOUT: Duration = Duration::from_secs(5);
/// The most of a reply that is read.
const MAX_REPLY: usize = 16 * 1024;

/// Where the Supervisor is and the token that says who is asking. The token
/// never appears in a `{:?}`.
#[derive(Clone, PartialEq, Eq)]
pub struct Supervisor {
    /// `http://supervisor`, or a test's fake.
    pub base: String,
    pub token: String,
}

impl std::fmt::Debug for Supervisor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Supervisor").field("base", &self.base).field("token", &"<set>").finish()
    }
}

#[derive(Deserialize)]
struct Envelope {
    result: Option<String>,
    data: Option<Service>,
}

#[derive(Deserialize)]
struct Service {
    host: String,
    port: serde_json::Value,
    #[serde(default)]
    ssl: bool,
    username: Option<String>,
    password: Option<String>,
}

/// Why there is no broker, for one log line. Never carries a body.
#[derive(Debug, PartialEq, Eq)]
pub enum Fault {
    /// The Supervisor answered, and offers no MQTT service (yet).
    NotProvided,
    /// The broker needs TLS, which this studio is built without.
    Tls,
    Other(String),
}

/// Ask the Supervisor for its MQTT broker.
///
/// # Errors
///
/// [`Fault`]: nothing offered, TLS, or the Supervisor could not be read.
pub async fn fetch(sup: &Supervisor) -> Result<MqttConfig, Fault> {
    match tokio::time::timeout(TIMEOUT, get(sup, "/services/mqtt")).await {
        Ok(r) => r.and_then(|(status, body)| decode(status, &body)),
        Err(_) => Err(Fault::Other("timed out asking the Supervisor".into())),
    }
}

/// The reply, read. Split out so it is tested without a socket.
///
/// # Errors
///
/// [`Fault`], as [`fetch`].
pub fn decode(status: u16, body: &[u8]) -> Result<MqttConfig, Fault> {
    if status == 400 || status == 404 {
        return Err(Fault::NotProvided);
    }
    if status != 200 {
        return Err(Fault::Other(format!("the Supervisor answered {status}")));
    }
    let env: Envelope = serde_json::from_slice(body).map_err(|_| Fault::Other("the Supervisor's answer was not understood".into()))?;
    if env.result.as_deref().is_some_and(|r| r != "ok") {
        return Err(Fault::NotProvided);
    }
    let svc = env.data.ok_or(Fault::NotProvided)?;
    let port = match &svc.port {
        serde_json::Value::Number(n) => n.as_u64(),
        serde_json::Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
    .and_then(|p| u16::try_from(p).ok())
    .filter(|p| *p != 0)
    .ok_or_else(|| Fault::Other("the Supervisor gave no usable broker port".into()))?;
    if svc.ssl {
        return Err(Fault::Tls);
    }
    if svc.host.trim().is_empty() {
        return Err(Fault::NotProvided);
    }
    Ok(MqttConfig {
        host: svc.host.trim().to_string(),
        port,
        username: svc.username.filter(|u| !u.is_empty()),
        password: svc.password.filter(|p| !p.is_empty()),
        discovery_prefix: DEFAULT_PREFIX.to_string(),
        instance: DEFAULT_INSTANCE.to_string(),
        name: DEFAULT_NAME.to_string(),
    })
}

async fn get(sup: &Supervisor, path: &str) -> Result<(u16, Vec<u8>), Fault> {
    let other = |e: std::io::Error| Fault::Other(format!("talking to the Supervisor: {e}"));
    let rest = sup.base.strip_prefix("http://").ok_or_else(|| Fault::Other("the Supervisor's address is not http://".into()))?;
    let authority = rest.trim_end_matches('/');
    let target = if authority.contains(':') { authority.to_string() } else { format!("{authority}:80") };
    let mut stream = TcpStream::connect(&target).await.map_err(other)?;
    let head = format!(
        "GET {path} HTTP/1.1\r\nHost: {authority}\r\nAuthorization: Bearer {}\r\nAccept: application/json\r\nConnection: close\r\n\r\n",
        sup.token
    );
    stream.write_all(head.as_bytes()).await.map_err(other)?;
    let mut raw = Vec::new();
    let mut chunk = [0u8; 2048];
    loop {
        let n = stream.read(&mut chunk).await.map_err(other)?;
        if n == 0 {
            break;
        }
        raw.extend_from_slice(&chunk[..n]);
        if raw.len() > MAX_REPLY {
            return Err(Fault::Other("the Supervisor's answer was too long".into()));
        }
    }
    let end = raw.windows(4).position(|w| w == b"\r\n\r\n").ok_or_else(|| Fault::Other("no HTTP head in the answer".into()))?;
    let head = String::from_utf8_lossy(&raw[..end]).to_string();
    let status = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| Fault::Other("no status in the answer".into()))?;
    let body = raw[end + 4..].to_vec();
    // aiohttp sends Content-Length; a chunked body is decoded just enough.
    let body = if head.to_ascii_lowercase().contains("transfer-encoding: chunked") { dechunk(&body) } else { body };
    Ok((status, body))
}

fn dechunk(mut b: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(nl) = b.windows(2).position(|w| w == b"\r\n") {
        let size = usize::from_str_radix(String::from_utf8_lossy(&b[..nl]).split(';').next().unwrap_or("").trim(), 16).unwrap_or(0);
        if size == 0 || b.len() < nl + 2 + size {
            break;
        }
        out.extend_from_slice(&b[nl + 2..nl + 2 + size]);
        b = b.get(nl + 2 + size + 2..).unwrap_or(&[]);
    }
    out
}

/// Keep the studio's [`super::Ha`] told what the Supervisor offers, until the
/// studio stops. Says one line when that changes, never the password.
pub fn spawn(st: &AppState, sup: Supervisor, recheck: Duration, retry: Duration) {
    let st = st.clone();
    let mut stop = st.stop.clone();
    tokio::spawn(async move {
        let mut told: Option<String> = None;
        loop {
            let (offered, wait) = match fetch(&sup).await {
                Ok(c) => {
                    let line = format!("broker at {}:{}", c.host, c.port);
                    if told.as_deref() != Some(&line) {
                        println!("studio: mqtt from the Supervisor: {line}");
                        told = Some(line);
                    }
                    (Some(c), recheck)
                }
                Err(f) => {
                    let line = match &f {
                        Fault::NotProvided => "Home Assistant offers no MQTT broker yet (install and start the Mosquitto app, or set a broker on the Settings screen)".to_string(),
                        Fault::Tls => "Home Assistant's MQTT broker needs TLS, which this studio does not speak; set a broker on the Settings screen".to_string(),
                        Fault::Other(why) => why.clone(),
                    };
                    if told.as_deref() != Some(&line) {
                        println!("studio: mqtt from the Supervisor: {line}");
                        told = Some(line);
                    }
                    (None, retry)
                }
            };
            st.ha.set_supervised(offered);
            tokio::select! {
                () = tokio::time::sleep(wait) => {}
                _ = stop.changed() => {}
            }
            if *stop.borrow() {
                return;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"{"result":"ok","data":{"addon":"core_mosquitto","host":"172.30.32.1","port":"1883","ssl":false,"username":"u","password":"password9","protocol":"3.1.1"}}"#;

    #[test]
    fn the_documented_answer_is_read() {
        let c = decode(200, GOOD.as_bytes()).unwrap();
        assert_eq!((c.host.as_str(), c.port, c.username.as_deref(), c.password.as_deref()), ("172.30.32.1", 1883, Some("u"), Some("password9")));
        assert!(!format!("{c:?}").contains("password9"));
        let n = decode(200, br#"{"result":"ok","data":{"host":"b","port":1884}}"#).unwrap();
        assert_eq!((n.port, n.username, n.password), (1884, None, None));
    }

    #[test]
    fn no_service_is_not_an_error_worth_shouting_about() {
        assert_eq!(decode(400, br#"{"result":"error","message":"x"}"#).unwrap_err(), Fault::NotProvided);
        assert_eq!(decode(200, br#"{"result":"error"}"#).unwrap_err(), Fault::NotProvided);
        assert!(matches!(decode(500, b"").unwrap_err(), Fault::Other(_)));
        assert!(matches!(decode(200, b"nope").unwrap_err(), Fault::Other(_)));
        assert_eq!(decode(200, br#"{"result":"ok","data":{"host":"b","port":8883,"ssl":true}}"#).unwrap_err(), Fault::Tls);
        assert!(matches!(decode(200, br#"{"result":"ok","data":{"host":"b","port":0}}"#).unwrap_err(), Fault::Other(_)));
    }

    #[test]
    fn the_token_stays_out_of_debug() {
        let s = Supervisor { base: "http://supervisor".into(), token: "tok-secret".into() };
        assert!(!format!("{s:?}").contains("tok-secret"));
    }

    #[test]
    fn a_chunked_body_is_decoded() {
        assert_eq!(dechunk(b"5\r\nhello\r\n0\r\n\r\n"), b"hello");
    }
}
