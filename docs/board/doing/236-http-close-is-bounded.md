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
