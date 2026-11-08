//! The panel telling the time by itself.
//!
//! The twenty-four clocks of `crates/clock`, the same engine the Studio runs,
//! as an idle mode: nothing here competes with a sender, and when a stream
//! starts the panel simply goes to it and the clock takes up again on the
//! minute when it ends. Portal, updates and the button are overlays above this
//! like they are above everything else.
//!
//! Three things meet here. [`sntp_task`] owns the time; the settings page and
//! the store own the look, kept in [`SHARED`]; and [`Face`],
//! which lives on the frame task, reads both and draws. The frame task is on
//! core 0 with the network: a frame costs a fraction of a millisecond and the
//! display on core 1 never sees any of this, only finished frames.

use alloc::boxed::Box;
use core::cell::RefCell;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};

use critical_section::Mutex;
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{IpAddress, IpEndpoint, Ipv4Address, Stack};
use embassy_time::{with_timeout, Duration, Instant, Timer};
use log::{info, warn};
use screeny_clock::dance;
use screeny_clock::engine::{Engine, Knobs, Mode, Request, Tick};
use screeny_clock::face::RESTS;
use screeny_clock::raster::{Look, Raster, Tint};
use screeny_clock::sntp;
use screeny_clock::tz;
use screeny_settings::{ClockConfig, IdleMode, Settings};

use crate::display::Frame;

// ---------------------------------------------------------------------------
// The time
// ---------------------------------------------------------------------------

/// `wall - monotonic`, microseconds. Two halves because this chip has no 64-bit
/// atomics; both are written and read only from core 0's one executor with no
/// `await` between, so they cannot tear.
static OFFSET_HI: AtomicU32 = AtomicU32::new(0);
static OFFSET_LO: AtomicU32 = AtomicU32::new(0);
static SYNCED: AtomicBool = AtomicBool::new(false);
/// Monotonic seconds of the last good answer.
static SYNCED_AT: AtomicU32 = AtomicU32::new(0);

fn set_offset(offset: i64) {
    OFFSET_HI.store((offset >> 32) as u32, Ordering::Relaxed);
    OFFSET_LO.store(offset as u32, Ordering::Relaxed);
    SYNCED_AT.store((Instant::now().as_micros() / 1_000_000) as u32, Ordering::Relaxed);
    SYNCED.store(true, Ordering::Relaxed);
}

/// Unix microseconds at monotonic `now_us`, once a server has answered.
fn wall_us(now_us: u64) -> Option<i64> {
    if !SYNCED.load(Ordering::Relaxed) {
        return None;
    }
    let hi = OFFSET_HI.load(Ordering::Relaxed) as i64;
    let lo = OFFSET_LO.load(Ordering::Relaxed) as i64;
    Some(now_us as i64 + ((hi << 32) | lo))
}

/// Whether the clock has a time to show.
pub fn synced() -> bool {
    SYNCED.load(Ordering::Relaxed)
}

/// Seconds since a server last answered.
pub fn synced_ago_s() -> Option<u32> {
    synced().then(|| {
        let now = (Instant::now().as_micros() / 1_000_000) as u32;
        now.wrapping_sub(SYNCED_AT.load(Ordering::Relaxed))
    })
}

/// The time on the wall, as hours, minutes and seconds.
pub fn wall_hms() -> Option<(u8, u8, u8)> {
    let unix = wall_us(Instant::now().as_micros())?.div_euclid(1_000_000);
    let local = tz::local(unix).rem_euclid(86_400);
    Some(((local / 3600) as u8, (local / 60 % 60) as u8, (local % 60) as u8))
}

// ---------------------------------------------------------------------------
// The settings
// ---------------------------------------------------------------------------

/// The look, and a generation bumped by every change so that [`Face`] only
/// re-reads on a change.
static SHARED: Mutex<RefCell<(ClockConfig, u32)>> = Mutex::new(RefCell::new((ClockConfig::DEFAULT, 0)));
/// A dance asked for from the settings page: `1` a new one, `2` the last again.
static ASKED: AtomicU8 = AtomicU8::new(0);

/// Called once at boot with what flash holds.
pub fn init(settings: &Settings) {
    critical_section::with(|cs| *SHARED.borrow_ref_mut(cs) = (settings.clock.sanitised(), 1));
}

/// The look and its generation.
pub fn snapshot() -> (ClockConfig, u32) {
    critical_section::with(|cs| *SHARED.borrow_ref(cs))
}

/// Apply a new look at once; flash follows when the sliders have stopped.
pub fn set_look(config: ClockConfig) {
    critical_section::with(|cs| {
        let mut s = SHARED.borrow_ref_mut(cs);
        *s = (config.sanitised(), s.1.wrapping_add(1));
    });
    crate::store::note_dirty(crate::store::DIRTY_CLOCK);
}

/// Compose a dance and perform it now.
pub fn dance_now() {
    ASKED.store(1, Ordering::Relaxed);
}

/// Perform the last dance again.
pub fn play_again() {
    ASKED.store(2, Ordering::Relaxed);
}

/// The settings as the Studio patch's knobs.
fn knobs_of(c: &ClockConfig) -> Knobs {
    Knobs {
        fade: c.fade as f32 / 10.0,
        calm: c.calm,
        speed: c.speed as f32,
        mode: if c.mode == 0 { Mode::Art } else { Mode::Time },
        dance: (c.dance as usize).min(dance::DANCES + 1),
        hours24: c.hours24,
        rest: RESTS[(c.rest as usize).min(RESTS.len() - 1)],
        ..Knobs::default()
    }
}

fn look_of(c: &ClockConfig) -> Look {
    let light = c.light as f32 / 100.0;
    let tint = |hue: u16, chroma: u16| Tint { hue: hue as f32, chroma: chroma as f32 / 1000.0, light };
    Look {
        half: c.weight as f32 / 200.0,
        tip: c.tip as f32 / 100.0,
        tints: [tint(c.hue, c.chroma), tint(c.hue2, c.chroma2)],
        ring: c.dials as f32 / 100.0,
        dark: c.dark as usize,
    }
}

// ---------------------------------------------------------------------------
// The picture
// ---------------------------------------------------------------------------

/// A step worth a line in the log: long enough to be seen if anything moved.
const SLOW_STEP_US: u32 = 50_000;
/// How often the frame costs go to the log.
const REPORT_US: u64 = 300_000_000;

/// The clock on the frame task. Boxed and made on first use, so a panel that
/// never shows it pays nothing but this module's statics.
pub struct Face {
    engine: Engine,
    raster: Raster,
    knobs: Knobs,
    generation: u32,
    born_us: u64,
    last_t: f64,
    worst_step_us: u32,
    worst_draw_us: u32,
    reported_us: u64,
}

impl Face {
    pub fn new(now_us: u64) -> Box<Face> {
        let (config, generation) = snapshot();
        let seed = now_us ^ (OFFSET_LO.load(Ordering::Relaxed) as u64).rotate_left(21);
        info!("clock: starting");
        Box::new(Face {
            engine: Engine::new(seed),
            raster: Raster::new(look_of(&config)),
            knobs: knobs_of(&config),
            generation,
            born_us: now_us,
            last_t: 0.0,
            worst_step_us: 0,
            worst_draw_us: 0,
            reported_us: now_us,
        })
    }

    /// Draw the clock into `frame`. `false` while there is no time to show,
    /// and the caller shows the status screen instead.
    pub fn draw(&mut self, frame: &mut Frame, now_us: u64) -> bool {
        let Some(wall) = wall_us(now_us) else {
            return false;
        };
        let (config, generation) = snapshot();
        if generation != self.generation {
            self.raster.set_look(look_of(&config));
            self.knobs = knobs_of(&config);
            self.generation = generation;
        }
        match ASKED.swap(0, Ordering::Relaxed) {
            1 => self.engine.ask(Request::Another),
            2 => self.engine.ask(Request::Again),
            _ => {}
        }

        let unix = wall.div_euclid(1_000_000);
        let now = tz::local(unix) as f64 + wall.rem_euclid(1_000_000) as f64 / 1e6;
        let t = (now_us - self.born_us) as f64 / 1e6;
        let dt = (t - self.last_t).max(0.0);
        self.last_t = t;

        let began = Instant::now();
        self.engine.step(Tick { t, dt, now }, &self.knobs);
        let stepped = Instant::now();
        let (settled, dims) = (self.engine.settled(), self.engine.dims());
        self.raster.draw(&mut frame.px, self.engine.angles(), dims, self.knobs.rest, settled);
        let drawn = Instant::now();

        let step_us = (stepped - began).as_micros() as u32;
        // At most about once a minute: a dance is composed once a minute.
        if step_us > SLOW_STEP_US {
            info!("clock: slow step {} us at {:.2} settled", step_us, settled);
        }
        self.worst_step_us = self.worst_step_us.max(step_us);
        self.worst_draw_us = self.worst_draw_us.max((drawn - stepped).as_micros() as u32);
        if now_us.wrapping_sub(self.reported_us) >= REPORT_US {
            info!(
                "clock: worst step {} us, worst draw {} us over 5 min; heap {} free",
                self.worst_step_us,
                self.worst_draw_us,
                esp_alloc::HEAP.free()
            );
            self.reported_us = now_us;
            self.worst_step_us = 0;
            self.worst_draw_us = 0;
        }
        true
    }
}

// ---------------------------------------------------------------------------
// Network time
// ---------------------------------------------------------------------------

/// Tried in order: time.google.com and time.cloudflare.com, by address. Both are
/// stable anycast addresses, and no DNS means no DNS code and one socket fewer
/// in RAM this chip does not have spare.
const SERVERS: [Ipv4Address; 2] = [Ipv4Address::new(216, 239, 35, 0), Ipv4Address::new(162, 159, 200, 1)];

/// A crystal is good for a few seconds a day, and the hour is plenty.
const EVERY: Duration = Duration::from_secs(3600);
const RETRY_FIRST: Duration = Duration::from_secs(5);
const RETRY_MOST: Duration = Duration::from_secs(300);

/// Ask one server. `true` if the clock was set.
async fn ask(socket: &mut UdpSocket<'_>, server: IpAddress, nonce: u64) -> bool {
    let mut packet = Box::new([0u8; sntp::PACKET]);
    sntp::request(&mut packet, nonce);
    let to = IpEndpoint::new(server, sntp::PORT);
    let sent = Instant::now().as_micros();
    match with_timeout(Duration::from_secs(1), socket.send_to(&packet[..], to)).await {
        Ok(Ok(())) => {}
        _ => return false,
    }
    let deadline = Duration::from_secs(2);
    let Ok(Ok((n, from))) = with_timeout(deadline, socket.recv_from(&mut packet[..])).await else {
        return false;
    };
    let got = Instant::now().as_micros();
    if from.endpoint.addr != server {
        return false;
    }
    match sntp::offset(&packet[..n], nonce, sent, got) {
        Ok(offset) => {
            let was = wall_us(got);
            set_offset(offset);
            match was {
                Some(w) => info!("clock: set by {}, {} ms out", server, (offset + got as i64 - w) / 1000),
                None => info!("clock: set by {}", server),
            }
            true
        }
        Err(why) => {
            warn!("clock: {} refused: {:?}", server, why);
            false
        }
    }
}

/// The exchange's buffers: on the heap for the few milliseconds they are used,
/// because in this task's future they would be `.bss`, and `.bss` is core 0's
/// stack.
struct Buffers {
    rx_meta: [PacketMetadata; 2],
    tx_meta: [PacketMetadata; 2],
    rx: [u8; 128],
    tx: [u8; 64],
}

async fn sync(stack: Stack<'static>, nonce: u64) -> bool {
    let mut b = Box::new(Buffers {
        rx_meta: [PacketMetadata::EMPTY; 2],
        tx_meta: [PacketMetadata::EMPTY; 2],
        rx: [0; 128],
        tx: [0; 64],
    });
    let b = &mut *b;
    let mut socket = UdpSocket::new(stack, &mut b.rx_meta, &mut b.rx, &mut b.tx_meta, &mut b.tx);
    if socket.bind(0).is_err() {
        warn!("clock: no port to ask from");
        return false;
    }
    for address in SERVERS {
        if ask(&mut socket, IpAddress::Ipv4(address), nonce).await {
            return true;
        }
    }
    false
}

/// Keeps the time, on a panel that is showing it. A panel in any other idle
/// mode sends nothing: time servers are somebody else's, and a frame buffer has
/// no business asking them.
#[embassy_executor::task]
pub async fn sntp_task(stack: Stack<'static>) {
    let mut retry = RETRY_FIRST;
    let mut asked = 0u64;
    loop {
        stack.wait_config_up().await;
        let wanted = {
            let guard = crate::net::CORE.lock().await;
            guard.as_ref().is_some_and(|core| core.idle_mode() == IdleMode::Clock)
        };
        if !wanted {
            Timer::after(RETRY_FIRST).await;
            continue;
        }
        asked += 1;
        let nonce = Instant::now().as_micros() ^ asked.rotate_left(48) ^ 0x5eed_c10c_0000_0000;
        // Boxed: a task's future is `.bss`, and this one is only alive for a moment.
        if Box::pin(sync(stack, nonce)).await {
            retry = RETRY_FIRST;
            Timer::after(EVERY).await;
        } else {
            warn!("clock: no time server answered; again in {} s", retry.as_secs());
            Timer::after(retry).await;
            retry = (retry * 2).min(RETRY_MOST);
        }
    }
}
