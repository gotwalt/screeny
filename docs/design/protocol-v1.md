# screeny protocol v1

Status: **accepted** (card 004, 2026-09-19). This is the source of truth for the
wire format. Transport, discovery and control came from card 003
(`docs/research/003-protocol-transport.md`); the codec set came from card 002
(`docs/research/002-frame-encoding.md`). Changes after this point go through a card
and bump `txtvers`/`proto` as described in section 5. Remaining open questions are
listed at the end; none block implementation.

Conventions in this document:

- All multi-byte integers are **little-endian**. (The ESP32 is little-endian and
  so is every host we care about; no byte swapping anywhere.)
- Byte offsets are zero-based and relative to the start of the UDP payload.
- "MUST", "SHOULD", "MAY" are used in the RFC 2119 sense.
- Reserved bits and fields MUST be written as zero. A receiver MUST ignore
  reserved bits it does not know, except where stated otherwise.

---

## 1. Transport and sizing

| | |
|---|---|
| Transport | UDP over IPv4. No TCP, no IPv6 in v1. |
| Frame port (default) | **49374** (0xC0DE) |
| Control port (default) | **49375** (0xC0DF) |
| Max UDP payload | **1472** bytes = 1500 MTU - 20 IPv4 - 8 UDP. Firmware MUST build with `ESP_RADIO_CONFIG_WIFI_MTU=1500`; esp-radio defaults to 1492, which silently caps payloads at 1464 (card 001). |
| Fixed header | **8** bytes |
| Max pixel payload | **1464** bytes |
| Nominal frame rate | 30 fps (33.333 ms period) |
| Delivery model | One frame per datagram. No retransmission. Newest wins. |

Both ports are defaults. The frame port is authoritative in the DNS-SD `SRV`
record and the control port in the `ctrl=` TXT key; senders MUST use the
discovered values when they have them and MAY fall back to the defaults when
given a bare IP address.

A sender MUST NOT emit a datagram whose UDP payload exceeds 1472 bytes, and
SHOULD set `IP_DONTFRAG` (macOS/BSD) or `IP_MTU_DISCOVER=IP_PMTUDISC_DO` (Linux)
so that an over-budget datagram fails locally rather than being fragmented. The
device does not reassemble IP fragments: embassy-net's `ipv4-reassembly` feature
is off unless explicitly enabled, so a fragmented datagram is silently dropped.

A sender MUST NOT send frame data to a broadcast or multicast address. Unicast
only. (802.11 sends multicast unacknowledged, at the lowest basic rate, buffered
to the DTIM beacon - see RFC 9119.) Multicast is used only for mDNS.

---

## 2. Common header

Every screeny packet on either port begins with the same 8 bytes. The meaning of
bytes 2, 3, 4-5 and 6-7 depends on the packet type in byte 1.

```
 offset  size  frame packet        control packet
 ------  ----  ------------------  ---------------------
 0       1     magic = 0x53 's'    magic = 0x53 's'
 1       1     ver_type            ver_type
 2       1     codec               op
 3       1     flags               flags
 4       2     seq        (u16le)  req_id       (u16le)
 6       2     len        (u16le)  len          (u16le)
 8       ...   pixel payload       body
```

### 2.1 `magic` (offset 0)

Always `0x53`. Cheap rejection of anything else that lands on the port. A packet
whose first byte is not `0x53` MUST be discarded without further parsing and
counted in `frames_rejected`.

### 2.2 `ver_type` (offset 1)

```
 bits 7..4  protocol version, 1 for this document
 bits 3..0  packet type
```

Packet types:

| Value | Name | Port | Direction |
|---|---|---|---|
| `0x0` | `FRAME` | frame | sender -> device |
| `0x1` | reserved (`FRAME_FRAG`, multi-datagram frames) | frame | - |
| `0x2` | `CONTROL` | control (also frame port, see §6.4) | both |
| `0x3`-`0xF` | reserved | - | - |

So a v1 frame's byte 1 is `0x10` and a v1 control packet's is `0x12`.

A receiver MUST discard any packet whose version is not 1 and count it in
`frames_rejected`. A device MAY reply to a `CONTROL` packet of an unknown
version with an `ERR_VERSION` error using version 1 framing.

A receiver MUST discard a `FRAME` received on the control port and a `CONTROL`
received on the frame port, **except** for the device's own replies described in
§6.4.

### 2.3 `len` (offset 6..8)

Number of bytes of payload/body following the 8-byte header. A receiver MUST
check `8 + len <= datagram_length` and discard the packet otherwise. Bytes
beyond `8 + len` are padding and MUST be ignored; a sender MAY pad. For a
`FRAME`, `len` MUST be `<= 1464`.

`len` is deliberately redundant with the UDP datagram length. It gives the
`no_std` decoder a bound to validate before indexing, it allows fixed-size
datagrams for codecs that prefer them, and it puts the payload at offset 8 so
that payload `u32` reads stay 4-byte aligned relative to the packet.

---

## 3. FRAME packet

```
 0  u8    magic      0x53
 1  u8    ver_type   0x10
 2  u8    codec      codec id, see §4
 3  u8    flags      see below
 4  u16   seq        little-endian, wraps mod 2^16
 6  u16   len        pixel payload length, 0..=1464
 8  ..    payload    opaque to the transport; decoded per `codec`
```

If `flags.HAS_TS` is set, the first **4 bytes of the payload** are a little-endian
`u32` sender timestamp in microseconds (free-running, arbitrary epoch, wrapping
every 71.6 minutes) and the pixel payload begins at offset 12 and is `len - 4`
bytes long. A v1 receiver MUST parse and skip these 4 bytes correctly even if it
ignores the value; this is the one flag bit that changes the layout, and it
exists so latency instrumentation can be added without a version bump.

### 3.1 `flags` (offset 3)

| Bit | Mask | Name | Meaning |
|---|---|---|---|
| 0 | `0x01` | `KEY` | This frame decodes standalone, without reference to any previous frame. Stateless codecs MUST set it on every frame. |
| 1 | `0x02` | `STATS_REQ` | After processing this frame, send one `TELEMETRY` reply to this datagram's source address and port (see §6.4). |
| 2 | `0x04` | `FINAL` | Last frame of this stream. Release the source lock immediately after displaying. |
| 3 | `0x08` | `HAS_TS` | A 4-byte `u32le` sender timestamp (us) precedes the pixel payload. |
| 4-7 | | reserved | MUST be 0. A receiver MUST ignore bits it does not know. |

### 3.2 Sequence numbers

`seq` increments by one per frame *sent* and wraps mod 2^16 (36.4 minutes at
30 fps). It MUST NOT be reset except when the sender restarts a stream; it MAY
start at any value.

Comparison is RFC 1982 serial-number arithmetic:

```rust
/// True if `a` is newer than `b` under mod-2^16 wrapping.
#[inline]
fn newer(a: u16, b: u16) -> bool {
    let d = a.wrapping_sub(b);
    d != 0 && d < 0x8000
}
```

A receiver keeps `last_seq` for the *active source only* and resets it when the
active source changes (§7).

- `newer(seq, last_seq)` -> accept; add `seq.wrapping_sub(last_seq) - 1` to
  `seq_gaps`; set `last_seq = seq`.
- otherwise -> discard, `frames_dropped_stale += 1` (duplicate or reordered).

### 3.3 Receiving: newest wins

The device MUST NOT let frames queue. On each wake of the frame task:

1. Drain the socket with non-blocking receives until it would block. Validate
   each datagram (§2). Keep the newest acceptable frame; for every earlier
   acceptable frame that is thereby thrown away, `frames_dropped_superseded += 1`.
2. Decode the survivor into the **back** buffer, timing the decode into
   `decode_us`.
3. Swap the back buffer into the display at a panel refresh boundary, so a
   partially decoded frame is never scanned out. `frames_shown += 1`.

The frame socket's receive buffer SHOULD be shallow - 4 packet-metadata slots
and `4 * 1472` bytes of payload - so that when the decoder falls behind, the
stack drops packets rather than accumulating latency. Note that esp-radio
already queues up to `rx_queue_size` (default 5) frames below this.

The device MUST NOT delay display to smooth jitter. No playout buffer, no
reordering window beyond the `newer()` test. Rationale in the research report
§1.5.

---

## 4. Codecs

The header `codec` byte (offset 2) selects the decoder. It is the same value card
002's lab calls the *mode byte*; on the wire it lives **only in the header** and the
pixel payload starts directly with the codec's own fields. (The lab prepends it to
the payload, so lab sizes are one byte larger than wire sizes.)

All codecs are stateless: every frame decodes standalone, so every `FRAME` sets
`flags.KEY`. Multi-byte fields are little-endian. Index planes are raster order
(row-major, top-left origin), MSB-first within a byte. Decoders produce 64x32
RGB888 **sRGB**; gamma to linear panel duty is the display driver's job, shared by
all codecs. Decoders are integer-only, allocation-free and need no scratch RAM.

| Id | Name | Payload size | Use |
|---|---|---|---|
| `0x00` | reserved | - | MUST be rejected |
| `0x02` | `PAL5` | 1376 fixed | 32-colour adaptive palette; the always-fits floor |
| `0x10` | `PAL8_LZ` | variable, <= 1464 | up to 256-colour palette + LZ indices; the workhorse |
| `0x11` | `PAL4_LZ` | variable, <= 1464 | 16-colour palette + LZ nibbles; text and UI, bit-exact |
| `0x28` | `BC1_DUAL` | 1296 fixed | 4x4 block codec; photographic and many-colour content |
| `0x7F` | `SOLID` | 3 | whole frame one colour |
| `0xF0`-`0xFE` | experimental / private | | Devices MAY reject |
| `0xFF` | reserved | - | MUST be rejected |

Other ids in `lab/src/dec/mod.rs` (`0x01`, `0x03`, `0x20`-`0x27`, `0x30`, `0x31`)
are lab-only and reserved; v1 devices do not advertise them.

### 4.1 `0x02 PAL5`

```
[palette : 32 x RGB888 = 96 B][low nibbles : 1024 B][bit-4 plane : 256 B]
```
Pixel i has index `nib(i) | bit4(i) << 4`. Nibbles are two per byte, high nibble
first; the bit plane is 8 pixels per byte, MSB first.

### 4.2 `0x10 PAL8_LZ`

```
[n-1 : u8][palette : n x RGB888][LZ stream -> exactly 2048 index bytes]
```
`n` is 1..=256. Every decoded index MUST be `< n`; otherwise the frame is corrupt.
The decoder inflates into the front of the frame buffer and expands indices to
pixels in place, back to front.

### 4.3 `0x11 PAL4_LZ`

```
[palette : 16 x RGB888 = 48 B][LZ stream -> exactly 1024 bytes of packed nibbles]
```

### 4.4 LZ stream

The byte-oriented LZ format is defined by the reference decoder
`lab/src/dec/lz.rs` (to be lifted unchanged into `crates/proto`), which is
normative until this section is expanded into prose by card 005. A decoder MUST
bounds-check every literal run and match (offset within already-produced output,
length within the remaining output) and MUST reject a stream that produces more or
fewer bytes than the codec requires.

### 4.5 `0x28 BC1_DUAL`

```
[flags : 16 B, one bit per block, MSB first, raster order of blocks][128 blocks x 10 B]

flag 0:  [e0 : RGB565 u16le][e1 : RGB565 u16le][idx : 3 bitplanes x 2 B]   8 levels
flag 1:  [e0 : RGB888][e1 : RGB888][idx : 16 x 2 bits = 4 B]               4 levels
```
Blocks are 4x4 pixels, 16 across and 8 down. Level k is
`lerp(e0, e1, W[k])`, `lerp(a,b,w) = (a*(256-w) + b*w + 128) >> 8`, with
`W4 = [0,85,171,256]` and `W8 = [0,37,73,110,146,183,219,256]`. RGB565 endpoints
expand to 8 bits by bit replication. Interpolation is in sRGB space. Exact bit
order within the index fields is as in `lab/src/dec/block.rs` (normative, as 4.4).

### 4.6 `0x7F SOLID`

```
[R][G][B]
```

### 4.7 Rules

- A device advertises the ids it can decode in the `codecs=` TXT key and in
  `GET_INFO`. A v1 device MUST support all five. A sender MUST NOT send a codec the
  device did not advertise.
- A frame with an unsupported or reserved codec, or one whose decoder reports an
  error, is discarded and counted in `frames_dropped_decode`. The previous frame
  stays on the panel. A decoder MUST NOT leave a partially decoded frame visible:
  decode into the back buffer and swap only on success.
- `codec` is per-frame: a sender MAY switch codecs frame to frame with no negotiation.
- Future stateful codecs (deltas) would clear `flags.KEY`; after any discarded frame
  the device MUST discard non-`KEY` frames until the next `KEY` frame. v1 has none.

### 4.8 Sender codec selection (informative)

The reference sender encodes each frame as (a) the palette ladder
`PAL4_LZ -> PAL8_LZ at 256/128/64/32 colours -> PAL5`, (b) dithered `PAL5`, and
(c) `BC1_DUAL`; decodes each; scores mean Oklab dE through a model of the panel
*with* temporal dithering; gives the previous frame's codec an 8% advantage
(hysteresis, because a change in the character of the error is visible); and sends
the winner. A sender given an indexed frame of <= 16 or <= 32 colours can skip all
of that: `PAL4_LZ` or `PAL5` is exact. See card 002 for measurements.

---

## 5. Discovery (DNS-SD / mDNS)

### 5.1 Names

| | |
|---|---|
| Service type | `_screeny._udp` (8 chars; RFC 6763 §7 allows 15) |
| Domain | `local.` |
| Instance name | default `screeny-<xxxxxx>`, where `xxxxxx` is the last three bytes of the station MAC in lowercase hex. User-settable via `SET_NAME`. |
| Host name | `screeny-<xxxxxx>.local.` with an `A` record for the station IPv4 address |
| `SRV` target/port | the host name and the **frame** port |
| `SRV` priority/weight | 0 / 0 |
| Record TTLs | 120 s for `SRV`/`TXT`/`A`, per RFC 6762 §10 for non-hostname records |

The device MUST also answer `_services._dns-sd._udp.local.` enumeration, which
`edge-mdns` does for you.

### 5.2 TXT record

Keys are lowercase ASCII, values are ASCII unless noted. Order is as listed;
`txtvers` MUST be first (RFC 6763 §6.5).

| Key | Example | Meaning |
|---|---|---|
| `txtvers` | `1` | TXT schema version |
| `proto` | `1` | screeny wire protocol version(s) supported, comma-separated |
| `w` | `64` | panel width in pixels |
| `h` | `32` | panel height in pixels |
| `codecs` | `1,3,5` | decimal codec ids the device can decode, comma-separated, preference order (most preferred first) |
| `mtu` | `1464` | max pixel payload bytes the device accepts |
| `ctrl` | `49375` | control UDP port |
| `fw` | `0.1.0` | firmware version |
| `id` | `a4cf12` | stable short device id (MAC suffix) |
| `name` | `Desk panel` | friendly name, UTF-8, may contain spaces |

Total stays far under RFC 6763 §6.2's 1300-byte ceiling. A sender MUST tolerate
unknown keys and missing optional keys; `proto`, `w`, `h`, `codecs` and `ctrl`
are required and a service missing any of them MUST be ignored.

### 5.3 Device side

`edge-mdns` 0.8 over `edge-nal-embassy` 0.9. Requirements on the stack:

- embassy-net feature `multicast`, plus
  `stack.join_multicast_group(Ipv4Address::new(224, 0, 0, 251))` after the
  address is configured and again after any reconnect.
- A UDP socket bound to `0.0.0.0:5353` with `set_hop_limit(Some(255))`
  (RFC 6762 §11 requires IP TTL 255 on mDNS sends).
- Re-announce (RFC 6762 §8.3) on link-up and on IP change: 2-8 unsolicited
  announcements, first pair one second apart.
- Send a goodbye packet (TTL 0) on a clean shutdown or before rebooting for
  `SET_WIFI`, so senders drop the stale record promptly.

### 5.4 Host side

`mdns-sd` 0.21.3.

```rust
let daemon = ServiceDaemon::new()?;
let rx = daemon.browse("_screeny._udp.local.")?;
// ServiceEvent::ServiceResolved(info) -> info.get_addresses(), info.get_port(),
// info.get_property_val_str("codecs"), ...
```

Senders MUST:

- treat an empty browse result as normal and retriable, never fatal (macOS
  local-network-privacy bugs can make multicast silently vanish until the socket
  is recreated - research report §2.4);
- always accept an explicit `--addr host[:port]` that skips discovery entirely;
- prefer an IPv4 address on the same subnet as one of the host's interfaces
  when a service resolves to several.

### 5.5 Discovery fallback without mDNS

A device MUST also answer a `GET_INFO` control packet sent to the **subnet
broadcast address** on the control port, replying by unicast. This is the
"mDNS is broken on this machine today" escape hatch and the basis of
`screeny discover --broadcast`. It MUST be rate-limited to one reply per source
address per second. Frame data is still never broadcast.

---

## 6. Control and telemetry

### 6.1 Packet

```
 0  u8    magic      0x53
 1  u8    ver_type   0x12
 2  u8    op         opcode, §6.3
 3  u8    flags      bit0 REPLY (0x01), bit1 ERROR (0x02), bits 2-7 reserved
 4  u16   req_id     little-endian, chosen by the requester, echoed in the reply
 6  u16   len        body length
 8  ..    body       opcode-specific
```

- A request has `REPLY` clear. A reply has `REPLY` set, the same `op`, and the
  same `req_id`.
- An error reply has `REPLY|ERROR` set and a 1-byte body containing an error
  code from §6.5.
- `req_id` 0 means "no reply wanted"; the device MUST NOT reply to a request
  with `req_id == 0` except where an opcode says otherwise.
- The device MUST reply to the source address and port of the request.
- Requests are idempotent or explicitly guarded (`REBOOT`). There is no
  retransmission in the protocol: a sender that gets no reply within 250 ms
  SHOULD retry up to 3 times with the **same** `req_id`, then give up.

### 6.2 Unsolicited packets from the device

The device sends two packets nobody asked for, both with `REPLY` set and
`req_id == 0`:

- `TELEMETRY` (`op = 0x03`), in response to a frame with `STATS_REQ`, sent to
  that frame's source address and port (§6.4). Rate-limited to one per 100 ms
  per source.
- `BUSY` (`op = 0x0C`), to a source whose frames are being rejected because
  another sender holds the lock (§7). Rate-limited to one per second per source.

### 6.3 Opcodes

| Op | Name | Request body | Reply body |
|---|---|---|---|
| `0x01` | `PING` | empty | `u32le uptime_ms` |
| `0x02` | `GET_INFO` | empty | DNS-SD TXT wire format, §6.6 |
| `0x03` | `TELEMETRY` | empty | 48-byte struct, §6.7 |
| `0x04` | `SET_BRIGHTNESS` | `u8 level` (0-255, clamped to the firmware cap) | `u8 applied` |
| `0x05` | `IDENTIFY` | `u16le duration_ms` (0 = stop) | empty |
| `0x06` | `SET_IDLE` | `u8 mode` (§7.4) | `u8 mode` |
| `0x07` | `RESET_STATS` | empty | empty |
| `0x08` | `RELEASE` | empty | empty |
| `0x09` | `SET_NAME` | `u8 n` then `n` bytes UTF-8, `n <= 32` | empty |
| `0x0A` | `GET_WIFI` | empty | `u8 n` then `n` bytes SSID, then `u8 state` |
| `0x0B` | `SET_WIFI` | §8.2 | empty (reply sent **before** disconnecting) |
| `0x0C` | `BUSY` | - (device -> sender only) | `u8 reason`, `u32le lock_holder_ms_remaining` |
| `0x0D` | `REBOOT` | `u32le 0x4F4F4252` (`"RBOO"` little-endian) | empty (sent before rebooting) |
| `0x0E`-`0x7F` | reserved | | |
| `0x80`-`0xFF` | experimental / private | | |

Notes:

- `IDENTIFY` overrides the display for `duration_ms` with a high-contrast
  pattern plus the device name and IP. It is the "which one is this?" button and
  MUST work in any state, including while another sender holds the lock.
- `RELEASE` clears the source lock only if the requester currently holds it.
- `SET_BRIGHTNESS`, `SET_IDLE` and `SET_NAME` persist across reboot.
  `SET_BRIGHTNESS` is always clamped by the compile-time firmware cap (the panel
  runs off laptop USB); `applied` in the reply is the value actually in effect,
  which is how a sender learns the cap.
- An unknown opcode gets `ERR_UNKNOWN_OP`, not silence, so a sender can probe.

### 6.4 Telemetry piggybacked on the frame stream

Setting `STATS_REQ` on a frame is the normal way to get telemetry. The device
replies with a `CONTROL`/`TELEMETRY` packet **from the frame port to the
datagram's source port** - this is the one case where a `CONTROL` packet appears
on the frame port, and a sender MUST accept it there. This keeps the steady
state at exactly one packet per frame from the sender.

A sender SHOULD set `STATS_REQ` on about one frame per second (e.g. whenever
`seq % 30 == 0`) and MUST NOT set it on more than one frame in 100 ms.

### 6.5 Error codes

| Code | Name | Meaning |
|---|---|---|
| `0x01` | `ERR_BAD_LENGTH` | `len` inconsistent with the datagram or with the opcode's fixed body size |
| `0x02` | `ERR_UNKNOWN_OP` | opcode not implemented |
| `0x03` | `ERR_VERSION` | protocol version not supported |
| `0x04` | `ERR_BUSY` | another sender holds the lock |
| `0x05` | `ERR_BAD_ARG` | body parsed but the value is out of range |
| `0x06` | `ERR_STORAGE` | persisting the setting failed |
| `0x07` | `ERR_WIFI` | Wi-Fi operation failed (see §8) |
| `0x08` | `ERR_NOT_PERMITTED` | op disabled in this build or requires auth |
| `0x09` | `ERR_RATE_LIMITED` | too many requests |

### 6.6 `GET_INFO` reply body

The body is **exactly the DNS-SD TXT record wire format**: a sequence of
length-prefixed `key=value` strings, each `u8 length` followed by that many
bytes, with the same keys and values as §5.2, in the same order. One table in
the firmware, one parser in the sender, and a sender that was handed a bare IP
gets identical metadata to one that browsed mDNS.

Example (`\x09` = 9 bytes of `txtvers=1`):

```
09 74 78 74 76 65 72 73 3D 31   "txtvers=1"
07 70 72 6F 74 6F 3D 31         "proto=1"
04 77 3D 36 34                  "w=64"
04 68 3D 33 32                  "h=32"
...
```

### 6.7 `TELEMETRY` reply body - 48 bytes

All fields little-endian. Counters are free-running since boot or since the last
`RESET_STATS`, and wrap; senders compute deltas between successive reads and
MUST handle wrap with `wrapping_sub`.

| Off | Size | Field | Meaning |
|---|---|---|---|
| 0 | `u32` | `uptime_ms` | milliseconds since boot, wraps at 49.7 days |
| 4 | `u32` | `frames_rx` | `FRAME` datagrams accepted from the active source |
| 8 | `u32` | `frames_shown` | frames actually pushed to the panel |
| 12 | `u32` | `frames_dropped_stale` | `seq` not newer than `last_seq` (duplicate/reordered) |
| 16 | `u32` | `frames_dropped_superseded` | a newer frame arrived before this one was displayed |
| 20 | `u32` | `frames_dropped_decode` | decode failed, unknown codec, or awaiting a `KEY` frame |
| 24 | `u32` | `frames_rejected` | bad magic/version/length, or not the active source |
| 28 | `u32` | `seq_gaps` | total count of sequence numbers never seen |
| 32 | `u16` | `interarrival_us` | EWMA of per-frame inter-arrival time, saturating at 65535 |
| 34 | `u16` | `jitter_us` | EWMA of `|d_i - interarrival_us|`, §6.8 |
| 36 | `u16` | `interarrival_max_us` | max since reset, saturating |
| 38 | `u16` | `decode_us` | EWMA of decode time |
| 40 | `u16` | `decode_us_max` | max decode time since reset |
| 42 | `u16` | `render_us_max` | max time to push a decoded frame to the panel buffer |
| 44 | `i8` | `rssi_dbm` | last beacon RSSI, dBm |
| 45 | `u8` | `brightness` | brightness currently applied, 0-255 |
| 46 | `u8` | `state` | 0 `IDLE`, 1 `LIVE`, 2 `HOLD`, 3 `IDENTIFY`, 4 `PROVISIONING` |
| 47 | `u8` | `last_codec` | codec id of the last frame shown |

The struct is versioned by `len`: a future device MAY append fields and set a
larger `len`; a v1 sender MUST read only the first 48 bytes and ignore the rest,
and MUST reject a body shorter than 48.

### 6.8 Jitter and inter-arrival

Measured on the device's own monotonic clock, so no clock synchronisation is
needed. Let `R_i` be the arrival time of frame `i` in microseconds and
`n_i = seq_i - seq_{i-1}` (so a lost frame is not counted as jitter):

```
d_i = (R_i - R_{i-1}) / n_i
interarrival_us += (d_i - interarrival_us) / 16
jitter_us       += (|d_i - interarrival_us| - jitter_us) / 16
```

Integer arithmetic throughout; both EWMAs use a shift of 4. Reset both, and
`last_seq`, whenever the active source changes.

This is the RFC 3550 §6.4.1 smoothing applied to inter-arrival differences
rather than to sender/receiver timestamp deltas, because we have no synchronised
clocks. One-way latency is not reported here: use `PING` (round trip / 2) or set
`HAS_TS` (§3).

### 6.9 How a sender uses telemetry

Poll once per second via `STATS_REQ`. Let `sent` be frames sent in the interval
and the deltas be over the same interval.

| Condition | Diagnosis | Sender's action |
|---|---|---|
| `d(frames_rx) < 0.95 * sent` for 3 consecutive seconds | network-limited (loss on the air or in the AP queue) | step the frame rate down 30 -> 24 -> 20 -> 15 fps; step back up one rung after 10 consecutive clean seconds |
| `d(frames_rx) ~= sent` but `d(frames_dropped_superseded) > 0` | decode-limited: the device gets everything and cannot draw it in time | do **not** lower the frame rate; switch to a cheaper codec or a smaller payload |
| `d(frames_dropped_decode) > 0` | bug, or a codec the device does not really support | hard error; report and stop using that codec |
| `d(frames_rejected) > 0` while sending | another sender holds the lock, or a version mismatch | expect a `BUSY`; surface it |
| `jitter_us` high but no drops | the link is bursty, display is unaffected | report only |

The split of the drop counters by cause exists precisely so a sender can tell
"the network is losing packets" from "the device cannot keep up", which need
opposite responses.

---

## 7. Source arbitration and idle state machine

### 7.1 Source identity

A source is the UDP 4-tuple of an incoming `FRAME`: (source IP, source port).
This costs zero header bytes. A sender that rebinds its socket becomes a new
source.

### 7.2 Constants

| Name | Value | Meaning |
|---|---|---|
| `LOCK_MS` | 500 | after the last accepted frame, the active source keeps exclusivity this long |
| `STREAM_TIMEOUT_MS` | 1000 | no frames for this long -> the stream is considered stopped |
| `HOLD_MS` | 10000 | how long the last frame stays lit after the stream stops |
| `FADE_MS` | 500 | cross-fade duration into the idle screen |
| `BUSY_MIN_INTERVAL_MS` | 1000 | minimum gap between `BUSY` packets to one source |
| `TELEMETRY_MIN_INTERVAL_MS` | 100 | minimum gap between `TELEMETRY` packets to one source |

`LOCK_MS` = 15 frames at 30 fps: long enough that a sender that hiccups does not
lose the panel, short enough that a human switching senders does not wait.

### 7.3 States

| From | Event | To | Side effects |
|---|---|---|---|
| `IDLE` | frame accepted (§7.4) | `LIVE` | adopt source, reset `last_seq`/jitter, show frame |
| `HOLD` | frame accepted | `LIVE` | adopt source, reset `last_seq`/jitter, show frame |
| `LIVE` | frame accepted | `LIVE` | show frame, re-arm timers |
| `LIVE` | `STREAM_TIMEOUT_MS` with no accepted frame | `HOLD` | release the lock; last frame stays lit |
| `LIVE` | frame with `FINAL`, or `RELEASE` | `HOLD` | release the lock immediately |
| `HOLD` | `HOLD_MS` elapsed | `IDLE` | cross-fade over `FADE_MS` to the idle screen |
| any | Wi-Fi link down | `HOLD` | release the lock; idle screen after `HOLD_MS` says the network is down |

In words: any accepted frame moves the device to `LIVE`, from `IDLE` or from
`HOLD`, and re-arms the timers. `STREAM_TIMEOUT_MS` with no accepted frame moves
`LIVE` -> `HOLD` and releases the lock. `HOLD_MS` in `HOLD` fades to the idle
screen and moves `HOLD` -> `IDLE`.

- `IDLE` - showing the status screen. No active source.
- `LIVE` - streaming. `active_source` is set; `last_frame_at` is recent.
- `HOLD` - the last received frame is still lit, but the lock has been released
  so any sender may take over instantly.
- `IDENTIFY` and `PROVISIONING` are display overlays, not stream states: frame
  handling continues underneath and the state byte in telemetry reports the
  overlay.

### 7.4 Frame admission rule

On a valid `FRAME` from source `S` at time `now`:

```
if active_source is None:
    adopt S; reset last_seq/jitter; state = LIVE; accept
else if active_source == S:
    accept (see §3.2 for the seq test)
else if now - last_frame_at >= LOCK_MS:
    adopt S; reset last_seq/jitter; state = LIVE; accept        # takeover
else:
    frames_rejected += 1
    send BUSY to S at most once per BUSY_MIN_INTERVAL_MS
    discard
```

On accepting a frame: `last_frame_at = now`, `frames_rx += 1`.

The lock is released immediately - `active_source = None` - when any of these
happen:

- a frame with `FINAL` set is displayed;
- the active source sends `RELEASE` (from any port on the same IP);
- `STREAM_TIMEOUT_MS` passes with no accepted frame (state becomes `HOLD`).

### 7.5 Idle behaviour

After `STREAM_TIMEOUT_MS` with no frames, state becomes `HOLD` and the last
frame stays lit unchanged. After `HOLD_MS` in `HOLD`, cross-fade over `FADE_MS`
to the idle screen and enter `IDLE`.

Idle modes, settable with `SET_IDLE` and persisted:

| Mode | Name | Behaviour |
|---|---|---|
| 0 | `STATUS` (default) | device name, IPv4 address, RSSI bars, a slow ambient animation |
| 1 | `HOLD_FOREVER` | stay in `HOLD`; never fade |
| 2 | `DIM` | fade the last frame to 10% brightness and hold it |
| 3 | `BLACK` | fade to black |

The default is `STATUS` and not `BLACK` on purpose: a black panel is
indistinguishable from a broken one, and the status screen answers "what is this
thing called and where is it" without a serial cable.

On Wi-Fi disconnection the device goes to `HOLD` immediately and, after
`HOLD_MS`, to an idle screen that says the network is down.

---

## 8. Runtime Wi-Fi provisioning

v1 ships two paths. Both write to the same store: `esp-storage` +
`sequential-storage` on a dedicated flash partition.

### 8.1 Serial console (primary)

A line-oriented reader on the existing USB-UART at **115200** baud (the bench
rule is <= 230400; 115200 is what espflash monitors at):

```
wifi set <ssid> <psk>     store credentials and rejoin
wifi get                  print the stored SSID (never the PSK) and join state
wifi clear                erase stored credentials
info                      the GET_INFO key/value pairs, one per line
stats                     telemetry, one field per line
reboot
```

Implement the parser so it can later be wrapped in Improv Serial framing
(`IMPROV` + version `0x01` + type + length + data + checksum, RPC command `0x01`
= send Wi-Fi settings) without restructuring. That is the v1.1 upgrade and buys
a browser-based installer.

### 8.2 `SET_WIFI` control packet (secondary)

```
 body:
   u8    ssid_len   1..=32
   u8[]  ssid       UTF-8
   u8    psk_len    0..=64   (0 = open network)
   u8[]  psk        UTF-8
   u8    flags      bit0 = persist (else try only until reboot)
```

The device MUST send the reply **before** disconnecting, because after
disconnecting it cannot. Then:

1. Store the credentials if `persist`.
2. Disconnect and attempt to join the new network, up to 3 attempts.
3. On success, re-announce over mDNS from the new address.
4. On failure, fall back per §8.3 and set the join state so `GET_WIFI` reports
   `ERR_WIFI`.

### 8.3 Fallback rule

1. Try stored credentials, 3 attempts.
2. If that fails, try the compile-time credentials.
3. If that also fails, display the failure on the panel: the SSID tried, the
   error, and "hold the button / connect serial". The device is a display; it
   should say why it is not working rather than requiring a serial monitor.

### 8.4 Security posture for v1

The project owner has stated the Wi-Fi password is not a secret, so `SET_WIFI`
and `REBOOT` are unauthenticated on the LAN in v1. This is a deliberate
simplification, not an oversight.

What an untrusted-LAN deployment would need, recorded now so it is not
rediscovered later (card 041):

- A device PIN, displayed by `IDENTIFY` on the panel, carried in every mutating
  control packet along with a monotonic counter to defeat replay. This is the
  minimum viable version and is a day of work.
- The proper version is a PIN-authenticated key exchange (SPAKE2) producing a
  ChaCha20-Poly1305 session key; `chacha20poly1305` builds `no_std`.
- Frame traffic stays unauthenticated even then. The worst case is a stranger
  drawing on the panel, and the `LOCK_MS` rule already bounds that to an
  annoyance rather than a takeover.
- Invariant that holds in **all** versions including v1: the PSK is never
  returned by `GET_WIFI`, never appears in telemetry, and is never shown on the
  panel or printed by the serial console.

---

## 9. Sender implementation notes (macOS, and Linux)

### 9.1 Pacing

- Use `std::time::Instant` for everything. It is monotonic on macOS
  (`clock_gettime(CLOCK_UPTIME_RAW)`). Never use `SystemTime` for pacing.
- Schedule absolutely, not incrementally, so error does not accumulate:

```rust
let period = Duration::from_nanos(33_333_333); // 30 fps
let start = Instant::now();
let mut n: u64 = 0;
loop {
    let target = start + period * (n as u32);
    sleep_until(target);           // see below
    if let Some(frame) = render(n) { send(&frame)?; }
    n += 1;
    // If we have fallen more than two periods behind, resynchronise rather
    // than bursting. Skipping is correct here: the device shows newest-wins.
    let behind = Instant::now().saturating_duration_since(start);
    let should_be = behind.as_nanos() / period.as_nanos();
    if should_be as u64 > n + 1 { n = should_be as u64; }
}
```

- `sleep_until`: sleep for `target - now - 1ms`, then spin on `Instant::now()`.
  `thread::sleep` accuracy varies by platform and load; spinning the last
  millisecond costs roughly 3% of one core at 30 fps. The `spin_sleep` crate
  does exactly this if a dependency is preferred.
- **Never burst to catch up.** The device's rx path queues ~5 frames below the
  socket and the AP aggregates; a catch-up burst just fills those queues and
  raises latency for every subsequent frame. Skip frames instead.

### 9.2 Socket setup

```rust
let sock = UdpSocket::bind("0.0.0.0:0")?;
sock.connect((device_ip, frame_port))?;   // caches the route; enables send()
```

- Set `IP_DONTFRAG` (macOS/BSD) so an over-budget datagram errors locally.
  On Linux, `IP_MTU_DISCOVER = IP_PMTUDISC_DO`.
- Leave `SO_SNDBUF` at the default. A paced one-packet-per-tick sender does not
  need a big send buffer, and a big one only hides pacing bugs.
- Optional QoS, worth measuring but not worth assuming: on macOS set
  `SO_NET_SERVICE_TYPE` to `NET_SERVICE_TYPE_VI` (3, "interactive video"), which
  the OS maps to a DSCP value and/or a WMM access category. Portable fallback is
  `IP_TOS` = AF41 (`0x88`). Whether the bench AP honours DSCP on the
  **downlink** to the ESP32 is the part that matters and is untested - card 013.
- Bind the control socket separately; do not reuse the frame socket, so a
  `TELEMETRY` reply arriving on the frame socket (§6.4) is unambiguous.

### 9.3 macOS local network privacy

- A CLI run from Terminal or over SSH, and its children, are **exempt** from the
  Local Network prompt on current macOS. `cargo run` just works.
- Closing Terminal while a child process is still running can revoke that
  exemption mid-run and break local connections. Do not background the sender
  by closing the window.
- A GUI app or launchd agent is not exempt: it needs
  `NSLocalNetworkUsageDescription` and will prompt. If we ever ship a menu-bar
  sender, budget for it.
- There is a known bug class where the kernel caches a denial after reboot and
  multicast silently does not arrive. Symptom: mDNS browse returns nothing
  forever. Workarounds: toggle the permission in Privacy & Security > Local
  Network, or close and reopen the socket. Therefore a browse that finds nothing
  MUST be retriable and `--addr` MUST always work.
- `dns-sd -B _screeny._udp` uses Apple's own responder and is the fastest way to
  tell "the device is not advertising" from "my process cannot see multicast".

### 9.4 Discovery-to-stream sequence

1. Browse `_screeny._udp.local.` for up to 3 s, or use `--addr`.
2. `GET_INFO` on the control port to confirm liveness and get the authoritative
   codec list (TXT may be cached and stale).
3. Choose the best codec present in both the device's `codecs` list and the
   sender's, preferring the device's order.
4. Stream, with `STATS_REQ` once a second, applying §6.9.
5. On exit, send one frame with `FINAL` set, or `RELEASE`.

---

## 10. Reference constants

```rust
pub const MAGIC: u8 = 0x53;
pub const VERSION: u8 = 1;

pub const TYPE_FRAME: u8 = 0x0;
pub const TYPE_CONTROL: u8 = 0x2;

pub const HEADER_LEN: usize = 8;
pub const MAX_UDP_PAYLOAD: usize = 1472;
pub const MAX_PIXEL_PAYLOAD: usize = MAX_UDP_PAYLOAD - HEADER_LEN; // 1464

pub const DEFAULT_FRAME_PORT: u16 = 49374;
pub const DEFAULT_CONTROL_PORT: u16 = 49375;

pub const SERVICE_TYPE: &str = "_screeny._udp.local.";

// frame flags
pub const F_KEY: u8 = 0x01;
pub const F_STATS_REQ: u8 = 0x02;
pub const F_FINAL: u8 = 0x04;
pub const F_HAS_TS: u8 = 0x08;

// control flags
pub const C_REPLY: u8 = 0x01;
pub const C_ERROR: u8 = 0x02;

pub const LOCK_MS: u32 = 500;
pub const STREAM_TIMEOUT_MS: u32 = 1_000;
pub const HOLD_MS: u32 = 10_000;
pub const FADE_MS: u32 = 500;
```

Worked example - a 30 fps sender's 31st frame, codec 3, keyframe, asking for
stats, 1200 bytes of pixels, `seq = 0x0100`:

```
53 10 03 03 00 01 B0 04  <1200 bytes>
^  ^  ^  ^  ^---^ ^---^
|  |  |  |  seq   len=0x04B0=1200
|  |  |  flags = KEY|STATS_REQ
|  |  codec 3
|  version 1, type FRAME
magic
```

Total datagram: 8 + 1200 = 1208 bytes UDP payload.

---

## 11. Decisions and open questions

Closed by card 004 (2026-09-19):

1. **Codec table** - done, section 4.
2. **Payload budget** - 1464 stays the default. The device states `mtu=` in TXT and
   the sender takes `--mtu`; the codec ladder already degrades to any budget >= 1376.
3. **Playout buffer** - none, but the firmware keeps a compile-time toggle for a
   1-frame buffer so card 013 can measure the difference on hardware.
4. **DDP** - the device never speaks DDP. A host-side proxy is parked as card 040.
5. **Ports 49374 / 49375** - confirmed.
6. **mDNS responder** - `edge-mdns` 0.8 compiles with the stack and works on
   hardware: macOS browses and resolves it (docs/research/004-first-bringup.md).
7. **Multiple devices from one sender** - sender-side only; no wire change.
8. **Control auth** - out of v1. Parked as card 041.
9. **`FRAME_FRAG`** - stays reserved and undefined. One frame, one datagram.
10. **Telemetry growth** - append fields and rely on `len`; no version bump.

Still open (do not block implementation):

- **`HAS_TS`** - whether `PING`/2 is enough latency resolution is card 013's call.
- **DSCP / `SO_NET_SERVICE_TYPE`** - unmeasured on this AP; section 9.2 stays advice.
- **LZ and BC1_DUAL bit-level prose** - sections 4.4 and 4.5 defer to the reference
  decoders; card 005 writes the prose and test vectors when it lifts them.
