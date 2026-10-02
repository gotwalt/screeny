//! Improv Wi-Fi over serial, on a TCP socket (card 362).
//!
//! The simulator has no UART, so the byte stream that ESP Web Tools and
//! `screeny improv` exchange with the firmware on UART0 is offered on a TCP
//! port instead (`--improv-port N`; `screeny improv --port tcp:127.0.0.1:N`).
//! What rides on it is the same conversation, decided by the same code:
//! [`screeny_provision::improv`]'s parser, encoders and [`Session`], with the
//! simulator's [`WifiModel`](crate::WifiModel) - itself the firmware's
//! `Provisioner` - answering "where is the join".
//!
//! Credentials take the path `POST /api/v1/wifi` takes
//! ([`SimHandle::post_wifi`]): the SSID goes to the model, and the password is
//! dropped here, because the simulator keeps none (spec section 8.4).
//!
//! One connection at a time, which is how a serial port behaves.

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use screeny_provision::improv::{DeviceInfo, Parser, Replies, Session, REPLY_MAX};

use crate::SimHandle;

/// How often the socket is polled and the session asked about the join.
const TICK: Duration = Duration::from_millis(20);

/// The Improv listener. Dropping it, or [`ImprovServer::shutdown`], stops and
/// joins its thread.
pub struct ImprovServer {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl ImprovServer {
    /// Bind `bind` and serve Improv for the device behind `handle`.
    ///
    /// # Errors
    /// Anything that stops the socket binding.
    pub fn start(bind: SocketAddr, handle: SimHandle, instance: String) -> io::Result<Self> {
        let listener = TcpListener::bind(bind)?;
        listener.set_nonblocking(true)?;
        let addr = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let thread = thread::Builder::new().name("sim-improv".into()).spawn(move || {
            while !flag.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((s, _)) => {
                        let _ = serve(s, &handle, &instance, &flag);
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => thread::sleep(TICK),
                    Err(_) => thread::sleep(TICK),
                }
            }
        })?;
        Ok(ImprovServer { addr, stop, thread: Some(thread) })
    }

    /// The address it bound.
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Stop and join.
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

impl Drop for ImprovServer {
    fn drop(&mut self) {
        self.stop_and_join();
    }
}

fn send(s: &mut TcpStream, replies: &mut Replies, info: &DeviceInfo<'_>) -> io::Result<()> {
    let mut out = [0u8; REPLY_MAX + 1];
    for r in replies.iter() {
        let n = r
            .encode(info, &mut out[..REPLY_MAX])
            .map_err(|_| io::Error::other("improv reply did not fit"))?;
        out[n] = b'\n';
        s.write_all(&out[..=n])?;
    }
    replies.clear();
    Ok(())
}

fn serve(
    mut s: TcpStream,
    handle: &SimHandle,
    instance: &str,
    stop: &AtomicBool,
) -> io::Result<()> {
    s.set_read_timeout(Some(TICK))?;
    s.set_nodelay(true)?;
    let info = DeviceInfo { firmware: "screeny-sim", version: "sim", chip: "sim", name: instance };
    let mut parser = Parser::new();
    let mut session = Session::new();
    let mut replies = Replies::new();
    let mut buf = [0u8; 64];
    while !stop.load(Ordering::SeqCst) {
        let n = match s.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => n,
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => 0,
            Err(e) => return Err(e),
        };
        let now_ms = (handle.now_us() / 1000) as u32;
        for &b in &buf[..n] {
            let Some(parsed) = parser.push(b) else { continue };
            if let Some(sub) = session.handle(parsed, &handle.wifi_observed(), now_ms, &mut replies)
            {
                // The password stops here: the model takes the SSID only.
                handle.post_wifi(&String::from_utf8_lossy(sub.ssid));
            }
            parser.wipe();
            send(&mut s, &mut replies, &info)?;
        }
        buf.fill(0);
        session.poll(&handle.wifi_observed(), now_ms, &mut replies);
        send(&mut s, &mut replies, &info)?;
    }
    Ok(())
}
