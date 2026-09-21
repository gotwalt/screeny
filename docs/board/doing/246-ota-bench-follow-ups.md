---
id: 246
title: Firmware + probe - four rough edges the OTA bench found
type: build
hardware: orchestrator flashes and uploads (the worker builds and host-tests only)
depends: [241, 245]
owner: opus worker (firmware session, 2026-09-21)
branch: card/246-ota-follow-ups
---

## Goal

OTA works end to end (cards 240/241/245, fw 0.7.0; the bench is in card 241's Log, last
section). Four small things it found. Fix them, nothing more (decision 10 in
`docs/design/device-web.md`: good enough, crash proof, not bomb-proof).

## The four, as measured on the device (2026-09-20)

1. **`busy` after a confirmed update, until a reboot.** After `ota: CONFIRMED at 60 s` the
   device answered every `POST /api/v1/firmware` with
   `{"ok":false,"written":0,"error":"busy","activating":false}` (HTTP 200, after reading
   ~750 KB of the body - the probe saw `write: Broken pipe`). `screeny reboot --yes` cleared
   it; the next upload was accepted. The refusal exists for an image that is *on trial*
   (its other slot is the one to roll back to); confirmation must lift it. Find what the
   `busy` check reads (`firmware/src/ota.rs`, `Upload::start`) and what `trial_task` leaves
   set at `CONFIRMED`. Also: a refusal that can be decided from state alone (`busy`) should
   be answered **before** reading the body, like `too_large` is.
2. **`screeny-probe fw-upload --activate` decides too early.** It printed `back after 28 s:
   fw 0.7.0 slot Ota0 state Valid ... uptime 148180 ms` - the *old* image, which had not
   gone down yet (activation happens ~2 s after the reply, the poll won the race) - and then
   `the device reports no update record - nothing to wait for`, while the device was in fact
   rebooting into its trial. Wait for `boot_id` (status) to **change**, then for
   `/api/v1/panic`'s `update.outcome` to leave `trial`; bounded, as now.
3. **Whose `fw_state` is it after a revert?** After a rollback `GET /api/v1/status` reads
   `fw 0.7.1, fw_slot ota_1, fw_state invalid` (or `aborted`) although the running image is
   fine: `fw_state` is reporting the `otadata` entry of the slot that was rolled back
   *from* (card 241's Log explains why `current_app_partition` names it). True to
   `otadata`, wrong to a reader. `fw_state` must describe the **running** slot; the failed
   update is already fully described by `/api/v1/panic`'s `update`. No new fields on
   `StatusReply` (card 243b: every byte there costs ~12x in stack). Keep the simulator,
   the goldens and spec 8.6 in step.
4. **The probe's restore guard does not run when the probe dies.** An HTTP-suite run that
   ended on a broken pipe left the panel named `probe-228` at brightness 49 for an hour
   (the orchestrator put it back by hand). The suite sets name/brightness/idle mode and
   restores them at the end; make the restore run on every exit path the process can
   survive (error returns, panics -> `Drop`), and make a *later* run notice a device still
   named `probe-*` and say so loudly instead of "restoring" to that name.

## Exit

`FW_VERSION` -> "0.7.1". All firmware builds over the `tools/fw-size.sh` floor (`.stack` >=
24,576; 26,200 now), numbers in the Log; `timeout 1200 cargo test` green; host tests for
1 (the state that lifts `busy`), 2 (against the simulator: make it able to model "answers
with the old boot_id for a few seconds, then goes away, then comes back"), 3 and 4.
Artefacts in the orchestrator's scratchpad (path in the worker prompt):
`screeny-fw-0.7.1-default.elf` and an upload image `screeny-fw-0.7.2-good.bin` (+ELF) made
as card 240's Log describes. Bench procedure for the orchestrator: serial-flash 0.7.1;
`fw-upload screeny-fw-0.7.2-good.bin --activate` and watch the probe wait correctly through
reboot -> trial -> confirmed; then **without rebooting** upload again (stage only) and see
it accepted; check `fw_state` reads `valid`.

## Rules

Branch `card/246-ota-follow-ups` from current `main`, own worktree; commit and Log as you
go by explicit path; do not merge or push. No hardware, no serial port, no LAN, no camera.
Every command bounded with `timeout`, **every wait written as
`timeout N bash -c 'until ...; do sleep S; done'`**, and before the final report run
`ps -axo pid,ppid,etime,command | grep -E 'sleep|until|timeout'` and kill what is yours.
Never write a real SSID or password (dummies `Example-Wifi1` / `password9`). Leave
`crates/art` and `crates/studio` alone; list every change to shared crates
(`device-api`, `sim`, `probe`).

## Log

### Item 1 - why a confirmed device answered `busy` for ever (2026-09-21)

**Root cause, one line: the boot classification is read once and never revised,
and the refusal asked it and nothing else.**

`Upload::start` (fw 0.7.0, `firmware/src/ota.rs`) refused with
`FirmwareError::Busy` when `boot_class().on_trial() || activating()`.
`boot_class()` reads the `BOOT` atomic, which `note_boot` sets **once**, in the
boot path, from `classify(booted, selected, state)`. A trial boot sets
`BOOT_TRIAL` and nothing ever changes it again: `confirm()` sets `CONFIRMED`,
calls `set_fw_state(FwState::Valid)` and logs `ota: CONFIRMED at 60 s`, and
leaves `BOOT` exactly where it was. So `on_trial()` stayed true for the whole
life of that boot and every later upload was refused - which is what the bench
saw, and why `screeny reboot --yes` cleared it (the next boot classifies
`Valid` -> `Settled`).

The fix keeps the safety rule and lifts it at the right moment. The rule now
lives in `crates/otastate` as a pure function with its own tests:

```rust
pub fn slot_is_spoken_for(boot: Boot, confirmed: bool, activating: bool) -> bool {
    activating || (boot.on_trial() && !confirmed)
}
```

The inactive slot is the escape hatch of an *undecided* trial; a confirmed
image is not rolling back anywhere, so the slot is free. `ota::slot_is_spoken_for()`
feeds it the three atomics (no flash, no lock), and `Upload::start` asks that.
`update_record()` is untouched, so `/api/v1/panic` still reports
`outcome: confirmed` after a confirm - which is what the probe's wait (item 2)
watches for.

**The second half - "answered before reading the body".** The handler already
decided `busy` and `too_large` before it read a byte; the delay was one layer
up. `Dispatch::call` must call `RequestBodyConnection::finalize()` before it
can write a reply, and picoserve's `finalize`, faced with a body the handler
did not read, **drains it** until the `read_request` timeout - 5 s here, which
on this radio is the ~750 KB the bench saw, and only then writes the refusal.
Its other path (`request.rs`, `finalize`, case 1) skips the drain entirely when
the handler has read *past the end of picoserve's request buffer*. So
`refuse_at_once()` reads `buffer_length() + 1` bytes - at most `HTTP_BUF`
(1,536) + 64, a millisecond of socket, never the image - and the refusal goes
out while the caller is still writing. The connection closes after the response
(`close_connection_after_response`), so the bytes still in flight are discarded
by the close, exactly as they were before. All three state-only refusals
(`unavailable` x2, `too_large`, `busy`) now go through one call site, because
this function is inlined into the frame every request pays for.

`FW_VERSION` -> `0.7.1`. `cargo test -p screeny-otastate`: 23 passed, 0 failed.
