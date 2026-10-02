//! Improv Wi-Fi over serial (card 362): the half of ESP Web Tools' install
//! dialog that asks for a network and a password.
//!
//! **The protocol is `screeny_provision::improv`** (frame parser, encoders and
//! the [`Session`]); this file is the wire. It reads UART0's receive side,
//! feeds the parser, answers on the same UART through the very printer the
//! logger uses, and routes credentials into the one join path that the portal,
//! `POST /api/v1/wifi` and `SET_WIFI` also use: [`crate::NEW_WIFI`].
//!
//! ## One wire, shared with the log
//!
//! The console is UART0 at 115200 (the ROM's rate, which is also ESP Web Tools'
//! and `espflash monitor`'s). `esp-println` writes it by register and never
//! reads it, so this task owns the receive side alone: a [`UartRx`] on GPIO3,
//! configured at 115200 so the rate is a fact of this file and not an
//! accident of the boot ROM. Replies go out through
//! [`esp_println::Printer::write_bytes`], which takes the same critical
//! section as a log line, so a frame is never torn by a log line from another
//! task. (The cost is the one every log line pays: the section is held for the
//! few milliseconds the bytes take to leave, about 5 ms for a 60-byte frame.
//! Frames are sent only in answer to the client and on a join result.)
//!
//! ## Scan: an empty answer, on purpose
//!
//! "Request scanned networks" is answered with the terminator alone, a result
//! with no strings, which the spec defines as "the scan is complete". An error
//! would make the dialog look broken; an empty list makes ESP Web Tools offer
//! the typed-SSID field it offers for hidden networks. A real scan is not free
//! here: the radio is owned by [`crate::provision`]'s task (a scan needs the
//! `WifiController`), a scan while associated stalls the station, and card 229
//! (a scan list for the portal) was dropped for exactly that scope. Revisit
//! them together.
//!
//! ## The password
//!
//! It lives in the parser's frame buffer and in this task's read buffer. Both
//! are zeroed as soon as the frame has been handed to [`crate::NEW_WIFI`]. The
//! only thing logged is the two lengths, like the HTTP forms do.

use embassy_time::{Duration, Timer};
use esp_hal::uart::{Config as UartConfig, UartRx};
use esp_hal::Blocking;
use log::{info, warn};
use screeny_provision::improv::{
    DeviceInfo, Observed, Parser, Replies, Session, REPLY_MAX,
};
use screeny_settings::Wifi;

/// How often the receive FIFO is drained.
const READ_EVERY: Duration = Duration::from_millis(10);
/// How often the session is asked about a join result.
const POLL_MS: u32 = 250;

/// The UART0 receiver, at the rate the console and ESP Web Tools use.
///
/// **Blocking driver, polled, and no `with_rx`.** The async driver and the pin
/// binding pulled ~2.3 KB of pin-signal tables into `.data` (`.stack` had 1.4 KB
/// to spare), and neither is needed: GPIO3 is U0RXD from reset, so the pin is
/// already routed, and a 128-byte FIFO polled every 10 ms cannot overflow at
/// 115200 baud (it takes 11 ms to fill).
pub fn rx(uart: esp_hal::peripherals::UART0<'static>) -> UartRx<'static, Blocking> {
    let cfg = UartConfig::default().with_baudrate(115_200);
    UartRx::new(uart, cfg).expect("uart0 config")
}

fn send(replies: &mut Replies, info: &DeviceInfo<'_>) {
    let mut out = [0u8; REPLY_MAX + 1];
    for r in replies.iter() {
        match r.encode(info, &mut out[..REPLY_MAX]) {
            Ok(n) => {
                // ESPHome ends every frame with a newline so the log stays
                // line-oriented around it.
                out[n] = b'\n';
                esp_println::Printer::write_bytes(&out[..=n]);
            }
            Err(_) => warn!("improv: a reply did not fit its buffer: {:?}", r),
        }
    }
    replies.clear();
}

/// Read UART0, speak Improv. Never returns.
#[embassy_executor::task]
pub async fn improv_task(mut rx: UartRx<'static, Blocking>, name: &'static str) {
    let info = DeviceInfo {
        firmware: "screeny-fw",
        version: crate::FW_VERSION,
        chip: "ESP32",
        name,
    };
    let mut parser = Parser::new();
    let mut session = Session::new();
    let mut replies = Replies::new();
    let mut buf = [0u8; 32];
    info!("improv: listening on uart0 at 115200");

    let mut last_poll = crate::now_ms();
    loop {
        Timer::after(READ_EVERY).await;
        // An overrun or a framing error is `Err` and costs the bytes; the
        // parser resynchronises on its own.
        let n = rx.read_buffered(&mut buf).unwrap_or(0);
        for &b in &buf[..n] {
            let Some(parsed) = parser.push(b) else { continue };
            let observed = observed();
            let now = crate::now_ms();
            if let Some(sub) = session.handle(parsed, &observed, now, &mut replies) {
                info!(
                    "improv: wifi settings received, ssid_len {} psk_len {}",
                    sub.ssid.len(),
                    sub.password.len()
                );
                match Wifi::new(sub.ssid, sub.password) {
                    Ok(wifi) => crate::NEW_WIFI.signal(crate::NewWifi {
                        wifi,
                        persist: true,
                    }),
                    Err(e) => warn!("improv: credentials refused: {:?}", e),
                }
            }
            // The frame may have held the password: do not wait for the next
            // byte to clear it.
            parser.wipe();
            send(&mut replies, &info);
        }
        if n > 0 {
            // The read buffer held it too.
            buf.fill(0);
        }
        let now = crate::now_ms();
        if now.wrapping_sub(last_poll) >= POLL_MS {
            last_poll = now;
            session.poll(&observed(), now, &mut replies);
            send(&mut replies, &info);
        }
    }
}

fn observed() -> Observed {
    crate::provision::improv_observed()
}
