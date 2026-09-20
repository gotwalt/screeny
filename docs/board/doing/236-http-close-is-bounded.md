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
