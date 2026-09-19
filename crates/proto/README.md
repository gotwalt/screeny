# screeny-proto (card 005)

The screeny wire protocol v1, in one `no_std` crate with **no dependencies**,
shared unchanged by the firmware (Xtensa, no alloc, no float) and by every host
tool. [`docs/design/protocol-v1.md`](../../docs/design/protocol-v1.md) is
normative; this crate is its executable form, and where they disagree the spec
wins and this crate is a bug.

```
cargo test -p screeny-proto
cargo build -p screeny-proto --target thumbv7em-none-eabi        # bare metal
cargo +esp build -Zbuild-std=core -p screeny-proto \
      --target xtensa-esp32-none-elf                             # the real one
```

## The API in one screen

```rust
// --- constants (spec 1, 2, 7.2, 10) ------------------------------------
MAGIC, VERSION, HEADER_LEN, MAX_UDP_PAYLOAD, MAX_PIXEL_PAYLOAD
DEFAULT_FRAME_PORT, DEFAULT_CONTROL_PORT, SERVICE_TYPE
W, H, NPIX, NBYTES
F_KEY, F_STATS_REQ, F_FINAL, F_HAS_TS      C_REPLY, C_ERROR
LOCK_MS, STREAM_TIMEOUT_MS, HOLD_MS, FADE_MS,
BUSY_MIN_INTERVAL_MS, TELEMETRY_MIN_INTERVAL_MS

// --- frames (architecture.md) ------------------------------------------
type Rgb888Frame = [u8; 6144];             // 64x32 sRGB, row-major
struct IndexedFrame<'a> { palette: &'a [[u8; 3]], indices: &'a [u8; NPIX] }
    fn expand(&self, dst: &mut Rgb888Frame) -> Result<(), DecodeError>

// --- packets (spec 2, 3, 6.1) ------------------------------------------
enum Packet<'a> { Frame(FramePacket<'a>), Control(ControlPacket<'a>) }
    fn parse(datagram: &[u8]) -> Result<Packet<'_>, Reject>

struct FramePacket<'a> { codec: u8, flags: u8, seq: u16,
                         timestamp_us: Option<u32>, payload: &'a [u8] }
    fn parse(&[u8]) -> Result<Self, Reject>       // rejects CONTROL: spec 2.2
    fn write(&self, out: &mut [u8]) -> Result<usize, BuildError>
    fn encoded_len(&self) -> usize
    fn is_key/wants_stats/is_final/has_ts(&self) -> bool

struct ControlPacket<'a> { op: u8, flags: u8, req_id: u16, body: &'a [u8] }
    fn parse / write / encoded_len / is_reply / is_error

enum Reject { Short, BadMagic, BadVersion, BadType,
              BadLength, TooLong, ShortTimestamp }
enum BuildError { BufferTooSmall, TooLong }

fn newer(a: u16, b: u16) -> bool           // RFC 1982, spec 3.2
fn gap(new: u16, last: u16) -> u16         // what to add to seq_gaps

// --- control (spec 6, 8.2) ---------------------------------------------
control::op::{PING, GET_INFO, TELEMETRY, SET_BRIGHTNESS, IDENTIFY, SET_IDLE,
              RESET_STATS, RELEASE, SET_NAME, GET_WIFI, SET_WIFI, BUSY, REBOOT}
control::{state, wifi_state, busy_reason}  // the undefined-until-now bytes
enum ErrorCode { BadLength, UnknownOp, Version, Busy, BadArg,
                 Storage, Wifi, NotPermitted, RateLimited }
enum IdleMode { Status, HoldForever, Dim, Black }

enum Request<'a> { Ping, GetInfo, Telemetry, SetBrightness(u8),
                   Identify { duration_ms }, SetIdle(IdleMode), ResetStats,
                   Release, SetName(&'a str), GetWifi, SetWifi(SetWifi<'a>),
                   Reboot }
    fn decode(op: u8, body: &[u8]) -> Result<Request<'_>, ErrorCode>
    fn write(&self, req_id: u16, out: &mut [u8]) -> Result<usize, BuildError>
    fn op / body_len / encoded_len

enum Reply<'a> { Ping { uptime_ms }, Info(&'a [u8]), Telemetry(Telemetry),
                 Brightness { applied }, Identify, Idle { mode }, ResetStats,
                 Release, SetName, Wifi { ssid, state }, SetWifi,
                 Busy { reason, lock_holder_ms_remaining }, Reboot,
                 Err { code } }
    fn decode(op: u8, flags: u8, body: &[u8]) -> Result<Reply<'_>, ErrorCode>
    fn write(&self, op: u8, req_id: u16, out: &mut [u8]) -> ...

struct Telemetry { ..18 fields, spec 6.7 }
    fn decode(&[u8]) -> Result<Telemetry, ErrorCode>   // len-tolerant
    fn write(&self, out: &mut [u8]) -> Result<usize, BuildError>

// --- DNS-SD TXT (spec 5.2, 6.6) ----------------------------------------
txt::iter(&[u8]) -> impl Iterator<Item = Entry<'_>>
txt::find(&[u8], key: &str) -> Option<&[u8]>
struct DeviceInfo<'a> { txtvers, proto, w, h, codecs, mtu, ctrl, fw, id, name }
    const DEFAULT; fn parse(&[u8]) -> Result<Self, TxtError>
    fn write(&self, out: &mut [u8]) -> Result<usize, BuildError>
    fn codec_ids(&self) -> impl Iterator<Item = u8>
    fn supports(&self, id: u8) -> bool
    fn best_codec(&self, mine: &[u8]) -> Option<u8>    // spec 9.4 step 3

// --- decoders (spec 4) -------------------------------------------------
dec::codec::{PAL5, PAL8_LZ, PAL4_LZ, BC1_DUAL, SOLID}
dec::{SUPPORTED_CODECS, CODECS_TXT, PAL5_LEN, BC1_DUAL_LEN, SOLID_LEN}
fn decode(codec: u8, payload: &[u8], dst: &mut Rgb888Frame)
      -> Result<(), DecodeError>
fn dec::is_supported(codec: u8) -> bool
enum DecodeError { Short, Long, UnsupportedCodec, Corrupt }
// plus the individual decoders and dec::lz::inflate, if you want them
```

## Using it

Device, frame socket:

```rust
let pkt = FramePacket::parse(&datagram)?;          // spec 2 MUSTs, all of them
if proto::newer(pkt.seq, last_seq) {
    seq_gaps += proto::gap(pkt.seq, last_seq) as u32;
    last_seq = pkt.seq;
    proto::decode(pkt.codec, pkt.payload, &mut back_buffer)?;  // spec 4
    swap_at_refresh_boundary();                    // spec 3.3, 4.7
}
```

Device, control socket:

```rust
let pkt = ControlPacket::parse(&datagram)?;
let n = match Request::decode(pkt.op, pkt.body) {
    Ok(Request::Ping) => Reply::Ping { uptime_ms }.write(pkt.op, pkt.req_id, &mut out),
    Ok(req) => handle(req).write(pkt.op, pkt.req_id, &mut out),
    Err(code) => Reply::Err { code: code.as_u8() }.write(pkt.op, pkt.req_id, &mut out),
}?;
```

Sender, after discovery:

```rust
let info = DeviceInfo::parse(get_info_reply.body)?;
let codec = info.best_codec(&my_codecs).ok_or(NoCommonCodec)?;
FramePacket { codec, flags: F_KEY, seq, timestamp_us: None, payload }
    .write(&mut buf)?;
```

## Things worth knowing before you build on it

- **The codec id is not in the payload.** The lab (`lab/src/dec`) puts a mode
  byte at the front; the wire puts it in the header. A lab payload is exactly
  one byte longer than the wire payload for the same frame. `decode` takes the
  codec as an argument.
- **Payloads must be exactly the right length.** Short is `Short`, long is
  `Long`. Padding goes beyond the header's `len`, where a receiver already
  ignores it (spec 2.3).
- **`write` owns the `HAS_TS` bit.** It is set or cleared from
  `timestamp_us`, so you cannot build a frame whose flags lie about its layout.
  `parse` strips the timestamp, so `payload` is always pixels.
- **A failed decode may have written part of `dst`.** Decode into the back
  buffer and swap only on success (spec 4.7).
- **Stack:** decoders need no scratch except `decode_pal8_lz`, which keeps a
  768-byte palette copy on the stack because the LZ output overwrites the
  source. Budget for it in the frame task.
- **`Request::decode` hands you the error code to reply with.** Unknown op,
  wrong body length and out-of-range value map onto spec 6.5 directly.
- **`Reply::write` takes the opcode separately**, because an error reply echoes
  the request's opcode rather than having one of its own.

## Totality

Everything here parses network input on a device with no MMU, so no public
function may panic, index out of bounds, write outside `dst`, allocate, or fail
to terminate on any input at all. The crate is `#![forbid(unsafe_code)]`, and
`Rgb888Frame` is a fixed-size array, so a stray index inside a decoder is a
panic in a debug build rather than silent corruption.

The evidence is `tests/mutation.rs`, which runs in about two seconds under
`cargo test` with a seeded xorshift and plain loops - no fuzzing infrastructure
to install or forget:

| | attempts |
|---|---|
| decoder payloads (corrupt, truncated, extended, spliced, random), every mutant offered to all five codecs and to seven reserved ids | 260 000 |
| packet parser, on mutated real datagrams and pure noise | 120 000 |
| control request and reply bodies, random opcodes | 80 000 |
| LZ streams: random, and valid streams mutated into the match path | 60 000 |
| DNS-SD TXT records | 60 000 |

Anything that parses must also re-encode to the same bytes and re-parse to the
same value, which is what stops "it did not panic" from passing for a decoder
that quietly returns nonsense.

## Tests

| file | what it pins |
|---|---|
| `tests/golden.rs` | byte vectors written by hand from the spec: the section 10 worked example, `HAS_TS` layout, every reject, every control op, telemetry offsets, the section 6.6 TXT example, RFC 1982 sequence arithmetic |
| `tests/codec_layout.rs` | payloads built by hand from the section 4 prose - nibble order, bit planes, index packing, interpolation weights, block raster order |
| `tests/lab_vectors.rs` | 27 vectors from the frozen card-002 lab decode to identical pixels |
| `tests/mutation.rs` | totality, above |

`tests/vectors/` holds the checked-in vectors; see its README for how to
regenerate them.

## Bare metal

| target | toolchain | result |
|---|---|---|
| `thumbv7em-none-eabi` | `stable` | builds |
| `xtensa-esp32-none-elf` | `esp`, with `-Zbuild-std=core` | builds |

The Xtensa build needs `-Zbuild-std=core` because the `esp` toolchain ships no
prebuilt `core` for that target; `lab/xtensa-bench/.cargo/config.toml` does the
same thing through a config file. The firmware crate will carry that config, so
it never appears on a command line there.
