//! `screeny-probe` — the bench tool card 008 measures the device with.
//!
//! It is deliberately **not** `crates/screeny` (card 009). That crate is the
//! product: discovery, encoders, a codec chooser, a friendly CLI. This one is
//! an instrument. It sends payloads somebody else encoded, it says exactly
//! what it sent and exactly what the device says it received, and it has a
//! `raw` mode for building datagrams a well-behaved sender would refuse to
//! build. Its answers are only worth anything because it shares
//! `crates/proto` with both the device and the simulator, so "the device
//! disagrees with the probe" can only mean one of them disagrees with the
//! spec.
//!
//! Prove any change against `cargo run -p screeny-sim -- --headless` before
//! pointing it at the hardware.
//!
//! ```text
//! screeny-probe [--addr HOST[:FRAMEPORT] | --name NAME] [--ctrl PORT] <command>
//!
//!   info                     GET_INFO, printed as key=value
//!   ping N                   N round trips, with the distribution
//!   stats                    one TELEMETRY request
//!   reset-stats              zero the counters
//!   brightness N             SET_BRIGHTNESS
//!   identify MS              IDENTIFY overlay for MS milliseconds
//!   idle MODE                SET_IDLE 0..=3
//!   name NAME                SET_NAME
//!   release                  RELEASE
//!   wifi                     GET_WIFI
//!   set-wifi SSID PSK [--persist]   SET_WIFI (takes the device off its network; bench only)
//!   reboot                   REBOOT (guarded)
//!   bench CMD ARG            private opcode 0x80: G/D/W/O/P
//!   stream [--codec ID|all|pattern] [--fps F] [--secs N] [--final]
//!   conformance [--only SEC] [--slow] [--cap-probe] [--restore-idle N] [--list]
//!                            the whole wire-level suite, rule by rule
//!   lock-test                an alias for `conformance --only 7`
//!   http [--http HOST[:PORT]] [--only N|SECTION] [--list]
//!        [--allow-reboot] [--allow-wifi-trial] [--cap-probe]
//!                            the HTTP API's conformance suite (card 228)
//!   fw-scan FILE             run the device's own image validator here, on a file
//!   fw-upload FILE [--activate] [--http HOST[:PORT]] [--force]
//!                            stage an image into the device's inactive slot (card 240)
//! ```
//!
//! The suites themselves are `src/suite/` (the wire) and `src/http/` (the
//! HTTP API), and they are libraries so that `crates/sim`'s integration tests
//! can run the same rules in process (cards 080 and 228).
//!
//! The bench one-liner after a flash is
//! `cargo run --release -p screeny-probe -- --addr 192.168.1.50 http`.

use std::net::{SocketAddr, ToSocketAddrs};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use screeny_proto::control::{ErrorCode, IdleMode, Request, SetWifi, Telemetry};
use screeny_proto::txt;

use screeny_probe::link::{Control, FrameLink, OwnedReply, Pacer};
use screeny_probe::{state_name, suite, vectors};

const USAGE: &str = "usage: screeny-probe [--addr HOST[:PORT] | --name NAME] [--ctrl PORT] [--vectors DIR] <command>\n\
                     commands: info | ping N | stats | reset-stats | brightness N | identify MS |\n\
                               idle MODE | name NAME | release | wifi | set-wifi SSID PSK [--persist] |\n\
                               reboot | bench CMD ARG |\n\
                               stream [--codec ID|all|pattern] [--fps F] [--secs N] [--final] |\n\
                               conformance [--only SECTION] [--slow] [--cap-probe]\n\
                                           [--restore-idle N] [--list] |\n\
                               lock-test (= conformance --only 7) |\n\
                               http [--http HOST[:PORT]] [--only N|SECTION] [--list]\n\
                                    [--allow-reboot] [--allow-wifi-trial] [--cap-probe] |\n\
                               fw-scan FILE | fw-upload FILE [--activate] [--http HOST[:PORT]] [--force]";

struct Args {
    host: String,
    frame_port: u16,
    ctrl_port: u16,
    vectors: String,
    rest: Vec<String>,
}

fn resolve(host: &str, port: u16) -> Result<SocketAddr, String> {
    (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("{host}:{port}: {e}"))?
        .find(|a| a.is_ipv4())
        .ok_or_else(|| format!("{host}: no IPv4 address"))
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        host: "screeny.local".into(),
        frame_port: screeny_proto::DEFAULT_FRAME_PORT,
        ctrl_port: screeny_proto::DEFAULT_CONTROL_PORT,
        vectors: format!("{}/../proto/tests/vectors", env!("CARGO_MANIFEST_DIR")),
        rest: Vec::new(),
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--addr" => {
                let v = it.next().ok_or("--addr needs a value")?;
                match v.rsplit_once(':') {
                    Some((h, p)) if !h.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => {
                        a.host = h.to_string();
                        a.frame_port = p.parse().map_err(|_| "bad port")?;
                    }
                    _ => a.host = v,
                }
            }
            // `--name` is the same sugar the other tools take, resolved
            // through the OS's mDNS responder rather than by browsing: the
            // device's host name is `<instance>.local` (spec section 5.1), and
            // `--addr` still always works when multicast is having a bad day.
            "--name" => {
                let v = it.next().ok_or("--name needs a value")?;
                let v = v.trim_end_matches('.');
                a.host = if v.ends_with(".local") {
                    v.to_string()
                } else {
                    format!("{v}.local")
                };
            }
            "--ctrl" => a.ctrl_port = it.next().ok_or("--ctrl needs a value")?.parse().map_err(|_| "bad port")?,
            "--vectors" => a.vectors = it.next().ok_or("--vectors needs a value")?,
            "-h" | "--help" => return Err(USAGE.into()),
            _ => {
                a.rest.push(arg);
                a.rest.extend(it);
                break;
            }
        }
    }
    if a.rest.is_empty() {
        return Err(USAGE.into());
    }
    Ok(a)
}

fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("screeny-probe: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<bool, String> {
    let args = parse_args()?;
    let cmd = args.rest[0].clone();
    let rest = &args.rest[1..];
    // `http --list` describes the catalogue and talks to nothing, so it has to
    // work with no device, no DNS and no network at all.
    if cmd == "http" && has_flag(rest, "--list") {
        return cmd_http_list();
    }
    // `fw-scan` reads a file and says what the device would make of it; it has
    // no device, so it must not need one to resolve (card 240).
    if cmd == "fw-scan" {
        return cmd_fw_scan(rest);
    }
    let ctrl_addr = resolve(&args.host, args.ctrl_port)?;
    let frame_addr = resolve(&args.host, args.frame_port)?;

    match cmd.as_str() {
        "info" => cmd_info(ctrl_addr),
        "ping" => cmd_ping(ctrl_addr, arg_num(rest, 0).unwrap_or(200)),
        "stats" => cmd_stats(ctrl_addr),
        "reset-stats" => simple(ctrl_addr, Request::ResetStats),
        "release" => simple(ctrl_addr, Request::Release),
        "wifi" => cmd_wifi(ctrl_addr),
        "brightness" => {
            let n = arg_num(rest, 0).ok_or("brightness needs a level 0..=255")?;
            match Control::connect(ctrl_addr)
                .map_err(|e| e.to_string())?
                .request(Request::SetBrightness(n as u8))?
            {
                OwnedReply::Brightness { applied } => {
                    println!("brightness applied {applied}");
                    Ok(true)
                }
                r => {
                    println!("unexpected reply {r:?}");
                    Ok(false)
                }
            }
        }
        "identify" => {
            let ms = arg_num(rest, 0).ok_or("identify needs a duration in ms")?;
            simple(
                ctrl_addr,
                Request::Identify {
                    duration_ms: ms as u16,
                },
            )
        }
        "idle" => {
            let m = arg_num(rest, 0).ok_or("idle needs a mode 0..=3")?;
            let mode = IdleMode::from_u8(m as u8).ok_or("mode must be 0..=3")?;
            simple(ctrl_addr, Request::SetIdle(mode))
        }
        "name" => {
            let n = rest.first().ok_or("name needs a name")?;
            simple(ctrl_addr, Request::SetName(n))
        }
        "reboot" => simple(ctrl_addr, Request::Reboot),
        // Spec 8.2. Deliberately not part of `conformance`: it takes the device
        // off its network. The PSK is an argument, so it lands in shell history;
        // this is a bench tool. Without `--persist` nothing is stored and a
        // reboot returns the device to its stored credentials.
        "set-wifi" => {
            let ssid = rest.first().ok_or("set-wifi needs SSID PSK [--persist]")?;
            let psk = rest.get(1).ok_or("set-wifi needs SSID PSK [--persist]")?;
            let persist = rest.iter().any(|a| a == "--persist");
            simple(ctrl_addr, Request::SetWifi(SetWifi { ssid, psk, persist }))
        }
        // Card 240. `fw-scan` is handled above, before anything is resolved.
        // `fw-upload` speaks HTTP, not UDP, and exists so that a bench
        // procedure is one line rather than a `curl` with a 240 s timeout, a
        // content type and a local validation step spelled out by hand.
        "fw-upload" => cmd_fw_upload(&args, rest),
        "bench" => cmd_bench(ctrl_addr, rest),
        "stream" => cmd_stream(&args, frame_addr, ctrl_addr, rest),
        "conformance" => cmd_conformance(frame_addr, ctrl_addr, rest, None),
        // Kept as an alias: the bench notes and cards 008 and 016 all say
        // `lock-test`, and section 7 is exactly what it used to mean.
        "lock-test" => cmd_conformance(frame_addr, ctrl_addr, rest, Some("7")),
        "http" => cmd_http(&args, ctrl_addr, rest),
        other => Err(format!("unknown command {other:?}\n{USAGE}")),
    }
}

fn arg_num(rest: &[String], i: usize) -> Option<u64> {
    rest.get(i)?.parse().ok()
}

fn flag(rest: &[String], name: &str) -> Option<String> {
    let i = rest.iter().position(|a| a == name)?;
    rest.get(i + 1).cloned()
}

fn has_flag(rest: &[String], name: &str) -> bool {
    rest.iter().any(|a| a == name)
}

fn simple(addr: SocketAddr, req: Request<'_>) -> Result<bool, String> {
    let r = Control::connect(addr)
        .map_err(|e| e.to_string())?
        .request(req)?;
    match r.err_code() {
        Some(c) => {
            println!("error reply {:?} ({c:#04x})", ErrorCode::from_u8(c));
            Ok(false)
        }
        None => {
            println!("ok ({r:?})");
            Ok(true)
        }
    }
}

// ---------------------------------------------------------------------------
// info / ping / stats
// ---------------------------------------------------------------------------

fn cmd_info(addr: SocketAddr) -> Result<bool, String> {
    let r = Control::connect(addr)
        .map_err(|e| e.to_string())?
        .request(Request::GetInfo)?;
    let OwnedReply::Info(body) = r else {
        return Err(format!("expected an info reply, got {r:?}"));
    };
    println!("GET_INFO body: {} bytes", body.len());
    for e in txt::iter(&body) {
        println!(
            "  {}={}",
            String::from_utf8_lossy(e.key),
            String::from_utf8_lossy(e.value.unwrap_or(b""))
        );
    }
    match txt::DeviceInfo::parse(&body) {
        Ok(info) => {
            let mine = screeny_proto::dec::SUPPORTED_CODECS;
            println!(
                "parsed: {}x{} proto {} mtu {} ctrl {} fw {} id {}",
                info.w, info.h, info.proto, info.mtu, info.ctrl, info.fw, info.id
            );
            println!(
                "codecs: {}",
                info.codec_ids()
                    .map(|c| format!("{} ({})", vectors::codec_name(c), c))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            println!("best common codec: {:?}", info.best_codec(&mine));
            Ok(info.codec_ids().count() == 5)
        }
        Err(e) => {
            println!("PARSE FAILED: {e:?}");
            Ok(false)
        }
    }
}

fn cmd_ping(addr: SocketAddr, n: u64) -> Result<bool, String> {
    let mut c = Control::connect(addr).map_err(|e| e.to_string())?;
    let mut rtts = Vec::with_capacity(n as usize);
    let mut lost = 0u64;
    for _ in 0..n {
        let t0 = Instant::now();
        match c.request(Request::Ping) {
            Ok(OwnedReply::Ping { .. }) => rtts.push(t0.elapsed().as_secs_f64() * 1000.0),
            Ok(other) => return Err(format!("unexpected ping reply {other:?}")),
            Err(_) => lost += 1,
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    if rtts.is_empty() {
        return Err("no ping replies at all".into());
    }
    rtts.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let pct = |p: f64| rtts[((rtts.len() - 1) as f64 * p) as usize];
    let mean = rtts.iter().sum::<f64>() / rtts.len() as f64;
    println!(
        "ping n={} lost={} | min {:.2} p50 {:.2} p90 {:.2} p99 {:.2} max {:.2} mean {:.2} ms",
        rtts.len(),
        lost,
        rtts[0],
        pct(0.50),
        pct(0.90),
        pct(0.99),
        rtts[rtts.len() - 1],
        mean
    );
    // A histogram, because the distribution is the point: WiFi RTT is
    // bimodal (beacon-aligned) far more often than it is Gaussian.
    let mut buckets = [0usize; 10];
    let edges = [1.0, 2.0, 3.0, 5.0, 8.0, 12.0, 20.0, 40.0, 100.0];
    for &r in &rtts {
        let b = edges.iter().position(|&e| r < e).unwrap_or(9);
        buckets[b] += 1;
    }
    for (i, &count) in buckets.iter().enumerate() {
        if count == 0 {
            continue;
        }
        let label = if i == 0 {
            "   <1".to_string()
        } else if i == 9 {
            " >100".to_string()
        } else {
            format!("{:5.0}", edges[i - 1])
        };
        println!(
            "  {label} ms  {:4}  {}",
            count,
            "#".repeat((count * 40 / rtts.len()).max(1))
        );
    }
    Ok(lost == 0)
}

fn cmd_stats(addr: SocketAddr) -> Result<bool, String> {
    let r = Control::connect(addr)
        .map_err(|e| e.to_string())?
        .request(Request::Telemetry)?;
    let OwnedReply::Telemetry(t) = r else {
        return Err(format!("expected telemetry, got {r:?}"));
    };
    print_telemetry(&t);
    Ok(true)
}

fn print_telemetry(t: &Telemetry) {
    println!("uptime_ms                 {}", t.uptime_ms);
    println!("frames_rx                 {}", t.frames_rx);
    println!("frames_shown              {}", t.frames_shown);
    println!("frames_dropped_stale      {}", t.frames_dropped_stale);
    println!("frames_dropped_superseded {}", t.frames_dropped_superseded);
    println!("frames_dropped_decode     {}", t.frames_dropped_decode);
    println!("frames_rejected           {}", t.frames_rejected);
    println!("seq_gaps                  {}", t.seq_gaps);
    println!("interarrival_us           {}", t.interarrival_us);
    println!("jitter_us                 {}", t.jitter_us);
    println!("interarrival_max_us       {}", t.interarrival_max_us);
    println!("decode_us                 {}", t.decode_us);
    println!("decode_us_max             {}", t.decode_us_max);
    println!("render_us_max             {}", t.render_us_max);
    println!("rssi_dbm                  {}", t.rssi_dbm);
    println!("brightness                {}", t.brightness);
    println!(
        "state                     {} ({})",
        t.state,
        state_name(t.state)
    );
    println!(
        "last_codec                {:#04x} ({})",
        t.last_codec,
        vectors::codec_name(t.last_codec)
    );
}

fn cmd_wifi(addr: SocketAddr) -> Result<bool, String> {
    let r = Control::connect(addr)
        .map_err(|e| e.to_string())?
        .request(Request::GetWifi)?;
    match r {
        OwnedReply::Wifi { ssid, state } => {
            println!("ssid {ssid:?} state {state}");
            // Section 8.4's invariant: the PSK is never returned. If it ever
            // is, this is where we would notice.
            Ok(true)
        }
        other => Err(format!("expected a wifi reply, got {other:?}")),
    }
}

/// The private bench opcode. Hand-built, because `crates/proto` quite rightly
/// does not know about `0x80`.
fn cmd_bench(addr: SocketAddr, rest: &[String]) -> Result<bool, String> {
    let c = rest.first().ok_or("bench needs a command letter")?;
    let arg = arg_num(rest, 1).ok_or("bench needs an argument")? as u8;
    let cmd = c.as_bytes()[0];
    let mut pkt = vec![
        screeny_proto::MAGIC,
        (screeny_proto::VERSION << 4) | screeny_proto::TYPE_CONTROL,
        0x80,
        0,
        7,
        0,
        2,
        0,
    ];
    pkt.push(cmd);
    pkt.push(arg);
    let mut ctrl = Control::connect(addr).map_err(|e| e.to_string())?;
    match ctrl.raw(&pkt)? {
        Some(_) => {
            println!("bench {} {} ok", *c, arg);
            Ok(true)
        }
        None => Err("no reply to bench opcode".into()),
    }
}

// ---------------------------------------------------------------------------
// stream
// ---------------------------------------------------------------------------

enum Source {
    Vectors(Vec<vectors::Vector>),
    Pattern,
}

fn cmd_stream(
    args: &Args,
    frame_addr: SocketAddr,
    ctrl_addr: SocketAddr,
    rest: &[String],
) -> Result<bool, String> {
    let fps: f64 = flag(rest, "--fps")
        .unwrap_or_else(|| "30".into())
        .parse()
        .map_err(|_| "bad --fps")?;
    let secs: f64 = flag(rest, "--secs")
        .unwrap_or_else(|| "10".into())
        .parse()
        .map_err(|_| "bad --secs")?;
    let want = flag(rest, "--codec").unwrap_or_else(|| "all".into());
    let send_final = has_flag(rest, "--final");

    let (source, label) = if want == "pattern" {
        (Source::Pattern, "generated pattern".to_string())
    } else {
        let all = vectors::load(std::path::Path::new(&args.vectors))?;
        let (v, label) = if want == "all" {
            (all, "all codecs".to_string())
        } else {
            let id = vectors::codec_by_name(&want).ok_or(format!("unknown codec {want:?}"))?;
            let v: Vec<_> = all.into_iter().filter(|v| v.codec == id).collect();
            if v.is_empty() {
                return Err(format!("no vectors for codec {want}"));
            }
            (v, format!("{} ({id})", vectors::codec_name(id)))
        };
        (Source::Vectors(v), label)
    };

    let mut ctrl = Control::connect(ctrl_addr).map_err(|e| e.to_string())?;
    // Give up any lock this host already holds and let it settle. Without
    // this, a second `stream` run started inside `LOCK_MS` of the first is
    // a *different* source (a new socket is a new 4-tuple, section 7.1) and
    // its first half-dozen frames are correctly refused with `BUSY` - which
    // is the device behaving and the measurement being wrong.
    ctrl.request(Request::Release)?;
    std::thread::sleep(Duration::from_millis(
        screeny_proto::LOCK_MS as u64 / 5 + 50,
    ));
    // A clean slate, so every number below is about this run and nothing else.
    ctrl.request(Request::ResetStats)?;
    let before = telemetry(&mut ctrl)?;

    let mut link = FrameLink::connect(frame_addr).map_err(|e| e.to_string())?;
    println!(
        "streaming {label} at {fps} fps for {secs} s from {} to {frame_addr}",
        link.local().map_err(|e| e.to_string())?
    );

    let mut pacer = Pacer::new(fps);
    let mut bytes = 0u64;
    let mut busy = 0u64;
    let mut telem_replies = 0u64;
    let mut last_telem: Option<Telemetry> = None;
    let deadline = Duration::from_secs_f64(secs);

    while pacer.elapsed() < deadline {
        let n = pacer.wait();
        let (codec, payload) = match &source {
            Source::Vectors(v) => {
                let v = &v[(n as usize) % v.len()];
                if n == 0 {
                    println!(
                        "  first vector: {} ({}, {} B)",
                        v.name,
                        vectors::codec_name(v.codec),
                        v.payload.len()
                    );
                }
                (v.codec, v.payload.clone())
            }
            Source::Pattern => {
                // Alternate: the moving PAL4_LZ picture most of the time, a
                // SOLID every eighth frame so both generated codecs are on
                // the wire and a codec switch happens mid-stream (section
                // 4.7: `codec` is per-frame and needs no negotiation).
                if n % 8 == 7 {
                    (screeny_proto::dec::codec::SOLID, vectors::solid_frame(n))
                } else {
                    (
                        screeny_proto::dec::codec::PAL4_LZ,
                        vectors::pal4_frame(n),
                    )
                }
            }
        };
        // Section 6.4: about one frame a second, never more than one in 100 ms.
        let stats_req = n.is_multiple_of(30);
        let mut flags = screeny_proto::F_KEY;
        if stats_req {
            flags |= screeny_proto::F_STATS_REQ;
        }
        link.send(codec, flags, &payload)
            .map_err(|e| e.to_string())?;
        bytes += payload.len() as u64 + 8;

        for r in link.poll() {
            match r {
                OwnedReply::Telemetry(t) => {
                    telem_replies += 1;
                    last_telem = Some(t);
                }
                OwnedReply::Busy { .. } => busy += 1,
                _ => {}
            }
        }
    }

    // Before the FINAL, before the drain sleep: the rate is about the send
    // loop and nothing else.
    let elapsed = pacer.elapsed().as_secs_f64();

    if send_final {
        // Section 7.4: a displayed FINAL releases the lock immediately.
        let (codec, payload) = match &source {
            Source::Vectors(v) => (v[0].codec, v[0].payload.clone()),
            Source::Pattern => (screeny_proto::dec::codec::SOLID, vectors::solid_frame(0)),
        };
        link.send(codec, screeny_proto::F_KEY | screeny_proto::F_FINAL, &payload)
            .map_err(|e| e.to_string())?;
    }

    // Let the last frames land and the last telemetry come back.
    std::thread::sleep(Duration::from_millis(300));
    for r in link.poll() {
        match r {
            OwnedReply::Telemetry(t) => {
                telem_replies += 1;
                last_telem = Some(t);
            }
            OwnedReply::Busy { .. } => busy += 1,
            _ => {}
        }
    }
    let after = telemetry(&mut ctrl)?;

    let sent = link.sent;
    let d = |a: u32, b: u32| a.wrapping_sub(b) as u64;
    let rx = d(after.frames_rx, before.frames_rx);
    let shown = d(after.frames_shown, before.frames_shown);
    let stale = d(after.frames_dropped_stale, before.frames_dropped_stale);
    let sup = d(
        after.frames_dropped_superseded,
        before.frames_dropped_superseded,
    );
    let dec = d(after.frames_dropped_decode, before.frames_dropped_decode);
    let rej = d(after.frames_rejected, before.frames_rejected);
    let gaps = d(after.seq_gaps, before.seq_gaps);
    // Timing fields come from the last telemetry the *stream* provoked, not
    // from the control-port read afterwards. A `--final` frame releases the
    // lock, and section 6.8 says a source change clears `interarrival_us`
    // and `jitter_us`, so reading them after the stream has ended reports
    // two zeroes and hides the measurement.
    let timing = last_telem.unwrap_or(after);

    println!();
    println!("--- {label} --------------------------------------");
    println!("elapsed                   {elapsed:.2} s");
    println!(
        "sent                      {sent}  ({:.1} fps, {:.0} kB/s, {} pacer skips)",
        sent as f64 / elapsed,
        bytes as f64 / elapsed / 1000.0,
        pacer.skipped
    );
    println!(
        "frames_rx                 {rx}   ({:.2}% of sent)",
        100.0 * rx as f64 / sent as f64
    );
    println!(
        "frames_shown              {shown}   ({:.2}% of sent)",
        100.0 * shown as f64 / sent as f64
    );
    println!("dropped stale             {stale}");
    println!("dropped superseded        {sup}");
    println!("dropped decode            {dec}");
    println!("rejected                  {rej}");
    println!("seq_gaps                  {gaps}");
    println!(
        "network loss              {} frames ({:.2}%)",
        sent.saturating_sub(rx),
        100.0 * sent.saturating_sub(rx) as f64 / sent as f64
    );
    println!(
        "interarrival / jitter     {} us / {} us  (max {} us)",
        timing.interarrival_us, timing.jitter_us, after.interarrival_max_us
    );
    println!(
        "decode                    {} us ewma, {} us max",
        timing.decode_us, after.decode_us_max
    );
    println!("render_us_max             {} us", after.render_us_max);
    println!(
        "state / last_codec        {} / {} ({:#04x})",
        state_name(after.state),
        vectors::codec_name(after.last_codec),
        after.last_codec
    );
    println!("rssi                      {} dBm", after.rssi_dbm);
    println!("piggybacked telemetry     {telem_replies} replies, {busy} BUSY");
    if let Some(t) = last_telem.as_ref() {
        // Section 3.3's identity. If this does not hold, a counter is lying
        // and every diagnosis in section 6.9 built on it is worthless.
        let lhs = t.frames_rx;
        let rhs = t
            .frames_shown
            .wrapping_add(t.frames_dropped_superseded)
            .wrapping_add(t.frames_dropped_decode);
        println!(
            "identity rx = shown+sup+dec   {lhs} vs {rhs}  {}",
            if lhs == rhs { "OK" } else { "MISMATCH" }
        );
    }

    let loss = sent.saturating_sub(rx) as f64 / sent as f64;
    let ok = dec == 0 && loss < 0.01 && rej == 0;
    println!(
        "verdict                   {}",
        if ok {
            "PASS (no decode drops, <1% loss, nothing rejected)"
        } else {
            "FAIL"
        }
    );
    Ok(ok)
}

fn telemetry(ctrl: &mut Control) -> Result<Telemetry, String> {
    match ctrl.request(Request::Telemetry)? {
        OwnedReply::Telemetry(t) => Ok(t),
        other => Err(format!("expected telemetry, got {other:?}")),
    }
}

// ---------------------------------------------------------------------------
// conformance
// ---------------------------------------------------------------------------

/// Run the wire-level conformance suite (`src/suite/`).
///
/// This replaced two hand-rolled command bodies - the old `conformance` and
/// `lock-test` - with one rule catalogue, so that the simulator's integration
/// tests and the bench run exactly the same checks (card 080). `lock-test`
/// survives as an alias for `--only 7`, because existing runbooks
/// and several cards name it.
fn cmd_conformance(
    frame_addr: SocketAddr,
    ctrl_addr: SocketAddr,
    rest: &[String],
    force_only: Option<&str>,
) -> Result<bool, String> {
    if has_flag(rest, "--list") {
        let rules = suite::all();
        println!("{} rules:", rules.len());
        for r in &rules {
            let mut tags = Vec::new();
            if r.flags & suite::LOOPBACK_ONLY != 0 {
                tags.push("loopback-only");
            }
            if r.flags & suite::SLOW != 0 {
                tags.push("slow");
            }
            if r.flags & suite::CAP_PROBE != 0 {
                tags.push("cap-probe");
            }
            println!(
                "  {:<5} {:<62} ~{:>4.1}s {}",
                r.section,
                r.name,
                r.secs,
                tags.join(",")
            );
        }
        return Ok(true);
    }

    let restore_idle: u8 = match flag(rest, "--restore-idle") {
        Some(v) => v.parse().map_err(|_| "--restore-idle needs 0..=3")?,
        None => 0,
    };
    if IdleMode::from_u8(restore_idle).is_none() {
        return Err("--restore-idle must be 0..=3".into());
    }

    let opts = suite::Opts {
        frame_addr,
        ctrl_addr,
        only: force_only
            .map(str::to_string)
            .or_else(|| flag(rest, "--only")),
        slow: has_flag(rest, "--slow"),
        cap_probe: has_flag(rest, "--cap-probe"),
        restore_idle,
    };
    suite::run(&opts).map(|s| s.ok())
}

// ---------------------------------------------------------------------------
// http
// ---------------------------------------------------------------------------

/// `http --list`: the catalogue, with each rule's number, section, route,
/// estimate, opt-in flags and citation. Talks to nothing.
fn cmd_http_list() -> Result<bool, String> {
    use screeny_probe::http;

    let rules = http::all();
    println!(
        "{} rules (CARD_223_LANDED is {}):",
        rules.len(),
        http::CARD_223_LANDED
    );
    for r in &rules {
        let mut tags = Vec::new();
        if r.flags & http::ALLOW_REBOOT != 0 {
            tags.push("allow-reboot");
        }
        if r.flags & http::ALLOW_WIFI_TRIAL != 0 {
            tags.push("allow-wifi-trial");
        }
        if r.flags & http::CAP_PROBE != 0 {
            tags.push("cap-probe");
        }
        if r.flags & http::NEEDS_UDP != 0 {
            tags.push("needs-udp");
        }
        if r.flags & http::KNOWN_223 != 0 {
            tags.push("known-223");
        }
        println!(
            "  #{:<3} {:<10} {:<60} ~{:>4.1}s {}",
            r.n,
            r.section,
            r.name,
            r.secs,
            tags.join(",")
        );
        println!("       {:<10} {}", r.route, r.cite);
    }
    Ok(true)
}

/// Run the HTTP conformance suite (`src/http/`).
///
/// The address defaults to the one `--addr`/`--name` already named, on port
/// 80, which is where the device serves it; `--http HOST[:PORT]` points it
/// somewhere else (a simulator on `127.0.0.1:8080`, say) without moving the
/// UDP ports, because two of the rules compare the two halves of the device
/// against each other.
fn cmd_http(args: &Args, ctrl_addr: SocketAddr, rest: &[String]) -> Result<bool, String> {
    use screeny_probe::http;

    // `--http` takes a host and an optional port; without it the HTTP API is
    // on the device this run is already pointed at, on port 80.
    let (host, port) = match flag(rest, "--http") {
        Some(v) => match v.rsplit_once(':') {
            Some((h, p)) if !h.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => (
                h.to_string(),
                p.parse::<u16>().map_err(|_| "bad --http port")?,
            ),
            _ => (v, 80),
        },
        None => (args.host.clone(), 80),
    };
    let http_addr = resolve(&host, port)?;

    let opts = http::Opts {
        http_addr,
        host,
        // The same device, so the rules that compare a browser's numbers with
        // a sender's have both halves. A `--http` pointed somewhere else is
        // still given this control port: if it does not answer, those rules
        // skip themselves and say so.
        ctrl_addr: Some(ctrl_addr),
        only: flag(rest, "--only"),
        allow_reboot: has_flag(rest, "--allow-reboot"),
        allow_wifi_trial: has_flag(rest, "--allow-wifi-trial"),
        cap_probe: has_flag(rest, "--cap-probe"),
        ctrlc: true,
    };
    http::run(&opts).map(|s| s.ok())
}

// ---------------------------------------------------------------------------
// Firmware images (card 240)
// ---------------------------------------------------------------------------

/// `fw-scan FILE`: run the device's own validator over an image, here.
///
/// The same `screeny-fwimage` scanner the firmware runs, on the same bytes, so
/// "this file would be accepted" is answerable on the bench Mac in a
/// millisecond instead of after fifteen seconds of flash writes on the device.
/// Talks to nothing.
fn cmd_fw_scan(rest: &[String]) -> Result<bool, String> {
    let path = rest.first().ok_or("fw-scan needs a path to an image")?;
    let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    Ok(report_scan(path, &bytes))
}

/// The slot in `firmware/partitions.csv`. Both `ota_0` and `ota_1` are 2 MiB.
const SLOT_LEN: u32 = 0x20_0000;

/// Scan `bytes` and print what the device would have said. Returns whether it
/// would be accepted.
fn report_scan(path: &str, bytes: &[u8]) -> bool {
    use screeny_fwimage::{Scan, SECTOR};
    let mut scan = Scan::new(SLOT_LEN);
    let mut failed = None;
    for piece in bytes.chunks(SECTOR) {
        if let Err(e) = scan.push(piece).and_then(|()| scan.check_front()) {
            failed = Some(e);
            break;
        }
    }
    let result = failed.map_or_else(|| scan.finish(), Err);
    match result {
        Ok(img) => {
            println!(
                "{path}: {} bytes on disk | image {} bytes, {} segments, {} trailing",
                bytes.len(),
                img.len,
                img.segments,
                img.trailing
            );
            println!(
                "  esp_app_desc.version {:?} | sha256 {}",
                img.version,
                hex32(&img.digest)
            );
            println!(
                "  {} of a {} byte slot ({:.1}%), {} sectors to stage",
                img.len,
                SLOT_LEN,
                f64::from(img.len) * 100.0 / f64::from(SLOT_LEN),
                bytes.len().div_ceil(SECTOR),
            );
            println!("  every check passed: the device would answer ok");
            true
        }
        Err(e) => {
            println!(
                "{path}: {} bytes on disk | REFUSED: {:?} after {} bytes",
                bytes.len(),
                e,
                scan.seen()
            );
            false
        }
    }
}

fn hex32(d: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in d {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// `fw-upload FILE [--activate] [--http HOST[:PORT]] [--force]`: stage an image on the
/// device.
///
/// Scans the file first and **refuses to send one the device would refuse**,
/// unless `--force` - which is exactly what a bench procedure wants when it is
/// deliberately uploading a broken image to watch the refusal. Prints the
/// reply and how long the whole thing took, which is the number to compare
/// with the device's own `ota:` log line.
fn cmd_fw_upload(args: &Args, rest: &[String]) -> Result<bool, String> {
    use screeny_device_api::reply::FirmwareReply;
    use screeny_device_api::route;
    use screeny_probe::http::client::Client;

    let path = rest.first().ok_or("fw-upload needs a path to an image")?;
    let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    let good = report_scan(path, &bytes);
    if !good && !has_flag(rest, "--force") {
        return Err(
            "this image would be refused; pass --force to send it anyway (which is what a \
             deliberate bad-image test wants)"
                .into(),
        );
    }

    let (host, port) = match flag(rest, "--http") {
        Some(v) => match v.rsplit_once(':') {
            Some((h, p)) if !h.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => (
                h.to_string(),
                p.parse::<u16>().map_err(|_| "bad --http port")?,
            ),
            _ => (v, 80),
        },
        None => (args.host.clone(), 80),
    };
    let addr = resolve(&host, port)?;
    // **The tool's default is the opposite of the API's, on purpose.**
    // `POST /api/v1/firmware` activates unless told otherwise, because the
    // natural reading of "send this firmware to the panel" is that the panel
    // then runs it. A *bench tool* that rebooted the device somebody is using
    // because they forgot a flag is a different thing, so here the reboot is
    // the thing you ask for and staging is what you get by default - the same
    // judgement that keeps the suite's rebooting rules behind `--allow-reboot`.
    let activate = has_flag(rest, "--activate");
    let path = if activate {
        route::FIRMWARE.to_owned()
    } else {
        format!("{}?activate=0", route::FIRMWARE)
    };
    // Far longer than the client's ordinary timeout: an upload is tens of
    // seconds of flash writes and the reply only comes at the end of them.
    let client = Client::new(addr, host.clone()).with_timeout(Duration::from_secs(240));
    // **Read before the upload, because afterwards it is too late.** The
    // device answers ~2 s before it restarts, so "has it rebooted yet" can
    // only be answered against the `boot_id` it had beforehand (card 246,
    // item 2). A short-timeout client of its own: this is one small GET and
    // the upload client waits four minutes.
    let before = screeny_probe::http::update::boot_id(
        &Client::new(addr, host.clone()).with_timeout(Duration::from_secs(3)),
    );
    match before {
        Some(b) => println!("the device is running boot_id {b}"),
        None => println!("the device did not answer GET {} before the upload", route::STATUS),
    }
    println!("uploading {} bytes to http://{addr}{path}", bytes.len());
    let t0 = Instant::now();
    let res = client.post_bytes(&path, &bytes)?;
    let elapsed = t0.elapsed();
    println!(
        "HTTP {} in {:.1} s ({:.0} KB/s)",
        res.status,
        elapsed.as_secs_f64(),
        bytes.len() as f64 / 1024.0 / elapsed.as_secs_f64().max(0.001),
    );
    if res.status != 200 {
        println!("  body: {}", String::from_utf8_lossy(&res.body));
        return Ok(false);
    }
    let reply: FirmwareReply = res.parse()?;
    println!(
        "  ok {} written {} error {:?} activating {}",
        reply.ok, reply.written, reply.error, reply.activating
    );
    if !reply.ok {
        return Ok(false);
    }
    if !reply.activating {
        println!(
            "  staged only: the image is in the inactive slot and nothing about what boots \
             has changed. Pass --activate to reboot into it."
        );
        return Ok(true);
    }
    // The waiting is `screeny_probe::http::update`, so that a test can drive
    // it against the simulator rather than against the one panel on the bench.
    let watch = screeny_probe::http::Watch::default();
    let outcome = screeny_probe::http::watch(&client.clone().with_timeout(Duration::from_secs(3)), before, &watch, &mut |line| {
        println!("{line}");
    });
    Ok(outcome.ok())
}

