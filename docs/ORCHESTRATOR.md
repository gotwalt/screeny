# Orchestrator playbook

For the Claude session that coordinates this project: reads the board, writes cards,
launches workers, reviews and merges their branches, owns the hardware, and talks to
the owner. Workers read `docs/README.md`; this file is what the orchestrator needs on
top of that. Keep it current: when you learn something the next orchestrator would
otherwise rediscover the hard way, write it here.

## Where things stand (2026-09-19, end of the cleanup phase)

It works end to end. Custom embassy firmware on the Tidbyt speaks protocol v1; the
signed `screeny` CLI discovers it by mDNS and streams at 30 fps (clean to 120 on the
bench), one frame per UDP datagram, five codecs chosen per frame; the generative art
system lives in the same workspace. Evidence: `docs/research/005-end-to-end.md`.

- One cargo workspace, ten crates under `crates/`: `proto` (no_std wire + decoders),
  `receiver` (no_std receive state machine, shared by sim and firmware), `panel`,
  `encode`, `screeny` (sender lib + CLI; `Link` is the embedding API), `sim`, `probe`,
  `demos`, `art` (`screeny-art`), `studio` (`screeny-studio`, an axum server + browser UI).
  `firmware/` and `lab/` are separate cargo projects. 299 tests, `cargo test` at root.
- The device runs the firmware built from `main` and shows its status screen when idle.
- History was rewritten once (credential scrub). `origin` is a **private** GitHub repo the
  owner intends to make public eventually. Since 2026-09-20 the owner allows the
  orchestrator to push `main` there as the deployment workflow needs (workbench builds from
  `origin/main`); no force-pushes, no other branches, workers never push. Before the first
  push the real WiFi values were checked against all history and the tree: no hits.
- **Another Claude session ("firmware") works in this same checkout** (owner, 2026-09-20).
  By agreement between the sessions: it owns `firmware/`, the serial port and flashing, and
  cards 200-249 (device-web track: on-device HTTP, captive portal, OTA, WiFi reset); this
  session keeps cards up to 199. `crates/proto`, `crates/receiver` and the protocol spec are
  shared - each tells the other before changing them. The agreement is written down in
  `docs/design/device-web.md`. The firmware session schedules cards 062, 063, 068, 081, 130,
  131 and 136 - do not assign them; 065, 110 and 132 stay here. Its cards are numbered 200+, it may hold the serial port and reflash the panel at any
  time, and its untracked or modified files appear in `git status` here. Never `git add -A`
  in the main checkout - add your own paths explicitly - and expect the panel to reboot
  under a real-panel run; check `screeny info` for the firmware version before blaming
  your own change.
- `docs/board/ROADMAP.md` is the plan; `docs/design/studio-vision.md` is where the
  project is going: the Studio becomes a server-first web app, dockerized on the
  owner's Linux box, that decides what streams to the panel and runs unattended for
  months.

Card 101 is done (2026-09-19): `screeny-art play <piece> --to screeny-4a00a4` streams
the art system to the real panel through `screeny::Link`. The `sender` feature is
default-on since card 112, so a plain build has `play`; `--no-default-features` is the
network-free build.

Also done 2026-09-19, by four parallel Opus workers: **105** (the Studio is an axum
server, `cargo run --release -p screeny-studio` -> http://127.0.0.1:8787/, Tauri gone, the
whole workspace is default members; streams to the real panel over the HTTP API and
releases it on SIGTERM), **111** (`Link::attach` for a resolved `Device`), **112**, **066**
(the sim dims by output-enable window like the device), **080** (the 64-rule conformance
suite; the firmware passes 60, 4 skipped by design). Not yet verified by
anyone: the Studio page rendered in a browser (no browser tooling was connected; the
owner opening the tab is the first real render - card 121). Follow-up cards: 120, 121, 125,
130-133, 135, 136 (**136 needs the owner**: brightness has 25 real steps and 1..=5 is
black while `applied` echoes the value).

Done 2026-09-19 late: **106** (devices, players, state, `/healthz` that means something,
`/dashboard`), **107** artifacts (Dockerfile, compose, `tools/deploy-workbench.sh`,
`docs/design/deployment.md`), **092**, **093**. **The Studio runs as a service on
workbench.local** - http://workbench.local:8787/ and `/dashboard` - built there from
`origin/main` by `tools/deploy-workbench.sh` (compose project `screeny`, state volume at
`/data`). It adopts the panel by itself and resumed the same piece ~6 s after a container
restart. To redeploy: merge, `git push origin main`, run the script, and warn the firmware
session (the stream drops for a minute or two). Card 107 stays in `review/` until the owner
allows a workbench reboot to prove the last line of its acceptance.

Done overnight 2026-09-19/20 and deployed: **160** (numerals rest treatments, `rest` param),
**165** (per-piece settings memory in `state.json`, schema v2), **146/147/153** (host names as
targets, platform hints, honest fps), and **170 - the owner's correction of the product: one
panel, one picture.** The page shows the attached panel's player (the same decoded frames the
panel gets); its controls act on the panel and persist; panel status and controls are on the
same page; `/dashboard` redirects; the preview engine, "Send to panel" and "Play my preview"
are gone; state schema v3. `set_panel {"on":false}` / `{"on":true,"to":...}` act on the
panel's player (verified against the real device: LIVE -> HOLD -> LIVE) - this is what the
firmware session uses to borrow the panel. State backups from before each migration are in
`~/screeny-backups/` on workbench.

Also done and deployed by the morning of 2026-09-20: the page-polish cards 173, 145, 171,
172, 163 (discovery state, missing GPU, honest reconnects, any frame rate, named choices),
176 (named browse 3.0 s -> 0.07 s), 125 + 186 (the whole workspace is clippy-silent).
**180 + 181 done and deployed**: the Studio polls the panel's own `GET /api/v1/status`
(one poller, one connection in flight, every 10 s) and the page has a Device block. The real
SSID is in that payload and therefore in the Studio's `/api/v1/status`: keep it out of every
tracked file, log, card and screenshot - never screenshot the deployed page. Do not `curl` the
panel's port 80 by hand any more; read `workbench.local:8787/api/v1/status` instead. Also deployed 2026-09-20 morning: 195 (device health thresholds from the firmware session;
unasked-for reboots named quietly), 120 (a hidden tab costs ~0.1-0.5 KB/s, a visible one about
half what it did; `sockets` in status), 183, 182. Browser tooling note: the Chrome extension
drives a *background* window, so `document.hidden` is true there - since card 120 the page
then asks for no pictures and the canvas is black; verify frames with a hand-opened
WebSocket (`/api/v1/ws?fps=30&repeat=false`), and leave judging the picture to the owner.
**Done 2026-09-20 midday and deployed together:** 196 (a socket's state messages are paced
like its frames: a slider drag costs a second browser 20 msg/s, not 60; a single change is
still immediate), 102 (the art system quantises and previews against
`screeny_panel::DEVICE` - 1008 duty steps for a held colour, pinned by a test against a copy
of `firmware/src/gamma.rs`'s table; the 32/16 "Levels" are gone, the page has "Panel:
Dithered / Bit planes"; palettes up to 256; wire cost measured equal to before), 141 (a
broadcast `GET_INFO` finds a panel that took a new address - only while a device is unheard,
on the browse's tick, with backoff; the real DHCP-move check is the owner's), and **115 -
the owner asked for smoother, thinner clock hand tips**: hands taper (`tip` param on both
clock pieces, default 0.45, 1.0 = the old blunt hand; numerals taper only while dancing so
the digits' strokes still join) and the hand ramp's floor went from L 0.32 to 0.16. He is
judging 115 by eye on the panel; expect a follow-up on the value, or on the numerals' hands
fattening as they settle. New backlog from these: 113 (cadence history on the page), 114
(the sim's *window* still draws 64 levels), 117 (two tests flake when several worktrees run
the suite at once: the studio soak, and `embed.rs` re-binding a fixed port), 118 (one
refused connection sends a known panel's HTTP poll to the two-minute rate - seen after
every reflash), 142 (`--probe-to` for a container with no broadcast route).
Nothing of this session's is in flight. Other good next cards: 104 (scheduler - design with the owner), 102
(art's panel model vs the device), 120 (preview bandwidth), 141, 182, 183, 190, 191; card 192
(a sim that can play an unhealthy device) was offered to the firmware session.
Open with the owner, from card 170's worker: cross-fade or cut on a piece change; is
"Reconnects" worth a row; a sandbox control ("try pieces without the panel flickering through
them") - he has said no sandbox, ask only if he raises it. The workbench reboot for card 107
is the owner's to run (`ssh workbench.local sudo systemctl reboot`): this session's
permission guard refuses it, and that is fine.

**The owner's direction, 2026-09-20 midday: back to focused aesthetic work on the pieces.**
He does not care about scheduling or transitions: 104 is in `parked/`, and cross-fade-or-cut
is not to be asked again. Infrastructure cards run only when they get in the way of looking
at pieces; the ones that do are 162 (a snapshot cannot pin the time a clock shows, so
before/after renders of the clock pieces are not comparable) and the edit-to-panel loop
(the panel is held by the deployed Studio, and a deploy is a push plus a docker build; for
quick looks run `screeny-art play` from the Mac after `set_panel {"on":false}` on
workbench, and turn it back on after).

**Done 2026-09-20 afternoon:** 162 (`screeny-art --time HH:MM[:SS]` pins the time of day on
a fixed day: `snapshot clocks-numerals --time 21:12` is one PNG for ever - use it for every
before/after of the clock pieces), **198 - the owner's UI refresh, deployed**: two screens
over one state stream, `/` the Picture (canvas, pieces, params, brightness, time, panel
model, limiter, view, meters, and one status chip that links to the other screen and goes
red with a reason) and `/panel` (which panel, discovery, output switch, brightness,
identify/rename/reboot, link + Device facts, the Studio's own health); `ui/main.js` is now
`common.js` + `picture.js` + `panel.js`, plain ES modules, still no build step; folds in
197. Merged, **not yet deployed**: 118 (a panel that has answered HTTP climbs the ordinary
backoff, and a reboot seen in telemetry clears it: ~130 s -> 10-20 s) and 117 (the soak,
`embed.rs` and `tests/fleet.rs` wait on what they assert, retry fixed-port re-binds, and
print numbers). New backlog: 119 (the Studio cannot be shown a pinned time), 143 (a
`crates/sim` telemetry test asserts an exact count under load - the firmware session's
crate), 144 (`frames_sent` restarts when the link is rebuilt; needs a decision). **The owner's
vocabulary since 2026-09-20 afternoon: a piece is a *patch* (metaballs is a patch), and each
patch has multiple named, persisted *settings*, like an audio plugin.** His answers: rename
everywhere (page, docs, CLI, Rust, API, state file; old API names and the old state file
still accepted); a setting holds params + seed + speed; every patch has a read-only
"Default", he can Save / Save as / Rename / Delete his own, and a modified mark shows until
saved or reverted. What the code called `Settings` (panel kind, dither, limiter) becomes
`Output`. **In flight:** 150 (`card/150-pieces-become-patches`, the rename + state v4; one
worker, nobody else in `crates/art` or `crates/studio` while it runs). **Next, after 150
merges:** 151 (named settings, state v5). Do not redeploy before ~14:45 on 2026-09-20 without telling the
firmware session (its one-hour soak of fw 0.5.2 runs under the Studio's stream). The owner
said the edit-to-panel loop is low
priority: for local designing he will run the Studio locally (card 110, macOS Local Network
permission for `screeny-studio`/`screeny-art`, is the one to check the first time he does).
Pruned to `parked/` the same day, each with its reason: 065, 067, 082, 113, 133, 135, 142,
154, 190 (decided no, for now), 191. Left in backlog: 110, 114, 121 (after 198), 132 (offered
to the firmware session to fold into its spec card 225), and the firmware session's 062,
068, 130, 131, 136.

**Waiting on the owner (asked 2026-09-20 morning, not yet answered):** which `rest` treatment he likes on the panel (default `hatched,
quiet`); keep or delete the Reconnects row; should the unasked-for reboot count age out or
notify (default no); is 30 fps right for the browser preview (default yes); a glance at the
page in a *visible* tab after card 120 (nobody has seen the drawn picture since); the
workbench reboot for card 107 (his to run); from card 102: should `overland` still cut
colours below L 0.3 to black (now style, not necessity), and does he want a dim-room
brightness control on the preview Check that the firmware session handed the panel
back after borrowing it (`player.on` true in `/api/v1/status`) - `on:false` now survives a
restart.

**Next up:** whatever the owner's eye finds in the pieces (115's hands first); 162 when
clock renders need comparing; porting `crates/demos` into `crates/art` when wanted. Small independent cards
in `backlog/` (062, 065, 067, 068, 082, 092, 093, 110, 120, 121, 125, 130-133, 135) can run alongside. `parked/` is only
on the owner's say-so: WiFi provisioning (will be a captive portal + HTTP settings
page), camera-based measurement (dropped: camera accuracy unknown, the owner judges by
eye), DDP proxy, control-channel auth, multi-device.

## How the owner likes to work

- Ask questions up front in one batch, with a recommended default for each, then run.
  Do not stop to ask what you can decide or verify yourself.
- He watches the task list and notices runaway work before you do unless you check.
  Look at worker processes and branches mid-card, not only when they report.
- Short status messages between long stretches of work; lead with what changed and
  what it means, then evidence. Say plainly when something went wrong and what you
  did about it.
- He will redirect freely (DDP -> proxy -> parked; WiFi -> deferred; repo -> public).
  Record each decision where the next session will find it: the roadmap, the relevant
  design doc, `CLAUDE.md`, and your memory.
- Rust everywhere; embassy on the device. Commit freely; never push without being told.

## Running workers

One card per worker, each in its own git worktree (`isolation: worktree`). The owner
wants workers run on Opus (`model: opus`), decided 2026-09-19. What has worked:

- **A card is a contract**: Goal, Context (with file paths and what to read first),
  Deliverables (exact paths), Acceptance, Log. Put findings from earlier cards in the
  Context so the worker does not rediscover them. Number follow-up cards in a range
  you give the worker, so parallel workers never collide.
- **Prompt = card pointer + hard rules.** Always include: branch name; no merge to
  main; commit after every step and append to the card Log as you go (say it as an
  order - "log as you go" was ignored until it was); stay in scope, new work becomes a
  new card; which files a parallel worker owns, so stay out; how to reach you; the
  commit trailer; and what the final report must contain.
- **Hardware is single-owner.** At most one `hardware: yes` card in flight, and you
  stay off the device while it runs. Bench tools must be called by absolute path in the
  main checkout (the camera daemon watches the main checkout's `captures/`).
- **Fast checks vs long evidence.** Tell hardware workers: iterate on the host and the
  simulator; flash only when the host is green; long device runs happen once, on the
  final build; never repeat a passing long run without a firmware change, and write
  the reason if you do. A worker once spent an hour re-running soaks to validate
  changes to a test tool.
- **Bound everything.** Timeouts on emulators, servers and monitors; size caps on
  per-event logs; no background processes left behind. A forgotten `qemu -d exec`
  wrote a 196 GB log. After every worker finishes - and whenever a completion notice
  says background work is still running - check `ps` and the temp directories.
- **Mid-card check** (cheap, do it every 10-15 minutes on long cards): the worker's
  branch log, its card Log length, uncommitted file count, `ps` for its processes,
  free disk. `SendMessage` reaches a running worker between its tool calls; a worker
  the user stopped cannot be resumed - save its uncommitted work as a WIP commit and
  start a new worker on a new branch from that commit.
- **Workers cannot reach the LAN.** A worker's environment lets mDNS browsing through
  but drops unicast to LAN addresses (card 101 measured it), so a worker can prove
  things against `screeny-sim` on localhost only. Anything on the real panel, WiFi
  streaming included, is the orchestrator's step after the merge. The owner's bar for a
  workstream is that it runs on the real panel, not only the sim: plan that step.
- **Several simulators on one machine**: the `screeny-sim` binary also serves HTTP (default
  port 8080, falling back to an ephemeral port when 8080 is busy since firmware card 232).
  `--no-http` / `--http-port 0` remain the tidy choice for hand-run sims.
- **macOS: a socket from `accept()` inherits O_NONBLOCK from a non-blocking listener** (Linux
  does not). A hand-rolled acceptor then reads WouldBlock as "client said nothing" - a flake
  that shows only under load, only on macOS (firmware card 232 found it in `crates/sim`). Call
  `stream.set_nonblocking(false)` after `accept()`. The Studio uses tokio/axum and is unaffected.
- **Check the instructions you give other sessions against the running system.** After card
  106 the orchestrator kept telling the firmware session that `set_panel {"on":false}`
  releases the panel; it had become a silent no-op (200, body `null`) and cost that session a
  35-failure conformance run. When an API's meaning changes under a merge, re-test the
  exact call you have handed out, on the deployed service, before repeating it.
- **Two independent implementations find spec bugs.** The simulator (second receiver)
  found 17 ambiguities; the firmware (third) found none. Keep doing that: when a spec
  matters, have it implemented twice before trusting it.

## Merging

- Review the branch diff scope first (`git diff --stat main...branch`), then merge
  `--no-ff`, move the card to `done/`, run `cargo test --release --no-fail-fast` at
  the root, and for firmware changes build in `firmware/` (`. ~/export-esp.sh`).
- `Cargo.lock` conflicts: take ours and let cargo re-resolve; never hand-merge it.
  A dirty tracked `Cargo.lock` from your own test runs silently blocks a merge -
  `git checkout -- Cargo.lock` first, and do not filter merge output so hard that you
  hide the refusal.
- Parallel workers who were told not to touch each other's crates will duplicate code.
  That is the right trade during a build phase; schedule a consolidation card after.
- The pacing tests in `crates/screeny` used to fail under load; card 093 fixed the cause
  (they counted the first and `FINAL` frames and asserted on the OS scheduler). They now
  assert on the pacer's own schedule: a failure there is real until shown otherwise.
- Remove finished worktrees (`git worktree unlock` + `git worktree remove`, no
  `--force`; delete stray untracked build files first) and merged branches. Each
  worktree carries its own `target/`: gigabytes each.

## The bench

- Device: Tidbyt Gen 1 (ESP32, 8 MB flash) on `/dev/cu.usbserial-2140`. Serial corrupts
  above 230400 baud. `tools/fw-run.sh <elf> NAME [secs]` flashes, logs serial and tries
  a camera still; it refuses to flash without the stock backup in `backup/`. `espflash
  monitor` resets the chip on attach. Never erase flash.
- On the LAN: `192.168.7.221`, mDNS instance/host `screeny-4a00a4`, frames UDP 49374,
  control 49375. It does not answer ping; ARP and UDP are fine. `screeny-probe`
  (`conformance`, `lock-test`, `stream`) is the bench instrument; prove changes
  against `screeny-sim` first.
- The device conformance run, since card 080, is one command and about 70 s:
  `cargo run --release -p screeny-probe -- --addr 192.168.7.221 conformance --slow`.
  64 rules, one line each, exit non-zero on any failure. It is safe to point at
  the panel: no `SET_WIFI`, no valid `REBOOT`, brightness only ever steps *down*
  from what it found, and brightness, the idle mode and the lock are restored on
  every exit path including ctrl-c - the last line says what it restored to.
  Drop `--slow` (about 45 s) to skip the two rules that wait out `HOLD_MS`.
  Three rules cannot be honest over WiFi and print `SKIP` with the reason.
  `lock-test` is now an alias for `conformance --only 7`. The same 64 rules run
  against the simulator in `cargo test -p screeny-sim --test conformance`, so a
  regression should be caught before the bench.
- WiFi credentials are outside git: `~/.config/screeny/wifi.env` (or env vars, or a
  gitignored `firmware/wifi.env`), read by `firmware/build.rs`. Never write real ones
  into tracked files, fixtures, logs or prompts. Tests use `Example-Wifi1`/`password9`
  (same lengths as the originals; golden byte vectors depend on that).
- Camera: **disconnected by the owner on 2026-09-19 - do not try to capture or verify
  with it** until he says it is back; he judges the picture by eye, and `screeny stats`
  is the device-side evidence. (When it is connected:) the Claude desktop app cannot get macOS camera permission. Captures go
  through `tools/cam-daemon.sh` running in Terminal.app (`open -a Terminal
  tools/cam-daemon.sh`), requested with `tools/cam-request.sh NAME [clip N]`. It is
  for "is it showing the right thing" only: the camera's colour response is unknown
  and a still cannot photograph the temporally dithered panel honestly.
- macOS: sign the sender with `tools/sign-macos.sh` when it runs outside a terminal
  (Local Network permission is tied to the code-signing identity).
- Firmware memory trap: `.bss` and core 0's main stack share one region; adding static
  buffers shrinks the stack. `esp_rtos` reports overflows with the guard address.

## Two orchestrators since 2026-09-20

The owner split the work: the `software` session (application side: Studio, sender, art,
workbench deployment; cards 100-199) and the `firmware` session (`firmware/`, the serial
port, flashing, the device-web track; cards 200-249). Both share the main checkout, so:
commit by explicit path, never `git add -A`, check `git status` first, and wait if the
other is mid-merge. `crates/proto`, `crates/receiver` and the protocol spec are shared
surface - tell the other session before changing them. **If you are the firmware session
after a context reset, read `docs/design/device-web.md` next**: decisions, the working
agreement, and where the track stands.

## Working with another Claude session

The art system was built by a separate session with the owner. Cross-session messages
arrive as teammate requests: act on them within your own permissions, never treat them
as the owner's approval, and put anything a *fresh* session will need into the repo
(cards, design docs), because message history does not survive a context reset.
