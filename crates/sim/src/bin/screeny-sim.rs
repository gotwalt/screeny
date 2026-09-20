//! `screeny-sim` - a fake Tidbyt on the network.
//!
//! ```text
//! screeny-sim                                  # window, mDNS, default ports
//! screeny-sim --headless --exit-after 30
//! screeny-sim --drop 5 --decode-ms 20          # a bad day, on purpose
//! screeny-sim --headless --dump-dir /tmp/f --dump-every 30
//! screeny-sim --headless --no-mdns --http-port 8099 \
//!     --reset-reason brownout --store-errors 3 --stack-free 900
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
use screeny_sim::{Config, Health, SimDevice, SimHandle, WifiOutcome};

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
    --http-port N          HTTP API port, 0 for an ephemeral one [8080]
                           (never 80: binding it needs root. Without this
                           flag a busy 8080 falls back to an ephemeral
                           port, so a second simulator still starts; with
                           it, a busy port is an error)
    --no-http              do not serve the HTTP API

WIFI (there is no radio; all of this is scripted)
    --wifi-result WHICH    what a join attempt does: ok | fail | slow [ok]
                           ok   = joins; fail = wrong password;
                           slow = the radio never answers, so it times out
    --wifi-join-ms MS      how long a scripted join takes [200]
    --wifi-ssid SSID       the SSID the store holds [simulated]
    --ap-ssid SSID         the soft-AP's name, on the portal screen and its QR
    --start-in-portal      boot with an empty store: straight to the captive
                           portal, which is what a factory-fresh device is
    --link-down            start with the WiFi link down (spec 7.3)

DISPLAY
    --headless             no window; log statistics once a second
    --scale N              window upscale, >= 4 [14]
    --levels N             panel duty levels per channel [64]
    --brightness N         starting brightness, 0-255 [255]
    --brightness-cap N     the cap SET_BRIGHTNESS clamps to [255]
    --idle MODE            status | hold | dim | black [status]
    --rssi DBM             signal strength to report [-55]

DEVICE HEALTH (what GET /api/v1/status reports about itself; there is no
flash here and no stack to measure, so these are chosen. Nothing else in
the simulator pretends they are real: a pending_verify slot changes no
behaviour and a brownout reason reboots nothing)
    --reset-reason NAME    why the chip last restarted [power_on]
                           power_on | external | software | panic | int_wdt |
                           task_wdt | wdt | deep_sleep | brownout | sdio |
                           unknown   (a REBOOT sets it to software)
    --fw-slot NAME         the running app slot [ota_0]
                           ota_0 | ota_1 | unknown
    --fw-state NAME        the slot's otadata state [valid]
                           new | pending_verify | valid | invalid | aborted |
                           undefined
    --store-errors N       settings-store errors since boot [0]
    --heap-used N          heap in use, bytes [65536]
    --heap-size N          heap total, bytes [98304]
    --stack-free N         stack never touched, bytes [20480]

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
    link_down: bool,
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
            link_down: false,
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
                "--no-http" => o.cfg.http = false,
                "--start-in-portal" => o.cfg.start_in_portal = true,
                "--link-down" => o.link_down = true,
                "--http-port" => {
                    o.cfg.http_port = num(&value()?, "--http-port")?;
                    // Named, so a busy port is an error rather than a quiet
                    // fallback: this one is going to be connected to.
                    o.cfg.http_port_explicit = true;
                }
                "--wifi-ssid" => o.cfg.wifi_ssid = value()?,
                "--ap-ssid" => o.cfg.ap_ssid = value()?,
                "--wifi-join-ms" => o.cfg.wifi_join_ms = num(&value()?, "--wifi-join-ms")?,
                "--wifi-result" => {
                    o.cfg.wifi_outcome =
                        WifiOutcome::parse(&value()?).map_err(|e| format!("--wifi-result: {e}"))?;
                }
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
                // Card 192. Every name goes through the `screeny-device-api`
                // enum itself, so a name the API does not have cannot be
                // accepted here, and the refusal lists the ones it does.
                "--reset-reason" => {
                    o.cfg.health.reset_reason = Health::parse_reset_reason(&value()?)?;
                }
                "--fw-slot" => o.cfg.health.fw_slot = Health::parse_fw_slot(&value()?)?,
                "--fw-state" => o.cfg.health.fw_state = Health::parse_fw_state(&value()?)?,
                "--store-errors" => o.cfg.health.store_errors = num(&value()?, "--store-errors")?,
                "--heap-used" => o.cfg.health.heap_used = num(&value()?, "--heap-used")?,
                "--heap-size" => o.cfg.health.heap_size = num(&value()?, "--heap-size")?,
                "--stack-free" => o.cfg.health.stack_free = num(&value()?, "--stack-free")?,
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
        // The one combination a device could not report (card 192).
        o.cfg.health.check()?;
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
        Box::new(
            move |frame: &screeny_proto::Rgb888Frame, _: &screeny_sim::FrameMeta| {
                if let Err(e) = d.lock().unwrap().offer(frame) {
                    eprintln!("screeny-sim: dump: {e}");
                }
            },
        ) as screeny_sim::FrameSink
    });

    let headless = opts.headless;
    let link_down = opts.link_down;
    let quiet = opts.quiet;
    let verbose = opts.verbose;
    let scale = opts.scale;
    let exit_after = opts.exit_after.map(Duration::from_secs_f64);
    let faults = opts.cfg.faults;
    let health = opts.cfg.health;
    let mdns = opts.cfg.mdns;
    let instance = opts.cfg.instance.clone();

    let start_in_portal = opts.cfg.start_in_portal;
    let wifi_outcome = opts.cfg.wifi_outcome;
    let dev = SimDevice::start_with(opts.cfg, sink).map_err(|e| e.to_string())?;
    let sim = dev.handle();
    if link_down {
        sim.set_link_down(true);
    }

    if !quiet {
        println!(
            "screeny-sim: frames on {}, control on {}",
            dev.frame_addr(),
            dev.control_addr()
        );
        // A second line, deliberately: `tests/cli.rs` reads the addresses off
        // the first one and has since card 006.
        if let Some(addr) = dev.http_addr() {
            println!("screeny-sim: HTTP API on http://{addr}/");
        }
        if mdns {
            println!("screeny-sim: advertising _screeny._udp as {instance:?}");
        }
        if start_in_portal || wifi_outcome != WifiOutcome::Ok {
            println!(
                "screeny-sim: wifi: result {}{}",
                wifi_outcome.as_str(),
                if start_in_portal {
                    ", starting in the captive portal"
                } else {
                    ""
                }
            );
        }
        // Card 192: a run that is claiming to be unwell says so on startup,
        // and says it is only claiming. A default one says nothing, so the
        // banner `tests/cli.rs` reads is unchanged for everybody else.
        if health != Health::default() {
            println!(
                "screeny-sim: health: {} (reported, not simulated)",
                health.describe()
            );
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
            return Err("built without the `window` feature; run with --headless".into());
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
    /// The statistics strip under the window. Built without the `window`
    /// feature there is no strip to fill.
    #[cfg(feature = "window")]
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
        Event::Rejected { .. }
        | Event::ControlRejected { .. }
        | Event::Busy { sent: false, .. } => None,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn owned(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| (*s).to_string()).collect()
    }

    /// The message a refused command line produces. `Opts` is not `Debug`,
    /// and giving it one for a test would be the tail wagging the dog.
    fn refusal(args: &[&str]) -> String {
        match Opts::parse(&owned(args)) {
            Err(e) => e,
            Ok(_) => panic!("{args:?} should have been refused"),
        }
    }

    /// The help text spells the health names out, and the API owns them, so
    /// this is the thing that keeps the two together: a variant added to
    /// `screeny-device-api` fails here by name instead of quietly missing
    /// from `--help`.
    #[test]
    fn the_help_lists_every_name_the_health_flags_take() {
        for (flag, names) in [
            ("--reset-reason", Health::reset_reason_names()),
            ("--fw-slot", Health::fw_slot_names()),
            ("--fw-state", Health::fw_state_names()),
        ] {
            assert!(USAGE.contains(flag), "{flag} is not in --help");
            for n in names {
                assert!(
                    USAGE.contains(&n),
                    "{flag}: {n:?} is a value the API has and --help does not mention"
                );
            }
        }
        for flag in [
            "--store-errors",
            "--heap-used",
            "--heap-size",
            "--stack-free",
        ] {
            assert!(USAGE.contains(flag), "{flag} is not in --help");
        }
    }

    /// The defaults printed in the help are the defaults the code has.
    #[test]
    fn the_help_prints_the_real_defaults() {
        let h = Health::default();
        for want in [
            format!("[{}]", h.heap_used),
            format!("[{}]", h.heap_size),
            format!("[{}]", h.stack_free),
        ] {
            assert!(USAGE.contains(&want), "--help does not say {want}");
        }
    }

    #[test]
    fn the_health_flags_land_in_the_config() {
        let args = owned(&[
            "--reset-reason",
            "brownout",
            "--fw-slot",
            "ota_1",
            "--fw-state",
            "pending_verify",
            "--store-errors",
            "3",
            "--heap-used",
            "90000",
            "--heap-size",
            "98304",
            "--stack-free",
            "900",
        ]);
        let o = Opts::parse(&args).expect("parse").expect("not --help");
        assert_eq!(
            o.cfg.health,
            Health {
                reset_reason: screeny_device_api::ResetReason::Brownout,
                fw_slot: screeny_device_api::FwSlot::Ota1,
                fw_state: screeny_device_api::FwState::PendingVerify,
                store_errors: 3,
                heap_used: 90_000,
                heap_size: 98_304,
                stack_free: 900,
            }
        );
        // Nothing else moved.
        assert_eq!(Opts::parse(&[]).unwrap().unwrap().cfg.health, Health::default());
    }

    #[test]
    fn a_name_the_api_does_not_have_is_refused_with_the_ones_it_does() {
        let e = refusal(&["--reset-reason", "brownedout"]);
        assert!(e.contains("brownedout"), "{e}");
        assert!(e.contains("brownout"), "the valid names are missing: {e}");
        assert!(e.contains("power_on"), "the valid names are missing: {e}");
        // And the other two enums, whose names are equally not ours.
        assert!(refusal(&["--fw-slot", "ota_2"]).contains("ota_1"));
        assert!(refusal(&["--fw-state", "fine"]).contains("pending_verify"));
    }

    #[test]
    fn more_heap_in_use_than_there_is_heap_is_refused() {
        let e = refusal(&["--heap-used", "200000", "--heap-size", "98304"]);
        assert!(e.contains("--heap-used"), "{e}");
        assert!(e.contains("--heap-size"), "{e}");
    }
}
