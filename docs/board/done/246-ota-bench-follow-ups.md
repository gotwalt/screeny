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

**And it costs nothing.** The first shape of this cost **640 bytes of `.stack`**
(26,200 -> 25,560), which is a lot for a nicety, and the measurement said it was
not the 64-byte scratch and not the second reader: it was
`let start: Result<Upload, Reply> = ...` - an `Upload` and a `Reply` held in a
temporary the compiler kept alive across the awaits, in the future of both HTTP
workers. Carrying a two-byte `Refused` instead, building the reply after the
release, and building the `Upload` outside the `Result` puts `.stack` back at
**26,200, byte for byte `main`'s** (checked by building `main`'s `firmware/src`
in this worktree with the same toolchain). That is card 227's lesson arriving
in a place nobody was looking: *anything* alive across an `await` here is
`.bss`, and `.bss` is core 0's stack.

### Item 3 - whose `fw_state` it is (2026-09-21)

`http::read_fw_health` reads `Ota::current_ota_state()`, which is the entry
with the **highest sequence number**. On a settled or trial boot that is the
running slot's own entry and everything is fine. After a rollback it is the
entry of the slot that was rolled back *from* - `esp-bootloader-esp-idf`'s
`current_app_partition` works from `max(ota_seq)` and never looks at the states
(card 241's Log, "the trap in the crate's own bookkeeping") - so the device
reported `fw 0.7.1, fw_slot ota_1, fw_state invalid` with a healthy 0.7.1
running: `fw_slot` from the MMU, `fw_state` from the other slot, two fields
describing two different images.

`screeny_otastate::running_state(booted, selected, selected_state)` is the
correction, in the one place, with tests:

* `booted == selected` - the entry read **is** the running slot's: report it
  unchanged, including the messy `invalid`/`aborted` case where the bootloader
  fell back to `ota_0` and `otadata` is in a state nobody should hide.
* `booted != selected` - a rollback. The bootloader will not hand over to an
  `invalid` or `aborted` entry, so the slot it chose is one it considers good
  and the image running is the one that confirmed itself last time: **`valid`**.
* either slot unknown - pass through what was read.

`note_boot` returns that instead of the raw state (and logs one `info!` line
naming both, so the `otadata` truth is still on the serial log). **No new
field on `StatusReply`**, nothing added to the reply at all. The simulator
needs no change: its `Ident::fw_state` is already documented as "the running
slot's state" and it has no `otadata` to disagree with.

Shared surface touched, both doc-only apart from the new function:
`crates/device-api/src/enums.rs` (`FwState`'s doc comment) and
**`docs/design/protocol-v1.md` §8.6 and §8.10 step 5**, which said the opposite
in as many words - §8.10 step 5 read "`fw_state` is the *rejected* entry's
state". Spec and code disagreed; the **spec was the one that was wrong** (it
described what 0.7.0 did, not what a reader can use), and it is now normative
the other way: `fw_slot` and `fw_state` always describe one image. The software
session shares that file: this is the change to tell them about.

`cargo test -p screeny-otastate`: 18 unit + 23 interruption tests, 0 failed.
`.stack` still 26,200.

### Item 2 - the probe decided before the device had gone (2026-09-21)

The race, named: **the device answers an activating upload about two seconds
before it restarts** (`ota::ACTIVATE_DELAY_MS`, which exists so the reply is
acknowledged before the connection is destroyed). So the first `GET
/api/v1/status` after the reply reaches the **old image**, which is running,
answering, and knows nothing about an update that has not started. The probe
took that as "it came back" (`fw 0.7.0 ... uptime 148180 ms` - its own uptime,
28 s after an upload) and then took `update: null` as "nothing to wait for" and
exited zero. Two conclusions, one mistake.

`crates/probe/src/http/update.rs` (new, `screeny_probe::http::watch`) waits for
something the old image cannot say:

1. `boot_id` is read **before** the upload (`update::boot_id`);
2. phase one polls `/api/v1/status` and **ignores every answer that still
   carries it**, saying so once ("still boot_id N - the old image, which has
   not restarted yet");
3. phase two polls `/api/v1/panic` until `update.outcome` leaves `trial`;
4. `update: null` after the device is back no longer ends it: there is a
   `no_record_grace` (20 s) for the new image to classify its own boot, and
   only after that is it reported as "a firmware without card 241".

Both phases are bounded as before (`Watch { reappear: 90 s, decide: 240 s }`),
the outcome is an enum (`Confirmed` / `Reverted` / `NeverCameBack` /
`NoRecord` / `Undecided`) and every line it used to print goes through a `say`
callback, so a test can read the transcript. If `boot_id` could not be read
before the upload it says that too, rather than quietly doing the old thing.

**The simulator got the clock it needed** (`crates/sim/src/ota.rs`, new, off by
default, `SimHandle::model_ota(Some(OtaTiming))`): an activating upload plays
old image -> **away** (connections closed unanswered, `http::no_answer()`) ->
trial with a new `boot_id` and the uploaded version -> confirmed. With the
model off, `POST /api/v1/firmware` still answers `activating: false` exactly as
card 224 decided, so no existing test or probe rule changes.

`crates/sim/tests/ota_watch.rs` (new, 3 tests, 1.5 s): the wait follows the
update through a reboot it did not see and reports `Confirmed` - having first
asserted that the old image *is* still answering with the old `boot_id` and no
record, which is the exact state 0.7.0's probe declared victory in; a device
that never restarts is `NeverCameBack` inside `reappear`; a device that
restarts with no `update` object is `NoRecord`, and only after the grace.

One bug found in my own model on the way: `running_image()` took the state lock
and then asked `phase()`, which takes it again. `std::sync::Mutex` is not
reentrant, so the first status read during a trial deadlocked the HTTP thread
and everything behind it - the test hung for the whole 900 s bound. Phase
first, lock once.

`cargo test -p screeny-sim`: 31 + 5 unit, and every integration file green,
including the 36 s conformance run - 0 failed.

### Item 4 - the restore that did not run, and the run that would have made it permanent (2026-09-21)

Two halves, and the first one had a specific cause. `RestoreGuard`'s `Drop`
really does cover a return, a `?` and a panic (the workspace unwinds; nothing
sets `panic = "abort"`), and the ctrl-c handler covers ctrl-c. What neither
covers is a **signal** - and every command on this bench is bounded with
`timeout`, which ends a run with `SIGTERM`. `ctrlc::set_handler` registers
SIGINT alone unless the crate's `termination` feature is on. It is now on
(`crates/probe/Cargo.toml`; the feature is `termination = []`, no new
dependency, and it builds offline), so the same handler covers **SIGINT,
SIGTERM and SIGHUP**, in the HTTP suite and in the UDP suite, which share it.
`SIGKILL` cannot be covered by anything and the docs now say so rather than
implying otherwise.

Which is why the second half exists: **the next run is what notices**. The
suite's own name has one definition (`http::PROBE_NAME`, `probe-228`) and one
prefix (`PROBE_NAME_PREFIX`, `probe-`), `http::name_is_a_leftover` is the
question, and `run()` asks it against the name it finds. If the device is
wearing one, it prints a block that cannot be missed -

```
  *** THIS DEVICE IS STILL CALLED "probe-228" ***
  That is a name this suite sets and always puts back, so a previous run died
  before it could - killed, or the machine went away. This run will NOT restore
  that name: it will leave the device with its default name (screeny-<id>).
  Brightness cannot be recovered the same way - it reads 49 now, and if that is
  not what it should be, set it by hand.
```

\- and takes the **default** name as the baseline (empty, which is
`screeny-<id>`), so both rule 44's own restore and the final one put the device
back to that instead of writing `probe-228` in again. Brightness is honest
about what it cannot know: a killed run's original brightness is gone, and the
warning says which value this one will leave behind. `RESTORE FAILED` at the
end now says the same thing.

Tests: three unit tests in `crates/probe/src/http/mod.rs` (the suite's name
matches its own prefix; `desk`, `Probe`, `screeny-4a00a4` and `""` are nobody's
leftovers; the baseline is the default name for a leftover and the found name
otherwise), and one end to end in `crates/sim/tests/http_conformance.rs` - a
simulator started **called `probe-228`**, the suite run against it, and the
device left called `""` afterwards.

`cargo test -p screeny-probe`: 20 passed. `--test http_conformance`: 6 passed
(the new one included).

### The half of item 1 that lives in the probe (2026-09-21)

Making the device answer sooner would have made the tool report *less*.
`Client::exchange` did `write_all(...)?`, so a reply that arrives while the
body is still going out - which is exactly what the new refusal does - is
thrown away and the caller sees `write: Broken pipe`. That is the line the
bench saw; the `{"error":"busy"}` beside it in the card came from somewhere
else, not from the probe. A write error is now **kept and only used if nothing
came back**: the read runs either way, and on Darwin and Linux the data already
in the receive buffer is handed over before the RST is reported (the same
behaviour card 236 relied on). One unit test, with a server that answers from
the head and hangs up on a 4 MB body: it fails with
`write: Connection reset by peer` against the old code and passes against this
one - checked, by putting the old three lines back.

### Numbers, artefacts and the bench procedure (2026-09-21) - **REVIEW**

**Rebased onto `main` at `38082f7`** (card 136 landed while this was being
written, and its own note says "the device check rides in card 246's bench
build" - so it has to be *in* the artefacts). The rebase was clean: no
conflicts, including in `docs/design/protocol-v1.md`, where 136 edited §6.3 and
the §8.6 brightness bullet and this card edited the §8.6 `status` bullet and
§8.10 step 5. Both are in the file. **The 0.7.1 artefacts below therefore carry
card 136's brightness floor**, and `screeny brightness 3` -> `applied 6` with a
lit panel is part of this bench run.

`tools/fw-size.sh`, floor 24,576, rebuilt from the rebased branch:

| build | `.stack` 0.7.0b | `.stack` **0.7.1** | `.bss` |
|---|---|---|---|
| default | 26,200 | **26,200** | 110,440 |
| `panic-test` | 26,120 | **26,120** | 110,504 |
| `http-selftest` | 25,768 | **25,768** | 110,824 |
| `start-in-portal` | 26,200 | **26,200** | 110,440 |
| `ota-test-unhealthy` | 26,200 | **26,200** | 110,440 |
| `ota-test-panic` | 26,120 | **26,120** | 110,504 |

**Not one byte of `.stack` moved.** The default image is 1,015,325 bytes
(0.7.0: 1,014,288), so the whole card is ~1 KB of flash and no RAM.

`timeout 1200 cargo test`: **957 passed, 0 failed, 1 ignored** (0 on 0.7.0's
843 + this card's new tests + card 136's).
`cargo clippy --workspace --all-targets`: silent.
Firmware clippy: 13 warnings, all pre-existing (`build.rs`, `gamma.rs`,
`store.rs`, ...) - **none in `src/ota.rs` or `src/http.rs`**, checked by
grepping the locations.

Artefacts, in the orchestrator's scratchpad
`.../3e658d49-1dd7-428e-933d-a29735a60119/scratchpad/card246/`:

| file | what |
|---|---|
| `screeny-fw-0.7.1-default.elf` | the serial-flash build, `esp_app_desc.version` `0.7.1` |
| `screeny-fw-0.7.2-good.bin` | the upload image, 1,015,376 bytes, version `0.7.2`, sha256 `96fdaaf8...` |
| `screeny-fw-0.7.2-good.elf` | the same build's ELF |

`screeny-probe fw-scan` on the `.bin`: every check passed, 5 segments, 248
sectors to stage. **The 0.7.2 version bump is not committed**: the branch reads
`0.7.1` and the tree is clean.

#### Bench procedure for the orchestrator

1. `tools/fw-run.sh` the 0.7.1 ELF (it erases `otadata`, so the device comes up
   `ota_0 / valid / Settled`). Check `screeny-probe status`: `fw 0.7.1`,
   `fw_state valid`.
2. Card 136 rides along: `screeny brightness 3` -> `applied 6`, panel lit, then
   put the level back.
3. `cargo run --release -p screeny-probe -- --addr 192.168.7.221 fw-upload
   <scratchpad>/screeny-fw-0.7.2-good.bin --activate`. Watch for, in order:
   `the device is running boot_id N`; `HTTP 200 in ~26 s`, `activating true`;
   **`at ~3 s: still boot_id N - the old image, which has not restarted yet`**
   (this line is the fix for item 2 - 0.7.0 exited here); `back after ~25 s:
   fw 0.7.2 ... boot_id M (was Some(N))`; `at ~30 s: Trial`; **`CONFIRMED`** at
   60-65 s.
4. **Without rebooting**, upload again, staged only:
   `fw-upload <the same .bin> --activate=0`... i.e. plain `fw-upload <bin>`
   (the tool stages by default). It must answer `ok true written ~1015376` and
   **not** `busy` - that is item 1. On 0.7.0 this was `busy` until a reboot.
5. `screeny-probe status`: `fw 0.7.2`, `fw_slot ota_1`, **`fw_state valid`**.
6. Optional, for item 3: the reverted reading only appears after a rollback, so
   it is only checkable with a deliberately bad image. If a rollback happens at
   all, `fw_state` must read `valid` for the slot that is running and
   `GET /api/v1/panic`'s `update` must be the thing that says what was
   rejected.
7. Item 4, with no hardware at risk: run `screeny-probe --addr 192.168.7.221
   http`, kill it mid-run with `kill <pid>` (a plain `SIGTERM`), and check the
   panel's name and brightness come back anyway. Then, to see the other half,
   set the name to `probe-228` by hand and start the suite: it prints the
   `*** THIS DEVICE IS STILL CALLED "probe-228" ***` block and leaves the
   device with its default name.

**Card state: REVIEW.** Branch `card/246-ota-follow-ups`, six commits on
`38082f7`, nothing merged and nothing pushed.

### Orchestrator, after the merge (2026-09-21)

Merged to `main` (246 first, then 230; the only conflict was `FW_VERSION`, both notes
kept, 0.8.0). Merged tree: 979 host tests pass, firmware `.stack` 25,800 (floor 24,576).
One bench for 246 + 230 + 136, on the device:
- Serial-flashed 0.8.0 from `main`; joined from the store; boot log shows the button task
  on GPIO15. `brightness 3` -> applied 6, panel lit (card 136); level put back.
- `fw-upload screeny-fw-0.8.1-good.bin --activate` (0.8.0 with the version string bumped,
  not committed): `still boot_id N - the old image`, back after 18 s as 0.8.1
  `PendingVerify` with a new `boot_id`, `Trial` at 19 s, **`CONFIRMED` at 64 s**. Then,
  **without a reboot**, the same image uploaded again: `ok true`, staged only - not
  `busy`. `fw_slot ota_1`, `fw_state valid`.
- The button, with the owner pressing: short press at idle -> status screen ~10 s; hold
  3 s and release -> countdown, "cancelled", nothing changed, still connected; hold 5 s ->
  portal and QR, re-provisioned from his iPhone through the portal, back on the LAN at the
  same address; rebooted once, rejoined from the store (`boot_count` 3, `panic_count` 0).
  Two things he saw by eye became **card 247**: the identify screen flickers over a live
  stream (reproduced with a network `IDENTIFY`, so older than the button), and the iPhone
  camera did not detect the QR (brightness was 56, not the default of decision 1).
- `screeny-probe http`: 39 passed, 0 failed, 6 skipped, 0 connects refused.
  `conformance --slow` with the panel released: 60 passed, 0 failed, 4 skipped. Panel
  given back to the Studio, LIVE at 30 fps.
Not benched: 246 item 3 (needs a rollback to show; host-tested) and item 4's SIGTERM
restore (host-tested; both suites restored cleanly on a normal exit).
