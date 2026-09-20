---
id: 236
title: Firmware - an HTTP worker gets back to LISTEN quickly whatever the client does; the probe says when it was refused
type: build
hardware: orchestrator flashes (the worker builds and host-tests only)
depends: [243]
owner: worker-236
branch: card/236-http-close-bounded
---

## Goal

smoltcp has no listen backlog, so a connection that arrives while neither of the two HTTP
workers is in `accept` is **refused**. That is by design and stays. What is not acceptable
is how long a worker stays away from `accept` after it has answered: on 2026-09-20
`screeny-probe http` (sequential requests, `Connection: close`) went from 30/0/8 to 9-17
`Connection refused (os error 61)` failures per run **with no firmware change** - an A/B
against the build that had passed an hour earlier failed the same way. Make the time from
"response written" to "listening again" short and bounded, independent of the client.

## Context: what was measured (orchestrator, bench Mac wired, WiFi off)

- Passing run (`captures/fw-0.5.2-bench.log`): the probe's source ports are strictly
  consecutive - no connection was refused. Failing runs
  (`captures/fw-0.5.2b-http-rerun.raw.log`): gaps in the port sequence, one per refusal,
  and the device logs **no error at all** - every connection it accepted was handled.
  Never quote a `captures/` line that contains an SSID.
- In the failing log picoserve's `N requests handled from ...` line (printed after the
  connection is fully closed) regularly arrives *after* the next connection has been
  accepted by the other worker, and `Listening on TCP:80...` after that: the close is
  what is slow, and it overlaps the next request. With two workers both closing, the
  third connection is refused.
- Hypothesis (check it against the sources, do not trust it): `close_connection_after_response`
  ends in a graceful close that waits for the peer's ACK/FIN (and possibly smoltcp's
  TIME-WAIT), so its length is set by the **client's** delayed-ACK behaviour, which macOS
  adapts over time - hence "no firmware change". The same weakness is what the owner's
  iPhone hit on the setup network (card 223's Log, finding 3).
- Code: `firmware/src/http.rs` `http_task` / `serve_on` (picoserve 0.20
  `Server::listen_and_serve`, `Config`, timeouts), embassy-net `TcpSocket`
  (`close`, `flush`, `abort`, `set_timeout`, keep-alive), smoltcp 0.13.1 TCP states.
  Sources are under `~/.cargo/registry/src/`.

## Steps

1. Read picoserve's serve loop and embassy-net/smoltcp's close path; write in the Log
   exactly which awaits sit between "last response byte queued" and the next `accept`,
   and which of them depend on the peer.
2. Bound it. Options to weigh, in the Log, with the one you chose: a short socket
   timeout for the closing phase; `flush` then `abort` (RST after everything is ACKed -
   is any client harmed? the response carries `Content-Length` and `Connection: close`);
   owning the accept loop instead of `listen_and_serve`; anything picoserve's `Config`
   already offers. **No new `.bss` worth mentioning**: `tools/fw-size.sh` `.stack` must
   stay >= 24,576 and should not drop by more than ~200 bytes (27,088 now). A third
   worker is not an option (7.5 KB).
3. `crates/probe` HTTP client: on `ECONNREFUSED` retry the connect up to 3 times, 100 ms
   apart, **and count it**: the summary line says how many connects were refused and
   retried, and a rule whose retries are exhausted fails as now. A suite that passes with
   refusals must say so in its last line - the orchestrator wants to see that number go
   to ~0 with your firmware change, not have it hidden.
4. If `crates/sim`'s HTTP server can model "busy worker refuses", do not build that; note
   it and move on (owner's decision 10: good enough, not bomb-proof).
5. `FW_VERSION` -> "0.5.3".

## Exit

All four firmware builds (default, `panic-test`, `http-selftest`, `start-in-portal`)
clean and over the `fw-size.sh` floor, numbers in the Log; `timeout 1200 cargo test`
green; the Log tells the orchestrator what to run on the bench and what the probe's
summary line should read before/after. Copy the default ELF to the scratchpad as
`screeny-fw-0.5.3-default.elf`.

## Rules

Branch `card/236-http-close-bounded` from current `main`, in your own worktree; commit and
Log as you go, by explicit path; do not merge, do not push. **You do not flash**: no serial
port, no LAN, no camera. Bounded commands only. Never write a real SSID or password
(dummies `Example-Wifi1` / `password9`). Leave `crates/art` and `crates/studio` alone.

## Log

### Step 1 - the await chain between "response written" and "listening again"

Read, at the pinned versions: `picoserve 0.20.0`, `embassy-net 0.9.1`, `smoltcp 0.13.1`,
all under `~/.cargo/registry/src/index.crates.io-.../`. Line numbers below are in those
sources.

**The chain, in order, with what each await is waiting for:**

1. `picoserve/src/lib.rs:466` - the handler's `write_to` returns `ResponseSent`. Every
   byte of the reply is now in smoltcp's 1,024-byte tx buffer (or already on the wire);
   `Content-Length` was measured by the counting pass before the first byte went out.
2. `lib.rs:468-473` - `KeepAlive::Close`, so `LoopResult::Stop(Ok(..))` and the serve
   loop falls out of `serve_and_shutdown`'s inner `async` block. No await.
3. `lib.rs:518-525` - `connection_flags.connection_must_be_aborted()` is false for a
   request whose body was read (or had none), so it is
   **`socket.shutdown(&config.timeouts, timer)`**, not `abort`.
4. `picoserve/src/io.rs:339-368`, the `impl Socket<EmbassyRuntime> for TcpSocket`:
   a. `self.close()` - `embassy-net/src/tcp.rs:416` -> `smoltcp::socket::tcp::close()`.
      Queues the FIN behind whatever is still in the tx buffer. Not an await.
      smoltcp will emit it as soon as everything buffered has been *sent*
      (`smoltcp/src/socket/tcp.rs:2285-2291`: `can_fin` needs
      `remote_last_seq == local_seq_no + tx_buffer.len()`; Nagle explicitly does not
      hold a FIN back, line 2281's `&& !want_fin`).
   b. `io.rs:351-364` - `select(` `run_with_timeout(timeouts.read_request /* 5 s */,`
      `rx.discard_all_data())`, `tx.flush().and_then(pend_forever)` `)`.
      `discard_all_data` (`io.rs:16-22`) loops `read()` **until it returns 0**, and
      embassy-net returns `Ok(0)` only for `smoltcp::RecvError::Finished`
      (`embassy-net/src/tcp.rs:538`), i.e. only once **the peer has closed its own write
      half**. The other arm can never finish the select: `tx.flush()` is followed by
      `pend_forever`. So this await is **"wait until the client's application calls
      `close()`"**, bounded only by the 5 s `read_request` timeout.
      *This is the peer-dependent await, and it is the whole bug.*
   c. `io.rs:366-371` - `run_with_timeout(timeouts.write /* 5 s */, self.flush())`.
      embassy-net's flush (`tcp.rs:629-651`) is pending while
      `send_queue() > 0` (unACKed data), or the state is `FinWait1 | Closing | LastAck`
      (our FIN unACKed), or a RST is still queued. So it resolves once **everything we
      sent, FIN included, has been acknowledged**. Peer-dependent too, but only by one
      ACK, and BSD/macOS force `TF_ACKNOW` on a FIN, so it is ~1 RTT.
5. The `TcpSocket` is consumed by `shutdown(self)` and dropped at its end;
   `Drop` (`embassy-net/src/tcp.rs:467`) removes the handle from smoltcp's `SocketSet`,
   so no TIME-WAIT entry lingers - the slot is free immediately.
6. `picoserve/src/lib.rs:766-771` - `log_info!("{} requests handled from {:?}", ..)`.
7. `lib.rs:723-733` - `continue`, a fresh `TcpSocket::new`, `log_info!("{}: Listening on
   TCP:{}...", ..)`, then `socket.accept(port).await`.

**Which part depends on the client: 4b, entirely.** The device is parked until the
*client application* closes its socket. `screeny-probe`'s own client
(`crates/probe/src/http/client.rs:269-298`) stops reading the moment `Content-Length` is
satisfied and drops the `TcpStream` when `exchange` returns, so on a good day its FIN is
one RTT behind - but nothing in the protocol requires that, macOS schedules the close
whenever it likes, and a browser or iOS's captive sheet can sit on a read connection for
much longer. That is exactly the shape of card 223 finding 3 (the sheet's second,
unused connection pinning a worker) seen from the other end of the connection, and it is
why an A/B with no firmware change could go from 30/0/8 to 9-17 refusals: the variable
is on the Mac.

**Second, smaller contributor, on the same path:** steps 6 and 7 are two `log_info!`s
per connection through `esp-println`'s **blocking** UART at 230,400 baud, plus
`"Received connection from {:?}"` at `lib.rs:747`. ~150 bytes of serial per connection is
~6.5 ms of core 0 spent *between* the close and the `accept` - on top of the formatting.
The module already says per-request logging belongs on the setup network only
(`firmware/src/http.rs:1285-1304`); picoserve's three lines were never subject to that
rule because picoserve writes them.

**Baseline build (unchanged tree, for the before/after):** `.stack` **27,088**,
`.bss` 110,272, `.data` 59,236, image 974,181.

### Step 2 - the options, and the one chosen

`picoserve::Config` offers nothing for this: `Timeouts` is exactly four durations
(`start_read_request`, `persistent_start_read_request`, `read_request`, `write`) and
`KeepAlive`, and the shutdown is hard-coded in `impl Socket for TcpSocket`. So every
option below is a change to how the socket is closed or to who owns the accept loop.

| option | what it costs | why not / why |
|---|---|---|
| Shorten `timeouts.read_request` | one constant | **No.** The same number bounds *reading a request*, where 5 s is wanted. Shortening it to bound the close would shorten the wrong thing, and would still leave the close depending on the peer, just less. |
| `keep_connection_alive()` | one line | **No.** Off the card, and it makes the shortage worse: two workers, two browsers, no server. [`HTTP_TASKS`] already argues this. |
| A third worker | 7,504 bytes of `.bss` | Excluded by the card, and `.bss` is core 0's stack. It would also not fix anything: three workers all waiting on three clients is the same failure one connection later. |
| `flush()` then `abort()` (RST once everything is ACKed) | small | **Nearly.** The delivery argument is sound (see below), but the RST is gratuitous: once `flush` has returned we can simply *drop*, and then the peer is only reset if it sends something - which, if it has finished reading, it does not. A RST we do not need is a RST that can turn a clean EOF into `ECONNRESET` on some client. |
| Wait for the peer's FIN with a short timer, then drop | small | **Nearly.** It keeps the textbook four-way close for a prompt client, at 1 RTT. But the bound would have to be ~20 ms to help, which is of the same order as a whole request cycle on this LAN - so in the bad case it still eats the budget it is meant to protect, and it is still the peer deciding. |
| **`close()`, wait for the acknowledgement, drop** | -112 bytes of `.bss` | **Chosen.** |

**The chosen close, and why it cannot truncate a response.** `TcpSocket::close()` queues
the FIN behind the last body byte; `TcpSocket::flush()` is pending while any sent byte is
unacknowledged or the state is still `FinWait1 | Closing | LastAck`, so when it returns,
**the peer's TCP has acknowledged every byte and the FIN**. The FIN occupies the sequence
number after the last body byte, so an ACK of the FIN *is* an ACK of the whole body -
there is no ordering in which the acknowledgement arrives and the body did not. On top of
that every reply this server sends carries `Content-Length` (picoserve measures each body
with a counting writer before it writes a header, the streamed status page included), so
no client here needs to see the close to know where the body ended: `screeny-probe`'s own
client stops on `Content-Length` and never reads to EOF at all
(`crates/probe/src/http/client.rs:269-275`).

What is given up is the second half of the four-way close. The socket is dropped in
`FIN-WAIT-2`; `Drop` removes it from smoltcp's set, so the client's FIN, whenever it
comes, matches no socket and smoltcp answers with a RST. Three cases:
- the client has read everything and closed - which is every client here, because
  `Content-Length` told it when to stop. Its own socket is closed by then; the RST
  changes nothing it can observe.
- the client is still reading out of its own receive buffer. It sends nothing, so it
  draws no RST, and it has every byte.
- the client has the bytes, has not read them, and emits a window update. It gets a RST
  with data in its socket buffer - and both Darwin's `soreceive` and Linux's
  `tcp_recvmsg` deliver buffered bytes to the application before they report the error,
  so the worst outcome is `ECONNRESET` where `EOF` would have been, after the last byte.
  That is decision 10's trade: good enough and crash proof, not bomb-proof.

**iOS / the setup network (card 223 finding 3) gets strictly better, not worse.** That
finding is this same shortage seen from the other side: the captive sheet opened a third
connection "while the worker that had just answered was between `close` and `accept`",
and iOS does not retry a refused connection. The window this card closes is exactly that
window. Nothing about the sheet's behaviour is relied on: the reply is complete and
acknowledged before the socket goes, and the page it fetches is the one that carries the
form, `200` with `no-store`, with a `Content-Length`.

**A stalled or malicious client still cannot pin a worker.** The per-phase deadlines are
untouched (3 s to start a request, 5 s to finish one, 5 s to take the reply) and the
close now adds `CLOSE_ACK_MS` = **500 ms** instead of up to 10 s. `set_timeout(45 s)` and
`set_keep_alive(30 s)`, which picoserve set on every accepted socket, are set here too -
they are the backstop for a half-open connection none of the picoserve phases can see.
Worst case after the change: ~8.5 s of stalling before the reply, 0.5 s after it. Before
the change the *ordinary* case could cost seconds after it.

**Second win on the same path, free.** Owning the accept loop also drops picoserve's three
`info!` lines per connection, two of which (`"N requests handled from .."`,
`"Listening on TCP:80..."`) sit between the close and the next `accept`: ~150 bytes
through `esp-println`'s **blocking** UART at 230,400 baud is ~6.5 ms of core 0 per
connection, on the exact path this card shortens, on the LAN, forever. This module's rule
was already that per-request logging belongs on the setup network only; picoserve's lines
were outside it only because picoserve wrote them. `Dispatch`'s setup-network line, which
is what found card 223's finding 3, is unchanged.

### Step 3 - the probe says when it was refused

`crates/probe/src/http/client.rs`: `Client::connect` retries a connect **only** on
`ErrorKind::ConnectionRefused` - `CONNECT_TRIES` = 3, `CONNECT_RETRY_DELAY` = 100 ms - and
increments a counter on every refusal, recovered or not. A timeout, a reset or an
unreachable host still fails on the first try, because those say something is wrong where
a refusal says "busy". The counter is an `Arc<AtomicU32>` so that the clone the restore
guard holds (`http/mod.rs:621`) counts into the same total.

`Summary` grows `refused: usize` and the last line now reads:

```text
http conformance: 34 passed, 0 failed, 5 skipped, 0 connects refused
```

and, when it is not zero:

```text
http conformance: 34 passed, 0 failed, 5 skipped, 11 connects refused (retried up to 3 times each; the device had no worker in accept)
```

So a run that passes *because of* the retries no longer prints the same line as one that
never needed them. A rule whose three tries are all refused fails as before, and all three
refusals are counted.

New test `a_refused_connect_is_retried_and_every_refusal_is_counted`: bind a loopback
port, drop the listener, and connect - the host's own stack then answers the SYN with a
RST, which is `ECONNREFUSED` on demand. It asserts three tries, three counted, the spacing,
and that a clone reports the same total.

### Step 4 - `crates/sim` is not taught to refuse

Not built, as the card says. `crates/sim/src/http.rs:176` is a `std::net::TcpListener`
with a thread per connection: it has the kernel's listen backlog, so it *cannot* refuse
the way smoltcp does without being rewritten to model a worker pool. The simulator's job
is the API's shapes, not the device's socket economics, and decision 10 says good enough.
The sim's conformance test now asserts `refused: 0`, which is what a server with a real
backlog owes.

### Numbers

`tools/fw-size.sh`, all four builds, before (at `main`) and after:

| build | `.stack` before | `.stack` after | `.bss` before | `.bss` after | image before | image after |
|---|---|---|---|---|---|---|
| default | 27,088 | **27,232** (+144) | 110,272 | 110,160 (-112) | 974,181 | 971,965 |
| `panic-test` | 27,024 | **27,152** (+128) | 110,336 | 110,224 (-112) | 975,137 | 972,853 |
| `http-selftest` | 26,784 | **26,800** (+16) | 110,544 | 110,544 (0) | 1,008,689 | 1,006,293 |
| `start-in-portal` | 27,088 | **27,232** (+144) | 110,272 | 110,160 (-112) | 974,125 | 971,909 |

All four are above the 24,576 floor and none fell; the card's "must not drop by more than
~200 bytes" is comfortably met in the other direction. The `.bss` that goes is
`ReadExt::discard_all_data`'s 128-byte buffer, which picoserve held across an await - i.e.
the fix pays for itself in the pool that breaks first. `http-selftest` gains least because
its second, in-memory `Server::serve` chain (`selftest_one`) never went near
`listen_and_serve`.

`cargo clippy --workspace --all-targets`: silent. `cargo clippy --release` in `firmware/`:
9 warnings, all pre-existing (`doc_lazy_continuation` in `store.rs`, a collapsible `if`,
and `serve_on`'s 8 arguments, which it already had).

`timeout 1200 cargo test` from the repo root: **759 passed, 0 failed, 1 ignored**, 49 test
binaries.

Ran it four times to be sure of that. Three of the four runs had **one** failure each, a
different one every time - `screeny-studio --test moved`
(`the_status_poll_follows_a_panel_that_moved`), `screeny-studio --test soak`, `screeny
--test pacing` (`holds_thirty_fps_within_one_percent`), `screeny --test loopback`
(`the_reported_rate_is_the_same_at_any_stream_length`). Every one of them passed on its
own immediately afterwards, every one of them is a wall-clock or port-binding assertion,
and none of them is reachable from anything this card touched (the probe's HTTP client,
the probe's summary, one `sim` assertion, `firmware/`). It is the same class as card 117's
port re-bind under parallel worktrees that the card-198 merge recorded, and this machine
was building Xtensa firmware between runs. **It is not caused by this card, and it is
worth a card of its own**: four host tests that fail under load are four tests that will
eventually be ignored by a human, which is how a suite stops meaning anything.

End-to-end smoke against the simulator (`screeny-sim --headless --no-mdns --http-port
8099`, `screeny-probe --addr 127.0.0.1:49475 http --http 127.0.0.1:8099`): 34 passed, 0
failed, 5 skipped, **0 connects refused**. No background process left.

### For the bench (the orchestrator)

1. Flash the default ELF in the scratchpad as `screeny-fw-0.5.3-default.elf` with
   `tools/fw-run.sh`. `GET /api/v1/status` should report `fw` **0.5.3**.
2. Mac WiFi **off**, wired, as `device-web.md` says for conformance.
3. `cargo run --release -p screeny-probe -- --addr 192.168.7.221 http`, and read the
   **last line**. Before this card, on 0.5.2, the same run was 9-17 `Connection refused
   (os error 61)` failures out of ~35 requests (and with this probe those would have shown
   as a large `connects refused` count rather than as failures). The number to expect now
   is **0, or a low single digit**; anything in double figures means the close is not the
   whole story and the next thing to look at is the gap between the last
   `Listening on tcp/80` line and the request that was refused.
4. Worth noticing on the serial log: it is **much quieter** during an HTTP run - the three
   picoserve lines per connection are gone. `net: http worker N listening on tcp/80` is
   still printed once per worker per AP flip, and the setup-network per-request line is
   unchanged.
5. The `screeny stats` / telemetry numbers should not move: nothing on the frame path
   changed, and core 0 does *less* work per connection than it did.

**Orchestrator, after the merge (2026-09-20): merged as 90e6392; fw 0.5.3 is on the device.**
Built from `main`: `.stack` 27,232. On the bench, Mac wired, WiFi off:
`screeny-probe http` three times in a row - **31 passed, 0 failed, 8 skipped, 0 connects
refused**, each time (0.5.2, same hour, same probe host: 9-17 refused per run).
`stack_free` 12,224 after them; `/api/v1/panic` clean. UDP conformance once, panel
released: **60/0/4**. Stream back at 30 fps, zero drops. So the close was the story; the
split between the peer-dependent `discard_all_data` and the blocking UART lines was not
measured and does not need to be. The four load-sensitive host tests the worker hit
(`studio moved`/`soak`, `screeny pacing`/`loopback`) are told to the `software` session;
not chased here (decision 10). fw 0.5.2 before it: a one-hour passive soak, 714 telemetry
lines, no WARN/ERROR/reset. The acceptance soak now runs on 0.5.3.
