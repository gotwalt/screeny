---
id: 243
title: Firmware 0.5.2 - a panic reboots and leaves a breadcrumb; the boot path gives 3 KB of stack back
type: build
hardware: orchestrator flashes (the worker builds and host-tests only)
depends: [234]
owner: worker-243
branch: card/243-panic-breadcrumb
---

## Goal

The owner's bar (decision 10 in `docs/design/device-web.md`): "as long as it's not running
out of memory and is pretty crash proof I'm happy". Two halves, one firmware build, 0.5.2:

1. **Crash-proof**: today a panic on core 0 ends in `interrupt_free(|| loop {})` - core 0
   spins forever, core 1 keeps the panel lit, nothing reboots, and the backtrace goes to a
   UART nobody reads (card 234's Log, candidate 2; that is very likely what the one silent
   stall of 0.5.1 was). After this card a panic **records a breadcrumb in RTC memory and
   resets the chip**, the device rejoins by itself in ~15 s, and the next boot's log and
   the status API say that a panic happened, when, and where.
2. **Memory**: the boot path's two 3 KB partition-table buffers set core 0's 13 KB stack
   demand; share one. And the `http-selftest` bench build must fit the `fw-size.sh` floor
   again.

## Context (read first)

- `docs/board/done/234-silent-stall-on-0.5.1.md` Log: "Instrument proposed" (the design
  to build: `esp-backtrace`'s `custom-halt` feature, a
  `#[esp_hal::ram(rtc_fast, persistent)]` array, `software_reset()`; cost 0 `.bss`) and the
  lock map.
- `docs/research/006-flash-store-ota.md` section G (the breadcrumb as planned for OTA) -
  card 241's revert logic will read the same breadcrumb, so shape it for that too: a
  magic/version word, a **boot counter**, the **reset reason** of the previous boot, uptime
  at the panic, the panic's `file:line` hashed or truncated into words, and a **consecutive
  panic count** (a crash loop must be recognisable: OTA's health check needs it).
- `docs/design/device-web.md`: "How to think about storage and RAM" - `.bss` comes out of
  core 0's stack; `tools/fw-size.sh <elf>` must show `.stack` >= 24,576 (27,376 on `main`).
- `firmware/src/main.rs` `read_fw_health` and `firmware/src/store.rs` `store::init`: each
  puts a ~3 KB partition-table buffer on main's stack.

## Steps

1. Panic path: custom halt -> write the breadcrumb -> reset. It must be safe from any
   context (interrupts off, either core, no allocation, no locks, no `await`, no logging
   after the backtrace has printed). Keep `esp-backtrace` printing the backtrace first.
   A panic on **core 1** (the display core) must take the same path.
2. Crash-loop guard: if the breadcrumb shows N consecutive panics within a short uptime
   (propose N and the window; e.g. 5 panics each under 60 s), stop resetting and halt with
   the panel showing a plain "crashed" screen - a device that reboots forever on USB power
   is worse than one that says so. Say in the Log what you chose and why.
3. Boot: log one line from the breadcrumb (previous reset reason, boot count, and the
   panic record if there is one). Add the same facts to `GET /api/v1/status`
   (`crates/device-api` owns the shape: add the fields there, with golden-file tests, and
   keep the simulator serving them - null/zero on the sim is fine) and one line on the
   status page. Tell the orchestrator in your report exactly which shared files changed.
4. A bench-only way to prove it: a cargo feature `panic-test` (off by default) that panics
   on core 0 some seconds after boot **once** (use the breadcrumb to not do it again), so
   the orchestrator can watch panic -> reset -> rejoin -> breadcrumb reported.
5. Stack lever: one shared partition-table buffer for `read_fw_health` and `store::init`
   (or read only what each needs). Report `stack: core 0 high-water` before/after from
   the code's own arithmetic; the orchestrator measures it on the device.
6. `http-selftest`: make its buffers the workers' (5.3 KB of `.bss` today) so
   `cargo build --release --features http-selftest` passes `tools/fw-size.sh`.
7. Ride-alongs (tiny, from cards 225 and 234): `provision::step` must not log inside the
   `MACHINE` critical section (`firmware/src/provision.rs` ~648); the setup form's
   `maxlength=63` -> 64 for the PSK; stale "spec section 8.3"/"6.9" citations for the
   `GET_WIFI` byte and `REBOOT` magic (`crates/device-api/src/enums.rs:65`,
   `crates/device-api/src/request.rs:135`, `crates/sim/src/wifi.rs:173,271`,
   `firmware/src/main.rs:500`, `firmware/src/provision.rs:180`) -> section 6.3;
   `firmware/src/http.rs:53` no longer says the scan is "also 223" (card 229 is dropped);
   one clause in `docs/design/protocol-v1.md` 7.3 saying frames under the setup-screen
   overlay are counted but not shown (8.1 already says it).
8. `FW_VERSION` -> "0.5.2".

## Exit

- `. ~/export-esp.sh && cd firmware && cargo build --release` clean (no new warnings), and
  the same with `--features panic-test`, `--features http-selftest`,
  `--features start-in-portal`; `tools/fw-size.sh` on each: `.stack` >= 24,576, numbers in
  the Log. **RTC memory, not `.bss`**, for the breadcrumb.
- `timeout 1200 cargo test` (workspace) green.
- The Log says exactly what the orchestrator should see on the bench for the `panic-test`
  build (the serial lines, in order) and for the default build.

## Rules

Branch `card/243-panic-breadcrumb` in your own worktree; commit and Log as you go, by
explicit path; do not merge, do not push. **You do not flash**: no serial port, no LAN, no
camera - the orchestrator flashes your ELFs and reports back. Bounded commands only; never
write a real SSID or password anywhere (dummies `Example-Wifi1` / `password9`; do not read
`~/.config/screeny/wifi.env`; never quote `captures/` lines containing an SSID). Leave
`crates/art` and `crates/studio` alone. `crates/device-api` and `crates/sim` are shared
with the `software` session: additive changes only, listed in your report.

## Log
