# screeny (card 009)

The host side: a library that takes frames and gets the best picture a 64x32
LED panel can show inside one UDP datagram onto it at a steady 30 fps, and the
one `screeny` binary that wraps it.

[`docs/design/protocol-v1.md`](../../docs/design/protocol-v1.md) is normative
and [`screeny-proto`](../proto) is its executable form; this crate is the half
that runs on a Mac. One binary on purpose: macOS Local Network permission and
code signing are per binary identity (card 015), so everything that talks to
the LAN lives in `screeny`.

```
cargo run -p screeny -- discover
cargo run --release -p screeny -- pattern bars
cargo test -p screeny
cargo bench -p screeny            # encode cost, by stage
```

Use `--release` for anything that streams. A debug build's encoder is roughly
ten times slower and will not hold 30 fps.

## The library in one screen

```rust
// --- frames ------------------------------------------------------------
struct Frame;                                  // 64x32 sRGB, derefs to [u8; 6144]
    Frame::black() / solid(c) / from_bytes(&[u8]) / from_pixels(Box<..>)
    get(x,y) set(x,y,c) at(p) set_at(p,c) as_bytes() distinct_colours()

struct FrameTime { index: u64, elapsed: Duration, fps: f64 }

trait FrameSource {                            // the seam demos plug into
    fn render(&mut self, t: FrameTime, out: &mut Frame) -> bool;   // false = end
    fn name(&self) -> &str;
}
struct FnSource<F>;                            // a closure as a FrameSource
struct RawReader<R>;                           // 6144-byte RGB888 frames from a reader
enum Pattern { Bars, Grey, Gradient, F, Checker, Sweep }   // also a FrameSource

// --- encode (spec 4) ---------------------------------------------------
struct EncodeConfig { codecs: Vec<u8>, profile: Profile, panel: Panel,
                      hysteresis: f64, dither: Dither, measure_stages: bool }
enum Profile { Full, Fast }
struct Encoded { codec: u8, payload: Vec<u8> }

struct Encoder;
    Encoder::new(EncodeConfig)
    fn encode(&mut self, &Rgb888Frame, budget: usize) -> Encoded
    fn encode_indexed(&mut self, &IndexedFrame, budget) -> Result<Encoded, DecodeError>
    fn last_stats(&self) -> FrameStats      // codec, bytes, elapsed, colours,
                                            // candidates, exact, score, stages
    fn set_codecs(Vec<u8>) / set_profile(Profile) / reset()

const MIN_BUDGET: usize;                    // 1376, the PAL5 floor
mod score;  fn mean_de(&Panel, a, b) -> f64 // panel-aware Oklab dE

// --- panel model (card 002) --------------------------------------------
struct Panel { bits: u32, subframes: u32 }
    emit1(v) emit(c) emit_oklab(c) emit_lut() distinct_levels()
const NOMINAL, TEMPORAL, DIMMED, DEEP;      // selection scores against TEMPORAL

// --- discovery (spec 5) ------------------------------------------------
struct Target { addr: Option<SocketAddr>, name: Option<String>,
                timeout: Option<Duration>, broadcast: bool }
    fn resolve(&self) -> Result<Device>
fn discover::browse(Duration, want: Option<usize>) -> Result<Vec<Device>>
fn discover::broadcast_probe(Duration, port) -> Result<Vec<Device>>   // spec 5.5

struct Device { instance, host, frame: SocketAddr, control: SocketAddr,
                addresses: Vec<IpAddr>, info: Option<DeviceInfo> }
struct DeviceInfo { txtvers, proto, w, h, codecs: Vec<u8>, codecs_raw,
                    mtu, ctrl, fw, id, name }
    parse(&[u8]) from_pairs(..) to_txt() supports(id) best_codec(&[u8])
    common_codecs(&[u8]) budget() speaks_v1()

// --- control (spec 6) --------------------------------------------------
struct ControlClient;                       // 250 ms timeout, 3 retries, same req_id
    connect(SocketAddr) ping() info() telemetry() set_brightness(u8)
    identify(ms) set_idle(IdleMode) reset_stats() release() set_name(&str)
    get_wifi() reboot() request(&Request, f)

// --- sender (spec 3, 6.9, 9) -------------------------------------------
struct SenderConfig { fps, budget: Option<usize>, profile, stats_interval,
                      adapt: bool, timestamps: bool, qos: bool, handshake: bool }
struct Sender;
    Sender::connect(Device, SenderConfig) -> Result<Sender>    // GET_INFO handshake
    fn run(&mut self, &mut dyn FrameSource, &AtomicBool) -> Result<()>
    fn run_with(.., &mut dyn FnMut(&SendStats))                // live stats
    fn send_frame(&mut self, &Rgb888Frame, final_frame: bool) -> Result<u8>
    fn finish() poll_feedback() stats() budget() device() local_addr()

struct SendStats { frames_sent, frames_skipped, bytes, by_codec, encode_total,
                   encode_max, min_gap, max_gap, fps, fps_changes, telemetry,
                   busy, decode_failures, codecs_withdrawn, codec_limited }
    actual_fps() mean_bytes() mean_encode() encode_pct(p)

fn sender::period_of(fps) -> Duration
fn sender::sleep_until(Instant)             // sleep, then spin the last ms

// --- errors ------------------------------------------------------------
enum Error { Io, Timeout, Device(ErrorCode), BadReply, NotFound, NoSuchDevice,
             Mdns, Metadata, NoCommonCodec, Budget }
    fn hint(&self) -> Option<String>        // the next step, in words
```

### Quick start

```rust
use std::sync::atomic::AtomicBool;
use screeny::{Pattern, Sender, SenderConfig, Target};

let device = Target::default().resolve()?;              // or Target { addr, .. }
let mut sender = Sender::connect(device, SenderConfig::default())?;
sender.run(&mut Pattern::Bars, &AtomicBool::new(false))?;
```

Your own renderer is a `FrameSource`:

```rust
use screeny::{FnSource, Frame, FrameTime};

let mut src = FnSource::new("sweep", |t: FrameTime, f: &mut Frame| {
    let x = (t.secs() * 30.0) as usize % 64;
    for y in 0..32 { f.set(x, y, [255, 255, 255]); }
    true                                                 // false ends the stream
});
```

Encoding on its own, with no network anywhere:

```rust
use screeny::encode::{EncodeConfig, Encoder};

let mut enc = Encoder::new(EncodeConfig::default());
let out = enc.encode(&frame, 1464);                      // (codec id, payload)
screeny_proto::decode(out.codec, &out.payload, &mut dst)?;   // always succeeds
```

## The commands

Every command takes the target options, and `--addr` always works even when
discovery does not:

| option | |
|---|---|
| `--addr IP[:PORT]` | talk to this address, skipping discovery entirely |
| `--name NAME` | pick a discovered device by instance or friendly name |
| `--timeout SECS` | how long to browse for (default 3) |
| `--broadcast` | use the broadcast `GET_INFO` probe instead of mDNS (spec 5.5) |
| `-v`, `--verbose` | more detail |

| command | what it does |
|---|---|
| `discover` | browse `_screeny._udp` and print every device, its ports, panel size, codecs, mtu and firmware. With `--addr`, describes that one. Finding nothing is normal and prints what to try next. |
| `info` | `GET_INFO` on the control port: the authoritative metadata, which can differ from a cached TXT record. |
| `stats [-n N] [--interval S] [--reset]` | follow telemetry (spec 6.7): per-second frame counts, the four drop counters by cause, inter-arrival and jitter, decode time, RSSI. |
| `brightness N` | set brightness 0-255; prints what the firmware actually applied, which is how you learn its cap. |
| `identify [--ms N]` | flash the "which one is this?" pattern. |
| `reboot --yes` | reboot the device. The flag is required. |
| `ping [-c N] [--interval S]` | round-trip time to the control port, with min/mean/median/max. |
| `pattern NAME [--list]` | stream a built-in test pattern. |
| `pipe` | stream raw 6144-byte RGB888 frames from stdin. |
| `encode-stats` | run the chooser over frames and print codec, size and time. Sends nothing. |

Streaming commands (`pattern`, `pipe`) share:

| option | |
|---|---|
| `--fps N` | target frame rate (default 30) |
| `--duration SECS` | stop after this long |
| `--budget BYTES` | payload budget; defaults to the device's advertised `mtu` |
| `--fast` | the faster encode profile (card 031) |
| `--no-adapt` | do not react to telemetry (spec 6.9) |
| `--timestamps` | set `HAS_TS` and stamp each frame |
| `--qos` | ask for interactive-video QoS; untested on the bench AP (card 013) |
| `--no-handshake` | skip `GET_INFO` and assume a v1 device |

`encode-stats` also takes `--budget`, `--fast`, `--pattern NAME`, `-n N`,
`--per-frame` and `--quality`.

### The patterns

| name | what it shows |
|---|---|
| `bars` | eight saturated bars over a grey ramp: colour, and channel order |
| `grey` | two ramps, one linear in code value and one linear in light: the gamma LUT is right when the second looks even |
| `gradient` | a smooth 2D RGB field: banding, dither structure, and the codec's worst case for colour count |
| `f` | a large asymmetric "F" with four different corner markers: rotation and mirroring |
| `checker` | a 1px checkerboard inside a coarse one: ghosting, row crosstalk, and the LZ coder's worst case |
| `sweep` | a bar moving across and down: tearing, latency, dropped frames |

### Examples

```sh
screeny discover
screeny info --name screeny-a4cf12
screeny pattern bars --duration 10
screeny pattern sweep --fps 30 --fast -v
screeny stats --reset -n 30
screeny brightness 40

# a renderer somewhere else, piped in
my-renderer --raw | screeny pipe --fps 30

# what the chooser would do, without sending anything
screeny encode-stats --pattern gradient -n 60 --quality
my-renderer --raw | screeny encode-stats --per-frame
```

## How it decides what to send

Per frame (spec 4.8):

1. Build the colour histogram once.
2. Walk the quality ladder, best first, and stop at the first rung that fits
   the budget: `PAL4_LZ` or `PAL8_LZ` on the frame's own colours (lossless),
   raw `PAL5` on its own colours if 32 or fewer (also lossless, and
   fixed-rate so it cannot overflow), then adaptive palettes at
   256/128/64/32/16 colours, then dithered `PAL5` as the floor.
3. **If that rung was lossless, send it.** Nothing can beat zero error, so
   the rest is skipped - which is most pixel-authored content.
4. Otherwise also encode dithered `PAL5` and `BC1_DUAL`, decode each
   candidate through `screeny_proto` - the decoder the firmware runs - score
   them by mean Oklab dE through a model of the panel *with* temporal
   dithering, give the previous frame's codec an 8% advantage, and send the
   winner.

Two things hold for every frame and every budget: the payload never exceeds
the budget, and it decodes. The second is true by construction, because
scoring decodes every candidate before choosing it.

A `SOLID` frame costs three bytes. An `IndexedFrame` of 256 colours or fewer
skips all of the above.

### Cost

Measured on an M-series laptop, release, over the five card 002 clips
(`cargo bench -p screeny` reproduces the shape of this from its own corpus):

| profile | mean | p95 | worst frame | mean dE |
|---|---|---|---|---|
| lab hybrid (card 002) | 6.03 ms | 12.75 ms | 16.11 ms | 6.14 |
| `Full` | **2.08 ms** | 4.92 ms | 9.44 ms | **6.08** |
| `Fast` | **1.16 ms** | 2.70 ms | 3.92 ms | 7.19 |

`Full` is the default and is what card 002 measured, only faster; `Fast`
shortens the LZ chains, cuts the Lloyd iterations and scores one pixel in
four, for about 18% more dE. Both leave most of the 33.3 ms frame period to
whatever is generating the frames.

## Pacing

Spec 9.1, and the two rules that matter:

- **Absolute schedule.** Frame `n` is due at `start + n * period`, so error
  cannot accumulate. Sleep to a millisecond before the target, then spin.
- **Skip, never burst.** If a render or encode runs long, the frames that
  were missed are skipped and the stream resumes on the next whole slot. The
  device shows newest-wins and queues about five frames below its socket, so
  a catch-up burst would only fill those queues and add latency to every
  frame after it.

Measured over ten seconds against the in-process receiver: 30.00 fps within
1%, no drift, and no two frames closer than a third of a period.

`STATS_REQ` rides on about one frame a second (spec 6.4), so the steady state
is exactly one packet per frame from the sender. The telemetry that comes back
drives spec 6.9's rules: sustained loss steps the frame rate down
30 -> 24 -> 20 -> 15 and back up after ten clean seconds, while frames being
*superseded* means the device cannot decode fast enough, so the codec set
drops to the cheap fixed-rate ones instead. A decode failure withdraws that
codec and says so, because it means one of the two implementations is wrong.

On exit - including ctrl-c - the sender sends one frame with `FINAL` so the
device releases the source lock at once rather than waiting out
`STREAM_TIMEOUT_MS`.

## Testing

No test touches the bench device. `tests/common` is a fake device built
directly on `screeny_proto`: two loopback sockets on ephemeral ports, the
spec's validation and sequence rules, proto's decoders, `STATS_REQ` answered
from the frame port per spec 6.4, the control opcodes, and a loss injector.

| file | what it pins |
|---|---|
| `tests/encode.rs` | every payload decodes and fits, for every pattern, adversarial frame and random frame, at budgets from 1464 down to 3; 32 colours or fewer are exact whatever the content; the advertised codec list is honoured; hysteresis |
| `tests/loopback.rs` | handshake, a stream arriving intact, pixel-exact low-colour frames, `STATS_REQ` cadence, 25% loss stepping the rate down, a clean link not adapting, every control op, timeouts and their hints |
| `tests/pacing.rs` | 30.0 fps within 1% over ten seconds, no drift, no burst; a 300 ms stall skipped rather than caught up; 10/24/60 fps |
| `tests/cli.rs` | the binary end to end: `pattern` at 30 fps, `pipe`, `encode-stats`, the control subcommands, and the failure messages |
| `tests/color.rs` | the fast cube root against libm, and the panel model against card 001's measurements |

`SCREENY_PACING_SECS` shortens the ten-second run while iterating.

## Things worth knowing before you build on it

- **The codec id is not in the payload.** The card 002 lab prepends a mode
  byte; the wire puts it in the header, so a lab payload is exactly one byte
  longer than the wire payload for the same frame. `Encoded` gives you the two
  separately.
- **Budgets below 1376 work but are not what card 002 measured.** The palette
  ladder's floor is a `PAL5` frame; below that the encoder falls through to
  `BC1_DUAL` (1296) and then `SOLID` (3), which always fits.
- **Use `--release`.** See above.
- **A browse that finds nothing is normal**, not an error. macOS can make
  multicast vanish until the socket is recreated; `dns-sd -B _screeny._udp`
  uses Apple's own responder and is the fastest way to tell "the device is not
  advertising" from "this process cannot see multicast". `--addr` always
  works.
- **Local Network permission.** A CLI run from Terminal or over SSH is exempt;
  a bundled, re-signed or launchd-started binary is not, and closing the
  Terminal window that started a running sender can revoke the exemption
  mid-run. Every error that could be caused by this says so. Doing anything
  about it is card 015.
- **The bring-up firmware advertises `codecs=raw`.** `screeny discover` finds
  it and prints exactly that, and `Sender::connect` refuses with
  `NoCommonCodec` rather than guessing. Card 008 is the firmware side.
