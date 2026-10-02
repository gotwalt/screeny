//! A bench client for Improv Wi-Fi over serial (card 362): ask a device what
//! ESP Web Tools would ask it, without a browser.
//!
//! The framing is `screeny_provision::improv`; this module only moves bytes.
//! It offers `info`, `state` and `scan` and **deliberately no way to send
//! credentials**: a password on a command line lands in shell history, and
//! ESP Web Tools is the tool for that half.
//!
//! A port is a serial device path (115200 8N1, raw, `HUPCL` cleared so closing
//! it does not drop DTR) or `tcp:HOST:PORT`, which is how the simulator
//! exposes the same conversation. Opening a USB serial port may reset the
//! board (DTR/RTS), so a request is re-sent every 700 ms until it is answered.

use std::io::{Read, Write};
use std::time::{Duration, Instant};

use screeny_provision::improv::{
    cmd, encode_rpc_command, for_each_result_string, scan_frame, ty, ErrorCode, Scan, State,
};

/// What went wrong talking Improv.
#[derive(Debug, thiserror::Error)]
pub enum ImprovError {
    /// The port could not be opened or read.
    #[error("{0}")]
    Io(#[from] std::io::Error),
    /// Nothing answered in time.
    #[error("no Improv answer from the device within {0:?}")]
    Timeout(Duration),
    /// This platform cannot open a serial device.
    #[error("serial ports are only supported on unix; use tcp:HOST:PORT")]
    Unsupported,
}

/// A byte pipe to a device.
pub trait Port: Read + Write {}
impl<T: Read + Write> Port for T {}

/// One frame from the device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    /// Current state.
    State(State),
    /// Error state.
    Error(ErrorCode),
    /// An RPC result.
    Result {
        /// The command it answers.
        command: u8,
        /// Its strings.
        strings: Vec<String>,
    },
}

/// What to ask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ask {
    /// Request device information.
    Info,
    /// Request current state.
    State,
    /// Request scanned networks.
    Scan,
}

impl Ask {
    fn command(self) -> u8 {
        match self {
            Ask::Info => cmd::GET_DEVICE_INFO,
            Ask::State => cmd::GET_CURRENT_STATE,
            Ask::Scan => cmd::GET_WIFI_NETWORKS,
        }
    }
}

/// The request frame for `ask`.
#[must_use]
pub fn request_frame(ask: Ask) -> Vec<u8> {
    let mut out = [0u8; 16];
    let n = encode_rpc_command(&mut out, ask.command(), &[]).expect("fits");
    out[..n].to_vec()
}

fn decode(ty_: u8, data: &[u8]) -> Option<Frame> {
    match ty_ {
        ty::CURRENT_STATE => State::from_u8(*data.first()?).map(Frame::State),
        ty::ERROR_STATE => Some(Frame::Error(match *data.first()? {
            0 => ErrorCode::None,
            1 => ErrorCode::InvalidRpc,
            2 => ErrorCode::UnknownRpc,
            3 => ErrorCode::UnableToConnect,
            4 => ErrorCode::NotAuthorized,
            5 => ErrorCode::BadHostname,
            _ => ErrorCode::Unknown,
        })),
        ty::RPC_RESULT => {
            let mut strings = Vec::new();
            let command = for_each_result_string(data, |s| {
                strings.push(String::from_utf8_lossy(s).into_owned());
            })?;
            Some(Frame::Result { command, strings })
        }
        _ => None,
    }
}

/// Pull every complete frame out of `buf`, leaving the unread tail.
pub fn drain_frames(buf: &mut Vec<u8>) -> Vec<Frame> {
    let mut out = Vec::new();
    loop {
        match scan_frame(buf) {
            Scan::Frame { ty, data, used } => {
                out.extend(decode(ty, data));
                buf.drain(..used);
            }
            Scan::Need { skip: 0 } => break,
            Scan::Need { skip } => {
                buf.drain(..skip);
            }
        }
    }
    out
}

/// Whether `frames` hold the answer to `ask`.
fn answered(ask: Ask, frames: &[Frame]) -> bool {
    frames.iter().any(|f| match (ask, f) {
        (Ask::Info, Frame::Result { command, .. }) => *command == cmd::GET_DEVICE_INFO,
        (Ask::State, Frame::State(_)) => true,
        (Ask::Scan, Frame::Result { command, strings }) => {
            *command == cmd::GET_WIFI_NETWORKS && strings.is_empty()
        }
        (_, Frame::Error(e)) => matches!(e, ErrorCode::InvalidRpc | ErrorCode::UnknownRpc),
        _ => false,
    })
}

/// Ask once, re-sending every 700 ms, and collect frames until the answer is
/// complete or `wait` runs out. For `state`, lingers 300 ms after the state
/// frame for the URL result that may follow it.
pub fn query(port: &mut dyn Port, ask: Ask, wait: Duration) -> Result<Vec<Frame>, ImprovError> {
    let req = request_frame(ask);
    let start = Instant::now();
    let mut next_send = start;
    let mut buf = Vec::new();
    let mut frames = Vec::new();
    let mut answered_at: Option<Instant> = None;
    let mut tmp = [0u8; 256];
    loop {
        let now = Instant::now();
        if answered_at.is_none() && now >= next_send {
            port.write_all(&req)?;
            port.flush()?;
            next_send = now + Duration::from_millis(700);
        }
        match port.read(&mut tmp) {
            Ok(0) => std::thread::sleep(Duration::from_millis(20)),
            Ok(n) => {
                buf.extend_from_slice(&tmp[..n]);
                frames.extend(drain_frames(&mut buf));
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::Interrupted
                ) =>
            {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return Err(e.into()),
        }
        if answered_at.is_none() && answered(ask, &frames) {
            answered_at = Some(Instant::now());
        }
        if let Some(t) = answered_at {
            let linger = if ask == Ask::State { Duration::from_millis(300) } else { Duration::ZERO };
            if t.elapsed() >= linger {
                return Ok(frames);
            }
        }
        if start.elapsed() >= wait {
            return Err(ImprovError::Timeout(wait));
        }
    }
}

/// The lines `screeny improv` prints for a set of frames.
#[must_use]
pub fn render(ask: Ask, frames: &[Frame]) -> Vec<String> {
    let mut lines = Vec::new();
    for f in frames {
        match (ask, f) {
            (Ask::Info, Frame::Result { command, strings }) if *command == cmd::GET_DEVICE_INFO => {
                for (label, v) in ["firmware", "version", "chip", "name"].iter().zip(strings) {
                    lines.push(format!("{label}: {v}"));
                }
            }
            (Ask::State, Frame::State(s)) => lines.push(format!("state: {s:?}")),
            (Ask::State, Frame::Result { command, strings }) if *command == cmd::GET_CURRENT_STATE => {
                if let Some(url) = strings.first() {
                    lines.push(format!("url: {url}"));
                }
            }
            (Ask::Scan, Frame::Result { command, strings }) if *command == cmd::GET_WIFI_NETWORKS => {
                if strings.is_empty() {
                    lines.push("scan complete".to_owned());
                } else {
                    lines.push(strings.join("  "));
                }
            }
            (_, Frame::Error(e)) if *e != ErrorCode::None => lines.push(format!("error: {e:?}")),
            _ => {}
        }
    }
    lines
}

/// Open `spec`: `tcp:HOST:PORT` or a serial device path.
pub fn open(spec: &str) -> Result<Box<dyn Port>, ImprovError> {
    if let Some(addr) = spec.strip_prefix("tcp:") {
        let s = std::net::TcpStream::connect(addr)?;
        s.set_nodelay(true)?;
        s.set_read_timeout(Some(Duration::from_millis(100)))?;
        return Ok(Box::new(s));
    }
    open_tty(spec)
}

#[cfg(unix)]
fn open_tty(path: &str) -> Result<Box<dyn Port>, ImprovError> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;

    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK)
        .open(path)?;
    let fd = file.as_raw_fd();
    // SAFETY: `fd` is open for the whole block and `t` is a plain C struct that
    // `tcgetattr` fully initialises before anything reads it.
    unsafe {
        let mut t: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(fd, &raw mut t) != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        libc::cfmakeraw(&raw mut t);
        t.c_cflag |= libc::CLOCAL | libc::CREAD;
        // Closing the port must not drop DTR: on a USB bridge that resets the board.
        t.c_cflag &= !libc::HUPCL;
        libc::cfsetspeed(&raw mut t, libc::B115200);
        t.c_cc[libc::VMIN] = 0;
        t.c_cc[libc::VTIME] = 0;
        if libc::tcsetattr(fd, libc::TCSANOW, &raw const t) != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    Ok(Box::new(file))
}

#[cfg(not(unix))]
fn open_tty(_path: &str) -> Result<Box<dyn Port>, ImprovError> {
    Err(ImprovError::Unsupported)
}
