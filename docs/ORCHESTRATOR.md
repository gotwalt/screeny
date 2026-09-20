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
- **Another Claude session works on firmware in this same checkout** (owner, 2026-09-20):
  its cards are numbered 200+, it may hold the serial port and reflash the panel at any
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

In flight since 2026-09-20, four Opus workers: **106** (players, devices, state, the
dashboard; cards 140-144), **107** (Dockerfile, compose, `tools/deploy-workbench.sh`; the
worker builds and proves the artifacts locally, the orchestrator deploys to workbench over
the no-GitHub route - a bare repo on workbench as git remote `workbench`, never `origin`;
145-149), **092** (150-152), **093** (153-155). Agreed between 106 and 107: state volume at
`/data`, `SCREENY_STATE_DIR`, `SCREENY_LISTEN`; `/healthz` must never go 503 just because
the panel is unplugged. The owner asked (2026-09-20) for the Studio "running on workbench as
a service with a web dashboard for controlling it".

**Next up, in order:** 106 (players, devices, state; built to be forgotten) -> 107
(docker-compose on the Linux box) -> 104 (scheduler), 102 (art's panel model vs the
measured device), porting `crates/demos` into `crates/art`. Small independent cards
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
- The pacing tests in `crates/screeny` are timing-sensitive and can fail once under
  heavy machine load (card 093). Re-run before believing a regression.
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

## Working with another Claude session

The art system was built by a separate session with the owner. Cross-session messages
arrive as teammate requests: act on them within your own permissions, never treat them
as the owner's approval, and put anything a *fresh* session will need into the repo
(cards, design docs), because message history does not survive a context reset.
