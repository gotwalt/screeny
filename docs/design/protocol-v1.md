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

A receiver's frame buffer is 1472 bytes, so a datagram longer than that cannot
be read whole. A receiver MUST discard such a datagram and count it in
`frames_rejected` rather than parse the truncated prefix it managed to read:
the bytes it has are not the bytes that were sent, and a `len` that happens to
fit the truncation would be a lie. (Card 006: a receiver that simply passes the
short read to the parser reports `ERR_BAD_LENGTH`-shaped nonsense instead.)

**Neither this rule nor §2.3's 1464-byte `len` ceiling can be observed over
WiFi.** A UDP payload over 1472 bytes is an IP packet over a 1500-byte MTU, so
it is fragmented; the device does not reassemble fragments, as the paragraph
above says, so the stack drops it before the receiver is ever offered it and
**nothing counts it**. `frames_rejected` stays where it was - which is exactly
what a violation of the rule looks like from outside. A sender that sets
`IP_DONTFRAG` as §9.2 advises cannot even transmit such a datagram. Both rules
are therefore verifiable only over loopback, or across a link whose MTU exceeds
1500: `screeny-probe conformance` marks them `LOOPBACK_ONLY` and prints
`SKIP  loopback only: the radio fragments it away` rather than a pass it cannot
justify, and `crates/sim` keeps the real assertions, where the rejection reason
is visible in process. This is a property of the rules, not a gap in either
implementation, and it is written here so that the next device-side test for
them is not written at all. (Card 132.)

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

`frames_rejected` counts the **frame port only**. A `CONTROL` discarded on the
frame port is counted in it, along with everything else this section rejects
there. A datagram discarded on the control port is counted nowhere: the control
channel answers rather than counts (§6.5), and folding control-port noise into
a counter a sender reads to diagnose its *video* stream (§6.9) would make that
counter useless.

### 2.3 `len` (offset 6..8)

Number of bytes of payload/body following the 8-byte header. A receiver MUST
check `8 + len <= datagram_length` and discard the packet otherwise. Bytes
beyond `8 + len` are padding and MUST be ignored; a sender MAY pad. For a
`FRAME`, `len` MUST be `<= 1464`. That last one is the rule §1 says cannot be
observed over WiFi: a `len` of 1465 that the datagram really backs up makes a
1473-byte datagram, which the radio fragments away. A `len` the datagram does
*not* back up is the `8 + len <= datagram_length` check above, and that one is
observable anywhere.

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

`STATS_REQ` says "after processing this frame", and *processing* begins at the
admission rule of §7.4. A frame from the source that holds the lock is answered
whether or not its pixels reach the panel: a duplicate, a reordered frame and
one whose payload does not decode all still get their `TELEMETRY`. This is
deliberate. The one frame a second a sender marks (§6.4) is exactly the frame
it cannot afford to lose track of, and withholding the reply precisely when
something has gone wrong would hide the failure the sender is asking about. A
frame that admission *rejects* - one from a source that does not hold the lock
- gets a `BUSY` instead (§6.2) and no telemetry: its sender is not entitled to
the counters of a stream that is not its own.

### 3.2 Sequence numbers

`seq` increments by one per frame *sent* and wraps mod 2^16 (36.4 minutes at
30 fps). It MAY start at any value.

A sender MUST NOT reset `seq` while continuing to send from the same socket,
including when it restarts a stream. A receiver keeps `last_seq` per source and
a source is a UDP 4-tuple (§7.1), so a sender that restarted at 0 from the same
port would have every frame rejected as stale until the sequence caught up, or
until `STREAM_TIMEOUT_MS` released the lock. A sender that wants a fresh
sequence MUST rebind its socket, which makes it a new source and resets
`last_seq` on the device for free. (Card 006 found the earlier wording -
"MUST NOT be reset except when the sender restarts a stream" - permitted
exactly the case that cannot work.)

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

Every frame that survives step 1 is an *accepted* frame in the sense of §7.4,
including the ones step 1 then throws away: each of them increments
`frames_rx`, re-arms `last_frame_at`, advances `last_seq` and feeds §6.8's
inter-arrival EWMAs, and only then may be counted in
`frames_dropped_superseded`. So for a single source and over any interval,

```
d(frames_rx) = d(frames_shown) + d(frames_dropped_superseded)
             + d(frames_dropped_decode)
```

which is the identity §6.9's table relies on: `frames_rx` has to mean "arrived
and was mine", not "was drawn", or a sender cannot tell a lossy network from a
device that cannot keep up.

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

A payload MUST be **exactly** the size its codec calls for: the fixed sizes in
the table above, and for the variable-rate codecs the palette plus an LZ stream
that ends where the index plane is complete (§4.4). A receiver MUST reject a
payload that is longer as well as one that is shorter. Padding, where a sender
wants it, goes beyond the header's `len` (§2.3), which is where a receiver
already ignores it.

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

A byte-aligned LZSS/LZ77. It was chosen over heatshrink because heatshrink's
bit-level coding needs a stateful bit reader and a ring buffer of its own,
whereas this format decompresses straight into the destination buffer: a
compare, a shift and a byte copy per item, no tables, no allocation, no scratch
RAM.

The output length is **not** carried in the stream. It is fixed by the codec -
2048 index bytes for `PAL8_LZ`, 1024 nibble bytes for `PAL4_LZ` - and the
decoder stops when it has produced exactly that many.

```
 stream  := group*
 group   := flags:u8  item{0,8}       ; item k is selected by bit (7-k) of flags
 item    := literal                   ; when the flag bit is 1
          | match                     ; when the flag bit is 0
 literal := b:u8                      ; b is appended to the output
 match   := b0:u8 b1:u8
            offset = ((b0 << 4) | (b1 >> 4)) + 1     ; 1..=4096, backwards
            length = (b1 & 0x0F) + 3                 ; 3..=18
```

A match copies `length` bytes from `offset` bytes before the current end of the
output, **one byte at a time**, so a match may overlap itself: `offset = 1,
length = 18` emits eighteen copies of the previous byte, and that is the
format's only run-length encoding. There is no separate window - the 4096-byte
offset range covers the whole 2048-byte index plane, so every byte already
produced is reachable.

A group's eight item slots are read in order, most significant flag bit first.
A decoder MUST stop at the first item boundary at which the output is complete,
so an encoder need not pad the final group and the unused low bits of a final
flag byte mean nothing. A sender MUST NOT emit trailing bytes after the item
that completes the output; a receiver MUST reject a payload that has them,
because padding belongs beyond the header's `len` (§2.3), not inside it.

A decoder MUST bounds-check every item before it moves a byte:

- a literal requires one more byte of input;
- a match requires two more bytes of input, `offset <= bytes produced so far`
  (otherwise it reaches before the start of the output), and
  `produced + length <= required output length` (otherwise it overruns);

and MUST reject a stream that produces more or fewer bytes than the codec
requires, or that ends before the output is complete. Every item appends at
least one byte, so a decoder that loops while the output is incomplete
terminates after at most `output length` iterations on any input whatsoever.

Reference implementation: `crates/proto/src/dec/lz.rs`, with vectors in
`crates/proto/tests/vectors/`.

### 4.5 `0x28 BC1_DUAL`

```
[flags : 16 B, one bit per block, MSB first, raster order of blocks][128 blocks x 10 B]

flag 0:  [e0 : RGB565 u16le][e1 : RGB565 u16le][idx : 3 bitplanes x 2 B]   8 levels
flag 1:  [e0 : RGB888][e1 : RGB888][idx : 16 x 2 bits = 4 B]               4 levels
```
Blocks are 4x4 pixels, 16 across and 8 down, in raster order of blocks: block
`n = by * 16 + bx` covers pixels `x = bx*4 .. bx*4+3`, `y = by*4 .. by*4+3`.
Within a block the 16 pixels are numbered `j = 0..15` in raster order,
`x = bx*4 + (j & 3)`, `y = by*4 + (j >> 2)`.

The flag plane is 16 bytes, one bit per block, MSB first: block `n`'s flag is
bit `7 - (n & 7)` of byte `n >> 3`. It selects what the block's 10 bytes spend
their bits on, because measurement said the two things a block can be short of
are *endpoint precision* (dark, smooth blocks, where RGB565's 5-bit steps are
coarse just where the panel is finest in relative terms) and *gradation*
(bright smooth ramps, where four levels band).

**Flag 0 - RGB565 endpoints, 8 levels.** Bytes 0..1 are `e0` and bytes 2..3 are
`e1`, each an RGB565 `u16le` (`rrrrrggg gggbbbbb` once assembled), expanded to 8
bits per channel by bit replication. Bytes 4..9 are three bitplanes of two bytes
each: plane `p` occupies bytes `4 + 2p .. 5 + 2p`, and pixel `j` takes bit
`7 - (j & 7)` of byte `(j >> 3)` within each plane. Plane 0 is the least
significant bit of the index, plane 2 the most, so
`idx = b0 | (b1 << 1) | (b2 << 2)`, giving `idx` in 0..=7.

**Flag 1 - RGB888 endpoints, 4 levels.** Bytes 0..2 are `e0` as R, G, B and
bytes 3..5 are `e1`. Bytes 6..9 hold sixteen 2-bit indices, four per byte, most
significant pair first: pixel `j` takes bits `7 - 2*(j & 3)` and `6 - 2*(j & 3)`
of byte `6 + (j >> 2)`, giving `idx` in 0..=3.

Level k is `lerp(e0, e1, W[k])`, `lerp(a,b,w) = (a*(256-w) + b*w + 128) >> 8`,
with `W4 = [0,85,171,256]` and `W8 = [0,37,73,110,146,183,219,256]`, applied per
channel. The weights are scaled to 256 so `W[k] + W[n-1-k] == 256`, which
reproduces BC1's 1/3 and 2/3 points to within one code value while replacing the
divide with a multiply and a shift. Interpolation is in sRGB (gamma) space: on a
linear-light LED panel the perceptually even ramp between two colours is the one
that is even in sRGB, so this is both the cheap option and the right one (card
002).

Every bit pattern is legal, so this codec has no corrupt case: a payload of
exactly 1296 bytes always decodes. Reference implementation:
`crates/proto/src/dec/block.rs`.

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
| `codecs` | `16,17,40,2,127` | decimal codec ids the device can decode, comma-separated, preference order (most preferred first) |
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
`screeny discover --broadcast`. Frame data is still never broadcast.

`GET_INFO` MUST be rate-limited to one reply per source address per second,
**except** that a request repeating a `req_id` the device has already answered
inside that window is §6.1's retransmission and MUST be answered again. Without
the exemption the rate limit and the retry rule contradict each other: a sender
whose first reply is lost retries with the same `req_id` and would be
stonewalled for a second, having no way to tell that from a device that is not
there.

The limit is written as applying to `GET_INFO` however it arrived, rather than
to broadcast `GET_INFO` only, because a receiver generally cannot tell which
address a datagram was sent to without `IP_RECVDSTADDR` or its equivalent, and
a rule two implementations read differently is worse than a rule that is very
slightly too strict. One new `GET_INFO` per source per second is ample for
§9.4's discovery sequence, and it bounds what a broadcast probe can amplify.
(Card 006.)

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
  with `req_id == 0` except where an opcode says otherwise. This covers error
  replies: a requester that said it did not want an answer does not get one
  even when its request was malformed. The request itself is still carried out
  if it is valid.
- A device MUST discard a `CONTROL` packet that arrives with `REPLY` set.
  Requests and replies are told apart by that bit and by nothing else, so a
  device that answered replies would answer its own, and two of them on one
  LAN would talk to each other indefinitely.
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

Both leave by the **frame** socket, addressed to the frame datagram's source
address and port. §6.4 says so for `TELEMETRY`; `BUSY` answers a frame too, and
the source it answers is a frame-port source, so it takes the same path. These
two are the whole of the exception in §2.2 to "a receiver MUST discard a
`CONTROL` received on the frame port": a sender MUST accept both there.

The 100 ms limit governs the **unsolicited** `TELEMETRY` of this section, which
is per source and nothing to do with the control port. A `TELEMETRY` *request*
(§6.3, `op = 0x03`) on the control port is solicited and is answered every
time.

`BUSY`'s `lock_holder_ms_remaining` is `LOCK_MS - (now - last_frame_at)` in
milliseconds, clamped at 0: how much longer the current holder keeps the panel
if it stops sending this instant. A sender that waits that long and retries is
guaranteed the takeover branch of §7.4.

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
- `RELEASE` clears the source lock only if the requester's **IP address**
  matches the active source's, as §7.4 says: it arrives on the control port, so
  its source *port* is never the frame stream's and a 4-tuple comparison would
  make the opcode impossible to use. A `RELEASE` from anyone else is answered
  with an ordinary acknowledgement and changes nothing - it is idempotent, and
  there is nothing for the requester to retry - not `ERR_BUSY`.
- `SET_BRIGHTNESS`, `SET_IDLE` and `SET_NAME` persist across reboot.
  `SET_BRIGHTNESS` is always clamped by the compile-time firmware cap (the panel
  runs off laptop USB); `applied` in the reply is the value actually in effect,
  which is how a sender learns the cap.
- An unknown opcode gets `ERR_UNKNOWN_OP`, not silence, so a sender can probe.
- `GET_WIFI`'s `state` byte is the join state: 0 `DISCONNECTED` (not associated
  and not trying), 1 `CONNECTING`, 2 `CONNECTED`, 3 `FAILED` (the last join
  attempt failed - this is what §8.2 means by "so `GET_WIFI` reports
  `ERR_WIFI`"). Other values are reserved.
- `BUSY`'s `reason` byte is 0 `LOCKED` (another source holds the lock, §7.4) in
  v1. Other values are reserved, so a sender MUST treat an unknown reason the
  same as `LOCKED`.

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

`ERR_BAD_LENGTH` covers both halves of its definition, and the second half is
worth spelling out: a `CONTROL` whose `len` the datagram does not back up is
answered with `ERR_BAD_LENGTH` rather than discarded in silence. The reply is
built from bytes 2 and 4-5, which are present in any datagram long enough to
have a header at all, so it echoes the right opcode and `req_id` even though
nothing after the header can be trusted. The same two bytes are what §2.2's
optional `ERR_VERSION` reply is built from. A datagram with fewer than eight
bytes, or with the wrong magic, has no header to read and is discarded
(§2.1), as is one whose `req_id` is 0 (§6.1). (Card 006.)

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

Two details the formulae leave open, both settled by card 006:

- **Seeding.** There is no `d_i` for the first frame of a stream, because there
  is no previous arrival. On the second frame a receiver MUST set
  `interarrival_us = d_1` and `jitter_us = 0` outright rather than running the
  EWMA up from zero, which is what RFC 3550 does and which stops the first
  second of every stream reporting an interval that is far too short and a
  jitter that is far too large.
- **`interarrival_max_us` is "max since reset"**, and the reset it means is
  `RESET_STATS`, not a change of source. It sits among the counters in §6.7 and
  behaves like one. `interarrival_us` and `jitter_us` are the two things a
  source change clears.

`RESET_STATS` zeroes the counters of §6.7 and these three, and nothing else: it
is a measurement control, not a stream control. It does not release the lock,
change the state, clear `last_seq`, or blank the panel, so `state` and
`last_codec` in the next telemetry reply still describe the frame that is lit.

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

- a frame with `FINAL` set is **displayed** - a `FINAL` frame that failed to
  decode was never displayed and releases nothing, so one damaged packet cannot
  hand the panel to a stranger; the stream timeout below will release it a
  second later if the sender really has finished;
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

The mode changes what the panel shows and, for mode 1 only, the state machine.
`HOLD_FOREVER` means the `HOLD -> IDLE` transition never fires, so the `state`
byte of §6.7 stays `HOLD` indefinitely and the lock is still released on the
stream timeout as usual. `DIM` and `BLACK` take the transition like `STATUS`
does - the state byte becomes `IDLE` after `HOLD_MS` and the cross-fade runs
over `FADE_MS` - and differ from it only in what is drawn at the far end. The
`IDLE` state is "no active source", not "showing the status screen". (Card 006.)

On Wi-Fi disconnection the device goes to `HOLD` immediately and, after
`HOLD_MS`, to an idle screen that says the network is down.

---

## 8. Runtime provisioning and the device's HTTP API

v1 ships three ways to put credentials on the device, and they all write one
store: `esp-storage` + `sequential-storage` on the dedicated `screeny`
partition, through `crates/settings`. The setup portal (§8.1) is the one a
person uses; `SET_WIFI` (§8.2) is the one on the control port; the settings
page is the HTTP API (§8.5-§8.9), which serves the rest of the device's web
surface as well. §8.3 is the order the device tries what it has and §8.4 is
the security posture.

### 8.1 The setup portal (primary)

A device that has no network (§8.3) raises a **soft-AP** and serves a setup
page on it. The radio is in APSTA mode, so the station half keeps doing
whatever it was doing; the ESP32 has one PHY, so the AP follows the station's
channel and a client on the AP MAY lose it for the length of a trial join.

| | |
|---|---|
| AP name | `screeny-<xxxxxx>`, the §5.1 device id. **Never the friendly name**: the QR below carries 14 characters and `screeny-4a00a4` is exactly 14. |
| Authentication | **open** (device-web decision 2). The home PSK crosses it in clear; §8.4. |
| Address | 192.168.4.1/24, static. The portal answers on that address only. |
| DHCP | `192.168.4.50`-`.53`, four leases, 600 s, router and DNS both 192.168.4.1. **No RFC 8910 option 114** (see below). |
| DNS | a catch-all: every name answers 192.168.4.1, TTL 10 s. |
| HTTP | §8.5, port 80, with the captive catch-all of §8.9. |
| Panel | the portal screen: a version 2-L QR of `WIFI:T:nopass;S:<ap name>;;`, the name and `192.168.4.1`. It does not alternate with anything. |
| Telemetry `state` | `PROVISIONING` (§6.7). While the portal screen is up a decoded frame is still counted but does not reach the panel. |

The page is `GET /setup`, and `GET /` on the AP interface is the same page. It
takes an ordinary urlencoded `ssid=&psk=` form (§8.7) and answers **HTML**, not
JSON: what posts to it is a `<form>` in a captive mini-browser with no
JavaScript, and a browser handed `{"result":"trying"}` shows a person a page of
JSON. Posting starts a trial join (§8.3) and the page reports the result on a
full-page reload, which is the only navigation the iOS mini-browser re-probes
on. The page has **no file input**: they do not work in a captive mini-browser,
so a firmware upload is on the LAN page only.

Two things the portal deliberately does not do, both measured on the owner's
phone (iOS 18.7, card 223): it does not send DHCP option 114, because RFC
8910/8908 want an HTTPS API endpoint on a hostname answering
`application/captive+json` and this device can offer neither; and it does not
redirect a captive probe. The catch-all answers with the setup page itself
(§8.9). The DNS catch-all and the HTTP catch-all carry the whole weight.

**The serial console this section used to specify does not exist and will not
be built.** There is no line reader on the UART: `wifi set` / `wifi get` /
`wifi clear` / `info` / `stats` / `reboot` were never implemented, and neither
was the Improv Serial framing they were shaped for. The portal above, the
settings page (§8.5) and `SET_WIFI` (§8.2) replaced the `wifi` commands;
`GET_INFO` and `TELEMETRY` on the control port, and `GET /api/v1/status` and
`GET /api/v1/telemetry` over HTTP, replaced `info` and `stats`; `REBOOT` and
`POST /api/v1/reboot` replaced `reboot`. The serial port is a log, not an
interface (device-web decision 5).

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

1. Disconnect and attempt to join the new network. This is §8.3's **trial**,
   and §8.3 has the attempt count, the exception for an authentication failure
   and what a failure falls back to. `POST /api/v1/wifi` and `POST /setup`
   (§8.6) enter the same path, with `persist` implied set - they carry no such
   bit - so there is one trial machine and three front doors.
2. On success, **then** store the credentials if `persist`, and re-announce over
   mDNS from the new address. Credentials MUST NOT be stored before they have
   joined: firmware 0.4.0 stored first, and one wrong `SET_WIFI` replaced a working
   pair in flash - the device ran on until its next reboot and then could join
   nothing. A store failure at this point cannot be reported in the reply (it has
   already been sent); the device counts it and carries on with the join it has.
3. On failure, fall back per §8.3 to the credentials it had, leave the store
   untouched, and set the join state so `GET_WIFI` reports `ERR_WIFI`.

### 8.3 Joining a network

The order, the counts and the timeouts are one state machine -
`crates/provision`'s `Provisioner`, which the firmware and `crates/sim` both
drive - and the constants below are its `Timing::SPEC`, named as it names them.
The machine never sees a password: an event carries the SSID and the caller
holds the credential until the machine asks for it to be committed, which is
§8.4's invariant made structural.

**At boot**, in order:

1. the **stored** credentials, if the store holds a pair;
2. the build's **compile-time** credentials, if it has any. A default build has
   none and its `build.rs` does not look for any; the one build that does is
   the off-by-default `bench-wifi` build, for testing (device-web decision 6).
   A compile-time pair that joins **seeds an empty store**; it never replaces a
   stored pair that failed, which is the owner's to replace and not a test
   build's to overwrite;
3. otherwise - nothing stored and nothing compiled in, or both exhausted - the
   **portal** of §8.1. The portal is never terminal.

Each target gets `join_attempts` = 3 attempts and each attempt is bounded by
`join_attempt_ms` = 15 s, so a target is ~45 s. The deadline is the machine's
own, so a radio that never answers still advances. **A join is not a join until
there is an address**: an association with no DHCP answer is a failed attempt.

**Credentials posted** - to `POST /api/v1/wifi`, to `POST /setup` or by
`SET_WIFI` - start a **trial**, from whatever state the device is in:

- **Nothing is written to the store.** The pair is held in RAM and committed
  only after the join has succeeded (§8.2 records what it cost to learn that).
- A trial makes up to `trial_attempts` = 3 attempts, **except that an
  authentication failure is not retried at all**: a wrong password is
  deterministic, and three attempts is 45 s of somebody holding a phone for the
  same answer.
- A trial posted **to the portal** keeps the AP up throughout, so the page that
  posted can be told what happened; a failure returns to the portal, which
  shows the reason.
- A trial posted **while the device is online or joining** raises no AP: there
  is one station, so it drops the association it has, and there is nobody on a
  setup network to inform. A failure goes back to the **stored** credentials
  (then the compile-time pair, then the portal only if there is nothing at
  all) and **never clears the store**. The panel stays the stream's and `ip` is
  `None` for the length of the trial.
- A failed online-origin trial is **sticky**: `GET_WIFI` reads `FAILED` and
  `GET /api/v1/wifi` carries the reason until the next post, a credentials wipe
  or a reboot - the previous network reconnecting is not an answer to "did the
  pair I just gave you work".
- A second post while a trial is in flight cancels it and starts again with the
  new pair, down the channel the second post arrived on.

On success the credentials are committed, mDNS re-announces (§5.3), and:

- a soft-AP that is up stays up for `ap_grace_ms` = 30 s, whatever raised it,
  so a phone standing on the portal can reload the page and read the new
  address; and
- after a **portal** trial only, the acquired address goes on the panel for
  `connected_screen_ms` = 60 s. It yields to a stream: the phone's page carries
  the address too, and a panel that is being sent a picture shows the picture.

**While the portal is up**, if the store holds credentials and **no station is
associated to the AP**, the stored pair is retried every `portal_retry_ms` =
10 minutes, so the 3 a.m. router reboot heals itself. A retry is suppressed
while somebody is on the AP, because it would cost them a ~45 s outage, and
suppressing it does not reset the timer: it fires on the first tick after the
last client leaves. The AP stays up across the retry.

**While online**, a link that goes down and stays down for `link_down_ms` =
60 s returns the device to step 1. A link that comes back inside that window is
not an event. (What the *frame* path does on a link loss is §7.3's: `HOLD` at
once.)

A credentials wipe - the button's five-second hold - erases the store and opens
the portal from any state, and after one `GET_WIFI` reads `DISCONNECTED` rather
than `FAILED`, because a wipe is not a failure. The machine defines it; no
event in firmware 0.5.1 produces one yet (card 230).

`GET_WIFI`'s state byte (§6.3) is this machine's, with the sticky trial result
folded in:

| Machine state | `GET_WIFI` `state` |
|---|---|
| before the first decision | 0 `DISCONNECTED` |
| joining, or a trial in flight | 1 `CONNECTING` |
| online | 2 `CONNECTED` |
| portal, nothing having failed to get there | 0 `DISCONNECTED` |
| portal after a failure, or a sticky failed trial | 3 `FAILED` |

`GET /api/v1/status`'s `wifi_state` is **the link** and takes no sticky value
(§8.6); the sticky result belongs to `GET /api/v1/wifi` alone.

### 8.4 Security posture for v1

The project owner has stated the Wi-Fi password is not a secret, so `SET_WIFI`
and `REBOOT` are unauthenticated on the LAN in v1, and so is **every route of
the HTTP API**, settings and the reserved firmware upload included
(device-web decision 3). The setup AP of §8.1 is **open**, so the home PSK
crosses it in clear while setup is happening (decision 2). All of this is a
deliberate simplification, not an oversight.

The API is nevertheless *shaped* for a PIN: every mutating request may carry a
`pin` and a monotonic `counter`, which are parsed and ignored today, and
`crates/device-api`'s `request::check_auth` is the single hook every mutating
route already calls. The error code `unauthorized` (401) is reserved for it and
nothing returns it.

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
  returned by `GET_WIFI`, never appears in telemetry, is never carried by any
  HTTP reply, and is never shown on the panel or written to a log line. It is
  enforced rather than remembered: no reply type in `crates/device-api` has a
  field for one (`tests/no_psk.rs` serialises a worst-case value of every reply
  and greps the bytes, and `screeny-probe http` rule 38 does the same over the
  wire), the one request type that holds a PSK prints its length and not its
  bytes, and `crates/provision`'s machine has nowhere to put one at all.

### 8.5 The HTTP API: transport

The device serves one HTTP API and two HTML pages. The routes, the request and
reply bodies, the error shape and every bound are **one crate**,
`crates/device-api` (`screeny-device-api`), which the firmware, `crates/sim`
and the Studio all link; `crates/device-api/tests/golden/` holds a checked-in
example of every body, and `screeny-probe http` holds a device to the 38 rules
of `crates/probe/src/http/rules.rs`. Rule numbers are cited below where one
pins a sentence.

| | |
|---|---|
| Transport | **TCP 80**, fixed. No TLS. |
| Version | HTTP/1.1. Every response carries `Connection: close`; keep-alive is off. |
| Authentication | none (§8.4). |
| Advertised | `_http._tcp.local.` on port 80, sharing the instance name, host name and `A` record of §5.1's `_screeny._udp`. |
| Concurrency | a device SHOULD serve at least **two** connections at once. |
| Timeouts | 3 s to send a request line, 5 s to finish a request that has started, 5 s for the response to be accepted. |

Keep-alive is off on purpose: with a handful of workers, one browser polling a
page would hold one of them for as long as the tab is open, and closing after
each response bounds the worst wait to one response time. Two workers is a
floor rather than a detail because smoltcp has **no listen backlog** - a
connection arriving while every worker is busy is *refused*, not queued - and
iOS does not retry a refused connection where macOS retries after a second.

**Which interface.** Every worker follows the soft-AP: while the AP of §8.1 is
up the API is served on 192.168.4.1 and **not** on the station address, and the
rest of the time it is served on the station address. The station has no
network while the portal is up, so there is nobody on the LAN for the borrowed
worker to have served; the window where both exist is §8.3's 30 s grace.

The request line, the headers and the body of a buffered route MUST fit the
server's request buffer together (1,536 bytes in firmware 0.5.1, against a
desktop browser's ~700 bytes of headers and §8.8's 384-byte body bound). A
request that overruns it is **answered** `payload_too_large`, not dropped.

### 8.6 The HTTP API: routes

| Method | Path | Request | Reply | Request <= |
|---|---|---|---|---|
| GET | `/` | - | the status page, or the setup page on the AP (§8.9) | - |
| GET | `/setup` | - | the setup page (§8.9) | - |
| POST | `/setup` | urlencoded `ssid=&psk=` | the setup page, saying what is happening | 384 |
| GET | `/api/v1/status` | - | `StatusReply` | - |
| GET | `/api/v1/telemetry` | - | `TelemetryReply` | - |
| GET | `/api/v1/networks` | - | `NetworksReply` | - |
| GET | `/api/v1/wifi` | - | `WifiReply` | - |
| POST | `/api/v1/wifi` | urlencoded `ssid=&psk=` | `{"result":"trying"}` | 384 |
| POST | `/api/v1/settings` | `{name?, brightness?, idle_mode?}` | `SettingsReply` | 373 |
| POST | `/api/v1/firmware` | **reserved**, see below | - | - |
| POST | `/api/v1/reboot` | `{"confirm":"RBOO"}` | `{"result":"rebooting"}` | 188 |
| POST | `/api/v1/identify` | `{"duration_ms":N}` | `{"result":"identifying"}` | 152 |

`GET` and `POST` are the only methods this API defines; §8.8 says what any
other verb gets. `api` is the first field of `StatusReply` and is
`API_VERSION` = 1, the same 1 as the path prefix: a breaking change to any
shape here bumps it and moves the prefix, a new optional field does not.

- **`status`** is the `GET_INFO` and telemetry numbers a person or the Studio
  wants in one request, plus `boot_id` (a random `u32` drawn once at boot, so a
  reader can tell a reboot from a link flap without inferring it from uptime
  going backwards), `stack_free`, `store_errors`, `portal`, `fw_slot`,
  `fw_state` and `reset_reason`. Its `wifi_state` is **the link** -
  `connected` / `connecting` / `disconnected` - and never the sticky result of
  the last credentials attempt (§8.3; probe rule 8).
- **`telemetry`** is the 48 bytes of §6.7 as named fields, so a browser and a
  UDP sender see the same numbers (rule 10). It is the numbers and not an
  interpretation of them: `state` and `last_codec` are the raw bytes here,
  where `status.state` is the same byte as a word.
- **`networks`** is a scan: at most 16 entries, strongest first, one really
  performed scan per `SCAN_MIN_INTERVAL_MS` = 10 s (rules 11, 12). A scan takes
  the radio off its channel for the better part of a second per band, which on
  a device that is also receiving frames is a visible stall, so a caller that
  asks sooner gets `rate_limited` and SHOULD be told in `detail` how long to
  wait; being refused does not push the window out. An SSID that is not UTF-8
  is left out of the list rather than shown wrongly.
- **`GET wifi`** is what the setup page's reload reads: `{state, ssid, ip,
  reason}`, where a trial in flight or just finished wins over the station's
  own state for as long as §8.3 says it is current, and `reason` is
  `auth` / `not_found` / `other`. `reason` is non-null exactly when `state` is
  `failed` (rule 14).
- **`POST wifi`** and **`POST /setup`** take the same bytes and start the same
  trial (§8.3); one answers JSON and the other HTML. The reply goes out
  **before** the radio work starts, for §8.2's reason, and the credentials
  reach flash only after they have joined. Neither carries a `persist` bit.
- **`settings`** changes any subset of the three live settings; an absent field
  means "leave it alone" and an empty request is a no-op, not an error. The
  reply is the whole settings state after clamping, not an echo, so a caller
  that moved only the brightness still learns the name and a caller whose
  brightness was capped learns the cap. `name: ""` means "go back to
  `screeny-<id>`" - a rule of this route only; `SET_NAME` (§6.3) takes the
  string literally.
- **`reboot`** takes the same four bytes as §6.3's `REBOOT` magic, spelled
  `"RBOO"`, so that a crawler, a prefetcher or a captive probe cannot restart
  the panel. A body that parses with the wrong word is `out_of_range`, not
  `bad_request`: what is wrong is the value (rule 34). The reply goes out
  before the restart.
- **`identify`** mirrors `IDENTIFY`, whose wire field is a `u16` of
  milliseconds, so a `duration_ms` above 65535 is `out_of_range` (rule 22).
- **`firmware`** is **reserved for cards 240/241** and is not specified here.
  Until it lands, a device MUST answer it `unavailable` rather than accepting
  an upload it cannot vouch for (§8.8).

`settings` and `identify` are carried out by building the control request of
§6.3 and handing it to the same code the control port calls, with `req_id` 0 -
§6.1's "no reply wanted". The brightness cap, the name truncation, the idle
mode, the debounced store write and the mDNS re-announce are therefore one
implementation with two front doors.

### 8.7 The HTTP API: bodies and errors

Request bodies are JSON, **except** `POST /api/v1/wifi` and `POST /setup`,
which are `application/x-www-form-urlencoded`. That is not a style choice: an
802.11 SSID is a byte string and not text, and the form is what an iOS captive
mini-browser can post at all. The parser takes bytes, decodes `+` and `%XX`,
ignores unknown keys so a page can carry a hidden field without a firmware
change, treats a missing or empty `psk` as an open network, refuses an empty
`ssid` (§8.2 says `ssid_len` is `1..=32`), and **refuses a duplicate key**
rather than taking the last one: `psk=right&psk=wrong` must not be a coin toss
about what reaches flash.

An SSID is bytes on the way in and text on the way out. One that is not UTF-8
is reported as `null` rather than lossily converted, because a lossy conversion
changes its length and misleads whoever is comparing it with what they typed.

Everything that fails answers **one shape** on every route, including the 404
for an unknown path:

```json
{"error":"<code>"}
{"error":"<code>","detail":"<a short sentence, <= 48 characters>"}
```

`detail` is for the person and is absent - not `null` - when there is nothing
useful to add; a sentence that does not fit is dropped rather than truncated.
The code is for the program and the HTTP status is a property of the code, so
that a browser switching on the status and a caller switching on the code
cannot disagree (rules 37, 27):

| Status | Codes |
|---|---|
| 400 | `bad_request`, `bad_form`, `bad_json`, `out_of_range` |
| 401 | `unauthorized` (reserved, §8.4) |
| 403 | `forbidden` |
| 404 | `not_found` |
| 405 | `method_not_allowed` |
| 409 | `busy` |
| 413 | `payload_too_large` |
| 429 | `rate_limited` |
| 500 | `storage`, `wifi`, `internal` |
| 503 | `unavailable` |

That is the closed set. A refusal a sender would have met on the control port
has the same name here: §6.5's `ERR_BAD_LENGTH` and `ERR_VERSION` are
`bad_request`, `ERR_UNKNOWN_OP` is `not_found`, `ERR_BUSY` is `busy`,
`ERR_BAD_ARG` is `out_of_range`, `ERR_STORAGE` is `storage`, `ERR_WIFI` is
`wifi`, `ERR_NOT_PERMITTED` is `forbidden` and `ERR_RATE_LIMITED` is
`rate_limited`.

### 8.8 The HTTP API: limits, methods and paths

- **Every route declares its own request bound** (§8.6's last column) and it is
  enforced on that route, not only at the global maximum of 384 bytes: a
  153-byte body to `identify` is `payload_too_large`, although 153 is well
  inside 384 (rules 28, 29). The global bound is the WiFi form's, which is the
  longest body any route takes.
- **Reply bounds are documentation, not buffers.** A JSON reply is measured
  into a counting writer and then streamed, so no reply needs a buffer; the
  bounds exist for callers deserialising into fixed arrays and for the honest
  answer to "how big can this get" (rule 36). Worst case: `status` 1,039,
  `telemetry` 426, `networks` 3,710, `wifi` 345, `settings` 247,
  `{"result":...}` 24. They assume every byte of every name escaping to six
  characters, which is why a real status reply is about 385 bytes.
- **A route this build cannot serve answers `unavailable` (503)**, not 404 and
  not a qualified success: the route exists and the device cannot serve it in
  this state, and retrying later is the right behaviour. Firmware 0.5.1 answers
  it for `GET /api/v1/networks` and `POST /api/v1/firmware`.
- **A known path with a method it does not have is 405; an unknown path is
  404** (rules 25, 27), and a verb this API has no method for at all - `PUT`,
  `DELETE`, `HEAD` - is 405 **in the error shape above**, not a server's
  built-in plain text (rule 26). `HEAD /` is a 405 by that rule.
- **A path is matched decoded and exactly**: `/api/v1/%73tatus` is `status`,
  and `/api/v1/status/` is not a route.

### 8.9 The HTTP API: the pages and the captive catch-all

`GET /` on the station interface is the **status page**: one self-contained
HTML file with its status table already rendered by the server, so that it
works with JavaScript disabled, and with no external stylesheet, script, font
or image - a device on a network with no route out must not be waiting on a CDN
(rule 30). With JavaScript the page replaces the same table every few seconds
from `GET /api/v1/status`.

`GET /` on the **AP** interface is the setup page of §8.1 instead: a client
that was dragged there by the QR code or by the catch-all is there to type a
network name, not to read a status table. `/setup` itself answers on both
interfaces, so the form is reachable over the LAN too.

**The captive catch-all.** On the AP interface, and only while the AP is
actually up, a request for a path this server does not have is answered with
**the setup page itself, `200`, `Cache-Control: no-store`** - not a redirect to
it, and not a 404. `/`, `/setup` and every route of §8.6 are unaffected, so a
phone on the setup network can still read the API. On the station interface the
same request is an ordinary 404: the catch-all is a property of the **listener**
and not of the `Host:` header.

It is a `200` and not a `302` because of what the owner's phone did on
2026-09-20 (iOS 18.7, card 223). The captive sheet fetches
`hotspot-detect.html` on one connection and opens a second it never uses, which
holds a worker for its read timeout; a redirect made it open a *third* within
milliseconds, while the worker that had just answered was between `close` and
`accept`; smoltcp refused the SYN and iOS did not retry it, so the sheet said
it could not connect to the server. Any reply that is not Apple's `Success`
page, not a `204` and not Microsoft's text marks the network as captive, so the
form does that job from the connection the sheet already has. `no-store` is
there because a cached captive probe is a sheet that never opens again.

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
    // Resume at the NEXT whole slot, not the current one: `n = should_be` would put
    // the next deadline at about "now", so the frame after a stall would leave
    // microseconds behind the stalled one - the two-frame burst this section
    // forbids (measured in card 009: 55 us apart after a 300 ms stall). One extra
    // skipped frame keeps the stream strictly paced. Same logic, same comment, in
    // `crates/screeny/src/sender.rs`.
    if should_be as u64 > n + 1 { n = should_be as u64 + 1; }
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

Worked example - a 30 fps sender's 31st frame, codec `0x10` (`PAL8_LZ`),
keyframe, asking for stats, 1200 bytes of pixels, `seq = 0x0100`:

```
53 10 10 03 00 01 B0 04  <1200 bytes>
^  ^  ^  ^  ^---^ ^---^
|  |  |  |  seq   len=0x04B0=1200
|  |  |  flags = KEY|STATS_REQ
|  |  codec 0x10 = PAL8_LZ
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

Closed by card 005 (2026-09-19), while lifting the decoders into
`crates/proto`:

11. **LZ and BC1_DUAL bit-level prose** - written, sections 4.4 and 4.5. The
    reference implementation is now `crates/proto/src/dec/`, with vectors in
    `crates/proto/tests/vectors/` cross-checked against the lab.
12. **Exact payload length** - a payload must be exactly its codec's size;
    over-long is a reject, not padding (section 4).
13. **`GET_WIFI` `state` and `BUSY` `reason`** - both bytes were undefined;
    values assigned in section 6.3.

Closed by card 006 (2026-09-19), while writing `crates/sim` - a second,
independent implementation of the receive side. Each of these was a place where
two readings of the text were both defensible, which is exactly what a second
implementation is for. None of them changes a byte on the wire.

14. **What `frames_rx` counts** - every *accepted* frame, including the ones
    the same drain then supersedes, so that the identity in section 3.3 holds
    and section 6.9's diagnosis works.
15. **Restarting a sequence** - a sender must rebind rather than reset `seq`
    from the same socket; the old wording allowed a case that could not work
    (section 3.2).
16. **`STATS_REQ` on a frame that is not shown** - answered anyway, for any
    frame that passes admission; a locked-out sender gets `BUSY` instead
    (sections 3.1 and 6.2).
17. **Which socket `BUSY` leaves by** - the frame socket, like `TELEMETRY`
    (section 6.2).
18. **Whether the 100 ms telemetry limit covers a `TELEMETRY` request** - no,
    only the unsolicited reply to a `STATS_REQ` frame (section 6.2).
19. **`BUSY`'s `lock_holder_ms_remaining`** - defined (section 6.2).
20. **`GET_INFO` rate limiting versus retries** - the limit counts new
    requests; a repeat of an answered `req_id` is a retransmission and is
    answered (section 5.5).
21. **`req_id == 0` and error replies** - silence covers errors too
    (section 6.1).
22. **A `CONTROL` with `REPLY` set arriving at a device** - discarded
    (section 6.1).
23. **Who may `RELEASE`** - matched on the IP address, since the control port
    is never the frame stream's port (sections 6.3 and 7.4).
24. **`ERR_BAD_LENGTH` for a header the datagram does not back up** - answered,
    from the two header fields that are certainly present (section 6.5).
    `crates/proto` grew `packet::peek` for it.
25. **Seeding the section 6.8 EWMAs, and what a source change clears** -
    defined (section 6.8).
26. **What `RESET_STATS` does not touch** - the lock, the state, `last_seq` and
    the panel (section 6.8).
27. **A `FINAL` frame that fails to decode** - releases nothing; only a
    *displayed* `FINAL` does (section 7.4).
28. **The `state` byte under each idle mode** - `HOLD_FOREVER` stays `HOLD`;
    `DIM` and `BLACK` reach `IDLE` like `STATUS` (section 7.5).
29. **A datagram larger than 1472 bytes** - discarded and counted, not parsed
    from its truncated prefix (section 1).
30. **What `frames_rejected` counts** - the frame port only (section 2.2).

Closed by card 225 (2026-09-20), bringing section 8 in line with what firmware
0.5.1 does and giving the HTTP API a normative home. Nothing here changes a
byte on the wire; `txtvers` and `proto` are unaffected.

31. **The serial console of 8.1** - struck. It was never built and will not be:
    the portal, the settings page and `SET_WIFI` are the three paths
    (device-web decision 5, section 8.1).
32. **Compile-time credentials** - step 2 of the join order, present only in a
    `bench-wifi` build, and they seed an **empty** store rather than replacing
    a stored pair that failed (device-web decision 6, section 8.3).
33. **What a posted pair does** - one trial machine behind three front doors,
    committing nothing until it has joined, not retrying an authentication
    failure, and falling back to the stored network with a sticky `FAILED`
    when it was posted to a device that was already online (section 8.3).
34. **The HTTP API** - sections 8.5-8.9: transport, the route table, the
    bodies, the one error shape and its statuses, the limits, the two pages
    and the captive catch-all. `POST /api/v1/firmware` is **reserved** for
    cards 240/241 and is deliberately not specified.

Still open (do not block implementation):

- **`HAS_TS`** - whether `PING`/2 is enough latency resolution is card 013's call.
- **DSCP / `SO_NET_SERVICE_TYPE`** - unmeasured on this AP; section 9.2 stays advice.
