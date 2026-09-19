//! `screeny-sim` - a fake Tidbyt on the network.
//!
//! ```text
//! screeny-sim                                  # window, mDNS, default ports
//! screeny-sim --headless --exit-after 30
//! screeny-sim --drop 5 --decode-ms 20          # a bad day, on purpose
//! screeny-sim --headless --dump-dir /tmp/f --dump-every 30
//! screeny-sim --help
//! ```

use std::io::Write;
use std::net::IpAddr;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use screeny_proto::control::IdleMode;
use screeny_sim::config::{PanelModel, Timing};
use screeny_sim::dump::Dumper;
use screeny_sim::event::{DropCause, Event};
use screeny_sim::{Config, SimDevice, SimHandle};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let opts = match Opts::parse(&args) {
        Ok(Some(o)) => o,
        Ok(None) => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("screeny-sim: {e}\n\nTry --help.");
            return ExitCode::FAILURE;
        }
    };

    match run(opts) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("screeny-sim: {e}");
            ExitCode::FAILURE
        }
    }
}

const USAGE: &str = "\
screeny-sim - a fake screeny panel that speaks protocol v1 over UDP

USAGE:
    screeny-sim [OPTIONS]

NETWORK
    --bind ADDR            address to bind both sockets to [0.0.0.0]
    --frame-port N         frame port, 0 for an ephemeral one [49374]
    --control-port N       control port, 0 for an ephemeral one [49375]
    --no-mdns              do not advertise over mDNS
    --instance NAME        mDNS instance name [screeny-sim]
                           (never `screeny`: that is the real device)
    --name NAME            friendly name, the name= TXT key
    --id HEX               short device id, the id= TXT key

DISPLAY
    --headless             no window; log statistics once a second
    --scale N              window upscale, >= 4 [14]
    --levels N             panel duty levels per channel [64]
    --brightness N         starting brightness, 0-255 [255]
    --brightness-cap N     the cap SET_BRIGHTNESS clamps to [255]
    --idle MODE            status | hold | dim | black [status]
    --rssi DBM             signal strength to report [-55]

FAULT INJECTION
    --drop PCT             discard this percentage of arriving frames
    --delay-ms MS          hold each frame a random 0..MS before processing
    --decode-ms MS         pretend decoding takes this long
    --fault-seed N         seed for the fault RNG, so a run repeats

RECORDING
    --dump-dir PATH        write displayed frames as 64x32 PNGs
    --dump-every N         write one frame in every N [1]

OTHER
    --exit-after SECS      stop after this many seconds
    --verbose              log every event, not just the once-a-second line
    --quiet                log nothing but errors
    -h, --help             this
";

struct Opts {
    cfg: Config,
    headless: bool,
    scale: usize,
    dump_dir: Option<String>,
    dump_every: u32,
    exit_after: Option<f64>,
    verbose: bool,
    quiet: bool,
}

impl Opts {
    /// `Ok(None)` means `--help` was asked for.
    fn parse(args: &[String]) -> Result<Option<Opts>, String> {
        let mut o = Opts {
            cfg: Config {
                mdns: true,
                ..Config::default()
            },
            headless: false,
            scale: 14,
            dump_dir: None,
            dump_every: 1,
            exit_after: None,
            verbose: false,
            quiet: false,
        };
        let mut it = args.iter().peekable();
        while let Some(a) = it.next() {
            let mut value = || -> Result<String, String> {
                it.next()
                    .cloned()
                    .ok_or_else(|| format!("{a} needs a value"))
            };
            match a.as_str() {
                "-h" | "--help" => return Ok(None),
                "--headless" => o.headless = true,
                "--no-mdns" => o.cfg.mdns = false,
                "--verbose" => o.verbose = true,
                "--quiet" => o.quiet = true,
                "--bind" => {
                    o.cfg.bind = value()?
                        .parse::<IpAddr>()
                        .map_err(|e| format!("--bind: {e}"))?
                }
                "--frame-port" => o.cfg.frame_port = num(&value()?, "--frame-port")?,
                "--control-port" => o.cfg.control_port = num(&value()?, "--control-port")?,
                "--instance" => o.cfg.instance = value()?,
                "--name" => o.cfg.name = value()?,
                "--id" => o.cfg.id = value()?,
                "--scale" => o.scale = num::<usize>(&value()?, "--scale")?.max(4),
                "--levels" => {
                    o.cfg.panel = PanelModel {
                        levels: num(&value()?, "--levels")?,
                    }
                }
                "--brightness" => o.cfg.brightness = num(&value()?, "--brightness")?,
                "--brightness-cap" => o.cfg.brightness_cap = num(&value()?, "--brightness-cap")?,
                "--rssi" => o.cfg.rssi_dbm = num(&value()?, "--rssi")?,
                "--idle" => {
                    o.cfg.idle_mode = match value()?.as_str() {
                        "status" => IdleMode::Status,
                        "hold" => IdleMode::HoldForever,
                        "dim" => IdleMode::Dim,
                        "black" => IdleMode::Black,
                        other => return Err(format!("--idle: unknown mode {other:?}")),
                    }
                }
                "--drop" => o.cfg.faults.drop_pct = num(&value()?, "--drop")?,
                "--delay-ms" => o.cfg.faults.delay_ms = num(&value()?, "--delay-ms")?,
                "--decode-ms" => o.cfg.faults.decode_ms = num(&value()?, "--decode-ms")?,
                "--fault-seed" => o.cfg.fault_seed = num(&value()?, "--fault-seed")?,
                "--dump-dir" => o.dump_dir = Some(value()?),
                "--dump-every" => o.dump_every = num::<u32>(&value()?, "--dump-every")?.max(1),
                "--exit-after" => o.exit_after = Some(num(&value()?, "--exit-after")?),
                other => return Err(format!("unknown option {other:?}")),
            }
        }
        o.cfg.timing = Timing::SPEC;
        if !(0.0..=100.0).contains(&o.cfg.faults.drop_pct) {
            return Err("--drop must be between 0 and 100".into());
        }
        if o.cfg.panel.levels < 2 {
            return Err("--levels must be at least 2".into());
        }
        if screeny_sim::instance_is_reserved(&o.cfg.instance) {
            return Err(format!(
                "--instance {:?} is reserved for the real device on this bench; \
                 pick something else (the default is {:?})",
                o.cfg.instance,
                screeny_sim::DEFAULT_INSTANCE
            ));
        }
        Ok(Some(o))
    }
}

fn num<T: std::str::FromStr>(s: &str, what: &str) -> Result<T, String>
where
    T::Err: std::fmt::Display,
{
    s.parse().map_err(|e| format!("{what}: {e}"))
}

fn run(opts: Opts) -> Result<(), String> {
    let dumper = match &opts.dump_dir {
        Some(dir) => Some(Arc::new(Mutex::new(
            Dumper::new(dir, opts.dump_every).map_err(|e| format!("--dump-dir {dir}: {e}"))?,
        ))),
        None => None,
    };
    let sink = dumper.as_ref().map(|d| {
        let d = Arc::clone(d);
        Box::new(move |frame: &screeny_proto::Rgb888Frame, _: &screeny_sim::FrameMeta| {
            if let Err(e) = d.lock().unwrap().offer(frame) {
                eprintln!("screeny-sim: dump: {e}");
            }
        }) as screeny_sim::FrameSink
    });

    let headless = opts.headless;
    let quiet = opts.quiet;
    let verbose = opts.verbose;
    let scale = opts.scale;
    let exit_after = opts.exit_after.map(Duration::from_secs_f64);
    let faults = opts.cfg.faults;
    let mdns = opts.cfg.mdns;
    let instance = opts.cfg.instance.clone();

    let dev = SimDevice::start_with(opts.cfg, sink).map_err(|e| e.to_string())?;
    let sim = dev.handle();

    if !quiet {
        println!(
            "screeny-sim: frames on {}, control on {}",
            dev.frame_addr(),
            dev.control_addr()
        );
        if mdns {
            println!("screeny-sim: advertising _screeny._udp as {instance:?}");
        }
        if !faults.is_clean() {
            println!(
                "screeny-sim: faults: drop {}%, delay 0..{} ms, decode {} ms",
                faults.drop_pct, faults.delay_ms, faults.decode_ms
            );
        }
    }

    let started = Instant::now();
    let mut reporter = Reporter::new(&sim, verbose);

    if headless {
        while exit_after.is_none_or(|d| started.elapsed() < d) {
            reporter.pump(&sim, quiet);
            std::thread::sleep(Duration::from_millis(20));
        }
    } else {
        #[cfg(feature = "window")]
        window_loop(&sim, scale, exit_after, started, &mut reporter, quiet)?;
        #[cfg(not(feature = "window"))]
        {
            let _ = scale;
            return Err(
                "built without the `window` feature; run with --headless".into(),
            );
        }
    }

    if let Some(d) = &dumper {
        if !quiet {
            println!("screeny-sim: wrote {} PNGs", d.lock().unwrap().written());
        }
    }
    dev.shutdown();
    Ok(())
}

#[cfg(feature = "window")]
fn window_loop(
    sim: &SimHandle,
    scale: usize,
    exit_after: Option<Duration>,
    started: Instant,
    reporter: &mut Reporter,
    quiet: bool,
) -> Result<(), String> {
    use minifb::{Key, Window, WindowOptions};
    use screeny_sim::window::Led;

    let mut led = Led::new(scale);
    let mut win = Window::new(
        "screeny-sim",
        led.width(),
        led.height(),
        WindowOptions::default(),
    )
    .map_err(|e| format!("cannot open a window: {e} (try --headless)"))?;
    win.set_target_fps(60);

    while win.is_open() && !win.is_key_down(Key::Escape) {
        if exit_after.is_some_and(|d| started.elapsed() >= d) {
            break;
        }
        let stats = reporter.pump(sim, quiet);
        let frame = sim.render_display();
        led.draw(&frame);
        led.clear_overlay();
        for (i, (text, colour)) in stats.overlay().iter().enumerate() {
            led.overlay_text(i, 8, text, *colour);
        }
        win.update_with_buffer(&led.buf, led.width(), led.height())
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Logging
// ---------------------------------------------------------------------------

/// Turns the event stream and the counters into a once-a-second line, and
/// into the three lines under the window.
struct Reporter {
    cursor: usize,
    verbose: bool,
    last_report: Instant,
    last_shown: u32,
    fps: f32,
}

/// What the overlay and the log line both want.
struct Line {
    fps: f32,
    codec: u8,
    bytes: usize,
    state: &'static str,
    source: String,
    t: screeny_proto::control::Telemetry,
}

impl Line {
    fn overlay(&self) -> Vec<(String, u32)> {
        let t = &self.t;
        vec![
            (
                format!(
                    "{:>5.1} fps  {:<8}  {:>4} B  {}",
                    self.fps,
                    codec_name(self.codec),
                    self.bytes,
                    self.state
                ),
                0xF0_F0_F0,
            ),
            (
                format!(
                    "rx {}  shown {}  gaps {}",
                    t.frames_rx, t.frames_shown, t.seq_gaps
                ),
                0x9A_D8_FF,
            ),
            (
                format!(
                    "stale {}  super {}  dec {}  rej {}",
                    t.frames_dropped_stale,
                    t.frames_dropped_superseded,
                    t.frames_dropped_decode,
                    t.frames_rejected
                ),
                if t.frames_dropped_decode > 0 || t.frames_rejected > 0 {
                    0xFF_A8_60
                } else {
                    0x80_90_A0
                },
            ),
            (format!("src {}", self.source), 0x80_90_A0),
        ]
    }

    fn log(&self) -> String {
        let t = &self.t;
        format!(
            "{:>5.1} fps  {:<8} {:>4} B  {}  rx {} shown {} gaps {} \
             stale {} super {} dec {} rej {}  jit {} us  src {}",
            self.fps,
            codec_name(self.codec),
            self.bytes,
            self.state,
            t.frames_rx,
            t.frames_shown,
            t.seq_gaps,
            t.frames_dropped_stale,
            t.frames_dropped_superseded,
            t.frames_dropped_decode,
            t.frames_rejected,
            t.jitter_us,
            self.source,
        )
    }
}

impl Reporter {
    fn new(sim: &SimHandle, verbose: bool) -> Self {
        Reporter {
            cursor: sim.event_cursor(),
            verbose,
            last_report: Instant::now(),
            last_shown: 0,
            fps: 0.0,
        }
    }

    /// Drain events, and emit the once-a-second line when it is due.
    fn pump(&mut self, sim: &SimHandle, quiet: bool) -> Line {
        let (cursor, events) = sim.events(self.cursor);
        self.cursor = cursor;
        if self.verbose && !quiet {
            for e in &events {
                if let Some(s) = describe(e) {
                    println!("  {s}");
                }
            }
        } else if !quiet {
            // Even without --verbose, the things a person would want to know
            // about get a line.
            for e in &events {
                if let Some(s) = describe_important(e) {
                    println!("  {s}");
                }
            }
        }

        let s = sim.snapshot();
        let elapsed = self.last_report.elapsed();
        if elapsed >= Duration::from_secs(1) {
            let d = s.telemetry.frames_shown.wrapping_sub(self.last_shown);
            self.fps = d as f32 / elapsed.as_secs_f32();
            self.last_shown = s.telemetry.frames_shown;
            self.last_report = Instant::now();
            let line = self.line(&s);
            if !quiet {
                println!("{}", line.log());
                let _ = std::io::stdout().flush();
            }
            return line;
        }
        self.line(&s)
    }

    fn line(&self, s: &screeny_sim::Snapshot) -> Line {
        Line {
            fps: self.fps,
            codec: s.shown.map(|m| m.codec).unwrap_or(0),
            bytes: s.shown.map(|m| m.bytes).unwrap_or(0),
            state: state_name(s.state_byte),
            source: match s.active_source {
                Some(a) => a.to_string(),
                None => "-".into(),
            },
            t: s.telemetry,
        }
    }
}

fn state_name(b: u8) -> &'static str {
    use screeny_proto::control::state;
    match b {
        state::IDLE => "IDLE",
        state::LIVE => "LIVE",
        state::HOLD => "HOLD",
        state::IDENTIFY => "IDENTIFY",
        state::PROVISIONING => "PROV",
        _ => "?",
    }
}

fn codec_name(c: u8) -> &'static str {
    use screeny_proto::dec::codec;
    match c {
        codec::PAL5 => "PAL5",
        codec::PAL8_LZ => "PAL8_LZ",
        codec::PAL4_LZ => "PAL4_LZ",
        codec::BC1_DUAL => "BC1_DUAL",
        codec::SOLID => "SOLID",
        0 => "-",
        _ => "?",
    }
}

/// The events worth a line even without `--verbose`.
fn describe_important(e: &Event) -> Option<String> {
    match e {
        Event::Shown { .. } | Event::TelemetrySent { .. } | Event::TelemetryRateLimited { .. } => {
            None
        }
        Event::Dropped {
            cause: DropCause::Stale | DropCause::Superseded,
            ..
        } => None,
        Event::Rejected { .. } | Event::ControlRejected { .. } | Event::Busy { sent: false, .. } => {
            None
        }
        other => describe(other),
    }
}

fn describe(e: &Event) -> Option<String> {
    Some(match e {
        Event::Shown {
            seq, codec, bytes, ..
        } => format!("shown seq {seq} {} {bytes} B", codec_name(*codec)),
        Event::Dropped { seq, cause, .. } => format!("dropped seq {seq}: {cause:?}"),
        Event::Rejected { from, reason } => match reason {
            Some(r) => format!("rejected from {from}: {r:?}"),
            None => format!("rejected from {from}: not the active source"),
        },
        Event::Busy { to, sent } => {
            format!("busy -> {to}{}", if *sent { "" } else { " (suppressed)" })
        }
        Event::TelemetrySent { to } => format!("telemetry -> {to}"),
        Event::TelemetryRateLimited { to } => format!("telemetry -> {to} (suppressed)"),
        Event::Control {
            from,
            op,
            req_id,
            error,
            replied,
        } => format!(
            "control op {op:#04x} req {req_id} from {from}{}{}",
            match error {
                Some(c) => format!(" -> error {c:#04x}"),
                None => String::new(),
            },
            if *replied { "" } else { " (no reply)" }
        ),
        Event::ControlRejected { from, reason } => {
            format!("control rejected from {from}: {reason:?}")
        }
        Event::StateChanged { from, to } => format!("state {from:?} -> {to:?}"),
        Event::LockTaken { source, displaced } => match displaced {
            Some(d) => format!("lock: {source} took it from {d}"),
            None => format!("lock: {source}"),
        },
        Event::LockReleased { source, reason } => format!("lock released by {source}: {reason:?}"),
        Event::Brightness { requested, applied } => {
            format!("brightness {requested} -> {applied}")
        }
        Event::Identify { duration_ms } => format!("identify for {duration_ms} ms"),
        Event::IdleMode { mode } => format!("idle mode {mode}"),
        Event::Renamed { name } => format!("renamed to {name:?}"),
        Event::StatsReset => "stats reset".into(),
        // Section 8.4: the PSK is never logged. The event does not carry one.
        Event::SetWifi { ssid, persist } => {
            format!("SET_WIFI ssid {ssid:?} persist {persist} (accepted, not acted on)")
        }
        Event::Reboot => "REBOOT (accepted, not acted on)".into(),
        _ => return None,
    })
}
