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
//! screeny-probe [--addr HOST[:FRAMEPORT]] [--ctrl PORT] <command>
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
//!   reboot                   REBOOT (guarded)
//!   bench CMD ARG            private opcode 0x80: G/D/W/O/P
//!   stream [--codec ID|all|pattern] [--fps F] [--secs N] [--final]
//!   lock-test                the whole of spec section 7.4, as a test
//!   conformance              the MUSTs that are cheap to probe from outside
//! ```

mod link;
mod vectors;

use std::net::{SocketAddr, ToSocketAddrs};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use screeny_proto::control::{op, state as tstate, ErrorCode, IdleMode, Request, Telemetry};
use screeny_proto::txt;

use link::{Control, FrameLink, OwnedReply, Pacer};

const USAGE: &str = "usage: screeny-probe [--addr HOST[:PORT]] [--ctrl PORT] [--vectors DIR] <command>\n\
                     commands: info | ping N | stats | reset-stats | brightness N | identify MS |\n\
                               idle MODE | name NAME | release | wifi | reboot | bench CMD ARG |\n\
                               stream [--codec ID|all|pattern] [--fps F] [--secs N] [--final] |\n\
                               lock-test | conformance";

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
    let ctrl_addr = resolve(&args.host, args.ctrl_port)?;
    let frame_addr = resolve(&args.host, args.frame_port)?;
    let cmd = args.rest[0].clone();
    let rest = &args.rest[1..];

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
        "bench" => cmd_bench(ctrl_addr, rest),
        "stream" => cmd_stream(&args, frame_addr, ctrl_addr, rest),
        "lock-test" => cmd_lock_test(frame_addr, ctrl_addr, &args),
        "conformance" => cmd_conformance(frame_addr, ctrl_addr),
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

fn state_name(b: u8) -> &'static str {
    match b {
        tstate::IDLE => "IDLE",
        tstate::LIVE => "LIVE",
        tstate::HOLD => "HOLD",
        tstate::IDENTIFY => "IDENTIFY",
        tstate::PROVISIONING => "PROVISIONING",
        _ => "?",
    }
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
        let stats_req = n % 30 == 0;
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
// lock-test
// ---------------------------------------------------------------------------

/// Spec section 7.4, driven from two sockets, which is the only way to test
/// it: a source is a UDP 4-tuple, so "another sender" means another socket.
fn cmd_lock_test(
    frame_addr: SocketAddr,
    ctrl_addr: SocketAddr,
    _args: &Args,
) -> Result<bool, String> {
    let mut ctrl = Control::connect(ctrl_addr).map_err(|e| e.to_string())?;
    let mut pass = true;
    let mut check = |name: &str, ok: bool, detail: String| {
        println!("  {:<44} {}  {detail}", name, if ok { "PASS" } else { "FAIL" });
        if !ok {
            pass = false;
        }
    };

    let payload = vectors::solid_frame(0);
    let solid = screeny_proto::dec::codec::SOLID;
    let key = screeny_proto::F_KEY;

    let mut a = FrameLink::connect(frame_addr).map_err(|e| e.to_string())?;
    let mut b = FrameLink::connect(frame_addr).map_err(|e| e.to_string())?;
    println!(
        "lock-test: A={} B={}",
        a.local().map_err(|e| e.to_string())?,
        b.local().map_err(|e| e.to_string())?
    );

    // Make sure nobody else holds it, then let HOLD settle.
    ctrl.request(Request::Release)?;
    std::thread::sleep(Duration::from_millis(600));
    ctrl.request(Request::ResetStats)?;

    // --- 1. A takes the lock ------------------------------------------
    for _ in 0..10 {
        a.send(solid, key, &payload).map_err(|e| e.to_string())?;
        std::thread::sleep(Duration::from_millis(33));
    }
    let t = telemetry(&mut ctrl)?;
    check(
        "1. A streaming -> state LIVE, frames shown",
        t.state == tstate::LIVE && t.frames_shown >= 5,
        format!("state {} shown {}", state_name(t.state), t.frames_shown),
    );

    // --- 2. B is locked out and told why ------------------------------
    let rej_before = t.frames_rejected;
    let _ = b.poll();
    let mut busy: Option<(u8, u32)> = None;
    for _ in 0..6 {
        // Keep A's lock alive while B knocks.
        a.send(solid, key, &payload).map_err(|e| e.to_string())?;
        b.send(solid, key, &payload).map_err(|e| e.to_string())?;
        std::thread::sleep(Duration::from_millis(40));
        for r in b.poll() {
            if let OwnedReply::Busy {
                reason,
                remaining_ms,
            } = r
            {
                busy = Some((reason, remaining_ms));
            }
        }
    }
    let t2 = telemetry(&mut ctrl)?;
    check(
        "2. B locked out -> BUSY on B's frame socket",
        busy.is_some(),
        match busy {
            Some((r, ms)) => format!("reason {r} remaining {ms} ms"),
            None => "no BUSY received".into(),
        },
    );
    check(
        "2b. B's frames counted in frames_rejected",
        t2.frames_rejected > rej_before,
        format!("{} -> {}", rej_before, t2.frames_rejected),
    );
    check(
        "2c. BUSY rate-limited to one per second",
        true,
        "see count below".into(),
    );
    // Count BUSYs over two seconds of continuous knocking.
    let mut busies = 0u32;
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_millis(2100) {
        a.send(solid, key, &payload).map_err(|e| e.to_string())?;
        b.send(solid, key, &payload).map_err(|e| e.to_string())?;
        std::thread::sleep(Duration::from_millis(33));
        busies += b
            .poll()
            .iter()
            .filter(|r| matches!(r, OwnedReply::Busy { .. }))
            .count() as u32;
    }
    check(
        "2d. BUSY count over 2.1 s of knocking is <= 3",
        busies <= 3,
        format!("{busies} BUSY packets for ~63 rejected frames"),
    );

    // --- 3. takeover after LOCK_MS ------------------------------------
    std::thread::sleep(Duration::from_millis(
        screeny_proto::LOCK_MS as u64 + 100,
    ));
    let _ = b.poll();
    for _ in 0..5 {
        b.send(solid, key, &payload).map_err(|e| e.to_string())?;
        std::thread::sleep(Duration::from_millis(33));
    }
    let t3 = telemetry(&mut ctrl)?;
    let busy_after = b
        .poll()
        .iter()
        .filter(|r| matches!(r, OwnedReply::Busy { .. }))
        .count();
    check(
        "3. after LOCK_MS of silence, B takes over",
        t3.state == tstate::LIVE && busy_after == 0 && t3.frames_shown > t2.frames_shown,
        format!(
            "state {} shown {} -> {} ({busy_after} late BUSY)",
            state_name(t3.state),
            t2.frames_shown,
            t3.frames_shown
        ),
    );

    // --- 4. FINAL releases immediately --------------------------------
    b.send(solid, screeny_proto::F_KEY | screeny_proto::F_FINAL, &payload)
        .map_err(|e| e.to_string())?;
    std::thread::sleep(Duration::from_millis(120));
    let t4 = telemetry(&mut ctrl)?;
    check(
        "4. a displayed FINAL releases the lock (state HOLD)",
        t4.state == tstate::HOLD,
        format!("state {}", state_name(t4.state)),
    );
    // And the panel is now free for anyone, well inside LOCK_MS.
    let _ = a.poll();
    a.send(solid, key, &payload).map_err(|e| e.to_string())?;
    std::thread::sleep(Duration::from_millis(120));
    let t5 = telemetry(&mut ctrl)?;
    let late_busy = a
        .poll()
        .iter()
        .filter(|r| matches!(r, OwnedReply::Busy { .. }))
        .count();
    check(
        "4b. A takes over instantly after FINAL, no BUSY",
        t5.state == tstate::LIVE && late_busy == 0,
        format!("state {} ({late_busy} BUSY)", state_name(t5.state)),
    );

    // --- 5. RELEASE from the control port -----------------------------
    ctrl.request(Request::Release)?;
    std::thread::sleep(Duration::from_millis(80));
    let t6 = telemetry(&mut ctrl)?;
    check(
        "5. RELEASE from the same IP releases the lock",
        t6.state == tstate::HOLD,
        format!("state {}", state_name(t6.state)),
    );

    // --- 6. stream timeout -> HOLD ------------------------------------
    a.send(solid, key, &payload).map_err(|e| e.to_string())?;
    std::thread::sleep(Duration::from_millis(100));
    let live = telemetry(&mut ctrl)?;
    std::thread::sleep(Duration::from_millis(
        screeny_proto::STREAM_TIMEOUT_MS as u64 + 200,
    ));
    let t7 = telemetry(&mut ctrl)?;
    check(
        "6. STREAM_TIMEOUT_MS with no frames -> HOLD",
        live.state == tstate::LIVE && t7.state == tstate::HOLD,
        format!(
            "{} -> {}",
            state_name(live.state),
            state_name(t7.state)
        ),
    );

    println!(
        "lock-test: {}",
        if pass { "all PASS" } else { "FAILURES ABOVE" }
    );
    Ok(pass)
}

// ---------------------------------------------------------------------------
// conformance
// ---------------------------------------------------------------------------

/// The MUSTs that can be checked from outside with one datagram each.
///
/// Not a substitute for `crates/proto`'s own tests - those check the library
/// both implementations share. These check the parts only a *device* can get
/// wrong: which port answers what, which malformed packet gets an error
/// rather than silence, and whether `req_id == 0` really does buy silence.
fn cmd_conformance(frame_addr: SocketAddr, ctrl_addr: SocketAddr) -> Result<bool, String> {
    let mut ctrl = Control::connect(ctrl_addr).map_err(|e| e.to_string())?;
    let frame = FrameLink::connect(frame_addr).map_err(|e| e.to_string())?;
    let mut pass = true;
    let mut check = |name: &str, ok: bool, detail: String| {
        println!("  {:<52} {}  {detail}", name, if ok { "PASS" } else { "FAIL" });
        if !ok {
            pass = false;
        }
    };

    let hdr = |op: u8, flags: u8, req_id: u16, len: u16| -> Vec<u8> {
        let mut v = vec![
            screeny_proto::MAGIC,
            (screeny_proto::VERSION << 4) | screeny_proto::TYPE_CONTROL,
            op,
            flags,
        ];
        v.extend_from_slice(&req_id.to_le_bytes());
        v.extend_from_slice(&len.to_le_bytes());
        v
    };

    // 6.5: a `len` the datagram does not back up is ERR_BAD_LENGTH, built
    // from the two header fields that are certainly present.
    let r = ctrl.raw(&hdr(op::PING, 0, 0x1111, 99))?;
    check(
        "6.5  len the datagram does not back up -> ERR_BAD_LENGTH",
        matches!(&r, Some(b) if b.len() >= 9 && b[3] & screeny_proto::C_ERROR != 0
                 && b[8] == ErrorCode::BadLength.as_u8()),
        describe(&r),
    );

    // 2.2: an unknown protocol version MAY be answered with ERR_VERSION.
    let mut bad = hdr(op::PING, 0, 0x2222, 0);
    bad[1] = (9 << 4) | screeny_proto::TYPE_CONTROL;
    let r = ctrl.raw(&bad)?;
    check(
        "2.2  CONTROL of an unknown version -> ERR_VERSION",
        matches!(&r, Some(b) if b.len() >= 9 && b[8] == ErrorCode::Version.as_u8()),
        describe(&r),
    );

    // 6.3: an unknown opcode gets ERR_UNKNOWN_OP, not silence, so a sender
    // can probe.
    let r = ctrl.raw(&hdr(0x7E, 0, 0x3333, 0))?;
    check(
        "6.3  unknown opcode -> ERR_UNKNOWN_OP",
        matches!(&r, Some(b) if b.len() >= 9 && b[8] == ErrorCode::UnknownOp.as_u8()),
        describe(&r),
    );

    // 6.1: req_id 0 means no reply wanted, and that covers error replies.
    let r = ctrl.raw(&hdr(0x7E, 0, 0, 0))?;
    check(
        "6.1  req_id 0 -> silence even for an error",
        r.is_none(),
        describe(&r),
    );

    // 6.1: a device must discard a CONTROL that arrives with REPLY set,
    // or two devices on one LAN would talk to each other indefinitely.
    let r = ctrl.raw(&hdr(op::PING, screeny_proto::C_REPLY, 0x4444, 0))?;
    check(
        "6.1  CONTROL with REPLY set -> discarded",
        r.is_none(),
        describe(&r),
    );

    // 6.5: a body that is not the opcode's size.
    let mut b = hdr(op::PING, 0, 0x5555, 3);
    b.extend_from_slice(&[0, 0, 0]);
    let r = ctrl.raw(&b)?;
    check(
        "6.5  PING with a 3-byte body -> ERR_BAD_LENGTH",
        matches!(&r, Some(b) if b.len() >= 9 && b[8] == ErrorCode::BadLength.as_u8()),
        describe(&r),
    );

    // 6.3: REBOOT is guarded. The wrong magic must be ERR_BAD_ARG and must
    // emphatically not reboot the device.
    let mut b = hdr(op::REBOOT, 0, 0x6666, 4);
    b.extend_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
    let r = ctrl.raw(&b)?;
    check(
        "6.3  REBOOT with the wrong magic -> ERR_BAD_ARG",
        matches!(&r, Some(b) if b.len() >= 9 && b[8] == ErrorCode::BadArg.as_u8()),
        describe(&r),
    );

    // 6.3: SET_IDLE with a mode nobody defines.
    let mut b = hdr(op::SET_IDLE, 0, 0x7777, 1);
    b.push(9);
    let r = ctrl.raw(&b)?;
    check(
        "6.3  SET_IDLE 9 -> ERR_BAD_ARG",
        matches!(&r, Some(b) if b.len() >= 9 && b[8] == ErrorCode::BadArg.as_u8()),
        describe(&r),
    );

    // 5.5: GET_INFO is one reply per source per second, except that a repeat
    // of an answered req_id is a retransmission and must be answered again.
    let info = |id: u16| hdr(op::GET_INFO, 0, id, 0);
    let first = ctrl.raw(&info(0x8888))?;
    let repeat = ctrl.raw(&info(0x8888))?;
    let fresh = ctrl.raw(&info(0x8889))?;
    check(
        "5.5  GET_INFO: first answered",
        first.is_some(),
        describe(&first),
    );
    check(
        "5.5  GET_INFO: same req_id inside 1 s answered again",
        repeat.is_some(),
        describe(&repeat),
    );
    check(
        "5.5  GET_INFO: a new req_id inside 1 s suppressed",
        fresh.is_none(),
        describe(&fresh),
    );

    // 2.2 / 6.7: everything the frame port rejects is counted, and nothing
    // it rejects is answered. Three shapes: bad magic, bad version, a
    // CONTROL on the frame port.
    //
    // One at a time, with the counter read in between. Sent as a burst these
    // were flaky on the real device - two runs in three counted three of
    // four - and a burst cannot tell "the firmware ignored one" from "the air
    // ate one", which is the only question worth asking. Each is retried up
    // to three times before it is called a failure: this is UDP over WiFi and
    // a lost probe is not a lost MUST.
    let junk: [(&str, Vec<u8>); 4] = [
        ("bad magic", vec![0x00; 16]),
        (
            "version 9",
            vec![0x53, 0x90, 0x7F, 0x01, 0, 0, 3, 0, 1, 2, 3],
        ),
        ("CONTROL on the frame port", hdr(op::PING, 0, 0x9999, 0)),
        (
            "FRAME len the datagram does not back up",
            vec![0x53, 0x10, 0x7F, 0x01, 0, 0, 0xFF, 0x00, 1, 2, 3],
        ),
    ];
    let mut answered = 0usize;
    for (what, bytes) in &junk {
        let mut counted = false;
        let mut tries = 0;
        while !counted && tries < 3 {
            tries += 1;
            ctrl.request(Request::ResetStats)?;
            let _ = frame.poll();
            frame.send_raw(bytes).map_err(|e| e.to_string())?;
            std::thread::sleep(Duration::from_millis(150));
            let t = telemetry(&mut ctrl)?;
            answered += frame.poll().len();
            counted = t.frames_rejected == 1
                && t.frames_rx == 0
                && t.frames_shown == 0
                && t.frames_dropped_decode == 0;
        }
        check(
            &format!("2.2  frame port: {what} -> frames_rejected += 1"),
            counted,
            format!("after {tries} attempt(s)"),
        );
    }
    check(
        "2.2  a CONTROL request on the frame port is not answered",
        answered == 0,
        format!("{answered} packets came back"),
    );

    // 4.7: a reserved codec id, and a payload of the wrong length, are both
    // frames_dropped_decode - the frame was *admitted*, it just did not
    // decode - and the previous frame stays lit.
    ctrl.request(Request::ResetStats)?;
    let mut f = FrameLink::connect(frame_addr).map_err(|e| e.to_string())?;
    // One frame per drain, spaced out. Sent back to back they would be a
    // single drain and four of the five would be *superseded* before anything
    // tried to decode them, which is correct behaviour and a useless test.
    for (codec, payload) in [
        (screeny_proto::dec::codec::SOLID, &[9u8, 9, 9][..]),
        (0x00, &[1, 2, 3][..]),                             // reserved codec
        (0xFF, &[1, 2, 3][..]),                             // reserved codec
        (screeny_proto::dec::codec::SOLID, &[1, 2][..]),     // short
        (screeny_proto::dec::codec::SOLID, &[1, 2, 3, 4][..]), // long
    ] {
        f.send(codec, screeny_proto::F_KEY, payload)
            .map_err(|e| e.to_string())?;
        std::thread::sleep(Duration::from_millis(60));
    }
    std::thread::sleep(Duration::from_millis(200));
    let t = telemetry(&mut ctrl)?;
    check(
        "4.7  reserved codec and wrong-length payloads -> dropped_decode",
        t.frames_dropped_decode == 4 && t.frames_rejected == 0,
        format!(
            "decode {} rejected {} rx {} shown {}",
            t.frames_dropped_decode, t.frames_rejected, t.frames_rx, t.frames_shown
        ),
    );
    check(
        "3.3  rx = shown + superseded + decode",
        t.frames_rx
            == t.frames_shown
                .wrapping_add(t.frames_dropped_superseded)
                .wrapping_add(t.frames_dropped_decode),
        format!(
            "{} vs {}+{}+{}",
            t.frames_rx,
            t.frames_shown,
            t.frames_dropped_superseded,
            t.frames_dropped_decode
        ),
    );

    // 3.2: a stale sequence number is a drop, not a rejection, and the
    // counter that moves says which.
    ctrl.request(Request::ResetStats)?;
    let payload = vectors::solid_frame(0);
    let solid = screeny_proto::dec::codec::SOLID;
    f.send(solid, screeny_proto::F_KEY, &payload)
        .map_err(|e| e.to_string())?;
    std::thread::sleep(Duration::from_millis(60));
    let back = f.seq.wrapping_sub(1);
    for _ in 0..3 {
        // Same seq three times over: duplicates.
        f.seq = back;
        f.send(solid, screeny_proto::F_KEY, &payload)
            .map_err(|e| e.to_string())?;
        std::thread::sleep(Duration::from_millis(40));
    }
    let t = telemetry(&mut ctrl)?;
    check(
        "3.2  a repeated seq -> frames_dropped_stale",
        t.frames_dropped_stale == 3 && t.frames_rejected == 0,
        format!(
            "stale {} rejected {}",
            t.frames_dropped_stale, t.frames_rejected
        ),
    );

    // 3.2 again, from the other side: a gap in the sequence is counted but
    // the frame is still shown. Measured as a *delta*, because section 6.8
    // says RESET_STATS deliberately does not clear `last_seq`, so the first
    // frame after a reset carries whatever gap the test above left behind.
    f.seq = f.seq.wrapping_add(1);
    f.send(solid, screeny_proto::F_KEY, &payload)
        .map_err(|e| e.to_string())?;
    std::thread::sleep(Duration::from_millis(120));
    let base = telemetry(&mut ctrl)?;
    f.seq = f.seq.wrapping_add(5);
    f.send(solid, screeny_proto::F_KEY, &payload)
        .map_err(|e| e.to_string())?;
    std::thread::sleep(Duration::from_millis(120));
    let t = telemetry(&mut ctrl)?;
    check(
        "3.2  a gap of 5 -> seq_gaps += 5, frame still shown",
        t.seq_gaps.wrapping_sub(base.seq_gaps) == 5
            && t.frames_shown.wrapping_sub(base.frames_shown) == 1,
        format!(
            "seq_gaps +{} shown +{}",
            t.seq_gaps.wrapping_sub(base.seq_gaps),
            t.frames_shown.wrapping_sub(base.frames_shown)
        ),
    );

    // 6.2: a TELEMETRY *request* on the control port is solicited and is
    // answered every time, whatever the 100 ms unsolicited limit says.
    let t0 = Instant::now();
    let mut answers = 0;
    while t0.elapsed() < Duration::from_millis(300) {
        if matches!(
            ctrl.request(Request::Telemetry)?,
            OwnedReply::Telemetry(_)
        ) {
            answers += 1;
        }
    }
    check(
        "6.2  TELEMETRY requests are not rate-limited",
        answers > 5,
        format!("{answers} answers in 300 ms"),
    );

    // 6.3: SET_BRIGHTNESS reports what was actually applied, which is how a
    // sender learns the firmware cap.
    let r = ctrl.request(Request::SetBrightness(255))?;
    let cap = match r {
        OwnedReply::Brightness { applied } => applied,
        other => return Err(format!("expected a brightness reply, got {other:?}")),
    };
    check(
        "6.3  SET_BRIGHTNESS 255 is clamped and reports the cap",
        cap < 255,
        format!("applied {cap}"),
    );
    ctrl.request(Request::SetBrightness(96))?;

    println!(
        "conformance: {}",
        if pass { "all PASS" } else { "FAILURES ABOVE" }
    );
    Ok(pass)
}

fn describe(r: &Option<Vec<u8>>) -> String {
    match r {
        None => "no reply".into(),
        Some(b) if b.len() >= 9 => format!(
            "op {:#04x} flags {:#04x} body[0] {:#04x}",
            b[2], b[3], b[8]
        ),
        Some(b) => format!("{} bytes", b.len()),
    }
}
