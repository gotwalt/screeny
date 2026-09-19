//! The `screeny` command line.
//!
//! One binary on purpose: macOS Local Network permission and code signing are
//! per binary identity (card 015), so everything that talks to the LAN lives
//! here.

use std::io::{IsTerminal, Read, Write};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};

use screeny::control::ControlClient;
use screeny::encode::{EncodeConfig, Encoder, Profile};
use screeny::frame::RawReader;
use screeny::panel::TEMPORAL;
use screeny::proto::control::state;
use screeny::proto::{dec, NBYTES};
use screeny::{codec_name, Device, Frame, FrameSource, Pattern, SendStats, Sender, SenderConfig};

/// Stream frames to a screeny panel over UDP.
#[derive(Parser, Debug)]
#[command(name = "screeny", version, about, long_about = None)]
struct Cli {
    #[command(flatten)]
    target: TargetArgs,

    /// Print more about what is happening.
    #[arg(short, long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    cmd: Cmd,
}

/// How to find the device. `--addr` always works and skips discovery.
#[derive(Args, Debug, Clone)]
struct TargetArgs {
    /// Device address as `IP` or `IP:port`, skipping discovery entirely.
    #[arg(long, global = true, value_name = "IP[:PORT]")]
    addr: Option<String>,

    /// Pick a discovered device by instance or friendly name.
    #[arg(long, global = true, value_name = "NAME")]
    name: Option<String>,

    /// How long to browse for, in seconds.
    #[arg(long, global = true, default_value_t = 3.0, value_name = "SECS")]
    timeout: f64,

    /// Use the broadcast GET_INFO probe instead of mDNS (spec 5.5).
    #[arg(long, global = true)]
    broadcast: bool,
}

impl TargetArgs {
    fn parse_addr(&self) -> Result<Option<SocketAddr>> {
        let Some(s) = &self.addr else { return Ok(None) };
        if let Ok(a) = s.parse::<SocketAddr>() {
            return Ok(Some(a));
        }
        if let Ok(ip) = s.parse::<std::net::IpAddr>() {
            return Ok(Some(SocketAddr::new(ip, screeny::proto::DEFAULT_FRAME_PORT)));
        }
        // A host name: let the resolver have it, defaulting the port.
        let with_port = if s.contains(':') {
            s.clone()
        } else {
            format!("{s}:{}", screeny::proto::DEFAULT_FRAME_PORT)
        };
        let mut it = std::net::ToSocketAddrs::to_socket_addrs(&with_port)
            .with_context(|| format!("resolving {s:?}"))?;
        it.next()
            .map(Some)
            .ok_or_else(|| anyhow::anyhow!("{s:?} resolved to no addresses"))
    }

    fn to_target(&self) -> Result<screeny::Target> {
        Ok(screeny::Target {
            addr: self.parse_addr()?,
            name: self.name.clone(),
            timeout: Some(Duration::from_secs_f64(self.timeout)),
            broadcast: self.broadcast,
        })
    }

    fn resolve(&self) -> Result<Device> {
        self.to_target()?.resolve().hinted()
    }
}

/// Turn a library error into a reported one, with its next step attached.
///
/// The hint is folded into the message rather than added as an `anyhow`
/// context because these are read by a person at a terminal: a paragraph
/// naming System Settings is more use than a chain of causes (spec 9.3).
trait Hinted<T> {
    fn hinted(self) -> Result<T>;
}

impl<T> Hinted<T> for screeny::Result<T> {
    fn hinted(self) -> Result<T> {
        self.map_err(|e| match e.hint() {
            Some(h) => anyhow::anyhow!("{e}\n\n{h}"),
            None => anyhow::anyhow!("{e}"),
        })
    }
}

/// Options shared by the streaming subcommands.
#[derive(Args, Debug, Clone)]
struct StreamArgs {
    /// Target frame rate.
    #[arg(long, default_value_t = 30.0)]
    fps: f64,

    /// Stop after this many seconds.
    #[arg(long, value_name = "SECS")]
    duration: Option<f64>,

    /// Pixel payload budget in bytes. Defaults to the device's advertised mtu.
    #[arg(long, value_name = "BYTES")]
    budget: Option<usize>,

    /// Use the faster, slightly lower quality encode profile (card 031).
    #[arg(long)]
    fast: bool,

    /// Do not adapt the frame rate or codec set to telemetry (spec 6.9).
    #[arg(long)]
    no_adapt: bool,

    /// Stamp frames with a sender timestamp (`HAS_TS`).
    #[arg(long)]
    timestamps: bool,

    /// Ask for interactive-video QoS. Untested on the bench AP (card 013).
    #[arg(long)]
    qos: bool,

    /// Skip the GET_INFO handshake and assume a v1 device.
    #[arg(long)]
    no_handshake: bool,
}

impl StreamArgs {
    fn config(&self) -> SenderConfig {
        SenderConfig {
            fps: self.fps,
            budget: self.budget,
            profile: if self.fast {
                Profile::Fast
            } else {
                Profile::Full
            },
            adapt: !self.no_adapt,
            timestamps: self.timestamps,
            qos: self.qos,
            handshake: !self.no_handshake,
            ..SenderConfig::default()
        }
    }
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// List screeny devices on the network.
    Discover,

    /// Print a device's GET_INFO metadata.
    Info,

    /// Follow a device's telemetry (spec 6.7).
    Stats {
        /// Seconds between samples.
        #[arg(long, default_value_t = 1.0)]
        interval: f64,
        /// Stop after this many samples. Runs until interrupted by default.
        #[arg(long, short = 'n')]
        count: Option<u32>,
        /// Reset the device's counters before starting.
        #[arg(long)]
        reset: bool,
    },

    /// Set the panel brightness, 0-255. The firmware clamps it.
    Brightness {
        /// Requested level.
        level: u8,
    },

    /// Flash a "which one is this?" pattern on the panel.
    Identify {
        /// How long to show it, milliseconds. 0 stops it.
        #[arg(long, default_value_t = 3000)]
        ms: u16,
    },

    /// Reboot the device.
    Reboot {
        /// Required: rebooting is the one control op that is not idempotent.
        #[arg(long)]
        yes: bool,
    },

    /// Measure round-trip time to the control port.
    Ping {
        /// How many pings to send.
        #[arg(long, short = 'c', default_value_t = 5)]
        count: u32,
        /// Seconds between pings.
        #[arg(long, default_value_t = 0.25)]
        interval: f64,
    },

    /// Stream a built-in test pattern.
    Pattern {
        /// Pattern name. Omit with --list to see them.
        #[arg(default_value = "bars")]
        name: String,
        /// List the patterns and exit.
        #[arg(long)]
        list: bool,
        #[command(flatten)]
        stream: StreamArgs,
    },

    /// Stream raw 6144-byte RGB888 frames from stdin.
    Pipe {
        #[command(flatten)]
        stream: StreamArgs,
    },

    /// Run the chooser over frames and report codec, size and time. Sends
    /// nothing.
    EncodeStats {
        /// Pixel payload budget.
        #[arg(long, default_value_t = screeny::proto::MAX_PIXEL_PAYLOAD)]
        budget: usize,
        /// Use the fast profile.
        #[arg(long)]
        fast: bool,
        /// Read this many frames of a built-in pattern instead of stdin.
        #[arg(long, value_name = "NAME")]
        pattern: Option<String>,
        /// How many frames to take (with --pattern, or as a limit on stdin).
        #[arg(long, short = 'n')]
        count: Option<usize>,
        /// Print one line per frame as well as the summary.
        #[arg(long)]
        per_frame: bool,
        /// Also report each frame's mean Oklab dE against the panel model.
        /// Costs about a millisecond per frame.
        #[arg(long)]
        quality: bool,
    },
}

fn main() {
    let cli = Cli::parse();
    if let Err(e) = run(&cli) {
        eprintln!("screeny: {e}");
        std::process::exit(1);
    }
}

fn run(cli: &Cli) -> Result<()> {
    match &cli.cmd {
        Cmd::Discover => cmd_discover(cli),
        Cmd::Info => cmd_info(cli),
        Cmd::Stats {
            interval,
            count,
            reset,
        } => cmd_stats(cli, *interval, *count, *reset),
        Cmd::Brightness { level } => cmd_brightness(cli, *level),
        Cmd::Identify { ms } => cmd_identify(cli, *ms),
        Cmd::Reboot { yes } => cmd_reboot(cli, *yes),
        Cmd::Ping { count, interval } => cmd_ping(cli, *count, *interval),
        Cmd::Pattern { name, list, stream } => cmd_pattern(cli, name, *list, stream),
        Cmd::Pipe { stream } => cmd_pipe(cli, stream),
        Cmd::EncodeStats {
            budget,
            fast,
            pattern,
            count,
            per_frame,
            quality,
        } => cmd_encode_stats(*budget, *fast, pattern.as_deref(), *count, *per_frame, *quality),
    }
}

// ---------------------------------------------------------------------------
// Discovery and control
// ---------------------------------------------------------------------------

fn cmd_discover(cli: &Cli) -> Result<()> {
    let t = cli.target.to_target()?;
    if let Some(addr) = t.addr {
        // `--addr` with `discover` means "tell me about this one".
        let mut d = Device::from_addr(addr);
        let mut ctl = ControlClient::connect(d.control).hinted()?;
        let info = ctl.info().hinted()?;
        d.apply(info);
        print_device(&d, cli.verbose);
        return Ok(());
    }
    let timeout = Duration::from_secs_f64(cli.target.timeout);
    let found = if t.broadcast {
        screeny::discover::broadcast_probe(timeout, screeny::proto::DEFAULT_CONTROL_PORT).hinted()?
    } else {
        screeny::discover::browse(timeout, None).hinted()?
    };
    if found.is_empty() {
        let e = screeny::Error::NotFound {
            service: screeny::proto::SERVICE_TYPE,
            secs: timeout.as_secs_f64(),
        };
        println!("no devices found.");
        if let Some(h) = e.hint() {
            println!("\n{h}");
        }
        return Ok(());
    }
    for d in &found {
        print_device(d, cli.verbose);
    }
    Ok(())
}

fn print_device(d: &Device, verbose: bool) {
    println!("{}", d.instance);
    println!("  frames   {}", d.frame);
    println!("  control  {}", d.control);
    if let Some(h) = &d.host {
        println!("  host     {h}");
    }
    if let Some(i) = &d.info {
        println!("  panel    {}x{}", i.w, i.h);
        if i.codecs.is_empty() {
            println!(
                "  codecs   {:?} - nothing this sender can produce, so it cannot \
                 stream to this device",
                i.codecs_raw
            );
        } else {
            println!(
                "  codecs   {}",
                i.codecs
                    .iter()
                    .map(|c| format!("{} ({c})", codec_name(*c)))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        println!("  mtu      {}", i.mtu);
        println!("  firmware {}", i.fw);
        println!("  id       {}", i.id);
        if !i.name.is_empty() {
            println!("  name     {}", i.name);
        }
        if verbose {
            println!("  proto    {}", i.proto);
            println!("  txtvers  {}", i.txtvers);
            if d.addresses.len() > 1 {
                println!(
                    "  addrs    {}",
                    d.addresses
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
        }
    }
}

fn control(cli: &Cli) -> Result<(Device, ControlClient)> {
    let d = cli.target.resolve()?;
    let c = ControlClient::connect(d.control).hinted()?;
    Ok((d, c))
}

fn cmd_info(cli: &Cli) -> Result<()> {
    let (mut d, mut ctl) = control(cli)?;
    let info = ctl.info().hinted()?;
    d.apply(info);
    print_device(&d, true);
    Ok(())
}

fn cmd_brightness(cli: &Cli, level: u8) -> Result<()> {
    let (_, mut ctl) = control(cli)?;
    let applied = ctl.set_brightness(level).hinted()?;
    if applied == level {
        println!("brightness {applied}");
    } else {
        println!("brightness {applied} (asked for {level}; the firmware cap is lower)");
    }
    Ok(())
}

fn cmd_identify(cli: &Cli, ms: u16) -> Result<()> {
    let (d, mut ctl) = control(cli)?;
    ctl.identify(ms).hinted()?;
    println!("{} is identifying for {ms} ms", d.label());
    Ok(())
}

fn cmd_reboot(cli: &Cli, yes: bool) -> Result<()> {
    if !yes {
        bail!("rebooting interrupts whatever is on the panel; pass --yes to confirm");
    }
    let (d, mut ctl) = control(cli)?;
    ctl.reboot().hinted()?;
    println!("{} is rebooting", d.label());
    Ok(())
}

fn cmd_ping(cli: &Cli, count: u32, interval: f64) -> Result<()> {
    let (d, mut ctl) = control(cli)?;
    println!("PING {} ({})", d.label(), d.control);
    let mut rtts = Vec::new();
    for i in 0..count {
        match ctl.ping() {
            Ok((rtt, uptime)) => {
                println!(
                    "  seq={i} time={:.2} ms  uptime={}",
                    rtt.as_secs_f64() * 1000.0,
                    fmt_ms(uptime)
                );
                rtts.push(rtt.as_secs_f64() * 1000.0);
            }
            Err(e) => println!("  seq={i} {e}"),
        }
        if i + 1 < count {
            std::thread::sleep(Duration::from_secs_f64(interval));
        }
    }
    if rtts.is_empty() {
        bail!("no replies from {}", d.control);
    }
    rtts.sort_by(f64::total_cmp);
    let n = rtts.len();
    let mean = rtts.iter().sum::<f64>() / n as f64;
    println!(
        "{n}/{count} replies, min {:.2} / mean {:.2} / median {:.2} / max {:.2} ms",
        rtts[0],
        mean,
        rtts[n / 2],
        rtts[n - 1]
    );
    Ok(())
}

fn fmt_ms(ms: u32) -> String {
    let s = ms / 1000;
    format!("{}h{:02}m{:02}s", s / 3600, (s / 60) % 60, s % 60)
}

fn state_name(s: u8) -> &'static str {
    match s {
        state::IDLE => "IDLE",
        state::LIVE => "LIVE",
        state::HOLD => "HOLD",
        state::IDENTIFY => "IDENTIFY",
        state::PROVISIONING => "PROVISIONING",
        _ => "?",
    }
}

fn cmd_stats(cli: &Cli, interval: f64, count: Option<u32>, reset: bool) -> Result<()> {
    let (d, mut ctl) = control(cli)?;
    if reset {
        ctl.reset_stats().hinted()?;
    }
    println!("{}", d.label());
    println!(
        "{:>8} {:>5} {:>8} {:>8} {:>7} {:>7} {:>7} {:>7} {:>9} {:>8} {:>5}",
        "uptime", "state", "rx/s", "shown/s", "stale", "supers", "decode", "reject", "inter/jit",
        "decode", "rssi"
    );
    let stop = install_ctrlc()?;
    let mut prev: Option<screeny::proto::control::Telemetry> = None;
    let mut n = 0u32;
    while !stop.load(Ordering::Relaxed) {
        let t = ctl.telemetry().hinted()?;
        let (rx, shown, stale, sup, dec_, rej) = match prev {
            Some(p) => (
                t.frames_rx.wrapping_sub(p.frames_rx),
                t.frames_shown.wrapping_sub(p.frames_shown),
                t.frames_dropped_stale.wrapping_sub(p.frames_dropped_stale),
                t.frames_dropped_superseded
                    .wrapping_sub(p.frames_dropped_superseded),
                t.frames_dropped_decode.wrapping_sub(p.frames_dropped_decode),
                t.frames_rejected.wrapping_sub(p.frames_rejected),
            ),
            None => (0, 0, 0, 0, 0, 0),
        };
        println!(
            "{:>8} {:>5} {:>8.1} {:>8.1} {:>7} {:>7} {:>7} {:>7} {:>4}/{:<4} {:>6}us {:>4}",
            fmt_ms(t.uptime_ms),
            state_name(t.state),
            f64::from(rx) / interval,
            f64::from(shown) / interval,
            stale,
            sup,
            dec_,
            rej,
            t.interarrival_us,
            t.jitter_us,
            t.decode_us,
            t.rssi_dbm,
        );
        if cli.verbose {
            println!(
                "         last codec {} brightness {} seq gaps {} decode max {}us render max {}us inter max {}us",
                codec_name(t.last_codec),
                t.brightness,
                t.seq_gaps,
                t.decode_us_max,
                t.render_us_max,
                t.interarrival_max_us
            );
        }
        prev = Some(t);
        n += 1;
        if count.is_some_and(|c| n >= c) {
            break;
        }
        sleep_interruptible(Duration::from_secs_f64(interval), &stop);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Streaming
// ---------------------------------------------------------------------------

fn install_ctrlc() -> Result<Arc<AtomicBool>> {
    let stop = Arc::new(AtomicBool::new(false));
    let s = stop.clone();
    ctrlc::set_handler(move || s.store(true, Ordering::SeqCst))
        .context("installing the ctrl-c handler")?;
    Ok(stop)
}

fn sleep_interruptible(d: Duration, stop: &AtomicBool) {
    let end = Instant::now() + d;
    while Instant::now() < end {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        std::thread::sleep(Duration::from_millis(20).min(end - Instant::now()));
    }
}

fn cmd_pattern(cli: &Cli, name: &str, list: bool, stream: &StreamArgs) -> Result<()> {
    if list {
        for p in Pattern::ALL {
            println!("  {:<9} {}", p.name(), p.blurb());
        }
        return Ok(());
    }
    let Some(mut p) = Pattern::from_name(name) else {
        bail!(
            "unknown pattern {name:?}; try one of: {}",
            Pattern::ALL
                .iter()
                .map(|p| p.name())
                .collect::<Vec<_>>()
                .join(", ")
        );
    };
    stream_source(cli, stream, &mut p)
}

fn cmd_pipe(cli: &Cli, stream: &StreamArgs) -> Result<()> {
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        bail!(
            "`screeny pipe` reads raw {NBYTES}-byte RGB888 frames from stdin; \
             pipe something into it"
        );
    }
    let mut src = RawReader::new(stdin.lock());
    let r = stream_source(cli, stream, &mut src);
    if src.truncated {
        eprintln!("screeny: stdin ended part-way through a frame; the last one was dropped");
    }
    r
}

/// Resolve, connect, and run the pacing loop with live stats.
fn stream_source(cli: &Cli, args: &StreamArgs, src: &mut dyn FrameSource) -> Result<()> {
    let device = cli.target.resolve()?;
    let cfg = args.config();
    let mut sender = Sender::connect(device, cfg).hinted()?;

    println!(
        "streaming {} to {} at {:.1} fps, budget {} B{}",
        src.name(),
        sender.device().label(),
        args.fps,
        sender.budget(),
        if args.fast { ", fast profile" } else { "" }
    );
    if sender.budget_is_tight() {
        eprintln!(
            "screeny: budget {} is below the {} bytes the palette ladder needs; \
             falling back to block and solid codecs",
            sender.budget(),
            screeny::encode::MIN_BUDGET
        );
    }

    let stop = install_ctrlc()?;
    if let Some(secs) = args.duration {
        let s = stop.clone();
        let d = Duration::from_secs_f64(secs);
        std::thread::spawn(move || {
            std::thread::sleep(d);
            s.store(true, Ordering::SeqCst);
        });
    }

    let verbose = cli.verbose;
    let mut last = 0u64;
    let mut tick = move |s: &SendStats| {
        let sent = s.frames_sent - last;
        last = s.frames_sent;
        let mut line = format!(
            "{:>7} frames  {:>5.1} fps  {:>6.0} B  enc {:>5.2}/{:>5.2} ms (mean/p95)",
            s.frames_sent,
            sent as f64,
            s.mean_bytes(),
            s.mean_encode().as_secs_f64() * 1000.0,
            s.encode_pct(0.95).as_secs_f64() * 1000.0
        );
        let codecs: Vec<String> = s
            .by_codec
            .iter()
            .map(|(c, n)| format!("{} {}%", codec_name(*c), n * 100 / s.frames_sent.max(1)))
            .collect();
        if !codecs.is_empty() {
            line.push_str(&format!("  [{}]", codecs.join(" ")));
        }
        if let Some(t) = &s.telemetry {
            line.push_str(&format!(
                "  dev: shown {} drop {}/{}/{} jit {}us",
                t.frames_shown,
                t.frames_dropped_stale,
                t.frames_dropped_superseded,
                t.frames_dropped_decode,
                t.jitter_us
            ));
        }
        if s.frames_skipped > 0 {
            line.push_str(&format!("  skipped {}", s.frames_skipped));
        }
        if s.busy > 0 {
            line.push_str(&format!("  BUSY x{}", s.busy));
        }
        if s.codec_limited {
            line.push_str("  [decode-limited: cheap codecs only]");
        }
        println!("{line}");
        if verbose {
            let _ = std::io::stdout().flush();
        }
    };

    let r = sender.run_with(src, &stop, &mut tick);
    let s = sender.stats();
    println!(
        "sent {} frames in {:.1} s ({:.2} fps), {} skipped, {:.0} B mean, \
         encode mean {:.2} ms / p95 {:.2} ms / max {:.2} ms",
        s.frames_sent,
        s.started.map_or(0.0, |t| t.elapsed().as_secs_f64()),
        s.actual_fps(),
        s.frames_skipped,
        s.mean_bytes(),
        s.mean_encode().as_secs_f64() * 1000.0,
        s.encode_pct(0.95).as_secs_f64() * 1000.0,
        s.encode_max.as_secs_f64() * 1000.0,
    );
    if !s.codecs_withdrawn.is_empty() {
        eprintln!(
            "screeny: the device failed to decode {}; those codecs were withdrawn \
             mid-stream. That is a bug in one of the two implementations.",
            s.codecs_withdrawn
                .iter()
                .map(|c| codec_name(*c))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    r.hinted()
}

// ---------------------------------------------------------------------------
// encode-stats
// ---------------------------------------------------------------------------

fn cmd_encode_stats(
    budget: usize,
    fast: bool,
    pattern: Option<&str>,
    count: Option<usize>,
    per_frame: bool,
    quality: bool,
) -> Result<()> {
    let mut enc = Encoder::new(EncodeConfig {
        profile: if fast { Profile::Fast } else { Profile::Full },
        ..EncodeConfig::default()
    });

    let mut frames: Box<dyn Iterator<Item = Result<Frame>>> = match pattern {
        Some(name) => {
            let p = Pattern::from_name(name)
                .ok_or_else(|| anyhow::anyhow!("unknown pattern {name:?}"))?;
            let n = count.unwrap_or(60);
            Box::new((0..n).map(move |i| Ok(p.frame(i as u64))))
        }
        None => {
            let stdin = std::io::stdin();
            if stdin.is_terminal() {
                bail!(
                    "reads raw {NBYTES}-byte RGB888 frames from stdin; pipe something in, \
                     or use --pattern NAME"
                );
            }
            Box::new(RawFrames {
                r: stdin.lock(),
                left: count.unwrap_or(usize::MAX),
            })
        }
    };

    if per_frame {
        println!(
            "{:>5} {:>7} {:>9} {:>6} {:>8}{}",
            "frame", "colours", "codec", "bytes", "ms", if quality { "      dE" } else { "" }
        );
    }
    let mut n = 0usize;
    let mut total = Duration::ZERO;
    let mut max = Duration::ZERO;
    let mut bytes = 0u64;
    let mut over = 0usize;
    let mut exact = 0usize;
    let mut de_sum = 0f64;
    let mut by_codec: std::collections::BTreeMap<u8, usize> = std::collections::BTreeMap::new();
    let mut dst = Box::new([0u8; NBYTES]);

    for f in frames.by_ref() {
        let f = f?;
        let out = enc.encode(&f, budget);
        let st = enc.last_stats();
        // Every payload this tool reports has been through the real decoder.
        screeny::proto::decode(out.codec, &out.payload, &mut dst)
            .map_err(|e| anyhow::anyhow!("frame {n}: {} did not decode: {e:?}", codec_name(out.codec)))?;
        let de = if quality {
            screeny::encode::score::mean_de(&TEMPORAL, &f, &dst) * 1000.0
        } else {
            0.0
        };
        if per_frame {
            let mut line = format!(
                "{:>5} {:>7} {:>9} {:>6} {:>8.2}",
                n,
                st.colours,
                codec_name(out.codec),
                out.payload.len(),
                st.elapsed.as_secs_f64() * 1000.0
            );
            if quality {
                line.push_str(&format!(" {de:>7.2}"));
            }
            println!("{line}");
        }
        n += 1;
        total += st.elapsed;
        max = max.max(st.elapsed);
        bytes += out.payload.len() as u64;
        de_sum += de;
        if out.payload.len() > budget {
            over += 1;
        }
        if st.exact {
            exact += 1;
        }
        *by_codec.entry(out.codec).or_insert(0) += 1;
    }

    if n == 0 {
        bail!("no complete frames on stdin");
    }
    println!(
        "{n} frames, budget {budget} B, profile {}",
        if fast { "fast" } else { "full" }
    );
    println!(
        "  bytes   mean {:.0}, {} over budget",
        bytes as f64 / n as f64,
        over
    );
    println!(
        "  encode  mean {:.2} ms, max {:.2} ms  ({:.1} fps ceiling, {:.0}% of a 33.3 ms frame)",
        total.as_secs_f64() * 1000.0 / n as f64,
        max.as_secs_f64() * 1000.0,
        n as f64 / total.as_secs_f64(),
        total.as_secs_f64() * 1000.0 / n as f64 / 33.333 * 100.0
    );
    println!(
        "  codecs  {}",
        by_codec
            .iter()
            .map(|(c, k)| format!("{} {}%", codec_name(*c), k * 100 / n))
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!("  exact   {}%", exact * 100 / n);
    if quality {
        println!("  dE      mean {:.2} (x1000, panel-aware Oklab)", de_sum / n as f64);
    }
    Ok(())
}

struct RawFrames<R> {
    r: R,
    left: usize,
}

impl<R: Read> Iterator for RawFrames<R> {
    type Item = Result<Frame>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.left == 0 {
            return None;
        }
        let mut buf = vec![0u8; NBYTES];
        let mut got = 0;
        while got < NBYTES {
            match self.r.read(&mut buf[got..]) {
                Ok(0) => {
                    return if got == 0 {
                        None
                    } else {
                        Some(Err(anyhow::anyhow!(
                            "stdin ended {} bytes into a frame",
                            got
                        )))
                    }
                }
                Ok(n) => got += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Some(Err(e.into())),
            }
        }
        self.left -= 1;
        Some(Frame::from_bytes(&buf).map_err(|n| anyhow::anyhow!("short frame: {n} bytes")))
    }
}

const _: () = {
    // `dec::SUPPORTED_CODECS` is what the encoder offers by default; if a
    // codec is ever added there, `codec_name` should learn about it too.
    assert!(dec::SUPPORTED_CODECS.len() == 5);
};
