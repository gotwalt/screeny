# 003 - Wire protocol, discovery and transport

Research for card 003. Conclusions first; evidence and links below. The concrete
draft spec that falls out of this is `docs/design/protocol-v1-draft.md`.

Research done 2026-09-19. Everything version-numbered below was checked against
crates.io / docs.rs / upstream source on that date, not from memory.

---

## 1. Conclusions

### 1.1 Do not adopt an existing protocol; do borrow from DDP

| Protocol | Header | Frame fit for 64x32 | Verdict |
|---|---|---|---|
| WLED DRGB / DNRGB (UDP 21324) | 2 / 4 bytes | RGB888 only, 490 px/pkt -> 5 pkts | Reject: strip-shaped, no codec field, no discovery |
| DDP (UDP 4048) | 10 bytes (14 w/ timecode) | 480 px/pkt -> 5 pkts | Reject as primary, worth a secondary mode later |
| Art-Net ArtDmx (UDP 6454) | 18 bytes | 170 px/universe -> 13 universes | Reject: DMX-shaped, absurd for a panel |
| E1.31 / sACN | 126 bytes | 170 px/universe -> 13 universes | Reject: header alone is 8.6% of an MTU |
| Pixelflut | TCP text, per-pixel | n/a | Reject: not a frame protocol; fun toy sender later |
| Tidbyt stock / Tronbyt | HTTPS poll or WebSocket, animated WebP | whole-image push | Nothing to interop with; no LAN streaming at all |

**None of them has a codec id.** That single byte is the whole point of this
project (card 002 is choosing a compressed pixel format so a full frame fits one
datagram), and no LED protocol in the wild carries one, because they all assume
raw RGB. A 64x32 RGB888 frame is 6144 bytes and cannot fit one datagram in any
of these, so "be compatible" would mean giving up the one-packet-per-frame rule.
We therefore define our own, and steal DDP's good ideas:

- DDP's discipline of a tiny header (10 bytes) explicitly to maximise payload
  per Ethernet frame. We go smaller (8) because we have fewer jobs to do.
- DDP's `push` flag semantics (the packet that says "display now").
- DDP's split of the id space into pixel data vs. control/config/status
  messages carried as structured payloads on the same protocol
  (`ID` 1 = pixel data, 246 control, 250 config, 251 status, 255 broadcast).
  We do the same split but on a separate port and with a binary body.
- DDP's `Query`/`Reply` flag pair for request/response. We use an explicit
  `REPLY` bit for the same reason.

**Recommendation on a secondary DDP mode: yes, but not in v1.** Speaking DDP-in
would make the panel work with xLights / LedFx / any DDP sender for free, and
the parser is trivial. But it requires a 6144-byte RGB888 reassembly buffer in
SRAM, which competes directly with the HUB75 DMA framebuffer (card 001's memory
budget is the gate), and none of the planned senders need it. Design for it now
(keep the display pipeline able to accept "a complete RGB888 frame" from a
source that is not our decoder), implement it when SRAM is known. Card 040
filed.

### 1.2 Header: 8 bytes, and here is what each one buys

Full layout is in the draft spec. Summary of the reasoning:

- **1 byte magic + 1 byte version/type.** We are on a dedicated UDP port, so
  this is not about demultiplexing; it is about refusing to render garbage when
  something else on the LAN scans the port, and about a v2 device politely
  rejecting a v1 sender instead of drawing noise. Packing type into the low
  nibble of the version byte saves a byte and gives us 16 packet types, which
  is plenty.
- **1 byte codec id.** Required by the card. Also does double duty as the
  per-frame mode switch card 002 wants (text frames lossless-RLE, visualizer
  frames lossy) with no extra field.
- **1 byte flags.** `KEY` is needed because card 002 is explicitly allowed to
  propose delta codecs, and a delta codec over a lossy transport needs the
  receiver to know which frames are self-contained. `STATS_REQ` piggybacks
  telemetry requests on the frame stream so the common case ("how am I doing?")
  costs zero extra packets from the sender. `FINAL` lets a well-behaved sender
  release the panel instantly instead of waiting out a timeout. `HAS_TS` is the
  escape hatch for one-way latency measurement (card 013) without a v2.
- **2 bytes sequence.** u8 wraps every 8.5 s at 30 fps, which is too short for
  a loss window and makes "is this newer?" ambiguous after a stall. u16 wraps
  every 36.4 minutes. Needed for newest-wins ordering, loss counting and gap
  detection.
- **2 bytes payload length.** Redundant with the UDP length, deliberately. It
  gives the `no_std` decoder a bound to validate against before it indexes
  (panicking on a malformed packet from the LAN is not acceptable), it permits
  trailing padding so a codec may emit fixed-size packets, and it lands the
  pixel payload at offset 8 so a codec reading `u32`s off the payload stays
  4-byte aligned relative to the packet start.

**Deliberately absent: a timestamp.** Four bytes every frame is 0.27% of the
pixel budget for something we do not need. Inter-arrival jitter is a
*differential* measurement and needs no clock sync: the device computes it from
its own monotonic clock. One-way latency needs synchronised clocks, which we do
not have, and is measured instead with a control-port `PING` (round trip / 2)
or, when card 013 needs it, by setting `HAS_TS`. **Deliberately absent: a source
id.** The UDP 4-tuple already identifies the sender, for free.

### 1.3 Two ports, not one

Frames on UDP **49374** (0xC0DE), control/telemetry on UDP **49375** (0xC0DF),
both in the dynamic/private range so there is nothing to collide with in the
IANA registry. The real port is published in the DNS-SD SRV record and the
`ctrl=` TXT key, so these are defaults, not constants senders may assume.

Rationale for two: the frame socket wants a *shallow* receive buffer that is
drained in a newest-wins loop and lets the stack drop the rest (section 1.5); a
control request that arrives in the middle of a 30 fps burst must not be one of
the packets that gets dropped, and a control reply needs a bigger, different
lifetime. Separate sockets also keep the hot receive path free of a branch on
packet type. The cost is one extra `UdpSocket` with a 1-packet-deep buffer -
about 1.6 KB - which is cheap next to the 5.9 KB frame socket.

### 1.4 ESP32 Wi-Fi behaviour: the findings that change the design

**Power save.** ESP-IDF's default is `WIFI_PS_MIN_MODEM`, under which received
data can be delayed by up to a full DTIM period, and field reports show ping
times blowing out from ~2.4 ms to over 2000 ms with DTIM=1
([esp-idf#9766](https://github.com/espressif/esp-idf/issues/9766)). Espressif's
own guide says `WIFI_PS_NONE` gives "minimum latency for receiving Wi-Fi data in
real time". **Good news for us:** `esp-radio` 0.18's `PowerSaveMode` derives
`Default` as `None`, and `esp_radio::wifi::new()` calls
`controller.set_power_saving(PowerSaveMode::default())` at init, so bare-metal
Rust starts with modem sleep *off*. The action item is therefore "never call
`set_power_saving` with anything else, and assert it in the firmware", not
"remember to disable it".

**Realistic latency and jitter.** The best public apples-to-apples numbers for
ESP32 over a real 2.4 GHz infrastructure network (two boards, HT20, 72 Mbps PHY,
~5 m from the AP, 8 other clients on the band) are Electric UI's benchmark:
median round trip ~9 ms for a 12-byte UDP payload and ~10 ms for 1024 bytes,
with **outliers around 25 ms** after tuning, and 50 ms+ before tuning. Halve for
one-way: **~4-5 ms typical, ~12 ms tail.** Separately, an Espressif issue
measuring `sendto`-to-`recvfrom` on a quiet link reports 540 +/- 200 us for the
raw path, but with periodic pile-ups of 10-20 ms after which ~10 packets come
out back to back ([esp-idf#15345](https://github.com/espressif/esp-idf/issues/15345)).
That burst-after-stall shape is the important one: **loss and jitter on this
link are bursty, not Poisson**, and the recovery is a burst, not a smooth
catch-up.

Consequences taken into the design:
- The 33.3 ms frame period is 3-8x the typical one-way latency but only ~2.5x
  the tail. Occasional late frames are normal and must not look like a glitch.
- A sender must never burst to catch up; the device's queues will just eat it.
- The device must drop old frames itself rather than let them queue.

**Queue depths that actually exist.** `esp_radio::wifi::ControllerConfig`
defaults: `rx_queue_size: 5` frames, `static_rx_buf_num: 10` (~1.6 KB each),
`dynamic_rx_buf_num: 32`, `ampdu_rx_enable: true`, `rx_ba_win: 6`. So there are
already ~5 frames (166 ms at 30 fps) of buffering between the radio and
embassy-net before we add a socket buffer. AMPDU aggregation is on by default,
which is what produces the "several packets arrive in one clump" pattern.

**Broadcast and multicast are second-class.** RFC 9119 is explicit: multicast
frames are not acknowledged, are sent at the lowest basic rate, and are buffered
by the AP until the next DTIM beacon whenever any associated station has power
save on - which on a home network is always true of somebody's phone. **Never
send frame data to a broadcast or multicast address.** Unicast only. Multicast
is used for exactly one thing: mDNS, where a lost query is retried anyway.

**Fragmentation.** embassy-net 0.9.1 gates IPv4 reassembly behind an opt-in
`ipv4-reassembly` feature and has no default features at all, so unless card 001
turns it on the device will silently drop any IP-fragmented datagram. Keeping
every packet <= 1472 bytes of UDP payload is not a nicety, it is a hard
requirement, and the sender should set `IP_DONTFRAG` so an over-budget frame
fails loudly on the host instead of vanishing on the device.

**Surprise worth flagging to card 001:** embassy-net 0.9.1 no longer depends on
smoltcp. It depends on **xarxa**, dirbaio's rewrite of smoltcp (single global
packet pool, zero-copy, works on packet bytes rather than `Repr` structs), and
pulls it as a *git* dependency pinned to a rev, not from crates.io. Feature
names moved with it (`multicast`, `ipv4-reassembly`, `mdns`, `iface-bind`).
`Stack::join_multicast_group(addr) -> Result<(), MulticastError>` is synchronous
and exists, gated on the `multicast` feature, and `UdpSocket::set_hop_limit`
exists, which mDNS needs (RFC 6762 wants TTL 255).

### 1.5 Show immediately; no playout buffer

Recommendation for this use case: **decode and display the newest frame the
moment it arrives. No reordering buffer, no playout delay.**

- The target content is an interactive visualizer and a fractal zoom. A 1-frame
  buffer costs a fixed 33 ms and a 2-frame buffer 67 ms, to hide jitter that is
  typically ~5 ms and tails to ~12-25 ms.
- The panel is refreshed by I2S DMA at a rate far above 30 Hz, so a frame that
  is 10 ms late does not tear or blank - the previous frame simply stays lit for
  a few more refresh cycles. That is visually identical to what a 1-frame
  playout buffer would have produced, at zero added latency.
- SRAM spent on playout buffers is SRAM not spent on the DMA framebuffer.
- Most of the jitter that matters is *sender-side* pacing jitter, which is far
  cheaper to fix on a macOS host with a monotonic absolute schedule than to
  paper over on the device.

The one thing the device must do is **decode into a back buffer and swap at a
panel refresh boundary**, so a half-decoded frame is never scanned out.

Concrete newest-wins receive loop: keep the frame socket's rx buffer shallow
(4 metadata slots, 4 x 1472 bytes payload), and on wake drain it with
`poll_recv_from` until it would block, keeping only the newest acceptable frame
and counting the rest as `frames_dropped_superseded`. Latency is then bounded by
decode time, not by queue depth, and the `frames_dropped_superseded` counter is
exactly the signal "the sender is faster than I can draw".

### 1.6 Discovery: `_screeny._udp.local.`

`_screeny._udp` is 8 characters, inside RFC 6763 §7's 15-byte limit for a
service name. Instance name defaults to `screeny-<last 3 MAC bytes in hex>`,
user-settable. SRV port is the frame port; TXT carries `txtvers`, `proto`, `w`,
`h`, `codecs`, `mtu`, `ctrl`, `fw`, `id`, `name`. Total TXT is well under the
1300-byte ceiling RFC 6763 §6.2 recommends.

- **Device side:** `edge-mdns` 0.8.0 (async, `no_std`, no-alloc, responder) on
  `edge-nal-embassy` 0.9.0, which already requires `embassy-net ^0.9` - so the
  versions line up with the current stack. It needs the embassy-net `multicast`
  feature and an explicit `join_multicast_group(224.0.0.251)`. Alternative if
  `edge-mdns` fights us: `hick-embassy` 0.2.0 (mDNS/DNS-SD for embassy on
  embassy-net, `no_std` + `alloc`) - but it needs an allocator and has ~500
  downloads, so treat it as plan B. Compile-testing one of these belongs to
  card 001's question 4.
- **Host side:** `mdns-sd` 0.21.3 (5.2M downloads, updated 2026-09-08, pure
  Rust, no async runtime dependency, runs its own daemon thread). Clear winner
  over `astro-dnssd` (Bonjour FFI, last touched 2025) and `zeroconf` (also FFI,
  needs Avahi on Linux). `simple-mdns` 0.7.0 is a reasonable second.
- **Fallback:** a unicast or subnet-broadcast `GET_INFO` on the control port,
  because macOS multicast has been flaky (see below) and because `--addr` must
  always work.

**macOS Local Network privacy, as of 2026.** Since Sequoia (15) and continuing
through Tahoe/26, connecting to local addresses needs the Local Network
permission. The important practical fact for this project: **command-line tools
run from Terminal or over SSH, and their child processes, are exempt** - so
`cargo run` in a terminal will just work and will not prompt. Caveats worth
knowing:
- Closing Terminal while a child process is still running can lose that
  exemption and break its local connections mid-run.
- A GUI app or a launchd agent is *not* exempt: it needs
  `NSLocalNetworkUsageDescription` and will prompt. If we ever ship a menu-bar
  sender, that is where the pain is.
- There is a real, acknowledged bug class where the kernel caches a *denial*
  after reboot because an IPC to the privacy daemon failed; symptom is multicast
  silently not arriving. Apple DTS's answer is toggle the permission off/on, or
  close and reopen the socket. Fixed in a 26.5 beta for the main case, still
  present in edge cases. **Design implication:** the sender must treat "mDNS
  browse returned nothing" as normal-and-recoverable - retry, and always offer
  `--addr` - never as fatal.
- `dns-sd -B _screeny._udp` is the zero-dependency way to check whether the
  device is advertising, since it goes through Apple's own responder.

### 1.7 Control and telemetry

Same 8-byte header shape as frames (so one parser), on the control port, with a
`req_id` the device echoes and a `REPLY` flag. Ops: `PING`, `GET_INFO`,
`GET_STATS`, `SET_BRIGHTNESS`, `IDENTIFY`, `SET_IDLE`, `RESET_STATS`, `RELEASE`,
`REBOOT`, `GET_WIFI`, `SET_WIFI`.

`GET_INFO`'s reply body is **DNS-SD TXT wire format** - a run of length-prefixed
`key=value` strings, the exact bytes of the mDNS TXT record. One table in the
firmware, one parser in the sender, and a sender that was handed a bare IP gets
identical metadata to one that browsed mDNS.

Telemetry is a fixed 48-byte little-endian struct (full field table in the
spec): uptime, frames received / shown / dropped split four ways by cause,
sequence gaps, EWMA and max inter-arrival, EWMA jitter, EWMA and max decode
time, max render time, RSSI, brightness, state, last codec.

**How a sender learns it is overrunning.** Set `STATS_REQ` on roughly one frame
per second; the device replies with a telemetry packet to that frame's source
port, so the steady-state cost is zero extra sender packets. Then:

- `frames_rx` rising slower than frames sent => **network-limited**. Loss on the
  air or in the AP's downlink queue. Response: step the frame rate down the
  ladder 30 -> 24 -> 20 -> 15 fps after 3 consecutive bad seconds, step back up
  after 10 clean ones.
- `frames_rx` keeping up but `frames_dropped_superseded` non-zero =>
  **decode-limited**. The device is receiving everything and throwing frames
  away because the previous one is still being drawn. Response is a cheaper
  codec, not a lower frame rate. The CLI should say which of the two it is;
  that distinction is the main reason the drop counters are split by cause.
- `frames_dropped_decode` non-zero => a codec bug or a codec the device does not
  actually support. Hard error, not a tuning signal.

### 1.8 Arbitration and idle

**Last writer wins, with a 500 ms lock.** The device binds to the 4-tuple of the
first sender it sees; a different source's frames are rejected until 500 ms
(15 frames) have passed with nothing from the incumbent. A rejected source gets
at most one `BUSY` notification per second so a CLI can say something useful
instead of appearing to work. `RELEASE`, or a frame with the `FINAL` flag, drops
the lock immediately. 500 ms is chosen so a sender that hiccups for a few frames
does not lose the panel, while a human switching senders does not wait.

**Idle: hold, then fade to a status screen. Never blank to black** - a black
panel is indistinguishable from a broken one. After 1 s with no frames the
stream is considered stopped and the lock released (state `HOLD`, last frame
still lit). After 10 s in `HOLD`, cross-fade over 500 ms to a status screen
showing the device name, IP and RSSI, which doubles as "where is my panel and
what is it called".

### 1.9 Runtime Wi-Fi provisioning, ranked by `no_std` effort

| # | Option | Effort in `no_std` | Verdict |
|---|---|---|---|
| 1 | Serial console command over the existing USB-UART | Lowest: a line-reader task + persistence | **v1** |
| 2 | `SET_WIFI` control packet | Low: the control channel and persistence already exist; add an op and a rejoin state machine | **v1** |
| 3 | Improv Wi-Fi over Serial | Small delta over 1: 6-byte `IMPROV` header + version + type + length + checksum, RPC 0x01 "send Wi-Fi settings" | v1.1 - buys a browser-based installer for ~150 lines |
| 4 | SoftAP + captive config page | Medium-high: AP mode, DHCP *server*, a DNS responder for the captive portal, an HTTP server, HTML, a scan list. This is what the Tronbyt firmware does, at 10.10.0.1 | v2, only if it must be provisioned with no cable |
| 5 | BLE / Improv over BLE | Highest: BLE stack plus single-radio Wi-Fi/BLE coexistence, big RAM cost | Not recommended here |

**v1 recommendation: 1 and 2 together**, credentials in `sequential-storage` on
a dedicated flash partition, plus a compile-time fallback (the `Example-Wifi1`
credentials) used when storage is empty. Add a project-specific rule that costs
nothing and saves a lot of bench time: **if the stored credentials fail to join
three times, fall back to the compiled-in ones; if those also fail, say so on
the panel.** The device is a display - it should tell you why it is not working
instead of making you open a serial monitor.

Improv Serial (option 3) is worth listing separately because it is an actual
published standard, not a bespoke command set: `IMPROV` (6 bytes) + version
`0x01` + type + length + data + checksum, with types 0x01 current state,
0x02 error state, 0x03 RPC command, 0x04 RPC result; RPC 0x01 is "send Wi-Fi
settings", 0x02 "request current state", 0x04 "request scanned networks".
Implementing 1 in a way that is one refactor away from 3 is free.

**Security.** The owner has said the Wi-Fi password is not a secret, so v1 leaves
`SET_WIFI` and `REBOOT` unauthenticated on the LAN. Do not over-engineer this.
But record what an untrusted-LAN deployment would need, because it is not
obvious after the fact:
- The dangerous ops are `SET_WIFI` and `REBOOT`; everything else is at worst
  vandalism. Minimum viable hardening is a device PIN that `IDENTIFY` displays
  on the panel, carried in mutating control packets together with a monotonic
  counter to stop replay.
- The real version is a PIN-authenticated key exchange (SPAKE2 is the right
  primitive) yielding a ChaCha20-Poly1305 session; `chacha20poly1305` builds
  `no_std`.
- `GET_WIFI` must return the SSID and never the PSK, and the PSK must never
  appear in telemetry or on the panel. That rule holds even in v1.
- Frame traffic itself is not worth authenticating - the attack is "a stranger
  draws on your LED panel" - and the 500 ms arbitration lock already keeps it to
  an annoyance rather than a takeover.

Card 041 filed for the hardening work.

---

## 2. Evidence

### 2.1 Prior art

**DDP** ([spec](http://www.3waylabs.com/ddp/), and the layout as implemented by
[`ddp-rs`](https://crates.io/crates/ddp-rs) 1.3.0, whose `protocol/mod.rs`
documents it byte by byte). 10-byte header, 14 with timecode, **big-endian**:

```
0      flags: bits 7:6 version (01), 0x10 timecode, 0x08 storage,
              0x04 reply, 0x02 query, 0x01 push
1      sequence number, low nibble, 1-15, 0 = unused
2      pixel config / data type
3      destination id: 1 default output, 246 control, 250 config,
              251 status, 254 DMX, 255 broadcast
4..8   offset into the display buffer, u32 BE (bytes, not pixels)
8..10  data length, u16 BE
10..14 optional timecode, u32 BE
```

Port 4048. `MAX_DATA_LENGTH = 480 * 3 = 1440`. Control/config/status payloads
are JSON objects (`{"status":{"man":..,"mod":..,"ver":..,"mac":..}}`,
`{"config":{"ip":..,"ports":[...]}}`). WLED implements DDP on 4048 and
[does not read the optional timecode](https://kno.wled.ge/interfaces/ddp/).
`ddp-rs`'s `FrameBuilder` is explicitly allocation-free and documented as usable
on bare metal, so if we ever do a DDP sender it is free.

**WLED realtime UDP** ([docs](https://kno.wled.ge/interfaces/udp-realtime/)),
port 21324 by default: byte 0 is the protocol id (1 WARLS, 2 DRGB, 3 DRGBW,
4 DNRGB, 5 DNRGBW), byte 1 is *a timeout in seconds* after which the device
returns to its normal mode (1-2 recommended, 255 = never). Max 490 RGB pixels
per DRGB packet; DNRGB adds a 2-byte start index so a strip can span packets.
Two ideas are worth stealing and one is worth avoiding: the **idle timeout in
the stream itself** is a good idea (we express it as a device-side constant plus
a `FINAL` flag instead, so it costs no per-frame bytes), and the fact that they
needed DNRGB at all is the argument against strip-shaped protocols for a panel.

**Art-Net** ([spec](https://art-net.org.uk/downloads/art-net.pdf)): UDP 6454,
ArtDmx header is 18 bytes (`"Art-Net\0"` 8, opcode 2, protocol version 2,
sequence+physical 2, universe 2, length 2), one universe = 512 channels = 170
RGB pixels. 64x32 needs 13 universes per frame. **E1.31/sACN**: ~126 bytes of
root/framing/DMP layers before any data. Both are the wrong shape and the wrong
overhead.

**Tidbyt**: the stock and community (`tronbyt/firmware-esp32`) firmwares fetch
**animated WebP** either by HTTP polling a URL or over a WebSocket, selected by
the scheme of `REMOTE_URL`. There is no LAN streaming mode and no mDNS; the
config portal is a SoftAP at a fixed 10.10.0.1. So there is no Tidbyt-compatible
thing to speak, and their SoftAP portal is the reference point for provisioning
option 4 above.

### 2.2 ESP32 Wi-Fi

- Espressif,
  [Wi-Fi Performance and Power Save](https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-guides/wifi-driver/wifi-performance-and-power-save.html):
  under modem sleep, received data can be delayed by up to a DTIM period
  (min power save) or a listen interval (max power save); `WIFI_PS_NONE`
  gives minimum receive latency.
- [esp-idf#9766](https://github.com/espressif/esp-idf/issues/9766): with
  DTIM=1 and modem sleep on, ping RTT ranged from 2.40 ms to **2278 ms**, with
  multiple samples over 1000 ms. This is the failure mode we are avoiding.
- [esp-idf#15345](https://github.com/espressif/esp-idf/issues/15345): raw
  `sendto`-to-`recvfrom` measured at **540 +/- 200 us**, but with pile-ups of
  10-20 ms followed by ~10 packets emitted back to back. Reporter had already
  tried tick-rate changes, TX buffer reduction, QoS settings and disabling power
  management; still open. Treat bursty stalls as a property of the link.
- [Electric UI, latency across wireless links for microcontrollers](https://electricui.com/blog/latency-comparison):
  two ESP32s, HT20 at 72 Mbps PHY, ~5 m from an AP with 8 other 2.4 GHz
  clients. Tuned: **median ~9 ms RTT at 12 B, ~10 ms at 1024 B, outliers ~25
  ms**; untuned WebSocket outliers past 50 ms. TCP/UDP/WebSocket differ little
  under these conditions.
- [`esp-radio` 0.18.0 `wifi` module](https://docs.espressif.com/projects/rust/esp-radio/0.18.0/esp32/esp_radio/wifi/index.html)
  and its source: `PowerSaveMode { None, Minimum, Maximum }` with
  `impl Default -> None`; `WifiController::set_power_saving`;
  `apply_power_saving` maps to `WIFI_PS_NONE` / `WIFI_PS_MIN_MODEM` /
  `WIFI_PS_MAX_MODEM`. `ControllerConfig::default()` is `rx_queue_size: 5`,
  `tx_queue_size: 3`, `static_rx_buf_num: 10` ("approximately 1.6KB of RAM"
  each), `dynamic_rx_buf_num: 32`, `static_tx_buf_num: 0`,
  `dynamic_tx_buf_num: 32`, `ampdu_rx_enable: true`, `ampdu_tx_enable: true`,
  `amsdu_tx_enable: false`, `rx_ba_win: 6`. `new()` calls
  `set_power_saving(PowerSaveMode::default())`. Also needs opt-level 2 or 3 to
  work at all, and `release_max_level_off` for performance.
- [RFC 9119, Multicast Considerations over IEEE 802 Wireless Media](https://www.rfc-editor.org/rfc/rfc9119.pdf):
  multicast/broadcast frames are unacknowledged, sent at the lowest basic rate,
  and buffered by the AP until the next DTIM beacon whenever any associated
  station has power save enabled.

### 2.3 Device network stack

- `embassy-net` **0.9.1**. Its `Cargo.toml` has **no `default` feature list** -
  every protocol is opt-in - and it depends on
  [**xarxa**](https://github.com/embassy-rs/xarxa) (git, pinned rev), not
  smoltcp. Relevant features: `udp`, `ipv4`, `medium-ethernet`, `dhcpv4`,
  `multicast` ("Join IP multicast groups, with IGMP (IPv4) and MLD (IPv6)
  membership reports"), `ipv4-reassembly` ("Reassemble incoming IPv4
  fragments"), `ipv4-fragmentation`, `mdns`, `hostname`, `iface-bind`.
- xarxa's README: it is dirbaio's rewrite of smoltcp (smoltcp maintainer
  2020-2026) with a single global packet pool, owned packet handles, zero-copy,
  and core logic operating on packet bytes instead of `Repr` structs. Same
  `no_std`, no-alloc goals. Relevant to card 001 because the dependency is a git
  rev and API names have moved.
- `Stack::join_multicast_group(impl Into<IpAddress>) -> Result<(), MulticastError>`
  and `has_multicast_group` are **synchronous**; `wait_config_up` etc. are async.
- `UdpSocket::new(stack, rx_meta, rx_buffer, tx_meta, tx_buffer)`; has
  `poll_recv_from`, `recv_from_with`, `packet_recv_capacity`,
  `payload_recv_capacity` and `set_hop_limit` (needed for mDNS's TTL 255).
- smoltcp 0.14.0 for comparison: `proto-ipv4-fragmentation` is in its *default*
  feature list, and reassembly defaults to a 1500-byte buffer with 1 concurrent
  reassembly. embassy-net does not inherit that default, hence the warning
  above.

### 2.4 Discovery

- [RFC 6763](https://www.rfc-editor.org/rfc/rfc6763) §7: a Service Name is at
  most 15 bytes (17 with the underscore and length byte). §6.1: each constituent
  string of a TXT record is at most 255 bytes. §6.2: keep the whole TXT record
  under 1300 bytes so it fits one Ethernet packet; larger is NOT RECOMMENDED.
  §6.5 recommends `txtvers` as the first key.
- Host crates, checked on crates.io 2026-09-19: **`mdns-sd` 0.21.3**, updated
  2026-09-08, 5,268,988 downloads, "mDNS Service Discovery library with no async
  runtime dependency", runs its own daemon thread, `ServiceDaemon::new()` /
  `browse(ty)` returning a channel of `ServiceEvent`, `ResolvedService` with
  `get_addresses` / `get_port` / `get_property_val_str`, interface selection via
  `IfPredicate` / `InterfaceId`. `simple-mdns` 0.7.0 (81 k downloads),
  `zeroconf` 0.18.0 (FFI to Bonjour/Avahi), `astro-dnssd` 0.3.6 (FFI, last
  published 2025-06).
- Device crates: **`edge-mdns` 0.8.0** (2026-06-25), "Async + `no_std` +
  no-alloc implementation of an mDNS responder", built on `domain` 0.12,
  `heapless` 0.9, optional `edge-nal` 0.7 / `embassy-sync` 0.8 /
  `embassy-time` 0.5. **`edge-nal-embassy` 0.9.0** depends on
  `embassy-net ^0.9` and `edge-nal ^0.7` - consistent with the rest of the
  stack. Plan B: `hick-embassy` 0.2.0 (`no_std` + `alloc`), 515 downloads.
- macOS Local Network privacy:
  [How local network privacy could affect you](https://eclecticlight.co/2026/01/14/how-local-network-privacy-could-affect-you/)
  (Jan 2026) - Sequoia and Tahoe; "Command tools run from Terminal or using SSH,
  including their child processes" are exempt, but "closing Terminal while child
  processes are still running can lose their exemption"; daemons, root
  processes, Bonjour, AirPlay and printing are also exempt; there is currently
  no way to reset an app's Local Network setting.
  [Apple Developer Forums 809211](https://developer.apple.com/forums/thread/809211):
  Apple DTS explains that after a reboot the kernel's permission cache is empty,
  an IPC to the user-context privacy daemon can fail, the kernel fails secure
  and **caches the denial**; symptom is multicast silently not arriving.
  Workarounds are toggling the permission or closing and reopening the socket;
  main case fixed in a macOS 26.5 beta, edge cases open.
  [TN3179](https://developer.apple.com/documentation/technotes/tn3179-understanding-local-network-privacy)
  is the reference for anything with a bundle.

### 2.5 Sender-side pacing and socket options

- `std::time::Instant` is monotonic on macOS (backed by
  `clock_gettime(CLOCK_UPTIME_RAW)` / `mach_absolute_time`). `SystemTime` is
  not, and must never be used for pacing.
- [`spin_sleep`](https://docs.rs/spin_sleep): `thread::sleep` accuracy varies by
  platform and state; the crate sleeps for the bulk of the interval and spins
  the last ~100 us. Spin-waiting gives sub-microsecond precision at the cost of
  burning a core. For 30 fps, sleeping until `target - 1 ms` and spinning the
  remainder is plenty and costs ~3% of one core.
- QoS: the standard DSCP-to-WMM mapping puts video traffic in `AC_VI`. On macOS
  the blessed API is `SO_NET_SERVICE_TYPE` (introduced in macOS 10.11/iOS 10)
  with `NET_SERVICE_TYPE_VI = 3` ("Interactive Video"), which the OS maps
  transparently to a DSCP value and/or a WMM access category depending on the
  link; Apple is explicit that these are categories of delay/jitter/loss
  tolerance, not priorities. Portable fallback is `IP_TOS` with AF41 (0x88).
  Whether a given consumer AP honours DSCP on the **downlink** to the ESP32 is
  the part that actually matters and is untested - measure it in card 013.

---

## 3. Open questions

Carried into the draft spec's own open-questions list; repeated here so this
report stands alone.

1. Codec ids and the real pixel budget - card 002 owns the table.
2. Is 1464 bytes of payload right, or should we leave headroom for a path with a
   sub-1500 MTU (VPN, some mesh APs)? `mtu=` in TXT lets the device state it;
   the sender needs a `--mtu` override either way.
3. Should a 1-frame playout buffer be a runtime toggle? Only the camera harness
   (cards 012/013) can say whether it helps, and only against real jitter.
4. DDP secondary receive mode in v1.1 or never - gated on card 001's SRAM
   budget. Card 040.
5. Is `HAS_TS` enough for card 013's one-way latency measurement, or does that
   card need a synchronised clock (SNTP on the device) anyway?
6. Ports 49374/49375 - fine, or should we ask IANA-registered space? (No, but
   record the decision.)
7. Does the bench AP honour DSCP/`SO_NET_SERVICE_TYPE` on the downlink?
8. `edge-mdns` vs `hick-embassy` on embassy-net 0.9.1/xarxa - needs a compile
   test, which is card 001's question 4.
9. Multi-device: one sender driving two panels is only addressed by discovery
   today. Does anything in the protocol need to change, or is it purely a
   sender-side concern?
10. On Wi-Fi disconnect mid-stream, is `HOLD` then status screen right, or
    should the panel say "disconnected" immediately?
