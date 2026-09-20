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

## The HTTP API (card 224)

The simulator serves **the device's own HTTP API** - every route in
`screeny_device_api::route::ROUTES`, with that crate's types, its error shape
and its status codes. The firmware (card 222/223) serves the same definition
from the same crate, so the Studio's device page and the `screeny-probe`
conformance subcommand can be built against this with no hardware.

```
cargo run -p screeny-sim -- --headless                  # API on :8080
cargo run -p screeny-sim -- --headless --http-port 9000
cargo run -p screeny-sim -- --headless --no-http        # UDP only
```

Port **8080**, never 80: binding 80 needs root and nothing on this bench runs
as root. `--http-port 0` binds an ephemeral one and the banner prints it;
that is what `Config::for_test()` does, so tests never collide.

**A second simulator still starts.** When 8080 was not asked for by name and is
already taken, the port falls back to an ephemeral one, a line on stderr says so
and the banner prints the address it really got - running two or three
simulators at once must not fail over an HTTP port nobody chose. Pass
`--http-port N` and a busy `N` *is* an error: that one is going to be connected
to.

`GET /` is a one-line placeholder that says the real page is card 222's and
links the routes. The HTML page will be shared with the firmware rather than
written twice.

### A curl per route

```
curl -s localhost:8080/api/v1/status
curl -s localhost:8080/api/v1/telemetry
curl -s localhost:8080/api/v1/networks
curl -s localhost:8080/api/v1/wifi

curl -s -X POST localhost:8080/api/v1/wifi \
     --data-urlencode 'ssid=Example-Wifi1' --data-urlencode 'psk=password9'

curl -s -X POST localhost:8080/api/v1/settings \
     -H 'content-type: application/json' \
     -d '{"brightness":96,"idle_mode":"status","name":"Desk panel"}'

curl -s -X POST localhost:8080/api/v1/firmware \
     -H 'content-type: application/octet-stream' --data-binary @app.bin

curl -s -X POST localhost:8080/api/v1/reboot \
     -H 'content-type: application/json' -d '{"confirm":"RBOO"}'

curl -s -X POST localhost:8080/api/v1/identify \
     -H 'content-type: application/json' -d '{"duration_ms":10000}'
```

Failures are one shape, `{"error":"<code>","detail"?:"..."}`, and the HTTP
status is a property of the code (`screeny_device_api::ErrorCode::status`).

Three things the simulator does **not** pretend about:

- `POST /api/v1/firmware` accepts the stream, discards it and reports
  `written`. It runs the two of research 006's five image checks that need no
  image parser - the `0xE9` magic and the 2 MiB slot length - and **installs
  nothing**. The other three (chip, project name, checksum, SHA-256) are not
  claimed.
- `POST /api/v1/reboot` does exactly what UDP `REBOOT` does: accepted, logged,
  not acted on. It does draw a new `boot_id`, which is the one thing a client
  can tell a restart by; `uptime_ms` keeps climbing, because resetting it would
  change what the simulator does on UDP.
- `fw_slot`, `fw_state`, `reset_reason`, `stack_free`, `heap_used`,
  `heap_size` and `store_errors` are constants. There is no flash here and no
  stack worth measuring.

### The captive-portal catch-all

A request whose `Host` is not the device's own gets research 007 section 4.3's
answer: while the portal is up, `302` to `http://192.168.4.1/` **with a
non-empty body** (iOS needs content to pop the sheet; Android calls a
`Content-Length <= 4` answer a failure rather than a portal); otherwise `404`.
The rule is about the shape of the `Host` - the portal IP, any bare IP
literal, `localhost`, `<instance>.local` - and no probe domain is named
anywhere in the code.

```
curl -s -o /dev/null -w '%{http_code} %{redirect_url}\n' \
     -H 'Host: captive.apple.com' localhost:8080/hotspot-detect.html
```

The soft-AP side proper - DHCP, the DNS catch-all - cannot be simulated
honestly on a host and is not attempted.

## WiFi, scripted (cards 081 and 224)

There is no radio, so the join outcome is chosen rather than discovered. The
join/portal state machine itself is **`screeny_provision`'s**, the same one
the firmware drives, so the two cannot drift.

```
cargo run -p screeny-sim -- --headless --start-in-portal
cargo run -p screeny-sim -- --headless --wifi-result fail
cargo run -p screeny-sim -- --headless --wifi-result slow --wifi-join-ms 500
cargo run -p screeny-sim -- --headless --link-down
```

| flag | what it does |
|---|---|
| `--wifi-result ok` | every join attempt succeeds (the default) |
| `--wifi-result fail` | every attempt fails with a wrong password: `failed` / `auth` |
| `--wifi-result slow` | the radio never answers, so the attempt runs into the machine's own timeout: `failed` / `other` |
| `--wifi-join-ms MS` | how long a scripted attempt takes (default 200) |
| `--start-in-portal` | boot with an empty store, which is what a factory-fresh device is |
| `--wifi-ssid` / `--ap-ssid` | the stored SSID, and the soft-AP's name on the portal screen and in its QR |
| `--link-down` | start with the link down (spec section 7.3) |

The boot join with `ok` completes at time zero, so the default simulator is
`CONNECTED` from its first instant exactly as it always has been; every later
join takes `--wifi-join-ms`.

In `Portal` and `Trial` the panel shows **`screeny_provision::screen::render`'s
own pixels**, so the window and the `--dump-dir` PNGs are what the device will
show, down to the QR's polarity and its three-pixel lit quiet zone. With the
link down, the idle screen says `NO NETWORK` rather than printing an address
the device can no longer be reached at.

`GET_WIFI`, the telemetry `state` byte's `PROVISIONING` overlay and
`GET /api/v1/wifi` all read that one machine, so they cannot disagree. Both
`SET_WIFI` over UDP and `POST /api/v1/wifi` feed it, and both reply **before**
the radio work, as spec section 8.2 requires.

**No PSK, anywhere.** Nothing in this crate stores one, no event has a field
for one, and `tests/http_wifi.rs` greps every reply, every event's `Debug` and
the snapshot for the posted secret.

## Discovery

It advertises `_screeny._udp` as **`screeny-sim`**, never `screeny`. The real
device on this bench answers to `screeny`, and a second responder claiming that
name would make discovery a coin toss. `instance_is_reserved` refuses
`screeny`, `screeny.local` and `screeny-<six hex digits>` before a socket is
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

Since card 224 it also serves the device's HTTP API and models the whole WiFi
join/portal life through `screeny_provision` - see the two sections above.

**Does not:** ghosting, refresh banding, temporal dithering (card 030), the
panel's real primaries, a radio, a soft-AP (DHCP and the DNS catch-all cannot
be simulated honestly on a host), flash, or a real restart. `REBOOT` is
accepted, logged and deliberately not acted on beyond a new `boot_id`. The PSK
is never stored and never logged - spec section 8.4's invariant holds here too,
and the `SetWifi` event has no field to put one in.

The window is not a photograph. It is close enough to judge dithering and thin
lines by, which is what the generative-art brief asks a preview for.

## Tests

112 of them, all on loopback and ephemeral ports, none needing mDNS, a
display or a network. Every HTTP request in the suites has a five-second
timeout and every wait is bounded.

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
| `tests/http_routes.rs` | every row of `screeny_device_api::route::ROUTES` served and parsed back **as the API's own types**; the error shape and status per code; the route's request bound; the scan rate limit; the captive-portal catch-all against all five OS probes; `boot_id` across a reboot; the listener released by `shutdown()` |
| `tests/http_wifi.rs` | a failed trial over HTTP and over UDP `SET_WIFI` reaching the same state with the store untouched; a successful trial committing once; a radio that never answers; `--wifi-result` changed between attempts; the button wipe; link down -> `HOLD` -> the `NO NETWORK` idle screen; the portal screen compared byte for byte with `screeny_provision`'s own render; the PSK in no reply, event or log line |
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
`screeny-proto`, `screeny-panel`, `screeny-receiver`, `screeny-device-api`,
`screeny-provision`, `serde`/`serde_json`, `mdns-sd` and `png`.

There is deliberately **no HTTP crate and no async runtime**: the server is
~350 lines over `TcpListener` in `src/http.rs`. This crate is
`crates/studio`'s dev-dependency, so whatever it links, everybody links.
