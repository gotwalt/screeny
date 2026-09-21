---
id: 199
title: The Panel screen says when the panel last panicked
type: build
hardware: no
depends: [180, 198, 243]
owner: worker (sonnet)
branch: card/199-panel-last-panic
---

## Goal

Firmware 0.5.2 (card 243) leaves a breadcrumb when it panics and serves it at
`GET /api/v1/panic`. A Studio that runs unattended for months is the thing that should
notice: show it on the Panel screen, and let it colour the Picture screen's status chip the
way the other device faults do.

## Context

- The route is **not** part of `GET /api/v1/status` (growing `StatusReply` cost the device
  3.5 KB of stack, so the firmware session moved it): `screeny_device_api` has `PanicReply`
  and `route::PANIC`; the reply is `{"boot_count":u32,"panic_count":u32,"last_panic":null |
  {"uptime_ms","boot","file","line","consecutive"}}`; goldens `panic.json` /
  `panic_none.json`; `screeny-sim` serves it.
- It cannot change while the device runs. **Read it once per `boot_id` change, never on the
  10 s poll** (the firmware session's request): the panel has one connection worker.
  `crates/studio/src/fleet.rs`'s status poller already sees `boot_id` in every status
  reply and already has the one-connection-in-flight rule; this is one more request on that
  same task, after a status read whose `boot_id` is new. Firmware older than 0.5.2 answers
  404: that is "no such route", said nowhere, not a fault.
- Card 195's rule for wording: say what is known ("panicked at panic.rs:42, 3 boots ago"),
  not what it might mean. `consecutive` > 1 is the one that deserves the fault tone.
- The SSID rule does not apply to this payload, but the usual one does: no real device's
  output in a fixture or a Log.

## Deliverables

- `devhttp.rs`: `get_panic`; `fleet.rs`: asked once per new `boot_id`; `devices.rs`: kept
  beside `facts`, in `/api/v1/status` under the device.
- `ui/panel.js` + `panel.html`: a line in the Device block; `common.js`'s `attention`: a
  reason for the chip when `consecutive` > 1.
- Tests against `screeny-sim`: read once per boot and not again; a 404 is silence; the
  connection count test in `tests/device_status.rs` still holds.

## Acceptance

The deliberate-panic build the firmware session keeps for this shows up on `/panel` within
a poll or two of the panel coming back, and never costs the panel a second connection.

## Log

### Note from the orchestrator (2026-09-20, evening)

From the firmware session, with fw 0.7.0 on the panel: there is now an always-on core-0
liveness watchdog (20 s). If the firmware wedges, the panel reboots itself and
`GET /api/v1/panic` says so - the reply has gained `"last_reset":"wdt"` and an `update`
field for OTA outcomes. Read `crates/device-api`'s `PanicReply` and its goldens for the
final shape before building this; a watchdog reset deserves the same line on `/panel` as a
panic. Known firmware bug at the time of writing: a firmware *upload* wedges core 0 (the
watchdog recovers it, nothing is activated); OTA does not work yet, and the firmware
session asked that no Studio upload support be built against it until it says so.

### The final shape (from the firmware session, 2026-09-20 evening; OTA passes on fw 0.7.0)

`GET /api/v1/panic` -> `{"boot_count","panic_count","last_panic":null|{uptime_ms,boot,
file,line,consecutive},"update":null|{"outcome":"trial"|"confirmed"|"reverted","reason":
null|"deadline"|"aborted"|"rejected","slot":"ota_0"|"ota_1","version":"x.y.z"},
"last_reset":"power_on"|"software"|"wdt"|...}`. Types and goldens are in
`crates/device-api`; the spec is `docs/design/protocol-v1.md` 8.6/8.10. Read once per
`boot_id` change, never on the poll. A wedge shows as `last_reset: "wdt"` and a climbing
`boot_count`; an update's outcome belongs on the same line of `/panel` (see card 185).

### Worker (sonnet), 2026-09-21

Worktree was mis-created off an older commit (127c829); rebranched from 6bdcc1c as the
orchestrator asked, before any other work, and confirmed the card file was still there.

**Plan**, from reading `crates/device-api` (`PanicReply`/`PanicRecord`/`UpdateRecord`, the
four goldens including the two the firmware session added, `panic_update_trial.json` /
`panic_update_reverted.json`), `crates/sim`'s `panic_breadcrumb` (always `last_panic: null,
last_reset: null`, `update` from `OtaModel::update_record()` - off unless a test turns it
on with `SimHandle::model_ota`; the simulator can never produce a `wdt` reset or a real
panic, which is a real gap for testing wording against it, noted below), `devhttp.rs`,
`fleet.rs`'s `status_once`/`spawn_device_http`, `devices.rs`'s `DeviceFacts`/`Registry`,
`health.rs`'s `DeviceStatus`, and `ui/panel.js`'s `showDevice`/`common.js`'s `attention`.

- `devhttp.rs`: the status reader was one function, `read_status`, hard-wired to
  `route::STATUS`. Generalised it to `read_json<T: DeserializeOwned>(path, addr, patience,
  cost)`, kept `get_status`/`get_status_counted` as thin wrappers, and added
  `get_panic`/`get_panic_counted` the same way. One connection-handling implementation for
  both routes, per `CLAUDE.md`'s "one implementation of each thing" - the 404-is-absent
  rule, the deadline and `Connection: close` were one copy already meant for `STATUS`
  and would have become two copies of the same seventy lines otherwise.
- Next: `devices.rs` gets a `PanicFacts` (mirrors `DeviceFacts`'s shape: `heard_unix` +
  `#[serde(flatten)] reply: PanicReply` + one derived boolean, `repeat` =
  `last_panic.consecutive > 1` - the one fact from here the brief says colours the chip),
  kept on `DeviceRecord` beside `facts` as `panic: Option<PanicFacts>`, plus a live-only
  `panic_asked_for: Option<u32>` so "once per `boot_id`" survives a failed or 404 read
  without a second field. `health.rs`'s `DeviceStatus` gets `panic`/`panic_ago` beside
  `facts`/`facts_ago`.
- `fleet.rs`'s `status_once`: after a successful status read (`heard_http`), ask
  `st.devices.want_panic(&id, boot_id)`; if true, one more sequential `spawn_blocking` on
  the same task calls `devhttp::get_panic_counted` at the same `addr`, and
  `Registry::heard_panic(&id, boot_id, Option<PanicReply>)` records the ask **either way** -
  success or fault - so a 404 (older firmware) or a timeout is not retried until the
  `boot_id` changes again. Never concurrent with the status read: it is the next line in
  the same `await`ed sequence, so there is still exactly one HTTP connection to a device in
  flight at a time across the whole fleet, which is the property
  `the_studio_never_opens_a_second_connection_to_a_device` already proves and this card
  must not break.
- **Decision, and a discrepancy with the spec worth flagging for the orchestrator**: spec
  8.6 says a reader "asks once per `boot_id`, **and again a minute or two later if it saw
  `trial`**". This card's own hard rules (from the coordinating session) say the opposite in
  plain terms: "the panic read happens once per new `boot_id` ... never on the 10 s poll"
  and "the test that proves 'read once per boot and not again' ... [is] the heart of this
  card." I followed the hard rule as written and tested it, since it is this card's explicit
  contract: `want_panic` never re-asks for the same `boot_id`, even after seeing `trial`. A
  trial that later confirms itself is therefore not picked up until the *next* reboot's
  `boot_id` (confirm or revert do not change `boot_id`). Noted in the report; not fixed here
  (`docs/design/protocol-v1.md` is out of scope for this card) - a new card if the owner
  wants the re-ask-after-trial behaviour.

**Built** (`devices.rs`, `fleet.rs`, `health.rs`):

- `devices.rs`: `PanicFacts` (`heard_unix` + `#[serde(flatten)] reply: PanicReply` +
  `repeat: bool` = `last_panic.consecutive > 1`), a normal `#[derive(Debug)]` this time -
  unlike `DeviceFacts`, nothing in `PanicReply` is credential-adjacent, so no hand-written
  redaction is needed. `DeviceRecord` gained `pub panic: Option<PanicFacts>` beside `facts`,
  and a private, live-only `panic_asked_for: Option<u32>` for the "once per `boot_id`" rule.
  `Registry` gained `want_panic(id, boot_id) -> bool` (true exactly when the last ask, if
  any, was for a different `boot_id`) and `heard_panic(id, boot_id, Option<PanicReply>)`,
  which marks the ask taken **either way** - the thing that makes a 404 or a timeout "once"
  rather than "once, then retried every ten seconds until the reboot".
- `fleet.rs`'s `status_once`: captures `reply.boot_id` before `heard_http` consumes the
  reply, then - still inside the same `for record in devices_now` iteration, still awaited
  before moving to the next device - asks `want_panic`, and if true does one more
  `spawn_blocking(devhttp::get_panic_counted)` at the same `addr`, on the same task,
  sequential with the status read that just finished. Its cost is metered the same way
  (`metered_http`); its outcome, success or fault, goes straight to `heard_panic` and
  nothing about a panic fault is logged - "404 is silence" extends to "any panic fault is
  silence", since the panic route is supplementary and its own failure is not the device's.
- `health.rs`'s `DeviceStatus` gained `panic: Option<PanicFacts>` and `panic_ago:
  Option<f64>` beside `facts`/`facts_ago`, filled the same way.
- `cargo build -p screeny-studio --lib` and `cargo clippy -p screeny-studio --all-targets`
  both clean after this step (one doc-comment lint fixed: a stray `- ` in a `///` line read
  as an unindented list continuation).

**The page** (`panel.js`, `panel.html`, `common.js`):

- `panel.js`'s `showDevice` gained one row, `['Panic', panic.text, panic.tone]`, from a new
  helper `panicLine(p)` placed between `showDevice` and `rebootLine`. One line, not three,
  because the brief asks for the panic, the watchdog reset and the update's outcome
  "covered" together - they are three symptoms of the same kind of event, said in the
  wording style of card 195 ("say what is known"): `panicked at net.rs:321, 1 boot ago`,
  `, 3 times in a row` appended when `consecutive > 1`, `the watchdog reset it` for
  `last_reset: "wdt"`, and `firmware 0.7.1 trial` / `confirmed` / `reverted (aborted)` for
  `update`, joined with ` · ` when more than one applies. The row is omitted entirely when
  there is nothing to say (no panic on record, no wdt reset, no update) - the ordinary case,
  matching how `Store errors` is only pushed when non-zero.
- Tone: `bad` for `p.repeat` (`consecutive > 1`) or a watchdog reset, `warn` for an isolated
  panic or a reverted update, nothing at all for a plain trial/confirmed - only the server's
  own `p.repeat` decides the fault case, the page never computes a threshold. This added two
  `'bad'` literals to `showDevice`'s block, so `tests/ui.rs`'s
  `the_page_can_show_what_only_the_device_knows` now expects six fault tones instead of four,
  and gained `p.repeat` in the "read, not decided" list.
- `common.js`'s `attention()` gained `if (device.panic && device.panic.repeat) return
  'repeated panics';`, right after the "stopped" check - `consecutive > 1` is the one fact
  from the panic route the brief says should colour the Picture screen's chip; an isolated
  panic, a wdt reset or an unconfirmed update stay on the Panel screen only, in line with the
  existing rule that only fault-level facts reach the chip.
- `panel.html`: no new elements (the row lives in the existing `#device-facts` `<dl>`, filled
  by `facts()` like every other row); the doc comment above `#device-block` now mentions the
  Panic row and the second route it comes from.

`node --check panel.js` and `node --check common.js`: clean. `cargo test -p screeny-studio
--test ui`: 27 passed, 0 failed (was 26 before the assertion-count fix above).
`cargo test -p screeny-studio --test device_status`: 7 passed, 0 failed, including
`the_studio_never_opens_a_second_connection_to_a_device` - the second, sequential connection
this card adds does not change the "one at a time" property that test proves.

**Tests** (`tests/device_status.rs`), against a hand-written `PanicServer` rather than
`screeny-sim`: the simulator's `GET /api/v1/panic` always answers `last_panic: null,
last_reset: null` and can never 404 (card 243 landed the route in it, and it has no crash
path), so proving "once per boot, not again", "a new boot asks again" and "404 is silence"
needs a server that can be told to do all three on command - the same reason `CountingServer`
and `Flood` in this file are hand-written rather than borrowed from the simulator.
`PanicServer` answers `GET /api/v1/status` with a movable `boot_id` and `GET /api/v1/panic`
with a canned reply or a 404, and counts each path's hits separately.

- `the_panic_route_is_read_once_per_boot_id_and_not_again`: five-plus status reads at a
  fixed `boot_id`, one panic read. Also checks the JSON shape that reaches
  `/api/v1/status` - `boot_count`, `panic_count`, `last_panic.{file,line,consecutive}` and
  the derived `repeat: true` for `consecutive: 2`.
- `a_new_boot_id_is_asked_for_its_panic_again`: one read at `boot_id` 1, then the server's
  `boot_id` moves to 2 and the studio is proven to ask again - `panic_hits` goes 1 -> 2 and
  no further.
- `a_404_from_the_panic_route_is_silence`: `devices[0].panic`/`panic_ago` stay `null`,
  `/api/v1/status`'s `ok` stays `true` with no `problems`, the *status* route's own
  `last_error` is untouched by the panic route's 404, and the panic route is asked once, not
  retried every 100 ms poll.

The existing `the_studio_never_opens_a_second_connection_to_a_device` (against its own
`CountingServer`, which answers any path 200 with a status-shaped body - so the studio's one
extra panic request per test run gets a reply it fails to parse as `PanicReply` and is
silently absorbed) still passes unmodified: the panic read is sequential with the status
read, never concurrent, so `most` stays 1.

`cargo build -p screeny-studio --tests` and `cargo clippy -p screeny-studio --all-targets`:
clean. `cargo test -p screeny-studio --test device_status`: 10 passed, 0 failed, run three
times back to back (including under `--test-threads=4`) with no flake. `ps` after every run
shows nothing of mine left running.

**Root `cargo test --release --no-fail-fast` (first full run) found one break**, not in a
file this card was told to touch but caused by it:
`crates/studio/tests/traffic.rs::the_http_and_control_paths_are_counted_separately` asserted
`http.out.packets == http.reads` - true until now because only a status read ever moved
`http.out.packets`. Card 199's one-time panic ask goes through the same `metered_http`
counter (deliberately: it is genuinely HTTP traffic to the panel's status API, and folding it
into the same "http" bucket is the right accounting, not a bug to work around), so the count
is `reads + 1` from the moment that ask has landed, and grows 1:1 with `reads` after that -
never again exactly equal to `reads`. Fixed the test to wait past the one-time ask
(`devices[0].panic` becomes non-null once it lands; the simulator's panic route never 404s)
and assert the `+1` explicitly, plus a second, stronger check that the *growth* from one read
to the next is 1:1 once the one-time ask is behind it. Re-ran
`the_http_and_control_paths_are_counted_separately` alone three times (release, twice more
after the fix): all green, no flake.

This is the only test outside the files the card names that needed a change, and the reason
is explained above rather than left as a silent edit.

### Finish

**Root `cargo test --release --no-fail-fast`, after the fix above**: 98 test-result blocks,
**988 passed, 0 failed**, no panics, no `FAILED` anywhere in the log.

**Root `cargo clippy --workspace --all-targets`**: exit 0, two warnings, both **pre-existing
and in files this card never touched or was allowed to touch** - `crates/receiver/tests/identify_overlay.rs`
(a doc-comment lint, `doc_lazy_continuation`) and `crates/sim/tests/arbitration.rs`
(`chunks_exact_to_as_chunks`). Confirmed with `git log` that both files last changed on card
247, well before this branch, and `git diff 6bdcc1c --` against them is empty - nothing here
introduced or touched either. `screeny-studio` and `screeny-device-api` (read, not written)
produce zero clippy output. Flagging these two for the orchestrator since `docs/README.md`
says clippy is expected to say nothing; likely a toolchain/lint-version drift since card 247
landed, not this card's concern.

`ps` after the full run: nothing of mine left running.

Card stays in `doing/`; the orchestrator moves it to `done/` on merge, per the coordinator's
instructions for this run (rather than `docs/README.md`'s usual `review/` step).
