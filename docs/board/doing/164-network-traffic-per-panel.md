---
id: 164
title: How much network a panel costs - KB/s out and in, on the Panel screen
type: build
hardware: no
depends: [198, 180]
owner: worker (card 164)
branch: card/164-network-per-panel
---

## Goal

The owner, 2026-09-20: "i'd like a KB/s tracker per panel to understand how much network
traffic we're sending (and receiving, during the http calls i assume)."

For each panel, the Studio counts every byte it sends to it and every byte it gets back,
over every path, and the Panel screen shows the rates.

## Context

- The paths between the Studio and one panel, all of which already go through a few
  places:
  1. **Frames**, UDP, Studio -> panel: `screeny::Link` inside the player
     (`crates/studio/src/player.rs`). The link's status already reports the last frame's
     `bytes` and `frames_sent`; it does not keep a running byte total. About 30 datagrams
     a second of 0.4-1.4 KB: this is nearly all of the traffic.
  2. **Control and telemetry**, UDP, both ways: `screeny::ControlClient` via
     `crates/studio/src/api.rs::on_device` (brightness, identify, name, reboot, stats) and
     the fleet's telemetry poll in `crates/studio/src/fleet.rs`. Requests out, replies and
     `TELEMETRY` datagrams in. Also whatever the link itself receives on its socket
     (`BUSY`, acks if any - read `crates/screeny/src/link.rs` / `sender.rs`).
  3. **HTTP status**, TCP, both ways: `crates/studio/src/devhttp.rs::get_status`, every
     10 s (and card 199's `/api/v1/panic` later): request bytes out, reply bytes in.
  4. mDNS browsing and the broadcast probe (card 141) are not traffic *with a panel*;
     leave them out and say so.
- Count **payload bytes plus a stated per-packet overhead** so the number means something
  on the wire: UDP datagrams are payload + 28 bytes (IPv4 + UDP headers); for HTTP count
  the bytes written and read on the socket and add nothing (TCP/IP overhead and ACKs are
  not visible from user space - say so in the tooltip/README rather than guessing). Keep
  both raw counters (`bytes`, `packets`, per direction, per path) in `/api/v1/status`
  under the device, and let the page compute the rate.
- Rates: an exponentially weighted or a sliding ~5 s window, computed in **one** place
  (server side is better: every browser then shows the same number, and the state is
  bounded - a few u64 per device). KB = 1000 bytes; show `KB/s`, one decimal under 10.
- Where it shows: the Panel screen's Link block (card 198, `panel.html` / `panel.js`):
  one line, e.g. `Network  36.4 KB/s out · 0.3 KB/s in`, with the split by path in the
  `title` or a second quiet line (`frames 36.1 · control 0.1 · http 0.2`). Nothing on the
  Picture screen. No chart - a number; if a chart is wanted later that is another card.
- Counters must not cost the render or send path anything measurable: atomics or a field
  the sender already owns; no locks on the frame path; no per-frame logging.
- Totals since the Studio started are worth keeping beside the rates ("2.1 GB out since
  Tuesday" is the number that answers "what does this cost my network").
- `crates/screeny` is shared with the `screeny` CLI: adding a running byte total to the
  link's/sender's stats is additive and fine; do not change its behaviour or the wire.

## Deliverables

- Byte and packet counters per path and direction (`crates/screeny` link stats as needed;
  `devhttp.rs`; the control path; the telemetry poll), kept per device in
  `crates/studio/src/devices.rs`, reported in `/api/v1/status`.
- The rate, computed once; the line on the Panel screen; `tests/ui.rs` id checks.
- Tests against `screeny-sim` on loopback: streaming a known patch for a few seconds gives
  a frames-out rate within 10% of `frames_sent x mean bytes`; an HTTP poll moves the http
  counters both ways; a device that is away counts nothing.
- README: what is counted and what is not.

## Acceptance

On the deployed Studio's `/panel`: a KB/s out that agrees with `fps x frame size` from the
same page, a small KB/s in, and totals that only grow.

## Log

### Claimed

Branch `card/164-network-per-panel`, cut from `main` at `f12bee6`. Read `crates/studio/README.md`,
`docs/board/done/198-studio-two-screens.md`, `180-*`, `118-*`, `crates/screeny/src/embed.rs`
(`Link`/`LinkStats`), `sender.rs`, `control.rs`, and the studio's `devhttp.rs`, `fleet.rs`,
`devices.rs`, `health.rs`, `api.rs::on_device`, `player.rs`, `ui/panel.*`, `ui/common.js`,
`tests/ui.rs`.

The shape settled on before writing anything:

- **`crates/screeny` gains counters only** - a `Wire { bytes, packets }` pair per direction on
  each socket the sender already owns. No wire change, no behaviour change, no lock: the sender
  adds two `u64` where it already increments `frames_sent`.
- **The studio accumulates per device in `devices.rs`**, three paths (frames, control, http) x two
  directions, and computes the rate in **one** place - the supervisor's 1 Hz tick - as an EWMA
  with a 5 s time constant, so every browser reads the same number and the state is a handful of
  `u64` and `f64` per device.
- The link is rebuilt whenever the studio re-aims it, so the registry takes **deltas** and a
  counter that went backwards is read as a fresh link. Totals therefore only grow.

### `crates/screeny`: the counters (commit `cf6886c`)

`net.rs` now has `Wire { bytes, packets }`, `Traffic { out, inbound }` and `UDP_OVERHEAD = 28`
(IPv4 20 + UDP 8). **Everything counted is payload**; the overhead is the caller's to add, and
the doc comment says out loud that there is no honest equivalent for TCP.

- `ControlClient` counts every datagram sent (**retries included**) and every one received
  (**including a stray that was not the reply being waited for**); `traffic()` reads it back.
- `SendStats` gains `frames` - the frame port, whole datagram payloads out and everything the
  panel sent back - and `control`, the `GET_INFO` handshake, which happens once per session.
  `bytes` still means pixels and is untouched, so card 153's rules still read as written.
- `Link` **banks the difference** each time it looks (`absorb`), so a session's counters cannot
  go with its socket. Called after each send, in `poll`, and before the sender is dropped in
  `lose`/`close`/`reaim`. Cost per frame: eight `u64` subtractions on values already written -
  no lock, no allocation, nothing logged.

Measured, `cargo test --release -p screeny --test traffic`, 4 passed in 0.52 s:

- `frames.out.packets == frames_sent` and the receiver saw exactly that many;
- `frames.out.bytes == bytes + 8 * frames_sent` - the 8-byte frame header is the whole of the
  difference from the pixel count;
- telemetry back on the frame port is counted, and is under a tenth of what went out;
- handshake: exactly 1 datagram out and 1 in per session, on the control port, and **0 of each**
  with `handshake: false`;
- a link re-aimed mid-life: `control.out.packets` 1 -> 2 and the frame counters strictly grew;
  `close()` adds the `FINAL` datagram rather than losing it;
- a link pointed at `127.0.0.1:1` counts exactly zero.

`cargo clippy -p screeny --all-targets`: silent.

### The studio: per device, and one rate (commit `b473011`)

`devices.rs` gains `Flow { bytes, packets }`, `PathFlow { out, in }`, `Rates`, `Traffic`
(the three paths, the rolled-up `total`, the rates and `window_s`) and a `TrafficMeter`
that is **not** on the wire. `DeviceRecord.traffic` holds it; it is live only and never
persisted, because a total read back out of a file would be a lie about this process's
uptime.

`health.rs` puts `Traffic` under each device on `/api/v1/status` as `traffic`. Additive:
no route renamed, nothing removed.

**Where the bytes are picked up**, which is the thing worth writing down:

| path | counted in | read into the registry by |
|---|---|---|
| frames out | `Sender::transmit`, beside `stats.frames_sent += 1` - the `n` that `pkt.write` returned, so header included | `fleet::supervise` -> `Player::link_traffic()` -> `Registry::metered_link` |
| frames in | `Sender::poll_feedback`, on the `recv` that already exists, **before** the packet is parsed | same |
| control (handshake) | `Sender::connect`, from the `ControlClient` it already builds | same - it rides in `LinkStats.traffic.control` |
| control (everything else) | `ControlClient::request`, on the `send` and the `recv` | `devices::control_call` / `identify_at_counted` -> `Registry::metered_control`, from `fleet::apply_brightness`, `fleet::poll_once` (both branches), `api::on_device`, `api::devices_refresh` |
| http | `devhttp::read_status` (`cost.out += head.len()`) and `read_reply` (`cost.inbound += n`) | `fleet::status_once` -> `Registry::metered_http` |

`Link::absorb` is the piece to understand: a `Sender`'s counters die with its socket and
the studio rebuilds the link whenever it re-aims it, so the link banks the **difference**
each time it looks (after each send, in `poll`, and before the sender is dropped in
`lose`/`close`/`reaim`). The registry does the same thing one level up, with one
"is this a new link" decision for the **whole** link rather than one per counter.

The rate is `Registry::sample_traffic`, called from `fleet::supervise` **after** everything
that tick added, and from nowhere else. EWMA, `alpha = 1 - e^(-dt/5s)`, so the figure
means the same thing whatever the tick period is - which is what lets the tests drive it
on a synthetic clock through `sample_traffic_at`.

Two bugs the unit tests caught: a per-counter "new link" test missed a second handshake
whose figure was byte-identical to the first, and the rate needed a clock a test could
step. 19 `devices::` unit tests pass.

### The page (commit `7bb0e16`)

Three rows in the Panel screen's Link block, from `networkRows()` in `ui/panel.js`:

```
Network   36.4 KB/s out · 0.3 KB/s in
By path   frames 36.1 · control 0.1 · http 0.2 KB/s out, averaged over 5 s
Sent      2.1 GB out · 4.3 MB in since the studio started
```

Every figure is read; the page does no arithmetic, and `tests/ui.rs` asserts there is no
`/` in the block. `common.js` gains `kbs()` and `netSize()` and nothing else - **KB is
1000 bytes** there, which `kb()`/`size()` are not, and the comment says why they are two
functions rather than one with a flag. No new ids, so the existing wiring check covers it.
21 ui tests pass.

### The acceptance, against `screeny-sim` (commit `e61bd1f`)

`crates/studio/tests/traffic.rs`, four tests, a simulator each on explicit loopback ports,
discovery off, no state file. Measured:

```
frames out: 150 datagrams x 1304 B + 28 B in 5.00 s = 39.9 KB/s;
            the page says 39.4 KB/s (1.4% out)
```

against the card's 10%. The stream is the test card with its line stopped, so "mean frame
bytes" is a fact rather than an average over scenes - the first version of this test
divided by the mean over the whole run and read **51% out** on `clocks-numerals`, which is
a fair warning about what this number means on a patch that changes scene.

Also shown: http counters move both ways on every poll and the reply is the bigger half;
Identify and a brightness change each move the control counters both ways; the totals only
grow across twelve reads **and across the panel being taken away and given back** (which
rebuilds the link and resets its own counters); two reads inside one tick are identical,
which is what "one rate, every browser" means; and a panel that is away reads 0 KB/s and
zero frame packets.

A third bug caught here: `total` was rolled up on the supervisor tick and so lagged the
rows above it by up to a second. It is computed at read time now (`TrafficMeter::reported`).

`cargo clippy --workspace --all-targets`: silent.

### READMEs (commit `6428716`)

`crates/studio/README.md`: "What a panel costs the network" - the three paths, the
overhead rule, **what is deliberately not counted** (mDNS and the broadcast probe are not
traffic with *a panel*; browser frames stay under `sockets`), and that the rate is worked
out once. `crates/screeny/README.md`: the new shapes in the API map, and the sentence that
`bytes` is pixels while `traffic` is what the link cost the network.

### Paused

**Paused by the orchestrator** (two workers at a time; the design cards take priority).
Nothing is half-written: the branch compiles, `cargo clippy --workspace --all-targets` is
silent, and every test named above passes. The card is **left in `doing/`**.

Where I had got to: I had just started a hand-run loopback pair - `screeny-sim` on
127.0.0.1:50900/50901 with `--no-mdns --http-port 50902`, and a studio on 127.0.0.1:58787
with `--no-discover --device-http-port 50902` and a scratch `--state-dir` - attached it,
set it to `testcard`, and was about to let it stream and `curl /api/v1/status` for a real
example payload to paste into this Log and the report. Both processes are stopped and
nothing is left behind.

**What is left to do**, in order:

1. Re-run that loopback pair under `timeout` and capture one real `traffic` block from
   `/api/v1/status` for the Log and the report. (Only ever my own loopback output: a real
   device's status payload carries the SSID.)
2. Look at `/panel` at 390 px and 1400 px, to check the three new rows wrap rather than
   overflow at phone width. `ui/style.css` is **untouched** so far, and the hope is that it
   stays that way - the rows are ordinary `facts()` rows. If the "By path" line is too long
   at 390 px, shorten the wording rather than adding CSS.
3. Full `cargo test --release --no-fail-fast` at the root, and the final clippy.
4. `git mv` the card to `review/`.

Nothing learned that is not written above. The one thing to carry in your head if somebody
else picks this up: **the counters are payload, the overhead is added when the figure is
reported**, and the two places that add it are `TrafficMeter::wire` (for the rate) and
`TrafficMeter::reported` (for `total`). Adding it anywhere else double-counts.

### Resumed: merged card 151 (`7559c69`)

`git merge main` (`04455c0`, card 151's merge). **No conflicts** - 151 and this card turn
out to touch disjoint parts of the four shared files, and git seated both without help:

- `player.rs` - 151 worked on the player's configuration; my one accessor sits above
  `brightness_policy`, untouched.
- `api.rs` - 151 worked on the change routes; my two edits are in `devices_refresh` and
  `on_device`, neither of which 151 went near.
- `ui/panel.js` - 151 reworked the head of the file and the settings control; my
  `networkRows()` call is one line inside `showLink`, which it left alone.
- `tests/ui.rs`, `ui/common.js`, `ui/style.css` - additive on both sides; `style.css` is
  still untouched by this card.

Checked rather than assumed: the diff against merged `main` is exactly the edits this card
made and nothing else, and everything builds and passes on top of 151 -
`screeny --test traffic` 4/4, `studio --test traffic` 4/4, `studio --test ui` **27**/27
(151 added six of those), `studio --lib devices::` 19/19,
`cargo clippy --workspace --all-targets` silent.

One thing worth noting: `tests/ui.rs`'s `!PICTURE_JS.contains("traffic")` still holds after
151 rewrote the Picture screen, so "nothing about network traffic on the Picture screen" is
still a fact about the file rather than a stale assertion.
