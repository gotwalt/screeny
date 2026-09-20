//! Screeny Studio: a desktop window onto the `screeny-art` pipeline.
//!
//! An engine thread renders the current piece at 30 fps off the wall clock,
//! exactly as the headless runner will. The window polls for the newest frame
//! and draws it as LEDs; it never drives the timing.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use screeny_art::meter::PAYLOAD_BYTES;
use screeny_art::output::{target_for, Output, PanelStatus, SenderOutput};
use screeny_art::piece::{local_now, Ctx, Params, PieceDef};
use screeny_art::{pieces, Piece, Pipeline, Settings, N};
use serde::Serialize;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::State;

/// Frames per second the engine may run at. The panel is assumed to take 60;
/// 30 is there to see what a piece looks like at the measured rate.
const RATES: [f64; 2] = [30.0, 60.0];
/// Frame packet header size; see `Engine::tick` and `ui/main.js`.
const HEADER: usize = 52;

struct Engine {
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
    packet: Vec<u8>,
    /// Set when the "send to panel" switch is on. All of the behaviour is in
    /// `screeny_art::output`; this is a field and four lines in `tick`.
    panel: Option<SenderOutput>,
    /// When the meter last took the connected device's budget and codec set.
    limits_at: Instant,
}

impl Engine {
    fn new() -> Self {
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
            packet: vec![0; HEADER + N * 3],
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

    fn tick(&mut self) {
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
        let mut p = Vec::with_capacity(HEADER + N * 3);
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
        self.packet = p;
    }

    fn snapshot(&self) -> StudioState {
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
}

fn fresh_seed() -> u32 {
    // Small enough to read out loud and type back in.
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(1, |d| d.subsec_nanos() ^ d.as_secs() as u32) % 1_000_000
}

fn log_seed(def: &PieceDef, seed: u32) {
    eprintln!("studio: piece={} seed={seed}", def.id);
}

type Shared = Arc<Mutex<Engine>>;

fn lock(engine: &Shared) -> MutexGuard<'_, Engine> {
    // A piece that panicked mid-render poisons the lock; the state is still usable.
    engine.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Serialize)]
struct ParamInfo {
    id: &'static str,
    label: &'static str,
    min: f32,
    max: f32,
    step: f32,
    default: f32,
}

#[derive(Serialize)]
struct PieceInfo {
    id: &'static str,
    name: &'static str,
    blurb: &'static str,
    params: Vec<ParamInfo>,
}

#[derive(Serialize)]
struct StudioState {
    piece: &'static str,
    seed: u32,
    params: std::collections::BTreeMap<&'static str, f32>,
    settings: Settings,
    paused: bool,
    speed: f64,
    fps: f64,
}

#[derive(Serialize)]
struct Bootstrap {
    pieces: Vec<PieceInfo>,
    payload_bytes: u32,
    state: StudioState,
}

#[tauri::command]
fn bootstrap(engine: State<Shared>) -> Bootstrap {
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
    Bootstrap { pieces, payload_bytes: PAYLOAD_BYTES, state: lock(&engine).snapshot() }
}

#[tauri::command]
fn frame(engine: State<Shared>) -> tauri::ipc::Response {
    tauri::ipc::Response::new(lock(&engine).packet.clone())
}

#[tauri::command]
fn set_piece(engine: State<Shared>, id: String) -> Result<StudioState, String> {
    let def = screeny_art::piece::find(&id).ok_or(format!("no piece called `{id}`"))?;
    let mut e = lock(&engine);
    e.def = def;
    e.params = Params::defaults(def.params);
    e.rebuild();
    Ok(e.snapshot())
}

#[tauri::command]
fn set_param(engine: State<Shared>, id: String, value: f32) -> Result<(), String> {
    let mut e = lock(&engine);
    let specs = e.def.params;
    if e.params.set(specs, &id, value) {
        Ok(())
    } else {
        Err(format!("{} has no parameter `{id}`", e.def.id))
    }
}

#[tauri::command]
fn reset_params(engine: State<Shared>) -> StudioState {
    let mut e = lock(&engine);
    e.params = Params::defaults(e.def.params);
    e.snapshot()
}

/// `seed: None` picks a new one.
#[tauri::command]
fn set_seed(engine: State<Shared>, seed: Option<u32>) -> StudioState {
    let mut e = lock(&engine);
    e.seed = seed.unwrap_or_else(fresh_seed);
    e.rebuild();
    e.snapshot()
}

#[tauri::command]
fn set_settings(engine: State<Shared>, settings: Settings) {
    lock(&engine).pipeline.settings = settings;
}

#[tauri::command]
fn set_playback(engine: State<Shared>, paused: bool, speed: f64, fps: f64) {
    let mut e = lock(&engine);
    e.paused = paused;
    e.speed = speed.clamp(0.0, 8.0);
    e.rate = if RATES.contains(&fps) { fps } else { e.rate };
}

/// What the piece says it is performing, if it is the kind that composes.
#[tauri::command]
fn piece_playing(engine: State<Shared>) -> Option<screeny_art::piece::Playing> {
    lock(&engine).piece.playing()
}

#[tauri::command]
fn piece_act(engine: State<Shared>, action: String) -> Option<screeny_art::piece::Playing> {
    let mut e = lock(&engine);
    e.piece.act(&action);
    e.piece.playing()
}

#[tauri::command]
fn restart(engine: State<Shared>) {
    lock(&engine).rebuild();
}

/// Turn "send to panel" on or off.
///
/// `to` is an mDNS instance name or an `IP[:PORT]`; empty means "the first
/// panel found". Deliberately deferred rather than blocking: the studio should
/// not freeze for three seconds while a browse runs, and a panel that is off
/// right now is not a different case from one unplugged later. Turning it off
/// drops the link, which sends `FINAL` and releases the panel at once.
#[tauri::command]
fn set_panel(engine: State<Shared>, on: bool, to: String) -> Option<PanelStatus> {
    let mut e = lock(&engine);
    e.panel = on.then(|| SenderOutput::deferred(target_for(&to)));
    e.limits_at = Instant::now() - Duration::from_secs(10);
    e.panel.as_ref().map(SenderOutput::status)
}

/// Link state, device, frame counters and the indexed exact/fallback split,
/// for the status strip. `None` when the switch is off.
#[tauri::command]
fn panel_status(engine: State<Shared>) -> Option<PanelStatus> {
    let mut e = lock(&engine);
    // Drive reconnection even while the engine is paused and no frames flow.
    if let Some(p) = e.panel.as_mut() {
        p.poll();
    }
    e.panel.as_ref().map(SenderOutput::status)
}

fn main() {
    let engine: Shared = Arc::new(Mutex::new(Engine::new()));

    let ticker = engine.clone();
    std::thread::Builder::new()
        .name("engine".into())
        .spawn(move || {
            let mut next = Instant::now();
            loop {
                // A panicking piece reports itself on stderr; keep the clock running.
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| lock(&ticker).tick()));
                next += Duration::from_secs_f64(1.0 / lock(&ticker).rate);
                let now = Instant::now();
                if next > now {
                    std::thread::sleep(next - now);
                } else {
                    next = now;
                }
            }
        })
        .expect("spawn engine thread");

    tauri::Builder::default()
        .manage(engine)
        .invoke_handler(tauri::generate_handler![
            bootstrap,
            frame,
            set_piece,
            set_param,
            reset_params,
            set_seed,
            set_settings,
            set_playback,
            piece_playing,
            piece_act,
            restart,
            set_panel,
            panel_status
        ])
        .run(tauri::generate_context!())
        .expect("run Screeny Studio");
}
