# screeny-sim (card 006)

A fake Tidbyt. It binds the two UDP ports, speaks the whole of
[`docs/design/protocol-v1.md`](../../docs/design/protocol-v1.md)'s receive
side, decodes every v1 codec, answers every control op, advertises itself over
mDNS, and draws what the panel would show as round LED dots.

There is one real panel on this bench and it has one owner. Everything else -
the sender (card 009), the demos (card 010), anybody writing generative art -
develops against this.

It is also the **second** implementation of the receiver. The firmware
(card 008) will be the third. Every disagreement between two readings of the
spec that this crate hit is written up in spec section 11, items 14-30.

```
cargo test -p screeny-sim          # 97 tests, all loopback, no mDNS, no window
cargo run  -p screeny-sim          # the window, on 49374/49375, advertising
cargo run  -p screeny-sim -- --help
```

## Running it

Windowed, which is the default:

```
cargo run -p screeny-sim
cargo run -p screeny-sim -- --scale 20 --name "Desk panel"
```

The window is 64x32 dots at `--scale` (default 14, minimum 4), on black, with a
four-line statistics strip underneath: fps, the codec and payload size of the
last frame, the stream state, the counters, the drops split by cause, and the
active source. Escape or closing the window stops it.

Headless, which is what a script or a CI job wants:

```
cargo run -p screeny-sim -- --headless
cargo run -p screeny-sim -- --headless --exit-after 30 --quiet
cargo run -p screeny-sim -- --headless --dump-dir /tmp/frames --dump-every 30
```

`--headless` logs one statistics line a second, plus a line for anything worth
noticing (a lock changing hands, a control op, a rejected packet). `--verbose`
logs every event. `--dump-dir` writes displayed frames as 64x32 PNGs of the
**decoded** pixels - no panel model, no brightness - so they diff cleanly
against what a sender thought it encoded. `--dump-every N` writes one frame in
every N, and it is exact: the hook is on the frame thread, not a poll.

Pretending to be a bad day:

```
cargo run -p screeny-sim -- --drop 5          # 5% of frames never arrive
cargo run -p screeny-sim -- --delay-ms 30     # a random 0..30 ms hold: jitter
cargo run -p screeny-sim -- --decode-ms 25    # too slow to draw what it gets
```

`--drop` models loss on the air, so nothing is counted: the packet never
happened, and what the device sees is a hole in the sequence numbers.
`--decode-ms` really does sleep, so frames really do pile up and
`frames_dropped_superseded` really does rise. That distinction - loss versus
overload - is the one spec section 6.9 asks a sender to respond to in opposite
ways, and it is the main reason this crate has fault injection at all.

`--fault-seed N` makes a lossy run repeat exactly.

## Discovery

It advertises `_screeny._udp` as **`screeny-sim`**, never `screeny`. The real
device on this bench answers to `screeny`, and a second responder claiming that
name would make discovery a coin toss. `instance_is_reserved` refuses
`screeny`, `screeny-4a00a4.local` and `screeny-<six hex digits>` before a socket is
opened, so the mistake is a startup error rather than an afternoon.

```
$ dns-sd -B _screeny._udp
Timestamp     A/R Flags if Domain  Service Type    Instance Name
13:50:27.438  Add     3 10 local.  _screeny._udp.  screeny-sim
13:50:27.652  Add     3 10 local.  _screeny._udp.  screeny
```

The TXT record is not built here: it is the same bytes the device answers
`GET_INFO` with, taken apart into key/value pairs, which is spec section 6.6's
whole point. `--no-mdns` turns it off; tests never use it.

## The library

This is what an integration test drives, and it is the entire API:

```rust
use std::time::Duration;
use screeny_sim::{Config, SimDevice};

// Loopback, ephemeral ports, no mDNS.
let dev = SimDevice::start(Config::for_test())?;
let sim = dev.handle();                  // cheap, cloneable, Send + Sync

let frame_addr = dev.frame_addr();       // ask, because port 0 was requested
let control_addr = dev.control_addr();

// ... send FRAME datagrams built with screeny_proto::FramePacket ...

let shot = sim.wait_for_frames(1, Duration::from_secs(1)).unwrap();
assert_eq!(&shot.decoded[..], expected_pixels);   // bit-exact, no panel model
assert_eq!(shot.telemetry.frames_dropped_decode, 0);
```

| | |
|---|---|
| `SimDevice::start(cfg)` | bind and run. `start_with(cfg, sink)` adds a per-displayed-frame callback |
| `SimDevice::{frame_addr, control_addr, handle, shutdown}` | dropping the device stops it too |
| `SimHandle::snapshot()` | one lock: the decoded frame, the panel-model frame, the displayed frame's metadata, the state, the active source, the counters, brightness, idle mode, name |
| `SimHandle::telemetry()` | the counters alone, as a `screeny_proto::control::Telemetry` |
| `SimHandle::render_display()` | what the panel is scanning out: stream frame, idle screen, cross-fade, `IDENTIFY` overlay |
| `SimHandle::info_bytes()` | the `GET_INFO` / TXT bytes |
| `SimHandle::{wait_for_frames, wait_until}` | block on a snapshot predicate |
| `SimHandle::{event_cursor, events, wait_for}` | the `Event` stream, for things no counter records |
| `SimHandle::{set_faults, faults}` | change the injected faults while it runs |

`Snapshot::decoded` is the frame **exactly as the sender sent it**. Assert on
that. `Snapshot::panel` is the same frame through the panel model, and
`render_display()` adds the idle screens and overlays; neither is bit-exact by
design.

`Config` carries the ports, the identity, the brightness cap, the panel model,
the faults, and `Timing`. `Timing::SPEC` is the spec's own section 7.2
constants and is the default; a test that does not want to wait out a ten
second `HOLD_MS` overrides them.

### Driving the state machine without sockets

`Core` is the receive state machine with no I/O and no wall clock in it - every
method takes `now_us`. `tests/core_rules.rs` uses it to check `LOCK_MS` and
`HOLD_MS` to the microsecond, and to use source addresses on different subnets
that loopback will not hand out.

```rust
let mut core = Core::new(&Config::for_test());
let mut out = Outbox::default();
core.offer_frame(now_us, from, &datagram, &mut out);   // spec 2, 3.2, 7.4
core.flush_frames(now_us, &mut out);                   // spec 3.3, 4.7
core.tick(now_us, &mut out);                           // spec 7.3's timers
core.control(now_us, from, &datagram, &mut out);       // spec 6
// out.from_frame_sock, out.from_control_sock, out.events
```

## What it models, and what it does not

**Does:** the whole wire protocol; the source lock, takeover and idle state
machine; the counters and both EWMAs, in the same integer arithmetic the
firmware will use; brightness and its cap; the panel's sRGB -> linear -> 64
duty levels -> sRGB quantisation; round dots with a gap and a little bloom.

**Does not:** ghosting, refresh banding, temporal dithering (card 030), the
panel's real primaries, or Wi-Fi. `GET_WIFI` answers a fixed SSID and
`CONNECTED`; `SET_WIFI` and `REBOOT` are accepted, logged and deliberately not
acted on. The PSK is never logged - spec section 8.4's invariant holds here
too, and the `SetWifi` event has no field to put one in.

The window is not a photograph. It is close enough to judge dithering and thin
lines by, which is what the generative-art brief asks a preview for.

## Tests

97 of them, all on loopback and ephemeral ports, none needing mDNS or a
display.

| file | what it pins |
|---|---|
| `tests/codecs.rs` | all 27 of `crates/proto`'s checked-in vectors, plus payloads built here from section 4's prose, arrive **bit-exact**; `HAS_TS`; padding; unsupported codecs leave the panel alone |
| `tests/sequence.rs` | wrap, duplicates, reordering, gaps, newest-wins under a slow decode, a new source resetting `last_seq`, the section 6.8 EWMAs |
| `tests/arbitration.rs` | adoption, `BUSY` and its rate limit, takeover, `FINAL`, `RELEASE`, the stream timeout, `HOLD`, every idle mode, `IDENTIFY` as an overlay - with compressed timings, and once with the spec's own |
| `tests/core_rules.rs` | the same rules on a virtual clock, exact to the microsecond, with senders on different IPs |
| `tests/control.rs` | every opcode, every error code, `req_id` 0, `ERR_VERSION`, the `GET_INFO` rate limit and its retry exemption, wrong-port packets |
| `tests/telemetry.rs` | `STATS_REQ` answered on the frame port, the 100 ms limit, section 6.9's loss-versus-overload split, the section 6.7 offsets |
| `tests/malformed.rs` | every `Reject` variant, 4000 random datagrams on both ports, corrupt payloads at every codec's exact length |
| `tests/faults.rs` | drop, delay and decode injection, the seed, and the frame sink |
| `tests/cli.rs` | the built binary, headless: the stats line, the PNGs, the flags, the refusal to claim `screeny` |
| unit tests | the panel model's LUT, the two fonts (rendered as ASCII art so a human can read them), the idle screens and cross-fade, the LED dot profile, PNG round-tripping |

The bit-exactness tests lean on `crates/proto/tests/vectors`, whose expected
pixels came out of the frozen card-002 lab and owe nothing to either this crate
or `proto`'s decoders. The hand-built encoders in `tests/common/mod.rs` - a
palette packer per codec and a small greedy LZSS - were written from the spec
rather than from the decoders, so round-tripping through them tests two
independent readings of section 4 against each other.

## Feature flags

`window` is on by default and pulls in `minifb`. Without it the library is
unchanged, the binary needs `--headless`, and the dependency tree is
`screeny-proto`, `mdns-sd` and `png`.
