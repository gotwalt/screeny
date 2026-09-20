//! The engine: one piece, rendered off the wall clock on its own thread.
//!
//! Unchanged in substance from the desktop app (card 101): the engine owns
//! time and rendering, and whatever is watching - a window then, a browser
//! now - only ever reads the newest frame. It knows nothing about HTTP.

use screeny_art::output::{Output, PanelStatus, SenderOutput};
use screeny_art::piece::{local_now, Ctx, Params, PieceDef, Playing};
use screeny_art::{pieces, Piece, Pipeline, Settings, N};
use serde::Serialize;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Frames per second the engine may run at. The panel is assumed to take 60;
/// 30 is there to see what a piece looks like at the measured rate.
pub const RATES: [f64; 2] = [30.0, 60.0];
/// Frame packet header size; see [`Engine::tick`] and `ui/main.js`.
pub const HEADER: usize = 52;
/// Bytes in one frame packet: the header and then `N` sRGB triples.
pub const PACKET_BYTES: usize = HEADER + N * 3;

pub struct Engine {
    def: &'static PieceDef,
    piece: Box<dyn Piece>,
    params: Params,
    seed: u32,
    pipeline: Pipeline,
    t: f64,
    last_tick: Instant,
    paused: bool,
    speed: f64,
    rate: f64,
    seq: u32,
    fps: f32,
    /// The newest frame packet, shared rather than copied: every browser
    /// watching holds the same allocation until the next tick replaces it.
    packet: Arc<Vec<u8>>,
    /// Set when the "send to panel" switch is on. All of the behaviour is in
    /// `screeny_art::output`; this is a field and four lines in `tick`.
    panel: Option<SenderOutput>,
    /// When the meter last took the connected device's budget and codec set.
    limits_at: Instant,
}

impl Engine {
    #[must_use]
    pub fn new() -> Self {
        let def = &pieces::ALL[0];
        let seed = fresh_seed();
        log_seed(def, seed);
        Engine {
            def,
            piece: (def.make)(seed as u64),
            params: Params::defaults(def.params),
            seed,
            pipeline: Pipeline::default(),
            t: 0.0,
            last_tick: Instant::now(),
            paused: false,
            speed: 1.0,
            rate: 60.0,
            seq: 0,
            fps: 0.0,
            packet: Arc::new(vec![0; PACKET_BYTES]),
            panel: None,
            limits_at: Instant::now(),
        }
    }

    /// Rebuild the piece from its seed and start its clock again.
    fn rebuild(&mut self) {
        log_seed(self.def, self.seed);
        self.piece = (self.def.make)(self.seed as u64);
        self.pipeline.reset();
        self.t = 0.0;
    }

    /// The rate the engine loop should run at right now.
    #[must_use]
    pub fn rate(&self) -> f64 {
        self.rate
    }

    /// The newest frame packet.
    #[must_use]
    pub fn packet(&self) -> Arc<Vec<u8>> {
        Arc::clone(&self.packet)
    }

    pub fn tick(&mut self) {
        let now = Instant::now();
        let wall = now.duration_since(self.last_tick).as_secs_f64();
        self.last_tick = now;
        let dt = if self.paused { 0.0 } else { wall * self.speed };
        self.t += dt;

        let frame = self.piece.render(&Ctx { t: self.t, dt, now: local_now(), params: &self.params });
        let out = self.pipeline.process(frame, wall);

        // The panel, if the switch is on. The link owns the cadence, so the
        // engine's rate stays the engine's business; a frame it has no slot
        // for is folded away and counted, not sent.
        if let Some(panel) = self.panel.as_mut() {
            // Measure against the device actually connected, once a second.
            if self.limits_at.elapsed() >= Duration::from_secs(1) {
                self.limits_at = Instant::now();
                let lim = panel.limits();
                if lim.connected {
                    self.pipeline.meter().set_limits(lim.budget, lim.codecs.clone());
                }
            }
            // The only errors are a malformed frame, which the pipeline
            // cannot produce; the network cannot fail a send.
            if let Err(e) = panel.send(&out.wire) {
                eprintln!("studio: sending to the panel: {e}");
            }
        }

        if wall > 0.0 {
            self.fps += (1.0 / wall as f32 - self.fps) * 0.1;
        }
        self.seq = self.seq.wrapping_add(1);
        let s = out.stats;
        let mut p = Vec::with_capacity(PACKET_BYTES);
        p.extend_from_slice(&self.seq.to_le_bytes());
        p.extend_from_slice(&(self.t as f32).to_le_bytes());
        p.extend_from_slice(&s.distinct_colours.to_le_bytes());
        p.extend_from_slice(&s.encoded_bytes.to_le_bytes());
        p.extend_from_slice(&u32::from(s.codec).to_le_bytes());
        p.extend_from_slice(&u32::from(s.exact).to_le_bytes());
        for v in [s.apl, s.apl_in, s.luma, s.dluma, s.dluma_peak, s.limiter_gain, self.fps] {
            p.extend_from_slice(&v.to_le_bytes());
        }
        debug_assert_eq!(p.len(), HEADER);
        p.extend_from_slice(&out.preview);
        self.packet = Arc::new(p);
    }

    #[must_use]
    pub fn snapshot(&self) -> StudioState {
        StudioState {
            piece: self.def.id,
            seed: self.seed,
            params: self.params.iter().collect(),
            settings: self.pipeline.settings,
            paused: self.paused,
            speed: self.speed,
            fps: self.rate,
        }
    }

    // ---- what the API does to the engine. One method per command. ----

    /// # Errors
    ///
    /// If no piece has that id.
    pub fn set_piece(&mut self, id: &str) -> Result<(), String> {
        let def = screeny_art::piece::find(id).ok_or_else(|| format!("no piece called `{id}`"))?;
        self.def = def;
        self.params = Params::defaults(def.params);
        self.rebuild();
        Ok(())
    }

    /// # Errors
    ///
    /// If the current piece has no such parameter.
    pub fn set_param(&mut self, id: &str, value: f32) -> Result<(), String> {
        let specs = self.def.params;
        if self.params.set(specs, id, value) {
            Ok(())
        } else {
            Err(format!("{} has no parameter `{id}`", self.def.id))
        }
    }

    pub fn reset_params(&mut self) {
        self.params = Params::defaults(self.def.params);
    }

    /// `seed: None` picks a new one.
    pub fn set_seed(&mut self, seed: Option<u32>) {
        self.seed = seed.unwrap_or_else(fresh_seed);
        self.rebuild();
    }

    pub fn set_settings(&mut self, settings: Settings) {
        self.pipeline.settings = settings;
    }

    pub fn set_playback(&mut self, paused: bool, speed: f64, fps: f64) {
        self.paused = paused;
        self.speed = speed.clamp(0.0, 8.0);
        self.rate = if RATES.contains(&fps) { fps } else { self.rate };
    }

    /// What the piece says it is performing, if it is the kind that composes.
    #[must_use]
    pub fn playing(&self) -> Option<Playing> {
        self.piece.playing()
    }

    pub fn act(&mut self, action: &str) -> Option<Playing> {
        self.piece.act(action);
        self.piece.playing()
    }

    pub fn restart(&mut self) {
        self.rebuild();
    }

    /// Turn "send to panel" on or off.
    ///
    /// `to` is an mDNS instance name or an `IP[:PORT]`; empty means "the first
    /// panel found". Deliberately deferred rather than blocking: a request
    /// should not hang for three seconds while a browse runs, and a panel that
    /// is off right now is not a different case from one unplugged later.
    /// Turning it off drops the link, which sends `FINAL` and releases the
    /// panel at once.
    pub fn set_panel(&mut self, on: bool, to: &str) -> Option<PanelStatus> {
        self.panel = on.then(|| SenderOutput::deferred(screeny_art::output::target_for(to)));
        self.limits_at = Instant::now() - Duration::from_secs(10);
        self.panel.as_ref().map(SenderOutput::status)
    }

    /// Link state, device, frame counters and the indexed exact/fallback
    /// split, for the status strip. `None` when the switch is off.
    pub fn panel_status(&mut self) -> Option<PanelStatus> {
        // Drive reconnection even if no frames are flowing.
        if let Some(p) = self.panel.as_mut() {
            p.poll();
        }
        self.panel.as_ref().map(SenderOutput::status)
    }

    /// Send `FINAL` so the panel is released the moment the server stops,
    /// rather than after the device's stream timeout.
    pub fn close_panel(&mut self) {
        if let Some(p) = self.panel.as_mut() {
            p.close();
        }
        self.panel = None;
    }
}

impl Default for Engine {
    fn default() -> Self {
        Engine::new()
    }
}

fn fresh_seed() -> u32 {
    // Small enough to read out loud and type back in.
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(1, |d| d.subsec_nanos() ^ d.as_secs() as u32) % 1_000_000
}

fn log_seed(def: &PieceDef, seed: u32) {
    eprintln!("studio: piece={} seed={seed}", def.id);
}

pub type Shared = Arc<Mutex<Engine>>;

/// A piece that panicked mid-render poisons the lock; the state is still
/// usable, so the studio carries on rather than taking the process with it.
pub fn lock(engine: &Shared) -> MutexGuard<'_, Engine> {
    engine.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Serialize)]
pub struct ParamInfo {
    pub id: &'static str,
    pub label: &'static str,
    pub min: f32,
    pub max: f32,
    pub step: f32,
    pub default: f32,
}

#[derive(Serialize)]
pub struct PieceInfo {
    pub id: &'static str,
    pub name: &'static str,
    pub blurb: &'static str,
    pub params: Vec<ParamInfo>,
}

#[derive(Clone, Serialize)]
pub struct StudioState {
    pub piece: &'static str,
    pub seed: u32,
    pub params: std::collections::BTreeMap<&'static str, f32>,
    pub settings: Settings,
    pub paused: bool,
    pub speed: f64,
    pub fps: f64,
}

/// Everything the UI needs to draw itself once.
#[derive(Serialize)]
pub struct Bootstrap {
    pub pieces: Vec<PieceInfo>,
    pub payload_bytes: u32,
    pub state: StudioState,
}

#[must_use]
pub fn bootstrap(engine: &Shared) -> Bootstrap {
    let pieces = pieces::ALL
        .iter()
        .map(|d| PieceInfo {
            id: d.id,
            name: d.name,
            blurb: d.blurb,
            params: d
                .params
                .iter()
                .map(|p| ParamInfo { id: p.id, label: p.label, min: p.min, max: p.max, step: p.step, default: p.default })
                .collect(),
        })
        .collect();
    Bootstrap { pieces, payload_bytes: screeny_art::meter::PAYLOAD_BYTES, state: lock(engine).snapshot() }
}
